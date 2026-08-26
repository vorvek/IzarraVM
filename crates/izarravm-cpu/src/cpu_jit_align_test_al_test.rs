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

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn compile_read_block(on: bool) -> jit::direct::Compilation {
    jit::direct::set_align_test_al_for_test(Some(on));
    let mut cpu = super::jit_direct::fresh();
    let mut bus = TestBus::with_memory(super::jit_direct::successful_read_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    super::jit_direct::prime_direct_memory_block(&mut cpu, &mut bus);
    jit::direct::compile(&mut cpu, super::jit_direct::READ_ENTRY, true)
        .expect("the primed read block recompiles")
}

fn compile_store_block(on: bool) -> jit::direct::Compilation {
    jit::direct::set_align_test_al_for_test(Some(on));
    let mut cpu = super::jit_direct::fresh();
    let mut bus = TestBus::with_memory(super::jit_direct::store_exit_program(0x4100));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    super::jit_direct::prime_direct_store_block(&mut cpu, &mut bus);
    jit::direct::compile(&mut cpu, super::jit_direct::STORE_ENTRY, true)
        .expect("the primed store block recompiles")
}

const LEAD: [u8; 2] = [0x89, 0xf6];
const TAIL: [u8; 2] = [0x89, 0xff];

fn enveloped(body: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let entry = super::jit_direct::READ_ENTRY;
    let mut code = LEAD.to_vec();
    let mut starts = vec![entry];
    starts.push(entry + code.len() as u32);
    code.extend_from_slice(body);
    starts.push(entry + code.len() as u32);
    code.extend_from_slice(&TAIL);
    code.push(0xf4);
    let mut memory = vec![0; 0x2000];
    memory[(entry - 1) as usize] = 0x90;
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(&code);
    memory[0x400..0x404].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    memory[0x420] = 0xa5;
    (memory, starts)
}

fn byte_only_program() -> (Vec<u8>, Vec<u32>) {
    enveloped(&[0x8a, 0x04, 0x33]) // mov al,[ebx+esi]
}

fn word_only_program() -> (Vec<u8>, Vec<u32>) {
    enveloped(&[0x0f, 0xb7, 0x0b]) // movzx ecx, word [ebx]
}

fn compile_custom_read(on: bool, program: (Vec<u8>, Vec<u32>)) -> jit::direct::Compilation {
    jit::direct::set_align_test_al_for_test(Some(on));
    let (memory, starts) = program;
    let mut cpu = super::jit_direct::fresh();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_jit_auto_admit(true);
    super::jit_direct::arm_read_fixture(&mut cpu);
    for linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    for page in [0u32, 0x400] {
        let read = bus
            .direct_page(page, BusAccessKind::DataRead)
            .unwrap()
            .unwrap();
        cpu.jit_fast_map.populate_read(
            page,
            page,
            read,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(page),
        );
    }
    match jit::direct::compile(&mut cpu, super::jit_direct::READ_ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the custom read block was a structural reject")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("the custom read block asked for a retry"),
    }
}

const DWORD_TEST_AL: [u8; 3] = [0xf6, 0xc0, 0x03];
const WORD_TEST_AL: [u8; 3] = [0xf6, 0xc0, 0x01];
const OFF_DWORD_ALIGN: [u8; 14] = [
    0x89, 0xc2, // mov edx, eax
    0x81, 0xe2, 0x03, 0x00, 0x00, 0x00, // and edx, 3
    0x81, 0xfa, 0x00, 0x00, 0x00, 0x00, // cmp edx, 0
];

/// M4 / M10: four dword loads shrink by 44 bytes and take the cheap form four times.
#[test]
fn align_test_al_density_gate_on_the_read_fixture() {
    let off = compile_read_block(false);
    let on = compile_read_block(true);
    assert_eq!(
        off.code.len().saturating_sub(on.code.len()),
        44,
        "four dword sites must shrink by 11 bytes each"
    );
    assert_eq!(
        occurrences(&on.code, &DWORD_TEST_AL),
        4,
        "ON must emit test al, 3 four times"
    );
    assert_eq!(
        occurrences(&off.code, &DWORD_TEST_AL),
        0,
        "OFF must not emit test al, 3 in the block body"
    );
    assert_eq!(occurrences(&off.code, &OFF_DWORD_ALIGN), 4);
    assert_eq!(occurrences(&on.code, &OFF_DWORD_ALIGN), 0);
    assert_eq!(off.align_test_al_sites(), 0);
    assert_eq!(on.align_test_al_sites(), 4);
    jit::direct::set_align_test_al_for_test(None);
}

/// M6: a byte-only block must not grow and must not charge the vacuity lane.
#[test]
fn align_test_al_skips_byte_loads() {
    let off = compile_custom_read(false, byte_only_program());
    let on = compile_custom_read(true, byte_only_program());
    assert_eq!(
        on.code.len(),
        off.code.len(),
        "Byte still skips the alignment helper"
    );
    assert_eq!(off.align_test_al_sites(), 0);
    assert_eq!(on.align_test_al_sites(), 0);
    jit::direct::set_align_test_al_for_test(None);
}

/// M1 word half: Word mask is 1, not 3.
#[test]
fn align_test_al_word_mask_is_one() {
    let on = compile_custom_read(true, word_only_program());
    assert_eq!(occurrences(&on.code, &WORD_TEST_AL), 1);
    assert_eq!(occurrences(&on.code, &DWORD_TEST_AL), 0);
    assert_eq!(on.align_test_al_sites(), 1);
    jit::direct::set_align_test_al_for_test(None);
}

/// M8: ON store alignment is the 9-byte cheap pair, not mov ecx,eax then test al.
#[test]
fn align_test_al_store_does_not_write_scratch() {
    let on = compile_store_block(true);
    let cheap = [0xf6, 0xc0, 0x03, 0x0f, 0x85];
    assert!(
        occurrences(&on.code, &cheap) >= 1,
        "ON store must emit test al, 3; jnz near"
    );
    let mov_ecx_then_test = [0x89, 0xc1, 0xf6, 0xc0, 0x03];
    assert_eq!(
        occurrences(&on.code, &mov_ecx_then_test),
        0,
        "ON must not mov ecx, eax immediately before the cheap test"
    );
    jit::direct::set_align_test_al_for_test(None);
}

/// I8: OFF dword-load helper bytes stay the HEAD four-instruction form.
#[test]
fn align_test_al_off_arm_keeps_the_four_instruction_helper() {
    let off = compile_read_block(false);
    assert_eq!(occurrences(&off.code, &OFF_DWORD_ALIGN), 4);
    assert_eq!(occurrences(&off.code, &DWORD_TEST_AL), 0);
    assert_eq!(off.align_test_al_sites(), 0);
    jit::direct::set_align_test_al_for_test(None);
}
