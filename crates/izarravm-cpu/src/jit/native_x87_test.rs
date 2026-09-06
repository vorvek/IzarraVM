// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::jit::direct::{MAX_X87_BLOCK_INSTRUCTIONS, MAX_X87_SLOTS, max_x87_block_core_clocks};
use crate::timing_class::class_table;
use crate::{AddressSize, DecodedOperand, OperandSize, Prefixes, RepKind, SegmentIndex};

fn addr() -> AddrMode {
    AddrMode {
        segment: SegmentIndex::Ds,
        base: Some(3),
        index: Some(6),
        scale: 2,
        disp: -12,
        address_size: AddressSize::Dword,
    }
}

fn insn(opcode: u16, mode: u8, reg: u8, rm: u8) -> DecodedInsn {
    DecodedInsn {
        len: 2,
        prefixes: Prefixes::default(),
        opcode,
        operand_size: OperandSize::Dword,
        address_size: AddressSize::Dword,
        modrm: Some(crate::ModRm { mode, reg, rm }),
        operand: (mode != 3).then_some(DecodedOperand::Mem(addr())),
        imm: 0,
        imm2: 0,
        group: DecodeGroup::Fpu,
        continuable: true,
        disp_len: 0,
        imm_len: 0,
    }
}

fn expected_selected(opcode: u16, mode: u8, reg: u8, rm: u8) -> bool {
    matches!(
        (opcode, mode, reg, rm),
        (0xd8, 0..=3, 0..=7, 0..=7)
            // reg 5 and 7 in the MEMORY modes are FLDCW and FNSTCW. They are absent from the
            // register row below, where (0xd9, 3, 5, ..) is FLD1/FLDZ at rm 0 and 6 only.
            | (0xd9, 0..=2, 0 | 2 | 3 | 5 | 7, 0..=7)
            | (0xd9, 3, 0 | 1, 0..=7)
            | (0xd9, 3, 5, 0 | 6)
            // 0xD9 /4: FCHS, FABS, FTST, FXAM. rm 2, 3, 6 and 7 are undefined in the interpreter
            // and must stay rejected, so this is an explicit rm list, not a range.
            | (0xd9, 3, 4, 0 | 1 | 4 | 5)
            // 0xD9 /7 rm=2 is FSQRT and rm=4 is FRNDINT (the FPU-loop slice's census row), the
            // two members of that group with a host equivalent -- a `vsqrtsd` and a `vroundsd`
            // under a runtime branch on the control word's RC field. The other six encodings
            // (FPREM, FYL2XP1, FSINCOS, FSCALE, FSIN, FCOS) stay rejected.
            | (0xd9, 3, 7, 2 | 4)
            // 0xDA memory forms: integer m32 arithmetic, all 8 sub-opcodes (`IntBinaryMemory`).
            // The register row keeps only `(0xda, 3, 5, 1)`, FUCOMPP; every other 0xDA mod=3
            // encoding is FCMOVcc and unrepresentable here, so it stays rejected.
            | (0xda, 0..=2, 0..=7, 0..=7)
            | (0xda, 3, 5, 1)
            // 0xDB memory /2 and /3: FIST/FISTP m32, one interpreter arm with a pop flag, plus
            // `/7` FSTP m80. `/5` (FLD m80) stays rejected -- deferred, see `StoreExtended80` --
            // and `/1`, `/4` and `/6` are undefined.
            | (0xdb, 0..=2, 0 | 2 | 3 | 7, 0..=7)
            // 0xDD memory forms: FLD/FST/FSTP m64 (`LoadF64`/`StoreF64`). `/1` (FISTTP,
            // unimplemented) and `/4`-`/7` (FLDENV/FRSTOR/FNSAVE/FSTSW m16) stay rejected, same
            // as the register-row absence pattern above documents for 0xDC.
            | (0xdd, 0..=2, 0 | 2 | 3, 0..=7)
            // 0xDC memory forms: f64 arithmetic, all 8 sub-opcodes (`BinaryMemoryF64`).
            | (0xdc, 0..=2, 0..=7, 0..=7)
            // 0xDC mod=3, the ST(i)-destination binaries. The ABSENCE of 2 and 3 from this
            // pattern is the negative assertion that FCOM/FCOMP with an ST(i) destination stay
            // on the interpreter, where they raise #UD.
            | (0xdc, 3, 0 | 1 | 4..=7, 0..=7)
            // 0xDD mod=3 /4 and /5: FUCOM/FUCOMP ST(i). `/0` (FFREE) stays rejected -- it writes
            // the tag word to Empty without touching a value, which `emit_store_physical` has no
            // shape for -- and so do `/6`/`/7`, which are undefined.
            | (0xdd, 3, 2..=5, 0..=7)
            | (0xde, 3, 0 | 1 | 4 | 5 | 6 | 7, 0..=7)
            | (0xde, 3, 3, 1)
            | (0xdf, 3, 4, 0)
            // 0xDF memory /5 and /7: FILD m64 and FISTP m64. `/4` and `/6` (FBLD/FBSTP,
            // unimplemented) stay rejected, and `/0`-`/3` are the m16 integer forms, which are
            // separately out of scope.
            | (0xdf, 0..=2, 5 | 7, 0..=7)
    )
}

#[test]
fn classifier_selects_exact_traced_slice() {
    let mut accepted = 0;
    for opcode in 0xd8..=0xdf {
        for mode in 0..=3 {
            for reg in 0..=7 {
                for rm in 0..=7 {
                    let classified = NativeX87Insn::classify(&insn(opcode, mode, reg, rm));
                    let expected = expected_selected(opcode, mode, reg, rm);
                    assert_eq!(
                        classified.is_some(),
                        expected,
                        "opcode={opcode:02x} mode={mode} reg={reg} rm={rm}"
                    );
                    accepted += usize::from(classified.is_some());
                }
            }
        }
    }
    // 461 before the control-word pair, 509 after it. 0xDC mod=3 adds six sub-opcodes across
    // eight rm values: 6 * 8 = 48, landing at 557. 0xDA memory forms add 3 modes x 8 sub-opcodes
    // x 8 rm values = 192, landing at 749. 0xDD memory (FLD/FST/FSTP m64) adds 3 modes x 3
    // sub-opcodes x 8 rm values = 72, landing at 821. 0xDC memory (f64 arithmetic) adds 3 modes
    // x 8 sub-opcodes x 8 rm values = 192, landing at 1013. 0xDF /5 memory (FILD m64) adds 3
    // modes x 1 sub-opcode x 8 rm values = 24, landing at 1037. 0xDD mod=3 /4 and /5
    // (FUCOM/FUCOMP) add 2 sub-opcodes x 8 rm values = 16, landing at 1053. 0xDB /2 memory
    // (FIST m32) adds 3 modes x 1 sub-opcode x 8 rm values = 24, landing at 1077. 0xD9 mod=3 /4
    // (FCHS/FABS/FTST/FXAM) adds 4 rm values, landing at 1081. FSQRT (0xD9 /7 rm=2) adds one,
    // landing at 1082. 0xDF /7 memory (FISTP m64) adds 3 modes x 8 rm values = 24, landing at
    // 1106. 0xDB /7 memory (FSTP m80) adds another 24, landing at 1130. FRNDINT (0xD9 /7 rm=4,
    // the FPU-loop slice) adds one, landing at 1131.
    //
    // WAIT (0x9B) is deliberately NOT in this total: it is not an escape opcode and the sweep
    // above runs 0xD8..=0xDF only. `classifier_rejects_bad_prefixes_and_malformed_decodes` is
    // where its ModRM-free shape is pinned.
    assert_eq!(accepted, 1131);
}

#[test]
fn classifier_preserves_operations_indices_and_addresses() {
    for extension in 0..=7 {
        let expected = NativeX87BinaryOp::from_extension(extension).unwrap();
        assert_eq!(
            NativeX87Insn::classify(&insn(0xd8, 0, extension, 5)),
            Some(NativeX87Insn::BinaryMemory {
                op: expected,
                addr: addr(),
            })
        );
        assert_eq!(
            NativeX87Insn::classify(&insn(0xd8, 3, extension, 5)),
            Some(NativeX87Insn::BinaryRegister {
                op: expected,
                index: 5,
            })
        );
        // 0xDA memory forms: same op mapping as 0xD8, riding `NativeX87BinaryOp` unchanged.
        assert_eq!(
            NativeX87Insn::classify(&insn(0xda, 0, extension, 5)),
            Some(NativeX87Insn::IntBinaryMemory {
                op: expected,
                addr: addr(),
            })
        );
        // 0xDC memory forms: f64 arithmetic, same op mapping again.
        assert_eq!(
            NativeX87Insn::classify(&insn(0xdc, 0, extension, 5)),
            Some(NativeX87Insn::BinaryMemoryF64 {
                op: expected,
                addr: addr(),
            })
        );
    }
    // FLD/FST/FSTP m64. `/1` and `/4`-`/7` are NOT FLD/FST/FSTP and must stay unclassifiable;
    // `/0`, `/2` and `/3` must classify with the right variant, pop flag and address.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdd, 0, 0, 5)),
        Some(NativeX87Insn::LoadF64 { addr: addr() })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdd, 0, 2, 5)),
        Some(NativeX87Insn::StoreF64 {
            addr: addr(),
            pop: false,
        })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdd, 0, 3, 5)),
        Some(NativeX87Insn::StoreF64 {
            addr: addr(),
            pop: true,
        })
    );
    for extension in [1u8, 4, 5, 6, 7] {
        assert!(
            NativeX87Insn::classify(&insn(0xdd, 0, extension, 5)).is_none(),
            "0xdd /{extension} memory (FISTTP/FLDENV/FRSTOR/FNSAVE/FSTSW) must stay \
             unclassifiable"
        );
    }
    // FIST/FISTP m32. Only the pop separates /2 from /3, and it drives `top_delta`, so the pair
    // is pinned rather than one standing in for both. `/1`, `/4` and `/6` are undefined and
    // `/5`/`/7` (FLD/FSTP m80) are separately out of scope; all five must stay unclassifiable.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdb, 0, 2, 5)),
        Some(NativeX87Insn::StoreI32 {
            addr: addr(),
            pop: false,
        })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdb, 0, 3, 5)),
        Some(NativeX87Insn::StoreI32 {
            addr: addr(),
            pop: true,
        })
    );
    // FSTP m80, and the FLD m80 beside it that must NOT come with it: 0xDB /5 is the deferred
    // load direction, and admitting it by widening the /7 arm is the realistic mistake.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdb, 0, 7, 5)),
        Some(NativeX87Insn::StoreExtended80 { addr: addr() })
    );
    for extension in [1u8, 4, 5, 6] {
        assert!(
            NativeX87Insn::classify(&insn(0xdb, 0, extension, 5)).is_none(),
            "0xdb /{extension} memory must stay unclassifiable"
        );
    }
    // FILD m64 (0xDF /5). Slice 40 is FILD-only: `/7` (FISTP m64) MUST stay unclassifiable here,
    // a positive assertion of the scope cut taken in review, not merely an absence from
    // `expected_selected` above. `/4` and `/6` (FBLD/FBSTP) stay unclassifiable too, same as the
    // 0xDD memory loop above.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdf, 0, 5, 5)),
        Some(NativeX87Insn::LoadI64 { addr: addr() })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdf, 0, 7, 5)),
        Some(NativeX87Insn::StoreI64 { addr: addr() })
    );
    for extension in [4u8, 6] {
        assert!(
            NativeX87Insn::classify(&insn(0xdf, 0, extension, 5)).is_none(),
            "0xdf /{extension} memory (FBLD/FBSTP) must stay unclassifiable"
        );
    }
    // 0xDC mod=3 must still classify as `BinaryRegisterDest`, not `BinaryMemoryF64`: the
    // register-mode branch is disjoint from the memory-mode branch above, and this is the
    // regression pin that the memory arm did not shadow it.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdc, 3, 0, 3)),
        Some(NativeX87Insn::BinaryRegisterDest {
            op: NativeX87StiOp::Add,
            index: 3,
        })
    );
    // 0xDA mod=3 is FCMOVcc territory except for FUCOMPP at reg=5, rm=1 (pinned separately
    // below); every other register-mode encoding must stay unclassifiable rather than being
    // misread as IntBinaryMemory.
    for reg in 0..=7u8 {
        for rm in 0..=7u8 {
            if (reg, rm) == (5, 1) {
                continue;
            }
            assert!(
                NativeX87Insn::classify(&insn(0xda, 3, reg, rm)).is_none(),
                "0xda mod=3 reg={reg} rm={rm} (FCMOVcc) must stay unclassifiable"
            );
        }
    }
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 0, 7)),
        Some(NativeX87Insn::LoadRegister { index: 7 })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 1, 6)),
        Some(NativeX87Insn::Exchange { index: 6 })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdf, 3, 4, 0)),
        Some(NativeX87Insn::StoreStatusAx)
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdd, 3, 3, 5)),
        Some(NativeX87Insn::StoreRegister {
            index: 5,
            pop: true,
        })
    );
    // 0xD9 /4. FCHS and FABS share a variant and are separated only by `negate`, so both are
    // pinned; the four undefined rm values must stay unclassifiable rather than falling into
    // either of them.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 4, 0)),
        Some(NativeX87Insn::SignOp { negate: true })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 4, 1)),
        Some(NativeX87Insn::SignOp { negate: false })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 4, 4)),
        Some(NativeX87Insn::TestZero)
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 4, 5)),
        Some(NativeX87Insn::Examine)
    );
    for rm in [2u8, 3, 6, 7] {
        assert!(
            NativeX87Insn::classify(&insn(0xd9, 3, 4, rm)).is_none(),
            "0xd9 mod=3 /4 rm={rm} is undefined and must stay unclassifiable"
        );
    }
    // FSQRT and FRNDINT, and the six 0xD9 /7 encodings around them that must NOT come with them.
    // FPREM, FYL2XP1, FSINCOS, FSCALE, FSIN and FCOS are transcendentals or partial remainders
    // that the interpreter computes through Rust `f64` library calls, and admitting any of them
    // by widening the rm match is the realistic mistake this loop exists to catch.
    //
    // FRNDINT joined on the FPU-loop slice and is rounding-control dependent, which is exactly
    // what the earlier version of this comment gave as the reason it could not join: the answer
    // is the runtime four-way branch in `emit_native_x87`, not a baked mode.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 7, 2)),
        Some(NativeX87Insn::SquareRoot)
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 7, 4)),
        Some(NativeX87Insn::RoundToInt)
    );
    for rm in [0u8, 1, 3, 5, 6, 7] {
        assert!(
            NativeX87Insn::classify(&insn(0xd9, 3, 7, rm)).is_none(),
            "0xd9 mod=3 /7 rm={rm} must stay unclassifiable"
        );
    }
    // FUCOM/FUCOMP ST(i). The pop flag is the only thing separating /4 from /5, and it drives
    // `top_delta`, so both are pinned with their index. `/0` (FFREE) stays unclassifiable.
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdd, 3, 4, 6)),
        Some(NativeX87Insn::UnorderedCompare {
            index: 6,
            pop: false,
        })
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xdd, 3, 5, 6)),
        Some(NativeX87Insn::UnorderedCompare {
            index: 6,
            pop: true,
        })
    );
    for extension in [0u8, 6, 7] {
        assert!(
            NativeX87Insn::classify(&insn(0xdd, 3, extension, 6)).is_none(),
            "0xdd mod=3 /{extension} (FFREE / undefined) must stay unclassifiable"
        );
    }
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 5, 0)),
        Some(NativeX87Insn::LoadOne)
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xd9, 3, 5, 6)),
        Some(NativeX87Insn::LoadZero)
    );
    assert_eq!(
        NativeX87Insn::classify(&insn(0xde, 3, 6, 2)),
        Some(NativeX87Insn::PopBinary {
            op: NativeX87StiOp::DivideReverse,
            index: 2,
        })
    );
    // 0xDC and 0xDE mod=3 share one classifier and one swap. Pinning the (op, index) PAIR for
    // every sub-opcode on both encodings, because the exhaustive cube test below only checks
    // `is_some()` and would survive a reg/rm transposition or a mis-transcribed op.
    //
    // The swap is the whole point: Intel's /4 is FSUBR, /5 is FSUB, /6 is FDIVR and /7 is FDIV
    // when the destination is ST(i), which is the reverse of the D8 forms above. FADD and FMUL
    // are commutative, so 0 and 1 cannot detect a swap at all and are pinned here instead.
    for (extension, expected) in [
        (0u8, NativeX87StiOp::Add),
        (1, NativeX87StiOp::Multiply),
        (4, NativeX87StiOp::SubtractReverse),
        (5, NativeX87StiOp::Subtract),
        (6, NativeX87StiOp::DivideReverse),
        (7, NativeX87StiOp::Divide),
    ] {
        assert_eq!(
            NativeX87Insn::classify(&insn(0xdc, 3, extension, 3)),
            Some(NativeX87Insn::BinaryRegisterDest {
                op: expected,
                index: 3,
            }),
            "dc /{extension}"
        );
        assert_eq!(
            NativeX87Insn::classify(&insn(0xde, 3, extension, 3)),
            Some(NativeX87Insn::PopBinary {
                op: expected,
                index: 3,
            }),
            "de /{extension}"
        );
    }
    // Sub-opcodes 2 and 3 are FCOM/FCOMP with an ST(i) destination, which `fpu_reg_arith_sti`
    // answers with `fpu_unsupported`, i.e. #UD. They are not merely unmatched here, they are
    // unrepresentable in `NativeX87StiOp`.
    for extension in [2u8, 3] {
        assert!(NativeX87Insn::classify(&insn(0xdc, 3, extension, 3)).is_none());
        assert!(NativeX87Insn::classify(&insn(0xde, 3, extension, 3)).is_none());
    }
    for opcode in [0xda, 0xde] {
        let (reg, rm) = if opcode == 0xda { (5, 1) } else { (3, 1) };
        assert_eq!(
            NativeX87Insn::classify(&insn(opcode, 3, reg, rm)),
            Some(NativeX87Insn::ComparePopPop)
        );
    }
}

#[test]
fn classifier_rejects_bad_prefixes_and_malformed_decodes() {
    let mut candidate = insn(0xd8, 0, 0, 0);
    candidate.prefixes.lock = true;
    assert!(NativeX87Insn::classify(&candidate).is_none());

    candidate = insn(0xd8, 0, 0, 0);
    candidate.prefixes.rep = Some(RepKind::Repe);
    assert!(NativeX87Insn::classify(&candidate).is_none());

    candidate = insn(0xd8, 0, 0, 0);
    candidate.prefixes.operand_size_override = true;
    candidate.prefixes.address_size_override = true;
    candidate.prefixes.segment_override = Some(SegmentIndex::Fs);
    assert!(NativeX87Insn::classify(&candidate).is_some());

    candidate = insn(0xd8, 0, 0, 0);
    candidate.operand = None;
    assert!(NativeX87Insn::classify(&candidate).is_none());
    candidate.operand = Some(DecodedOperand::Reg(0));
    assert!(NativeX87Insn::classify(&candidate).is_none());

    candidate = insn(0xd8, 3, 0, 0);
    candidate.operand = Some(DecodedOperand::Mem(addr()));
    assert!(NativeX87Insn::classify(&candidate).is_none());

    for bad in [
        crate::ModRm {
            mode: 4,
            reg: 0,
            rm: 0,
        },
        crate::ModRm {
            mode: 0,
            reg: 8,
            rm: 0,
        },
        crate::ModRm {
            mode: 0,
            reg: 0,
            rm: 8,
        },
    ] {
        candidate = insn(0xd8, 0, 0, 0);
        candidate.modrm = Some(bad);
        assert!(NativeX87Insn::classify(&candidate).is_none());
    }

    candidate = insn(0xd8, 0, 0, 0);
    candidate.group = DecodeGroup::Alu;
    assert!(NativeX87Insn::classify(&candidate).is_none());
    candidate = insn(0xd8, 0, 0, 0);
    candidate.imm = 1;
    assert!(NativeX87Insn::classify(&candidate).is_none());
    candidate = insn(0xd8, 0, 0, 0);
    candidate.modrm = None;
    assert!(NativeX87Insn::classify(&candidate).is_none());
    // WAIT/FWAIT. `decode`'s FPU arm fetches a ModRM for every opcode EXCEPT 0x9b, so the ONLY
    // shape that classifies is the one with neither a ModRM nor an operand -- and it classifies
    // as `Wait` since the FPU-loop slice, where it previously fell through the `insn.modrm?`
    // below it. Both halves are pinned: a 0x9b that arrives carrying either field is a decode
    // that did not happen, and admitting it would emit a slot whose `metadata().memory` says None
    // while an operand exists.
    candidate = insn(0x9b, 3, 0, 0);
    candidate.modrm = None;
    assert_eq!(
        NativeX87Insn::classify(&candidate),
        Some(NativeX87Insn::Wait)
    );
    candidate = insn(0x9b, 3, 0, 0);
    assert!(NativeX87Insn::classify(&candidate).is_none());
    candidate = insn(0x9b, 0, 0, 5);
    candidate.modrm = None;
    assert!(NativeX87Insn::classify(&candidate).is_none());
    candidate = insn(0x1d8, 0, 0, 0);
    assert!(NativeX87Insn::classify(&candidate).is_none());
}

#[test]
fn classifier_preserves_16_bit_memory_addressing_for_the_emitter() {
    let mut candidate = insn(0xd9, 0, 0, 6);
    candidate.prefixes.address_size_override = true;
    candidate.address_size = AddressSize::Word;
    let mut word_addr = addr();
    word_addr.address_size = AddressSize::Word;
    candidate.operand = Some(DecodedOperand::Mem(word_addr));

    // Classification is architectural. A backend without 16-bit EA lowering can reject it.
    let classified = NativeX87Insn::classify(&candidate).unwrap();
    assert_eq!(classified, NativeX87Insn::LoadF32 { addr: word_addr });
    assert_eq!(classified.metadata().memory.unwrap().width, 4);
}

#[test]
fn stack_effects_advance_every_top_with_wraparound() {
    let push = NativeX87Insn::LoadOne;
    let pop = NativeX87Insn::PopBinary {
        op: NativeX87StiOp::Add,
        index: 1,
    };
    let pop_twice = NativeX87Insn::ComparePopPop;

    for top in 0..8 {
        assert_eq!(push.advance_top(top), top.wrapping_add(7) & 7);
        assert_eq!(pop.advance_top(top), top.wrapping_add(1) & 7);
        assert_eq!(pop_twice.advance_top(top), top.wrapping_add(2) & 7);
    }
}

#[test]
fn metadata_matches_interpreter_timing_and_memory_effects() {
    let cases = [
        (
            insn(0xd8, 0, 3, 0),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 4,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xd8, 3, 1, 2),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        // 0xDA memory forms: integer m32 arithmetic. `IntConvert16` (256 at I586), not
        // `BinaryMemory`'s `F32Mem` (8), which is the whole point of giving this shape its own
        // arm rather than joining `BinaryMemory`. FADD is the non-popping representative.
        (
            insn(0xda, 0, 0, 0),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 4,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // FICOMP, the popping representative: `pops` must read true here for the same reason
        // `top_delta` must join the `BinaryMemory | BinaryRegister` group for this shape.
        (
            insn(0xda, 0, 3, 0),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 4,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 0, 0, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 4,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 0, 2, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 4,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 0, 3, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 4,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 3, 1, 1),
            NativeX87Metadata {
                raw_clocks: 4,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        // FLDCW. Four clocks, not the fourteen every other 0xd9 memory form charges
        // (fpu_exec.rs, `execute_fpu_memory`, the 0xd9 reg 5 arm is `Ok(clocks(4))`), and the
        // first width-2 access in the table.
        (
            insn(0xd9, 0, 5, 0),
            NativeX87Metadata {
                raw_clocks: 4,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 2,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // FNSTCW, `Ok(clocks(14))` on the same arm.
        (
            insn(0xd9, 0, 7, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 2,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xdb, 0, 0, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 4,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xdb, 0, 3, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 4,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xde, 3, 0, 1),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xdd, 3, 3, 1),
            NativeX87Metadata {
                raw_clocks: 3,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 3, 5, 0),
            NativeX87Metadata {
                raw_clocks: 4,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        // The ST(i)-destination pair. Same twenty clocks and same Register class on both, from
        // the single `Ok(clocks(20))` in `fpu_reg_arith_sti`. `pops` is the only differing
        // field and this test is the ONLY reader of it anywhere in the crate, which is why the
        // case is here rather than resting on a runtime fixture.
        (
            insn(0xdc, 3, 7, 1),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xde, 3, 7, 1),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xde, 3, 3, 1),
            NativeX87Metadata {
                raw_clocks: 5,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xdf, 3, 4, 0),
            NativeX87Metadata {
                raw_clocks: 3,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        // FLD m64: `F64Mem`, not `F32Mem`, and an 8-byte read. No conversion, so `pops` is
        // false the same as `LoadF32`.
        (
            insn(0xdd, 0, 0, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 8,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // FST m64: an 8-byte write, no pop.
        (
            insn(0xdd, 0, 2, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 8,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // FSTP m64: same shape as FST but popping.
        (
            insn(0xdd, 0, 3, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 8,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        // FADD m64: the non-popping 0xDC memory representative. `F64Mem` at raw 20, matching
        // the interpreter's `fpu_mem_arith` clocks(20).
        (
            insn(0xdc, 0, 0, 0),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 8,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // FCOMP m64: the popping 0xDC memory representative, mirroring the 0xDA FICOMP row
        // above (`pops` must read true here, same reason `top_delta` must join the
        // `BinaryMemory | IntBinaryMemory | BinaryRegister` group for this shape).
        (
            insn(0xdc, 0, 3, 0),
            NativeX87Metadata {
                raw_clocks: 20,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 8,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        // FILD m64: `IntConvert16`, NOT `IntConvert32` (B4's realistic copy-paste mistake), and
        // an 8-byte read. No pop, joining the push group the same as `LoadI32`.
        (
            insn(0xdf, 0, 5, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Read,
                    width: 8,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // The 0xD9 /4 group's three clock figures. FCHS/FABS at 6, FTST at 4 and FXAM at 8 are
        // the whole reason these are three metadata arms rather than one; folding any two
        // together is invisible in every value and shows only here and in `run timing differs`.
        (
            insn(0xd9, 3, 4, 0),
            NativeX87Metadata {
                raw_clocks: 6,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 3, 4, 4),
            NativeX87Metadata {
                raw_clocks: 4,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 3, 4, 5),
            NativeX87Metadata {
                raw_clocks: 8,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        // FSQRT: `clocks(70)`, the largest raw figure in the table by a factor of three. It is
        // pinned because it is also the shape most likely to be "rounded" toward its neighbours,
        // and because the `MAX_X87_BLOCK_CORE_CLOCKS` derivation below reads it.
        (
            insn(0xd9, 3, 7, 2),
            NativeX87Metadata {
                raw_clocks: 70,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        // FIST m32, the NON-popping half of the 0xDB /2-/3 pair. `pops` false against FISTP's
        // true is the only field that moves, and `IntConvert32` (272 at I586) must not drift to
        // the `IntConvert16` its 0xDF sibling uses -- `execute_fpu` derives the class from the
        // opcode byte, and 0xdb is the one escape that maps to IntConvert32.
        (
            insn(0xdb, 0, 2, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 4,
                }),
                pops: false,
                terminates_block: false,
            },
        ),
        // FSTP m80: the largest row in the reject census, and the one whose class is easiest to
        // get wrong. `IntConvert32` (272 at I586) because the class comes from the opcode byte
        // and 0xdb maps there; `F64Mem` (8) is what "it stores a float" would suggest and would
        // undercharge it 34-fold. The access is ten bytes wide and a WRITE.
        (
            insn(0xdb, 0, 7, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 10,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        // FISTP m64: `IntConvert16` like its FILD sibling and UNLIKE `StoreI32`'s
        // `IntConvert32`. The class comes from the opcode byte, so the two integer STORES
        // disagree while the two 0xDF forms agree -- the opposite of what grouping by operation
        // would suggest, and the reason this row sits next to the FIST m32 one.
        (
            insn(0xdf, 0, 7, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                memory: Some(NativeX87MemoryAccess {
                    direction: NativeX87MemoryDirection::Write,
                    width: 8,
                }),
                pops: true,
                terminates_block: false,
            },
        ),
        // FUCOM/FUCOMP ST(i): `clocks(4)`, not the 20 the ordered register compare
        // (`BinaryRegister`, pinned two rows from the top of this list) charges. Both rows are
        // register-form compares and only the concrete number separates them, which is why the
        // popping and non-popping variants are both here rather than one standing in for the pair.
        (
            insn(0xdd, 3, 4, 2),
            NativeX87Metadata {
                raw_clocks: 4,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xdd, 3, 5, 2),
            NativeX87Metadata {
                raw_clocks: 4,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
    ];
    for (insn, expected) in cases {
        assert_eq!(NativeX87Insn::classify(&insn).unwrap().metadata(), expected);
    }
}

#[test]
fn real_divide_forms_route_to_their_distinct_classes() {
    let cases = [
        (insn(0xd8, 0, 6, 1), TimingClass::X87MemDiv32),
        (insn(0xd8, 0, 7, 1), TimingClass::X87MemDiv32),
        (insn(0xdc, 0, 6, 1), TimingClass::X87MemDiv64),
        (insn(0xdc, 0, 7, 1), TimingClass::X87MemDiv64),
        (insn(0xd8, 3, 6, 1), TimingClass::X87RegDiv),
        (insn(0xd8, 3, 7, 1), TimingClass::X87RegDiv),
        (insn(0xdc, 3, 6, 1), TimingClass::X87RegDiv),
        (insn(0xdc, 3, 7, 1), TimingClass::X87RegDiv),
        (insn(0xde, 3, 6, 1), TimingClass::X87RegDiv),
        (insn(0xde, 3, 7, 1), TimingClass::X87RegDiv),
    ];
    for (decoded, class) in cases {
        let native = NativeX87Insn::classify(&decoded).expect("divide form lowers");
        assert_eq!(native.timing_class(), class, "{decoded:?}");
    }

    for decoded in [
        insn(0xd8, 0, 0, 1),
        insn(0xdc, 0, 1, 1),
        insn(0xd8, 3, 0, 1),
        insn(0xdc, 3, 1, 1),
        insn(0xde, 3, 0, 1),
    ] {
        let native = NativeX87Insn::classify(&decoded).expect("nondivide form lowers");
        assert_ne!(native.timing_class(), TimingClass::X87RegDiv, "{decoded:?}");
        assert_ne!(
            native.timing_class(),
            TimingClass::X87MemDiv32,
            "{decoded:?}"
        );
        assert_ne!(
            native.timing_class(),
            TimingClass::X87MemDiv64,
            "{decoded:?}"
        );
    }
}

/// Neither control-word form moves TOP, pushes or pops. That is what lets the pair join an
/// otherwise integer block, and it is also why such a block ends up TOP-pinned for no
/// architectural reason (`jit_direct_reject_x87_top`).
/// THE `top_delta` PIN, and it exists because nothing else catches the field cheaply.
///
/// `top_delta` is 0 here where the popping sibling `PopBinary` is 1. A wrong value has two
/// separate consequences and only one of them is loud: every LATER x87 slot in the block
/// addresses the wrong physical XMM (a hard state divergence the value battery catches), and
/// `x87_exit_top` is poisoned so `link_compatible` silently refuses an edge it should accept
/// (no test failure at all, just lost performance). `metadata().pops` is NOT a cross-check on
/// this: that field has no readers anywhere in the crate.
///
/// Exhaustive over both the sub-opcode and the register index, which the fixture-based catcher
/// cannot be.
#[test]
fn sti_destination_binaries_leave_the_stack_position_alone() {
    for extension in [0u8, 1, 4, 5, 6, 7] {
        for rm in 0..=7 {
            let classified = NativeX87Insn::classify(&insn(0xdc, 3, extension, rm)).unwrap();
            assert_eq!(classified.top_delta(), 0, "dc /{extension} st({rm})");
            for top in 0..8 {
                assert_eq!(classified.advance_top(top), top, "dc /{extension} st({rm})");
            }
            // The popping sibling must still move TOP by one, so this pins the pair rather than
            // just the new arm: a refactor that made them agree would be caught here.
            let popping = NativeX87Insn::classify(&insn(0xde, 3, extension, rm)).unwrap();
            assert_eq!(popping.top_delta(), 1, "de /{extension} st({rm})");
        }
    }
}

#[test]
fn control_word_forms_leave_the_stack_position_alone() {
    for candidate in [insn(0xd9, 0, 5, 0), insn(0xd9, 0, 7, 0)] {
        let classified = NativeX87Insn::classify(&candidate).unwrap();
        assert_eq!(classified.top_delta(), 0);
        for top in 0..8 {
            assert_eq!(classified.advance_top(top), top);
        }
    }
}

#[test]
fn native_gate_has_architectural_priority() {
    assert_eq!(
        native_x87_gate(CpuPersona::I386, CR0_NE, 0, 1),
        NativeX87Gate::DeviceNotAvailable
    );
    assert_eq!(
        native_x87_gate(CpuPersona::I586, CR0_EM | CR0_NE, 0, 1),
        NativeX87Gate::DeviceNotAvailable
    );
    assert_eq!(
        native_x87_gate(CpuPersona::I586, CR0_TS, 0, 0),
        NativeX87Gate::DeviceNotAvailable
    );
    assert_eq!(
        native_x87_gate(CpuPersona::I586, CR0_NE, 0x003e, 0x0001),
        NativeX87Gate::PendingException
    );
    assert_eq!(
        native_x87_gate(CpuPersona::I586, CR0_NE, 0x003f, 0x0001),
        NativeX87Gate::Ready
    );
    assert_eq!(
        native_x87_gate(CpuPersona::I586, 0, 0, 1),
        NativeX87Gate::Ready
    );
}

#[test]
fn top_and_tag_helpers_cover_wraparound_and_value_classes() {
    let status = x87_with_top(0x4001, 7);
    assert_eq!(x87_top(status), 7);
    assert_eq!(x87_physical_index(status, 2), 1);
    assert_eq!(x87_top(x87_push_top(status)), 6);
    assert_eq!(x87_top(x87_pop_top(status)), 0);
    assert_eq!(status & !X87_TOP_MASK, 0x4001 & !X87_TOP_MASK);

    let mut tags = 0xffff;
    tags = x87_with_tag(tags, 7, NativeX87Tag::Valid);
    tags = x87_with_tag(tags, 0, NativeX87Tag::Zero);
    tags = x87_with_tag(tags, 1, NativeX87Tag::Special);
    assert_eq!(x87_tag_at(tags, 7), NativeX87Tag::Valid);
    assert_eq!(x87_tag_at(tags, 0), NativeX87Tag::Zero);
    assert_eq!(x87_tag_at(tags, 1), NativeX87Tag::Special);
    assert_eq!(x87_tag_at(tags, 2), NativeX87Tag::Empty);

    assert_eq!(x87_value_tag(1.0), NativeX87Tag::Valid);
    assert_eq!(x87_value_tag(f64::MIN_POSITIVE / 2.0), NativeX87Tag::Valid);
    assert_eq!(x87_value_tag(0.0), NativeX87Tag::Zero);
    assert_eq!(x87_value_tag(-0.0), NativeX87Tag::Zero);
    assert_eq!(x87_value_tag(f64::INFINITY), NativeX87Tag::Special);
    assert_eq!(x87_value_tag(f64::NAN), NativeX87Tag::Special);
}

#[test]
fn finite_and_rounding_eligibility_match_the_fast_slice() {
    assert!(native_x87_compare_eligible(1.0, -2.0));
    assert!(!native_x87_compare_eligible(f64::NAN, 1.0));
    assert!(native_x87_binary_result_eligible(
        NativeX87BinaryOp::Multiply,
        2.0,
        3.0,
        6.0
    ));
    assert!(!native_x87_binary_result_eligible(
        NativeX87BinaryOp::Divide,
        1.0,
        0.0,
        f64::INFINITY
    ));
    assert!(!native_x87_binary_result_eligible(
        NativeX87BinaryOp::Add,
        f64::MAX,
        f64::MAX,
        f64::INFINITY
    ));

    assert_eq!(
        NativeX87RoundingMode::from_control(0x037f),
        NativeX87RoundingMode::NearestEven
    );
    assert_eq!(
        NativeX87RoundingMode::from_control(0x0f7f),
        NativeX87RoundingMode::Truncate
    );
    assert_eq!(native_x87_i32_result(0x037f, 2.5), Some(2));
    assert_eq!(native_x87_i32_result(0x037f, 3.5), Some(4));
    assert_eq!(native_x87_i32_result(0x0f7f, -3.9), Some(-3));
    assert_eq!(native_x87_i32_result(0x077f, 1.0), None);
    assert_eq!(native_x87_i32_result(0x0b7f, 1.0), None);
    assert_eq!(native_x87_i32_result(0x037f, f64::NAN), None);
    assert_eq!(native_x87_i32_result(0x037f, 2_147_483_648.0), None);
    assert_eq!(
        native_x87_i32_result(0x0f7f, -2_147_483_648.9),
        Some(i32::MIN)
    );
}

#[test]
fn weighted_timing_batches_exactly() {
    let sequence = [
        NativeX87Insn::classify(&insn(0xd8, 3, 1, 1)).unwrap(),
        NativeX87Insn::classify(&insn(0xd9, 0, 0, 1)).unwrap(),
        NativeX87Insn::classify(&insn(0xdb, 0, 0, 1)).unwrap(),
        NativeX87Insn::classify(&insn(0xdf, 3, 4, 0)).unwrap(),
    ];
    for persona in [CpuPersona::I486, CpuPersona::I586] {
        let table = class_table(persona);
        let weighted = sequence
            .iter()
            .map(|op| op.metadata().weighted_fp_clocks(op.timing_class(), table))
            .sum::<u64>();
        let aggregate = scale_weighted_fp_clocks(weighted, 3);

        let mut individual_clocks = 0;
        let mut remainder = 3;
        for op in sequence {
            let step = scale_weighted_fp_clocks(
                op.metadata().weighted_fp_clocks(op.timing_class(), table),
                remainder,
            );
            individual_clocks += step.clocks;
            remainder = step.remainder;
        }
        assert_eq!(aggregate.clocks, individual_clocks);
        assert_eq!(aggregate.remainder, remainder);
    }

    assert_eq!(
        repeated_weighted_fp_clocks(3_808, 4_096, 160),
        Some(15_597_728)
    );
    assert_eq!(repeated_weighted_fp_clocks(u64::MAX, 2, 0), None);
}

#[test]
fn substrate_layout_stays_compact() {
    assert!(core::mem::size_of::<NativeX87Insn>() <= 24);
    assert!(core::mem::size_of::<NativeX87Metadata>() <= 12);

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        let layout = native_x87_layout();
        let state_size = core::mem::size_of::<X87>();
        assert_eq!(layout.st_stride, core::mem::size_of::<f64>());
        assert!(layout.st + 8 * layout.st_stride <= state_size);
        assert!(layout.control + core::mem::size_of::<u16>() <= state_size);
        assert!(layout.status + core::mem::size_of::<u16>() <= state_size);
        assert!(layout.tag + core::mem::size_of::<u16>() <= state_size);
        assert_ne!(layout.control, layout.status);
        assert_ne!(layout.status, layout.tag);
    }
}

/// Every distinct `NativeX87Insn` shape, for the clock-bound derivation below.
///
/// The `match` beneath it is what keeps this honest: it has no wildcard arm, so a new variant is
/// a compile error here, and whoever fixes that error is standing in the right file to add the
/// shape to this list. That is the same enforcement the emit dispatch relies on.
fn every_x87_shape() -> Vec<NativeX87Insn> {
    let mut shapes = vec![
        NativeX87Insn::LoadOne,
        NativeX87Insn::LoadZero,
        NativeX87Insn::ComparePopPop,
        NativeX87Insn::StoreStatusAx,
        NativeX87Insn::SignOp { negate: true },
        NativeX87Insn::SignOp { negate: false },
        NativeX87Insn::TestZero,
        NativeX87Insn::Examine,
        NativeX87Insn::SquareRoot,
        NativeX87Insn::Wait,
        NativeX87Insn::RoundToInt,
        NativeX87Insn::LoadF32 { addr: addr() },
        NativeX87Insn::LoadI32 { addr: addr() },
        NativeX87Insn::LoadControlWord { addr: addr() },
        NativeX87Insn::StoreControlWord { addr: addr() },
        NativeX87Insn::LoadRegister { index: 3 },
        NativeX87Insn::Exchange { index: 3 },
    ];
    shapes.push(NativeX87Insn::LoadF64 { addr: addr() });
    shapes.push(NativeX87Insn::LoadI64 { addr: addr() });
    shapes.push(NativeX87Insn::StoreI64 { addr: addr() });
    shapes.push(NativeX87Insn::StoreExtended80 { addr: addr() });
    for pop in [false, true] {
        shapes.push(NativeX87Insn::StoreF32 { addr: addr(), pop });
        shapes.push(NativeX87Insn::StoreRegister { index: 3, pop });
        shapes.push(NativeX87Insn::StoreF64 { addr: addr(), pop });
        shapes.push(NativeX87Insn::UnorderedCompare { index: 3, pop });
        shapes.push(NativeX87Insn::StoreI32 { addr: addr(), pop });
    }
    for extension in 0..=7 {
        let op = NativeX87BinaryOp::from_extension(extension).expect("binary op");
        shapes.push(NativeX87Insn::BinaryMemory { op, addr: addr() });
        shapes.push(NativeX87Insn::IntBinaryMemory { op, addr: addr() });
        shapes.push(NativeX87Insn::BinaryRegister { op, index: 3 });
        shapes.push(NativeX87Insn::BinaryMemoryF64 { op, addr: addr() });
    }
    for op in [
        NativeX87StiOp::Add,
        NativeX87StiOp::Multiply,
        NativeX87StiOp::Subtract,
        NativeX87StiOp::SubtractReverse,
        NativeX87StiOp::Divide,
        NativeX87StiOp::DivideReverse,
    ] {
        shapes.push(NativeX87Insn::PopBinary { op, index: 3 });
        shapes.push(NativeX87Insn::BinaryRegisterDest { op, index: 3 });
    }
    shapes
}

/// The registration gate for `every_x87_shape`. No wildcard arm, deliberately.
fn shape_is_enumerated(insn: NativeX87Insn) -> bool {
    match insn {
        NativeX87Insn::BinaryMemory { .. }
        | NativeX87Insn::IntBinaryMemory { .. }
        | NativeX87Insn::BinaryRegister { .. }
        | NativeX87Insn::LoadF32 { .. }
        | NativeX87Insn::StoreF32 { .. }
        | NativeX87Insn::LoadRegister { .. }
        | NativeX87Insn::Exchange { .. }
        | NativeX87Insn::StoreRegister { .. }
        | NativeX87Insn::LoadOne
        | NativeX87Insn::LoadZero
        | NativeX87Insn::LoadI32 { .. }
        | NativeX87Insn::StoreI32 { .. }
        | NativeX87Insn::PopBinary { .. }
        | NativeX87Insn::BinaryRegisterDest { .. }
        | NativeX87Insn::ComparePopPop
        | NativeX87Insn::StoreStatusAx
        | NativeX87Insn::LoadControlWord { .. }
        | NativeX87Insn::StoreControlWord { .. }
        | NativeX87Insn::LoadF64 { .. }
        | NativeX87Insn::StoreF64 { .. }
        | NativeX87Insn::BinaryMemoryF64 { .. }
        | NativeX87Insn::LoadI64 { .. }
        | NativeX87Insn::StoreI64 { .. }
        | NativeX87Insn::StoreExtended80 { .. }
        | NativeX87Insn::UnorderedCompare { .. }
        | NativeX87Insn::SignOp { .. }
        | NativeX87Insn::TestZero
        | NativeX87Insn::Examine
        | NativeX87Insn::SquareRoot
        | NativeX87Insn::Wait
        | NativeX87Insn::RoundToInt => true,
    }
}

/// `MAX_X87_BLOCK_CORE_CLOCKS` must DOMINATE the worst block the compiler can actually build,
/// derived from the metadata table rather than restated as a literal.
///
/// It is the per-hop cost bound for a chain of x87 blocks (`compute_global_block_upper`), and
/// there is no runtime clock check inside a chain: this static bound is the only thing keeping up
/// to `MAX_CHAIN_BLOCKS` hops inside a device deadline.
///
/// The bound is sized for `FpOpClass::IntConvert16` (raw 20, I586 scale 256, 640 core clocks per
/// slot, eight slots plus the raw allowance = 5,240 exactly). It was RAISED AHEAD of that class's
/// first member (0xDA m32int) by an owner ruling so that slice would measure its own effect
/// cleanly, and the headroom is now consumed: `IntBinaryMemory` is that first member, the
/// derivation below lands on exactly 5,240, and the equality assertion pins the bound tight. Any
/// future costlier shape must raise `MAX_X87_BLOCK_CORE_CLOCKS` in step with adding itself here,
/// or this test fails instead of shipping a silent under-estimate of the chain budget.
#[test]
fn max_x87_block_core_clocks_dominates_every_shape_in_the_metadata_table() {
    let shapes = every_x87_shape();
    assert!(shapes.iter().copied().all(shape_is_enumerated));

    // The bound must cover every native x87 shape in both fast persona tables.
    {
        let bound = max_x87_block_core_clocks();
        // Ceil per slot, then sum, which is at least ceil(sum / den) and is how the bound was
        // built.
        let worst_fp_slot = shapes
            .iter()
            .flat_map(|insn| {
                [CpuPersona::I486, CpuPersona::I586].map(|persona| {
                    insn.metadata()
                        .weighted_fp_clocks(insn.timing_class(), class_table(persona))
                        .div_ceil(u64::from(FP_TIMING_DEN))
                })
            })
            .max()
            .expect("at least one x87 shape");

        // `DirectKind::X87` charges 0 raw clocks, so the raw term comes from the NON-x87
        // instructions sharing the block. A block with any x87 slot holds at most
        // MAX_X87_BLOCK_INSTRUCTIONS of them, and no kind with a constant charge exceeds the 10 a
        // near RET costs.
        const WORST_CONSTANT_RAW_CLOCKS: u64 = 10;
        let derived = MAX_X87_SLOTS as u64 * worst_fp_slot
            + MAX_X87_BLOCK_INSTRUCTIONS as u64 * WORST_CONSTANT_RAW_CLOCKS;

        // The bound is TIGHT in both epochs, and the equality is what makes a mutation that
        // UNDERCHARGES a shape (a wrong class, `fp_class` or `raw_clocks`) fail here rather than
        // pass a `<=` trivially by falling back under the old worst case.
        assert_eq!(
            derived, bound,
            "the bound is {bound} but the metadata table derives {derived} \
             ({MAX_X87_SLOTS} slots x {worst_fp_slot} core clocks plus \
             {MAX_X87_BLOCK_INSTRUCTIONS} x {WORST_CONSTANT_RAW_CLOCKS} raw). Both sides of this \
             pin must move together: it feeds the chain quota, so widening it changes when \
             devices advance."
        );
    }
}

/// Every native x87 shape must route to its retained I386 literal.
#[test]
fn native_x87_base_literals_match_the_386_class_column() {
    for insn in every_x87_shape() {
        {
            let persona = CpuPersona::I386;
            assert_eq!(
                class_table(persona).raw(insn.timing_class()),
                u32::from(insn.metadata().raw_clocks),
                "{insn:?} on {persona:?} routes to {:?}, whose I386 column is not the shape's \
                 own literal",
                insn.timing_class()
            );
        }
    }
}

/// Native x87 settlement uses the class table without an extra multiplier.
#[test]
fn native_x87_charges_the_class_table_without_an_extra_multiplier() {
    let fistp = NativeX87Insn::classify(&insn(0xdb, 0, 3, 1)).expect("FISTP m32 lowers");
    assert_eq!(fistp.timing_class(), TimingClass::X87StoreInt32);

    // Epoch 2: Intel's 6 clocks (raw 72), with the dial at identity.
    assert_eq!(
        fistp
            .metadata()
            .weighted_fp_clocks(fistp.timing_class(), class_table(CpuPersona::I586)),
        72 * u64::from(FP_TIMING_DEN),
    );
}
