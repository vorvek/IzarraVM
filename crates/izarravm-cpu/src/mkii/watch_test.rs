// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{CpuGsw, SegmentIndex, jit, tests::TestBus};
use izarravm_bus::{BusAccessKind, DirectPage};

#[test]
fn mkii_source_writes_survive_direct_clear_and_deferred_calls() {
    let mut cpu = CpuGsw::default();
    cpu.mkii_watch_source(0x1200, 8);
    cpu.jit_direct.clear();
    assert!(cpu.code_write_watched(0x1202, 1));
    assert!(!cpu.note_code_write(0x1210, 1));
    assert!(!cpu.jit_direct.mkii.code_dirty);

    cpu.deferred_code_writes.open();
    assert!(cpu.note_code_write(0x1202, 1));
    assert!(cpu.jit_direct.mkii.code_dirty);
    cpu.deferred_code_writes.close();
    cpu.drain_deferred_code_writes();
}

#[test]
fn mkii_source_union_handles_alias_spans_and_address_wrap() {
    let mut state = State::default();
    for (start, len) in [(0x100, 8), (0x110, 8), (0x106, 12), (u32::MAX - 1, 4)] {
        state.add_source(start, len);
    }
    for (start, len, hit) in [
        (0xf0, 16, false),
        (0xff, 2, true),
        (0x117, 1, true),
        (0x118, 4, false),
        (u32::MAX, 2, true),
        (1, 1, true),
        (2, 1, false),
        (0x100, 0, false),
    ] {
        assert_eq!(state.note_write(start, len), hit, "{start:x}+{len}");
        state.code_dirty = false;
    }
}

#[test]
fn mkii_backing_change_invalidates_sources_and_clone_drops_ownership() {
    let mut cpu = CpuGsw::default();
    cpu.mkii_watch_source(0x1800, 4);
    let clone = cpu.clone();
    assert!(!clone.code_write_watched(0x1800, 1));
    assert!(clone.jit_direct.mkii.sources.is_empty());
    cpu.note_direct_map_changed();
    assert!(cpu.jit_direct.mkii.code_dirty);
    assert!(cpu.jit_direct.mkii.mapping_dirty);
    assert!(cpu.code_write_watched(0x1800, 1));
}

#[test]
fn mkii_source_acquisition_sweeps_every_clear_alias() {
    const PHYSICAL: u32 = 0x3000;
    const ALIAS: u32 = 0x0051_3000;

    #[repr(align(4096))]
    struct Page([u8; 4096]);

    let mut cpu = CpuGsw::default();
    let mut bytes = Box::new(Page([0; 4096]));
    for linear in [PHYSICAL, ALIAS] {
        assert!(cpu.jit_fast_map.populate_write(
            linear,
            PHYSICAL,
            DirectPage {
                physical_page: PHYSICAL,
                ptr: bytes.0.as_mut_ptr(),
                len: bytes.0.len(),
                writable: true,
                mapping_epoch: 0,
            },
            jit::fast_map::PagePermissions::UNPAGED,
            false,
        ));
        assert!(!cpu.jit_fast_map.page_watched_bit_for_test(linear));
    }

    cpu.mkii_watch_source(PHYSICAL + 0x20, 4);

    for linear in [PHYSICAL, ALIAS] {
        assert!(
            !cpu.jit_fast_map.has_write_mapping(linear, PHYSICAL),
            "mkII source acquisition left a clear alias live at {linear:#x}"
        );
        assert!(cpu.jit_fast_map.populate_write(
            linear,
            PHYSICAL,
            DirectPage {
                physical_page: PHYSICAL,
                ptr: bytes.0.as_mut_ptr(),
                len: bytes.0.len(),
                writable: true,
                mapping_epoch: 0,
            },
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(PHYSICAL),
        ));
        assert!(cpu.jit_fast_map.page_watched_bit_for_test(linear));
    }

    cpu.jit_direct.clear();
    assert!(cpu.code_write_watched(PHYSICAL + 0x20, 1));
}

#[test]
fn fast_map_hit_on_mkii_source_defers_the_dirty_write() {
    const SOURCE: u32 = 0x3000;

    let mut cpu = CpuGsw::default();
    let mut bus = TestBus::with_memory(vec![0; 0x8000]);
    bus.enable_direct_pages_for_test();
    cpu.set_jit_auto_admit(true);
    cpu.mkii_watch_source(SOURCE, 1);

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        SOURCE,
        0,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(cpu.jit_fast_map.has_write_mapping(SOURCE, SOURCE));
    assert!(cpu.jit_fast_map.page_watched_bit_for_test(SOURCE));

    cpu.deferred_code_writes.open();
    let hits_before = cpu.fast_map_probe_counters().hits;
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        SOURCE,
        0xaa,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert_eq!(cpu.fast_map_probe_counters().hits, hits_before + 1);
    assert!(cpu.jit_direct.mkii.code_dirty);
    assert_eq!(bus.memory_byte_for_test(SOURCE), 0xaa);

    cpu.deferred_code_writes.close();
    cpu.drain_deferred_code_writes();
}
