// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The R15 table-bases emission A/B (`dev_docs/2026-08-07-r15-table-bases-design.md`), split
//! from `cpu_jit_direct_test.rs` for the source-line ceiling; it borrows that battery's
//! store-driving helpers.

use super::jit_direct::{
    arm_store_fixture, drive, fresh, prime_direct_store_block, store_exit_program,
};
use super::*;

/// The R15 table-bases A/B: both arms run the same store program to the same guest result, and
/// the R15 arm's emission is strictly smaller (each baked 10-byte table imm64 becomes a 7-byte
/// R15-relative load). The size assertion is what proves the arm actually flipped emission — the
/// non-vacuity rule — and the landed store is the correctness half; it lands either way,
/// natively or via an exit's re-run.
#[test]
fn r15_table_arm_shrinks_the_store_block_and_lands_identically() {
    let target = 0x4100u32;
    let mut emitted = [0u64; 2];
    for (slot, r15_on) in [(0usize, false), (1, true)] {
        let mut cpu = fresh();
        cpu.jit_direct.r15_tables = r15_on;
        let mut bus = TestBus::with_memory(store_exit_program(target));
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        prime_direct_store_block(&mut cpu, &mut bus);
        emitted[slot] = cpu.jit_direct.total_live_code_len_for_test();
        assert_ne!(
            emitted[slot], 0,
            "r15_on={r15_on}: a block must have installed"
        );
        arm_store_fixture(&mut cpu);
        drive(&mut cpu, &mut bus);
        assert_eq!(
            &bus.memory[target as usize..target as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "r15_on={r15_on}: the store must land"
        );
    }
    assert!(
        emitted[1] < emitted[0],
        "the R15 arm must emit strictly less: imm64 arm {} bytes, R15 arm {} bytes",
        emitted[0],
        emitted[1],
    );
}
