// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn classify(insn: &DecodedInsn, lin: u32, entry_lin: u32) -> Option<DirectKind> {
    if insn.group == DecodeGroup::Fpu {
        if insn.operand_size != OperandSize::Dword {
            return None;
        }
        let native = NativeX87Insn::classify(insn)?;
        // The two FPU-loop rows are gated HERE rather than inside `NativeX87Insn::classify`,
        // which is a pure encoding table -- `native_x87_test.rs` asserts it opcode by opcode with
        // no CPU and no environment in scope, and threading a policy knob through it would make
        // every one of those rows read the ambient arm. This is the same seam `classify` already
        // uses for `rotate_rows_enabled` and `count_lanes_enabled`.
        //
        // The Word bar for both rows is the `operand_size != Dword` early return above, which is
        // the FPU branch's blanket bar and predates this slice: in a CS.D = 0 segment every
        // unprefixed instruction decodes at `Word`, WAIT and FRNDINT included, so neither is
        // admitted there whether or not a prefix is present.
        if matches!(native, NativeX87Insn::Wait | NativeX87Insn::RoundToInt)
            && !fpu_loop_rows_enabled()
        {
            return None;
        }
        let addr = match native {
            NativeX87Insn::BinaryMemory { addr, .. }
            | NativeX87Insn::IntBinaryMemory { addr, .. }
            | NativeX87Insn::LoadF32 { addr }
            | NativeX87Insn::StoreF32 { addr, .. }
            | NativeX87Insn::LoadI32 { addr }
            | NativeX87Insn::StoreI32 { addr, .. }
            // Every variant whose `metadata().memory` is Some must appear here. The `_ => None`
            // below is silent: a missing arm leaves `addr: None`, `emit_x87_slot`'s
            // `addr.expect(..)` panics at block compilation, and behind that panic
            // `DirectKind::read_segment` would have dropped the segment from the block's
            // `SegmentLayout` mask and made `kind_segment_access_supported` trivially true.
            | NativeX87Insn::LoadControlWord { addr }
            | NativeX87Insn::StoreControlWord { addr }
            | NativeX87Insn::LoadF64 { addr }
            | NativeX87Insn::StoreF64 { addr, .. }
            | NativeX87Insn::BinaryMemoryF64 { addr, .. }
            | NativeX87Insn::LoadI64 { addr }
            | NativeX87Insn::StoreI64 { addr }
            | NativeX87Insn::StoreExtended80 { addr } => Some(direct_addr(addr)?),
            _ => None,
        };
        return Some(DirectKind::X87 { insn: native, addr });
    }
    let operand_width = match insn.operand_size {
        OperandSize::Word => MemoryWidth::Word,
        OperandSize::Dword => MemoryWidth::Dword,
    };
    // The Jcc ranges are the only control transfers admitted at Word size. Both are matched on
    // the FULL u16 opcode here, above the `u8::try_from(insn.opcode)` truncation further down, so
    // `0x0f80..=0x0f8f` is well-typed and `0x70..=0x7f` cannot alias the two-byte 0x0f7x block the
    // way it would below the truncation.
    //
    // A Word-size relative branch masks its target to 16 bits, and the emitted form bakes an
    // unmasked delta. What makes that safe is the compile loop's `control_target_limit` clamp,
    // which refuses any Word control target above the wrap. Admitting a control transfer here
    // WITHOUT that clamp is a silent wrong-branch miscompile, not a missed lowering.
    // The BYTE-OPERAND opcodes below are admitted at Word size for a reason that is structural
    // rather than per-opcode, and it is worth stating once here instead of at each arm.
    //
    // `operand_size` is computed from CS.D and the 0x66 prefix ALONE and is opcode-independent
    // (`decode.rs`), so in a 16-bit code segment EVERY unprefixed instruction reports `Word`,
    // byte-operand forms included. This gate is therefore a blanket filter that catches them as
    // collateral. Admitting them changes nothing about how they lower, because their width is a
    // property of the FORM: each produces a kind carrying a literal `MemoryWidth::Byte`, or a
    // kind with no width at all. Nor can the operand size leak past this function: `DirectInsn`
    // carries only `lin`, `len`, `weighted_fp_clocks` and `kind`, and `EmitInput` carries no
    // `OperandSize` either, so every width decision downstream comes from the kind.
    //
    // The byte set is CLOSED over its shared classifier arms on purpose. `0x04..=0x3c` step 8 are
    // all `form == 4` of the ALU group and reach one arm; `0x00..=0x38` and `0x02..=0x3a` step 8
    // are `form == 0` and `form == 2` and reach one arm each; `0xf6` is the byte half of the
    // `0xf6 | 0xf7` group arm, whose every Dword-producing path is keyed `opcode == 0xf7`.
    // Admitting one member of a shared arm while refusing its sibling would be arbitrary, and
    // what makes 16-bit blocks link is a CONTIGUOUS admissible region rather than any single
    // opcode.
    //
    // Forms 0 and 2 joined the set on the 16-bit campaign's first slice, and the reason they were
    // not here before is that the 32-bit fixtures do not reach them: a barrier census over
    // `.bench/bench16_c` at 586 with `IZARRAVM_JIT16=1` ranks byte-encoded opcodes at 14.73% of
    // all 347,134,532 block-stopping hits, `0xfe` at 3.39% and `0x38` at 2.69% heading them.
    // Both forms satisfy the structural rule above rather than bending it: form 0 produces
    // `AluRegByte`, which has no width field at all, or `AluMemDest` carrying a literal
    // `MemoryWidth::Byte`; form 2 produces `AluRegByte` for the register shape and refuses the
    // memory shape inside its arm, so the `None` it already returns is what a 16-bit
    // `0x0a /0 mem` keeps getting. `0xa0`, `0xa2` and `0xfe` are the same case one arm at a time
    // (`Load`/`Store`/`IncDecReg`, every one a literal `MemoryWidth::Byte`).
    //
    // `0xa1` and `0xa3` sat between them in the opcode map and WERE the counterexample worth
    // naming, because proximity is exactly how they would have got swept in: both hard-coded
    // `MemoryWidth::Dword`, so admitting either at Word size would have moved four bytes where the
    // guest moves two. That is no longer the shape of the answer. The V86 loop-A slice gave both
    // arms `operand_width` -- the same fix MOVZX/MOVSX got with `dst_width` rather than being kept
    // off the list -- and put them on the GATED allowlist below. The hazard the paragraph named was
    // real and is now expressed instead of avoided; see the two arms for the width argument at each
    // end. `0xd0` does still stay refused, and not for a width reason: no classify arm exists for
    // it, which makes listing it a no-op that reads like a lowering.
    //
    // `0x8c` is the one non-byte member and it is here for the same structural reason rather than
    // as an exception: its interpreter arm writes `OperandSize::Word` unconditionally, so the
    // 66-prefixed and unprefixed encodings have identical semantics and `MovSegToReg` carries no
    // width to get wrong. Its Dword-sibling hazard does not exist because it has no Dword sibling.
    //
    // Deliberately NOT here, and each would be a miscompile rather than a missed lowering:
    // `0xf7` and `0xa9`. Both are the Dword sibling of an admitted byte form and their kinds
    // hard-code Dword with no width field. (`0xc7` and `0x81` left this list when they grew width
    // fields of their own; `0xa3` left it with the V86 loop-A slice, for the reason the paragraph
    // above now gives; `0x85` left it with the Word TEST row; `0x8d` left it with the S1 width
    // lift, which gave `Lea` a `width` and narrowed its destination write with
    // `emit_write_gpr16`.)
    //
    // `0xb8..=0xbf` WAS on that list and is the 16-bit campaign's fourth slice. It left the same
    // way `0x83` and `0xc7` did, by growing the width field the list existed to compensate for:
    // `MovImm` now carries one and `emit`'s Word arm stages the immediate through RDX and narrows
    // with `emit_write_gpr16`, which defines sixteen bits and preserves the rest exactly as
    // `write_gpr_sized(.., Word, ..)` does. `0xc7`'s REGISTER form still produces `MovImm` and is
    // still refused at Word, by the arm rather than by this list, so it keeps its own test.
    //
    // THE WORD ALU REGISTER FORMS, forms 1 and 3, are the 16-bit campaign's second slice.
    // `0x01`/`0x09`/`0x21`/`0x29`/`0x31` and `0x03`/`0x0b`/`0x23`/`0x2b`/`0x33` join `0x39`/`0x3b`,
    // which have been carrying the Word lane as CMP since before this list existed. An older
    // version of this comment called them "worse still" than the Dword siblings, because
    // `emit_alu_preloaded`'s Word branch once ignored `op`, hard-coded SUB and wrote to a scratch
    // register, which is correct only for CMP. That branch handles the whole non-carry op set
    // with a `mov_r16_r16` write-back now, and `0x83` has been exercising exactly that write-back
    // in production since its own slice, with a mutation record behind it. The census asked: a
    // 16-bit workload ranks these ten rows near 19% of block-stopping hits.
    //
    // Two exclusions hold the boundary, and each is enforced in the ARM rather than by this
    // list, because a list is the wrong place for a rule the next reader has to re-derive:
    // ADC and SBB at Word (no carry-in lane), and form 1's MEMORY shape at Word (see the arms).
    // Form 3's memory shape used to be the third: it is admitted as of the B2 slice, because it
    // only READS and so lowers through the relaxed lean read site. See that arm.
    //
    // `0x83` WAS on that list and is now admitted, which is the second half of this slice. Its
    // register form produces `AluImm`, which carries a `width` field as of this commit, and its
    // memory form is refused inside the arm (see there). The census ranks `0x83 /5` word at
    // 9,776,289 doom exits, forty-seven apart from `0x60` PUSHAD -- one function prologue, so the
    // two must land together or neither's exits go anywhere.
    //
    // `0x81` joined on 2026-08-08 (the wolf3d demo-workload census ranked its `/7` word register
    // form at 634M block-stopping hits, the single largest row). The immediate-path check the
    // previous version of this comment demanded is satisfied by inspection of the emitter's word
    // lane: `emit_alu` stages ANY immediate through `mov_r32_imm32(RCX, ..)` and the word lane
    // masks BOTH operands with `and .., 0xffff` before the 66-prefixed `alu_r16_r16`, so a raw
    // `fetch_u16` immediate (0..0xFFFF) computes exactly as `0x83`'s sign-extended imm8 already
    // did. The arm below is shared with `0x83` and was width-complete before this admission:
    // ADC/SBB word-refused, memory word form certified by `cpu_jit_word_memory_test.rs`.
    //
    // THE SIXTEEN-BIT MEMORY ROWS (rejected-row campaign, slice 3) add `0xc7` and the four
    // MOVZX/MOVSX opcodes. The domain question this list exists to answer is not "which opcode",
    // it is which of two kinds of path a form lands on, and the two answer differently:
    //
    //  * Paths that CARRY a `MemoryWidth` end to end express Word already, and 0x89 and 0x8b have
    //    been proving it in production since they were admitted -- `emit_store`'s Word arm is
    //    `store_r16_disp8` plus `emit_dynamic_word_increment`, guarded by `emit_wide_page_guard`
    //    at 2-byte alignment and by `emit_watched_store_guard`, whose `needs_alignment_guard`
    //    branch probes the LAST byte of the access as well as the first. So a two-byte store
    //    writes exactly two bytes, cannot straddle a page, and cannot slip past a code watch that
    //    covers only its second byte. `0xc7`'s memory form is that same `Store`, and `0x83`'s is
    //    `AluMemDest`, whose Word arms are complete at every stage (`movzx_r32_word_disp8` read,
    //    `alu_r16_r16` candidate, descriptor tag 0x100, `store_r16_disp8` write-back, word
    //    counters on both the RAM and the mode-13 path) and were simply never reachable.
    //  * Paths that hard-code a 32-bit destination write cannot, and no allowlist entry fixes
    //    that. MOVZX/MOVSX were the case here: `emit_load_extend` ended in `mov_r32_r32`, which
    //    defines all 32 bits where a 66-prefixed form defines 16. The fix is the `dst_width`
    //    field, not the admission -- see the MOVZX arm below.
    //
    // Still OUT for the second reason, and each would be a miscompile: `0xb8..=0xbf` and `0xc7`'s
    // REGISTER form, both of which produce `MovImm`, whose `mov_r32_imm32(home(dst), imm)` has no
    // width and would clobber the destination's high half. `0xc7` is on the list because its
    // MEMORY form is the census row; its register form is refused inside its own arm. Neither
    // fixture measures a `0xc7` register word row, so building `MovImm` a width would be an
    // unmeasured admission with no row to attribute it to.
    //
    // THE SIXTEEN-BIT REGISTER SHIFT LANE (rejected-row campaign, slice 3b) adds `0xc1`, and it is
    // the second kind of path above rather than the first: `Shift` hard-coded a 32-bit destination
    // write until this slice gave it a `width` field, so the admission and the field are one
    // change. Slice 3 is what asked -- lowering quake's `0x0FB6` memory-word row let its blocks
    // extend one instruction further and stop on `shl cx, imm8` instead, relocating 30,692 exits
    // onto `0xC1 /4` and costing quake +8.78% blocks installed and ~1% of wall.
    //
    // `0xd1` is the SAME classifier arm and is now admitted with it, which is the 16-bit
    // campaign's third slice. It was held out while no fixture measured a `0xd1` word row; one
    // does now, and it is the largest single opcode in that census at 21.86% of 260,594,435
    // block-stopping hits.
    //
    // No emitter work, and the reason is stronger than a shared arm: `0xd1` and `0xc1` with an
    // immediate of 1 produce the SAME `DirectKind::Shift`, and `shift_r16_imm8` has no by-one
    // encoding at all, so `66 D1 /4` assembles as the bytes `66 C1 E1 01`. `DirectInsn` carries
    // only `lin`, `len`, `weighted_fp_clocks` and `kind`, and the compile loop reads
    // `insn.opcode` in exactly two places, neither of which separates the two. The clock charge
    // is the same 2 down both paths because the interpreter's group-2 arm returns one
    // `clocks(2)` for the whole `0xc0..=0xd3` range without discriminating.
    //
    // What it buys is a QUARTER of that 21.86%: the arm admits register `/4`, `/5` and `/7` only,
    // which the census splits at 10.75%. `/2` RCL register word is 10.91% on its own, larger than
    // every admitted shift together, and it is the other half of the `shl ax,1` / `rcl dx,1`
    // idiom that shifts a 32-bit quantity through two 16-bit registers. Expect the exits this
    // removes to relocate straight onto RCL. It has no arm at any width and would need the
    // incoming CF loaded before the rotate, so it is a real slice rather than a list entry.
    //
    // `0xd3` (the shift-by-CL group) is a different arm entirely and stays out, with
    // `emit_shift_cl` still Dword-only.
    //
    // `0xec` (IN AL,DX) is the V86 PORT CALL-OUT slice, and it is admitted here only because the
    // helper gained its second arm in the SAME commit. It is the first entry on this list whose
    // value is zero on its own and NEGATIVE on its own -- see the long note on the `0xec`
    // classifier arm below for the measurement that says so, and for why the pair moves together.
    if insn.operand_size == OperandSize::Word
        && !matches!(
            insn.opcode,
            0x00 | 0x08 | 0x10 | 0x18 | 0x20 | 0x28 | 0x30 | 0x38
                | 0x02 | 0x0a | 0x12 | 0x1a | 0x22 | 0x2a | 0x32 | 0x3a
                | 0x01 | 0x09 | 0x21 | 0x29 | 0x31
                | 0x03 | 0x0b | 0x23 | 0x2b | 0x33
                | 0x04 | 0x0c | 0x14 | 0x1c | 0x24 | 0x2c | 0x34 | 0x39 | 0x3b | 0x3c
                | 0x06 | 0x0e | 0x16 | 0x1e
                // POP SS, the S4 part-2 row. It is on this list rather than folded in with the
                // pushes above because it LOADS the stack segment: the arm below turns it into an
                // `InterpretOne` call-out whose resume depends on R2 finding the SS record
                // unchanged, and the loader census ranks it at 483,000 block-stopping hits.
                | 0x17
                | 0x40..=0x4f
                | 0x50..=0x5f
                | 0x68
                | 0x6a
                | 0x70..=0x7f
                | 0x80
                | 0x81
                | 0x83
                | 0x84
                | 0x88
                | 0x89
                | 0x8a
                | 0x8b
                | 0x8c
                // POP r/m16, the S2 `InterpretOne` row. Word matters more than Dword here: the
                // loader is Watcom-compiled 16-bit C and the census row is the word form. There is
                // no emitter and no width field to get wrong, because the helper runs the decode
                // line through the interpreter, so this entry is admission and nothing else.
                | 0x8f
                // LEA r16, m. Admitted with the S1 width lift, which is where `DirectKind::Lea`
                // grew its `width` field: the arm used to end in a full 32-bit destination write,
                // and the field is the admission rather than an accompaniment to it. The Tomb
                // Raider DOS/4GW loader census of 2026-08-21 ranks the word row at 1,744,694
                // block-stopping hits.
                | 0x8d
                | 0x8e
                // XCHG, the whole family, admitted to the S3 `InterpretOne` allowlist. Every one
                // of them decodes at `OperandSize::Word` in a 16-bit segment whatever its actual
                // operand width -- `0x86` is a byte exchange and takes its size from the encoding
                // -- so without this entry the Word gate refuses the loader's rows before the
                // classifier arm can see them. The census ranks `0x87` register word at 1.21 M
                // block-stopping hits and `0x93`/`0x97` at 507 k.
                //
                // No width field to get wrong: the arm produces a call-out, and the helper runs
                // the interpreter's own arm at whatever width the decode line carries.
                | 0x86
                | 0x87
                | 0x90
                | 0x91..=0x97
                | 0x98
                | 0x99
                | 0xa0
                | 0xa2
                | 0xa8
                | 0xb0..=0xb7
                | 0xb8..=0xbf
                | 0xc1
                | 0xd1
                | 0xc2
                // ENTER imm16, imm8 and LEAVE, the Watcom frame-pointer pair. Every function in
                // 16-bit C compiled with a frame pointer opens with one and closes with the
                // other, which is why they head the loader census at 1,977,855 and 1,277,833
                // block-stopping hits. Both are gated further inside their arms: ENTER at level 0
                // only, and both through the stack-width matrix, which builds the (operand, SS.B)
                // cell that has an emitter and refuses the one that does not.
                | 0xc8
                | 0xc9
                | 0xc3
                | 0xc6
                | 0xc7
                // The BIT-STRING family, admitted to the S3 `InterpretOne` allowlist. In a 16-bit
                // segment every one of them decodes at Word, so without this entry the loader's
                // rows never reach the classifier arm. What makes it safe is that the native `Bt`
                // lowering below is now gated on Dword IN ITS ARM rather than by this list's
                // silence: at Word the interpreter masks the bit index with `& 15` and that kind
                // carries no width, so the gate had to move somewhere it is stated.
                | 0x0fa3
                | 0x0fab
                | 0x0fb3
                | 0x0fbb
                | 0x0fba
                | 0x0fb6
                | 0x0fb7
                | 0x0fbe
                | 0x0fbf
                | 0xe8
                | 0xe9
                | 0xeb
                | 0xec
                | 0x0f80..=0x0f8f
                | 0xf6
                // CLI, the S3 policy widening's seventh row, at 244 k block-stopping hits in the
                // post-S2 loader census. It is a CALL-OUT rather than a lowering because of the
                // resume predicate rather than the emitter: clearing IF is one `and` on the flag
                // shadow, but the run loop's interrupt DELIVERY points are what the block's
                // boundaries are, and only R3 can decide whether the edge the instruction just
                // took is one of them.
                //
                // It resumes. Design review M8 made R3's IF clause DIRECTIONAL: IF going 1 to 0
                // cannot make an interrupt serviceable, so the run loop has no delivery point on
                // that edge and the block may carry on; IF going 0 to 1 resyncs, because the
                // boundary after it is exactly where the run loop would deliver. A CLI that
                // clears an already-clear IF resumes for the same reason.
                //
                // `0xfb` STI joined on 2026-08-22 (S4d). Design review M8 had removed it because
                // it takes the IF 0-to-1 edge AND arms `interrupt_shadow`, failing two clauses of
                // R3 on every execution. Both clauses are now scoped to the row rather than
                // relaxed globally, and the row pays for the relaxation with a pendency test the
                // other rows do not run: see `InterpretOneRow::arms_interrupt_shadow` and the
                // pendency note in `interpret_one_step`. Loader census: 486,000 block-stopping
                // hits.
                | 0xfa | 0xfb
                // CLD / STD. A POLICY lift and nothing else: `emit_direction_flag` is one `or` or
                // `and` on the flag shadow, DF sits outside the lazy arithmetic descriptor, and
                // neither interpreter arm consults `operand_size`, so the two widths are the same
                // operation and `DirectionFlag` carries no width to get wrong. The deferral this
                // replaces asked for a measurement; the loader census is it, at 736,877
                // block-stopping hits for the word row.
                | 0xfc
                | 0xfd
                | 0xfe
                | 0xff
        )
        // THE V86 LOOP-A ROWS (`IZARRAVM_V86_LOOP_ROWS`, default ON since 2026-08-20) are a
        // SECOND allowlist,
        // written as its own term rather than folded into the list above so that the gate-off arm
        // is byte-identical to the pre-slice tree by inspection rather than by reading a
        // hundred-line `matches!`.
        //
        // `0x05..=0x3d` step 8 are ALU form 5, the accumulator with a full-width immediate, and
        // `0x3d` CMP AX,imm16 is the census row (96,182,170 interpreted hits). The whole form is
        // admitted for the closure rule at the top of this file: all eight members reach ONE arm,
        // which produces `AluImm { dst: 0, width: operand_width }` -- a kind that has carried a
        // width field since the `0x81` slice and whose Word lane (`emit_alu`'s `mov ecx, imm32`
        // then a 66-prefixed `alu_r16_r16` with both operands masked, then `emit_write_gpr16`) has
        // been in production on `0x81`/`0x83` since then. `decode` fetches this immediate with
        // `fetch_immediate(operand_size)`, which at Word is a zero-extended `fetch_u16`, so the
        // emitter's `imm & 0xffff` is exact rather than a truncation. ADC (`0x15`) and SBB
        // (`0x1d`) are refused at Word by the forms-1|3|5 guard below, for the reason forms 1 and
        // 3 already refuse them: no carry-in lane.
        //
        // `0xa1` / `0xa3` are the moffs word forms, and they are the pair the long note above
        // names as the COUNTEREXAMPLE that must not be swept in by proximity. That note was right
        // about the hazard and is now the statement of the fix: both arms carried a hard-coded
        // `MemoryWidth::Dword`, and both now carry `operand_width`, exactly as MOVZX/MOVSX grew
        // `dst_width` rather than being kept off the list. `0xa0` / `0xa2` keep their literal
        // `MemoryWidth::Byte`, because THEIR width is a property of the form.
        //
        // `0x07` / `0x1f` POP ES / POP DS and `0xf8` / `0xf9` CLC / STC have arms of their own
        // below, both gated there as well; the entries here are what lets a 16-bit segment reach
        // them at all.
        //
        // `0xfc` / `0xfd` CLD / STD were deliberately NOT here, even though the same census
        // measures a `0xfc` word row at 1,642,514 hits: they are a different arm from CLC/STC (a
        // DF write with no lazy-descriptor interaction), so admitting them under this knob would
        // have put a second mechanism behind it and made that slice's A/B unattributable. The
        // follow-on it asked for is the S1 width lift, and the pair sits on the UNGATED list
        // above rather than here, which is what keeps the two attributable apart.
        && !(v86_loop_rows_enabled()
            && matches!(
                insn.opcode,
                0x05 | 0x0d | 0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d
                    | 0x07
                    | 0x1f
                    | 0xa1
                    | 0xa3
                    | 0xf8
                    | 0xf9
            ))
        // THE WORD TEST ROW (`IZARRAVM_TEST_WORD_ROWS`, default OFF) is a THIRD allowlist, written
        // as its own term for the reason the V86 term above is: the gate-off arm stays byte-
        // identical to the pre-slice tree by inspection rather than by reading a hundred-line
        // `matches!`.
        //
        // `0x85` is named in this file's header as a Dword sibling whose kind hard-codes Dword,
        // and that WAS the whole reason it was refused at Word. `DirectKind::Test` carries a
        // `width` as of this slice, exactly as `0xC7`, `0x81`, `0xB8..=0xBF` and `0xA1`/`0xA3`
        // each grew one before it, so the header entry is discharged rather than excepted -- see
        // the arm below and `test_word_rows_enabled` for the census that ranked it (duke3d-586,
        // 53,583,389 runtime hits, 42.2% of the whole rejected table) and for the suffix
        // measurement that prices its extension exposure at ONE instruction.
        //
        // ONLY `0x85`, and only the REGISTER form, which the arm enforces. `0xA9` (TEST eAX, imm)
        // is the other Word-shaped member of the family and its emitter is already fully
        // width-parameterised, so it is one entry away -- it stays out because the census measures
        // ZERO `0xA9` rows at any width, and an unmeasured admission is the campaign's standing
        // refusal rather than a free ride. `0x84`/`0xA8` are byte forms and were never at issue.
        && !(test_word_rows_enabled() && insn.opcode == 0x85)
        // Group 3, admitted by SUB-OPCODE rather than by adding `0xf7` to the list above, because
        // the group's members do not get the same answer and the list cannot say so.
        //
        // `/0` (TEST r/m16, imm16) is the width-safe one: `emit_test_preloaded` has carried a full
        // Word lane since the width field landed. The wolf3d demo-workload census ranks the
        // register form at 634M block-stopping hits.
        //
        // `/2../7` (NOT, NEG, MUL, IMUL, DIV, IDIV) are the S3 policy widening's fourth row and
        // are admitted as `InterpretOne` CALL-OUTS, never as lowerings. Every one of those arms
        // deliberately carries no width field and names this gate as the only thing keeping a word
        // form out of it; the classifier arm below therefore intercepts the Word forms and routes
        // them to the helper BEFORE any of them is reached, so the comments they carry stay true.
        // The post-S2 loader census ranks /6 DIV r16 at 729 k block-stopping hits, /4 MUL r16 at
        // 482 k and /0 word memory TEST at 242 k.
        //
        // `/1` is not a group-3 operation at all -- the interpreter answers it with
        // `undefined_opcode` -- so it stays refused here rather than being compiled into a block
        // that can only ever fault.
        && !(insn.opcode == 0xf7 && insn.modrm.is_some_and(|m| m.reg != 1))
    {
        return None;
    }
    // SETcc r/m8, BOTH operand forms. Byte-wide whatever the operand-size prefix says (the
    // interpreter's arm calls `write_operand_u8` without consulting `operand_size`), but 0x0f9x
    // is NOT in the Word-size allowlist above, so a 66-prefixed encoding never reaches here and
    // the point is moot rather than relied on. Keyed on the full u16 for the reason 0x0faf below
    // documents: the `u8::try_from` truncation further down cannot see a two-byte opcode.
    //
    // That Word gate is ALSO the memory form's width bar, and it is a bar on the FORM rather than
    // on the prefix: an unprefixed `0F 94 /0 mem` in a CS.D = 0 segment decodes at
    // `OperandSize::Word` with prefix mask 0 and is refused by the same line that refuses the
    // 66-prefixed one in a 32-bit segment. `setcc_word_segment_memory_form_stays_a_barrier` pins
    // both directions.
    //
    // The memory form is the tombraid FMV census's `0x0F94 /0 memory dword` row at 27,602,402
    // interpreted hits, one per iteration of the 32-bit FPU loop, and it sits behind
    // `IZARRAVM_FPU_LOOP_ROWS`. All sixteen conditions ride the gate together, for the closure
    // rule stated at the top of this file: the condition is a raw four-bit code handed to
    // `Encoder::setcc` exactly as the register form already hands it, so refusing fifteen
    // siblings of one measured row would be arbitrary rather than conservative.
    if let 0x0f90..=0x0f9f = insn.opcode {
        let condition = (insn.opcode & 0x0f) as u8;
        let DecodedOperand::Reg(dst) = insn.operand? else {
            let DecodedOperand::Mem(addr) = insn.operand? else {
                return None;
            };
            if !fpu_loop_rows_enabled() {
                return None;
            }
            return Some(DirectKind::SetCcMem {
                condition,
                addr: direct_addr(addr)?,
            });
        };
        return Some(DirectKind::SetCc { condition, dst });
    }
    // IMUL r32, r/m32, both operand forms. Must stay below the Word-size gate above: a
    // 66-prefixed IMUL decodes with OperandSize::Word and is not in that gate's allowlist, so it
    // already falls through to `None` there. Moving this arm above the gate, or adding 0x0faf to
    // the allowlist, would silently lower a 16-bit IMUL as a 32-bit multiply instead: the
    // destination's high 16 bits would be clobbered rather than preserved, and CF/OF would be
    // computed against the wrong width.
    if insn.opcode == 0x0faf {
        // Keyed on the full u16 opcode rather than the u8 truncation further down: that
        // truncation (`u8::try_from(insn.opcode).ok()`) returns None for every two-byte opcode,
        // so the u8 arms below are unreachable for 0x0faf regardless. Matching the full u16 here
        // keeps that explicit and local instead of relying on the truncation's behavior.
        //
        // Both forms share ONE arm so the gate placement above cannot come to apply to one and
        // not the other. The `?` on `direct_addr` returns None from `classify`, not from the
        // match, which is what every other memory arm in this file does for an unsupported
        // address size or scale.
        let m = insn.modrm?;
        return match insn.operand? {
            DecodedOperand::Reg(src) => Some(DirectKind::Imul { dst: m.reg, src }),
            DecodedOperand::Mem(addr) => Some(DirectKind::ImulMem {
                dst: m.reg,
                addr: direct_addr(addr)?,
            }),
        };
    }
    // The BIT-STRING family, everything the native `Bt` arm below does not take, as `InterpretOne`
    // call-outs. The S3 policy widening's third row: the post-S2 loader census ranks `0F BA /4`
    // dword register at 971 k block-stopping hits (5.5%) and `0F A3` memory dword at 257 k.
    //
    // ROUTED HERE, ABOVE the `u8::try_from(insn.opcode).ok()` truncation further down, for the
    // reason 0x0faf and MOVZX/MOVSX state: that truncation returns None for every two-byte opcode,
    // so an arm among the u8 arms below would be silently unreachable. Nothing would fail; the
    // admission would simply never fire.
    //
    // What this arm takes, and what it deliberately leaves to the native one immediately below:
    //
    // | form | answer |
    // |---|---|
    // | `0F A3` register, Dword | falls through to the native `Bt` lowering, which is free |
    // | `0F A3` register, Word | call-out: at Word the interpreter masks the index with `& 15`, not `& 31`, and `DirectKind::Bt` carries no width |
    // | `0F A3` memory, any width | call-out: the effective address is adjusted by the bit index at runtime, which a static `DirectAddr` cannot express |
    // | `0F AB`, `0F B3`, `0F BB`, any form | call-out: BTS/BTR/BTC WRITE the operand back and `Bt` does not |
    // | `0F BA /4../7`, any form | call-out: the index is an immediate rather than a register, and /5../7 write back |
    // | `0F BA /0../3` | refused: not defined bit-test ops, the interpreter #UDs them before the operation runs |
    //
    // The register forms of BTS/BTR/BTC ride the arm with their memory forms, and that is the
    // closure rule at the top of this file rather than a widening for its own sake: `0F BA /5`
    // register is a call-out because the whole `0F BA` group is one interpreter arm, so refusing
    // `0F AB` register -- the same operation with the index in a register instead of an immediate,
    // through the same `bit_string_op` -- would be arbitrary. They reach one helper because the
    // helper runs the decode line.
    //
    // The whole family joins the `OperandSize::Word` allowlist above with this slice. That is what
    // lets a Word form reach here at all, and it is safe only because of the first two table rows:
    // the native `Bt` lowering is now guarded on Dword explicitly rather than by the allowlist's
    // absence, so the `& 15` masking difference cannot reach an emitter that assumes `& 31`.
    if matches!(insn.opcode, 0x0fa3 | 0x0fab | 0x0fb3 | 0x0fbb | 0x0fba) {
        let m = insn.modrm?;
        if insn.opcode == 0x0fba && m.reg < 4 {
            return None;
        }
        let native_bt = insn.opcode == 0x0fa3
            && insn.operand_size == OperandSize::Dword
            && matches!(insn.operand?, DecodedOperand::Reg(_));
        if !native_bt {
            return Some(DirectKind::CallOut {
                helper: CallOutHelper::InterpretOne {
                    row: InterpretOneRow::BitString,
                },
            });
        }
    }
    // BT r/m32, r32, REGISTER form only. Keyed on the full u16 opcode and placed ABOVE the
    // `u8::try_from(insn.opcode).ok()` truncation for the same reason 0x0faf and the MOVZX/MOVSX
    // family are: that truncation returns None for every two-byte opcode, so an arm among the u8
    // arms below would be unreachable and nothing would fail, the lowering would simply never
    // fire.
    //
    // Reached only for the Dword register form, which the arm above filters to. It used to be
    // reached for the Word forms as well and be kept out of them by the `OperandSize::Word` gate's
    // silence; that gate now admits the family, so the width test moved into the arm above where
    // it is stated. At Word the interpreter masks the bit index with `& 15`, not `& 31`
    // (`bits = operand_size.bytes() * 8`), and this kind carries no width.
    //
    // Only 0xa3 of the four-opcode family, and only the register form. BTS/BTR/BTC (0xab, 0xb3,
    // 0xbb) WRITE the operand back; this arm's kind does not, and the interpreter skips the
    // write-back for op 0 alone. The memory form adjusts its effective address by the bit index
    // at runtime, which a static DirectAddr cannot express.
    if insn.opcode == 0x0fa3 {
        let m = insn.modrm?;
        return match insn.operand? {
            DecodedOperand::Reg(rm) => Some(DirectKind::Bt { rm, index: m.reg }),
            DecodedOperand::Mem(_) => None,
        };
    }
    // MOVZX and MOVSX, memory form only. Keyed on the full u16 opcode and placed ABOVE the
    // `u8::try_from(insn.opcode).ok()` truncation further down, for the same reason 0x0faf is:
    // that truncation returns None for every two-byte opcode, so an arm added among the u8 arms
    // below (next to the 0x8a/0x8b MOV forms it most resembles) would be UNREACHABLE. Nothing
    // would fail; the lowering would simply never fire, and only the pre-flight counter would
    // notice. It also has to stay BELOW the OperandSize::Word gate above, but no longer because
    // that gate refuses it: all four ARE now in the allowlist, and what makes that safe is the
    // `dst_width` field below. An earlier version of this comment said the gate must refuse them
    // "because `write_gpr_sized` at Word merges into the low 16 bits instead of replacing all
    // 32". That statement of the hazard was exactly right and is now the statement of the fix:
    // the merge is expressed rather than avoided.
    //
    // `width` is the SOURCE width and comes from the sub-opcode. `dst_width` is the DESTINATION
    // width and comes from `operand_width`, i.e. from CS.D and the 0x66 prefix. They are
    // independent -- `66 0F B6` is a byte source into a word destination -- and confusing them in
    // either direction is a miscompile: `width` from `operand_width` turns every byte capture
    // into a dword read; `dst_width` hard-coded to Dword clobbers the destination's high half on
    // the 66-prefixed forms. The doom census ranks `0x0FB6` memory word at 1,442,795 exits and
    // quake carries the same opcode at 31,216, which is what this pair of fields buys.
    if matches!(insn.opcode, 0x0fb6 | 0x0fb7 | 0x0fbe | 0x0fbf) {
        let m = insn.modrm?;
        let width = if matches!(insn.opcode, 0x0fb6 | 0x0fbe) {
            MemoryWidth::Byte
        } else {
            MemoryWidth::Word
        };
        let signed = matches!(insn.opcode, 0x0fbe | 0x0fbf);
        // Both operand forms share ONE arm so the gate placement above cannot come to apply to
        // one and not the other. For the register form `src` is the raw ModRM rm field, which at
        // Byte width is a byte-register index where 4..=7 are AH/CH/DH/BH; the emitter reuses the
        // interpreter's own lane arithmetic rather than repeating it.
        let DecodedOperand::Mem(addr) = insn.operand? else {
            let DecodedOperand::Reg(src) = insn.operand? else {
                return None;
            };
            return Some(DirectKind::MovExtendReg {
                dst: m.reg,
                src,
                width,
                dst_width: operand_width,
                signed,
            });
        };
        return Some(DirectKind::LoadExtend {
            dst: m.reg,
            width,
            dst_width: operand_width,
            signed,
            addr: direct_addr(addr)?,
            // Every one of the four interpreter arms returns clocks(3) (execute.rs). The
            // DirectKind::raw_clocks default arm returns 2, which would undercharge each of these
            // by one clock and break byte identity on executed_cpu_core_clocks without failing any
            // unit test, so this is carried as a field the way Load and Store carry theirs.
            raw_clocks: 3,
        });
    }
    if matches!(insn.opcode, 0x0fa4 | 0x0fa5 | 0x0fac | 0x0fad) {
        let m = insn.modrm?;
        let count = if matches!(insn.opcode, 0x0fa4 | 0x0fac) {
            ShiftCount::Immediate(insn.imm as u8)
        } else {
            ShiftCount::Cl
        };
        let left = matches!(insn.opcode, 0x0fa4 | 0x0fa5);
        return match insn.operand? {
            DecodedOperand::Reg(dst) => Some(DirectKind::DoubleShiftReg {
                left,
                dst,
                src: m.reg,
                count,
            }),
            DecodedOperand::Mem(addr) => Some(DirectKind::DoubleShiftMem {
                left,
                src: m.reg,
                count,
                addr: direct_addr(addr)?,
            }),
        };
    }
    let opcode = u8::try_from(insn.opcode).ok();
    if let Some(opcode) = opcode {
        if opcode < 0x40 {
            let op = (opcode >> 3) & 7;
            let form = opcode & 7;
            // ADC and SBB have no Word lane, so refuse them here rather than leaving the
            // allowlist as the only thing between them and a miscompile. They take the incoming
            // CF as an OPERAND; `emit_alu_preloaded`'s Word lane masks both operands with `and`,
            // which CLEARS host CF, then tags the descriptor as the SUB class. An admitted
            // `66 11 /r` would compute `adc ax, bx` without the carry in and then evaluate its
            // lazy CF as `a < b`.
            //
            // The reason this guard exists at all is that the rule stated above says the byte set
            // is closed over its shared classifier arms, and this slice admits five of eight
            // members of two shared arms. Without a guard here the next reader applying that rule
            // lands the bug above in one line. `0x81 | 0x83` states the same refusal the same way.
            //
            // Forms 1, 3 and 5 ONLY. The other forms in this group are byte-width by encoding and
            // reach lanes that carry no `operand_width` at all, so their ADC and SBB members are
            // correct at Word and are admitted: `0x10`/`0x18`/`0x12`/`0x1a` produce `AluRegByte`
            // and `0x14`/`0x1c` produce `AluByteImm`. Widening this guard to the whole group
            // refuses those six as collateral, which is a regression rather than a fix.
            //
            // Form 5 (the accumulator with a full-width immediate) joined the guard with the
            // V86 loop-A slice, which is what first lets a form-5 opcode reach here at Word. It
            // produces `AluImm { width: operand_width }` and routes through the same
            // `emit_alu_preloaded` Word lane forms 1 and 3 do, so `0x15` ADC AX,imm16 and `0x1d`
            // SBB AX,imm16 have exactly the failure this guard already describes. The guard is
            // UNGATED on purpose: it can only fire on a form the gate admits, and stating the
            // refusal unconditionally is what stops the next reader re-deriving it.
            if insn.operand_size == OperandSize::Word
                && matches!(form, 1 | 3 | 5)
                && matches!(op, 2 | 3)
            {
                return None;
            }
            match form {
                // Byte r/m destination, byte register source. BOTH operand forms as of the
                // rejected-row campaign's Slice 7: the register form is `AluRegByte`, the byte
                // lane that "does not exist yet" once did not.
                //
                // Operand roles follow `execute_alu_decoded`'s form-0 arm exactly: `a` is the
                // r/m (the destination, written back unless op is CMP) and `b` is `modrm.reg`.
                // That is the OPPOSITE assignment from form 2 below, and getting it backwards is
                // silent for the commutative ops and wrong for SUB/SBB/CMP — which is why the two
                // arms name `dst` and `src` explicitly rather than sharing a helper.
                //
                // Width is a property of the form, not of the prefix — the interpreter's arm
                // reads `read_operand_u8`/`read_gpr8` and charges the same `clocks(2)` as every
                // other ALU form without consulting `operand_size` — so `MemoryWidth::Byte` is a
                // literal here rather than `operand_width`. 0x38 is deliberately NOT in the
                // Word-size allowlist above, so a 66-prefixed encoding never reaches this arm.
                0 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::AluRegByte {
                            op,
                            dst,
                            src: m.reg,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                            op,
                            source: StoreSource::Reg(m.reg),
                            width: MemoryWidth::Byte,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                1 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::AluReg {
                            op,
                            dst,
                            src: m.reg,
                            width: operand_width,
                        }),
                        // Word memory is refused for the WRITING ops and admitted for CMP, which
                        // is the shape that already ships: `0x39` has been compiling word memory
                        // in quake's renderer since before this slice, so `op != 7` here rather
                        // than a blanket refusal, or that regresses.
                        //
                        // A missed lowering rather than a hazard, and the reason is economics.
                        // 16-bit DOS code has no alignment discipline, and an `AluMemDest` slot
                        // lowers through `emit_alu_mem_dest` -- one of the ELEVEN memory sites
                        // that still refuse a misaligned access outright. Guard 3 relaxed only the
                        // two lean one-lookup sites (the plain load and the plain store), so
                        // naming the guard here would now be wrong: the refusal is the SITE's, not
                        // the guard's. Admitted today, an odd operand would sit INSIDE the block
                        // and side-exit at that slot on every execution, so nothing after it
                        // retires natively.
                        //
                        // This is the hook for the read-modify-write follow-on. An RMW slot needs
                        // a read deposit AND a write deposit inside one slot -- guard 3's stubs
                        // each carry one -- which is what has to be built before this arm can be
                        // opened, not a census row.
                        DecodedOperand::Mem(_)
                            if insn.operand_size == OperandSize::Word && op != 7 =>
                        {
                            None
                        }
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                            op,
                            source: StoreSource::Reg(m.reg),
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                // Byte register destination, byte r/m source — the form that had NO arm at all
                // before Slice 7, in either operand shape. Register only.
                //
                // Roles are form 0's mirrored, and they follow `execute_alu_decoded`'s form-2 arm:
                // `a` is `modrm.reg` (the destination, written back through `write_gpr8` unless op
                // is CMP) and `b` is the r/m.
                //
                // The MEMORY form is deliberately absent and is a missed lowering rather than a
                // hazard — the `else` returns None and the instruction stays the barrier it is
                // today. `AluMemSource` looks as if it already covers it (its read match has a
                // `MemoryWidth::Byte` arm) but that arm is UNREACHABLE and incomplete: it falls
                // into `mov eax, home(dst)` and `emit_alu_preloaded`, which has no byte lane at
                // all and would read a 32-bit register where a byte lane is meant, and
                // `DirectKind::byte_reads` does not count `AluMemSource` either, so the bus
                // accounting would be short a byte read. Building it is a second mechanism behind
                // one census measurement (quake 21,686 exits on `0x32 /0`, doom zero); this arm
                // is the register lane and nothing else.
                2 => {
                    let m = insn.modrm?;
                    let DecodedOperand::Reg(src) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::AluRegByte {
                        op,
                        dst: m.reg,
                        src,
                    });
                }
                3 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(src) => Some(DirectKind::AluReg {
                            op,
                            dst: m.reg,
                            src,
                            width: operand_width,
                        }),
                        // Word memory is ADMITTED here, unlike form 1 above, and the difference
                        // between the two arms is the SITE rather than the opcode. `AluMemSource`
                        // only READS guest memory, through `emit_ram_read_pointer`, which
                        // dispatches to the RELAXED lean one-lookup read site whenever
                        // `one_lookup_load` is on (the default). So a misaligned page-local operand
                        // is served natively with the split bus charge instead of side-exiting at
                        // that slot on every execution — which is exactly the economics that keep
                        // form 1's read-modify-write shape refused, since site 6 carries the
                        // unrelaxed guard and would need a read deposit AND a write deposit inside
                        // one slot. `a_misaligned_word_alu_memory_source_runs_natively` in
                        // `cpu_jit_misaligned_memory_test.rs` pins that behavior as an exact split
                        // delta rather than leaving it to this comment.
                        //
                        // What this admission newly exercises is the COMBINATION of Word with a
                        // register write-back after a memory read: `AluMemSource` at Word used to
                        // reach only CMP, so the read lane (`movzx_r32_word_disp8`) and the
                        // write-back lane (`emit_alu_preloaded`'s `mov_r16_r16`) were certified
                        // separately and the pair was not. It is now, by
                        // `the_word_memory_source_alu_matches_the_interpreter_for_every_admitted_op`
                        // in `cpu_jit_word_memory_test.rs`, which runs all five writing ops plus
                        // `0x3b` CMP as the non-writing control against a block-free interpreter
                        // with `0xdead` in every destination's high half.
                        //
                        // ADC and SBB are still refused, by the forms-1|3 guard above and by a
                        // release assert in `emit_alu_preloaded`; nothing here reaches them.
                        //
                        // The census asked for this one. `IZARRAVM_DIRECT_BARRIER_CENSUS=1` on
                        // peachdrm-586 ranks `0x2B` SUB r16,r/m16 word memory at 655,103,963 of
                        // 661,739,172 barrier runtime_hits — 99.0%, a single shape. The whole
                        // non-carry set lands together because it is one arm and one emitter path,
                        // and because the relocation trap predicts `0x03` would simply inherit
                        // `0x2B`'s exits otherwise.
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemSource {
                            op,
                            dst: m.reg,
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                // ALU accumulator forms with an imm8: ADD/OR/ADC/SBB/AND/SUB/XOR/CMP AL, imm8
                // (0x04/0x0c/0x14/0x1c/0x24/0x2c/0x34/0x3c). Semantically this is the 0x80 group
                // with the destination fixed to AL, so it reuses `AluByteImm` and
                // `emit_alu_byte_imm` unchanged; `dst: 0` is AL exactly as the interpreter's
                // `read_gpr8(0)`/`write_gpr8(0)` pair means it, and op 7 CMP suppresses the
                // writeback inside the emitter.
                //
                // This arm stays inside this `match form`, BELOW the OperandSize::Word gate near
                // the top of `classify`, and its placement is still load-bearing. What CHANGED:
                // the whole `0x04..=0x3c` family is now IN that gate's allowlist, so a Word-size
                // `3C ib` reaches this arm and is lowered as a byte op.
                //
                // That is correct, and it was checked against the interpreter rather than against
                // the architecture. `decode` fetches this immediate with an unconditional
                // `fetch_u8` for `form == 4` (only `form == 5` consults `operand_size`), and
                // `execute`'s matching arm uses `read_gpr8(0)`, `BusWidth::Byte`, `write_gpr8(0)`
                // and `clocks(2)` without ever reading `operand_size`. So a 66-prefixed `3C ib`
                // in 32-bit code and an unprefixed one at CS.D = 0 are the same operation on the
                // same lane for the same clocks. An earlier version of this comment warned that
                // admitting it "would lower a 16-bit-prefixed form as a byte op": that is true,
                // and it is what the interpreter does.
                //
                // It must not consult `operand_width`: byte width is a property of the form, not
                // of the prefix. It must not touch `insn.modrm` or `insn.operand` either, which
                // are None here because `decode` only parses a ModRM for forms below 4.
                4 => {
                    return Some(DirectKind::AluByteImm {
                        op,
                        dst: 0,
                        imm: insn.imm as u8,
                        // The accumulator short form's immediate sits at offset ONE, not two, so
                        // it is out of `imm8_lane_for`'s admitted shape by its opcode test. Baked,
                        // as it always was.
                        lane: None,
                    });
                }
                // ALU accumulator forms with a full-width immediate (0x05/0x0d/.../0x3d). On the
                // GATED half of the Word allowlist since the V86 loop-A slice, so `operand_width`
                // is Word here whenever `IZARRAVM_V86_LOOP_ROWS` is on and the segment is 16-bit,
                // and Dword otherwise. It has always been passed rather than hard-coded, which is
                // what let the admission be an allowlist entry rather than an emitter change; the
                // carry members are refused at Word by the forms-1|3|5 guard above.
                5 => {
                    return Some(DirectKind::AluImm {
                        op,
                        dst: 0,
                        imm: insn.imm,
                        lane: None,
                        width: operand_width,
                    });
                }
                _ => {}
            }
        }
        match opcode {
            0x40..=0x4f => {
                return Some(DirectKind::IncDecReg {
                    dst: opcode & 7,
                    is_dec: opcode >= 0x48,
                    width: operand_width,
                });
            }
            // PUSHAD / POPAD, the MEMORY class of interpreter call-out slot. Ranked one and three
            // in the post-Phase-5 census (9,776,336 and 7,005,733 doom dispatcher exits) and
            // coupled to the `0x83 /5` word row below: the three are one function prologue and its
            // epilogue, so lowering any one alone only relocates its exits onto the next
            // instruction.
            //
            // A CALL-OUT rather than a lowering, and for a SIZE reason rather than a reachability
            // one -- emitted code can reach guest memory perfectly well; eight guarded accesses per
            // instruction is what does not fit a one-host-page block. `jit/direct/callout.rs`
            // carries the class table, the two-phase resident-then-move design, and a note on the
            // single-wide-guard shape that would beat this if the family is ever worth more wall.
            //
            // Dword only. Neither opcode is in the OperandSize::Word allowlist above, so PUSHA and
            // POPA (the 16-bit forms, which move sixteen bytes rather than thirty-two) fall to
            // `None` and stay barriers. The helper additionally refuses a 16-bit STACK (SS.B = 0)
            // at run time, which is an orthogonal axis the allowlist cannot see.
            //
            // No ModRM, no `insn.operand`, no operand of any kind: both are register-file-implicit.
            0x60 => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::PushAllDword,
                });
            }
            0x61 => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::PopAllDword,
                });
            }
            0x50..=0x57 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Reg(opcode - 0x50),
                });
            }
            0x58..=0x5f => {
                return Some(DirectKind::Pop { dst: opcode - 0x58 });
            }
            // LEAVE is classified at the 0xc9 arm below and its (operand size x SS.B) cell is
            // chosen by the stack-width matrix, not here. Three of the four cells are built as of
            // the S1 width lift; the fourth, a Dword operand on a 16-bit stack, would move four
            // bytes with a 16-bit pointer and stays refused.
            // NOP. In the Word allowlist since 2026-08-08: the claim that "no 16-bit block exists
            // on any persona" predated the JIT16 flip (wolf3d runs billions of 16-bit entries),
            // and the wolf3d demo-workload census measured `0x90` word at 79M block-stopping
            // hits. `Nop` emits nothing width-dependent.
            0x90 => {
                return Some(DirectKind::Nop);
            }
            // XCHG, the whole family, as `InterpretOne` call-outs. The S3 policy widening's second
            // row: `0x87` register form is the post-S2 loader census's second row at 1.21 M
            // block-stopping hits (6.9%) and `0x93`/`0x97` add 507 k.
            //
            // A call-out and not a lowering, and the reason is the SHAPE rather than the census.
            // Every one of these is a CROSS-WRITE: the interpreter reads both operands, then
            // writes both back (`execute.rs` 0x86/0x87/0x91..=0x97). A lowered memory form is a
            // guarded read and a guarded store to the SAME address with the register exchange in
            // between, which is two address computations, two fast-map probes, two permission
            // checks and two side-exit stub sets for an instruction that appears once per Watcom
            // pointer swap. The helper is a fixed call whatever the form.
            //
            // ALL FOUR FORMS ride one arm. `0x86` is the byte width and `0x87` the operand width;
            // `0x91..=0x97` take the register from the low three opcode bits and exchange it with
            // the accumulator. They reach ONE helper because the helper runs the decode line, so
            // the closure rule at the top of this file applies at its strongest here: there is no
            // per-form lowering that could be right for one and wrong for another.
            //
            // `0x90` is NOT in this arm and must not be. It is XCHG (E)AX,(E)AX architecturally,
            // but it has a native `Nop` lowering that emits nothing at all, which is strictly
            // better than a call-out; the arm above keeps it.
            //
            // `0x94` XCHG (E)AX,(E)SP writes the stack pointer, and that is sound for the reason
            // the module docs derive for POPAD: `emit_store_homes` and the unconditional reload
            // cover all eight GPRs, and later slots address the stack through `home(4)`, which the
            // reload has just refreshed. Nothing bakes an ESP value.
            //
            // LOCK is refused upstream by `prefixes_supported_for`, which matters here more than
            // for most rows: `XCHG` with a memory operand is implicitly locked on real silicon and
            // the explicit prefix is common. A LOCK'd form never reaches this arm.
            0x86 | 0x87 | 0x91..=0x97 => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::InterpretOne {
                        row: InterpretOneRow::Xchg,
                    },
                });
            }
            // CBW / CWDE. Unlike NOP and CLD/STD immediately below, this one IS in the
            // Word-size allowlist above: the interpreter's arm switches on `operand_size` (the
            // Word case is CBW, Dword is CWDE), so a 16-bit block containing it is a real
            // lowering rather than dead code no counter could gate. Accumulator-implicit: no
            // ModRM, no `insn.operand`.
            0x98 => {
                return Some(DirectKind::Cwde {
                    width: operand_width,
                });
            }
            // IN AL, DX -- the FIRST and, this phase, the ONLY interpreter call-out slot. Not a
            // native lowering: the block spills, routes this one instruction through the
            // interpreter's port path (which needs the bus, and the bus is not reachable from
            // emitted code), reloads, and keeps running. See `jit/direct/callout.rs` for the
            // helper contract and the abnormal-set enumeration.
            //
            // Ranked here by the Phase 3 class table: exits to blocks rejected at this opcode's
            // barrier are the single largest identified share of doom's unbound static exit pool.
            //
            // Byte-wide and accumulator-implicit: no ModRM, no `insn.operand`, the port comes
            // from the live DX. The interpreter's 0xec arm does NOT consult `operand_size` (it
            // reads DX and writes AL at `BusWidth::Byte` unconditionally), so the form is
            // operand-size-invariant, and 0xec is now ON the Word-size allowlist above.
            //
            // THE 2026-08-11 REFUTATION BELOW IS SUPERSEDED, and is kept because its measurement
            // is still the reason the admission cannot travel alone. What it refuted was the ONE
            // LINE version -- the allowlist entry with the helper's blanket V86 refusal left in
            // place. That version is still provably negative, and re-deriving it from the census
            // is still the mistake the note was written to stop.
            //
            // What changed is the helper. `dev_docs/wolf-v86-port-callout-design.md` takes the
            // 08-11 numbers apart into THREE gates rather than the two the earlier reading saw,
            // and shows that the 136.8M call-outs which executed could only have arrived by
            // CHAINED entry (a chained transfer never returns to `run_direct_block`, so its
            // entry gate cannot have refused them) -- i.e. links into a call-out block already
            // bind and already fire at nine-figure volume on this exact fixture. The remaining
            // 100% abnormal rate was the helper's first statement, and `port_read_al_dx` now has
            // a two-phase arm that answers the TSS I/O-bitmap question purely (TLB hits only, an
            // uncharged RAM peek, refuse on any doubt) and only then charges. The entry gate in
            // `run.rs` is deliberately UNCHANGED: it refuses dispatcher entries as before, and
            // the chain is what gets in.
            //
            // So the two changes are ONE change and must stay one: the allowlist entry without
            // the helper arm buys a spill, a call, a reload and a side exit where a free barrier
            // used to be. Reverting either half alone re-creates exactly the measurement below.
            //
            // The superseded reading, verbatim:
            // `dev_docs/wolf3d-586-measurement-results.md` ranks this row at
            // 370,316,594 of 381,560,241 block-stopping hits (97.05%) on wolf3d-586 and recommends
            // exactly one line: `0xec` added to the list above. Built and run (2026-08-11,
            // A/B/B/A at ProcessorIndex 8, 12e9 clocks) it serves ZERO port reads natively, and
            // the reason is the one fact a barrier census cannot see -- THE GUEST IS IN V86 MODE.
            // wolf3d-586's CONFIG.SYS loads TOKAEMM, so every 16-bit block runs under the V86
            // monitor, and V86 is the FIRST thing `port_read_al_dx` refuses (module docs, abnormal
            // producer 1: the TSS I/O-bitmap probe page-walks, which is unsupportable from inside
            // a live block). With the admission in, the full run measures
            // `side_exit_callout_abnormal` 136,772,308 against `jit_direct_callout_executed`
            // 136,772,308 -- ONE HUNDRED PERCENT of the call-outs that ran returned abnormal --
            // plus 233,559,698 whole-block entries refused up front by `run_direct_block`'s
            // `callout_port_slots` privilege gate. So the admission buys a spill, a call, a reload
            // and a side exit where a free barrier used to be, and refuses 9.3% of this fixture's
            // block entries outright, in exchange for nothing at all.
            //
            // (The four wall legs were 289.706 / 294.872 / 304.269 / 312.868 s in A/B/B/A order --
            // a monotonic 8% HOST DRIFT that swamps the effect, so the drift-corrected wall delta
            // of −0.57% is noise and is NOT the evidence here. The evidence is that not one port
            // read was served: `perf.instructions` was byte-identical at 15,218,471,683 on all
            // four legs and the framebuffer invariant passed on all four, so the two arms differ
            // only in host cost, and the abnormal ratio says the admission has no upside to trade
            // that cost against.)
            //
            // The Dword form is NOT affected and is healthy: on the 32-bit protected-mode fixtures
            // the same helper reads `side_exit_callout_abnormal` at 1,080 of 9,868,635 (doom-486),
            // 1,085 of 26,612,249 (doom-586) and 135,164 of 1,836,356,449 (gp2-586). The problem
            // is V86, not the mechanism.
            //
            // The note's own item 1 -- "a V86-capable port call-out ... that is the slice that
            // unlocks 97% of this census; it is a real slice, not a list entry" -- is the slice
            // that shipped, with the residency probe read off the TLB and the bus rather than off
            // the FastMap. Item 2 (a compile-time V86 refusal of the slot) is now moot.
            // (End of the superseded reading.)
            //
            // The Approximate-class gate is INHERITED, not re-stated. `block_continuable`
            // (decode.rs) admits the IN forms only on I486/I586, so on the Accurate 386 class
            // `insn.continuable` is false and the compile walk stops at this instruction BEFORE
            // it ever reaches `classify`. That is what makes the 386 personas provably
            // byte-identical across this slice.
            0xec => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::PortReadAlDx,
                });
            }
            // POP r/m16/32 (group 1A), the first row on the generic `InterpretOne` allowlist and
            // the top of the Tomb Raider loader's barrier census after the S1 width lift at
            // 12.4% of block-stopping hits.
            //
            // It is a CALL-OUT and not a lowering, and the reason is the whole point of the S2
            // mechanism rather than a property of this opcode: POP r/m has no emitter, is not
            // worth one on its own, and used to end the block. Now it costs one helper call and
            // the block continues. What makes it the right FIRST row is that it STORES: the
            // deferred-code-write contract (design review B2) is exercised on day one instead of
            // being carried untested behind rows that only read.
            //
            // BOTH forms, register and memory, and every width. The register form is a two-line
            // interpreter arm and the memory form is the census row; refusing the cheap sibling of
            // an admitted row is the arbitrariness this file's header rules out, and both reach
            // the identical helper because the helper runs the decode line rather than a lowering.
            //
            // `reg != 0` is refused HERE rather than left to the helper. Those encodings are
            // illegal and the interpreter's arm answers them with `undefined_opcode`, which the
            // helper would deliver through its fault arm -- correct, but it would burn a call-out
            // and a governor execution on every one of them, and a block would be compiled around
            // an instruction that can only ever fault.
            //
            // Everything else the row needs is already gated above and inherited rather than
            // restated: `prefixes_supported_for` refuses REP, LOCK and the address-size override,
            // so the helper never meets a REP whose single step would be one iteration of many;
            // the Word allowlist decides whether a 16-bit form is admitted at all; and
            // `block_continuable` admits 0x8F through `DecodeGroup::Stack` on every persona, so
            // there is no class gate to inherit.
            0x8f if insn.modrm.is_some_and(|m| m.reg == 0) => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::InterpretOne {
                        row: InterpretOneRow::PopRm,
                    },
                });
            }
            // CWD / CDQ, same reasoning as 0x98 immediately above.
            0x99 => {
                return Some(DirectKind::Cdq {
                    width: operand_width,
                });
            }
            // CLD / STD. Ranked third in the runtime-weighted reject audit at 1.37M dispatcher
            // exits (10.9% of rejected-target exits) despite being worth only ~0.06pp of
            // instruction coverage -- coverage share and dispatch-exit share are different
            // quantities, and an earlier slice dismissed this opcode on the wrong one.
            //
            // ON the OperandSize::Word allowlist above since the S1 width lift. The old refusal
            // said the entry "would be dead code no counter could gate", which was true when no
            // 16-bit block existed on any persona and stopped being true at the JIT16 flip. The
            // Tomb Raider loader census measures the word row at 736,877 block-stopping hits.
            // Nothing in the emitter moved: `DirectionFlag` carries no width and neither
            // interpreter arm reads `operand_size`.
            0xfc | 0xfd => {
                return Some(DirectKind::DirectionFlag {
                    set: opcode == 0xfd,
                });
            }
            // CLI, an `InterpretOne` call-out. See the entry beside `0xfa` on the Word allowlist
            // above for why it resumes and why STI is not here with it.
            //
            // It is the only admitted row that touches no memory and no general register, so it is
            // also the cheapest possible proof that the mechanism's cost is the CALL rather than
            // the work: a demoted CLI slot and an admitted one differ by exactly one helper
            // invocation.
            0xfa => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::InterpretOne {
                        row: InterpretOneRow::Cli,
                    },
                });
            }
            // STI, the S4d row and CLI's mirror in encoding but not in consequence. CLI only
            // clears IF, which the run loop has no delivery point for; STI sets IF and arms the
            // one-instruction shadow, and a native block has no point at which that shadow is
            // consumed.
            //
            // What makes it admissible is that the shadow is decided at the BLOCK BOUNDARY instead
            // of inside the helper (design section 10.1, B1), and that the row refuses to resume
            // while an interrupt is pending (B2), which is what keeps the run's end point where
            // the interpreter would have put it. The owner accepted the residual caveat on
            // 2026-08-22: a pending interrupt is delivered at the next block boundary rather than
            // after exactly one shadowed instruction.
            //
            // V86 with IOPL < 3 needs nothing here: the interpreter's arm calls `check_v86_iopl`
            // and the helper's fault arm delivers the #GP exactly as it does for every other row.
            0xfb => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::InterpretOne {
                        row: InterpretOneRow::Sti,
                    },
                });
            }
            // CLC (0xf8) and STC (0xf9). Behind `IZARRAVM_V86_LOOP_ROWS`; `0xf8` is the tombraid
            // loop-A census's fourth row at 95,090,745 interpreted hits, and it is the loop's LAST
            // instruction before the `ret`, so nothing after it retires natively while it stays a
            // barrier.
            //
            // STC rides the arm with it by the closure rule at the top of this file: the emitter
            // is one `or`/`and` on the flag shadow selected by this bool, and refusing one
            // polarity of a two-polarity arm would be arbitrary. CMC (0xf5) is NOT here: it reads
            // the incoming CF and complements it, which is a different operation needing the
            // current value rather than a constant, and no census row measures it.
            //
            // Unlike CLD/STD above these ARE on the Word allowlist (under the gate), because the
            // measured row is a 16-bit one. Neither instruction consults `operand_size` in the
            // interpreter -- both are one `set_flag(FLAG_CF, ..)` -- so the two widths are the
            // same operation and the kind carries no width to get wrong.
            0xf8 | 0xf9 if v86_loop_rows_enabled() => {
                return Some(DirectKind::CarryFlag {
                    set: opcode == 0xf9,
                });
            }
            // LEAVE. One kind out of this arm whatever the operand size; `stack_width_kind`
            // splits it into `Leave` (Dword on a 32-bit stack), `Leave16 { stack32 }` (Word on
            // either) and a refusal (Dword on a 16-bit stack). Deciding it here is impossible:
            // SS.B is CPU state and `classify` has no CPU.
            0xc9 => {
                return Some(DirectKind::Leave);
            }
            // ENTER imm16, imm8. WORD operand size and nesting level 0 only.
            //
            // The level bar is the real one. `decode` masks `imm2` to five bits, and a level above
            // zero copies the enclosing display: a loop of `level - 1` stack reads and pushes plus
            // one more push, each with its own fault point and its own partial-commit rewind. That
            // is a different instruction from the one this kind emits, and it stays a hard
            // boundary. Watcom's prologue is `enter imm16, 0`, so the census row and the admitted
            // form are the same thing.
            //
            // The Dword operand form is refused because no emitter exists for it and no row asks:
            // it would push four bytes and take the frame pointer at the full width. `stack32` is
            // a placeholder here, exactly as `Push`'s `StoreSource::Flags { mask: u32::MAX }` is;
            // `stack_width_kind` resolves it and nothing between the two reads it.
            0xc8 => {
                if insn.operand_size != OperandSize::Word || insn.imm2 != 0 {
                    return None;
                }
                return Some(DirectKind::Enter16 {
                    alloc: insn.imm as u16,
                    stack32: false,
                });
            }
            // PUSHFD. Fifth in the runtime-weighted reject audit at 1,194,127 dispatcher exits
            // (9.5%). The persona mask and the V86 refusal are resolved in `stack_width_kind`,
            // which has the CPU; `u32::MAX` is the placeholder until then and must never reach
            // the emitter.
            0x9c => {
                return Some(DirectKind::Push {
                    source: StoreSource::Flags { mask: u32::MAX },
                });
            }
            // SAHF. Behind `IZARRAVM_FPU_LOOP_ROWS` (default off); the tombraid FMV census ranks
            // the dword row at 55,203,044 interpreted hits, two per iteration of the 32-bit FPU
            // loop, second only to WAIT.
            //
            // Deliberately NOT added to the `OperandSize::Word` allowlist above, and the reason is
            // the NOP/CLD one rather than a hazard: SAHF's interpreter arm never consults
            // `operand_size` (it reads AH and rewrites five EFLAGS bits, both width-invariant), so
            // a Word admission would be CORRECT and simply unmeasured -- no census row, 16-bit or
            // otherwise, and this campaign does not ship admissions with no counter to gate them.
            // `sahf_word_segment_form_stays_a_barrier` pins that the unprefixed form in a
            // CS.D = 0 segment is refused rather than lowered or panicking, which is the shape the
            // count-lane slice got wrong in the other direction by barring on the PREFIX.
            //
            // LAHF (0x9f) is the sibling and stays out: it writes AH from the flag byte, needs its
            // own emitter, and the census measures no row for it at all.
            0x9e if fpu_loop_rows_enabled() => {
                return Some(DirectKind::Sahf);
            }
            // PUSH DS (0x1e), PUSH ES (0x06), PUSH CS (0x0e) and PUSH SS (0x16), the read half
            // of the segment family. All four are ordinary word stack stores of a value the block already pins: the selector is baked
            // from the `SegmentLayout` in `emit_store`, exactly as `MovSegToReg` bakes one, and
            // `selector_segment` reports the segment so `data_matches` refuses re-entry after a
            // guest reload.
            //
            // PUSH CS (0x0e) joined on 2026-08-08 when the wolf3d demo-workload census
            // ranked it at 158M block-stopping hits: it needs NO `selector_segment` entry because
            // CS is not in `SegmentLayout.data` at all — `SegmentLayout::selector` reads the
            // separate `cs` field and `cs_matches` pins it for every block unconditionally, the
            // same argument that already carries `mov r16, cs` through `MovSegToReg`.
            //
            // PUSH SS joined on 2026-08-22, and what kept it out until then was a misreading this
            // arm carried in prose: "it belongs to the family the write half excludes over the
            // interrupt shadow". That argument is about POP SS and MOV SS, which LOAD the stack
            // segment and arm a one-instruction shadow (`load_segment_arming_ss_shadow`). PUSH SS
            // only READS the selector and arms nothing: the interpreter's 0x16 arm is the 0x06 arm
            // with a different `SegmentIndex` and the same two clocks. SS takes the ORDINARY data
            // path here, unlike CS. It lives in `SegmentLayout.data`, so `selector_segment` must
            // report it, and every push already has it in `used` because `write_segment` names SS
            // as the segment the store goes through. The tombraid DOS/4GW loader census of
            // 2026-08-22 ranks it at 747,415 block-stopping hits, the largest remaining barrier
            // row after S3.
            0x0e | 0x16 | 0x1e | 0x06 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Selector(match opcode {
                        0x0e => SegmentIndex::Cs,
                        0x16 => SegmentIndex::Ss,
                        0x1e => SegmentIndex::Ds,
                        _ => SegmentIndex::Es,
                    }),
                });
            }
            // POP ES (0x07) and POP DS (0x1f), the read half's mirror: a word off the stack loaded
            // into a segment register. Behind `IZARRAVM_V86_LOOP_ROWS`; `0x07` is the tombraid
            // loop-A census's largest row at 97,347,816 interpreted hits and 95,057,524 static
            // unbound exits, and `0x1f` rides the same arm with 6,016,460 of its own.
            //
            // POP SS (0x17) has its OWN arm below and is not part of this one. FS and GS have no
            // `POP` encoding in the one-byte map at all, so the DS/ES pair is the whole family
            // here.
            //
            // WORD ONLY, and the refusal is in this arm rather than downstream. At `Dword` the
            // interpreter pops FOUR bytes and loads the low 16 (386 PRM), which is a different
            // stack movement and a different bus charge; no fixture measures that form, and
            // `stack_width_kind`'s `(kind, true, Dword)` arm would otherwise wave it straight
            // through to an emitter that only knows the 16-bit shape.
            0x07 | 0x1f if v86_loop_rows_enabled() => {
                if insn.operand_size != OperandSize::Word {
                    return None;
                }
                return Some(DirectKind::PopSegReal {
                    segment: if opcode == 0x07 {
                        SegmentIndex::Es
                    } else {
                        SegmentIndex::Ds
                    },
                });
            }
            // POP SS, the S4 part-2 row, and DELIBERATELY not a `PopSegReal` with a third segment.
            //
            // Two things separate it from POP ES and POP DS, and each one on its own decides the
            // answer. It arms the one-instruction interrupt shadow
            // (`load_segment_arming_ss_shadow`), which no lowering can honour: the interpreter
            // consumes the shadow at the start of the next instruction and a native block has no
            // such point. And it loads the STACK segment, so a resumed block's remaining slots
            // address through the record it just wrote.
            //
            // Both are answered by the call-out rather than by a refusal. The shadow is left armed
            // and decided at the block boundary (design section 10.1, B1); the record is
            // byte-compared by R2 after the step, so a stack SWITCH resyncs before any later slot
            // runs and only a reload of the same record resumes. The loader measured that split at
            // 484,385 same-record loads against 488,498 record-moving ones across both SS arms,
            // which is design review 10.1 M5 and the gate this row was built behind.
            //
            // NOT through `stack_width_kind`. `PopSegReal` is refused there in protected mode and
            // refused again on a 32-bit stack, because its emitter knows one shape: three stores
            // computing `base = selector << 4` off a 16-bit stack read. This row has no emitter at
            // all -- the helper runs the interpreter's own `0x17` arm -- so every mode and both
            // stack widths are the interpreter's, including the 386 PRM rule that a Dword POP SS
            // moves four bytes of stack and loads the low sixteen. `DirectKind::CallOut` is not in
            // `uses_stack`, so the width matrix is not consulted and does not need to be.
            //
            // The V86 and protected-mode fault paths are the interpreter's too: a null selector, a
            // non-writable descriptor and a privilege mismatch each raise #GP and a present-bit
            // clear raises #SS, all from inside `load_protected_segment` and all delivered by the
            // helper's fault arm exactly as they are at a boundary.
            0x17 => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::InterpretOne {
                        row: InterpretOneRow::PopSs,
                    },
                });
            }
            0x68 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(insn.imm),
                });
            }
            0x6a => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(crate::sign_extend_u8(insn.imm as u8)),
                });
            }
            // THREE-operand IMUL, register source only: `IMUL r32, r/m32, imm32` (0x69) and
            // `IMUL r32, r/m32, imm8` (0x6b). `decode` has already fetched the immediate and
            // sign-extended the imm8 into `insn.imm`, so the two opcodes reach one kind.
            //
            // These are the rejected-row campaign's `non_continuable` rows, and lowering them
            // takes TWO changes, not one. `block_continuable` (decode.rs) routes 0x69/0x6b to
            // `DecodeGroup::Misc` -- the "heterogeneous one-off single-byte block", a decode
            // CLASSIFICATION neighbourhood, not a semantic class -- and refuses the whole group
            // bar TEST AL/AX,imm. The compile walk consults `insn.continuable` before it ever
            // reaches `classify`, so this arm is dead without the walk's own admission
            // (`jit_admits_non_continuable`, direct.rs) and that admission relocates the row onto
            // `hard_boundary` without this arm. Neither half is worth landing alone.
            //
            // MEMORY form deliberately absent. It is a missed lowering, not a hazard: the `else`
            // returns None and the instruction stays exactly the barrier it is today. Building it
            // means an `ImulMemImm` alongside `ImulMem` with the full memory side-exit set, for a
            // row the census measures at 473 quake exits and zero doom -- an unmeasured mechanism
            // by this campaign's standing rule. The register row is doom's largest at 244,547.
            //
            // Below the `OperandSize::Word` gate, and 0x69/0x6b are absent from its allowlist, so
            // a 66-prefixed encoding falls to None above and never reaches here. That is
            // load-bearing rather than incidental: at Word the interpreter multiplies at sixteen
            // bits and `write_gpr_sized(.., Word, ..)` PRESERVES the destination's high half,
            // while the emitted `imul r32, r32, imm32` defines all thirty-two and computes CF/OF
            // against the wrong width. Same argument as 0x0faf's arm above, same two failure
            // modes.
            0x69 | 0x6b => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(src) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::ImulImm {
                    dst: m.reg,
                    src,
                    imm: insn.imm,
                });
            }
            0x80 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    // `lane: None` here for `imm_lane_for`'s reason on the `0x81` arm below:
                    // `classify` has no `&CpuGsw` and no physical address, and a lane needs both.
                    // The compile loop attaches one through `imm8_lane_for` for exactly this
                    // shape when `IZARRAVM_IMM8_LANES` admits it.
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluByteImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm as u8,
                        lane: None,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x81 | 0x83 => {
                let m = insn.modrm?;
                // ADC (/2) and SBB (/3) are refused at Word size and only there. They consume the
                // incoming CF as an operand, which the Dword path handles with a branch on the
                // EFLAGS shadow (`emit_carry_alu_preloaded`) that has no sixteen-bit twin. Refusing
                // is a missed lowering; the census measures zero Word `0x83 /2` and `/3`.
                if insn.operand_size == OperandSize::Word && matches!(m.reg, 2 | 3) {
                    return None;
                }
                return match insn.operand? {
                    // `lane: None` here is deliberate and not a stub: `classify` has no `&CpuGsw`
                    // and no physical address, and a lane needs both. The compile loop attaches
                    // one through `imm_lane_for` for the single shape that qualifies, exactly as
                    // `stack_width_kind` resolves the CPU-dependent stack forms after this point.
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm,
                        lane: None,
                        width: operand_width,
                    }),
                    // The MEMORY form now carries `operand_width`. It was Dword-only with the note
                    // that "the Word path through it has never been exercised and the
                    // read/modify/write triple would need its own differential row"; that row now
                    // exists (`cpu_jit_word_memory_test.rs`) and this is what it certifies.
                    //
                    // 16-bit read-modify-write is both halves of the width hazard in one
                    // instruction, and `emit_alu_mem_dest` already expressed both: the read is
                    // `movzx_r32_word_disp8`, so the ALU sees a 16-bit operand and the lazy
                    // descriptor's `a`/`b` are 16-bit values; the write-back is `store_r16_disp8`,
                    // so the two adjacent bytes are untouched. `emit_alu_candidate` uses
                    // `alu_r16_r16` -- a real 66-prefixed host instruction -- so CF and OF are the
                    // 16-bit ones rather than a 32-bit operation's masked afterwards, and
                    // `emit_commit_alu_candidate` tags the descriptor 0x100 for Word where it tags
                    // 0x200 for Dword. Nothing in that path had to be built; it had no caller.
                    //
                    // ADC (/2) and SBB (/3) are already refused at Word above, so the one op class
                    // with no word lane cannot reach here. This is quake's largest surviving row:
                    // `0x83 /7` memory word at 162,440 exits, with doom carrying /5, /0 and /7
                    // together at 12,192.
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: operand_width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x84 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::TestByte { a, b: m.reg });
            }
            // TEST r/m16|32, r16|32, REGISTER form. `width` is `operand_width` as of the
            // 2026-08-21 duke slice; it was a hard-coded Dword, which is why the header's
            // "deliberately NOT here" list named this opcode. The Word arm is reached only when
            // `test_word_rows_enabled()` opened the gate above, so the Dword behaviour is
            // unchanged in both arms and the Word one exists only under the knob.
            //
            // The MEMORY form falls to `return None` at both widths, exactly as it did before this
            // slice. It has no kind, no emitter and no census row on any measured fixture.
            //
            // No `raw_clocks` field is needed and none is added: the interpreter's 0x85 arm ends
            // in `Ok(clocks(2))` without consulting `operand_size`, and `DirectKind::raw_clocks`
            // has no `Test` arm, so both widths ride the `_ => 2` default and charge exactly what
            // the interpreter charges. This is the field `Load`, `Store` and `MovExtendReg` each
            // had to add; TEST is the case where the default is already right, and saying so here
            // is what keeps the next reader from adding a wrong one.
            0x85 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Test {
                    a,
                    b: m.reg,
                    width: operand_width,
                });
            }
            0xa8 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Byte,
                });
            }
            0xa9 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Dword,
                });
            }
            0x88 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovRegByte { dst, src: m.reg }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x89 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovReg {
                        dst,
                        src: m.reg,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8a => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovRegByte { dst: m.reg, src }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8b => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovReg {
                        dst: m.reg,
                        src,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8c => {
                // MOV r/m16, Sreg. The interpreter writes `OperandSize::Word` unconditionally
                // ("always a word store regardless of operand size", execute.rs 0x8c), so this
                // kind carries no width and both the prefixed and unprefixed encodings lower
                // identically — which is what lets 0x8c join the Word-size allowlist above.
                // reg 6 and 7 are left to the interpreter deliberately. `segment_from_reg_field`
                // folds them into GS through a catch-all `_` arm rather than by intent, and
                // reproducing an accident is how a lowering and its oracle drift apart; 0..=5 are
                // the encodings with a named answer.
                let m = insn.modrm?;
                let segment = match m.reg {
                    0 => SegmentIndex::Es,
                    1 => SegmentIndex::Cs,
                    2 => SegmentIndex::Ss,
                    3 => SegmentIndex::Ds,
                    4 => SegmentIndex::Fs,
                    5 => SegmentIndex::Gs,
                    _ => return None,
                };
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    // The MEMORY form, an `InterpretOne` call-out and the first row of the S3
                    // policy widening. It tops the post-S2 loader census at 2.20 M block-stopping
                    // hits (12.4%), and it is a call-out rather than a lowering for the same
                    // reason 0x8F is: a two-byte store of a selector is not worth an emitter, an
                    // address computation, a fast-map probe, a code-watch guard and a side-exit
                    // stub set, and until this row was lifted it ended the block instead.
                    //
                    // ALL FOUR Sreg values the census measures ride the arm together, and so do
                    // the other two of 0..=5: the helper runs the decode line, so there is no
                    // per-segment lowering to get wrong and refusing four of six would be the
                    // arbitrariness this file's header rules out. `/6` and `/7` stay refused by
                    // the match above, on the same ground the register form refuses them --
                    // `segment_from_reg_field` folds them into GS through a catch-all rather
                    // than by intent, and a block compiled around an accident is worse than a
                    // boundary.
                    //
                    // No width question. The interpreter writes `OperandSize::Word` whatever the
                    // operand size, which is what already lets `0x8c` sit on the Word allowlist,
                    // and the call-out inherits that by running the interpreter's own arm.
                    return Some(DirectKind::CallOut {
                        helper: CallOutHelper::InterpretOne {
                            row: InterpretOneRow::MovRmSreg,
                        },
                    });
                };
                return Some(DirectKind::MovSegToReg { dst, segment });
            }
            // MOV Sreg, r/m16, the write half of the segment family. FOUR answers now, and the
            // arm's whole job is to keep them apart:
            //
            // | form | answer |
            // |---|---|
            // | `/0` ES or `/3` DS, register source, real mode or V86 | `LoadSegReal`, a lowering |
            // | `/0` ES or `/3` DS, register source, protected mode | an `InterpretOne` call-out, chosen in `stack_width_kind` where the mode is in scope |
            // | `/0` ES, `/3` DS, `/4` FS or `/5` GS, memory source, any mode | an `InterpretOne` call-out |
            // | `/4` FS or `/5` GS, register source, any mode | an `InterpretOne` call-out |
            // | `/2` SS, any source, any mode | an `InterpretOne` call-out on its OWN census row |
            // | `/1` CS, `/6`, `/7` | refused |
            //
            // The call-out arms are the S3 policy widening's eighth row: the post-S2 loader census
            // ranks `0x8E` at 1.27 M block-stopping hits, of which the memory form is 786 k.
            //
            // WHAT THE CALL-OUT ADMITS THAT THE LOWERING CANNOT. `LoadSegReal` emits
            // `base = selector << 4` and nothing else, which is what a segment load IS in real
            // mode and V86 and is nothing like what it is in protected mode: a GDT or LDT fetch
            // with type, privilege and present checks, an accessed-bit write-back, and three
            // fault vectors. The helper runs the interpreter's arm, so every one of those is the
            // interpreter's, exactly.
            //
            // WHAT IT COSTS. R2 compares all six cached segment records, so a load that CHANGES
            // the record resyncs and a load of the same selector onto the same descriptor
            // resumes. A guest that reloads DS with a new selector therefore resyncs every time
            // and the governor demotes the slot after three of its first eight executions, which
            // is the boundary it had before. A guest that reloads the SAME selector -- the
            // re-establishing `mov ds, ax` a 16-bit C runtime emits at every function that could
            // have changed it -- resumes. Both are correct; only the second is a win, and the
            // governor is what stops the first from being a loss.
            //
            // `/1` (CS), `/6` and `/7` are refused HERE rather than left off the allowlist,
            // because they are not loads at all: the interpreter raises #GP(0) for each
            // (`execute.rs`, the 0x8e arm). Compiling a block around an instruction that can only
            // fault burns a call-out and a governor execution on every execution, and 0x8c is NOT
            // the symmetric case to copy -- `MOV r16, CS` is legal where `MOV CS, r16` is not.
            //
            // A CALL-OUT SLOT REPORTS NO SEGMENT WRITE, unlike the `LoadSegReal` lowering it
            // replaces: `DirectKind::written_segment` answers `None` for it, so the block's
            // `dirty_segments` mask does not learn that this instruction can move a record and the
            // compile walk goes on admitting later slots that address through it. That is safe for
            // exactly one reason, and it is worth stating because it is not local: R2
            // byte-compares all six cached records after the step, so a slot that moved one ends
            // the run before any later slot executes. A resumed block therefore baked nothing
            // stale, and a block that would have baked something stale never resumed.
            //
            // `/2` (SS) is a call-out too since S4 part 2, and a row of its OWN rather than a
            // `/2` folded into `MovSreg`. What used to keep it out was R3's shadow clause: loading
            // SS arms a one-instruction interrupt shadow (`load_segment_arming_ss_shadow`) and
            // every row had to find that flag clear. The clause is now scoped per row
            // (`InterpretOneRow::arms_interrupt_shadow`), the shadow is decided at the block
            // boundary rather than inside the helper, and the row carries the pendency test that
            // bounds what leaving it armed can cost.
            //
            // A SEPARATE ROW because the census question is different. `MovSreg` asks whether a
            // guest re-establishes the same data segment; `MovSsReg` asks how often a guest
            // switches STACKS at this instruction, which the loader answered as 484,385
            // same-record against 488,498 record-moving across both SS arms. One label would
            // average two unrelated populations.
            //
            // Every form and every mode, unlike `/0` and `/3`: those keep the real-mode
            // `LoadSegReal` lowering for their register form and only become a call-out where the
            // lowering cannot go. There is no SS lowering to keep.
            0x8e => {
                let m = insn.modrm?;
                let segment = match m.reg {
                    0 => SegmentIndex::Es,
                    2 => {
                        return Some(DirectKind::CallOut {
                            helper: CallOutHelper::InterpretOne {
                                row: InterpretOneRow::MovSsReg,
                            },
                        });
                    }
                    3 => SegmentIndex::Ds,
                    4 | 5 => {
                        return Some(DirectKind::CallOut {
                            helper: CallOutHelper::InterpretOne {
                                row: InterpretOneRow::MovSreg,
                            },
                        });
                    }
                    _ => return None,
                };
                let DecodedOperand::Reg(src) = insn.operand? else {
                    return Some(DirectKind::CallOut {
                        helper: CallOutHelper::InterpretOne {
                            row: InterpretOneRow::MovSreg,
                        },
                    });
                };
                return Some(DirectKind::LoadSegReal { segment, src });
            }
            0x8d => {
                let m = insn.modrm?;
                let DecodedOperand::Mem(addr) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Lea {
                    dst: m.reg,
                    addr: direct_addr(addr)?,
                    // The OPERAND size, which is what the interpreter passes to
                    // `write_gpr_sized`. The ADDRESS size is a different question and reaches the
                    // emitter through the block's `address_wrap`.
                    width: operand_width,
                });
            }
            0xa0 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                        disp_lane: None,
                    },
                    raw_clocks: 4,
                });
            }
            // MOV AX/EAX, moffs. `width` is `operand_width` as of the V86 loop-A slice; it was a
            // hard-coded `MemoryWidth::Dword`, which is why `0xa1` had to be kept off the Word
            // allowlist and why the note at the top of this file names it as the counterexample
            // that proximity would sweep in. The kind is `Load`, which has carried a width end to
            // end since `0x8b`: at Word `emit_load` reads two bytes through
            // `movzx_r32_word_disp8` and writes the destination with `emit_write_gpr16`, merging
            // into AX and leaving EAX's high half exactly as `write_gpr_sized(0, Word, ..)` does.
            // `raw_clocks: 4` is right at both widths -- the interpreter's arm returns `clocks(4)`
            // without consulting `operand_size`.
            //
            // The moffs displacement is fetched at ADDRESS size and zero-extended (`decode`), so a
            // 16-bit address mode cannot produce a displacement the block's `AddressWrap::Word`
            // would have to mask.
            //
            // The segment comes from the override when there is one, which is what the tombraid
            // row needs: its form is `mov ax, es:[0x6c]`, prefix mask 32.
            0xa1 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: operand_width,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                        disp_lane: None,
                    },
                    raw_clocks: 4,
                });
            }
            0xa2 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                        disp_lane: None,
                    },
                    raw_clocks: 4,
                });
            }
            // MOV moffs, AX/EAX. `width` is `operand_width` for `0xa1`'s reason above, and the
            // store side of the argument is `0x89`'s: `emit_store`'s Word arm is
            // `store_r16_disp8` plus `emit_dynamic_word_increment`, guarded by
            // `emit_wide_page_guard` at 2-byte alignment and by `emit_watched_store_guard`. So a
            // two-byte store writes exactly two bytes and cannot straddle a page. A MISALIGNED one
            // is served rather than refused: `emit_store` dispatches to the relaxed lean store
            // site, which splits the charge instead of side-exiting.
            //
            // Its census row is small (`0x00A3` word, 593,213 hits) but real, and it is in the
            // slice because it sits in the SAME compile unit as the rows that are not: the
            // `0xc8fe7` neighbourhood is `mov ax, es:[0x6c]` / `mov cs:[0xf5], ax`, so admitting
            // the load and refusing the store would relocate the stop one instruction along.
            0xa3 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: operand_width,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                        disp_lane: None,
                    },
                    raw_clocks: 4,
                });
            }
            0xb0..=0xb7 => {
                return Some(DirectKind::MovImmByte {
                    dst: opcode - 0xb0,
                    imm: insn.imm as u8,
                });
            }
            0xb8..=0xbf => {
                return Some(DirectKind::MovImm {
                    dst: opcode - 0xb8,
                    imm: insn.imm,
                    width: operand_width,
                });
            }
            // MOV r/m, imm (group 11). `0xc7`'s MEMORY form is now width-carrying: the doom census
            // ranks `0xC7 /0` memory WORD at 742,811 exits and quake carries 240 of the same row.
            //
            // Three things had to line up and all three are the interpreter's, not a re-derivation:
            //  * the IMMEDIATE. `decode` fetches it with `fetch_immediate(operand_size)`, which at
            //    Word is `u32::from(fetch_u16(..))` -- zero-extended into the same `insn.imm`. So
            //    `emit_read_store_value`'s `imm & 0xffff` for a Word `StoreSource::Imm` is exactly
            //    the two bytes decode read, and the mask is a no-op rather than a truncation. This
            //    is the check `0x81` is still refused for wanting; here it passes.
            //  * the STORE. `write_operand_sized(.., Word, ..)` writes two bytes and touches no
            //    third, which `emit_store`'s Word arm matches instruction for instruction.
            //  * the CLOCKS. The interpreter's arm returns `Ok(clocks(2))` without consulting
            //    `operand_size`, so the `raw_clocks: 2` below is right at both widths.
            //
            // The REGISTER form is refused at Word, and that is the whole asymmetry: it produces
            // `MovImm`, which writes `home(dst)` with a 32-bit immediate move and has no width to
            // narrow. Refusing is a missed lowering that no census row asks for; admitting would
            // clobber the destination's high 16 bits.
            0xc6 | 0xc7 => {
                let m = insn.modrm?;
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xc6 {
                    MemoryWidth::Byte
                } else {
                    operand_width
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) if opcode == 0xc6 => Some(DirectKind::MovImmByte {
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Reg(_) if insn.operand_size == OperandSize::Word => None,
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovImm {
                        dst,
                        imm: insn.imm,
                        width: MemoryWidth::Dword,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Imm(insn.imm),
                        width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            // SHL r8, imm8 (0xC0 /4), REGISTER form. Second in the duke3d-586 re-census's
            // refused-row ranking behind the ROL above: 32,839,852 runtime hits and 31,743,121
            // static unbound exits, the latter up from 5.7M for the same reason ROL's grew.
            //
            // ONE sub-opcode, and the narrowness is the point rather than laziness. The census
            // measures `/4` alone; `/6` is its undocumented SAL alias and would be free to add,
            // but no row asks for it and an alias admitted on inspection is exactly the unmeasured
            // admission this file refuses elsewhere. `/5` SHR and `/7` SAR have no byte row at
            // all, the four byte rotates have neither a row nor an emitter, and 0xD0 (the same
            // group by an implicit 1) tops out at 49,021 runtime hits across every sub-opcode --
            // three orders below the floor this slice is working to.
            //
            // The width is the OPCODE's, not the prefix's. An unprefixed 0xC0 in a 32-bit segment
            // decodes with `OperandSize::Dword`, which is verbatim what the census row says
            // (`operand_form: register`, `operand_size: dword`, `prefix_mask: 0`), so this arm
            // hard-codes `MemoryWidth::Byte` the way 0xC6's does and must never read
            // `operand_width`. The consequence at the other end is a MISSED lowering, not a
            // hazard: 0xC0 is absent from the `OperandSize::Word` allowlist at the top of this
            // file, so a 66-prefixed encoding and a 16-bit code segment both refuse before
            // reaching here even though the guest semantics would be identical. Adding the entry
            // is safe on its own terms but has no measured row behind it.
            //
            // The MEMORY form is refused by the `let-else` below, as every other register-only arm
            // in this file refuses it, and it is a separate census row this slice does not claim.
            0xc0 => {
                let m = insn.modrm?;
                // The off arm (`IZARRAVM_ROTATE_ROWS=0`, the opt-out from the shipped default since
                // the 2026-08-19/20 flip). Read HERE rather than in the
                // emitter so the off arm is the pre-slice refusal byte for byte: this whole
                // opcode had no arm before the slice, so returning None from the top of it puts
                // the row back in the census as the same `hard_boundary` it was ranked as.
                //
                // `rotate_rows_enabled` is true on BOTH admitting arms (`on` and `heat_gated`).
                // The heat gate cannot live here -- it needs the physical address and the heat
                // map, neither of which is in this function's signature -- so it runs one step
                // later, in the compile loop, and downgrades this admission to the very same
                // `HardBoundary` the `!enabled` return produces. See `rotate_rows_arm`.
                if !rotate_rows_enabled() {
                    return None;
                }
                if m.reg != 4 {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Shift {
                    op: 4,
                    dst,
                    count: insn.imm as u8,
                    width: MemoryWidth::Byte,
                    // `classify` never attaches a lane: it has neither the physical address nor
                    // the cpu in scope. `count_lane_for` fills this in from the compile walk, at
                    // the last point before the slot is committed.
                    lane: None,
                });
            }
            0xc1 | 0xd1 => {
                let m = insn.modrm?;
                // reg 0 and reg 1 are the two lowered ROTATES and MUST be admitted by this guard,
                // not appended after it: neither passes `matches!(m.reg, 4..=7)`, so a rotate arm
                // placed below the guard would be unreachable and the whole lowering would be dead
                // code that no negative test could detect.
                //
                // `/0` ROL joined `/1` ROR on 2026-08-09 and it is the largest row in the
                // duke3d-586 re-census by BOTH currencies: 260,659,304 runtime hits, the hottest
                // interpreted instruction in the trace, AND 111,123,374 static unbound exits, the
                // largest refused-row seam, up from 10.3M once the disp-lane slice raised coverage
                // and blocks started compiling up TO it and seaming every pass.
                //
                // RCL (/2) and RCR (/3) stay out and the reason is structural rather than a
                // missing row: both take the incoming CF as a rotate INPUT (`shift_rotate` seeds
                // `cf` from `flag(FLAG_CF)` before its loop), which needs the guest flags loaded
                // into the host before the rotate rather than only captured after it -- the shape
                // `emit_carry_alu_preloaded` has, and a slice of its own rather than a list entry.
                if !matches!(m.reg, 0 | 1 | 4..=7) {
                    return None;
                }
                // The off arm (`IZARRAVM_ROTATE_ROWS=0`, the opt-out since the 2026-08-19/20 flip),
                // and it covers `/0` ALONE. `/1` ROR
                // was lowered before this slice and stays ungated: the off arm has to restore the
                // pre-slice world, not a no-rotates world, or an A/B would price two slices as
                // one. `/4..=7` are older still. Read here, above the shared `let-else` and the
                // Word guard, so the off arm reproduces the pre-slice refusal exactly -- ROL had
                // no arm at all then, so the row goes back to being an ordinary `hard_boundary`.
                //
                // The `heat_gated` arm reads as ADMITTING here and is narrowed one step later, in
                // the compile loop, where the physical address and the heat map are in scope; see
                // `rotate_rows_arm` and `rotate_row_count_byte_is_patched`. `/1` ROR is outside
                // that gate too, exactly as it is outside this one.
                if m.reg == 0 && !rotate_rows_enabled() {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                // The RAW immediate, unmasked, matching what the Shift arm has always stored. The
                // architectural five-bit mask is applied in the emitter.
                let count = if opcode == 0xd1 { 1 } else { insn.imm as u8 };
                if matches!(m.reg, 0 | 1) {
                    // BOTH rotates are REFUSED at Word, and this guard is the only thing that
                    // stops them now that 0xc1 is on the Word allowlist. `RotateReg` carries no
                    // width and its emitter is `shift_r32_imm8(op, ..)` plus `emit_set_cf_only`,
                    // so a 66-prefixed rotate routed through it would rotate 32 bits where the
                    // guest rotates 16 -- wrong result AND wrong CF, since the bit rotated across
                    // the boundary comes from bit 31 instead of bit 15. The duke3d-586 re-census
                    // measures the ROL row at `operand_size: dword` with `prefix_mask: 0`, and no
                    // fixture measures a Word row for either sub-opcode, so this is a refusal with
                    // nothing to buy rather than a missed lowering worth the second emitter lane.
                    if insn.operand_size == OperandSize::Word {
                        return None;
                    }
                    return Some(DirectKind::RotateReg {
                        op: m.reg,
                        dst,
                        count,
                        lane: None,
                    });
                }
                return Some(DirectKind::Shift {
                    op: m.reg,
                    dst,
                    count,
                    width: operand_width,
                    lane: None,
                });
            }
            // Group 2 by CL. Sixth in the runtime-weighted reject audit: /7 alone is 807,607
            // dispatcher exits and the three shift sub-ops together are 7.9% of rejected-target
            // exits. Same /4..=7 admission as the imm8 arm's shift half; ROL (/0), ROR (/1),
            // RCL (/2) and RCR (/3) stay out -- ROR because its flag rules differ and the other
            // three for the imm8 arm's standing reasons (no measured rejects; RCL/RCR consume
            // the incoming CF as a rotate input the emitted form never loads).
            0xd3 => {
                let m = insn.modrm?;
                if !matches!(m.reg, 4..=7) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::ShiftCl { op: m.reg, dst });
            }
            0xf6 | 0xf7 => {
                let m = insn.modrm?;
                // GROUP 3 AT WORD, `/2../7`, as `InterpretOne` call-outs. The S3 policy widening's
                // fourth row: NOT, NEG, MUL, IMUL, DIV and IDIV, both operand forms.
                //
                // FIRST in the arm, and that placement is the whole safety argument. Every
                // lowering below carries NO width field and says so in its own comment: `NegReg`
                // writes a full 32-bit destination, `MulReg` replaces EDX and EAX rather than
                // merging DX and AX, `DivReg`/`DivMem` read four bytes. Each names the
                // `OperandSize::Word` gate as the only thing keeping a 66-prefixed form away from
                // it. That gate now admits `0xf7 /2../7`, so the guard has to move HERE, in front
                // of them, or every one of those comments becomes a miscompile. The Dword forms
                // fall past this and reach their emitters unchanged.
                //
                // `/2` NOT has no lowering at any width and is a call-out at Word only, which is
                // where the census measures it. Widening it to Dword would be an unmeasured
                // admission, which this file's header rules out; the Dword form stays the boundary
                // it has always been.
                //
                // DIV and IDIV can raise #DE from inside the helper. That is the ordinary
                // RESYNC-after-fault path and needs nothing new: `finish_instruction` rewinds onto
                // the instruction, delivers the exception, counts and charges it, and the block
                // reports the prefix only. It is the first admitted row whose fault arm fires on
                // ORDINARY DATA rather than on a bad address, which is why it has a fixture of its
                // own.
                if opcode == 0xf7
                    && insn.operand_size == OperandSize::Word
                    && matches!(m.reg, 2..=7)
                {
                    return Some(DirectKind::CallOut {
                        helper: CallOutHelper::InterpretOne {
                            row: InterpretOneRow::Group3,
                        },
                    });
                }
                // NEG r/m32, register form. Deliberately carries NO width field: this arm sits
                // below the OperandSize::Word gate at the top of `classify`, and that allowlist
                // (which does not contain 0xf7) is the ONLY thing stopping a 586-mode `66 F7 /3`
                // from reaching here, since the persona gate admits word ops on 586 and
                // `prefixes_supported` accepts the operand-size override. A `width` field would
                // invite a future edit to pass `operand_width`, which is MemoryWidth::Word in
                // exactly that case, and a 16-bit NEG would then be lowered as a 32-bit one,
                // clobbering the destination's high half. Same hazard the 0x0faf comment above
                // describes. In a 16-bit segment the unprefixed form is Word (gated the same way)
                // and the 66-prefixed form is rejected earlier for carrying a prefix at all, so
                // NEG is simply never lowered there.
                if opcode == 0xf7 && m.reg == 3 {
                    let DecodedOperand::Reg(dst) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::NegReg { dst });
                }
                // MUL r/m32, register form. `reg == 4` is the UNSIGNED multiply; /5 next to it is
                // the signed IMUL, whose overflow rule is different (the product not sign-extending
                // back from the low half rather than the high half being nonzero), so this must not
                // widen to `4..=5`. Carries no width field for the same reason NegReg does not: the
                // OperandSize::Word gate above is the only thing keeping a 586-mode `66 F7 /4` out,
                // and a 16-bit MUL writes DX and AX as halves of the existing EDX and EAX rather
                // than replacing them.
                if opcode == 0xf7 && m.reg == 4 {
                    let DecodedOperand::Reg(src) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::MulReg { src });
                }
                // IMUL r/m32, one-operand SIGNED multiply, memory form. `reg == 5`, the signed
                // sibling of the /4 above, whose overflow rule is different: the product failing to
                // sign-extend back from the low half, rather than the high half being nonzero.
                //
                // ORDERING INVARIANT, and it is load-bearing rather than cosmetic. This arm MUST
                // stay BELOW the /4 arm. That arm's `else { return None }` returns from `classify`,
                // not from the arm, so a /4 with a MEMORY operand is already unreachable by the time
                // control gets here. Move this arm above it and widen either to `4..=5` and an
                // unsigned `mul dword [mem]` is emitted as a signed multiply, with the wrong EDX and
                // the wrong CF and OF. `mul_memory_form_stays_interpreter_only` is what catches it.
                //
                // `opcode == 0xf7` is equally load-bearing: this arm sits inside the shared
                // `0xf6 | 0xf7` group arm, and 0xF6 /5 is the BYTE IMUL, which multiplies AL and
                // writes only AX. Without the test it would be read as a dword and lowered as the
                // dword multiply. `imul_byte_form_stays_interpreter_only` is what catches that.
                //
                // No width field and no raw_clocks field. The OperandSize::Word gate above keeps a
                // 66-prefixed form out, and the whole group-3 arm returns clocks(2), which is
                // already the DirectKind::raw_clocks default. This is the opposite of 0x0FAF, where
                // the interpreter charges clocks(9) and the default undercharges by 7.
                if opcode == 0xf7 && m.reg == 5 {
                    return match insn.operand? {
                        // The REGISTER form, added by the rejected-row campaign's F7 slice. It
                        // shares this arm with the memory form rather than sitting in its own
                        // `if` because everything that makes the memory form correct here --
                        // `opcode == 0xf7` excluding the byte IMUL, the position BELOW /4
                        // excluding an unsigned multiply, the Word gate excluding a 66-prefixed
                        // encoding -- is exactly what makes the register form correct, and
                        // splitting them would invite one of the three to be re-derived wrongly.
                        DecodedOperand::Reg(src) => Some(DirectKind::ImulRegAcc { src }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::ImulMemAcc {
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                // DIV (/6) and IDIV (/7) r/m32, BOTH operand forms. The sub-opcode pair is ONE arm
                // because the emitter's two bodies are selected by `signed` and both are
                // separately tested; what must not be widened is the pair of ENCODER primitives
                // behind them.
                //
                // The MEMORY form is the tombraid FMV census's `0xF7 /7 memory dword` row at
                // 27,602,949 interpreted hits, one per iteration of the 32-bit FPU loop, and it
                // sits behind `IZARRAVM_FPU_LOOP_ROWS`. `/6` rides the gate with it by the closure
                // rule at the top of this file rather than by a second measurement: the two
                // sub-opcodes have shared this arm since it was written, and splitting them at the
                // memory form alone would be the arbitrary half-admission that rule exists to stop.
                //
                // What this arm used to say, and what the emitter now has to answer:
                //
                // > MEMORY is deliberately absent, and the reason is the fault rather than the
                // > address. A memory DIV can side-exit for two independent reasons -- the read's
                // > own guards and the divide guard -- at the same slot, and the second must not
                // > be reachable before the first has been proved not to fire.
                //
                // `emit_div_mem` answers it with the DEFERRED mode-13 completion `Ret`/`JmpMem`
                // already use; see the `DirectKind::DivMem` doc comment for why the ordering
                // hazard is the read COUNTER rather than the exits themselves.
                //
                // No `raw_clocks`: group 3 charges `clocks(2)` for every sub-opcode and both
                // operand forms, which is the `_ => 2` default. No `width` on either kind: the
                // `OperandSize::Word` gate at the top of `classify` excludes 0xf7, exactly as for
                // `MulReg`, so both forms are dword-only by construction rather than by the
                // absence of a 0x66 prefix.
                if opcode == 0xf7 && matches!(m.reg, 6 | 7) {
                    let signed = m.reg == 7;
                    let DecodedOperand::Reg(src) = insn.operand? else {
                        let DecodedOperand::Mem(addr) = insn.operand? else {
                            return None;
                        };
                        if !fpu_loop_rows_enabled() {
                            return None;
                        }
                        return Some(DirectKind::DivMem {
                            addr: direct_addr(addr)?,
                            signed,
                        });
                    };
                    return Some(DirectKind::DivReg { src, signed });
                }
                if m.reg != 0 {
                    return None;
                }
                // `/0` TEST. `0xf6` is the byte form regardless of prefix; `0xf7` follows the
                // operand size, and it is the group-3 member with a real Word LOWERING --
                // `emit_test_preloaded` has carried a full Word lane (66-prefixed `test`, 0x100
                // descriptor tag) since the width field landed, with no caller until the wolf3d
                // census ranked the register form at 634M block-stopping hits.
                //
                // The word MEMORY form is an `InterpretOne` CALL-OUT as of the S3 policy widening,
                // where it is the loader census's 242 k row. It is not a lowering, and the reason
                // is an admission question rather than a capability one: the refusal it used to
                // carry here said "no fixture measures a row for it", the census now does, and the
                // call-out answers it without an emitter change at all.
                let width = if opcode == 0xf6 {
                    MemoryWidth::Byte
                } else {
                    operand_width
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::TestImmReg {
                        dst,
                        imm: insn.imm,
                        width,
                    }),
                    DecodedOperand::Mem(addr) => {
                        if width == MemoryWidth::Word {
                            return Some(DirectKind::CallOut {
                                helper: CallOutHelper::InterpretOne {
                                    row: InterpretOneRow::Group3,
                                },
                            });
                        }
                        Some(DirectKind::TestImmMem {
                            imm: insn.imm,
                            width,
                            addr: direct_addr(addr)?,
                        })
                    }
                };
            }
            0xc2 | 0xc3 => {
                // No width gate here. `OperandSize` has exactly two variants, so the only widths
                // that reach this arm are Word and Dword, and both are wanted: the Word-size
                // allowlist above decides admission and the compile loop's stack-width matrix
                // rewrites the Word form into its own kind. A Byte check would read as live and
                // be provably unreachable.
                return Some(DirectKind::Ret {
                    release: if opcode == 0xc2 { insn.imm as u16 } else { 0 },
                });
            }
            // INC/DEC r/m8. The REGISTER form is lowered; the MEMORY form is an `InterpretOne`
            // call-out as of the S3 policy widening, where the post-S2 loader census ranks it at
            // 484 k block-stopping hits.
            //
            // The memory form's old refusal named a real cost and the call-out pays none of it:
            // `emit_rmw_inc_dec` handles Dword and Word and debug-asserts on the rest, and a Byte
            // path would need its own code-watch width, its own counter lane and an answer for the
            // fact that a byte access takes NO alignment guard at all. The helper runs the
            // interpreter's arm instead, which has all three already.
            //
            // This is the row the deferred-code-write probe in `note_code_write_inner` was fixed
            // for. A byte store reaches the invalidation choke on `changed` alone, without the
            // `code_write_watched` pre-gate the sized path makes, so before that fix every
            // execution of this row would have recorded a write, failed R5 and RESYNCed.
            //
            // `dst` here is a BYTE-REGISTER index, where 4..7 mean AH/CH/DH/BH rather than
            // ESP/EBP/ESI/EDI. The emitter's byte branch reads and writes through the lane
            // helpers for exactly that reason; a `home(dst)` on this value would hit the wrong
            // register entirely.
            0xfe => {
                let m = insn.modrm?;
                if !matches!(m.reg, 0 | 1) {
                    return None;
                }
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::IncDecReg {
                        dst,
                        is_dec: m.reg == 1,
                        width: MemoryWidth::Byte,
                    }),
                    DecodedOperand::Mem(_) => Some(DirectKind::CallOut {
                        helper: CallOutHelper::InterpretOne {
                            row: InterpretOneRow::IncDecRm8,
                        },
                    }),
                };
            }
            0xff => {
                let m = insn.modrm?;
                // /6 PUSH r/m32, memory form only. The REGISTER form is architecturally
                // `PUSH r32` and is refused: its clock charge would have to be checked against
                // 0x50..0x57 rather than assumed, and the attribution census measures zero
                // occurrences of it on this corpus. Refusing it is a missed lowering worth
                // nothing; mapping it onto `Push` without checking is a timing bug.
                //
                // /3 far CALL and /5 far JMP are not lowered here; both load a descriptor, which
                // needs machinery this classifier does not have.
                //
                // /2 CALL r32, REGISTER form only. The interpreter reads the target from the GPR
                // BEFORE the return EIP is pushed (execute_extended.rs, group 5 arm 2), which is
                // why the emit arm reloads home(dst) before the ESP adjust rather than after: the
                // register form is a dynamic-target control transfer, needing the same successor
                // machinery `Ret` and `JmpMem` use.
                //
                // Classified regardless of operand width; there is no width gate here the way /4
                // JMP has one. `CallReg` IS `uses_stack()`, so a 66-prefixed form routes into the
                // stack-width admission matrix in the compile loop, which refuses it for lack of a
                // `CallReg16` mapping arm, the same PushMem precedent that guards PUSH r/m32.
                //
                // The MEMORY form IS lowered now, as `CallMem`. The "census measures zero
                // occurrences" note this comment used to carry was true when `CallReg` landed and
                // is not any more: the post-Phase-5 census ranks `0xFF /2` memory dword as doom's
                // largest rejected row, 1,847,385 attributed exits over 3,076,346 interpreted
                // executions per timedemo. Quake still measures zero at any width, so that fixture
                // is a control for this arm rather than a second sample.
                //
                // The Dword gate is `JmpMem`'s, for `JmpMem`'s reason and not by analogy: `0xff`
                // sits in the `OperandSize::Word` allowlist above, so a 66-prefixed `FF /2` in
                // 32-bit code reaches here at Word size, where the interpreter reads TWO bytes and
                // masks EIP to 16 bits.
                //
                // It is REDUNDANT TODAY and is kept anyway, stated plainly because a mutation
                // proved it: `CallMem` is `uses_stack()`, so the Word form also reaches the
                // stack-width admission matrix, which has no `CallMem16` arm and refuses it in
                // both stack widths. Deleting either check alone leaves the Word form refused by
                // the other, so `word_size_call_through_memory_stays_refused` cannot distinguish
                // them and does not claim to. What the gate is for is the case the matrix stops
                // covering the moment anyone adds a `CallMem16` mapping arm: the pushed return
                // address and the READ WIDTH of the target are independent, and only this gate
                // constrains the second. `JmpMem` has no matrix at all -- it is not a stack kind --
                // so there the same check is the only one there is.
                if m.reg == 2 {
                    let return_delta = lin
                        .wrapping_add(u32::from(insn.len))
                        .wrapping_sub(entry_lin);
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::CallReg { dst, return_delta }),
                        DecodedOperand::Mem(addr) => {
                            if insn.operand_size != OperandSize::Dword {
                                return None;
                            }
                            Some(DirectKind::CallMem {
                                addr: direct_addr(addr)?,
                                return_delta,
                            })
                        }
                    };
                }
                if m.reg == 6 {
                    let DecodedOperand::Mem(addr) = insn.operand? else {
                        return None;
                    };
                    // WORD is an `InterpretOne` call-out, the S3 policy widening's sixth row;
                    // Dword keeps `PushMem`.
                    //
                    // The split is the stack, not the read. `PushMem` is a `uses_stack()` kind, so
                    // the Word form already reached the compile loop's stack-width matrix, which
                    // has no `PushMem16` cell and refuses it in BOTH stack widths -- a Word push
                    // moves two bytes and decrements the pointer by two, and no emitter builds
                    // that. Deciding it here rather than there is what keeps the matrix's arms
                    // about emitters: a call-out is not a stack kind and never reaches it.
                    //
                    // The operand size and the STACK width are independent, and only the first is
                    // in scope here. That is enough: the call-out is correct for either stack
                    // width because the interpreter's own `push` reads SS.B for itself.
                    if insn.operand_size != OperandSize::Dword {
                        return Some(DirectKind::CallOut {
                            helper: CallOutHelper::InterpretOne {
                                row: InterpretOneRow::PushRm,
                            },
                        });
                    }
                    return Some(DirectKind::PushMem {
                        addr: direct_addr(addr)?,
                    });
                }
                // /4 JMP r/m32, BOTH operand forms. `0xff` is in the `OperandSize::Word` allowlist
                // above, so a 66-prefixed `FF /4` in 32-bit code reaches this arm at Word size.
                // NOTHING downstream refuses that: `uses_stack` is false for a jump, so the
                // stack-width admission matrix never sees this kind, and `static_control_target`
                // is `None` for a dynamic target, so the Word control clamp never sees it either.
                // This check is the only gate, on I586 (every other persona refuses Word before
                // reaching here). At Word size the interpreter reads TWO bytes and masks EIP to
                // 16 bits; lowering that as the Dword construction reads four bytes and jumps
                // unmasked, a miscompile twice over.
                //
                // The gate is SHARED by the two operand forms rather than duplicated inside each,
                // and it is the only thing refusing the register form at Word: the residual
                // `0xFF /4` register word census row (78,585 exits) stays out through this line
                // and nothing else. `jmp_reg_stays_refused_at_word_size` pins that, and deleting
                // this check is mutation M1.
                //
                // The register form is lowered as `JmpReg`. The "census zero, PUSH-r32-style
                // clock risk" note this arm used to carry recorded two objections and both have
                // been answered: the duke3d-486 census reads 11,718,562 static exits and
                // 11,736,700 interpreted executions here (32.8M/32.8M at 586, its fourth-largest
                // rejected row), and the clock charge is not a guess -- `execute_extended.rs`
                // group-5 arm 4 returns `clocks(7)` unconditionally, reading its target through
                // `read_operand_sized`, which serves the register and memory operands alike.
                // NOT ON THE `InterpretOne` ALLOWLIST, and the reason is structural rather than
                // a matter of census weight. The S3 policy widening was asked to consider the
                // Word memory form (510 k block-stopping hits on the post-S2 loader census, with
                // a segment override) and REFUTED it.
                //
                // An `InterpretOne` slot resumes only when `ResumeSnapshot::allows_resume`'s R1
                // holds, and R1 demands `cpu.registers.eip == slot_start + insn_len`. A JMP sets
                // EIP to its TARGET. The two are equal only for a jump to the next instruction,
                // so the slot resyncs on every real execution, and the governor demotes it after
                // three of the first eight -- back to the boundary it replaced, having paid a
                // spill, a call, a run, a reload and a side exit three times over to get there.
                //
                // The compile walk makes the same point from the other side. `DirectKind::JmpMem`
                // is `is_terminal()` and a `CallOut` is not, so admitting the row would let the
                // walk keep appending slots AFTER the jump: slots the resync guarantees can never
                // retire, carried in the block's static accounting and its budget bound.
                //
                // A native `JmpMem16` is the shape that would serve this row, and it is an S4
                // question about an emitter rather than an S3 question about policy.
                if m.reg == 4 {
                    if insn.operand_size != OperandSize::Dword {
                        return None;
                    }
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::JmpReg { dst }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::JmpMem {
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                if !matches!(m.reg, 0 | 1) {
                    return None;
                }
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::IncDecReg {
                        dst,
                        is_dec: m.reg == 1,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::RmwIncDec {
                        is_dec: m.reg == 1,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x70..=0x7f if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jcc {
                    condition: opcode & 0x0f,
                    taken_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            0xe8 if insn.group == DecodeGroup::Branch => {
                let return_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Call {
                    return_delta,
                    target_delta: return_delta.wrapping_add(insn.imm),
                });
            }
            0xe9 | 0xeb if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jmp {
                    target_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            _ => {}
        }
    }
    if matches!(insn.opcode, 0x0f80..=0x0f8f) && insn.group == DecodeGroup::Branch {
        let end_delta = lin
            .wrapping_add(u32::from(insn.len))
            .wrapping_sub(entry_lin);
        return Some(DirectKind::Jcc {
            condition: (insn.opcode & 0x0f) as u8,
            taken_delta: end_delta.wrapping_add(insn.imm),
        });
    }
    None
}

pub(super) fn direct_addr(addr: crate::AddrMode) -> Option<DirectAddr> {
    // Both address sizes. The 16-bit modes already arrive in exactly this shape:
    // `parse_16bit_address` emits the eight register pairs as base/index at scale 1, with the
    // displacement sign-extended and SS selected for the BP forms. The 64K wrap is applied by the
    // emitter as a block property, because the address size is a pure function of CS.D.
    if !matches!(addr.scale, 1 | 2 | 4 | 8) {
        return None;
    }
    Some(DirectAddr {
        segment: addr.segment,
        base: addr.base,
        index: addr.index,
        scale: addr.scale,
        disp: addr.disp as u32,
        // `classify` has no `&CpuGsw` and no physical address, and a lane needs both. The
        // compile loop attaches one through `disp_lane_for` for the single shape that
        // qualifies, exactly as `imm_lane_for` does for `AluImm`.
        disp_lane: None,
    })
}
