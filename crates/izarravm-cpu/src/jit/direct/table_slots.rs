// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The write-once table-base slots emitted code loads R15-relative. Moved
//! verbatim out of `direct.rs` to keep that file under the source-line ceiling; nothing here
//! changed but the module boundary. `direct.rs` re-exports this module's contents
//! (`use table_slots::*`), the `frame` precedent.

use super::*;

/// The write-once table bases emitted code loads R15-relative when
/// `JitState::r15_tables` is on: the four fast-map SoA arrays
/// (`NativeMapBases`) and the two code-watch page tables, in the slot order
/// `emit::table_slot_offset` indexes. Each source allocation is created once
/// and never freed or moved for the CPU's life (fast_map.rs storage,
/// code_watch.rs `table_base`), which is the same invariant the baked-imm64
/// emission already depended on; `publish` merely re-states it where a
/// violation would finally be VISIBLE instead of a silent miscompile.
///
/// Host pointers, not guest state: `Clone` resets to default and `PartialEq`
/// ignores the slots, `CallOutTable`'s shape and reason. A cloned CPU gets a
/// fresh `BlockCache` (its clone drops compiled blocks), so its first compile
/// republishes before any slot-reading block can run.
#[derive(Debug, Default)]
pub(crate) struct NativeTableSlots {
    pub(super) slots: [usize; 6],
}

pub(crate) const TABLE_SLOT_FLAGS: usize = 0;
pub(crate) const TABLE_SLOT_READ_BIASES: usize = 1;
pub(crate) const TABLE_SLOT_WRITE_BIASES: usize = 2;
pub(crate) const TABLE_SLOT_PHYSICAL_PAGES: usize = 3;
pub(crate) const TABLE_SLOT_CODE_WATCH_STICKY: usize = 4;
pub(crate) const TABLE_SLOT_CODE_WATCH_NATIVE: usize = 5;

impl NativeTableSlots {
    /// Record `value` in `slot`. Idempotent by invariant; a republish that
    /// CHANGES a nonzero slot means a table base moved while emitted code
    /// could still hold it, which the imm64 arm would miscompile silently.
    pub(crate) fn publish(&mut self, slot: usize, value: usize) {
        debug_assert!(
            self.slots[slot] == 0 || self.slots[slot] == value,
            "published table base changed: slot {slot} held {:#x}, now {value:#x}",
            self.slots[slot],
        );
        self.slots[slot] = value;
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn publish_map(&mut self, map: NativeMapBases) {
        self.publish(TABLE_SLOT_FLAGS, map.flags());
        self.publish(TABLE_SLOT_READ_BIASES, map.read_biases());
        self.publish(TABLE_SLOT_WRITE_BIASES, map.write_biases());
        self.publish(TABLE_SLOT_PHYSICAL_PAGES, map.physical_pages());
    }

    pub(crate) fn publish_code_watch(&mut self, tables: [usize; 2]) {
        self.publish(TABLE_SLOT_CODE_WATCH_STICKY, tables[0]);
        self.publish(TABLE_SLOT_CODE_WATCH_NATIVE, tables[1]);
    }
}

impl Clone for NativeTableSlots {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for NativeTableSlots {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for NativeTableSlots {}
