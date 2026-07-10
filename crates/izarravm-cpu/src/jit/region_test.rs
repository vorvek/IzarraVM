// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

unsafe extern "C" fn stub_entry(
    _cpu: *mut crate::CpuGsw,
    _bus: *mut std::ffi::c_void,
    _ctx: *mut RegionCtx,
) -> i64 {
    0
}

fn stub_region(entry_lin: u32, d: bool) -> CompiledRegion {
    CompiledRegion {
        // A trivial valid buffer: a single RET (never called by these tests).
        buf: ExecutableBuffer::new(&[0xc3]).expect("W^X alloc on a supported host"),
        entry: stub_entry,
        ctx: Box::new(RegionCtx {
            step_fn: None,
            inline_step_fn: None,
            set_pending_add_fn: None,
            set_shift_flags_fn: None,
            charge_fetch_fn: None,
            bus_clocks_fn: None,
            line_live_fn: None,
            slots: Vec::new(),
            terminal_slot: 0,
            is_loop: true,
            entry_eip: 0,
            raw_clocks: 0,
            insn_count: 0,
            run_total_at_entry: 0,
            bus_at_run_start: 0,
            cap: 0,
            rem0: 0,
            scale_num: 1,
            scale_den: 1,
            d: true,
            exit: Default::default(),
            fault: None,
            halted: false,
            folded_raw_bus: 0,
            fold_bus_cost: 0,
            fetch_cost: 0,
            store_finish_fn: None,
            native_insn_count: 0,
            helper_exit_count: 0,
        }),
        entry_lin,
        d,
        phys_lo: entry_lin,
        phys_hi: entry_lin + 0x32,
        valid_epoch: 0,
        is_loop: true,
        mode_key: 0,
        has_native_fold: false,
        has_native_store: false,
    }
}

#[test]
fn install_returns_one_based_indices_and_get_round_trips() {
    let mut table = RegionTable::default();
    assert_eq!(table.len(), 0);
    let idx = table.install(stub_region(0x0011_0920, true));
    assert_eq!(idx.get(), 1);
    let region = table.get(idx).expect("installed region is retrievable");
    assert_eq!(region.entry_lin, 0x0011_0920);
    assert!(region.d);
    assert!(table.get(std::num::NonZeroU32::new(2).unwrap()).is_none());
    assert!(table.get_mut(idx).is_some());
}

#[test]
fn find_locates_a_region_by_its_decode_line_key() {
    let mut table = RegionTable::default();
    let idx = table.install(stub_region(0x0047_3DF8, true));
    assert_eq!(table.find(0x0047_3DF8, true), Some(idx));
    // Same address under the other D bit is a different key.
    assert_eq!(table.find(0x0047_3DF8, false), None);
    assert_eq!(table.find(0x0011_0920, true), None);
}
