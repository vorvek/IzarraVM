// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Generic forward-decode basic-block builder. Starting at any hot linear PC,
//! `build_block` walks the decode cache forward, collecting continuable slots until the first
//! block terminator, and `try_admit` compiles the result into one native region. Supported
//! register-only runs execute after an exact budget preflight. Other slots and the terminal use
//! the precise helper path. Self-loops use a native back-edge; linear blocks return to the
//! interpreter at their boundary.
//!
//! ## What the builder vouches for (the region's admission invariants)
//!
//! - The block is a maximal run of interior-eligible slots. `continuable` (resolved once at decode,
//!   `block_continuable`) covers MOST of the spec §2.9 terminator predicate, inverted: it excludes
//!   control-flow mutators, CR/DR/segment/paging changers, HLT, far transfers, INT/IRET, MOV-CR/DR,
//!   LGDT.., OUT, INS/OUTS, and the clock readers RDTSC/WRMSR (all non-continuable). IN and
//!   TEST-acc-imm ARE admitted as interior continuations in the Approximate class (the
//!   poll-loop win); they are runtime step-breaks, NOT compile-time terminators, and
//!   `region_step`'s per-slot `requires_step_break()` check ends the block when a real device is
//!   actually touched.
//! - Only the TERMINAL slot may transfer control, change interrupt visibility, or replace DS. An interior slot
//!   must fall through to `lin+len` (else the next slot's snapshot would not be what runs), so
//!   branches / near RET / near indirect CALL/JMP always end the block. A relative branch whose
//!   static target is the entry is a self-loop back-edge (the drawcolumn case, generalized).
//! - `continuable` alone is NOT the terminator predicate: it admits STI/CLI/POPF, segment loads,
//!   and the SS-loads
//!   (the interpreter runs them inline with a per-instruction interrupt check). The region defers
//!   that check to the boundary, so those IF/shadow changers also end the block as its terminal
//!   slot (`changes_interrupt_visibility`); the deferred post-region check then fires at exactly
//!   the interpreter's boundary. This is the spec §2.9 behavioral predicate, enumerated from the
//!   interpreter's own IF-writer and shadow-arming sites.
//! - Every slot's decode is live in the cache (generation-current), unprefixed, and contained in
//!   the entry's 4 KB page. The physical span is captured at admission; a narrow-SMC kill inside
//!   it stales the slot table via the epoch.
//! - The block key includes the CPU mode/size bitmask (`CpuGsw::jit_mode_key`), validated at
//!   entry, so a block compiled for one mode is never reused in another at the same phys/d
//!   (spec §2.2).
//!
//! ## Self-patched immediates (unchanged from the drawcolumn spike)
//!
//! A store that patches a slot's immediate bumps the decode generation (SMC watch), which kills
//! the stamped line and the region stamp; the interpreter re-decodes and admission re-runs the
//! builder against the FRESH decodes. `try_admit` finds the existing region, rebuilds its slot
//! table wholesale (so patched immediates ride along in `DecodedInsn.imm`), and re-emits the
//! buffer (v2 bakes the add-imm immediates into the emitted bytes).

use std::num::NonZeroU32;

use super::encoder::{Encoder, Label, Reg};
use super::exec_mem::ExecutableBuffer;
use super::region::{CompiledRegion, JIT_REGION_TABLE_CAP};
use super::step::{RegionCtx, RegionEntryFn, RegionExitKind, Slot, SlotKind};
use crate::{
    AddressSize, CpuGsw, DecodeGroup, DecodedInsn, DecodedOperand, OperandSize, Prefixes,
    SegmentIndex,
};

/// Cap on a compiled block's slot count, to bound the emit and the compile pass. A block that
/// reaches the cap ends linearly (its tail is interpreted); real hot loops are far smaller.
const MAX_BLOCK_SLOTS: usize = 128;

/// The (opcode, mode-derived operand size) -> template DISPATCH. Classify an interior slot into its
/// emitted-code strategy: the register-only mov/add/shr forms (modrm mode 3, 32-bit) inline natively;
/// everything else (memory operands, modrm-less ops) goes through the v1 per-slot step. The gpr
/// indices and immediates are read from the captured decode so a self-patched immediate (the add-imm
/// slots) stays current across re-stamps. Only interior slots pass through here: `build_block`
/// classifies the terminal slot separately (a loop back-edge is `BackEdge`, a linear end is `Memory`),
/// both of which run through the full step.
///
/// This function plus the `SlotKind` enum plus the `emit_region` match ARE the template dispatch: to
/// add a native template for a new opcode, add a `SlotKind` variant, classify it here (gating on
/// operand size / mode as the width-safety fix requires), and emit it in `emit_region`. A
/// function-pointer table keyed by opcode is deliberately NOT used - the enum keeps exhaustiveness
/// checking and compiles to a jump table anyway, so it is the idiomatic dispatch for a single host.
fn classify_slot(insn: &DecodedInsn) -> SlotKind {
    // `mov r8, [mem]` (0x8A with a memory operand): route to the specialized byte-load executor
    // (dispatch removal, Stage 1 of the Round 3 template). The register form (0x8A mode 3) has a
    // Reg operand, not Mem, so it stays on the full step.
    if insn.opcode == 0x8a && matches!(insn.operand, Some(DecodedOperand::Mem(_))) {
        return SlotKind::MemLoadU8;
    }
    // `mov [mem], r8` (0x88 with a memory operand): the byte-store executor. The register form
    // (0x88 mode 3) has a Reg operand, not Mem, so it stays on the full step.
    if insn.opcode == 0x88 && matches!(insn.operand, Some(DecodedOperand::Mem(_))) {
        return SlotKind::MemStoreU8;
    }
    // Sized `mov r16/r32,[mem]` (0x8B) and `mov [mem],r16/r32` (0x89). Only the memory form has a Mem
    // operand; the register forms (mode 3) fall through to the RegMov inline path / full step below.
    if insn.opcode == 0x8b && matches!(insn.operand, Some(DecodedOperand::Mem(_))) {
        return SlotKind::MemLoadSized;
    }
    if insn.opcode == 0x89 && matches!(insn.operand, Some(DecodedOperand::Mem(_))) {
        return SlotKind::MemStoreSized;
    }
    let Some(m) = insn.modrm else {
        // A modrm-less interior op (push/pop/nop/int-free single-byte forms) has no inline
        // template yet; run it through the full step. (The terminal back-edge Jcc is classified
        // by build_block, never here.)
        return SlotKind::Memory;
    };
    // mode 3 = register-only (no memory operand). The three inline-able opcode classes:
    // 0x8B mov r32,r32 (reg=dst, rm=src); 0x81 /0 add r32,imm32 (reg=0, rm=dst); 0xC1 /5 shr
    // r32,imm8 (reg=5, rm=dst). The inline emit is 32-bit ONLY (load_r32/add_r32_imm32/shr_r32
    // and Dword flags), so it is correct only at 32-bit operand size. Operand size is mode-derived
    // (`CpuGsw::operand_size`): in a 32-bit code segment the r32 form is unprefixed and the r16
    // form carries 0x66 (rejected by build_block); in a 16-bit code segment it is the OPPOSITE, so
    // the unprefixed r16 form would reach here. Gating on `Dword` keeps those 16-bit forms on the
    // full trampoline step (correct width, interpreter-identical) instead of a wrong-width inline.
    if m.mode == 3 && insn.operand_size == OperandSize::Dword {
        match insn.opcode {
            0x8B => {
                return SlotKind::RegMov {
                    dst: m.reg,
                    src: m.rm,
                };
            }
            0x81 if m.reg == 0 => {
                return SlotKind::RegAddImm {
                    dst: m.rm,
                    imm: insn.imm,
                };
            }
            0xC1 if m.reg == 5 => {
                return SlotKind::RegShrImm {
                    dst: m.rm,
                    count: insn.imm as u8,
                };
            }
            _ => {}
        }
    }
    SlotKind::Memory
}

/// Whether an instruction transfers control (so it cannot be an interior slot: after it, EIP is
/// not `lin+len`). These are exactly the `continuable` forms that do not fall through:
/// - the whole Branch group (Jcc / JMP rel / LOOP / JCXZ / CALL rel),
/// - near RET (0xC2/0xC3),
/// - the near indirect CALL (0xFF /2) and JMP (0xFF /4) within ControlFlow (its /0 /1 /6 forms
///   are INC/DEC/PUSH r/m, which DO fall through and stay interior).
///
/// Everything else `continuable` (ALU/DataMove/Stack/Group/FlagsMisc/BitManip/CondMove/Fpu/
/// StringOps, plus the Approximate-class IN and TEST) falls through and is interior.
pub(crate) fn is_control_transfer(insn: &DecodedInsn) -> bool {
    match insn.group {
        DecodeGroup::Branch => true,
        DecodeGroup::ControlFlow => {
            matches!(insn.opcode, 0xc2 | 0xc3)
                || (insn.opcode == 0xff && matches!(insn.modrm.map(|m| m.reg), Some(2) | Some(4)))
        }
        _ => false,
    }
}

/// The precise transfer shape the unit simulator records for `insn` starting at linear `lin` in the
/// code segment based at `cs_base`. This hook emits the EXACT kind of every control transfer; the
/// sim's `effective_kind` then lowers the rich kinds per its config (the default config lowers all
/// four to `Indirect`, preserving v1 semantics). The classification:
/// - a relative near JMP (0xEB / 0xE9) or Jcc (short 0x70-0x7F, near 0x0F80-0x0F8F) has a statically
///   computable target and is `DirectNear`;
/// - a near CALL rel (0xE8) is `CallNear` with the same statically computable target (recursion, not
///   a loop back-edge);
/// - a LOOP/LOOPcc/JCXZ near branch (0xE0-0xE3) is `LoopNear` with the same rel8 target arithmetic;
/// - a near indirect CALL (0xFF /2) is `CallIndirect` (no static target);
/// - a near RET (0xC2/0xC3) is `Return`;
/// - every other `is_control_transfer` form (the near indirect JMP 0xFF /4) is `Indirect`;
/// - a non-control-transfer instruction is `None`.
///
/// Reuses `is_control_transfer` as the single control-flow authority, so the far / INT / IRET /
/// RETF terminators (not `continuable`, hence never `is_control_transfer` here) fall to `None` and
/// are closed by the sim's `is_terminator` check.
///
/// Target convention: the sim keys everything by LINEAR address, but `relative_jump` masks the new
/// EIP with `operand_size.mask()` (a 16-bit-operand branch truncates the IP within the segment), so
/// the delta arithmetic runs in offset-within-segment space and only then rebases: `target =
/// cs_base + ((lin - cs_base + len + imm) & mask)`. Without the mask a wrapping 16-bit branch would
/// record a wrong target. `CallNear` and `LoopNear` use the IDENTICAL arithmetic (they too carry a
/// sign-extended relative displacement in `insn.imm`).
pub(crate) fn observed_transfer(
    insn: &DecodedInsn,
    lin: u32,
    cs_base: u32,
) -> super::unit_sim::TransferKind {
    use super::unit_sim::TransferKind;
    if !is_control_transfer(insn) {
        return TransferKind::None;
    }
    // The masked, rebased linear target shared by DirectNear / CallNear / LoopNear (all three carry
    // a sign-extended relative displacement in `insn.imm`).
    let relative_target = || {
        let target_eip = lin
            .wrapping_sub(cs_base)
            .wrapping_add(u32::from(insn.len))
            .wrapping_add(insn.imm)
            & insn.operand_size.mask();
        cs_base.wrapping_add(target_eip)
    };
    match insn.opcode {
        0xeb | 0xe9 | 0x70..=0x7f | 0x0f80..=0x0f8f => TransferKind::DirectNear {
            target: relative_target(),
        },
        0xe8 => TransferKind::CallNear {
            target: relative_target(),
        },
        0xe0..=0xe3 => TransferKind::LoopNear {
            target: relative_target(),
        },
        0xc2 | 0xc3 => TransferKind::Return,
        // Near indirect CALL is 0xFF /2; the only other `is_control_transfer` 0xFF form is /4 (near
        // indirect JMP), which stays `Indirect`.
        0xff if matches!(insn.modrm.map(|m| m.reg), Some(2)) => TransferKind::CallIndirect,
        _ => TransferKind::Indirect,
    }
}

/// Whether an instruction changes interrupt visibility (IF/TF) or arms the one-instruction
/// interrupt shadow. These are `continuable` (the interpreter runs them inline, with its
/// per-instruction interrupt-transition check), but the region DEFERS that check to the whole-
/// region boundary, so an IF-writer cannot sit INSIDE a region: a mid-region transition would be
/// seen a slot too late. They therefore end the block as its TERMINAL slot (spec §2.9 category b):
/// the region executes the change last and returns, so `run_straight_line`'s own post-region
/// interrupt-transition check fires at exactly the boundary the interpreter would break at (for
/// STI and the SS-loads, the armed shadow correctly suppresses that check until the next
/// interpreted instruction consumes it). The set:
/// - STI (0xFB) / CLI (0xFA): write IF (STI also arms the shadow).
/// - POPF/POPFD (0x9D): write IF and TF from the stack.
/// - POP SS (0x17), MOV SS (0x8E /2), LSS (0x0F B2): arm the SS-load shadow (386 PRM 11-16).
///   Only the SS destination arms it.
pub(crate) fn changes_interrupt_visibility(insn: &DecodedInsn) -> bool {
    match insn.opcode {
        0xfa | 0xfb | 0x9d | 0x17 | 0x0fb2 => true,
        0x8e => matches!(insn.modrm.map(|m| m.reg), Some(2)),
        _ => false,
    }
}

/// Whether an instruction replaces DS. Native byte-memory probes are admitted only for flat DS and
/// entry checks cannot protect a later slot if an interior instruction changes that descriptor.
pub(crate) fn changes_native_memory_context(insn: &DecodedInsn) -> bool {
    insn.opcode == 0x1f || (insn.opcode == 0x8e && insn.modrm.is_some_and(|modrm| modrm.reg == 3))
}

/// Whether an instruction is eligible to be an INTERIOR slot of a compiled block: it falls through
/// to the next instruction, changes no interrupt visibility, and the interpreter would run it as a
/// straight-line continuation. This is the interior half of the §2.9 terminator predicate; a slot
/// that fails it either ends the block (control transfer / IF-shadow change, as the terminal slot)
/// or is a hard terminator (`!continuable`). Exposed for the terminator-contract test.
#[cfg(test)]
#[cfg_attr(
    not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )),
    allow(dead_code)
)]
pub(crate) fn is_interior_eligible(insn: &DecodedInsn) -> bool {
    insn.continuable
        && insn.prefixes == Prefixes::default()
        && !is_control_transfer(insn)
        && !changes_interrupt_visibility(insn)
        && !changes_native_memory_context(insn)
}

/// The static target of a relative branch that could be a self-loop back-edge, in linear space,
/// or `None` when the instruction is not such a branch. The Branch-group relative forms (Jcc /
/// JMP rel / LOOP / JCXZ) store the sign-extended displacement in `insn.imm`, so the target is
/// `lin + len + imm`. CALL rel (0xE8) is excluded: it pushes a return address, so a "call to
/// entry" is recursion, not a loop back-edge, and the native back-edge would drop the push.
fn loop_back_edge_target(insn: &DecodedInsn, lin: u32) -> Option<u32> {
    if insn.group != DecodeGroup::Branch || insn.opcode == 0xe8 {
        return None;
    }
    Some(lin.wrapping_add(u32::from(insn.len)).wrapping_add(insn.imm))
}

/// Forward-decode a basic block starting at `entry_lin`, returning its slot table and whether it
/// is a self-loop. `None` when the entry is cold (the interpreter warms it; admission retries) or
/// the entry itself is a terminator (nothing to compile). Mirrors `run_straight_line`'s own
/// continuation gate: warm, unprefixed, `continuable`, and contained in the entry's code page.
///
/// The block extends until the first control transfer (which becomes the terminal slot) or until
/// the next instruction is a terminator / page-crosses / is cold (the last sequential slot is then
/// terminal). Interior slots are classified by `classify_slot`; the terminal slot runs through the
/// full step (`BackEdge` for a self-loop, `Memory` otherwise) so `region_step`'s index-based
/// terminal handling drives it.
pub(crate) fn build_block(cpu: &CpuGsw, entry_lin: u32, d: bool) -> Option<(Vec<Slot>, bool)> {
    let mut slots: Vec<Slot> = Vec::new();
    let mut lin = entry_lin;
    let entry_page = entry_lin & !0xfff;
    let is_loop = loop {
        if lin & !0xfff != entry_page {
            break false;
        }
        let insn = match cpu.decode_cache.get(lin, d) {
            Some(i) => i,
            None => break false, // cold ahead: the block is whatever we have so far (linear).
        };
        let physical = cpu.decode_cache.line_phys_start(lin, d)?;
        // The run loop's own continuation gate: unprefixed + continuable + page-local. A slot that
        // fails it is a terminator; the block ends before it (linear).
        if insn.prefixes != Prefixes::default() || !insn.continuable {
            break false;
        }
        if (lin & 0xfff) + u32::from(insn.len) > 0x1000 {
            break false;
        }
        // An instruction ends the block as its terminal slot if it transfers control (after it
        // EIP is not `lin+len`) or changes interrupt visibility (the region defers its interrupt
        // check to the boundary, so an IF/shadow change must be the last thing it does).
        let ends_block = is_control_transfer(&insn)
            || changes_interrupt_visibility(&insn)
            || changes_native_memory_context(&insn);
        let this_lin = lin;
        lin = lin.wrapping_add(u32::from(insn.len));
        slots.push(Slot {
            insn,
            lin: this_lin,
            physical,
            kind: classify_slot(&insn),
        });
        if ends_block {
            // Terminal slot. `is_loop` only when it is a relative branch back to the entry; an
            // interrupt-visibility change is never a back-edge, so it ends a linear block.
            break loop_back_edge_target(&insn, this_lin) == Some(entry_lin);
        }
        if slots.len() >= MAX_BLOCK_SLOTS {
            break false; // cap reached; end linearly, the tail interprets.
        }
    };
    if slots.is_empty() {
        return None; // the entry is a terminator: nothing to compile.
    }
    // Force the terminal slot through the full step: a self-loop back-edge is `BackEdge`, a linear
    // end is `Memory`. Both emit `emit_full_step_call`, and `region_step` handles the terminal by
    // its index (`terminal_slot`), so the kind only steers the emitter away from an inline path.
    let last = slots.len() - 1;
    slots[last].kind = if is_loop {
        SlotKind::BackEdge
    } else {
        SlotKind::Memory
    };
    Some((slots, is_loop))
}

/// A hot linear block repays its entry cost only when its interior is a useful native group. The
/// terminal remains on the precise helper path and is excluded from this count.
fn native_linear_candidate(slots: &[Slot], load_enabled: bool, store_enabled: bool) -> bool {
    let Some((_, interior)) = slots.split_last() else {
        return false;
    };
    interior.len() >= 4
        && interior.iter().all(|slot| match slot.kind {
            SlotKind::RegMov { .. } | SlotKind::RegAddImm { .. } | SlotKind::RegShrImm { .. } => {
                true
            }
            SlotKind::MemLoadU8 => load_enabled && native_load_eligible(&slot.insn).is_some(),
            SlotKind::MemStoreU8 => store_enabled && native_store_eligible(&slot.insn).is_some(),
            _ => false,
        })
}

/// Total bytes the prologue reserves below the six pushed callee-saved registers, sized so every
/// `call` site sees RSP % 16 == 0 AND leaves room for a 5th stack-passed argument. At entry
/// RSP % 16 == 8 (after the return-address push); 6 pushes subtract 48, leaving RSP at 8 mod 16;
/// 56 moves it to 0 mod 16. 32 is the Win64 shadow space (a callee's
/// [RSP+0..32]); the native group finish call has five Win64 arguments, whose fifth
/// argument lands at [RSP+32], so the reserve must cover [0..40] at least. 56 gives that with
/// alignment. Harmless on SysV64 (no shadow space, but the alignment holds).
const STACK_RESERVE: u32 = 56;

/// Whether a `MemLoadU8` slot's `mov r8, [EA]` has an address form the native probe supports,
/// returning `(base, index, disp)` if so. Requirements are a 32-bit address, a base register, scale
/// one when an index is present, and DS as the access segment. Other forms use `region_step`.
fn native_load_eligible(insn: &DecodedInsn) -> Option<(u8, Option<u8>, i32)> {
    let Some(DecodedOperand::Mem(addr)) = insn.operand else {
        return None;
    };
    insn.modrm?;
    if addr.address_size != AddressSize::Dword {
        return None;
    }
    let base = addr.base?;
    if addr.index.is_some() && addr.scale != 1 {
        return None;
    }
    if addr.segment != SegmentIndex::Ds {
        return None;
    }
    Some((base, addr.index, addr.disp))
}

/// Whether a `MemStoreU8` slot's `mov [EA], r8` has an address form the native probe supports.
fn native_store_eligible(insn: &DecodedInsn) -> Option<(u8, Option<u8>, i32)> {
    let Some(DecodedOperand::Mem(addr)) = insn.operand else {
        return None;
    };
    insn.modrm?;
    if addr.address_size != AddressSize::Dword {
        return None;
    }
    let base = addr.base?;
    if addr.index.is_some() && addr.scale != 1 {
        return None;
    }
    if addr.segment != SegmentIndex::Ds {
        return None;
    }
    Some((base, addr.index, addr.disp))
}

/// Emit the guest-state update for one register-only native slot. Fetch, clock, cap, and liveness
/// accounting is emitted separately, either once for a group or through the proven slot helper.
fn emit_native_register_slot(e: &mut Encoder, slot: &Slot) {
    match slot.kind {
        SlotKind::RegMov { dst, src } => {
            emit_load_guest32(e, Reg::RAX, src);
            if dst == 5 {
                e.mov_r32_r32(Reg::RBX, Reg::RAX);
            } else if dst == 7 {
                e.mov_r32_r32(Reg::R14, Reg::RAX);
            }
            e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
        }
        SlotKind::RegAddImm { dst, imm } => {
            emit_load_guest32(e, Reg::RAX, dst);
            e.mov_r32_r32(Reg::RCX, Reg::RAX);
            e.add_r32_imm32(Reg::RAX, imm);
            if dst == 5 {
                e.mov_r32_r32(Reg::RBX, Reg::RAX);
            } else if dst == 7 {
                e.mov_r32_r32(Reg::R14, Reg::RAX);
            }
            e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
            let pending_off = core::mem::offset_of!(CpuGsw, pending_flags) as i32;
            e.mov_r32_imm32(Reg::RDX, 0x8000_0200);
            e.store_r32_disp32(Reg::R12, pending_off, Reg::RDX);
            e.store_r32_disp32(Reg::R12, pending_off + 4, Reg::RCX);
            e.mov_r32_imm32(Reg::RDX, imm);
            e.store_r32_disp32(Reg::R12, pending_off + 8, Reg::RDX);
            e.store_r32_disp32(Reg::R12, pending_off + 12, Reg::RAX);
        }
        SlotKind::RegShrImm { dst, count } => {
            emit_load_guest32(e, Reg::RAX, dst);
            e.mov_r32_r32(Reg::RCX, Reg::RAX);
            e.shr_r32_imm8(Reg::RAX, count);
            if dst == 5 {
                e.mov_r32_r32(Reg::RBX, Reg::RAX);
            } else if dst == 7 {
                e.mov_r32_r32(Reg::R14, Reg::RAX);
            }
            e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
            emit_set_shift_flags_shr_call(e, count);
        }
        _ => unreachable!(),
    }
}

/// Emit the region chain for `slots`: pin cpu/bus/ctx in R12/R13/R15, pin hot guest gprs (ebp->RBX,
/// edi->R14) and regs-base in RBP, emit native ALU for Reg* slots and exact byte helpers for
/// supported byte memory slots. Other memory slots and the back edge use the full step helper.
///
/// `regs_offset` is `offset_of!(CpuGsw, registers)`, baked in so the inline slots address `gpr[]`
/// as `[cpu + regs_offset + 4*i]` from the cpu pointer in R12. The emitted bytes depend on the slot
/// kinds and their baked immediates, so the buffer is re-emitted on every fresh admission (the
/// re-stamp path refreshes the slot table; the next fresh admission re-reads the immediates from
/// the fresh decodes).
///
/// TEMPLATE ABI (the contract a native slot template emits against; win64 + SysV64):
/// - PINNED (do not clobber for calls): R12=cpu, R13=bus, R15=ctx, RBX=guest_ebp (hot), R14=guest_edi (hot).
/// - RBP holds the regs base (cpu + regs_offset) for non-hot gpr[] access.
/// - SCRATCH, free to use: RAX/RCX/RDX (volatile). Fn pointers (step etc) loaded on demand from ctx into RAX.
/// - Guest gpr[i] lives at `[RBP + 4*i]`; hot 5/7 live in RBX/R14 with zero traffic.
/// - EARLY EXIT: a slot that must leave the block (fault or run boundary) jumps to the shared `exit`
///   label. Today only Memory/BackEdge slots exit, via the step fn's nonzero return + `jnz exit`; a
///   faulting NATIVE template (the Round 3 memory fast path) will jump to `exit` after spilling, per
///   the re-plan's fault rule. A register-only guest operation itself cannot fault.
fn emit_region(slots: &[Slot], regs_offset: u32, paged: bool) -> Vec<u8> {
    let mut e = Encoder::new();
    e.push(Reg::RBX);
    e.push(Reg::RBP);
    e.push(Reg::R12);
    e.push(Reg::R13);
    e.push(Reg::R14);
    e.push(Reg::R15);
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::R12, Reg::RCX); // cpu
        e.mov_r64_r64(Reg::R13, Reg::RDX); // bus
        e.mov_r64_r64(Reg::R15, Reg::R8); // ctx
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::R12, Reg::RDI); // cpu
        e.mov_r64_r64(Reg::R13, Reg::RSI); // bus
        e.mov_r64_r64(Reg::R15, Reg::RDX); // ctx
    }
    e.sub_r64_imm32(Reg::RSP, STACK_RESERVE);
    // v3 RA: use RBP as regs base (pinned callee saved). Load hot gprs (ebp->RBX, edi->R14) for
    // zero-mem-traffic access in inline slots. Step/inline fn ptrs loaded on demand (R15=ctx).
    e.mov_r64_r64(Reg::RBP, Reg::R12);
    if regs_offset != 0 {
        e.add_r64_imm32(Reg::RBP, regs_offset);
    }
    // load pinned guest gprs (ebp index 5 to host RBX, edi index 7 to host R14)
    e.load_r32_disp8(Reg::RBX, Reg::RBP, gpr_disp(5));
    e.load_r32_disp8(Reg::R14, Reg::RBP, gpr_disp(7));

    let loop_top = e.label();
    let exit = e.label();
    e.place(loop_top);
    let mut i = 0usize;
    while i < slots.len() {
        let slot = &slots[i];
        let is_inline = matches!(
            slot.kind,
            SlotKind::RegMov { .. } | SlotKind::RegAddImm { .. } | SlotKind::RegShrImm { .. }
        );
        let group_len = slots[i..]
            .iter()
            .take_while(|slot| {
                matches!(
                    slot.kind,
                    SlotKind::RegMov { .. }
                        | SlotKind::RegAddImm { .. }
                        | SlotKind::RegShrImm { .. }
                )
            })
            .count();
        let do_group = group_len >= 2;
        if do_group {
            // Collect and emit consecutive register-only inline slots.
            let group_start = i;
            let fallback = e.label();
            let after = e.label();
            emit_native_group_guard(&mut e, group_start as u32, group_len as u32, fallback);
            while i < slots.len() {
                let s = &slots[i];
                if !matches!(
                    s.kind,
                    SlotKind::RegMov { .. }
                        | SlotKind::RegAddImm { .. }
                        | SlotKind::RegShrImm { .. }
                ) {
                    break;
                }
                emit_native_register_slot(&mut e, s);
                i += 1;
            }
            let group_count = (i - group_start) as u32;
            emit_native_group_finish(&mut e, group_start as u32, group_count, exit);
            e.jmp(after);
            e.place(fallback);
            for (k, fallback_slot) in slots.iter().enumerate().take(i).skip(group_start) {
                emit_native_register_slot(&mut e, fallback_slot);
                emit_inline_bookkeeping_call(&mut e, k as u32, exit);
            }
            e.place(after);
        } else if is_inline {
            emit_native_register_slot(&mut e, slot);
            emit_inline_bookkeeping_call(&mut e, i as u32, exit);
            i += 1;
        } else {
            // Memory / terminal slot: original handling (no batching).
            let k32 = i as u32;
            let native: Option<(bool, u8, Option<u8>, i32)> = match slot.kind {
                SlotKind::MemLoadU8 => native_load_eligible(&slot.insn)
                    .map(|(base, index, disp)| (false, base, index, disp)),
                SlotKind::MemStoreU8 => native_store_eligible(&slot.insn)
                    .map(|(base, index, disp)| (true, base, index, disp)),
                _ => None,
            };
            if let Some((is_store, base, index, disp)) = native {
                let miss = e.label();
                let after = e.label();
                emit_native_memory_guard(&mut e, is_store, miss);
                emit_u8_address(&mut e, base, index, disp, is_store, miss, paged);
                emit_native_u8_call(&mut e, k32, exit);
                e.jmp(after);
                e.place(miss);
                emit_full_step_call(&mut e, k32);
                e.test_al_al();
                e.jnz(exit);
                // step may have mutated hot gprs; reload pins for subsequent native ops
                e.load_r32_disp8(Reg::RBX, Reg::RBP, gpr_disp(5));
                e.load_r32_disp8(Reg::R14, Reg::RBP, gpr_disp(7));
                e.place(after);
            } else {
                emit_full_step_call(&mut e, k32);
                e.test_al_al();
                e.jnz(exit);
                // reload pins after step (may have written gprs)
                e.load_r32_disp8(Reg::RBX, Reg::RBP, gpr_disp(5));
                e.load_r32_disp8(Reg::R14, Reg::RBP, gpr_disp(7));
            }
            i += 1;
        }
    }
    e.jmp(loop_top);
    e.place(exit);
    // v3 RA epilogue: no blind spill (pins may be stale after step exits; mem version is authoritative).
    // Write-through on hot dst writes + reload-after-step keep pins in sync for native paths.
    e.add_r64_imm32(Reg::RSP, STACK_RESERVE);
    e.pop(Reg::R15);
    e.pop(Reg::R14);
    e.pop(Reg::R13);
    e.pop(Reg::R12);
    e.pop(Reg::RBP);
    e.pop(Reg::RBX);
    e.ret();
    e.finish()
}

/// Byte displacement of `gpr[i]` within `Registers` (i in 0..8): `4 * i`, fitting in an i8 disp8.
fn gpr_disp(i: u8) -> i8 {
    (i as i32 * 4) as i8
}

/// Emit a load of guest gpr `i` (0..7) into host `dst`, using the pinned host value for the v3
/// hot registers (5->RBX, 7->R14) to avoid a memory roundtrip. Non-hot use [RBP + disp].
fn emit_load_guest32(e: &mut Encoder, dst: Reg, gpr: u8) {
    if gpr == 5 {
        e.mov_r32_r32(dst, Reg::RBX);
    } else if gpr == 7 {
        e.mov_r32_r32(dst, Reg::R14);
    } else {
        e.load_r32_disp8(dst, Reg::RBP, gpr_disp(gpr));
    }
}

/// Emit the TLB linear->physical translate + permission/dirty checks for the paged native probe.
/// Entry: linear EA in RAX. On TLB-HIT + present (implied by cached) + permitted + (for write: dirty
/// already set), leaves physical (phys_base | (lin & 0xfff)) in RAX and falls through. On any miss,
/// #PF condition, or write-to-clean, jumps to `miss` (caller does region_step which walks/faults
/// correctly). Never raises fault inside the native sequence. Uses scratch RAX/RCX/RDX; R12=cpu.
fn emit_tlb_translate(e: &mut Encoder, miss: Label, is_write: bool) {
    // On entry RAX holds linear (flat DS EA).
    // Save lin_off = linear & 0xfff in RCX for final phys combine; turn RAX into page_num (>>12) for tag/slot.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0000_0fff);
    e.shr_r32_imm8(Reg::RAX, 12); // RAX = page_num (tag value)

    // slot = page_num & 63; RDX = &entries[slot]
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, (crate::TLB_ENTRIES as u32) - 1);
    e.shl_r32_imm8(Reg::RDX, 4);
    let tlb_ent_off = core::mem::offset_of!(CpuGsw, tlb) as u32
        + core::mem::offset_of!(crate::Tlb, entries) as u32;
    e.add_r64_imm32(Reg::RDX, tlb_ent_off);
    e.add_r64_r64(Reg::RDX, Reg::R12); // RDX = &TlbEntry

    // Tag check (must precede gen check; direct-mapped TLB can alias on slot).
    e.cmp_r32_disp8(Reg::RAX, Reg::RDX, 0);
    e.jnz(miss);

    // gen match
    let tlb_gen_off = core::mem::offset_of!(CpuGsw, tlb) as u32
        + core::mem::offset_of!(crate::Tlb, generation) as u32;
    e.load_r32_disp32(Reg::RAX, Reg::R12, tlb_gen_off as i32);
    e.cmp_r32_disp8(Reg::RAX, Reg::RDX, 8);
    e.jnz(miss);

    // Protection and dirty checks (mirror translate_linear_checked hit path).
    let cpl_off = core::mem::offset_of!(CpuGsw, cpl) as i32;
    let cr0_off = core::mem::offset_of!(CpuGsw, control) as i32
        + core::mem::offset_of!(crate::ControlRegisters, cr0) as i32;

    // user = (cpl == 3)
    e.movzx_r32_byte_disp32(Reg::RAX, Reg::R12, cpl_off);
    e.cmp_r32_imm32(Reg::RAX, 3);
    let is_user = e.label();
    let after_prot = e.label();
    e.jz(is_user);

    // supervisor: if write && wp && !writable -> miss
    if is_write {
        e.load_r32_disp32(Reg::RAX, Reg::R12, cr0_off);
        e.and_r32_imm32(Reg::RAX, crate::CR0_WP);
        let no_wp = e.label();
        e.jz(no_wp);
        e.movzx_r32_byte_disp32(Reg::RAX, Reg::RDX, 12);
        e.cmp_r32_imm32(Reg::RAX, 0);
        e.jz(miss);
        e.place(no_wp);
    }
    e.jmp(after_prot);

    e.place(is_user);
    // user: !entry.user -> miss ; if write && !writable -> miss
    e.movzx_r32_byte_disp32(Reg::RAX, Reg::RDX, 13);
    e.cmp_r32_imm32(Reg::RAX, 0);
    e.jz(miss);
    if is_write {
        e.movzx_r32_byte_disp32(Reg::RAX, Reg::RDX, 12);
        e.cmp_r32_imm32(Reg::RAX, 0);
        e.jz(miss);
    }
    e.place(after_prot);

    // write to non-dirty: bail so interpreter walk sets D
    if is_write {
        e.movzx_r32_byte_disp32(Reg::RAX, Reg::RDX, 14);
        e.cmp_r32_imm32(Reg::RAX, 0);
        e.jz(miss);
    }

    // Diagnostic bump after full TLB translate success (tag, generation, protection, and dirty).
    emit_native_hit_counter(
        e,
        core::mem::offset_of!(crate::PerfCounters, jit_paged_tlb_successes),
    );

    // phys = entry.phys | lin_off; result in RAX for the byte helper
    e.load_r32_disp8(Reg::RAX, Reg::RDX, 4);
    e.or_r32_r32(Reg::RAX, Reg::RCX);
}

/// Increment one `PerfCounters` field directly from emitted code.
fn emit_native_hit_counter(e: &mut Encoder, perf_field_off: usize) {
    let off = (core::mem::offset_of!(CpuGsw, perf) + perf_field_off) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R12, off);
    e.add_r64_imm32(Reg::RAX, 1);
    e.store_r64_disp32(Reg::R12, off, Reg::RAX);
}

/// Compute a flat-DS byte address and translate it through the live TLB when paging is enabled.
/// Success leaves the physical address in EAX without changing guest state.
pub(crate) fn emit_u8_address(
    e: &mut Encoder,
    base: u8,
    index: Option<u8>,
    disp: i32,
    is_write: bool,
    miss: Label,
    paged: bool,
) {
    debug_assert_eq!(core::mem::size_of::<crate::TlbEntry>(), 16);
    debug_assert_eq!(
        core::mem::offset_of!(crate::TlbEntry, phys),
        4,
        "TLB probe loads entry.phys from [entry+4]"
    );

    // RAX = EA = base [+ index] [+ disp]. This is linear under the runtime flat-DS gate.
    emit_load_guest32(e, Reg::RAX, base);
    if let Some(idx) = index {
        emit_load_guest32(e, Reg::RCX, idx);
        e.add_r32_r32(Reg::RAX, Reg::RCX);
    }
    if disp != 0 {
        e.add_r32_imm32(Reg::RAX, disp as u32);
    }

    if paged {
        // TLB fast-path translate: linear in RAX -> on hit+permitted+dirty-ok, RAX becomes the
        // physical (full addr); on miss / #PF / !dirty-for-write bail to miss (region_step does
        // the full walk + correct fault). Preserves the low 12 bits logic by saving off and
        // recombining phys_base | off.
        emit_tlb_translate(e, miss, is_write);
    }
}

/// Call the exact byte-memory helper. EAX contains the physical address produced above.
fn emit_native_u8_call(e: &mut Encoder, k: u32, exit: Label) {
    #[cfg(windows)]
    {
        e.store_r32_disp8(Reg::RSP, 32, Reg::RAX);
        e.mov_r64_r64(Reg::RCX, Reg::R12);
        e.mov_r64_r64(Reg::RDX, Reg::R13);
        e.mov_r64_r64(Reg::R8, Reg::R15);
        e.mov_r32_imm32(Reg::R9, k);
    }
    #[cfg(not(windows))]
    {
        e.mov_r32_r32(Reg::R8, Reg::RAX);
        e.mov_r64_r64(Reg::RDI, Reg::R12);
        e.mov_r64_r64(Reg::RSI, Reg::R13);
        e.mov_r64_r64(Reg::RDX, Reg::R15);
        e.mov_r32_imm32(Reg::RCX, k);
    }
    let off = core::mem::offset_of!(RegionCtx, native_u8_fn) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R15, off);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jnz(exit);
}

fn emit_native_memory_guard(e: &mut Encoder, is_store: bool, miss: Label) {
    let off = if is_store {
        core::mem::offset_of!(RegionCtx, native_store_enabled)
    } else {
        core::mem::offset_of!(RegionCtx, native_load_enabled)
    } as i32;
    e.load_r32_disp32(Reg::RAX, Reg::R15, off);
    e.cmp_r32_imm32(Reg::RAX, 0);
    e.jz(miss);
}

/// Load step_fn from ctx (offset 0) on demand and call it. Used by Memory and BackEdge slots.
/// Under v3 RA, RBX holds a live pinned guest gpr (ebp) and must not be used for the fn pointer.
fn emit_full_step_call(e: &mut Encoder, k: u32) {
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R12);
        e.mov_r64_r64(Reg::RDX, Reg::R13);
        e.mov_r64_r64(Reg::R8, Reg::R15);
        e.mov_r32_imm32(Reg::R9, k);
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R12);
        e.mov_r64_r64(Reg::RSI, Reg::R13);
        e.mov_r64_r64(Reg::RDX, Reg::R15);
        e.mov_r32_imm32(Reg::RCX, k);
    }
    e.load_r64_disp8(Reg::RAX, Reg::R15, 0); // step_fn at ctx+0 (on-demand load)
    e.call_r64(Reg::RAX);
}

/// The trampoline-call form of inline-slot bookkeeping (used currently). Calls `region_inline_slot`
/// via the ctx fn pointer — the Rust trampoline does begin_instruction + charge_cached_fetch +
/// raw_clocks += 2 + insn_count += 1 + cap check + line_live probe.
fn emit_inline_bookkeeping_call(e: &mut Encoder, k: u32, exit: Label) {
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R12);
        e.mov_r64_r64(Reg::RDX, Reg::R13);
        e.mov_r64_r64(Reg::R8, Reg::R15);
        e.mov_r32_imm32(Reg::R9, k);
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R12);
        e.mov_r64_r64(Reg::RSI, Reg::R13);
        e.mov_r64_r64(Reg::RDX, Reg::R15);
        e.mov_r32_imm32(Reg::RCX, k);
    }
    e.load_r64_disp8(Reg::RAX, Reg::R15, 8);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jnz(exit);
}

/// Call the exact budget preview before a native group changes guest state. A nonzero result takes
/// the emitted per-slot fallback for this group.
fn emit_native_group_guard(e: &mut Encoder, first: u32, count: u32, fallback: Label) {
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R13);
        e.mov_r64_r64(Reg::RDX, Reg::R15);
        e.mov_r32_imm32(Reg::R8, first);
        e.mov_r32_imm32(Reg::R9, count);
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R13);
        e.mov_r64_r64(Reg::RSI, Reg::R15);
        e.mov_r32_imm32(Reg::RDX, first);
        e.mov_r32_imm32(Reg::RCX, count);
    }
    let off = core::mem::offset_of!(RegionCtx, native_group_guard_fn) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R15, off);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jnz(fallback);
}

/// Commit exact fetch and clock bookkeeping for a completed native group.
fn emit_native_group_finish(e: &mut Encoder, first: u32, count: u32, exit: Label) {
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R12);
        e.mov_r64_r64(Reg::RDX, Reg::R13);
        e.mov_r64_r64(Reg::R8, Reg::R15);
        e.mov_r32_imm32(Reg::R9, first);
        e.mov_r32_imm32(Reg::RAX, count);
        e.store_r32_disp8(Reg::RSP, 32, Reg::RAX);
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R12);
        e.mov_r64_r64(Reg::RSI, Reg::R13);
        e.mov_r64_r64(Reg::RDX, Reg::R15);
        e.mov_r32_imm32(Reg::RCX, first);
        e.mov_r32_imm32(Reg::R8, count);
    }
    let off = core::mem::offset_of!(RegionCtx, native_group_finish_fn) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R15, off);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jnz(exit);
}

/// Call `CpuGsw::jit_set_shift_flags_shr(cpu, value, count)` with cpu in R12, the original value
/// in RCX (moved to its arg reg), and `count` baked as an immediate.
fn emit_set_shift_flags_shr_call(e: &mut Encoder, count: u8) {
    #[cfg(windows)]
    {
        e.mov_r32_r32(Reg::RDX, Reg::RCX); // value -> RDX (arg1)
        e.mov_r64_r64(Reg::RCX, Reg::R12); // cpu -> RCX (arg0)
        e.mov_r32_imm32(Reg::R8, u32::from(count)); // count -> R8 (arg2)
    }
    #[cfg(not(windows))]
    {
        e.mov_r32_r32(Reg::RSI, Reg::RCX); // value -> RSI (arg1)
        e.mov_r64_r64(Reg::RDI, Reg::R12); // cpu -> RDI (arg0)
        e.mov_r32_imm32(Reg::RDX, u32::from(count)); // count -> RDX (arg2)
    }
    e.load_r64_disp8(Reg::RAX, Reg::R15, SET_SHIFT_FLAGS_FN_OFF);
    e.call_r64(Reg::RAX);
}

/// Byte offsets of the fn pointers within `RegionCtx` (set by the dispatch, loaded by the inline
/// emit). Each `Option<unsafe extern "C" fn>` is 8 bytes (null-pointer optimization).
/// Field order: step_fn(0), inline_step_fn(8), set_pending_add_fn(16), set_shift_flags_fn(24).
const SET_SHIFT_FLAGS_FN_OFF: i8 = 24;

/// Try to admit a region at `entry_lin` (admits any block shape, linear or loop). Test-only helper;
/// production auto-admission uses `try_admit_gated` with `reject_linear` set, and the forced-address
/// override passes `reject_linear = false` directly.
#[cfg(test)]
#[cfg_attr(
    not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )),
    allow(dead_code)
)]
pub(crate) fn try_admit(cpu: &mut CpuGsw, entry_lin: u32, d: bool) -> Option<NonZeroU32> {
    try_admit_gated(cpu, entry_lin, d, false)
}

/// Try to admit a region at `entry_lin`: build the block from the live decode cache, then either
/// refresh the already-installed region for this key (the re-stamp path after an SMC patch or a
/// mode change; the fresh decodes carry any patched immediates) or emit + install a new one.
/// Returns the table index for the caller to stamp into the decode line, or `None` when the block
/// is not (yet) buildable or the host has no W^X backend.
///
/// With `reject_linear`, hot linear blocks need four native interior slots and no interior helper
/// slots. Short or mixed blocks retain the interpreter path that avoided the measured broad-linear
/// admission regression. Self-loops and forced test admission keep their existing behavior.
pub(crate) fn try_admit_gated(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    reject_linear: bool,
) -> Option<NonZeroU32> {
    // Unpaged ring-0 protected mode is the V86 monitor in production today. Hotness admission
    // stays out of that transition path; forced and test admission remain available for
    // differential coverage.
    if reject_linear && cpu.is_ring0_protected() && !cpu.is_paging_enabled() {
        return None;
    }
    // The BIOS HLE stub window is a no-compile zone (the fetch seam must see those fetches;
    // defensive here, since forced admission should never point at it).
    if (0xff000..0xff400).contains(&entry_lin) {
        return None;
    }
    let (slots, is_loop) = build_block(cpu, entry_lin, d)?;
    if reject_linear && !is_loop {
        let ds_flat = cpu.jit_segment_flat(SegmentIndex::Ds);
        let load_enabled = ds_flat && cpu.jit_segment_readable(SegmentIndex::Ds);
        let store_enabled = ds_flat && cpu.jit_segment_writable(SegmentIndex::Ds);
        if !native_linear_candidate(&slots, load_enabled, store_enabled) {
            return None;
        }
    }
    let last = &slots[slots.len() - 1];
    // The block is a forward, non-wrapping linear run (build_block only extends forward and keeps
    // every slot page-local), so `end > entry_lin` always holds here. Bail rather than underflow if
    // a pathological block ever wraps the 4 GiB linear space (end <= entry_lin) or its physical
    // span overflows u32; the caller then interprets, which is always correct.
    let end = last.lin.wrapping_add(u32::from(last.insn.len));
    if end <= entry_lin {
        return None;
    }
    let span = end - entry_lin;
    // Physical span from the entry line (builder-warmed, single page by the containment rule so
    // contiguity holds); narrow SMC kills inside it stale the slot table via the epoch.
    let phys_lo = cpu.decode_cache.line_phys_start(entry_lin, d)?;
    let phys_hi = phys_lo.checked_add(span - 1)?;
    let epoch = cpu.decode_cache.jit_smc_epoch;
    let mode_key = cpu.jit_mode_key();
    let regs_offset = core::mem::offset_of!(CpuGsw, registers) as u32;
    let paged = cpu.control.cr0 & crate::CR0_PG != 0;
    let has_native_load = slots
        .iter()
        .any(|slot| slot.kind == SlotKind::MemLoadU8 && native_load_eligible(&slot.insn).is_some());
    let has_native_store = slots.iter().any(|slot| {
        slot.kind == SlotKind::MemStoreU8 && native_store_eligible(&slot.insn).is_some()
    });
    if let Some(idx) = cpu.jit_regions.find(entry_lin, d) {
        let region = cpu
            .jit_regions
            .get_mut(idx)
            .expect("find returned a live index");
        region.ctx.slots = slots;
        region.ctx.terminal_slot = (region.ctx.slots.len() - 1) as u32;
        region.ctx.is_loop = is_loop;
        region.phys_lo = phys_lo;
        region.phys_hi = phys_hi;
        region.valid_epoch = epoch;
        region.is_loop = is_loop;
        region.mode_key = mode_key;
        region.has_native_load = has_native_load;
        region.has_native_store = has_native_store;
        // v2 bakes the slot kinds and the add-imm immediates into the emitted bytes (unlike v1,
        // whose buffer encoded only the slot count). A self-patch changes an add slot's immediate,
        // and a rebuild can change the block shape, so the buffer is re-emitted from the fresh slot
        // table.
        let code = emit_region(&region.ctx.slots, regs_offset, paged);
        if let Some(buf) = ExecutableBuffer::new(&code) {
            // SAFETY: same transmute proof as the fresh-admission path below; `code` was produced
            // by emit_region to exactly the RegionEntryFn convention.
            region.entry =
                unsafe { std::mem::transmute::<*const u8, RegionEntryFn>(buf.entry_ptr()) };
            region.buf = buf;
        } else {
            // W^X alloc failed (unsupported host): drop the region so admission does not point at
            // stale emitted bytes, and bump the decode generation so no other line keeps a stamp
            // into the now-empty table (the clear() contract). The caller treats None as "not
            // admitted" and interprets instead.
            cpu.jit_regions.clear();
            cpu.jit_direct.clear();
            cpu.decode_cache.invalidate_and_clear_code_marks();
            cpu.perf.code_invalidations += 1;
            return None;
        }
        return Some(idx);
    }
    // Fresh install (no reusable entry). If the table is at capacity, drop it wholesale and bump the
    // decode generation so no stale stamp survives, then interpret this pass; the now-empty table
    // re-admits on the next warm hit. Coarse but O(1) and correct (JIT_REGION_TABLE_CAP).
    if cpu.jit_regions.len() >= JIT_REGION_TABLE_CAP {
        cpu.jit_regions.clear();
        cpu.jit_direct.clear();
        cpu.decode_cache.invalidate_and_clear_code_marks();
        cpu.perf.code_invalidations += 1;
        cpu.perf.jit_table_clears += 1;
        return None;
    }
    let terminal_slot = (slots.len() - 1) as u32;
    let code = emit_region(&slots, regs_offset, paged);
    let buf = ExecutableBuffer::new(&code)?;
    // SAFETY: `code` was produced by `emit_region` to exactly the `RegionEntryFn` calling
    // convention (alignment proof at STACK_RESERVE); `entry_ptr` stays valid for `buf`'s life,
    // and `buf` lives in the CompiledRegion beside the fn pointer.
    let entry: RegionEntryFn =
        unsafe { std::mem::transmute::<*const u8, RegionEntryFn>(buf.entry_ptr()) };
    let ctx = Box::new(RegionCtx {
        step_fn: None,            // written by the dispatch on every entry
        inline_step_fn: None,     // written by the dispatch on every entry
        set_pending_add_fn: None, // written by the dispatch on every entry
        set_shift_flags_fn: None, // written by the dispatch on every entry
        native_u8_fn: None,       // written by the dispatch on every entry
        slots,
        terminal_slot,
        is_loop,
        entry_eip: 0,
        raw_clocks: 0,
        insn_count: 0,
        run_total_at_entry: 0,
        bus_at_run_start: 0,
        cap: 0,
        rem0: 0,
        scale_num: 1,
        scale_den: 1,
        smc_epoch_at_entry: 0,
        d,
        exit: RegionExitKind::Boundary,
        fault: None,
        halted: false,
        native_insn_count: 0,
        helper_exit_count: 0,
        native_memory_helper_count: 0,
        native_load_enabled: 0,
        native_store_enabled: 0,
        native_u8_clock_bound: 0,
        native_group_guard_fn: None,
        native_group_finish_fn: None,
    });
    let idx = cpu.jit_regions.install(CompiledRegion {
        buf,
        entry,
        ctx,
        entry_lin,
        d,
        phys_lo,
        phys_hi,
        valid_epoch: epoch,
        is_loop,
        mode_key,
        has_native_load,
        has_native_store,
    });
    Some(idx)
}
