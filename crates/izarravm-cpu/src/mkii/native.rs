// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::ops::{Input, Operation, Pure};
use super::runtime::Frame;
use crate::jit::encoder::{Encoder, Label, Reg};
use crate::jit::exec_mem::ExecutableBuffer;
use crate::{BusWidth, CpuGsw, PendingFlags, Registers};

#[path = "region_native.rs"]
mod region;

const SAVED: [Reg; 3] = [Reg::RBX, Reg::R12, Reg::R13];
const STACK_BYTES: u32 = 32;

#[cfg(target_os = "windows")]
const ARGS: [Reg; 4] = [Reg::RCX, Reg::RDX, Reg::R8, Reg::R9];
#[cfg(not(target_os = "windows"))]
const ARGS: [Reg; 4] = [Reg::RDI, Reg::RSI, Reg::RDX, Reg::RCX];

pub(super) fn compile(operations: &[Operation]) -> Option<ExecutableBuffer> {
    let mut e = Encoder::new();
    let info = prologue(&mut e);
    e.mov_r64_r64(Reg::RBX, ARGS[0]);
    e.mov_r64_r64(Reg::R12, ARGS[1]);
    e.mov_r64_r64(Reg::R13, ARGS[2]);
    let exit = e.label();
    let mut index = 0;
    while index < operations.len() {
        let operation = &operations[index];
        if operation.region_len < 2 {
            index = emit_legacy_span(&mut e, operations, index, exit);
            continue;
        }
        let end = index + operation.region_len;
        let slow = e.label();
        let next = e.label();
        call_helper(&mut e, 13, operation as *const Operation as usize);
        e.cmp_r32_imm32(Reg::RAX, 2);
        e.jcc(4, exit);
        e.test_r32_r32(Reg::RAX, Reg::RAX);
        e.jcc(4, slow);
        region::emit(&mut e, &operations[index..end], exit);
        e.jmp(next);
        e.place(slow);
        while index < end {
            index = emit_legacy_span(&mut e, operations, index, exit);
        }
        e.place(next);
    }
    call_helper(&mut e, 11, 0);
    e.place(exit);
    epilogue(&mut e);
    ExecutableBuffer::new_with_unwind(&e.finish(), &info)
}

fn emit_legacy_span(e: &mut Encoder, operations: &[Operation], index: usize, exit: Label) -> usize {
    let operation = &operations[index];
    let end = index + operation.span_len;
    let slow = e.label();
    let next = e.label();
    if operation.span_len > 1 {
        call_helper(e, 12, operation as *const Operation as usize);
        e.cmp_r32_imm32(Reg::RAX, 2);
        e.jcc(4, exit);
        e.test_r32_r32(Reg::RAX, Reg::RAX);
        e.jcc(4, slow);
        if operation.memory_cmp_branch {
            emit_memory_cmp_branch(e, operation, &operations[index + 1]);
        } else {
            for op in &operations[index..end] {
                emit_pure(e, op.pure.unwrap().0);
            }
        }
        e.jmp(next);
    }
    e.place(slow);
    for operation in &operations[index..end] {
        call_helper(e, operation.helper, operation as *const Operation as usize);
        e.test_r32_r32(Reg::RAX, Reg::RAX);
        e.jcc(4, exit);
        if let Some((pure, _)) = operation.pure {
            emit_pure(e, pure);
        }
    }
    e.place(next);
    end
}

fn call_helper(e: &mut Encoder, slot: usize, operation: usize) {
    e.mov_r64_r64(ARGS[0], Reg::RBX);
    e.mov_r64_r64(ARGS[1], Reg::R12);
    e.mov_r64_r64(ARGS[2], Reg::R13);
    e.mov_r64_imm64(ARGS[3], operation as u64);
    e.call_m64_disp32(
        Reg::R13,
        (std::mem::offset_of!(Frame, helpers) + slot * std::mem::size_of::<usize>()) as i32,
    );
}

fn gpr_offset(index: u8, width: BusWidth) -> i32 {
    let (index, high) = if width == BusWidth::Byte {
        (index & 3, index >> 2)
    } else {
        (index, 0)
    };
    (std::mem::offset_of!(CpuGsw, registers)
        + std::mem::offset_of!(Registers, gpr)
        + usize::from(index) * 4
        + usize::from(high)) as i32
}

fn load(e: &mut Encoder, dst: Reg, input: Input, width: BusWidth) {
    match input {
        Input::Immediate(value) => e.mov_r32_imm32(dst, value & mask(width)),
        Input::Reg(index) => {
            let offset = gpr_offset(index, width);
            match width {
                BusWidth::Byte => e.movzx_r32_byte_disp32(dst, Reg::RBX, offset),
                BusWidth::Word => e.movzx_r32_word_disp32(dst, Reg::RBX, offset),
                BusWidth::Dword => e.load_r32_disp32(dst, Reg::RBX, offset),
            }
        }
    }
}

fn store(e: &mut Encoder, index: u8, src: Reg, width: BusWidth) {
    let offset = gpr_offset(index, width);
    match width {
        BusWidth::Byte => {
            e.mov_r64_r64(Reg::R11, Reg::RBX);
            e.add_r64_imm32(Reg::R11, offset as u32);
            e.store_r8_disp8(Reg::R11, 0, src);
        }
        BusWidth::Word => e.store_r16_disp32(Reg::RBX, offset, src),
        BusWidth::Dword => e.store_r32_disp32(Reg::RBX, offset, src),
    }
}

fn mask(width: BusWidth) -> u32 {
    match width {
        BusWidth::Byte => 0xff,
        BusWidth::Word => 0xffff,
        BusWidth::Dword => u32::MAX,
    }
}

fn emit_pure(e: &mut Encoder, pure: Pure) {
    match pure {
        Pure::Nop => {}
        Pure::Mov { dst, src, width } => {
            load(e, Reg::RAX, src, width);
            store(e, dst, Reg::RAX, width);
        }
        Pure::Alu {
            op,
            dst,
            src,
            width,
            store: write,
        } => {
            load(e, Reg::RAX, Input::Reg(dst), width);
            load(e, Reg::RDX, src, width);
            emit_arithmetic(e, op, width);
            if write {
                store(e, dst, Reg::RCX, width);
            }
        }
    }
}

fn emit_arithmetic(e: &mut Encoder, op: u8, width: BusWidth) {
    let op = carry_free_alu(op);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.alu_r32_r32(if op == 7 { 5 } else { op }, Reg::RCX, Reg::RDX);
    if width != BusWidth::Dword {
        e.alu_r32_imm32(4, Reg::RCX, mask(width));
    }
    let offset = std::mem::offset_of!(CpuGsw, pending_flags) as i32;
    let operation = match op {
        0 => 0,
        5 | 7 => 1,
        _ => 2,
    };
    let width_tag = match width {
        BusWidth::Byte => 0,
        BusWidth::Word => 1,
        BusWidth::Dword => 2,
    };
    e.store_u32_imm_disp32(
        Reg::RBX,
        offset + std::mem::offset_of!(PendingFlags, tag) as i32,
        0x8000_0000 | (width_tag << 8) | operation,
    );
    for (field, register) in [
        (std::mem::offset_of!(PendingFlags, a), Reg::RAX),
        (std::mem::offset_of!(PendingFlags, b), Reg::RDX),
        (std::mem::offset_of!(PendingFlags, result), Reg::RCX),
    ] {
        e.store_r32_disp32(Reg::RBX, offset + field as i32, register);
    }
}

fn emit_memory_cmp_branch(e: &mut Encoder, compare: &Operation, branch: &Operation) {
    let width = if compare.insn.opcode == 0x3a {
        BusWidth::Byte
    } else {
        compare.insn.operand_size.bus_width()
    };
    load(
        e,
        Reg::RAX,
        Input::Reg(compare.insn.modrm.unwrap().reg),
        width,
    );
    e.load_r64_disp32(
        Reg::R11,
        Reg::R13,
        std::mem::offset_of!(Frame, memory_ptr) as i32,
    );
    match width {
        BusWidth::Byte => e.movzx_r32_byte_disp32(Reg::RDX, Reg::R11, 0),
        BusWidth::Word => e.movzx_r32_word_disp32(Reg::RDX, Reg::R11, 0),
        BusWidth::Dword => e.load_r32_disp32(Reg::RDX, Reg::R11, 0),
    }
    emit_arithmetic(e, 7, width);
    emit_branch(e, branch, 7, width);
}

fn carry_free_alu(op: u8) -> u8 {
    match op {
        2 => 0,
        3 => 5,
        op => op,
    }
}

fn emit_branch(e: &mut Encoder, branch: &Operation, op: u8, width: BusWidth) {
    let op = carry_free_alu(op);
    match width {
        BusWidth::Byte => e.alu_r8_r8(op, Reg::RAX, Reg::RDX),
        BusWidth::Word => e.alu_r16_r16(op, Reg::RAX, Reg::RDX),
        BusWidth::Dword => e.alu_r32_r32(op, Reg::RAX, Reg::RDX),
    }
    let untaken = e.label();
    e.jcc((branch.insn.opcode as u8 & 15) ^ 1, untaken);
    let target = branch
        .eip
        .wrapping_add(u32::from(branch.insn.len))
        .wrapping_add(branch.insn.imm)
        & branch.insn.operand_size.mask();
    e.store_u32_imm_disp32(
        Reg::RBX,
        (std::mem::offset_of!(CpuGsw, registers) + std::mem::offset_of!(Registers, eip)) as i32,
        target,
    );
    e.store_u32_imm_disp32(
        Reg::R13,
        std::mem::offset_of!(Frame, branch_taken) as i32,
        1,
    );
    e.place(untaken);
}

fn prologue(e: &mut Encoder) -> Vec<u8> {
    let mut saves = Vec::with_capacity(SAVED.len());
    for reg in SAVED {
        e.push(reg);
        saves.push((e.position() as u8, reg.0 << 4));
    }
    e.sub_r64_imm32(Reg::RSP, STACK_BYTES);
    let end = e.position() as u8;
    let mut info = vec![1, end, 4, 0, end, 0x32];
    for (offset, operation) in saves.into_iter().rev() {
        info.extend_from_slice(&[offset, operation]);
    }
    info
}

fn epilogue(e: &mut Encoder) {
    e.add_r64_imm32(Reg::RSP, STACK_BYTES);
    for reg in SAVED.into_iter().rev() {
        e.pop(reg);
    }
    e.ret();
}

#[cfg(test)]
#[path = "native_test.rs"]
mod tests;
