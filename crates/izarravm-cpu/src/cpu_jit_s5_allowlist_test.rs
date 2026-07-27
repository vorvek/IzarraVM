// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The Tier 0 Word allowlist: byte-by-construction forms plus near JMP.
//!
//! These are the ONLY production-reachable surface of that slice on `main`. 16-bit code still
//! needs the continuation early-out gone, so on the pinned Quake corpus these forms are reachable
//! only through a 0x66 prefix in 32-bit code, which no compiler emits because the prefix has no
//! architectural effect on a byte-operand instruction. Byte identity on that corpus is therefore
//! an INERTNESS claim and not evidence, which makes these fixtures the whole argument.
//!
//! A sweep for `0x66` followed by any of these opcodes returned zero hits across the crate before
//! this file existed, so nothing here duplicates existing coverage.

use super::*;

/// Every byte-operand opcode the slice admits, one case per opcode.
///
/// One case each on purpose: these are separate `matches!` entries and separate classifier arms,
/// so a parameterised loop that silently dropped one would leave the rest green. The precedent is
/// the near/short Jcc pair, which exists side by side for the same reason.
///
/// The stream opens with three `inc` slots so the form under test is never the block's first
/// instruction, and the assertion is an EXACT count: `>= 3` is satisfied by the fillers alone
/// with the form under test refused.
#[test]
fn word_size_byte_forms_are_lowered() {
    let cases: &[(&str, &[u8])] = &[
        ("0x04 add al,imm8", &[0x66, 0x04, 0x05]),
        ("0x0c or al,imm8", &[0x66, 0x0c, 0x05]),
        ("0x14 adc al,imm8", &[0x66, 0x14, 0x05]),
        ("0x1c sbb al,imm8", &[0x66, 0x1c, 0x05]),
        ("0x24 and al,imm8", &[0x66, 0x24, 0x05]),
        ("0x2c sub al,imm8", &[0x66, 0x2c, 0x05]),
        ("0x34 xor al,imm8", &[0x66, 0x34, 0x05]),
        ("0x3c cmp al,imm8", &[0x66, 0x3c, 0x05]),
        ("0x80 /0 add r/m8,imm8", &[0x66, 0x80, 0xc1, 0x03]),
        ("0x84 test r/m8,r8", &[0x66, 0x84, 0xc0]),
        ("0x88 mov r/m8,r8", &[0x66, 0x88, 0xc4]),
        ("0x8a mov r8,r/m8", &[0x66, 0x8a, 0xd8]),
        ("0xa8 test al,imm8", &[0x66, 0xa8, 0x05]),
        ("0xb0 mov al,imm8", &[0x66, 0xb0, 0x07]),
        ("0xb7 mov bh,imm8", &[0x66, 0xb7, 0x07]),
        ("0xc6 /0 mov r/m8,imm8", &[0x66, 0xc6, 0xc1, 0x09]),
        ("0xf6 /0 test r/m8,imm8", &[0x66, 0xf6, 0xc1, 0x0f]),
    ];

    for &(label, form) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, 4,
            "{label}: the byte form must join the block rather than end it"
        );
        assert_eq!(
            compilation.span.guest_len,
            3 + form.len() as u16,
            "{label}: block extent"
        );
        // A byte form must never claim a wide access. The wide accessors feed
        // `has_wide_accesses` and the block-entry alignment precondition, and a byte access takes
        // no alignment guard at all.
        assert_eq!(compilation.word_reads, 0, "{label}: word reads");
        assert_eq!(compilation.word_stores, 0, "{label}: word stores");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// The allowlist boundary, and the test that stops the slice widening by accident.
///
/// Each of these is the DWORD sibling of an admitted byte form, and its kind hard-codes Dword
/// with no width field, so admitting it would be a miscompile rather than a missed lowering. If a
/// later edit replaces the opcode list with a range, every positive fixture above still passes
/// and only this one fails.
///
/// `0x01` and `0x31` are the sharpest: `AluReg` DOES carry a width, but `emit_alu_preloaded`'s
/// Word branch ignores `op`, hard-codes SUB, and writes the result to a scratch register instead
/// of the destination. That is correct only for the CMP forms `0x39` and `0x3b` already admitted,
/// so admitting ADD or XOR would leave the destination unchanged and set flags from a
/// subtraction, silently, because the guarding `debug_assert_eq!` is compiled out of a release
/// build.
#[test]
fn word_size_dword_siblings_stay_refused() {
    let cases: &[(&str, &[u8])] = &[
        ("0x05 add eax,imm", &[0x66, 0x05, 0x34, 0x12]),
        ("0x81 /0 add r/m,imm16", &[0x66, 0x81, 0xc1, 0x34, 0x12]),
        ("0x83 /0 add r/m,imm8", &[0x66, 0x83, 0xc1, 0x03]),
        ("0x85 test r/m,r", &[0x66, 0x85, 0xc0]),
        ("0x8d lea", &[0x66, 0x8d, 0x40, 0x10]),
        ("0xa9 test eax,imm", &[0x66, 0xa9, 0x34, 0x12]),
        ("0xb8 mov eax,imm", &[0x66, 0xb8, 0x34, 0x12]),
        ("0xc7 /0 mov r/m,imm", &[0x66, 0xc7, 0xc1, 0x34, 0x12]),
        ("0xf7 /0 test r/m,imm", &[0x66, 0xf7, 0xc1, 0x34, 0x12]),
        ("0x01 add r/m,r", &[0x66, 0x01, 0xc1]),
        ("0x31 xor r/m,r", &[0x66, 0x31, 0xc1]),
    ];

    for &(label, form) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, 3,
            "{label}: must stay refused at Word size, so the block is the three fillers"
        );
    }
}

/// A Word near JMP rides the control-target clamp, exactly as the Word CALL does.
///
/// The taken target is baked as an unmasked delta from the block entry while the interpreter
/// masks it to 16 bits, so the clamp refusing any Word target above 0xFFFF is what makes the bake
/// correct. `static_control_target` already matched `Jmp` before this slice, so the clamp needed
/// no edit; this pins that it genuinely applies to the newly admitted form.
///
/// `flat_fixture` rather than the 16-bit-stack helper: `Jmp` is not `uses_stack()`, so the
/// stack-width matrix passes it through on a 32-bit stack. Using the 16-bit-stack fixture would
/// still pass and would hide that asymmetry from the next reader.
#[test]
fn a_word_near_jmp_above_the_wrap_is_refused_while_the_same_block_below_it_compiles() {
    // inc eax; inc ecx; inc edx; 66 e9 10 00 (jmp +0x10 at Word operand size).
    const CODE: [u8; 7] = [0x40, 0x41, 0x42, 0x66, 0xe9, 0x10, 0x00];
    const HIGH: u32 = 0x1_0100;

    let (mut cpu, mut bus) = flat_fixture(ENTRY, &CODE);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let low = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(low.span.instructions, 4, "control: the jmp must lower");
    assert!(
        low.successors[0].is_some(),
        "a lowered Jmp records its static target"
    );

    // The same stream at an entry whose Word target crosses the wrap. The block still compiles,
    // from the slots ahead of the refused terminal, so the assertion is on SHAPE and not on a
    // refusal to compile at all.
    let (mut high_cpu, mut high_bus) = flat_fixture(HIGH, &CODE);
    warm(
        &mut high_cpu,
        &mut high_bus,
        &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3],
    );
    let high = compiled(jit::direct::compile(&mut high_cpu, HIGH, true));
    assert_eq!(
        high.span.instructions, 3,
        "a Word jmp whose target crosses the 16-bit wrap must be refused"
    );

    // The Dword control at the SAME high entry, so a clamp leaking into the 32-bit path fails
    // here rather than passing unnoticed.
    const DWORD_CODE: [u8; 8] = [0x40, 0x41, 0x42, 0xe9, 0x10, 0x00, 0x00, 0x00];
    let (mut wide, mut wide_bus) = flat_fixture(HIGH, &DWORD_CODE);
    warm(
        &mut wide,
        &mut wide_bus,
        &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3],
    );
    let wide_block = compiled(jit::direct::compile(&mut wide, HIGH, true));
    assert_eq!(
        wide_block.span.instructions, 4,
        "the clamp must not apply at Dword operand size"
    );
}
