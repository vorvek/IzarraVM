// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_DIRECT_ALIGN_TEST_AL`: `emit_alignment_test` uses `test al, mask; jnz`. DEFAULT OFF.
//!
//! Commit 1 of the slice is the knob, the spelling table, the thread-local override, and the
//! vacuity lane. Emission is unchanged: `align_test_al_sites` is zero on both arms until commit 2.

use super::*;

/// The `IZARRAVM_DIRECT_ALIGN_TEST_AL` spelling table. The knob caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process
/// and never in an order the harness controls -- hence the parse function is exercised directly.
#[test]
fn align_test_al_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_align_test_al_arm_for_test;
    assert!(
        !parse(Err(VarError::NotPresent)),
        "unset must name the OFF arm: this knob ships default OFF"
    );
    assert!(
        !parse(Ok(String::new())),
        "the empty string is the OFF arm, the same arm as unset"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the off arm");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the on arm");
    }
}

/// A typo must PANIC rather than silently running the default. A mistyped ladder leg that fell
/// through would be read as "the arm I asked for changed nothing".
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_ALIGN_TEST_AL")]
fn a_mistyped_align_test_al_arm_panics() {
    let _ = jit::direct::parse_align_test_al_arm_for_test(Ok("yes".to_string()));
}

/// THE DEFAULT PIN. Reads the AMBIENT knob so the suite is runnable on both arms.
#[test]
fn align_test_al_ships_off_by_default() {
    jit::direct::set_align_test_al_for_test(None);
    let ambient = std::env::var("IZARRAVM_DIRECT_ALIGN_TEST_AL");
    let expected = jit::direct::parse_align_test_al_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::align_test_al_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_ALIGN_TEST_AL={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            !expected,
            "IZARRAVM_DIRECT_ALIGN_TEST_AL must default OFF until a ladder prices the cheap form"
        );
    }
}
