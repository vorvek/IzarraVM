// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Emitted memory sites load their table bases through R15, from `CpuGsw::native_table_slots`.
//! The pins here look for that load in the code bytes of a default block.

use super::jit_direct::{
    READ_ENTRY, STORE_ENTRY, prime_direct_memory_block, prime_direct_store_block,
    store_exit_program, successful_read_program,
};
use super::*;
use jit::direct::{TABLE_SLOT_LOAD_BIASES, TABLE_SLOT_STORE_BIASES, table_slot_offset};
use jit::encoder::{Encoder, Reg};

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn r15_load_bytes(dst: Reg, slot: usize) -> Vec<u8> {
    let mut e = Encoder::new();
    e.load_r64_disp32(dst, Reg::R15, table_slot_offset(slot));
    e.finish()
}

fn baked_base_bytes(dst: Reg, value: usize) -> Vec<u8> {
    let mut e = Encoder::new();
    e.mov_r64_imm64(dst, value as u64);
    e.finish()
}

/// A default store block reads the store-bias table base from the R15 slot. The proof that
/// nothing else changed is the byte comparison against the build that had both forms. The
/// `== 0` assertion only guards against a baked form coming back.
#[test]
fn store_block_loads_the_store_bias_base_through_r15() {
    let mut cpu = super::jit_direct::fresh();
    let mut bus = TestBus::with_memory(store_exit_program(0x4100));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);
    let compilation = jit::direct::compile(&mut cpu, STORE_ENTRY, true)
        .expect("the primed store block recompiles");
    let base = cpu
        .jit_fast_map
        .native_bases()
        .expect("fast map bases")
        .store_biases();
    let r15 = r15_load_bytes(Reg::RDI, TABLE_SLOT_STORE_BIASES);
    assert!(
        occurrences(&compilation.code, &r15) >= 1,
        "the store block must load the store-bias base from [r15 + slot]"
    );
    assert_eq!(
        occurrences(&compilation.code, &baked_base_bytes(Reg::RDI, base)),
        0,
        "the store-bias base must not be baked as an imm64"
    );
}

/// The read twin of the store pin, at the load-bias slot.
#[test]
fn read_block_loads_the_load_bias_base_through_r15() {
    let mut cpu = super::jit_direct::fresh();
    let mut bus = TestBus::with_memory(successful_read_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_memory_block(&mut cpu, &mut bus);
    let compilation =
        jit::direct::compile(&mut cpu, READ_ENTRY, true).expect("the primed read block recompiles");
    let base = cpu
        .jit_fast_map
        .native_bases()
        .expect("fast map bases")
        .load_biases();
    let r15 = r15_load_bytes(Reg::RDI, TABLE_SLOT_LOAD_BIASES);
    assert!(
        occurrences(&compilation.code, &r15) >= 1,
        "the read block must load the load-bias base from [r15 + slot]"
    );
    assert_eq!(
        occurrences(&compilation.code, &baked_base_bytes(Reg::RDI, base)),
        0,
        "the load-bias base must not be baked as an imm64"
    );
}
