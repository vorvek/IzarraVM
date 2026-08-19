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
/// `0x01` and `0x31` were here and have gone, with the rest of the ALU register forms 1 and 3.
/// The reason they stayed was that nothing had measured them, not that they would miscompile; a
/// 16-bit workload now ranks the ten rows near 19% of block-stopping hits. What replaced them is
/// `word_size_alu_register_forms_are_lowered` plus two tests that carry the arm's own boundary,
/// because this slice moved that boundary INTO the classifier arm:
/// `word_size_alu_carry_forms_stay_refused` and `word_size_alu_memory_shapes_split_by_site`. The
/// carry test is purely negative and would keep passing if the allowlist entries were reverted, so
/// it cannot live in this table; the memory test is mixed since the B2 slice — its form-1 row is
/// negative for the same reason, and its lowered rows belong with the other positive fixtures
/// rather than in a list of opcodes held OUT.
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
/// `0xb8..=0xbf` has gone too, to `word_size_mov_imm_register_forms_are_lowered`. After that
/// slice this list guards no `MovImm` producer at all: `0xc7`'s register form is the other one and
/// it is refused inside the classifier arm, so it has `the_word_size_0xc7_register_form_stays_refused`
/// rather than a row here.
#[test]
fn word_size_dword_siblings_stay_refused() {
    // `0x81` and `0xf7 /0` left this table on 2026-08-08 (the wolf3d demo-workload census ranked
    // them at 634M block-stopping hits each); their admissions are pinned by
    // `word_size_0x81_register_forms_are_lowered` and
    // `word_size_group3_test_forms_follow_the_slice` below.
    let cases: &[(&str, &[u8])] = &[
        ("0x05 add eax,imm", &[0x66, 0x05, 0x34, 0x12]),
        ("0x85 test r/m,r", &[0x66, 0x85, 0xc0]),
        ("0x8d lea", &[0x66, 0x8d, 0x40, 0x10]),
        ("0xa9 test eax,imm", &[0x66, 0xa9, 0x34, 0x12]),
        ("0xf7 /2 not r/m", &[0x66, 0xf7, 0xd1]),
        ("0xf7 /3 neg r/m", &[0x66, 0xf7, 0xd9]),
        ("0xf7 /4 mul r/m", &[0x66, 0xf7, 0xe1]),
        ("0xf7 /6 div r/m", &[0x66, 0xf7, 0xf1]),
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

/// `0x81` at Word size: the six non-carry sub-ops of the REGISTER form are lowered, exactly the
/// `0x83` slice above with a two-byte immediate. The wolf3d demo-workload census asked for it:
/// `0x81 /7` word register (CMP CX, imm16) is 634M block-stopping hits, the largest single row.
/// ADC (/2) and SBB (/3) stay refused by the shared arm; the emitter's word lane masks both
/// operands before a 66-prefixed `alu_r16_r16`, so the raw imm16 needs no admission-side care.
#[test]
fn word_size_0x81_register_forms_are_lowered() {
    let cases: &[(&str, u8)] = &[
        ("/0 add", 0),
        ("/1 or", 1),
        ("/4 and", 4),
        ("/5 sub", 5),
        ("/6 xor", 6),
        ("/7 cmp", 7),
    ];

    for &(label, op) in cases {
        let form = [0x66u8, 0x81, 0xc0 | (op << 3) | 1, 0x34, 0x12];
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
        assert_eq!(compilation.word_reads, 0, "{label}: word reads");
        assert_eq!(compilation.word_stores, 0, "{label}: word stores");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// `0x81` ADC and SBB keep their Word refusal after the opcode's admission: the shared arm's
/// carry-in refusal, not the allowlist, is what holds them now, so this pins the arm.
#[test]
fn word_size_0x81_carry_forms_stay_refused() {
    for (label, op) in [("/2 adc", 2u8), ("/3 sbb", 3)] {
        let form = [0x66u8, 0x81, 0xc0 | (op << 3) | 1, 0x34, 0x12];
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
            "{label}: the carry forms have no word lane and must end the block"
        );
    }
}

/// Group 3's `/0` at Word size: the REGISTER form is lowered through `TestImmReg`'s word lane,
/// the MEMORY form stays refused (no measured row), and both halves are what the gate's
/// sub-opcode escape promises. The wolf3d census ranked the register form at 634M
/// block-stopping hits.
#[test]
fn word_size_group3_test_forms_follow_the_slice() {
    let register = [0x66u8, 0xf7, 0xc1, 0x34, 0x12];
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(&register);
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "test cx, imm16: the register form must join the block"
    );
    assert_eq!(compilation.word_reads, 0, "register form touches no memory");
    assert_eq!(
        compilation.word_stores, 0,
        "register form touches no memory"
    );

    // MEMORY form: `test word [eax+0x10], imm16`.
    let memory_form = [0x66u8, 0xf7, 0x40, 0x10, 0x34, 0x12];
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(&memory_form);
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "test word [mem], imm16: the memory form must stay refused"
    );
}

/// `PUSH DS` and `PUSH ES` at Word size, the read half of the segment family.
///
/// One word store and no reads: the selector is a compile-time constant baked from the block's
/// `SegmentLayout`, so the only memory the slot touches is the stack. `PUSH CS` joined on
/// 2026-08-08 (158M wolf3d census hits): `SegmentLayout::selector` reads the separate `cs`
/// field and `cs_matches` pins CS for every block unconditionally, so keeping CS out of the
/// `selector_segment` mask stays correct. `PUSH SS` is asserted refused in the table below: it
/// belongs to the family the write half excludes over the interrupt shadow.
#[test]
fn word_size_push_segment_forms_are_lowered() {
    for (label, opcode) in [
        ("0x1e push ds", 0x1eu8),
        ("0x06 push es", 0x06),
        ("0x0e push cs", 0x0e),
    ] {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(&[0x66, opcode]);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        // Two things this fixture does not give you by default, and both look identical to the
        // opcode still being a barrier if you miss them.
        //
        // SS.B must be 0. `fresh()` sets `default_size_32` on CS and SS, and `stack_width_kind`
        // implements no (SS.B = 1, Word) cell for any push at all -- `Push16` exists only for
        // (SS.B = 0, Word), which is the cell real 16-bit code runs in.
        //
        // ESP must be live BEFORE compiling. The default of 0 puts the store page at 0xFFFFFFFE,
        // which cannot resolve, and the block comes back Retry.
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = false;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
        cpu.registers.set_esp(0x1000);
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
        assert_eq!(compilation.word_stores, 1, "{label}: word stores");
        assert_eq!(compilation.word_reads, 0, "{label}: word reads");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// The segment pushes and pops that stay refused, all of them by the allowlist rather than by an
/// arm, so this table is the right place for them.
///
/// SS.B and ESP are set up exactly as in the positive test above, and that is what makes these
/// refusals ATTRIBUTABLE. Leave either alone and every row comes back at three instructions
/// because no push can compile in that cell at all, and the table passes while proving nothing
/// about the allowlist.
#[test]
fn word_size_push_segment_forms_outside_the_slice_stay_refused() {
    let cases: &[(&str, &[u8])] = &[
        ("0x16 push ss", &[0x66, 0x16]),
        ("0x1f pop ds", &[0x66, 0x1f]),
        ("0x07 pop es", &[0x66, 0x07]),
        ("0x17 pop ss", &[0x66, 0x17]),
    ];

    for &(label, form) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(form);
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = false;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
        cpu.registers.set_esp(0x1000);
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

/// `MOV DS, r16` and `MOV ES, r16` at Word size, the write half of the segment family.
///
/// `flat_fixture` is real mode with big limits, which is the mode this lowering is admitted in.
#[test]
fn word_size_load_segment_forms_are_lowered() {
    // ModRM 0xc0 | (reg << 3) | rm, register source AX.
    for (label, reg) in [("/0 mov es,ax", 0u8), ("/3 mov ds,ax", 3)] {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(&[0x66, 0x8e, 0xc0 | (reg << 3)]);
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
        // The load touches no guest memory at all: it writes a CPU field, not the bus.
        assert_eq!(compilation.word_reads, 0, "{label}: word reads");
        assert_eq!(compilation.word_stores, 0, "{label}: word stores");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// The `0x8e` shapes that stay refused, all inside the classifier arm rather than by the
/// allowlist, so each would keep passing if the allowlist entry were reverted.
///
/// `/1`, `/6` and `/7` are the ones that matter most: they are not segment loads at all. The
/// interpreter raises #GP(0) for each, so lowering them would turn a fault into a silent write.
/// `/2` is SS, refused over the one-instruction interrupt shadow a native block cannot honour.
/// The memory form is refused because the slice is the register shape and nothing else.
#[test]
fn word_size_load_segment_shapes_outside_the_slice_stay_refused() {
    let cases: &[(&str, &[u8])] = &[
        ("/1 mov cs,ax is #GP", &[0x66, 0x8e, 0xc8]),
        ("/6 mov ?,ax is #GP", &[0x66, 0x8e, 0xf0]),
        ("/7 mov ?,ax is #GP", &[0x66, 0x8e, 0xf8]),
        ("/2 mov ss,ax arms the shadow", &[0x66, 0x8e, 0xd0]),
        ("/4 mov fs,ax is out of scope", &[0x66, 0x8e, 0xe0]),
        ("/5 mov gs,ax is out of scope", &[0x66, 0x8e, 0xe8]),
        (
            "/3 memory form",
            &[0x66, 0x8e, 0x1d, 0x00, 0x20, 0x00, 0x00],
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
            "{label}: must stay refused, so the block is the three fillers"
        );
    }
}

/// `MOV Sreg, r16` stays refused in PROTECTED mode, where a segment load is a descriptor fetch
/// with type, privilege and present checks rather than `selector << 4`.
///
/// The refusal lives beside the `stack_width_kind` call because `classify` has no CPU. The mode
/// key is what makes it sufficient once made -- a block admitted in real mode can never be
/// entered in protected mode -- but the key alone would not stop the block being COMPILED here,
/// which is the failure this pins.
#[test]
fn the_protected_mode_load_segment_form_stays_refused() {
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(&[0x66, 0x8e, 0xd8]);
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    cpu.control.cr0 |= CR0_PE;
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "protected mode must stay refused, so the block is the three fillers"
    );
}

/// `MOV r16, imm16` at Word size, the 16-bit campaign's fourth slice.
///
/// All eight destinations, because `home()` is a table lookup and one case cannot see a
/// mis-indexed entry. `0xbc` (SP) and `0xbf` (DI) are the two that matter most: SP's home is R12,
/// the SIB-escape register, and DI's is RBX, the ONE guest home that is not an extended register
/// and so the only encoding that takes no REX at all.
#[test]
fn word_size_mov_imm_register_forms_are_lowered() {
    for dst in 0..8u8 {
        let form = [0x66u8, 0xb8 + dst, 0x34, 0x12];
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
            "0xb8+{dst}: the word form must join the block rather than end it"
        );
        // A register move touches no memory at any width.
        assert_eq!(compilation.word_reads, 0, "0xb8+{dst}: word reads");
        assert_eq!(compilation.word_stores, 0, "0xb8+{dst}: word stores");
        assert_eq!(compilation.dword_reads, 0, "0xb8+{dst}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "0xb8+{dst}: dword stores");
    }
}

/// The ALU register forms 1 and 3 at Word size, the 16-bit campaign's second slice.
///
/// Every opcode is listed on its own rather than looped over a range, for the reason the tables
/// above give: the classifier decides `2 | 3` separately and a range would hide a member.
#[test]
fn word_size_alu_register_forms_are_lowered() {
    // Form 1 is `op r/m16, r16` and form 3 is `op r16, r/m16`. ModRM 0xc1 names CX and AX either
    // way round, which is enough to compile; the differential tests exercise the operand roles.
    let cases: &[(&str, u8)] = &[
        ("0x01 add r/m,r", 0x01),
        ("0x09 or r/m,r", 0x09),
        ("0x21 and r/m,r", 0x21),
        ("0x29 sub r/m,r", 0x29),
        ("0x31 xor r/m,r", 0x31),
        ("0x03 add r,r/m", 0x03),
        ("0x0b or r,r/m", 0x0b),
        ("0x23 and r,r/m", 0x23),
        ("0x2b sub r,r/m", 0x2b),
        ("0x33 xor r,r/m", 0x33),
    ];

    for &(label, opcode) in cases {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(&[0x66, opcode, 0xc1]);
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

/// ADC and SBB in forms 1 and 3 stay refused at Word size, and the guard is inside the classifier
/// arm rather than in the allowlist.
///
/// This is the boundary that matters most in the whole slice. The allowlist alone would not hold
/// it: the file's stated rule is that the byte set is closed over its shared classifier arms, and
/// the slice admits five of eight members of two shared arms, so the next reader closing the
/// family lands a silent miscompile in one line. `emit_alu_preloaded`'s Word lane masks both
/// operands with `and`, which CLEARS host CF, then tags the descriptor as the SUB class: an
/// admitted `66 11 /r` computes `adc` without its carry in and evaluates its lazy CF as `a < b`.
#[test]
fn word_size_alu_carry_forms_stay_refused() {
    let cases: &[(&str, &[u8])] = &[
        ("0x11 adc r/m16,r16", &[0x66, 0x11, 0xc1]),
        ("0x19 sbb r/m16,r16", &[0x66, 0x19, 0xc1]),
        ("0x13 adc r16,r/m16", &[0x66, 0x13, 0xc1]),
        ("0x1b sbb r16,r/m16", &[0x66, 0x1b, 0xc1]),
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

/// Form 1's MEMORY shape stays refused at Word size while its Dword shape ships, and form 3's
/// memory shape is now LOWERED at both sizes.
///
/// The two arms part company on the SITE, not on the opcode, and that is the whole content of this
/// test since the B2 slice:
///
/// * Form 1 (`0x01 add m,r`) is `AluMemDest`, a read-modify-write through site 6, which still
///   refuses a misaligned access outright — guard 3 relaxed only the two lean one-lookup sites.
///   16-bit DOS code has no alignment discipline, so admitted, an odd operand would sit inside the
///   block and side-exit at that slot on every execution, and nothing after it would retire
///   natively. Refused, it ends the block instead. Its Dword row must keep compiling, which is the
///   second half of what this asserts. (An earlier version of this comment said "these forms lower
///   through the read-modify-write memory site" of forms 1 AND 3; that was form-1-only and was
///   already wrong for form 3, which never writes memory.)
/// * Form 3 (`0x03 add r,m`, `0x33 xor r,m`) is `AluMemSource`, a pure READ through the relaxed
///   lean one-lookup read site, so the alignment economics that hold form 1 out do not apply. The
///   B2 peachdrm census ranked `0x2B` word memory at 99.0% of barrier runtime_hits and the slice
///   admitted the whole non-carry set; these rows now assert the lowering with the same three
///   numbers the CMP control uses, and `cpu_jit_word_memory_test.rs` carries the state comparison.
///
/// CMP was the exception before the slice and is the control after it: `0x39` and `0x3b` have been
/// compiling word memory in quake's renderer since before either change, and `0x39` is the row that
/// says form 1's refusal is still `op != 7` rather than blanket. A blanket refusal there regressed
/// `quake_word_renderer_families_match_interpreter_state_flags_memory_and_timing`, which is what
/// caught it.
#[test]
fn word_size_alu_memory_shapes_split_by_site() {
    let cases: &[(&str, &[u8], &[u8])] = &[(
        "0x01 add m,r",
        &[0x66, 0x01, 0x0d, 0x00, 0x20, 0x00, 0x00],
        &[0x01, 0x0d, 0x00, 0x20, 0x00, 0x00],
    )];
    // The CMP pair is the control: word memory, no write-back, admitted before the 16-bit memory
    // slices and still admitted after them. If a future edit turns form 1's `op != 7` guard into a
    // blanket refusal `0x39` fails here rather than only in the quake renderer fixture.
    //
    // The two form-3 writing rows below it are the B2 admission, asserted with the same three
    // numbers: the block covers all four slots, the slot counts ONE word read, and it stores
    // nothing — `AluMemSource` never writes guest memory, so a `word_stores` of 1 would mean the
    // slot had been classified as the read-modify-write shape.
    for (label, form) in [
        ("0x39 cmp m,r", [0x66, 0x39, 0x0d, 0x00, 0x20, 0x00, 0x00]),
        ("0x3b cmp r,m", [0x66, 0x3b, 0x0d, 0x00, 0x20, 0x00, 0x00]),
        ("0x03 add r,m", [0x66, 0x03, 0x0d, 0x00, 0x20, 0x00, 0x00]),
        ("0x33 xor r,m", [0x66, 0x33, 0x0d, 0x00, 0x20, 0x00, 0x00]),
    ] {
        let mut code = vec![0x40, 0x41, 0x42];
        code.extend_from_slice(&form);
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
            "{label}: this word memory form must be admitted"
        );
        assert_eq!(compilation.word_reads, 1, "{label}: word reads");
        assert_eq!(compilation.word_stores, 0, "{label}: word stores");
    }

    for &(label, word_form, dword_form) in cases {
        for (form, expected, width) in [(word_form, 3, "word"), (dword_form, 4, "dword")] {
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
                compilation.span.instructions, expected,
                "{label} at {width} size"
            );
        }
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
                cpu.physical_page_watched(page),
            )
        } else {
            cpu.jit_fast_map.populate_read(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
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

/// `0xC1` at Word size: the four shift sub-ops of the REGISTER form are lowered.
///
/// The admission half of the rejected-row campaign's Slice 3b. Quake's `0xC1 /7` SAR (63,039
/// exits) and `0xC1 /4` SHL (62,934, the row Slice 3's MOVZX lowering relocated 30,692 exits onto)
/// clear the campaign's 100k floor only as a pair, and the wall argument is the Slice 3 ladder's
/// rather than the census's.
///
/// Every sub-op listed separately rather than as a range, for the reason the `0x83` table gives.
/// `/6` in particular is the undocumented SAL alias of `/4`: the interpreter handles it in a
/// `4 | 6` arm and the host encodes it identically, so a range would hide the one member whose
/// correctness is an aliasing claim rather than a decode claim.
#[test]
fn word_size_shift_forms_are_lowered() {
    // ModRM 0xc0 | (op << 3) | 1 -- destination CX, sub-op in `reg`.
    let cases: &[(&str, u8)] = &[("/4 shl", 4), ("/5 shr", 5), ("/6 sal", 6), ("/7 sar", 7)];

    for &(label, op) in cases {
        // `0xc1` takes an imm8 and `0xd1` supplies an implicit 1, and the arm they share turns
        // both into the same `Shift`. Both encodings are asserted because the allowlist entries
        // are separate: dropping either one leaves the other passing.
        let forms: [(&str, Vec<u8>); 2] = [
            ("0xc1", vec![0x66, 0xc1, 0xc0 | (op << 3) | 1, 0x03]),
            ("0xd1", vec![0x66, 0xd1, 0xc0 | (op << 3) | 1]),
        ];
        for (opcode, form) in forms {
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
                "{opcode} {label}: the word form must join the block rather than end it"
            );
            // A register shift touches no memory at any width.
            assert_eq!(compilation.word_reads, 0, "{opcode} {label}: word reads");
            assert_eq!(compilation.word_stores, 0, "{opcode} {label}: word stores");
            assert_eq!(compilation.dword_reads, 0, "{opcode} {label}: dword reads");
            assert_eq!(
                compilation.dword_stores, 0,
                "{opcode} {label}: dword stores"
            );
        }
    }
}

/// The group-2 shapes that stay refused at Word size, and the boundary that keeps them out.
///
/// Three separate refusals share this table, and they are refused in three different places:
///
/// * **The four ROTATES of `0xC1` itself.** `/2` RCL and `/3` RCR have never had a classify arm
///   at any width. `/0` ROL and `/1` ROR both do -- ROR since slice 3b, ROL since the 2026-08-09
///   duke3d-586 re-census slice -- and BOTH are refused at Word by one guard: `RotateReg` carries
///   no width and emits `shift_r32_imm8(op, ..)`, so a 66-prefixed rotate routed through it would
///   rotate 32 bits and take CF from bit 31 instead of bit 15. That guard is the only thing
///   standing between the allowlist entry and a miscompile, which is why both have a row here and
///   a mutation in the differential file. ROL's row was a "no arm anywhere" refusal when it was
///   written and is a genuine Word-guard refusal now; nothing about the assertion changed, which
///   is the point of pinning the shape rather than the reason.
/// * **The four ROTATES of `0xD1`.** Same split as `0xC1`'s, and they matter more now that the
///   opcode is on the allowlist: `/0` and `/1` reach the Word guard through `0xD1` far more often
///   than through `0xC1`, and `/2` RCL is the largest single row in the 16-bit census at 10.91%,
///   so it is the shape most likely to be admitted by a hurried edit.
/// * **`0xD3`, the shift-by-CL group.** A different arm, still Dword-only: `emit_shift_cl` has no
///   sixteen-bit lane and would be a second emitter primitive.
///
/// The MEMORY form of `0xC1` is here too, at BOTH operand sizes, because its refusal is neither
/// the allowlist's nor new -- the classify arm binds `DecodedOperand::Reg` and returns None for
/// anything else. Pinning both sizes is what says the Word entry did not widen it by accident.
#[test]
fn the_word_size_group_two_shapes_outside_the_shift_lane_stay_refused() {
    // The group-2 admission knob is FORCED ON for this table, and it is the ROL rows that need it.
    // On the OFF arm the knob refuses them BEFORE the Word guard runs -- they would still pass
    // while certifying nothing about the guard this test exists to pin, and a Word admission
    // slipped into the rotate branch would survive. The shipped default has been ON since the
    // 2026-08-19/20 re-measurement (`jit::direct::rotate_rows_enabled` carries both A/Bs), so the
    // force is now insurance against the default moving rather than the thing that makes these rows
    // reachable. The knob's own arms are pinned in
    // `the_rotate_rows_knob_defaults_on_and_the_off_arm_restores_the_pre_slice_admissions`; the
    // Word guard is pinned here, and neither may stand in for the other.
    jit::direct::set_rotate_rows_for_test(Some(true));
    assert!(
        jit::direct::rotate_rows_enabled(),
        "the ROL rows below pin the Word guard, which the off arm never reaches"
    );
    let cases: &[(&str, &[u8])] = &[
        ("0xc1 /0 rol cx,imm8", &[0x66, 0xc1, 0xc1, 0x03]),
        ("0xc1 /1 ror cx,imm8", &[0x66, 0xc1, 0xc9, 0x03]),
        ("0xc1 /2 rcl cx,imm8", &[0x66, 0xc1, 0xd1, 0x03]),
        ("0xc1 /3 rcr cx,imm8", &[0x66, 0xc1, 0xd9, 0x03]),
        ("0xd1 /0 rol cx,1", &[0x66, 0xd1, 0xc1]),
        ("0xd1 /1 ror cx,1", &[0x66, 0xd1, 0xc9]),
        ("0xd1 /2 rcl cx,1", &[0x66, 0xd1, 0xd1]),
        ("0xd1 /3 rcr cx,1", &[0x66, 0xd1, 0xd9]),
        ("0xd3 /4 shl cx,cl", &[0x66, 0xd3, 0xe1]),
        ("0xd3 /7 sar cx,cl", &[0x66, 0xd3, 0xf9]),
        (
            "0xc1 /4 shl word [m],imm8",
            &[0x66, 0xc1, 0x25, 0x00, 0x20, 0x00, 0x00, 0x03],
        ),
        (
            "0xc1 /4 shl dword [m],imm8",
            &[0xc1, 0x25, 0x00, 0x20, 0x00, 0x00, 0x03],
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
            "{label}: must stay refused, so the block is the three fillers"
        );
    }
}

/// The DWORD `0xC1` register form is unaffected by the Word admission.
///
/// Both the shift lane and `/1` ROR must still lower at Dword. The ROR row is the one that
/// matters: the refusal this slice added is keyed on `insn.operand_size == OperandSize::Word`, and
/// a guard written one character wider -- refusing the opcode outright, or testing the wrong
/// polarity -- would silently retire the existing Dword ROR lowering and no other test in the tree
/// asserts its admission.
#[test]
fn the_dword_group_two_register_forms_are_unaffected() {
    let cases: &[(&str, &[u8])] = &[
        ("0xc1 /1 ror ecx,imm8", &[0xc1, 0xc9, 0x03]),
        ("0xc1 /4 shl ecx,imm8", &[0xc1, 0xe1, 0x03]),
        ("0xc1 /7 sar ecx,imm8", &[0xc1, 0xf9, 0x03]),
        ("0xd1 /1 ror ecx,1", &[0xd1, 0xc9]),
        ("0xd1 /4 shl ecx,1", &[0xd1, 0xe1]),
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
            "{label}: the dword form must still join the block"
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

/// A 66-prefixed `FF /2` MEMORY form (`call word [0x800]`) must stay REFUSED.
///
/// TWO checks refuse this and the fixture cannot tell them apart, which is recorded here rather
/// than glossed because a mutation matrix established it. `classify` carries an explicit Dword
/// gate, and `CallMem` is also `uses_stack()`, so the Word form additionally reaches the
/// stack-width admission matrix, which has no `CallMem16` arm and refuses it in both stack widths.
/// Deleting either one alone leaves the other refusing, and this fixture stays green. What it
/// asserts is therefore the OUTCOME -- the Word form does not lower -- and not which check
/// produced it.
///
/// The outcome is worth pinning on its own terms: at Word operand size the interpreter reads TWO
/// bytes for the target and masks EIP to 16 bits (`read_operand_sized(.., Word, ..)` then
/// `target & operand_size.mask()`), so lowering the Word form as the Dword construction reads four
/// bytes and jumps unmasked, a miscompile twice over.
///
/// SS is widened for `word_size_call_through_a_register_stays_refused`'s reason, so that the 0x66
/// prefix is the ONLY difference between the two cases.
///
/// EXACT counts, paired against a POSITIVE CONTROL: the unprefixed form at the same entry must
/// lower and grow the block to four instructions.
#[test]
fn word_size_call_through_memory_stays_refused() {
    let cases: &[(&str, &[u8], u8)] = &[
        (
            "unprefixed control",
            &[0xff, 0x15, 0x00, 0x08, 0x00, 0x00],
            4,
        ),
        (
            // 66 ff 15 disp32: call word [0x800] at Word operand size.
            "0x66-prefixed",
            &[0x66, 0xff, 0x15, 0x00, 0x08, 0x00, 0x00],
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

/// A 66-prefixed `FF /4` REGISTER form (`jmp bx`) must stay REFUSED — and here, unlike every
/// paired fixture above, there is genuinely only ONE check that can refuse it.
///
/// The residual census row this pins is small and real: 78,585 exits at duke3d-486 against the
/// 11.7M the Dword register form carries. It is deliberately left unlowered.
///
/// `JmpReg` is not `uses_stack()`, so the stack-width admission matrix never sees it — the
/// escape hatch that redundantly covers `CallReg`, `CallMem` and `PushMem` at Word does not exist
/// for this kind. `static_control_target` is `None` for a dynamic target, so the Word control
/// clamp never sees it either. The classifier's `insn.operand_size != OperandSize::Dword` gate,
/// shared with the memory form one line above it, is the whole defence. Deleting it is mutation
/// M1, and this fixture is the only thing in the tree that goes red.
///
/// What the gate prevents is an EIP-mask miscompile: at Word size the interpreter reads TWO bytes
/// for the target and masks EIP to 16 bits (`read_operand_sized(.., Word, ..)` then
/// `target & operand_size.mask()`), while the Dword construction takes the full register and jumps
/// unmasked. `jmp bx` with EBX = 0x1234_0500 lands at 0x0500 architecturally and at 0x1234_0500
/// natively — two different blocks, not a rounding difference.
///
/// EXACT counts, paired against a POSITIVE CONTROL: the unprefixed `FF /4` register form at the
/// same entry must lower and grow the block to four instructions. Without the control, "three
/// instructions" is satisfied identically by correct refusal and by `JmpReg` never reaching the
/// classifier at all. No stack widening is needed for either case, so the 0x66 prefix is the only
/// difference between them.
#[test]
fn word_size_jmp_through_a_register_stays_refused() {
    let cases: &[(&str, &[u8], u8)] = &[
        ("unprefixed control", &[0xff, 0xe3], 4),
        (
            // 66 ff e3: jmp bx at Word operand size.
            "0x66-prefixed",
            &[0x66, 0xff, 0xe3],
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
