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

/// LEAVE's stack slot, addressed off EBP rather than ESP. LEAVE sets `ESP <- EBP` and then
/// pops, so the popped dword sits at `SS:EBP`. Addressing it off EBP lets the memory guard
/// run BEFORE any guest register is written, which is what every other lowered kind does:
/// a memory side exit commits the guest homes and returns to the run loop at an instruction
/// boundary, where an interrupt can be delivered, so a half-applied LEAVE would push the
/// interrupt frame at `[EBP-4]` instead of `[ESP-4]`.
fn frame_addr() -> DirectAddr {
    DirectAddr {
        segment: SegmentIndex::Ss,
        base: Some(5),
        index: None,
        scale: 1,
        disp: 0,
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
    e.sub_r64_imm32(Reg::RSP, NATIVE_STACK_LEN);
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        e.store_r64_disp32(Reg::RSP, STACK_SAVED_RSI, Reg::RSI);
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
            DirectKind::MovExtendReg {
                dst,
                src,
                width,
                signed,
            } => emit_mov_extend_reg(&mut e, dst, src, width, signed),
            DirectKind::MovImm { dst, imm } => e.mov_r32_imm32(home(dst), imm),
            DirectKind::MovImmByte { dst, imm } => {
                e.mov_r32_imm32(Reg::RDX, u32::from(imm));
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::Lea { dst, addr } => {
                // LEA never reaches a segment, so it is the one address consumer that would have
                // been missed by putting the wrap on the segmented helper. The interpreter writes
                // `mem.offset`, which a Word `AddrMode` has already masked, while this path adds
                // the whole 32-bit base register.
                emit_effective_address(&mut e, addr, memory.address_wrap);
                e.mov_r32_r32(home(dst), Reg::RAX);
            }
            DirectKind::IncDecReg { dst, is_dec, width } => {
                emit_inc_dec_reg(&mut e, dst, is_dec, width);
            }
            // Zero bytes, on purpose. The slot still costs its instruction, its raw clocks and
            // its EIP advance, all of which the loop tail below charges from the slot list.
            DirectKind::Nop => {}
            DirectKind::Bt { rm, index } => {
                emit_bt_reg(&mut e, rm, index);
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
            DirectKind::TestByte { a, b } => emit_test_byte(&mut e, a, b),
            DirectKind::Imul { dst, src } => emit_imul(&mut e, dst, src),
            DirectKind::ImulMemAcc { addr } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_imul_mem_acc(&mut e, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    MemoryWidth::Dword.needs_alignment_guard(),
                    memory.cpl3,
                    // Read-only. The destination is the implicit EAX and EDX pair, not memory, so
                    // nothing here writes through the fast map.
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
            DirectKind::ImulMem { dst, addr } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_imul_mem(&mut e, dst, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    MemoryWidth::Dword.needs_alignment_guard(),
                    memory.cpl3,
                    // Read-only, exactly as AluMemSource and TestImmMem pass it.
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
            DirectKind::NegReg { dst } => emit_neg_reg(&mut e, dst),
            DirectKind::MulReg { src } => emit_mul_reg(&mut e, src),
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
            DirectKind::RotateRightReg { dst, count } => emit_rotate_right_reg(&mut e, dst, count),
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
            DirectKind::LoadExtend {
                dst,
                width,
                signed,
                addr,
                ..
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_load_extend(&mut e, dst, width, signed, addr, memory, reasons);
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
                emit_store(
                    &mut e,
                    source,
                    width,
                    addr,
                    memory,
                    reasons,
                    memory.address_wrap,
                );
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
            DirectKind::PushMem { addr } => {
                let side = e.label();
                // TWO MemorySideExits, one per address, and both wrong ways panic at emit time.
                // One shared set built from the source addr trips
                // `expect("finite native segment has a limit side exit")` whenever SS has a finite
                // limit and the source segment is flat, because `MemorySideExits::new` derives that
                // Option from a single addr. Reusing one set and appending twice trips the "label
                // placed twice" assertion instead.
                //
                // Both append to the SAME per-slot `side` label. The resolver at the end of this
                // function chains any number of stubs per target.
                //
                // The permission flag must be `memory.cpl3` on BOTH sets, not `false` on the read:
                // `emit_read_permission_check` emits its jump whenever `memory.cpl3` is true, and
                // `append_stubs` places the target only when told to, so a hardcoded `false` leaves
                // a referenced-but-unplaced label and panics the encoder on any CPL3 block. The
                // code-watch flag stays `false` on the read, which is correct: the source performs
                // no write, so that label is genuinely never referenced, and an unreferenced label
                // is free.
                let source_reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                let stack_reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                emit_push_mem(&mut e, addr, memory, source_reasons, stack_reasons);
                source_reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    true,
                    memory.cpl3,
                    false,
                );
                stack_reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    true,
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
                // LAST, and after the side exit is published. A faulting push must leave ESP at
                // its pre-instruction value.
                e.alu_r32_imm32(5, home(4), 4);
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
                    AddressWrap::None,
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
            // PUSH on a 16-bit stack: two bytes at `(SP - 2) & 0xFFFF`, then SP alone advances.
            //
            // Three things differ from the 32-bit arm above and each one is load-bearing. The
            // displacement is -2 rather than -4 and the width is Word, so the slot matches the
            // interpreter's `operand_size.bytes()`. The address wraps at 64K, applied inside
            // `emit_store` before the segment limit compare. And the pointer update is a 16-bit
            // register operation, which on x86-64 preserves bits 31 to 16 rather than
            // zero-extending, exactly reproducing `write_gpr16(4, sp)`.
            //
            // The store still PRECEDES the pointer update, which is the invariant the 32-bit arm
            // already carries: a faulting push must leave SP at its pre-instruction value, or a
            // lazy-commit host that retries the instruction double-decrements it
            // (`memory.rs:1208-1216`, traced to a real Quake crt1 crash). The side exit is
            // published between the two for the same reason.
            DirectKind::Push16 { source } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(2))));
                emit_store(
                    &mut e,
                    source,
                    MemoryWidth::Word,
                    stack_addr(0u32.wrapping_sub(2)),
                    memory,
                    reasons,
                    AddressWrap::Word,
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
                e.alu_r16_imm16(5, home(4), 2);
            }
            DirectKind::Pop { dst } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                emit_ram_read_pointer(
                    &mut e,
                    MemoryWidth::Dword,
                    stack_addr(0),
                    memory,
                    reasons,
                    AddressWrap::None,
                );
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
            // POP on a 16-bit stack: two bytes read at `SP & 0xFFFF`, SP alone advances, and the
            // destination is MERGED into rather than replaced.
            //
            // Three things differ from the 32-bit arm above, and each is load-bearing. The read
            // is Word and its address wraps at 64K, which the read path only gained a parameter
            // for with this slice; the pointer advance is a 16-bit register op, preserving bits
            // 31 to 16; and the destination write is `mov r16, r16`, which merges into the low
            // half exactly as `write_gpr_sized(index, Word, ..)` does, where a 32-bit move would
            // clobber the high half.
            //
            // THE ORDER OF THE LAST TWO IS THE POP SP CASE. When `dst` is 4 the destination IS
            // the stack pointer, and the interpreter advances first and assigns second
            // (`memory.rs` advances inside `pop`, `execute.rs` assigns after it returns), so the
            // final SP is the LOADED WORD, not the advanced pointer. The 32-bit arm above
            // already has this order and the Dword case is pinned on both backends; reversing
            // it here would leave POP SP holding loaded + 2.
            DirectKind::Pop16 { dst } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                emit_ram_read_pointer(
                    &mut e,
                    MemoryWidth::Word,
                    stack_addr(0),
                    memory,
                    reasons,
                    AddressWrap::Word,
                );
                e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0);
                e.alu_r16_imm16(0, home(4), 2);
                emit_write_gpr16(&mut e, dst, Reg::RDX);
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
            // RET near on a 16-bit stack: two bytes read at `SP & 0xFFFF`, the CS limit checked
            // BEFORE any stack release, then SP alone advances by `2 + release`.
            //
            // The order matches `near_return`: the interpreter checks the limit first, then
            // releases the operand width, sets EIP, and releases the immediate. Nothing between
            // its two releases is observable, so one 16-bit add of `2 + release` is congruent
            // mod 2^16 with both of them. `wrapping_add` is what makes that true for a release of
            // 0xFFFE or more, where the sum overflows a u16.
            //
            // The re-load after the completion is not optional, for the reason spelled out in the
            // 32-bit arm above: the completion clobbers RDX. At Word it would be worse there than
            // here, because the word increment loads `1 << 32`, whose low half is zero, so the
            // return address would come out as 0 rather than 1.
            DirectKind::Ret16 { release } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                let limit = memory.segments.cs.limit;
                let limit_exit = (limit != u32::MAX).then(|| e.label());
                emit_ram_read_pointer_inner(
                    &mut e,
                    MemoryWidth::Word,
                    stack_addr(0),
                    memory,
                    reasons,
                    AddressWrap::Word,
                );
                e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0);
                if let Some(limit_exit) = limit_exit {
                    e.cmp_r32_imm32(Reg::RDX, limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::Other));
                }
                emit_mode13_read_completion(&mut e, MemoryWidth::Word);
                e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0);
                e.alu_r16_imm16(0, home(4), release.wrapping_add(2));
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
            DirectKind::Leave => {
                // ESP <- EBP, then POP EBP. The guard runs first, against SS:EBP, which is the
                // address the pop reads from precisely because ESP is about to become EBP.
                // Nothing guest-visible is written until the read has been guarded and performed,
                // so a side exit leaves the whole instruction un-started.
                //
                // `AddressWrap::None` matches the 32-bit `Pop` arm: this kind only ever reaches
                // the emitter on a 32-bit stack, because the stack-width admission matrix in
                // `compile_with_instruction_limit` refuses every `uses_stack()` kind that is not
                // an admitted (SS.B, operand size) pair, and no `Leave16` exists to be admitted.
                let side = e.label();
                let frame = frame_addr();
                let reasons = MemorySideExits::new(&mut e, memory, Some(frame));
                emit_ram_read_pointer(
                    &mut e,
                    MemoryWidth::Dword,
                    frame,
                    memory,
                    reasons,
                    AddressWrap::None,
                );
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                // home(5) is read here and overwritten below, so the order of these three
                // matters: ESP takes EBP's old value before EBP takes the popped one.
                e.mov_r32_r32(home(4), home(5));
                e.add_r32_imm32(home(4), 4);
                e.mov_r32_r32(home(5), Reg::RDX);
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
                // successful x87 instruction cannot make #MF pending for the next slot. No native
                // arm can write status bits 0..5 either: `emit_set_top` touches the TOP field,
                // `emit_condition` bits 8/9/10/14, and `emit_store_physical` and `emit_pop` the
                // tag half above bit 16.
                //
                // FLDCW breaks that invariant from the OTHER side, which is why the gate is
                // re-armed below. The gate condition is `status & 0x3f & !(control & 0x3f)`, and
                // FLDCW changes the MASK: an exception bit set by an earlier INTERPRETED
                // instruction can be masked at block entry and unmasked mid-block. The
                // interpreter re-checks `pending_unmasked_exception` before every x87
                // instruction; a block that emitted its gate once would not.
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
                // Ordering inside the FLDCW slot is already right: the memory pointer emits only
                // guards, then the gate, then the load. So the FLDCW's own check runs against the
                // PRE-FLDCW control word, matching the interpreter, and the next x87 slot rechecks
                // against the new one.
                x87_gate_emitted = !matches!(insn, NativeX87Insn::LoadControlWord { .. });
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
                    AddressWrap::None,
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
                    x87_entry_top.is_some(),
                );
                terminal = true;
                break;
            }
            // CALL rel16 on a 16-bit stack: the return IP is pushed as two bytes at
            // `(SP - 2) & 0xFFFF`, SP alone advances, and the target wraps at 64K.
            //
            // The pushed value needs no masking of its own. `StoreSource::EipDelta` loads the
            // LIVE eip and adds the delta, and the Word store truncates the result, which is
            // exactly what the interpreter's `push(self.registers.eip, Word)` does.
            //
            // The target is NOT masked here either, and does not need to be: the compile loop
            // refuses this kind unless `entry_eip + target_delta` is at or below 0xFFFF, so the
            // architectural mask is a no-op on every admitted block. That refusal only happens
            // because `static_control_target` matches this variant; drop it there and the target
            // is baked unmasked.
            DirectKind::Call16 {
                return_delta,
                target_delta,
            } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(2))));
                emit_store(
                    &mut e,
                    StoreSource::EipDelta(return_delta),
                    MemoryWidth::Word,
                    stack_addr(0u32.wrapping_sub(2)),
                    memory,
                    reasons,
                    AddressWrap::Word,
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
                e.alu_r16_imm16(5, home(4), 2);
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
                    x87_entry_top.is_some(),
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
                    x87_entry_top.is_some(),
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
                    AddressWrap::None,
                );
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                if let Some(limit_exit) = limit_exit {
                    e.cmp_r32_imm32(Reg::RDX, limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::Other));
                }
                emit_mode13_read_completion(&mut e, MemoryWidth::Dword);
                // Re-load the return target. `emit_mode13_read_completion` clobbers RDX on its
                // mode13 branch (`emit_dynamic_increment` is `mov RDX, 1` then an add), which the
                // comments on the load helpers already state. This is the only site that held a
                // live value in RDX across it, so a near RET whose stack read landed on a mode13
                // page and whose target passed the CS-limit check jumped to EIP 1. RDI still
                // holds the pointer; every other read site avoids this by loading RDX after the
                // completion rather than before, and this one now does the same.
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
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
                        x87_entry_top.is_some(),
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
                        x87_entry_top.is_some(),
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
            x87_entry_top.is_some(),
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
        e.load_r64_disp32(Reg::RSI, Reg::RSP, STACK_SAVED_RSI);
        emit_restore_x87_host_xmms(&mut e);
    }
    emit_store_homes(&mut e);
    emit_return(&mut e, COUNTER_ALL);
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

fn emit_store_unresolved_reason(e: &mut Encoder, reason: UnresolvedReason) {
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.store_u32_imm_disp32(
        Reg::RAX,
        core::mem::offset_of!(NativeExit, unresolved_reason) as i32,
        reason as u32,
    );
}

fn emit_advance_eip(e: &mut Encoder, delta: u32) {
    if delta == 0 {
        return;
    }
    e.load_r32_disp32(Reg::RAX, Reg::R15, eip_offset());
    e.add_r32_imm32(Reg::RAX, delta);
    e.store_r32_disp32(Reg::R15, eip_offset(), Reg::RAX);
}

#[allow(clippy::too_many_arguments)]
fn emit_completed_path(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    eip_delta: u32,
    link_cell: Option<usize>,
    shared_return: Label,
    accounting: StaticAccounting,
    x87_source: bool,
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
        // The cell address lives in RCX (not RAX) for the whole branch, including past the
        // quota decrement below, which clobbers RAX. The x87 boundary-spill check further down
        // needs the cell address again right before the transfer, so it must survive in a
        // register the quota bookkeeping never touches.
        e.mov_r64_imm64(Reg::RCX, link_cell as u64);
        e.load_r64_disp8(
            Reg::RDX,
            Reg::RCX,
            core::mem::offset_of!(LinkCell, portal) as i8,
        );
        // A FLOAT source loads `body` and lands on the target directly: its x87 register cache is
        // already live, and a float-to-integer edge is handled by the `spilling` flag below. An
        // INTEGER source loads `integer_entry`, which equals `body` for an integer target and is
        // the shared x87 re-entry pad for a float one. Selected at compile time from the source's
        // own class, so neither class pays a branch, and the 377 M linked transfers of a pure
        // integer chain are unchanged: `integer_entry == body` for every integer target.
        //
        // The zero test still means "unresolved or hidden" for both, because `clear()` zeroes both
        // fields. It ALSO covers the float target whose pad could not be built: `publish_x87`
        // stores zero rather than `body` in that case, so an integer source takes the unresolved
        // path instead of entering an unloaded register cache.
        e.load_r64_disp8(
            Reg::RDX,
            Reg::RDX,
            if x87_source {
                core::mem::offset_of!(BlockPortal, body) as i8
            } else {
                core::mem::offset_of!(BlockPortal, integer_entry) as i8
            },
        );
        e.cmp_r64_imm32(Reg::RDX, 0);
        e.jz(unresolved);
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_QUOTA);
        e.sub_r64_imm32(Reg::RAX, 1);
        e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
        e.jz(returning);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        // Whether THIS edge spills is a per-slot LinkCell property, not something known at
        // compile time (the same source slot can be relinked from an integer target to a float
        // one), so it is a runtime check. An integer source never sets x87_source, so the
        // pure-integer chain pays nothing here: this whole arm does not exist for it.
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if x87_source {
            let transfer = e.label();
            e.test_byte_disp8_imm8(Reg::RCX, core::mem::offset_of!(LinkCell, spilling) as i8, 1);
            e.jz(transfer);
            // Float-to-integer edge: flush the live x87 physical cache and packed status/tag
            // back to CpuGsw.fpu, then restore what Windows needs preserved across the call,
            // before handing control to a body that was never compiled with x87 in mind and so
            // never does either of these things itself.
            emit_x87_spill(e, Reg::R15);
            #[cfg(target_os = "windows")]
            {
                e.load_r64_disp32(Reg::RSI, Reg::RSP, STACK_SAVED_RSI);
                emit_restore_x87_host_xmms(e);
            }
            e.place(transfer);
        }
        #[cfg(not(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )))]
        debug_assert!(!x87_source);
        e.jmp_r64(Reg::RDX);
        e.place(unresolved);
        let hidden = e.label();
        e.load_r64_disp8(
            Reg::RDX,
            Reg::RCX,
            core::mem::offset_of!(LinkCell, portal) as i8,
        );
        e.mov_r64_imm64(Reg::RDI, zero_portal().address() as u64);
        e.cmp_r64_r64(Reg::RDX, Reg::RDI);
        e.jnz(hidden);
        emit_store_unresolved_reason(e, UnresolvedReason::StaticUnbound);
        e.jmp(returning);
        e.place(hidden);
        emit_store_unresolved_reason(e, UnresolvedReason::StaticHidden);
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
    let dynamic_hidden_or_unbound = e.label();
    let unresolved_done = e.label();
    for link_cell in link_cells {
        let next = e.label();
        e.mov_r64_imm64(Reg::RAX, link_cell as u64);
        e.cmp_r32_disp8(
            Reg::RDX,
            Reg::RAX,
            core::mem::offset_of!(LinkCell, target_eip) as i8,
        );
        e.jnz(next);
        e.load_r64_disp8(
            Reg::RCX,
            Reg::RAX,
            core::mem::offset_of!(LinkCell, portal) as i8,
        );
        // Reads `body`, NOT `integer_entry`, and deliberately: `try_link_inner` keeps strict
        // has_x87 equality on the RET PIC path, so this can only ever bind a same-class target,
        // where the two portal fields are equal. See the comment on that check.
        e.load_r64_disp8(
            Reg::RCX,
            Reg::RCX,
            core::mem::offset_of!(BlockPortal, body) as i8,
        );
        e.cmp_r64_imm32(Reg::RCX, 0);
        e.jz(dynamic_hidden_or_unbound);
        e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_QUOTA);
        e.sub_r64_imm32(Reg::RDI, 1);
        e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RDI);
        e.cmp_r64_imm32(Reg::RDI, 0);
        e.jz(shared_return);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        e.jmp_r64(Reg::RCX);
        e.place(next);
    }
    emit_store_unresolved_reason(e, UnresolvedReason::DynamicMissOrUnbound);
    e.jmp(unresolved_done);
    e.place(dynamic_hidden_or_unbound);
    let dynamic_hidden = e.label();
    e.load_r64_disp8(
        Reg::RCX,
        Reg::RAX,
        core::mem::offset_of!(LinkCell, portal) as i8,
    );
    e.mov_r64_imm64(Reg::RDI, zero_portal().address() as u64);
    e.cmp_r64_r64(Reg::RCX, Reg::RDI);
    e.jnz(dynamic_hidden);
    emit_store_unresolved_reason(e, UnresolvedReason::DynamicMissOrUnbound);
    e.jmp(unresolved_done);
    e.place(dynamic_hidden);
    emit_store_unresolved_reason(e, UnresolvedReason::DynamicHidden);
    e.place(unresolved_done);
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
    emit_ram_read_pointer(e, width, addr, memory, sides, memory.address_wrap);
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

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_load_extend(
    e: &mut Encoder,
    dst: u8,
    width: MemoryWidth,
    signed: bool,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    // The memory half is emit_load's verbatim, including every side exit, the cross-page guard and
    // the mode13 completion, all of which emit_ram_read_pointer already parameterises by width.
    // The destination write is what differs. emit_load loads a zero-extended value into RDX and
    // then NARROWS it back through emit_write_gpr8 or emit_write_gpr16, because a MOV r8, r/m8 has
    // to preserve the destination's upper bits. MOVZX and MOVSX must not narrow: they define all
    // 32 bits, so the write is the full home, exactly as emit_load's own Dword arm does it.
    //
    // Reading the pointer BEFORE writing the destination is not incidental. Every side exit is
    // resolved inside emit_ram_read_pointer, so an instruction that faults leaves the destination
    // register untouched, which is what the interpreter does: it faults inside the read, before
    // write_gpr_sized runs.
    emit_ram_read_pointer(e, width, addr, memory, sides, memory.address_wrap);
    match (width, signed) {
        (MemoryWidth::Byte, false) => e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0),
        (MemoryWidth::Byte, true) => e.movsx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0),
        (MemoryWidth::Word, false) => e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0),
        (MemoryWidth::Word, true) => e.movsx_r32_word_disp8(Reg::RDX, Reg::RDI, 0),
        // classify derives the width from the sub-opcode and can only produce Byte or Word. This
        // arm exists so that a future edit passing `operand_width` (which is Dword for every
        // admitted form) fails loudly instead of silently emitting a dword read.
        (MemoryWidth::Dword, _) => {
            unreachable!("MOVZX/MOVSX source width is only ever Byte or Word")
        }
    }
    e.mov_r32_r32(home(dst), Reg::RDX);
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
            x87_memory_width(access),
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
            x87_memory_width(access),
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
    width: MemoryWidth,
    write: bool,
) {
    // No x87 memory form is byte-wide, so the guard is unconditional here rather than gated on
    // `needs_alignment_guard()` the way `emit_ram_read_pointer_inner` gates it.
    debug_assert!(width.needs_alignment_guard());
    let map = memory.map.expect("x87 memory block has fast-map bases");
    emit_segmented_linear_address(e, addr, width, memory, sides, memory.address_wrap);
    // The width here is what the slice's performance rests on, not its byte identity:
    // `BusCycle::clocks_for` ignores width, so a Word access charged as a Dword costs the same
    // bus clocks. What a Dword guard WOULD do is refuse every 2-aligned-but-not-4-aligned
    // control word, and Quake keeps the saved and the chop-mode word in adjacent 2-byte stack
    // slots, so one of each pair is 4-aligned and the other is not by construction.
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

    if write {
        emit_write_permission_check(e, memory.cpl3, sides.permission);
        emit_write_pointer(e, map, sides.unavailable_or_kind);
        let unwatched = e.label();
        emit_code_watch_branch(
            e,
            width,
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
    width: MemoryWidth,
    map: NativeMapBases,
) {
    // These dynamic counters MUST name the same width the static registration does. `run.rs`
    // computes `ram_word_reads = word_reads - mode13_word_reads` with a plain subtraction, so a
    // static dword against a dynamic word underflows to a u64 near 2^64 and is charged straight
    // to the bus. That, and not the bus-clock cost of the access itself, is how a width mistake
    // in this pair shows up.
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_READ_KIND);
    e.mov_r64_r64(Reg::RCX, Reg::RAX);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    let mode13 = e.label();
    let done = e.label();
    e.jz(mode13);
    if direction == NativeX87MemoryDirection::Write {
        match width {
            MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
            _ => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
        }
    }
    e.jmp(done);
    e.place(mode13);
    match direction {
        NativeX87MemoryDirection::Read => match width {
            MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS),
            _ => emit_dynamic_increment(e, STACK_MODE13_DWORD_READS),
        },
        NativeX87MemoryDirection::Write => {
            match width {
                MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES),
                _ => emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES),
            }
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
    wrap: AddressWrap,
) {
    emit_ram_read_pointer_inner(e, width, addr, memory, sides, wrap);
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
    wrap: AddressWrap,
) {
    let map = memory.map.expect("native read has fast-map bases");
    emit_segmented_linear_address(e, addr, width, memory, sides, wrap);
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
    _: AddressWrap,
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
    _: AddressWrap,
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

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_load_extend(
    _: &mut Encoder,
    _: u8,
    _: MemoryWidth,
    _: bool,
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
    emit_ram_read_pointer(e, width, addr, memory, sides, memory.address_wrap);
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
fn emit_imul_mem(
    e: &mut Encoder,
    dst: u8,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    // The memory half is emit_alu_mem_source's Dword arm verbatim. Every side exit, the cross-page
    // guard and the mode13 completion are resolved inside emit_ram_read_pointer, before anything
    // here writes a guest register or a flag, so a faulting IMUL leaves the destination and the
    // flags untouched. That is what the interpreter does: it faults inside read_operand_sized,
    // before read_gpr_sized and before imul_truncated (execute_extended.rs, the 0x0faf arm).
    //
    // RCX is loaded AFTER emit_ram_read_pointer and not before, because
    // emit_mode13_read_completion clobbers RCX (and RDX). RDI survives it and still holds the
    // pointer. home(dst) is safe throughout: GUEST_HOMES is R8-R14 and RBX, disjoint from every
    // scratch register this path uses.
    emit_ram_read_pointer(
        e,
        MemoryWidth::Dword,
        addr,
        memory,
        sides,
        memory.address_wrap,
    );
    e.load_r32_disp8(Reg::RCX, Reg::RDI, 0);
    // The tail is emit_imul's verbatim, and it is correct here for the same reason: the
    // interpreter reaches BOTH operand forms through one imul_truncated (core.rs), which ends in
    // set_flag(FLAG_CF | FLAG_OF, significant). That mask has more than one bit, so it cannot take
    // the single-bit CF-override shortcut; it materializes whatever was pending and writes just
    // those two bits, leaving SF/ZF/AF/PF alone. Capturing CF and OF into the RBP shadow and
    // storing the whole word is that materialize-then-write in one store.
    //
    // Host `imul r32, r32` sets CF = OF = "the 32-bit truncation does not sign-extend back from
    // the full product", which is `significant` exactly, so the flags are right by construction
    // rather than by a recomputation that could drift. Nothing may be inserted between the
    // multiply and emit_capture_flags's pushfq.
    e.imul_r32_r32(home(dst), Reg::RCX);
    emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_imul_mem_acc(
    e: &mut Encoder,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    // Memory half is emit_imul_mem's verbatim. Every side exit resolves inside
    // emit_ram_read_pointer, before any guest register or flag is written, so a faulting IMUL
    // leaves EAX, EDX and the flags untouched, which is what the interpreter does: it faults
    // inside read_operand_sized, before `mul` runs.
    //
    // RCX is loaded AFTER emit_ram_read_pointer because emit_mode13_read_completion clobbers RCX
    // and RDX while leaving RDI holding the pointer.
    //
    // ORDERING INVARIANT, stated as a requirement rather than as an absence. This form has a
    // LARGER aliasing surface than the register form, not a smaller one: the address base and the
    // address index are read from guest homes and either may be EAX or EDX, the two registers this
    // instruction implicitly overwrites. It is safe only because emit_ram_read_pointer resolves
    // the whole effective address into RAX before anything below writes a home.
    emit_ram_read_pointer(
        e,
        MemoryWidth::Dword,
        addr,
        memory,
        sides,
        memory.address_wrap,
    );
    e.load_r32_disp8(Reg::RCX, Reg::RDI, 0);
    // The tail is emit_mul_reg's with the SIGNED primitive. Guest EAX and EDX live in the homes R8
    // and R10 while the host instruction hardwires RAX and RDX, which are emitter scratch, so the
    // accumulator is moved in and the two halves are moved back out.
    //
    // Host `imul r/m32` sets CF = OF = "the full product does not sign-extend back from the low
    // half", which is exactly the interpreter's `significant` for the signed case (core.rs). The
    // UNSIGNED sibling's rule is "the high half is nonzero", a different predicate, which is why
    // the encoder primitive is not shared with mul_r32.
    //
    // The two write-back movs sit between the multiply and emit_capture_flags's pushfq. That is
    // safe because `mov r32, r32` does not write EFLAGS, and it is emit_mul_reg's shipped ordering
    // rather than emit_imul_mem's, which has nothing at all in that gap.
    e.mov_r32_r32(Reg::RAX, home(0));
    e.imul_r32(Reg::RCX);
    e.mov_r32_r32(home(0), Reg::RAX);
    e.mov_r32_r32(home(2), Reg::RDX);
    emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
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
    emit_ram_read_pointer(e, width, addr, memory, sides, memory.address_wrap);
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
        emit_ram_read_pointer(e, width, addr, memory, sides, memory.address_wrap);
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
    emit_segmented_linear_address(e, addr, width, memory, sides, memory.address_wrap);
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
    emit_segmented_linear_address(
        e,
        addr,
        MemoryWidth::Dword,
        memory,
        sides,
        memory.address_wrap,
    );
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
fn emit_imul_mem(_: &mut Encoder, _: u8, _: DirectAddr, _: MemoryEmitContext, _: MemorySideExits) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_imul_mem_acc(_: &mut Encoder, _: DirectAddr, _: MemoryEmitContext, _: MemorySideExits) {
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

pub(super) fn emit_effective_address(e: &mut Encoder, addr: DirectAddr, wrap: AddressWrap) {
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
    // The 64K wrap belongs HERE rather than in the segmented helper, because the effective
    // address is also consumed raw by LEA, which never goes through a segment at all. Applying
    // it at the point the address is FORMED makes "mask before the limit compare" a property of
    // this function instead of an obligation on every caller.
    if wrap == AddressWrap::Word {
        e.and_r32_imm32(Reg::RAX, 0xFFFF);
    }
}

/// Whether the effective address wraps at 64K before the segment base is added.
///
/// `Word` is the 16-bit stack and (later) 16-bit addressing: the architectural EA is
/// `(base + index + disp) & 0xFFFF`. Masking the 32-bit sum ONCE is equivalent, because addition
/// is congruent mod 2^16, and it matches `resolve_memory_addr_mode`'s `(sum as u16)`.
///
/// It is a PARAMETER rather than a field on `DirectAddr` deliberately. A `DirectAddr` field would
/// ride inside kinds that are in clif's `lowerable()` allowlist, such as `Load`, and clif would
/// lower them without the mask. That is the same trap as putting a width field on `Push`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AddressWrap {
    None,
    Word,
}

fn emit_segmented_linear_address(
    e: &mut Encoder,
    addr: DirectAddr,
    width: MemoryWidth,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    wrap: AddressWrap,
) {
    // The mask lands inside `emit_effective_address`, which is BEFORE the limit compare below.
    // That ordering is load-bearing: the compare sits between the effective address and the
    // segment base, so masking afterwards would compare an address the guest never forms.
    emit_effective_address(e, addr, wrap);
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
    wrap: AddressWrap,
) {
    let map = memory.map.expect("native store has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native store has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides, wrap);
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
    emit_segmented_linear_address(e, addr, width, memory, sides, memory.address_wrap);
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
    emit_segmented_linear_address(
        e,
        addr,
        MemoryWidth::Dword,
        memory,
        sides,
        memory.address_wrap,
    );
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

/// PUSH r/m32 through memory (0xFF /6).
///
/// Both accesses refuse every page kind but plain RAM, exactly as `emit_rmw_inc_dec_dword` does.
/// That is what keeps `emit_mode13_read_completion` out of this slot: it increments the dynamic
/// mode-13 read count as soon as the read resolves, and the STORE guards below can still side
/// exit afterwards, at which point the block reports the dynamic counters against a static
/// snapshot taken before the slot. `run.rs`'s `dword_reads - exit.mode13_dword_reads` would go
/// negative, panicking a debug build and wrapping a release one into the bus charge.
///
/// A push whose source is in the mode-13 aperture therefore side exits and the interpreter runs
/// it. That is a missed lowering worth approximately nothing on this corpus, and it makes the
/// underflow unreachable rather than merely avoided by ordering.
///
/// The two accesses take DIFFERENT address wraps. The source takes the block's own
/// `memory.address_wrap`, which is Word whenever CS.D is 0; the stack cell takes `None`, because
/// the stack-width matrix has already restricted this kind to a 32-bit stack. CS.D = 0 with
/// SS.B = 1 is admissible, so the two genuinely differ.
///
/// The caller emits `sub esp, 4` AFTER this returns and after publishing the side exit. That is
/// the invariant `Push16` records: a faulting push must leave ESP at its pre-instruction value,
/// or a lazy-commit host that retries the instruction double-decrements it.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_push_mem(
    e: &mut Encoder,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    source_sides: MemorySideExits,
    stack_sides: MemorySideExits,
) {
    let map = memory.map.expect("native push-mem has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native push-mem has code-watch tables");

    // The SOURCE read. RAM only, and no read-completion counter.
    emit_segmented_linear_address(
        e,
        addr,
        MemoryWidth::Dword,
        memory,
        source_sides,
        memory.address_wrap,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, source_sides.cross_page_or_alignment);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    // The masked KIND goes to RDI. RDX must keep the RAW flags byte, because
    // `emit_read_permission_check` below consumes it. Same split `emit_rmw_inc_dec_dword` uses.
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(source_sides.unavailable_or_kind);
    // LOAD-BEARING, and its absence is a privilege bug rather than a missed lowering: without it
    // a ring-3 `push dword [supervisor_page]` reads supervisor memory natively instead of side
    // exiting to the page fault. `emit_ram_read_pointer_inner` calls this in exactly this
    // position, between the kind check and the bias lookup.
    emit_read_permission_check(e, memory.cpl3, source_sides.permission);
    emit_read_pointer(e, map, source_sides.unavailable_or_kind);
    e.load_r32_disp8(Reg::RDI, Reg::RDI, 0);
    // Park it: the stack store's address and kind path clobbers RAX, RCX, RDX and RDI.
    e.store_r64_disp32(Reg::RSP, STACK_PUSH_MEM_VALUE, Reg::RDI);

    // The STACK write at SS:[ESP-4]. RAM only.
    emit_segmented_linear_address(
        e,
        stack_addr(0u32.wrapping_sub(4)),
        MemoryWidth::Dword,
        memory,
        stack_sides,
        AddressWrap::None,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, stack_sides.cross_page_or_alignment);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(stack_sides.unavailable_or_kind);
    emit_write_permission_check(e, memory.cpl3, stack_sides.permission);
    emit_watched_store_guard(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        stack_sides.code_watch,
    );
    // RCX MUST be recomputed here. `emit_code_watch_branch` leaves one of three watch-probe
    // intermediates in it depending on which path reached the join, and none of them is the page
    // index the bias lookup below needs. Without this the write-bias table is indexed with
    // whichever intermediate the guest's address happened to produce.
    // `emit_rmw_inc_dec_dword` recomputes immediately after its own watch join for this reason.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(stack_sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);
    e.load_r64_disp32(Reg::RDI, Reg::RSP, STACK_PUSH_MEM_VALUE);
    e.store_r32_disp8(Reg::RDX, 0, Reg::RDI);
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

/// MOVZX and MOVSX, register form. No memory access, no flags on any path, so the lazy-flag
/// descriptor is untouched and there are no side exits.
fn emit_mov_extend_reg(e: &mut Encoder, dst: u8, src: u8, width: MemoryWidth, signed: bool) {
    // emit_read_store_value already extracts the byte or word and ZERO-extends it, including the
    // case that makes this slice dangerous: at Byte width `src` 4..=7 means AH/CH/DH/BH, which is
    // bits 8-15 of home(src - 4), and no host home's bits 8-15 are addressable as an x86-64
    // high-byte register. That lane arithmetic is `read_gpr8` transliterated, so reusing it keeps
    // the two definitions from drifting instead of re-deriving it here.
    emit_read_store_value(e, StoreSource::Reg(src), width, Reg::RDX);
    match (width, signed) {
        (MemoryWidth::Byte, true) => e.movsx_r32_r8(Reg::RDX, Reg::RDX),
        (MemoryWidth::Word, true) => e.movsx_r32_r16(Reg::RDX, Reg::RDX),
        (MemoryWidth::Byte | MemoryWidth::Word, false) => {}
        // classify derives the width from the sub-opcode and can only produce Byte or Word. The
        // arm covers BOTH polarities on purpose: at Dword, emit_read_store_value falls through to
        // a plain 32-bit move with no mask, so an unsigned Dword would silently copy the whole
        // source register instead of failing. Mirrors emit_load_extend's guard.
        (MemoryWidth::Dword, _) => {
            unreachable!("MOVZX/MOVSX source width is only ever Byte or Word")
        }
    }
    // These instructions define all 32 bits, so the write is the full register home. Narrowing it
    // the way a MOV r8/r16 has to would preserve the destination's upper bits and be wrong.
    //
    // dst == src is safe by construction, including `movzx eax, ah`: the whole value is
    // materialised into RDX before the single write, and no guest home is RDX.
    e.mov_r32_r32(home(dst), Reg::RDX);
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
            // Word as well as Dword: a 16-bit CALL pushes the return IP as two bytes. The value
            // is computed the same way either width, from the LIVE eip plus the delta, and the
            // caller's `store_r16_disp8` truncates it exactly as `push(.., Word)` does. Byte is
            // still nonsense and stays refused.
            debug_assert!(!matches!(width, MemoryWidth::Byte));
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

// Unlike `emit_rmw_inc_dec_dword`, whose only call site is inside `emit_rmw_inc_dec`'s own gated
// body, `emit_push_mem` is called directly from the `DirectKind::PushMem` arm in the ungated
// `emit` match, the same position `emit_store` and `emit_rmw_inc_dec` are called from. It needs
// both cfg variants for the same reason those two do.
#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_push_mem(
    _: &mut Encoder,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
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

/// BT r/m32, r32 with a register destination. Writes CF alone.
///
/// The host takes the bit offset modulo 32 for a register operand, which is exactly the guest's
/// `raw_index & (bits - 1)` at Dword. No write-back: BT is op 0 in the BT/BTS/BTR/BTC family and
/// the interpreter's `bit_string_op` skips the write for that op alone.
fn emit_bt_reg(e: &mut Encoder, rm: u8, index: u8) {
    e.bt_r32_r32(home(rm), home(index));
    emit_capture_flags(e, crate::FLAG_CF);
    emit_set_cf_only(e);
}

fn emit_inc_dec_reg(e: &mut Encoder, dst: u8, is_dec: bool, width: MemoryWidth) {
    if matches!(width, MemoryWidth::Byte) {
        emit_inc_dec_reg8(e, dst, is_dec);
        return;
    }
    e.mov_r32_r32(Reg::RAX, home(dst));
    match width {
        MemoryWidth::Byte => unreachable!("handled above"),
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

/// INC/DEC r8. Modelled on `emit_alu_byte_imm`, NOT on the word/dword body above: `dst` is a
/// byte-register index where 4..7 are AH/CH/DH/BH, so `home(dst)` would reach guest EBP/ESI/EDI
/// and increment the wrong register by 32 bits.
///
/// Three orderings here are each a silent divergence if broken:
///   - the arithmetic is `alu_r8_r8`, not a 32-bit add on the zero-extended byte, or 0xFF + 1
///     yields 0x100 rather than 0x00 and the host OF/SF/ZF/AF/PF are computed at the wrong width;
///   - `emit_capture_flags` runs BEFORE `emit_pending_inc_dec`, whose `and`/`shl`/`or` clobber
///     host flags;
///   - `emit_write_gpr8` runs AFTER it, because that helper shifts its value register in place
///     and would corrupt the descriptor's recorded result.
///
/// CF is excluded from the capture: INC and DEC preserve it, which is why the descriptor carries
/// the old CF through as an override.
fn emit_inc_dec_reg8(e: &mut Encoder, dst: u8, is_dec: bool) {
    emit_read_store_value(e, StoreSource::Reg(dst), MemoryWidth::Byte, Reg::RAX);
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.mov_r32_imm32(Reg::RCX, 1);
    e.alu_r8_r8(if is_dec { 5 } else { 0 }, Reg::RDX, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, MemoryWidth::Byte, Reg::RAX, Reg::RDX);
    emit_write_gpr8(e, dst, Reg::RDX);
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

fn emit_test_byte(e: &mut Encoder, a: u8, b: u8) {
    emit_read_store_value(e, StoreSource::Reg(a), MemoryWidth::Byte, Reg::RAX);
    emit_read_store_value(e, StoreSource::Reg(b), MemoryWidth::Byte, Reg::RCX);
    emit_test_preloaded(e, MemoryWidth::Byte);
}

fn emit_imul(e: &mut Encoder, dst: u8, src: u8) {
    e.imul_r32_r32(home(dst), home(src));
    // Two-operand IMUL defines CF and OF together and otherwise leaves SF/ZF/AF/PF exactly as
    // they were. The interpreter reaches that through set_flag(FLAG_CF | FLAG_OF, ...): the mask
    // has more than one bit set, so it cannot take the single-bit CF-override shortcut and
    // instead materializes whatever flags were pending before writing just those two bits, which
    // is what leaves the rest untouched. Here that means capturing only CF/OF from the host
    // multiply and writing the whole flags word back in one go: emit_logic_live_af is not needed
    // the way TEST needs it, because TEST leaves its own descriptor live for a later read while
    // IMUL clears pending_flags and commits the full word right away.
    emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

fn emit_neg_reg(e: &mut Encoder, dst: u8) {
    // The interpreter models NEG as `alu_sub(0, value, 0)`, which OVERWRITES pending_flags
    // wholesale with a Sub descriptor and never reads or materializes whatever was pending
    // before. So this has to reproduce that descriptor, not merely the resulting flags: the
    // campaign compares the raw pending_flags word, and materializing eagerly here would agree
    // on eflags() while differing on every byte of the descriptor.
    //
    // a is the constant 0 and b is the OLD destination, which is the reverse of the usual ALU
    // roles. Passing None for a makes emit_pending store an immediate zero, so the zero is
    // structural rather than something a later edit could clobber in a register.
    //
    // Tag 0x8000_0201 is Sub at Dword: op 1 in bits 0-7, width 2 in bits 8-15, cf_override bits
    // 16-17 clear, has-pending bit 31. Identical to what PendingFlags::from_legacy builds for
    // alu_sub, and to the SUB tag emit_alu_preloaded already uses.
    e.mov_r32_r32(Reg::RCX, home(dst));
    e.xor_r64_self(home(dst));
    e.alu_r32_r32(5, home(dst), Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    emit_pending(e, 0x8000_0201, None, Some(Reg::RCX), home(dst));
}

fn emit_mul_reg(e: &mut Encoder, src: u8) {
    // One-operand MUL is the same FLAG shape as two-operand IMUL, so the tail below is
    // emit_imul's verbatim: the interpreter ends in set_flag(FLAG_CF | FLAG_OF, significant)
    // (core.rs), whose mask has more than one bit and therefore cannot take the single-bit
    // CF-override shortcut. It materializes whatever was pending and then writes just those two
    // bits, which is what leaves SF/ZF/AF/PF alone. RBP is a running materialized-eflags shadow,
    // so capturing CF/OF from the host multiply into it and storing the whole word is that same
    // materialize-then-write in one store.
    //
    // Host `mul` sets CF = OF = (high half nonzero), which is `product >> 32 != 0` exactly. The
    // flags are right by construction rather than by a recomputation that could drift from the
    // interpreter's.
    //
    // What is NOT shared with IMUL is the operand shuffle. Guest EAX and EDX live in the homes R8
    // and R10 while the host instruction hardwires RAX and RDX, which are emitter scratch. Reading
    // the multiplicand out of its home AFTER loading RAX and BEFORE writing either home is what
    // makes `src == 0` (EAX squared) and `src == 2` (EDX supplying the multiplicand it is about to
    // be overwritten by) come out right; no home is RAX or RDX, so the source is still intact when
    // the multiply reads it. The two writes are plain movs and clear no flags.
    e.mov_r32_r32(Reg::RAX, home(0));
    e.mul_r32(home(src));
    e.mov_r32_r32(home(0), Reg::RAX);
    e.mov_r32_r32(home(2), Reg::RDX);
    emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
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

/// Publish a CF-ONLY flag write, reproducing `CpuGsw::set_flag(FLAG_CF, ..)` in emitted code.
///
/// Some instructions write CF and architecturally leave SF, ZF, PF, AF and OF alone: a rotate at
/// a count above 1, and BT. For those, publishing RBP wholesale to `eflags` (what `emit_shift`
/// does) would commit whatever the last deferred op left in the shadow and destroy a live
/// descriptor's authority over the other five bits.
///
/// The caller must already have captured CF into RBP, because this reads CF from there. RBP is
/// the running materialized shadow and is never stale; it is `registers.eflags` whose arithmetic
/// bits go stale while a descriptor is live, which is exactly why this has two branches.
///
/// Eagerly materializing instead would be a divergence, not a safe conservatism: the interpreter
/// does NOT materialize on a single-bit CF write, so the two would agree on `eflags()` and differ
/// on every byte of the raw descriptor, which the campaign compares.
fn emit_set_cf_only(e: &mut Encoder) {
    let no_descriptor = e.label();
    let done = e.label();
    e.load_r32_disp32(Reg::RDI, Reg::R15, pending_offset());
    e.mov_r32_r32(Reg::RAX, Reg::RDI);
    // Bit 31 is the has-pending bit. There is no `js` in the encoder, so mask and test for zero.
    e.and_r32_imm32(Reg::RAX, 1 << 31);
    e.jz(no_descriptor);
    // Live descriptor: reproduce PendingFlags::with_cf_override in place. Bit 16 marks an override
    // present, bit 17 carries its value.
    e.and_r32_imm32(Reg::RDI, !(1u32 << 17));
    e.or_r32_imm32(Reg::RDI, 1 << 16);
    e.mov_r32_r32(Reg::RAX, Reg::RBP);
    e.and_r32_imm32(Reg::RAX, crate::FLAG_CF);
    e.shl_r32_imm8(Reg::RAX, 17);
    e.or_r32_r32(Reg::RDI, Reg::RAX);
    e.store_r32_disp32(Reg::R15, pending_offset(), Reg::RDI);
    // set_flag's CF branch also does `eflags |= 0x2` before returning. Bit 1 appears to be set on
    // every path that writes eflags, so this is expected to be a no-op, but it is reproduced
    // rather than relied on: `Registers` derives PartialEq and the campaign compares the raw
    // eflags field, so an assumption that turned out false would be a silent divergence.
    e.load_r32_disp32(Reg::RAX, Reg::R15, eflags_offset());
    e.or_r32_imm32(Reg::RAX, 0x2);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RAX);
    e.jmp(done);
    e.place(no_descriptor);
    // No descriptor: set_flag falls through to set_flag_live, which writes CF straight into
    // eflags. The tag stays zero, which is what materialize_flags leaves behind, so nothing here
    // touches it.
    e.load_r32_disp32(Reg::RAX, Reg::R15, eflags_offset());
    e.and_r32_imm32(Reg::RAX, !crate::FLAG_CF);
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.or_r32_r32(Reg::RAX, Reg::RDI);
    e.or_r32_imm32(Reg::RAX, 0x2);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RAX);
    e.place(done);
}

fn emit_rotate_right_reg(e: &mut Encoder, dst: u8, raw_count: u8) {
    // The five-bit mask is applied HERE, on the raw decoded immediate, the same way emit_shift
    // does it. classify stores the immediate unmasked, so selecting the shape below on the raw
    // byte would misread `ror eax, 32` as a fourth case instead of the no-op it is.
    let count = raw_count & 0x1f;
    if count == 0 {
        // `shift_rotate` returns before touching the value or any flag, and the interpreter's
        // write-back stores the unchanged value into the register. A genuine no-op.
        return;
    }
    // Host ROR agrees with the interpreter by construction. CF is the bit rotated into the MSB,
    // which is what `shift_rotate`'s loop leaves in `cf`, and at count 1 OF is the XOR of the
    // result's top two bits, which is what its OF arm computes for a right rotate.
    e.shift_r32_imm8(1, home(dst), count);
    if count == 1 {
        // Two set_flag calls, and their ORDER is what makes this branch-free. set_flag(FLAG_CF)
        // writes the override into the descriptor; set_flag(FLAG_OF) then sees a mask that is not
        // exactly FLAG_CF, so it materializes WITH that override already applied and writes OF
        // live, clearing the descriptor. The net final eflags is therefore the materialized word
        // with CF and OF replaced by this rotate's, which is exactly RBP after capturing those two
        // bits, because RBP is the running materialized shadow. With no descriptor live the same
        // three instructions are still right: materialize_flags is a no-op and RBP equals eflags.
        emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
        e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
        emit_clear_pending(e);
        return;
    }
    // Counts 2 through 31 write ONLY CF, which is the single mask set_flag updates in place
    // instead of materializing. Nothing else may move: unlike a shift, a rotate architecturally
    // PRESERVES SF, ZF, PF and AF, so publishing RBP to eflags here (what emit_shift does) would
    // commit whatever the last deferred op left and destroy a live descriptor's authority.
    //
    // Capturing only CF keeps the shadow exact for a later slot in the same block. materialized
    // CF comes from the override once one is present, and every other bit still comes from the
    // untouched descriptor, so RBP's other bits must stay frozen at their pre-rotate values. The
    // host rotate's OF is deliberately NOT captured: at counts above 1 it is undefined on x86, and
    // the guest's OF is still owned by the descriptor.
    emit_capture_flags(e, crate::FLAG_CF);
    emit_set_cf_only(e);
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

/// Emit the SHARED x87 re-entry pad. One of these exists per `BlockCache`, in its own executable
/// mapping outside the arena, and every float block's portal points its `integer_entry` at it.
///
/// Reached only by `jmp RDX` from `emit_completed_path` in an INTEGER source block, so on entry:
/// RCX holds the `LinkCell` address (kept out of RAX there precisely so it survives the quota
/// decrement), R15 holds the `CpuGsw` pointer, RDI was just zeroed, RSP is the source block's
/// frame, and RAX/RDX are dead. RBP holds guest EFLAGS and RBX/R12-R14/R8-R11 are guest homes;
/// this pad touches none of them.
///
/// What it exists for: a float block's prologue loads the x87 register cache into XMM4-11 and
/// packs the status/tag word into RSI, and its body addresses that cache relative to the TOP
/// baked at compile time. A chained entry skips the prologue, so without this pad an integer
/// source reaching a float body would run against an unloaded cache. The pad performs exactly the
/// prologue's x87 work, which is also what restores the frame induction that
/// `SpanMeta::link_compatible`'s refusal used to provide: every block that can later reach a
/// float-to-integer crossing (which reloads RSI and XMM6-11 from the frame) was entered either
/// through an x87 prologue or through here, and both write those slots.
///
/// The TOP guard runs FIRST, before any save, so the bail has nothing to undo and must NOT spill:
/// the x87 cache is not live on this path (any earlier float segment was flushed at its own
/// float-to-integer crossing), and running `emit_x87_spill` would write whatever XMM4-11 happen to
/// hold into `CpuGsw.fpu.st`. The bail is therefore byte-identical to an integer block's
/// `shared_return`.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_x87_reentry_pad() -> Vec<u8> {
    let mut e = Encoder::new();
    let bail = e.label();

    // Guard: the target's baked TOP against the CPU's live TOP. `NO_ENTRY_TOP` is 0xFF, outside
    // the legal 0..=7 range, so a cell that was never given a TOP always takes the bail rather
    // than matching a guest that happens to sit at TOP 0.
    e.movzx_r32_byte_disp8(
        Reg::RAX,
        Reg::RCX,
        core::mem::offset_of!(LinkCell, entry_top) as i8,
    );
    e.movzx_r32_word_disp32(
        Reg::RDX,
        Reg::R15,
        crate::jit::x87_avx2_emit::status_offset(),
    );
    e.shr_r32_imm8(Reg::RDX, crate::jit::native_x87::X87_TOP_SHIFT as u8);
    e.and_r32_imm32(Reg::RDX, 7);
    e.cmp_r64_r64(Reg::RAX, Reg::RDX);
    e.jnz(bail);

    // Exactly the prologue's x87 work, in the prologue's order.
    #[cfg(target_os = "windows")]
    {
        e.store_r64_disp32(Reg::RSP, STACK_SAVED_RSI, Reg::RSI);
        emit_save_x87_host_xmms(&mut e);
    }
    crate::jit::x87_avx2_emit::emit_enter(&mut e, Reg::R15);

    // Reload the portal AFTER the enter rather than holding it across: `emit_enter` clobbers only
    // XMM4-11, RSI and RAX today, but `emit_native_x87`'s memory arms use RDI, so keeping the
    // portal there would break the moment an RDI scratch appeared in the enter path.
    e.load_r64_disp8(
        Reg::RDI,
        Reg::RCX,
        core::mem::offset_of!(LinkCell, portal) as i8,
    );
    e.load_r64_disp8(
        Reg::RDX,
        Reg::RDI,
        core::mem::offset_of!(BlockPortal, body) as i8,
    );
    e.jmp_r64(Reg::RDX);

    e.place(bail);
    emit_store_unresolved_reason(&mut e, UnresolvedReason::X87TopMismatch);
    emit_store_homes(&mut e);
    // `emit_return` already ends with `add rsp`, the reversed pops of SAVED_HOST_REGS, and `ret`.
    // Guest EFLAGS in RBP needs no store: it is mirrored to `CpuGsw.eflags` at every
    // flag-producing site, so the memory copy is current at any block boundary.
    emit_return(&mut e, COUNTER_ALL);
    e.finish()
}

fn emit_store_homes(e: &mut Encoder) {
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.store_r32_disp32(Reg::R15, gpr_offset(index), home);
    }
}

#[cfg(target_os = "windows")]
pub(super) const X87_NONVOLATILE_XMMS: [Xmm; 6] = [
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
        e.vmovupd_disp32_xmm(Reg::RSP, STACK_X87_XMM_BASE + (index as i32) * 16, xmm);
    }
}

#[cfg(target_os = "windows")]
fn emit_restore_x87_host_xmms(e: &mut Encoder) {
    for (index, xmm) in X87_NONVOLATILE_XMMS.into_iter().enumerate() {
        e.vmovupd_xmm_disp32(xmm, Reg::RSP, STACK_X87_XMM_BASE + (index as i32) * 16);
    }
}

fn emit_return(e: &mut Encoder, counter_mask: u16) {
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
    e.add_r64_imm32(Reg::RSP, NATIVE_STACK_LEN);
    for reg in SAVED_HOST_REGS.into_iter().rev() {
        e.pop(reg);
    }
    e.ret();
}
