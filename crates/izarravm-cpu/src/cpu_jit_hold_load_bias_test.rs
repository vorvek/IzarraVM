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

use jit::encoder::{Encoder, Reg, Xmm};

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn hoist_bytes() -> Vec<u8> {
    let mut e = Encoder::new();
    e.load_r64_disp32(
        Reg::RSI,
        Reg::R15,
        jit::direct::table_slot_offset(jit::direct::TABLE_SLOT_LOAD_BIASES),
    );
    e.finish()
}

fn flags_copy_bytes() -> Vec<u8> {
    let mut e = Encoder::new();
    e.mov_r32_r32(Reg::RBP, {
        #[cfg(target_os = "windows")]
        {
            Reg::RDX
        }
        #[cfg(not(target_os = "windows"))]
        {
            Reg::RSI
        }
    });
    e.finish()
}

#[cfg(target_os = "windows")]
fn rsi_save_bytes() -> Vec<u8> {
    let mut e = Encoder::new();
    e.store_r64_disp32(Reg::RSP, 160, Reg::RSI);
    e.finish()
}

#[cfg(target_os = "windows")]
fn rsi_restore_bytes() -> Vec<u8> {
    let mut e = Encoder::new();
    e.load_r64_disp32(Reg::RSI, Reg::RSP, 160);
    e.finish()
}

#[cfg(target_os = "windows")]
fn xmm6_restore_bytes() -> Vec<u8> {
    let mut e = Encoder::new();
    e.vmovupd_xmm_disp32(Xmm::XMM6, Reg::RSP, 168);
    e.finish()
}

fn compile_read_block(on: bool) -> jit::direct::Compilation {
    let mut cpu = super::jit_direct::fresh();
    cpu.jit_direct
        .rebuild_after_hold_load_bias_flip_for_test(on);
    let mut bus = TestBus::with_memory(super::jit_direct::successful_read_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    super::jit_direct::prime_direct_memory_block(&mut cpu, &mut bus);
    jit::direct::compile(&mut cpu, super::jit_direct::READ_ENTRY, true)
        .expect("the primed read block recompiles")
}

fn compile_store_block(on: bool) -> jit::direct::Compilation {
    let mut cpu = super::jit_direct::fresh();
    cpu.jit_direct
        .rebuild_after_hold_load_bias_flip_for_test(on);
    let mut bus = TestBus::with_memory(super::jit_direct::store_exit_program(0x4100));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    super::jit_direct::prime_direct_store_block(&mut cpu, &mut bus);
    jit::direct::compile(&mut cpu, super::jit_direct::STORE_ENTRY, true)
        .expect("the primed store block recompiles")
}

/// M10: an ON integer load block charges the vacuity lane. A store-only block does not.
#[test]
fn hold_load_bias_probes_count_rsi_arm_load_sites() {
    let off_load = compile_read_block(false);
    assert_eq!(
        off_load.hold_load_bias_probes(),
        0,
        "OFF never takes the RSI probe"
    );
    let on_load = compile_read_block(true);
    assert!(
        on_load.hold_load_bias_probes() > 0,
        "ON integer loads must charge the vacuity lane, got 0"
    );
    let on_store = compile_store_block(true);
    assert_eq!(
        on_store.hold_load_bias_probes(),
        0,
        "store-bias probes do not use RSI"
    );
    jit::direct::set_hold_load_bias_for_test(None);
}

/// M2: the table load sits above body_offset so hops inherit it.
#[test]
fn hoist_load_sits_above_body_offset() {
    let on = compile_read_block(true);
    let hoist = hoist_bytes();
    let body = on.body_offset();
    assert!(
        occurrences(&on.code[..body], &hoist) == 1,
        "the integer prologue must load RSI from the table slot"
    );
    assert_eq!(
        occurrences(&on.code[body..], &hoist),
        0,
        "a load-only body has no call-out reload; hops must not re-pay the table load"
    );
    let off = compile_read_block(false);
    assert_eq!(
        occurrences(&off.code, &hoist),
        0,
        "OFF emission must not touch RSI"
    );
    jit::direct::set_hold_load_bias_for_test(None);
}

/// M9 / M14: Windows save, then FLAGS copy, then table load.
#[test]
fn prologue_order_is_save_flags_then_table_load() {
    let on = compile_read_block(true);
    let hoist = hoist_bytes();
    let flags = flags_copy_bytes();
    let hoist_at = on
        .code
        .windows(hoist.len())
        .position(|w| w == hoist.as_slice())
        .expect("ON integer prologue emits the table load");
    let flags_at = on
        .code
        .windows(flags.len())
        .position(|w| w == flags.as_slice())
        .expect("prologue copies FLAGS_ARG into RBP");
    assert!(
        flags_at < hoist_at,
        "table load before FLAGS copy would clobber SysV RSI into RBP"
    );
    #[cfg(target_os = "windows")]
    {
        let save = rsi_save_bytes();
        let save_at = on
            .code
            .windows(save.len())
            .position(|w| w == save.as_slice())
            .expect("Windows integer ON saves host RSI");
        assert!(
            save_at < hoist_at,
            "table load before the save clobbers host RSI"
        );
    }
    jit::direct::set_hold_load_bias_for_test(None);
}

/// M15: integer ON restores RSI and does not restore XMM6.
#[cfg(target_os = "windows")]
#[test]
fn integer_on_epilogue_restores_rsi_only() {
    let on = compile_read_block(true);
    assert!(
        occurrences(&on.code, &rsi_restore_bytes()) >= 1,
        "integer ON must restore host RSI"
    );
    assert_eq!(
        occurrences(&on.code, &xmm6_restore_bytes()),
        0,
        "integer ON never saved XMM6-11; restoring them is an ABI break"
    );
    let off = compile_read_block(false);
    assert_eq!(
        occurrences(&off.code, &rsi_restore_bytes()),
        0,
        "OFF integer epilogue does not restore RSI"
    );
    jit::direct::set_hold_load_bias_for_test(None);
}

/// M4 / M11 / M15 pad half.
#[cfg(target_os = "windows")]
#[test]
fn x87_pad_omits_rsi_save_and_restores_rsi_on_bail_only() {
    jit::direct::set_hold_load_bias_for_test(Some(false));
    let off = jit::direct::emit_x87_reentry_pad();
    jit::direct::set_hold_load_bias_for_test(Some(true));
    let on = jit::direct::emit_x87_reentry_pad();
    let save = rsi_save_bytes();
    let restore = rsi_restore_bytes();
    let xmm6 = xmm6_restore_bytes();
    assert_eq!(
        occurrences(&off, &save),
        1,
        "OFF pad saves RSI on the success path"
    );
    assert_eq!(
        occurrences(&off, &restore),
        0,
        "OFF bail must not restore from a stale slot"
    );
    assert_eq!(
        occurrences(&on, &save),
        0,
        "ON pad must not overwrite host RSI in the slot"
    );
    assert_eq!(
        occurrences(&on, &restore),
        1,
        "ON bail must restore host RSI"
    );
    assert_eq!(
        occurrences(&on, &xmm6),
        0,
        "pad bail must not restore XMMs the TOP guard never saved"
    );
    jit::direct::set_hold_load_bias_for_test(None);
}

/// M1: two loads, two pages, one integer block. ON native matches the interpreter.
#[test]
fn on_arm_multi_page_loads_match_the_interpreter() {
    let mut native = super::jit_direct::fresh();
    native
        .jit_direct
        .rebuild_after_hold_load_bias_flip_for_test(true);
    let mut native_bus = TestBus::with_memory(super::jit_direct::successful_read_program());
    native_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;
    super::jit_direct::prime_direct_memory_block(&mut native, &mut native_bus);
    super::jit_direct::arm_read_fixture(&mut native);
    native.registers.eip = super::jit_direct::READ_ENTRY;
    native.elapsed_clocks = 0;
    native.timing_rem = 0;
    native.core_clocks_so_far = 0;
    native_bus.trace = BusTrace::default();
    super::jit_direct::drive(&mut native, &mut native_bus);

    let mut interp = super::jit_direct::fresh();
    interp.set_jit_auto_admit(false);
    let mut interp_bus = TestBus::with_memory(super::jit_direct::successful_read_program());
    interp_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    super::jit_direct::arm_read_fixture(&mut interp);
    interp.registers.eip = super::jit_direct::READ_ENTRY - 1;
    interp.elapsed_clocks = 0;
    interp.timing_rem = 0;
    interp.core_clocks_so_far = 0;
    super::jit_direct::drive(&mut interp, &mut interp_bus);

    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.eflags(), interp.eflags());
    jit::direct::set_hold_load_bias_for_test(None);
}
