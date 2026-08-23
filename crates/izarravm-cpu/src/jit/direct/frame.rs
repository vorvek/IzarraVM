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
/// Low 32 bits: the block's STATIC dword-read count, written once by
/// `emit_add_static_accounting`.
///
/// High 32 bits: `split_extra_bytes`, the extra byte cycles owed by MISALIGNED
/// RAM accesses served natively at the two lean one-lookup sites -- `bytes() - 1`
/// per access, deposited dynamically by `emit_dynamic_split_extra` from the load
/// and store stubs' RAM tails.
///
/// The lane's NAME therefore lies twice over: the high half is not dword reads,
/// and it is fed by stores as well as reads. Both are deliberate. It is
/// numerically exact because `run.rs` prices `ram_byte_writes` through the same
/// `jit_data_cost_clocks(Byte)` as `ram_byte_reads`, so one shared pool of extra
/// byte cycles is the right shape; and it is free because this high half was
/// already zeroed by the prologue, already copied out by `emit_return` as part of
/// a full 64-bit lane, and had no consumer at all.
///
/// `STACK_RAM_DWORD_WRITES`'s high half USED to be free in the same way and this
/// comment used to offer it to the next reader. **It is taken.** Since the
/// V86/real-mode far-return slice it carries the far-return ledger, deposited by
/// `RetFar16` through `emit_dynamic_word_increment` and unpacked in `run.rs` into
/// `jit_direct_far_ret_native`. Do not double-allocate it.
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
/// Where `RetFar16` parks the offset it popped first, across the SECOND stack read's address,
/// guard and pointer path, which clobbers RAX, RCX, RDX and RDI.
///
/// Aliased onto the ALU slot by `STACK_PUSH_MEM_VALUE`'s argument, and it needs all three parts of
/// it: `RetFar16` is not an ALU kind, it is not `PushMem` (the two can never appear in one slot's
/// emission -- both are terminal or, in `PushMem`'s case, never co-emitted with a far return in
/// the same slot), and both the write and the read happen inside a single slot's emission. It must
/// NOT be `STACK_READ_KIND` (80): `emit_code_watch_branch` writes `STACK_WATCH_PAGE`, the same
/// slot, on a read's path -- and this value has to survive exactly such a path. Nor
/// `STACK_STUB_RETURN` (= `STACK_ALU_FLAGS` = 144), which the shared read stub uses across its
/// CALL.
///
/// 136 is outside `STACK_ZERO_FILL_LEN` = 128, so the prologue does not clear it. Irrelevant: the
/// write precedes the read inside one slot, and no path reads it without having written it.
pub(super) const STACK_RET_FAR_OFFSET: i32 = STACK_ALU_OLD_RESULT;
pub(super) const STACK_ALU_FLAGS: i32 = 144;
/// Where a shared stub (store OR read pad, designs D4 of each) parks its CALL's return
/// address: the stub's `pop qword [rsp+..]` prologue moves the return address here and
/// restores RSP to the frame level in one instruction, so every frame-offset helper emits
/// unchanged inside the stub, and the epilogue is `jmp qword [rsp+..]`.
///
/// Aliased onto the ALU-flags slot by the `STACK_PUSH_MEM_VALUE` argument. The safety
/// argument, restated for the read twin: every stub caller writes and reads this slot inside a
/// single CALL's lifetime, and no caller has a LIVE value in the ALU scratch cluster at the
/// call point — the store stubs are reached from `Store`/x87-pointer emission (no ALU-cluster
/// use at all), and the read stubs' callers that touch the cluster (`AluMemDest`'s CMP form,
/// `AluMemSource`, `TestImmMem`) do so only AFTER the read helper returns, never across it.
/// The emitters that hold cluster state ACROSS a memory front (`AluMemDest`'s writing forms,
/// double-shift, RMW) resolve through `emit_read_pointer`/`emit_write_pointer` directly and
/// must not adopt the stub-call shape without moving this slot.
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

/// The prologue's zero-fill window: frame bytes `0..STACK_ZERO_FILL_LEN`, cleared as whole
/// 32-byte vector stores instead of one 8-byte store per accumulator.
///
/// Thirteen slots need zeroing — `STACK_ITERATIONS`, the seven `dynamic_counter_fields` lanes
/// and the five static accumulators — and they are scattered across 8..128 with
/// `STACK_EXIT` (56) and `STACK_READ_KIND` (80) interleaved between them. Clearing the whole
/// window and writing `STACK_EXIT` and `STACK_QUOTA` afterwards is four stores instead of
/// thirteen, per block ENTRY (a chained transfer jumps to `body_offset` and skips the prologue
/// entirely, so this is per entry and not per hop). `STACK_READ_KIND` is pure scratch, so
/// clearing it costs nothing but the bytes it shares with the vector store.
///
/// It stops at 128 because the ALU scratch cluster starts there and is deliberately NOT
/// zeroed; the asserts below pin both halves of that boundary, so moving a slot across it
/// fails the build rather than silently losing its initialisation.
///
/// One side effect worth naming rather than leaving to be rediscovered: `STACK_READ_KIND`
/// (and its alias `STACK_WATCH_PAGE`) now starts every entry at a DETERMINISTIC zero, where
/// before it held whatever the previous entry left in the slot. That is strictly safer but it
/// changes a failure mode — a future read-before-write of that slot now reads page 0 and
/// resolves something wrong, instead of reading garbage and failing loudly. Nothing reads it
/// before writing it today; every parker writes it inside the same slot's emission.
pub(super) const STACK_ZERO_FILL_LEN: i32 = 128;
const _: () = {
    assert!(STACK_ZERO_FILL_LEN as u32 <= NATIVE_STACK_LEN);
    assert!(STACK_ZERO_FILL_LEN % 32 == 0);
    // Every slot the prologue must start at zero lives inside the window...
    assert!(STACK_ITERATIONS >= 0 && (STACK_ITERATIONS as i32) < STACK_ZERO_FILL_LEN);
    assert!(STACK_INSTRUCTIONS >= 0 && (STACK_INSTRUCTIONS as i32) < STACK_ZERO_FILL_LEN);
    assert!(STACK_RAW_CLOCKS >= 0 && (STACK_RAW_CLOCKS as i32) < STACK_ZERO_FILL_LEN);
    assert!(STACK_BYTE_READS >= 0 && (STACK_BYTE_READS as i32) < STACK_ZERO_FILL_LEN);
    assert!(STACK_DWORD_READS >= 0 && (STACK_DWORD_READS as i32) < STACK_ZERO_FILL_LEN);
    assert!(
        STACK_WEIGHTED_FP_CLOCKS >= 0 && (STACK_WEIGHTED_FP_CLOCKS as i32) < STACK_ZERO_FILL_LEN
    );
    assert!(STACK_RAM_BYTE_WRITES >= 0 && (STACK_RAM_BYTE_WRITES as i32) < STACK_ZERO_FILL_LEN);
    assert!(STACK_RAM_DWORD_WRITES >= 0 && (STACK_RAM_DWORD_WRITES as i32) < STACK_ZERO_FILL_LEN);
    assert!(
        STACK_MODE13_BYTE_WRITES >= 0 && (STACK_MODE13_BYTE_WRITES as i32) < STACK_ZERO_FILL_LEN
    );
    assert!(
        STACK_MODE13_DWORD_WRITES >= 0 && (STACK_MODE13_DWORD_WRITES as i32) < STACK_ZERO_FILL_LEN
    );
    assert!(
        STACK_MODE13_DIRTY_PAGES >= 0 && (STACK_MODE13_DIRTY_PAGES as i32) < STACK_ZERO_FILL_LEN
    );
    assert!(STACK_MODE13_BYTE_READS >= 0 && (STACK_MODE13_BYTE_READS as i32) < STACK_ZERO_FILL_LEN);
    assert!(
        STACK_MODE13_DWORD_READS >= 0 && (STACK_MODE13_DWORD_READS as i32) < STACK_ZERO_FILL_LEN
    );
    // ...and the two slots the prologue writes AFTER the fill are inside it too, which is why
    // the write order matters.
    assert!(STACK_QUOTA >= 0 && (STACK_QUOTA as i32) < STACK_ZERO_FILL_LEN);
    assert!(STACK_EXIT >= 0 && (STACK_EXIT as i32) < STACK_ZERO_FILL_LEN);
    // The ALU scratch cluster is outside, and stays outside.
    assert!(STACK_ALU_ADDRESS_KIND >= STACK_ZERO_FILL_LEN);
    assert!(STACK_ALU_OLD_RESULT >= STACK_ZERO_FILL_LEN);
    assert!(STACK_ALU_FLAGS >= STACK_ZERO_FILL_LEN);
    assert!(STACK_SHIFT_COUNT >= STACK_ZERO_FILL_LEN);
};
// The Windows-only areas above the base frame. The prologue writes BOTH of these BEFORE the
// fill runs (the x87 entry path saves RSI and XMM6-11 immediately after `sub rsp`), so a
// `BASE_STACK_LEN` that ever shrank under the fill window would have the fill quietly eat a
// saved host register on its way past — handing garbage RSI or XMM state back to the Rust
// caller with nothing else complaining.
#[cfg(target_os = "windows")]
const _: () = {
    assert!(STACK_SAVED_RSI >= STACK_ZERO_FILL_LEN);
    assert!(STACK_X87_XMM_BASE >= STACK_ZERO_FILL_LEN);
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
///
/// **A per-block mask is not merely unprofitable, it is UNSOUND, and that is why this stays
/// unconditional** (recorded 2026-08-11, after the rt-2.0 JIT audit proposed recovering it).
/// A chained native transfer jumps to a successor's `body_offset`, skipping its prologue, and
/// the block that finally leaves native code runs ITS OWN `emit_return` against the frame the
/// whole chain accumulated into. So neither end of a chain knows the lane set:
///
///   * masking the PROLOGUE by the entry block's kinds leaves a successor accumulating into a
///     slot nobody zeroed, and
///   * masking `emit_return` by the exiting block's kinds drops a predecessor's counts.
///
/// Links form at runtime between any two blocks sharing a mode key, so no compile-time union
/// bounds either set. Both failures are guest-visible bus accounting. The prologue cost was
/// recovered instead by making the zeroing WIDER, not narrower — see `STACK_ZERO_FILL_LEN`.
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
