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
use crate::{Cpu386, DecodeGroup, DecodedInsn, DecodedOperand, OperandSize, Prefixes};

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

/// Emit the region chain for `slots`: pin cpu/bus/ctx in R12/R13/R15 and the two step functions
/// in RBX (Memory/BackEdge) and R14 (inline register-only), then per slot either inline the guest
/// op natively (mov r,r / add r,imm / shr r,imm against gpr[] + a flag-helper call, followed by the
/// inline bookkeeping call) or, for Memory/BackEdge slots, re-load the args and call the full v1
/// step. After the final slot (the back-edge Jcc's step returns 0 only when taken) an unconditional
/// `jmp` closes the native loop.
///
/// `regs_offset` is `offset_of!(Cpu386, registers)`, baked in so the inline slots address `gpr[]`
/// as `[cpu + regs_offset + 4*i]` from the cpu pointer in R12. The emitted bytes depend on the slot
/// kinds and their baked immediates, so the buffer is re-emitted on every fresh admission (the
/// re-stamp path refreshes the slot table; the next fresh admission re-reads the immediates from
/// the fresh decodes).
///
/// TEMPLATE ABI (the contract a native slot template emits against; win64 + SysV64):
/// - PINNED, do not clobber: R12=cpu, R13=bus, R15=ctx, R14=regs-base (=cpu+regs_offset), RBX=step_fn.
/// - SCRATCH, free to use: RAX/RCX/RDX (volatile). No other host reg is safe across a call-out.
/// - Guest gpr[i] lives at `[R14 + 4*i]` (`gpr_disp`); read/write it there, write-through (no
///   residency yet - that is a later round, which will free some pins).
/// - EARLY EXIT: a slot that must leave the block (fault or run boundary) jumps to the shared `exit`
///   label. Today only Memory/BackEdge slots exit, via the step fn's nonzero return + `jnz exit`; a
///   faulting NATIVE template (the Round 3 memory fast path) will jump to `exit` after spilling, per
///   the re-plan's fault rule. A reg-only template never faults and never exits mid-body.
fn emit_region(slots: &[Slot], regs_offset: u32, scale_den: u32) -> Vec<u8> {
    let mode = NATIVE_BOOKKEEPING.load(std::sync::atomic::Ordering::Relaxed);
    let mut e = Encoder::new();
    e.push(Reg::RBX);
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
    e.load_r64_disp8(Reg::RBX, Reg::R15, 0); // ctx.step_fn (repr(C), first field)
    e.load_r64_disp8(Reg::R14, Reg::R15, 8); // ctx.inline_step_fn (second field)
    // R14 is reused below as the regs pointer for inline gpr access, so move inline_step_fn into a
    // caller-saved scratch that survives across the inline body. We have no spare callee-saved
    // register after RBX/R12/R13/R14/R15, so load inline_step_fn fresh per inline slot from ctx+8.
    // Compute the regs base into R14 = cpu + regs_offset (regs_offset is 0 today, so this is just a
    // copy; the add keeps it correct if Cpu386's layout ever shifts, tracked by the offset guard).
    e.mov_r64_r64(Reg::R14, Reg::R12);
    if regs_offset != 0 {
        e.add_r64_imm32(Reg::R14, regs_offset);
    }

    let loop_top = e.label();
    let exit = e.label();
    e.place(loop_top);
    for (k, slot) in slots.iter().enumerate() {
        let k32 = k as u32;
        // The next slot's linear address, for the native path's line-live probe. Inline slots are
        // never the terminal slot, so `k+1` always exists here.
        let next_lin = slots.get(k + 1).map(|s| s.lin).unwrap_or(0);
        let bookkeeping = |e: &mut Encoder| {
            if mode == 1 {
                emit_native_bookkeeping(e, slot.lin, slot.insn.len, scale_den, next_lin, exit);
            } else {
                emit_inline_bookkeeping_call(e, k32, exit);
            }
        };
        match slot.kind {
            SlotKind::RegMov { dst, src } => {
                e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(src));
                e.store_r32_disp8(Reg::R14, gpr_disp(dst), Reg::RAX);
                bookkeeping(&mut e);
            }
            SlotKind::RegAddImm { dst, imm } => {
                e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(dst));
                e.mov_r32_r32(Reg::RCX, Reg::RAX);
                e.add_r32_imm32(Reg::RAX, imm);
                e.store_r32_disp8(Reg::R14, gpr_disp(dst), Reg::RAX);
                emit_set_pending_add_call(&mut e, imm);
                bookkeeping(&mut e);
            }
            SlotKind::RegShrImm { dst, count } => {
                e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(dst));
                e.mov_r32_r32(Reg::RCX, Reg::RAX);
                e.shr_r32_imm8(Reg::RAX, count);
                e.store_r32_disp8(Reg::R14, gpr_disp(dst), Reg::RAX);
                emit_set_shift_flags_shr_call(&mut e, count);
                bookkeeping(&mut e);
            }
            SlotKind::Memory
            | SlotKind::BackEdge
            | SlotKind::MemLoadU8
            | SlotKind::MemStoreU8
            | SlotKind::MemLoadSized
            | SlotKind::MemStoreSized => {
                emit_full_step_call(&mut e, k32);
                e.test_al_al();
                e.jnz(exit);
            }
        }
    }
    e.jmp(loop_top);
    e.place(exit);
    e.add_r64_imm32(Reg::RSP, STACK_RESERVE);
    e.pop(Reg::R15);
    e.pop(Reg::R14);
    e.pop(Reg::R13);
    e.pop(Reg::R12);
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

/// Emit the native UNPAGED byte-load fast path for `mov r8, [EA]`, where EA is a flat-DS (base 0)
/// address so linear == physical: `[base]` or `[base + index]` (SIB scale 1) plus a `disp`. The
/// caller gates this on unpaged + flat DS + scale 1 (else it must emit the interpreter fallback).
///
/// ABI (as `emit_region`): R12 = cpu, R14 = regs base (cpu + regs_offset); scratch RAX/RCX/RDX. On a
/// page-cache HIT it derefs the CPU-side `data_read_pages` host pointer and writes the loaded byte into
/// `gpr[dst]`'s byte lane (the `write_gpr8` semantics); on a MISS (the physical page is not in the
/// cache) it jumps to `miss`, where the caller emits the identical interpreter leaf. No bus charge is
/// emitted here - the cost-fold accounts the fetch + data clocks separately.
#[allow(dead_code)] // proven in isolation by native_load_probe_reads_the_right_byte; wired next
pub(crate) fn emit_load_u8_probe(
    e: &mut Encoder,
    base: u8,
    index: Option<u8>,
    disp: i32,
    dst: u8,
    miss: Label,
) {
    // The emitted deref hardcodes the entry stride (shl 4 == *16) and the ptr field offset (+8); pin
    // the layout it assumes so a struct change fails loudly here instead of reading a wrong pointer.
    debug_assert_eq!(core::mem::size_of::<crate::DirectPageCacheEntry>(), 16);
    debug_assert_eq!(
        core::mem::offset_of!(crate::DirectPageCacheEntry, ptr),
        8,
        "the deref loads entry.ptr from [entry+8]"
    );

    // RAX = EA = base [+ index] [+ disp]  (linear == physical for an unpaged flat-DS access).
    e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(base));
    if let Some(idx) = index {
        e.load_r32_disp8(Reg::RCX, Reg::R14, gpr_disp(idx));
        e.add_r32_r32(Reg::RAX, Reg::RCX);
    }
    if disp != 0 {
        e.add_r32_imm32(Reg::RAX, disp as u32);
    }
    // RCX = page = EA & !0x0fff.
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
    // HIT: offset = EA & 0x0fff; ptr = entry.ptr ([RDX+8]); byte = *(ptr + offset); write gpr8.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0fff);
    e.load_r64_disp8(Reg::RAX, Reg::RDX, 8);
    e.movzx_r32_byte_sib(Reg::RAX, Reg::RAX, Reg::RCX);
    e.store_r8_disp8(Reg::R14, gpr8_disp(dst), Reg::RAX);
}

/// Reload cpu/bus/ctx and the slot index, then `call rbx` (the full region_step). Used by Memory
/// and BackEdge slots.
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
    e.call_r64(Reg::RBX);
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

/// The native cap-check form of inline-slot bookkeeping (implemented but not used: the native
/// arithmetic is ~14s slower than the trampoline on Doom 8G because the Rust compiler optimizes
/// the trampoline's register usage better than hand-emitted loads/stores). Kept for the future
/// register-allocation effort where guest values in host registers will change the cost balance.
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

/// Call `Cpu386::jit_set_pending_add(cpu, a, b)` with cpu in R12, `a` (the old gpr value) already
/// in ECX, and `b` = `imm` loaded into the next arg register. Saves/restores the caller-saved
/// scratch around the call so the inline bookkeeping call's arg setup is undisturbed.
fn emit_set_pending_add_call(e: &mut Encoder, imm: u32) {
    // The caller put the original gpr value (`a`) in ECX. Move it to its arg register BEFORE
    // loading cpu into RCX (which would clobber it). Win64: arg0=RCX(cpu), arg1=RDX(a), arg2=R8(b).
    // SysV: arg0=RDI(cpu), arg1=RSI(a), arg2=RDX(b).
    #[cfg(windows)]
    {
        e.mov_r32_r32(Reg::RDX, Reg::RCX); // a -> RDX (arg1)
        e.mov_r64_r64(Reg::RCX, Reg::R12); // cpu -> RCX (arg0)
        e.mov_r32_imm32(Reg::R8, imm); // imm -> R8 (arg2)
    }
    #[cfg(not(windows))]
    {
        e.mov_r32_r32(Reg::RSI, Reg::RCX); // a -> RSI (arg1)
        e.mov_r64_r64(Reg::RDI, Reg::R12); // cpu -> RDI (arg0)
        e.mov_r32_imm32(Reg::RDX, imm); // imm -> RDX (arg2)
    }
    // The helper is a Rust method; the emitter cannot address it by offset, so the dispatch stores
    // a raw fn pointer in ctx and we load+call it indirectly.
    e.load_r64_disp8(Reg::RAX, Reg::R15, SET_PENDING_ADD_FN_OFF);
    e.call_r64(Reg::RAX);
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
        // v2 bakes the slot kinds and the add-imm immediates into the emitted bytes (unlike v1,
        // whose buffer encoded only the slot count). A self-patch changes an add slot's immediate,
        // and a rebuild can change the block shape, so the buffer is re-emitted from the fresh slot
        // table.
        let code = emit_region(&region.ctx.slots, regs_offset, scale_den);
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
    let code = emit_region(&slots, regs_offset, scale_den);
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
    });
    Some(idx)
}
