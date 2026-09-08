// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::CpuGsw;

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
