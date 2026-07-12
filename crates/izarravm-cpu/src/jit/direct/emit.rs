// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn stack_addr(disp: u32) -> DirectAddr {
    DirectAddr {
        segment: SegmentIndex::Ss,
        base: Some(4),
        index: None,
        scale: 1,
        disp,
    }
}

#[derive(Clone, Copy)]
struct MemorySideExits {
    cross_page_or_alignment: Label,
    unavailable_or_kind: Label,
    permission: Label,
    code_watch: Label,
    segment_limit: Option<Label>,
}

impl MemorySideExits {
    fn new(e: &mut Encoder, memory: MemoryEmitContext, addr: Option<DirectAddr>) -> Self {
        Self {
            cross_page_or_alignment: e.label(),
            unavailable_or_kind: e.label(),
            permission: e.label(),
            code_watch: e.label(),
            segment_limit: addr
                .filter(|addr| memory.segments.descriptor(addr.segment).limit != u32::MAX)
                .map(|_| e.label()),
        }
    }

    fn append_stubs(
        self,
        stubs: &mut Vec<(Label, Label, SideExitReason)>,
        common: Label,
        cross_page: bool,
        permission: bool,
        code_watch: bool,
    ) {
        if cross_page {
            stubs.push((
                self.cross_page_or_alignment,
                common,
                SideExitReason::CrossPageOrAlignment,
            ));
        }
        stubs.push((
            self.unavailable_or_kind,
            common,
            SideExitReason::UnavailableOrKind,
        ));
        if permission {
            stubs.push((self.permission, common, SideExitReason::Permission));
        }
        if code_watch {
            stubs.push((self.code_watch, common, SideExitReason::CodeWatch));
        }
        if let Some(segment_limit) = self.segment_limit {
            stubs.push((segment_limit, common, SideExitReason::Other));
        }
    }
}

pub(super) fn emit(input: EmitInput<'_>) -> EmittedCode {
    let EmitInput {
        slots,
        span,
        raw_clocks,
        weighted_fp_clocks,
        byte_reads,
        word_reads,
        dword_reads,
        byte_stores,
        word_stores,
        dword_stores,
        self_loop,
        x87_entry_top,
        memory,
        link_cell_ptrs,
    } = input;
    let full_accounting = StaticAccounting {
        instructions: span.instructions,
        raw_clocks: raw_clocks as u16,
        byte_reads,
        word_reads,
        dword_reads,
        weighted_fp_clocks,
    };
    let mut e = Encoder::new();
    for reg in SAVED_HOST_REGS {
        e.push(reg);
    }
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        e.push(Reg::RSI);
    }
    let native_stack_len = if x87_entry_top.is_some() {
        AVX2_X87_STACK_LEN
    } else {
        NATIVE_STACK_LEN
    };
    e.sub_r64_imm32(Reg::RSP, native_stack_len);
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        emit_save_x87_host_xmms(&mut e);
    }
    e.mov_r64_r64(Reg::R15, CPU_ARG);
    e.mov_r32_r32(Reg::RBP, FLAGS_ARG);
    e.mov_r64_r64(Reg::RAX, EXIT_ARG);
    e.store_r64_disp8(Reg::RSP, STACK_EXIT, Reg::RAX);
    e.mov_r32_r32(Reg::RAX, QUOTA_ARG);
    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
    e.xor_r64_self(Reg::RAX);
    e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RAX);
    for (_, stack_offset, _) in dynamic_counter_fields() {
        e.store_r64_disp8(Reg::RSP, stack_offset, Reg::RAX);
    }
    for stack_offset in [
        STACK_INSTRUCTIONS,
        STACK_RAW_CLOCKS,
        STACK_BYTE_READS,
        STACK_DWORD_READS,
        STACK_WEIGHTED_FP_CLOCKS,
    ] {
        e.store_r64_disp8(Reg::RSP, stack_offset, Reg::RAX);
    }
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.load_r32_disp32(home, Reg::R15, gpr_offset(index));
    }
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    if x87_entry_top.is_some() {
        emit_x87_enter(&mut e, Reg::R15);
    }
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    debug_assert!(x87_entry_top.is_none());
    let loop_entry = e.label();
    let body_offset = e.position();
    e.place(loop_entry);

    let mut completed = 0u8;
    let mut completed_raw = 0u16;
    let mut completed_weighted_fp_clocks = 0u32;
    let mut completed_byte_reads = 0u8;
    let mut completed_word_reads = 0u8;
    let mut completed_dword_reads = 0u8;
    let mut side_exits = Vec::new();
    let mut side_exit_reason_stubs = Vec::new();
    let shared_return = e.label();
    let self_loop_return = self_loop.then(|| e.label());
    let mut terminal = false;
    let mut x87_gate_emitted = false;
    let mut current_x87_top = x87_entry_top;
    for slot in slots {
        match slot.kind {
            DirectKind::MovReg { dst, src, width } => match width {
                MemoryWidth::Word => e.mov_r16_r16(home(dst), home(src)),
                MemoryWidth::Dword => e.mov_r32_r32(home(dst), home(src)),
                MemoryWidth::Byte => unreachable!("byte register moves use MovRegByte"),
            },
            DirectKind::MovRegByte { dst, src } => {
                emit_read_store_value(&mut e, StoreSource::Reg(src), MemoryWidth::Byte, Reg::RDX);
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::MovImm { dst, imm } => e.mov_r32_imm32(home(dst), imm),
            DirectKind::MovImmByte { dst, imm } => {
                e.mov_r32_imm32(Reg::RDX, u32::from(imm));
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::Lea { dst, addr } => {
                emit_effective_address(&mut e, addr);
                e.mov_r32_r32(home(dst), Reg::RAX);
            }
            DirectKind::IncDecReg { dst, is_dec, width } => {
                emit_inc_dec_reg(&mut e, dst, is_dec, width);
            }
            DirectKind::AluReg {
                op,
                dst,
                src,
                width,
            } => {
                emit_alu(&mut e, op, dst, Some(src), None, width);
            }
            DirectKind::AluImm { op, dst, imm } => {
                emit_alu(&mut e, op, dst, None, Some(imm), MemoryWidth::Dword);
            }
            DirectKind::AluByteImm { op, dst, imm } => {
                emit_alu_byte_imm(&mut e, op, dst, imm);
            }
            DirectKind::AluMemSource {
                op,
                dst,
                width,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_alu_mem_source(&mut e, op, dst, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    false,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::AluMemDest {
                op,
                source,
                width,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_alu_mem_dest(&mut e, op, source, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    op != 7,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Test { a, b } => emit_test(&mut e, a, b),
            DirectKind::TestImmReg { dst, imm, width } => {
                emit_test_imm_reg(&mut e, dst, imm, width);
            }
            DirectKind::TestImmMem { imm, width, addr } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_test_imm_mem(&mut e, imm, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    false,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Shift { op, dst, count } => emit_shift(&mut e, op, dst, count),
            DirectKind::DoubleShiftReg {
                left,
                dst,
                src,
                count,
            } => emit_double_shift_reg(&mut e, left, dst, src, count),
            DirectKind::DoubleShiftMem {
                left,
                src,
                count,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_double_shift_mem(&mut e, left, src, count, addr, memory, reasons);
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, true);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Load {
                dst, width, addr, ..
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_load(&mut e, dst, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    false,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Store {
                source,
                width,
                addr,
                ..
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_store(&mut e, source, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    true,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::RmwIncDec {
                is_dec,
                width,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_rmw_inc_dec(&mut e, is_dec, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    true,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Push { source } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                emit_store(
                    &mut e,
                    source,
                    MemoryWidth::Dword,
                    stack_addr(0u32.wrapping_sub(4)),
                    memory,
                    reasons,
                );
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, true);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                e.alu_r32_imm32(5, home(4), 4);
            }
            DirectKind::Pop { dst } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                emit_ram_read_pointer(&mut e, MemoryWidth::Dword, stack_addr(0), memory, reasons);
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                e.add_r32_imm32(home(4), 4);
                e.mov_r32_r32(home(dst), Reg::RDX);
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, false);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::X87 { insn, addr } => {
                let side = e.label();
                let eligibility = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, addr);
                let top = current_x87_top.expect("x87 block must carry an entry TOP");
                // Every exceptional fast-path result exits before changing x87 state, so a
                // successful x87 instruction cannot make #MF pending for the next slot.
                emit_x87_slot(
                    &mut e,
                    insn,
                    addr,
                    memory,
                    reasons,
                    X87SlotEmitState {
                        eligibility_side: eligibility,
                        check_gate: !x87_gate_emitted,
                        top,
                    },
                );
                current_x87_top = Some(insn.advance_top(top));
                x87_gate_emitted = true;
                if let Some(access) = insn.metadata().memory {
                    reasons.append_stubs(
                        &mut side_exit_reason_stubs,
                        side,
                        true,
                        memory.cpl3,
                        access.direction == NativeX87MemoryDirection::Write,
                    );
                }
                side_exit_reason_stubs.push((eligibility, side, SideExitReason::Other));
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Call {
                return_delta,
                target_delta,
            } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                emit_store(
                    &mut e,
                    StoreSource::EipDelta(return_delta),
                    MemoryWidth::Dword,
                    stack_addr(0u32.wrapping_sub(4)),
                    memory,
                    reasons,
                );
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, true);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                e.alu_r32_imm32(5, home(4), 4);
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_completed_path(
                    &mut e,
                    span,
                    false,
                    target_delta,
                    Some(link_cell_ptrs[0]),
                    shared_return,
                    full_accounting,
                );
                terminal = true;
                break;
            }
            DirectKind::Jmp { target_delta } => {
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_completed_path(
                    &mut e,
                    span,
                    false,
                    target_delta,
                    Some(link_cell_ptrs[0]),
                    shared_return,
                    full_accounting,
                );
                terminal = true;
                break;
            }
            DirectKind::Ret { release } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                let limit = memory.segments.cs.limit;
                let limit_exit = (limit != u32::MAX).then(|| e.label());
                emit_ram_read_pointer_inner(
                    &mut e,
                    MemoryWidth::Dword,
                    stack_addr(0),
                    memory,
                    reasons,
                );
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                if let Some(limit_exit) = limit_exit {
                    e.cmp_r32_imm32(Reg::RDX, limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::Other));
                }
                emit_mode13_read_completion(&mut e, MemoryWidth::Dword);
                e.add_r32_imm32(home(4), 4 + u32::from(release));
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, false);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_completed_dynamic_path(
                    &mut e,
                    span,
                    Reg::RDX,
                    link_cell_ptrs,
                    shared_return,
                    full_accounting,
                );
                terminal = true;
                break;
            }
            DirectKind::Jcc {
                condition,
                taken_delta,
            } => {
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_load_host_flags(&mut e);
                let taken = e.label();
                e.jcc(condition, taken);
                if self_loop {
                    emit_dynamic_increment(&mut e, STACK_ITERATIONS);
                    emit_advance_eip(&mut e, u32::from(span.guest_len));
                    e.jmp(self_loop_return.expect("self loop must have a return stub"));
                } else {
                    emit_completed_path(
                        &mut e,
                        span,
                        false,
                        u32::from(span.guest_len),
                        Some(link_cell_ptrs[0]),
                        shared_return,
                        full_accounting,
                    );
                }
                e.place(taken);
                if self_loop {
                    emit_dynamic_increment(&mut e, STACK_ITERATIONS);
                    debug_assert_eq!(taken_delta, 0);
                    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_QUOTA);
                    e.sub_r64_imm32(Reg::RAX, 1);
                    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
                    e.jnz(loop_entry);
                    emit_advance_eip(&mut e, taken_delta);
                    e.jmp(self_loop_return.expect("self loop must have a return stub"));
                } else {
                    emit_completed_path(
                        &mut e,
                        span,
                        false,
                        taken_delta,
                        Some(link_cell_ptrs[1]),
                        shared_return,
                        full_accounting,
                    );
                }
                terminal = true;
                break;
            }
        }
        completed += 1;
        completed_raw += slot.kind.raw_clocks() as u16;
        completed_weighted_fp_clocks += slot.weighted_fp_clocks;
        completed_byte_reads += slot.kind.byte_reads();
        completed_word_reads += slot.kind.word_reads();
        completed_dword_reads += slot.kind.dword_reads();
    }
    if !terminal {
        emit_completed_path(
            &mut e,
            span,
            false,
            u32::from(span.guest_len),
            Some(link_cell_ptrs[0]),
            shared_return,
            full_accounting,
        );
    }
    if let Some(self_loop_return) = self_loop_return {
        e.place(self_loop_return);
        emit_accounting(
            &mut e,
            span,
            true,
            StaticAccounting::default(),
            true,
            full_accounting,
        );
        e.jmp(shared_return);
    }
    let side_return = (!side_exits.is_empty()).then(|| e.label());
    for (common, eip_delta, exit) in side_exits {
        let stub_count = side_exit_reason_stubs
            .iter()
            .filter(|(_, target, _)| *target == common)
            .count();
        let mut stub_index = 0;
        for &(label, target, reason) in &side_exit_reason_stubs {
            if target != common {
                continue;
            }
            stub_index += 1;
            e.place(label);
            e.mov_r8_imm8(Reg::RDX, reason as u8);
            if stub_index != stub_count {
                e.jmp(common);
            }
        }
        debug_assert_ne!(stub_count, 0);
        e.place(common);
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
        let reason_offset = u32::try_from(core::mem::offset_of!(NativeExit, side_exit_reason))
            .expect("native side-exit reason offset must fit a u32");
        e.add_r64_imm32(Reg::RAX, reason_offset);
        e.store_r8_disp8(Reg::RAX, 0, Reg::RDX);
        emit_add_static_accounting(&mut e, exit);
        e.mov_r64_imm64(Reg::RAX, u64::from(exit.instructions));
        e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RAX);
        emit_advance_eip(&mut e, eip_delta);
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
        e.store_imm32_disp32(
            Reg::RAX,
            core::mem::offset_of!(NativeExit, side_exit) as i32,
            1,
        );
        e.jmp(side_return.expect("side exit must have shared accounting"));
    }
    if let Some(side_return) = side_return {
        e.place(side_return);
        if self_loop {
            emit_add_repeated_accounting(&mut e, full_accounting);
        }
        emit_fetch_trace(
            &mut e,
            span,
            self_loop,
            TracePrefix::Stack(STACK_READ_KIND),
            false,
        );
        e.jmp(shared_return);
    }
    e.place(shared_return);
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    if x87_entry_top.is_some() {
        emit_x87_spill(&mut e, Reg::R15);
    }
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        emit_restore_x87_host_xmms(&mut e);
    }
    emit_store_homes(&mut e);
    emit_return(&mut e, COUNTER_ALL, x87_entry_top.is_some());
    debug_assert_eq!(usize::from(completed), slots.len());
    debug_assert_eq!(u32::from(completed_raw), raw_clocks);
    debug_assert_eq!(completed_weighted_fp_clocks, weighted_fp_clocks);
    debug_assert_eq!(completed_byte_reads, byte_reads);
    debug_assert_eq!(completed_word_reads, word_reads);
    debug_assert_eq!(completed_dword_reads, dword_reads);
    debug_assert_eq!(
        slots.iter().map(|slot| slot.kind.byte_stores()).sum::<u8>(),
        byte_stores
    );
    debug_assert_eq!(
        slots.iter().map(|slot| slot.kind.word_stores()).sum::<u8>(),
        word_stores
    );
    debug_assert_eq!(
        slots
            .iter()
            .map(|slot| slot.kind.dword_stores())
            .sum::<u8>(),
        dword_stores
    );
    EmittedCode {
        code: e.finish(),
        body_offset,
    }
}

fn emit_accounting(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    prefix: StaticAccounting,
    completed: bool,
    full: StaticAccounting,
) {
    if self_loop {
        emit_add_repeated_accounting(e, full);
    } else if completed {
        emit_add_static_accounting(e, full);
    }
    emit_add_static_accounting(e, prefix);
    emit_fetch_trace(
        e,
        span,
        self_loop,
        TracePrefix::Immediate(u32::from(prefix.instructions)),
        completed,
    );
}

fn accounting_fields(accounting: StaticAccounting) -> [(i8, u32); 5] {
    [
        (STACK_INSTRUCTIONS, u32::from(accounting.instructions)),
        (STACK_RAW_CLOCKS, u32::from(accounting.raw_clocks)),
        (STACK_BYTE_READS, u32::from(accounting.byte_reads)),
        (STACK_DWORD_READS, u32::from(accounting.dword_reads)),
        (STACK_WEIGHTED_FP_CLOCKS, accounting.weighted_fp_clocks),
    ]
}

fn emit_add_static_accounting(e: &mut Encoder, accounting: StaticAccounting) {
    for (stack_offset, value) in accounting_fields(accounting) {
        if value != 0 {
            e.mov_r32_imm32(Reg::RDX, value);
            e.add_r64_to_mem_disp8(Reg::RSP, stack_offset, Reg::RDX);
        }
    }
    if accounting.word_reads != 0 {
        e.mov_r64_imm64(Reg::RDX, u64::from(accounting.word_reads) << 32);
        e.add_r64_to_mem_disp8(Reg::RSP, STACK_BYTE_READS, Reg::RDX);
    }
}

fn emit_add_repeated_accounting(e: &mut Encoder, accounting: StaticAccounting) {
    for (stack_offset, value) in accounting_fields(accounting) {
        if value == 0 {
            continue;
        }
        e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_ITERATIONS);
        if value != 1 {
            e.imul_r64_imm32(Reg::RDX, value);
        }
        e.add_r64_to_mem_disp8(Reg::RSP, stack_offset, Reg::RDX);
    }
    if accounting.word_reads != 0 {
        e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_ITERATIONS);
        if accounting.word_reads != 1 {
            e.imul_r64_imm32(Reg::RDX, u32::from(accounting.word_reads));
        }
        e.shift_r64_imm8(4, Reg::RDX, 32);
        e.add_r64_to_mem_disp8(Reg::RSP, STACK_BYTE_READS, Reg::RDX);
    }
}

#[derive(Clone, Copy)]
enum TracePrefix {
    Immediate(u32),
    Stack(i8),
}

fn emit_fetch_trace(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    prefix: TracePrefix,
    completed: bool,
) {
    let trace_len_offset = core::mem::offset_of!(NativeExit, trace_len) as i32;
    let trace_ptr_offset = core::mem::offset_of!(NativeExit, trace_ptr) as i32;
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.load_r64_disp32(Reg::RCX, Reg::RAX, trace_ptr_offset);
    e.cmp_r64_imm32(Reg::RCX, 0);
    let done = e.label();
    e.jz(done);
    e.load_r32_disp32(Reg::RDI, Reg::RAX, trace_len_offset);
    e.mov_r64_r64(Reg::RDX, Reg::RDI);
    e.shift_r64_imm8(4, Reg::RDX, 4);
    e.add_r64_r64(Reg::RCX, Reg::RDX);
    e.store_u32_imm_disp32(
        Reg::RCX,
        core::mem::offset_of!(NativeBlockTrace, linear) as i32,
        span.key.linear,
    );
    e.store_u32_imm_disp32(
        Reg::RCX,
        core::mem::offset_of!(NativeBlockTrace, physical) as i32,
        span.key.physical,
    );
    if self_loop {
        e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_ITERATIONS);
        e.store_r32_disp32(
            Reg::RCX,
            core::mem::offset_of!(NativeBlockTrace, repetitions) as i32,
            Reg::RDX,
        );
    } else {
        e.store_u32_imm_disp32(
            Reg::RCX,
            core::mem::offset_of!(NativeBlockTrace, repetitions) as i32,
            u32::from(completed),
        );
    }
    match prefix {
        TracePrefix::Immediate(prefix) => e.store_u32_imm_disp32(
            Reg::RCX,
            core::mem::offset_of!(NativeBlockTrace, prefix_instructions) as i32,
            prefix,
        ),
        TracePrefix::Stack(offset) => {
            e.load_r64_disp8(Reg::RDX, Reg::RSP, offset);
            e.store_r32_disp32(
                Reg::RCX,
                core::mem::offset_of!(NativeBlockTrace, prefix_instructions) as i32,
                Reg::RDX,
            );
        }
    }
    e.add_r32_imm32(Reg::RDI, 1);
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.store_r32_disp32(Reg::RAX, trace_len_offset, Reg::RDI);
    e.place(done);
}

fn emit_increment_exit_u32(e: &mut Encoder, offset: usize) {
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.load_r32_disp32(Reg::RDI, Reg::RAX, offset as i32);
    e.add_r32_imm32(Reg::RDI, 1);
    e.store_r32_disp32(Reg::RAX, offset as i32, Reg::RDI);
}

fn emit_advance_eip(e: &mut Encoder, delta: u32) {
    if delta == 0 {
        return;
    }
    e.load_r32_disp32(Reg::RAX, Reg::R15, eip_offset());
    e.add_r32_imm32(Reg::RAX, delta);
    e.store_r32_disp32(Reg::R15, eip_offset(), Reg::RAX);
}

fn emit_completed_path(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    eip_delta: u32,
    link_cell: Option<usize>,
    shared_return: Label,
    accounting: StaticAccounting,
) {
    emit_accounting(
        e,
        span,
        self_loop,
        StaticAccounting::default(),
        true,
        accounting,
    );
    emit_advance_eip(e, eip_delta);
    if let Some(link_cell) = link_cell {
        let unresolved = e.label();
        let returning = e.label();
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_QUOTA);
        e.sub_r64_imm32(Reg::RAX, 1);
        e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
        e.jz(returning);
        e.mov_r64_imm64(Reg::RAX, link_cell as u64);
        e.load_r64_disp32(Reg::RAX, Reg::RAX, 0);
        e.cmp_r64_imm32(Reg::RAX, 0);
        e.jz(unresolved);
        e.mov_r64_r64(Reg::RDX, Reg::RAX);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        e.jmp_r64(Reg::RDX);
        e.place(unresolved);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, unresolved_exits));
        e.place(returning);
    }
    e.jmp(shared_return);
}

fn emit_completed_dynamic_path(
    e: &mut Encoder,
    span: BlockSpan,
    target: Reg,
    link_cells: [usize; 2],
    shared_return: Label,
    accounting: StaticAccounting,
) {
    e.store_r32_disp32(Reg::R15, eip_offset(), target);
    emit_accounting(
        e,
        span,
        false,
        StaticAccounting::default(),
        true,
        accounting,
    );
    e.load_r32_disp32(Reg::RDX, Reg::R15, eip_offset());
    let bind = e.label();
    e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_QUOTA);
    e.sub_r64_imm32(Reg::RDI, 1);
    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RDI);
    for link_cell in link_cells {
        let next = e.label();
        e.mov_r64_imm64(Reg::RAX, link_cell as u64);
        e.load_r64_disp8(
            Reg::RCX,
            Reg::RAX,
            core::mem::offset_of!(LinkCell, body) as i8,
        );
        e.cmp_r64_imm32(Reg::RCX, 0);
        e.jz(next);
        e.cmp_r32_disp8(
            Reg::RDX,
            Reg::RAX,
            core::mem::offset_of!(LinkCell, target_eip) as i8,
        );
        e.jnz(next);
        e.cmp_r64_imm32(Reg::RDI, 0);
        e.jz(shared_return);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        e.jmp_r64(Reg::RCX);
        e.place(next);
    }
    e.cmp_r64_imm32(Reg::RDI, 0);
    e.jz(bind);
    emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, unresolved_exits));
    e.place(bind);
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.mov_r64_imm64(Reg::RCX, link_cells[0] as u64);
    e.store_r64_disp32(
        Reg::RAX,
        core::mem::offset_of!(NativeExit, dynamic_link_cell) as i32,
        Reg::RCX,
    );
    e.store_r32_disp32(
        Reg::RAX,
        core::mem::offset_of!(NativeExit, dynamic_target_eip) as i32,
        Reg::RDX,
    );
    e.jmp(shared_return);
}

#[derive(Clone, Copy, Default)]
struct StaticAccounting {
    instructions: u8,
    raw_clocks: u16,
    byte_reads: u8,
    word_reads: u8,
    dword_reads: u8,
    weighted_fp_clocks: u32,
}

fn side_exit(
    instructions: u8,
    raw_clocks: u16,
    byte_reads: u8,
    word_reads: u8,
    dword_reads: u8,
    weighted_fp_clocks: u32,
) -> StaticAccounting {
    StaticAccounting {
        instructions,
        raw_clocks,
        byte_reads,
        word_reads,
        dword_reads,
        weighted_fp_clocks,
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_load(
    e: &mut Encoder,
    dst: u8,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer(e, width, addr, memory, sides);
    match width {
        MemoryWidth::Byte => {
            e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0);
            emit_write_gpr8(e, dst, Reg::RDX);
        }
        MemoryWidth::Word => {
            e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0);
            emit_write_gpr16(e, dst, Reg::RDX);
        }
        MemoryWidth::Dword => {
            e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
            e.mov_r32_r32(home(dst), Reg::RDX);
        }
    }
}

#[derive(Clone, Copy)]
struct X87SlotEmitState {
    eligibility_side: Label,
    check_gate: bool,
    top: u8,
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_slot(
    e: &mut Encoder,
    insn: NativeX87Insn,
    addr: Option<DirectAddr>,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    state: X87SlotEmitState,
) {
    let access = insn.metadata().memory;
    if let Some(access) = access {
        emit_x87_memory_pointer(
            e,
            addr.expect("x87 memory operation has a direct address"),
            memory,
            sides,
            access.direction == NativeX87MemoryDirection::Write,
        );
    }
    emit_native_x87(
        e,
        insn,
        Avx2X87EmitContext {
            cpu: Reg::R15,
            memory: access.map(|_| Reg::RDI),
            side_exit: state.eligibility_side,
            check_gate: state.check_gate,
            top: state.top,
        },
    );
    if let Some(access) = access {
        emit_x87_memory_completion(
            e,
            access.direction,
            memory.map.expect("x87 memory block has fast-map bases"),
        );
    }
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_x87_slot(
    _: &mut Encoder,
    _: NativeX87Insn,
    _: Option<DirectAddr>,
    _: MemoryEmitContext,
    _: MemorySideExits,
    _: X87SlotEmitState,
) {
    unreachable!("direct x87 lowering is x86-64-only")
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_memory_pointer(
    e: &mut Encoder,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    write: bool,
) {
    let map = memory.map.expect("x87 memory block has fast-map bases");
    emit_segmented_linear_address(e, addr, MemoryWidth::Dword, memory, sides);
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);

    if write {
        emit_write_permission_check(e, memory.cpl3, sides.permission);
        emit_write_pointer(e, map, sides.unavailable_or_kind);
        let unwatched = e.label();
        emit_code_watch_branch(
            e,
            MemoryWidth::Dword,
            map,
            memory
                .code_watch_tables
                .expect("x87 store has code-watch tables"),
            sides.code_watch,
            unwatched,
        );
        e.place(unwatched);
    } else {
        emit_read_permission_check(e, memory.cpl3, sides.permission);
        emit_read_pointer(e, map, sides.unavailable_or_kind);
    }

    // Preserve the guest address and page kind across the x87 emitter while RDI remains the host
    // memory pointer. This stack slot is no longer needed by the completed code-watch probe.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_KIND_MASK));
    e.shift_r64_imm8(4, Reg::RDX, 32);
    e.or_r64_r64(Reg::RDX, Reg::RAX);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_memory_completion(
    e: &mut Encoder,
    direction: NativeX87MemoryDirection,
    map: NativeMapBases,
) {
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_READ_KIND);
    e.mov_r64_r64(Reg::RCX, Reg::RAX);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    let mode13 = e.label();
    let done = e.label();
    e.jz(mode13);
    if direction == NativeX87MemoryDirection::Write {
        emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
    }
    e.jmp(done);
    e.place(mode13);
    match direction {
        NativeX87MemoryDirection::Read => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
        }
        NativeX87MemoryDirection::Write => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
            emit_mode13_dirty_bit(e, map);
        }
    }
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_ram_read_pointer(
    e: &mut Encoder,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer_inner(e, width, addr, memory, sides);
    emit_mode13_read_completion(e, width);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_ram_read_pointer_inner(
    e: &mut Encoder,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native read has fast-map bases");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);

    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));

    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDI);
    emit_read_permission_check(e, memory.cpl3, sides.permission);
    emit_read_pointer(e, map, sides.unavailable_or_kind);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_mode13_read_completion(e: &mut Encoder, width: MemoryWidth) {
    let done = e.label();
    e.load_r64_disp8(Reg::RCX, Reg::RSP, STACK_READ_KIND);
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jnz(done);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_READS),
    }
    e.place(done);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_ram_read_pointer(
    _: &mut Encoder,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_ram_read_pointer_inner(
    _: &mut Encoder,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_mode13_read_completion(_: &mut Encoder, _: MemoryWidth) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_load(
    _: &mut Encoder,
    _: u8,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_alu_mem_source(
    e: &mut Encoder,
    op: u8,
    dst: u8,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer(e, width, addr, memory, sides);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RCX, Reg::RDI, 0),
    }
    e.mov_r32_r32(Reg::RAX, home(dst));
    emit_alu_preloaded(e, op, dst, width);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_test_imm_mem(
    e: &mut Encoder,
    imm: u32,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer(e, width, addr, memory, sides);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RAX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RAX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RAX, Reg::RDI, 0),
    }
    e.mov_r32_imm32(Reg::RCX, imm);
    emit_test_preloaded(e, width);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_alu_mem_dest(
    e: &mut Encoder,
    op: u8,
    source: StoreSource,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("memory ALU has fast-map bases");
    if op == 7 {
        emit_ram_read_pointer(e, width, addr, memory, sides);
        match width {
            MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RAX, Reg::RDI, 0),
            MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RAX, Reg::RDI, 0),
            MemoryWidth::Dword => e.load_r32_disp8(Reg::RAX, Reg::RDI, 0),
        }
        emit_read_store_value(e, source, width, Reg::RCX);
        match width {
            MemoryWidth::Byte => emit_alu_byte_preloaded(e, op),
            MemoryWidth::Word | MemoryWidth::Dword => emit_alu_preloaded(e, op, 0, width),
        }
        return;
    }

    let code_watch_tables = memory
        .code_watch_tables
        .expect("writing memory ALU has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // Save the effective address and page kind before ADC/SBB load host flags into RAX. Nothing
    // below this point mutates architectural state until every pointer and code-watch check passes.
    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);
    emit_read_pointer(e, map, sides.unavailable_or_kind);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RDX, Reg::RDI, 0),
    }
    emit_read_store_value(e, source, width, Reg::RCX);
    emit_alu_candidate(e, op, width);

    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_alu_result_guard(e, width, map, code_watch_tables, sides.code_watch);

    emit_commit_alu_candidate(e, op, source, width);
    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
    }

    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);
    e.place(mode13);
    match width {
        MemoryWidth::Byte => {
            emit_dynamic_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Word => {
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Dword => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
        }
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_double_shift_mem(
    e: &mut Encoder,
    left: bool,
    src: u8,
    count: ShiftCount,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("memory double shift has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("memory double shift has code-watch tables");
    emit_segmented_linear_address(e, addr, MemoryWidth::Dword, memory, sides);
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // Save the effective address and page kind before computing the candidate. Architectural
    // flags, registers, and memory remain untouched until every pointer and code-watch check passes.
    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);
    emit_read_pointer(e, map, sides.unavailable_or_kind);
    e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT, Reg::RDX);
    emit_double_shift_candidate(e, left, src, count, Reg::RDX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT + 4, Reg::RDX);

    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_alu_result_guard(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        sides.code_watch,
    );

    emit_commit_double_shift_flags(e, count);
    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
    e.store_r32_disp8(Reg::RDI, 0, Reg::RDX);

    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
    e.jmp(done);
    e.place(mode13);
    emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
    emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_alu_mem_source(
    _: &mut Encoder,
    _: u8,
    _: u8,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_test_imm_mem(
    _: &mut Encoder,
    _: u32,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_alu_mem_dest(
    _: &mut Encoder,
    _: u8,
    _: StoreSource,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_double_shift_mem(
    _: &mut Encoder,
    _: bool,
    _: u8,
    _: ShiftCount,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

fn emit_effective_address(e: &mut Encoder, addr: DirectAddr) {
    e.mov_r32_imm32(Reg::RAX, addr.disp);
    if let Some(base) = addr.base {
        e.add_r32_r32(Reg::RAX, home(base));
    }
    if let Some(index) = addr.index {
        if addr.scale == 1 {
            e.add_r32_r32(Reg::RAX, home(index));
        } else {
            e.mov_r32_r32(Reg::RCX, home(index));
            e.shl_r32_imm8(Reg::RCX, addr.scale.trailing_zeros() as u8);
            e.add_r32_r32(Reg::RAX, Reg::RCX);
        }
    }
}

fn emit_segmented_linear_address(
    e: &mut Encoder,
    addr: DirectAddr,
    width: MemoryWidth,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_effective_address(e, addr);
    let descriptor = memory.segments.descriptor(addr.segment);
    if descriptor.limit != u32::MAX {
        let Some(max_start) = descriptor.limit.checked_sub(width.bytes() - 1) else {
            e.jmp(
                sides
                    .segment_limit
                    .expect("finite native segment has a limit side exit"),
            );
            return;
        };
        e.cmp_r32_imm32(Reg::RAX, max_start);
        e.jcc(
            7,
            sides
                .segment_limit
                .expect("finite native segment has a limit side exit"),
        );
    }
    if descriptor.base != 0 {
        e.add_r32_imm32(Reg::RAX, descriptor.base);
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_store(
    e: &mut Encoder,
    source: StoreSource,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native store has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native store has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));

    let ram = e.label();
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(ram);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    e.jmp(sides.unavailable_or_kind);

    e.place(ram);
    emit_write_permission_check(e, memory.cpl3, sides.permission);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_store_guard(e, width, map, code_watch_tables, sides.code_watch);
    emit_store_value(e, source, width);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);

    e.place(mode13);
    emit_write_permission_check(e, memory.cpl3, sides.permission);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_store_guard(e, width, map, code_watch_tables, sides.code_watch);
    emit_store_value(e, source, width);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES),
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_rmw_inc_dec(
    e: &mut Encoder,
    is_dec: bool,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    if matches!(width, MemoryWidth::Dword) {
        emit_rmw_inc_dec_dword(e, is_dec, addr, memory, sides);
        return;
    }
    debug_assert!(matches!(width, MemoryWidth::Word));
    let map = memory.map.expect("native RMW has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native RMW has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    emit_wide_page_guard(e, width, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // INC/DEC always changes its operand, so a watched chunk exits before any mutation.
    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        width,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);

    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RCX, Reg::RDI, 0),
    }
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => {
            e.mov_r32_imm32(Reg::RDI, 1);
            e.alu_r16_r16(if is_dec { 5 } else { 0 }, Reg::RAX, Reg::RDI);
        }
        MemoryWidth::Dword => e.alu_r32_imm32(if is_dec { 5 } else { 0 }, Reg::RAX, 1),
    }
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, width, Reg::RCX, Reg::RAX);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDX, 0, Reg::RAX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDX, 0, Reg::RAX),
    }

    e.load_r64_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r64_r64(Reg::RCX, Reg::RAX);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);
    e.place(mode13);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => {
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Dword => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
        }
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_rmw_inc_dec_dword(
    e: &mut Encoder,
    is_dec: bool,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native RMW has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native RMW has code-watch tables");
    emit_segmented_linear_address(e, addr, MemoryWidth::Dword, memory, sides);
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(sides.unavailable_or_kind);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);

    e.load_r32_disp8(Reg::RCX, Reg::RDI, 0);
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    e.alu_r32_imm32(if is_dec { 5 } else { 0 }, Reg::RAX, 1);
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, MemoryWidth::Dword, Reg::RCX, Reg::RAX);
    e.store_r32_disp8(Reg::RDX, 0, Reg::RAX);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
}

fn emit_wide_page_guard(e: &mut Encoder, width: MemoryWidth, side: Label) {
    debug_assert!(width.needs_alignment_guard());
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, width.bytes() - 1);
    e.cmp_r32_imm32(Reg::RDX, 0);
    e.jnz(side);
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, 0x0fff);
    e.cmp_r32_imm32(Reg::RDX, 0x1000 - width.bytes());
    e.jcc(7, side);
}

fn emit_pending_inc_dec(e: &mut Encoder, is_dec: bool, width: MemoryWidth, old: Reg, result: Reg) {
    let base = pending_offset();
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.shl_r32_imm8(Reg::RDI, 17);
    let width_tag = match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 0x100,
        MemoryWidth::Dword => 0x200,
    };
    e.or_r32_imm32(
        Reg::RDI,
        0x8001_0000 | width_tag | if is_dec { 1 } else { 0 },
    );
    e.store_r32_disp32(Reg::R15, base, Reg::RDI);
    e.store_r32_disp32(Reg::R15, base + 4, old);
    e.store_u32_imm_disp32(Reg::R15, base + 8, 1);
    e.store_r32_disp32(Reg::R15, base + 12, result);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_write_permission_check(e: &mut Encoder, memory_cpl3: bool, side: Label) {
    // A ring-0 write to a supervisor read-only PTE is valid while CR0.WP is clear. A populated
    // write bias already proves the page walk admitted the current context. Ring 3 additionally
    // requires both architectural permission bits.
    if memory_cpl3 {
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER | NATIVE_PAGE_WRITABLE));
        e.cmp_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER | NATIVE_PAGE_WRITABLE));
        e.jnz(side);
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_read_permission_check(e: &mut Encoder, memory_cpl3: bool, side: Label) {
    if memory_cpl3 {
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER));
        e.cmp_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER));
        e.jnz(side);
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_read_pointer(e: &mut Encoder, map: NativeMapBases, side: Label) {
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(side);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_write_pointer(e: &mut Encoder, map: NativeMapBases, side: Label) {
    e.mov_r64_imm64(Reg::RDI, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(side);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_watched_store_guard(
    e: &mut Encoder,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    side: Label,
) {
    let unwatched = e.label();
    emit_code_watch_branch(e, width, map, code_watch_tables, side, unwatched);
    e.place(unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_watched_alu_result_guard(
    e: &mut Encoder,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    side: Label,
) {
    let unwatched = e.label();
    emit_code_watch_branch(e, width, map, code_watch_tables, side, unwatched);
    e.place(unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_code_watch_branch(
    e: &mut Encoder,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    watched: Label,
    unwatched: Label,
) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.physical_pages() as u64);
    e.load_r32_sib_scale4(Reg::RCX, Reg::RDX, Reg::RCX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.store_r64_disp8(Reg::RSP, STACK_WATCH_PAGE, Reg::RCX);
    let second = e.label();
    emit_code_watch_table_branch(e, width, code_watch_tables[0], watched, second);
    e.place(second);
    emit_code_watch_table_branch(e, width, code_watch_tables[1], watched, unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_code_watch_table_branch(
    e: &mut Encoder,
    width: MemoryWidth,
    code_watch_table: usize,
    watched: Label,
    unwatched: Label,
) {
    e.load_r64_disp8(Reg::RCX, Reg::RSP, STACK_WATCH_PAGE);
    e.mov_r64_imm64(Reg::RDX, code_watch_table as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, 0);
    e.jz(unwatched);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0fff);
    e.shift_r32_imm8(5, Reg::RCX, 4);
    e.bt_r64_mem(Reg::RDX, Reg::RCX);
    e.jcc(2, watched);
    if width.needs_alignment_guard() {
        e.mov_r32_r32(Reg::RCX, Reg::RAX);
        e.and_r32_imm32(Reg::RCX, 0x0fff);
        e.add_r32_imm32(Reg::RCX, width.bytes() - 1);
        e.shift_r32_imm8(5, Reg::RCX, 4);
        e.bt_r64_mem(Reg::RDX, Reg::RCX);
        e.jcc(2, watched);
    }
    e.jmp(unwatched);
}

fn emit_read_store_value(e: &mut Encoder, source: StoreSource, width: MemoryWidth, value: Reg) {
    match source {
        StoreSource::Reg(src) => match width {
            MemoryWidth::Byte => {
                let lane = if src < 4 { src } else { src - 4 };
                e.mov_r32_r32(value, home(lane));
                if src >= 4 {
                    e.shift_r32_imm8(5, value, 8);
                }
                e.and_r32_imm32(value, 0xff);
            }
            MemoryWidth::Word => {
                e.mov_r32_r32(value, home(src));
                e.and_r32_imm32(value, 0xffff);
            }
            MemoryWidth::Dword => e.mov_r32_r32(value, home(src)),
        },
        StoreSource::Imm(imm) => e.mov_r32_imm32(
            value,
            match width {
                MemoryWidth::Byte => imm & 0xff,
                MemoryWidth::Word => imm & 0xffff,
                MemoryWidth::Dword => imm,
            },
        ),
        StoreSource::EipDelta(delta) => {
            debug_assert!(matches!(width, MemoryWidth::Dword));
            e.load_r32_disp32(value, Reg::R15, eip_offset());
            e.add_r32_imm32(value, delta);
        }
    }
}

fn emit_store_value(e: &mut Encoder, source: StoreSource, width: MemoryWidth) {
    match width {
        MemoryWidth::Byte => {
            emit_read_store_value(e, source, width, Reg::RDX);
            e.store_r8_disp8(Reg::RDI, 0, Reg::RDX);
        }
        MemoryWidth::Word => {
            emit_read_store_value(e, source, width, Reg::RDX);
            e.store_r16_disp8(Reg::RDI, 0, Reg::RDX);
        }
        MemoryWidth::Dword => {
            emit_read_store_value(e, source, width, Reg::RDX);
            e.store_r32_disp8(Reg::RDI, 0, Reg::RDX);
        }
    }
}

fn emit_dynamic_increment(e: &mut Encoder, offset: i8) {
    e.mov_r64_imm64(Reg::RDX, 1);
    e.add_r64_to_mem_disp8(Reg::RSP, offset, Reg::RDX);
}

fn emit_dynamic_word_increment(e: &mut Encoder, byte_counter_offset: i8) {
    e.mov_r64_imm64(Reg::RDX, 1u64 << 32);
    e.add_r64_to_mem_disp8(Reg::RSP, byte_counter_offset, Reg::RDX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_mode13_dirty_bit(e: &mut Encoder, map: NativeMapBases) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.physical_pages() as u64);
    e.load_r32_sib_scale4(Reg::RDX, Reg::RDI, Reg::RCX);
    e.add_r32_imm32(Reg::RDX, 0u32.wrapping_sub(0x000a_0000));
    e.shift_r32_imm8(5, Reg::RDX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_r64(Reg::RDI, Reg::RSP);
    e.add_r64_imm32(Reg::RDI, u32::from(STACK_MODE13_DIRTY_PAGES as u8));
    e.bts_r64_mem(Reg::RDI, Reg::RDX);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_store(
    _: &mut Encoder,
    _: StoreSource,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_rmw_inc_dec(
    _: &mut Encoder,
    _: bool,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

fn emit_write_gpr8(e: &mut Encoder, index: u8, value: Reg) {
    let (home, shift, mask) = if index < 4 {
        (home(index), 0, !0xff)
    } else {
        (home(index - 4), 8, !0xff00)
    };
    if shift != 0 {
        e.shl_r32_imm8(value, shift);
    }
    e.and_r32_imm32(home, mask);
    e.or_r32_r32(home, value);
}

fn emit_write_gpr16(e: &mut Encoder, index: u8, value: Reg) {
    e.mov_r16_r16(home(index), value);
}

fn home(index: u8) -> Reg {
    GUEST_HOMES[usize::from(index & 7)]
}

fn gpr_offset(index: usize) -> i32 {
    (core::mem::offset_of!(CpuGsw, registers)
        + core::mem::offset_of!(Registers, gpr)
        + index * core::mem::size_of::<u32>()) as i32
}

fn eip_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eip)) as i32
}

fn eflags_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eflags)) as i32
}

fn pending_offset() -> i32 {
    core::mem::offset_of!(CpuGsw, pending_flags) as i32
}

fn emit_alu_candidate(e: &mut Encoder, op: u8, width: MemoryWidth) {
    debug_assert_ne!(op, 7);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT, Reg::RDX);
    if matches!(op, 2 | 3) {
        emit_load_host_flags(e);
    }
    match width {
        MemoryWidth::Byte => e.alu_r8_r8(op, Reg::RDX, Reg::RCX),
        MemoryWidth::Word => e.alu_r16_r16(op, Reg::RDX, Reg::RCX),
        MemoryWidth::Dword => e.alu_r32_r32(op, Reg::RDX, Reg::RCX),
    }
    e.pushfq();
    e.pop(Reg::RAX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_FLAGS, Reg::RAX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT + 4, Reg::RDX);
}

fn emit_commit_alu_candidate(e: &mut Encoder, op: u8, source: StoreSource, width: MemoryWidth) {
    let load_values = |e: &mut Encoder| {
        e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_OLD_RESULT);
        emit_read_store_value(e, source, width, Reg::RCX);
        e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
    };
    let capture = |e: &mut Encoder, defined: u32| {
        e.load_r32_disp32(Reg::RDI, Reg::RSP, STACK_ALU_FLAGS);
        e.and_r32_imm32(Reg::RBP, !defined);
        e.and_r32_imm32(Reg::RDI, defined);
        e.or_r32_r32(Reg::RBP, Reg::RDI);
    };
    let width_tag = match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 0x100,
        MemoryWidth::Dword => 0x200,
    };

    if matches!(op, 2 | 3) {
        e.mov_r32_r32(Reg::RDI, Reg::RBP);
        e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
        let carry = e.label();
        let done = e.label();
        e.jnz(carry);
        capture(e, ARITH_FLAGS);
        load_values(e);
        emit_pending(
            e,
            0x8000_0000 | width_tag | u32::from(op == 3),
            Some(Reg::RAX),
            Some(Reg::RCX),
            Reg::RDX,
        );
        e.jmp(done);
        e.place(carry);
        capture(e, ARITH_FLAGS);
        e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
        emit_clear_pending(e);
        e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
        e.place(done);
        return;
    }

    if matches!(op, 1 | 4 | 6) {
        capture(e, LOGIC_FLAGS);
        load_values(e);
        emit_pending(e, 0x8000_0002 | width_tag, None, None, Reg::RDX);
        emit_logic_live_af(e);
        e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
        return;
    }

    debug_assert!(matches!(op, 0 | 5));
    capture(e, ARITH_FLAGS);
    load_values(e);
    emit_pending(
        e,
        0x8000_0000 | width_tag | u32::from(op == 5),
        Some(Reg::RAX),
        Some(Reg::RCX),
        Reg::RDX,
    );
}

fn emit_alu(
    e: &mut Encoder,
    op: u8,
    dst: u8,
    src: Option<u8>,
    imm: Option<u32>,
    width: MemoryWidth,
) {
    e.mov_r32_r32(Reg::RAX, home(dst));
    if let Some(src) = src {
        e.mov_r32_r32(Reg::RCX, home(src));
    } else {
        e.mov_r32_imm32(Reg::RCX, imm.expect("register or immediate source"));
    }
    emit_alu_preloaded(e, op, dst, width);
}

/// Emit an ALU operation with the old destination in EAX and the source in ECX.
fn emit_alu_preloaded(e: &mut Encoder, op: u8, dst: u8, width: MemoryWidth) {
    if matches!(width, MemoryWidth::Word) {
        debug_assert_eq!(op, 7, "the current word ALU family only admits CMP");
        e.and_r32_imm32(Reg::RAX, 0xffff);
        e.and_r32_imm32(Reg::RCX, 0xffff);
        e.mov_r32_r32(Reg::RDX, Reg::RAX);
        e.alu_r16_r16(5, Reg::RDX, Reg::RCX);
        emit_capture_flags(e, ARITH_FLAGS);
        emit_pending(e, 0x8000_0101, Some(Reg::RAX), Some(Reg::RCX), Reg::RDX);
        return;
    }
    debug_assert!(matches!(width, MemoryWidth::Dword));
    if matches!(op, 2 | 3) {
        emit_carry_alu_preloaded(e, op, home(dst));
        return;
    }
    let writes = op != 7;
    let target = if writes {
        home(dst)
    } else {
        e.mov_r32_r32(Reg::RDX, Reg::RAX);
        Reg::RDX
    };
    let host_op = if op == 7 { 5 } else { op };
    e.alu_r32_r32(host_op, target, Reg::RCX);

    if matches!(op, 1 | 4 | 6) {
        emit_capture_flags(e, LOGIC_FLAGS);
        emit_pending(e, 0x8000_0202, None, None, target);
        emit_logic_live_af(e);
    } else {
        emit_capture_flags(e, ARITH_FLAGS);
        let tag = if op == 0 { 0x8000_0200 } else { 0x8000_0201 };
        emit_pending(e, tag, Some(Reg::RAX), Some(Reg::RCX), target);
    }
}

fn emit_carry_alu_preloaded(e: &mut Encoder, op: u8, target: Reg) {
    debug_assert!(matches!(op, 2 | 3));
    let carry = e.label();
    let done = e.label();
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.jnz(carry);

    e.alu_r32_r32(op, target, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    emit_pending(
        e,
        if op == 2 { 0x8000_0200 } else { 0x8000_0201 },
        Some(Reg::RAX),
        Some(Reg::RCX),
        target,
    );
    e.jmp(done);

    e.place(carry);
    emit_load_host_flags(e);
    e.alu_r32_r32(op, target, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
    e.place(done);
}

fn emit_inc_dec_reg(e: &mut Encoder, dst: u8, is_dec: bool, width: MemoryWidth) {
    e.mov_r32_r32(Reg::RAX, home(dst));
    match width {
        MemoryWidth::Byte => unreachable!("register INC/DEC is word or dword"),
        MemoryWidth::Word => {
            e.mov_r32_imm32(Reg::RDX, 1);
            e.alu_r16_r16(if is_dec { 5 } else { 0 }, home(dst), Reg::RDX);
        }
        MemoryWidth::Dword => e.alu_r32_imm32(if is_dec { 5 } else { 0 }, home(dst), 1),
    }
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    if matches!(width, MemoryWidth::Word) {
        e.and_r32_imm32(Reg::RAX, 0xffff);
        e.mov_r32_r32(Reg::RDX, home(dst));
        e.and_r32_imm32(Reg::RDX, 0xffff);
        emit_pending_inc_dec(e, is_dec, width, Reg::RAX, Reg::RDX);
    } else {
        emit_pending_inc_dec(e, is_dec, width, Reg::RAX, home(dst));
    }
}

fn emit_alu_byte_imm(e: &mut Encoder, op: u8, dst: u8, imm: u8) {
    emit_read_store_value(e, StoreSource::Reg(dst), MemoryWidth::Byte, Reg::RAX);
    e.mov_r32_imm32(Reg::RCX, u32::from(imm));
    emit_alu_byte_preloaded(e, op);

    if op != 7 {
        emit_write_gpr8(e, dst, Reg::RDX);
    }
}

fn emit_alu_byte_preloaded(e: &mut Encoder, op: u8) {
    e.mov_r32_r32(Reg::RDX, Reg::RAX);

    if matches!(op, 2 | 3) {
        emit_carry_alu_byte(e, op);
    } else {
        let host_op = if op == 7 { 5 } else { op };
        e.alu_r8_r8(host_op, Reg::RDX, Reg::RCX);
        if matches!(op, 1 | 4 | 6) {
            emit_pending(e, 0x8000_0002, None, None, Reg::RDX);
            emit_capture_flags(e, LOGIC_FLAGS);
            emit_logic_live_af(e);
            e.load_r32_disp32(Reg::RDX, Reg::R15, pending_offset() + 12);
        } else {
            emit_pending(
                e,
                if op == 0 { 0x8000_0000 } else { 0x8000_0001 },
                Some(Reg::RAX),
                Some(Reg::RCX),
                Reg::RDX,
            );
            emit_capture_flags(e, ARITH_FLAGS);
        }
    }
}

fn emit_carry_alu_byte(e: &mut Encoder, op: u8) {
    debug_assert!(matches!(op, 2 | 3));
    let carry = e.label();
    let done = e.label();
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.jnz(carry);

    e.alu_r8_r8(op, Reg::RDX, Reg::RCX);
    emit_pending(
        e,
        if op == 2 { 0x8000_0000 } else { 0x8000_0001 },
        Some(Reg::RAX),
        Some(Reg::RCX),
        Reg::RDX,
    );
    emit_capture_flags(e, ARITH_FLAGS);
    e.jmp(done);

    e.place(carry);
    emit_load_host_flags(e);
    e.alu_r8_r8(op, Reg::RDX, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
    e.place(done);
}

fn emit_test(e: &mut Encoder, a: u8, b: u8) {
    e.mov_r32_r32(Reg::RDX, home(a));
    e.alu_r32_r32(4, Reg::RDX, home(b));
    emit_capture_flags(e, LOGIC_FLAGS);
    emit_pending(e, 0x8000_0202, None, None, Reg::RDX);
    emit_logic_live_af(e);
}

fn emit_test_imm_reg(e: &mut Encoder, dst: u8, imm: u32, width: MemoryWidth) {
    emit_read_store_value(e, StoreSource::Reg(dst), width, Reg::RAX);
    e.mov_r32_imm32(Reg::RCX, imm);
    emit_test_preloaded(e, width);
}

fn emit_test_preloaded(e: &mut Encoder, width: MemoryWidth) {
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    match width {
        MemoryWidth::Byte => e.alu_r8_r8(4, Reg::RDX, Reg::RCX),
        MemoryWidth::Word => e.alu_r16_r16(4, Reg::RDX, Reg::RCX),
        MemoryWidth::Dword => e.alu_r32_r32(4, Reg::RDX, Reg::RCX),
    }
    emit_capture_flags(e, LOGIC_FLAGS);
    emit_pending(
        e,
        match width {
            MemoryWidth::Byte => 0x8000_0002,
            MemoryWidth::Word => 0x8000_0102,
            MemoryWidth::Dword => 0x8000_0202,
        },
        None,
        None,
        Reg::RDX,
    );
    emit_logic_live_af(e);
}

fn emit_shift(e: &mut Encoder, op: u8, dst: u8, raw_count: u8) {
    let count = raw_count & 0x1f;
    if count == 0 {
        return;
    }
    e.shift_r32_imm8(op, home(dst), count);
    let mut defined = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;
    if count == 1 {
        defined |= crate::FLAG_OF;
    }
    emit_capture_flags(e, defined);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

fn emit_double_shift_reg(e: &mut Encoder, left: bool, dst: u8, src: u8, count: ShiftCount) {
    emit_double_shift_candidate(e, left, src, count, home(dst));
    emit_commit_double_shift_flags(e, count);
}

fn emit_double_shift_candidate(
    e: &mut Encoder,
    left: bool,
    src: u8,
    count: ShiftCount,
    target: Reg,
) {
    let immediate = match count {
        ShiftCount::Immediate(count) => Some(count),
        ShiftCount::Cl => {
            e.mov_r32_r32(Reg::RCX, home(1));
            e.store_r32_disp32(Reg::RSP, STACK_SHIFT_COUNT, Reg::RCX);
            None
        }
    };
    e.double_shift_r32(left, target, home(src), immediate);
    e.pushfq();
    e.pop(Reg::RAX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_FLAGS, Reg::RAX);
}

fn emit_commit_double_shift_flags(e: &mut Encoder, count: ShiftCount) {
    const DEFINED: u32 = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;
    match count {
        ShiftCount::Immediate(count) => match count & 0x1f {
            0 => {}
            1 => emit_merge_double_shift_flags(e, DEFINED | crate::FLAG_OF),
            _ => emit_merge_double_shift_flags(e, DEFINED),
        },
        ShiftCount::Cl => {
            let one = e.label();
            let done = e.label();
            e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_SHIFT_COUNT);
            e.and_r32_imm32(Reg::RAX, 0x1f);
            e.cmp_r32_imm32(Reg::RAX, 0);
            e.jz(done);
            e.cmp_r32_imm32(Reg::RAX, 1);
            e.jz(one);
            emit_merge_double_shift_flags(e, DEFINED);
            e.jmp(done);
            e.place(one);
            emit_merge_double_shift_flags(e, DEFINED | crate::FLAG_OF);
            e.place(done);
        }
    }
}

fn emit_merge_double_shift_flags(e: &mut Encoder, defined: u32) {
    e.load_r32_disp32(Reg::RDI, Reg::RSP, STACK_ALU_FLAGS);
    e.and_r32_imm32(Reg::RBP, !defined);
    e.and_r32_imm32(Reg::RDI, defined);
    e.or_r32_r32(Reg::RBP, Reg::RDI);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

fn emit_capture_flags(e: &mut Encoder, defined: u32) {
    e.pushfq();
    e.pop(Reg::RDI);
    e.and_r32_imm32(Reg::RBP, !defined);
    e.and_r32_imm32(Reg::RDI, defined);
    e.or_r32_r32(Reg::RBP, Reg::RDI);
}

fn emit_load_host_flags(e: &mut Encoder) {
    e.mov_r32_r32(Reg::RAX, Reg::RBP);
    e.and_r32_imm32(Reg::RAX, ARITH_FLAGS | 0x2);
    e.push(Reg::RAX);
    e.popfq();
}

fn emit_logic_live_af(e: &mut Encoder) {
    e.load_r32_disp32(Reg::RDI, Reg::R15, eflags_offset());
    e.and_r32_imm32(Reg::RDI, !crate::FLAG_AF);
    e.mov_r32_r32(Reg::RDX, Reg::RBP);
    e.and_r32_imm32(Reg::RDX, crate::FLAG_AF);
    e.or_r32_r32(Reg::RDI, Reg::RDX);
    e.or_r32_imm32(Reg::RDI, 0x2);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RDI);
}

fn emit_pending(e: &mut Encoder, tag: u32, a: Option<Reg>, b: Option<Reg>, result: Reg) {
    let base = pending_offset();
    e.store_u32_imm_disp32(Reg::R15, base, tag);
    if let Some(a) = a {
        e.store_r32_disp32(Reg::R15, base + 4, a);
    } else {
        e.store_u32_imm_disp32(Reg::R15, base + 4, 0);
    }
    if let Some(b) = b {
        e.store_r32_disp32(Reg::R15, base + 8, b);
    } else {
        e.store_u32_imm_disp32(Reg::R15, base + 8, 0);
    }
    e.store_r32_disp32(Reg::R15, base + 12, result);
}

fn emit_clear_pending(e: &mut Encoder) {
    let base = pending_offset();
    for offset in [0, 4, 8, 12] {
        e.store_u32_imm_disp32(Reg::R15, base + offset, 0);
    }
}

fn emit_store_homes(e: &mut Encoder) {
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.store_r32_disp32(Reg::R15, gpr_offset(index), home);
    }
}

#[cfg(target_os = "windows")]
const X87_NONVOLATILE_XMMS: [Xmm; 6] = [
    Xmm::XMM6,
    Xmm::XMM7,
    Xmm::XMM8,
    Xmm::XMM9,
    Xmm::XMM10,
    Xmm::XMM11,
];

#[cfg(target_os = "windows")]
fn emit_save_x87_host_xmms(e: &mut Encoder) {
    for (index, xmm) in X87_NONVOLATILE_XMMS.into_iter().enumerate() {
        e.vmovupd_disp32_xmm(Reg::RSP, NATIVE_STACK_LEN as i32 + (index as i32) * 16, xmm);
    }
}

#[cfg(target_os = "windows")]
fn emit_restore_x87_host_xmms(e: &mut Encoder) {
    for (index, xmm) in X87_NONVOLATILE_XMMS.into_iter().enumerate() {
        e.vmovupd_xmm_disp32(xmm, Reg::RSP, NATIVE_STACK_LEN as i32 + (index as i32) * 16);
    }
}

fn emit_return(e: &mut Encoder, counter_mask: u16, cached_x87: bool) {
    e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_EXIT);
    for (bit, stack_offset, output_offset) in dynamic_counter_fields() {
        if counter_mask & bit != 0 {
            e.load_r64_disp8(Reg::RAX, Reg::RSP, stack_offset);
            e.store_r64_disp32(Reg::RDI, output_offset as i32, Reg::RAX);
        }
    }
    for (stack_offset, output_offset) in [
        (
            STACK_INSTRUCTIONS,
            core::mem::offset_of!(NativeExit, instructions),
        ),
        (
            STACK_RAW_CLOCKS,
            core::mem::offset_of!(NativeExit, raw_clocks),
        ),
        (
            STACK_BYTE_READS,
            core::mem::offset_of!(NativeExit, byte_reads),
        ),
        (
            STACK_DWORD_READS,
            core::mem::offset_of!(NativeExit, dword_reads),
        ),
        (
            STACK_WEIGHTED_FP_CLOCKS,
            core::mem::offset_of!(NativeExit, weighted_fp_clocks),
        ),
    ] {
        e.load_r64_disp8(Reg::RAX, Reg::RSP, stack_offset);
        e.store_r64_disp32(Reg::RDI, output_offset as i32, Reg::RAX);
    }
    let native_stack_len = if cached_x87 {
        AVX2_X87_STACK_LEN
    } else {
        NATIVE_STACK_LEN
    };
    e.add_r64_imm32(Reg::RSP, native_stack_len);
    #[cfg(target_os = "windows")]
    if cached_x87 {
        e.pop(Reg::RSI);
    }
    for reg in SAVED_HOST_REGS.into_iter().rev() {
        e.pop(reg);
    }
    e.ret();
}
