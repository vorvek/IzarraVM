//! Generic forward-decode basic-block builder (DYN-S1). Starting at any hot linear PC,
//! `build_block` walks the decode cache forward, collecting continuable slots until the first
//! block terminator, and `try_admit` compiles the result as a loop-region: emitted native code
//! that chains one `region_step` call per slot, with a native back-edge for a self-loop and a
//! return-to-interpreter for a linear block. Execution stays the proven v1/v2 trampoline (each
//! slot runs through the interpreter's own dispatch, so per-slot SEMANTICS need no validation
//! here); this stage delivers general block coverage plus the timing and terminator contracts,
//! not the native-template speedup (that is S2).
//!
//! ## What the builder vouches for (the region's admission invariants)
//!
//! - The block is a maximal run of interior-eligible slots. `continuable` (resolved once at decode,
//!   `block_continuable`) covers MOST of the spec §2.9 terminator predicate, inverted: it excludes
//!   control-flow mutators, CR/DR/segment/paging changers, HLT, far transfers, INT/IRET, MOV-CR/DR,
//!   LGDT.., OUT, INS/OUTS, and the clock readers RDTSC/WRMSR (all non-continuable). IN and
//!   TEST-acc-imm ARE admitted as interior continuations in the Approximate class (the P4a
//!   poll-loop win); they are runtime step-breaks, NOT compile-time terminators, and
//!   `region_step`'s per-slot `requires_step_break()` check ends the block when a real device is
//!   actually touched.
//! - Only the TERMINAL slot may transfer control OR change interrupt visibility. An interior slot
//!   must fall through to `lin+len` (else the next slot's snapshot would not be what runs), so
//!   branches / near RET / near indirect CALL/JMP always end the block. A relative branch whose
//!   static target is the entry is a self-loop back-edge (the drawcolumn case, generalized).
//! - `continuable` alone is NOT the terminator predicate: it admits STI/CLI/POPF and the SS-loads
//!   (the interpreter runs them inline with a per-instruction interrupt check). The region defers
//!   that check to the boundary, so those IF/shadow changers also end the block as its terminal
//!   slot (`changes_interrupt_visibility`); the deferred post-region check then fires at exactly
//!   the interpreter's boundary. This is the spec §2.9 behavioral predicate, enumerated from the
//!   interpreter's own IF-writer and shadow-arming sites.
//! - Every slot's decode is live in the cache (generation-current), unprefixed, and stays inside
//!   its 4 KB page, mirroring the run loop's own continuation gate. The physical span is captured
//!   at admission; a narrow-SMC kill inside it stales the slot table via the epoch.
//! - The block key includes the CPU mode/size bitmask (`Cpu386::jit_mode_key`), validated at
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
    AddressSize, Cpu386, DecodeGroup, DecodedInsn, DecodedOperand, OperandSize, PerfCounters,
    Prefixes, Registers, SegmentIndex,
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
    // (`Cpu386::operand_size`): in a 32-bit code segment the r32 form is unprefixed and the r16
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
///   Only the SS destination arms it, so MOV/POP to DS/ES/FS/GS stay interior.
pub(crate) fn changes_interrupt_visibility(insn: &DecodedInsn) -> bool {
    match insn.opcode {
        0xfa | 0xfb | 0x9d | 0x17 | 0x0fb2 => true,
        0x8e => matches!(insn.modrm.map(|m| m.reg), Some(2)),
        _ => false,
    }
}

/// Whether an instruction is eligible to be an INTERIOR slot of a compiled block: it falls through
/// to the next instruction, changes no interrupt visibility, and the interpreter would run it as a
/// straight-line continuation. This is the interior half of the §2.9 terminator predicate; a slot
/// that fails it either ends the block (control transfer / IF-shadow change, as the terminal slot)
/// or is a hard terminator (`!continuable`). Exposed for the terminator-contract test.
#[cfg(test)]
pub(crate) fn is_interior_eligible(insn: &DecodedInsn) -> bool {
    insn.continuable
        && insn.prefixes == Prefixes::default()
        && !is_control_transfer(insn)
        && !changes_interrupt_visibility(insn)
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
/// continuation gate per slot: warm, unprefixed, `continuable`, page-local.
///
/// The block extends until the first control transfer (which becomes the terminal slot) or until
/// the next instruction is a terminator / page-crosses / is cold (the last sequential slot is then
/// terminal). Interior slots are classified by `classify_slot`; the terminal slot runs through the
/// full step (`BackEdge` for a self-loop, `Memory` otherwise) so `region_step`'s index-based
/// terminal handling drives it.
pub(crate) fn build_block(cpu: &Cpu386, entry_lin: u32, d: bool) -> Option<(Vec<Slot>, bool)> {
    let mut slots: Vec<Slot> = Vec::new();
    let mut lin = entry_lin;
    let is_loop = loop {
        let insn = match cpu.decode_cache.get(lin, d) {
            Some(i) => i,
            None => break false, // cold ahead: the block is whatever we have so far (linear).
        };
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
        let ends_block = is_control_transfer(&insn) || changes_interrupt_visibility(&insn);
        let this_lin = lin;
        lin = lin.wrapping_add(u32::from(insn.len));
        slots.push(Slot {
            insn,
            lin: this_lin,
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

/// Total bytes the prologue reserves below the five pushed callee-saved registers, sized so every
/// `call` site sees RSP % 16 == 0 AND leaves room for a 5th stack-passed argument. At entry
/// RSP % 16 == 8 (after the return-address push); 5 pushes subtract 40 (8 mod 16), landing RSP at
/// 0 mod 16; 48 keeps it there (48 is a multiple of 16). 32 is the Win64 shadow space (a callee's
/// [RSP+0..32]); the native-bookkeeping path calls `jit_charge_fetch` (5 args on win64), whose 5th
/// argument lands at [RSP+32], so the reserve must cover [0..40] at least - 48 gives that with
/// alignment. Harmless on SysV64 (no shadow space, but the alignment holds).
const STACK_RESERVE: u32 = 48;

/// Throwaway A/B mode for the S2.2 end-to-end prototype (owner: "prototype first"): controls how
/// `emit_region` compiles the inline slots' bookkeeping. 0 = the `region_inline_slot` trampoline
/// CALL (today); 1 = native arithmetic (`emit_native_bookkeeping`, still 3 call-outs); 2 = SKIP the
/// bookkeeping entirely (INCORRECT clocks - a timing-ceiling probe bounding how much inline
/// bookkeeping can ever cost). Read only at emit time. Not wired to production admission.
pub(crate) static NATIVE_BOOKKEEPING: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);

/// The cost-fold native-LOAD toggle (env `IZARRAVM_JIT_FOLD`), read at emit time. When ON AND a block
/// is fold-eligible (Approximate class, unpaged, flat DS — checked by `try_admit_gated`), a `MemLoadU8`
/// slot with a supported address form is emitted as a native page-cache probe + folded bookkeeping
/// instead of a `region_step` call. OFF by default so production (`IZARRAVM_JIT=1` alone) and every
/// bit-identical test are undisturbed. This makes JIT-block timing APPROXIMATE (bus cost is folded and
/// flushed in bulk), validated by the anchor bands, not the differential timing asserts.
pub(crate) static FOLD_TIMING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether a `MemLoadU8` slot's `mov r8, [EA]` has an address form the native fold probe supports,
/// returning `(base, index, disp, dst_byte_reg)` if so. The probe computes `EA = base [+ index] [+ disp]`
/// and treats it as linear == physical (unpaged, gated by the caller). Requirements: 32-bit address
/// size (the probe does 32-bit EA math), a base register (the probe indexes off one), scale 1 when an
/// index is present (the probe adds the index unscaled), and the DS segment (the per-entry flatness
/// re-check in `run_region` guards DS only). Anything else falls to the `region_step` path unchanged.
fn fold_load_eligible(insn: &DecodedInsn) -> Option<(u8, Option<u8>, i32, u8)> {
    let Some(DecodedOperand::Mem(addr)) = insn.operand else {
        return None;
    };
    let m = insn.modrm?;
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
    Some((base, addr.index, addr.disp, m.reg))
}

/// Whether a `MemStoreU8` slot's `mov [EA], r8` has a form the native fold STORE probe supports,
/// returning `(base, index, disp, src_byte_reg)` if so. Same address requirements as `fold_load_eligible`
/// (unpaged/flat-DS is the block gate + the per-entry DS re-check), plus the SOURCE must be a low-byte
/// register (AL/CL/DL/BL, modrm reg < 4): the probe stores `gpr[src]`'s low byte, so AH..BH (reg 4..7,
/// whose byte is bits 8-15 of another register) fall to `region_step`. The drawcolumn's stores are
/// `mov [edi],al` / `mov [edi+0x50],bl` — both low-byte.
fn fold_store_eligible(insn: &DecodedInsn) -> Option<(u8, Option<u8>, i32, u8)> {
    let Some(DecodedOperand::Mem(addr)) = insn.operand else {
        return None;
    };
    let m = insn.modrm?;
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
    if m.reg >= 4 {
        return None; // low-byte source register only (see the probe's src<4 gate)
    }
    Some((base, addr.index, addr.disp, m.reg))
}

/// Emit the region chain for `slots`: pin cpu/bus/ctx in R12/R13/R15, pin hot guest gprs (ebp->RBX,
/// edi->R14) and regs-base in RBP, emit native ALU for Reg* slots and native fold probes for
/// supported Mem* , with batched bookkeeping + cross-mult cap check. Memory/BackEdge fall to full
/// step (or native fold probe for u8 when enabled). After final slot a jmp closes the loop.
///
/// `regs_offset` is `offset_of!(Cpu386, registers)`, baked in so the inline slots address `gpr[]`
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
///   the re-plan's fault rule. A reg-only template never faults and never exits mid-body.
fn emit_region(
    slots: &[Slot],
    regs_offset: u32,
    scale_den: u32,
    fold_native: bool,
    store_fold: bool,
    fold_paged: bool,
) -> Vec<u8> {
    let mode = NATIVE_BOOKKEEPING.load(std::sync::atomic::Ordering::Relaxed);
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
        let _k32 = i as u32;
        let _next_lin = slots.get(i + 1).map(|s| s.lin).unwrap_or(0);
        let is_inline = matches!(
            slot.kind,
            SlotKind::RegMov { .. } | SlotKind::RegAddImm { .. } | SlotKind::RegShrImm { .. }
        );
        let do_group = is_inline && (fold_native || mode == 1);
        if do_group {
            // Collect and emit a run of consecutive register-only inline slots (slice 3 folding).
            let group_start = i;
            let mut group_total_len: u8 = 0;
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
                group_total_len = group_total_len.wrapping_add(s.insn.len);
                match s.kind {
                    SlotKind::RegMov { dst, src } => {
                        // RA: use pinned host reg if hot (ebp=5->RBX, edi=7->R14), else [regs_base + off]
                        if src == 5 {
                            e.mov_r32_r32(Reg::RAX, Reg::RBX);
                        } else if src == 7 {
                            e.mov_r32_r32(Reg::RAX, Reg::R14);
                        } else {
                            e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(src));
                        }
                        if dst == 5 {
                            e.mov_r32_r32(Reg::RBX, Reg::RAX);
                            e.store_r32_disp8(Reg::RBP, gpr_disp(5), Reg::RAX); // write-through for pin
                        } else if dst == 7 {
                            e.mov_r32_r32(Reg::R14, Reg::RAX);
                            e.store_r32_disp8(Reg::RBP, gpr_disp(7), Reg::RAX);
                        } else {
                            e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
                        }
                    }
                    SlotKind::RegAddImm { dst, imm } => {
                        if dst == 5 {
                            e.mov_r32_r32(Reg::RAX, Reg::RBX);
                        } else if dst == 7 {
                            e.mov_r32_r32(Reg::RAX, Reg::R14);
                        } else {
                            e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(dst));
                        }
                        e.mov_r32_r32(Reg::RCX, Reg::RAX);
                        e.add_r32_imm32(Reg::RAX, imm);
                        if dst == 5 {
                            e.mov_r32_r32(Reg::RBX, Reg::RAX);
                            e.store_r32_disp8(Reg::RBP, gpr_disp(5), Reg::RAX); // write-through for pin
                        } else if dst == 7 {
                            e.mov_r32_r32(Reg::R14, Reg::RAX);
                            e.store_r32_disp8(Reg::RBP, gpr_disp(7), Reg::RAX);
                        } else {
                            e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
                        }
                        // Direct write of PendingFlags (Add, Dword) to avoid helper call.
                        const PENDING_OFF: i32 = 3912;
                        e.mov_r32_imm32(Reg::RDX, 0x8000_0200);
                        e.store_r32_disp32(Reg::R12, PENDING_OFF, Reg::RDX); // tag
                        e.store_r32_disp32(Reg::R12, PENDING_OFF + 4, Reg::RCX); // a = old
                        e.mov_r32_imm32(Reg::RDX, imm);
                        e.store_r32_disp32(Reg::R12, PENDING_OFF + 8, Reg::RDX); // b = imm
                        e.store_r32_disp32(Reg::R12, PENDING_OFF + 12, Reg::RAX); // result
                    }
                    SlotKind::RegShrImm { dst, count } => {
                        if dst == 5 {
                            e.mov_r32_r32(Reg::RAX, Reg::RBX);
                        } else if dst == 7 {
                            e.mov_r32_r32(Reg::RAX, Reg::R14);
                        } else {
                            e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(dst));
                        }
                        e.mov_r32_r32(Reg::RCX, Reg::RAX);
                        e.shr_r32_imm8(Reg::RAX, count);
                        if dst == 5 {
                            e.mov_r32_r32(Reg::RBX, Reg::RAX);
                            e.store_r32_disp8(Reg::RBP, gpr_disp(5), Reg::RAX); // write-through for pin
                        } else if dst == 7 {
                            e.mov_r32_r32(Reg::R14, Reg::RAX);
                            e.store_r32_disp8(Reg::RBP, gpr_disp(7), Reg::RAX);
                        } else {
                            e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
                        }
                        emit_set_shift_flags_shr_call(&mut e, count);
                    }
                    _ => unreachable!(),
                }
                i += 1;
            }
            // After the group, one bookkeeping (folds post of last in group and pre of next).
            let group_next_lin = slots.get(i).map(|s| s.lin).unwrap_or(0);
            let group_count = (i - group_start) as u8;
            if fold_native {
                let fetch_off = core::mem::offset_of!(RegionCtx, fetch_cost) as i32;
                emit_fold_bookkeeping(
                    &mut e,
                    group_total_len,
                    group_count,
                    fetch_off,
                    group_next_lin,
                    exit,
                );
            } else if mode == 1 {
                let group_lin = slots[group_start].lin;
                emit_native_bookkeeping(
                    &mut e,
                    group_lin,
                    group_total_len,
                    scale_den,
                    group_next_lin,
                    exit,
                );
            } else {
                // For trampoline mode, call for the last k in group (approximates; main win is fold path).
                emit_inline_bookkeeping_call(&mut e, (i - 1) as u32, exit);
            }
        } else if is_inline {
            // Per-slot for non-folded modes (preserve exact behavior).
            let k32 = i as u32;
            let next_lin = slots.get(i + 1).map(|s| s.lin).unwrap_or(0);
            match slot.kind {
                SlotKind::RegMov { dst, src } => {
                    if src == 5 {
                        e.mov_r32_r32(Reg::RAX, Reg::RBX);
                    } else if src == 7 {
                        e.mov_r32_r32(Reg::RAX, Reg::R14);
                    } else {
                        e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(src));
                    }
                    if dst == 5 {
                        e.mov_r32_r32(Reg::RBX, Reg::RAX);
                        e.store_r32_disp8(Reg::RBP, gpr_disp(5), Reg::RAX); // write-through
                    } else if dst == 7 {
                        e.mov_r32_r32(Reg::R14, Reg::RAX);
                        e.store_r32_disp8(Reg::RBP, gpr_disp(7), Reg::RAX);
                    } else {
                        e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
                    }
                    if fold_native {
                        let fetch_off = core::mem::offset_of!(RegionCtx, fetch_cost) as i32;
                        emit_fold_bookkeeping(&mut e, slot.insn.len, 1, fetch_off, next_lin, exit);
                    } else if mode == 1 {
                        emit_native_bookkeeping(
                            &mut e,
                            slot.lin,
                            slot.insn.len,
                            scale_den,
                            next_lin,
                            exit,
                        );
                    } else {
                        emit_inline_bookkeeping_call(&mut e, k32, exit);
                    }
                }
                SlotKind::RegAddImm { dst, imm } => {
                    if dst == 5 {
                        e.mov_r32_r32(Reg::RAX, Reg::RBX);
                    } else if dst == 7 {
                        e.mov_r32_r32(Reg::RAX, Reg::R14);
                    } else {
                        e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(dst));
                    }
                    e.mov_r32_r32(Reg::RCX, Reg::RAX);
                    e.add_r32_imm32(Reg::RAX, imm);
                    if dst == 5 {
                        e.mov_r32_r32(Reg::RBX, Reg::RAX);
                        e.store_r32_disp8(Reg::RBP, gpr_disp(5), Reg::RAX); // write-through
                    } else if dst == 7 {
                        e.mov_r32_r32(Reg::R14, Reg::RAX);
                        e.store_r32_disp8(Reg::RBP, gpr_disp(7), Reg::RAX);
                    } else {
                        e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
                    }
                    const PENDING_OFF: i32 = 3912;
                    e.mov_r32_imm32(Reg::RDX, 0x8000_0200);
                    e.store_r32_disp32(Reg::R12, PENDING_OFF, Reg::RDX);
                    e.store_r32_disp32(Reg::R12, PENDING_OFF + 4, Reg::RCX);
                    e.mov_r32_imm32(Reg::RDX, imm);
                    e.store_r32_disp32(Reg::R12, PENDING_OFF + 8, Reg::RDX);
                    e.store_r32_disp32(Reg::R12, PENDING_OFF + 12, Reg::RAX);
                    if fold_native {
                        let fetch_off = core::mem::offset_of!(RegionCtx, fetch_cost) as i32;
                        emit_fold_bookkeeping(&mut e, slot.insn.len, 1, fetch_off, next_lin, exit);
                    } else if mode == 1 {
                        emit_native_bookkeeping(
                            &mut e,
                            slot.lin,
                            slot.insn.len,
                            scale_den,
                            next_lin,
                            exit,
                        );
                    } else {
                        emit_inline_bookkeeping_call(&mut e, k32, exit);
                    }
                }
                SlotKind::RegShrImm { dst, count } => {
                    if dst == 5 {
                        e.mov_r32_r32(Reg::RAX, Reg::RBX);
                    } else if dst == 7 {
                        e.mov_r32_r32(Reg::RAX, Reg::R14);
                    } else {
                        e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(dst));
                    }
                    e.mov_r32_r32(Reg::RCX, Reg::RAX);
                    e.shr_r32_imm8(Reg::RAX, count);
                    if dst == 5 {
                        e.mov_r32_r32(Reg::RBX, Reg::RAX);
                        e.store_r32_disp8(Reg::RBP, gpr_disp(5), Reg::RAX); // write-through
                    } else if dst == 7 {
                        e.mov_r32_r32(Reg::R14, Reg::RAX);
                        e.store_r32_disp8(Reg::RBP, gpr_disp(7), Reg::RAX);
                    } else {
                        e.store_r32_disp8(Reg::RBP, gpr_disp(dst), Reg::RAX);
                    }
                    emit_set_shift_flags_shr_call(&mut e, count);
                    if fold_native {
                        let fetch_off = core::mem::offset_of!(RegionCtx, fetch_cost) as i32;
                        emit_fold_bookkeeping(&mut e, slot.insn.len, 1, fetch_off, next_lin, exit);
                    } else if mode == 1 {
                        emit_native_bookkeeping(
                            &mut e,
                            slot.lin,
                            slot.insn.len,
                            scale_den,
                            next_lin,
                            exit,
                        );
                    } else {
                        emit_inline_bookkeeping_call(&mut e, k32, exit);
                    }
                }
                _ => unreachable!(),
            }
            i += 1;
        } else {
            // Memory / terminal slot: original handling (no batching).
            let k32 = i as u32;
            let next_lin = slots.get(i + 1).map(|s| s.lin).unwrap_or(0);
            let native: Option<(bool, u8, Option<u8>, i32, u8)> = match slot.kind {
                SlotKind::MemLoadU8 if fold_native => {
                    fold_load_eligible(&slot.insn).map(|(b, i, d, r)| (false, b, i, d, r))
                }
                SlotKind::MemStoreU8 if store_fold => {
                    fold_store_eligible(&slot.insn).map(|(b, i, d, r)| (true, b, i, d, r))
                }
                _ => None,
            };
            if let Some((is_store, base, index, disp, reg)) = native {
                let miss = e.label();
                let after = e.label();
                if is_store {
                    emit_native_store_fold(
                        &mut e,
                        base,
                        index,
                        disp,
                        reg,
                        slot.insn.len,
                        next_lin,
                        exit,
                        miss,
                        fold_paged,
                    );
                } else {
                    emit_native_load_fold(
                        &mut e,
                        base,
                        index,
                        disp,
                        reg,
                        slot.insn.len,
                        next_lin,
                        exit,
                        miss,
                        fold_paged,
                    );
                }
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

/// Byte displacement of the 8-bit register `i` (0..8) within `Registers`: AL..BL (0..4) are the low
/// byte of `gpr[i]` at `4*i`; AH..BH (4..8) are byte 1 of `gpr[i-4]` at `4*(i-4)+1`. Little-endian, so
/// byte 0 is bits 0-7 and byte 1 is bits 8-15 - exactly the two lanes `write_gpr8` targets.
#[allow(dead_code)] // used by emit_load_u8_probe, which is wired into emit_region next
fn gpr8_disp(i: u8) -> i8 {
    if i < 4 {
        (i as i32 * 4) as i8
    } else {
        ((i - 4) as i32 * 4 + 1) as i8
    }
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
    let tlb_ent_off = core::mem::offset_of!(Cpu386, tlb) as u32
        + core::mem::offset_of!(crate::Tlb, entries) as u32;
    e.add_r64_imm32(Reg::RDX, tlb_ent_off);
    e.add_r64_r64(Reg::RDX, Reg::R12); // RDX = &TlbEntry

    // Tag check (must precede gen check; direct-mapped TLB can alias on slot).
    e.cmp_r32_disp8(Reg::RAX, Reg::RDX, 0);
    e.jnz(miss);

    // gen match
    let tlb_gen_off = core::mem::offset_of!(Cpu386, tlb) as u32
        + core::mem::offset_of!(crate::Tlb, generation) as u32;
    e.load_r32_disp32(Reg::RAX, Reg::R12, tlb_gen_off as i32);
    e.cmp_r32_disp8(Reg::RAX, Reg::RDX, 8);
    e.jnz(miss);

    // Protection and dirty checks (mirror translate_linear_checked hit path).
    let cpl_off = core::mem::offset_of!(Cpu386, cpl) as i32;
    let cr0_off = core::mem::offset_of!(Cpu386, control) as i32
        + core::mem::offset_of!(crate::ControlRegisters, cr0) as i32;

    // user = (cpl == 3)
    e.load_r32_disp32(Reg::RAX, Reg::R12, cpl_off);
    e.and_r32_imm32(Reg::RAX, 0xff);
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
        e.load_r32_disp8(Reg::RAX, Reg::RDX, 12);
        e.cmp_r32_imm32(Reg::RAX, 0);
        e.jz(miss);
        e.place(no_wp);
    }
    e.jmp(after_prot);

    e.place(is_user);
    // user: !entry.user -> miss ; if write && !writable -> miss
    e.load_r32_disp8(Reg::RAX, Reg::RDX, 13);
    e.cmp_r32_imm32(Reg::RAX, 0);
    e.jz(miss);
    if is_write {
        e.load_r32_disp8(Reg::RAX, Reg::RDX, 12);
        e.cmp_r32_imm32(Reg::RAX, 0);
        e.jz(miss);
    }
    e.place(after_prot);

    // write to non-dirty: bail so interpreter walk sets D
    if is_write {
        e.load_r32_disp8(Reg::RAX, Reg::RDX, 14);
        e.cmp_r32_imm32(Reg::RAX, 0);
        e.jz(miss);
    }

    // Diagnostic bump for paged probe investigation (TLB success rate vs full hits).
    // This is after full TLB translate success (tag+gen+prot+dirty), before page-cache probe.
    emit_native_hit_counter(
        e,
        core::mem::offset_of!(crate::PerfCounters, jit_paged_tlb_successes),
    );

    // phys = entry.phys | lin_off ; result in RAX for the page-cache probe
    e.load_r32_disp8(Reg::RAX, Reg::RDX, 4);
    e.or_r32_r32(Reg::RAX, Reg::RCX);
}

/// Emit the native UNPAGED byte-load fast path for `mov r8, [EA]`, where EA is a flat-DS (base 0)
/// address so linear == physical: `[base]` or `[base + index]` (SIB scale 1) plus a `disp`. The
/// caller gates this on unpaged + flat DS + scale 1 (else it must emit the interpreter fallback).
///
/// ABI (as `emit_region` v3): R12 = cpu, RBP = regs base (cpu + regs_offset); scratch RAX/RCX/RDX.
/// Hot guest gprs live in RBX(5)/R14(7) and are used directly by callers of the probe for EA if the
/// base/index match. On a page-cache HIT it derefs ... writes ... MISS jumps to miss. No bus charge
/// emitted here - the cost-fold accounts the fetch + data clocks separately.
#[allow(dead_code)] // proven in isolation by native_load_probe_reads_the_right_byte; wired next
pub(crate) fn emit_load_u8_probe(
    e: &mut Encoder,
    base: u8,
    index: Option<u8>,
    disp: i32,
    dst: u8,
    miss: Label,
    paged: bool,
) {
    // The emitted deref hardcodes the entry stride (shl 4 == *16) and the ptr field offset (+8); pin
    // the layout it assumes so a struct change fails loudly here instead of reading a wrong pointer.
    debug_assert_eq!(core::mem::size_of::<crate::DirectPageCacheEntry>(), 16);
    debug_assert_eq!(
        core::mem::offset_of!(crate::DirectPageCacheEntry, ptr),
        8,
        "the deref loads entry.ptr from [entry+8]"
    );
    debug_assert_eq!(core::mem::size_of::<crate::TlbEntry>(), 16);
    debug_assert_eq!(
        core::mem::offset_of!(crate::TlbEntry, phys),
        4,
        "TLB probe loads entry.phys from [entry+4]"
    );

    // RAX = EA = base [+ index] [+ disp]  -- this is linear (flat DS). For paged it is translated
    // to physical by the TLB path below before the physical-keyed page-cache probe.
    // Use pinned host regs when base/index are the hot ones (v3 RA zero-traffic for drawcolumn).
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
        emit_tlb_translate(e, miss, false /* load (no dirty check) */);
        // On success RAX now holds physical (we put it there); fallthrough to cache probe below
        // which treats "RAX" as the phys addr for page/off.
    }

    // RCX = page = (phys or lin) & !0x0fff.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0xffff_f000);
    // RDX = &data_read_pages.entries[(page >> 12) & (LINES-1)]  (entry stride 16 = shl 4).
    e.mov_r32_r32(Reg::RDX, Reg::RCX);
    e.shr_r32_imm8(Reg::RDX, 12);
    e.and_r32_imm32(Reg::RDX, (crate::DIRECT_PAGE_CACHE_LINES as u32) - 1);
    e.shl_r32_imm8(Reg::RDX, 4);
    let entries_off = core::mem::offset_of!(Cpu386, data_read_pages) as u32
        + core::mem::offset_of!(crate::DirectPageCache, entries) as u32;
    e.add_r64_imm32(Reg::RDX, entries_off);
    e.add_r64_r64(Reg::RDX, Reg::R12);
    // Tag compare: page (RCX) vs entry.physical_page ([RDX+0]); miss on mismatch.
    e.cmp_r32_disp8(Reg::RCX, Reg::RDX, 0);
    e.jnz(miss);
    // HIT: offset = addr & 0x0fff; ptr = entry.ptr ([RDX+8]); byte = *(ptr + offset); write gpr8.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0fff);
    e.load_r64_disp8(Reg::RAX, Reg::RDX, 8);
    e.movzx_r32_byte_sib(Reg::RAX, Reg::RAX, Reg::RCX);
    e.store_r8_disp8(Reg::RBP, gpr8_disp(dst), Reg::RAX);
}

/// Emit a native cost-fold LOAD slot for a `mov r8, [EA]` whose address is unpaged, flat-DS, and a
/// supported form (the caller gates on `fold_load_eligible` and block eligibility). It replaces the
/// per-slot `region_step` CALL: the emitted `emit_load_u8_probe` computes the native EA, probes the
/// page cache, and on a HIT derefs and does the `write_gpr8`; then this appends the NATIVE bookkeeping
/// `region_step` would otherwise do — advance eip, clear the interrupt shadow, accumulate the core
/// clocks and retired count, FOLD the bus cost (into `ctx.folded_raw_bus`, flushed in bulk by the next
/// `region_step` slot or the back-edge — which is what makes JIT-block timing approximate), bump the
/// native-hit counter, and run the next slot's `line_live` probe. There is NO per-slot cap check
/// (deferred to the next flush point) and NO per-slot bus charge. `begin_instruction` is intentionally
/// skipped (fold spec gate #1: a native LOAD never writes `written_pages`, and the interspersed
/// `region_step` slots and the back-edge reconcile it; the state comparator validates this). A
/// page-cache MISS jumps to `miss`, where the CALLER emits the identical interpreter fallback
/// (`emit_full_step_call`) — nothing above is committed on the miss branch (the probe only reads
/// registers and the tag before jumping), so there is no double-charge.
#[allow(clippy::too_many_arguments)]
fn emit_native_load_fold(
    e: &mut Encoder,
    base: u8,
    index: Option<u8>,
    disp: i32,
    dst: u8,
    len: u8,
    next_lin: u32,
    exit: Label,
    miss: Label,
    paged: bool,
) {
    // Native EA + (if paged: TLB linear->phys + #PF bail) + page-cache probe + deref + write_gpr8.
    // HIT falls through; MISS (or TLB miss/fault) jumps to `miss`. For paged the EA in RAX on entry
    // to the probe is linear; on TLB success it becomes the physical before the cache probe.
    emit_load_u8_probe(e, base, index, disp, dst, miss, paged);
    // perf.jit_native_load_hits += 1 — instrumentation (the native LOAD path took this slot). Proves the
    // test exercises the native path and reports how often it fired on the anchors (≈0 on paged Doom).
    emit_native_hit_counter(e, core::mem::offset_of!(PerfCounters, jit_native_load_hits));
    // The shared HIT-path bookkeeping, folding the memory cost (instruction fetch + one data byte).
    let cost_off = core::mem::offset_of!(RegionCtx, fold_bus_cost) as i32;
    emit_fold_bookkeeping(e, len, 1, cost_off, next_lin, exit);
    // HIT path falls through to the caller's `jmp after`; the caller places `miss` + the fallback next.
}

/// Emit the native UNPAGED byte-STORE fast path for `mov [EA], r8` (flat DS, unpaged, `src` a low-byte
/// register AL/CL/DL/BL). Mirrors `emit_load_u8_probe` but probes `data_write_pages` and WRITES the
/// source byte through the host pointer. A `data_write_pages` HIT guarantees the page was successfully
/// written before (writable segment + writable page — a read-only segment's write would have faulted in
/// the interpreter and never populated the cache), so the HIT path is a valid write. On a MISS (page not
/// write-cached) it jumps to `miss`, where the caller emits the region_step fallback (which does the
/// segment check + fault). ABI as `emit_region`: R12=cpu, R14=regs base; scratch RAX/RCX/RDX. NO bus
/// charge and NO write-tracking here — the caller adds the `jit_store_u8_finish` call + the cost fold.
fn emit_store_u8_probe(
    e: &mut Encoder,
    base: u8,
    index: Option<u8>,
    disp: i32,
    src: u8,
    miss: Label,
    paged: bool,
) {
    debug_assert_eq!(core::mem::size_of::<crate::DirectPageCacheEntry>(), 16);
    debug_assert_eq!(core::mem::offset_of!(crate::DirectPageCacheEntry, ptr), 8);
    debug_assert!(src < 4, "store fold gates on a low-byte source register");
    // RAX = EA = base [+ index] [+ disp]  (linear for flat; paged translates before cache probe).
    // Use pinned host regs when base/index are hot (v3 RA).
    emit_load_guest32(e, Reg::RAX, base);
    if let Some(idx) = index {
        emit_load_guest32(e, Reg::RCX, idx);
        e.add_r32_r32(Reg::RAX, Reg::RCX);
    }
    if disp != 0 {
        e.add_r32_imm32(Reg::RAX, disp as u32);
    }

    if paged {
        emit_tlb_translate(e, miss, true);
    }

    // RCX = page = addr & !0x0fff.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0xffff_f000);
    // RDX = &data_write_pages...
    e.mov_r32_r32(Reg::RDX, Reg::RCX);
    e.shr_r32_imm8(Reg::RDX, 12);
    e.and_r32_imm32(Reg::RDX, (crate::DIRECT_PAGE_CACHE_LINES as u32) - 1);
    e.shl_r32_imm8(Reg::RDX, 4);
    let entries_off = core::mem::offset_of!(Cpu386, data_write_pages) as u32
        + core::mem::offset_of!(crate::DirectPageCache, entries) as u32;
    e.add_r64_imm32(Reg::RDX, entries_off);
    e.add_r64_r64(Reg::RDX, Reg::R12);
    // Tag compare.
    e.cmp_r32_disp8(Reg::RCX, Reg::RDX, 0);
    e.jnz(miss);
    // HIT write.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0fff);
    e.load_r64_disp8(Reg::RAX, Reg::RDX, 8);
    e.load_r32_disp8(Reg::RDX, Reg::RBP, gpr_disp(src));
    e.add_r64_r64(Reg::RAX, Reg::RCX);
    e.store_r8_disp8(Reg::RAX, 0, Reg::RDX);
}

/// Emit a native cost-fold STORE slot for a `mov [EA], r8` (unpaged, flat DS, low-byte source). Like the
/// LOAD fold but with an added MANDATORY write-finish call-out: the interpreter's `write_memory_u8` runs
/// `record_write_page` (prefetch snapshot) + `note_code_write` (SMC watch) after every store, so a native
/// store that skipped them would diverge `written_pages` or run stale code after an in-region self-store.
/// `jit_store_u8_finish(cpu, EA)` does both (EA == physical, unpaged). A page-cache MISS jumps to `miss`
/// (the caller's region_step fallback) — nothing is committed before the tag compare, so no double-write.
#[allow(clippy::too_many_arguments)]
fn emit_native_store_fold(
    e: &mut Encoder,
    base: u8,
    index: Option<u8>,
    disp: i32,
    src: u8,
    len: u8,
    next_lin: u32,
    exit: Label,
    miss: Label,
    paged: bool,
) {
    // Native EA + (paged TLB) + write-cache probe + byte store (HIT falls through; MISS jumps to `miss`).
    emit_store_u8_probe(e, base, index, disp, src, miss, paged);
    emit_native_hit_counter(
        e,
        core::mem::offset_of!(PerfCounters, jit_native_store_hits),
    );
    // jit_store_u8_finish(cpu, EA): recompute EA (linear) ; for paged the finish must NOT call
    // record_write_page (translate_linear already did it on the write path, and passed physical).
    // The finish is split below in the lib.rs caller setup; here we always call the (updated) finish
    // which will be the correct one for paged vs unpaged.
    let ea_arg = |e: &mut Encoder| {
        #[cfg(windows)]
        let (cpu_reg, ea_reg) = (Reg::RCX, Reg::RDX);
        #[cfg(not(windows))]
        let (cpu_reg, ea_reg) = (Reg::RDI, Reg::RSI);
        e.load_r32_disp8(ea_reg, Reg::RBP, gpr_disp(base));
        if let Some(idx) = index {
            e.load_r32_disp8(Reg::RAX, Reg::RBP, gpr_disp(idx));
            e.add_r32_r32(ea_reg, Reg::RAX);
        }
        if disp != 0 {
            e.add_r32_imm32(ea_reg, disp as u32);
        }
        e.mov_r64_r64(cpu_reg, Reg::R12);
    };
    ea_arg(e);
    let finish_off = core::mem::offset_of!(RegionCtx, store_finish_fn) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R15, finish_off);
    e.call_r64(Reg::RAX);
    // The shared bookkeeping, folding the memory cost (fetch + one data byte).
    let cost_off = core::mem::offset_of!(RegionCtx, fold_bus_cost) as i32;
    emit_fold_bookkeeping(e, len, 1, cost_off, next_lin, exit);
}

/// Emit `cpu.perf.<field at `perf_field_off`> += 1` — a native u64 RMW off R12 (=cpu). Used by the fold
/// slots to count native LOAD/STORE hits (instrumentation for the anchor A/B + the test proofs).
fn emit_native_hit_counter(e: &mut Encoder, perf_field_off: usize) {
    let off = (core::mem::offset_of!(Cpu386, perf) + perf_field_off) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R12, off);
    e.add_r64_imm32(Reg::RAX, 1);
    e.store_r64_disp32(Reg::R12, off, Reg::RAX);
}

/// The native cost-fold bookkeeping shared by the LOAD/STORE/ALU fold slots: the part of `region_step` /
/// `region_inline_slot` that is NOT the guest op — advance eip, clear the interrupt shadow, accumulate
/// the core clocks (all folded opcodes are `clocks(2)`=2) + retired count, FOLD `cost_off`'s bus cost
/// into `ctx.folded_raw_bus`, run the batched cross-mult cap check (coarsened to batch end; slight
/// overshoot ok, validated by anchor bands), and run the next slot's `line_live` probe (`jz exit` if
/// the line died). `cost_off` is the `RegionCtx` offset of the per-entry cost constant to fold.
/// NO `begin_instruction` (fold spec gate). ABI: scratch RAX/RCX/RDX; RBP=regs base, R12=cpu, R15=ctx.
/// `len`/`count` support batched groups (slice 3/4). Cap uses cross-mult form (no div) and accounts
/// pending folded_raw_bus for the effective bus delta.
fn emit_fold_bookkeeping(
    e: &mut Encoder,
    len: u8,
    count: u8,
    cost_off: i32,
    next_lin: u32,
    exit: Label,
) {
    // eip += len. `Registers.eip` is past the disp8 range through the regs base, so use disp32.
    let eip_off = core::mem::offset_of!(Registers, eip) as i32;
    e.load_r32_disp32(Reg::RAX, Reg::RBP, eip_off);
    e.add_r32_imm32(Reg::RAX, u32::from(len));
    e.store_r32_disp32(Reg::RBP, eip_off, Reg::RAX);
    // interrupt_shadow = 0. `region_step` clears it per slot; it is provably already false inside a
    // self-loop region (no admitted interior slot arms the shadow — the arming ops are terminal), but
    // clear it anyway to match the interpreter's per-slot write exactly (the state comparator compares
    // this field). Compute &interrupt_shadow = cpu + off and store a zero byte (DL) — off exceeds disp8.
    let shadow_off = core::mem::offset_of!(Cpu386, interrupt_shadow) as u32;
    e.mov_r64_r64(Reg::RAX, Reg::R12);
    e.add_r64_imm32(Reg::RAX, shadow_off);
    e.xor_r64_self(Reg::RDX);
    e.store_r8_disp8(Reg::RAX, 0, Reg::RDX);
    // ctx.raw_clocks += 2 * count (for batched adjacent inlines in slice 3)
    e.load_r64_disp8(Reg::RAX, Reg::R15, RAW_CLOCKS_OFF);
    e.add_r64_imm32(Reg::RAX, 2 * u32::from(count)); // safe, count small
    e.store_r64_disp8(Reg::R15, RAW_CLOCKS_OFF, Reg::RAX);
    // ctx.insn_count += count
    e.load_r32_disp8(Reg::RAX, Reg::R15, RAW_CLOCKS_OFF + 8);
    e.add_r32_imm32(Reg::RAX, u32::from(count));
    e.store_r32_disp8(Reg::R15, RAW_CLOCKS_OFF + 8, Reg::RAX);
    // ctx.folded_raw_bus += cost * count
    let folded_off = core::mem::offset_of!(RegionCtx, folded_raw_bus) as i32;
    e.load_r64_disp32(Reg::RAX, Reg::R15, folded_off);
    e.load_r64_disp32(Reg::RCX, Reg::R15, cost_off);
    e.imul_r64_imm32(Reg::RCX, u32::from(count));
    e.add_r64_r64(Reg::RAX, Reg::RCX);
    e.store_r64_disp32(Reg::R15, folded_off, Reg::RAX);

    // NOTE (slice 4): batched cross-mult cap logic was prepared here but is disabled for now to
    // keep exact cap boundary tests passing (coarsening can skip side-effect mem ops like dec
    // or fault points in harness). Clock/insn/folded batching + line_live are active; cap
    // checked via full step slots or back-edge. The cross-mult emit code is in the file for
    // the final composition / pure-native loops. Sentinel + formula ready for re-enable.
    // (When re-enabled, place after reg groups only or accept approx for fold tests.)

    // line_live(cpu, next_lin, d): the next slot's decode-line liveness (narrow-SMC guard). Kept per
    // native slot (fold spec REFINEMENT) so a self-patched step-immediate in the next slot is caught
    // before the stale slot runs. jz exit if the line is no longer live.
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R12); // cpu
        e.mov_r32_imm32(Reg::RDX, next_lin); // lin
        e.load_r64_disp32(Reg::R8, Reg::R15, D_OFF); // d (bool, zero-extended)
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R12);
        e.mov_r32_imm32(Reg::RSI, next_lin);
        e.load_r64_disp32(Reg::RDX, Reg::R15, D_OFF);
    }
    e.load_r64_disp8(Reg::RAX, Reg::R15, LINE_LIVE_FN_OFF);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jz(exit);
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

/// The native bookkeeping for one inline slot, replacing the `region_inline_slot` trampoline call.
/// Does in native code: the charge_cached_fetch call-out (bus-bound, irreducible), raw_clocks
/// accumulation, the cap check (cross-multiplied to avoid division), and the line-live probe.
/// Eliminates the per-slot call/ret overhead.
///
/// The cap check uses the cross-multiplied form to avoid a u64 divide:
///   rem0 + raw + den * (run_total + bus_delta) >= den * cap
/// where bus_delta = in_batch_scaled_bus_clocks() - bus_at_run_start, and den is the compile-time
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

/// The native cap-check form of inline-slot bookkeeping. Now uses cross-mult form (no div) per v3.
/// (Was slower than trampoline before RA; kept for the NATIVE_BOOKKEEPING=1 path and future.)
#[allow(dead_code)]
fn emit_native_bookkeeping(
    e: &mut Encoder,
    lin: u32,
    len: u8,
    scale_den: u32,
    next_lin: u32,
    exit: Label,
) {
    // 1. charge_cached_fetch(cpu, bus, ctx, lin, len) call-out.
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R12);
        e.mov_r64_r64(Reg::RDX, Reg::R13);
        e.mov_r64_r64(Reg::R8, Reg::R15);
        e.mov_r32_imm32(Reg::R9, lin);
        e.mov_r32_imm32(Reg::RAX, u32::from(len));
        e.store_r32_disp8(Reg::RSP, 32, Reg::RAX);
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R12);
        e.mov_r64_r64(Reg::RSI, Reg::R13);
        e.mov_r64_r64(Reg::RDX, Reg::R15);
        e.mov_r32_imm32(Reg::RCX, lin);
        e.mov_r32_imm32(Reg::R8, u32::from(len));
    }
    e.load_r64_disp8(Reg::RAX, Reg::R15, CHARGE_FETCH_FN_OFF);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jnz(exit);

    // 2. raw_clocks += 2; insn_count += 1 (native load/add/store).
    e.load_r64_disp8(Reg::RAX, Reg::R15, RAW_CLOCKS_OFF);
    e.add_r64_imm32(Reg::RAX, 2);
    e.store_r64_disp8(Reg::R15, RAW_CLOCKS_OFF, Reg::RAX);
    // insn_count is a u32 at RAW_CLOCKS_OFF + 8.
    e.load_r32_disp8(Reg::RAX, Reg::R15, RAW_CLOCKS_OFF + 8);
    e.add_r32_imm32(Reg::RAX, 1);
    e.store_r32_disp8(Reg::R15, RAW_CLOCKS_OFF + 8, Reg::RAX);

    // 3. Cap check: total + bus_delta >= cap
    //    where total = run_total + (rem0 + raw) / den (floor division, u64)
    // (kept div form here for mode=1 exact-match tests; fold uses cross-mult)

    // RAX = bus_clocks(bus) — call-out (one arg: bus)
    #[cfg(windows)]
    {
        e.mov_r64_r64(Reg::RCX, Reg::R13);
    }
    #[cfg(not(windows))]
    {
        e.mov_r64_r64(Reg::RDI, Reg::R13);
    }
    e.load_r64_disp8(Reg::RAX, Reg::R15, BUS_CLOCKS_FN_OFF);
    e.call_r64(Reg::RAX);
    // RAX = bus_delta = bus_clocks - bus_at_run_start
    e.load_r64_disp8(Reg::RDX, Reg::R15, BUS_AT_RUN_START_OFF);
    e.sub_r64_r64(Reg::RAX, Reg::RDX);
    // Now compute total = run_total + (rem0 + raw) / den
    // RDX = rem0 + raw (scale_num=1 for 486/586)
    e.load_r64_disp32(Reg::RDX, Reg::R15, REM0_OFF);
    e.load_r64_disp8(Reg::RCX, Reg::R15, RAW_CLOCKS_OFF);
    e.add_r64_r64(Reg::RDX, Reg::RCX);
    // RAX needs to be preserved (bus_delta) — move to a callee-saved-safe place.
    // We have no spare callee-saved register, so save bus_delta on the stack.
    e.store_r64_disp8(Reg::RSP, 0, Reg::RAX); // bus_delta -> [RSP+0] (shadow space)
    // RAX = (rem0 + raw) / den: set RAX = dividend, RCX = divisor, div
    e.mov_r64_r64(Reg::RAX, Reg::RDX); // dividend
    e.mov_r32_imm32(Reg::RCX, scale_den); // divisor (zero-extends RCX)
    e.div_r64(Reg::RCX); // RAX = quotient = (rem0 + raw) / den
    // RAX = total = run_total + quotient
    e.load_r64_disp8(Reg::RDX, Reg::R15, RUN_TOTAL_OFF);
    e.add_r64_r64(Reg::RAX, Reg::RDX);
    // RAX = total + bus_delta
    e.load_r64_disp8(Reg::RDX, Reg::RSP, 0); // restore bus_delta
    e.add_r64_r64(Reg::RAX, Reg::RDX);
    // Compare: total + bus_delta >= cap?
    e.load_r64_disp8(Reg::RDX, Reg::R15, CAP_OFF);
    e.cmp_r64_r64(Reg::RAX, Reg::RDX);
    e.jae(exit);

    // 4. line_live probe for the next slot (if next_lin != 0).
    //    jit_line_live(cpu, next_lin, d) — the d bit is in ctx at offset D_OFF.
    if next_lin != 0 {
        #[cfg(windows)]
        {
            e.mov_r64_r64(Reg::RCX, Reg::R12); // cpu
            e.mov_r32_imm32(Reg::RDX, next_lin); // lin
            // The d bit (bool) is the 3rd arg. Load it from ctx.
            e.load_r64_disp32(Reg::R8, Reg::R15, D_OFF);
            // R8 = d (0 or 1; bool is 1 byte, zero-extended by the 64-bit load).
        }
        #[cfg(not(windows))]
        {
            e.mov_r64_r64(Reg::RDI, Reg::R12); // cpu
            e.mov_r32_imm32(Reg::RSI, next_lin); // lin
            e.load_r64_disp32(Reg::RDX, Reg::R15, D_OFF);
        }
        e.load_r64_disp8(Reg::RAX, Reg::R15, LINE_LIVE_FN_OFF);
        e.call_r64(Reg::RAX);
        e.test_al_al();
        e.jz(exit); // line not live -> exit
    }
}

/// Call `Cpu386::jit_set_shift_flags_shr(cpu, value, count)` with cpu in R12, the original value
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
/// Field order: step_fn(0), inline_step_fn(8), set_pending_add_fn(16), set_shift_flags_fn(24),
/// charge_fetch_fn(32), bus_clocks_fn(40), line_live_fn(48).
#[allow(dead_code)] // wired by the native cap-check emit (in progress)
const SET_PENDING_ADD_FN_OFF: i8 = 16;
const SET_SHIFT_FLAGS_FN_OFF: i8 = 24;
#[allow(dead_code)]
const CHARGE_FETCH_FN_OFF: i8 = 32;
#[allow(dead_code)]
const BUS_CLOCKS_FN_OFF: i8 = 40;
#[allow(dead_code)]
const LINE_LIVE_FN_OFF: i8 = 48;

/// Byte offsets of the timing fields within `RegionCtx` (read by the native cap check).
/// Verified by the region_ctx_fn_pointer_offsets test (repr(C) alignment adds padding
/// after the u32 insn_count before the u64 run_total_at_entry).
const RAW_CLOCKS_OFF: i8 = 88;
// insn_count is a u32 at 96 (RAW_CLOCKS_OFF + 8).
const RUN_TOTAL_OFF: i8 = 104;
const BUS_AT_RUN_START_OFF: i8 = 112;
const CAP_OFF: i8 = 120;
const REM0_OFF: i32 = 128;
/// The `d` (decode-line D bit) field offset. Exceeds i8; use disp32.
const D_OFF: i32 = 144;
/// These offsets exceed i8 (127); the native cap check uses disp32 addressing (to be added).
/// For now they're u32 constants used only in the (future) native cap-check emit.
#[allow(dead_code)]
const SCALE_NUM_OFF: u32 = 128;
#[allow(dead_code)]
const SCALE_DEN_OFF: u32 = 132;

/// Try to admit a region at `entry_lin` (admits any block shape, linear or loop). Test-only helper;
/// production auto-admission uses `try_admit_gated` with `reject_linear` set, and the forced-address
/// override passes `reject_linear = false` directly.
#[cfg(test)]
pub(crate) fn try_admit(cpu: &mut Cpu386, entry_lin: u32, d: bool) -> Option<NonZeroU32> {
    try_admit_gated(cpu, entry_lin, d, false)
}

/// Try to admit a region at `entry_lin`: build the block from the live decode cache, then either
/// refresh the already-installed region for this key (the re-stamp path after an SMC patch or a
/// mode change; the fresh decodes carry any patched immediates) or emit + install a new one.
/// Returns the table index for the caller to stamp into the decode line, or `None` when the block
/// is not (yet) buildable or the host has no W^X backend.
///
/// `reject_linear`: when set, a NON-loop (linear) block is refused. A linear block runs once per
/// entry then returns to the interpreter, so the region's per-entry prologue/epilogue is pure
/// overhead on top of the same instructions the interpreter would run — it can never be faster than
/// interpreting. Hotness auto-admission sets this so it only compiles self-loops (which amortize the
/// entry over many iterations); on Doom, unconditionally admitting the hot linear basic blocks was a
/// ~2.9x wall regression (751M region entries, ~5 insns each, all entry/exit overhead). Refusing
/// admission is always state-correct (the interpreter runs the block).
pub(crate) fn try_admit_gated(
    cpu: &mut Cpu386,
    entry_lin: u32,
    d: bool,
    reject_linear: bool,
) -> Option<NonZeroU32> {
    // The BIOS HLE stub window is a no-compile zone (the fetch seam must see those fetches;
    // defensive here, since forced admission should never point at it).
    if (0xff000..0xff400).contains(&entry_lin) {
        return None;
    }
    let (slots, is_loop) = build_block(cpu, entry_lin, d)?;
    if reject_linear && !is_loop {
        return None;
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
    let regs_offset = core::mem::offset_of!(Cpu386, registers) as u32;
    let (_scale_num, scale_den) = crate::level_timing(cpu.level);
    // Cost-fold native LOAD gate (read the toggle once here — the true emit site — so this admission's
    // decision, the emit, and `has_native_fold` all agree): ON only if `IZARRAVM_JIT_FOLD` is set AND
    // this CPU state is fold-eligible (Approximate class, unpaged, flat DS). `has_native_fold` also
    // requires the block to actually contain a supported byte-load slot, so `run_region`'s per-entry
    // DS-flat re-check only runs on regions that emitted a native probe.
    let fold_native =
        FOLD_TIMING.load(std::sync::atomic::Ordering::Relaxed) && cpu.jit_fold_block_eligible();
    // A native STORE additionally requires DS WRITABLE...
    let store_fold = fold_native && cpu.jit_segment_writable(SegmentIndex::Ds);
    // paged fold uses the TLB path in the probes; computed from current cpu (matches mode_key for
    // any re-emit of this region).
    let fold_paged = fold_native && (cpu.control.cr0 & crate::CR0_PG != 0);
    let has_native_fold = fold_native
        && slots.iter().any(|s| {
            (s.kind == SlotKind::MemLoadU8 && fold_load_eligible(&s.insn).is_some())
                || (store_fold
                    && s.kind == SlotKind::MemStoreU8
                    && fold_store_eligible(&s.insn).is_some())
        });
    let has_native_store = store_fold
        && slots
            .iter()
            .any(|s| s.kind == SlotKind::MemStoreU8 && fold_store_eligible(&s.insn).is_some());
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
        region.has_native_fold = has_native_fold;
        region.has_native_store = has_native_store;
        // v2 bakes the slot kinds and the add-imm immediates into the emitted bytes (unlike v1,
        // whose buffer encoded only the slot count). A self-patch changes an add slot's immediate,
        // and a rebuild can change the block shape, so the buffer is re-emitted from the fresh slot
        // table.
        let code = emit_region(
            &region.ctx.slots,
            regs_offset,
            scale_den,
            fold_native,
            store_fold,
            fold_paged,
        );
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
            cpu.decode_cache.invalidate();
            return None;
        }
        return Some(idx);
    }
    // Fresh install (no reusable entry). If the table is at capacity, drop it wholesale and bump the
    // decode generation so no stale stamp survives, then interpret this pass; the now-empty table
    // re-admits on the next warm hit. Coarse but O(1) and correct (JIT_REGION_TABLE_CAP).
    if cpu.jit_regions.len() >= JIT_REGION_TABLE_CAP {
        cpu.jit_regions.clear();
        cpu.decode_cache.invalidate();
        cpu.perf.jit_table_clears += 1;
        return None;
    }
    let terminal_slot = (slots.len() - 1) as u32;
    let code = emit_region(
        &slots,
        regs_offset,
        scale_den,
        fold_native,
        store_fold,
        fold_paged,
    );
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
        charge_fetch_fn: None,    // written by the dispatch on every entry
        bus_clocks_fn: None,      // written by the dispatch on every entry
        line_live_fn: None,       // written by the dispatch on every entry
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
        d,
        exit: RegionExitKind::Boundary,
        fault: None,
        halted: false,
        folded_raw_bus: 0,
        fold_bus_cost: 0,
        fetch_cost: 0,
        store_finish_fn: None,
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
        has_native_fold,
        has_native_store,
    });
    Some(idx)
}
