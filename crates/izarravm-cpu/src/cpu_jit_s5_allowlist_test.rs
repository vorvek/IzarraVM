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
/// `0x01` and `0x31` remain refused for a reason that has SOFTENED rather than gone: the Word
/// branch of `emit_alu_preloaded` no longer hard-codes SUB and no longer drops the result into a
/// scratch register -- it handles the whole non-carry op set with a `mov_r16_r16` write-back, as
/// the `0x83` slice needed. They stay out because nothing has measured them, not because they
/// would miscompile; admitting an unmeasured opcode is a formation change with no census row to
/// attribute it to.
///
/// Two opcodes have moved OUT of this list as the rejected-row campaign measured them, and each
/// left a differently-shaped remainder behind:
/// * `0x83` -> `word_size_0x83_register_forms_are_lowered` and
///   `word_size_memory_immediate_forms_are_lowered`. Only its two CARRY sub-ops stay refused.
/// * `0xc7` -> `word_size_memory_immediate_forms_are_lowered` for the memory form. Its REGISTER
///   form is still refused, but by an arm inside the classifier rather than by this list, so it
///   has its own test (`the_word_size_0xc7_register_form_stays_refused`) and must NOT be asserted
///   here: a case in this table would keep passing if the allowlist entry were reverted, which is
///   the opposite of what this table is for.
///
/// `0xb8..=0xbf` stays, and it is now the only `MovImm` producer this list still guards.
#[test]
fn word_size_dword_siblings_stay_refused() {
    let cases: &[(&str, &[u8])] = &[
        ("0x05 add eax,imm", &[0x66, 0x05, 0x34, 0x12]),
        ("0x81 /0 add r/m,imm16", &[0x66, 0x81, 0xc1, 0x34, 0x12]),
        ("0x85 test r/m,r", &[0x66, 0x85, 0xc0]),
        ("0x8d lea", &[0x66, 0x8d, 0x40, 0x10]),
        ("0xa9 test eax,imm", &[0x66, 0xa9, 0x34, 0x12]),
        ("0xb8 mov eax,imm", &[0x66, 0xb8, 0x34, 0x12]),
        ("0xbf mov edi,imm", &[0x66, 0xbf, 0x34, 0x12]),
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

/// `0x83` at Word size: the six non-carry sub-ops of the REGISTER form are lowered, and nothing
/// else about the opcode is.
///
/// This is the admission half of the rejected-row campaign's Slice 1 (`0x83 /5` SUB r16,imm8 is
/// 9,776,289 doom dispatcher exits, forty-seven apart from PUSHAD). Every sub-op is listed
/// separately rather than looped over a range, for the reason the byte-form table above gives:
/// the classifier decides `2 | 3` on their own and a range would hide a member.
#[test]
fn word_size_0x83_register_forms_are_lowered() {
    // ModRM 0xc0 | (op << 3) | 1 -- destination CX, sub-op in `reg`.
    let cases: &[(&str, u8)] = &[
        ("/0 add", 0),
        ("/1 or", 1),
        ("/4 and", 4),
        ("/5 sub", 5),
        ("/6 xor", 6),
        ("/7 cmp", 7),
    ];

    for &(label, op) in cases {
        let form = [0x66u8, 0x83, 0xc0 | (op << 3) | 1, 0x03];
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(&form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, 4,
            "{label}: the word form must join the block rather than end it"
        );
        // A register ALU form touches no memory whatever the width says.
        assert_eq!(compilation.word_reads, 0, "{label}: word reads");
        assert_eq!(compilation.word_stores, 0, "{label}: word stores");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// The two `0x83` shapes that stay refused at Word size, and the boundary that keeps them out.
///
/// ADC (/2) and SBB (/3) consume the incoming CF as an OPERAND, which the Dword lane handles by
/// branching on the EFLAGS shadow (`emit_carry_alu_preloaded`) and which has no sixteen-bit twin.
/// That is a missed lowering; admitting either without a sixteen-bit carry lane would be the
/// miscompile. Both fixtures measure zero Word `0x83 /2` and `/3` exits.
///
/// The MEMORY forms were here too and have MOVED to the test below: the rejected-row campaign's
/// Slice 3 admitted them, and quake's `0x83 /7` memory word at 162,440 exits is what asked.
#[test]
fn word_size_0x83_carry_forms_stay_refused() {
    let cases: &[(&str, &[u8])] = &[
        ("/2 adc r/m16,imm8", &[0x66, 0x83, 0xd1, 0x03]),
        ("/3 sbb r/m16,imm8", &[0x66, 0x83, 0xd9, 0x03]),
        (
            "/2 adc m16,imm8",
            &[0x66, 0x83, 0x15, 0x00, 0x20, 0x00, 0x00, 0x03],
        ),
        (
            "/3 sbb m16,imm8",
            &[0x66, 0x83, 0x1d, 0x00, 0x20, 0x00, 0x00, 0x03],
        ),
    ];

    for &(label, form) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        map_word_operand_page(&mut cpu, &mut bus);
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

/// The linear address every Word memory-form case below addresses. Its page is untouched by
/// `flat_fixture`, which maps only the page holding the code, so the mapping below is the only one.
const WORD_OPERAND: u32 = 0x2000;

/// Map `WORD_OPERAND`'s page for READ and WRITE.
///
/// Without this the compile refuses for want of a direct-page mapping and a positive assertion
/// would fail for a reason that has nothing to do with the allowlist -- and, worse, a NEGATIVE
/// assertion would pass for that reason. The carry-form test above calls it too, so its two
/// memory rows are refused by the classifier rather than by an unmapped page.
fn map_word_operand_page(cpu: &mut CpuGsw, bus: &mut TestBus) {
    let page = WORD_OPERAND & !0xfff;
    for (kind, write) in [
        (BusAccessKind::DataRead, false),
        (BusAccessKind::DataWrite, true),
    ] {
        let host = bus.direct_page(page, kind).unwrap().unwrap();
        let populated = if write {
            cpu.jit_fast_map.populate_write(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
            )
        } else {
            cpu.jit_fast_map.populate_read(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
            )
        };
        assert!(populated, "the word operand page must map");
    }
}

/// `0x83` and `0xC7` at Word size, MEMORY form: the rejected-row campaign's Slice 3.
///
/// `0x83`'s memory form is a sixteen-bit READ-MODIFY-WRITE, which is both halves of the width
/// hazard in one instruction -- the read must not widen and the write-back must touch exactly two
/// bytes. `0xC7`'s is a plain two-byte immediate store. Neither needed an emitter change:
/// `emit_alu_mem_dest` and `emit_store` have carried complete Word arms all along and had no
/// caller producing one. What this test pins is that they now have one.
///
/// The wide-access accessors are asserted explicitly. A Word memory form that registered a DWORD
/// access would take the wrong alignment guard and mis-charge the bus, and `word_reads` /
/// `word_stores` are the static half of the static-versus-dynamic agreement the emitted
/// completions rely on. `/7` CMP is the read-only sub-op and must register a read and NO store,
/// which is the one row that would catch `op: 0..=6` in `word_stores` being widened to `0..=7`.
#[test]
fn word_size_memory_immediate_forms_are_lowered() {
    // ModRM 0x05 | (op << 3): mod 00, rm 101 -- a disp32 absolute operand, no base register.
    let cases: &[(&str, &[u8], u8, u8)] = &[
        ("0x83 /0 add m16,imm8", &[0x66, 0x83, 0x05], 1, 1),
        ("0x83 /1 or m16,imm8", &[0x66, 0x83, 0x0d], 1, 1),
        ("0x83 /4 and m16,imm8", &[0x66, 0x83, 0x25], 1, 1),
        ("0x83 /5 sub m16,imm8", &[0x66, 0x83, 0x2d], 1, 1),
        ("0x83 /6 xor m16,imm8", &[0x66, 0x83, 0x35], 1, 1),
        ("0x83 /7 cmp m16,imm8", &[0x66, 0x83, 0x3d], 1, 0),
        ("0xc7 /0 mov m16,imm16", &[0x66, 0xc7, 0x05], 0, 1),
    ];

    for &(label, head, word_reads, word_stores) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(head);
        code.extend_from_slice(&WORD_OPERAND.to_le_bytes());
        // 0xc7 takes an imm16 at Word operand size, the rest a sign-extended imm8.
        if head[1] == 0xc7 {
            code.extend_from_slice(&[0x34, 0x12]);
        } else {
            code.push(0x03);
        }
        let form_len = code.len() - 3;
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        map_word_operand_page(&mut cpu, &mut bus);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, 4,
            "{label}: the word memory form must join the block rather than end it"
        );
        assert_eq!(
            compilation.span.guest_len,
            3 + form_len as u16,
            "{label}: block extent"
        );
        assert_eq!(compilation.word_reads, word_reads, "{label}: word reads");
        assert_eq!(compilation.word_stores, word_stores, "{label}: word stores");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// The `0xC7` REGISTER form stays refused at Word size, and it is the asymmetry that matters.
///
/// `0xc7` is on the Word allowlist for its memory form, so the gate no longer stops the register
/// form; only the `DecodedOperand::Reg(_) if operand_size == Word` arm inside the classifier does.
/// Admitting it would produce `MovImm`, whose `mov_r32_imm32(home(dst), imm)` writes all 32 bits
/// where `write_gpr_sized(.., Word, ..)` writes 16, clobbering the destination's high half.
///
/// `0xb8..=0xbf` is the same kind with the same hazard and is kept out by the allowlist instead;
/// it is covered by `word_size_dword_siblings_stay_refused` above. This test is the one that
/// would fail if someone "simplified" the classifier by deleting the arm now that the opcode is
/// on the list.
#[test]
fn the_word_size_0xc7_register_form_stays_refused() {
    for (label, form) in [
        ("0xc7 /0 mov cx,imm16", [0x66u8, 0xc7, 0xc1, 0x34, 0x12]),
        ("0xc7 /0 mov ax,imm16", [0x66, 0xc7, 0xc0, 0x34, 0x12]),
    ] {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(&form);
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

/// A 66-prefixed `FF /6` must stay REFUSED, and nothing else in the crate can catch it.
///
/// `0xff` is in the Word allowlist, so this form reaches the classifier and produces a `PushMem`.
/// What refuses it is the stack-width admission matrix, which is consulted only for kinds whose
/// `uses_stack()` is true. Admitting it would push two bytes while decrementing ESP by four,
/// which is a miscompile rather than a missed lowering.
///
/// The assertion is an EXACT count, and it is paired against a POSITIVE CONTROL: the unprefixed
/// `FF /6` at the same entry, on the same stack width, must lower and grow the block to four
/// instructions. Without the control, "the block is three instructions" is satisfied identically
/// by the Word form being correctly refused OR by `PushMem` never reaching the classifier at all,
/// and the fixture cannot tell those apart.
///
/// Both cases widen SS to a 32-bit stack explicitly. `flat_fixture` widens the CS and SS LIMITS
/// but leaves `SS.default_size_32` alone, and the stack-width admission matrix refuses `PushMem`
/// on a 16-bit stack (`SS.B` = 0) regardless of operand size. Left unwidened, the control would
/// also come out to 3 instructions, for a reason that has nothing to do with the 0x66 prefix, and
/// the pairing would prove nothing. Widening SS here makes the prefix the ONLY difference between
/// the two cases.
#[test]
fn word_size_push_through_memory_stays_refused() {
    let cases: &[(&str, &[u8], u8)] = &[
        (
            "unprefixed control",
            &[0xff, 0x35, 0x00, 0x08, 0x00, 0x00],
            4,
        ),
        (
            // 66 ff 35 00 08 00 00: push word [0x800] at Word operand size.
            "0x66-prefixed",
            &[0x66, 0xff, 0x35, 0x00, 0x08, 0x00, 0x00],
            3,
        ),
    ];

    for &(label, form, expected_instructions) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
        cpu.registers.set_esp(0x1000);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, expected_instructions,
            "{label}: the Dword form must lower and the Word form must stay refused"
        );
    }
}

/// A 66-prefixed `FF /2` register form (`call bx`) must stay REFUSED, and nothing else in the
/// crate can catch it.
///
/// `CallReg` IS `uses_stack()`, so unlike `JmpMem` this goes through the stack-width admission
/// matrix rather than through the classifier's own operand-size check. The matrix has no
/// `CallReg16` mapping arm, so a Word form falls to the matrix's catch-all `Retry` regardless of
/// `SS.B`, the same PushMem precedent.
///
/// Both cases widen SS to a 32-bit stack explicitly, for the same reason
/// `word_size_push_through_memory_stays_refused` does: `flat_fixture` leaves `SS.default_size_32`
/// alone, and on a 16-bit stack (`SS.B` = 0) even the UNPREFIXED Dword control has no matrix arm
/// (the `(false, Dword)` cell is a stop, four bytes on a 16-bit SP not being built yet) and would
/// also come out refused, for a reason that has nothing to do with the 0x66 prefix. Widening SS
/// here makes the prefix the ONLY difference between the two cases.
///
/// The assertion is an EXACT count, paired against a POSITIVE CONTROL: the unprefixed `FF /2`
/// register form at the same entry, on the same stack width, must lower and grow the block to
/// four instructions.
#[test]
fn word_size_call_through_a_register_stays_refused() {
    let cases: &[(&str, &[u8], u8)] = &[
        ("unprefixed control", &[0xff, 0xd3], 4),
        (
            // 66 ff d3: call bx at Word operand size.
            "0x66-prefixed",
            &[0x66, 0xff, 0xd3],
            3,
        ),
    ];

    for &(label, form, expected_instructions) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
        cpu.registers.set_esp(0x1000);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, expected_instructions,
            "{label}: the Dword form must lower and the Word form must stay refused"
        );
    }
}

/// A 66-prefixed `FF /4` must stay REFUSED, and nothing else in the crate can catch it.
///
/// `0xff` is in the Word allowlist, so this form reaches the classifier and would otherwise
/// produce a `JmpMem`. Nothing downstream refuses it the way the stack-width matrix refuses
/// `PushMem`'s Word form: `uses_stack` is false for a jump, so that matrix never sees this kind,
/// and `static_control_target` is `None` for a dynamic target, so the Word control clamp never
/// sees it either. The classifier's own operand-size check is the ONLY gate. At Word size the
/// interpreter reads TWO bytes and masks EIP to 16 bits; lowering that as the Dword construction
/// would read four bytes and jump unmasked, a miscompile twice over.
///
/// The assertion is an EXACT count, paired against a POSITIVE CONTROL: the unprefixed `FF /4` at
/// the same entry must lower and grow the block to four instructions. Without the control, "the
/// block is three instructions" is satisfied identically by the Word form being correctly refused
/// OR by `JmpMem` never reaching the classifier at all, and the fixture cannot tell those apart.
/// `JmpMem` needs no stack widening, unlike the `PushMem` pairing above: the only difference
/// between the two cases here is the 0x66 prefix.
#[test]
fn word_size_jmp_through_memory_stays_refused() {
    let cases: &[(&str, &[u8], u8)] = &[
        (
            "unprefixed control",
            &[0xff, 0x25, 0x00, 0x08, 0x00, 0x00],
            4,
        ),
        (
            // 66 ff 25 00 08 00 00: jmp word [0x800] at Word operand size.
            "0x66-prefixed",
            &[0x66, 0xff, 0x25, 0x00, 0x08, 0x00, 0x00],
            3,
        ),
    ];

    for &(label, form, expected_instructions) in cases {
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
            compilation.span.instructions, expected_instructions,
            "{label}: the Dword form must lower and the Word form must stay refused"
        );
    }
}
