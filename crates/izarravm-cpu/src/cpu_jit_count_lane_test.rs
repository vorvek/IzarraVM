// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Mutable COUNT lanes: the group-2 shift/rotate immediate byte (`0xC1 /0` ROL, `0xC1 /1` ROR,
//! `0xC1 /4..=7` the dword shifts, and `0xC0 /4` SHL r8 -- register forms, no prefixes) read out of
//! guest RAM on every execution instead of being baked into host code, so a guest patch of that
//! byte keeps the compiled block.
//!
//! This is L2 arm 2 of the 2026-08-19 duke re-profile, and it is the RE-TEST TRIGGER
//! `rotate_rows_enabled` names: duke3d patches the count byte of its group-2 shifts, and since the
//! `IZARRAVM_ROTATE_ROWS` default flip those sites are admitted, so every count patch kills a
//! compiled block. See `count_lane_for` for the admission argument and `emit_rotate_reg_lane` /
//! `emit_shift_lane` for the runtime three-way branch that is this slice's whole correctness cost.
//!
//! **The arm is default OFF.** Every positive fixture here forces it on through
//! `set_count_lanes_for_test`, which is thread-local, so a fixture that forgot would test the
//! refusal and call it a lowering. `count_lane_is_refused_on_the_default_arm` is the fixture that
//! proves the forcing is doing something.

use super::*;

/// The `IZARRAVM_COUNT_LANES` spelling table. `count_lanes_enabled` caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls -- hence the parse function is exercised directly.
#[test]
fn count_lanes_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_count_lanes_arm_for_test;
    assert!(!parse(Err(VarError::NotPresent)), "unset is the base");
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must be the base");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must be the slice");
    }
}

/// A typo must not silently run the base. See `parse_count_lanes_arm` for why guessing is worse
/// than failing: a leg that quietly ran the base would be read as the slice doing nothing.
#[test]
#[should_panic(expected = "names no arm")]
fn an_unrecognised_count_lanes_spelling_panics() {
    jit::direct::parse_count_lanes_arm_for_test(Ok("true".to_string()));
}

/// The two knobs are INDEPENDENT LEVERS, pinned as a test because the obvious simplification --
/// hang the count lane off `IZARRAVM_IMM8_LANES`, they are both one-byte lanes -- would destroy the
/// 2x2 the ladder needs (see `rotate_rows_enabled`'s cross-term paragraph). Forcing one arm must
/// leave the other's reading alone, in both directions.
#[test]
fn the_count_arm_and_the_imm8_arm_are_separate_levers() {
    jit::direct::set_count_lanes_for_test(Some(true));
    jit::direct::set_imm8_lanes_for_test(Some(false));
    assert!(jit::direct::count_lanes_enabled());
    assert!(!jit::direct::imm8_lanes_enabled());
    jit::direct::set_count_lanes_for_test(Some(false));
    jit::direct::set_imm8_lanes_for_test(Some(true));
    assert!(!jit::direct::count_lanes_enabled());
    assert!(jit::direct::imm8_lanes_enabled());
    jit::direct::set_count_lanes_for_test(None);
    jit::direct::set_imm8_lanes_for_test(None);
}
