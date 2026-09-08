// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The specialized word-TEST helper is retained only for the synthetic
//! unrepresentable-address fallback. Classifier coverage lives in `direct_test`.

use super::*;

#[test]
fn test_word_fallback_helper_publication_is_host_state_and_clone_clears_it() {
    let mut bus = sixteen_bit_bus(vec![0; 0x2000]);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    let (table, helper) = jit::direct::CallOutTable::publish(&mut bus);
    assert_eq!(table.bus, (&mut bus as *mut TestBus).cast::<()>());
    assert_ne!(helper, 0);
    assert_ne!(helper, table.interpret_one);
    cpu.native_callout = table;
    cpu.native_table_slots.interpret_test_word = helper;
    let copy = cpu.clone();
    assert_eq!(copy, cpu);
    assert!(copy.native_callout.bus.is_null());
    assert_eq!(copy.native_table_slots.interpret_test_word, 0);
}
