// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `0xE4` IN AL,imm8 -- the `PortReadAlImm8` call-out, behind `IZARRAVM_DIRECT_IN_IMM8_CALLOUT`
//! (`dev_docs/specs/2026-08-27-gp2-in-imm8-callout-design.md` rev 3, gp2's B2 residue).
//!
//! This file covers the KNOB and the `classify` ARM: whether `0xE4` reaches the compile walk at
//! all, and with what `port` baked into the slot. The HELPER's own contract (status encoding,
//! exact charge, zero-partial-effects, the privilege refusals, the engagement counter) is covered
//! in `cpu_jit_callout_test.rs`, beside `PortReadAlDx`'s equivalent fixtures, per rev 3 §8.2's
//! placement.
//!
//! **Every fixture here states its arm through `set_direct_in_imm8_callout_for_test`, in both
//! directions**, for `cpu_jit_test_word_row_test.rs`'s reason: the default is OFF, so a positive
//! fixture that read the ambient knob would be testing the refusal and calling it a lowering, and
//! a refusal fixture that inherited the arm would go vacuous the day the default moves.

use super::*;

const ENTRY: u32 = 0x401;
const STACK_TOP: u32 = 0x4000;
/// A port that fits the imm8 encoding (`<= 0xff`) and is not `0`, so a `port == 0` bug cannot
/// hide behind a zero-initialized field.
const PORT: u8 = 0x40;

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu.set_eip(ENTRY);
    cpu
}

/// Select the arm for this thread and PROVE the selection took.
fn select_in_imm8_callout(enabled: bool) {
    jit::direct::set_direct_in_imm8_callout_for_test(Some(enabled));
    assert_eq!(
        jit::direct::direct_in_imm8_callout_armed(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_DIRECT_IN_IMM8_CALLOUT"
    );
}

// ---------------------------------------------------------------------------------------------
// The knob
// ---------------------------------------------------------------------------------------------

/// THE DEFAULT PIN. Catches a flip of `parse_direct_in_imm8_callout_arm`'s `NotPresent` arm: this
/// row is default-off and a default that moved without a ladder would change every shipped
/// binary's admission silently.
#[test]
fn in_imm8_callout_ships_off_by_default() {
    jit::direct::set_direct_in_imm8_callout_for_test(None);
    let ambient = std::env::var("IZARRAVM_DIRECT_IN_IMM8_CALLOUT");
    let expected = jit::direct::parse_direct_in_imm8_callout_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::direct_in_imm8_callout_armed(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_IN_IMM8_CALLOUT={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            !expected,
            "IZARRAVM_DIRECT_IN_IMM8_CALLOUT must default OFF; the row has not been priced on a \
             wall ladder that authorized a flip"
        );
    }
}

/// The spelling table (mutant table gate 1): unset and `""` must BOTH parse `false`, matching
/// `IZARRAVM_TEST_WORD_ROWS`'s convention rather than `IZARRAVM_ATA_POLL_SKIP`'s inverted one.
///
/// Catches: a `_ => false` fallthrough replacing the panic (a mistyped ladder leg would silently
/// run the base and be read as "the slice under test changed nothing"), and a spelling that
/// accidentally inverted `""` against unset.
#[test]
fn in_imm8_callout_spelling_table_names_both_arms() {
    use std::env::VarError;
    let unset = jit::direct::parse_direct_in_imm8_callout_arm_for_test(Err(VarError::NotPresent));
    let empty = jit::direct::parse_direct_in_imm8_callout_arm_for_test(Ok(String::new()));
    assert!(
        !unset,
        "unset must name the OFF arm: this row is default-off"
    );
    assert_eq!(
        unset, empty,
        "unset and \"\" must name the SAME arm (the TEST_WORD_ROWS convention), not \
         IZARRAVM_ATA_POLL_SKIP's inverted one"
    );
    for off in ["0", "off", "OFF", " off ", "Off"] {
        assert!(
            !jit::direct::parse_direct_in_imm8_callout_arm_for_test(Ok(off.to_string())),
            "{off:?} must name the off arm"
        );
    }
    for on in ["1", "on", "ON", " on ", "On"] {
        assert!(
            jit::direct::parse_direct_in_imm8_callout_arm_for_test(Ok(on.to_string())),
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "poll", "imm8"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_direct_in_imm8_callout_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_DIRECT_IN_IMM8_CALLOUT={typo:?} names no arm and must panic rather than \
             silently running the base"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The `classify` arm
// ---------------------------------------------------------------------------------------------

/// Build `mov esi,esi` / `in al,PORT` / `mov edi,edi` at `ENTRY`, mid-block exactly as
/// `cpu_jit_callout_test.rs`'s `slot_block` does for `0xEC`, and try to compile it.
fn compile_imm8_block(port: u8) -> Option<jit::direct::Compilation> {
    let code = [0x89, 0xf6, 0xe4, port, 0x89, 0xff];
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.registers.set_esp(STACK_TOP);
    for &linear in &[ENTRY, ENTRY + 2, ENTRY + 4] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation),
        jit::direct::CompileOutcome::StructuralReject(_)
        | jit::direct::CompileOutcome::Retry(_) => None,
    }
}

/// With the knob OFF, `0xE4` must stay a barrier -- byte-identical to the pre-slice tree. This is
/// the A/B base every ladder leg on this row is read against.
///
/// Catches: a classify arm that forgot its `direct_in_imm8_callout_armed()` guard, i.e. the row
/// shipping admitted while the knob says off, which would make the gate-OFF leg disagree with
/// main and destroy the base.
#[test]
fn in_imm8_callout_stays_a_barrier_with_the_gate_off() {
    select_in_imm8_callout(false);
    assert!(
        compile_imm8_block(PORT).is_none(),
        "0xE4 must not compile into a block with the gate off"
    );
    jit::direct::set_direct_in_imm8_callout_for_test(None);
}

/// With the knob ON, `0xE4` joins the block as a `PortReadAlImm8` call-out slot carrying the
/// instruction's own immediate port.
///
/// Catches: a missing or wrongly-keyed allowlist term (the block would still refuse to compile);
/// the port immediate lost or wrong (`callout_port_slots` would still be right, since the DX and
/// imm8 helpers share the same class predicate, but the emitted slot would serve the wrong port --
/// covered end-to-end by `imm8_call_out_matches_the_interpreter_mid_block` in
/// `cpu_jit_callout_test.rs`); and the Word-size allowlist term (exercised separately below).
#[test]
fn in_imm8_callout_joins_the_block_with_the_gate_on() {
    select_in_imm8_callout(true);
    let compilation = compile_imm8_block(PORT).expect("0xE4 must compile with the gate on");
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must extend THROUGH the call-out, not stop at it"
    );
    assert_eq!(
        compilation.callout_slots, 1,
        "the call-out must be counted for the budget bound"
    );
    assert_eq!(
        compilation.callout_port_slots, 1,
        "PortReadAlImm8 is in the PORT class, exactly like PortReadAlDx"
    );
    jit::direct::set_direct_in_imm8_callout_for_test(None);
}

/// `classify` admits `0xE4` for ANY immediate port, unconditionally (rev 3 §1.1, ROUND-2 item 5) --
/// there is no per-port allowlist to consult. Two arbitrary ports, one of them the PIT counter
/// port the design's own port-class table names.
#[test]
fn in_imm8_callout_admits_any_port_unconditionally() {
    select_in_imm8_callout(true);
    for port in [0x00u8, 0x40, 0x61, 0xff] {
        assert!(
            compile_imm8_block(port).is_some(),
            "port {port:#04x} must be admitted -- the arm is not port-gated"
        );
    }
    jit::direct::set_direct_in_imm8_callout_for_test(None);
}

/// The Word-size allowlist term: `0xE4` is operand-size-invariant (the interpreter's arm always
/// reads and writes a byte), so a 16-bit segment must admit it too, on the same knob.
///
/// Catches: the new term being written against the wrong opcode, or against `insn.operand_size`
/// instead of `insn.opcode`.
#[test]
fn in_imm8_callout_joins_a_sixteen_bit_block_with_the_gate_on() {
    let code = [0x89, 0xf6, 0xe4, PORT, 0x89, 0xff];
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(STACK_TOP);
    cpu.set_eip(ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    for &linear in &[ENTRY, ENTRY + 2, ENTRY + 4] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }

    select_in_imm8_callout(false);
    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, false),
        jit::direct::CompileOutcome::StructuralReject(_) | jit::direct::CompileOutcome::Retry(_)
    ));

    select_in_imm8_callout(true);
    match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => {
            assert_eq!(compilation.span.instructions, 3);
            assert_eq!(compilation.callout_port_slots, 1);
        }
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("0xE4 must compile in a 16-bit segment with the gate on: structurally rejected")
        }
        jit::direct::CompileOutcome::Retry(_) => {
            panic!("0xE4 must compile in a 16-bit segment with the gate on: retry requested")
        }
    }
    jit::direct::set_direct_in_imm8_callout_for_test(None);
}
