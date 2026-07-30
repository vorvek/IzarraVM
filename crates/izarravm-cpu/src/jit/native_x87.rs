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

/// The six arithmetic operations available when the DESTINATION is ST(i) and the source is
/// ST(0). That is both `0xDC` mod=3 (no pop) and `0xDE` mod=3 (pop), which the interpreter
/// serves with ONE function, `fpu_reg_arith_sti(reg, i, pop)`.
///
/// Compare and compare-pop are deliberately UNREPRESENTABLE here rather than merely unmatched.
/// `fpu_reg_arith_sti` returns `fpu_unsupported` for sub-opcodes 2 and 3, which raises #UD, so a
/// lowered compare would rewrite C0/C2/C3 and, for the pop form, pop the stack, where the guest
/// takes a fault. Making them unrepresentable means a widened pattern in a caller produces a
/// clean classification reject instead of a panic inside `direct::compile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87StiOp {
    Add,
    Multiply,
    Subtract,
    SubtractReverse,
    Divide,
    DivideReverse,
}

impl NativeX87StiOp {
    /// The DC and DE register encodings SWAP subtract with reverse-subtract and divide with
    /// reverse-divide relative to the D8 forms. Intel: `/4` FSUBR, `/5` FSUB, `/6` FDIVR,
    /// `/7` FDIV, all writing ST(i).
    ///
    /// This mirrors the `match reg { 4 => 5, 5 => 4, 6 => 7, 7 => 6, other => other }` inside
    /// `fpu_reg_arith_sti`, and it is the ONLY place the swap lives, shared by both encodings
    /// the way the interpreter shares one function. FADD and FMUL are commutative, so a swap
    /// applied to sub-opcodes 0 and 1 would be invisible in every value; only 4 through 7
    /// discriminate.
    const fn from_sti_extension(extension: u8) -> Option<Self> {
        Some(match extension {
            0 => Self::Add,
            1 => Self::Multiply,
            4 => Self::SubtractReverse,
            5 => Self::Subtract,
            6 => Self::DivideReverse,
            7 => Self::Divide,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeX87Insn {
    BinaryMemory {
        op: NativeX87BinaryOp,
        addr: AddrMode,
    },
    /// 0xDA memory forms: integer m32 arithmetic against ST(0). The operand is read as an
    /// i32 and converted; the op set is exactly BinaryMemory's (same /ext mapping), so the
    /// compare/pop behavior rides `NativeX87BinaryOp` unchanged.
    IntBinaryMemory {
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
    /// FIST/FISTP m32 (0xDB /2 no pop, /3 pop). One interpreter arm serves both
    /// (`execute_fpu_memory`'s `0xdb => 2 | 3`, fpu_exec.rs:223-229) and only the pop separates
    /// them, exactly as `StoreF32` and `StoreF64` model their own /2-/3 pairs.
    StoreI32 {
        addr: AddrMode,
        pop: bool,
    },
    PopBinary {
        op: NativeX87StiOp,
        index: u8,
    },
    /// `0xDC` mod=3. Destination ST(i), source ST(0), no pop. Identical to `PopBinary` in every
    /// respect except that it does not pop, which is exactly how the interpreter models it.
    BinaryRegisterDest {
        op: NativeX87StiOp,
        index: u8,
    },
    ComparePopPop,
    StoreStatusAx,
    /// FLDCW m16 (D9 /5 memory). The first 16-bit x87 memory access in the backend, and the
    /// only lowered form that writes the control word, which `emit_fistp_chop_guard` and
    /// `emit_gate` both read at runtime.
    LoadControlWord {
        addr: AddrMode,
    },
    /// FNSTCW m16 (D9 /7 memory).
    StoreControlWord {
        addr: AddrMode,
    },
    /// FLD m64 (0xDD /0). The m64 IS the native f64 representation, so unlike `LoadF32` there
    /// is no conversion: the eight bytes at `addr` are the resident value's bit pattern
    /// verbatim. A NaN or infinity bit pattern is legal in guest memory, so the emitted form
    /// still runs a finite guard before caching it, the same reason `LoadF32` does.
    LoadF64 {
        addr: AddrMode,
    },
    /// FST/FSTP m64 (0xDD /2 no pop, /3 pop). Also no conversion: `f64::to_bits` unchanged.
    StoreF64 {
        addr: AddrMode,
        pop: bool,
    },
    /// `0xDC` memory forms: f64 arithmetic against ST(0), all eight extensions. The interpreter
    /// serves this with the same `fpu_mem_arith` shape 0xDA's integer forms use, condition
    /// triple included for the compare variants, so the op set rides `NativeX87BinaryOp`
    /// unchanged.
    BinaryMemoryF64 {
        op: NativeX87BinaryOp,
        addr: AddrMode,
    },
    /// FUCOM/FUCOMP ST(i) (0xDD /4 no pop, /5 pop). The interpreter treats these exactly like
    /// FCOM/FCOMP -- one `fpu_compare` and an optional pop, with the unordered-versus-signaling
    /// NaN distinction unmodelled (`fpu_dd_register`, fpu_exec.rs:656-666) -- so the emitted form
    /// is `BinaryRegister`'s compare path verbatim. What is NOT shared is the timing: this is
    /// `clocks(4)` against `BinaryRegister`'s 20, which is the whole reason it cannot be folded
    /// into `NativeX87BinaryOp` and must carry its own metadata arm.
    UnorderedCompare {
        index: u8,
        pop: bool,
    },
    /// FILD m64 (0xDF /5). Slice 40, FILD-only scope: the m64 INTEGER load. Unlike `LoadI32`
    /// this converts a 64-bit integer, which can exceed f64's 53-bit mantissa and round; see the
    /// emit arm's comment for what that does and does not put at risk. `StoreI64` (FISTP m64,
    /// 0xDF /7) is DEFERRED to a later slice: its admitted population is plausibly near zero
    /// once the i32-range guard is reused unchanged, and dropping it here removes the whole
    /// range/chop risk surface from this slice. 0xDF /7 stays unclassified below.
    LoadI64 {
        addr: AddrMode,
    },
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
            | Self::LoadI32 { .. }
            | Self::LoadF64 { .. }
            | Self::LoadI64 { .. } => -1,
            Self::BinaryMemory { op, .. }
            | Self::IntBinaryMemory { op, .. }
            | Self::BinaryRegister { op, .. }
            | Self::BinaryMemoryF64 { op, .. } => {
                if op.pops() {
                    1
                } else {
                    0
                }
            }
            Self::StoreF32 { pop: true, .. }
            | Self::StoreRegister { pop: true, .. }
            | Self::StoreI32 { pop: true, .. }
            | Self::StoreF64 { pop: true, .. }
            | Self::UnorderedCompare { pop: true, .. }
            | Self::PopBinary { .. } => 1,
            Self::ComparePopPop => 2,
            Self::StoreF32 { pop: false, .. }
            | Self::StoreRegister { pop: false, .. }
            | Self::StoreI32 { pop: false, .. }
            | Self::StoreF64 { pop: false, .. }
            | Self::UnorderedCompare { pop: false, .. }
            | Self::Exchange { .. }
            | Self::StoreStatusAx
            // Neither control-word form touches the register stack, the status word or the tag
            // word, so both are TOP-insensitive. That is what makes them safe to admit, and it is
            // also why they pin a block to its compile-time TOP for no architectural reason; see
            // `jit_direct_reject_x87_top` in the campaign log.
            | Self::LoadControlWord { .. }
            | Self::StoreControlWord { .. }
            // 0 where `PopBinary` is 1, and this is the single most dangerous field in the file.
            // It feeds the emitter's running TOP, so getting it wrong makes every LATER x87 slot
            // in the block address the wrong physical XMM, and it feeds `x87_exit_top`, where the
            // only symptom is a link `link_compatible` silently refuses. `metadata().pops` looks
            // like a redundant cross-check on this and IS NOT: that field has no NON-TEST readers
            // in the crate (`metadata_matches_interpreter_timing_and_memory_effects` asserts the
            // whole struct by equality, so it does read `pops`, but nothing outside a test does).
            // `top_delta` is the sole live authority for the stack position.
            | Self::BinaryRegisterDest { .. } => 0,
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
                // The control-word pair. Both arms MUST stay in this `modrm.mode != 3` branch:
                // (0xd9, 5) also exists in the register branch below, where it is FLD1 (rm 0) and
                // FLDZ (rm 6), and (0xd9, 7) is FSQRT/FRNDINT territory there. The early return
                // above is what keeps the two branches disjoint, so an arm placed in the wrong
                // one would either shadow FLD1/FLDZ or be dead code no negative test could see.
                (0xd9, 5) => Some(Self::LoadControlWord { addr }),
                (0xd9, 7) => Some(Self::StoreControlWord { addr }),
                (0xdb, 0) => Some(Self::LoadI32 { addr }),
                (0xdb, 2) => Some(Self::StoreI32 { addr, pop: false }),
                (0xdb, 3) => Some(Self::StoreI32 { addr, pop: true }),
                (0xda, extension) => Some(Self::IntBinaryMemory {
                    op: NativeX87BinaryOp::from_extension(extension)?,
                    addr,
                }),
                // FLD/FST/FSTP m64. `/1` (FISTTP, unimplemented) and `/4`-`/7` (FLDENV, FRSTOR,
                // FSAVE, FSTSW m16) are NOT here and fall to the catch-all `None` below.
                (0xdd, 0) => Some(Self::LoadF64 { addr }),
                (0xdd, 2) => Some(Self::StoreF64 { addr, pop: false }),
                (0xdd, 3) => Some(Self::StoreF64 { addr, pop: true }),
                (0xdc, extension) => Some(Self::BinaryMemoryF64 {
                    op: NativeX87BinaryOp::from_extension(extension)?,
                    addr,
                }),
                // FILD m64. `/7` (FISTP m64, deferred to a later slice) and `/4`/`/6` (FBLD,
                // FBSTP, unimplemented) are NOT here and fall to the catch-all `None` below.
                (0xdf, 5) => Some(Self::LoadI64 { addr }),
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
            (0xdd, 4, index) => Some(Self::UnorderedCompare { index, pop: false }),
            (0xdd, 5, index) => Some(Self::UnorderedCompare { index, pop: true }),
            // 0xDC and 0xDE mod=3 are the same instruction apart from the pop, and the
            // interpreter dispatches both into one `fpu_reg_arith_sti`. They share one classifier
            // here for the same reason: the sub-opcode swap has one home rather than two
            // hand-written copies that can drift.
            //
            // `from_sti_extension` returns None for sub-opcodes 2 and 3, and the `?` returns from
            // `classify` rather than from the match, which is what the 0xd8 arm above already
            // does. That is the whole reject for DC/DE 2 and 3, and it is structural: those two
            // are not expressible in `NativeX87StiOp` at all.
            //
            // The 0xDE arm MUST stay below the `(0xde, 3, 1)` FCOMPP arm above. It does, and
            // `from_sti_extension(3)` is None regardless, so the ordering is belt and braces.
            (0xdc, extension, index) => Some(Self::BinaryRegisterDest {
                op: NativeX87StiOp::from_sti_extension(extension)?,
                index,
            }),
            (0xde, extension, index) => Some(Self::PopBinary {
                op: NativeX87StiOp::from_sti_extension(extension)?,
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
        let read_word = Some(NativeX87MemoryAccess {
            direction: Read,
            width: 2,
        });
        let write_word = Some(NativeX87MemoryAccess {
            direction: Write,
            width: 2,
        });
        let read_qword = Some(NativeX87MemoryAccess {
            direction: Read,
            width: 8,
        });
        let write_qword = Some(NativeX87MemoryAccess {
            direction: Write,
            width: 8,
        });
        match self {
            Self::BinaryMemory { op, .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::F32Mem,
                memory: read_dword,
                pops: op.pops(),
                terminates_block: false,
            },
            // Its own arm rather than joining `BinaryMemory`'s: that arm hard-codes
            // `FpOpClass::F32Mem`, which is wrong for an integer memory operand and would
            // undercharge this shape 32x against the interpreter's IntConvert16 timing tail
            // (`fpu_exec.rs:87`).
            Self::IntBinaryMemory { op, .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::IntConvert16,
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
            Self::StoreI32 { pop, .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::IntConvert32,
                memory: write_dword,
                pops: pop,
                terminates_block: false,
            },
            // Both ST(i)-destination forms are `Ok(clocks(20))` on the same interpreter arm
            // (`fpu_reg_arith_sti`), and `execute_fpu` assigns FpOpClass::Register to every
            // mod=3 form. The ONLY field that differs is `pops`, and that field has no
            // non-test readers: it is documentation for a test to check, and `top_delta` is
            // what actually carries the stack effect.
            Self::PopBinary { .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: true,
                terminates_block: false,
            },
            Self::BinaryRegisterDest { .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: false,
                terminates_block: false,
            },
            // `clocks(4)`, verified at fpu_exec.rs:665. Copying `BinaryRegister`'s 20 is the
            // realistic mistake here (both are register-form compares) and would overcharge every
            // FUCOM by 5x its weighted cost; the mutation battery targets the concrete number.
            Self::UnorderedCompare { pop, .. } => NativeX87Metadata {
                raw_clocks: 4,
                fp_class: FpOpClass::Register,
                memory: None,
                pops: pop,
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
            // FLDCW is `Ok(clocks(4))` and FNSTCW `Ok(clocks(14))` (fpu_exec.rs, the 0xd9 arm of
            // `execute_fpu_memory`). Four is the ONLY 0xd9 memory form in this table that is not
            // 14, so copying its neighbour is the natural mistake here; it is mutation-tested
            // against the concrete number. `fp_class` is F32Mem for both because `execute_fpu`
            // derives the class from the OPCODE BYTE for every memory form, not from the operand
            // width, and 0xd9 maps to F32Mem there.
            Self::LoadControlWord { .. } => NativeX87Metadata {
                raw_clocks: 4,
                fp_class: FpOpClass::F32Mem,
                memory: read_word,
                pops: false,
                terminates_block: false,
            },
            Self::StoreControlWord { .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::F32Mem,
                memory: write_word,
                pops: false,
                terminates_block: false,
            },
            // 0xDD /0, verified against the interpreter (fpu_exec.rs:178-182): `Ok(clocks(14))`.
            // `fp_class` is F64Mem, not F32Mem: `execute_fpu` derives the class from the opcode
            // byte, and 0xdd (unlike 0xd9) maps to F64Mem in the timing tail (fpu_exec.rs:88-89).
            Self::LoadF64 { .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::F64Mem,
                memory: read_qword,
                pops: false,
                terminates_block: false,
            },
            // 0xDD /2 and /3, both verified `Ok(clocks(14))` (fpu_exec.rs:183-191). Only `pops`
            // differs between the two sub-opcodes, mirroring `StoreF32`.
            Self::StoreF64 { pop, .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::F64Mem,
                memory: write_qword,
                pops: pop,
                terminates_block: false,
            },
            // 0xDC memory, all eight extensions, verified `Ok(clocks(20))` (fpu_exec.rs:124-128,
            // `fpu_mem_arith`). Same raw clocks as `BinaryMemory`'s F32 counterpart; only the
            // class and the operand width differ.
            Self::BinaryMemoryF64 { op, .. } => NativeX87Metadata {
                raw_clocks: 20,
                fp_class: FpOpClass::F64Mem,
                memory: read_qword,
                pops: op.pops(),
                terminates_block: false,
            },
            // 0xDF /5, verified against the interpreter (fpu_exec.rs:271-274): `Ok(clocks(14))`.
            // `fp_class` is `IntConvert16`, NOT `IntConvert32`: `execute_fpu` derives the class
            // from the opcode byte, and 0xdf (like 0xda and 0xde) maps to `IntConvert16` in the
            // timing tail (fpu_exec.rs:87), unlike 0xdb which maps to `IntConvert32`. Copying
            // `LoadI32`'s `IntConvert32` here is the realistic mistake (raw scale 272 against the
            // correct 256, a 6 percent undercharge) and is what the mutation battery targets.
            Self::LoadI64 { .. } => NativeX87Metadata {
                raw_clocks: 14,
                fp_class: FpOpClass::IntConvert16,
                memory: read_qword,
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
