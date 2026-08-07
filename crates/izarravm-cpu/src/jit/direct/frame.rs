// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The emitted code's REGISTER and STACK-FRAME layout: which host registers hold guest state,
//! how large the native frame is, what occupies each slot in it, and which slots are the dynamic
//! counter lanes copied out on exit.
//!
//! Moved verbatim out of `direct.rs` to keep that file under the source-line ceiling; nothing
//! here changed but the module boundary. `direct.rs` re-exports this module's contents
//! (`use frame::*`), so `emit`'s `use super::*` continues to see every name unqualified exactly
//! as before.
//!
//! Keeping the whole layout in ONE file is the point rather than an accident: the slot constants,
//! the frame length they must fit inside, the const-asserts that check that, and the counter-lane
//! table are a single invariant spread across four declarations. Separating any of them from the
//! others is how a slot silently starts overlapping another -- see the STACK_PUSH_MEM_VALUE and
//! STACK_SAVED_RSI notes below, both of which exist because that nearly happened.

use super::*;

pub(super) const GUEST_HOMES: [Reg; 8] = [
    Reg::R8,
    Reg::R9,
    Reg::R10,
    Reg::R11,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::RBX,
];
pub(crate) const SAVED_HOST_REGS: [Reg; 7] = [
    Reg::RBX,
    Reg::RBP,
    Reg::RDI,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::R15,
];
pub(super) const ARITH_FLAGS: u32 = crate::FLAG_CF
    | crate::FLAG_PF
    | crate::FLAG_AF
    | crate::FLAG_ZF
    | crate::FLAG_SF
    | crate::FLAG_OF;
pub(super) const LOGIC_FLAGS: u32 = ARITH_FLAGS & !crate::FLAG_AF;

#[cfg(target_os = "windows")]
pub(super) const CPU_ARG: Reg = Reg::RCX;
#[cfg(not(target_os = "windows"))]
pub(super) const CPU_ARG: Reg = Reg::RDI;
#[cfg(target_os = "windows")]
pub(super) const FLAGS_ARG: Reg = Reg::RDX;
#[cfg(not(target_os = "windows"))]
pub(super) const FLAGS_ARG: Reg = Reg::RSI;
#[cfg(target_os = "windows")]
pub(super) const QUOTA_ARG: Reg = Reg::R8;
#[cfg(not(target_os = "windows"))]
pub(super) const QUOTA_ARG: Reg = Reg::RDX;
#[cfg(target_os = "windows")]
pub(super) const EXIT_ARG: Reg = Reg::R9;
#[cfg(not(target_os = "windows"))]
pub(super) const EXIT_ARG: Reg = Reg::RCX;

// Base frame: 20 accounting and scratch slots at 8 bytes each, offsets 0 to
// 152 below (STACK_QUOTA through STACK_SHIFT_COUNT), filling 160 bytes.
pub(super) const BASE_STACK_LEN: u32 = 160;
// One frame shape for every block, x87-bearing or not. A chained native
// transfer jumps straight into a target block's body, skipping its
// prologue, so the target's own epilogue always runs against whatever
// frame the entering block's prologue built. If the two frame shapes
// differ, that teardown pops the wrong bytes. On Windows the frame also
// carries the saved-RSI slot below and the x87 XMM6-11 save area; RSI is
// callee-saved there and doubles as the x87 tag-cache scratch register, and
// none of the XMM6-11 registers are. On non-Windows RSI is not
// callee-saved and there is no non-volatile XMM to save, so the frame is
// just the base.
#[cfg(target_os = "windows")]
pub(crate) const NATIVE_STACK_LEN: u32 = BASE_STACK_LEN + 8 + 6 * 16;
#[cfg(not(target_os = "windows"))]
pub(crate) const NATIVE_STACK_LEN: u32 = BASE_STACK_LEN;
pub(super) const STACK_QUOTA: i8 = 0;
pub(super) const STACK_ITERATIONS: i8 = 8;
pub(super) const STACK_RAM_BYTE_WRITES: i8 = 16;
pub(super) const STACK_RAM_DWORD_WRITES: i8 = 24;
pub(super) const STACK_MODE13_BYTE_WRITES: i8 = 32;
pub(super) const STACK_MODE13_DWORD_WRITES: i8 = 40;
pub(super) const STACK_MODE13_DIRTY_PAGES: i8 = 48;
pub(super) const STACK_EXIT: i8 = 56;
pub(super) const STACK_MODE13_BYTE_READS: i8 = 64;
pub(super) const STACK_MODE13_DWORD_READS: i8 = 72;
pub(super) const STACK_READ_KIND: i8 = 80;
pub(super) const STACK_WATCH_PAGE: i8 = STACK_READ_KIND;
pub(super) const STACK_WEIGHTED_FP_CLOCKS: i8 = 88;
pub(super) const STACK_INSTRUCTIONS: i8 = 96;
pub(super) const STACK_RAW_CLOCKS: i8 = 104;
pub(super) const STACK_BYTE_READS: i8 = 112;
pub(super) const STACK_DWORD_READS: i8 = 120;
pub(super) const STACK_ALU_ADDRESS_KIND: i32 = 128;
pub(super) const STACK_ALU_OLD_RESULT: i32 = 136;
/// Where `emit_push_mem` parks the dword it read from the source operand, across the stack
/// store's own address and kind path, which clobbers RAX, RCX, RDX and RDI. Those four are the
/// whole scratch set: `GUEST_HOMES` is R8 to R14 plus RBX, R15 is the CPU pointer, RBP is the
/// guest flag shadow, and RSI is host callee-saved and spilled only for x87 blocks.
///
/// Aliased onto the ALU slot deliberately. `PushMem` is not an ALU kind, the two never appear in
/// one slot's emission, and every use of either is written and read inside a single slot. It must
/// NOT be `STACK_READ_KIND`: `emit_code_watch_branch` writes `STACK_WATCH_PAGE`, which is the
/// same slot, on the store's path.
///
/// 136 is outside disp8 range, so this slot is reached with the disp32 load and store forms.
pub(super) const STACK_PUSH_MEM_VALUE: i32 = STACK_ALU_OLD_RESULT;
pub(super) const STACK_ALU_FLAGS: i32 = 144;
/// Where a shared store stub parks its CALL's return address (one-lookup store design D4):
/// the stub's `pop qword [rsp+..]` prologue moves the return address here and restores RSP to
/// the frame level in one instruction, so every frame-offset helper emits unchanged inside the
/// stub, and the epilogue is `jmp qword [rsp+..]`.
///
/// Aliased onto the ALU-flags slot by the `STACK_PUSH_MEM_VALUE` argument: the stubs are
/// reached only from `Store` and x87-memory-pointer slot emission, neither of which touches
/// the ALU scratch cluster, and every use of either slot is written and read inside a single
/// slot's emission. The constraint this buys: an emitter that DOES use the ALU cluster
/// (AluMemDest, double-shift, RMW) must not adopt the stub-call shape without moving this slot.
pub(super) const STACK_STUB_RETURN: i32 = STACK_ALU_FLAGS;
pub(super) const STACK_SHIFT_COUNT: i32 = 152;
// Beyond the base frame: the saved host RSI slot, then the x87 XMM6-11
// save area right after it. Both Windows only, see NATIVE_STACK_LEN above.
#[cfg(target_os = "windows")]
pub(super) const STACK_SAVED_RSI: i32 = BASE_STACK_LEN as i32;
#[cfg(target_os = "windows")]
pub(super) const STACK_X87_XMM_BASE: i32 = STACK_SAVED_RSI + 8;
// The saved-RSI slot and the XMM6-11 save area must both land inside the frame NATIVE_STACK_LEN
// actually allocates. A wrong STACK_X87_XMM_BASE (a stale copy of an old constant, say) would
// make the first XMM save overwrite the saved RSI slot and hand garbage RSI back to the Rust
// caller, silently, since the frame-size test only checks the sub rsp / add rsp immediates
// agree, not that the areas inside the frame do not collide.
#[cfg(target_os = "windows")]
const _: () = {
    assert!(STACK_SAVED_RSI as u32 + 8 <= STACK_X87_XMM_BASE as u32);
    assert!(
        STACK_X87_XMM_BASE as u32 + emit::X87_NONVOLATILE_XMMS.len() as u32 * 16
            <= NATIVE_STACK_LEN
    );
};

/// The seven dynamic counter lanes, each pairing the emitter stack slot that accumulates it with
/// the `NativeExit` field it is copied into on the way out.
///
/// The single source of truth for the SET and the ORDER of those lanes: `emit`'s prologue zeroes
/// exactly these stack slots and `emit_return` copies exactly these slots out, so a lane can only
/// ever be added or dropped in both places at once.
///
/// Every lane is unconditional. There was once a per-`DirectKind` mask that nominated a subset,
/// but it never reached `emit_return`, which was always called with the all-bits constant; it was
/// removed rather than wired up, because gating the copies would save about five stores per block
/// exit — unmeasurable against this campaign's layout noise — on the hottest shared exit path,
/// where a wrongly-cleared lane is a guest-visible bus-accounting error rather than a diagnostic
/// one.
pub(super) fn dynamic_counter_fields() -> [(i8, usize); 7] {
    [
        (
            STACK_RAM_BYTE_WRITES,
            core::mem::offset_of!(NativeExit, ram_byte_writes),
        ),
        (
            STACK_RAM_DWORD_WRITES,
            core::mem::offset_of!(NativeExit, ram_dword_writes),
        ),
        (
            STACK_MODE13_BYTE_WRITES,
            core::mem::offset_of!(NativeExit, mode13_byte_writes),
        ),
        (
            STACK_MODE13_DWORD_WRITES,
            core::mem::offset_of!(NativeExit, mode13_dword_writes),
        ),
        (
            STACK_MODE13_DIRTY_PAGES,
            core::mem::offset_of!(NativeExit, mode13_dirty_pages),
        ),
        (
            STACK_MODE13_BYTE_READS,
            core::mem::offset_of!(NativeExit, mode13_byte_reads),
        ),
        (
            STACK_MODE13_DWORD_READS,
            core::mem::offset_of!(NativeExit, mode13_dword_reads),
        ),
    ]
}
