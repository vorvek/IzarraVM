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
/// `word_size_alu_register_forms_are_lowered` plus `word_size_alu_memory_shapes_split_by_site`,
/// which carries form 1's memory-vs-register boundary.
///
/// `0x11`/`0x13`/`0x19`/`0x1b`, ADC and SBB in forms 1 and 3, joined the same way on the L1 width
/// lift: `word_size_alu_carry_forms_are_lowered` is their positive fixture now that
/// `emit_carry_alu_preloaded` carries a Word lane, so they no longer belong in this held-out table
/// either.
///
/// Two opcodes have moved OUT of this list as the rejected-row campaign measured them, and each
/// left a differently-shaped remainder behind:
/// * `0x83` -> `word_size_0x83_register_forms_are_lowered`, `word_size_0x83_carry_forms_are_lowered`
///   and `word_size_memory_immediate_forms_are_lowered`. No sub-op stays refused any more.
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
/// `0x05` carries a THIRD column because it moved on 2026-08-20: the V86 loop-A slice put ALU
/// form 5 on the Word allowlist behind `IZARRAVM_V86_LOOP_ROWS`, so it is an allowlist refusal on
/// the OFF arm and a lowering on the ON one. Running the table on both arms is what keeps the
/// other seven rows covered on both while pinning `0x05`'s flip in the place that owns it.
///
/// **`0x85` moved the same way on 2026-08-21, behind `IZARRAVM_TEST_WORD_ROWS`** (the duke census
/// slice: twelve rows, 42.2% of duke3d-586's whole rejected table). Its `DirectKind` grew a width
/// field, which is the very property this table's header names as the reason the opcode was
/// refused, so the row is now GATE-DEPENDENT rather than unconditionally refused. It is handled
/// the way `0x05` is: the refusal table states `IZARRAVM_TEST_WORD_ROWS=off` explicitly rather
/// than inheriting the ambient knob -- this suite is run on BOTH arms of every admission gate, and
/// a row that leaned on the default would go red on the ON arm while certifying nothing on the OFF
/// one -- and the flip itself is asserted at the end so the explicit `off` cannot quietly become
/// the whole test. `cpu_jit_test_word_row_test.rs` pins it from the other side.
///
/// `0xa9` stays in the table unconditionally, and that is deliberate rather than incidental: it is
/// the one member of the TEST family the duke slice measured at ZERO census rows and therefore
/// left refused, so this row is what keeps it out.
#[test]
fn word_size_dword_siblings_stay_refused() {
    // Stated, not inherited. See the header: with the ambient knob on, every `0x85` row below
    // would fail while asserting nothing about the allowlist.
    jit::direct::set_test_word_rows_for_test(Some(false));
    // `0x81` and `0xf7 /0` left this table on 2026-08-08 (the wolf3d demo-workload census ranked
    // them at 634M block-stopping hits each); their admissions are pinned by
    // `word_size_0x81_register_forms_are_lowered` and
    // `word_size_group3_test_forms_follow_the_slice` below.
    //
    // `0xf7 /2`, `/3`, `/4` and `/6` left it with the S3 policy widening, and NOT because they
    // grew a width field the way `0x8d` did: they have none, and lowering any of them at Word
    // would still be the miscompile this table's header describes. What changed is that the
    // classifier now intercepts the Word forms IN FRONT of those lowerings and routes them to an
    // `InterpretOne` call-out, so the block carries the instruction without an emitter. Their
    // admission is pinned by `group3_word_subops_join_as_call_outs_not_lowerings`
    // (cpu_jit_test_imm_test.rs), which asserts the SLOT CLASS and not just the block length, and
    // the flip is asserted at the end of this test so leaving the table is not a silent removal.
    //
    // `0x8d` left it with the S1 width lift, and for the reason this table's header gives rather
    // than as an exception to it: the row was refused because `DirectKind::Lea` hard-coded a
    // 32-bit destination write, and it grew a `width` field in the same commit that admitted it.
    // The Tomb Raider loader census ranks the word row at 1,744,694 block-stopping hits. Its
    // admission is pinned from the other side by `cpu_jit_width_lift_test.rs`
    // (`lea16_writes_low_half_only` and `lea16_at_a_dword_address_size_keeps_the_high_half`), and
    // the flip is asserted at the end of this test so leaving the table is not a silent removal.
    //
    // The `bool` is `refused_on_the_v86_on_arm` and it names ONE gate: `IZARRAVM_V86_LOOP_ROWS`,
    // the arm the loop below sweeps. It was written when that was the only gate in play; two are
    // now, so the name is spelled out rather than left as "refused on the ON arm too", which at the
    // `0x85` row would read as a claim about `IZARRAVM_TEST_WORD_ROWS` and be FALSE — that row is
    // refused here only because the test states `TEST_WORD_ROWS=off` above, and it is asserted
    // admitted under `=on` at the end of this test. (2026-08-21 review, N5.)
    //
    // `0x05` is the one row whose value is `false`: its whole form (ADD/OR/AND/SUB/XOR/CMP with a
    // full-width immediate) joins the allowlist under the V86 gate, which
    // `cpu_jit_v86_loop_rows_test.rs` pins from the other side.
    let cases: &[(&str, &[u8], bool)] = &[
        ("0x05 add eax,imm", &[0x66, 0x05, 0x34, 0x12], false),
        // Refused on both V86 arms, but only while TEST_WORD_ROWS is off — stated at the top of
        // this test and flipped at the bottom.
        ("0x85 test r/m,r", &[0x66, 0x85, 0xc0], true),
        ("0xa9 test eax,imm", &[0x66, 0xa9, 0x34, 0x12], true),
    ];

    for arm in [false, true] {
        jit::direct::set_v86_loop_rows_for_test(Some(arm));
        for &(label, form, refused_on_the_v86_on_arm) in cases {
            if arm && !refused_on_the_v86_on_arm {
                continue;
            }
            word_size_sibling_stays_refused(label, form, arm);
        }
    }
    // ...and the row that FLIPS, asserted rather than skipped, so the `continue` above cannot
    // quietly become the whole test.
    jit::direct::set_v86_loop_rows_for_test(Some(true));
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(&[0x66, 0x05, 0x34, 0x12]);
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "0x05 add ax,imm16 must JOIN the block once IZARRAVM_V86_LOOP_ROWS admits ALU form 5"
    );
    jit::direct::set_v86_loop_rows_for_test(None);

    // ...and the SECOND row that flips, for the same reason and in the same shape. Without this
    // the `set_test_word_rows_for_test(Some(false))` at the top of the test would be a way of
    // making the `0x85` row pass rather than a way of making it mean something.
    jit::direct::set_test_word_rows_for_test(Some(true));
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(&[0x66, 0x85, 0xc0]);
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "0x85 test ax,ax must JOIN the block once IZARRAVM_TEST_WORD_ROWS admits the Word \
         register form"
    );
    jit::direct::set_test_word_rows_for_test(None);

    // ...and the group-3 rows that left the table outright, asserted here so their removal above
    // is a moved set rather than a deleted one. The slot-class assertion lives with them in
    // `group3_word_subops_join_as_call_outs_not_lowerings`; what this one owes is the flip.
    for (label, form) in [
        ("0xf7 /2 not r/m", [0x66u8, 0xf7, 0xd1]),
        ("0xf7 /3 neg r/m", [0x66, 0xf7, 0xd9]),
        ("0xf7 /4 mul r/m", [0x66, 0xf7, 0xe1]),
        ("0xf7 /6 div r/m", [0x66, 0xf7, 0xf1]),
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
            compilation.span.instructions, 4,
            "{label} at Word must JOIN the block since the S3 policy widening"
        );
        assert_eq!(
            compilation.callout_interpret_one_slots, 1,
            "{label} must join as a call-out, never through a dword lowering"
        );
    }

    // ...and the row that left the table outright, asserted here so its removal above is a moved
    // row rather than a deleted one. `0x8d` is ungated: it needs neither knob.
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(&[0x66, 0x8d, 0x40, 0x10]);
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "0x8d lea ax,[eax+0x10] must JOIN the block since the S1 width lift"
    );
}

/// One row of the table above: three filler INCs, the form under test, and the block must stop at
/// the fillers because the form is off the Word allowlist.
fn word_size_sibling_stays_refused(label: &str, form: &[u8], arm: bool) {
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
        "{label}: must stay refused at Word size on the V86-loop-rows={arm} arm, so the block is \
         the three fillers"
    );
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
/// ADC (/2) and SBB (/3) join them as of the L1 width lift; see
/// `word_size_0x81_carry_forms_are_lowered` just below.
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

/// `0x81` ADC and SBB REGISTER forms are lowered at Word size, the same L1 lift `0x83` gets: the
/// two opcodes share the classifier arm and `0x81`'s immediate is just wider.
#[test]
fn word_size_0x81_carry_forms_are_lowered() {
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
            compilation.span.instructions, 4,
            "{label}: the word form must join the block rather than end it"
        );
    }
}

/// Group 3's `/0` at Word size: the REGISTER form is lowered through `TestImmReg`'s word lane and
/// the MEMORY form joins the block as an `InterpretOne` call-out. The wolf3d census ranked the
/// register form at 634M block-stopping hits and the post-S2 loader census ranks the memory form
/// at 242 k.
///
/// The memory half used to assert a REFUSAL, on the ground that no fixture measured a row for it.
/// The loader census measures one, and the S3 policy widening answers it with the call-out rather
/// than with an emitter, so the assertion moved from "the block ends here" to "the block carries
/// it, and carries it as a call-out": lowering it through `TestImmMem` at Word would still be the
/// bug the old refusal guarded against, and the slot-count check is what says it did not happen.
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
        compilation.span.instructions, 4,
        "test word [mem], imm16: the memory form must join the block"
    );
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "test word [mem], imm16: it must join as a call-out, not through TestImmMem's word lane"
    );
    assert_eq!(
        compilation.word_reads, 0,
        "a call-out slot declares no static access"
    );
}

/// `PUSH DS`, `PUSH ES`, `PUSH CS` and `PUSH SS` at Word size, the read half of the segment
/// family.
///
/// One word store and no reads: the selector is a compile-time constant baked from the block's
/// `SegmentLayout`, so the only memory the slot touches is the stack. `PUSH CS` joined on
/// 2026-08-08 (158M wolf3d census hits): `SegmentLayout::selector` reads the separate `cs`
/// field and `cs_matches` pins CS for every block unconditionally, so keeping CS out of the
/// `selector_segment` mask stays correct. `PUSH SS` MOVED HERE on 2026-08-22 from the refusal
/// table below (747,415 tombraid loader census hits). The shadow argument that kept it out is
/// about POP SS and MOV SS, which LOAD the stack segment; PUSH SS reads the selector and arms
/// nothing. Unlike CS it takes the ordinary data path, so it has to be in `used`, and every push
/// already puts it there through `write_segment`.
#[test]
fn word_size_push_segment_forms_are_lowered() {
    for (label, opcode) in [
        ("0x1e push ds", 0x1eu8),
        ("0x06 push es", 0x06),
        ("0x0e push cs", 0x0e),
        ("0x16 push ss", 0x16),
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
/// `0x07` and `0x1f` moved on 2026-08-20: the V86 loop-A slice lowers POP ES and POP DS behind
/// `IZARRAVM_V86_LOOP_ROWS`, so both are allowlist refusals on the OFF arm and lowerings on the ON
/// one. `0x16` PUSH SS left this table on 2026-08-22 for the positive one above; it loads nothing
/// and arms nothing, so the shadow argument never applied to it.
///
/// `0x17` POP SS left it on the same day for a different reason and is asserted from the other
/// side just below: it is an `InterpretOne` call-out on BOTH arms, independent of this gate,
/// because the interrupt shadow it arms is decided at the block boundary rather than refused at
/// compile time. That pairing is what the table now holds: two rows that flip with the gate and
/// one that does not depend on it at all.
///
/// `flat_fixture` builds a REAL-mode CPU (`fresh()` with CS.D forced on), and the rows above force
/// SS.B off, which is exactly the cell `PopSegReal` is admitted in: real mode, 16-bit stack, Word
/// operand size. So on the ON arm the two flipping rows really do join the block here, and this
/// table asserts the flip in both directions rather than describing it.
#[test]
fn word_size_push_segment_forms_outside_the_slice_stay_refused() {
    // The `bool` is "refused on the ON arm too".
    let cases: &[(&str, &[u8], bool)] = &[
        ("0x1f pop ds", &[0x66, 0x1f], false),
        ("0x07 pop es", &[0x66, 0x07], false),
    ];

    for arm in [false, true] {
        jit::direct::set_v86_loop_rows_for_test(Some(arm));
        for &(label, form, refused_on_the_on_arm) in cases {
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

            let refused = !arm || refused_on_the_on_arm;
            let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
            assert_eq!(
                compilation.span.instructions,
                if refused { 3 } else { 4 },
                "{label}: on the V86-loop-rows={arm} arm this must {} at Word size",
                if refused {
                    "stay refused, leaving the block at the three fillers"
                } else {
                    "JOIN the block as a fourth slot"
                }
            );
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// `POP SS` joins the block as an `InterpretOne` call-out at BOTH operand sizes and on both arms
/// of the V86 loop-rows gate, which is the row's whole admission claim stated positively.
///
/// Both sizes because the two are different instructions to the interpreter: the 386 PRM has a
/// Dword POP SS move four bytes of stack and load the low sixteen, and the Word allowlist entry
/// only reaches the first of them. If the Dword form ever needed refusing, it would have to be
/// refused in the arm, and this is the assertion that would fail.
///
/// Both gate arms because `0x17` is not part of the V86 loop-rows slice at all. Its two
/// neighbours in the encoding, `0x07` and `0x1f`, are, and the table above pins them flipping;
/// this pins that the stack segment does not flip with them.
///
/// MUTATION: route `0x17` through the `PopSegReal` arm instead of its own and the Dword row fails
/// here, because `stack_width_kind` refuses that kind at every cell but the 16-bit Word one.
#[test]
fn word_and_dword_pop_ss_join_the_block_as_call_outs() {
    for arm in [false, true] {
        jit::direct::set_v86_loop_rows_for_test(Some(arm));
        for (label, form) in [
            ("0x17 pop ss", &[0x17u8][..]),
            ("66 17 pop ss", &[0x66, 0x17]),
        ] {
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
                compilation.span.instructions, 4,
                "{label}: must join the block (v86 loop rows = {arm})"
            );
            assert_eq!(
                compilation.callout_interpret_one_slots, 1,
                "{label}: must join as a call-out, never as a lowering (v86 loop rows = {arm})"
            );
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
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

/// Every `0x8e` shape that is NOT the real-mode register lowering, and the answer each gets.
///
/// `/1`, `/6` and `/7` are the ones that matter most and they stay REFUSED: they are not segment
/// loads at all, the interpreter raises #GP(0) for each, and compiling a block around an
/// instruction that can only fault burns a call-out on every execution.
///
/// `/2` SS was refused beside them over the one-instruction interrupt shadow until S4 part 2,
/// which admits it as a call-out on its own census row: the shadow clause is scoped per row now
/// and the flag is decided at the block boundary. It sits in this table rather than with `/4` and
/// `/5` because it is still the shape whose answer a widening is most likely to get wrong, and
/// because a refusal reappearing here would be a silent revert.
///
/// `/4` FS, `/5` GS and the memory forms were refused here as "out of scope" until the S3 policy
/// widening, which admits them as `InterpretOne` CALL-OUTS. The claim moves rather than lapses:
/// what this table said was that they must not be LOWERED as `LoadSegReal`, which emits
/// `base = selector << 4` and nothing else, and the slot-class column is how that is said now.
///
/// `/3`'s memory form flipped again with the L4 slice: `flat_fixture` is REAL MODE (`fresh()`
/// with the segment limits stretched to `u32::MAX`, not protected mode), so ES/DS memory-source
/// `MOV Sreg, m16` now lowers through `LoadSegRealMem` exactly as the register form already did,
/// joining the block with ZERO call-out slots. `/2`, `/4` and `/5` are unaffected -- SS keeps its
/// own row and FS/GS have no real-mode lowering -- so they still call out.
#[test]
fn word_size_load_segment_shapes_outside_the_slice_get_their_own_answers() {
    // (label, bytes, call-out slots; `None` means the shape must end the block)
    let cases: &[(&str, &[u8], Option<u8>)] = &[
        ("/1 mov cs,ax is #GP", &[0x66, 0x8e, 0xc8], None),
        ("/6 mov ?,ax is #GP", &[0x66, 0x8e, 0xf0], None),
        ("/7 mov ?,ax is #GP", &[0x66, 0x8e, 0xf8], None),
        ("/2 mov ss,ax calls out", &[0x66, 0x8e, 0xd0], Some(1)),
        ("/4 mov fs,ax calls out", &[0x66, 0x8e, 0xe0], Some(1)),
        ("/5 mov gs,ax calls out", &[0x66, 0x8e, 0xe8], Some(1)),
        (
            "/3 memory form lowers natively in real mode",
            &[0x66, 0x8e, 0x1d, 0x00, 0x20, 0x00, 0x00],
            Some(0),
        ),
    ];

    for &(label, form, call_outs) in cases {
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
        match call_outs {
            None => assert_eq!(
                compilation.span.instructions, 3,
                "{label}: must stay refused, so the block is the three fillers"
            ),
            Some(slots) => {
                assert_eq!(
                    compilation.span.instructions, 4,
                    "{label}: must join the block"
                );
                assert_eq!(
                    compilation.callout_interpret_one_slots, slots,
                    "{label}: call-out slot count must match its lowering class"
                );
            }
        }
    }
}

/// `MOV Sreg, r16` in PROTECTED mode, where a segment load is a descriptor fetch with type,
/// privilege and present checks rather than `selector << 4`.
///
/// The decision lives beside the `stack_width_kind` call because `classify` has no CPU. It used to
/// be a refusal; since the S3 policy widening it is an `InterpretOne` call-out, which runs
/// `load_protected_segment` with every check, every fault vector and the accessed-bit write-back,
/// and then lets R2 decide whether the block may carry on.
///
/// What this pins is that it is NOT `LoadSegReal`. The mode key would stop a real-mode block from
/// being ENTERED in protected mode, but it would not stop one being COMPILED here, which is the
/// failure this test has always been about.
#[test]
fn the_protected_mode_load_segment_form_calls_out_rather_than_lowering() {
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
        compilation.span.instructions, 4,
        "protected mode must join the block"
    );
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "protected mode must call out, never lower a descriptor fetch as selector << 4"
    );
}

/// `POP ES` and `POP DS` keep the protected-mode refusal `MOV Sreg` left behind.
///
/// They share `stack_width_kind`'s arm with `LoadSegReal` and used to share its answer. They are
/// not on the `InterpretOne` allowlist -- no census row measures them -- so the arm splits, and
/// this is the half that would otherwise have been swept along by proximity.
#[test]
fn the_protected_mode_pop_segment_form_stays_refused() {
    for (label, opcode) in [("pop es", 0x07u8), ("pop ds", 0x1f)] {
        let mut code = vec![0x40, 0x41, 0x42];
        code.push(opcode);
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
            "{label}: must stay refused in protected mode"
        );
        assert_eq!(compilation.callout_interpret_one_slots, 0);
    }
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

/// ADC and SBB in forms 1 and 3 are LOWERED at Word size, the L1 width lift: the register forms
/// gained a Word carry lane in `emit_carry_alu_preloaded`, so the guard that used to hold them out
/// is gone from the classifier arm.
///
/// This is the boundary that mattered most before the lift, so it is worth restating why the
/// allowlist alone could not have held it and why the arm can now let go: the file's stated rule
/// is that the byte set is closed over its shared classifier arms, and the slice that opened
/// forms 1 and 3 admitted five of eight members of two shared arms, so a silent miscompile was one
/// line away for as long as the sixth and seventh members reached the OLD `emit_alu_preloaded`
/// Word lane -- it masked both operands with `and`, which CLEARS host CF, then tagged the
/// descriptor as the SUB class, so an admitted `66 11 /r` would have computed `adc` without its
/// carry in. `emit_carry_alu_preloaded`'s Word arm loads the host CF from the guest shadow before
/// the masked `alu_r16_r16`, which is the lane that closes the gap; the differential coverage lives
/// in `cpu_jit_word_memory_test.rs` and `cpu_jit_sweep_lowering_test.rs`, and this fixture only
/// pins that the classifier arm actually emits a slot instead of ending the block.
#[test]
fn word_size_alu_carry_forms_are_lowered() {
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
            compilation.span.instructions, 4,
            "{label}: the word form must join the block rather than end it"
        );
        assert_eq!(compilation.word_reads, 0, "{label}: word reads");
        assert_eq!(compilation.word_stores, 0, "{label}: word stores");
        assert_eq!(compilation.dword_reads, 0, "{label}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{label}: dword stores");
    }
}

/// Both form 1 and form 3's MEMORY shapes are now LOWERED at Word size, and form 1's Dword shape
/// keeps shipping.
///
/// Before the S-B ALU-rows slice the two arms parted company on the SITE: form 1 (`AluMemDest`,
/// a read-modify-write through site 6) refused every writing op at Word, while form 3
/// (`AluMemSource`, a pure read through a relaxed lean one-lookup site) admitted the whole
/// non-carry set. That split is gone -- `emit_wide_page_guard`'s alignment half is unconditional
/// at Word regardless of site, so a misaligned form-1 operand side-exits rather than sitting
/// inside the block, and the site distinction the old comment argued from was economics, not a
/// correctness gate. See the classify arm's own comment for the citation (`860698e5`).
///
/// CMP (`0x39`, `0x3b`) was the pre-slice exception on form 1 and the S-B ALU-rows slice's own
/// control: it has been compiling word memory in quake's renderer since before either change, and
/// now sits beside every other form-1 op rather than alone.
#[test]
fn word_size_alu_memory_shapes_split_by_site() {
    // Every op runs through both forms now: form 1 (`m,r`) and form 3 (`r,m`), all admitted at
    // Word, the block covering all four slots. Form 1 counts one word read AND one word store
    // (`AluMemDest` is a read-modify-write); form 3 counts one word read and zero stores
    // (`AluMemSource` never writes guest memory).
    for (label, form, word_reads, word_stores) in [
        (
            "0x39 cmp m,r",
            [0x66, 0x39, 0x0d, 0x00, 0x20, 0x00, 0x00],
            1,
            0,
        ),
        (
            "0x3b cmp r,m",
            [0x66, 0x3b, 0x0d, 0x00, 0x20, 0x00, 0x00],
            1,
            0,
        ),
        (
            "0x03 add r,m",
            [0x66, 0x03, 0x0d, 0x00, 0x20, 0x00, 0x00],
            1,
            0,
        ),
        (
            "0x33 xor r,m",
            [0x66, 0x33, 0x0d, 0x00, 0x20, 0x00, 0x00],
            1,
            0,
        ),
        (
            "0x01 add m,r",
            [0x66, 0x01, 0x0d, 0x00, 0x20, 0x00, 0x00],
            1,
            1,
        ),
        (
            "0x21 and m,r",
            [0x66, 0x21, 0x0d, 0x00, 0x20, 0x00, 0x00],
            1,
            1,
        ),
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
        assert_eq!(compilation.word_reads, word_reads, "{label}: word reads");
        assert_eq!(compilation.word_stores, word_stores, "{label}: word stores");
    }

    // The Dword form-1 row is unaffected: it was admitted before this slice and stays admitted.
    let (mut cpu, mut bus) = flat_fixture(
        ENTRY,
        &[0x40, 0x41, 0x42, 0x01, 0x0d, 0x00, 0x20, 0x00, 0x00],
    );
    map_word_operand_page(&mut cpu, &mut bus);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "0x01 add m,r at dword size"
    );
}

/// The two `0x83` carry shapes, register AND memory, are LOWERED at Word size -- the L1 width
/// lift's headline row.
///
/// ADC (/2) and SBB (/3) consume the incoming CF as an OPERAND. The REGISTER form used to have no
/// sixteen-bit lane for that at all; `emit_carry_alu_preloaded` grew one. The MEMORY form's
/// emitter (`emit_alu_candidate` / `emit_commit_alu_candidate`) was already width-parameterised and
/// already branched on the incoming CF for ANY width, so admitting it here was a pure classifier
/// change -- pyramid-586's `0x83 word mem /2` row is 18,902,081 runtime hits, the largest
/// non-monitor row in the corpus's slowest game, and it is this memory case exactly.
#[test]
fn word_size_0x83_carry_forms_are_lowered() {
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
            compilation.span.instructions, 4,
            "{label}: the word form must join the block rather than end it"
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
/// Two separate refusals share this table now (a third, the ROL/ROR Word guard, was RETIRED by
/// `vorvek/direct-word-rot1` -- see below):
///
/// * **RCL (`/2`) and RCR (`/3`) of both `0xC1` and `0xD1`.** Neither has ever had a classify arm
///   at any width -- the standing structural reason is that both take the incoming CF as a rotate
///   INPUT, which needs the guest flags loaded before the rotate rather than only captured after
///   it. `/2` RCL is the largest single row in the 16-bit census at 10.91%, so it is the shape most
///   likely to be admitted by a hurried edit, which is why it still has a row here even though the
///   refusal predates this file.
/// * **`0xD3`, the shift-by-CL group.** A different arm, still Dword-only: `emit_shift_cl` has no
///   sixteen-bit lane and would be a second emitter primitive.
///
/// **ROL (`/0`) and ROR (`/1`) of `0xC1` and `0xD1` moved OUT of this table.**
/// `vorvek/direct-word-rot1` gave `RotateReg` a `width` field and `emit_rotate_reg` a
/// `shift_r16_imm8` arm, so a 66-prefixed rotate now narrows to the guest's own 16 bits instead of
/// rotating 32 -- the guard this table used to pin is gone by design, not by accident. Their
/// positive coverage is `group2_word_rotate_register_form_is_lowered` in
/// `cpu_jit_test_imm_test.rs` and the differential sweep in `cpu_jit_word_rotate_test.rs`.
///
/// **The MEMORY form of `0xC1 /4` moved OUT of this table too**, for the same class of reason and
/// on `vorvek/direct-rot-mem-lane`: the `else` branch of that register-only bind now produces
/// `DirectKind::RotateShiftMem`, so the refusal this table used to pin at both operand sizes is
/// gone by design. What replaced it here is a positive row on the RCL memory form, which is the
/// assertion the table actually needs now: the memory admission must not have swept `/2` and `/3`
/// in with the rest. The lane's own coverage is `cpu_jit_group2_mem_test.rs`.
#[test]
fn the_word_size_group_two_shapes_outside_the_shift_lane_stay_refused() {
    // NO KNOB FORCE, and that absence is deliberate rather than an oversight. Every row left in
    // this table refuses UNCONDITIONALLY, on both arms of `IZARRAVM_ROTATE_ROWS`: RCL/RCR have
    // never had a classify arm at any width at EITHER operand form. A force here would be a gate
    // that cannot fail -- it would pass identically with the knob on or off, certifying nothing
    // about the knob and hiding that fact from a reader. (The one row that DID depend on the
    // knob, `0xC1`/`0xD1 /0` ROL, moved OUT of this table as of `vorvek/direct-word-rot1`:
    // `RotateReg` now carries a `width` field and is admitted at Word, so its positive coverage
    // -- knob on and off -- lives in `cpu_jit_word_rotate_test.rs` and
    // `group2_word_rotate_register_form_is_lowered` instead. `0xD3 /4` and `/7`, shift-by-CL,
    // moved out the same way on the S-B ALU-rows slice: `ShiftCl` now carries a `width` field
    // and is admitted at Word behind `IZARRAVM_WORD_SHIFT_CL_ROWS`, and its positive coverage
    // lives in `cpu_jit_word_shift_cl_test.rs`. `/2` RCL and `/3` RCR are what is left here for
    // `0xD3`: `classify`'s arm narrows to `matches!(m.reg, 4..=7)`, so the rotate sub-opcodes
    // never reach the admission at all.)
    let cases: &[(&str, &[u8])] = &[
        ("0xc1 /2 rcl cx,imm8", &[0x66, 0xc1, 0xd1, 0x03]),
        ("0xc1 /3 rcr cx,imm8", &[0x66, 0xc1, 0xd9, 0x03]),
        ("0xd1 /2 rcl cx,1", &[0x66, 0xd1, 0xd1]),
        ("0xd1 /3 rcr cx,1", &[0x66, 0xd1, 0xd9]),
        ("0xd3 /2 rcl cx,cl", &[0x66, 0xd3, 0xd1]),
        ("0xd3 /3 rcr cx,cl", &[0x66, 0xd3, 0xd9]),
        // The MEMORY forms of RCL and RCR, at both operand sizes. These replace the two `/4`
        // memory rows the group-2 memory lane admitted: what has to be pinned now is that the
        // lane's `matches!(reg, 0 | 1 | 4..=7)` whitelist did not sweep `/2` and `/3` in with the
        // rest of the family.
        (
            "0xc1 /2 rcl word [m],imm8",
            &[0x66, 0xc1, 0x15, 0x00, 0x20, 0x00, 0x00, 0x03],
        ),
        (
            "0xc1 /3 rcr dword [m],imm8",
            &[0xc1, 0x1d, 0x00, 0x20, 0x00, 0x00, 0x03],
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
/// `0xff` is in the Word allowlist, so this form reaches the classifier. Until the S3 policy
/// widening it produced a `PushMem` there and was refused one step later by the stack-width
/// admission matrix, which is consulted only for kinds whose `uses_stack()` is true; admitting it
/// as `PushMem` would push two bytes while decrementing ESP by four, which is a miscompile rather
/// than a missed lowering.
///
/// It now joins the block as an `InterpretOne` call-out instead, decided in the classifier arm
/// rather than in the matrix. The claim this fixture makes is unchanged in substance and moves
/// from a count to a SLOT CLASS: the Word form must not be lowered as `PushMem`, and the way to
/// say so now that it compiles is that its slot is a call-out and the block declares no static
/// stack access for it.
///
/// The POSITIVE CONTROL stays and does more work than before: the unprefixed `FF /6` at the same
/// entry, on the same stack width, must lower through `PushMem` with no call-out slot at all. The
/// two cases now differ in HOW they join rather than in whether they do, which is a sharper pairing
/// than the old one.
///
/// Both cases widen SS to a 32-bit stack explicitly. `flat_fixture` widens the CS and SS LIMITS
/// but leaves `SS.default_size_32` alone, and the stack-width admission matrix refuses `PushMem`
/// on a 16-bit stack (`SS.B` = 0) regardless of operand size. Left unwidened, the control would
/// also come out to 3 instructions, for a reason that has nothing to do with the 0x66 prefix, and
/// the pairing would prove nothing. Widening SS here makes the prefix the ONLY difference between
/// the two cases.
#[test]
fn word_size_push_through_memory_calls_out_rather_than_lowering() {
    let cases: &[(&str, &[u8], u8)] = &[
        (
            "unprefixed control",
            &[0xff, 0x35, 0x00, 0x08, 0x00, 0x00],
            0,
        ),
        (
            // 66 ff 35 00 08 00 00: push word [0x800] at Word operand size.
            "0x66-prefixed",
            &[0x66, 0xff, 0x35, 0x00, 0x08, 0x00, 0x00],
            1,
        ),
    ];

    for &(label, form, expected_call_outs) in cases {
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
            compilation.span.instructions, 4,
            "{label}: both forms must carry the whole four-slot block"
        );
        assert_eq!(
            compilation.callout_interpret_one_slots, expected_call_outs,
            "{label}: the Dword form must lower through PushMem and the Word form must call out"
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
/// `word_size_push_through_memory_calls_out_rather_than_lowering` does: `flat_fixture` leaves `SS.default_size_32`
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
