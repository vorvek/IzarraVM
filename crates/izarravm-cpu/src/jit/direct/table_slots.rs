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
#[derive(Debug)]
pub(crate) struct NativeTableSlots {
    pub(super) slots: [usize; 6 + 1 + STORE_STUB_COUNT + 1 + READ_STUB_COUNT],
}

// Manual because `Default` is not derivable past 32 array elements.
impl Default for NativeTableSlots {
    fn default() -> Self {
        Self {
            slots: [0; 6 + 1 + STORE_STUB_COUNT + 1 + READ_STUB_COUNT],
        }
    }
}

pub(crate) const TABLE_SLOT_FLAGS: usize = 0;
pub(crate) const TABLE_SLOT_READ_BIASES: usize = 1;
pub(crate) const TABLE_SLOT_WRITE_BIASES: usize = 2;
pub(crate) const TABLE_SLOT_PHYSICAL_PAGES: usize = 3;
pub(crate) const TABLE_SLOT_CODE_WATCH_STICKY: usize = 4;
pub(crate) const TABLE_SLOT_CODE_WATCH_NATIVE: usize = 5;
/// The one-lookup store table (`FastMapStorage::store_biases`, design D1).
pub(crate) const TABLE_SLOT_STORE_BIASES: usize = 6;
/// The store-stub pad's published entry addresses (design D4), one slot per stub in the pad's
/// canonical order; emitted sites `call qword [r15 + slot]` straight through them. The layout
/// helpers below are the single source of the (family, width, cpl) -> slot mapping — the pad
/// emitter records offsets in the identical order.
pub(crate) const TABLE_SLOT_STORE_STUBS: usize = 7;
/// 3 mode13 stubs (byte/word/dword) + 6 slow stubs (3 widths x cpl0/cpl3) + 8 x87 resolve
/// stubs (word/dword/qword/tbyte x cpl0/cpl3).
pub(crate) const STORE_STUB_COUNT: usize = 3 + 6 + 8;

/// Slot of the mode13 fast stub for a GPR store width (byte 0 / word 1 / dword 2).
pub(crate) const fn store_stub_slot_m13(width_index: usize) -> usize {
    TABLE_SLOT_STORE_STUBS + width_index
}

/// Slot of the slow store stub for a GPR store width and privilege arm.
pub(crate) const fn store_stub_slot_slow(width_index: usize, cpl3: bool) -> usize {
    TABLE_SLOT_STORE_STUBS + 3 + width_index * 2 + cpl3 as usize
}

/// Slot of the x87 resolve-only stub (word 0 / dword 1 / qword 2 / tbyte 3).
pub(crate) const fn store_stub_slot_x87(width_index: usize, cpl3: bool) -> usize {
    TABLE_SLOT_STORE_STUBS + 9 + width_index * 2 + cpl3 as usize
}

/// The one-lookup LOAD table (`FastMapStorage::load_biases`, load design D1). Appended AFTER
/// the store slots — the array lives at the CpuGsw TAIL precisely so growth like this cannot
/// move `pending_flags` or any hot interpreter field (the #719 layout-tax lesson; the pinned
/// offset tests are this commit's proof).
pub(crate) const TABLE_SLOT_LOAD_BIASES: usize = TABLE_SLOT_STORE_STUBS + STORE_STUB_COUNT;
/// The read-resolve stub pad's published entry addresses (load design D4). Three families:
///
/// * COUNTING stubs for the lean sites, per GPR width x cpl (6): resolve AND move the mode13
///   read lane on a mode13 success, so the site's slow join needs no completion bytes — the
///   restructure the L8 size swap forced (the first-cut inline mode13 arm + cold completion
///   made every read site ~40 bytes LARGER than the classic front; native mode13 reads exist
///   only under chained 13h — Mode X produces no read fills, review F3 — so they are stub-cold
///   by evidence, unlike the store side's doom-hot aperture writes).
/// * PARK-ONLY stubs for the Ret/Ret16/JmpMem trio, per cpl (2): park the bare kind, never
///   count — the trio's own deferred completion moves the lane after its CS-limit exit.
/// * x87 stubs, per cpl (2): park the `kind << 32 | linear` pack for the untouched x87
///   completion, never count.
pub(crate) const TABLE_SLOT_READ_STUBS: usize = TABLE_SLOT_LOAD_BIASES + 1;
pub(crate) const READ_STUB_COUNT: usize = 6 + 2 + 2;

/// Slot of the counting read stub for a GPR width (byte 0 / word 1 / dword 2) and privilege.
pub(crate) const fn read_stub_slot_counting(width_index: usize, cpl3: bool) -> usize {
    TABLE_SLOT_READ_STUBS + width_index * 2 + cpl3 as usize
}

/// Slot of the trio's park-only read stub for a privilege arm.
pub(crate) const fn read_stub_slot_park(cpl3: bool) -> usize {
    TABLE_SLOT_READ_STUBS + 6 + cpl3 as usize
}

/// Slot of the x87 read-resolve stub for a privilege arm.
pub(crate) const fn read_stub_slot_x87(cpl3: bool) -> usize {
    TABLE_SLOT_READ_STUBS + 8 + cpl3 as usize
}

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
        self.publish(TABLE_SLOT_STORE_BIASES, map.store_biases());
        self.publish(TABLE_SLOT_LOAD_BIASES, map.load_biases());
    }

    /// Publish the store-stub pad's entry addresses, in the pad's canonical order. Write-once
    /// like every slot: the pad is built once per `BlockCache` and never replaced, so a changed
    /// republish means a pad moved under live code and the debug_assert fires.
    pub(crate) fn publish_store_stubs(&mut self, addresses: [usize; STORE_STUB_COUNT]) {
        for (index, address) in addresses.into_iter().enumerate() {
            self.publish(TABLE_SLOT_STORE_STUBS + index, address);
        }
    }

    /// Publish the read-resolve stub pad's entry addresses (load design D4), in the pad's
    /// canonical order — the store pad's write-once contract verbatim.
    pub(crate) fn publish_read_stubs(&mut self, addresses: [usize; READ_STUB_COUNT]) {
        for (index, address) in addresses.into_iter().enumerate() {
            self.publish(TABLE_SLOT_READ_STUBS + index, address);
        }
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
