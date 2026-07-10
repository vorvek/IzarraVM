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
        // REX is required for 64-bit operands or extended registers. Pointer
        // and qword operations always set W, so this prefix is unconditional.
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

    /// `mov dst32, [base + disp32]` (8B /r, mod=10 disp32, no REX.W, SIB if `base` is RSP/R12) -- the
    /// 32-bit-operand, 32-bit-displacement load. Reads a 32-bit field past the disp8 range (the native
    /// fold path reads `Registers.eip` at offset ~128 through the regs-base register). Zero-extends to
    /// 64 bits like `load_r32_disp8`.
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

    /// `mov [base + disp32], src32` (89 /r, mod=10 disp32, no REX.W, SIB if `base` is RSP/R12) -- the
    /// 32-bit store mirror of `load_r32_disp32`, for writing back a 32-bit field past the disp8 range
    /// (the native fold path writes the advanced `Registers.eip`).
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
#[path = "encoder_test.rs"]
mod tests;
