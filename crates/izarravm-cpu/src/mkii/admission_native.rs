// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::mkii::runtime::{Session, Stats};
use crate::{ControlRegisters, CpuPersona, SegmentIndex, SegmentRegister};
use izarravm_bus::{MkiiBusSessionParts, MkiiCounterPath};
use std::mem::offset_of;
use std::sync::OnceLock;

const SESSION: usize = offset_of!(Frame, session);
const BUS: usize = SESSION + offset_of!(Session, bus);

pub(super) fn emit(e: &mut Encoder, op: &Operation, persona: CpuPersona, prepared: Label) {
    emit_with(e, op, persona, prepared, |persona, segments| {
        code_for(persona, segments).map(|code| code.entry_ptr() as usize)
    });
}

pub(super) fn emit_with(
    e: &mut Encoder,
    op: &Operation,
    persona: CpuPersona,
    prepared: Label,
    lookup: impl FnOnce(CpuPersona, u8) -> Option<usize>,
) {
    if cfg!(feature = "int-trace") || cfg!(feature = "timing-class-histogram") {
        return;
    }
    let (num, den) = crate::level_timing(persona);
    let region = op.region.as_ref().unwrap();
    let raw = region.prefixes[op.region_len].raw_core;
    let Some(product) = raw.checked_mul(u64::from(num)) else {
        return;
    };
    if product.checked_add(u64::from(den - 1)).is_none() {
        return;
    }
    let Some(entry) = lookup(persona, region.segments) else {
        return;
    };
    e.mov_r32_imm32(Reg::R10, op.eip);
    e.mov_r64_imm64(Reg::R8, product / u64::from(den));
    e.mov_r64_imm64(Reg::R9, product % u64::from(den));
    e.mov_r64_imm64(Reg::RAX, entry as u64);
    e.call_r64(Reg::RAX);
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(5, prepared);
}

pub(super) fn code_for(persona: CpuPersona, segments: u8) -> Option<&'static Code> {
    static STUBS: [OnceLock<Option<Code>>; 192] = [const { OnceLock::new() }; 192];
    if segments >= 64 {
        return None;
    }
    let persona_index = match persona {
        CpuPersona::I386 => 0,
        CpuPersona::I486 => 1,
        CpuPersona::I586 => 2,
    };
    STUBS[persona_index * 64 + usize::from(segments)]
        .get_or_init(|| {
            let mut e = Encoder::new();
            emit_body(&mut e, persona, segments);
            let bytes = e.finish();
            Some(Code {
                buffer: ExecutableBuffer::new_with_unwind(&bytes, &[1, 0, 0, 0])?,
                body: 0,
                #[cfg(test)]
                unwind_points: vec![0, bytes.len() - 1],
            })
        })
        .as_ref()
}

fn emit_body(e: &mut Encoder, persona: CpuPersona, segments: u8) {
    let (_, den) = crate::level_timing(persona);
    let miss = e.label();
    e.load_r32_disp32(
        Reg::RAX,
        Reg::R13,
        (SESSION + offset_of!(Session, enabled)) as i32,
    );
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(4, miss);
    e.load_r32_disp32(Reg::RAX, Reg::R13, offset_of!(Frame, source_present) as i32);
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(4, miss);
    for (base, offset) in [
        (Reg::R13, offset_of!(Frame, stop)),
        (Reg::RBX, offset_of!(CpuGsw, interrupt_shadow)),
        (Reg::RBX, offset_of!(CpuGsw, rep_resume_active)),
        (Reg::RBX, offset_of!(CpuGsw, written_count)),
        (Reg::RBX, offset_of!(CpuGsw, written_pages_overflow)),
        (Reg::RBX, offset_of!(CpuGsw, rmw_census_enabled)),
        (Reg::RBX, offset_of!(CpuGsw, slot_census_enabled)),
    ] {
        clear_byte(e, base, offset, miss);
    }
    #[cfg(feature = "reflected-call-memo")]
    clear_byte(
        e,
        Reg::RBX,
        offset_of!(CpuGsw, reflected_call_journal),
        miss,
    );
    #[cfg(feature = "reflected-call-diagnostic")]
    clear_byte(
        e,
        Reg::RBX,
        offset_of!(CpuGsw, retire_gates)
            + offset_of!(crate::RetireGates, reflected_call_diag_armed),
        miss,
    );
    e.load_r32_disp32(
        Reg::RAX,
        Reg::RBX,
        offset_of!(CpuGsw, last_written_page) as i32,
    );
    e.cmp_r32_imm32(Reg::RAX, crate::NO_LAST_WRITTEN_PAGE);
    e.jcc(5, miss);
    e.movzx_r32_byte_disp32(
        Reg::RAX,
        Reg::RBX,
        (offset_of!(CpuGsw, fast_map_serve_enabled) + offset_of!(crate::FastMapServeGate, enabled))
            as i32,
    );
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(4, miss);
    e.load_r64_disp32(Reg::RAX, Reg::RBX, offset_of!(CpuGsw, jit_direct) as i32);
    for offset in [
        offset_of!(crate::mkii::State, code_dirty),
        offset_of!(crate::mkii::State, mapping_dirty),
    ] {
        e.movzx_r32_byte_disp32(
            Reg::RDX,
            Reg::RAX,
            (offset_of!(crate::jit::JitState, mkii) + offset) as i32,
        );
        e.test_r32_r32(Reg::RDX, Reg::RDX);
        e.jcc(5, miss);
    }
    e.load_r64_disp32(Reg::RAX, Reg::RBX, offset_of!(CpuGsw, class_table) as i32);
    e.load_r64_disp32(Reg::RDX, Reg::R13, offset_of!(Frame, table) as i32);
    e.cmp_r64_r64(Reg::RAX, Reg::RDX);
    e.jcc(5, miss);
    let registers = offset_of!(CpuGsw, registers);
    e.load_r32_disp32(
        Reg::RAX,
        Reg::RBX,
        (registers + offset_of!(Registers, eip)) as i32,
    );
    e.cmp_r64_r64(Reg::RAX, Reg::R10);
    e.jcc(5, miss);
    e.load_r32_disp32(
        Reg::RAX,
        Reg::RBX,
        (offset_of!(CpuGsw, control) + offset_of!(ControlRegisters, cr0)) as i32,
    );
    e.test_r32_imm32(Reg::RAX, crate::CR0_PE);
    e.jcc(4, miss);
    e.load_r32_disp32(
        Reg::RAX,
        Reg::RBX,
        (registers + offset_of!(Registers, eflags)) as i32,
    );
    e.test_r32_imm32(Reg::RAX, crate::FLAG_VM | crate::FLAG_TF);
    e.jcc(5, miss);
    let cs = registers
        + offset_of!(Registers, segments)
        + SegmentIndex::Cs.index() * std::mem::size_of::<SegmentRegister>();
    clear_byte(
        e,
        Reg::RBX,
        cs + offset_of!(SegmentRegister, default_size_32),
        miss,
    );
    for (offset, width) in [
        (offset_of!(SegmentRegister, selector), BusWidth::Word),
        (offset_of!(SegmentRegister, base), BusWidth::Dword),
        (offset_of!(SegmentRegister, limit), BusWidth::Dword),
        (offset_of!(SegmentRegister, access), BusWidth::Byte),
        (offset_of!(SegmentRegister, default_size_32), BusWidth::Byte),
    ] {
        field(e, Reg::RAX, Reg::RBX, cs + offset, width);
        field(e, Reg::RDX, Reg::R13, offset_of!(Frame, cs) + offset, width);
        e.cmp_r64_r64(Reg::RAX, Reg::RDX);
        e.jcc(5, miss);
    }
    let aligned = e.label();
    e.movzx_r32_byte_disp32(Reg::RAX, Reg::RBX, offset_of!(CpuGsw, cpl) as i32);
    e.cmp_r32_imm32(Reg::RAX, 3);
    e.jcc(5, aligned);
    clear_byte(e, Reg::RBX, offset_of!(CpuGsw, alignment_armed), miss);
    e.place(aligned);
    if segments != 0 {
        e.load_r64_disp32(
            Reg::RAX,
            Reg::R13,
            (SESSION + offset_of!(Session, load_biases)) as i32,
        );
        e.cmp_r64_imm32(Reg::RAX, 0);
        e.jcc(4, miss);
        for index in 0..6 {
            if segments & (1 << index) == 0 {
                continue;
            }
            let access = registers
                + offset_of!(Registers, segments)
                + index * std::mem::size_of::<SegmentRegister>()
                + offset_of!(SegmentRegister, access);
            e.movzx_r32_byte_disp32(Reg::RAX, Reg::RBX, access as i32);
            e.and_r32_imm32(Reg::RAX, 0x0a);
            e.cmp_r32_imm32(Reg::RAX, 0x08);
            e.jcc(4, miss);
        }
    }
    counter(e, offset_of!(MkiiBusSessionParts, mapping_epoch), Reg::R10);
    e.load_r64_disp32(Reg::RAX, Reg::R13, offset_of!(Frame, source_mapping) as i32);
    e.cmp_r64_r64(Reg::RAX, Reg::R10);
    e.jcc(5, miss);
    e.load_r64_disp32(Reg::RAX, Reg::R13, offset_of!(Frame, source_cost) as i32);
    e.load_r64_disp32(
        Reg::RDX,
        Reg::R13,
        (BUS + offset_of!(MkiiBusSessionParts, cost_epoch)) as i32,
    );
    e.cmp_r64_r64(Reg::RAX, Reg::RDX);
    e.jcc(5, miss);

    e.load_r64_disp32(Reg::RAX, Reg::RBX, offset_of!(CpuGsw, timing_rem) as i32);
    e.cmp_r64_imm32(Reg::RAX, den);
    e.jcc(3, miss);
    e.add_r64_r64(Reg::R9, Reg::RAX);
    let no_carry = e.label();
    e.cmp_r64_imm32(Reg::R9, den);
    e.jcc(2, no_carry);
    e.sub_r64_imm32(Reg::R9, den);
    e.add_r64_imm32(Reg::R8, 1);
    e.place(no_carry);
    e.load_r64_disp32(
        Reg::RAX,
        Reg::RBX,
        offset_of!(CpuGsw, elapsed_clocks) as i32,
    );
    e.add_r64_r64(Reg::RAX, Reg::R8);
    e.jcc(2, miss);

    counter(e, offset_of!(MkiiBusSessionParts, trace_clocks), Reg::RAX);
    e.load_r64_disp32(
        Reg::RDX,
        Reg::R13,
        (BUS + offset_of!(MkiiBusSessionParts, trace_origin)) as i32,
    );
    e.sub_r64_r64(Reg::RAX, Reg::RDX);
    e.jcc(2, miss);
    counter(e, offset_of!(MkiiBusSessionParts, isa_clocks), Reg::RCX);
    e.add_r64_r64(Reg::RAX, Reg::RCX);
    e.jcc(2, miss);
    e.load_r64_disp32(
        Reg::RDX,
        Reg::R13,
        (SESSION + offset_of!(Session, raw_limit)) as i32,
    );
    e.cmp_r64_r64(Reg::RAX, Reg::RDX);
    e.jcc(7, miss);
    e.load_r64_disp32(
        Reg::RDX,
        Reg::R13,
        (BUS + offset_of!(MkiiBusSessionParts, bus_numerator)) as i32,
    );
    e.imul_r64_r64(Reg::RAX, Reg::RDX);
    let lower_ok = e.label();
    e.load_r64_disp32(Reg::RCX, Reg::R13, offset_of!(Frame, bus_at_entry) as i32);
    e.cmp_r64_imm32(Reg::RCX, 0);
    e.jcc(4, lower_ok);
    e.sub_r64_imm32(Reg::RCX, 1);
    threshold(e, miss);
    e.cmp_r64_r64(Reg::RAX, Reg::RCX);
    e.jcc(6, miss);
    e.place(lower_ok);
    e.load_r64_disp32(Reg::RDX, Reg::R13, offset_of!(Frame, total) as i32);
    e.add_r64_r64(Reg::RDX, Reg::R8);
    e.jcc(2, miss);
    e.load_r64_disp32(Reg::RCX, Reg::R13, offset_of!(Frame, cap) as i32);
    e.sub_r64_r64(Reg::RCX, Reg::RDX);
    e.jcc(2, miss);
    e.cmp_r64_imm32(Reg::RCX, 0);
    e.jcc(4, miss);
    e.load_r64_disp32(Reg::RDX, Reg::R13, offset_of!(Frame, bus_at_entry) as i32);
    e.add_r64_r64(Reg::RCX, Reg::RDX);
    e.jcc(2, miss);
    e.sub_r64_imm32(Reg::RCX, 1);
    threshold(e, miss);
    e.cmp_r64_r64(Reg::RAX, Reg::RCX);
    e.jcc(7, miss);

    for (offset, value) in [
        (offset_of!(Frame, region_epoch), Reg::R10),
        (offset_of!(Frame, region_full_core), Reg::R8),
        (offset_of!(Frame, region_full_rem), Reg::R9),
    ] {
        e.store_r64_disp32(Reg::R13, offset as i32, value);
    }
    for (offset, value) in [
        (offset_of!(Frame, region_completed), 0),
        (offset_of!(Frame, region_guard_miss), 0),
        (offset_of!(Frame, branch_taken), 0),
        (offset_of!(Frame, region_inert), 1),
    ] {
        e.store_u32_imm_disp32(Reg::R13, offset as i32, value);
    }
    for (source, target) in [
        (
            offset_of!(Session, load_biases),
            offset_of!(Frame, region_load_biases),
        ),
        (
            offset_of!(Session, mapping_epochs),
            offset_of!(Frame, region_mapping_epochs),
        ),
    ] {
        e.load_r64_disp32(Reg::RAX, Reg::R13, (SESSION + source) as i32);
        e.store_r64_disp32(Reg::R13, target as i32, Reg::RAX);
    }
    e.movzx_r32_byte_disp32(Reg::RAX, Reg::RBX, offset_of!(CpuGsw, cpl) as i32);
    e.mov_r32_imm32(Reg::RDX, 0);
    e.cmp_r32_imm32(Reg::RAX, 3);
    e.setcc(4, Reg::RDX);
    e.store_r32_disp32(Reg::R13, offset_of!(Frame, region_user) as i32, Reg::RDX);
    e.cmp_r32_imm32(Reg::RAX, 0);
    e.setcc(4, Reg::RDX);
    e.store_r32_disp32(Reg::R13, offset_of!(Frame, region_monitor) as i32, Reg::RDX);
    e.load_r32_disp32(
        Reg::RAX,
        Reg::RBX,
        (registers + offset_of!(Registers, eflags)) as i32,
    );
    e.test_r32_imm32(Reg::RAX, crate::FLAG_IF);
    e.setcc(5, Reg::RDX);
    e.mov_r64_r64(Reg::R11, Reg::R13);
    e.add_r64_imm32(Reg::R11, offset_of!(Frame, region_can_take) as u32);
    e.store_r8_disp8(Reg::R11, 0, Reg::RDX);
    e.load_r64_disp32(Reg::RAX, Reg::R13, offset_of!(Frame, total) as i32);
    e.store_r64_disp32(
        Reg::RBX,
        offset_of!(CpuGsw, core_clocks_so_far) as i32,
        Reg::RAX,
    );
    for offset in [
        offset_of!(Stats, regions),
        offset_of!(Stats, native_admissions),
    ] {
        let offset = (offset_of!(Frame, stats) + offset) as i32;
        e.load_r64_disp32(Reg::RAX, Reg::R13, offset);
        e.add_r64_imm32(Reg::RAX, 1);
        e.store_r64_disp32(Reg::R13, offset, Reg::RAX);
    }
    e.mov_r32_imm32(Reg::RAX, 1);
    e.ret();
    e.place(miss);
    e.mov_r32_imm32(Reg::RAX, 0);
    e.ret();
}

fn field(e: &mut Encoder, dst: Reg, base: Reg, offset: usize, width: BusWidth) {
    match width {
        BusWidth::Byte => e.movzx_r32_byte_disp32(dst, base, offset as i32),
        BusWidth::Word => e.movzx_r32_word_disp32(dst, base, offset as i32),
        BusWidth::Dword => e.load_r32_disp32(dst, base, offset as i32),
    }
}

fn clear_byte(e: &mut Encoder, base: Reg, offset: usize, miss: Label) {
    e.movzx_r32_byte_disp32(Reg::RAX, base, offset as i32);
    e.test_r32_r32(Reg::RAX, Reg::RAX);
    e.jcc(5, miss);
}

fn counter(e: &mut Encoder, offset: usize, dst: Reg) {
    let direct = e.label();
    e.load_r32_disp32(
        Reg::R11,
        Reg::R13,
        (BUS + offset + offset_of!(MkiiCounterPath, root_offset)) as i32,
    );
    e.add_r64_r64(Reg::R11, Reg::R12);
    e.load_r32_disp32(
        Reg::RDX,
        Reg::R13,
        (BUS + offset + offset_of!(MkiiCounterPath, pointee_offset)) as i32,
    );
    e.cmp_r32_imm32(Reg::RDX, MkiiCounterPath::DIRECT);
    e.jcc(4, direct);
    e.load_r64_disp32(Reg::R11, Reg::R11, 0);
    e.add_r64_r64(Reg::R11, Reg::RDX);
    e.place(direct);
    e.load_r64_disp32(dst, Reg::R11, 0);
}

fn threshold(e: &mut Encoder, miss: Label) {
    e.load_r64_disp32(
        Reg::RDX,
        Reg::R13,
        (SESSION + offset_of!(Session, threshold_limit)) as i32,
    );
    e.cmp_r64_r64(Reg::RCX, Reg::RDX);
    e.jcc(7, miss);
    e.load_r64_disp32(
        Reg::RDX,
        Reg::R13,
        (BUS + offset_of!(MkiiBusSessionParts, bus_denominator)) as i32,
    );
    e.imul_r64_r64(Reg::RCX, Reg::RDX);
}
