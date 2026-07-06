//! Recognizes Doom's R_DrawColumn inner loop (the 15-instruction, 51-byte, 2-pixel-unrolled
//! column rasterizer at linear 0x473DF8 in the shipped DOS build) from the decode cache, and
//! compiles it as a loop-region: emitted native code that chains one `region_step` call per
//! instruction slot with a native back-edge. This is intentionally a pattern match against ONE
//! specific shape (spike 4 proves the machinery); coverage scales by adding shape tables, not by
//! generalizing this one.
//!
//! ## What the matcher must guarantee (the region's admission invariants)
//!
//! The step function executes each slot through the interpreter's own dispatch, so per-slot
//! SEMANTICS need no validation here. What the matcher vouches for is the region's control-flow
//! and environment assumptions:
//! - Straight line: every slot but the last falls through to the next; the last is a rel8 Jcc
//!   whose taken target is exactly the entry (rel == -span), so "taken" is the native back-edge
//!   and "not taken" resumes interpretation at the fall-through.
//! - No IF writers, no HLT, no TSC/MSR readers, no far transfers, no port I/O in the opcode
//!   table. These make the region's deferred accounting sound: `can_take_interrupt` cannot
//!   transition inside, and nothing reads `elapsed_clocks` mid-region. A future shape table must
//!   preserve this exclusion list (`continuable` alone does NOT: STI is continuable).
//! - No in-loop store may alias the region's own code bytes. The back-edge re-probes only the
//!   entry slot's line, and the staleness epoch is checked at entry, not per iteration; a shape
//!   that patches an EARLIER slot from inside the loop would re-run the stale slot snapshot on
//!   the next iteration. The drawcolumn stores target the framebuffer and the column counter,
//!   never the 51 code bytes (the self-patch comes from setup code outside the loop).
//! - Every slot's decode is live in the cache (generation-current), unprefixed, `continuable`,
//!   and stays inside its 4 KB page, mirroring the run loop's own continuation gate.
//!
//! ## Self-patched immediates
//!
//! The rasterizer's setup code rewrites the two `add ebp,imm32` step values in place before
//! column batches. Each patch bumps the decode generation (SMC watch), which kills the stamped
//! line and with it the region stamp; the interpreter then re-decodes the loop on its next pass
//! and admission re-runs this matcher against the FRESH decodes. `try_admit` finds the existing
//! region and replaces its slot table wholesale, so the new immediates ride along in
//! `DecodedInsn.imm` and the emitted buffer (which encodes only the slot count) is reused. The
//! two imm32 slots therefore have `imm: None` (any value matches); the structurally fixed
//! immediates (the shift counts, the destination stride) are pinned.

use std::num::NonZeroU32;

use super::encoder::{Encoder, Label, Reg};
use super::exec_mem::ExecutableBuffer;
use super::region::CompiledRegion;
use super::step::{RegionCtx, RegionEntryFn, RegionExitKind, Slot, SlotKind};
use crate::{Cpu386, DecodedInsn, Prefixes};

/// One slot's structural requirements. `modrm` is (mode, reg, rm); `imm` pins a structural
/// immediate (`None` = any value, the self-patched or don't-care slots).
struct SlotSpec {
    opcode: u16,
    len: u8,
    modrm: Option<(u8, u8, u8)>,
    imm: Option<u32>,
}

#[allow(non_snake_case)]
fn S(opcode: u16, len: u8, modrm: Option<(u8, u8, u8)>, imm: Option<u32>) -> SlotSpec {
    SlotSpec {
        opcode,
        len,
        modrm,
        imm,
    }
}

/// The R_DrawColumn inner loop, as disassembled from the mid-demo census dump (kickoff brief):
/// two unrolled texture-step/plot pairs, the colormap double-indirection, the in-memory count
/// DEC, and the rel8 back-edge. The memory operands' exact base/index registers are NOT pinned
/// here: execution follows the stored `DecodedInsn`, so a same-structure loop with different
/// registers would compile and run just as correctly.
#[rustfmt::skip]
fn drawcolumn_shape() -> [SlotSpec; 15] {
    [
        S(0x8b, 2, Some((3, 1, 5)), None),       // mov ecx,ebp
        S(0x81, 6, Some((3, 0, 5)), None),       // add ebp,imm32   (self-patched)
        S(0x88, 2, Some((0, 0, 7)), None),       // mov [edi],al
        S(0xc1, 3, Some((3, 5, 1)), Some(25)),   // shr ecx,25
        S(0x8b, 2, Some((3, 2, 5)), None),       // mov edx,ebp
        S(0x81, 6, Some((3, 0, 5)), None),       // add ebp,imm32   (self-patched)
        S(0x88, 3, Some((1, 3, 7)), None),       // mov [edi+0x50],bl
        S(0xc1, 3, Some((3, 5, 2)), Some(25)),   // shr edx,25
        S(0x8a, 3, Some((0, 0, 4)), None),       // mov al,[esi+ecx]
        S(0x81, 6, Some((3, 0, 7)), Some(0xa0)), // add edi,0xa0
        S(0x8a, 3, Some((0, 3, 4)), None),       // mov bl,[esi+edx]
        S(0xff, 6, Some((0, 1, 5)), None),       // dec dword [disp32]
        S(0x8a, 2, Some((0, 0, 0)), None),       // mov al,[eax]
        S(0x8a, 2, Some((0, 3, 3)), None),       // mov bl,[ebx]
        S(0x75, 2, None, None),                  // jnz -> entry (rel checked below)
    ]
}

/// Classify a matched instruction into its emitted-code strategy. The register-only mov/add/shr
/// forms (modrm mode 3) inline natively; everything else (memory operands, the back-edge Jcc) goes
/// through the v1 per-slot step. The gpr indices and immediates are read from the captured decode
/// so self-patched immediates (the add-imm slots) stay current across re-stamps.
///
/// Only the exact opcodes the drawcolumn shape admits reach here (the matcher already verified
/// opcode + modrm), so the classification is exhaustive over the shape; a slot that matched the
/// shape but is not one of the three inline-able register forms falls through to Memory.
fn classify_slot(insn: &DecodedInsn) -> SlotKind {
    let Some(m) = insn.modrm else {
        // The only modrm-less slot in the shape is the final rel8 Jnz back-edge.
        return SlotKind::BackEdge;
    };
    // mode 3 = register-only (no memory operand). The three inline-able opcode classes:
    // 0x8B mov r32,r32 (reg=dst, rm=src); 0x81 /0 add r32,imm32 (reg=0, rm=dst); 0xC1 /5 shr
    // r32,imm8 (reg=5, rm=dst). All are 32-bit operand size in this loop.
    if m.mode == 3 {
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

/// Walk the decode cache from `entry_lin` and match the shape. `None` when any slot is cold
/// (the interpreter warms it naturally; admission just retries later) or does not match.
pub(crate) fn match_drawcolumn(cpu: &Cpu386, entry_lin: u32, d: bool) -> Option<Vec<Slot>> {
    let shape = drawcolumn_shape();
    let mut slots = Vec::with_capacity(shape.len());
    let mut lin = entry_lin;
    for spec in &shape {
        let insn = cpu.decode_cache.get(lin, d)?;
        if insn.opcode != spec.opcode || insn.len != spec.len {
            return None;
        }
        // Unprefixed and continuable, like the run loop's own gate; a page-crossing slot would
        // fail that gate too, so the region must not cover it.
        if insn.prefixes != Prefixes::default() || !insn.continuable {
            return None;
        }
        if (lin & 0xfff) + u32::from(insn.len) > 0x1000 {
            return None;
        }
        if let Some((mode, reg, rm)) = spec.modrm {
            let m = insn.modrm?;
            if m.mode != mode || m.reg != reg || m.rm != rm {
                return None;
            }
        }
        if let Some(imm) = spec.imm {
            if insn.imm != imm {
                return None;
            }
        }
        let kind = classify_slot(&insn);
        slots.push(Slot { insn, lin, kind });
        lin = lin.wrapping_add(u32::from(insn.len));
    }
    // The back-edge must land exactly on the entry. rel8 was sign-extended into `imm` at
    // decode; eip-space arithmetic (target = eip-after-jnz + rel) reduces to rel == -span.
    let span = lin.wrapping_sub(entry_lin);
    let jnz = &slots[shape.len() - 1].insn;
    if jnz.imm as i32 != -(span as i32) {
        return None;
    }
    Some(slots)
}

/// Total bytes the prologue reserves below the five pushed callee-saved registers, sized so every
/// `call` site sees RSP % 16 == 0. At entry RSP % 16 == 8 (after the return-address push); 5 pushes
/// subtract 40 (8 mod 16), landing RSP at 0 mod 16; subtracting a multiple of 16 keeps it there, so
/// 32 bytes (the Win64 shadow space) is exactly right. v1 used 40 with 4 pushes (4 pushes left RSP
/// at 8 mod 16, so +40 was needed to reach 0); the fifth callee-saved (R14, the regs pointer) flips
/// the parity and 32 is now correct. Harmless on SysV64 (no shadow space, but the alignment holds).
const STACK_RESERVE: u32 = 32;

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
fn emit_region(slots: &[Slot], regs_offset: u32) -> Vec<u8> {
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
        match slot.kind {
            SlotKind::RegMov { dst, src } => {
                // Native: load gpr[src] into EAX, store EAX into gpr[dst]. No flags, no helper.
                // gpr[i] is at [R14 + 4*i] (R14 = regs base).
                e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(src));
                e.store_r32_disp8(Reg::R14, gpr_disp(dst), Reg::RAX);
                // Then the inline bookkeeping call (fetch charge + eip advance + break checks).
                emit_inline_bookkeeping_call(&mut e, k32, exit);
            }
            SlotKind::RegAddImm { dst, imm } => {
                // Native: load gpr[dst] into EAX (this is `a`, the operand for the flag helper),
                // add imm32, store result back to gpr[dst]. Then call jit_set_pending_add(cpu, a,
                // imm) with the original value (still in a second scratch) and the immediate.
                e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(dst)); // RAX = a (old value)
                // ECX = a (save for the helper arg; the add clobbers EAX).
                e.mov_r32_r32(Reg::RCX, Reg::RAX);
                e.add_r32_imm32(Reg::RAX, imm); // RAX = result
                e.store_r32_disp8(Reg::R14, gpr_disp(dst), Reg::RAX); // write gpr[dst]
                // Call jit_set_pending_add(cpu=R12, a=ECX, imm). imm is a compile-time constant;
                // pass it in the third arg register.
                emit_set_pending_add_call(&mut e, imm);
                emit_inline_bookkeeping_call(&mut e, k32, exit);
            }
            SlotKind::RegShrImm { dst, count } => {
                // Native: load gpr[dst] into EAX (the original value, for CF), shr by count, store
                // result back. Then call jit_set_shift_flags_shr(cpu, value=EAX-saved, count).
                e.load_r32_disp8(Reg::RAX, Reg::R14, gpr_disp(dst)); // RAX = original value
                e.mov_r32_r32(Reg::RCX, Reg::RAX); // ECX = original (helper arg)
                e.shr_r32_imm8(Reg::RAX, count); // RAX = result
                e.store_r32_disp8(Reg::R14, gpr_disp(dst), Reg::RAX);
                emit_set_shift_flags_shr_call(&mut e, count);
                emit_inline_bookkeeping_call(&mut e, k32, exit);
            }
            SlotKind::Memory | SlotKind::BackEdge => {
                // The full v1 per-slot step (decode dispatch + bus-bound operand resolution).
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

/// Reload cpu/bus/ctx and the slot index, then `call [ctx+8]` (region_inline_slot). The inline step
/// pointer is loaded fresh per slot from `[ctx + 8]` (no spare callee-saved register holds it
/// across the inline body). `exit` is the region exit label; the test+jnz after the call returns
/// there on STOP.
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
    // Load inline_step_fn from [ctx+8] into a scratch (RAX is dead here: the native op already
    // committed its result to gpr) and call it indirectly.
    e.load_r64_disp8(Reg::RAX, Reg::R15, 8);
    e.call_r64(Reg::RAX);
    e.test_al_al();
    e.jnz(exit);
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

/// Byte offsets of the two flag-helper fn pointers within `RegionCtx` (set by the dispatch, loaded
/// by the inline emit). These follow `step_fn` (off 0) and `inline_step_fn` (off 8), so they start
/// at 16. Each `Option<unsafe extern "C" fn>` is one pointer wide (8 bytes) under the null-pointer
/// optimization.
const SET_PENDING_ADD_FN_OFF: i8 = 16;
const SET_SHIFT_FLAGS_FN_OFF: i8 = 24;

/// Try to admit a region at `entry_lin`: match the shape against the live decode cache, then
/// either refresh the already-installed region's slot table (the re-stamp path after an SMC
/// patch; the self-patched immediates ride along in the fresh decodes) or emit + install a new
/// one. Returns the table index for the caller to stamp into the decode line, or `None` when
/// the shape does not (yet) match or the host has no W^X backend.
pub(crate) fn try_admit(cpu: &mut Cpu386, entry_lin: u32, d: bool) -> Option<NonZeroU32> {
    // The BIOS HLE stub window is a no-compile zone (the fetch seam must see those fetches;
    // defensive here, since forced admission should never point at it).
    if (0xff000..0xff400).contains(&entry_lin) {
        return None;
    }
    let slots = match_drawcolumn(cpu, entry_lin, d)?;
    let last = &slots[slots.len() - 1];
    let span = last.lin.wrapping_add(u32::from(last.insn.len)) - entry_lin;
    // Physical span from the entry line (matcher-warmed, single page so contiguity holds);
    // narrow SMC kills inside it stale the slot table via the epoch.
    let phys_lo = cpu.decode_cache.line_phys_start(entry_lin, d)?;
    let phys_hi = phys_lo + (span - 1);
    let epoch = cpu.decode_cache.jit_smc_epoch;
    let regs_offset = core::mem::offset_of!(Cpu386, registers) as u32;
    if let Some(idx) = cpu.jit_regions.find(entry_lin, d) {
        let region = cpu
            .jit_regions
            .get_mut(idx)
            .expect("find returned a live index");
        region.ctx.slots = slots;
        region.phys_lo = phys_lo;
        region.phys_hi = phys_hi;
        region.valid_epoch = epoch;
        // v2 bakes the slot kinds and the add-imm immediates into the emitted bytes (unlike v1,
        // whose buffer encoded only the slot count). A self-patch changes an add slot's immediate,
        // so the buffer must be re-emitted from the fresh slot table. The kinds themselves are
        // shape-fixed (the matcher re-verified them), but the immediates and the regs offset are
        // re-read here for correctness.
        let code = emit_region(&region.ctx.slots, regs_offset);
        if let Some(buf) = ExecutableBuffer::new(&code) {
            // SAFETY: same transmute proof as the fresh-admission path below; `code` was produced
            // by emit_region to exactly the RegionEntryFn convention.
            region.entry =
                unsafe { std::mem::transmute::<*const u8, RegionEntryFn>(buf.entry_ptr()) };
            region.buf = buf;
        } else {
            // W^X alloc failed (unsupported host): drop the region so admission does not point at
            // stale emitted bytes. The caller treats None as "not admitted" and interprets instead.
            cpu.jit_regions.clear();
            return None;
        }
        return Some(idx);
    }
    let jnz_slot = (slots.len() - 1) as u32;
    let code = emit_region(&slots, regs_offset);
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
        slots,
        jnz_slot,
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
    });
    Some(idx)
}
