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

use super::encoder::{Encoder, Reg};
use super::exec_mem::ExecutableBuffer;
use super::region::CompiledRegion;
use super::step::{RegionCtx, RegionEntryFn, RegionExitKind, Slot};
use crate::{Cpu386, Prefixes};

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
        slots.push(Slot { insn, lin });
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

/// Total bytes the prologue reserves below the four pushed callee-saved registers: 32 bytes of
/// Win64 shadow space plus 8 pad bytes so every `call` site sees RSP % 16 == 0 (at entry
/// RSP % 16 == 8 after the return-address push; 4 pushes make 40 more, still 8 mod 16; +40
/// lands on 0). Harmless on SysV64.
const STACK_RESERVE: u32 = 40;

/// Emit the region chain for `n_slots` slots: pin cpu/bus/ctx in R12/R13/R15, load the step
/// function from `[ctx + 0]` into RBX, then per slot re-load the three pointer args plus the
/// slot index and `call rbx; test al,al; jnz exit`. Status 0 falls through; after the final
/// slot (the back-edge Jcc's step returns 0 only when taken) an unconditional `jmp` closes the
/// native loop. The emitted bytes depend on nothing but `n_slots`, which is what makes the
/// buffer reusable across re-stamps.
fn emit_region(n_slots: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    e.push(Reg::RBX);
    e.push(Reg::R12);
    e.push(Reg::R13);
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

    let loop_top = e.label();
    let exit = e.label();
    e.place(loop_top);
    for k in 0..n_slots {
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
        e.test_al_al();
        e.jnz(exit);
    }
    e.jmp(loop_top);
    e.place(exit);
    e.add_r64_imm32(Reg::RSP, STACK_RESERVE);
    e.pop(Reg::R15);
    e.pop(Reg::R13);
    e.pop(Reg::R12);
    e.pop(Reg::RBX);
    e.ret();
    e.finish()
}

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
    if let Some(idx) = cpu.jit_regions.find(entry_lin, d) {
        let region = cpu
            .jit_regions
            .get_mut(idx)
            .expect("find returned a live index");
        region.ctx.slots = slots;
        region.phys_lo = phys_lo;
        region.phys_hi = phys_hi;
        region.valid_epoch = epoch;
        return Some(idx);
    }
    let jnz_slot = (slots.len() - 1) as u32;
    let code = emit_region(slots.len() as u32);
    let buf = ExecutableBuffer::new(&code)?;
    // SAFETY: `code` was produced by `emit_region` to exactly the `RegionEntryFn` calling
    // convention (alignment proof at STACK_RESERVE); `entry_ptr` stays valid for `buf`'s life,
    // and `buf` lives in the CompiledRegion beside the fn pointer.
    let entry: RegionEntryFn =
        unsafe { std::mem::transmute::<*const u8, RegionEntryFn>(buf.entry_ptr()) };
    let ctx = Box::new(RegionCtx {
        step_fn: None, // written by the dispatch on every entry
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
