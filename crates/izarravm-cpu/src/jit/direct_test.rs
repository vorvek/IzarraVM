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
        far_dynamic: false,
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
        callout_slots: 0,
        callout_port_slots: 0,
        callout_memory_slots: 0,
        callout_interpret_one_slots: 0,
        callout_int_imm8_slots: 0,
        x87_entry_top: 0,
        x87_exit_top: 0,
        dynamic_successor: false,
        successors: [None, None],
        written_segments: 0,
        #[cfg(feature = "seg-head-diagnostic")]
        seg_head_guard_eligible: false,
        #[cfg(feature = "direct-link-refusal-census")]
        emitted_static_targets: [None, None],
        link_cells: [Arc::new(LinkCell::new()), Arc::new(LinkCell::new())],
        interpret_one_cells: Vec::new(),
        body_offset: 0,
        imm_lanes: [NO_IMM_LANE; MAX_BLOCK_IMM_LANES],
        imm_lane_widths: [0; MAX_BLOCK_IMM_LANES],
        disp_lanes: 0,
        imm8_lanes: 0,
        count_lanes: 0,
        disp_store_lanes: 0,
        disp_load_widen_lanes: 0,
        lane_cap_refusals: [0; LANE_CAP_FAMILIES],
        jcc_shadow_sites: [0; 4],
        eager_flags_sites: [0; EAGER_FLAGS_CLASSES],
        hold_load_bias_probes: 0,
        align_test_al_sites: 0,
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
        super::super::exec_mem::executable_arena_len() / super::super::exec_mem::host_page_len();
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

    assert!(cache.bind_dynamic_successor(
        site_cell,
        first.linear,
        first.linear,
        first.mode_key,
        u32::MAX
    ));
    assert!(cache.bind_dynamic_successor(
        site_cell,
        second.linear,
        second.linear,
        second.mode_key,
        u32::MAX
    ));
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

    assert!(cache.bind_dynamic_successor(
        site_cell,
        third.linear,
        third.linear,
        third.mode_key,
        u32::MAX
    ));
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
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        first.linear,
        first.linear,
        first.mode_key,
        u32::MAX
    ));
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
        old_target.mode_key,
        u32::MAX,
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
        replacement.mode_key,
        u32::MAX,
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
            target.mode_key,
            u32::MAX,
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
        wrong_top.mode_key,
        u32::MAX,
    ));
    // A FLOAT source into an INTEGER target now binds on the dynamic path, and carries the same
    // spilling mark the static path uses: `emit_completed_dynamic_path` emits the boundary spill
    // that mark drives. This used to be refused as `LinkRefusal::DynamicFloatToInteger`.
    assert!(cache.bind_dynamic_successor(
        site_cell,
        integer.linear,
        integer.linear,
        integer.mode_key,
        u32::MAX,
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
        matching_top.mode_key,
        u32::MAX,
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
    assert!(cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key,
        u32::MAX
    ));
    assert!(cell.linked());

    cache.invalidate_translation();
    assert!(!cell.linked());
    assert_eq!(cell.target_eip.load(Ordering::Acquire), target.linear);
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key,
        u32::MAX,
    ));

    cache
        .revalidate_translation(source)
        .expect("source revalidation");
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key,
        u32::MAX,
    ));
    cache
        .revalidate_translation(target)
        .expect("target revalidation");
    assert!(cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key,
        u32::MAX
    ));
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
    assert_eq!(
        cache.stats.arena_compaction_ns, 0,
        "nothing may charge compaction wall before a compaction runs"
    );

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
    // The timer must actually be wired to the body, not merely declared. A rebuild allocates a
    // whole fresh arena and VirtualProtects it, so it cannot land inside one clock tick.
    assert!(
        stats.arena_compaction_ns > 0,
        "arena_compaction_ns stayed 0 across a real compaction"
    );
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
    // The two spans OVERLAP at 0x20_024..0x20_027 rather than merely sitting in one 16-byte
    // granule, so the shared-ownership property holds at any granularity. The earlier form used
    // adjacent 8-byte blocks at 0x20_020 and 0x20_028 and depended on both landing in the same
    // 16-byte chunk, which stopped being true when granules shrank to 4 bytes.
    let first = BlockKey::new(0x1000, 0x20_020, 7);
    let second = BlockKey::new(0x2000, 0x20_024, 7);
    install_trivial(&mut cache, first, 8);
    install_trivial(&mut cache, second, 8);
    assert!(cache.range_hits_compiled_code(0x20_020, 16));

    assert_eq!(cache.retire_physical_range_for_test(first.physical, 1), 1);
    assert!(
        cache.range_hits_compiled_code(0x20_024, 1),
        "the overlapping block still owns the shared watch granule"
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
    assert_eq!(cache.page_key_count_for_test(0x40_010), 1);

    // Seen has no PageKeys row. Only the Rejected span dies. The leftover Seen stays Compile,
    // which is the production-shaped statement of window-shrink §4.1: isolated and overlapping
    // points survive, decode is an independent door, and kill-count 2 would mean point rows
    // were restored.
    assert_eq!(cache.retire_physical_range_for_test(0x40_010, 1), 1);
    assert_eq!(cache.page_key_count_for_test(0x40_010), 0);
    assert!(matches!(cache.probe(seen), BlockProbe::Compile));
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    assert!(matches!(cache.probe(adjacent), BlockProbe::Compile));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn physical_invalidation_window_skips_far_keys_and_reaches_span_tails() {
    let mut cache = BlockCache::default();
    let low = BlockKey::new(0x1000, 0x70_010, 7);
    let high = BlockKey::new(0x2000, 0x70_800, 7);
    install_trivial(&mut cache, low, 16);
    install_trivial(&mut cache, high, 16);

    // A write between the two blocks roots no span within the window bound:
    // the sorted page index must examine zero keys and kill nothing.
    let result = cache.invalidate_physical_range(0x70_400, 4, false);
    assert_eq!(result.blocks, 0);
    assert_eq!(
        result.keys_scanned, 0,
        "far keys must stay outside the window"
    );
    assert!(matches!(cache.probe(low), BlockProbe::Ready(_)));
    assert!(matches!(cache.probe(high), BlockProbe::Ready(_)));

    // A write on the LAST byte of a span whose key roots 15 bytes lower is only
    // reachable through the max_span widening of the window's lower bound.
    let result = cache.invalidate_physical_range(0x70_01f, 1, false);
    assert_eq!(result.blocks, 1, "the span tail must still be reached");
    assert!(matches!(cache.probe(low), BlockProbe::Interpret));
    assert!(matches!(cache.probe(high), BlockProbe::Ready(_)));
}

/// Non-vacuity guard for the stage-A SMC census. Every assert here would still pass against a
/// census that counted nothing, EXCEPT the four that demand non-zero rows and the one that demands
/// the window filter exclude a call — which is the point (see the "fixtures that cannot fail"
/// rule: prove the new guard fires).
#[cfg(all(
    feature = "smc-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn smc_census_units_close_and_its_window_filter_is_not_vacuous() {
    let mut cache = BlockCache::default();
    assert!(
        cache.smc_census_snapshot().is_none(),
        "an unarmed census must not exist"
    );
    cache.enable_smc_census_for_test(Some((100, 200)));
    let low = BlockKey::new(0x1000, 0x70_010, 7);
    let high = BlockKey::new(0x2000, 0x70_800, 7);
    install_trivial(&mut cache, low, 16);
    install_trivial(&mut cache, high, 16);

    // Outside the pinned window: the whole-run phase counts it, the windowed phase must not.
    cache.smc_census_set_clock(10);
    assert_eq!(
        cache.invalidate_physical_range(0x70_400, 4, false).blocks,
        0
    );
    // Inside it, and killing.
    cache.smc_census_set_clock(150);
    assert_eq!(
        cache.invalidate_physical_range(0x70_01f, 1, false).blocks,
        1
    );

    let snapshot = cache.smc_census_snapshot().expect("the census is armed");
    let whole = snapshot.whole_run.units;
    let windowed = snapshot.windowed.units;
    assert_eq!(snapshot.window, Some((100, 200)));
    assert_eq!(whole.scan_calls, 2);
    assert_eq!(
        windowed.scan_calls, 1,
        "the window filter must have excluded the first call"
    );
    assert_eq!(whole.scan_calls_no_kill, 1);
    assert_eq!(whole.scan_calls_kill, 1);
    assert_eq!(whole.keys_killed, 1);
    assert_eq!(whole.retire_calls_effective, 1);
    assert_eq!(whole.waiting_retain_calls, 2 * whole.unlink_calls_effective);

    // The closure set the profile JSON asserts, checked here so a break shows up in `cargo test`
    // rather than only after a multi-minute fixture run.
    assert_eq!(
        whole.keys_scanned,
        whole.probes_elided
            + whole.entries_get_misses
            + whole.keys_surviving
            + whole.lane_accept_keys
            + whole.keys_killed
    );
    // The pre-filter's own split of the window, and the probe population it leaves behind.
    assert_eq!(
        whole.keys_scanned,
        whole.probes_elided + whole.entries_get_calls
    );
    assert_eq!(
        whole.entries_get_calls,
        whole.keys_killed
            + whole.keys_surviving
            + whole.lane_accept_keys
            + whole.entries_get_misses
    );
    assert_eq!(
        whole.probe_divergences, 0,
        "the inline pre-filter skipped a row the authoritative test would have killed"
    );
    assert_eq!(
        whole.point_rows_scanned, 0,
        "a Seen/Dormant key still has a PageKeys row"
    );
    assert_eq!(whole.keys_scanned, whole.window_len_sum);
    assert_eq!(whole.page_visits, whole.page_removes + whole.page_absent);
    assert_eq!(
        whole.page_removes,
        whole.page_reinserts + whole.page_dropped_empty
    );
    assert_eq!(whole.window_searches, whole.page_removes);
    assert_eq!(
        whole.survivors_moved,
        whole.keys_surviving + whole.lane_accept_keys + whole.probes_elided
    );
    // Page occupancy is NOT the window length, which is the whole reason `page_keys_len_sum`
    // exists (design §12.6). Two keys sit on the page; the two windows saw zero and one.
    assert_eq!(whole.page_keys_len_sum, 4);
    assert_eq!(whole.window_len_sum, 1);

    let top = &snapshot.whole_run.pages[0];
    assert_eq!(top.page, 0x70);
    assert_eq!(top.counts.keys_killed, 1);
    assert_eq!(
        top.error.keys_killed, 0,
        "a table with free slots displaces nothing, so its bound is exact"
    );
    assert_eq!(snapshot.whole_run.page_displacements, 0);
    assert_eq!(snapshot.whole_run.page_totals.keys_killed, 1);
    // Two page visits, one of which killed nothing. Pinned to the exact value, not to a range: a
    // never-incremented `no_kill_visits` reads zero and every range assert would have passed.
    assert_eq!(snapshot.whole_run.page_totals.no_kill_visits, 1);
    assert_eq!(snapshot.whole_run.page_totals.page_visits, 2);
    // ...and the ROW says zero, which is correct and is the trap this assert exists to pin. The
    // Space-Saving stream is kill events, so page 0x70 was not admitted until the visit that
    // killed; its context columns count only visits made WHILE RESIDENT. Only `page_totals` is
    // complete. Never compute a per-page rate from a row's context columns.
    assert_eq!(top.counts.no_kill_visits, 0);
    assert_eq!(top.counts.page_visits, 1);
    // The windowed phase saw only the killing visit, so there both agree.
    assert_eq!(snapshot.windowed.page_totals.no_kill_visits, 0);
    assert_eq!(snapshot.windowed.page_totals.page_visits, 1);
    assert!(
        cache.clone().smc_census_snapshot().is_none(),
        "a lockstep clone must not double-count its parent"
    );
}

/// Window-shrink non-vacuity. C1's elision fixture put two Seen points in the widened window
/// and pinned `probes_elided == 2`. After the bijection those points have no row: the window
/// is the compiled span alone, occupancy is one, and the parked keys still probe Compile.
#[cfg(all(
    feature = "smc-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn inline_span_prefilter_elides_point_keys_the_write_cannot_reach() {
    let mut cache = BlockCache::default();
    cache.enable_smc_census_for_test(None);
    let parked_low = BlockKey::new(0x1012, 0x80_012, 7);
    let parked_high = BlockKey::new(0x1014, 0x80_014, 7);
    assert!(matches!(cache.probe(parked_low), BlockProbe::Interpret));
    assert!(matches!(cache.probe(parked_high), BlockProbe::Interpret));
    let compiled = BlockKey::new(0x2020, 0x80_020, 7);
    install_trivial(&mut cache, compiled, 16);
    assert_eq!(cache.page_key_count_for_test(0x80_020), 1);

    let result = cache.invalidate_physical_range(0x80_020, 1, false);
    assert_eq!(result.blocks, 1, "the compiled block must still die");
    assert_eq!(
        result.keys_scanned, 1,
        "point keys must not occupy the window"
    );
    assert!(matches!(cache.probe(parked_low), BlockProbe::Compile));
    assert!(matches!(cache.probe(parked_high), BlockProbe::Compile));

    let whole = cache
        .smc_census_snapshot()
        .expect("the census is armed")
        .whole_run
        .units;
    assert_eq!(whole.keys_scanned, 1);
    assert_eq!(whole.probes_elided, 0);
    assert_eq!(
        whole.entries_get_calls, 1,
        "only the overlapping span row may reach the hash map"
    );
    assert_eq!(whole.keys_killed, 1);
    assert_eq!(whole.keys_surviving, 0);
    assert_eq!(whole.entries_get_misses, 0);
    assert_eq!(whole.probe_divergences, 0);
    assert_eq!(whole.point_rows_scanned, 0);
    assert_eq!(
        whole.keys_scanned,
        whole.probes_elided
            + whole.entries_get_misses
            + whole.keys_surviving
            + whole.lane_accept_keys
            + whole.keys_killed
    );
    assert_eq!(
        whole.keys_scanned,
        whole.probes_elided + whole.entries_get_calls
    );
    assert_eq!(
        whole.entries_get_calls,
        whole.keys_killed
            + whole.keys_surviving
            + whole.lane_accept_keys
            + whole.entries_get_misses
    );
    assert_eq!(
        whole.survivors_moved,
        whole.keys_surviving + whole.lane_accept_keys + whole.probes_elided
    );
}

/// PROVE THE GATE GOES RED. `probe_divergences == 0` and the pre-filter's `debug_assert!` read the
/// same predicate — "a skipped row the authoritative per-state test would have killed" — and a
/// predicate that cannot fail is not an instrument. Hand-corrupt one `lens` element so the filter
/// under-bounds a live 16-byte block to a point, then write inside the span it no longer claims.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "the inline pre-filter skipped a row")]
fn a_corrupted_row_coverage_trips_the_prefilter_divergence_gate() {
    let mut cache = BlockCache::default();
    let compiled = BlockKey::new(0x2020, 0x80_020, 7);
    install_trivial(&mut cache, compiled, 16);
    cache.corrupt_page_len_for_test(compiled, 1);
    // `max_span` is untouched, so the row is still IN the window; only its own coverage lies.
    let _ = cache.invalidate_physical_range(0x80_028, 1, false);
}

/// An armed census with NO pinned window has no windowed phase, and events that arrive before the
/// write choke has ever stashed a clock must not invent one. `set_clock` is reachable only from
/// the choke, so the initial `in_window` is the only thing standing between a pre-choke retire and
/// a phase the run never asked for.
#[cfg(all(
    feature = "smc-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn smc_census_without_a_window_leaves_its_windowed_phase_empty() {
    let mut cache = BlockCache::default();
    cache.enable_smc_census_for_test(None);
    let key = BlockKey::new(0x1000, 0x90_010, 7);
    install_trivial(&mut cache, key, 16);
    // Deliberately NO `smc_census_set_clock` before this: that is the pre-choke case.
    assert_eq!(
        cache.invalidate_physical_range(0x90_010, 4, false).blocks,
        1
    );

    let snapshot = cache.smc_census_snapshot().expect("the census is armed");
    assert_eq!(snapshot.window, None);
    assert_eq!(snapshot.whole_run.units.keys_killed, 1);
    assert_eq!(snapshot.whole_run.units.retire_calls_effective, 1);
    assert_eq!(
        snapshot.windowed.units,
        Default::default(),
        "an unpinned run must report no windowed phase at all"
    );
    assert!(snapshot.windowed.pages.is_empty());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn physical_invalidation_window_survives_a_middle_kill_in_sorted_order() {
    let mut cache = BlockCache::default();
    let first = BlockKey::new(0x1000, 0x80_010, 7);
    let middle = BlockKey::new(0x2000, 0x80_040, 7);
    let last = BlockKey::new(0x3000, 0x80_080, 7);
    install_trivial(&mut cache, first, 16);
    install_trivial(&mut cache, middle, 16);
    install_trivial(&mut cache, last, 16);

    // Killing the middle span must close the hole without disturbing the
    // sorted order the later windows depend on.
    assert_eq!(cache.retire_physical_range_for_test(0x80_040, 1), 1);
    assert!(matches!(cache.probe(first), BlockProbe::Ready(_)));
    assert!(matches!(cache.probe(middle), BlockProbe::Interpret));
    assert!(matches!(cache.probe(last), BlockProbe::Ready(_)));
    assert_eq!(cache.retire_physical_range_for_test(0x80_080, 1), 1);
    assert_eq!(cache.retire_physical_range_for_test(0x80_010, 1), 1);
    assert_eq!(cache.retire_physical_range_for_test(0x80_010, 0x100), 0);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn physical_invalidation_checks_both_pages_of_a_cross_page_write() {
    let mut cache = BlockCache::default();
    let low = BlockKey::new(0x1000, 0x4fff, 7);
    let high = BlockKey::new(0x2000, 0x5000, 7);
    install_trivial(&mut cache, low, 1);
    install_trivial(&mut cache, high, 1);

    assert_eq!(cache.retire_physical_range_for_test(0x4fff, 2), 2);
    assert!(matches!(cache.probe(low), BlockProbe::Interpret));
    assert!(matches!(cache.probe(high), BlockProbe::Interpret));
}

#[cfg(all(
    feature = "smc-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn retire_key_for_recompile_drops_the_span_row() {
    let mut cache = BlockCache::default();
    cache.enable_smc_census_for_test(None);
    let key = BlockKey::new(0x1000, 0x60_010, 7);
    install_trivial(&mut cache, key, 16);
    assert_eq!(cache.page_key_count_for_test(0x60_010), 1);
    assert!(cache.retire_key_for_recompile(key));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    assert_eq!(cache.page_key_count_for_test(0x60_010), 0);

    let result = cache.invalidate_physical_range(0x60_010, 16, false);
    assert_eq!(result.blocks, 0);
    assert_eq!(result.keys_scanned, 0);
    let whole = cache
        .smc_census_snapshot()
        .expect("the census is armed")
        .whole_run
        .units;
    assert_eq!(whole.page_absent, 1);
    assert_eq!(whole.page_keys_len_sum, 0);
    assert_eq!(whole.point_rows_scanned, 0);
}

#[test]
fn overlapping_write_leaves_a_dormant_key_parked() {
    let mut cache = BlockCache::default();
    let parked = BlockKey::new(0x1000, 0x40_010, 7);
    let neighbor = BlockKey::new(0x2000, 0x40_010, 9);
    cache.park_dormant_for_test(parked, DormantReason::SpanHot, None);
    assert!(matches!(cache.probe(neighbor), BlockProbe::Interpret));
    assert!(matches!(cache.probe(neighbor), BlockProbe::Compile));
    reject(&mut cache, neighbor, 16);
    assert!(cache.is_dormant_for_test(parked));

    assert_eq!(cache.retire_physical_range_for_test(0x40_010, 1), 1);
    assert!(cache.is_dormant_for_test(parked));
    assert!(matches!(cache.probe(parked), BlockProbe::Rejected));
    assert!(matches!(cache.probe(neighbor), BlockProbe::Interpret));
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
    cpu.mark_decode_code_for_test(overlap.physical, 1);
    cpu.mark_block_code_for_test(overlap.physical, 1);
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

/// `DormantLift` is the seam the sticky-decline memo's write site reads, and `StillDormant` is
/// the ONLY shape it may write a memo for. This pins why, which is the part a CPU-level fixture
/// cannot show: the loop programs those fixtures drive carry no SMC heat, so `Lifted` never
/// arises in them and a write site widened to `!= NotDormant` would pass the whole battery.
///
/// A memo written after a `Lifted` would be flatly wrong: the key is `Seen` by then, so the full
/// chain's next verdict is `BlockProbe::Compile`, not a decline at all.
#[test]
fn lift_cold_smc_dormant_reports_which_of_its_three_shapes_it_took() {
    let mut cache = BlockCache::default();
    let mut heat = SmcHeatMap::default();

    // Absent, then Seen: not Dormant either way, so there is nothing to memoise.
    let seen = key(0x2000);
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, seen, 0),
        DormantLift::NotDormant
    );
    assert!(matches!(cache.probe(seen), BlockProbe::Interpret));
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, seen, 0),
        DormantLift::NotDormant
    );

    // Dormant with no heat stamp (compile Retry, G4 cover failure): parked, and parked again.
    cache.dormant(
        seen,
        DormantReason::CompileRetry,
        Some(RetryCause::TooShort),
    );
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, seen, 0),
        DormantLift::StillDormant
    );
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, seen, 0),
        DormantLift::StillDormant,
        "a failing lift must not mutate, or the memo's zero-staleness proof loses its footing"
    );

    // Dormant WITH a heat stamp: still parked inside the stamping epoch, lifted in the next one.
    let hot = key(0x3000);
    assert!(matches!(cache.probe(hot), BlockProbe::Interpret));
    cache.demote_smc_hot(&mut heat, hot, 7);
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, hot, 7),
        DormantLift::StillDormant,
        "inside the stamping epoch the lift provably cannot fire; that is what the memo replays"
    );
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, hot, 8),
        DormantLift::Lifted,
        "a later epoch ages the stamp out and the key returns to Seen"
    );
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, hot, 8),
        DormantLift::NotDormant,
        "and the key is Seen now — which is exactly why no memo may be written for a Lifted"
    );
}

/// A lane-trial cache with both arms of the mechanism STATED rather than inherited: the trial
/// knob forced on, and the budget forced to `budget`. Neither fixture below may lean on a default.
fn lane_trial_cache(budget: u32) -> JitState {
    let mut cache = BlockCache::default();
    cache.set_lane_trial_for_test(true);
    set_lane_trial_budget_for_test(Some(budget));
    cache
}

/// INV-B1 and INV-B2. The budget is a HARD per-`(key, epoch)` count, and the ONLY thing that
/// restores it is the epoch turning over — not a probe, not a kill, not a demote. A per-probe reset
/// would make the budget unbounded per epoch, which IS the recompile storm the G1 gate exists to
/// prevent.
///
/// RED before this slice: `lane_trial_spend` granted once per key per epoch unconditionally, so the
/// second grant here was refused.
#[test]
fn lane_trial_budget_is_exactly_the_configured_number_per_epoch() {
    let mut cache = lane_trial_cache(2);
    let hot = key(0x4000);

    assert!(
        cache.lane_trial_spend(hot, 5),
        "the first ask of an epoch is always granted, exactly as before this slice"
    );
    cache.note_lane_trial_install(hot);
    assert!(
        cache.lane_trial_spend(hot, 5),
        "an EARNED second grant is what budget 2 buys"
    );
    cache.note_lane_trial_install(hot);
    assert!(
        !cache.lane_trial_spend(hot, 5),
        "the budget is a hard count: a third grant at budget 2 is refused however well it earned"
    );
    assert_eq!(cache.stalls.lane_trial_budget_refusals, 1);
    assert_eq!(cache.lane_trial_record_for_test(hot), Some((5, 2, true)));
    assert_eq!(
        cache.stalls.lane_trial_installs, 2,
        "a re-grant's install is ONE install; the counter keeps its historical meaning so its \
         series stays comparable across the flip"
    );

    // The epoch turning over is the only reset, and it restarts `spent` rather than decrementing.
    assert!(cache.lane_trial_spend(hot, 6));
    assert_eq!(
        cache.lane_trial_record_for_test(hot),
        Some((6, 1, false)),
        "a new epoch RESTARTS the record; it does not decrement or age the old one"
    );
    assert_eq!(cache.stalls.lane_trial_first_grants, 2);
    assert_eq!(cache.stalls.lane_trials, 3);
    set_lane_trial_budget_for_test(None);
}

/// INV-B3, the nascar guard, in both halves. 86.1% of nascar's trials never install: its parked
/// keys are overwhelmingly compilations that registered no lane at all, and re-granting there would
/// recompile identical code to fail identically — a recompile storm with extra steps. The earned
/// gate is what makes a raised budget worth nothing to that population.
///
/// The SECOND half is what pins the per-GRANT clear rather than a sticky `earned`: without it, one
/// earned install would let a key spend grants 2, 3 and 4 with none of them installing, and the
/// mutation "drop the `slot.earned &&` conjunct" would be unkillable because the conjunct is a
/// constant after the first earn.
#[test]
fn an_unearned_trial_gets_no_regrant_even_under_a_raised_budget() {
    let mut cache = lane_trial_cache(3);

    // Half one: a trial that never installed buys nothing, budget or no budget.
    let never = key(0x5000);
    assert!(cache.lane_trial_spend(never, 9));
    assert!(
        !cache.lane_trial_spend(never, 9),
        "an unearned key gets exactly one trial per epoch, unchanged from before this slice"
    );

    // Half two: earning ONE install buys exactly ONE re-grant, not the rest of the budget.
    let earner = key(0x6000);
    assert!(cache.lane_trial_spend(earner, 9));
    cache.note_lane_trial_install(earner);
    assert!(cache.lane_trial_spend(earner, 9), "the earned re-grant");
    assert!(
        !cache.lane_trial_spend(earner, 9),
        "the re-grant did NOT install, so probation is back on even with budget left"
    );
    assert_eq!(
        cache.lane_trial_record_for_test(earner),
        Some((9, 2, false))
    );
    assert_eq!(cache.stalls.lane_trial_regrants, 1);
    set_lane_trial_budget_for_test(None);
}

/// INV-B5. All three of `lane_trials`, `lane_trial_first_grants` and `lane_trial_regrants` are REAL
/// counters, and this asserts the identity between them. It is a real test only because
/// `first_grants` is counted rather than derived as `lane_trials - regrants`: under the derived
/// form the identity holds by construction and the fixture tests nothing.
#[test]
fn regrants_are_counted_apart_from_first_trials() {
    let mut cache = lane_trial_cache(3);
    let hot = key(0x7000);

    assert!(cache.lane_trial_spend(hot, 2));
    assert_eq!(
        (
            cache.stalls.lane_trial_first_grants,
            cache.stalls.lane_trial_regrants
        ),
        (1, 0),
        "a first grant moves first_grants and not regrants"
    );

    cache.note_lane_trial_install(hot);
    assert!(cache.lane_trial_spend(hot, 2));
    assert_eq!(
        (
            cache.stalls.lane_trial_first_grants,
            cache.stalls.lane_trial_regrants
        ),
        (1, 1),
        "a re-grant moves the reverse"
    );

    assert!(!cache.lane_trial_spend(hot, 2));
    assert_eq!(
        (
            cache.stalls.lane_trial_first_grants,
            cache.stalls.lane_trial_regrants
        ),
        (1, 1),
        "a REFUSED grant moves neither"
    );
    assert_eq!(
        cache.stalls.lane_trials,
        cache.stalls.lane_trial_first_grants + cache.stalls.lane_trial_regrants
    );
    // INV-B1's checkable form, the exact stop floor the ladder reads on every arm.
    assert!(
        cache.stalls.lane_trial_regrants
            <= u64::from(lane_trial_budget() - 1) * cache.stalls.lane_trial_first_grants
    );
    set_lane_trial_budget_for_test(None);
}

/// INV-B4, across the value-type change AND across Task 0's park-epoch side map. A census map that
/// outlives its keys is a stale-key bug in the diagnostic that decides the campaign.
///
/// Host-gated with the other `reset_storage` fixtures: only a NON-EMPTY clear routes through
/// `reset_storage`, so the block install below is load-bearing and it needs a real emitter.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn lane_trial_records_are_cleared_with_cache_storage() {
    let mut cache = lane_trial_cache(2);
    let mut heat = SmcHeatMap::default();
    let hot = key(0x8000);
    install_trivial(&mut cache, key(0x8100), 16);

    assert!(cache.lane_trial_spend(hot, 3));
    cache.demote_smc_hot(&mut heat, hot, 3);
    assert_eq!(cache.lane_trial_records_len_for_test(), 1);
    #[cfg(feature = "direct-admission-census")]
    assert_eq!(cache.park_epochs_len_for_test(), 1);

    cache.clear();
    assert_eq!(
        cache.lane_trial_records_len_for_test(),
        0,
        "the trial records are keyed by BlockKey and every key is gone after a storage reset"
    );
    #[cfg(feature = "direct-admission-census")]
    assert_eq!(
        cache.park_epochs_len_for_test(),
        0,
        "and so is the park-length side map, or the census outlives its keys"
    );
    set_lane_trial_budget_for_test(None);
}

/// Task 0's gross/earned split. Without this the two counters could both be the union, and the
/// go/no-go rule they gate would pass on a population Lever B refuses by design: a trial spent on a
/// compile that died at `Retry` / `StructuralReject` / cover / install is never re-granted.
#[test]
fn task0_counters_split_the_spent_population() {
    let mut cache = lane_trial_cache(1);

    // One key whose trial INSTALLED and was then killed — convertible.
    let earner = key(0x9000);
    assert!(cache.lane_trial_spend(earner, 4));
    cache.note_lane_trial_install(earner);
    assert!(!cache.lane_trial_spend(earner, 4));
    cache.note_heat_demote_trial_spent(earner, 4);

    // One key whose trial's compile FAILED — spent, unconvertible.
    let failed = key(0xa000);
    assert!(cache.lane_trial_spend(failed, 4));
    assert!(!cache.lane_trial_spend(failed, 4));
    cache.note_heat_demote_trial_spent(failed, 4);

    // And one that never asked this epoch: a first-ask or knob-off refusal counts nothing.
    cache.note_heat_demote_trial_spent(key(0xb000), 4);
    cache.note_heat_demote_trial_spent(earner, 5);

    assert_eq!(cache.stalls.heat_demote_trial_spent, 2);
    assert_eq!(cache.stalls.heat_demote_trial_spent_earned, 1);
    set_lane_trial_budget_for_test(None);
}

/// The park-LENGTH diagnostic, in EPOCHS and honest about what it cannot see. The second half is
/// what makes "record an unlifted park as length 0" killable — a mutation that would bias the
/// campaign's steering number toward the answer §1.5 says is most dangerous to get wrong.
#[cfg(feature = "direct-admission-census")]
#[test]
fn park_length_counts_epochs_and_censors_honestly() {
    let mut cache = BlockCache::default();
    let mut heat = SmcHeatMap::default();

    let lifted = key(0xc000);
    assert!(matches!(cache.probe(lifted), BlockProbe::Interpret));
    cache.demote_smc_hot(&mut heat, lifted, 4);
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, lifted, 7),
        DormantLift::Lifted
    );
    assert_eq!(
        cache.stalls.dormant_heat_park_epochs, 3,
        "the span is the EPOCH delta, not a count of parks"
    );
    assert_eq!(cache.stalls.park_lifts, 1);

    // A park that never lifts contributes NOTHING to the sum and one to the censoring count
    // `smc_heat_demotions - park_lifts`. Those are the LONGEST parks, so the mean over the lifted
    // ones is a lower bound and is published with the censoring rate beside it.
    let stuck = key(0xd000);
    assert!(matches!(cache.probe(stuck), BlockProbe::Interpret));
    cache.demote_smc_hot(&mut heat, stuck, 4);
    assert_eq!(
        cache.lift_cold_smc_dormant(&mut heat, stuck, 4),
        DormantLift::StillDormant
    );
    assert_eq!(cache.stalls.dormant_heat_park_epochs, 3);
    assert_eq!(cache.stalls.park_lifts, 1);
    assert_eq!(
        cache.park_epochs_len_for_test(),
        1,
        "still parked, uncounted"
    );
}

/// INV-B7: the stale-epoch reset runs ABOVE the grant decision, never below it. This is the ENTIRE
/// defence against cross-epoch `earned` leakage — with the reset below the decision, a key that
/// earned an install at epoch E arrives at E+1 and is granted as an earned RE-grant rather than as
/// a first trial, silently doubling the per-epoch budget for every key that ever earns.
#[test]
fn an_earn_at_epoch_e_does_not_buy_a_regrant_at_epoch_e_plus_1() {
    let mut cache = lane_trial_cache(3);
    let hot = key(0xe000);

    assert!(cache.lane_trial_spend(hot, 11));
    cache.note_lane_trial_install(hot);
    assert_eq!(cache.lane_trial_record_for_test(hot), Some((11, 1, true)));

    assert!(cache.lane_trial_spend(hot, 12));
    assert_eq!(
        cache.lane_trial_record_for_test(hot),
        Some((12, 1, false)),
        "the earn from epoch 11 is cleared by the reset before the decision can read it"
    );
    assert_eq!(
        (
            cache.stalls.lane_trial_first_grants,
            cache.stalls.lane_trial_regrants
        ),
        (2, 0),
        "the grant at E+1 is a FIRST trial, not an earned re-grant"
    );
    set_lane_trial_budget_for_test(None);
}

/// R2-G's other seam. `note_lane_trial_install` on a key with no slot is a SILENT no-op, never an
/// `expect` and never an insert. A slot always exists in production (`lane_trial` implies a prior
/// grant), so an `expect` would be "correct" today and would turn a diagnostic into a crash the
/// first time a `reset_storage` landed between the grant and the install.
#[test]
fn note_lane_trial_install_on_a_missing_slot_is_a_silent_no_op() {
    let mut cache = BlockCache::default();
    cache.note_lane_trial_install(key(0xf000));
    assert_eq!(cache.stalls.lane_trial_installs, 1);
    assert_eq!(
        cache.lane_trial_records_len_for_test(),
        0,
        "no slot is fabricated: a fabricated one would hold epoch 0 and could be read as earned"
    );
}

/// The knob's spelling table. A PARAMETER has no `0` or `off` spelling — `1` is how the pre-slice
/// arm is named — and a typo must PANIC rather than fall through to the default, which a ladder leg
/// would read as "the arm I asked for changed nothing".
///
/// The unset row MOVED with the 2026-08-29 flip (1 -> 4). It is asserted against the constant and
/// not against a literal `4`, so the default and its ceiling cannot drift apart silently.
#[test]
fn lane_trial_budget_spelling_table() {
    use std::env::VarError;

    assert_eq!(
        parse_lane_trial_budget_arm_for_test(Err(VarError::NotPresent)),
        MAX_LANE_TRIAL_BUDGET,
        "unset is the shipped default, which the ladder moved to the ceiling"
    );
    for (spelling, budget) in [("1", 1), ("2", 2), (" 3 ", 3), ("4", 4)] {
        assert_eq!(
            parse_lane_trial_budget_arm_for_test(Ok(spelling.to_string())),
            budget,
            "{spelling:?}"
        );
    }
    for typo in ["", "0", "off", "5", "yes", "true", "-1", "2.0"] {
        assert!(
            std::panic::catch_unwind(|| parse_lane_trial_budget_arm_for_test(Ok(typo.to_string())))
                .is_err(),
            "{typo:?} names no arm and must panic rather than run the default"
        );
    }
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

    // The other half of G1, retargeted at byte granularity. There USED to be a "data byte sharing
    // a watched granule with cold code but outside the block's own span" scenario, and this test
    // pinned that such a byte was admitted to the scan and still heated nothing. At one-byte
    // granules that scenario no longer exists: a watched granule IS a code byte, so there is no
    // unwatched byte inside one to write.
    //
    // The analogous property at this granularity is the stronger one the granule change bought:
    // a write to a byte the block does not cover is never ADMITTED at all. The scan is not
    // entered (smc_scan_calls does not move), nothing heats, and the block survives Ready. If the
    // guard's granularity ever widens again this assertion is the one that fails first.
    let mut cold = CpuGsw::default();
    let data_key = BlockKey::new(0x2000, 0x61_000, 7);
    install_trivial(&mut cold.jit_direct, data_key, 2); // watches 0x61_000..0x61_001
    let scans_before = cold.perf.smc_scan_calls;
    for _ in 0..8 {
        // Outside the span, and at byte granularity therefore outside the watch.
        cold.note_code_write(0x61_002, 1);
    }
    assert_eq!(
        cold.perf.smc_scan_calls, scans_before,
        "a write outside the watched bytes must never reach the invalidation scan"
    );
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

    // LOCK, REP/REPNE and the address-size override are still unsupported in both widths.
    for (name, other) in [
        (
            "lock",
            Prefixes {
                lock: true,
                ..Prefixes::default()
            },
        ),
        (
            "address size",
            Prefixes {
                address_size_override: true,
                ..Prefixes::default()
            },
        ),
        (
            "rep",
            Prefixes {
                rep: Some(crate::RepKind::Repe),
                ..Prefixes::default()
            },
        ),
        (
            "repne",
            Prefixes {
                rep: Some(crate::RepKind::Repne),
                ..Prefixes::default()
            },
        ),
    ] {
        for d in [false, true] {
            for size in [OperandSize::Word, OperandSize::Dword] {
                let mut prefixes = other;
                prefixes.operand_size_override = (size == OperandSize::Dword) != d;
                assert!(
                    !prefixes_supported_for(prefixes, size, d),
                    "{name} must stay unsupported at d={d}"
                );
            }
        }
    }
}

/// The slice-6 admission: an explicit override naming one of the five DATA segments passes the
/// gate at both operand sizes and in both code widths; a CS override does not.
///
/// The CS half is the load-bearing one. Refusing CS is a DECISION (12,674 doom exits stay a
/// barrier), taken because CS is the only segment `SegmentLayout` homes twice — in its own `cs`
/// field and at index 1 of `data` — so it is the only one where admitting a memory kind would make
/// a lowered access depend on the two homes agreeing. A future edit that "tidied" the gate into
/// admitting every `Option<SegmentIndex>` would silently take that decision back, and only this
/// assertion would notice.
#[test]
fn the_prefix_gate_admits_the_five_data_segment_overrides_and_refuses_cs() {
    for d in [false, true] {
        for size in [OperandSize::Word, OperandSize::Dword] {
            let expected_override = (size == OperandSize::Dword) != d;
            for segment in [
                SegmentIndex::Es,
                SegmentIndex::Ss,
                SegmentIndex::Ds,
                SegmentIndex::Fs,
                SegmentIndex::Gs,
            ] {
                assert!(
                    prefixes_supported_for(
                        Prefixes {
                            operand_size_override: expected_override,
                            segment_override: Some(segment),
                            ..Prefixes::default()
                        },
                        size,
                        d,
                    ),
                    "{segment:?} override must be admitted at d={d} size={size:?}"
                );
                // The operand-size clause still has to agree. Admitting the segment override must
                // not turn the gate into "any prefix set containing one".
                assert!(
                    !prefixes_supported_for(
                        Prefixes {
                            operand_size_override: !expected_override,
                            segment_override: Some(segment),
                            ..Prefixes::default()
                        },
                        size,
                        d,
                    ),
                    "{segment:?} override with the wrong operand-size override must refuse"
                );
                // ... and so does every other prefix, alongside the admitted override.
                assert!(
                    !prefixes_supported_for(
                        Prefixes {
                            operand_size_override: expected_override,
                            segment_override: Some(segment),
                            lock: true,
                            ..Prefixes::default()
                        },
                        size,
                        d,
                    ),
                    "{segment:?} override plus LOCK must refuse"
                );
            }
            // CS follows `IZARRAVM_V86_LOOP_ROWS` as of 2026-08-20 and is the ONLY clause of this
            // gate that does. Both arms are asserted here, in the same loop as the five data
            // segments, so a future edit that gated the whole segment admission behind that knob
            // instead of only its CS clause fails on the rows above rather than passing quietly.
            for arm in [false, true] {
                set_v86_loop_rows_for_test(Some(arm));
                assert_eq!(
                    prefixes_supported_for(
                        Prefixes {
                            operand_size_override: expected_override,
                            segment_override: Some(SegmentIndex::Cs),
                            ..Prefixes::default()
                        },
                        size,
                        d,
                    ),
                    arm,
                    "a CS override is refused explicitly on the off arm and admitted on the on \
                     arm; it is never refused by omission"
                );
            }
            set_v86_loop_rows_for_test(None);
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
    // The Word/Word `Loop` cell rides the same predicate as `Call16`: `counter_word` decides the
    // decrement width in the emitter, but the TARGET is still `taken_delta`, checked against the
    // identical clamp. Without this row the wrap guard could regress for LOOP alone and every
    // assertion above would keep passing.
    assert!(!static_control_target_within_limit(
        DirectKind::Loop {
            taken_delta: 0x40,
            counter_word: true,
        },
        0x1_0100,
        word
    ));
    assert!(static_control_target_within_limit(
        DirectKind::Loop {
            taken_delta: 0x40,
            counter_word: true,
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
        let lowered =
            direct_addr(word_addr(base, index, segment, -2)).expect("every 16-bit mode must lower");
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
    let disp_only = direct_addr(word_addr(None, None, SegmentIndex::Ds, 0x1234))
        .expect("disp16-only must lower");
    assert_eq!(disp_only.base, None);
    assert_eq!(disp_only.index, None);
    assert_eq!(disp_only.disp, 0x1234);

    // A scale other than 1, 2, 4 or 8 is still refused, at either address size.
    let mut bad_scale = word_addr(Some(3), Some(6), SegmentIndex::Ds, 0);
    bad_scale.scale = 3;
    assert!(direct_addr(bad_scale).is_none());
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
        disp_lane: None,
    };

    let mut unmasked = Encoder::new();
    emit_effective_address(&mut unmasked, addr, AddressWrap::None, 0);
    let unmasked = unmasked.finish();

    let mut masked = Encoder::new();
    emit_effective_address(&mut masked, addr, AddressWrap::Word, 0);
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

#[test]
fn direct_exit_and_link_cell_layouts_are_pinned() {
    // L8 grew `NativeExit` by one `u64` (`far_call_native`), placed beside the other `u64`
    // fields rather than after the trailing `u32`s so the struct grows by exactly 8 bytes and
    // not 16 -- see that field's own doc. Every offset from `side_exit_reason` onward moves by
    // that same 8.
    assert_eq!(core::mem::size_of::<NativeExit>(), 152);
    assert_eq!(core::mem::align_of::<NativeExit>(), 8);
    assert_eq!(core::mem::offset_of!(NativeExit, unresolved_reason), 124);
    assert_eq!(core::mem::offset_of!(NativeExit, dynamic_target_eip), 144);
    #[cfg(feature = "direct-link-refusal-census")]
    {
        assert_eq!(
            core::mem::offset_of!(NativeExit, direct_link_refusal_census_id),
            148
        );
        assert_eq!(core::mem::size_of::<LinkCell>(), 24);
        assert_eq!(core::mem::align_of::<LinkCell>(), 8);
        assert_eq!(core::mem::offset_of!(LinkCell, portal), 0);
        assert_eq!(core::mem::offset_of!(LinkCell, target_eip), 8);
        assert_eq!(
            core::mem::offset_of!(LinkCell, direct_link_refusal_census_id),
            12
        );
        assert_eq!(core::mem::offset_of!(LinkCell, entry_top), 16);
        assert_eq!(core::mem::offset_of!(LinkCell, spilling), 17);
    }
    #[cfg(not(feature = "direct-link-refusal-census"))]
    {
        assert_eq!(core::mem::size_of::<LinkCell>(), 16);
        assert_eq!(core::mem::align_of::<LinkCell>(), 8);
        assert_eq!(core::mem::offset_of!(LinkCell, portal), 0);
        assert_eq!(core::mem::offset_of!(LinkCell, target_eip), 8);
        assert_eq!(core::mem::offset_of!(LinkCell, entry_top), 12);
        assert_eq!(core::mem::offset_of!(LinkCell, spilling), 13);
    }
}

/// Bucket lookup by LABEL, not by index. The bucket layout is derived from `LinkRefusal::COUNT`
/// and `LinkClearCause::COUNT`, so a new cause shifts every index past it and a row that
/// hard-codes 13 silently starts asserting about its neighbour instead.
#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
fn census_bucket(buckets: &[(&'static str, u64)], label: &str) -> u64 {
    buckets
        .iter()
        .find(|(name, _)| *name == label)
        .map(|(_, count)| *count)
        .unwrap_or_else(|| panic!("census bucket {label} must exist"))
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn direct_link_refusal_census_registers_static_cells_and_closes_exactly() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    assert_eq!(
        cache.direct_link_refusal_census_snapshot(),
        Some(crate::DirectLinkRefusalCensusSnapshot::default())
    );

    let source = key(0x1000);
    let fallthrough = LinkTarget {
        linear: 0x1100,
        mode_key: source.mode_key,
    };
    let taken = LinkTarget {
        linear: 0x1200,
        mode_key: source.mode_key,
    };
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut compilation = trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    compilation.successors = [Some(fallthrough), Some(taken)];
    compilation.emitted_static_targets = [Some(fallthrough), Some(taken)];
    cache.install(&compilation).expect("source install");

    let snapshot = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(
        snapshot.rows.len(),
        2,
        "Jcc must register both emitted arms"
    );
    assert_eq!(snapshot.rows[0].id, 1);
    assert_eq!(snapshot.rows[0].slot, 0);
    assert_eq!(snapshot.rows[0].target_linear, fallthrough.linear);
    assert_eq!(snapshot.rows[0].state, "not_attempted");
    assert_eq!(snapshot.rows[1].id, 2);
    assert_eq!(snapshot.rows[1].slot, 1);
    assert_eq!(snapshot.rows[1].target_linear, taken.linear);
    let labels: Vec<&str> = snapshot.rows[0]
        .buckets
        .iter()
        .map(|(label, _)| *label)
        .collect();
    assert_eq!(
        labels,
        [
            "suppressed",
            "not_attempted",
            "refused_inactive",
            "refused_stale_epoch",
            "refused_segment_layout",
            "refused_declined",
            "refused_block_shape",
            "refused_dynamic_integer_to_float",
            "refused_dynamic_float_to_integer",
            "refused_missing_x87_pad",
            "cleared_replaced",
            "cleared_retired",
            "cleared_flushed",
            "cleared_reset",
            "cleared_chain_widen",
            "cleared_data_segment_decline",
            "unexpected_linked",
            "closed",
        ]
    );

    cache.note_direct_link_refusal_exit(1);
    cache.note_direct_link_refusal_exit(2);
    cache.note_direct_link_refusal_exit(0);
    cache.note_direct_link_refusal_exit(u32::MAX);
    cache.clear();
    cache.note_direct_link_refusal_exit(1);
    let snapshot = cache
        .direct_link_refusal_census_snapshot()
        .expect("reset retains census rows");
    assert_eq!(snapshot.seen, 5);
    assert_eq!(snapshot.missing_id, 1);
    assert_eq!(snapshot.invalid_id, 1);
    assert_eq!(snapshot.rows[0].state, "closed");
    assert_eq!(snapshot.rows[0].unbound_exits, 2);
    assert_eq!(snapshot.rows[1].unbound_exits, 1);
    assert_eq!(
        snapshot.seen,
        snapshot.missing_id
            + snapshot.invalid_id
            + snapshot
                .rows
                .iter()
                .map(|row| row.unbound_exits)
                .sum::<u64>()
    );
    for row in &snapshot.rows {
        assert_eq!(
            row.unbound_exits,
            row.buckets.iter().map(|(_, count)| count).sum::<u64>()
        );
    }
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn direct_link_refusal_census_registers_before_the_first_link_attempt() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    let source = key(0x1800);
    let target = key(0x1900);
    let target_id = install_trivial(&mut cache, target, 1);

    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut compilation = trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    compilation.successors = [
        Some(LinkTarget {
            linear: target.linear,
            mode_key: target.mode_key,
        }),
        None,
    ];
    compilation.emitted_static_targets = compilation.successors;
    cache.install(&compilation).expect("source install");

    let snapshot = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.source_linear == source.linear)
        .expect("source row");
    assert_eq!(row.state, "unexpected_linked");
    assert_eq!(row.last_target_generation, Some(target_id.generation()));
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn direct_link_refusal_census_clone_is_unarmed() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    install_trivial(&mut cache, key(0x1a00), 1);
    assert!(cache.direct_link_refusal_census_snapshot().is_some());
    assert!(
        cache
            .clone()
            .direct_link_refusal_census_snapshot()
            .is_none()
    );
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
#[should_panic(expected = "cannot be toggled with installed blocks")]
fn direct_link_refusal_census_cannot_toggle_with_live_blocks() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    install_trivial(&mut cache, key(0x1b00), 1);
    cache.set_direct_link_refusal_census_enabled(false);
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn direct_link_refusal_state_machine_maps_every_bucket() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    let source = key(0x1c00);
    let target = key(0x1d00);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut compilation = trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    compilation.successors = [
        Some(LinkTarget {
            linear: target.linear,
            mode_key: target.mode_key,
        }),
        None,
    ];
    compilation.emitted_static_targets = compilation.successors;
    let source_id = cache.install(&compilation).expect("source install");
    let target_id = install_trivial(&mut cache, target, 1);
    let source_index = source_id.index();

    // The two retired dynamic refusal variants and Reset's immediately closed state have no
    // stable guest route. Pin the same transition functions used by the live sites instead.
    for reason in LinkRefusal::ALL {
        cache.note_direct_link_refused(source_index, 0, reason, target_id);
        cache.note_direct_link_refusal_exit(1);
    }
    for cause in LinkClearCause::ALL {
        cache.note_direct_link_cleared(source_index, 0, cause, target_id);
        cache.note_direct_link_refusal_exit(1);
    }
    cache.note_direct_link_linked(source_index, 0, target_id);
    cache.note_direct_link_refusal_exit(1);

    let snapshot = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    let row = &snapshot.rows[0];
    for (label, count) in &row.buckets {
        let exercised = label.starts_with("refused_")
            || label.starts_with("cleared_")
            || *label == "unexpected_linked";
        assert_eq!(*count, u64::from(exercised), "bucket {label}");
    }
    assert_eq!(
        row.unbound_exits,
        (LinkRefusal::COUNT + LinkClearCause::COUNT + 1) as u64
    );
    assert_eq!(row.last_target_generation, Some(target_id.generation()));
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
#[test]
fn segment_write_static_cell_is_registered_as_suppressed_fallthrough() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    let source = key(0x2000);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut compilation = trivial_compilation(BlockSpan::new(source, 3, 1).expect("source span"));
    compilation.emitted_static_targets = [
        Some(LinkTarget {
            linear: source.linear + 3,
            mode_key: source.mode_key,
        }),
        None,
    ];
    cache.install(&compilation).expect("source install");

    cache.note_direct_link_refusal_exit(1);
    let snapshot = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(snapshot.rows[0].slot, 0);
    assert_eq!(snapshot.rows[0].target_linear, source.linear + 3);
    assert_eq!(snapshot.rows[0].state, "suppressed");
    assert_eq!(snapshot.rows[0].buckets[0], ("suppressed", 1));
}

#[cfg(all(
    feature = "direct-link-refusal-census",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
/// The refusal this row needs is a REAL one. It used to perturb the target's DS descriptor on a
/// `trivial_compilation` whose `used` mask is 0 (direct_test.rs:64), which was a refusal only
/// while `link_compatible` compared all six frozen descriptors unconditionally. Under the chain
/// mask a segment NOBODY in the chain pins is not a reason to refuse, so that edge now links and
/// the row would have been testing generation history through a path it never takes. Both ends
/// therefore PIN DS here — a genuine class-D conflict, refused before and after the mask.
#[test]
fn link_refusal_census_preserves_generation_history_through_retry_and_source_retire() {
    let mut cache = BlockCache::default();
    cache.set_direct_link_refusal_census_enabled(true);
    let source = key(0x3000);
    let target = key(0x3100);
    let target_link = LinkTarget {
        linear: target.linear,
        mode_key: target.mode_key,
    };

    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.segment_layout.used |= segment_bit(SegmentIndex::Ds);
    source_compilation.successors = [Some(target_link), None];
    source_compilation.emitted_static_targets = source_compilation.successors;
    cache.install(&source_compilation).expect("source install");

    assert!(matches!(cache.probe(target), BlockProbe::Interpret));
    assert!(matches!(cache.probe(target), BlockProbe::Compile));
    let mut target_g1 = trivial_compilation(BlockSpan::new(target, 1, 1).expect("target span"));
    target_g1.segment_layout.used |= segment_bit(SegmentIndex::Ds);
    target_g1.segment_layout.data[segment_index(SegmentIndex::Ds)].selector ^= 8;
    let target_g1_id = cache.install(&target_g1).expect("first target install");
    cache.note_direct_link_refusal_exit(1);
    let refused = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(refused.rows[0].state, "refused_segment_layout");
    assert_eq!(
        refused.rows[0].last_target_generation,
        Some(target_g1_id.generation())
    );
    assert_eq!(
        census_bucket(&refused.rows[0].buckets, "refused_segment_layout"),
        1
    );

    assert_eq!(cache.retire_physical_range_for_test(target.physical, 1), 1);
    assert!(matches!(cache.probe(target), BlockProbe::Interpret));
    assert!(matches!(cache.probe(target), BlockProbe::Compile));
    let target_g2 = trivial_compilation(BlockSpan::new(target, 1, 1).expect("target retry span"));
    let target_g2_id = cache.install(&target_g2).expect("second target install");
    assert_ne!(target_g1_id.generation(), target_g2_id.generation());
    cache.note_direct_link_refusal_exit(1);

    let linked = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(linked.rows[0].state, "unexpected_linked");
    assert_eq!(
        linked.rows[0].last_target_generation,
        Some(target_g2_id.generation())
    );
    assert_eq!(
        census_bucket(&linked.rows[0].buckets, "refused_segment_layout"),
        1
    );
    assert_eq!(
        census_bucket(&linked.rows[0].buckets, "unexpected_linked"),
        1
    );

    assert_eq!(cache.retire_physical_range_for_test(source.physical, 1), 1);
    cache.note_direct_link_refusal_exit(1);
    let closed = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(closed.rows[0].state, "closed");
    assert_eq!(
        closed.rows[0].last_target_generation,
        Some(target_g2_id.generation())
    );
    assert_eq!(
        census_bucket(&closed.rows[0].buckets, "refused_segment_layout"),
        1
    );
    assert_eq!(
        census_bucket(&closed.rows[0].buckets, "unexpected_linked"),
        1
    );
    assert_eq!(census_bucket(&closed.rows[0].buckets, "closed"), 1);
    assert_eq!(closed.rows[0].unbound_exits, 3);

    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_retry =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source retry span"));
    source_retry.successors = [Some(target_link), None];
    source_retry.emitted_static_targets = source_retry.successors;
    cache.install(&source_retry).expect("source retry install");
    let retried = cache
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    let source_rows: Vec<_> = retried
        .rows
        .iter()
        .filter(|row| row.source_linear == source.linear)
        .collect();
    assert_eq!(source_rows.len(), 2);
    assert_eq!(source_rows[0].state, "closed");
    assert!(source_rows[1].id > source_rows[0].id);
    assert_eq!(source_rows[1].state, "unexpected_linked");
    assert_eq!(
        source_rows[1].last_target_generation,
        Some(target_g2_id.generation())
    );

    cache.clear();
    let reset = cache
        .direct_link_refusal_census_snapshot()
        .expect("reset retains census");
    let source_rows: Vec<_> = reset
        .rows
        .iter()
        .filter(|row| row.source_linear == source.linear)
        .collect();
    assert_eq!(source_rows[1].state, "closed");
    assert_eq!(
        source_rows[1].last_target_generation,
        Some(target_g2_id.generation())
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
    cache.dormant(heat, DormantReason::SpanHot, None);
    assert_eq!(
        cache.classify_unbound_target(heat),
        UnboundTarget::DormantHeat
    );
    let other = key(0x1200);
    assert!(matches!(cache.probe(other), BlockProbe::Interpret));
    cache.dormant(
        other,
        DormantReason::CompileRetry,
        Some(RetryCause::TooShort),
    );
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

#[cfg(any(
    all(
        feature = "direct-admission-census",
        target_os = "windows",
        target_arch = "x86_64"
    ),
    all(
        feature = "direct-admission-census",
        target_os = "linux",
        target_arch = "x86_64"
    )
))]
#[test]
fn admission_census_rejected_probe_classifier_covers_every_cache_state_and_disabled_cache() {
    let mut cache = BlockCache::default();
    let absent = key(0x1800);
    assert_eq!(cache.classify_rejected_probe(absent), None);

    let seen = key(0x1900);
    assert!(matches!(cache.probe(seen), BlockProbe::Interpret));
    assert_eq!(cache.classify_rejected_probe(seen), None);

    let dormant = key(0x1a00);
    assert!(matches!(cache.probe(dormant), BlockProbe::Interpret));
    cache.dormant(
        dormant,
        DormantReason::CompileRetry,
        Some(RetryCause::TooShort),
    );
    assert_eq!(
        cache.classify_rejected_probe(dormant),
        Some(AdmissionDecline::DormantProbe)
    );

    let rejected = key(0x1b00);
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    cache.reject(RejectedSpan {
        key: rejected,
        guest_len: 1,
    });
    assert_eq!(
        cache.classify_rejected_probe(rejected),
        Some(AdmissionDecline::RejectedProbe)
    );

    let compiled = key(0x1c00);
    install_trivial(&mut cache, compiled, 1);
    assert_eq!(cache.classify_rejected_probe(compiled), None);

    cache.direct.disabled = true;
    assert_eq!(cache.classify_rejected_probe(dormant), None);
    assert_eq!(cache.classify_rejected_probe(rejected), None);
}

/// The six link-clear causes account for every cleared link: their sum equals the aggregate
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

    // ChainWiden: a link made downstream widens a block's chain segment requirement past what an
    // already-linked predecessor can satisfy, and that inbound edge is cut. Fresh blocks for the
    // same reason the reset section below needs them.
    let widen_root = key(0x1600);
    let widen_middle = key(0x1700);
    let widen_tail = key(0x1800);
    let widen_root_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(widen_root, 1, 1).expect("widen root span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x1111)],
        ),
    );
    let widen_middle_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(widen_middle, 1, 1).expect("widen middle span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    let widen_tail_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(widen_tail, 1, 1).expect("widen tail span"),
            &[SegmentIndex::Ds, SegmentIndex::Es],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    assert!(cache.try_link(widen_root_id, 0, widen_middle_id));
    assert!(cache.try_link(widen_middle_id, 0, widen_tail_id));
    assert_eq!(cache.outbound[widen_root_id.index()][0], None);
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::ChainWiden as usize],
        1
    );

    // DataSegmentDecline: the promoting Strict On reject cuts the still-live outbound cell
    // before it retires. Re-installed and re-linked between turns because the first
    // `DATA_SEGMENT_RETIRE_CAP` of them still retire, and a retired key has nothing left to
    // decline. The extra turn is the promote; leaf-ness afterwards rides live_data.
    let declined_source = key(0x1900);
    let declined_target = key(0x1a00);
    let declined_target_id = install_trivial(&mut cache, declined_target, 1);
    let declined_span =
        BlockSpan::new(declined_source, 1, 1).expect("declined source span must be page local");
    let live = [crate::SegmentRegister::real(0); 6];
    super::set_segment_retire_governor_for_test(Some(SegmentRetireGovernor::On));
    for _ in 0..=DATA_SEGMENT_RETIRE_CAP {
        // `install_trivial`'s probe ladder, inline: a fresh key needs Interpret then Compile, and
        // a key just returned to `Seen` by a retire needs one more probe to be offered again.
        for _ in 0..4 {
            if matches!(cache.probe(declined_source), BlockProbe::Compile) {
                break;
            }
        }
        let source_id = cache
            .install(&trivial_compilation(declined_span))
            .expect("the declined source must install");
        assert!(cache.try_link(source_id, 0, declined_target_id));
        cache.retire_key_for_data_segment(
            declined_source,
            DataSegmentRejectArm::Strict,
            true,
            &live,
        );
    }
    super::set_segment_retire_governor_for_test(None);
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::DataSegmentDecline as usize],
        1,
        "the decline cuts exactly the one live outbound cell, under its own cause"
    );

    // All SIX sites: deleting any single increment above must break the sum.
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

#[test]
fn table_slots_are_host_state_not_guest_state() {
    let mut slots = NativeTableSlots::default();
    slots.publish(TABLE_SLOT_FLAGS, 0x1000);
    // Never guest-visible: equality ignores the slots (canonical-state and
    // lockstep comparisons must not diff host pointers), and a clone must not
    // inherit pointers into another CPU's tables.
    assert_eq!(slots, NativeTableSlots::default());
    assert_eq!(
        slots.clone().slots,
        [0; 7 + STORE_STUB_COUNT + 1 + READ_STUB_COUNT]
    );
}

#[test]
fn an_idempotent_republish_is_accepted() {
    let mut slots = NativeTableSlots::default();
    slots.publish(TABLE_SLOT_FLAGS, 0x1000);
    slots.publish(TABLE_SLOT_FLAGS, 0x1000);
    assert_eq!(slots.slots[TABLE_SLOT_FLAGS], 0x1000);
}

/// The write-once invariant's alarm must actually fire — a changed base with
/// live emitted code is a miscompile on the imm64 arm and a desync on the R15
/// arm, and this panic is the only place either becomes visible.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "published table base changed")]
fn a_changed_republish_panics() {
    let mut slots = NativeTableSlots::default();
    slots.publish(TABLE_SLOT_FLAGS, 0x1000);
    slots.publish(TABLE_SLOT_FLAGS, 0x2000);
}

/// Pin the two independent spellings of "naturally aligned" where they MUST agree.
///
/// `BusWidth::misaligned_at` is the single Rust spelling used by the interpreter and the bus.
/// The emitter cannot share it: it works in `MemoryWidth`, which carries `Qword`/`Tbyte`, and
/// emits `and r32, alignment_bytes() - 1` into machine code rather than calling Rust.
///
/// `Qword`/`Tbyte` are DELIBERATELY absent from this test: `alignment_bytes()` is BELOW the
/// access size for those two (both answer 4), because a 4-aligned Qword at page offset 0xFFC
/// still crosses and a Tbyte at 0xFF8 does -- see `emit_wide_page_guard`'s comment. Folding the
/// two predicates would either break that or contaminate the shared one. What the carve-out owes
/// is exactly this: agreement on the overlapping domain, so the two cannot drift where a reader
/// would assume they match.
/// The two `MemoryWidth` vocabulary accessors pinned at EVERY width, including the two the
/// emitter's pads never build.
///
/// This exists because the two are equal for Byte, Word and Dword, which is every width that
/// reaches their call sites today. Any test built only from those three passes with the two
/// method bodies swapped, so it would certify nothing about which name means what. Qword and
/// Tbyte are the only widths where the distinction is observable, and the `assert_ne!` at the end
/// is the line that fails if the two are ever folded back into one.
#[test]
fn memory_width_alignment_mask_and_split_charge_are_distinct_facts() {
    // (name, width, alignment_mask, split_extra_bytes). The name is carried rather than derived:
    // `MemoryWidth` has no `Debug` impl and this test is not a reason to give a hot emitter enum
    // one.
    for (name, width, mask, extra) in [
        ("Byte", MemoryWidth::Byte, 0, 0),
        ("Word", MemoryWidth::Word, 1, 1),
        ("Dword", MemoryWidth::Dword, 3, 3),
        ("Qword", MemoryWidth::Qword, 3, 7),
        ("Tbyte", MemoryWidth::Tbyte, 3, 9),
    ] {
        assert_eq!(width.alignment_mask(), mask, "alignment_mask({name})");
        assert_eq!(
            width.split_extra_bytes(),
            extra,
            "split_extra_bytes({name})"
        );
    }

    // The separation itself, at the two widths that can express it. An alignment mask asks how
    // the address must be shaped; the split charge says how many extra byte cycles a misaligned
    // access owes. They coincide only where the width self-aligns.
    assert_ne!(
        MemoryWidth::Qword.alignment_mask(),
        MemoryWidth::Qword.split_extra_bytes()
    );
    assert_ne!(
        MemoryWidth::Tbyte.alignment_mask(),
        MemoryWidth::Tbyte.split_extra_bytes()
    );

    // The mask is exactly one below the requirement it enforces, at every width.
    for width in [
        MemoryWidth::Byte,
        MemoryWidth::Word,
        MemoryWidth::Dword,
        MemoryWidth::Qword,
        MemoryWidth::Tbyte,
    ] {
        assert_eq!(width.alignment_mask(), width.alignment_bytes() - 1);
    }
}

#[test]
fn memory_width_alignment_matches_bus_width_bytes_where_both_exist() {
    assert_eq!(
        MemoryWidth::Byte.alignment_bytes(),
        izarravm_bus::BusWidth::Byte.bytes()
    );
    assert_eq!(
        MemoryWidth::Word.alignment_bytes(),
        izarravm_bus::BusWidth::Word.bytes()
    );
    assert_eq!(
        MemoryWidth::Dword.alignment_bytes(),
        izarravm_bus::BusWidth::Dword.bytes()
    );

    // And the mask form the emitter encodes is the same mask `misaligned_at` applies.
    for width in [
        izarravm_bus::BusWidth::Byte,
        izarravm_bus::BusWidth::Word,
        izarravm_bus::BusWidth::Dword,
    ] {
        for address in 0u32..8 {
            assert_eq!(
                width.misaligned_at(address),
                address % width.bytes() != 0,
                "misaligned_at({width:?}, {address:#x})"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Chain-used link mask (dev_docs/plans/2026-08-18-chain-used-link-mask.md, NON-ADOPTING slice)
//
// The contract these rows pin: a static edge is refused over a frozen `data` descriptor only when
// some block in the CHAIN — the source's requirement or the target's — actually pins that
// segment. A descriptor nobody pins is not a reason to refuse (class B); a descriptor both ends
// pin with different values still is (class D). Because the requirement is transitive, a link
// made downstream can widen a block's requirement and retroactively invalidate an already-live
// inbound edge, which is then CUT (`LinkClearCause::ChainWiden`).
//
// MUTATION EVIDENCE (2026-08-18, applied by hand, observed, restored). Each row names the fixture
// that caught it; a mutation nobody catches is a fixture bug, not a free pass.
//
// | mutation | caught by |
// |---|---|
// | predicate reads `segment_layouts` instead of `chain_layouts` | `link_mask_judges_a_new_predecessor_against_the_widened_requirement` |
// | worklist never seeded (propagation made shallow) | `link_mask_cuts_an_inbound_edge_...` AND `link_mask_widens_an_inbound_edge_...` |
// | the `None` arm's `unlink_outbound` deleted (widen, never cut) | `link_mask_cuts_an_inbound_edge_a_downstream_widen_invalidates` |
// | `merge_chain`'s conflict arm made permissive | `link_mask_still_refuses_class_d_...`, plus both worklist rows |
// | `chain_layouts` reset dropped from `install`'s recycled arm | `link_mask_resets_the_chain_requirement_when_a_slot_is_recycled` |
// | run.rs's linked entry arm relaxed to `data_matches` | `direct_chain_entry_validates_a_segment_only_the_successor_uses` (execution level) |
//
// Two findings worth keeping. FIRST: the transitive row does NOT catch the `segment_layouts`
// predicate on its own -- the worklist still cuts the edge behind it -- which is why the
// new-predecessor row exists. SECOND: the run.rs mutation survived the ENTIRE crate until the
// execution-level row was written; no BlockCache fixture can see it, because that hole is in what
// the dispatcher proves, not in which edges form.
// ---------------------------------------------------------------------------------------------

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn chain_mask_compilation(
    span: BlockSpan,
    pinned: &[SegmentIndex],
    descriptors: &[(SegmentIndex, u16)],
) -> Compilation {
    let mut compilation = trivial_compilation(span);
    let mut used = 0u8;
    for segment in pinned {
        used |= segment_bit(*segment);
    }
    compilation.segment_layout.used = used;
    for (segment, selector) in descriptors {
        let slot = &mut compilation.segment_layout.data[segment_index(*segment)];
        slot.selector = *selector;
        slot.base = u32::from(*selector) << 4;
    }
    compilation
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn link_target_of(key: BlockKey) -> LinkTarget {
    LinkTarget {
        linear: key.linear,
        mode_key: key.mode_key,
    }
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn install_chain_block(cache: &mut JitState, compilation: &Compilation) -> BlockId {
    let key = compilation.span.key;
    assert!(matches!(cache.probe(key), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    cache.install(compilation).expect("chain block install")
}

/// Class B: the two snapshots differ on ES, which NEITHER block pins. Whole-array equality
/// refuses this edge; it is 68.03% of prince-586's link refusals and the class its hot pair is
/// in.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_admits_class_b_difference_on_a_segment_nobody_pins() {
    let mut cache = BlockCache::default();
    let source = key(0x4_1010);
    let target = key(0x4_1020);

    let mut source_compilation = chain_mask_compilation(
        BlockSpan::new(source, 1, 1).expect("source span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x1111)],
    );
    source_compilation.successors[0] = Some(link_target_of(target));
    let source_id = install_chain_block(&mut cache, &source_compilation);

    let target_compilation = chain_mask_compilation(
        BlockSpan::new(target, 1, 1).expect("target span"),
        &[],
        &[(SegmentIndex::Es, 0x2222)],
    );
    let target_id = install_chain_block(&mut cache, &target_compilation);

    assert_eq!(cache.outbound[source_id.index()][0], Some(target_id));
    assert!(cache.has_linked_successor(source_id));
    assert_eq!(
        cache.stalls.link_refusals[LinkRefusal::SegmentLayout as usize],
        0
    );
}

/// prince-586's hot pair, modelled as what it is: a 2-CYCLE whose two members differ on an ES
/// neither of them pins. Both edges must form, because it is the pair being chain-eligible — not
/// one edge existing — that lifts the quota clamp at run.rs:2294.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_chains_the_prince_hot_pair_two_cycle() {
    let mut cache = BlockCache::default();
    let first = key(0x4_1030);
    let second = key(0x4_1040);

    let mut first_compilation = chain_mask_compilation(
        BlockSpan::new(first, 1, 1).expect("first span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x1111)],
    );
    first_compilation.successors[0] = Some(link_target_of(second));
    let first_id = install_chain_block(&mut cache, &first_compilation);

    let mut second_compilation = chain_mask_compilation(
        BlockSpan::new(second, 1, 1).expect("second span"),
        &[],
        &[(SegmentIndex::Es, 0x2222)],
    );
    second_compilation.successors[0] = Some(link_target_of(first));
    let second_id = install_chain_block(&mut cache, &second_compilation);

    assert_eq!(cache.outbound[first_id.index()][0], Some(second_id));
    assert_eq!(cache.outbound[second_id.index()][0], Some(first_id));
    assert!(cache.has_linked_successor(first_id));
    assert!(cache.has_linked_successor(second_id));
}

/// Class D: both ends pin ES and disagree about it. This is a REAL conflict — one of the two
/// blocks would run against a base the other's entry check never validated — and it must stay
/// refused. The guard against over-admission; it passes before and after the mask lands.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_still_refuses_class_d_conflict_on_a_segment_both_pin() {
    let mut cache = BlockCache::default();
    let source = key(0x4_1050);
    let target = key(0x4_1060);

    let mut source_compilation = chain_mask_compilation(
        BlockSpan::new(source, 1, 1).expect("source span"),
        &[SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x1111)],
    );
    source_compilation.successors[0] = Some(link_target_of(target));
    let source_id = install_chain_block(&mut cache, &source_compilation);

    let target_compilation = chain_mask_compilation(
        BlockSpan::new(target, 1, 1).expect("target span"),
        &[SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    install_chain_block(&mut cache, &target_compilation);

    assert_eq!(cache.outbound[source_id.index()][0], None);
    assert!(!cache.has_linked_successor(source_id));
    assert_eq!(
        cache.stalls.link_refusals[LinkRefusal::SegmentLayout as usize],
        1
    );
}

/// THE DISCRIMINATING ROW. `R -> S` is class B on ES (neither pins it, and their ES descriptors
/// DIFFER). `S -> T` is admissible on its own terms because `S` and `T` agree on ES, which `T`
/// pins. Admitting both and stopping there is the depth-2 miscompile: entering at `R` validates
/// `R`'s ES, and the chain then runs `T`'s body against `S`'s ES base.
///
/// A shallow implementation — one that computes each block's requirement from its own snapshot,
/// or from its direct successors without walking back to predecessors — leaves `R -> S` live and
/// FAILS here. The only sound outcomes are widen-`R` (impossible in this row: `R`'s own ES
/// descriptor disagrees) or CUT `R -> S`.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_cuts_an_inbound_edge_a_downstream_widen_invalidates() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1070);
    let middle = key(0x4_1080);
    let tail = key(0x4_1090);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x1111)],
    );
    root_compilation.successors[0] = Some(link_target_of(middle));
    let root_id = install_chain_block(&mut cache, &root_compilation);

    let mut middle_compilation = chain_mask_compilation(
        BlockSpan::new(middle, 1, 1).expect("middle span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    middle_compilation.successors[0] = Some(link_target_of(tail));
    let middle_id = install_chain_block(&mut cache, &middle_compilation);

    // The ordering hazard this row is most likely to die of: if the edge never formed, every
    // assertion below passes for the wrong reason.
    assert_eq!(
        cache.outbound[root_id.index()][0],
        Some(middle_id),
        "class-B edge R -> S must exist before the widen that is supposed to cut it"
    );

    let tail_compilation = chain_mask_compilation(
        BlockSpan::new(tail, 1, 1).expect("tail span"),
        &[SegmentIndex::Ds, SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    let tail_id = install_chain_block(&mut cache, &tail_compilation);

    assert_eq!(cache.outbound[middle_id.index()][0], Some(tail_id));
    assert_eq!(
        cache.outbound[root_id.index()][0],
        None,
        "R -> S must be CUT: S's requirement now names ES and R disagrees about it"
    );
    assert!(!cache.has_linked_successor(root_id));
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::ChainWiden as usize],
        1,
        "the cut must be attributed to the widen, not to a replace or a retire"
    );
    // S absorbed the widen even though R could not.
    assert_ne!(
        cache.chain_layouts[middle_id.index()].used & segment_bit(SegmentIndex::Es),
        0
    );
    // R-A's load-bearing claim: `unlink_outbound` does NOT re-park. A cut edge reverts to the zero
    // portal and reports `StaticUnbound`; it must not join `waiting`, where `resolve_waiting`
    // would retry it forever against a conflict the monotone widen can never undo.
    assert!(
        cache
            .waiting
            .values()
            .flatten()
            .all(|source| source.block != root_id),
        "a ChainWiden cut must not re-park the source for an absorbing retry loop"
    );
}

/// The mirror arm: same shape, but all three blocks hold the SAME ES descriptor. The widen then
/// SUCCEEDS at `R` instead of cutting, so the chain stays whole. Together with the row above this
/// pins both arms of the worklist; a "cut on every widen" implementation passes that row and
/// fails this one.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_widens_an_inbound_edge_that_can_follow_the_chain() {
    let mut cache = BlockCache::default();
    let root = key(0x4_10a0);
    let middle = key(0x4_10b0);
    let tail = key(0x4_10c0);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(middle));
    let root_id = install_chain_block(&mut cache, &root_compilation);

    let mut middle_compilation = chain_mask_compilation(
        BlockSpan::new(middle, 1, 1).expect("middle span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    middle_compilation.successors[0] = Some(link_target_of(tail));
    let middle_id = install_chain_block(&mut cache, &middle_compilation);
    assert_eq!(cache.outbound[root_id.index()][0], Some(middle_id));

    let tail_compilation = chain_mask_compilation(
        BlockSpan::new(tail, 1, 1).expect("tail span"),
        &[SegmentIndex::Ds, SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    let tail_id = install_chain_block(&mut cache, &tail_compilation);

    assert_eq!(cache.outbound[middle_id.index()][0], Some(tail_id));
    assert_eq!(
        cache.outbound[root_id.index()][0],
        Some(middle_id),
        "the widen agrees at R, so the edge must survive"
    );
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::ChainWiden as usize],
        0
    );
    // And the widen actually REACHED R, two hops from the link that caused it. Without this the
    // row would pass against an implementation that never propagates at all.
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "R's chain requirement must have absorbed the ES that T pins"
    );
    // Non-adoption: only the mask moved. R's own descriptors are still R's.
    assert_eq!(
        cache.chain_layouts[root_id.index()].data,
        cache.segment_layouts[root_id.index()].data
    );
}

/// A NEW predecessor arriving at an already-widened block must be judged against that block's
/// CHAIN requirement, not against its own frozen snapshot. `R` pins only DS, but everything
/// downstream of it needs ES, so an incoming edge that disagrees about ES has to be refused even
/// though neither `P` nor `R` reads ES itself.
///
/// This is the row that catches a predicate wired to `segment_layouts` instead of
/// `chain_layouts`: the propagation would leave `P` with a narrower requirement than the target
/// it links to, which is the invariant the whole design rests on.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_judges_a_new_predecessor_against_the_widened_requirement() {
    let mut cache = BlockCache::default();
    let root = key(0x4_10d0);
    let tail = key(0x4_10e0);
    let newcomer = key(0x4_10f0);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(tail));
    let root_id = install_chain_block(&mut cache, &root_compilation);

    let tail_compilation = chain_mask_compilation(
        BlockSpan::new(tail, 1, 1).expect("tail span"),
        &[SegmentIndex::Ds, SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    let tail_id = install_chain_block(&mut cache, &tail_compilation);
    assert_eq!(cache.outbound[root_id.index()][0], Some(tail_id));
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "R's requirement must name the ES its successor pins"
    );

    let mut newcomer_compilation = chain_mask_compilation(
        BlockSpan::new(newcomer, 1, 1).expect("newcomer span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x1111)],
    );
    newcomer_compilation.successors[0] = Some(link_target_of(root));
    let newcomer_id = install_chain_block(&mut cache, &newcomer_compilation);

    assert_eq!(
        cache.outbound[newcomer_id.index()][0],
        None,
        "P disagrees about the ES R's chain requires, so the edge must be refused"
    );
    assert_eq!(
        cache.stalls.link_refusals[LinkRefusal::SegmentLayout as usize],
        1
    );
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::ChainWiden as usize],
        0,
        "a refusal is not a cut"
    );
}

/// A translation flush drops every edge while the blocks stay compiled, so the requirements those
/// edges justified must go with them. Leaving a widened mask behind is monotone in the SAFE
/// direction but permanently over-strict: the class-B edges this slice exists to admit would be
/// refused for a segment nothing live reaches any more, and nothing ever narrows the mask again.
///
/// Sound because a flush leaves NO live edge to violate: the reset restores exactly the state
/// `install` would have written, which is the only other writer of this array.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_forgets_chain_requirements_a_translation_flush_invalidated() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1210);
    let tail = key(0x4_1220);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(tail));
    let root_id = install_chain_block(&mut cache, &root_compilation);
    let tail_compilation = chain_mask_compilation(
        BlockSpan::new(tail, 1, 1).expect("tail span"),
        &[SegmentIndex::Ds, SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    let tail_id = install_chain_block(&mut cache, &tail_compilation);
    assert_eq!(cache.outbound[root_id.index()][0], Some(tail_id));
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "the row is vacuous unless the requirement really widened first"
    );

    cache.invalidate_translation();

    assert!(cache.inbound.is_empty());
    assert_eq!(cache.outbound[root_id.index()][0], None);
    for index in [root_id.index(), tail_id.index()] {
        assert_eq!(
            cache.chain_layouts[index], cache.segment_layouts[index],
            "a flush leaves no live edge, so no block may keep a widened requirement"
        );
    }
}

/// A recycled slot must not serve the retired occupant's WIDENED requirement to its successor.
/// `install` resets the chain layout in the same statement that writes the block's own layout;
/// without that reset the new block inherits a stale mask AND stale descriptors, and refuses
/// edges it should admit.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_mask_resets_the_chain_requirement_when_a_slot_is_recycled() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1110);
    let tail = key(0x4_1120);
    let newcomer = key(0x4_1130);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(tail));
    let root_id = install_chain_block(&mut cache, &root_compilation);
    let tail_compilation = chain_mask_compilation(
        BlockSpan::new(tail, 1, 1).expect("tail span"),
        &[SegmentIndex::Ds, SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    install_chain_block(&mut cache, &tail_compilation);
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0
    );

    assert_eq!(cache.retire_physical_range_for_test(root.physical, 1), 1);
    let reborn = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(root, 1, 1).expect("reborn span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x1111)],
        ),
    );
    assert_eq!(
        reborn.index(),
        root_id.index(),
        "the row is vacuous unless the slot really is recycled"
    );
    assert_eq!(
        cache.chain_layouts[reborn.index()],
        cache.segment_layouts[reborn.index()],
        "a fresh occupant starts at its own layout"
    );

    // And the reset is observable through the predicate, not just the field: an edge that agrees
    // with the NEW occupant must link.
    let mut newcomer_compilation = chain_mask_compilation(
        BlockSpan::new(newcomer, 1, 1).expect("newcomer span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x1111)],
    );
    newcomer_compilation.successors[0] = Some(link_target_of(root));
    let newcomer_id = install_chain_block(&mut cache, &newcomer_compilation);
    assert_eq!(cache.outbound[newcomer_id.index()][0], Some(reborn));
}

// ---------------------------------------------------------------------------
// The retry lift (S4 part 2). `lift_cold_smc_dormant` re-admits keys the SMC heat gate parked,
// driven by a stamp aging out. Nothing re-admitted the keys the COMPILE WALK parked, and on the
// tombraid loader phase 194 of 466 of those are `DecodeMiss`, a cause that clears the moment the
// interpreter runs the same bytes and refills the line.
// ---------------------------------------------------------------------------

/// A clearable cause lifts at exactly `RETRY_LIFT_VISITS`, and not one visit sooner.
///
/// The exact count matters more than it looks: the lift's cost is a compile attempt, and a gate
/// that fired at the first visit would hand every dormant key one compile per memo era. The
/// boundary is asserted from both sides.
///
/// MUTATION: change the comparison to `<=` and the key lifts one visit early, which the
/// still-dormant assertion inside the loop catches.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn a_clearable_retry_cause_lifts_at_the_visit_threshold() {
    set_retry_lift_for_test(Some(true));
    let mut cache = BlockCache::default();
    let k = key(0x2000);
    cache.park_dormant_for_test(k, DormantReason::CompileRetry, Some(RetryCause::DecodeMiss));
    assert!(matches!(cache.probe(k), BlockProbe::Rejected));

    for visit in 1..RETRY_LIFT_VISITS {
        assert!(
            !cache.lift_clearable_retry_dormant(k),
            "visit {visit} lifted before the threshold"
        );
        assert!(cache.is_dormant_for_test(k));
    }
    assert!(
        cache.lift_clearable_retry_dormant(k),
        "the threshold visit must lift"
    );
    assert!(!cache.is_dormant_for_test(k), "a lifted key is Seen again");
    assert_eq!(cache.stall_snapshot().retry_lifts, 1);
    // Seen, so the next observation is a compile rather than another decline.
    assert!(matches!(cache.probe(k), BlockProbe::Compile));
    set_retry_lift_for_test(None);
}

/// A DETERMINISTIC cause is never lifted, however long it waits.
///
/// `TooShort` is the post-walk min-length rule: it reads the shape of the block the walk formed,
/// which is a function of the code bytes and the key, so a re-walk reaches the same answer for
/// ever. Lifting it would be a compile attempt per key per window with a guaranteed park behind
/// it, on the population that is already the largest unattributed exit class.
///
/// MUTATION: make `clearable_by_retry` return true for `TooShort` and this fails on the first
/// iteration past the threshold.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn a_deterministic_retry_cause_is_never_lifted() {
    set_retry_lift_for_test(Some(true));
    let mut cache = BlockCache::default();
    for cause in [
        RetryCause::TooShort,
        RetryCause::AdmissionMatrix,
        RetryCause::X87Cap,
        RetryCause::CalloutCap,
        RetryCause::MemoryAluCap,
        RetryCause::StackAccessCap,
        RetryCause::PostWalk,
        RetryCause::HostPageLen,
        RetryCause::PageCross,
        RetryCause::SegmentLimit,
    ] {
        let k = key(0x3000 + (cause as u32) * 0x10);
        cache.park_dormant_for_test(k, DormantReason::CompileRetry, Some(cause));
        for _ in 0..(u32::from(RETRY_LIFT_VISITS) * 3) {
            assert!(
                !cache.lift_clearable_retry_dormant(k),
                "{} was lifted",
                cause.label()
            );
        }
        assert!(cache.is_dormant_for_test(k), "{}", cause.label());
    }
    assert_eq!(cache.stall_snapshot().retry_lifts, 0);
    // And the heat lane keeps its own answer: a `SpanHot` park carries no cause at all, so this
    // arm must not touch it even though `lift_cold_smc_dormant` would.
    let heat = key(0x3900);
    cache.park_dormant_for_test(heat, DormantReason::SpanHot, None);
    for _ in 0..(u32::from(RETRY_LIFT_VISITS) * 3) {
        assert!(!cache.lift_clearable_retry_dormant(heat));
    }
    set_retry_lift_for_test(None);
}

/// One lift per key per cause. A key that comes straight back with the SAME cause is parked
/// permanently, and the counter says so.
///
/// The lift is an offer to re-walk once. A key that takes it and lands on the same gate has
/// supplied the evidence that re-walking does not help, and without this rule it would buy
/// another window and another compile for ever -- which is precisely the treadmill
/// `note_demoted_callout_site` exists to avoid one level down.
///
/// A DIFFERENT cause is a different question and gets its own window, which is the boundary this
/// also pins -- and the THIRD leg is what makes the bound real: the key comes back to the first
/// cause, and that is a repark too. `clearable_by_retry` names two causes, so a key can spend at
/// most two lifts, and an alternating key runs out rather than looping. That leg fails against a
/// per-key record, which remembers only the last cause.
///
/// MUTATION: drop the `retry_lift_spent` read from `dormant` and the second window fires; make it
/// a `HashMap<BlockKey, RetryCause>` again and the third leg lifts for ever.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn a_lifted_key_that_reparks_with_the_same_cause_is_never_lifted_again() {
    set_retry_lift_for_test(Some(true));
    let mut cache = BlockCache::default();
    let k = key(0x4000);
    cache.park_dormant_for_test(k, DormantReason::CompileRetry, Some(RetryCause::DecodeMiss));
    cache.set_dormant_visits_for_test(k, RETRY_LIFT_VISITS - 1);
    assert!(cache.lift_clearable_retry_dormant(k));

    // Straight back with the same answer.
    cache.dormant(k, DormantReason::CompileRetry, Some(RetryCause::DecodeMiss));
    assert_eq!(cache.stall_snapshot().retry_lift_reparks, 1);
    for _ in 0..(u32::from(RETRY_LIFT_VISITS) * 3) {
        assert!(
            !cache.lift_clearable_retry_dormant(k),
            "a permanently parked key must never lift again"
        );
    }
    assert_eq!(cache.stall_snapshot().retry_lifts, 1);

    // A DIFFERENT clearable cause is a different question, and gets its own window.
    let other = key(0x4100);
    cache.park_dormant_for_test(
        other,
        DormantReason::CompileRetry,
        Some(RetryCause::DecodeMiss),
    );
    cache.set_dormant_visits_for_test(other, RETRY_LIFT_VISITS - 1);
    assert!(cache.lift_clearable_retry_dormant(other));
    cache.dormant(
        other,
        DormantReason::CompileRetry,
        Some(RetryCause::TranslationMismatch),
    );
    assert_eq!(
        cache.stall_snapshot().retry_lift_reparks,
        1,
        "a different cause is not a repark"
    );
    cache.set_dormant_visits_for_test(other, RETRY_LIFT_VISITS - 1);
    assert!(cache.lift_clearable_retry_dormant(other));
    assert_eq!(cache.stall_snapshot().retry_lifts, 3);

    // THE THIRD LEG, and the one the bound actually rests on: back to the FIRST cause. Both
    // `DecodeMiss` and `TranslationMismatch` are clearable, so a key that alternates them was the
    // one shape a per-key record could not stop -- it remembered only the last cause and was
    // overwritten every time, which is a lift every window for ever. The record is a set of
    // (key, cause) PAIRS, so this key has now spent both of the two causes there are.
    cache.dormant(
        other,
        DormantReason::CompileRetry,
        Some(RetryCause::DecodeMiss),
    );
    assert_eq!(
        cache.stall_snapshot().retry_lift_reparks,
        2,
        "coming back to a cause it already spent IS a repark"
    );
    for _ in 0..(u32::from(RETRY_LIFT_VISITS) * 3) {
        assert!(
            !cache.lift_clearable_retry_dormant(other),
            "an alternating key must run out of causes, not lift for ever"
        );
    }
    assert_eq!(cache.stall_snapshot().retry_lifts, 3);
    set_retry_lift_for_test(None);
}

/// The retry lift is OFF by default, and the off arm is the pre-slice behaviour exactly.
///
/// Every fixture above forces the arm ON, which is the convention this backend's knobs use and is
/// also what makes this test necessary: with the arm stated everywhere else, nothing was asserting
/// what an unstated build does. `IZARRAVM_RETRY_LIFT` defaulted OFF on 2026-08-22 while the duke
/// regression is unattributed, so this is the arm that ships.
///
/// MUTATION: drop the gate read from `lift_clearable_retry_dormant` and the key lifts here.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn the_retry_lift_is_off_by_default() {
    let mut cache = BlockCache::default();
    let k = key(0x5000);
    cache.park_dormant_for_test(k, DormantReason::CompileRetry, Some(RetryCause::DecodeMiss));
    for _ in 0..(u32::from(RETRY_LIFT_VISITS) * 3) {
        assert!(
            !cache.lift_clearable_retry_dormant(k),
            "the default arm must never lift"
        );
    }
    assert!(cache.is_dormant_for_test(k));
    assert_eq!(cache.stall_snapshot().retry_lifts, 0);

    // Control: the same key on the ON arm lifts at the threshold, so the assertion above is about
    // the gate and not about the fixture.
    set_retry_lift_for_test(Some(true));
    cache.set_dormant_visits_for_test(k, RETRY_LIFT_VISITS - 1);
    assert!(cache.lift_clearable_retry_dormant(k));
    set_retry_lift_for_test(None);
}

/// `IZARRAVM_RETRY_LIFT`'s spelling table. Default OFF, `1` / `on` is the opt-in.
#[test]
fn the_retry_lift_knob_spellings() {
    assert!(
        !parse_retry_lift_arm_for_test(Err(std::env::VarError::NotPresent)),
        "unset is OFF"
    );
    for on in ["1", "on", "ON", " on "] {
        assert!(parse_retry_lift_arm_for_test(Ok(on.to_string())), "{on}");
    }
    for off in ["0", "off", "OFF", "", "  "] {
        assert!(!parse_retry_lift_arm_for_test(Ok(off.to_string())), "{off}");
    }
}

/// `BlockState` did not grow when `Dormant` learned its retry cause, its visit count and its
/// permanence.
///
/// `entries` is the map every probe, every invalidation and every classify walks, so a wider value
/// is a broader cache-miss cost than anything the payload buys -- and it would be an
/// everywhere-regression with exactly the shape the duke numbers have, which is why this is pinned
/// rather than argued. It is free because `Rejected(RejectedSpan)` already carries a `BlockKey`
/// plus a `u16` and `Compiled(BlockId)` already forces 8-byte alignment: the four bytes
/// `DormantEntry` adds land inside padding the enum was already paying for.
#[test]
fn the_block_state_payload_stayed_the_same_width() {
    assert_eq!(
        std::mem::size_of::<BlockState>(),
        std::mem::size_of::<RejectedSpan>() + std::mem::align_of::<BlockId>(),
        "BlockState must stay as wide as its largest arm plus one aligned tag"
    );
    assert!(std::mem::size_of::<DormantEntry>() <= std::mem::size_of::<RejectedSpan>());
}

// ---------------------------------------------------------------------------------------------
// The chain-requirement entry check: the knob, the narrowing, and the two arms of `entry_layout`.
// dev_docs/specs/2026-08-25-chain-requirement-entry-check-design.md
// ---------------------------------------------------------------------------------------------

/// Restores the ambient `IZARRAVM_CHAIN_ENTRY_CHECK` arm when it drops, so a fixture that asserts
/// its way out of the middle of a row cannot leave the override set for whatever runs next on the
/// same thread.
struct ChainEntryCheckArm;

impl ChainEntryCheckArm {
    fn forced(armed: bool) -> Self {
        set_chain_entry_check_for_test(Some(armed));
        Self
    }
}

impl Drop for ChainEntryCheckArm {
    fn drop(&mut self) {
        set_chain_entry_check_for_test(None);
    }
}

/// The spelling table, tested without touching the process environment: the shipped reading is
/// cached in a `OnceLock`, so the env itself is assertable exactly once per process and never in
/// an order the harness controls.
///
/// **UNSET AND `""` REACH THE SAME ARM HERE**, unlike `IZARRAVM_SEGMENT_RETIRE_GOVERNOR`, whose
/// unset arm is `cap` and whose `""` arm is off. That difference is the whole reason this row
/// exists: the two variables are set together in every leg of this slice's ladder, and a reader
/// who assumes they null alike disarms the shipped governor default without noticing.
#[test]
fn the_chain_entry_check_spelling_table_is_exact() {
    use parse_chain_entry_check_arm_for_test as parse;
    // DEFAULT ON since the 2026-08-25 flip. Unset and `` still reach the SAME arm as each other
    // -- the flip moved the default, not the rule -- but that arm is now the ARMED one, so an
    // OFF leg must EXPORT `0`.
    assert!(
        parse(Err(std::env::VarError::NotPresent)),
        "unset must name the shipped default, which is ON since the flip"
    );
    assert!(
        parse(Ok(String::new())),
        "`` follows unset here, deliberately -- and unset is now ON"
    );
    for spelling in ["0", "off", "OFF", "  0  ", " off \n"] {
        assert!(
            !parse(Ok(spelling.to_string())),
            "{spelling:?} must name the OFF arm: the escape and the A/B base"
        );
    }
    for spelling in ["1", "on", "ON", "chain", "Chain", " chain "] {
        assert!(
            parse(Ok(spelling.to_string())),
            "{spelling:?} must name the chain-requirement arm"
        );
    }
}

/// A MISSPELLED LADDER LEG MUST FAIL LOUDLY rather than silently running the default, which for
/// this knob is also the base arm of the A/B -- so a typo would be read as "the slice I asked for
/// changed nothing", the one wrong conclusion an arm ladder exists to avoid.
#[test]
fn an_unrecognised_chain_entry_check_spelling_refuses_to_guess() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcomes: Vec<_> = ["chained", "cap", "2", "true", "yes", "chain_layout"]
        .into_iter()
        .map(|spelling| {
            let panicked = std::panic::catch_unwind(|| {
                parse_chain_entry_check_arm_for_test(Ok(spelling.to_string()))
            })
            .is_err();
            (spelling, panicked)
        })
        .collect();
    std::panic::set_hook(previous);

    for (spelling, panicked) in outcomes {
        assert!(
            panicked,
            "IZARRAVM_CHAIN_ENTRY_CHECK={spelling:?} names no arm and must panic rather than \
             silently falling through to the default"
        );
    }
}

/// `P` links to `A`, whose requirement pins ES, so `chain(P)` widens to carry ES. Retiring `A`
/// takes `P`'s last outbound edge, and `P`'s requirement must fall back to its OWN layout: with
/// no live edge the cone is `P` alone.
///
/// **THE CUT SITE THIS ROW GUARDS IS NOT `unlink_outbound`.** `unlink_block`'s inbound walk clears
/// each predecessor's cell INLINE and never calls that helper, and it is the `Retired` cause --
/// the largest cut population on every row. A narrowing wired only into `unlink_outbound` passes
/// every other row in this file and fails here.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn chain_requirement_narrows_when_a_retired_successor_cuts_the_edge() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1310);
    let tail = key(0x4_1320);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(tail));
    let root_id = install_chain_block(&mut cache, &root_compilation);
    let tail_compilation = chain_mask_compilation(
        BlockSpan::new(tail, 1, 1).expect("tail span"),
        &[SegmentIndex::Ds, SegmentIndex::Es],
        &[(SegmentIndex::Es, 0x2222)],
    );
    install_chain_block(&mut cache, &tail_compilation);
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "the row is vacuous unless the requirement really widened first"
    );

    assert_eq!(cache.retire_physical_range_for_test(tail.physical, 1), 1);

    assert_eq!(
        cache.outbound[root_id.index()],
        [None, None],
        "retiring the only successor must leave the root with no outbound edge"
    );
    assert_eq!(
        cache.chain_layouts[root_id.index()],
        cache.segment_layouts[root_id.index()],
        "with no live edge the cone is this block alone, so its requirement is its own layout"
    );
    assert_eq!(
        cache.stalls.chain_requirement_narrowed[LinkClearCause::Retired as usize],
        1,
        "the narrowing must be attributed to the RETIRED cut site, not to a replace or a widen"
    );
}

/// The narrowing is NOT behind `IZARRAVM_CHAIN_ENTRY_CHECK`, and this row is what says so. It is a
/// correctness prerequisite of the armed arm and, on the OFF arm, a strictly more accurate
/// statement about a live link graph; gating it would hand the two arms different link graphs to
/// reason about, and the OFF arm would then stop being the base the ladder needs.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn chain_requirement_narrows_on_both_arms() {
    for armed in [false, true] {
        let _arm = ChainEntryCheckArm::forced(armed);
        let mut cache = BlockCache::default();
        assert_eq!(
            cache.chain_entry_check_armed(),
            armed,
            "the arm is read once, at construction; a fixture that sets it later tests nothing"
        );
        let root = key(0x4_1410);
        let tail = key(0x4_1420);

        let mut root_compilation = chain_mask_compilation(
            BlockSpan::new(root, 1, 1).expect("root span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x2222)],
        );
        root_compilation.successors[0] = Some(link_target_of(tail));
        let root_id = install_chain_block(&mut cache, &root_compilation);
        install_chain_block(
            &mut cache,
            &chain_mask_compilation(
                BlockSpan::new(tail, 1, 1).expect("tail span"),
                &[SegmentIndex::Ds, SegmentIndex::Es],
                &[(SegmentIndex::Es, 0x2222)],
            ),
        );
        assert_ne!(
            cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
            0
        );

        assert_eq!(cache.retire_physical_range_for_test(tail.physical, 1), 1);

        assert_eq!(
            cache.chain_layouts[root_id.index()],
            cache.segment_layouts[root_id.index()],
            "the narrowing must fire on the {armed} arm too -- it is not gated"
        );
    }
}

/// **THE MISCOMPILE ROW.** The narrowing predicate must be `outbound == [None, None]` and must
/// NEVER be `!has_linked_successor(..)`.
///
/// `LinkCell::linked()` reads the target PORTAL's visibility, and a decode-slot suspension or an
/// arena compaction clears and later REPUBLISHES portals without touching `outbound` and without
/// re-running `try_link_inner`. So `has_linked_successor` reverts to true with no merge and no
/// widen behind it. Narrow on that state and the sequence is: hide the successor, narrow the
/// root, re-show the successor, enter the root under the narrowed mask, chain into a body against
/// a base nobody validated.
///
/// The row builds exactly that window -- both successors portal-hidden while both `outbound` slots
/// are still `Some` -- and then runs a cut on the OTHER slot, which is what calls the helper. The
/// requirement must not move.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn hiding_a_successor_does_not_narrow_the_chain_requirement() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1510);
    let pinning = key(0x4_1520);
    let plain = key(0x4_1530);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(pinning));
    root_compilation.successors[1] = Some(link_target_of(plain));
    let root_id = install_chain_block(&mut cache, &root_compilation);
    let pinning_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(pinning, 1, 1).expect("pinning span"),
            &[SegmentIndex::Ds, SegmentIndex::Es],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    let plain_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(plain, 1, 1).expect("plain span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    assert_eq!(cache.outbound[root_id.index()][0], Some(pinning_id));
    assert_eq!(cache.outbound[root_id.index()][1], Some(plain_id));
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "the row is vacuous unless the requirement really carries the successor's ES"
    );

    // The window: portals cleared, `outbound` untouched. This is what `suspend_decode_slot` and
    // `compact_arena` do, and neither of them re-runs a merge on the way back.
    cache.block_portals[pinning_id.index()].clear();
    cache.block_portals[plain_id.index()].clear();
    assert!(
        !cache.has_linked_successor(root_id),
        "the row is vacuous unless the VISIBILITY predicate really reads false here"
    );
    assert_ne!(
        cache.outbound[root_id.index()],
        [None, None],
        "and unless the LINK GRAPH still says both edges exist"
    );

    cache.unlink_outbound(root_id, 1, LinkClearCause::ChainWiden);

    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "a hidden successor is still a successor: narrowing here is a wrong-base miscompile the \
         moment root dispatch republishes its portal"
    );
    assert_eq!(
        cache.stalls.chain_requirement_narrowed.iter().sum::<u64>(),
        0,
        "and the counter must not claim a narrowing that did not happen"
    );
}

/// **R2-1, the round-2 finding, and the row that fails on the unfixed code.**
///
/// `try_link_inner` snapshots the source's chain requirement BEFORE its refusal chain, then calls
/// `unlink_outbound` on the relink-replace path -- which narrows -- and then hands the merge to
/// `widen_chain_requirement`. Handing it the PRE-CUT snapshot writes the cut edge's bits straight
/// back and silently undoes the narrowing, and nothing asserts against it: the monotonicity check
/// passes because the pre-cut value contains the narrowed one, and the non-adoption check compares
/// `data`, which the narrowing does not touch.
///
/// Here `P` links to a successor that pins ES, then relinks the same slot to one that does not.
/// After the relink `P`'s only live edge is to a block that pins nothing beyond DS, so ES has no
/// business in `P`'s requirement. Recomputing the merge from the post-cut requirement is what
/// makes that true, and it changes no edge admission: the refusal decision was already taken from
/// the pre-cut value, and a narrower source can only make a merge succeed where it already did.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn a_replaced_edge_does_not_restore_the_cut_successors_requirement() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1610);
    let pinning = key(0x4_1620);
    let plain = key(0x4_1630);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(pinning));
    let root_id = install_chain_block(&mut cache, &root_compilation);
    let pinning_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(pinning, 1, 1).expect("pinning span"),
            &[SegmentIndex::Ds, SegmentIndex::Es],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    let plain_id = install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(plain, 1, 1).expect("plain span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    assert_eq!(cache.outbound[root_id.index()][0], Some(pinning_id));
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "the row is vacuous unless the requirement really carries the first successor's ES"
    );

    assert!(
        cache.try_link(root_id, 0, plain_id),
        "the replacement edge must be admitted, or the row proves nothing about the replace path"
    );

    assert_eq!(cache.outbound[root_id.index()], [Some(plain_id), None]);
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::Replaced as usize],
        1
    );
    assert_eq!(
        cache.stalls.chain_requirement_narrowed[LinkClearCause::Replaced as usize],
        1,
        "the replace emptied the last slot, so the cut must have narrowed"
    );
    assert_eq!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "the cut successor's ES must NOT come back: the merge handed to the propagation has to be \
         recomputed from the POST-cut requirement, not from the snapshot taken before the cut"
    );
    assert_eq!(
        cache.chain_layouts[root_id.index()],
        cache.segment_layouts[root_id.index()],
        "and the replacement pins nothing beyond what the root pins itself"
    );
}

/// A wholesale flush resets every requirement, and it may only do that once every edge is gone.
/// Resetting first would leave an armed cell able to reach a body under a requirement that no
/// longer names the segment that body's cone pins -- stale-too-NARROW, the miscompile direction.
/// The `debug_assert!` in `invalidate_translation` is what makes the reordering fail; this row is
/// what makes the assert run on a shape that has edges to drop.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn chain_requirement_narrows_only_after_every_edge_is_dropped() {
    let mut cache = BlockCache::default();
    let root = key(0x4_1710);
    let tail = key(0x4_1720);

    let mut root_compilation = chain_mask_compilation(
        BlockSpan::new(root, 1, 1).expect("root span"),
        &[SegmentIndex::Ds],
        &[(SegmentIndex::Es, 0x2222)],
    );
    root_compilation.successors[0] = Some(link_target_of(tail));
    let root_id = install_chain_block(&mut cache, &root_compilation);
    install_chain_block(
        &mut cache,
        &chain_mask_compilation(
            BlockSpan::new(tail, 1, 1).expect("tail span"),
            &[SegmentIndex::Ds, SegmentIndex::Es],
            &[(SegmentIndex::Es, 0x2222)],
        ),
    );
    assert_ne!(
        cache.chain_layouts[root_id.index()].used & segment_bit(SegmentIndex::Es),
        0,
        "the row is vacuous unless there is a widened requirement for the flush to drop"
    );

    cache.invalidate_translation();

    assert!(cache.outbound.iter().all(|slots| *slots == [None, None]));
    assert_eq!(
        cache.chain_layouts[root_id.index()],
        cache.segment_layouts[root_id.index()]
    );
    assert_eq!(
        cache.stalls.chain_requirement_narrowed.iter().sum::<u64>(),
        0,
        "the wholesale reset is not the per-block helper and must not be counted as one"
    );
}

/// `entry_layout` selects an ARRAY, and it takes exactly ONE indexed copy either way.
///
/// The one-copy half is mutant M14: the 2026-08-18 plan pinned that the entry check must REPLACE
/// the 116-byte fetch rather than add a second, and "this accessor reads one `Vec`" is not
/// otherwise assertable in Rust. The counter is bumped by the single accessor both arms go
/// through, so it catches a second array read that goes through that accessor; a mutant that
/// indexes a `Vec` directly instead is a review kill and the sweep records it as one.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn entry_layout_selects_one_array_and_fetches_it_once() {
    for armed in [false, true] {
        let _arm = ChainEntryCheckArm::forced(armed);
        let mut cache = BlockCache::default();
        let root = key(0x4_1810);
        let tail = key(0x4_1820);

        let mut root_compilation = chain_mask_compilation(
            BlockSpan::new(root, 1, 1).expect("root span"),
            &[SegmentIndex::Ds],
            &[(SegmentIndex::Es, 0x2222)],
        );
        root_compilation.successors[0] = Some(link_target_of(tail));
        let root_id = install_chain_block(&mut cache, &root_compilation);
        install_chain_block(
            &mut cache,
            &chain_mask_compilation(
                BlockSpan::new(tail, 1, 1).expect("tail span"),
                &[SegmentIndex::Ds, SegmentIndex::Es],
                &[(SegmentIndex::Es, 0x2222)],
            ),
        );
        // The two arrays must actually DIFFER here, or neither half of this row means anything.
        assert_ne!(
            cache.chain_layouts[root_id.index()],
            cache.segment_layouts[root_id.index()]
        );

        cache.entry_layout_fetches.store(0, Ordering::Relaxed);
        let fetched = cache.entry_layout(root_id).expect("the block is live");
        assert_eq!(
            cache.entry_layout_fetches.load(Ordering::Relaxed),
            1,
            "the entry check takes ONE 116-byte copy; a second is the cost regression M14 names"
        );
        let expected = if armed {
            cache.chain_layouts[root_id.index()]
        } else {
            cache.segment_layouts[root_id.index()]
        };
        assert_eq!(
            fetched, expected,
            "the {armed} arm must read the array its contract names"
        );
        // `cs_matches` shares this fetch on both arms, so the two layouts must agree about CS or
        // routing it through the chain requirement would change what CS the entry check proves.
        assert_eq!(
            cache.chain_layouts[root_id.index()].cs,
            cache.segment_layouts[root_id.index()].cs,
            "a merge constructs `cs: self.cs`, so chain.cs == own.cs always"
        );
    }
}

/// `BAKES_CS_BIT` names no segment and the masked compare cannot see it. **Do not add
/// `& SEGMENT_MASK_BITS` at the entry check**: it would be inert today, and an inert mask migrates
/// to a consumer where it is not inert.
#[test]
fn bakes_cs_bit_is_invisible_to_the_entry_mask() {
    let cpu = CpuGsw::default();
    let mut layout = SegmentLayout::capture(&cpu, 0, 0, 0).expect("default layout");
    layout.used = BAKES_CS_BIT;
    // Move every descriptor the mask could name. Bit 6 names none of them, so the compare passes.
    let mut moved = cpu;
    for segment in SEGMENT_ORDER {
        let mut record = moved.registers.segment(segment);
        record.base = record.base.wrapping_add(0x1000);
        moved.registers.set_segment(segment, record);
    }
    assert!(
        layout.data_matches(&moved),
        "bit 6 is not a segment: `segment_bit` cannot produce it and `data_matches` walks \
         SEGMENT_ORDER only"
    );
    assert!(
        !layout.all_data_matches(&moved),
        "the row is vacuous unless the descriptors really moved"
    );
    for segment in SEGMENT_ORDER {
        assert_ne!(
            segment_bit(segment),
            BAKES_CS_BIT,
            "no segment may ever be given bit 6"
        );
        assert_eq!(
            segment_bit(segment) & SEGMENT_MASK_BITS,
            segment_bit(segment)
        );
    }
}
