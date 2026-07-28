// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::jit::direct::{MAX_X87_BLOCK_CORE_CLOCKS, MAX_X87_BLOCK_INSTRUCTIONS, MAX_X87_SLOTS};
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
            | (0xda, 3, 5, 1)
            | (0xdb, 0..=2, 0 | 3, 0..=7)
            // 0xDC mod=3, the ST(i)-destination binaries. The ABSENCE of 2 and 3 from this
            // pattern is the negative assertion that FCOM/FCOMP with an ST(i) destination stay
            // on the interpreter, where they raise #UD.
            | (0xdc, 3, 0 | 1 | 4..=7, 0..=7)
            | (0xdd, 3, 2 | 3, 0..=7)
            | (0xde, 3, 0 | 1 | 4 | 5 | 6 | 7, 0..=7)
            | (0xde, 3, 3, 1)
            | (0xdf, 3, 4, 0)
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
    // eight rm values: 6 * 8 = 48.
    assert_eq!(accepted, 557);
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
    candidate = insn(0x9b, 3, 0, 0);
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
                fp_class: FpOpClass::F32Mem,
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
                fp_class: FpOpClass::Register,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 0, 0, 0),
            NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::F32Mem,
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
                fp_class: FpOpClass::F32Mem,
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
                fp_class: FpOpClass::F32Mem,
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
                fp_class: FpOpClass::Register,
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
                fp_class: FpOpClass::F32Mem,
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
                fp_class: FpOpClass::F32Mem,
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
                fp_class: FpOpClass::IntConvert32,
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
                fp_class: FpOpClass::IntConvert32,
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
                fp_class: FpOpClass::Register,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xdd, 3, 3, 1),
            NativeX87Metadata {
                raw_clocks: 3,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xd9, 3, 5, 0),
            NativeX87Metadata {
                raw_clocks: 4,
                fp_class: FpOpClass::Register,
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
                fp_class: FpOpClass::Register,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
        (
            insn(0xde, 3, 7, 1),
            NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xde, 3, 3, 1),
            NativeX87Metadata {
                raw_clocks: 5,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: true,
                terminates_block: false,
            },
        ),
        (
            insn(0xdf, 3, 4, 0),
            NativeX87Metadata {
                raw_clocks: 3,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        ),
    ];
    for (insn, expected) in cases {
        assert_eq!(NativeX87Insn::classify(&insn).unwrap().metadata(), expected);
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
        let weighted = sequence
            .iter()
            .map(|op| op.metadata().weighted_fp_clocks(persona))
            .sum::<u64>();
        let aggregate = scale_weighted_fp_clocks(weighted, 3);

        let mut individual_clocks = 0;
        let mut remainder = 3;
        for op in sequence {
            let step =
                scale_weighted_fp_clocks(op.metadata().weighted_fp_clocks(persona), remainder);
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
        NativeX87Insn::LoadF32 { addr: addr() },
        NativeX87Insn::LoadI32 { addr: addr() },
        NativeX87Insn::StoreI32 { addr: addr() },
        NativeX87Insn::LoadControlWord { addr: addr() },
        NativeX87Insn::StoreControlWord { addr: addr() },
        NativeX87Insn::LoadRegister { index: 3 },
        NativeX87Insn::Exchange { index: 3 },
    ];
    for pop in [false, true] {
        shapes.push(NativeX87Insn::StoreF32 { addr: addr(), pop });
        shapes.push(NativeX87Insn::StoreRegister { index: 3, pop });
    }
    for extension in 0..=7 {
        let op = NativeX87BinaryOp::from_extension(extension).expect("binary op");
        shapes.push(NativeX87Insn::BinaryMemory { op, addr: addr() });
        shapes.push(NativeX87Insn::BinaryRegister { op, index: 3 });
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
        | NativeX87Insn::StoreControlWord { .. } => true,
    }
}

/// `MAX_X87_BLOCK_CORE_CLOCKS` must DOMINATE the worst block the compiler can actually build,
/// derived from the metadata table rather than restated as a literal.
///
/// It is the per-hop cost bound for a chain of x87 blocks (`compute_global_block_upper`), and
/// there is no runtime clock check inside a chain: this static bound is the only thing keeping up
/// to `MAX_CHAIN_BLOCKS` hops inside a device deadline. It carried no derivation and no test, and
/// it is TIGHT rather than generous: today's worst slot reproduces it exactly.
///
/// That matters because the next opcode-board item, 0xDA m32int arithmetic, would be the first
/// member of `FpOpClass::IntConvert16`, whose I586 scale is 256 against IntConvert32's 272 but
/// whose raw clocks are 20 against 14. It costs 640 core clocks per slot against today's worst of
/// 476, and eight of them plus the raw allowance is 5,240 against a bound of 3,928. Without this
/// test that ships as a 33 percent under-estimate of the chain budget with nothing failing.
#[test]
fn max_x87_block_core_clocks_dominates_every_shape_in_the_metadata_table() {
    let shapes = every_x87_shape();
    assert!(shapes.iter().copied().all(shape_is_enumerated));

    // Ceil per slot, then sum, which is at least ceil(sum / den) and is how the bound was built.
    let worst_fp_slot = shapes
        .iter()
        .map(|insn| {
            insn.metadata()
                .weighted_fp_clocks(CpuPersona::I586)
                .div_ceil(u64::from(FP_TIMING_DEN))
        })
        .max()
        .expect("at least one x87 shape");

    // `DirectKind::X87` charges 0 raw clocks, so the raw term comes from the NON-x87 instructions
    // sharing the block. A block with any x87 slot holds at most MAX_X87_BLOCK_INSTRUCTIONS of
    // them, and no kind with a constant charge exceeds the 10 a near RET costs.
    const WORST_CONSTANT_RAW_CLOCKS: u64 = 10;
    let derived = MAX_X87_SLOTS as u64 * worst_fp_slot
        + MAX_X87_BLOCK_INSTRUCTIONS as u64 * WORST_CONSTANT_RAW_CLOCKS;

    assert!(
        derived <= MAX_X87_BLOCK_CORE_CLOCKS,
        "MAX_X87_BLOCK_CORE_CLOCKS is {MAX_X87_BLOCK_CORE_CLOCKS} but the metadata table now \
         allows a block costing {derived} ({MAX_X87_SLOTS} slots x {worst_fp_slot} core clocks \
         plus {MAX_X87_BLOCK_INSTRUCTIONS} x {WORST_CONSTANT_RAW_CLOCKS} raw). Raise the constant \
         and re-measure: it feeds the chain quota, so widening it changes when devices advance."
    );
}
