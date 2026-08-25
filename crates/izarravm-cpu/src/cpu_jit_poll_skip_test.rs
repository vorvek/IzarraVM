// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_DIRECT_POLL_SKIP` (GP2 call-out-site poll skip), unit-level tests that do not need a
//! real bus. The engagement/mechanism fixtures that need `CpuBus::callout_poll_skip`'s real body
//! live in `izarravm-machine`'s `machine_direct_poll_skip_test.rs` (design BLOCKER D: `TestBus`
//! must not implement that method).

use super::*;

/// **M-28.** The `IZARRAVM_DIRECT_POLL_SKIP` spelling table: unset and `""` name the SAME arm
/// (the default, OFF while under evaluation) -- the `IZARRAVM_CHAIN_ENTRY_CHECK` /
/// `IZARRAVM_JCC_SHADOW` shape, deliberately NOT `IZARRAVM_ATA_POLL_SKIP`'s (design §6).
#[test]
fn direct_poll_skip_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_direct_poll_skip_arm_for_test;
    assert!(
        !parse(Err(VarError::NotPresent)),
        "unset must name the OFF arm: this knob ships default OFF"
    );
    assert!(
        !parse(Ok(String::new())),
        "the empty string must name the SAME arm as unset -- the default, deliberately NOT \
         ATA's inverted shape"
    );
    for off in ["0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the OFF arm");
    }
    for on in ["1", "on", "ON", " On ", "poll", "POLL"] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the ON arm");
    }
}

/// A mistyped ladder leg must PANIC rather than silently run the default -- the one wrong
/// conclusion an arm ladder exists to avoid.
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_POLL_SKIP")]
fn a_mistyped_direct_poll_skip_arm_panics() {
    let _ = jit::direct::parse_direct_poll_skip_arm_for_test(Ok("yes".to_string()));
}

/// Non-UTF-8 is not a spelling of either arm -- reaches the panic, not the unset silence.
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_POLL_SKIP")]
fn non_utf8_direct_poll_skip_arm_panics() {
    let _ = jit::direct::parse_direct_poll_skip_arm_for_test(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("x"),
    )));
}

/// THE DEFAULT PIN: with the ambient env var read exactly as `direct_poll_skip_armed` reads it,
/// the process-wide OnceLock reading must agree with the spelling table, and with the variable
/// unset the arm must be OFF. Reads the AMBIENT knob deliberately (on the `jcc_shadow_ships_off_
/// by_default` model) so the suite stays runnable on either arm.
#[test]
fn direct_poll_skip_ships_off_by_default() {
    let ambient = std::env::var("IZARRAVM_DIRECT_POLL_SKIP");
    let expected = jit::direct::parse_direct_poll_skip_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::direct_poll_skip_armed(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_POLL_SKIP={ambient:?}"
    );
    // GP2 poll-skip revision review N5: `ambient.is_err()` alone is imprecise here -- it reads as
    // covering every `VarError`, but `NotUnicode` would already have panicked one line up (inside
    // `parse_direct_poll_skip_arm_for_test`, pinned by `non_utf8_direct_poll_skip_arm_panics`
    // above), so this branch is only ever reached for `NotPresent` in practice. Spell that out
    // instead of the broader, misleading `is_err()`.
    if matches!(ambient, Err(std::env::VarError::NotPresent)) {
        assert!(
            !expected,
            "IZARRAVM_DIRECT_POLL_SKIP must default OFF until a ladder prices the poll-skip arm"
        );
    }
}
