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
    Subtract,
    SubtractReverse,
    Divide,
    DivideReverse,
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
    StoreRegister {
        index: u8,
        pop: bool,
    },
    LoadOne,
    LoadZero,
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
    ComparePopPop,
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
    /// Net architectural TOP movement after a successful instruction. Positive values pop,
    /// negative values push, and zero leaves the stack position unchanged.
    pub(crate) const fn top_delta(self) -> i8 {
        match self {
            Self::LoadF32 { .. }
            | Self::LoadRegister { .. }
            | Self::LoadOne
            | Self::LoadZero
            | Self::LoadI32 { .. } => -1,
            Self::BinaryMemory { op, .. } | Self::BinaryRegister { op, .. } => {
                if op.pops() {
                    1
                } else {
                    0
                }
            }
            Self::StoreF32 { pop: true, .. }
            | Self::StoreRegister { pop: true, .. }
            | Self::StoreI32 { .. }
            | Self::PopBinary { .. } => 1,
            Self::ComparePopPop => 2,
            Self::StoreF32 { pop: false, .. }
            | Self::StoreRegister { pop: false, .. }
            | Self::Exchange { .. }
            | Self::StoreStatusAx => 0,
        }
    }

    pub(crate) const fn advance_top(self, top: u8) -> u8 {
        top.wrapping_add_signed(self.top_delta()) & 7
    }

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
            (0xd9, 5, 0) => Some(Self::LoadOne),
            (0xd9, 5, 6) => Some(Self::LoadZero),
            (0xda, 5, 1) | (0xde, 3, 1) => Some(Self::ComparePopPop),
            (0xdd, 2, index) => Some(Self::StoreRegister { index, pop: false }),
            (0xdd, 3, index) => Some(Self::StoreRegister { index, pop: true }),
            (0xde, 0, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::Add,
                index,
            }),
            (0xde, 1, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::Multiply,
                index,
            }),
            (0xde, 4, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::SubtractReverse,
                index,
            }),
            (0xde, 5, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::Subtract,
                index,
            }),
            (0xde, 6, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::DivideReverse,
                index,
            }),
            (0xde, 7, index) => Some(Self::PopBinary {
                op: NativeX87PopOp::Divide,
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
            Self::LoadRegister { .. } | Self::Exchange { .. } | Self::LoadOne | Self::LoadZero => {
                NativeX87Metadata {
                    raw_clocks: 4,
                    fp_class: FpOpClass::Register,
                    memory: None,
                    pops: false,
                    terminates_block: false,
                }
            }
            Self::StoreRegister { pop, .. } => NativeX87Metadata {
                raw_clocks: 3,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: pop,
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
            Self::ComparePopPop => NativeX87Metadata {
                raw_clocks: 5,
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
#[path = "native_x87_test.rs"]
mod tests;
