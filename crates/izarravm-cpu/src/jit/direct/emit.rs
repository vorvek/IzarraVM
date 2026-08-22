// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::jit::encoder::Ymm;

// The RMW and push-through-memory emitters live in `emit/mem.rs`: `emit.rs` reached the
// 5,000-line file-policy ceiling. Both cfg variants of every moved item went together, so the
// call sites below resolve identically on every target.
mod mem;
// The one-lookup store path (fast sites + the shared stub pad) lives in `emit/store_fast.rs`
// for the same file-policy reason as `mem`.
mod store_fast;
// The one-lookup load path (lean/parking read sites + the read-resolve pad) lives in
// `emit/load_fast.rs` for the same source-line-ceiling reason.
mod load_fast;

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use load_fast::{emit_ram_read_pointer_fast, emit_read_probe_parking, emit_x87_read_pointer_fast};
use mem::{
    emit_call_mem, emit_code_watch_branch, emit_push_mem, emit_read_pointer, emit_rmw_inc_dec,
    emit_table_base, emit_watched_alu_result_guard, emit_watched_store_guard, emit_write_pointer,
};
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use store_fast::{emit_store_fast, emit_x87_store_pointer_fast};

fn stack_addr(disp: u32) -> DirectAddr {
    DirectAddr {
        segment: SegmentIndex::Ss,
        base: Some(4),
        index: None,
        scale: 1,
        disp,
        disp_lane: None,
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
        disp_lane: None,
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
            stubs.push((segment_limit, common, SideExitReason::SegmentLimit));
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
        interpret_one_cells,
        fetch_trace,
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
    // Clear the accumulator window in whole 32-byte stores rather than one 8-byte store per
    // slot: four stores instead of thirteen, per block ENTRY. `STACK_ZERO_FILL_LEN` carries the
    // slot inventory and the const-asserts that keep it honest.
    //
    // ORDER IS LOAD-BEARING: `STACK_EXIT` and `STACK_QUOTA` live inside the window, so they are
    // written after the fill, not before.
    //
    // AVX, like the x87 host-XMM save below. That is not a new host requirement: no block is
    // ever admitted at all unless `jit::host_supported()` (AVX2) holds — `native_keys_admitted`
    // screens every key on it — so every host that can reach this emitter can execute it.
    e.vxorpd(Xmm::XMM0, Xmm::XMM0, Xmm::XMM0);
    let mut zero_fill = 0i32;
    while zero_fill < STACK_ZERO_FILL_LEN {
        e.vmovupd_disp32_ymm(Reg::RSP, zero_fill, Ymm::YMM0);
        zero_fill += 32;
    }
    e.mov_r64_r64(Reg::R15, CPU_ARG);
    e.mov_r32_r32(Reg::RBP, FLAGS_ARG);
    e.mov_r64_r64(Reg::RAX, EXIT_ARG);
    e.store_r64_disp8(Reg::RSP, STACK_EXIT, Reg::RAX);
    e.mov_r32_r32(Reg::RAX, QUOTA_ARG);
    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
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
    // Which `InterpretOne` cell the next such slot takes. The cells are allocated by the compile
    // walk in slot order, so a cursor is the whole of the mapping; keying them by slot index would
    // have meant a sparse array `MAX_BLOCK_INSTRUCTIONS` long for at most
    // `MAX_BLOCK_CALLOUT_SLOTS` entries.
    let mut interpret_one_index = 0usize;
    for slot in slots {
        match slot.kind {
            DirectKind::MovReg { dst, src, width } => match width {
                MemoryWidth::Word => e.mov_r16_r16(home(dst), home(src)),
                MemoryWidth::Dword => e.mov_r32_r32(home(dst), home(src)),
                MemoryWidth::Byte => unreachable!("byte register moves use MovRegByte"),
                MemoryWidth::Qword | MemoryWidth::Tbyte => {
                    unreachable!("register moves are never 8- or 10-byte wide")
                }
            },
            DirectKind::MovRegByte { dst, src } => {
                emit_read_store_value(&mut e, StoreSource::Reg(src), MemoryWidth::Byte, Reg::RDX);
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::MovExtendReg {
                dst,
                src,
                width,
                dst_width,
                signed,
            } => emit_mov_extend_reg(
                &mut e,
                dst,
                src,
                ExtendWidths::new(width, dst_width),
                signed,
            ),
            // One host instruction at each width. The Word arm staged through RDX when it landed,
            // reusing `MovSegToReg`'s shape rather than adding an encoder form for one kind, and
            // that measured wall-flat on bench16_c while the block structure improved: two host
            // instructions per slot against the Dword arm's one, on an opcode common enough for
            // the difference to cancel what the longer blocks bought. `mov_r16_imm16` exists for
            // that measurement, and `MovSegToReg` keeps the staged shape because its value is a
            // baked selector rather than a decoded immediate.
            DirectKind::MovImm { dst, imm, width } => match width {
                MemoryWidth::Dword => e.mov_r32_imm32(home(dst), imm),
                // `imm as u16` is exact rather than a truncation: `decode`'s `fetch_immediate`
                // zero-extends a Word immediate, so bits 31..16 are already zero here.
                MemoryWidth::Word => e.mov_r16_imm16(home(dst), imm as u16),
                MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
                    unreachable!("classify produces MovImm only at Word or Dword")
                }
            },
            // Sixteen bits only, upper half preserved: the interpreter's 0x8c arm stores at
            // `OperandSize::Word` whatever the prefix says. `mov_r32_imm32` into the scratch
            // first because there is no 16-bit immediate form on the encoder and none is worth
            // adding for one kind; the upper half of RDX is dead either way.
            // `MOV Sreg, r16` in real mode or V86: the field writes `load_segment_real_mode`
            // performs for a non-CS segment, and nothing else. `set_segment` is one
            // array-element assignment, and the only extra work the interpreter does is a
            // CS-only code-cache invalidation that DS and ES never reach (`classify` lowers
            // only `/0` and `/3`, so `segment` here is only ever ES or DS).
            //
            // Every constant comes from `SegmentRegister::real` rather than being written out
            // here, so the two cannot drift. Only `base = selector << 4` is duplicated, and a
            // differential pins it.
            //
            // There is deliberately NO limit store: a real-mode segment load leaves the cached
            // limit alone, which is what unreal/flat-real mode is (see
            // `CpuGsw::load_segment_real_mode`). `emit_segmented_linear_address` bakes the
            // ENTRY limit of every segment it addresses through, and the compile walk's
            // dirty-segment rule ends the block at the first later slot that touches a segment
            // a `LoadSegReal` has written, so the baked limit can never go stale behind this.
            //
            // The same lowering is admitted under V86, where the limit must be 0xFFFF. It is,
            // and that is now true by construction rather than by luck: BOTH V86 entries
            // canonicalize all six segments -- the IRET-into-V86 tail calls `load_segment_real`
            // directly, and a task switch into a V86 task commits EFLAGS.VM before its
            // segment-restore loop, so every selector that loop handles goes through
            // `load_segment_checked`'s V86 branch, INCLUDING a null one (that branch's
            // null-selector short-circuit is explicitly gated off in V86; see `task_switch`).
            // Every in-V86 load thereafter takes the same branch. So "leave the limit" and
            // "write 0xFFFF" coincide there, and omitting the store is correct in both modes.
            // `task_switch_into_v86_builds_a_null_data_selector_as_a_real_mode_segment` is the
            // test that keeps the one hole in that argument closed.
            //
            // The access store IS still required: it is what a real-mode load recomputes.
            // `default_size_32` rides along in the same 16-bit store because it is re-stamped
            // false rather than preserved (again, see `load_segment_real_mode`).
            DirectKind::LoadSegReal { segment, src } => {
                const REAL: SegmentRegister = SegmentRegister::real(0);
                let base = segment_field_base(segment);
                e.mov_r32_r32(Reg::RAX, home(src));
                e.and_r32_imm32(Reg::RAX, 0xffff);
                // The selector is stored BEFORE the shift turns RAX into the base.
                e.store_r16_disp32(Reg::R15, base + selector_offset(), Reg::RAX);
                // One 16-bit store covers `access` and `default_size_32`, which are adjacent.
                // `default_size_32` lands as 0x00, a valid `bool` bit pattern.
                e.mov_r32_imm32(
                    Reg::RDX,
                    u32::from(REAL.access) | (u32::from(REAL.default_size_32) << 8),
                );
                e.store_r16_disp32(Reg::R15, base + access_offset(), Reg::RDX);
                e.shift_r32_imm8(4, Reg::RAX, 4);
                e.store_r32_disp32(Reg::R15, base + base_offset(), Reg::RAX);
            }
            DirectKind::MovSegToReg { dst, segment } => {
                let selector = memory.segments.selector(segment);
                e.mov_r32_imm32(Reg::RDX, u32::from(selector));
                e.mov_r16_r16(home(dst), Reg::RDX);
            }
            // RDX is cleared BEFORE the flag load, not after: `setcc` writes only DL, so the
            // upper 24 bits would otherwise be whatever the previous slot left there and
            // `emit_write_gpr8` would OR that into the guest register. The clear has to precede
            // `emit_load_host_flags` because XOR writes the host flags that load just set up.
            DirectKind::SetCc { condition, dst } => {
                e.xor_r64_self(Reg::RDX);
                emit_load_host_flags(&mut e);
                e.setcc(condition, Reg::RDX);
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            // SETcc m8. The condition byte is produced FIRST -- before `emit_store` touches the
            // address, the page kind or the write pointer -- and parked in the frame, because
            // `emit_store` materialises its source last and this source needs `emit_load_host_
            // flags`, whose `push`/`popfq` pair and RAX use cannot run inside a store's live
            // scratch. See `StoreSource::ParkedByte`.
            //
            // The park must also precede `MemorySideExits::new`'s guards in EFFECT even though it
            // precedes them in emission: it writes only a frame scratch slot and no guest state,
            // so a memory side exit after it leaves the instruction un-started exactly as a plain
            // `Store`'s does. The flag READ is likewise harmless to repeat -- RBP is not consumed.
            DirectKind::SetCcMem { condition, addr } => {
                let side = e.label();
                e.xor_r64_self(Reg::RDX);
                emit_load_host_flags(&mut e);
                e.setcc(condition, Reg::RDX);
                e.store_r64_disp32(Reg::RSP, STACK_PUSH_MEM_VALUE, Reg::RDX);
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_store(
                    &mut e,
                    StoreSource::ParkedByte,
                    MemoryWidth::Byte,
                    addr,
                    memory,
                    reasons,
                    memory.address_wrap,
                );
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    MemoryWidth::Byte.needs_alignment_guard(),
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
            // SAHF. RBP is the running materialized-EFLAGS shadow, so it is already what the
            // interpreter's `materialize_flags()` would settle to; the three lines below are the
            // interpreter's own `eflags = (eflags & !0xd5) | (ah & 0xd5) | 0x02` applied to it,
            // then the publish and the descriptor teardown that `materialize_flags` owes.
            //
            // Read AH the way `emit_read_store_value`'s byte arm reads a high lane: guest AH is
            // bits 8..15 of `home(0)`.
            //
            // `emit_clear_pending` is MANDATORY and is the whole reason this cannot be an RBP
            // write alone: SAHF's five bits are all inside `ARITH_FLAGS`, so a live descriptor
            // that survived this instruction would recompute them from a pre-SAHF operand pair at
            // the next reader and silently overwrite what the guest just loaded.
            DirectKind::Sahf => emit_sahf(&mut e),
            // CBW / CWDE. Dword widens AX to EAX in one step: `movsx_r32_r16` reads the source's
            // low 16 bits and defines all 32, so `home(0)` as both source and destination is
            // safe -- the instruction reads before it writes, same as `emit_mov_extend_reg`'s
            // `dst == src` case. Word (CBW) widens AL to AX and must leave EAX's upper 16 bits
            // alone, so it goes through RDX and `emit_write_gpr16`: `movsx_r32_r8` already sign
            // extends AL across all 32 bits, and the low 16 of that is exactly AL sign-extended
            // to 16, so no further shift is needed.
            DirectKind::Cwde { width } => match width {
                MemoryWidth::Dword => e.movsx_r32_r16(home(0), home(0)),
                MemoryWidth::Word => {
                    e.movsx_r32_r8(Reg::RDX, home(0));
                    emit_write_gpr16(&mut e, 0, Reg::RDX);
                }
                MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
                    unreachable!("classify only ever produces Word or Dword for 0x98")
                }
            },
            // CWD / CDQ. Both widths fill the "upper half" register with 32 copies of the
            // accumulator's sign bit via `sar reg, 31`; the mutation record for this slice is
            // this SAR flipped to a SHR, which fails the fixture at eax = 0x8000_0000. Dword
            // fills EDX from EAX directly. Word must leave EDX's upper 16 bits alone, so the
            // sign is materialized in RDX (via `movsx_r32_r16`, which extends AX's sign across
            // all 32 bits the same way the Dword arm's copy-then-SAR does) and only the low 16
            // are written through `emit_write_gpr16`.
            DirectKind::Cdq { width } => match width {
                MemoryWidth::Dword => {
                    e.mov_r32_r32(home(2), home(0));
                    e.shift_r32_imm8(7, home(2), 31);
                }
                MemoryWidth::Word => {
                    e.movsx_r32_r16(Reg::RDX, home(0));
                    e.shift_r32_imm8(7, Reg::RDX, 31);
                    emit_write_gpr16(&mut e, 2, Reg::RDX);
                }
                MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
                    unreachable!("classify only ever produces Word or Dword for 0x99")
                }
            },
            DirectKind::MovImmByte { dst, imm } => {
                e.mov_r32_imm32(Reg::RDX, u32::from(imm));
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::Lea { dst, addr, width } => {
                // LEA never reaches a segment, so it is the one address consumer that would have
                // been missed by putting the wrap on the segmented helper. The interpreter writes
                // `mem.offset`, which a Word `AddrMode` has already masked, while this path adds
                // the whole 32-bit base register.
                //
                // The WRITE width is the operand size and is a separate question from the wrap
                // above. At Word the interpreter's `write_gpr_sized(reg, Word, offset)` merges
                // sixteen bits and leaves the destination's high half alone, which is what
                // `emit_write_gpr16` does and what the `mov_r32_r32` below would destroy.
                emit_effective_address(&mut e, addr, memory.address_wrap);
                match width {
                    MemoryWidth::Dword => e.mov_r32_r32(home(dst), Reg::RAX),
                    MemoryWidth::Word => emit_write_gpr16(&mut e, dst, Reg::RAX),
                    MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
                        unreachable!("classify only ever produces Word or Dword for 0x8d")
                    }
                }
            }
            DirectKind::IncDecReg { dst, is_dec, width } => {
                emit_inc_dec_reg(&mut e, dst, is_dec, width);
            }
            // Zero bytes, on purpose. The slot still costs its instruction, its raw clocks and
            // its EIP advance, all of which the loop tail below charges from the slot list.
            DirectKind::Nop => {}
            DirectKind::DirectionFlag { set } => emit_direction_flag(&mut e, set),
            // CLC / STC. The flag shadow gets the constant bit, then `emit_set_cf_only` publishes
            // it exactly as `set_flag(FLAG_CF, ..)` would -- through the pending descriptor's CF
            // override when one is live, straight into EFLAGS when none is, and with the trailing
            // `eflags |= 0x2` on both paths.
            //
            // This is NOT `emit_direction_flag`'s shape and must not be simplified into it. DF
            // sits outside `ARITH_FLAGS`, so no lazy descriptor can ever recompute it and a plain
            // RBP-plus-EFLAGS write is complete (which is also why that arm can skip the
            // `eflags |= 0x2` this one reproduces). CF is inside it: a live descriptor that
            // survived this instruction would recompute CF from the pre-CLC operand pair at the
            // next reader and silently overwrite what the guest just set.
            DirectKind::CarryFlag { set } => {
                if set {
                    e.or_r32_imm32(Reg::RBP, crate::FLAG_CF);
                } else {
                    e.and_r32_imm32(Reg::RBP, !crate::FLAG_CF);
                }
                emit_set_cf_only(&mut e);
            }
            DirectKind::ShiftCl { op, dst } => emit_shift_cl(&mut e, op, dst),
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
            // The lane form differs from the baked form in exactly one instruction: where the
            // source operand comes from. `emit_alu` would `mov ecx, imm32`; this loads the same
            // four bytes out of guest RAM instead, so a guest patch of the immediate field takes
            // effect on the next entry with no recompile. Everything after that -- the old
            // destination in EAX, the operation, the flag capture, the lazy-flag record -- is the
            // shared `emit_alu_preloaded`, so a lane slot and a baked slot cannot diverge in
            // result or in flags.
            //
            // RDX is the address scratch and is free here: `GUEST_HOMES` is R8-R14 plus RBX, so
            // no guest register lives in it, and RDX is dead by the time `emit_alu_preloaded`
            // dispatches — every op path may clobber it (the CMP path stages its non-written
            // result there) but none reads it on entry.
            DirectKind::AluImm {
                op,
                dst,
                imm,
                lane,
                width,
            } => match lane {
                // A lane is Dword-only (`IMM_LANE_WIDTH` is four and `imm_lane_for` matches the
                // width), so this arm hard-codes it rather than passing `width` through: passing it
                // would read as if a Word lane were possible.
                Some(lane) => {
                    debug_assert!(matches!(width, MemoryWidth::Dword));
                    e.mov_r64_imm64(Reg::RDX, lane.host as u64);
                    e.load_r32_disp32(Reg::RCX, Reg::RDX, 0);
                    e.mov_r32_r32(Reg::RAX, home(dst));
                    emit_alu_preloaded(&mut e, op, dst, MemoryWidth::Dword);
                }
                None => emit_alu(&mut e, op, dst, None, Some(imm), width),
            },
            // The one-byte lane form, and it differs from the baked form in exactly one
            // instruction for `AluImm`'s reason: where the source operand comes from.
            // `emit_alu_byte_imm` would `mov ecx, imm32` with the compile-time byte
            // zero-extended; this zero-extends the byte out of guest RAM instead, so a guest
            // patch of that one byte takes effect on the next entry with no recompile.
            // `emit_alu_byte_preloaded` reads only CL, so the zero-extension is a convenience
            // and not a correctness term -- but it keeps RCX's upper bits defined, which is what
            // the baked form's `mov r32, imm32` also does.
            //
            // RDX is the address scratch and is free here: `GUEST_HOMES` is R8-R14 plus RBX, and
            // `emit_alu_byte_preloaded`'s FIRST instruction is `mov edx, eax`, so nothing reads
            // RDX on entry. The destination byte is read into RAX BEFORE the address is staged,
            // which matters if `dst` ever aliased the scratch -- it cannot (it is a guest home),
            // but the ordering costs nothing and removes the question.
            DirectKind::AluByteImm { op, dst, imm, lane } => match lane {
                Some(lane) => {
                    debug_assert_eq!(u32::from(lane.width), IMM8_LANE_WIDTH);
                    emit_read_store_value(
                        &mut e,
                        StoreSource::Reg(dst),
                        MemoryWidth::Byte,
                        Reg::RAX,
                    );
                    e.mov_r64_imm64(Reg::RDX, lane.host as u64);
                    e.movzx_r32_byte_disp32(Reg::RCX, Reg::RDX, 0);
                    emit_alu_byte_preloaded(&mut e, op);
                    if op != 7 {
                        emit_write_gpr8(&mut e, dst, Reg::RDX);
                    }
                }
                None => emit_alu_byte_imm(&mut e, op, dst, imm),
            },
            DirectKind::AluRegByte { op, dst, src } => {
                emit_alu_reg_byte(&mut e, op, dst, src);
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
            // The Dword arm is the ORIGINAL `emit_test`, called with the original arguments, so
            // the gate-OFF binary emits byte-identical code for every TEST it has ever emitted.
            // Routing Dword through the width-parameterised helper instead would have been tidier
            // and would have changed three host instructions on a path this slice is not measuring.
            DirectKind::Test {
                a,
                b,
                width: MemoryWidth::Dword,
            } => emit_test(&mut e, a, b),
            // Word (`IZARRAVM_TEST_WORD_ROWS`). `emit_test_byte`'s shape at the other narrow
            // width: read both register operands through `emit_read_store_value`, which masks to
            // the width, then hand them to the same `emit_test_preloaded` that has been emitting
            // the Byte form in production. That helper already carries the 16-bit `alu_r16_r16`
            // and the Word lazy-flag descriptor `0x8000_0102`; neither is new code.
            //
            // TEST writes no register, so the usual Word hazard -- preserving the destination's
            // high half -- does not arise here at all. What has to be right is the FLAGS, and the
            // helper gets them from a genuine 16-bit host AND, which is what the interpreter's
            // `self.alu(4, value, reg, BusWidth::Word)` computes: CF/OF cleared, SF from bit 15,
            // ZF over sixteen bits, PF over the low byte, AF left live by `emit_logic_live_af`.
            DirectKind::Test {
                a,
                b,
                width: MemoryWidth::Word,
            } => {
                emit_read_store_value(&mut e, StoreSource::Reg(a), MemoryWidth::Word, Reg::RAX);
                emit_read_store_value(&mut e, StoreSource::Reg(b), MemoryWidth::Word, Reg::RCX);
                emit_test_preloaded(&mut e, MemoryWidth::Word);
            }
            // `classify` builds this kind from `operand_width`, which is Word or Dword and nothing
            // else, so the remaining widths are unreachable rather than unhandled.
            DirectKind::Test { .. } => {
                unreachable!("TEST r/m,r is only ever Word- or Dword-wide")
            }
            DirectKind::TestByte { a, b } => emit_test_byte(&mut e, a, b),
            DirectKind::Imul { dst, src } => emit_imul(&mut e, dst, src),
            DirectKind::ImulImm { dst, src, imm } => emit_imul_imm(&mut e, dst, src, imm),
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
            DirectKind::ImulRegAcc { src } => emit_imul_reg_acc(&mut e, src),
            // ONE side exit, and it is the divide guard rather than a memory reason: the operands
            // are registers, so nothing here can fault on an address. The guard fires BEFORE any
            // home or flag is written, so the exit leaves the instruction un-started -- the same
            // contract `X87Eligibility` has, and for the same kind of reason (a property of the
            // data, not of the address).
            DirectKind::DivReg { src, signed } => {
                let side = e.label();
                let guard = e.label();
                emit_div_reg(&mut e, src, signed, guard);
                side_exit_reason_stubs.push((guard, side, SideExitReason::DivideGuard));
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
            // TWO side-exit families through ONE `side` label: the read's memory reasons and the
            // divide guard. That is the pairing `DivReg`'s comment said had to be ordered before
            // the memory form could exist, and `emit_div_mem` is where the ordering lives.
            DirectKind::DivMem { addr, signed } => {
                let side = e.label();
                let guard = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_div_mem(&mut e, addr, signed, memory, reasons, guard);
                side_exit_reason_stubs.push((guard, side, SideExitReason::DivideGuard));
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    MemoryWidth::Dword.needs_alignment_guard(),
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
            // The count-lane forms differ from the baked forms in exactly one thing: where the
            // COUNT comes from. That one thing costs more here than it does for `AluByteImm`,
            // because the baked emitters select their whole flag shape from the count's value at
            // emission — see `emit_shift_lane` and `emit_rotate_reg_lane` for the runtime three-way
            // branch that reproduces the selection.
            DirectKind::Shift {
                op,
                dst,
                count,
                width,
                lane,
            } => match lane {
                Some(lane) => emit_shift_lane(&mut e, op, dst, width, lane),
                None => emit_shift(&mut e, op, dst, count, width),
            },
            DirectKind::RotateReg {
                op,
                dst,
                count,
                lane,
            } => match lane {
                Some(lane) => emit_rotate_reg_lane(&mut e, op, dst, lane),
                None => emit_rotate_reg(&mut e, op, dst, count),
            },
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
                dst_width,
                signed,
                addr,
                ..
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_load_extend(
                    &mut e,
                    dst,
                    ExtendWidths::new(width, dst_width),
                    signed,
                    addr,
                    memory,
                    reasons,
                );
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
                // PUSHFD's `materialize_flags()`, in the interpreter's order: settle the lazy
                // descriptor BEFORE the store, so a stack fault leaves the same CPU state the
                // interpreter would. RBP already holds the settled value; what is missing is
                // publishing it and tearing the descriptor down.
                if matches!(source, StoreSource::Flags { .. }) {
                    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
                    emit_clear_pending(&mut e);
                }
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
            // POP Sreg on a 16-bit stack (0x07 ES, 0x1F DS) in real mode or V86. `Pop16`'s read
            // followed by `LoadSegReal`'s write, both unchanged from the arms above.
            //
            // The ORDER is the interpreter's: `pop` advances SP and only then does `load_segment`
            // write the register (execute.rs, the 0x07 / 0x1f arms). Nothing between the two is
            // observable here -- there is no fault path left once the read's guards have passed,
            // and the destination is a segment register, so it can never alias the stack pointer
            // the way `Pop16`'s POP SP case can.
            //
            // The selector is read with `movzx`, so RDX holds it zero-extended and the copy into
            // RAX is exact rather than a truncation. `LoadSegReal` starts from `home(src)` and
            // cannot be shared literally; RDX is then free to stage the access constant, and the
            // read completion has already run (and clobbered RDX) inside `emit_ram_read_pointer`,
            // before the `movzx`, so `Ret16`'s reload hazard does not apply here.
            //
            // Every write below is AFTER `emit_ram_read_pointer`'s guards, which jump to `side`,
            // so a memory exit leaves the instruction un-started: SP has not moved and the
            // segment register still holds its old descriptor.
            //
            // There is deliberately NO limit store, for the reason `LoadSegReal`'s arm gives at
            // length: a real-mode load leaves the cached limit alone (unreal mode) and a V86 entry
            // has already canonicalized all six segments to 0xFFFF. That argument is about the
            // FIELD rather than about where the selector came from, so a selector popped off the
            // stack changes nothing in it.
            DirectKind::PopSegReal { segment } => {
                const REAL: SegmentRegister = SegmentRegister::real(0);
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
                let base = segment_field_base(segment);
                e.mov_r32_r32(Reg::RAX, Reg::RDX);
                // The selector is stored BEFORE the shift turns RAX into the base.
                e.store_r16_disp32(Reg::R15, base + selector_offset(), Reg::RAX);
                // One 16-bit store covers `access` and `default_size_32`, which are adjacent.
                e.mov_r32_imm32(
                    Reg::RDX,
                    u32::from(REAL.access) | (u32::from(REAL.default_size_32) << 8),
                );
                e.store_r16_disp32(Reg::R15, base + access_offset(), Reg::RDX);
                e.shift_r32_imm8(4, Reg::RAX, 4);
                e.store_r32_disp32(Reg::R15, base + base_offset(), Reg::RAX);
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
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::SegmentLimit));
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
                    x87_entry_top.is_some(),
                    fetch_trace,
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
            // LEAVE at Word operand size, both stack widths. The guard runs first, against
            // SS:(E)BP, for the reason the Dword arm above gives: that is the address the pop
            // reads from precisely because the pointer is about to become the frame pointer, and
            // guarding it there leaves a side exit with the whole instruction un-started.
            //
            // Two things differ from the Dword arm and each follows a DIFFERENT width. The READ is
            // Word and the destination write merges sixteen bits, both from the operand size. The
            // pointer move and its advance follow SS.B: 32-bit register operations on a 32-bit
            // stack, and 16-bit ones on a 16-bit stack, where they preserve bits 31 to 16 rather
            // than zero-extending and so reproduce `write_gpr16(4, ..)` exactly.
            //
            // The ORDER of the last two writes matters and matches the Dword arm: home(5) is read
            // into the pointer before the popped word overwrites it. There is no POP SP hazard
            // here the way `Pop16` has one, because the destination is fixed at BP.
            DirectKind::Leave16 { stack32 } => {
                let side = e.label();
                let frame = frame_addr();
                let reasons = MemorySideExits::new(&mut e, memory, Some(frame));
                emit_ram_read_pointer(
                    &mut e,
                    MemoryWidth::Word,
                    frame,
                    memory,
                    reasons,
                    if stack32 {
                        AddressWrap::None
                    } else {
                        AddressWrap::Word
                    },
                );
                e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0);
                if stack32 {
                    e.mov_r32_r32(home(4), home(5));
                    e.add_r32_imm32(home(4), 2);
                } else {
                    e.mov_r16_r16(home(4), home(5));
                    e.alu_r16_imm16(0, home(4), 2);
                }
                emit_write_gpr16(&mut e, 5, Reg::RDX);
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
            // ENTER imm16, 0 at Word operand size, both stack widths.
            //
            // The store PRECEDES every pointer update, which is the invariant `Push16` and `Push`
            // both carry: a faulting push must leave the pointer at its pre-instruction value, or
            // a lazy-commit host that retries the instruction double-decrements it. The side exit
            // is published between the store and the updates for the same reason, so an exit
            // leaves the instruction un-started.
            //
            // The three register operations after it are the interpreter's three steps in its
            // order. The pointer moves by two FIRST, then BP takes the pointer (which is why the
            // store reads home(5) before this line overwrites it), then the frame allocation.
            // Splitting the two subtractions is not an accident of style: BP must hold the
            // pointer AFTER the push and BEFORE the allocation, so they cannot be folded into one.
            //
            // On a 32-bit stack the pointer arithmetic is 32-bit throughout while BP still takes
            // only the low half, which is the 386 PRM 17-62 split: the saved frame pointer is read
            // at StackAddrSize and written at the operand size.
            DirectKind::Enter16 { alloc, stack32 } => {
                let side = e.label();
                let slot_addr = stack_addr(0u32.wrapping_sub(2));
                let wrap = if stack32 {
                    AddressWrap::None
                } else {
                    AddressWrap::Word
                };
                let reasons = MemorySideExits::new(&mut e, memory, Some(slot_addr));
                emit_store(
                    &mut e,
                    StoreSource::Reg(5),
                    MemoryWidth::Word,
                    slot_addr,
                    memory,
                    reasons,
                    wrap,
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
                if stack32 {
                    e.alu_r32_imm32(5, home(4), 2);
                } else {
                    e.alu_r16_imm16(5, home(4), 2);
                }
                e.mov_r16_r16(home(5), home(4));
                if alloc != 0 {
                    if stack32 {
                        e.alu_r32_imm32(5, home(4), u32::from(alloc));
                    } else {
                        e.alu_r16_imm16(5, home(4), alloc);
                    }
                }
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
                side_exit_reason_stubs.push((eligibility, side, SideExitReason::X87Eligibility));
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
            // The interpreter call-out slot. Two exits, both through the ordinary side-exit
            // machinery so nothing about EIP advance, fetch tracing or prefix accounting is
            // reinvented here (`jit/direct/callout.rs` carries the helper contract):
            //
            //   abnormal   EIP at the call-out, prefix = the slots BEFORE it. Byte-for-byte the
            //              state the run loop sees today when a block ends at an IN barrier.
            //   step break EIP AFTER the call-out, prefix = the slots before it PLUS the call-out
            //              itself. `raw_clocks` in the static prefix stays `completed_raw`: this
            //              instruction's charge is runtime and was already added to the lane at
            //              the call site, so counting it here too would double it.
            DirectKind::CallOut { helper } => {
                let abnormal_common = e.label();
                let abnormal_stub = e.label();
                let step_break_common = e.label();
                let step_break_stub = e.label();
                // The `InterpretOne` class needs its slot cell (the fourth argument and the
                // governor byte) and the two RESYNC stubs; the other three classes need neither
                // and emit neither, which is what keeps their bytes identical.
                let slot_cell = helper.interprets_one().then(|| {
                    let cell = interpret_one_cells
                        .get(interpret_one_index)
                        .copied()
                        .expect("every InterpretOne slot must have been allocated a cell");
                    interpret_one_index += 1;
                    cell
                });
                let resync_labels =
                    slot_cell.map(|_| ((e.label(), e.label()), (e.label(), e.label())));
                callout::emit_call_out(
                    &mut e,
                    helper,
                    completed_raw,
                    abnormal_stub,
                    step_break_stub,
                    slot_cell,
                    resync_labels.map(|((stub, _), (fault_stub, _))| (stub, fault_stub)),
                );
                side_exit_reason_stubs.push((
                    abnormal_stub,
                    abnormal_common,
                    SideExitReason::CallOutAbnormal,
                ));
                side_exits.push((
                    abnormal_common,
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
                side_exit_reason_stubs.push((
                    step_break_stub,
                    step_break_common,
                    SideExitReason::CallOutStepBreak,
                ));
                side_exits.push((
                    step_break_common,
                    slot.lin
                        .wrapping_add(u32::from(slot.len))
                        .wrapping_sub(span.key.linear),
                    side_exit(
                        completed + 1,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                // The two RESYNC exits, and the whole of what makes them different from every
                // other exit in this function: their EIP DELTA IS ZERO. The helper already left
                // `cpu.eip` at the architectural next address -- which for a fault is the
                // handler's entry, not the instruction after the slot -- so `emit_advance_eip`
                // must add nothing. Every other exit here computes a delta from the block's entry
                // linear because the body never moved EIP; these two are the exception because the
                // body did.
                if let Some(((resync_stub, resync_common), (fault_stub, fault_common))) =
                    resync_labels
                {
                    side_exit_reason_stubs.push((
                        resync_stub,
                        resync_common,
                        SideExitReason::CallOutResync,
                    ));
                    side_exits.push((
                        resync_common,
                        0,
                        side_exit(
                            // RETIRED: the instruction ran, so the block owns its retirement and
                            // its fetch, exactly as the step-break arm does.
                            completed + 1,
                            completed_raw,
                            completed_byte_reads,
                            completed_word_reads,
                            completed_dword_reads,
                            completed_weighted_fp_clocks,
                        ),
                    ));
                    side_exit_reason_stubs.push((
                        fault_stub,
                        fault_common,
                        SideExitReason::CallOutResyncFault,
                    ));
                    side_exits.push((
                        fault_common,
                        0,
                        side_exit(
                            // NOT retired: `finish_instruction` already counted the instruction in
                            // `perf.instructions` and charged its clocks, and the helper charged
                            // its fetch, so the block reports the prefix and nothing more.
                            completed,
                            completed_raw,
                            completed_byte_reads,
                            completed_word_reads,
                            completed_dword_reads,
                            completed_weighted_fp_clocks,
                        ),
                    ));
                }
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
                    fetch_trace,
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
                    fetch_trace,
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
                    fetch_trace,
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
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::SegmentLimit));
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
                    x87_entry_top.is_some(),
                    fetch_trace,
                );
                terminal = true;
                break;
            }
            DirectKind::JmpMem { addr } => {
                // Modelled on the Ret arm above, minus the ESP adjust: the address is the
                // operand's own `addr`, not the stack slot, and the wrap is `memory.address_wrap`
                // (a 66-prefixed Dword form in a CS.D = 0 segment needs the Word wrap), not
                // `AddressWrap::None`.
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                let limit = memory.segments.cs.limit;
                let limit_exit = (limit != u32::MAX).then(|| e.label());
                emit_ram_read_pointer_inner(
                    &mut e,
                    MemoryWidth::Dword,
                    addr,
                    memory,
                    reasons,
                    memory.address_wrap,
                );
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                if let Some(limit_exit) = limit_exit {
                    e.cmp_r32_imm32(Reg::RDX, limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::SegmentLimit));
                }
                emit_mode13_read_completion(&mut e, MemoryWidth::Dword);
                // Re-load the target. `emit_mode13_read_completion` clobbers RDX on its mode13
                // branch, the exact bug the Ret arm shipped once and fixed above: the completion
                // is emitted AFTER the last side-exit branch, so RDI still holds the pointer and
                // reloading from it is the only way to get the target back into RDX.
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
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
                    x87_entry_top.is_some(),
                    fetch_trace,
                );
                terminal = true;
                break;
            }
            // JMP r32, REGISTER form. `CallReg` minus the push and `JmpMem` minus the read, which
            // leaves the smallest dynamic-successor arm there is: check the target against the CS
            // limit, move it into RDX, take the dynamic path.
            //
            // No `MemorySideExits` and no parking slot, because this kind touches no memory: the
            // limit check is its ONLY side exit, so the `side` label is allocated inside the
            // `limit != u32::MAX` branch rather than above it. Allocating it unconditionally would
            // leave an unreferenced exit on every flat-CS block, which is the common case.
            //
            // ORDER IS LOAD-BEARING, and differently from `CallReg`. `CallReg` publishes before
            // its `sub esp, 4` so a faulting push leaves ESP untouched; this arm has no guest byte
            // to protect at all. What it has instead is the resume point: the side exit records
            // `completed` and `completed_raw` as they stand, so it must be pushed BEFORE they
            // advance for this slot, or the interpreter re-enters past the very instruction that
            // faulted with its 7 clocks already charged. That is mutation M4.
            DirectKind::JmpReg { dst } => {
                let limit = memory.segments.cs.limit;
                if limit != u32::MAX {
                    let side = e.label();
                    let limit_exit = e.label();
                    e.cmp_r32_imm32(home(dst), limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::SegmentLimit));
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
                // Reading `home(dst)` is safe for EVERY dst including ESP: nothing in this arm has
                // written a guest home, so the value is the architectural pre-instruction one.
                e.mov_r32_r32(Reg::RDX, home(dst));
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
                    x87_entry_top.is_some(),
                    fetch_trace,
                );
                terminal = true;
                break;
            }
            // CALL r32, REGISTER form. Modelled on the `Call` arm above (the store, the publish,
            // then `sub esp, 4`), with two differences: the CS-limit check from `Ret`/`JmpMem`
            // runs FIRST, against `home(dst)` directly rather than a loaded dword, and the tail is
            // the dynamic path rather than the static one.
            //
            // The limit check must run before ANY mutation, matching the interpreter's own fault
            // ordering: the interpreter pushes first and only faults on the NEXT fetch, so a
            // native side exit here has to leave every guest-visible byte untouched, letting the
            // interpreter's re-run of this same instruction reproduce push-then-fault exactly.
            DirectKind::CallReg { dst, return_delta } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                let limit = memory.segments.cs.limit;
                let limit_exit = (limit != u32::MAX).then(|| e.label());
                if let Some(limit_exit) = limit_exit {
                    e.cmp_r32_imm32(home(dst), limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::SegmentLimit));
                }
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
                // Pre-adjust: correct for EVERY dst, ESP included. `emit_store` cannot have
                // clobbered `home(dst)`: its scratch set is RAX/RCX/RDX/RDI, and GUEST_HOMES is
                // R8-R14 plus RBX, so the register read at the top of this arm is still live here.
                e.mov_r32_r32(Reg::RDX, home(dst));
                // AFTER the side exit is published, the same faulting-push invariant `Call` keeps:
                // a faulting push must leave ESP at its pre-instruction value.
                e.alu_r32_imm32(5, home(4), 4);
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
                    x87_entry_top.is_some(),
                    fetch_trace,
                );
                terminal = true;
                break;
            }
            // CALL r/m32, MEMORY form. The read half is `PushMem`'s (two `MemorySideExits`, a
            // RAM-only source with no read completion, the same parking slot); the tail is
            // `CallReg`'s (publish the side exit, THEN `sub esp, 4`, then the dynamic path).
            //
            // The CS-limit check sits INSIDE `emit_call_mem`, between the target load and the
            // stack store, which is the only position that satisfies both orderings this
            // instruction has to respect at once: it needs the loaded target to compare, and it
            // must precede every guest-visible mutation so a limit side exit lets the interpreter
            // reproduce push-then-fault from an untouched state. `CallReg` puts it first because
            // its target needs no load; `JmpMem` puts it after its load because it never mutates.
            DirectKind::CallMem { addr, return_delta } => {
                let side = e.label();
                let source_reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                let stack_reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                let limit = memory.segments.cs.limit;
                let limit_exit = (limit != u32::MAX).then(|| e.label());
                emit_call_mem(
                    &mut e,
                    addr,
                    return_delta,
                    memory,
                    source_reasons,
                    stack_reasons,
                    limit_exit.map(|label| (limit, label)),
                );
                if let Some(limit_exit) = limit_exit {
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::SegmentLimit));
                }
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
                // AFTER the side exit is published, the same faulting-push invariant `Call`,
                // `CallReg` and `PushMem` all keep: a faulting push leaves ESP untouched. The
                // target is live in RDX across this and stays live: `home(4)` is a GUEST_HOMES
                // register, never a scratch one.
                e.alu_r32_imm32(5, home(4), 4);
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
                    x87_entry_top.is_some(),
                    fetch_trace,
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
                        fetch_trace,
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
                        fetch_trace,
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
            fetch_trace,
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
            fetch_trace,
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
        // The side-exit prefix count is parked ONLY for the shared `side_return` fetch-trace
        // append below (`TracePrefix::Stack(STACK_READ_KIND)`); nothing else reads the slot
        // after a side exit. With the trace elided the park has no consumer, so it goes too.
        if fetch_trace {
            e.mov_r64_imm64(Reg::RAX, u64::from(exit.instructions));
            e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RAX);
        }
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
        if fetch_trace {
            emit_fetch_trace(
                &mut e,
                span,
                self_loop,
                TracePrefix::Stack(STACK_READ_KIND),
                false,
            );
        }
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
    emit_return(&mut e);
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
    fetch_trace: bool,
) {
    if self_loop {
        emit_add_repeated_accounting(e, full);
    } else if completed {
        emit_add_static_accounting(e, full);
    }
    emit_add_static_accounting(e, prefix);
    if fetch_trace {
        emit_fetch_trace(
            e,
            span,
            self_loop,
            TracePrefix::Immediate(u32::from(prefix.instructions)),
            completed,
        );
    }
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

#[cfg(feature = "direct-link-refusal-census")]
fn emit_store_direct_link_refusal_census_id(e: &mut Encoder) {
    e.load_r32_disp32(
        Reg::RDX,
        Reg::RCX,
        core::mem::offset_of!(LinkCell, direct_link_refusal_census_id) as i32,
    );
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.store_r32_disp32(
        Reg::RAX,
        core::mem::offset_of!(NativeExit, direct_link_refusal_census_id) as i32,
        Reg::RDX,
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
    fetch_trace: bool,
) {
    emit_accounting(
        e,
        span,
        self_loop,
        StaticAccounting::default(),
        true,
        accounting,
        fetch_trace,
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
        #[cfg(feature = "direct-link-refusal-census")]
        emit_store_direct_link_refusal_census_id(e);
        e.jmp(returning);
        e.place(hidden);
        emit_store_unresolved_reason(e, UnresolvedReason::StaticHidden);
        e.place(returning);
    }
    e.jmp(shared_return);
}

/// The RET PIC completion. Mirrors `emit_completed_path`'s register convention on purpose: RCX
/// holds the `LinkCell` address across the whole taken branch and RDX holds the resolved target,
/// which is what lets the x87 boundary spill below be the same code in both places.
///
/// The cell has to be in RCX rather than RAX because `emit_increment_exit_u32` clobbers RAX, and
/// the spill has to happen after the quota decrement (an exhausted quota returns to the block's
/// own epilogue, which spills for itself). Holding the target in RDX rather than RCX costs one
/// `mov` and buys the same property: `x87_avx2_emit::emit_spill` clobbers RAX and RSI and leaves
/// RCX/RDX alone. RDX is free to reuse at that point because the two unresolved epilogues reach
/// it only from branches taken BEFORE the move, and the EIP it held is in `CpuGsw.eip` anyway.
#[allow(clippy::too_many_arguments)]
fn emit_completed_dynamic_path(
    e: &mut Encoder,
    span: BlockSpan,
    target: Reg,
    link_cells: [usize; 2],
    shared_return: Label,
    accounting: StaticAccounting,
    x87_source: bool,
    fetch_trace: bool,
) {
    e.store_r32_disp32(Reg::R15, eip_offset(), target);
    emit_accounting(
        e,
        span,
        false,
        StaticAccounting::default(),
        true,
        accounting,
        fetch_trace,
    );
    e.load_r32_disp32(Reg::RDX, Reg::R15, eip_offset());
    let dynamic_hidden_or_unbound = e.label();
    let unresolved_done = e.label();
    for link_cell in link_cells {
        let next = e.label();
        e.mov_r64_imm64(Reg::RCX, link_cell as u64);
        e.cmp_r32_disp8(
            Reg::RDX,
            Reg::RCX,
            core::mem::offset_of!(LinkCell, target_eip) as i8,
        );
        e.jnz(next);
        e.load_r64_disp8(
            Reg::RAX,
            Reg::RCX,
            core::mem::offset_of!(LinkCell, portal) as i8,
        );
        // The same compile-time field selection `emit_completed_path` makes, for the same reason.
        // A FLOAT source loads `body` and lands on the target directly, its register cache already
        // live, with the float-to-integer case handled by the `spilling` test below. An INTEGER
        // source loads `integer_entry`, which IS `body` for an integer target and is the shared
        // x87 re-entry pad for a float one - and the pad is why the cell has to be in RCX at the
        // jump, which is the register it reads its entry TOP and its portal from.
        //
        // The zero test keeps meaning "unresolved or hidden" for both, because `clear()` zeroes
        // both fields, and it also covers the float target whose pad could not be built:
        // `publish_x87` stores zero rather than `body` there, so an integer source takes the
        // unresolved path instead of entering an unloaded register cache.
        e.load_r64_disp8(
            Reg::RAX,
            Reg::RAX,
            if x87_source {
                core::mem::offset_of!(BlockPortal, body) as i8
            } else {
                core::mem::offset_of!(BlockPortal, integer_entry) as i8
            },
        );
        e.cmp_r64_imm32(Reg::RAX, 0);
        e.jz(dynamic_hidden_or_unbound);
        e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_QUOTA);
        e.sub_r64_imm32(Reg::RDI, 1);
        e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RDI);
        e.cmp_r64_imm32(Reg::RDI, 0);
        e.jz(shared_return);
        e.mov_r64_r64(Reg::RDX, Reg::RAX);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        // The same per-slot runtime check `emit_completed_path` makes, and for the same reason:
        // whether THIS edge spills is a `LinkCell` property, not a compile-time one, because the
        // cell can be relinked from a float target to an integer one. An integer source never
        // sets `x87_source`, so a pure integer chain does not pay for this arm's existence.
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if x87_source {
            let transfer = e.label();
            e.test_byte_disp8_imm8(Reg::RCX, core::mem::offset_of!(LinkCell, spilling) as i8, 1);
            e.jz(transfer);
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
        e.place(next);
    }
    emit_store_unresolved_reason(e, UnresolvedReason::DynamicMissOrUnbound);
    e.jmp(unresolved_done);
    e.place(dynamic_hidden_or_unbound);
    let dynamic_hidden = e.label();
    e.load_r64_disp8(
        Reg::RAX,
        Reg::RCX,
        core::mem::offset_of!(LinkCell, portal) as i8,
    );
    e.mov_r64_imm64(Reg::RDI, zero_portal().address() as u64);
    e.cmp_r64_r64(Reg::RAX, Reg::RDI);
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
        // classify only ever produces a GPR-sized `Load` (Byte, Word or Dword); the x87 memory
        // forms route through `emit_x87_slot`/`x87_avx2_emit`, not here.
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR loads are never 8- or 10-byte wide")
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
    widths: ExtendWidths,
    signed: bool,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let ExtendWidths {
        source: width,
        destination: dst_width,
    } = widths;
    // The memory half is emit_load's verbatim, including every side exit, the cross-page guard and
    // the mode13 completion, all of which emit_ram_read_pointer already parameterises by width.
    // The destination write is what differs, and it differs in BOTH directions from emit_load's.
    // emit_load loads a zero-extended value into RDX and then narrows it back through
    // emit_write_gpr8 or emit_write_gpr16, because a MOV r8, r/m8 preserves the destination's
    // upper bits. MOVZX and MOVSX narrow only to the OPERAND size, which is a different width from
    // the source: 0F B6 extends a byte across all 32 bits, 66 0F B6 extends the same byte across
    // 16 and leaves the destination's high half alone.
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
        // classify derives the SOURCE width from the sub-opcode and can only produce Byte or Word.
        // This arm exists so that a future edit passing `operand_width` here -- which is now a live
        // local rather than a constant, so the confusion is easier to make than it was -- fails
        // loudly instead of silently emitting a dword read.
        (MemoryWidth::Dword, _) => {
            unreachable!("MOVZX/MOVSX source width is only ever Byte or Word")
        }
        (MemoryWidth::Qword | MemoryWidth::Tbyte, _) => {
            unreachable!("MOVZX/MOVSX source width is only ever Byte or Word")
        }
    }
    emit_extend_write_back(e, dst, dst_width);
}

/// The two widths a MOVZX/MOVSX carries, travelling together.
///
/// They are the same type and mean opposite things -- `source` is Byte or Word and comes from the
/// sub-opcode, `destination` is Word or Dword and is the operand size -- so as two positional
/// arguments they are silently swappable, and swapping them is precisely the miscompile this
/// slice exists to prevent. Named fields make the swap a compile error instead. The constructor
/// asserts the domains in debug builds, which is where a `classify` edit that passed
/// `operand_width` for the source would be caught.
#[derive(Clone, Copy)]
struct ExtendWidths {
    source: MemoryWidth,
    destination: MemoryWidth,
}

impl ExtendWidths {
    fn new(source: MemoryWidth, destination: MemoryWidth) -> Self {
        debug_assert!(matches!(source, MemoryWidth::Byte | MemoryWidth::Word));
        debug_assert!(matches!(
            destination,
            MemoryWidth::Word | MemoryWidth::Dword
        ));
        Self {
            source,
            destination,
        }
    }
}

/// The write-back half of MOVZX/MOVSX, shared by the memory and register forms so the two cannot
/// drift on the one property that admits the 66-prefixed encodings.
///
/// The value is already fully extended in RDX. Dword defines all 32 bits of the destination; Word
/// defines only the low 16 and preserves the high 16, which is `write_gpr_sized(.., Word, ..)`
/// verbatim. Emitting the Dword form for a Word operand size is not a lost optimisation, it is a
/// clobber of architectural state the instruction must not touch, and it is the reason these four
/// opcodes were kept out of classify's Word allowlist until this field existed.
fn emit_extend_write_back(e: &mut Encoder, dst: u8, dst_width: MemoryWidth) {
    match dst_width {
        MemoryWidth::Dword => e.mov_r32_r32(home(dst), Reg::RDX),
        MemoryWidth::Word => emit_write_gpr16(e, dst, Reg::RDX),
        // `dst_width` is `operand_width`, which `classify` derives from CS.D and the 0x66 prefix
        // alone; it has no other value. Byte gets its own arm rather than joining the wide ones so
        // that a caller confusing `dst_width` with the SOURCE width -- the one real hazard here --
        // names the mistake it made.
        MemoryWidth::Byte => unreachable!("MOVZX/MOVSX destination width is never Byte"),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("MOVZX/MOVSX destination width is never 8- or 10-byte wide")
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
            memory.r15_tables,
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

    if write && memory.one_lookup_store {
        // The one-lookup probe replaces the classify, the resolve AND the kind-pack tail
        // below — the fast arms park `STACK_READ_KIND` from the statically-known kind, and
        // the resolve stub parks it on the way out.
        emit_x87_store_pointer_fast(e, width, memory, sides);
        return;
    }
    if !write && memory.one_lookup_load {
        // The read twin (load design D5), same replacement scope: the fast arm parks the RAM
        // pack, the read-resolve stub parks every other case, and the untouched completion
        // does all width accounting from the pack.
        emit_x87_read_pointer_fast(e, memory, sides);
        return;
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
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
        // The same two D3 shapes as `emit_store`'s arms; RCX is the live page index the
        // helper's re-read consumes.
        emit_store_write_resolve(
            e,
            width,
            map,
            memory
                .code_watch_tables
                .expect("x87 store has code-watch tables"),
            memory,
            sides,
        );
    } else {
        emit_read_permission_check(e, memory.cpl3, sides.permission);
        emit_read_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    }

    // Preserve the guest address and page kind across the x87 emitter while RDI remains the host
    // memory pointer. This stack slot is no longer needed by the completed code-watch probe.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
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
    r15_tables: bool,
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
            MemoryWidth::Byte => unreachable!("no x87 memory form is byte-wide"),
            MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
            MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
            // Two dword transactions per m64 access (`write_qword` splits into two independent
            // 4-aligned dword writes; see `MemoryWidth::alignment_bytes`), so the dword lane
            // moves by 2 rather than 1. This is the RAM lane, which is not mode-13: it feeds
            // `exit.ram_dword_writes`, charged directly at run.rs.
            MemoryWidth::Qword => emit_dynamic_increment_by(e, STACK_RAM_DWORD_WRITES, 2),
            // An m80 write is three transactions, not two: `write_extended80` issues
            // `write_qword`'s two dwords and then a word at +8. Both lanes move, and they must
            // move together -- charging only the dword pair here against a static registration
            // that counts the word too is exactly the underflow `x87_memory_width`'s comment
            // warns about.
            MemoryWidth::Tbyte => {
                emit_dynamic_increment_by(e, STACK_RAM_DWORD_WRITES, 2);
                emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES);
            }
        }
    }
    e.jmp(done);
    e.place(mode13);
    match direction {
        NativeX87MemoryDirection::Read => match width {
            MemoryWidth::Byte => unreachable!("no x87 memory form is byte-wide"),
            MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS),
            MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_READS),
            // Same reasoning as the RAM write arm above: an m64 read wholly inside the aperture
            // increments mode13_dword_reads by 2, not 1. A read cannot straddle RAM and the
            // aperture (the guard admits only single-page accesses), so this is all-or-nothing
            // per access.
            MemoryWidth::Qword => emit_dynamic_increment_by(e, STACK_MODE13_DWORD_READS, 2),
            // No x87 m80 READ form is lowered (FLD m80 is deferred), so this arm is unreachable
            // rather than merely unused, and it says so instead of guessing an accounting.
            MemoryWidth::Tbyte => unreachable!("no x87 m80 read form is lowered"),
        },
        NativeX87MemoryDirection::Write => {
            match width {
                MemoryWidth::Byte => unreachable!("no x87 memory form is byte-wide"),
                MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES),
                MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES),
                MemoryWidth::Qword => emit_dynamic_increment_by(e, STACK_MODE13_DWORD_WRITES, 2),
                // The aperture mirror of the RAM arm above, same three transactions.
                MemoryWidth::Tbyte => {
                    emit_dynamic_increment_by(e, STACK_MODE13_DWORD_WRITES, 2);
                    emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES);
                }
            }
            emit_mode13_dirty_bit(e, r15_tables, map);
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
    if memory.one_lookup_load {
        // The lean one-lookup site REPLACES the pair below wholesale (load design D3a): its
        // fast RAM arm writes no frame slot and its mode13 arms count inline or in the cold
        // stub join, so composing it with the trailing completion would stale-read or
        // double-count (design F5).
        emit_ram_read_pointer_fast(e, width, addr, memory, sides, wrap);
        return;
    }
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

    if memory.one_lookup_load {
        // The parking probe (load design D3b): kind parked in STACK_READ_KIND, pointer in RDI,
        // NO counter moved — the direct callers of this `_inner` form (Ret/Ret16/JmpMem) run
        // their CS-limit side exit and only then call `emit_mode13_read_completion`, and that
        // deferred-increment ordering must survive the probe swap byte-identically.
        emit_read_probe_parking(e, memory, sides);
        return;
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);

    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
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
    emit_read_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
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
        // This path serves the GPR read kinds (Load, AluMemSource, RmwIncDec's read half,
        // ImulMem...); the x87 memory forms have their own dynamic completion,
        // `emit_x87_memory_completion`, which is where the Qword +2 accounting lives.
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR memory reads are never 8- or 10-byte wide")
        }
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
    _: ExtendWidths,
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-source operands are never 8- or 10-byte wide")
        }
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("TEST immediate memory operands are never 8- or 10-byte wide")
        }
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
            MemoryWidth::Qword | MemoryWidth::Tbyte => {
                unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
            }
        }
        emit_read_store_value(e, source, width, Reg::RCX);
        match width {
            MemoryWidth::Byte => emit_alu_byte_preloaded(e, op),
            MemoryWidth::Word | MemoryWidth::Dword => emit_alu_preloaded(e, op, 0, width),
            MemoryWidth::Qword | MemoryWidth::Tbyte => {
                unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
            }
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    // D3 Group B (review F1): the flags byte is DEAD in RDX by the guard position below — RDX
    // holds the ALU result candidate there, and testing THAT for bit 6 would skip SMC detection
    // on guest data. So the bit rides beside the kind in the `STACK_ALU_ADDRESS_KIND` pack.
    // cpl0 only: the fold consumes RDX, which the cpl3 permission check needs first, and no
    // scratch survives to carry it (H3) — cpl3 blocks keep the unconditional guard.
    let watch_fast = memory.watch_page_bit && !memory.cpl3;
    if watch_fast {
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_WATCHED));
        e.or_r32_r32(Reg::RDI, Reg::RDX);
    }
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // Save the effective address and page kind before ADC/SBB load host flags into RAX. Nothing
    // below this point mutates architectural state until every pointer and code-watch check passes.
    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);
    emit_read_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
        }
    }
    emit_read_store_value(e, source, width, Reg::RCX);
    emit_alu_candidate(e, op, width);

    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    let skip_guard = e.label();
    if watch_fast {
        // The candidate lives in the pending slots, not RDX, so the pack reload may clobber it.
        e.load_r64_disp32(Reg::RDX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
        e.shift_r64_imm8(5, Reg::RDX, 32);
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_WATCHED));
        e.jz(skip_guard);
    }
    emit_watched_alu_result_guard(
        e,
        width,
        memory.r15_tables,
        map,
        code_watch_tables,
        sides.code_watch,
    );
    e.place(skip_guard);

    emit_commit_alu_candidate(e, op, source, width);
    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
        }
    }

    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    // Unconditional kind masking (H6): the pack's upper word may carry PAGE_WATCHED beside the
    // kind, and an unmasked equality compare would misroute every watched mode-13 ALU dest.
    // Emitted on the bit-off arm too — a no-op there, and immune to the arms drifting apart.
    e.and_r32_imm32(Reg::RCX, u32::from(NATIVE_KIND_MASK));
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
        }
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
        }
    }
    emit_mode13_dirty_bit(e, memory.r15_tables, map);
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    // D3 Group B, exactly as `emit_alu_mem_dest`: the candidate owns RDX at the guard position,
    // so the bit rides the `STACK_ALU_ADDRESS_KIND` pack; cpl0 only (H3).
    let watch_fast = memory.watch_page_bit && !memory.cpl3;
    if watch_fast {
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_WATCHED));
        e.or_r32_r32(Reg::RDI, Reg::RDX);
    }
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // Save the effective address and page kind before computing the candidate. Architectural
    // flags, registers, and memory remain untouched until every pointer and code-watch check passes.
    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);
    emit_read_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT, Reg::RDX);
    emit_double_shift_candidate(e, left, src, count, Reg::RDX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT + 4, Reg::RDX);

    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    let skip_guard = e.label();
    if watch_fast {
        e.load_r64_disp32(Reg::RDX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
        e.shift_r64_imm8(5, Reg::RDX, 32);
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_WATCHED));
        e.jz(skip_guard);
    }
    emit_watched_alu_result_guard(
        e,
        MemoryWidth::Dword,
        memory.r15_tables,
        map,
        code_watch_tables,
        sides.code_watch,
    );
    e.place(skip_guard);

    emit_commit_double_shift_flags(e, count);
    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
    e.store_r32_disp8(Reg::RDI, 0, Reg::RDX);

    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    // Unconditional kind masking (H6), as in `emit_alu_mem_dest`.
    e.and_r32_imm32(Reg::RCX, u32::from(NATIVE_KIND_MASK));
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
    e.jmp(done);
    e.place(mode13);
    emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
    emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
    emit_mode13_dirty_bit(e, memory.r15_tables, map);
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
    // The lane form differs from the baked form in exactly where the displacement comes from:
    // the four bytes of the guest instruction's disp32 field, loaded on every execution, so a
    // guest patch of that field takes effect on the next entry with no recompile. Like the
    // baked arm, the lane arm writes RAX and nothing else — and the 32-bit load clears the
    // upper half, leaving exactly the state `mov eax, imm32` leaves — so the two arms present
    // one register contract to every caller. (The scale != 1 index path below clobbers RCX in
    // BOTH arms; that is this function's pre-existing contract, not the lane's.) Everything
    // downstream (base/index adds, the 64K wrap, the segment-limit compare, the fast-map
    // lookup and its guards) already runs on the runtime value, which is what makes a patched
    // displacement take the same side exits the baked form would.
    match addr.disp_lane {
        Some(lane) => {
            e.mov_r64_imm64(Reg::RAX, lane.host as u64);
            e.load_r32_disp32(Reg::RAX, Reg::RAX, 0);
        }
        None => e.mov_r32_imm32(Reg::RAX, addr.disp),
    }
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
/// ride inside many kinds (`Load` among them) whose emitters would then need to remember to
/// apply the mask individually. That is the same trap as putting a width field on `Push`.
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
        // `bytes() - 1` here is the access's EXTENT, the offset of its last byte, not an alignment
        // mask and not the split charge. It shares a spelling with `split_extra_bytes` and
        // nothing else, so it stays written out rather than adopting either name.
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
    // `PUSH Sreg`'s selector becomes an immediate here and nowhere earlier. This is the first
    // point that holds the `SegmentLayout`, and the constant it bakes is the same one
    // `data_matches` pins on entry, because both read the segment the block was captured under.
    let source = match source {
        StoreSource::Selector(segment) => {
            StoreSource::Imm(u32::from(memory.segments.selector(segment)))
        }
        other => other,
    };
    if memory.one_lookup_store {
        emit_store_fast(e, source, width, addr, memory, sides, wrap);
        return;
    }
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
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
    emit_store_write_resolve(e, width, map, code_watch_tables, memory, sides);
    emit_store_value(e, source, width);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
        // classify only ever produces a GPR-sized `Store` (Byte, Word or Dword); the x87 store
        // forms (StoreF64, StoreI32) route through `emit_x87_slot`, not here.
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    e.jmp(done);

    e.place(mode13);
    emit_store_write_resolve(e, width, map, code_watch_tables, memory, sides);
    emit_store_value(e, source, width);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    emit_mode13_dirty_bit(e, memory.r15_tables, map);
    e.place(done);
}

/// The permission check + write-pointer resolve + code-watch consultation shared by both of
/// `emit_store`'s page-kind arms (and the x87 store pointer), in the two D3 emission shapes.
///
/// With the watched-page bit ON, the hot path RE-READS the flags byte through the page index
/// still live in RCX and skips the full guard when `PAGE_WATCHED` is clear. Re-reading (one
/// imm64 + one L1-hot byte load) rather than carrying the byte across the permission check is
/// deliberate twice over: all four scratch registers are occupied at these sites, and the
/// carry-by-duplication shape this replaced (a watched arm re-running the check + resolve) grew
/// store-dense blocks past the one-host-page install limit — the doom drawcolumn fixture caught
/// it. It is also what makes the cpl3 arm uniform (H3): the byte is re-read AFTER the check
/// destroys RDX, so there is nothing to preserve. With the bit OFF this reproduces the
/// pre-slice sequence exactly.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_store_write_resolve(
    e: &mut Encoder,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_write_permission_check(e, memory.cpl3, sides.permission);
    emit_write_pointer(e, memory.r15_tables, map, sides.unavailable_or_kind);
    if memory.watch_page_bit {
        // RCX is the page index from the kind classify — the permission check never touches it
        // and the pointer resolve only reads it. RDX is dead (the store value loads later, and
        // the guard re-derives its own scratches).
        let unwatched = e.label();
        emit_table_base(
            e,
            memory.r15_tables,
            TABLE_SLOT_FLAGS,
            map.flags(),
            Reg::RDX,
        );
        e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_WATCHED));
        e.jz(unwatched);
        emit_code_watch_branch(
            e,
            width,
            memory.r15_tables,
            map,
            code_watch_tables,
            sides.code_watch,
            unwatched,
        );
        e.place(unwatched);
    } else {
        emit_watched_store_guard(
            e,
            width,
            memory.r15_tables,
            map,
            code_watch_tables,
            sides.code_watch,
        );
    }
}

/// The wide guard's PAGE-CROSSING half: refuse an access whose last byte lands on the next page.
/// Scratch: RDX. Four instructions, and it must be emitted BEFORE the alignment half (see
/// `emit_wide_page_guard`).
///
/// The crossing bound uses `bytes()` (the transaction's actual size), not `alignment_bytes()`
/// (the guard's alignment requirement). For a Byte, Word or Dword access that is also ALIGNED the
/// two are equal and this check cannot fire: an aligned access of size N can never sit within N
/// bytes of the page end. It stopped being dead there the moment the lean one-lookup load and
/// store sites began serving MISALIGNED accesses natively -- a Word at offset 0xFFF or a Dword at
/// 0xFFD-0xFFF really does straddle -- and at those two sites this compare is now the ONLY thing
/// keeping a served access inside the one page its FastMap entry was resolved against.
///
/// It was already LIVE for the two wide widths, both of which have `alignment_bytes() = 4` below
/// their size. A 4-aligned Qword can start as late as offset 0xFFC and its second dword half
/// crosses into the next page. A 4-aligned Tbyte is worse: 10 bytes against a 4-byte alignment
/// refuses everything from 0xFF8 up, which is what keeps `write_extended80`'s trailing word at +8
/// inside the page the pointer was resolved against.
fn emit_page_cross_bound(e: &mut Encoder, width: MemoryWidth, cross: Label) {
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, 0x0fff);
    e.cmp_r32_imm32(Reg::RDX, 0x1000 - width.bytes());
    e.jcc(7, cross);
}

/// The wide guard's ALIGNMENT half. Four instructions, and the only half whose target varies by
/// call site: eleven sites send it to `cross_page_or_alignment` (through `emit_wide_page_guard`),
/// the two lean one-lookup sites send it to their own slow stub instead.
///
/// **`scratch` has exactly two legal values, and passing the wrong one produces a silent wrong
/// answer rather than a fault.**
///
/// * `Reg::RDX` at twelve of the thirteen sites -- the eleven refusing sites plus the lean READ
///   site, where the guard precedes `emit_load_bias_probe` and that pad's contract is "RDX is
///   untouched" (`emit/load_fast.rs`).
/// * `Reg::RCX` at the lean STORE site, and ONLY there, and only because the alignment half is
///   emitted AFTER `emit_read_store_value` has put the store value in RDX. RCX is free by the
///   store pad's own rule: the page index is not part of the stub contract, and every stub that
///   needs it recomputes it from RAX.
///
/// Backwards is undetectable at runtime: RDX at the store site clobbers the value the slow stub
/// spills and stores, and RCX at any other site clobbers a live page index. `and r32, imm32` is
/// `81 /4 id` and `mov r32, r32` is `89 /r` at either register, so both spellings are the same
/// four instructions at the same four encoding lengths.
fn emit_alignment_test(e: &mut Encoder, width: MemoryWidth, scratch: Reg, misaligned: Label) {
    debug_assert!(
        matches!(scratch, Reg::RDX | Reg::RCX),
        "the alignment test's scratch is RDX everywhere except the lean store site, where the \
         value materialisation owns RDX and the scratch must be RCX"
    );
    e.mov_r32_r32(scratch, Reg::RAX);
    e.and_r32_imm32(scratch, width.alignment_mask());
    e.cmp_r32_imm32(scratch, 0);
    e.jnz(misaligned);
}

/// Both halves, both verdicts to one label: the eleven sites that refuse a wide access outright.
///
/// SIZE-identical to the pre-decomposition emission -- eight instructions, two not-taken branches
/// on the aligned path, same verdict, same side exit -- but deliberately NOT byte-identical: the
/// two tests swap positions, so the `and` immediates move and the `jnz` and `ja` trade places.
/// Every branch this emitter produces is a fixed-length near form, so the swap cannot move a
/// block's size either.
///
/// **The crossing bound comes FIRST, and that ordering is load-bearing rather than cosmetic.** At
/// the two relaxed sites the alignment half's target is a local recovery path that serves the
/// access, and a page-CROSSING access must never reach it. Testing the crossing bound first makes
/// that structural instead of a second test inside the recovery. The wrapper keeps the same order
/// so there is one order to reason about, not two.
///
/// **Precondition, stated rather than assumed: no caller may rely on the host flag state this
/// leaves behind.** The reorder changes it -- ZF/CF now come from the crossing `cmp` rather than
/// the alignment one. Every consumer at all thirteen sites establishes its own flags before
/// branching (`emit_load_bias_probe` and `emit_store_bias_probe` both end in a `test`,
/// `emit_ram_read_pointer_inner`'s next flag setter is a shift), and guest EFLAGS live in RBP,
/// never in host flags across a memory front.
fn emit_wide_page_guard(e: &mut Encoder, width: MemoryWidth, side: Label) {
    debug_assert!(width.needs_alignment_guard());
    emit_page_cross_bound(e, width, side);
    emit_alignment_test(e, width, Reg::RDX, side);
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
        // Only the RMW INC/DEC paths call this, and they only ever pass Word or Dword.
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("INC/DEC memory operands are never 8- or 10-byte wide")
        }
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

/// MOVZX and MOVSX, register form. No memory access, no flags on any path, so the lazy-flag
/// descriptor is untouched and there are no side exits.
fn emit_mov_extend_reg(e: &mut Encoder, dst: u8, src: u8, widths: ExtendWidths, signed: bool) {
    let ExtendWidths {
        source: width,
        destination: dst_width,
    } = widths;
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
        (MemoryWidth::Dword | MemoryWidth::Qword | MemoryWidth::Tbyte, _) => {
            unreachable!("MOVZX/MOVSX source width is only ever Byte or Word")
        }
    }
    // The write is the operand size, not the source size -- see `emit_extend_write_back`.
    //
    // dst == src is safe by construction at either destination width, including `movzx eax, ah`
    // and `movzx ax, al`: the whole value is materialised into RDX before the single write, and no
    // guest home is RDX.
    emit_extend_write_back(e, dst, dst_width);
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
            // No x87 memory form reaches `emit_read_store_value`: the m64 forms move through
            // `x87_avx2_emit`'s `vmovsd` arms, not through a GPR home.
            MemoryWidth::Qword | MemoryWidth::Tbyte => {
                unreachable!("GPR-sourced values are never 8- or 10-byte wide")
            }
        },
        // PUSHFD. RBP is the running materialized-EFLAGS shadow, so it is already the value the
        // interpreter's `materialize_flags()` would settle to; the descriptor teardown that must
        // accompany it is emitted by the `Push` arm before this runs, not here, because this
        // helper is shared with sources that must NOT clear it.
        StoreSource::Flags { mask } => {
            e.mov_r32_r32(value, Reg::RBP);
            e.and_r32_imm32(
                value,
                match width {
                    MemoryWidth::Byte => mask & 0xff,
                    MemoryWidth::Word => mask & 0xffff,
                    MemoryWidth::Dword => mask,
                    MemoryWidth::Qword | MemoryWidth::Tbyte => {
                        unreachable!("PUSHFD is never 8- or 10-byte wide")
                    }
                },
            );
        }
        StoreSource::Imm(imm) => e.mov_r32_imm32(
            value,
            match width {
                MemoryWidth::Byte => imm & 0xff,
                MemoryWidth::Word => imm & 0xffff,
                MemoryWidth::Dword => imm,
                MemoryWidth::Qword | MemoryWidth::Tbyte => {
                    unreachable!("GPR-sourced values are never 8- or 10-byte wide")
                }
            },
        ),
        // `emit_store` rewrites this to `Imm` before it can reach here, because that is the only
        // caller with a `SegmentLayout` to resolve it from. Reaching this arm means a second store
        // path grew and did not.
        StoreSource::Selector(segment) => {
            unreachable!("PUSH {segment:?} must be resolved to an immediate in emit_store")
        }
        // The byte `SetCcMem` computed at the top of its slot. Byte width only: the parker is the
        // only producer and it stores a `setcc` result, so a wider read here would pick up the
        // upper bytes of a frame slot nothing defined. The `debug_assert` is the whole check --
        // there is no correct wider behaviour to fall back to.
        StoreSource::ParkedByte => {
            debug_assert!(matches!(width, MemoryWidth::Byte));
            e.load_r64_disp32(value, Reg::RSP, STACK_PUSH_MEM_VALUE);
        }
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
}

fn emit_dynamic_increment(e: &mut Encoder, offset: i8) {
    emit_dynamic_increment_by(e, offset, 1);
}

/// The parameterised form `emit_dynamic_increment` wraps. Only the x87 Qword completion needs
/// `n != 1` today (two dword transactions per m64 access), so the ~15 existing single-increment
/// call sites stay on the thin wrapper above rather than all naming `1` explicitly.
fn emit_dynamic_increment_by(e: &mut Encoder, offset: i8, n: u64) {
    e.mov_r64_imm64(Reg::RDX, n);
    e.add_r64_to_mem_disp8(Reg::RSP, offset, Reg::RDX);
}

fn emit_dynamic_word_increment(e: &mut Encoder, byte_counter_offset: i8) {
    e.mov_r64_imm64(Reg::RDX, 1u64 << 32);
    e.add_r64_to_mem_disp8(Reg::RSP, byte_counter_offset, Reg::RDX);
}

/// Deposit `extra` EXTRA byte cycles -- the ones a misaligned RAM access owes beyond the single
/// wide cycle its static count already charges -- into the HIGH half of `STACK_DWORD_READS`.
///
/// The lane's low half is the block's static dword-read count and the two cannot interact:
/// `emit_add_static_accounting` writes the low half with `mov r32, imm32` plus a 64-bit add and
/// never touches the high half, and `emit_add_repeated_accounting`'s product is a small per-block
/// count times at most `MAX_NATIVE_SELF_LOOP_ITERATIONS`. `run.rs` asserts that beside the unpack.
///
/// **The lane's name lies, and it is worth knowing why that was chosen.** This quantity rides in
/// a lane called "dword reads" and is fed by STORES as well as reads. It is numerically right --
/// `run.rs` prices `ram_byte_writes` through the same `jit_data_cost_clocks(Byte)` as
/// `ram_byte_reads`, so one shared pool of extra byte cycles is exact -- and it costs nothing:
/// the high half was already copied out by `emit_return` as part of a full 64-bit lane, already
/// zeroed by the prologue's vector fill, and had no consumer. `STACK_RAM_DWORD_WRITES` has an
/// equally free high half if a future reader would rather split reads from stores; the single
/// pool is chosen because it is one deposit helper and one clock term.
///
/// Scratch: RDX, like both increment primitives above. Every caller is a STUB tail past the point
/// where the access is committed, so RDX is dead there.
fn emit_dynamic_split_extra(e: &mut Encoder, extra: u32) {
    // Word or Dword. The x87 widths keep refusing misaligned accesses, so 7 and 9 never arrive.
    debug_assert!(extra == 1 || extra == 3);
    e.mov_r64_imm64(Reg::RDX, u64::from(extra) << 32);
    e.add_r64_to_mem_disp8(Reg::RSP, STACK_DWORD_READS, Reg::RDX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_mode13_dirty_bit(e: &mut Encoder, r15_tables: bool, map: NativeMapBases) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        r15_tables,
        TABLE_SLOT_PHYSICAL_PAGES,
        map.physical_pages(),
        Reg::RDI,
    );
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

pub(super) fn gpr_offset(index: usize) -> i32 {
    (core::mem::offset_of!(CpuGsw, registers)
        + core::mem::offset_of!(Registers, gpr)
        + index * core::mem::size_of::<u32>()) as i32
}

/// Byte offset from the `CpuGsw` pointer in R15 to a segment's descriptor.
///
/// `SegmentIndex::index` is the mapping `set_segment` itself uses, so the emitter and the
/// interpreter cannot disagree about which slot they mean. `direct.rs` carries a second copy of
/// the same mapping for `SegmentLayout.data`; the two agree today and the const assertion below
/// keeps them that way, but this uses the one the write path uses.
fn segment_field_base(segment: SegmentIndex) -> i32 {
    (core::mem::offset_of!(CpuGsw, registers)
        + core::mem::offset_of!(Registers, segments)
        + segment.index() * core::mem::size_of::<SegmentRegister>()) as i32
}

fn selector_offset() -> i32 {
    core::mem::offset_of!(SegmentRegister, selector) as i32
}

fn base_offset() -> i32 {
    core::mem::offset_of!(SegmentRegister, base) as i32
}

/// `access` and `default_size_32` are adjacent, and the emitter writes both with one 16-bit
/// store. The assertion is what makes that legal rather than lucky.
fn access_offset() -> i32 {
    const _: () = assert!(
        core::mem::offset_of!(SegmentRegister, default_size_32)
            == core::mem::offset_of!(SegmentRegister, access) + 1,
        "LoadSegReal writes access and default_size_32 as one 16-bit store",
    );
    core::mem::offset_of!(SegmentRegister, access) as i32
}

fn eip_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eip)) as i32
}

pub(super) fn eflags_offset() -> i32 {
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
        }
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
        // Only `emit_alu_mem_dest`'s write path calls this, for a GPR-sourced ALU memory
        // destination, never 8-byte wide.
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("ALU memory-dest operands are never 8- or 10-byte wide")
        }
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
    // The WORD lane. Widened from CMP-only to the whole non-carry op set, with write-back, as the
    // emitter half of admitting `0x83` to `classify`'s OperandSize::Word allowlist.
    //
    // Three properties make this sixteen-bit rather than a masked thirty-two-bit op, and each is
    // one line: the operands are masked to sixteen bits BEFORE the operation so CF and the lazy
    // descriptor's `a`/`b` are sixteen-bit values; the operation is `alu_r16_r16`, a real 66-prefixed
    // instruction, so the host's own CF/OF/SF/ZF are the sixteen-bit ones; and the result is merged
    // back with `mov_r16_r16`, which writes only the destination's low half and preserves its high
    // half exactly as the interpreter's `write_gpr_sized(.., Word, ..)` does. A 32-bit `mov` there
    // would clobber the high half, which is the miscompile the old allowlist existed to prevent.
    //
    // RDX holds the result, zero-extended because RAX was masked before the copy, which is what the
    // lazy evaluator wants for a Word descriptor.
    if matches!(width, MemoryWidth::Word) {
        // Must fail in release too, since the emitter runs in the release JIT, and the failure
        // this guards is silent: the `and` masks below clear host CF, so an ADC that reached here
        // would compute without its carry in and then tag the descriptor as the SUB class.
        assert!(
            !matches!(op, 2 | 3),
            "ADC/SBB take the incoming CF as an operand and have no word lane; classify refuses \
             them at Word size"
        );
        let logic = matches!(op, 1 | 4 | 6);
        e.and_r32_imm32(Reg::RAX, 0xffff);
        e.and_r32_imm32(Reg::RCX, 0xffff);
        e.mov_r32_r32(Reg::RDX, Reg::RAX);
        // CMP is SUB without the write-back, exactly as the Dword lane below maps it.
        let host_op = if op == 7 { 5 } else { op };
        e.alu_r16_r16(host_op, Reg::RDX, Reg::RCX);
        emit_capture_flags(e, if logic { LOGIC_FLAGS } else { ARITH_FLAGS });
        // AFTER the capture: `mov` writes no flags, but putting it before would still be wrong to
        // read as safe, and after is where the Dword lane does its equivalent too.
        if op != 7 {
            e.mov_r16_r16(home(dst), Reg::RDX);
        }
        // Word tag is 0x100 where Dword's is 0x200; the low byte is the operation class (0 add,
        // 1 sub, 2 logic), the same encoding `PendingFlags::op`/`width` decode.
        if logic {
            emit_pending(e, 0x8000_0102, None, None, Reg::RDX);
            // Must follow `emit_pending`: it clobbers RDX, which is the result this just stored.
            emit_logic_live_af(e);
        } else {
            let tag = if op == 0 { 0x8000_0100 } else { 0x8000_0101 };
            emit_pending(e, tag, Some(Reg::RAX), Some(Reg::RCX), Reg::RDX);
        }
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("register INC/DEC is never 8- or 10-byte wide")
        }
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

/// The BYTE-LANE register ALU: `op r8, r8`, both operand orders.
///
/// `emit_alu_byte_imm`'s body with a second register read where it materialises an immediate, and
/// the sharing is deliberate rather than incidental: the interpreter reaches ALU forms 0, 2 and 4
/// through ONE `self.alu(op, a, b, BusWidth::Byte)` call in `execute_alu_decoded`, so the flags,
/// the lazy descriptor and the truncation are the same operation on the same lane whatever the
/// source is. Only where `a` and `b` come from differs, and the two orders are resolved by
/// `classify` before this is reached (form 0 takes the r/m as the destination, form 2 the reg).
///
/// Both operands are read BEFORE anything is written, which is what makes the aliasing cases come
/// out right without a special case: `add al, ah` and `xor ch, ch` name two byte lanes of ONE
/// 32-bit home, and `cmp bl, bl` names one lane twice. The write-back through `emit_write_gpr8`
/// touches only the destination lane's eight bits, exactly as `write_gpr8` does.
///
/// **The ordering inside `emit_alu_byte_preloaded` is `emit_pending` THEN `emit_capture_flags`,**
/// on both its branches and inside `emit_carry_alu_byte` — the descriptor is recorded first and
/// the host flags captured after. That is the opposite of `emit_inc_dec_reg8`, whose comment
/// makes the reverse order load-bearing, and the difference is worth stating rather than glossing:
/// it is safe HERE only because `emit_pending` emits nothing but `mov`s and stores, so it writes
/// no host flag between the `alu_r8_r8` and the capture. Adding anything that sets flags to
/// `emit_pending` — a test, a compare, an add — would silently corrupt every byte ALU result's
/// flags, and this function would be one of the callers it broke. (An earlier version of this
/// comment stated the order backwards; the code has never had it either way but the claim was
/// wrong, and a wrong claim about a flag window is an invitation.)
///
/// `emit_write_gpr8` runs AFTER both, because it shifts its value register in place and would
/// clobber the descriptor's recorded result if it ran before. That hazard IS `emit_inc_dec_reg8`'s
/// verbatim.
///
/// CMP (op 7) suppresses the write-back, matching `write_back = op != 7` in the interpreter's arm.
fn emit_alu_reg_byte(e: &mut Encoder, op: u8, dst: u8, src: u8) {
    emit_read_store_value(e, StoreSource::Reg(dst), MemoryWidth::Byte, Reg::RAX);
    emit_read_store_value(e, StoreSource::Reg(src), MemoryWidth::Byte, Reg::RCX);
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

/// IMUL r32, r/m32, imm — the three-operand form, register source (0x69 and 0x6B).
///
/// `dst = src * imm`, which is one host instruction because the host has the same three-operand
/// encoding with the same truncation and the same CF/OF rule. The flag tail is `emit_imul`'s
/// verbatim and correct for the same reason: the interpreter reaches BOTH the two-operand and the
/// three-operand form through one `imul_truncated` (core.rs), which ends in
/// `set_flag(FLAG_CF | FLAG_OF, significant)`. That mask has more than one bit set, so it cannot
/// take the single-bit CF-override shortcut and instead materializes whatever was pending before
/// writing just those two bits -- leaving SF/ZF/AF/PF exactly as they were.
///
/// Unlike `emit_imul` this reads a source it does not write, and no shuffle is needed for it:
/// every operand is a `GUEST_HOMES` register (R8-R14 plus RBX), the host instruction reads `src`
/// and writes `dst` in one go, and `src == dst` is the ordinary in-place multiply the guest
/// encoding permits.
fn emit_imul_imm(e: &mut Encoder, dst: u8, src: u8, imm: u32) {
    e.imul_r32_r32_imm32(home(dst), home(src), imm);
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

/// IMUL r/m32, one-operand SIGNED multiply, register form (0xF7 /5).
///
/// `emit_mul_reg`'s body with the SIGNED primitive, and the two are written out separately for
/// the reason `mul_r32` and `imul_r32` are: the overflow rules differ (the full product failing
/// to sign-extend back from the low half, against the high half being nonzero), so a shared body
/// parameterised on the sub-opcode would make picking the wrong one a one-character edit.
///
/// The operand shuffle carries the same `src == 0` / `src == 2` argument `emit_mul_reg` states:
/// the multiplicand is read out of its home AFTER RAX is loaded and BEFORE either home is
/// written, and no home is RAX or RDX, so a `IMUL EAX` or an `IMUL EDX` still reads the
/// pre-instruction value.
fn emit_imul_reg_acc(e: &mut Encoder, src: u8) {
    e.mov_r32_r32(Reg::RAX, home(0));
    e.imul_r32(home(src));
    e.mov_r32_r32(home(0), Reg::RAX);
    e.mov_r32_r32(home(2), Reg::RDX);
    emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

/// DIV (0xF7 /6) and IDIV (0xF7 /7) r/m32, register form.
///
/// # Keeping #DE out of the host
///
/// This is the only lowering whose guest instruction has an architectural fault, and the fault is
/// raised by the HOST divide instruction rather than by a guard we choose to run. A guest divide
/// that reached a host `div` with a zero divisor would raise a host #DE on the JIT stack --
/// SIGFPE / EXCEPTION_INT_DIVIDE_BY_ZERO inside the emulator process, not a guest fault. So the
/// whole design is: prove the host divide cannot fault, then run it.
///
/// The guard may be CONSERVATIVE but never permissive. A side exit ends the native run AT this
/// instruction with nothing done, and the interpreter re-executes it whole -- raising #DE by its
/// own rules if that is what the guest's operands call for. So the guard has to be a SUFFICIENT
/// condition for the host divide's safety, and only its cost argues for making it tight.
///
/// ## DIV (/6): the guard is EXACTLY the interpreter's fault set
///
/// `cmp edx, ecx; jae exit` before a 32-bit `div ecx` covers both of `CpuGsw::div`'s unsigned
/// error returns and nothing else:
///
/// * divisor zero -- `edx >= 0` is unsigned-true for every EDX, so a zero divisor ALWAYS exits.
///   The zero test is subsumed rather than omitted.
/// * quotient overflow -- the interpreter's rule is `EDX:EAX / divisor > 0xffff_ffff`, and that
///   holds if and only if `EDX >= divisor`. Forward: `EDX >= divisor` gives
///   `EDX*2^32 + EAX >= divisor*2^32`, so the quotient is at least 2^32. Backward:
///   `EDX <= divisor - 1` gives `EDX*2^32 + EAX <= (divisor-1)*2^32 + (2^32 - 1)
///   = divisor*2^32 - 1 < divisor*2^32`, so the quotient is at most 2^32 - 1.
///
/// It is also the condition the host `div` itself faults on, which is the point.
///
/// ## IDIV (/7): divide at SIXTY-FOUR bits, then compare
///
/// A host 32-bit `idiv` faults on exactly the quotient-overflow case the guest defines, and there
/// is no cheap exact predicate for it on the operands. At 64 bits there is no predicate to find:
/// with `|divisor| >= 2` the quotient is at most `|dividend| / 2 <= 2^62`, so `idiv rcx` cannot
/// overflow, and the guest's own 32-bit range rule becomes a COMPARISON on the answer. That is
/// also a transliteration of the interpreter, which computes `i64 / i64` and then range-checks --
/// so the quotient and the remainder agree by construction rather than by a re-derivation.
///
/// Three guards, in order:
///
/// 1. `divisor == 0` -- the interpreter's first error return, and the host's first fault.
/// 2. `divisor == -1` -- CONSERVATIVE, and the only place this function refuses more than the
///    interpreter faults on. It exists to remove `i64::MIN / -1`, the sole remaining 64-bit
///    overflow, without a 64-bit immediate compare against the dividend. What it costs is a side
///    exit on a legal `IDIV` by -1, which is a negation nothing emits; `side_exit_divide_guard`
///    is the always-on evidence for whether that stays true, and if it ever does not, the exact
///    form is `divisor == -1 && dividend == i64::MIN`.
/// 3. quotient outside `i32` -- the interpreter's range check, done after the divide as
///    `movsxd rcx, eax; cmp rax, rcx`. The divide has run by then, but it has written only RAX
///    and RDX, which are emitter scratch, so the exit still leaves the instruction un-started.
///
/// # Flags
///
/// Both forms leave the guest's arithmetic flags ARCHITECTURALLY UNDEFINED, and `CpuGsw::div`
/// implements that by touching neither `eflags` nor `pending_flags` -- so this emits no flag
/// handling at all. That is not an omission: capturing the host divide's flags, or clearing the
/// pending descriptor, would each diverge from the interpreter on a fixture that reads a lazy
/// flag produced BEFORE the divide. The host divide does write host EFLAGS, which is harmless
/// because the guest shadow is RBP and no block carries host flags across a slot boundary.
///
/// # A guard exit at the block's ENTRY slot, and what stops it looping
///
/// Recorded because it is the one shape of this exit that is not obviously terminating, and
/// because what saves it is machinery that predates this slice and could be "tidied" away by
/// someone who does not know anything depends on it.
///
/// A `DivReg` really can be a block's FIRST slot in production -- nothing in `classify` or the
/// compile loop prevents it, and `compile_leading_block(&[0xf7, 0xf3])` builds exactly that. A
/// guard exit there retires ZERO instructions and leaves EIP where it entered, so on its own it
/// would re-dispatch the same block, refuse again, and never make progress.
///
/// TWO independent mechanisms stop that, and BOTH are needed because each covers a case the
/// other does not:
///
/// * **Mid-run.** The exit reports `DirectBlockOutcome::Prefix` (run.rs, the `if side_exit` tail
///   of `run_direct_block`), which becomes `DirectContinuation::Prefix` and sets
///   `direct_runtime.skip_direct_once`. The next loop iteration's admission gate consumes that
///   latch with a `mem::take` and returns `ContinuationDispatch::Skipped`, so the INTERPRETER
///   executes the divide -- and raises #DE if the operands call for it.
/// * **At a run boundary.** `run_budgeted_inner` clears `skip_direct_once` on entry, so a guard
///   exit that ends a run does not carry its latch forward. It does not need to: that loop's
///   `first` flag always interprets a run's first instruction, and the Direct dispatcher is only
///   consulted on continuations.
///
/// This is not new with DIV -- any slot-0 side exit has the same shape, and a memory guard on an
/// entry-slot `Load` reaches it today. What IS new is that this is the first exit reason that is
/// a pure property of the DATA with no address component, so it can fire on a block that will
/// never bind differently, arbitrarily often. That makes the liveness argument worth stating
/// where the guard is rather than leaving it implicit in the run loop.
fn emit_div_reg(e: &mut Encoder, src: u8, signed: bool, guard: Label) {
    if signed {
        // The divisor first and SIGN-extended: it is the one operand whose width changes.
        e.movsxd_r64_r32(Reg::RCX, home(src));
    } else {
        e.mov_r32_r32(Reg::RCX, home(src));
    }
    emit_div_preloaded(e, signed, guard);
    emit_div_write_back(e);
}

/// The divide itself, with the divisor already staged in RCX -- sign-extended to 64 bits for the
/// signed form, zero-extended (a plain `mov r32, r32`) for the unsigned one.
///
/// Split out of `emit_div_reg` so the MEMORY form shares one body rather than a transcription of
/// it, and split at the DIVISOR because that is the only thing the two operand forms disagree
/// about: the dividend is EDX:EAX either way, the guards are the same three (or one) tests, and
/// the quotient/remainder land in the same two registers. The split is byte-neutral for the
/// register form -- the instructions below are emitted in exactly the order they were when this
/// was one function, which is what keeps the gate-off build identical to main.
///
/// Leaves the quotient in RAX and the remainder in RDX. It does NOT write them back to the guest
/// homes: `emit_div_mem` has one more thing to do (the deferred mode-13 read completion) between
/// the last guard and the point where the frame may be disturbed, so the write-back is the
/// caller's, through `emit_div_write_back`.
fn emit_div_preloaded(e: &mut Encoder, signed: bool, guard: Label) {
    // 0x3 is the x86 above-or-equal / not-carry condition, 0x4 is zero, 0x5 is not-zero.
    const JAE: u8 = 0x3;
    const JE: u8 = 0x4;
    const JNE: u8 = 0x5;
    if signed {
        // EDX:EAX assembled into RAX as one 64-bit dividend. `mov r32, r32` zero-extends, so the
        // two halves compose with a shift and an OR rather than needing a mask.
        e.mov_r32_r32(Reg::RAX, home(0));
        e.mov_r32_r32(Reg::RDX, home(2));
        // ModRM /4 is SHL.
        e.shift_r64_imm8(4, Reg::RDX, 32);
        e.or_r64_r64(Reg::RAX, Reg::RDX);
        // Guard 1 and guard 2. `cmp_r64_imm32` sign-extends its immediate to 64 bits, so
        // `u32::MAX` here is the 64-bit -1 and not 0x0000_0000_ffff_ffff.
        e.cmp_r64_imm32(Reg::RCX, 0);
        e.jcc(JE, guard);
        e.cmp_r64_imm32(Reg::RCX, u32::MAX);
        e.jcc(JE, guard);
        // RDX still holds the shifted high half; `cqo` overwrites it with the dividend's sign,
        // which is why the assembly above had to finish first.
        e.cqo();
        e.idiv_r64(Reg::RCX);
        // Guard 3. RCX is free here -- the divisor has been consumed -- and the divide has
        // written only RAX and RDX, so this exit is still pre-effect.
        e.movsxd_r64_r32(Reg::RCX, Reg::RAX);
        e.cmp_r64_r64(Reg::RAX, Reg::RCX);
        e.jcc(JNE, guard);
    } else {
        e.mov_r32_r32(Reg::RAX, home(0));
        e.mov_r32_r32(Reg::RDX, home(2));
        // The whole unsigned guard, both fault conditions: see the derivation above.
        e.alu_r32_r32(7, Reg::RDX, Reg::RCX);
        e.jcc(JAE, guard);
        e.div_r32(Reg::RCX);
    }
}

/// Quotient to EAX, remainder to EDX, exactly as `CpuGsw::div`'s Dword arms write them. The
/// signed form's `edx` is the low half of a 64-bit remainder whose magnitude is below the
/// divisor's, so the truncation is the interpreter's own `remainder as u32`.
///
/// Separate from `emit_div_preloaded` so the memory form can put the deferred mode-13 read
/// completion AFTER this and still have every guard exit land before both. This is the point of no
/// return for the slot: past it the instruction has committed guest state and no exit may be taken.
fn emit_div_write_back(e: &mut Encoder) {
    e.mov_r32_r32(home(0), Reg::RAX);
    e.mov_r32_r32(home(2), Reg::RDX);
}

/// DIV / IDIV r/m32, MEMORY form (`0xF7 /6`, `/7`), behind `IZARRAVM_FPU_LOOP_ROWS`.
///
/// # The ordering, which is the whole of this function
///
/// `DivReg`'s comment named the hazard as "two independent side-exit reasons at the same slot".
/// The reason the second one cannot simply follow the first is NOT the exits, it is the mode-13
/// read counter between them. `emit_ram_read_pointer` deposits `mode13_dword_reads` into the frame
/// before it returns, `emit_return` copies that lane out on EVERY exit including a side exit, and
/// a side exit re-runs the whole instruction in the interpreter -- so a divide-guard exit taken
/// after the deposit charges one guest read twice. That is a guest-visible bus-accounting error,
/// the same class `dynamic_counter_fields` refuses to make maskable.
///
/// So this uses the DEFERRED completion shape `Ret` and `JmpMem` already use for their CS-limit
/// exit: `emit_ram_read_pointer_inner`, which parks the page kind and moves no counter, then every
/// guard, then the commit, then `emit_mode13_read_completion` last. In order:
///
/// 1. the address, alignment and page-kind guards (`_inner`) -- exit, nothing deposited;
/// 2. the divisor load out of RDI, and its sign extension for IDIV;
/// 3. the divide guards, including IDIV's post-divide quotient-range one -- exit, still nothing
///    deposited, and still pre-effect because the divide has written only RAX and RDX;
/// 4. `emit_div_write_back`, the commit;
/// 5. `emit_mode13_read_completion`, which clobbers RCX and RDX and is why 4 comes before it.
///
/// # Why the divisor is re-extended rather than loaded sign-extended
///
/// Two instructions (`load_r32_disp8` then `movsxd_r64_r32` on the same register) instead of one
/// `movsxd r64, m32` the encoder does not have. Adding that primitive for one call site would be a
/// second thing to test for no saving that any counter can see.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_div_mem(
    e: &mut Encoder,
    addr: DirectAddr,
    signed: bool,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    guard: Label,
) {
    emit_ram_read_pointer_inner(
        e,
        MemoryWidth::Dword,
        addr,
        memory,
        sides,
        memory.address_wrap,
    );
    e.load_r32_disp8(Reg::RCX, Reg::RDI, 0);
    if signed {
        e.movsxd_r64_r32(Reg::RCX, Reg::RCX);
    }
    emit_div_preloaded(e, signed, guard);
    emit_div_write_back(e);
    emit_mode13_read_completion(e, MemoryWidth::Dword);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_div_mem(
    _: &mut Encoder,
    _: DirectAddr,
    _: bool,
    _: MemoryEmitContext,
    _: MemorySideExits,
    _: Label,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

/// SAHF (0x9E), behind `IZARRAVM_FPU_LOOP_ROWS`.
///
/// The interpreter is `materialize_flags()` then
/// `eflags = (eflags & !0xd5) | (ah & 0xd5) | 0x02`, and RBP is the running materialized shadow --
/// so the settle is already done and what is left is the mask-merge, the publish and the
/// descriptor teardown. That is the same three-step tail `emit_rotate_reg`'s count-1 branch runs,
/// for the same stated reason: with a live descriptor the net eflags is the materialized word with
/// this instruction's bits replaced, which is exactly RBP after the merge; with no descriptor live
/// `materialize_flags` is a no-op and RBP already equals eflags.
///
/// `0xd5` is CF|PF|AF|ZF|SF and is a strict subset of `ARITH_FLAGS`. **OF is the sixth member and
/// is deliberately NOT in the mask**: SAHF preserves it, and widening the mask to `ARITH_FLAGS`
/// would clear it whenever AH's bit 11 happened to be clear -- silently, because AH's bit 11 does
/// not exist.
///
/// AH is bits 8..15 of `home(0)`, read the way `emit_read_store_value`'s byte arm reads any high
/// lane. RAX and RDX are emitter scratch and no guest home is either, so `home(0)` cannot alias
/// them even when the guest is using EAX.
fn emit_sahf(e: &mut Encoder) {
    const SAHF_MASK: u32 =
        crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_AF | crate::FLAG_ZF | crate::FLAG_SF;
    // AH into RDX, zero-extended.
    e.mov_r32_r32(Reg::RDX, home(0));
    e.shift_r32_imm8(5, Reg::RDX, 8);
    e.and_r32_imm32(Reg::RDX, SAHF_MASK);
    e.and_r32_imm32(Reg::RBP, !SAHF_MASK);
    e.or_r32_r32(Reg::RBP, Reg::RDX);
    // The interpreter's trailing `| 0x02`. Bit 1 is expected to be set on every path that writes
    // eflags already, but it is reproduced rather than relied on for `emit_set_cf_only`'s reason:
    // `Registers` derives `PartialEq` and the campaign compares the raw eflags field.
    e.or_r32_imm32(Reg::RBP, 0x2);
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
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("TEST operands are never 8- or 10-byte wide")
        }
    }
    emit_capture_flags(e, LOGIC_FLAGS);
    emit_pending(
        e,
        match width {
            MemoryWidth::Byte => 0x8000_0002,
            MemoryWidth::Word => 0x8000_0102,
            MemoryWidth::Dword => 0x8000_0202,
            MemoryWidth::Qword | MemoryWidth::Tbyte => {
                unreachable!("TEST operands are never 8- or 10-byte wide")
            }
        },
        None,
        None,
        Reg::RDX,
    );
    emit_logic_live_af(e);
}

/// Group-2 immediate shift (0xC1 /4../7, 0xD1 /4../7), register destination, Word or Dword.
///
/// **The two widths differ in ONE instruction and nothing else, and that is the finding rather
/// than a convenience.** Everything below the shift -- which flags are `defined`, the eager
/// publish to `eflags`, the `emit_clear_pending` -- is width-invariant because
/// `CpuGsw::shift_rotate` routes both widths through the same `set_shift_result_flags`, which
/// materializes and writes live at either width. Only the host shift itself has to narrow, and a
/// 66-prefixed `C1 /op` narrows all four flag derivations at once:
///
/// | flag | guest at Word (`shift_rotate` + `set_shift_result_flags`) | `66 C1 /op` |
/// |---|---|---|
/// | CF | the last bit shifted out: bit 15 for SHL/SAL, bit 0 for SHR/SAR | same, from the 16-bit operand |
/// | OF | only at a masked count of 1: SHL `msb(result) ^ CF` with msb at bit 15; SHR `msb(ORIGINAL)`; SAR always 0 | same, and the host leaves OF undefined above count 1 exactly where the interpreter leaves the previous value in place |
/// | SF | bit 15 of the result | same |
/// | ZF / PF | the 16-bit result; PF over its low byte | same |
/// | AF | untouched at both widths -- `set_shift_result_flags` re-writes the value it read | not in `defined`, so RBP's AF survives |
///
/// A masked-Dword lowering gets every one of those wrong: CF from bit 31, SF from bit 31, ZF over
/// 32 bits, and SAR shifting in zeros where the guest shifts in bit 15.
///
/// **A count of 0 emits NOTHING, at both widths, and that is load-bearing.** `shift_rotate`
/// returns before touching the value or a flag ("a zero count affects no flags at all"), so no
/// flag moves, no descriptor is created and no live descriptor is destroyed -- and the
/// write-back the interpreter still performs is `write_gpr16(dst, value & 0xffff)` over the value
/// it just read, i.e. the identity. Emitting the host shift with a count of 0 would be wrong in
/// the other direction only for the flags, but emitting the `defined` merge below would publish
/// RBP to `eflags` and clear a descriptor the guest keeps.
///
/// The five-bit mask is applied HERE rather than passed through, so the `count == 0` and
/// `count == 1` shapes are selected on the architectural count. `classify` stores the immediate
/// raw, so testing `raw_count` would misread `shl ax, 32` as a shift by 32 instead of the no-op
/// it is.
///
/// **Counts of 16 to 31 at Word are architecturally UNDEFINED for a 16-bit operand** (Intel
/// documents results and flags as undefined once the count reaches the operand size) and are
/// still lowered, deliberately. The reference this tree matches is its own interpreter, and the
/// two agree across the whole range: a sweep of SHL/SHR/SAR over ten operand values and every
/// count 1..=31 on this host reproduced `shift_rotate`'s single-bit loop -- result, CF, OF, SF,
/// ZF and PF -- with zero mismatches. `word_shifts_match_the_interpreter_for_every_count` pins
/// that as a test rather than an assumption, so a host that disagreed would fail the suite loudly
/// instead of miscompiling quietly.
fn emit_shift(e: &mut Encoder, op: u8, dst: u8, raw_count: u8, width: MemoryWidth) {
    debug_assert!(
        matches!(op, 4..=7),
        "emit_shift is the SHIFT lane; rotates route to emit_rotate_reg"
    );
    let count = raw_count & 0x1f;
    if count == 0 {
        return;
    }
    match width {
        // The Byte lane returns rather than falling through, because it has a write-back to place
        // AFTER the flag publish and the shared tail cannot express that ordering.
        MemoryWidth::Byte => {
            emit_shift_reg8(e, op, dst, count);
            return;
        }
        MemoryWidth::Word => e.shift_r16_imm8(op, home(dst), count),
        MemoryWidth::Dword => e.shift_r32_imm8(op, home(dst), count),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("group-2 immediate shifts reach the emitter only at Byte, Word or Dword")
        }
    }
    emit_commit_shift_flags(e, count);
}

/// SHL r8, imm8 (0xC0 /4). The masked count is already known non-zero; `emit_shift` owns that
/// case for every width so the no-op shape cannot diverge between the lanes.
///
/// Modelled on `emit_inc_dec_reg8` rather than on the body above, and for the same reason: `dst`
/// is a BYTE-register index where 4..7 name AH/CH/DH/BH, so `home(dst)` would reach the guest EBP,
/// ESI or EDI home and shift the wrong register by 32 bits. The read/modify/write-back through
/// `emit_read_store_value` and `emit_write_gpr8` touches exactly the destination lane's eight
/// bits, which is `write_gpr8`'s contract and the one the interpreter's
/// `write_operand_sized(.., Byte, ..)` reaches.
///
/// Two orderings are each a silent divergence if broken, and they are `emit_inc_dec_reg8`'s
/// verbatim:
///   - `emit_commit_shift_flags` runs BEFORE `emit_write_gpr8`, because that helper's `shl`, `and`
///     and `or` clobber the host flags the capture is reading;
///   - `emit_write_gpr8` runs after, because it also shifts its value register IN PLACE, and a
///     later reader of RDX would see the lane-positioned copy rather than the byte result.
///
/// The arithmetic is a genuine 8-bit `shl`, not a 32-bit shift of the zero-extended byte. That is
/// what makes the flags the host's rather than something this function has to reconstruct:
/// `shift_rotate` at `BusWidth::Byte` takes CF from bit 7, SF from bit 7 and ZF/PF from the 8-bit
/// result, which an 8-bit host shift does by construction and a 32-bit one gets wrong in all
/// three. `0x80 shl 1` is the shortest witness -- 8 bits gives 0x00 with CF set and ZF set, 32
/// bits gives 0x100 with CF clear and ZF clear.
///
/// **Counts of 8 to 31 are lowered too, and that part is MEASURED rather than derived.** x86 masks
/// the count to five bits at every operand size, so a byte shift by 8 or more shifts the operand
/// entirely away, and the SDM leaves CF undefined once the count reaches the operand width. The
/// reference this tree matches is its own interpreter, not the manual: a 48-case host probe over
/// this lane's seeds and counts reproduced `shift_rotate`'s single-bit loop exactly -- result, CF,
/// OF, SF, ZF and PF -- and `BYTE_SHIFT_COUNTS` pins 8 and 31 as cases so a host that disagreed
/// would fail the suite loudly instead of miscompiling quietly. That is the same argument, and the
/// same kind of evidence, that `emit_shift`'s Word lane makes for its counts of 16 to 31.
fn emit_shift_reg8(e: &mut Encoder, op: u8, dst: u8, count: u8) {
    debug_assert_eq!(op, 4, "only SHL r8 (0xC0 /4) has a byte lane");
    debug_assert!(count != 0, "emit_shift owns the no-op count");
    emit_read_store_value(e, StoreSource::Reg(dst), MemoryWidth::Byte, Reg::RDX);
    e.shift_r8_imm8(op, Reg::RDX, count);
    emit_commit_shift_flags(e, count);
    emit_write_gpr8(e, dst, Reg::RDX);
}

/// Publish a group-2 SHIFT's flags: the four it always defines, plus OF at a masked count of 1.
///
/// Shared by all three width lanes because the interpreter shares them -- `set_shift_result_flags`
/// is reached once from `shift_rotate`'s `matches!(op, 4..=7)` branch with an `of` of `None` above
/// count 1, and its `None` arm re-reads the CURRENT OF, i.e. preserves it. Publishing the whole
/// RBP shadow reproduces that: `emit_capture_flags` merges only the `defined` bits, so OF above
/// count 1 and AF at every count keep their pre-shift values in the shadow and are republished
/// unchanged.
///
/// This is the licence a ROTATE does not have, which is why `emit_rotate_reg` does not call it.
fn emit_commit_shift_flags(e: &mut Encoder, count: u8) {
    // The same const the lane form merges under its runtime branch, so the two cannot drift into
    // defining different flag sets for the same instruction at the same count.
    let mut defined = SHIFT_DEFINED;
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

/// ROL and ROR r32, imm8 (0xC1 and 0xD1, sub-opcodes /0 and /1), register destination.
///
/// `op` is the guest ModRM `reg` field and goes STRAIGHT into `shift_r32_imm8`'s `/op` slot: host
/// group 2 numbers ROL 0 and ROR 1 exactly as the guest does, so there is no translation to get
/// wrong. Everything below this line is direction-independent, which is the whole reason the two
/// sub-opcodes share one function -- the flag contract, not the rotate, is what the code is for.
fn emit_rotate_reg(e: &mut Encoder, op: u8, dst: u8, raw_count: u8) {
    debug_assert!(matches!(op, 0 | 1), "only ROL and ROR reach this emitter");
    // The five-bit mask is applied HERE, on the raw decoded immediate, the same way emit_shift
    // does it. classify stores the immediate unmasked, so selecting the shape below on the raw
    // byte would misread `ror eax, 32` as a fourth case instead of the no-op it is.
    let count = raw_count & 0x1f;
    if count == 0 {
        // `shift_rotate` returns before touching the value or any flag, and the interpreter's
        // write-back stores the unchanged value into the register. A genuine no-op.
        return;
    }
    // Both host rotates agree with the interpreter by construction, and BOTH halves of that claim
    // have to be read off `shift_rotate` rather than off the manual, because the manual calls OF
    // undefined above count 1 and this tree's oracle is the interpreter.
    //
    // CF is the bit rotated across the boundary -- out of the MSB for ROL, out of bit 0 for ROR --
    // which is what the loop leaves in `cf` for either direction. At count 1 the interpreter's OF
    // arm splits by op: `0 | 2 => top ^ cf` for a left rotate and `1 | 3 => top ^ (bit 30 of the
    // result)` for a right one. Those are the SAME two definitions x86 gives ROL and ROR, so the
    // host computes each one for us and the split never appears in this function.
    e.shift_r32_imm8(op, home(dst), count);
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

/// The flags a group-2 SHIFT always defines, at every width and every non-zero count. OF joins
/// them at a masked count of exactly 1 and nowhere else.
///
/// Shared by the baked path (`emit_commit_shift_flags`, which adds OF from a compile-time count)
/// and the lane path (`emit_shift_lane`, which adds it under a runtime branch), because the two
/// must define the same set for the same instruction at the same count — the lane form's whole
/// claim is that it changes where the count comes from and nothing else.
const SHIFT_DEFINED: u32 = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;

/// The COUNT-LANE form of ROL/ROR r32 (`0xC1 /0`, `/1`): the count byte is read out of guest RAM
/// on every execution instead of being baked, so a guest patch of it keeps the block.
///
/// # Why this is a branch and not a swapped operand
///
/// `emit_rotate_reg`'s correctness argument is a COMPILE-TIME split on the masked count, and the
/// three shapes are not variations on one flag update — they are three different contracts:
///
/// * **0**: `shift_rotate` returns before touching the value or a flag. No flag moves, no
///   descriptor is created, and no live descriptor is destroyed. This is the shape a "conservative"
///   publish gets WRONG rather than approximately right: publishing RBP here would commit whatever
///   the last deferred op left in the shadow AND clear a descriptor the guest still owns.
/// * **1**: capture `CF|OF`, publish RBP to `eflags`, clear the descriptor. See `emit_rotate_reg`'s
///   two-`set_flag`-calls paragraph for why that is exactly what the interpreter settles to.
/// * **2..31**: capture CF alone and route it through `emit_set_cf_only`, because a rotate
///   architecturally PRESERVES SF, ZF, PF and AF and must not publish the shadow wholesale.
///
/// So the lane form carries the split as a RUNTIME three-way branch over the loaded byte. This is
/// the cost `rotate_rows_enabled`'s "THE DESIGN COST" paragraph priced, and it is why this family
/// was not in L2 arm 1 beside the flag-neutral `0x80` byte ALU.
///
/// # The ordering rules, each of which is a silent divergence if broken
///
/// * **The mask comes first.** `& 0x1f` is applied to the LOADED byte before any shape test, for
///   the reason `emit_rotate_reg` masks `raw_count`: the guest count is architecturally five bits,
///   so a patched `0x20` is the no-op shape and a patched `0x21` is the count-1 shape. Selecting on
///   the raw byte would misread both.
/// * **The test comes BEFORE the rotate, and each arm carries its own rotate.** A `cmp` placed
///   after the host rotate would destroy the very flags the capture is about to read. Duplicating
///   the rotate is what buys a flag capture that is still the host's own answer.
/// * **The count-0 arm jumps clear of everything**, including both flag paths. It emits no rotate,
///   which is right in its own terms too: the interpreter's write-back at count 0 is the identity.
///
/// # Registers
///
/// RDX stages the lane's host pointer and RCX the masked count, both emitter scratch (`GUEST_HOMES`
/// is R8-R14 plus RBX), so neither can alias `home(dst)` — not even when `dst` is guest ECX or EDX.
/// Nothing between the mask and the last read of RCX writes it: `rol r32, cl` only READS CL, and
/// `emit_capture_flags`/`emit_set_cf_only`/`emit_clear_pending` work in RAX, RDI and RBP.
fn emit_rotate_reg_lane(e: &mut Encoder, op: u8, dst: u8, lane: ImmLane) {
    debug_assert!(matches!(op, 0 | 1), "only ROL and ROR reach this emitter");
    debug_assert_eq!(u32::from(lane.width), IMM8_LANE_WIDTH);
    let one = e.label();
    let done = e.label();
    e.mov_r64_imm64(Reg::RDX, lane.host as u64);
    e.movzx_r32_byte_disp32(Reg::RCX, Reg::RDX, 0);
    e.and_r32_imm32(Reg::RCX, 0x1f);
    e.jz(done);
    e.cmp_r32_imm32(Reg::RCX, 1);
    e.jz(one);
    // Counts 2 through 31: CF only, in place, exactly as `emit_rotate_reg`'s tail does it. The
    // host rotate's OF is deliberately NOT captured — above count 1 it is undefined on x86 and the
    // guest's OF is still owned by the descriptor.
    e.shift_r32_cl(op, home(dst));
    emit_capture_flags(e, crate::FLAG_CF);
    emit_set_cf_only(e);
    e.jmp(done);
    e.place(one);
    // Count 1: the imm-form host rotate, byte-identical to what the baked emitter picks at this
    // count, then the `CF|OF` capture and the eager publish.
    e.shift_r32_imm8(op, home(dst), 1);
    emit_capture_flags(e, crate::FLAG_CF | crate::FLAG_OF);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
    e.place(done);
}

/// The COUNT-LANE form of the group-2 SHIFTS: `0xC1 /4..=7` at Dword and `0xC0 /4` at Byte.
///
/// The same runtime three-way branch `emit_rotate_reg_lane` carries, over the same masked loaded
/// byte, and for the same reason — but the two upper arms differ only in ONE bit of the `defined`
/// mask rather than in which flag path they take, because `emit_commit_shift_flags` is shared by
/// every count above zero and adds `FLAG_OF` at exactly count 1. So the tail (the publish, the
/// descriptor clear, and at Byte the write-back) is emitted once and both arms fall into it, which
/// is `emit_shift_cl`'s compactness argument applied here: that slice was reverted once for
/// inlining a full merge into both arms.
///
/// **Count 0 still jumps clear of everything**, and at Byte that includes the WRITE-BACK. Skipping
/// it is not an optimisation of an identity store: `emit_write_gpr8` shifts its value register in
/// place and rewrites the destination's lane, so running it over a value the count-0 path never
/// read would write scratch into a guest register.
///
/// **Word is unreachable because `count_lane_for` BARS IT ON THE KIND'S WIDTH**, and that sentence
/// is written this way because the first version of this slice made the weaker claim — that the
/// prefix bar and `len == 3` refused Word already, since a Word `0xC1` needs a `0x66`. In a
/// 16-bit code segment the operand size follows CS.D, so `c1 e0 03` is an unprefixed three-byte
/// `shl ax, 3` at Word, and the arm below was reached and PANICKED THE COMPILER on ordinary DOS
/// code. The width bar is now explicit at the admission site, and
/// `a_word_group_two_shift_in_a_sixteen_bit_segment_takes_no_count_lane` is the regression
/// fixture. That is what makes `shift_r16_cl` a helper this tree does not owe.
fn emit_shift_lane(e: &mut Encoder, op: u8, dst: u8, width: MemoryWidth, lane: ImmLane) {
    debug_assert!(
        matches!(op, 4..=7),
        "emit_shift_lane is the SHIFT lane; rotates route to emit_rotate_reg_lane"
    );
    debug_assert_eq!(u32::from(lane.width), IMM8_LANE_WIDTH);
    let one = e.label();
    let merge = e.label();
    let done = e.label();
    e.mov_r64_imm64(Reg::RDX, lane.host as u64);
    e.movzx_r32_byte_disp32(Reg::RCX, Reg::RDX, 0);
    e.and_r32_imm32(Reg::RCX, 0x1f);
    e.jz(done);
    match width {
        // The byte lane's read/modify/write-back through `emit_read_store_value` and
        // `emit_write_gpr8` is `emit_shift_reg8`'s verbatim, and so is the ordering it enforces:
        // the flag publish runs BEFORE the write-back, because that helper's `shl`, `and` and `or`
        // clobber the host flags the capture reads. RDX is re-used as the value register once the
        // count is safely in RCX; `dst` is a BYTE-register index, so `home(dst)` would reach the
        // wrong register for indices 4..7 and the helpers exist precisely to avoid it.
        MemoryWidth::Byte => {
            debug_assert_eq!(op, 4, "only SHL r8 (0xC0 /4) has a byte lane");
            emit_read_store_value(e, StoreSource::Reg(dst), MemoryWidth::Byte, Reg::RDX);
            e.cmp_r32_imm32(Reg::RCX, 1);
            e.jz(one);
            e.shift_r8_cl(op, Reg::RDX);
            emit_capture_flags(e, SHIFT_DEFINED);
            e.jmp(merge);
            e.place(one);
            e.shift_r8_imm8(op, Reg::RDX, 1);
            emit_capture_flags(e, SHIFT_DEFINED | crate::FLAG_OF);
            e.place(merge);
            e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
            emit_clear_pending(e);
            emit_write_gpr8(e, dst, Reg::RDX);
        }
        MemoryWidth::Dword => {
            e.cmp_r32_imm32(Reg::RCX, 1);
            e.jz(one);
            e.shift_r32_cl(op, home(dst));
            emit_capture_flags(e, SHIFT_DEFINED);
            e.jmp(merge);
            e.place(one);
            e.shift_r32_imm8(op, home(dst), 1);
            emit_capture_flags(e, SHIFT_DEFINED | crate::FLAG_OF);
            e.place(merge);
            e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
            emit_clear_pending(e);
        }
        MemoryWidth::Word | MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("count lanes are admitted only at the unprefixed Byte and Dword forms")
        }
    }
    e.place(done);
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

/// Group-2 shift/rotate by CL (0xD3 /op), register destination.
///
/// The count is runtime data, which forces three differences from `emit_shift`:
///
/// 1. The masked count stays in RCX across the shift. Nothing between the mask and the last read
///    writes RCX -- `shl r32, cl` only READS CL, and `pushfq`/`pop rax` do not touch it -- so no
///    stack slot is needed for it, unlike the double-shift path this was first modelled on.
/// 2. The host flags are captured with `pushfq` immediately after the shift, because the count
///    test below would otherwise destroy them.
/// 3. A zero count must merge NOTHING. `CpuGsw::shift_rotate` returns before touching a flag when
///    `count & 0x1f == 0` ("a zero count affects no flags at all"), and the host agrees, so the
///    captured value at that point is the masking `and`'s leftovers rather than the shift's.
///
/// COMPACTNESS IS THE POINT, not style. The first version of this lowering spilled the count and
/// the flags to the frame and inlined a full merge -- including `emit_clear_pending`'s four
/// stores -- into BOTH the count==1 and count>1 arms. That came to ~31 host instructions per
/// site, grew generated code 51% and arena compactions 47.6%, and the slice was reverted for it.
/// Here the two arms are two instructions each and everything after them, the OR, the EFLAGS
/// publish and the single `emit_clear_pending`, is shared.
fn emit_shift_cl(e: &mut Encoder, op: u8, dst: u8) {
    const DEFINED: u32 = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;
    const WITH_OF: u32 = DEFINED | crate::FLAG_OF;
    let one = e.label();
    let merge = e.label();
    let done = e.label();

    // Guest CL is the low byte of guest ECX. RCX is emitter scratch and GUEST_HOMES is R8-R14
    // plus RBX, so this can never clobber `home(dst)` -- not even when `dst` is ECX itself.
    e.mov_r32_r32(Reg::RCX, home(1));
    e.and_r32_imm32(Reg::RCX, 0x1f);
    e.shift_r32_cl(op, home(dst));
    e.pushfq();
    e.pop(Reg::RAX);

    e.test_r32_r32(Reg::RCX, Reg::RCX);
    e.jz(done);
    e.cmp_r32_imm32(Reg::RCX, 1);
    e.jz(one);
    e.and_r32_imm32(Reg::RBP, !DEFINED);
    e.and_r32_imm32(Reg::RAX, DEFINED);
    e.jmp(merge);
    e.place(one);
    e.and_r32_imm32(Reg::RBP, !WITH_OF);
    e.and_r32_imm32(Reg::RAX, WITH_OF);
    e.place(merge);
    e.or_r32_r32(Reg::RBP, Reg::RAX);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
    e.place(done);
}

/// CLD / STD. Touches ONLY bit 10.
///
/// Both the RBP shadow and the in-memory EFLAGS have to move. RBP is the running
/// materialized-eflags value and roughly ten sites publish it wholesale to `CpuGsw.eflags`
/// (`store_r32_disp32(R15, eflags_offset(), RBP)`), so writing memory alone would let the next
/// such publish resurrect the old DF. Writing RBP alone would lose it at the next reader that
/// goes to memory.
///
/// No `emit_clear_pending` here, unlike every arithmetic site: DF is outside the lazy flag
/// descriptor's ARITH mask entirely, so the descriptor stays valid across this instruction.
fn emit_direction_flag(e: &mut Encoder, set: bool) {
    if set {
        e.or_r32_imm32(Reg::RBP, crate::FLAG_DF);
    } else {
        e.and_r32_imm32(Reg::RBP, !crate::FLAG_DF);
    }
    e.load_r32_disp32(Reg::RAX, Reg::R15, eflags_offset());
    if set {
        e.or_r32_imm32(Reg::RAX, crate::FLAG_DF);
    } else {
        e.and_r32_imm32(Reg::RAX, !crate::FLAG_DF);
    }
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RAX);
}

// The pushfq/pop below moves RSP by 8 for one instruction: the accepted unwind gap
// described in jit/unwind.rs's module doc ("Known, accepted gap" beside the pushfq list).
fn emit_capture_flags(e: &mut Encoder, defined: u32) {
    e.pushfq();
    e.pop(Reg::RDI);
    e.and_r32_imm32(Reg::RBP, !defined);
    e.and_r32_imm32(Reg::RDI, defined);
    e.or_r32_r32(Reg::RBP, Reg::RDI);
}

// The push/popfq below moves RSP by 8 for one instruction: the same accepted unwind gap as
// `emit_capture_flags` above (see jit/unwind.rs's module doc).
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
/// Reached by `jmp RDX` from `emit_completed_path` OR from `emit_completed_dynamic_path`, in an
/// INTEGER source block either way. Both sites establish the same state, which is why the dynamic
/// path had to adopt the static one's register convention (the LinkCell in RCX) before it could
/// select `integer_entry`. On entry:
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
    emit_return(&mut e);
    e.finish()
}

pub(super) fn emit_store_homes(e: &mut Encoder) {
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

fn emit_return(e: &mut Encoder) {
    e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_EXIT);
    for (stack_offset, output_offset) in dynamic_counter_fields() {
        e.load_r64_disp8(Reg::RAX, Reg::RSP, stack_offset);
        e.store_r64_disp32(Reg::RDI, output_offset as i32, Reg::RAX);
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
