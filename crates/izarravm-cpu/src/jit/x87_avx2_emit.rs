// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! AVX2 x87 lowering that keeps the eight physical registers and status/tag words resident for a
//! linked native chain. Guest arithmetic remains scalar because x87 operations are ordered and
//! stack-dependent, so each physical x87 register has a fixed scalar host register.

use super::{
    encoder::{Encoder, Label, Reg, Xmm},
    native_x87::{
        NativeX87BinaryOp, NativeX87Insn, NativeX87StiOp, X87_CONDITION_MASK, X87_TOP_MASK,
        X87_TOP_SHIFT, native_x87_layout,
    },
};
use crate::{CR0_EM, CR0_NE, CR0_TS, ControlRegisters, CpuGsw};

pub(crate) const CACHE_REG: Reg = Reg::RSI;
const VALUE0: Xmm = Xmm::XMM0;
const VALUE1: Xmm = Xmm::XMM1;
const ZERO: Xmm = Xmm::XMM2;

#[derive(Clone, Copy)]
pub(crate) struct Avx2X87EmitContext {
    pub(crate) cpu: Reg,
    pub(crate) memory: Option<Reg>,
    pub(crate) side_exit: Label,
    pub(crate) check_gate: bool,
    pub(crate) top: u8,
}

pub(crate) fn emit_enter(e: &mut Encoder, cpu: Reg) {
    for physical in 0..8 {
        e.vmovsd_xmm_disp32(
            physical_cache(physical),
            cpu,
            st_offset() + i32::from(physical) * 8,
        );
    }
    e.movzx_r32_word_disp32(CACHE_REG, cpu, status_offset());
    e.movzx_r32_word_disp32(Reg::RAX, cpu, tag_offset());
    e.shl_r32_imm8(Reg::RAX, 16);
    e.or_r32_r32(CACHE_REG, Reg::RAX);
}

pub(crate) fn emit_spill(e: &mut Encoder, cpu: Reg) {
    for physical in 0..8 {
        e.vmovsd_disp32_xmm(
            cpu,
            st_offset() + i32::from(physical) * 8,
            physical_cache(physical),
        );
    }
    e.store_r16_disp32(cpu, status_offset(), CACHE_REG);
    e.mov_r32_r32(Reg::RAX, CACHE_REG);
    e.shr_r32_imm8(Reg::RAX, 16);
    e.store_r16_disp32(cpu, tag_offset(), Reg::RAX);
    e.vzeroupper();
}

pub(crate) fn emit_native_x87(e: &mut Encoder, insn: NativeX87Insn, context: Avx2X87EmitContext) {
    if context.check_gate {
        emit_gate(e, context.cpu, context.side_exit);
    }
    let top = context.top & 7;
    match insn {
        NativeX87Insn::BinaryMemory { op, .. } => {
            let memory = context
                .memory
                .expect("x87 memory source needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vcvtss2sd_disp32(VALUE1, VALUE1, memory, 0);
            emit_finite_guard(e, VALUE1, context.side_exit);
            emit_binary_st0(e, top, op, context.side_exit);
        }
        // No operand finite guard here, and that is deliberate rather than an oversight: the
        // guard's contract is that no NaN or infinity may reach `emit_compare`
        // (`emit_binary_st0` has no guard of its own on the compare path). For this variant the
        // contract holds by construction on BOTH inputs: `emit_load_physical` finite-guards
        // ST(0) below, and a `cvtsi2sd` result is always finite, an integer can never convert to
        // NaN or infinity. The arithmetic path guards its own RESULT inside `emit_binary_st0`, so
        // an FIDIV by integer zero produces an infinity, fails that guard, and side exits, with
        // the interpreter recording the ZE exception exactly as it does today.
        //
        // Tier 2 (m64/m80 memory operands, a later slice) CAN carry a NaN or infinity operand and
        // must NOT copy this arm's shape without adding the guard back.
        NativeX87Insn::IntBinaryMemory { op, .. } => {
            let memory = context
                .memory
                .expect("x87 int memory source needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vcvtsi2sd_i32_disp32(VALUE1, VALUE1, memory, 0);
            emit_binary_st0(e, top, op, context.side_exit);
        }
        NativeX87Insn::BinaryRegister { op, index } => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_load_physical(e, physical(top, index), VALUE1, context.side_exit);
            emit_binary_st0(e, top, op, context.side_exit);
        }
        NativeX87Insn::LoadF32 { .. } => {
            let memory = context.memory.expect("FLD needs a host pointer");
            e.vcvtss2sd_disp32(VALUE0, VALUE0, memory, 0);
            emit_finite_guard(e, VALUE0, context.side_exit);
            emit_push(e, top, VALUE0);
        }
        NativeX87Insn::StoreF32 { pop, .. } => {
            let memory = context.memory.expect("FST needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vcvtsd2ss(VALUE0, VALUE0, VALUE0);
            e.vmovss_disp32_xmm(memory, 0, VALUE0);
            if pop {
                emit_pop(e, top);
            }
        }
        NativeX87Insn::LoadRegister { index } => {
            emit_load_physical(e, physical(top, index), VALUE0, context.side_exit);
            emit_push(e, top, VALUE0);
        }
        NativeX87Insn::Exchange { index } => {
            let other = physical(top, index);
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_load_physical(e, other, VALUE1, context.side_exit);
            emit_store_physical(e, top, VALUE1);
            emit_store_physical(e, other, VALUE0);
        }
        NativeX87Insn::StoreRegister { index, pop } => {
            let destination = physical(top, index);
            if index == 0 && pop {
                emit_pop(e, top);
            } else {
                emit_load_physical(e, top, VALUE0, context.side_exit);
                emit_store_physical(e, destination, VALUE0);
                if pop {
                    emit_pop(e, top);
                }
            }
        }
        NativeX87Insn::LoadOne => {
            e.mov_r64_imm64(Reg::RDX, 1.0f64.to_bits());
            e.vmovq_xmm_r64(VALUE0, Reg::RDX);
            emit_push(e, top, VALUE0);
        }
        NativeX87Insn::LoadZero => {
            e.vxorpd(VALUE0, VALUE0, VALUE0);
            emit_push(e, top, VALUE0);
        }
        NativeX87Insn::LoadI32 { .. } => {
            let memory = context.memory.expect("FILD needs a host pointer");
            e.vcvtsi2sd_i32_disp32(VALUE0, VALUE0, memory, 0);
            emit_push(e, top, VALUE0);
        }
        NativeX87Insn::StoreI32 { pop, .. } => {
            let memory = context.memory.expect("FIST/FISTP needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_fistp_chop_guard(
                e,
                context.cpu,
                context.side_exit,
                -2_147_483_649.0,
                6,
                2_147_483_648.0,
            );
            e.vcvttsd2si_r32(Reg::RDX, VALUE0);
            e.store_r32_disp32(memory, 0, Reg::RDX);
            if pop {
                emit_pop(e, top);
            }
        }
        // FSTP m80. `write_extended80` (fpu_exec.rs:917-957) as straight-line bit surgery.
        //
        // `emit_load_physical` has already excluded NaN and infinity, so the interpreter's two
        // special-pattern branches are unreachable and what remains is its zero branch and its
        // normal branch. Those two are exactly `biased == 0 && fraction == 0` and `biased != 0`,
        // and the gap between them -- `biased == 0 && fraction != 0`, a subnormal -- is the one
        // input this refuses, because that is where the interpreter stops being exact.
        //
        // Register budget is the reason for the shape rather than the order. RDI holds the host
        // pointer for the whole arm and RAX/RCX/RDX are the only scratch, so there is no register
        // free to park a `1 << 63` constant in while the sign is also live. The sign is therefore
        // re-extracted from VALUE0 after the mantissa is finished, which costs three instructions
        // and no spill.
        NativeX87Insn::StoreExtended80 { .. } => {
            let memory = context.memory.expect("FSTP m80 needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);

            e.vmovq_r64_xmm(Reg::RDX, VALUE0);
            e.mov_r64_r64(Reg::RCX, Reg::RDX);
            e.shift_r64_imm8(5, Reg::RCX, 52);
            e.mov_r32_r32(Reg::RAX, Reg::RCX);
            // Sets ZF, which is the zero-or-subnormal test: `and` against the exponent field.
            e.and_r32_imm32(Reg::RAX, 0x7ff);
            let zero_or_subnormal = e.label();
            let sign_and_emit = e.label();
            e.jz(zero_or_subnormal);

            // Normal. `shl 11` moves the 52-bit fraction to bits 62..11 and drags the exponent's
            // low bit into bit 63, which the explicit integer bit then overwrites, so the two
            // steps together are exactly `(1 << 63) | (fraction << 11)`.
            e.shift_r64_imm8(4, Reg::RDX, 11);
            e.mov_r64_imm64(Reg::RCX, 1u64 << 63);
            e.or_r64_r64(Reg::RDX, Reg::RCX);
            // biased - 1023 + 16383. Range-safe without a guard: a non-subnormal finite f64 has
            // `biased` in 1..=2046, so the result is 15361..=17406 and cannot reach the 0x7FFF
            // the interpreter reserves for NaN and infinity.
            e.add_r32_imm32(Reg::RAX, 15_360);
            e.jmp(sign_and_emit);

            e.place(zero_or_subnormal);
            // `shl 12` discards the sign and exponent, leaving the fraction. Zero for a true
            // zero, in which case RDX is now the all-zero mantissa the interpreter writes and RAX
            // is the zero exponent it writes; non-zero for a subnormal, which leaves.
            e.shift_r64_imm8(4, Reg::RDX, 12);
            e.jnz(context.side_exit);

            e.place(sign_and_emit);
            // The sign comes off VALUE0 rather than off a saved copy: the normal path clobbered
            // RCX with the integer-bit constant, and -0.0 makes this load-bearing -- it is a
            // zero whose stored sign-exponent word is 0x8000, not 0.
            e.vmovq_r64_xmm(Reg::RCX, VALUE0);
            e.shift_r64_imm8(5, Reg::RCX, 63);
            e.shl_r32_imm8(Reg::RCX, 15);
            e.or_r32_r32(Reg::RAX, Reg::RCX);

            e.store_r64_disp32(memory, 0, Reg::RDX);
            e.store_r16_disp32(memory, 8, Reg::RAX);
            emit_pop(e, top);
        }
        // FISTP m64. The i32 arm above with a wider conversion, a wider store and -- the part
        // that is not a widening -- a strict low bound; see `emit_fistp_chop_guard`.
        NativeX87Insn::StoreI64 { .. } => {
            let memory = context.memory.expect("FISTP m64 needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_fistp_chop_guard(
                e,
                context.cpu,
                context.side_exit,
                -9_223_372_036_854_775_808.0,
                2,
                9_223_372_036_854_775_808.0,
            );
            e.vcvttsd2si_r64(Reg::RDX, VALUE0);
            e.store_r64_disp32(memory, 0, Reg::RDX);
            emit_pop(e, top);
        }
        // 0xDC and 0xDE mod=3 differ by exactly one emitted step. Sharing the body makes that
        // structural instead of a comment: the interpreter models both with one function.
        NativeX87Insn::BinaryRegisterDest { op, index } => {
            emit_sti_binary(e, top, index, op, context.side_exit);
        }
        NativeX87Insn::PopBinary { op, index } => {
            emit_sti_binary(e, top, index, op, context.side_exit);
            emit_pop(e, top);
        }
        // FCHS and FABS, both as one bit of the operand's raw pattern in a GPR rather than a
        // packed-double mask in an XMM: there is no `vandpd`/`vxorpd`-with-constant helper in the
        // encoder, and a 64-bit round trip is the same instruction count as building the mask
        // would be. `emit_store_physical` re-derives the tag from the stored value, which is what
        // keeps FCHS on a zero correct -- the interpreter's `set` tags -0.0 as Zero, and so does
        // the `vucomisd` against zero inside `emit_store_physical`.
        NativeX87Insn::SignOp { negate } => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vmovq_r64_xmm(Reg::RDX, VALUE0);
            if negate {
                e.mov_r64_imm64(Reg::RAX, 1u64 << 63);
                e.xor_r64_r64(Reg::RDX, Reg::RAX);
            } else {
                // Shift the sign out and back rather than masking, for the same
                // no-64-bit-immediate-AND reason the negate arm builds its constant.
                e.shift_r64_imm8(4, Reg::RDX, 1);
                e.shift_r64_imm8(5, Reg::RDX, 1);
            }
            e.vmovq_xmm_r64(VALUE0, Reg::RDX);
            emit_store_physical(e, top, VALUE0);
        }
        // FSQRT. The guard on the RESULT is the whole arm: the interpreter stores a NaN and
        // raises IE for a negative operand, and the resident cache cannot hold a NaN, so this
        // must side exit before `emit_store_physical` and let the interpreter record the
        // exception. `emit_finite_guard` tests the exponent field for all-ones, which catches
        // that NaN as well as an infinity.
        NativeX87Insn::SquareRoot => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vsqrtsd(VALUE0, VALUE0, VALUE0);
            emit_finite_guard(e, VALUE0, context.side_exit);
            emit_store_physical(e, top, VALUE0);
        }
        // WAIT/FWAIT. The body is EMPTY on purpose: the whole instruction is the gate above, and
        // when `check_gate` is false the gate has already run earlier in this block and still
        // holds (see `NativeX87Insn::Wait` for why that is exact rather than optimistic). No
        // register, no status bit, no TOP movement -- an emitted WAIT that touched any of them
        // would be wrong, because the interpreter's 0x9b arm returns straight out of
        // `execute_fpu_decoded` without reaching `execute_fpu_register` or the FPU state at all.
        NativeX87Insn::Wait => {}
        // FRNDINT. The four-way branch on the control word's RC field IS the instruction: RC is a
        // runtime value and `vroundsd` takes its mode as an immediate, so the four modes are four
        // emitted instructions with three compares in front of them.
        //
        // The immediates are `fpu_round_rc`'s four arms in `fpu_round_rc`'s order --
        // 0 round-to-nearest-even, 1 floor, 2 ceil, 3 truncate -- which is also the RC encoding's
        // own order, so the mapping is the identity and there is no translation to get wrong.
        // Bit 3 (0x08) suppresses the precision exception, matching an interpreter that does not
        // model one; without it a host MXCSR flag would move where no guest state does.
        //
        // Scratch: RDX only, which `emit_load_physical` has just finished with (it is that
        // helper's tag scratch), and which nothing below `emit_store_physical` reads.
        NativeX87Insn::RoundToInt => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.movzx_r32_word_disp32(Reg::RDX, context.cpu, control_offset());
            e.shr_r32_imm8(Reg::RDX, 10);
            e.and_r32_imm32(Reg::RDX, 3);
            let done = e.label();
            let arms = [e.label(), e.label(), e.label()];
            for (mode, arm) in arms.iter().enumerate() {
                e.cmp_r32_imm32(Reg::RDX, mode as u32 + 1);
                e.jz(*arm);
            }
            e.vroundsd(VALUE0, VALUE0, VALUE0, 0x08);
            e.jmp(done);
            for (mode, arm) in arms.iter().enumerate() {
                e.place(*arm);
                e.vroundsd(VALUE0, VALUE0, VALUE0, (mode as u8 + 1) | 0x08);
                if mode + 1 < arms.len() {
                    e.jmp(done);
                }
            }
            e.place(done);
            emit_store_physical(e, top, VALUE0);
        }
        // FTST. The only compare shape that loads ONE physical register: its right-hand side is
        // the literal +0.0, not a stack slot, so there is no second tag or finite guard.
        NativeX87Insn::TestZero => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vxorpd(ZERO, ZERO, ZERO);
            emit_compare(e, VALUE0, ZERO);
        }
        // FXAM. `emit_load_physical` has already side exited on empty, NaN and infinity, so the
        // only classes reachable here are finite zero (C3) and finite non-zero (C2), with C1 the
        // sign bit and C0 always clear. That is why this arm can decide the whole classification
        // from one `vucomisd` without consulting the tag word the interpreter reads.
        NativeX87Insn::Examine => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.and_r32_imm32(CACHE_REG, !u32::from(X87_CONDITION_MASK));
            e.vmovq_r64_xmm(Reg::RDX, VALUE0);
            e.shift_r64_imm8(5, Reg::RDX, 63);
            e.shl_r32_imm8(Reg::RDX, 9);
            e.vxorpd(ZERO, ZERO, ZERO);
            e.vucomisd(VALUE0, ZERO);
            let zero = e.label();
            let done = e.label();
            e.jz(zero);
            e.or_r32_imm32(Reg::RDX, 1 << 10);
            e.jmp(done);
            e.place(zero);
            e.or_r32_imm32(Reg::RDX, 1 << 14);
            e.place(done);
            e.or_r32_r32(CACHE_REG, Reg::RDX);
        }
        // FUCOM/FUCOMP. Byte for byte the `BinaryRegister` compare path: the interpreter models
        // the unordered forms with the same `fpu_compare` the ordered ones use, so anything that
        // would distinguish them (a signaling-NaN #IA) is unreachable on both sides. Both
        // `emit_load_physical` calls finite-guard, so `emit_compare` never sees an unordered pair
        // and never has to write the C3=C2=C0 triple, which is the same contract the 0xDC memory
        // arm's comment spells out.
        NativeX87Insn::UnorderedCompare { index, pop } => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_load_physical(e, physical(top, index), VALUE1, context.side_exit);
            emit_compare(e, VALUE0, VALUE1);
            if pop {
                emit_pop(e, top);
            }
        }
        NativeX87Insn::ComparePopPop => {
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_load_physical(e, physical(top, 1), VALUE1, context.side_exit);
            emit_compare(e, VALUE0, VALUE1);
            emit_pop(e, top);
            emit_pop(e, top.wrapping_add(1) & 7);
        }
        NativeX87Insn::StoreStatusAx => {
            e.mov_r32_r32(Reg::RAX, CACHE_REG);
            e.and_r32_imm32(Reg::RAX, 0xffff);
            e.and_r32_imm32(Reg::R8, 0xffff_0000);
            e.or_r32_r32(Reg::R8, Reg::RAX);
        }
        // The control word is NOT part of the resident cache: `emit_enter` loads only ST(0..7)
        // into XMM4-11 and the packed status/tag word into CACHE_REG. Control lives only in
        // `CpuGsw.fpu.control`, and both of its readers, `emit_gate` above and
        // `emit_fistp_chop_guard` below, load it from there at RUNTIME. That is exactly what
        // makes a lowered FLDCW inside a block safe: a later FISTP slot sees the value this
        // arm just wrote, in both directions.
        //
        // RDX is the scratch register every other arm here uses, and `memory` is RDI, set up by
        // `emit_x87_memory_pointer` and required to survive the gate. Neither arm touches
        // CACHE_REG, which is correct: neither instruction changes the status or tag word.
        NativeX87Insn::LoadControlWord { .. } => {
            let memory = context.memory.expect("FLDCW needs a host pointer");
            e.movzx_r32_word_disp32(Reg::RDX, memory, 0);
            e.store_r16_disp32(context.cpu, control_offset(), Reg::RDX);
        }
        NativeX87Insn::StoreControlWord { .. } => {
            let memory = context.memory.expect("FNSTCW needs a host pointer");
            e.movzx_r32_word_disp32(Reg::RDX, context.cpu, control_offset());
            e.store_r16_disp32(memory, 0, Reg::RDX);
        }
        // m64 IS the native f64 representation: no conversion, unlike LoadF32's vcvtss2sd. The
        // finite guard is still mandatory: a NaN or infinity bit pattern is legal in guest
        // memory and the resident cache cannot hold one, the same reason LoadF32 guards its
        // converted value.
        NativeX87Insn::LoadF64 { .. } => {
            let memory = context.memory.expect("FLD m64 needs a host pointer");
            e.vmovsd_xmm_disp32(VALUE0, memory, 0);
            emit_finite_guard(e, VALUE0, context.side_exit);
            emit_push(e, top, VALUE0);
        }
        // No conversion and no guard: `emit_load_physical` already finite-guards the value
        // coming off the resident cache, so it is finite by construction before it reaches the
        // store.
        NativeX87Insn::StoreF64 { pop, .. } => {
            let memory = context.memory.expect("FST/FSTP m64 needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vmovsd_disp32_xmm(memory, 0, VALUE0);
            if pop {
                emit_pop(e, top);
            }
        }
        // No finite guard here, for the same reason `LoadI32` needs none: an i64 magnitude is at
        // most 2^63, which is finite under every rounding mode, so a `vcvtsi2sd` result here can
        // never be NaN or infinity.
        //
        // What is NOT the same as `LoadI32`: an i32 fits exactly in f64's 53-bit mantissa, so
        // that conversion is always exact. An i64 does not. Above 2^53 this conversion ROUNDS,
        // and the interpreter's `as f64` (`fpu_exec.rs:819`) is round-to-nearest-even while the
        // emitted `vcvtsi2sd` rounds per MXCSR.RC. Nothing in this crate ever sets MXCSR, so the
        // two agree only because the host's default MXCSR is 0x1F80 (RC = 00, round-to-nearest).
        // That is empirically true, not architecturally guaranteed, and it is not new here:
        // `vaddsd`/`vmulsd`/`vsubsd`/`vdivsd` already carry the identical unstated dependency,
        // campaign-wide, and no fixture can catch it because both sides read the same MXCSR.
        NativeX87Insn::LoadI64 { .. } => {
            let memory = context.memory.expect("FILD m64 needs a host pointer");
            e.vcvtsi2sd_i64_disp32(VALUE0, VALUE0, memory, 0);
            emit_push(e, top, VALUE0);
        }
        // Unlike the 0xDA arm above, a Tier 2 memory operand CAN be NaN or infinity, so this is
        // the arm that MUST carry the guard the 0xDA arm's comment explains omitting: without
        // it, an unordered FCOM m64 would reach `emit_compare` and write C3 alone instead of the
        // interpreter's C3=C2=C0 triple.
        NativeX87Insn::BinaryMemoryF64 { op, .. } => {
            let memory = context
                .memory
                .expect("0xDC memory source needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            e.vmovsd_xmm_disp32(VALUE1, memory, 0);
            emit_finite_guard(e, VALUE1, context.side_exit);
            emit_binary_st0(e, top, op, context.side_exit);
        }
    }
}

fn emit_gate(e: &mut Encoder, cpu: Reg, side_exit: Label) {
    e.load_r32_disp32(Reg::RAX, cpu, cr0_offset());
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, CR0_EM | CR0_TS);
    e.cmp_r32_imm32(Reg::RDX, 0);
    e.jnz(side_exit);

    e.and_r32_imm32(Reg::RAX, CR0_NE);
    e.cmp_r32_imm32(Reg::RAX, 0);
    let ready = e.label();
    e.jz(ready);
    e.mov_r32_r32(Reg::RAX, CACHE_REG);
    e.and_r32_imm32(Reg::RAX, 0x3f);
    e.movzx_r32_word_disp32(Reg::RDX, cpu, control_offset());
    e.alu_r32_imm32(6, Reg::RDX, 0x3f);
    e.alu_r32_r32(4, Reg::RAX, Reg::RDX);
    e.cmp_r32_imm32(Reg::RAX, 0);
    e.jnz(side_exit);
    e.place(ready);
}

/// ST(i) op ST(0), result to ST(i). The shared body of `0xDC` mod=3 and `0xDE` mod=3; the pop
/// belongs to the caller.
///
/// VALUE0 is `a` and VALUE1 is `b`, matching `fpu_arith(op, a, b)` argument for argument with
/// `a = fpu.get(i)` and `b = fpu.get(0)`, which is why the six arms transcribe directly. Every
/// guard fires before `emit_store_physical`, so an exceptional input or result leaves for the
/// interpreter with no x87 state changed. `index == 0` is safe by construction: destination
/// equals top, both loads read the same physical cache register into two different scratch
/// XMMs, and the single store re-derives the tag from the stored value.
///
/// `NativeX87StiOp` has six variants and no compare, so this needs no `unreachable!` arm on
/// either caller's path.
fn emit_sti_binary(e: &mut Encoder, top: u8, index: u8, op: NativeX87StiOp, side_exit: Label) {
    let destination = physical(top, index);
    emit_load_physical(e, destination, VALUE0, side_exit);
    emit_load_physical(e, top, VALUE1, side_exit);
    match op {
        NativeX87StiOp::Add => e.vaddsd(VALUE0, VALUE0, VALUE1),
        NativeX87StiOp::Multiply => e.vmulsd(VALUE0, VALUE0, VALUE1),
        NativeX87StiOp::Subtract => e.vsubsd(VALUE0, VALUE0, VALUE1),
        NativeX87StiOp::Divide => e.vdivsd(VALUE0, VALUE0, VALUE1),
        NativeX87StiOp::SubtractReverse => e.vsubsd(VALUE0, VALUE1, VALUE0),
        NativeX87StiOp::DivideReverse => e.vdivsd(VALUE0, VALUE1, VALUE0),
    }
    emit_finite_guard(e, VALUE0, side_exit);
    emit_store_physical(e, destination, VALUE0);
}

fn emit_binary_st0(e: &mut Encoder, top: u8, op: NativeX87BinaryOp, side_exit: Label) {
    if op.is_compare() {
        emit_compare(e, VALUE0, VALUE1);
        if op.pops() {
            emit_pop(e, top);
        }
        return;
    }
    match op {
        NativeX87BinaryOp::Add => e.vaddsd(VALUE0, VALUE0, VALUE1),
        NativeX87BinaryOp::Multiply => e.vmulsd(VALUE0, VALUE0, VALUE1),
        NativeX87BinaryOp::Subtract => e.vsubsd(VALUE0, VALUE0, VALUE1),
        NativeX87BinaryOp::Divide => e.vdivsd(VALUE0, VALUE0, VALUE1),
        NativeX87BinaryOp::SubtractReverse => e.vsubsd(VALUE0, VALUE1, VALUE0),
        NativeX87BinaryOp::DivideReverse => e.vdivsd(VALUE0, VALUE1, VALUE0),
        NativeX87BinaryOp::Compare | NativeX87BinaryOp::ComparePop => unreachable!(),
    }
    emit_finite_guard(e, VALUE0, side_exit);
    emit_store_physical(e, top, VALUE0);
}

fn emit_load_physical(e: &mut Encoder, physical: u8, destination: Xmm, side_exit: Label) {
    let shift = 16 + u32::from(physical & 7) * 2;
    e.mov_r32_r32(Reg::RDX, CACHE_REG);
    e.shr_r32_imm8(Reg::RDX, shift as u8);
    e.and_r32_imm32(Reg::RDX, 3);
    e.cmp_r32_imm32(Reg::RDX, 2);
    e.jcc(3, side_exit);

    e.vmovsd_xmm_xmm(destination, destination, physical_cache(physical));
    emit_finite_guard(e, destination, side_exit);
}

fn emit_store_physical(e: &mut Encoder, physical: u8, value: Xmm) {
    let cache = physical_cache(physical);
    e.vmovsd_xmm_xmm(cache, cache, value);

    let shift = 16 + u32::from(physical & 7) * 2;
    e.and_r32_imm32(CACHE_REG, !(3u32 << shift));
    e.vxorpd(ZERO, ZERO, ZERO);
    e.vucomisd(value, ZERO);
    let nonzero = e.label();
    e.jnz(nonzero);
    e.or_r32_imm32(CACHE_REG, 1 << shift);
    e.place(nonzero);
}

fn emit_push(e: &mut Encoder, top: u8, value: Xmm) {
    let new_top = top.wrapping_add(7) & 7;
    emit_store_physical(e, new_top, value);
    emit_set_top(e, new_top);
}

fn emit_pop(e: &mut Encoder, top: u8) {
    let shift = 16 + u32::from(top & 7) * 2;
    e.or_r32_imm32(CACHE_REG, 3 << shift);
    emit_set_top(e, top.wrapping_add(1) & 7);
}

fn emit_set_top(e: &mut Encoder, top: u8) {
    e.and_r32_imm32(CACHE_REG, !u32::from(X87_TOP_MASK));
    e.or_r32_imm32(CACHE_REG, u32::from(top & 7) << X87_TOP_SHIFT);
}

fn emit_finite_guard(e: &mut Encoder, value: Xmm, side_exit: Label) {
    e.vmovq_r64_xmm(Reg::RDX, value);
    e.shift_r64_imm8(5, Reg::RDX, 52);
    e.and_r32_imm32(Reg::RDX, 0x7ff);
    e.cmp_r32_imm32(Reg::RDX, 0x7ff);
    e.jz(side_exit);
}

fn emit_compare(e: &mut Encoder, lhs: Xmm, rhs: Xmm) {
    let equal = e.label();
    let below = e.label();
    let done = e.label();
    e.vucomisd(lhs, rhs);
    e.jz(equal);
    e.jcc(2, below);
    emit_condition(e, 0);
    e.jmp(done);
    e.place(equal);
    emit_condition(e, 1 << 14);
    e.jmp(done);
    e.place(below);
    emit_condition(e, 1 << 8);
    e.place(done);
}

fn emit_condition(e: &mut Encoder, set: u32) {
    e.and_r32_imm32(CACHE_REG, !u32::from(X87_CONDITION_MASK));
    if set != 0 {
        e.or_r32_imm32(CACHE_REG, set);
    }
}

/// The integer-store admission guard, shared by FIST/FISTP m32 and FISTP m64.
///
/// Two conditions. RC must be truncate, because that is the only rounding mode `vcvttsd2si`
/// implements and the interpreter applies `fpu_round_rc` before its own range check. And the
/// operand in VALUE0 must ROUND into the destination's range: the interpreter tests the rounded
/// value, so the bounds here are stated on the unrounded operand and differ from the integer
/// limits by design.
///
/// `low_exit_cc` is the parameter that is easy to get wrong, and it is genuinely different
/// between the two widths rather than a copy. For m32 the bound is `-2^31 - 1`, which is exactly
/// representable, and `trunc(v) >= -2^31` holds for every `v` strictly above it -- so the exit is
/// JBE (6), refusing `v <= -2^31 - 1`. For m64 the same reasoning would want `-2^63 - 1`, which
/// is NOT representable: at that magnitude f64 steps by 2048, so no double lies strictly between
/// `-2^63 - 1` and `-2^63` and `trunc(v) >= -2^63` collapses to `v >= -2^63`. The exit is
/// therefore JB (2) against `-2^63` itself. Using JBE there would wrongly refuse the single most
/// likely out-of-range-looking input, exactly `-2^63`, which IS in range.
///
/// The high side needs no such split: both widths refuse `v >= 2^N`, JAE (3), because `2^N` is
/// representable and one past the largest storable integer in both cases.
fn emit_fistp_chop_guard(
    e: &mut Encoder,
    cpu: Reg,
    side_exit: Label,
    low: f64,
    low_exit_cc: u8,
    high: f64,
) {
    e.movzx_r32_word_disp32(Reg::RAX, cpu, control_offset());
    e.and_r32_imm32(Reg::RAX, 0x0c00);
    e.cmp_r32_imm32(Reg::RAX, 0x0c00);
    e.jnz(side_exit);

    e.mov_r64_imm64(Reg::RDX, low.to_bits());
    e.vmovq_xmm_r64(VALUE1, Reg::RDX);
    e.vucomisd(VALUE0, VALUE1);
    e.jcc(low_exit_cc, side_exit);
    e.mov_r64_imm64(Reg::RDX, high.to_bits());
    e.vmovq_xmm_r64(VALUE1, Reg::RDX);
    e.vucomisd(VALUE0, VALUE1);
    e.jcc(3, side_exit);
}

const fn physical(top: u8, logical: u8) -> u8 {
    top.wrapping_add(logical) & 7
}

const fn physical_cache(physical: u8) -> Xmm {
    Xmm(4 + (physical & 7))
}

const fn cr0_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, control) + core::mem::offset_of!(ControlRegisters, cr0)) as i32
}

const fn fpu_base_offset() -> usize {
    core::mem::offset_of!(CpuGsw, fpu)
}

const fn st_offset() -> i32 {
    (fpu_base_offset() + native_x87_layout().st) as i32
}

const fn control_offset() -> i32 {
    (fpu_base_offset() + native_x87_layout().control) as i32
}

/// Byte offset of `CpuGsw.fpu.status`. Public to the jit module because the shared x87 re-entry
/// pad reads the live TOP out of it to guard against a target block's baked TOP.
pub(crate) const fn status_offset() -> i32 {
    (fpu_base_offset() + native_x87_layout().status) as i32
}

const fn tag_offset() -> i32 {
    (fpu_base_offset() + native_x87_layout().tag) as i32
}

#[cfg(test)]
#[path = "x87_avx2_emit_test.rs"]
mod tests;
