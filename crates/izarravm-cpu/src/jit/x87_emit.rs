// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Direct SSE2 lowering for the profiled x87 instruction slice.

use super::{
    encoder::{Encoder, Label, Reg, Xmm},
    native_x87::{
        NativeX87BinaryOp, NativeX87Insn, NativeX87PopOp, X87_CONDITION_MASK, X87_TOP_MASK,
        X87_TOP_SHIFT, native_x87_layout,
    },
};
use crate::{CR0_EM, CR0_NE, CR0_TS, ControlRegisters, CpuGsw};

#[derive(Clone, Copy)]
pub(crate) struct NativeX87EmitContext {
    pub(crate) cpu: Reg,
    pub(crate) memory: Option<Reg>,
    pub(crate) side_exit: Label,
    pub(crate) check_gate: bool,
}

pub(crate) fn emit_native_x87(e: &mut Encoder, insn: NativeX87Insn, context: NativeX87EmitContext) {
    if context.check_gate {
        emit_gate(e, context.cpu, context.side_exit);
    }
    match insn {
        NativeX87Insn::BinaryMemory { op, .. } => {
            let memory = context
                .memory
                .expect("x87 memory source needs a host pointer");
            emit_load_st(e, context.cpu, 0, Xmm::XMM0, context.side_exit);
            e.movss_xmm_disp32(Xmm::XMM1, memory, 0);
            e.cvtss2sd(Xmm::XMM1, Xmm::XMM1);
            emit_finite_guard(e, Xmm::XMM1, context.side_exit);
            emit_binary_st0(e, context.cpu, op, context.side_exit);
        }
        NativeX87Insn::BinaryRegister { op, index } => {
            emit_load_st(e, context.cpu, 0, Xmm::XMM0, context.side_exit);
            emit_load_st(e, context.cpu, index, Xmm::XMM1, context.side_exit);
            emit_binary_st0(e, context.cpu, op, context.side_exit);
        }
        NativeX87Insn::LoadF32 { .. } => {
            let memory = context.memory.expect("FLD needs a host pointer");
            e.movss_xmm_disp32(Xmm::XMM0, memory, 0);
            e.cvtss2sd(Xmm::XMM0, Xmm::XMM0);
            emit_finite_guard(e, Xmm::XMM0, context.side_exit);
            emit_push(e, context.cpu, Xmm::XMM0);
        }
        NativeX87Insn::StoreF32 { pop, .. } => {
            let memory = context.memory.expect("FST needs a host pointer");
            emit_load_st(e, context.cpu, 0, Xmm::XMM0, context.side_exit);
            e.cvtsd2ss(Xmm::XMM0, Xmm::XMM0);
            e.movss_disp32_xmm(memory, 0, Xmm::XMM0);
            if pop {
                emit_pop(e, context.cpu);
            }
        }
        NativeX87Insn::LoadRegister { index } => {
            emit_load_st(e, context.cpu, index, Xmm::XMM0, context.side_exit);
            emit_push(e, context.cpu, Xmm::XMM0);
        }
        NativeX87Insn::Exchange { index } => {
            emit_load_st(e, context.cpu, 0, Xmm::XMM0, context.side_exit);
            emit_load_st(e, context.cpu, index, Xmm::XMM1, context.side_exit);
            emit_store_st(e, context.cpu, 0, Xmm::XMM1);
            emit_store_st(e, context.cpu, index, Xmm::XMM0);
        }
        NativeX87Insn::LoadI32 { .. } => {
            let memory = context.memory.expect("FILD needs a host pointer");
            e.load_r32_disp32(Reg::RDX, memory, 0);
            e.cvtsi2sd_r32(Xmm::XMM0, Reg::RDX);
            emit_push(e, context.cpu, Xmm::XMM0);
        }
        NativeX87Insn::StoreI32 { .. } => {
            let memory = context.memory.expect("FISTP needs a host pointer");
            emit_load_st(e, context.cpu, 0, Xmm::XMM0, context.side_exit);
            emit_fistp_chop_guard(e, context.cpu, context.side_exit);
            e.cvttsd2si_r32(Reg::RDX, Xmm::XMM0);
            e.store_r32_disp32(memory, 0, Reg::RDX);
            emit_pop(e, context.cpu);
        }
        NativeX87Insn::PopBinary { op, index } => {
            emit_load_st(e, context.cpu, index, Xmm::XMM0, context.side_exit);
            emit_load_st(e, context.cpu, 0, Xmm::XMM1, context.side_exit);
            match op {
                NativeX87PopOp::Add => e.addsd(Xmm::XMM0, Xmm::XMM1),
                NativeX87PopOp::Multiply => e.mulsd(Xmm::XMM0, Xmm::XMM1),
            }
            emit_finite_guard(e, Xmm::XMM0, context.side_exit);
            emit_store_st(e, context.cpu, index, Xmm::XMM0);
            emit_pop(e, context.cpu);
        }
        NativeX87Insn::StoreStatusAx => {
            e.movzx_r32_word_disp32(Reg::RAX, context.cpu, status_offset());
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
    e.movzx_r32_word_disp32(Reg::RAX, cpu, status_offset());
    e.and_r32_imm32(Reg::RAX, 0x3f);
    e.movzx_r32_word_disp32(Reg::RDX, cpu, control_offset());
    e.alu_r32_imm32(6, Reg::RDX, 0x3f);
    e.alu_r32_r32(4, Reg::RAX, Reg::RDX);
    e.cmp_r32_imm32(Reg::RAX, 0);
    e.jnz(side_exit);
    e.place(ready);
}

fn emit_load_st(e: &mut Encoder, cpu: Reg, logical: u8, dst: Xmm, side_exit: Label) {
    emit_physical_index(e, cpu, logical, Reg::RCX);
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    e.shl_r32_imm8(Reg::RCX, 1);
    e.movzx_r32_word_disp32(Reg::RDX, cpu, tag_offset());
    e.shr_r32_cl(Reg::RDX);
    e.and_r32_imm32(Reg::RDX, 3);
    e.cmp_r32_imm32(Reg::RDX, 2);
    e.jcc(3, side_exit);
    e.movsd_xmm_sib_scale8_disp32(dst, cpu, Reg::RAX, st_offset());
    emit_finite_guard(e, dst, side_exit);
}

fn emit_finite_guard(e: &mut Encoder, value: Xmm, side_exit: Label) {
    e.movq_r64_xmm(Reg::RDX, value);
    e.shift_r64_imm8(5, Reg::RDX, 52);
    e.and_r32_imm32(Reg::RDX, 0x7ff);
    e.cmp_r32_imm32(Reg::RDX, 0x7ff);
    e.jz(side_exit);
}

fn emit_binary_st0(e: &mut Encoder, cpu: Reg, op: NativeX87BinaryOp, side_exit: Label) {
    if op.is_compare() {
        emit_compare(e, cpu, Xmm::XMM0, Xmm::XMM1);
        if op.pops() {
            emit_pop(e, cpu);
        }
        return;
    }
    match op {
        NativeX87BinaryOp::Add => e.addsd(Xmm::XMM0, Xmm::XMM1),
        NativeX87BinaryOp::Multiply => e.mulsd(Xmm::XMM0, Xmm::XMM1),
        NativeX87BinaryOp::Subtract => e.subsd(Xmm::XMM0, Xmm::XMM1),
        NativeX87BinaryOp::Divide => e.divsd(Xmm::XMM0, Xmm::XMM1),
        NativeX87BinaryOp::SubtractReverse => {
            e.movsd_xmm_xmm(Xmm::XMM2, Xmm::XMM1);
            e.subsd(Xmm::XMM2, Xmm::XMM0);
            e.movsd_xmm_xmm(Xmm::XMM0, Xmm::XMM2);
        }
        NativeX87BinaryOp::DivideReverse => {
            e.movsd_xmm_xmm(Xmm::XMM2, Xmm::XMM1);
            e.divsd(Xmm::XMM2, Xmm::XMM0);
            e.movsd_xmm_xmm(Xmm::XMM0, Xmm::XMM2);
        }
        NativeX87BinaryOp::Compare | NativeX87BinaryOp::ComparePop => unreachable!(),
    }
    emit_finite_guard(e, Xmm::XMM0, side_exit);
    emit_store_st(e, cpu, 0, Xmm::XMM0);
}

fn emit_compare(e: &mut Encoder, cpu: Reg, lhs: Xmm, rhs: Xmm) {
    let equal = e.label();
    let below = e.label();
    let done = e.label();
    e.ucomisd(lhs, rhs);
    e.jz(equal);
    e.jcc(2, below);
    emit_condition(e, cpu, 0);
    e.jmp(done);
    e.place(equal);
    emit_condition(e, cpu, 1 << 14);
    e.jmp(done);
    e.place(below);
    emit_condition(e, cpu, 1 << 8);
    e.place(done);
}

fn emit_condition(e: &mut Encoder, cpu: Reg, set: u32) {
    e.movzx_r32_word_disp32(Reg::RAX, cpu, status_offset());
    e.and_r32_imm32(Reg::RAX, !u32::from(X87_CONDITION_MASK) & 0xffff);
    if set != 0 {
        e.or_r32_imm32(Reg::RAX, set);
    }
    e.store_r16_disp32(cpu, status_offset(), Reg::RAX);
}

fn emit_push(e: &mut Encoder, cpu: Reg, value: Xmm) {
    e.movzx_r32_word_disp32(Reg::RDX, cpu, status_offset());
    e.mov_r32_r32(Reg::RAX, Reg::RDX);
    e.shr_r32_imm8(Reg::RAX, X87_TOP_SHIFT as u8);
    e.add_r32_imm32(Reg::RAX, 7);
    e.and_r32_imm32(Reg::RAX, 7);
    e.and_r32_imm32(Reg::RDX, !u32::from(X87_TOP_MASK) & 0xffff);
    e.shl_r32_imm8(Reg::RAX, X87_TOP_SHIFT as u8);
    e.or_r32_r32(Reg::RDX, Reg::RAX);
    e.store_r16_disp32(cpu, status_offset(), Reg::RDX);
    emit_store_st(e, cpu, 0, value);
}

fn emit_pop(e: &mut Encoder, cpu: Reg) {
    emit_physical_index(e, cpu, 0, Reg::RCX);
    e.mov_r64_r64(Reg::RAX, cpu);
    e.add_r64_imm32(Reg::RAX, tag_offset() as u32);
    e.mov_r32_r32(Reg::RDX, Reg::RCX);
    e.shl_r32_imm8(Reg::RDX, 1);
    e.bts_r16_mem(Reg::RAX, Reg::RDX);
    e.add_r32_imm32(Reg::RDX, 1);
    e.bts_r16_mem(Reg::RAX, Reg::RDX);

    e.movzx_r32_word_disp32(Reg::RDX, cpu, status_offset());
    e.mov_r32_r32(Reg::RAX, Reg::RDX);
    e.shr_r32_imm8(Reg::RAX, X87_TOP_SHIFT as u8);
    e.add_r32_imm32(Reg::RAX, 1);
    e.and_r32_imm32(Reg::RAX, 7);
    e.and_r32_imm32(Reg::RDX, !u32::from(X87_TOP_MASK) & 0xffff);
    e.shl_r32_imm8(Reg::RAX, X87_TOP_SHIFT as u8);
    e.or_r32_r32(Reg::RDX, Reg::RAX);
    e.store_r16_disp32(cpu, status_offset(), Reg::RDX);
}

fn emit_store_st(e: &mut Encoder, cpu: Reg, logical: u8, value: Xmm) {
    emit_physical_index(e, cpu, logical, Reg::RCX);
    e.movsd_sib_scale8_disp32_xmm(cpu, Reg::RCX, st_offset(), value);

    e.mov_r64_r64(Reg::RAX, cpu);
    e.add_r64_imm32(Reg::RAX, tag_offset() as u32);
    e.mov_r32_r32(Reg::RDX, Reg::RCX);
    e.shl_r32_imm8(Reg::RDX, 1);
    e.btr_r16_mem(Reg::RAX, Reg::RDX);
    e.add_r32_imm32(Reg::RDX, 1);
    e.btr_r16_mem(Reg::RAX, Reg::RDX);

    e.xorpd(Xmm::XMM2, Xmm::XMM2);
    e.ucomisd(value, Xmm::XMM2);
    let nonzero = e.label();
    e.jnz(nonzero);
    e.alu_r32_imm32(5, Reg::RDX, 1);
    e.bts_r16_mem(Reg::RAX, Reg::RDX);
    e.place(nonzero);
}

fn emit_fistp_chop_guard(e: &mut Encoder, cpu: Reg, side_exit: Label) {
    e.movzx_r32_word_disp32(Reg::RAX, cpu, control_offset());
    e.and_r32_imm32(Reg::RAX, 0x0c00);
    e.cmp_r32_imm32(Reg::RAX, 0x0c00);
    e.jnz(side_exit);

    e.mov_r64_imm64(Reg::RDX, (-2_147_483_649.0f64).to_bits());
    e.movq_xmm_r64(Xmm::XMM1, Reg::RDX);
    e.ucomisd(Xmm::XMM0, Xmm::XMM1);
    e.jcc(6, side_exit);
    e.mov_r64_imm64(Reg::RDX, 2_147_483_648.0f64.to_bits());
    e.movq_xmm_r64(Xmm::XMM1, Reg::RDX);
    e.ucomisd(Xmm::XMM0, Xmm::XMM1);
    e.jcc(3, side_exit);
}

fn emit_physical_index(e: &mut Encoder, cpu: Reg, logical: u8, dst: Reg) {
    e.movzx_r32_word_disp32(dst, cpu, status_offset());
    e.shr_r32_imm8(dst, X87_TOP_SHIFT as u8);
    if logical != 0 {
        e.add_r32_imm32(dst, u32::from(logical));
    }
    e.and_r32_imm32(dst, 7);
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
mod tests {
    use super::*;
    use crate::jit::exec_mem::ExecutableBuffer;

    #[cfg(target_os = "windows")]
    const CPU_ARG: Reg = Reg::RCX;
    #[cfg(not(target_os = "windows"))]
    const CPU_ARG: Reg = Reg::RDI;
    #[cfg(target_os = "windows")]
    const MEMORY_ARG: Reg = Reg::RDX;
    #[cfg(not(target_os = "windows"))]
    const MEMORY_ARG: Reg = Reg::RSI;

    fn execute(cpu: &mut CpuGsw, memory: &mut u32, insn: NativeX87Insn) -> bool {
        let mut e = Encoder::new();
        e.push(Reg::R15);
        e.push(Reg::RDI);
        e.mov_r64_r64(Reg::R15, CPU_ARG);
        e.mov_r64_r64(Reg::RDI, MEMORY_ARG);
        let side_exit = e.label();
        let done = e.label();
        emit_native_x87(
            &mut e,
            insn,
            NativeX87EmitContext {
                cpu: Reg::R15,
                memory: Some(Reg::RDI),
                side_exit,
                check_gate: true,
            },
        );
        e.mov_r32_imm32(Reg::RAX, 0);
        e.jmp(done);
        e.place(side_exit);
        e.mov_r32_imm32(Reg::RAX, 1);
        e.place(done);
        e.pop(Reg::RDI);
        e.pop(Reg::R15);
        e.ret();
        let code = e.finish();
        assert!(
            code.len() < 512,
            "single x87 slot emitted {} bytes",
            code.len()
        );
        let buffer = ExecutableBuffer::new(&code).expect("x87 test code allocation");
        let function: unsafe extern "C" fn(*mut CpuGsw, *mut u32) -> u32 =
            unsafe { core::mem::transmute(buffer.entry_ptr()) };
        unsafe { function(cpu, memory) != 0 }
    }

    fn cpu_with_stack(values: &[f64]) -> CpuGsw {
        let mut cpu = CpuGsw::default();
        for &value in values.iter().rev() {
            cpu.fpu.push(value);
        }
        cpu
    }

    #[test]
    fn register_arithmetic_compare_and_stack_changes_execute() {
        let mut cpu = cpu_with_stack(&[2.0, 3.0]);
        let mut memory = 0;
        assert!(!execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::BinaryRegister {
                op: NativeX87BinaryOp::Add,
                index: 1,
            }
        ));
        assert_eq!(cpu.fpu.get(0), 5.0);
        assert_eq!(cpu.fpu.get(1), 3.0);

        assert!(!execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::BinaryRegister {
                op: NativeX87BinaryOp::ComparePop,
                index: 1,
            }
        ));
        assert_eq!(cpu.fpu.top(), 7);
        assert_eq!(cpu.fpu.get(0), 3.0);
        assert_eq!(cpu.fpu.status & X87_CONDITION_MASK, 0);
    }

    #[test]
    fn memory_load_store_and_chop_conversion_execute() {
        let mut cpu = cpu_with_stack(&[]);
        let mut memory = 1.5f32.to_bits();
        assert!(!execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::LoadF32 { addr: dummy_addr() }
        ));
        assert_eq!(cpu.fpu.get(0), 1.5);

        cpu.fpu.control = 0x0f7f;
        cpu.fpu.set(0, -3.9);
        assert!(!execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::StoreI32 { addr: dummy_addr() }
        ));
        assert_eq!(memory as i32, -3);
        assert_eq!(cpu.fpu.top(), 0);
    }

    #[test]
    fn exceptional_values_and_architectural_gates_exit_before_mutation() {
        let mut cpu = cpu_with_stack(&[1.0, f64::INFINITY]);
        let before = cpu.fpu.clone();
        let mut memory = 0x1234_5678;
        assert!(execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::BinaryRegister {
                op: NativeX87BinaryOp::Add,
                index: 1,
            }
        ));
        assert_eq!(cpu.fpu, before);
        assert_eq!(memory, 0x1234_5678);

        cpu.control.cr0 = CR0_TS;
        assert!(execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::LoadI32 { addr: dummy_addr() }
        ));
        assert_eq!(cpu.fpu, before);

        cpu.control.cr0 = 0;
        cpu.fpu.control = 0x037f;
        cpu.fpu.set(0, 3.5);
        let before = cpu.fpu.clone();
        assert!(execute(
            &mut cpu,
            &mut memory,
            NativeX87Insn::StoreI32 { addr: dummy_addr() }
        ));
        assert_eq!(cpu.fpu, before);
        assert_eq!(memory, 0x1234_5678);
    }

    fn dummy_addr() -> crate::AddrMode {
        crate::AddrMode {
            segment: crate::SegmentIndex::Ds,
            base: None,
            index: None,
            scale: 1,
            disp: 0,
            address_size: crate::AddressSize::Dword,
        }
    }
}
