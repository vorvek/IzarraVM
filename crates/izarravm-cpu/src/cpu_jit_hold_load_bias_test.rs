// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_DIRECT_HOLD_LOAD_BIAS`: integer Direct blocks hold `load_biases` in RSI so each
//! load-bias probe is one indexed load. DEFAULT OFF.
//!
//! Commit 1 of the slice is the knob, the spelling table, the thread-local override, and the
//! vacuity lane. Emission is unchanged: `hold_load_bias_probes` is zero on both arms until
//! commit 2.

use super::*;

/// The `IZARRAVM_DIRECT_HOLD_LOAD_BIAS` spelling table. The knob caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process
/// and never in an order the harness controls -- hence the parse function is exercised directly.
#[test]
fn hold_load_bias_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_hold_load_bias_arm_for_test;
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

/// A typo must PANIC rather than silently run the default. A mistyped ladder leg that fell
/// through would be read as "the arm I asked for changed nothing".
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_HOLD_LOAD_BIAS")]
fn a_mistyped_hold_load_bias_arm_panics() {
    let _ = jit::direct::parse_hold_load_bias_arm_for_test(Ok("yes".to_string()));
}

/// THE DEFAULT PIN. Reads the AMBIENT knob so the suite is runnable on both arms.
#[test]
fn hold_load_bias_ships_off_by_default() {
    jit::direct::set_hold_load_bias_for_test(None);
    let ambient = std::env::var("IZARRAVM_DIRECT_HOLD_LOAD_BIAS");
    let expected = jit::direct::parse_hold_load_bias_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::hold_load_bias_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_HOLD_LOAD_BIAS={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            !expected,
            "IZARRAVM_DIRECT_HOLD_LOAD_BIAS must default OFF until a ladder prices the hoist"
        );
    }
}

/// Vacuity lane is zero while emission is still HEAD. True on both arms until commit 2.
#[test]
fn hold_load_bias_probes_are_zero_while_emission_is_unchanged() {
    jit::direct::set_hold_load_bias_for_test(Some(true));
    let mut cpu = super::jit_direct::fresh();
    let mut bus = TestBus::with_memory(super::jit_direct::store_exit_program(0x4100));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    super::jit_direct::prime_direct_store_block(&mut cpu, &mut bus);
    let compilation = jit::direct::compile(&mut cpu, super::jit_direct::STORE_ENTRY, true)
        .expect("the primed store block recompiles");
    assert_eq!(
        compilation.hold_load_bias_probes(),
        0,
        "commit 1 does not emit the RSI probe, so the vacuity lane stays zero on ON"
    );
    jit::direct::set_hold_load_bias_for_test(None);
}
