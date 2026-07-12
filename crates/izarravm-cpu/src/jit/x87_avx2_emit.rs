// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! AVX2 x87 lowering that keeps the eight physical registers and status/tag words resident for a
//! linked native chain. Guest arithmetic remains scalar because x87 operations are ordered and
//! stack-dependent, so each physical x87 register has a fixed scalar host register.

use super::{
    encoder::{Encoder, Label, Reg, Xmm},
    native_x87::{
        NativeX87BinaryOp, NativeX87Insn, NativeX87PopOp, X87_CONDITION_MASK, X87_TOP_MASK,
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
        NativeX87Insn::StoreI32 { .. } => {
            let memory = context.memory.expect("FISTP needs a host pointer");
            emit_load_physical(e, top, VALUE0, context.side_exit);
            emit_fistp_chop_guard(e, context.cpu, context.side_exit);
            e.vcvttsd2si_r32(Reg::RDX, VALUE0);
            e.store_r32_disp32(memory, 0, Reg::RDX);
            emit_pop(e, top);
        }
        NativeX87Insn::PopBinary { op, index } => {
            let destination = physical(top, index);
            emit_load_physical(e, destination, VALUE0, context.side_exit);
            emit_load_physical(e, top, VALUE1, context.side_exit);
            match op {
                NativeX87PopOp::Add => e.vaddsd(VALUE0, VALUE0, VALUE1),
                NativeX87PopOp::Multiply => e.vmulsd(VALUE0, VALUE0, VALUE1),
                NativeX87PopOp::Subtract => e.vsubsd(VALUE0, VALUE0, VALUE1),
                NativeX87PopOp::Divide => e.vdivsd(VALUE0, VALUE0, VALUE1),
                NativeX87PopOp::SubtractReverse => e.vsubsd(VALUE0, VALUE1, VALUE0),
                NativeX87PopOp::DivideReverse => e.vdivsd(VALUE0, VALUE1, VALUE0),
            }
            emit_finite_guard(e, VALUE0, context.side_exit);
            emit_store_physical(e, destination, VALUE0);
            emit_pop(e, top);
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

fn emit_fistp_chop_guard(e: &mut Encoder, cpu: Reg, side_exit: Label) {
    e.movzx_r32_word_disp32(Reg::RAX, cpu, control_offset());
    e.and_r32_imm32(Reg::RAX, 0x0c00);
    e.cmp_r32_imm32(Reg::RAX, 0x0c00);
    e.jnz(side_exit);

    e.mov_r64_imm64(Reg::RDX, (-2_147_483_649.0f64).to_bits());
    e.vmovq_xmm_r64(VALUE1, Reg::RDX);
    e.vucomisd(VALUE0, VALUE1);
    e.jcc(6, side_exit);
    e.mov_r64_imm64(Reg::RDX, 2_147_483_648.0f64.to_bits());
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

const fn status_offset() -> i32 {
    (fpu_base_offset() + native_x87_layout().status) as i32
}

const fn tag_offset() -> i32 {
    (fpu_base_offset() + native_x87_layout().tag) as i32
}

#[cfg(test)]
#[path = "x87_avx2_emit_test.rs"]
mod tests;
