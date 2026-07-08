//! A minimal direct-byte x86-64 encoder, forked from the seed JIT (tag jit-seed-slice1a). Not a
//! general assembler: only the instruction forms the emitted region chains need, each unit-tested
//! against a hand-derived byte sequence. Comments referencing "the strcpy block" describe the
//! seed emitter these primitives were proven on; the loop-region emitter reuses them unchanged.
//!
//! The v1 region chain uses only a subset (the call/test/branch scaffolding); the rest are kept
//! dead for the planned inlining slices, each already unit-tested below.
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
        // REX prefix is only required when 64-bit-operand-size or an extended (8-15) register is
        // used; Slice 1a always sets W=true for pointer/qword ops, so this is unconditional here.
        let byte =
            0x40 | (u8::from(w) << 3) | (u8::from(r) << 2) | (u8::from(x) << 1) | u8::from(b);
        self.bytes.push(byte);
    }

    fn modrm(&mut self, md: u8, reg: u8, rm: u8) {
        self.bytes.push((md << 6) | ((reg & 7) << 3) | (rm & 7));
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

    /// `mov dst32, imm32` (B8+rd id; REX.B if dst is extended).
    pub(crate) fn mov_r32_imm32(&mut self, dst: Reg, imm: u32) {
        if dst.ext() {
            self.bytes.push(0x41);
        }
        self.bytes.push(0xB8 + dst.low3());
        self.bytes.extend_from_slice(&imm.to_le_bytes());
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

    /// `test al, al` (84 C0) -- tests the low byte of RAX (the C-ABI return register for a `u8`).
    pub(crate) fn test_al_al(&mut self) {
        self.bytes.push(0x84);
        self.bytes.push(0xC0);
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
mod tests {
    use super::*;

    #[test]
    fn push_pop_known_bytes() {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.pop(Reg::R15);
        assert_eq!(
            e.finish(),
            vec![
                0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x41, 0x5F
            ]
        );
    }

    #[test]
    fn mov_r64_r64_known_bytes() {
        // mov r12, rcx ; mov r13, rdx ; mov r15, r8 ; mov rbx, r9
        let mut e = Encoder::new();
        e.mov_r64_r64(Reg::R12, Reg::RCX);
        e.mov_r64_r64(Reg::R13, Reg::RDX);
        e.mov_r64_r64(Reg::R15, Reg::R8);
        e.mov_r64_r64(Reg::RBX, Reg::R9);
        assert_eq!(
            e.finish(),
            vec![
                0x49, 0x89, 0xCC, 0x49, 0x89, 0xD5, 0x4D, 0x89, 0xC7, 0x4C, 0x89, 0xCB
            ]
        );
    }

    #[test]
    fn mov_r32_r32_known_bytes() {
        // Non-extended pair: mov eax, ecx -- no REX byte at all.
        let mut e = Encoder::new();
        e.mov_r32_r32(Reg::RAX, Reg::RCX);
        assert_eq!(e.finish(), vec![0x89, 0xC8]);

        // Extended pair: mov r12d, r9d -- REX present (R from src ext, B from dst ext).
        let mut e = Encoder::new();
        e.mov_r32_r32(Reg::R12, Reg::R9);
        assert_eq!(e.finish(), vec![0x45, 0x89, 0xCC]);
    }

    #[test]
    fn mov_r32_imm32_known_bytes() {
        // mov r9d, 0x12345678 -- REX.B set (R9 is extended), then B8+1, then imm32 LE.
        let mut e = Encoder::new();
        e.mov_r32_imm32(Reg::R9, 0x1234_5678);
        assert_eq!(e.finish(), vec![0x41, 0xB9, 0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn mov_r64_imm64_known_bytes() {
        // mov rbx, 0x0102030405060708 -- REX.W only (RBX not extended), B8+3, imm64 LE.
        let mut e = Encoder::new();
        e.mov_r64_imm64(Reg::RBX, 0x0102_0304_0506_0708);
        assert_eq!(
            e.finish(),
            vec![0x48, 0xBB, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn cmp_r64_imm32_known_bytes() {
        // cmp rcx, 10 -- ModRM reg field is /7, distinct from add's /0 and sub's /5.
        let mut e = Encoder::new();
        e.cmp_r64_imm32(Reg::RCX, 10);
        assert_eq!(e.finish(), vec![0x48, 0x81, 0xF9, 0x0A, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn xor_self_known_bytes() {
        let mut e = Encoder::new();
        e.xor_r64_self(Reg::R14);
        assert_eq!(e.finish(), vec![0x4D, 0x31, 0xF6]);
    }

    #[test]
    fn sub_add_rsp_known_bytes() {
        let mut e = Encoder::new();
        e.sub_r64_imm32(Reg::RSP, 32);
        e.add_r64_imm32(Reg::RSP, 32);
        assert_eq!(
            e.finish(),
            vec![
                0x48, 0x81, 0xEC, 0x20, 0x00, 0x00, 0x00, 0x48, 0x81, 0xC4, 0x20, 0x00, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn load_disp8_known_bytes() {
        let mut e = Encoder::new();
        // mov rax, [r15+0]
        e.load_r64_disp8(Reg::RAX, Reg::R15, 0);
        assert_eq!(e.finish(), vec![0x49, 0x8B, 0x47, 0x00]);
    }

    #[test]
    fn store_disp8_known_bytes() {
        let mut e = Encoder::new();
        // mov [r15+32], rax
        e.store_r64_disp8(Reg::R15, 32, Reg::RAX);
        assert_eq!(e.finish(), vec![0x49, 0x89, 0x47, 0x20]);
    }

    #[test]
    fn load_r32_disp8_known_bytes() {
        // mov eax, [r15+0] -- REX.B only (r15 extended), no REX.W (32-bit operand). The 32-bit load
        // zero-extends to 64 bits, exactly what the inline slot wants when reading a guest gpr.
        let mut e = Encoder::new();
        e.load_r32_disp8(Reg::RAX, Reg::R15, 0);
        assert_eq!(e.finish(), vec![0x41, 0x8B, 0x47, 0x00]);
    }

    #[test]
    fn store_r32_disp8_known_bytes() {
        // mov [r15+32], eax -- REX.B only (r15 extended), no REX.W.
        let mut e = Encoder::new();
        e.store_r32_disp8(Reg::R15, 32, Reg::RAX);
        assert_eq!(e.finish(), vec![0x41, 0x89, 0x47, 0x20]);
    }

    #[test]
    fn load_r32_sib_known_bytes() {
        // mov r11d, [r12 + r9] -- REX.W=0,R=1(r11),X=1(r9 index),B=1(r12 base) = 0100_0111 = 0x47;
        // 8B; ModRM mod=00,reg=r11&7=3,rm=100(SIB) = 00_011_100 = 0x1C; SIB scale=0,index=r9&7=1,
        // base=r12&7=4 = 00_001_100 = 0x0C.
        let mut e = Encoder::new();
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9);
        assert_eq!(e.finish(), vec![0x47, 0x8B, 0x1C, 0x0C]);

        // mov eax, [esi + ecx] -- no extended regs, so no REX. 8B; ModRM mod=00,reg=eax(0),rm=100 =
        // 0x04; SIB scale=0,index=ecx(1),base=esi(6) = 00_001_110 = 0x0E. Matches the guest bytes
        // `8B 04 0E` the probe's interpreter side executes for the same operation.
        let mut e = Encoder::new();
        e.load_r32_sib(Reg::RAX, Reg::RSI, Reg::RCX);
        assert_eq!(e.finish(), vec![0x8B, 0x04, 0x0E]);
    }

    #[test]
    fn add_r32_imm32_known_bytes() {
        // add eax, 0xa0 -- no REX (eax not extended), 81 /0 id. ModRM mod=11,reg=0(/0),rm=0(eax)
        // = 11_000_000 = 0xC0.
        let mut e = Encoder::new();
        e.add_r32_imm32(Reg::RAX, 0xa0);
        assert_eq!(e.finish(), vec![0x81, 0xC0, 0xA0, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn shr_r32_imm8_known_bytes() {
        // shr ecx, 25 -- no REX (ecx not extended), C1 /5 ib. ModRM mod=11,reg=5(/5),rm=1(ecx)
        // = 11_101_001 = 0xE9; count 25 = 0x19. This is the drawcolumn shift slot.
        let mut e = Encoder::new();
        e.shr_r32_imm8(Reg::RCX, 25);
        assert_eq!(e.finish(), vec![0xC1, 0xE9, 0x19]);
    }

    #[test]
    fn and_r32_imm32_known_bytes() {
        // and eax, 0xfffff000 -- no REX, 81 /4 id. ModRM mod=11,reg=4(/4),rm=0(eax) = 0xE0.
        let mut e = Encoder::new();
        e.and_r32_imm32(Reg::RAX, 0xffff_f000);
        assert_eq!(e.finish(), vec![0x81, 0xE0, 0x00, 0xF0, 0xFF, 0xFF]);
        // and r12d, 0xff -- REX.B (r12 extended). ModRM rm=r12&7=4 -> 0xE4.
        let mut e = Encoder::new();
        e.and_r32_imm32(Reg::R12, 0xff);
        assert_eq!(e.finish(), vec![0x41, 0x81, 0xE4, 0xFF, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn shl_r32_imm8_known_bytes() {
        // shl eax, 4 -- no REX, C1 /4 ib (reg field /4, vs shr's /5). ModRM 11_100_000 = 0xE0.
        let mut e = Encoder::new();
        e.shl_r32_imm8(Reg::RAX, 4);
        assert_eq!(e.finish(), vec![0xC1, 0xE0, 0x04]);
    }

    #[test]
    fn add_r32_r32_known_bytes() {
        // add eax, ecx -- no REX, 01 /r. ModRM mod=11,reg=ecx(1),rm=eax(0) = 0xC8.
        let mut e = Encoder::new();
        e.add_r32_r32(Reg::RAX, Reg::RCX);
        assert_eq!(e.finish(), vec![0x01, 0xC8]);
        // add r12d, r9d -- REX (R from src r9 ext, B from dst r12 ext) = 0x45. ModRM 11_001_100 = 0xCC.
        let mut e = Encoder::new();
        e.add_r32_r32(Reg::R12, Reg::R9);
        assert_eq!(e.finish(), vec![0x45, 0x01, 0xCC]);
    }

    #[test]
    fn movzx_r32_byte_sib_known_bytes() {
        // movzx eax, byte [rsi+rcx] -- no REX. 0F B6; ModRM mod=00,reg=eax(0),rm=100(SIB)=0x04;
        // SIB scale=0,index=rcx(1),base=rsi(6) = 0x0E.
        let mut e = Encoder::new();
        e.movzx_r32_byte_sib(Reg::RAX, Reg::RSI, Reg::RCX);
        assert_eq!(e.finish(), vec![0x0F, 0xB6, 0x04, 0x0E]);
        // movzx r11d, byte [r12+r9] -- REX.R(r11)+X(r9)+B(r12) = 0x47; ModRM reg=r11&7=3,rm=100 = 0x1C;
        // SIB index=r9&7=1,base=r12&7=4 = 0x0C.
        let mut e = Encoder::new();
        e.movzx_r32_byte_sib(Reg::R11, Reg::R12, Reg::R9);
        assert_eq!(e.finish(), vec![0x47, 0x0F, 0xB6, 0x1C, 0x0C]);
    }

    #[test]
    fn store_r8_disp8_known_bytes() {
        // mov [r14+8], al -- REX.B (r14 extended). 88; ModRM mod=01,reg=al(0),rm=r14&7=6 = 0x46; disp 8.
        let mut e = Encoder::new();
        e.store_r8_disp8(Reg::R14, 8, Reg::RAX);
        assert_eq!(e.finish(), vec![0x41, 0x88, 0x46, 0x08]);
        // mov [r12+8], al -- r12&7=4 forces a SIB byte (0x24). REX.B. ModRM 0x44.
        let mut e = Encoder::new();
        e.store_r8_disp8(Reg::R12, 8, Reg::RAX);
        assert_eq!(e.finish(), vec![0x41, 0x88, 0x44, 0x24, 0x08]);
        // mov [rax+1], cl -- no REX, no SIB. ModRM mod=01,reg=cl(1),rm=rax(0) = 0x48; disp 1.
        let mut e = Encoder::new();
        e.store_r8_disp8(Reg::RAX, 1, Reg::RCX);
        assert_eq!(e.finish(), vec![0x88, 0x48, 0x01]);
    }

    #[test]
    fn movzx_byte_sib_reads_the_right_byte() {
        // End-to-end: fn(base, idx) -> i64 returns the zero-extended byte at [base+idx]. Proves the
        // SIB addressing + zero-extension actually execute, not just the byte shape.
        use super::super::exec_mem::ExecutableBuffer;
        let mut e = Encoder::new();
        #[cfg(windows)]
        {
            e.movzx_r32_byte_sib(Reg::RAX, Reg::RCX, Reg::RDX); // win64 arg0=RCX base, arg1=RDX idx
        }
        #[cfg(not(windows))]
        {
            e.movzx_r32_byte_sib(Reg::RAX, Reg::RDI, Reg::RSI); // sysv arg0=RDI base, arg1=RSI idx
        }
        e.ret();
        let bytes = e.finish();
        let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
        let f: extern "C" fn(*const u8, i64) -> i64 =
            unsafe { std::mem::transmute(buf.entry_ptr()) };
        let data = [0x11u8, 0x22, 0x33, 0xAB, 0x55];
        assert_eq!(f(data.as_ptr(), 3), 0xAB);
        assert_eq!(f(data.as_ptr(), 0), 0x11);
    }

    #[test]
    fn store_r8_writes_only_the_low_byte() {
        // End-to-end: fn(dst, val) stores the low byte of `val` to dst[1], leaving dst[0]/dst[2]
        // untouched -- the write_gpr8 byte-lane semantics the probe relies on.
        use super::super::exec_mem::ExecutableBuffer;
        let mut e = Encoder::new();
        #[cfg(windows)]
        {
            e.mov_r32_r32(Reg::RAX, Reg::RDX); // val -> EAX (AL = low byte); dst is RCX
            e.store_r8_disp8(Reg::RCX, 1, Reg::RAX);
        }
        #[cfg(not(windows))]
        {
            e.mov_r32_r32(Reg::RAX, Reg::RSI); // val -> EAX (AL = low byte); dst is RDI
            e.store_r8_disp8(Reg::RDI, 1, Reg::RAX);
        }
        e.ret();
        let bytes = e.finish();
        let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
        let f: extern "C" fn(*mut u8, i64) = unsafe { std::mem::transmute(buf.entry_ptr()) };
        let mut data = [0xEEu8, 0xEE, 0xEE];
        f(data.as_mut_ptr(), 0x1234_567A);
        assert_eq!(
            data,
            [0xEE, 0x7A, 0xEE],
            "only dst[1]'s low byte should change"
        );
    }

    #[test]
    fn load_store_disp8_through_rsp_emits_a_sib_byte() {
        // RSP (low3 == 0b100) can NEVER be a ModRM base directly -- the encoding is reserved to
        // mean "a SIB byte follows" regardless of mod or REX.B. Omitting the SIB byte here would
        // silently shift every following byte by one and corrupt the rest of the instruction
        // stream (this is exactly the bug that originally broke the strcpy block's stack-scratch
        // spill/reload with a host access violation instead of a clean assertion failure).
        let mut e = Encoder::new();
        // mov [rsp+32], rax -- REX.W only (neither RSP nor RAX extended), opcode 89,
        // modrm mod=01,reg=rax&7=0,rm=rsp&7=4 -> 01_000_100 = 0x44, SIB 0x24, disp8 0x20.
        e.store_r64_disp8(Reg::RSP, 32, Reg::RAX);
        assert_eq!(e.finish(), vec![0x48, 0x89, 0x44, 0x24, 0x20]);

        // mov rcx, [rsp+32] -- modrm mod=01,reg=rcx&7=1,rm=rsp&7=4 -> 01_001_100 = 0x4C.
        let mut e = Encoder::new();
        e.load_r64_disp8(Reg::RCX, Reg::RSP, 32);
        assert_eq!(e.finish(), vec![0x48, 0x8B, 0x4C, 0x24, 0x20]);
    }

    #[test]
    fn store_then_load_through_rsp_round_trips_a_real_value() {
        // A real end-to-end check, not just byte shape: emit a function that reserves stack space,
        // stores a value to `[rsp+32]`, reloads it into a different register, restores the stack,
        // and returns it. If the SIB byte were missing, the reload would read garbage (or the
        // emitted bytes would desync entirely and likely crash the process).
        use super::super::exec_mem::ExecutableBuffer;
        let mut e = Encoder::new();
        e.sub_r64_imm32(Reg::RSP, 48);
        e.mov_r64_imm64(Reg::RAX, 0x99);
        e.store_r64_disp8(Reg::RSP, 32, Reg::RAX);
        e.mov_r64_imm64(Reg::RAX, 0); // clobber RAX so the reload below is observable
        e.load_r64_disp8(Reg::RAX, Reg::RSP, 32);
        e.add_r64_imm32(Reg::RSP, 48);
        e.ret();
        let bytes = e.finish();
        let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        assert_eq!(f(), 0x99);
    }

    #[test]
    fn cmp_r64_r64_known_bytes() {
        // cmp r14, rbx -- REX.W=1,R=0(rbx not extended),B=1(r14 extended) = 0100_1001 = 0x49;
        // opcode 39; modrm mod=11,reg=rbx&7=3,rm=r14&7=6 -> 11_011_110 = 0xDE
        let mut e = Encoder::new();
        e.cmp_r64_r64(Reg::R14, Reg::RBX);
        assert_eq!(e.finish(), vec![0x49, 0x39, 0xDE]);
    }

    #[test]
    fn add_r64_r64_known_bytes() {
        // add r14, rbx -- same register/REX/ModRM pattern as cmp, opcode 01 instead of 39.
        let mut e = Encoder::new();
        e.add_r64_r64(Reg::R14, Reg::RBX);
        assert_eq!(e.finish(), vec![0x49, 0x01, 0xDE]);
    }

    #[test]
    fn sub_r64_r64_known_bytes() {
        // sub r14, rbx -- same pattern, opcode 29.
        let mut e = Encoder::new();
        e.sub_r64_r64(Reg::R14, Reg::RBX);
        assert_eq!(e.finish(), vec![0x49, 0x29, 0xDE]);
    }

    #[test]
    fn imul_r64_r64_known_bytes() {
        // imul r14, rbx -- REX.W=1,R=1(r14 is reg/ext),B=0(rbx not ext) = 0100_1100 = 0x4C;
        // opcode 0F AF; modrm mod=11,reg=r14&7=6,rm=rbx&7=3 -> 11_110_011 = 0xF3
        let mut e = Encoder::new();
        e.imul_r64_r64(Reg::R14, Reg::RBX);
        assert_eq!(e.finish(), vec![0x4C, 0x0F, 0xAF, 0xF3]);
    }

    #[test]
    fn imul_r64_imm32_known_bytes() {
        // imul rax, rax, 12 -- REX.W=1, 69 /r id. modrm mod=11,reg=0(rax),rm=0(rax) = 0xC0.
        let mut e = Encoder::new();
        e.imul_r64_imm32(Reg::RAX, 12);
        assert_eq!(e.finish(), vec![0x48, 0x69, 0xC0, 0x0C, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn load_r64_disp32_known_bytes() {
        // mov rax, [r15 + 200] -- REX.W=1,R=0(rax),B=1(r15) = 0x49; 8B; mod=10,reg=rax&7=0,rm=r15&7=7
        // = 10_000_111 = 0x87; disp32 = 200 = 0xC8 0x00 0x00 0x00.
        let mut e = Encoder::new();
        e.load_r64_disp32(Reg::RAX, Reg::R15, 200);
        assert_eq!(e.finish(), vec![0x49, 0x8B, 0x87, 0xC8, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn store_r64_disp32_known_bytes() {
        // mov [r15 + 200], rax -- same as load but opcode 89.
        let mut e = Encoder::new();
        e.store_r64_disp32(Reg::R15, 200, Reg::RAX);
        assert_eq!(e.finish(), vec![0x49, 0x89, 0x87, 0xC8, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn call_indirect_known_bytes() {
        let mut e = Encoder::new();
        e.call_r64(Reg::RAX);
        assert_eq!(e.finish(), vec![0xFF, 0xD0]);
    }

    #[test]
    fn test_al_al_known_bytes() {
        let mut e = Encoder::new();
        e.test_al_al();
        assert_eq!(e.finish(), vec![0x84, 0xC0]);
    }

    #[test]
    fn not_r64_known_bytes() {
        let mut e = Encoder::new();
        e.not_r64(Reg::R14);
        // REX.W=1,B=1 (r14 extended) = 0100_1001 = 0x49; opcode F7; modrm mod=11,reg=2(/2),rm=r14&7=6 -> 11_010_110 = 0xD6
        assert_eq!(e.finish(), vec![0x49, 0xF7, 0xD6]);
    }

    #[test]
    fn backward_jz_lands_on_the_placed_label() {
        // top: xor r14,r14 (3-byte filler, just to give the jump real distance) ; jz top
        let mut e = Encoder::new();
        let top = e.label();
        e.place(top);
        e.xor_r64_self(Reg::R14);
        e.jz(top);
        let bytes = e.finish();
        // jz opcode starts at offset 3 (after the 3-byte xor); rel32 is at bytes[5..9].
        let rel = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
        // end of the jz instruction is offset 9; target (0) - end (9) = -9.
        assert_eq!(rel, -9);
    }

    #[test]
    fn backward_jnz_lands_on_the_placed_label() {
        let mut e = Encoder::new();
        let top = e.label();
        e.place(top);
        e.xor_r64_self(Reg::R14);
        e.jnz(top);
        let bytes = e.finish();
        assert_eq!(bytes[3], 0x0F);
        assert_eq!(bytes[4], 0x85);
        let rel = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
        assert_eq!(rel, -9);
    }

    #[test]
    fn backward_ja_lands_on_the_placed_label() {
        let mut e = Encoder::new();
        let top = e.label();
        e.place(top);
        e.xor_r64_self(Reg::R14);
        e.ja(top);
        let bytes = e.finish();
        assert_eq!(bytes[3], 0x0F);
        assert_eq!(bytes[4], 0x87);
        let rel = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
        assert_eq!(rel, -9);
    }

    #[test]
    fn ja_executes_only_when_unsigned_above() {
        // A real end-to-end check of the condition (not just byte shape): emit
        // `fn() -> i64 { if 5u64 > 3u64 { 1 } else { 0 } }` using cmp_r64_r64 + ja, run it, and
        // confirm the taken branch matches the actual x86 CF=0&&ZF=0 semantics, not just the byte
        // pattern.
        use super::super::exec_mem::ExecutableBuffer;
        let mut e = Encoder::new();
        e.mov_r64_imm64(Reg::RAX, 5);
        e.mov_r64_imm64(Reg::RCX, 3);
        e.cmp_r64_r64(Reg::RAX, Reg::RCX); // 5 - 3 > 0
        let above = e.label();
        e.ja(above);
        e.mov_r64_imm64(Reg::RAX, 0);
        let end = e.label();
        e.jmp(end);
        e.place(above);
        e.mov_r64_imm64(Reg::RAX, 1);
        e.place(end);
        e.ret();
        let bytes = e.finish();
        let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        assert_eq!(f(), 1);
    }

    #[test]
    fn ja_correctly_treats_u64_max_as_unsigned_largest_not_negative() {
        // The exact bug this primitive exists to avoid: cap_clocks == u64::MAX (the "no cap"
        // sentinel some callers pass) must compare as the LARGEST u64, not as -1. A signed `jg`
        // would take the branch here (13 > -1 signed); `ja` must NOT, since 13 is not above
        // u64::MAX unsigned.
        use super::super::exec_mem::ExecutableBuffer;
        let mut e = Encoder::new();
        e.mov_r64_imm64(Reg::RAX, 13);
        e.mov_r64_imm64(Reg::RCX, u64::MAX);
        e.cmp_r64_r64(Reg::RAX, Reg::RCX); // 13 vs u64::MAX, unsigned: 13 is NOT above
        let above = e.label();
        e.ja(above);
        e.mov_r64_imm64(Reg::RAX, 0); // not-above path: expected outcome
        let end = e.label();
        e.jmp(end);
        e.place(above);
        e.mov_r64_imm64(Reg::RAX, 1);
        e.place(end);
        e.ret();
        let bytes = e.finish();
        let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        assert_eq!(f(), 0);
    }

    #[test]
    fn forward_jmp_patches_to_a_later_label() {
        let mut e = Encoder::new();
        let exit = e.label();
        e.jmp(exit); // E9 at offset 0, 5 bytes total
        e.xor_r64_self(Reg::R14); // 3 bytes of filler the jump must skip
        e.place(exit);
        let bytes = e.finish();
        let rel = i32::from_le_bytes(bytes[1..5].try_into().unwrap());
        // end of the jmp instruction is offset 5; target (8) - end (5) = 3.
        assert_eq!(rel, 3);
    }

    #[test]
    fn forward_jnz_patches_to_a_later_label() {
        let mut e = Encoder::new();
        let exit = e.label();
        e.jnz(exit); // 0F 85 at offset 0, 6 bytes total
        e.xor_r64_self(Reg::R14); // 3 bytes of filler
        e.place(exit);
        let bytes = e.finish();
        let rel = i32::from_le_bytes(bytes[2..6].try_into().unwrap());
        // end of the jnz instruction is offset 6; target (9) - end (6) = 3.
        assert_eq!(rel, 3);
    }

    #[test]
    #[should_panic(expected = "a jcc/jmp target label was never placed")]
    fn finish_panics_on_an_unresolved_forward_label() {
        let mut e = Encoder::new();
        let exit = e.label();
        e.jmp(exit); // forward reference, queued as a patch
        // `exit` is never `place`d -- `finish` must panic resolving the queued patch.
        let _ = e.finish();
    }

    #[test]
    #[should_panic(expected = "label placed twice")]
    fn place_panics_if_the_same_label_is_placed_twice() {
        let mut e = Encoder::new();
        let here = e.label();
        e.place(here);
        e.place(here);
    }

    #[test]
    fn executes_an_emitted_increment_function() {
        // A real end-to-end check that the encoder's bytes actually run: emit
        // `fn(x: i64) -> i64 { x + 1 }` for whichever ABI the host uses, via mov_r64_r64 from the
        // arg register into rax then add_r64_imm32 and ret.
        use super::super::exec_mem::ExecutableBuffer;
        let mut e = Encoder::new();
        #[cfg(windows)]
        e.mov_r64_r64(Reg::RAX, Reg::RCX); // win64 arg0 = RCX
        #[cfg(not(windows))]
        e.mov_r64_r64(Reg::RAX, Reg::RDI); // sysv64 arg0 = RDI
        e.add_r64_imm32(Reg::RAX, 1);
        e.ret();
        let bytes = e.finish();
        let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
        let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        assert_eq!(f(41), 42);
    }
}
