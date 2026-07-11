// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Pure classification and state helpers for the direct x64 x87 lowering.

use super::super::{
    AddrMode, CR0_EM, CR0_NE, CR0_TS, CpuPersona, DecodeGroup, DecodedInsn, DecodedOperand,
    FP_TIMING_DEN, FpOpClass, fp_timing_class,
};

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::super::{X87, fpu::NativeX87Layout};

pub(crate) const X87_TOP_SHIFT: u16 = 11;
pub(crate) const X87_TOP_MASK: u16 = 0x7 << X87_TOP_SHIFT;
pub(crate) const X87_CONDITION_MASK: u16 = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub(crate) enum NativeX87Tag {
    Valid = 0,
    Zero = 1,
    Special = 2,
    Empty = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87Gate {
    Ready,
    DeviceNotAvailable,
    PendingException,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87BinaryOp {
    Add,
    Multiply,
    Compare,
    ComparePop,
    Subtract,
    SubtractReverse,
    Divide,
    DivideReverse,
}

impl NativeX87BinaryOp {
    const fn from_extension(extension: u8) -> Option<Self> {
        Some(match extension {
            0 => Self::Add,
            1 => Self::Multiply,
            2 => Self::Compare,
            3 => Self::ComparePop,
            4 => Self::Subtract,
            5 => Self::SubtractReverse,
            6 => Self::Divide,
            7 => Self::DivideReverse,
            _ => return None,
        })
    }

    pub(crate) const fn pops(self) -> bool {
        matches!(self, Self::ComparePop)
    }

    pub(crate) const fn is_compare(self) -> bool {
        matches!(self, Self::Compare | Self::ComparePop)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87PopOp {
    Add,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87Insn {
    BinaryMemory {
        op: NativeX87BinaryOp,
        addr: AddrMode,
    },
    BinaryRegister {
        op: NativeX87BinaryOp,
        index: u8,
    },
    LoadF32 {
        addr: AddrMode,
    },
    StoreF32 {
        addr: AddrMode,
        pop: bool,
    },
    LoadRegister {
        index: u8,
    },
    Exchange {
        index: u8,
    },
    LoadI32 {
        addr: AddrMode,
    },
    StoreI32 {
        addr: AddrMode,
    },
    PopBinary {
        op: NativeX87PopOp,
        index: u8,
    },
    StoreStatusAx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87MemoryDirection {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeX87MemoryAccess {
    pub(crate) direction: NativeX87MemoryDirection,
    pub(crate) width: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeX87Metadata {
    pub(crate) raw_clocks: u8,
    pub(crate) fp_class: FpOpClass,
    pub(crate) memory: Option<NativeX87MemoryAccess>,
    pub(crate) pops: bool,
    pub(crate) terminates_block: bool,
}

impl NativeX87Metadata {
    pub(crate) const fn weighted_fp_clocks(self, persona: CpuPersona) -> u64 {
        self.raw_clocks as u64 * fp_timing_class(persona, self.fp_class) as u64
    }
}

impl NativeX87Insn {
    pub(crate) fn classify(insn: &DecodedInsn) -> Option<Self> {
        if insn.group != DecodeGroup::Fpu
            || insn.prefixes.lock
            || insn.prefixes.rep.is_some()
            || insn.imm != 0
            || insn.imm2 != 0
        {
            return None;
        }

        let opcode = u8::try_from(insn.opcode).ok()?;
        let modrm = insn.modrm?;
        if modrm.mode > 3 || modrm.reg > 7 || modrm.rm > 7 {
            return None;
        }

        if modrm.mode != 3 {
            let addr = match insn.operand {
                Some(DecodedOperand::Mem(addr)) => addr,
                _ => return None,
            };
            return match (opcode, modrm.reg) {
                (0xd8, extension) => Some(Self::BinaryMemory {
                    op: NativeX87BinaryOp::from_extension(extension)?,
                    addr,
                }),
                (0xd9, 0) => Some(Self::LoadF32 { addr }),
                (0xd9, 2) => Some(Self::StoreF32 { addr, pop: false }),
                (0xd9, 3) => Some(Self::StoreF32 { addr, pop: true }),
                (0xdb, 0) => Some(Self::LoadI32 { addr }),
                (0xdb, 3) => Some(Self::StoreI32 { addr }),
                _ => None,
            };
        }

        if insn.operand.is_some() {
            return None;
        }
        match (opcode, modrm.reg, modrm.rm) {
            (0xd8, extension, index) => Some(Self::BinaryRegister {
                op: NativeX87BinaryOp::from_extension(extension)?,
                index,
            }),
            (0xd9, 0, index) => Some(Self::LoadRegister { index }),
            (0xd9, 1, index) => Some(Self::Exchange { index }),
            (0xde, 0, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::Add,
                index,
            }),
            (0xde, 1, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::Multiply,
                index,
            }),
            (0xdf, 4, 0) => Some(Self::StoreStatusAx),
            _ => None,
        }
    }

    pub(crate) const fn metadata(self) -> NativeX87Metadata {
        use NativeX87MemoryDirection::{Read, Write};

        let read_dword = Some(NativeX87MemoryAccess {
            direction: Read,
            width: 4,
        });
        let write_dword = Some(NativeX87MemoryAccess {
            direction: Write,
            width: 4,
        });
        match self {
            Self::BinaryMemory { op, .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::F32Mem,
                memory: read_dword,
                pops: op.pops(),
                terminates_block: false,
            },
            Self::BinaryRegister { op, .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: op.pops(),
                terminates_block: false,
            },
            Self::LoadF32 { .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::F32Mem,
                memory: read_dword,
                pops: false,
                terminates_block: false,
            },
            Self::StoreF32 { pop, .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::F32Mem,
                memory: write_dword,
                pops: pop,
                terminates_block: false,
            },
            Self::LoadRegister { .. } | Self::Exchange { .. } => NativeX87Metadata {
                raw_clocks: 4,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: false,
                terminates_block: false,
            },
            Self::LoadI32 { .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::IntConvert32,
                memory: read_dword,
                pops: false,
                terminates_block: false,
            },
            Self::StoreI32 { .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::IntConvert32,
                memory: write_dword,
                pops: true,
                terminates_block: false,
            },
            Self::PopBinary { .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: true,
                terminates_block: false,
            },
            Self::StoreStatusAx => NativeX87Metadata {
                raw_clocks: 3,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: false,
                terminates_block: false,
            },
        }
    }
}

pub(crate) const fn native_x87_gate(
    persona: CpuPersona,
    cr0: u32,
    control: u16,
    status: u16,
) -> NativeX87Gate {
    if !persona.has_fpu() || cr0 & (CR0_EM | CR0_TS) != 0 {
        NativeX87Gate::DeviceNotAvailable
    } else if cr0 & CR0_NE != 0 && status & 0x3f & !(control & 0x3f) != 0 {
        NativeX87Gate::PendingException
    } else {
        NativeX87Gate::Ready
    }
}

pub(crate) const fn x87_top(status: u16) -> u8 {
    ((status & X87_TOP_MASK) >> X87_TOP_SHIFT) as u8
}

pub(crate) const fn x87_physical_index(status: u16, logical: u8) -> u8 {
    (x87_top(status) + logical) & 7
}

pub(crate) const fn x87_with_top(status: u16, top: u8) -> u16 {
    (status & !X87_TOP_MASK) | (((top & 7) as u16) << X87_TOP_SHIFT)
}

pub(crate) const fn x87_push_top(status: u16) -> u16 {
    x87_with_top(status, x87_top(status).wrapping_add(7) & 7)
}

pub(crate) const fn x87_pop_top(status: u16) -> u16 {
    x87_with_top(status, x87_top(status).wrapping_add(1) & 7)
}

pub(crate) const fn x87_tag_at(tag_word: u16, physical: u8) -> NativeX87Tag {
    match (tag_word >> (((physical & 7) as u16) * 2)) & 3 {
        0 => NativeX87Tag::Valid,
        1 => NativeX87Tag::Zero,
        2 => NativeX87Tag::Special,
        _ => NativeX87Tag::Empty,
    }
}

pub(crate) const fn x87_with_tag(tag_word: u16, physical: u8, tag: NativeX87Tag) -> u16 {
    let shift = ((physical & 7) as u16) * 2;
    (tag_word & !(3 << shift)) | ((tag as u16) << shift)
}

pub(crate) fn x87_value_tag(value: f64) -> NativeX87Tag {
    if value == 0.0 {
        NativeX87Tag::Zero
    } else if value.is_finite() {
        NativeX87Tag::Valid
    } else {
        NativeX87Tag::Special
    }
}

pub(crate) fn native_x87_compare_eligible(lhs: f64, rhs: f64) -> bool {
    lhs.is_finite() && rhs.is_finite()
}

pub(crate) fn native_x87_binary_result_eligible(
    op: NativeX87BinaryOp,
    lhs: f64,
    rhs: f64,
    result: f64,
) -> bool {
    if op.is_compare() {
        return native_x87_compare_eligible(lhs, rhs);
    }
    if !lhs.is_finite() || !rhs.is_finite() || !result.is_finite() {
        return false;
    }
    match op {
        NativeX87BinaryOp::Divide => rhs != 0.0,
        NativeX87BinaryOp::DivideReverse => lhs != 0.0,
        _ => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87RoundingMode {
    NearestEven,
    Down,
    Up,
    Truncate,
}

impl NativeX87RoundingMode {
    pub(crate) const fn from_control(control: u16) -> Self {
        match (control >> 10) & 3 {
            0 => Self::NearestEven,
            1 => Self::Down,
            2 => Self::Up,
            _ => Self::Truncate,
        }
    }

    pub(crate) const fn has_direct_sse2_conversion(self) -> bool {
        matches!(self, Self::NearestEven | Self::Truncate)
    }

    fn round(self, value: f64) -> f64 {
        match self {
            Self::NearestEven => value.round_ties_even(),
            Self::Down => value.floor(),
            Self::Up => value.ceil(),
            Self::Truncate => value.trunc(),
        }
    }
}

pub(crate) fn native_x87_i32_result(control: u16, value: f64) -> Option<i32> {
    let mode = NativeX87RoundingMode::from_control(control);
    if !mode.has_direct_sse2_conversion() || !value.is_finite() {
        return None;
    }
    let rounded = mode.round(value);
    if !(-2_147_483_648.0..=2_147_483_647.0).contains(&rounded) {
        return None;
    }
    Some(rounded as i32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeFpScale {
    pub(crate) clocks: u64,
    pub(crate) remainder: u64,
}

pub(crate) fn scale_weighted_fp_clocks(weighted_clocks: u64, remainder: u64) -> NativeFpScale {
    debug_assert!(remainder < u64::from(FP_TIMING_DEN));
    let total = u128::from(weighted_clocks) + u128::from(remainder);
    let denominator = u128::from(FP_TIMING_DEN);
    NativeFpScale {
        clocks: (total / denominator) as u64,
        remainder: (total % denominator) as u64,
    }
}

pub(crate) fn repeated_weighted_fp_clocks(
    per_iteration: u64,
    full_iterations: u64,
    prefix: u64,
) -> Option<u64> {
    per_iteration
        .checked_mul(full_iterations)?
        .checked_add(prefix)
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) const fn native_x87_layout() -> NativeX87Layout {
    X87::native_layout()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        }
    }

    fn expected_selected(opcode: u16, mode: u8, reg: u8, rm: u8) -> bool {
        matches!(
            (opcode, mode, reg, rm),
            (0xd8, 0..=3, 0..=7, 0..=7)
                | (0xd9, 0..=2, 0 | 2 | 3, 0..=7)
                | (0xd9, 3, 0 | 1, 0..=7)
                | (0xdb, 0..=2, 0 | 3, 0..=7)
                | (0xde, 3, 0 | 1, 0..=7)
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
        assert_eq!(accepted, 409);
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
}
