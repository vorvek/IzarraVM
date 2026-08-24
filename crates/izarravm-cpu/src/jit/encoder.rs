// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal direct-byte x86-64 encoder for emitted region chains. It is not a
//! general assembler. Each supported instruction form has a byte-level test.
//! The region compiler uses only the subset it needs for a given host path.
#![allow(dead_code)]

/// A host x86-64 general-purpose register, numbered 0-15 in the standard encoding order
/// (RAX=0 .. RDI=7, R8=8 .. R15=15). The raw encoding number is the public tuple field `.0`;
/// callers needing the REX-extension bit use `.0 >= 8` (see `Reg::ext`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reg(pub u8);

impl Reg {
    pub const RAX: Reg = Reg(0);
    pub const RCX: Reg = Reg(1);
    pub const RDX: Reg = Reg(2);
    pub const RBX: Reg = Reg(3);
    pub const RSP: Reg = Reg(4);
    pub const RBP: Reg = Reg(5);
    // Not read by the strcpy block's emitter (it passes SI/DI as plain GPR indices to the
    // inc_gpr16 callback rather than addressing the host's own RSI/RDI), but kept and unit-tested
    // as part of the encoder's register-name completeness for whatever block a future slice adds.
    #[allow(dead_code)]
    pub const RSI: Reg = Reg(6);
    #[allow(dead_code)]
    pub const RDI: Reg = Reg(7);
    pub const R8: Reg = Reg(8);
    pub const R9: Reg = Reg(9);
    pub const R10: Reg = Reg(10);
    pub const R11: Reg = Reg(11);
    pub const R12: Reg = Reg(12);
    pub const R13: Reg = Reg(13);
    pub const R14: Reg = Reg(14);
    pub const R15: Reg = Reg(15);

    fn low3(self) -> u8 {
        self.0 & 0x7
    }
    fn ext(self) -> bool {
        self.0 >= 8
    }
}

/// A host x86-64 SIMD register in the standard XMM0-XMM15 encoding order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Xmm(pub u8);

impl Xmm {
    pub const XMM0: Xmm = Xmm(0);
    pub const XMM1: Xmm = Xmm(1);
    pub const XMM2: Xmm = Xmm(2);
    pub const XMM3: Xmm = Xmm(3);
    pub const XMM4: Xmm = Xmm(4);
    pub const XMM5: Xmm = Xmm(5);
    pub const XMM6: Xmm = Xmm(6);
    pub const XMM7: Xmm = Xmm(7);
    pub const XMM8: Xmm = Xmm(8);
    pub const XMM9: Xmm = Xmm(9);
    pub const XMM10: Xmm = Xmm(10);
    pub const XMM11: Xmm = Xmm(11);
    pub const XMM12: Xmm = Xmm(12);
    pub const XMM13: Xmm = Xmm(13);
    pub const XMM14: Xmm = Xmm(14);
    pub const XMM15: Xmm = Xmm(15);

    fn low3(self) -> u8 {
        self.0 & 0x7
    }

    fn ext(self) -> bool {
        self.0 >= 8
    }
}

/// A host x86-64 AVX register in the standard YMM0-YMM15 encoding order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ymm(pub u8);

impl Ymm {
    pub const YMM0: Ymm = Ymm(0);
    pub const YMM1: Ymm = Ymm(1);
    pub const YMM2: Ymm = Ymm(2);
    pub const YMM3: Ymm = Ymm(3);
    pub const YMM4: Ymm = Ymm(4);
    pub const YMM5: Ymm = Ymm(5);
    pub const YMM6: Ymm = Ymm(6);
    pub const YMM7: Ymm = Ymm(7);
    pub const YMM8: Ymm = Ymm(8);
    pub const YMM9: Ymm = Ymm(9);
    pub const YMM10: Ymm = Ymm(10);
    pub const YMM11: Ymm = Ymm(11);
    pub const YMM12: Ymm = Ymm(12);
    pub const YMM13: Ymm = Ymm(13);
    pub const YMM14: Ymm = Ymm(14);
    pub const YMM15: Ymm = Ymm(15);

    fn low3(self) -> u8 {
        self.0 & 0x7
    }

    fn ext(self) -> bool {
        self.0 >= 8
    }
}

/// A forward- or backward-reference point in the emitted byte stream. `Encoder::label()`
/// creates one (its position is filled in once it is actually placed by `Encoder::place`);
/// `Encoder::jcc`/`Encoder::jmp` reference one before it may be placed (a forward jump), and the
/// patch is resolved in `Encoder::finish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Label(usize);

enum PatchKind {
    Rel32AfterJcc, // 2-byte 0F 8x opcode + 4-byte rel32, patch the last 4 bytes
    // 1-byte E9 opcode + 4-byte rel32, patch the last 4 bytes. Only `Encoder::jmp` constructs
    // this; the strcpy block's emitter never calls `jmp` (its non-fallthrough control flow is
    // all conditional: `ja`/`jnz`/`jz`), so this variant is presently unused outside `jmp`'s own
    // unit test. Kept for a future block with an unconditional branch.
    #[allow(dead_code)]
    Rel32AfterJmp,
}

struct Patch {
    /// Byte offset of the START of the instruction being patched (the opcode byte(s)), so the
    /// patch can recompute "end of this instruction" itself from `kind`.
    instr_start: usize,
    kind: PatchKind,
    target: Label,
}

#[derive(Clone, Copy)]
struct VexOp {
    map: u8,
    pp: u8,
    w: bool,
    vector_256: bool,
    opcode: u8,
}

impl VexOp {
    const fn new(map: u8, pp: u8, w: bool, vector_256: bool, opcode: u8) -> Self {
        Self {
            map,
            pp,
            w,
            vector_256,
            opcode,
        }
    }
}

pub(crate) struct Encoder {
    bytes: Vec<u8>,
    label_positions: Vec<Option<usize>>,
    patches: Vec<Patch>,
}

impl Encoder {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            label_positions: Vec::new(),
            patches: Vec::new(),
        }
    }

    pub(crate) fn label(&mut self) -> Label {
        self.label_positions.push(None);
        Label(self.label_positions.len() - 1)
    }

    pub(crate) fn position(&self) -> usize {
        self.bytes.len()
    }

    /// Bind `label` to the CURRENT write position (the next byte emitted is the label's target).
    /// Panics if `label` has already been placed: silently rebinding a jump target to a second
    /// position would produce wrong-but-plausible code rather than a clear failure.
    pub(crate) fn place(&mut self, label: Label) {
        assert!(
            self.label_positions[label.0].is_none(),
            "label placed twice"
        );
        self.label_positions[label.0] = Some(self.bytes.len());
    }

    fn rex(&mut self, w: bool, r: bool, x: bool, b: bool) {
        // REX is required for 64-bit operands or extended registers. Pointer
        // and qword operations always set W, so this prefix is unconditional.
        let byte =
            0x40 | (u8::from(w) << 3) | (u8::from(r) << 2) | (u8::from(x) << 1) | u8::from(b);
        self.bytes.push(byte);
    }

    fn optional_rex(&mut self, w: bool, r: bool, x: bool, b: bool) {
        if w || r || x || b {
            self.rex(w, r, x, b);
        }
    }

    fn modrm(&mut self, md: u8, reg: u8, rm: u8) {
        self.bytes.push((md << 6) | ((reg & 7) << 3) | (rm & 7));
    }

    fn scalar_xmm_reg_reg(&mut self, prefix: u8, opcode: u8, dst: Xmm, src: Xmm) {
        self.bytes.push(prefix);
        self.optional_rex(false, dst.ext(), false, src.ext());
        self.bytes.extend_from_slice(&[0x0F, opcode]);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    fn scalar_xmm_mem_disp32(&mut self, prefix: u8, opcode: u8, xmm: Xmm, base: Reg, disp32: i32) {
        self.bytes.push(prefix);
        self.optional_rex(false, xmm.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, opcode]);
        self.modrm(0b10, xmm.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    fn scalar_xmm_mem_sib_scale8_disp32(
        &mut self,
        prefix: u8,
        opcode: u8,
        xmm: Xmm,
        base: Reg,
        index: Reg,
        disp32: i32,
    ) {
        assert!(index != Reg::RSP, "RSP cannot be a SIB index");
        self.bytes.push(prefix);
        self.optional_rex(false, xmm.ext(), index.ext(), base.ext());
        self.bytes.extend_from_slice(&[0x0F, opcode]);
        self.modrm(0b10, xmm.low3(), 0b100);
        self.bytes
            .push((0b11 << 6) | (index.low3() << 3) | base.low3());
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// Emit the shortest valid VEX prefix for the requested fields. `src1` is the logical
    /// register encoded through the inverted vvvv field. `None` emits raw vvvv=1111, as required
    /// by instructions where that field is reserved.
    fn vex_prefix(
        &mut self,
        op: VexOp,
        reg_ext: bool,
        index_ext: bool,
        rm_ext: bool,
        src1: Option<u8>,
    ) {
        assert!(
            (1..=3).contains(&op.map),
            "VEX map must be 0F, 0F38, or 0F3A"
        );
        assert!(op.pp < 4, "VEX mandatory-prefix field must fit two bits");
        if let Some(src1) = src1 {
            assert!(src1 < 16, "VEX vvvv source must be XMM0-XMM15");
        }
        let encoded_vvvv = src1.map_or(0x0f, |register| (!register) & 0x0f);
        let third =
            (u8::from(op.w) << 7) | (encoded_vvvv << 3) | (u8::from(op.vector_256) << 2) | op.pp;

        // VEX2 fixes X, B, and W to zero and selects the 0F opcode map. High ModRM.reg values
        // remain available through its inverted R bit.
        if op.map == 1 && !op.w && !index_ext && !rm_ext {
            self.bytes.push(0xc5);
            self.bytes.push((u8::from(!reg_ext) << 7) | (third & 0x7f));
        } else {
            self.bytes.push(0xc4);
            self.bytes.push(
                (u8::from(!reg_ext) << 7)
                    | (u8::from(!index_ext) << 6)
                    | (u8::from(!rm_ext) << 5)
                    | op.map,
            );
            self.bytes.push(third);
        }
    }

    fn vex_reg_rm(&mut self, op: VexOp, reg: u8, src1: Option<u8>, rm: u8) {
        assert!(reg < 16 && rm < 16, "VEX register must be below 16");
        self.vex_prefix(op, reg >= 8, false, rm >= 8, src1);
        self.bytes.push(op.opcode);
        self.modrm(0b11, reg & 7, rm & 7);
    }

    fn vex_mem_disp32(&mut self, op: VexOp, reg: u8, src1: Option<u8>, base: Reg, disp32: i32) {
        assert!(reg < 16, "VEX ModRM.reg must be below 16");
        self.vex_prefix(op, reg >= 8, false, base.ext(), src1);
        self.bytes.push(op.opcode);
        self.modrm(0b10, reg & 7, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `push r64` (50+rd, REX.B set iff `r` is an extended register).
    pub(crate) fn push(&mut self, r: Reg) {
        if r.ext() {
            self.bytes.push(0x41);
        }
        self.bytes.push(0x50 + r.low3());
    }

    /// `pop r64` (58+rd).
    pub(crate) fn pop(&mut self, r: Reg) {
        if r.ext() {
            self.bytes.push(0x41);
        }
        self.bytes.push(0x58 + r.low3());
    }

    /// `mov dst, src` (REX.W + 89 /r, MOV r/m64,r64: src is the ModRM reg field, dst is rm).
    pub(crate) fn mov_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.ext(), false, dst.ext());
        self.bytes.push(0x89);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `cmp a, b` (REX.W + 39 /r, CMP r/m64,r64: computes `a - b` and sets flags, discarding the
    /// result). Mirrors `mov_r64_r64`'s exact register/ModRM pattern with opcode 0x39 in place of
    /// 0x89 -- needed to compare the runtime `cap_clocks` value (not a compile-time immediate, so
    /// `cmp_r64_imm32` does not apply) against the accumulated raw-clocks register.
    pub(crate) fn cmp_r64_r64(&mut self, a: Reg, b: Reg) {
        self.rex(true, b.ext(), false, a.ext());
        self.bytes.push(0x39);
        self.modrm(0b11, b.low3(), a.low3());
    }

    /// `add dst, src` (REX.W + 01 /r, ADD r/m64, r64: dst += src). Same register/ModRM pattern as
    /// `mov_r64_r64` with opcode 0x01. Used by the native cap check to sum the scaled-core and
    /// bus-growth terms.
    #[allow(dead_code)]
    pub(crate) fn add_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.ext(), false, dst.ext());
        self.bytes.push(0x01);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `or dst64, src64` (REX.W + 09 /r).
    pub(crate) fn or_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.ext(), false, dst.ext());
        self.bytes.push(0x09);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `sub dst, src` (REX.W + 29 /r, SUB r/m64, r64: dst -= src). Mirrors `add_r64_r64` with
    /// opcode 0x29.
    #[allow(dead_code)]
    pub(crate) fn sub_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.ext(), false, dst.ext());
        self.bytes.push(0x29);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `imul dst, src` (REX.W + 0F AF /r, IMUL r64, r/m64: dst *= src, signed). Used by the
    /// native cap check to multiply by the scale denominator.
    #[allow(dead_code)]
    pub(crate) fn imul_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, dst.ext(), false, src.ext());
        self.bytes.push(0x0F);
        self.bytes.push(0xAF);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    /// `imul dst, src` (0F AF /r, IMUL r32, r/m32: dst *= src, signed, truncated to 32 bits).
    /// Deliberately NOT built on `imul_r64_r64`: that helper always sets REX.W, which multiplies
    /// at 64 bits and reports overflow against the 64-bit product instead of the 32-bit one. The
    /// guest's two-operand IMUL defines CF/OF from whether the 32-bit truncated result sign-
    /// extends back to the full product, so 0x0001_0000 * 0x0001_0000 must set CF=OF=1 here,
    /// while the REX.W form reports 0 for the same inputs.
    pub(crate) fn imul_r32_r32(&mut self, dst: Reg, src: Reg) {
        self.optional_rex(false, dst.ext(), false, src.ext());
        self.bytes.push(0x0F);
        self.bytes.push(0xAF);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    /// `imul dst, src, imm32` (69 /r id, IMUL r32, r/m32, imm32: dst = src * imm32, signed,
    /// truncated to 32 bits). The three-operand form: `src` is READ and `dst` is WRITTEN, and the
    /// two may be the same register.
    ///
    /// NO REX.W, and the reason is `imul_r32_r32`'s verbatim: the guest's three-operand IMUL
    /// defines CF/OF from whether the 32-bit truncated result sign-extends back to the full
    /// product (`imul_truncated`, core.rs), and the 64-bit form reports overflow against the
    /// 64-bit product instead. `imul_r64_imm32` immediately below IS the REX.W form and is a
    /// different instruction for a different purpose (the native cap check's scale multiply);
    /// they are written out separately rather than sharing a `w` parameter so that picking the
    /// wrong width is not a one-character edit.
    ///
    /// The `reg` field is the DESTINATION and `rm` is the source, which is the opposite of the
    /// group-3 forms above where `reg` is the sub-opcode. REX.R therefore comes from `dst` and
    /// REX.B from `src`.
    pub(crate) fn imul_r32_r32_imm32(&mut self, dst: Reg, src: Reg, imm: u32) {
        self.optional_rex(false, dst.ext(), false, src.ext());
        self.bytes.push(0x69);
        self.modrm(0b11, dst.low3(), src.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mul src` (F7 /4, MUL r/m32 register form: EDX:EAX = EAX * src, UNSIGNED). Sets CF and OF
    /// together to whether the high half is nonzero, and leaves SF/ZF/AF/PF undefined, which is
    /// exactly the guest's one-operand MUL.
    ///
    /// Deliberately NOT parameterised on the group-3 sub-opcode. /5 is IMUL, the SIGNED sibling
    /// with a different overflow rule (the product not sign-extending back from the low half), and
    /// /6 and /7 are DIV and IDIV, which fault. A shared `group3_r32(op, ..)` helper would make
    /// picking the wrong one a one-character edit with no encoder-level test able to see it.
    pub(crate) fn mul_r32(&mut self, src: Reg) {
        self.optional_rex(false, false, false, src.ext());
        self.bytes.push(0xF7);
        self.modrm(0b11, 4, src.low3());
    }

    /// `imul src` (F7 /5, IMUL r/m32 register form: EDX:EAX = EAX * src, SIGNED). Sets CF and OF
    /// together to whether the full product fails to sign-extend back from the low half, and
    /// leaves SF/ZF/AF/PF undefined, which is exactly the guest's one-operand IMUL.
    ///
    /// Written out separately from `mul_r32` rather than sharing a helper parameterised on the
    /// group-3 sub-opcode, for the reason that function's own comment gives: /4 is the UNSIGNED
    /// sibling with a different overflow rule, so picking the wrong reg field would be a
    /// one-character edit that a shared helper's test could not see.
    pub(crate) fn imul_r32(&mut self, src: Reg) {
        self.optional_rex(false, false, false, src.ext());
        self.bytes.push(0xF7);
        self.modrm(0b11, 5, src.low3());
    }

    /// `div src` (F7 /6, DIV r/m32 register form: EDX:EAX / src UNSIGNED, quotient to EAX and
    /// remainder to EDX).
    ///
    /// **RAISES #DE** when `src` is zero or the quotient does not fit 32 bits, and a #DE inside
    /// emitted code is a HOST exception on the JIT stack, not a guest fault. Every caller must
    /// prove both conditions excluded before this is reached; `emit_div_reg` carries the proof.
    /// Written out separately from `mul_r32`/`imul_r32` for the reason those two give about each
    /// other -- but the stakes here are higher, because picking /6 where /4 was meant is not a
    /// wrong answer, it is a process abort.
    pub(crate) fn div_r32(&mut self, src: Reg) {
        self.optional_rex(false, false, false, src.ext());
        self.bytes.push(0xF7);
        self.modrm(0b11, 6, src.low3());
    }

    /// `idiv src` (REX.W F7 /7, IDIV r/m64 register form: RDX:RAX / src SIGNED, quotient to RAX
    /// and remainder to RDX).
    ///
    /// SIXTY-FOUR bit on purpose, and it is the whole reason the guest's 32-bit IDIV can be
    /// lowered at all. The guest's dividend is 64 bits wide (EDX:EAX) and its quotient is 32, so
    /// a host 32-bit IDIV faults on exactly the quotient-overflow case the guest defines -- which
    /// would have to be predicted BEFORE the divide, and there is no cheap exact predicate for it.
    /// At 64 bits the quotient always fits whenever `|src| >= 2`, so the overflow test becomes a
    /// COMPARISON on the answer instead of a prediction. See `emit_div_reg`.
    ///
    /// Still raises #DE on a zero divisor, and on `i64::MIN / -1`; both are excluded by that
    /// function's guards.
    pub(crate) fn idiv_r64(&mut self, src: Reg) {
        self.rex(true, false, false, src.ext());
        self.bytes.push(0xF7);
        self.modrm(0b11, 7, src.low3());
    }

    /// `cqo` (REX.W 99): sign-extend RAX across RDX, the 64-bit sibling of CDQ. The dividend
    /// preparation `idiv_r64` requires; writes no flags.
    pub(crate) fn cqo(&mut self) {
        self.rex(true, false, false, false);
        self.bytes.push(0x99);
    }

    /// `movsxd dst64, src32` (REX.W 63 /r): sign-extend a 32-bit register into a 64-bit one.
    ///
    /// The one place a guest dword becomes a host qword with its sign intact, which is what makes
    /// a signed 32-bit divisor and a signed 32-bit quotient comparable against 64-bit values.
    /// Writes no flags.
    pub(crate) fn movsxd_r64_r32(&mut self, dst: Reg, src: Reg) {
        self.rex(true, dst.ext(), false, src.ext());
        self.bytes.push(0x63);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    /// `movsx dst32, byte [base + disp8]` (0F BE /r, MOVSX r32, r/m8: SIGN-extend). The signed
    /// sibling of `movzx_r32_byte_disp8`; the two differ only in the second opcode byte, so they
    /// are written out separately rather than sharing a parameterised helper where picking the
    /// wrong one would be a one-character edit no encoder test could see.
    pub(crate) fn movsx_r32_byte_disp8(&mut self, dst: Reg, base: Reg, disp8: i8) {
        self.optional_rex(false, dst.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xBE]);
        self.modrm(0b01, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `movsx dst32, src8` (0F BE /r with mod=11, MOVSX r32, r8: SIGN-extend a register).
    /// Register sibling of `movsx_r32_byte_disp8`. Every current caller passes either RDX or a
    /// `GUEST_HOMES` member. `GUEST_HOMES` is R8-R14 plus RBX: the seven extended homes force
    /// `REX.B` through `optional_rex`'s `src.ext()`, and RBX (low3 = 3) encodes as BL with no
    /// REX. No home -- and not RDX -- is RSP/RBP/RSI/RDI, so the AH/CH/DH/BH aliasing encoding
    /// (legacy index 4..=7 with no REX) is unreachable for either family. Anyone adding a caller
    /// with a legacy non-RDX register (RSP/RBP/RSI/RDI in particular) must re-derive this
    /// guarantee rather than assume it.
    /// Written out separately from the word form rather than parameterised, for the reason the
    /// memory pair records: the second opcode byte is the whole difference and a shared helper's
    /// test could not see it being picked wrongly.
    pub(crate) fn movsx_r32_r8(&mut self, dst: Reg, src: Reg) {
        self.optional_rex(false, dst.ext(), false, src.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xBE]);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    /// `movsx dst32, src16` (0F BF /r with mod=11, MOVSX r32, r16: SIGN-extend a register).
    pub(crate) fn movsx_r32_r16(&mut self, dst: Reg, src: Reg) {
        self.optional_rex(false, dst.ext(), false, src.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xBF]);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    /// `movsx dst32, word [base + disp8]` (0F BF /r, MOVSX r32, r/m16: SIGN-extend).
    pub(crate) fn movsx_r32_word_disp8(&mut self, dst: Reg, base: Reg, disp8: i8) {
        self.optional_rex(false, dst.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xBF]);
        self.modrm(0b01, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `imul dst, imm32` (REX.W + 69 /r id, the three-operand IMUL r64, r/m64, imm32 form: dst =
    /// dst * imm32). Used by the native cap check to multiply the bus delta by the scale
    /// denominator (a small compile-time constant like 12 for 586).
    pub(crate) fn imul_r64_imm32(&mut self, dst: Reg, imm: u32) {
        self.rex(true, dst.ext(), false, dst.ext());
        self.bytes.push(0x69);
        self.modrm(0b11, dst.low3(), dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dst64, [base + disp32]` (REX.W + 8B /r, mod=10 disp32, SIB if base is RSP/R12). The
    /// 32-bit-displacement form for ctx fields past offset 127.
    pub(crate) fn load_r64_disp32(&mut self, dst: Reg, base: Reg, disp32: i32) {
        self.rex(true, dst.ext(), false, base.ext());
        self.bytes.push(0x8B);
        self.modrm(0b10, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `mov [base + disp32], src64` (REX.W + 89 /r, mod=10 disp32, SIB if base is RSP/R12).
    pub(crate) fn store_r64_disp32(&mut self, base: Reg, disp32: i32, src: Reg) {
        self.rex(true, src.ext(), false, base.ext());
        self.bytes.push(0x89);
        self.modrm(0b10, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `mov [base + disp32], imm32` (REX.W + C7 /0 id, the store-immediate-to-memory form). Used
    /// to initialize ctx fields from native code.
    #[allow(dead_code)]
    pub(crate) fn store_imm32_disp32(&mut self, base: Reg, disp32: i32, imm: u32) {
        self.rex(true, false, false, base.ext());
        self.bytes.push(0xC7);
        self.modrm(0b10, 0, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dword [base + disp32], imm32` (C7 /0 id, without REX.W).
    pub(crate) fn store_u32_imm_disp32(&mut self, base: Reg, disp32: i32, imm: u32) {
        if base.ext() {
            self.rex(false, false, false, true);
        }
        self.bytes.push(0xC7);
        self.modrm(0b10, 0, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dst32, src32` (32-bit move, no REX.W; used for passing a small u32 arg). REX byte is
    /// emitted only if an extended register is involved. Unit-tested but not yet called by the
    /// strcpy block's emitter (its u32 args are all compile-time constants, emitted via
    /// `mov_r32_imm32` instead) -- kept for a future block that needs to move a 32-bit value
    /// between registers.
    #[allow(dead_code)]
    pub(crate) fn mov_r32_r32(&mut self, dst: Reg, src: Reg) {
        if src.ext() || dst.ext() {
            self.rex(false, src.ext(), false, dst.ext());
        }
        self.bytes.push(0x89);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `mov dst16, src16` (66 + 89 /r). The upper 16 bits of the destination are preserved.
    pub(crate) fn mov_r16_r16(&mut self, dst: Reg, src: Reg) {
        self.bytes.push(0x66);
        self.optional_rex(false, src.ext(), false, dst.ext());
        self.bytes.push(0x89);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `mov dst32, imm32` (B8+rd id; REX.B if dst is extended).
    pub(crate) fn mov_r32_imm32(&mut self, dst: Reg, imm: u32) {
        if dst.ext() {
            self.bytes.push(0x41);
        }
        self.bytes.push(0xB8 + dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dst8, imm8` for AL/CL/DL/BL.
    pub(crate) fn mov_r8_imm8(&mut self, dst: Reg, imm: u8) {
        assert!(
            dst.low3() < 4 && !dst.ext(),
            "mov_r8_imm8 dst must be AL/CL/DL/BL"
        );
        self.bytes.extend_from_slice(&[0xB0 + dst.low3(), imm]);
    }

    /// `mov dst64, imm64` (REX.W + B8+rd io).
    pub(crate) fn mov_r64_imm64(&mut self, dst: Reg, imm: u64) {
        self.rex(true, false, false, dst.ext());
        self.bytes.push(0xB8 + dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// Whether `base.low3()` requires a SIB byte after the ModRM byte. x86-64 reserves ModRM.rm
    /// == 0b100 (RSP's low3 bits, also shared by R12) to mean "a SIB byte follows" -- it can never
    /// mean "RSP/R12 is the base register" directly, for ANY mod (00/01/10), independent of
    /// REX.B. Skipping the SIB byte for these two bases silently shifts every following byte by
    /// one and runs the wrong instruction stream (caught the hard way: this gap originally broke
    /// `[rsp+disp8]` addressing with a host access violation, not a clean test failure).
    fn needs_sib(base: Reg) -> bool {
        base.low3() == 0b100
    }

    /// `mov dst64, [base + disp8]` (REX.W + 8B /r, mod=01 disp8, SIB if `base` is RSP/R12).
    /// `disp8` must fit in `i8`.
    pub(crate) fn load_r64_disp8(&mut self, dst: Reg, base: Reg, disp8: i8) {
        self.rex(true, dst.ext(), false, base.ext());
        self.bytes.push(0x8B);
        self.modrm(0b01, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            // scale=00, index=100 (none), base=100 -- "no index, RSP/R12 as the base", the only
            // base value that can reach this branch (see `needs_sib`).
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `mov [base + disp8], src` (REX.W + 89 /r, mod=01 disp8, SIB if `base` is RSP/R12) -- the
    /// store mirror of `load_r64_disp8` (which uses 8B, the load form of the same MOV r/m64,r64
    /// family). `disp8` must fit in `i8`.
    pub(crate) fn store_r64_disp8(&mut self, base: Reg, disp8: i8, src: Reg) {
        self.rex(true, src.ext(), false, base.ext());
        self.bytes.push(0x89);
        self.modrm(0b01, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `mov dst32, [base + disp8]` (8B /r, mod=01 disp8, no REX.W, SIB if `base` is RSP/R12) -- the
    /// 32-bit-operand mirror of `load_r64_disp8`. Reads a 32-bit guest register (gpr[i]) from the
    /// `Registers` array the v2 inline slots address by offset; no REX.W so the load zero-extends to
    /// 64 bits (the host register's upper half is cleared, matching the x86-64 rule for 32-bit ops).
    /// REX is emitted only when an extended register is used (a bare 0x40 REX with no bits set is
    /// legal but wasteful; the 64-bit primitives always set W so this guard does not apply there).
    pub(crate) fn load_r32_disp8(&mut self, dst: Reg, base: Reg, disp8: i8) {
        if dst.ext() || base.ext() {
            self.rex(false, dst.ext(), false, base.ext());
        }
        self.bytes.push(0x8B);
        self.modrm(0b01, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `mov [base + disp8], src32` (89 /r, mod=01 disp8, no REX.W, SIB if `base` is RSP/R12) -- the
    /// 32-bit-operand store mirror, for writing a computed guest gpr[i] back to the `Registers`
    /// array.
    pub(crate) fn store_r32_disp8(&mut self, base: Reg, disp8: i8, src: Reg) {
        if src.ext() || base.ext() {
            self.rex(false, src.ext(), false, base.ext());
        }
        self.bytes.push(0x89);
        self.modrm(0b01, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `mov dst32, [base + disp32]` (8B /r, mod=10 disp32, no REX.W, SIB if `base` is RSP/R12) -- the
    /// 32-bit-operand, 32-bit-displacement load. Zero-extends like `load_r32_disp8`.
    pub(crate) fn load_r32_disp32(&mut self, dst: Reg, base: Reg, disp32: i32) {
        if dst.ext() || base.ext() {
            self.rex(false, dst.ext(), false, base.ext());
        }
        self.bytes.push(0x8B);
        self.modrm(0b10, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `movzx dst32, byte [base + disp32]` (0F B6 /r, mod=10 disp32, no REX.W).
    pub(crate) fn movzx_r32_byte_disp32(&mut self, dst: Reg, base: Reg, disp32: i32) {
        if dst.ext() || base.ext() {
            self.rex(false, dst.ext(), false, base.ext());
        }
        self.bytes.extend_from_slice(&[0x0F, 0xB6]);
        self.modrm(0b10, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `mov [base + disp32], src32` (89 /r, mod=10 disp32, no REX.W, SIB if `base` is RSP/R12) -- the
    /// 32-bit store mirror of `load_r32_disp32` for fields past the disp8 range.
    pub(crate) fn store_r32_disp32(&mut self, base: Reg, disp32: i32, src: Reg) {
        if src.ext() || base.ext() {
            self.rex(false, src.ext(), false, base.ext());
        }
        self.bytes.push(0x89);
        self.modrm(0b10, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `mov dst32, [base + index]` (8B /r, mod=00, rm=100 SIB, scale=1). No REX.W (32-bit load,
    /// zero-extends to 64). The base must NOT be RBP/R13 (SIB base=101 with mod=00 means "disp32,
    /// no base") and the index must NOT be RSP (SIB index=100 means "no index"). Used by the G0'
    /// CPU-ceiling probe for the texture sample `mov eax,[esi_host + ecx]`, where esi_host is a host
    /// pointer (the guest texture base folded in) so a guest base+index memory operand lowers to one
    /// host SIB access with no extra address-add (which would clobber the loop's live flags).
    pub(crate) fn load_r32_sib(&mut self, dst: Reg, base: Reg, index: Reg) {
        assert!(base.low3() != 0b101, "SIB base RBP/R13 needs a disp form");
        assert!(index.low3() != 0b100, "SIB index RSP means no-index");
        if dst.ext() || index.ext() || base.ext() {
            self.rex(false, dst.ext(), index.ext(), base.ext());
        }
        self.bytes.push(0x8B);
        self.modrm(0b00, dst.low3(), 0b100); // rm=100 -> a SIB byte follows
        self.bytes.push((index.low3() << 3) | base.low3()); // scale=00
    }

    /// `mov dst64, [base + index*8]` (REX.W + 8B /r, SIB scale=8). The direct map uses
    /// this to load one pointer bias from its `usize` array by linear-page index.
    pub(crate) fn load_r64_sib_scale8(&mut self, dst: Reg, base: Reg, index: Reg) {
        assert!(base.low3() != 0b101, "SIB base RBP/R13 needs a disp form");
        assert!(index.low3() != 0b100, "SIB index RSP means no-index");
        self.rex(true, dst.ext(), index.ext(), base.ext());
        self.bytes.push(0x8B);
        self.modrm(0b00, dst.low3(), 0b100);
        self.bytes
            .push((0b11 << 6) | (index.low3() << 3) | base.low3());
    }

    /// `mov dst32, [base + index*4]` (8B /r, SIB scale=4).
    pub(crate) fn load_r32_sib_scale4(&mut self, dst: Reg, base: Reg, index: Reg) {
        assert!(base.low3() != 0b101, "SIB base RBP/R13 needs a disp form");
        assert!(index.low3() != 0b100, "SIB index RSP means no-index");
        if dst.ext() || index.ext() || base.ext() {
            self.rex(false, dst.ext(), index.ext(), base.ext());
        }
        self.bytes.push(0x8B);
        self.modrm(0b00, dst.low3(), 0b100);
        self.bytes
            .push((0b10 << 6) | (index.low3() << 3) | base.low3());
    }

    /// `add dst32, imm32` (81 /0 id, no REX.W) -- the 32-bit-operand ADD-immmediate form the v2
    /// inline `add r32, imm32` slot uses against a host scratch register holding the guest gpr
    /// value. Sets the host flags, but those are not the guest flags (the guest flag update is a
    /// separate helper call); the emitted code does not read host flags after this.
    pub(crate) fn add_r32_imm32(&mut self, dst: Reg, imm: u32) {
        if dst.ext() {
            self.rex(false, false, false, dst.ext());
        }
        self.bytes.push(0x81);
        self.modrm(0b11, 0, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `shr dst32, imm8` (C1 /5 ib, no REX.W) -- the 32-bit logical right shift by an immediate,
    /// the v2 inline `shr r32, imm8` slot. Count is the pinned drawcolumn value (25) at emit time.
    pub(crate) fn shr_r32_imm8(&mut self, dst: Reg, count: u8) {
        if dst.ext() {
            self.rex(false, false, false, dst.ext());
        }
        self.bytes.push(0xC1);
        self.modrm(0b11, 5, dst.low3());
        self.bytes.push(count);
    }

    /// `and dst32, imm32` (81 /4 id, no REX.W) -- the 32-bit AND-immediate form. Used by the native
    /// memory probe to page-align a guest address (`& !0x0fff`), extract a page offset (`& 0x0fff`),
    /// and mask a page-cache slot index (`& (LINES-1)`). ModRM reg field is /4, distinct from add's
    /// /0. Zero-extends the result to 64 bits (any 32-bit op clears the host register's upper half).
    pub(crate) fn and_r32_imm32(&mut self, dst: Reg, imm: u32) {
        if dst.ext() {
            self.rex(false, false, false, dst.ext());
        }
        self.bytes.push(0x81);
        self.modrm(0b11, 4, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `cmp r32, imm32` (81 /7 id, no REX.W). Byte-exact tested; used by the paged TLB probe
    /// for cpl==3 and similar small-imm checks without needing a 64-bit form.
    #[allow(dead_code)]
    pub(crate) fn cmp_r32_imm32(&mut self, r: Reg, imm: u32) {
        if r.ext() {
            self.rex(false, false, false, r.ext());
        }
        self.bytes.push(0x81);
        self.modrm(0b11, 7, r.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `test r32, imm32` (F7 /0 id, no REX.W) -- a NON-DESTRUCTIVE bit test, the `81`-family shape
    /// of `cmp_r32_imm32` one ModRM `/n` over (test is /0, cmp is /7).
    ///
    /// Exists for the OF-bearing Jcc predicates of the `IZARRAVM_JCC_SHADOW` lowering, whose mask
    /// (`FLAG_OF` = 0x800) does not fit `test_r8_low_imm8`'s byte lane. Every other shadow
    /// predicate masks a bit below 8 and takes the shorter byte form; see `emit_shadow_test`,
    /// which picks between the two purely on the mask.
    pub(crate) fn test_r32_imm32(&mut self, r: Reg, imm: u32) {
        if r.ext() {
            self.rex(false, false, false, r.ext());
        }
        self.bytes.push(0xF7);
        self.modrm(0b11, 0, r.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `or r32, r32` (09 /r, no REX.W) -- dst |= src. Byte-exact tested for the paged probe's
    /// physical = (entry.phys | (linear & 0xfff)).
    #[allow(dead_code)]
    pub(crate) fn or_r32_r32(&mut self, dst: Reg, src: Reg) {
        if dst.ext() || src.ext() {
            self.rex(false, src.ext(), false, dst.ext());
        }
        self.bytes.push(0x09);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `or r32, imm32` (81 /1 id) for completeness with the TLB phys combine path.
    #[allow(dead_code)]
    pub(crate) fn or_r32_imm32(&mut self, dst: Reg, imm: u32) {
        if dst.ext() {
            self.rex(false, false, false, dst.ext());
        }
        self.bytes.push(0x81);
        self.modrm(0b11, 1, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `shl dst32, imm8` (C1 /4 ib, no REX.W) -- the 32-bit logical left shift by an immediate, the
    /// mirror of `shr_r32_imm8` (ModRM reg /4 vs shr's /5). Used by the native memory probe to scale
    /// a page-cache slot index by the entry size (`slot * 16` = `shl 4`).
    pub(crate) fn shl_r32_imm8(&mut self, dst: Reg, count: u8) {
        if dst.ext() {
            self.rex(false, false, false, dst.ext());
        }
        self.bytes.push(0xC1);
        self.modrm(0b11, 4, dst.low3());
        self.bytes.push(count);
    }

    /// `add dst32, src32` (01 /r, no REX.W) -- the 32-bit register-register ADD, mirroring
    /// `mov_r32_r32`'s register/ModRM pattern with opcode 0x01. Used by the native memory probe to
    /// fold a `[base+index]` guest effective address (scale 1) into one host register. Sets host
    /// flags, but the emitted probe does not read them.
    pub(crate) fn add_r32_r32(&mut self, dst: Reg, src: Reg) {
        if src.ext() || dst.ext() {
            self.rex(false, src.ext(), false, dst.ext());
        }
        self.bytes.push(0x01);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// A 32-bit register-register ALU operation. `op` is the x86 ALU group number used by both
    /// the primary opcode families and group 1: ADD=0 through CMP=7.
    pub(crate) fn alu_r32_r32(&mut self, op: u8, dst: Reg, src: Reg) {
        const OPCODES: [u8; 8] = [0x01, 0x09, 0x11, 0x19, 0x21, 0x29, 0x31, 0x39];
        let opcode = OPCODES[usize::from(op)];
        if src.ext() || dst.ext() {
            self.rex(false, src.ext(), false, dst.ext());
        }
        self.bytes.push(opcode);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// A 16-bit register-register ALU operation. The operation numbers match `alu_r32_r32`.
    pub(crate) fn alu_r16_r16(&mut self, op: u8, dst: Reg, src: Reg) {
        const OPCODES: [u8; 8] = [0x01, 0x09, 0x11, 0x19, 0x21, 0x29, 0x31, 0x39];
        assert!(op < 8, "ALU group must fit three bits");
        self.bytes.push(0x66);
        self.optional_rex(false, src.ext(), false, dst.ext());
        self.bytes.push(OPCODES[usize::from(op)]);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// A 16-bit register-immediate ALU operation, `66 [REX] 81 /op iw`. The operation numbers
    /// match `alu_r32_imm32`.
    ///
    /// The 16-bit width is the whole point: an x86-64 16-bit register operation writes only the
    /// low 16 bits and PRESERVES bits 31 to 16, where the 32-bit form would zero-extend. That is
    /// exactly `write_gpr16`'s `(slot & 0xffff_0000) | value`, so a 16-bit stack pointer update
    /// needs no masking or merging around it.
    ///
    /// The register form uses `modrm(0b11, ..)` and therefore never needs a SIB byte. That
    /// matters here specifically: the guest ESP home is `Reg::R12`, whose `low3()` is the SIB
    /// escape, so a memory-form encoding would emit a spurious `0x24` and desync the stream.
    pub(crate) fn alu_r16_imm16(&mut self, op: u8, dst: Reg, imm: u16) {
        assert!(op < 8, "ALU group must fit three bits");
        self.bytes.push(0x66);
        self.optional_rex(false, false, false, dst.ext());
        self.bytes.push(0x81);
        self.modrm(0b11, op, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `MOV r16, imm16`, register destination. Defines bits 15..0 and leaves 31..16 alone, which
    /// is what a guest `MOV r16, imm16` does and what a 32-bit move would get wrong.
    ///
    /// Deliberately `C7 /0` rather than the shorter `B8+r`. Both are legal, but the prefix order
    /// is the hazard worth designing against: `66` must come BEFORE the REX, because a REX is
    /// honoured only when it immediately precedes the opcode. Seven of the eight guest homes are
    /// extended registers, so a `41 66 ...` sequence would not fault, it would silently address a
    /// different host register and therefore a different GUEST register. Sitting in the
    /// `C7`-shaped neighbourhood means this diffs cleanly against `alu_r16_imm16` and
    /// `shift_r16_imm8` above, which already order their prefixes correctly, rather than against
    /// `mov_r32_imm32`, which pushes its REX first because it has no `66` to order against.
    ///
    /// The register form uses `modrm(0b11, ..)` and so never needs a SIB byte, which matters for
    /// the same reason it does in `alu_r16_imm16`: the guest ESP home is `Reg::R12`, whose
    /// `low3()` is the SIB escape.
    pub(crate) fn mov_r16_imm16(&mut self, dst: Reg, imm: u16) {
        self.bytes.push(0x66);
        self.optional_rex(false, false, false, dst.ext());
        self.bytes.push(0xc7);
        self.modrm(0b11, 0, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// An 8-bit register-register ALU operation using AL, CL, DL, or BL.
    pub(crate) fn alu_r8_r8(&mut self, op: u8, dst: Reg, src: Reg) {
        const OPCODES: [u8; 8] = [0x00, 0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x38];
        assert!(op < 8, "ALU group must fit three bits");
        assert!(
            dst.0 < 4 && src.0 < 4,
            "byte ALU scratch registers must be AL through BL"
        );
        self.bytes.push(OPCODES[usize::from(op)]);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// A 32-bit group-1 immediate ALU operation (`81 /op id`).
    pub(crate) fn alu_r32_imm32(&mut self, op: u8, dst: Reg, imm: u32) {
        assert!(op < 8, "ALU group must fit three bits");
        if dst.ext() {
            self.rex(false, false, false, true);
        }
        self.bytes.push(0x81);
        self.modrm(0b11, op, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `test dst32, src32` (85 /r).
    pub(crate) fn test_r32_r32(&mut self, dst: Reg, src: Reg) {
        if src.ext() || dst.ext() {
            self.rex(false, src.ext(), false, dst.ext());
        }
        self.bytes.push(0x85);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// A 32-bit group-2 immediate shift (`C1 /op ib`).
    pub(crate) fn shift_r32_imm8(&mut self, op: u8, dst: Reg, count: u8) {
        assert!(op < 8, "shift group must fit three bits");
        if dst.ext() {
            self.rex(false, false, false, true);
        }
        self.bytes.push(0xC1);
        self.modrm(0b11, op, dst.low3());
        self.bytes.push(count);
    }

    /// An 8-bit group-2 immediate shift (`C0 /op ib`).
    ///
    /// Restricted to AL through BL for the reason `alu_r8_r8` is: without a REX prefix, ModRM
    /// register numbers 4 through 7 name AH/CH/DH/BH, and the emitter's byte lane works in
    /// scratch registers rather than encoding a guest byte lane directly. Asserting is what keeps
    /// a future caller from reaching for RSP through this and silently encoding AH.
    ///
    /// The 8-bit width is the point, exactly as the 16-bit width is for `shift_r16_imm8`: the host
    /// computes every flag against the 8-bit operand -- CF from bit 7 for a left shift and bit 0
    /// for a right one, SF from bit 7, ZF and PF from the 8-bit result -- so it does the narrowing
    /// the interpreter's `BusWidth::Byte` does instead of the emitter reconstructing it.
    ///
    /// The count byte is passed through verbatim; the host applies the architectural five-bit mask
    /// itself, and the caller must not pre-mask differently.
    pub(crate) fn shift_r8_imm8(&mut self, op: u8, dst: Reg, count: u8) {
        assert!(op < 8, "shift group must fit three bits");
        assert!(
            dst.0 < 4,
            "byte shift scratch registers must be AL through BL"
        );
        self.bytes.push(0xC0);
        self.modrm(0b11, op, dst.low3());
        self.bytes.push(count);
    }

    /// An 8-bit group-2 shift/rotate by CL (`D2 /op`) -- the CL-count twin of `shift_r8_imm8`, and
    /// it exists for the count-lane emitter, whose count is runtime data and cannot be encoded as
    /// an immediate at all.
    ///
    /// Restricted to AL through BL for `shift_r8_imm8`'s reason: without a REX prefix, ModRM
    /// register numbers 4 through 7 name AH/CH/DH/BH, and the emitter's byte lane works in scratch
    /// registers rather than encoding a guest byte lane directly.
    ///
    /// The 8-bit width is the point, exactly as it is for the imm8 form: the host computes every
    /// flag against the 8-bit operand -- CF from bit 7 for a left shift and bit 0 for a right one,
    /// SF from bit 7, ZF and PF from the 8-bit result -- so it does the narrowing the interpreter's
    /// `BusWidth::Byte` does instead of the emitter reconstructing it.
    ///
    /// The host applies the architectural five-bit count mask to CL itself, exactly as it does for
    /// the imm8 form. The count-lane emitter masks anyway, because it must SELECT on the masked
    /// value; the two masks agree, so the extra one is a shape test and never a semantic change.
    pub(crate) fn shift_r8_cl(&mut self, op: u8, dst: Reg) {
        assert!(op < 8, "shift group must fit three bits");
        assert!(
            dst.0 < 4,
            "byte shift scratch registers must be AL through BL"
        );
        self.bytes.push(0xD2);
        self.modrm(0b11, op, dst.low3());
    }

    /// A 16-bit group-2 immediate shift (`66 [REX] C1 /op ib`).
    ///
    /// The 16-bit width is the whole point, exactly as it is for `alu_r16_imm16`: an x86-64 16-bit
    /// shift writes only the low 16 bits and PRESERVES bits 31 to 16, which is `write_gpr16`'s
    /// `(slot & 0xffff_0000) | value`. It also computes every flag against the 16-bit operand --
    /// CF from bit 15 for a left shift and bit 0 for a right one, SF from bit 15, ZF and PF from
    /// the 16-bit result -- so the host does the width narrowing the interpreter's `BusWidth::Word`
    /// does, rather than the emitter having to reconstruct it.
    ///
    /// The count byte is passed through verbatim; the host applies the architectural five-bit mask
    /// itself, at Word size as at Dword, and the caller must not pre-mask differently.
    pub(crate) fn shift_r16_imm8(&mut self, op: u8, dst: Reg, count: u8) {
        assert!(op < 8, "shift group must fit three bits");
        self.bytes.push(0x66);
        self.optional_rex(false, false, false, dst.ext());
        self.bytes.push(0xC1);
        self.modrm(0b11, op, dst.low3());
        self.bytes.push(count);
    }

    /// A 32-bit SHLD/SHRD register form. `count=None` selects CL; otherwise the supplied imm8 is
    /// encoded verbatim and the host applies the architectural five-bit count mask.
    pub(crate) fn double_shift_r32(&mut self, left: bool, dst: Reg, src: Reg, count: Option<u8>) {
        self.optional_rex(false, src.ext(), false, dst.ext());
        self.bytes.extend_from_slice(&[
            0x0f,
            match (left, count.is_some()) {
                (true, true) => 0xa4,
                (true, false) => 0xa5,
                (false, true) => 0xac,
                (false, false) => 0xad,
            },
        ]);
        self.modrm(0b11, src.low3(), dst.low3());
        if let Some(count) = count {
            self.bytes.push(count);
        }
    }

    /// `shr dst32, cl` (D3 /5).
    pub(crate) fn shr_r32_cl(&mut self, dst: Reg) {
        self.shift_r32_cl(5, dst);
    }

    /// A 32-bit shift/rotate by CL (D3 /op). The host applies the architectural five-bit count
    /// mask itself, exactly as it does for the imm8 form in `shift_r32_imm8`.
    pub(crate) fn shift_r32_cl(&mut self, op: u8, dst: Reg) {
        assert!(op < 8, "shift group must fit three bits");
        if dst.ext() {
            self.rex(false, false, false, true);
        }
        self.bytes.push(0xD3);
        self.modrm(0b11, op, dst.low3());
    }

    /// `movzx dst32, byte [base + index]` (0F B6 /r, mod=00, rm=100 SIB, scale=1, no REX.W) -- load a
    /// byte from `[base + index]` and zero-extend it to 32 bits (clearing the host register's upper 64
    /// bits). The native byte-load probe uses this for the final deref off the host page pointer plus
    /// the in-page offset. Same SIB constraints as `load_r32_sib`: base must not be RBP/R13 (SIB
    /// base=101, mod=00 means "disp32, no base") and index must not be RSP (SIB index=100 means "no
    /// index").
    pub(crate) fn movzx_r32_byte_sib(&mut self, dst: Reg, base: Reg, index: Reg) {
        assert!(base.low3() != 0b101, "SIB base RBP/R13 needs a disp form");
        assert!(index.low3() != 0b100, "SIB index RSP means no-index");
        if dst.ext() || index.ext() || base.ext() {
            self.rex(false, dst.ext(), index.ext(), base.ext());
        }
        self.bytes.push(0x0F);
        self.bytes.push(0xB6);
        self.modrm(0b00, dst.low3(), 0b100); // rm=100 -> a SIB byte follows
        self.bytes.push((index.low3() << 3) | base.low3()); // scale=00
    }

    /// `movzx dst32, byte [base + disp8]` (0F B6 /r). Used after the direct-map probe has
    /// reduced a guest byte read to one checked host pointer.
    pub(crate) fn movzx_r32_byte_disp8(&mut self, dst: Reg, base: Reg, disp8: i8) {
        if dst.ext() || base.ext() {
            self.rex(false, dst.ext(), false, base.ext());
        }
        self.bytes.push(0x0F);
        self.bytes.push(0xB6);
        self.modrm(0b01, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `movzx dst32, word [base + disp8]` (0F B7 /r).
    pub(crate) fn movzx_r32_word_disp8(&mut self, dst: Reg, base: Reg, disp8: i8) {
        self.optional_rex(false, dst.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xB7]);
        self.modrm(0b01, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `mov [base + disp8], src8` (88 /r, mod=01 disp8, SIB if `base` is RSP/R12) -- store the low
    /// byte of `src` to `[base + disp8]`. The native byte-load probe uses this to write a loaded byte
    /// into a guest register's byte lane in the `Registers` array (low byte at `4*i`, high byte at
    /// `4*(i-4)+1`), which IS the `write_gpr8` semantics (the surrounding 24/16 bits are untouched).
    /// `src` must be one of RAX/RCX/RDX/RBX (low3 < 4): those name AL/CL/DL/BL with no REX; a
    /// register with low3 >= 4 (SPL/BPL/SIL/DIL) would need a REX prefix to name its low byte, which
    /// this form does not emit (the probe only ever passes a scratch in that range).
    pub(crate) fn store_r8_disp8(&mut self, base: Reg, disp8: i8, src: Reg) {
        // A hard assert (not debug_assert), matching this file's SIB guards: the REX logic below only
        // consults `base`, so a src with low3 >= 4 would silently encode the wrong byte register
        // (e.g. RSI -> `mov [rax],dh` with no REX) - a wrong-code bug with no runtime signal. Must
        // fail in release too, since the emitter runs in the release JIT.
        assert!(
            src.low3() < 4 && !src.ext(),
            "store_r8_disp8 src must be AL/CL/DL/BL (no REX byte-register)"
        );
        if base.ext() {
            self.rex(false, false, false, base.ext());
        }
        self.bytes.push(0x88);
        self.modrm(0b01, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `mov word [base + disp8], src16` (66 + 89 /r).
    pub(crate) fn store_r16_disp8(&mut self, base: Reg, disp8: i8, src: Reg) {
        self.bytes.push(0x66);
        self.optional_rex(false, src.ext(), false, base.ext());
        self.bytes.push(0x89);
        self.modrm(0b01, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `cmp reg32, [base + disp8]` (3B /r, mod=01 disp8, no REX.W, SIB if `base` is RSP/R12) -- compare
    /// a 32-bit register against a memory dword, setting flags (for a following `jnz`/`jz`). The native
    /// memory probe uses this to compare the page-cache entry's `physical_page` field against the
    /// computed guest page in a register, without spending a 4th scratch to load the field first. Same
    /// ModRM/REX/SIB shape as `load_r32_disp8` (8B), opcode 0x3B (CMP r32, r/m32).
    pub(crate) fn cmp_r32_disp8(&mut self, reg: Reg, base: Reg, disp8: i8) {
        if reg.ext() || base.ext() {
            self.rex(false, reg.ext(), false, base.ext());
        }
        self.bytes.push(0x3B);
        self.modrm(0b01, reg.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `cmp reg8, [base + disp8]` (3A /r). The source must name AL/CL/DL/BL.
    pub(crate) fn cmp_r8_disp8(&mut self, reg: Reg, base: Reg, disp8: i8) {
        assert!(
            reg.low3() < 4 && !reg.ext(),
            "cmp_r8_disp8 register must be AL/CL/DL/BL"
        );
        if base.ext() {
            self.rex(false, false, false, true);
        }
        self.bytes.push(0x3A);
        self.modrm(0b01, reg.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `test byte [base + disp8], imm8` (F6 /0 ib, mod=01 disp8, SIB if `base` is RSP/R12) --
    /// test a memory byte against an immediate mask, setting ZF for a following `jz`/`jnz`,
    /// without a register load first. The x87 link-relaxation boundary check reads a LinkCell's
    /// spilling flag straight out of the cell address already sitting in `base`.
    pub(crate) fn test_byte_disp8_imm8(&mut self, base: Reg, disp8: i8, imm8: u8) {
        if base.ext() {
            self.rex(false, false, false, true);
        }
        self.bytes.push(0xF6);
        self.modrm(0b01, 0, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
        self.bytes.push(imm8);
    }

    /// `setcc dst8` (0F 90+cc /0, mod=11) -- write 1 or 0 into the low byte of `dst` from the
    /// host flags, leaving the other 24 bits alone. The condition encoding is x86's own four-bit
    /// code, the same one `jcc` takes and the same one the guest's ModRM-less opcode low nibble
    /// carries, so no translation is needed at any layer.
    ///
    /// A REX prefix is emitted for RSP/RBP/RSI/RDI even without an extension bit, because without
    /// one those encodings name AH/CH/DH/BH instead of SPL/BPL/SIL/DIL. That is inert for RDX,
    /// the only register this has a caller for today, and it is here so the next caller cannot
    /// silently write the wrong lane.
    pub(crate) fn setcc(&mut self, condition: u8, dst: Reg) {
        assert!(condition < 16, "condition code must fit four bits");
        if dst.ext() || matches!(dst.low3(), 4..=7) {
            self.rex(false, false, false, dst.ext());
        }
        self.bytes.push(0x0F);
        self.bytes.push(0x90 | condition);
        self.modrm(0b11, 0, dst.low3());
    }

    /// `cmp reg16, word [base + disp8]` (66 + 3B /r).
    pub(crate) fn cmp_r16_disp8(&mut self, reg: Reg, base: Reg, disp8: i8) {
        self.bytes.push(0x66);
        self.optional_rex(false, reg.ext(), false, base.ext());
        self.bytes.push(0x3B);
        self.modrm(0b01, reg.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `add qword [base + disp8], src64` (REX.W + 01 /r).
    pub(crate) fn add_r64_to_mem_disp8(&mut self, base: Reg, disp8: i8, src: Reg) {
        self.rex(true, src.ext(), false, base.ext());
        self.bytes.push(0x01);
        self.modrm(0b01, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.push(disp8 as u8);
    }

    /// `bt qword [base], index64` (REX.W + 0F A3 /r). A memory bit index addresses a bit string,
    /// so indices beyond 63 advance through consecutive bitmap words.
    pub(crate) fn bt_r64_mem(&mut self, base: Reg, index: Reg) {
        assert!(base.low3() != 0b101, "BT base RBP/R13 needs a disp form");
        self.rex(true, index.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xA3]);
        self.modrm(0b00, index.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
    }

    /// `bt rm32, index32` (0F A3 /r) with a REGISTER destination.
    ///
    /// Unlike the memory forms below this takes `mod == 0b11`, so it needs no SIB byte and no
    /// RBP/R13 displacement guard, and it must NOT set REX.W: the operand is 32-bit and the host
    /// takes the bit offset modulo 32, which is exactly the guest's `index & 31`.
    pub(crate) fn bt_r32_r32(&mut self, rm: Reg, index: Reg) {
        self.optional_rex(false, index.ext(), false, rm.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xA3]);
        self.modrm(0b11, index.low3(), rm.low3());
    }

    /// `bts qword [base], index64` (REX.W + 0F AB /r).
    pub(crate) fn bts_r64_mem(&mut self, base: Reg, index: Reg) {
        assert!(base.low3() != 0b101, "BTS base RBP/R13 needs a disp form");
        self.rex(true, index.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xAB]);
        self.modrm(0b00, index.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
    }

    /// `and dst64, src64` (REX.W + 21 /r).
    pub(crate) fn and_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.ext(), false, dst.ext());
        self.bytes.push(0x21);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// `xor dst64, src64` (REX.W + 31 /r).
    pub(crate) fn xor_r64_r64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.ext(), false, dst.ext());
        self.bytes.push(0x31);
        self.modrm(0b11, src.low3(), dst.low3());
    }

    /// A 64-bit group-2 immediate shift (`REX.W + C1 /op ib`).
    pub(crate) fn shift_r64_imm8(&mut self, op: u8, dst: Reg, count: u8) {
        assert!(op < 8, "shift group must fit three bits");
        self.rex(true, false, false, dst.ext());
        self.bytes.push(0xC1);
        self.modrm(0b11, op, dst.low3());
        self.bytes.push(count);
    }

    /// `bt dst64, imm8` (REX.W 0F BA /4 ib): copy bit `index` of `dst` into CF and touch nothing
    /// else. The call-out status word is read this way rather than by shifting because bits 33 and
    /// 34 have to be distinguished from each other and from bit 32 while RAX still holds the whole
    /// status: a shift would fold them together.
    pub(crate) fn bt_r64_imm8(&mut self, dst: Reg, index: u8) {
        debug_assert!(index < 64, "a quadword bit index must fit six bits");
        self.rex(true, false, false, dst.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xBA]);
        self.modrm(0b11, 4, dst.low3());
        self.bytes.push(index);
    }

    /// `movzx dst32, word [base + disp32]` (0F B7 /r).
    pub(crate) fn movzx_r32_word_disp32(&mut self, dst: Reg, base: Reg, disp32: i32) {
        self.optional_rex(false, dst.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, 0xB7]);
        self.modrm(0b10, dst.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `mov word [base + disp32], src16` (66 + 89 /r).
    pub(crate) fn store_r16_disp32(&mut self, base: Reg, disp32: i32, src: Reg) {
        self.bytes.push(0x66);
        self.optional_rex(false, src.ext(), false, base.ext());
        self.bytes.push(0x89);
        self.modrm(0b10, src.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    fn bit_r16_mem(&mut self, opcode: u8, base: Reg, index: Reg) {
        assert!(
            base.low3() != 0b101,
            "word bit-operation base RBP/R13 needs a disp form"
        );
        self.bytes.push(0x66);
        self.optional_rex(false, index.ext(), false, base.ext());
        self.bytes.extend_from_slice(&[0x0F, opcode]);
        self.modrm(0b00, index.low3(), base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
    }

    /// `bt word [base], index16` (66 + 0F A3 /r).
    pub(crate) fn bt_r16_mem(&mut self, base: Reg, index: Reg) {
        self.bit_r16_mem(0xA3, base, index);
    }

    /// `btr word [base], index16` (66 + 0F B3 /r).
    pub(crate) fn btr_r16_mem(&mut self, base: Reg, index: Reg) {
        self.bit_r16_mem(0xB3, base, index);
    }

    /// `bts word [base], index16` (66 + 0F AB /r).
    pub(crate) fn bts_r16_mem(&mut self, base: Reg, index: Reg) {
        self.bit_r16_mem(0xAB, base, index);
    }

    /// `vmovupd dst, ymmword [base + disp32]`.
    pub(crate) fn vmovupd_ymm_disp32(&mut self, dst: Ymm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 1, false, true, 0x10),
            dst.0,
            None,
            base,
            disp32,
        );
    }

    /// `vmovupd ymmword [base + disp32], src`.
    pub(crate) fn vmovupd_disp32_ymm(&mut self, base: Reg, disp32: i32, src: Ymm) {
        self.vex_mem_disp32(
            VexOp::new(1, 1, false, true, 0x11),
            src.0,
            None,
            base,
            disp32,
        );
    }

    /// `vmovupd dst, xmmword [base + disp32]`.
    pub(crate) fn vmovupd_xmm_disp32(&mut self, dst: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 1, false, false, 0x10),
            dst.0,
            None,
            base,
            disp32,
        );
    }

    /// `vmovupd xmmword [base + disp32], src`.
    pub(crate) fn vmovupd_disp32_xmm(&mut self, base: Reg, disp32: i32, src: Xmm) {
        self.vex_mem_disp32(
            VexOp::new(1, 1, false, false, 0x11),
            src.0,
            None,
            base,
            disp32,
        );
    }

    /// `vpermpd dst, src, imm8`.
    pub(crate) fn vpermpd(&mut self, dst: Ymm, src: Ymm, imm: u8) {
        self.vex_reg_rm(VexOp::new(3, 1, true, true, 0x01), dst.0, None, src.0);
        self.bytes.push(imm);
    }

    /// `vblendpd dst, src1, src2, imm8`.
    pub(crate) fn vblendpd(&mut self, dst: Ymm, src1: Ymm, src2: Ymm, imm: u8) {
        self.vex_reg_rm(
            VexOp::new(3, 1, false, true, 0x0d),
            dst.0,
            Some(src1.0),
            src2.0,
        );
        self.bytes.push(imm);
    }

    /// `vbroadcastsd dst, src`. The register-source form requires AVX2.
    pub(crate) fn vbroadcastsd(&mut self, dst: Ymm, src: Xmm) {
        self.vex_reg_rm(VexOp::new(2, 1, false, true, 0x19), dst.0, None, src.0);
    }

    fn vex_scalar_binary(&mut self, opcode: u8, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_reg_rm(
            VexOp::new(1, 3, false, false, opcode),
            dst.0,
            Some(src1.0),
            src2.0,
        );
    }

    /// `vaddsd dst, src1, src2`.
    pub(crate) fn vaddsd(&mut self, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_scalar_binary(0x58, dst, src1, src2);
    }

    /// `vmulsd dst, src1, src2`.
    pub(crate) fn vmulsd(&mut self, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_scalar_binary(0x59, dst, src1, src2);
    }

    /// `vsubsd dst, src1, src2`.
    pub(crate) fn vsubsd(&mut self, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_scalar_binary(0x5c, dst, src1, src2);
    }

    /// `vdivsd dst, src1, src2`.
    pub(crate) fn vdivsd(&mut self, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_scalar_binary(0x5e, dst, src1, src2);
    }

    /// `vsqrtsd dst, src1, src2`. Unary in effect but three-operand in encoding: like every
    /// scalar VEX form the upper lane comes from `src1`, so callers that want a pure unary pass
    /// the same register for `src1` and `src2`.
    pub(crate) fn vsqrtsd(&mut self, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_scalar_binary(0x51, dst, src1, src2);
    }

    /// `vucomisd lhs, rhs`.
    pub(crate) fn vucomisd(&mut self, lhs: Xmm, rhs: Xmm) {
        self.vex_reg_rm(VexOp::new(1, 1, false, false, 0x2e), lhs.0, None, rhs.0);
    }

    /// `vmovsd dst, merge, src`.
    pub(crate) fn vmovsd_xmm_xmm(&mut self, dst: Xmm, merge: Xmm, src: Xmm) {
        self.vex_reg_rm(
            VexOp::new(1, 3, false, false, 0x10),
            dst.0,
            Some(merge.0),
            src.0,
        );
    }

    /// `vmovsd dst, qword [base + disp32]`.
    pub(crate) fn vmovsd_xmm_disp32(&mut self, dst: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 3, false, false, 0x10),
            dst.0,
            None,
            base,
            disp32,
        );
    }

    /// `vmovsd qword [base + disp32], src`.
    pub(crate) fn vmovsd_disp32_xmm(&mut self, base: Reg, disp32: i32, src: Xmm) {
        self.vex_mem_disp32(
            VexOp::new(1, 3, false, false, 0x11),
            src.0,
            None,
            base,
            disp32,
        );
    }

    /// `vmovss dst, merge, src`.
    pub(crate) fn vmovss_xmm_xmm(&mut self, dst: Xmm, merge: Xmm, src: Xmm) {
        self.vex_reg_rm(
            VexOp::new(1, 2, false, false, 0x10),
            dst.0,
            Some(merge.0),
            src.0,
        );
    }

    /// `vmovss dst, dword [base + disp32]`.
    pub(crate) fn vmovss_xmm_disp32(&mut self, dst: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 2, false, false, 0x10),
            dst.0,
            None,
            base,
            disp32,
        );
    }

    /// `vmovss dword [base + disp32], src`.
    pub(crate) fn vmovss_disp32_xmm(&mut self, base: Reg, disp32: i32, src: Xmm) {
        self.vex_mem_disp32(
            VexOp::new(1, 2, false, false, 0x11),
            src.0,
            None,
            base,
            disp32,
        );
    }

    /// `vroundsd dst, merge, src, imm8`.
    pub(crate) fn vroundsd(&mut self, dst: Xmm, merge: Xmm, src: Xmm, imm: u8) {
        self.vex_reg_rm(
            VexOp::new(3, 1, false, false, 0x0b),
            dst.0,
            Some(merge.0),
            src.0,
        );
        self.bytes.push(imm);
    }

    /// `vcvtss2sd dst, merge, src`.
    pub(crate) fn vcvtss2sd(&mut self, dst: Xmm, merge: Xmm, src: Xmm) {
        self.vex_reg_rm(
            VexOp::new(1, 2, false, false, 0x5a),
            dst.0,
            Some(merge.0),
            src.0,
        );
    }

    /// `vcvtss2sd dst, merge, dword [base + disp32]`.
    pub(crate) fn vcvtss2sd_disp32(&mut self, dst: Xmm, merge: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 2, false, false, 0x5a),
            dst.0,
            Some(merge.0),
            base,
            disp32,
        );
    }

    /// `vcvtsd2ss dst, merge, src`.
    pub(crate) fn vcvtsd2ss(&mut self, dst: Xmm, merge: Xmm, src: Xmm) {
        self.vex_reg_rm(
            VexOp::new(1, 3, false, false, 0x5a),
            dst.0,
            Some(merge.0),
            src.0,
        );
    }

    /// `vcvtsd2ss dst, merge, qword [base + disp32]`.
    pub(crate) fn vcvtsd2ss_disp32(&mut self, dst: Xmm, merge: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 3, false, false, 0x5a),
            dst.0,
            Some(merge.0),
            base,
            disp32,
        );
    }

    fn vcvtsi2sd_reg(&mut self, dst: Xmm, merge: Xmm, src: Reg, wide: bool) {
        self.vex_reg_rm(
            VexOp::new(1, 3, wide, false, 0x2a),
            dst.0,
            Some(merge.0),
            src.0,
        );
    }

    /// `vcvtsi2sd dst, merge, src32`.
    pub(crate) fn vcvtsi2sd_r32(&mut self, dst: Xmm, merge: Xmm, src: Reg) {
        self.vcvtsi2sd_reg(dst, merge, src, false);
    }

    /// `vcvtsi2sd dst, merge, src64`.
    pub(crate) fn vcvtsi2sd_r64(&mut self, dst: Xmm, merge: Xmm, src: Reg) {
        self.vcvtsi2sd_reg(dst, merge, src, true);
    }

    /// `vcvtsi2sd dst, merge, dword [base + disp32]`.
    pub(crate) fn vcvtsi2sd_i32_disp32(&mut self, dst: Xmm, merge: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 3, false, false, 0x2a),
            dst.0,
            Some(merge.0),
            base,
            disp32,
        );
    }

    /// `vcvtsi2sd dst, merge, qword [base + disp32]`. The REX.W sibling of
    /// `vcvtsi2sd_i32_disp32`: same map/pp/opcode, `w` set true so `vex_prefix` forces the
    /// 3-byte C4 form and the memory operand is read as a 64-bit integer instead of 32-bit.
    pub(crate) fn vcvtsi2sd_i64_disp32(&mut self, dst: Xmm, merge: Xmm, base: Reg, disp32: i32) {
        self.vex_mem_disp32(
            VexOp::new(1, 3, true, false, 0x2a),
            dst.0,
            Some(merge.0),
            base,
            disp32,
        );
    }

    fn vcvttsd2si(&mut self, dst: Reg, src: Xmm, wide: bool) {
        self.vex_reg_rm(VexOp::new(1, 3, wide, false, 0x2c), dst.0, None, src.0);
    }

    /// `vcvttsd2si dst32, src`.
    pub(crate) fn vcvttsd2si_r32(&mut self, dst: Reg, src: Xmm) {
        self.vcvttsd2si(dst, src, false);
    }

    /// `vcvttsd2si dst64, src`.
    pub(crate) fn vcvttsd2si_r64(&mut self, dst: Reg, src: Xmm) {
        self.vcvttsd2si(dst, src, true);
    }

    /// `vmovq dst, src64`.
    pub(crate) fn vmovq_xmm_r64(&mut self, dst: Xmm, src: Reg) {
        self.vex_reg_rm(VexOp::new(1, 1, true, false, 0x6e), dst.0, None, src.0);
    }

    /// `vmovq dst64, src`.
    pub(crate) fn vmovq_r64_xmm(&mut self, dst: Reg, src: Xmm) {
        self.vex_reg_rm(VexOp::new(1, 1, true, false, 0x7e), src.0, None, dst.0);
    }

    /// `vxorpd dst, src1, src2`.
    pub(crate) fn vxorpd(&mut self, dst: Xmm, src1: Xmm, src2: Xmm) {
        self.vex_reg_rm(
            VexOp::new(1, 1, false, false, 0x57),
            dst.0,
            Some(src1.0),
            src2.0,
        );
    }

    /// End an AVX block before returning to code that may use legacy SSE.
    pub(crate) fn vzeroupper(&mut self) {
        self.vex_prefix(
            VexOp::new(1, 0, false, false, 0x77),
            false,
            false,
            false,
            None,
        );
        self.bytes.push(0x77);
    }

    /// `movsd dst, qword [base + disp32]` (F2 + 0F 10 /r).
    pub(crate) fn movsd_xmm_disp32(&mut self, dst: Xmm, base: Reg, disp32: i32) {
        self.scalar_xmm_mem_disp32(0xF2, 0x10, dst, base, disp32);
    }

    /// `movsd qword [base + disp32], src` (F2 + 0F 11 /r).
    pub(crate) fn movsd_disp32_xmm(&mut self, base: Reg, disp32: i32, src: Xmm) {
        self.scalar_xmm_mem_disp32(0xF2, 0x11, src, base, disp32);
    }

    /// `movsd dst, src` (F2 + 0F 10 /r).
    pub(crate) fn movsd_xmm_xmm(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x10, dst, src);
    }

    /// `movss dst, dword [base + disp32]` (F3 + 0F 10 /r).
    pub(crate) fn movss_xmm_disp32(&mut self, dst: Xmm, base: Reg, disp32: i32) {
        self.scalar_xmm_mem_disp32(0xF3, 0x10, dst, base, disp32);
    }

    /// `movss dword [base + disp32], src` (F3 + 0F 11 /r).
    pub(crate) fn movss_disp32_xmm(&mut self, base: Reg, disp32: i32, src: Xmm) {
        self.scalar_xmm_mem_disp32(0xF3, 0x11, src, base, disp32);
    }

    /// `movsd dst, qword [base + index*8 + disp32]`.
    pub(crate) fn movsd_xmm_sib_scale8_disp32(
        &mut self,
        dst: Xmm,
        base: Reg,
        index: Reg,
        disp32: i32,
    ) {
        self.scalar_xmm_mem_sib_scale8_disp32(0xF2, 0x10, dst, base, index, disp32);
    }

    /// `movsd qword [base + index*8 + disp32], src`.
    pub(crate) fn movsd_sib_scale8_disp32_xmm(
        &mut self,
        base: Reg,
        index: Reg,
        disp32: i32,
        src: Xmm,
    ) {
        self.scalar_xmm_mem_sib_scale8_disp32(0xF2, 0x11, src, base, index, disp32);
    }

    /// `cvtss2sd dst, src` (F3 + 0F 5A /r).
    pub(crate) fn cvtss2sd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF3, 0x5A, dst, src);
    }

    /// `cvtsd2ss dst, src` (F2 + 0F 5A /r).
    pub(crate) fn cvtsd2ss(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x5A, dst, src);
    }

    /// `addsd dst, src` (F2 + 0F 58 /r).
    pub(crate) fn addsd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x58, dst, src);
    }

    /// `mulsd dst, src` (F2 + 0F 59 /r).
    pub(crate) fn mulsd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x59, dst, src);
    }

    /// `subsd dst, src` (F2 + 0F 5C /r).
    pub(crate) fn subsd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x5C, dst, src);
    }

    /// `divsd dst, src` (F2 + 0F 5E /r).
    pub(crate) fn divsd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x5E, dst, src);
    }

    /// `sqrtsd dst, src` (F2 + 0F 51 /r).
    pub(crate) fn sqrtsd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0xF2, 0x51, dst, src);
    }

    /// `ucomisd lhs, rhs` (66 + 0F 2E /r).
    pub(crate) fn ucomisd(&mut self, lhs: Xmm, rhs: Xmm) {
        self.scalar_xmm_reg_reg(0x66, 0x2E, lhs, rhs);
    }

    /// `xorpd dst, src` (66 + 0F 57 /r).
    pub(crate) fn xorpd(&mut self, dst: Xmm, src: Xmm) {
        self.scalar_xmm_reg_reg(0x66, 0x57, dst, src);
    }

    /// `cvtsi2sd dst, src32` (F2 + 0F 2A /r).
    pub(crate) fn cvtsi2sd_r32(&mut self, dst: Xmm, src: Reg) {
        self.bytes.push(0xF2);
        self.optional_rex(false, dst.ext(), false, src.ext());
        self.bytes.extend_from_slice(&[0x0F, 0x2A]);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    fn cvttsd2si(&mut self, dst: Reg, src: Xmm, wide: bool) {
        self.bytes.push(0xF2);
        self.optional_rex(wide, dst.ext(), false, src.ext());
        self.bytes.extend_from_slice(&[0x0F, 0x2C]);
        self.modrm(0b11, dst.low3(), src.low3());
    }

    /// `cvttsd2si dst32, src` (F2 + 0F 2C /r).
    pub(crate) fn cvttsd2si_r32(&mut self, dst: Reg, src: Xmm) {
        self.cvttsd2si(dst, src, false);
    }

    /// `cvttsd2si dst64, src` (F2 + REX.W + 0F 2C /r).
    pub(crate) fn cvttsd2si_r64(&mut self, dst: Reg, src: Xmm) {
        self.cvttsd2si(dst, src, true);
    }

    fn mov_xmm_gpr(&mut self, opcode: u8, wide: bool, xmm: Xmm, gpr: Reg) {
        self.bytes.push(0x66);
        self.optional_rex(wide, xmm.ext(), false, gpr.ext());
        self.bytes.extend_from_slice(&[0x0F, opcode]);
        self.modrm(0b11, xmm.low3(), gpr.low3());
    }

    /// `movq dst, src64` (66 + REX.W + 0F 6E /r).
    pub(crate) fn movq_xmm_r64(&mut self, dst: Xmm, src: Reg) {
        self.mov_xmm_gpr(0x6E, true, dst, src);
    }

    /// `movq dst64, src` (66 + REX.W + 0F 7E /r).
    pub(crate) fn movq_r64_xmm(&mut self, dst: Reg, src: Xmm) {
        self.mov_xmm_gpr(0x7E, true, src, dst);
    }

    /// `movd dst, src32` (66 + 0F 6E /r).
    pub(crate) fn movd_xmm_r32(&mut self, dst: Xmm, src: Reg) {
        self.mov_xmm_gpr(0x6E, false, dst, src);
    }

    /// `movd dst32, src` (66 + 0F 7E /r).
    pub(crate) fn movd_r32_xmm(&mut self, dst: Reg, src: Xmm) {
        self.mov_xmm_gpr(0x7E, false, src, dst);
    }

    /// `xor dst64, dst64` (REX.W + 31 /r), the standard zero-register idiom.
    pub(crate) fn xor_r64_self(&mut self, dst: Reg) {
        self.rex(true, dst.ext(), false, dst.ext());
        self.bytes.push(0x31);
        self.modrm(0b11, dst.low3(), dst.low3());
    }

    /// `add dst64, imm32` (REX.W + 81 /0 id).
    pub(crate) fn add_r64_imm32(&mut self, dst: Reg, imm: u32) {
        self.rex(true, false, false, dst.ext());
        self.bytes.push(0x81);
        self.modrm(0b11, 0, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `sub dst64, imm32` (REX.W + 81 /5 id).
    pub(crate) fn sub_r64_imm32(&mut self, dst: Reg, imm: u32) {
        self.rex(true, false, false, dst.ext());
        self.bytes.push(0x81);
        self.modrm(0b11, 5, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `cmp r64, imm32` (REX.W + 81 /7 id). Unit-tested but not called by the strcpy block's
    /// emitter: its one runtime comparison is against `cap_clocks` (not a compile-time constant),
    /// which uses `cmp_r64_r64` + `ja` instead (see that method's doc comment). Kept for a future
    /// block that compares against a fixed immediate.
    #[allow(dead_code)]
    pub(crate) fn cmp_r64_imm32(&mut self, r: Reg, imm: u32) {
        self.rex(true, false, false, r.ext());
        self.bytes.push(0x81);
        self.modrm(0b11, 7, r.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `not r64` (REX.W + F7 /2), two's-complement negate-minus-one (used to encode the JIT's
    /// fault-return sentinel `-raw_clocks - 1`).
    pub(crate) fn not_r64(&mut self, r: Reg) {
        self.rex(true, false, false, r.ext());
        self.bytes.push(0xF7);
        self.modrm(0b11, 2, r.low3());
    }

    /// `call r64` (FF /2, indirect through a register).
    pub(crate) fn call_r64(&mut self, target: Reg) {
        if target.ext() {
            self.bytes.push(0x41);
        }
        self.bytes.push(0xFF);
        self.modrm(0b11, 2, target.low3());
    }

    /// `call qword [base + disp32]` (FF /2, mod=10, no REX.W — FF /2 is 64-bit by default in
    /// long mode). The one-lookup store sites use this against R15 slots: 7 bytes, no scratch
    /// register, unlike the 13-byte `mov r64, imm64` + `call r64` call-out pattern.
    pub(crate) fn call_m64_disp32(&mut self, base: Reg, disp32: i32) {
        self.optional_rex(false, false, false, base.ext());
        self.bytes.push(0xFF);
        self.modrm(0b10, 2, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `ret` (C3). Unit-tested but not emitted today: the shared store stubs were designed
    /// around call/ret and BUILT on the pop/jmp-through-slot mechanism instead (which keeps
    /// RSP at the frame level inside the stub), so this is kept for a future caller exactly
    /// as `cmp_r64_imm32` is.
    #[allow(dead_code)]
    pub(crate) fn ret_near(&mut self) {
        self.bytes.push(0xC3);
    }

    /// `and r64, imm32` (REX.W + 81 /4 id), the immediate SIGN-extended to 64 bits — so
    /// `and rdi, -4` clears exactly the low two bits, which is how the mode13 stub strips the
    /// store-bias tags without touching the pointer's high half.
    pub(crate) fn and_r64_imm32(&mut self, dst: Reg, imm: u32) {
        self.rex(true, false, false, dst.ext());
        self.bytes.push(0x81);
        self.modrm(0b11, 4, dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    /// `pop qword [base + disp32]` (8F /0, mod=10). EVERY shared store stub uses this as its
    /// prologue: it moves the CALL's return address into a frame slot AND restores RSP to the
    /// frame level in one instruction, so every frame-offset helper emits at its normal
    /// displacement inside the stub. The displacement is computed against the RESTORED RSP:
    /// with an RSP base, POP computes the operand address after the increment (SDM vol.2,
    /// POP, "address of the operand is computed after the increment").
    pub(crate) fn pop_m64_disp32(&mut self, base: Reg, disp32: i32) {
        self.optional_rex(false, false, false, base.ext());
        self.bytes.push(0x8F);
        self.modrm(0b10, 0, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `jmp qword [base + disp32]` (FF /4, mod=10) — the slow store stubs' epilogue, returning
    /// through the frame slot their `pop` prologue parked the return address in.
    pub(crate) fn jmp_m64_disp32(&mut self, base: Reg, disp32: i32) {
        self.optional_rex(false, false, false, base.ext());
        self.bytes.push(0xFF);
        self.modrm(0b10, 4, base.low3());
        if Self::needs_sib(base) {
            self.bytes.push(0x24);
        }
        self.bytes.extend_from_slice(&disp32.to_le_bytes());
    }

    /// `test r8_low, imm8` (F6 /0 ib) against the LOW byte lane of any GPR, emitting the empty
    /// REX (0x40) for RSP/RBP/RSI/RDI so the encoding selects SPL/BPL/SIL/DIL rather than
    /// AH/CH/DH/BH. The rest of the encoder deliberately avoids these byte registers (guest
    /// high-byte lanes must map to shifts, see `emit_read_store_value`) — this form exists for
    /// HOST-side bit tests on scratch registers, the store-bias tag probe first.
    pub(crate) fn test_r8_low_imm8(&mut self, r: Reg, imm: u8) {
        if r.ext() {
            self.bytes.push(0x41);
        } else if matches!(r, Reg::RSP | Reg::RBP | Reg::RSI | Reg::RDI) {
            self.bytes.push(0x40);
        }
        self.bytes.push(0xF6);
        self.modrm(0b11, 0, r.low3());
        self.bytes.push(imm);
    }

    /// `test al, al` (84 C0) -- tests the low byte of RAX (the C-ABI return register for a `u8`).
    pub(crate) fn test_al_al(&mut self) {
        self.bytes.push(0x84);
        self.bytes.push(0xC0);
    }

    pub(crate) fn pushfq(&mut self) {
        self.bytes.push(0x9C);
    }

    pub(crate) fn popfq(&mut self) {
        self.bytes.push(0x9D);
    }

    /// Emit a near conditional branch using the guest x86 condition-code nibble.
    pub(crate) fn jcc(&mut self, condition: u8, target: Label) {
        assert!(condition < 16, "condition code must fit four bits");
        let instr_start = self.bytes.len();
        self.bytes.push(0x0F);
        self.bytes.push(0x80 | condition);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
        self.queue_or_resolve(instr_start, PatchKind::Rel32AfterJcc, target);
    }

    /// `jz label` (near, rel32; 0F 84 cd). Both forward and backward references are supported:
    /// if `target` is already placed, the offset is computed immediately; otherwise a patch is
    /// queued and resolved in `finish`.
    pub(crate) fn jz(&mut self, target: Label) {
        let instr_start = self.bytes.len();
        self.bytes.push(0x0F);
        self.bytes.push(0x84);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
        self.queue_or_resolve(instr_start, PatchKind::Rel32AfterJcc, target);
    }

    /// `jnz label` (near, rel32; 0F 85 cd). Same shape as `jz`, opposite condition.
    pub(crate) fn jnz(&mut self, target: Label) {
        let instr_start = self.bytes.len();
        self.bytes.push(0x0F);
        self.bytes.push(0x85);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
        self.queue_or_resolve(instr_start, PatchKind::Rel32AfterJcc, target);
    }

    /// `ja label` (near, unsigned above, rel32; 0F 87 cd). Same shape as `jz`/`jnz`, the
    /// unsigned-above condition (CF=0 and ZF=0) -- needed for the strcpy block's runtime
    /// `cap_clocks` comparison, which cannot use `cmp_r64_imm32` (the cap is not a compile-time
    /// constant) and so cannot be tested with plain `jz`/`jnz` either. Unsigned (not the signed
    /// `jg`/0F 8F) because `cap_clocks` is a `u64` whose full range includes values at or above
    /// `i64::MAX` (notably `u64::MAX`, the "no cap" sentinel some callers pass) -- a signed
    /// comparison would misread such a value as negative and exit immediately.
    pub(crate) fn ja(&mut self, target: Label) {
        let instr_start = self.bytes.len();
        self.bytes.push(0x0F);
        self.bytes.push(0x87);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
        self.queue_or_resolve(instr_start, PatchKind::Rel32AfterJcc, target);
    }

    /// `jae label` (near, unsigned above-or-equal, rel32; 0F 83 cd). CF=0, i.e. the unsigned >=
    /// condition. Used by the native cap check (`rem0 + raw >= threshold`).
    pub(crate) fn jae(&mut self, target: Label) {
        let instr_start = self.bytes.len();
        self.bytes.push(0x0F);
        self.bytes.push(0x83);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
        self.queue_or_resolve(instr_start, PatchKind::Rel32AfterJcc, target);
    }

    /// `xor rdx, rdx` then `div src` — unsigned divide RDX:RAX (= RAX zero-extended) by src.
    /// Quotient in RAX, remainder in RDX. This is the two-instruction idiom for u64 / u64.
    /// Requires RDX and RAX as caller-saved scratch; the divisor is `src`.
    pub(crate) fn div_r64(&mut self, src: Reg) {
        // xor rdx, rdx (REX.W + 31 /r, self-xor for zero)
        self.rex(true, Reg::RDX.ext(), false, Reg::RDX.ext());
        self.bytes.push(0x31);
        self.modrm(0b11, Reg::RDX.low3(), Reg::RDX.low3());
        // div src (REX.W + F7 /6)
        self.rex(true, false, false, src.ext());
        self.bytes.push(0xF7);
        self.modrm(0b11, 6, src.low3());
    }

    /// `jmp label` (near, rel32; E9 cd). Unit-tested but not called by the strcpy block's emitter
    /// (every exit from its loop body is a conditional branch -- `ja`/`jnz`/`jz` -- never an
    /// unconditional one). Kept for a future block whose control flow needs it.
    #[allow(dead_code)]
    pub(crate) fn jmp(&mut self, target: Label) {
        let instr_start = self.bytes.len();
        self.bytes.push(0xE9);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
        self.queue_or_resolve(instr_start, PatchKind::Rel32AfterJmp, target);
    }

    /// `jmp r64` (FF /4).
    pub(crate) fn jmp_r64(&mut self, target: Reg) {
        self.optional_rex(false, false, false, target.ext());
        self.bytes.push(0xFF);
        self.modrm(0b11, 4, target.low3());
    }

    pub(crate) fn ret(&mut self) {
        self.bytes.push(0xC3);
    }

    fn queue_or_resolve(&mut self, instr_start: usize, kind: PatchKind, target: Label) {
        if let Some(target_pos) = self.label_positions[target.0] {
            self.patch_now(instr_start, &kind, target_pos);
        } else {
            self.patches.push(Patch {
                instr_start,
                kind,
                target,
            });
        }
    }

    fn instr_end(start: usize, kind: &PatchKind) -> usize {
        match kind {
            PatchKind::Rel32AfterJcc => start + 6, // 0F 8x + rel32
            PatchKind::Rel32AfterJmp => start + 5, // E9 + rel32
        }
    }

    fn patch_now(&mut self, instr_start: usize, kind: &PatchKind, target_pos: usize) {
        let end = Self::instr_end(instr_start, kind);
        let rel = target_pos as i64 - end as i64;
        let rel = i32::try_from(rel).expect("strcpy block is far smaller than 2 GiB");
        self.bytes[end - 4..end].copy_from_slice(&rel.to_le_bytes());
    }

    /// Resolve every still-forward patch (a label referenced before it was placed) and return the
    /// finished byte stream. Panics if a referenced label was never placed -- a programming error
    /// in the emitter, not a runtime condition.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        for patch in std::mem::take(&mut self.patches) {
            let target_pos = self.label_positions[patch.target.0]
                .expect("a jcc/jmp target label was never placed");
            self.patch_now(patch.instr_start, &patch.kind, target_pos);
        }
        self.bytes
    }
}

#[cfg(test)]
#[path = "encoder_test.rs"]
mod tests;
