// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::jit::fast_map::{
    NATIVE_LOAD_BIAS_MODE13, NATIVE_LOAD_BIAS_SUPERVISOR, NATIVE_LOAD_BIAS_TAG_MASK,
};
use crate::mkii::ops::Read;
use crate::{AddressSize, SegmentRegister};

pub(super) fn emit(e: &mut Encoder, operations: &[Operation], exit: Label) {
    let commit = e.label();
    let done = e.label();
    let mut exits = Vec::new();
    for (index, op) in operations.iter().enumerate() {
        if op.needs_carry_zero() {
            let miss = e.label();
            emit_carry_zero_guard(e, miss);
            exits.push((miss, index, 2));
        }
        if let Some((pure, _)) = op.region_pure() {
            emit_pure(e, pure);
        } else if let Some(read) = op.read {
            let miss = e.label();
            emit_read(e, read, miss);
            exits.push((miss, index, 1));
        } else {
            let (alu, width) = operations[index - 1].branch_alu().unwrap();
            let taken = e.label();
            emit_branch(e, op, alu, width, taken);
            exits.push((taken, index + 1, 0));
        }
    }
    e.store_u32_imm_disp32(
        Reg::R13,
        std::mem::offset_of!(Frame, region_completed) as i32,
        operations.len() as u32,
    );
    let last = operations.last().unwrap();
    if operations.len() >= 2
        && last
            .eip
            .checked_add(u32::from(last.insn.len))
            .is_some_and(|eip| eip < 0x10000)
    {
        e.load_r32_disp32(
            Reg::RAX,
            Reg::R13,
            std::mem::offset_of!(Frame, region_inert) as i32,
        );
        e.test_r32_r32(Reg::RAX, Reg::RAX);
        e.jcc(4, commit);
        emit_inert_finish(e, operations);
        e.jmp(done);
    }
    e.jmp(commit);
    for (label, completed, reason) in exits {
        e.place(label);
        e.store_u32_imm_disp32(
            Reg::R13,
            std::mem::offset_of!(Frame, region_completed) as i32,
            completed as u32,
        );
        e.store_u32_imm_disp32(
            Reg::R13,
            std::mem::offset_of!(Frame, region_guard_miss) as i32,
            reason,
        );
        e.jmp(commit);
    }
    e.place(commit);
    call_helper(e, 14, operations.as_ptr() as usize);
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(4, exit);
    e.place(done);
}

fn emit_inert_finish(e: &mut Encoder, operations: &[Operation]) {
    let cost = &operations[0].region.as_ref().unwrap().prefixes[operations.len()];
    let perf = std::mem::offset_of!(CpuGsw, perf);
    let stats = std::mem::offset_of!(Frame, stats);
    let last = operations.last().unwrap();
    e.store_u32_imm_disp32(
        Reg::R13,
        std::mem::offset_of!(Frame, region_inert) as i32,
        0,
    );
    e.store_u32_imm_disp32(
        Reg::RBX,
        (std::mem::offset_of!(CpuGsw, registers) + std::mem::offset_of!(Registers, eip)) as i32,
        last.eip + u32::from(last.insn.len),
    );
    e.load_r64_disp32(
        Reg::RAX,
        Reg::R13,
        std::mem::offset_of!(Frame, region_full_rem) as i32,
    );
    e.store_r64_disp32(
        Reg::RBX,
        std::mem::offset_of!(CpuGsw, timing_rem) as i32,
        Reg::RAX,
    );
    e.load_r64_disp32(
        Reg::RAX,
        Reg::R13,
        std::mem::offset_of!(Frame, region_full_core) as i32,
    );
    for (base, offset) in [
        (Reg::RBX, std::mem::offset_of!(CpuGsw, elapsed_clocks)),
        (Reg::R13, std::mem::offset_of!(Frame, total)),
    ] {
        e.load_r64_disp32(Reg::RDX, base, offset as i32);
        e.add_r64_r64(Reg::RDX, Reg::RAX);
        e.store_r64_disp32(base, offset as i32, Reg::RDX);
    }
    let counters = e.label();
    e.load_r32_disp32(
        Reg::RDX,
        Reg::R13,
        std::mem::offset_of!(Frame, region_monitor) as i32,
    );
    e.test_r32_r32(Reg::RDX, Reg::RDX);
    e.jcc(4, counters);
    let monitor =
        (perf + std::mem::offset_of!(crate::PerfCounters, monitor_resident_core_clocks)) as i32;
    e.load_r64_disp32(Reg::RDX, Reg::RBX, monitor);
    e.add_r64_r64(Reg::RDX, Reg::RAX);
    e.store_r64_disp32(Reg::RBX, monitor, Reg::RDX);
    e.place(counters);
    for (base, offset, count) in [
        (
            Reg::RBX,
            perf + std::mem::offset_of!(crate::PerfCounters, instructions),
            operations.len() as u64,
        ),
        (
            Reg::RBX,
            perf + std::mem::offset_of!(crate::PerfCounters, data_direct_reads),
            cost.reads,
        ),
        (
            Reg::RBX,
            perf + std::mem::offset_of!(crate::PerfCounters, direct_data_pointer_reads),
            cost.reads,
        ),
        (
            Reg::RBX,
            std::mem::offset_of!(CpuGsw, fast_map_probe)
                + std::mem::offset_of!(crate::FastMapProbeCounters, hits),
            cost.reads,
        ),
        (
            Reg::R13,
            stats + std::mem::offset_of!(crate::mkii::runtime::Stats, native),
            operations.len() as u64,
        ),
        (
            Reg::R13,
            stats + std::mem::offset_of!(crate::mkii::runtime::Stats, carry_native),
            cost.carry_ops,
        ),
    ] {
        if count != 0 {
            e.load_r64_disp32(Reg::RDX, base, offset as i32);
            e.add_r64_imm32(Reg::RDX, u32::try_from(count).unwrap());
            e.store_r64_disp32(base, offset as i32, Reg::RDX);
        }
    }
}

fn emit_carry_zero_guard(e: &mut Encoder, miss: Label) {
    let pending = std::mem::offset_of!(CpuGsw, pending_flags) as i32;
    let tag = pending + std::mem::offset_of!(PendingFlags, tag) as i32;
    let live = e.label();
    let override_cf = e.label();
    let subtract = e.label();
    let allowed = e.label();
    e.load_r32_disp32(Reg::R8, Reg::RBX, tag);
    e.test_r32_imm32(Reg::R8, 1 << 31);
    e.jcc(4, live);
    e.test_r32_imm32(Reg::R8, 1 << 16);
    e.jcc(5, override_cf);
    e.alu_r32_imm32(4, Reg::R8, 255);
    e.cmp_r32_imm32(Reg::R8, 1);
    e.jcc(7, allowed);
    e.load_r32_disp32(
        Reg::R9,
        Reg::RBX,
        pending + std::mem::offset_of!(PendingFlags, a) as i32,
    );
    e.load_r32_disp32(
        Reg::R10,
        Reg::RBX,
        pending + std::mem::offset_of!(PendingFlags, b) as i32,
    );
    e.test_r32_r32(Reg::R8, Reg::R8);
    e.jcc(5, subtract);
    e.add_r64_r64(Reg::R9, Reg::R10);
    e.load_r32_disp32(Reg::R8, Reg::RBX, tag);
    e.shift_r32_imm8(5, Reg::R8, 8);
    e.alu_r32_imm32(4, Reg::R8, 255);
    let word = e.label();
    let dword = e.label();
    let compare = e.label();
    e.test_r32_r32(Reg::R8, Reg::R8);
    e.jcc(5, word);
    e.mov_r32_imm32(Reg::R10, 255);
    e.jmp(compare);
    e.place(word);
    e.cmp_r32_imm32(Reg::R8, 1);
    e.jcc(5, dword);
    e.mov_r32_imm32(Reg::R10, 65535);
    e.jmp(compare);
    e.place(dword);
    e.mov_r32_imm32(Reg::R10, u32::MAX);
    e.place(compare);
    e.cmp_r64_r64(Reg::R9, Reg::R10);
    e.jcc(7, miss);
    e.jmp(allowed);
    e.place(subtract);
    e.alu_r32_r32(7, Reg::R9, Reg::R10);
    e.jcc(2, miss);
    e.jmp(allowed);
    e.place(override_cf);
    e.test_r32_imm32(Reg::R8, 1 << 17);
    e.jcc(5, miss);
    e.jmp(allowed);
    e.place(live);
    e.load_r32_disp32(
        Reg::R8,
        Reg::RBX,
        (std::mem::offset_of!(CpuGsw, registers) + std::mem::offset_of!(Registers, eflags)) as i32,
    );
    e.test_r32_imm32(Reg::R8, crate::FLAG_CF);
    e.jcc(5, miss);
    e.place(allowed);
}

fn emit_read(e: &mut Encoder, read: Read, miss: Label) {
    let address = read.address;
    let address_width = if address.address_size == AddressSize::Word {
        BusWidth::Word
    } else {
        BusWidth::Dword
    };
    e.mov_r32_imm32(Reg::R10, address.disp as u32);
    if let Some(base) = address.base {
        load(e, Reg::RAX, Input::Reg(base), address_width);
        e.alu_r32_r32(0, Reg::R10, Reg::RAX);
    }
    if let Some(index) = address.index {
        load(e, Reg::RAX, Input::Reg(index), address_width);
        if address.address_size == AddressSize::Dword && address.scale != 1 {
            e.shift_r32_imm8(4, Reg::RAX, address.scale.trailing_zeros() as u8);
        }
        e.alu_r32_r32(0, Reg::R10, Reg::RAX);
    }
    if address.address_size == AddressSize::Word {
        e.alu_r32_imm32(4, Reg::R10, 0xffff);
    }
    let segment = (std::mem::offset_of!(CpuGsw, registers)
        + std::mem::offset_of!(Registers, segments)
        + address.segment.index() * std::mem::size_of::<SegmentRegister>())
        as i32;
    e.load_r32_disp32(
        Reg::R8,
        Reg::RBX,
        segment + std::mem::offset_of!(SegmentRegister, limit) as i32,
    );
    e.load_r32_disp32(
        Reg::R9,
        Reg::RBX,
        segment + std::mem::offset_of!(SegmentRegister, base) as i32,
    );
    let bounds = e.label();
    let checked = e.label();
    e.cmp_r32_imm32(Reg::R8, u32::MAX);
    e.jcc(5, bounds);
    e.test_r32_r32(Reg::R9, Reg::R9);
    e.jcc(4, checked);
    e.place(bounds);
    e.mov_r32_r32(Reg::RDX, Reg::R10);
    if read.width != BusWidth::Byte {
        e.alu_r32_imm32(0, Reg::RDX, read.width.bytes() - 1);
        e.jcc(2, miss);
    }
    e.movzx_r32_byte_disp32(
        Reg::RAX,
        Reg::RBX,
        segment + std::mem::offset_of!(SegmentRegister, access) as i32,
    );
    e.alu_r32_imm32(4, Reg::RAX, 0x1c);
    e.cmp_r32_imm32(Reg::RAX, 0x14);
    let down = e.label();
    e.jcc(4, down);
    e.alu_r32_r32(7, Reg::RDX, Reg::R8);
    e.jcc(7, miss);
    e.jmp(checked);
    e.place(down);
    e.alu_r32_r32(7, Reg::R10, Reg::R8);
    e.jcc(6, miss);
    e.movzx_r32_byte_disp32(
        Reg::RAX,
        Reg::RBX,
        segment + std::mem::offset_of!(SegmentRegister, default_size_32) as i32,
    );
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(5, checked);
    e.cmp_r32_imm32(Reg::RDX, 0xffff);
    e.jcc(7, miss);
    e.place(checked);
    e.alu_r32_r32(0, Reg::R10, Reg::R9);
    // Natural alignment of 1/2/4-byte accesses also proves 4 KiB page locality.
    if read.width != BusWidth::Byte {
        e.test_r32_imm32(Reg::R10, read.width.bytes() - 1);
        e.jcc(5, miss);
    }
    e.mov_r32_r32(Reg::R11, Reg::R10);
    e.shift_r32_imm8(5, Reg::R11, 12);
    e.load_r64_disp32(
        Reg::R8,
        Reg::R13,
        std::mem::offset_of!(Frame, region_mapping_epochs) as i32,
    );
    e.load_r64_sib_scale8(Reg::R8, Reg::R8, Reg::R11);
    e.load_r64_disp32(
        Reg::R9,
        Reg::R13,
        std::mem::offset_of!(Frame, region_epoch) as i32,
    );
    e.cmp_r64_r64(Reg::R8, Reg::R9);
    e.jcc(5, miss);
    e.load_r64_disp32(
        Reg::R8,
        Reg::R13,
        std::mem::offset_of!(Frame, region_load_biases) as i32,
    );
    e.load_r64_sib_scale8(Reg::R8, Reg::R8, Reg::R11);
    e.test_r32_imm32(Reg::R8, NATIVE_LOAD_BIAS_MODE13 as u32);
    e.jcc(5, miss);
    let permitted = e.label();
    e.test_r32_imm32(Reg::R8, NATIVE_LOAD_BIAS_SUPERVISOR as u32);
    e.jcc(4, permitted);
    e.load_r32_disp32(
        Reg::RAX,
        Reg::R13,
        std::mem::offset_of!(Frame, region_user) as i32,
    );
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(5, miss);
    e.place(permitted);
    e.and_r64_imm32(Reg::R8, !(NATIVE_LOAD_BIAS_TAG_MASK as u32));
    e.add_r64_r64(Reg::R8, Reg::R10);
    match read.width {
        BusWidth::Byte => e.movzx_r32_byte_disp32(Reg::RDX, Reg::R8, 0),
        BusWidth::Word => e.movzx_r32_word_disp32(Reg::RDX, Reg::R8, 0),
        BusWidth::Dword => e.load_r32_disp32(Reg::RDX, Reg::R8, 0),
    }
    if let Some(alu) = read.alu {
        load(e, Reg::RAX, Input::Reg(read.dst), read.width);
        emit_arithmetic(e, alu, read.width);
        if alu != 7 {
            store(e, read.dst, Reg::RCX, read.width);
        }
    } else {
        store(e, read.dst, Reg::RDX, read.width);
    }
}
