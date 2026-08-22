// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The ABI between emitted code and the run loop: how a block is ENTERED, what it writes on the
//! way OUT, and the two reason codes that explain why it left.
//!
//! Extracted from `jit/direct.rs` as pure code motion. Nothing here changed in the move -- the
//! types, their `repr`s, their derives, their discriminants and their doc comments are the text
//! that file carried, and `direct.rs` re-exports every name so no caller's path changed.
//!
//! It is a coherent unit rather than an arbitrary slice, and the shape is worth stating because
//! it is what decides whether a future type belongs here: `DirectEntryFn` is the entry signature,
//! `NativeExit` is the single out-parameter it fills, `NativeBlockTrace` is the optional buffer
//! `NativeExit::trace_ptr` points at, and `SideExitReason` and `UnresolvedReason` are the two
//! enums whose discriminants live in `NativeExit`'s `side_exit_reason` and `unresolved_reason`
//! fields. Everything here is read by `run.rs` after a native return; nothing here is read DURING
//! one.
//!
//! **The discriminants are a wire format.** Emitted code stores the integer, `run.rs` compares
//! against the enum, and the two are matched only by these values -- so renumbering any variant
//! silently remaps what a stale compiled block reports. `SideExitReason::MAX` exists so `run.rs`
//! can bound-assert what it reads rather than trusting it.
//!
//! What deliberately did NOT come along: `dynamic_counter_fields`, which names `NativeExit` field
//! offsets but pairs each one with a slot in the emitter's own stack layout (`STACK_*`). It is
//! emitter shape, not exit ABI, and moving it would have made this commit something other than
//! pure motion. (It once sat beside a family of `COUNTER_*` bit masks naming the same lanes; those
//! were deleted when the per-kind mask they fed turned out never to reach the emitter.)

use crate::CpuGsw;

pub(crate) type DirectEntryFn = unsafe extern "C" fn(*mut CpuGsw, u32, u32, *mut NativeExit);

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SideExitReason {
    None = 0,
    CrossPageOrAlignment = 1,
    UnavailableOrKind = 2,
    Permission = 3,
    CodeWatch = 4,
    /// The catch-all. The now-removed clif backend was its only remaining producer (two sites
    /// in its `lower.rs`); every Direct producer names itself, because `Other` was 99.7% of the
    /// side-exit growth across the x87 sweep and could not be attributed to any of its six
    /// emitters. `allow(dead_code)` because no producer builds it anymore, and the discriminant
    /// must keep its value regardless: `run.rs` still has a catch-all arm for it, and
    /// renumbering would silently remap what emitted code stores.
    #[allow(dead_code)]
    Other = 5,
    /// A CS segment-limit check on a computed control-transfer target: the six
    /// `Ret`/`Ret16`/`JmpMem`/`CallReg`/`CallMem` sites plus `MemorySideExits`' own limit stub.
    SegmentLimit = 6,
    /// An x87 slot's eligibility guard: a non-finite value, an Empty or Special tag, a
    /// non-truncate rounding mode at a FIST, an out-of-range integer conversion, or a subnormal
    /// at FSTP m80. Unlike every other reason here this one is a per-EXECUTION property of the
    /// data, not a per-compile property of the address, so it can fire on a block that will
    /// never bind differently.
    X87Eligibility = 7,
    /// An interpreter call-out slot reported an ABNORMAL status: the helper refused before any
    /// guest-visible effect. EIP is at the call-out instruction and the run ends there, so the
    /// interpreter re-executes it and delivers whatever it delivers today.
    CallOutAbnormal = 8,
    /// An interpreter call-out slot completed and the BUS asked for a step break (a port access
    /// touched time-dependent device state). The run ends at the boundary AFTER the call-out,
    /// which is exactly where `run_straight_line`'s post-instruction `requires_step_break` check
    /// ends an interpreted continuation.
    CallOutStepBreak = 9,
    /// A lowered DIV or IDIV refused its operands. The guard fires on a superset of the
    /// interpreter's #DE conditions, the run ends AT the instruction with nothing done, and the
    /// interpreter re-executes it -- delivering #DE where the guest's rules say it must. A
    /// per-EXECUTION property of the data like `X87Eligibility`, not a per-compile property of
    /// the address, so it can fire on a block that will never bind differently.
    DivideGuard = 10,
    /// An `InterpretOne` call-out RAN its instruction and the resume predicate then refused: some
    /// input the rest of the block depends on moved under it (a segment record, a control
    /// register, IF going 0 to 1, a mapping epoch, a write onto a watched code page). The
    /// instruction RETIRED, so the exit reports `prefix + 1`, and EIP is left exactly where the
    /// interpreter put it -- the stub advances it by zero.
    CallOutResync = 11,
    /// An `InterpretOne` call-out's step returned `Err` and `finish_instruction` delivered it. The
    /// fault path already counted the instruction in `perf.instructions` and already charged its
    /// clocks, so the exit reports `prefix` and adds no clocks of its own. EIP is wherever the
    /// delivery left it, which is normally the handler's first byte.
    CallOutResyncFault = 12,
}

impl SideExitReason {
    /// The largest discriminant emitted code can store, for the `run.rs` bound assertion.
    pub(crate) const MAX: u32 = Self::CallOutResyncFault as u32;
}

#[repr(u32)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnresolvedReason {
    #[default]
    None = 0,
    StaticUnbound,
    StaticHidden,
    DynamicMissOrUnbound,
    DynamicHidden,
    /// The shared x87 re-entry pad refused the crossing: the target float block's baked entry TOP
    /// does not match the CPU's live TOP, so its register cache cannot be entered for it.
    X87TopMismatch,
}

/// Fetch replay retained for buses that observe individual code addresses. Production RAM timing
/// uses the aggregate counters in `NativeExit` and leaves this trace disabled.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeBlockTrace {
    pub(crate) linear: u32,
    pub(crate) physical: u32,
    pub(crate) repetitions: u32,
    pub(crate) prefix_instructions: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeExit {
    pub(crate) instructions: u64,
    pub(crate) raw_clocks: u64,
    // Byte counters use the low lane and word counters use the high lane. Native chain bounds
    // keep both 32-bit lanes well below overflow while preserving the original exit layout.
    pub(crate) byte_reads: u64,
    /// Low 32 bits: the block's STATIC dword-read count.
    ///
    /// High 32 bits: `split_extra_bytes` -- the EXTRA byte cycles owed by misaligned RAM accesses
    /// served natively, `bytes() - 1` apiece, and fed by STORES as well as reads. The lane's name
    /// covers only its low half; `run.rs` masks before every consumer and prices the high half
    /// through `jit_data_cost_clocks(Byte)`, the same dial `ram_byte_writes` and `ram_byte_reads`
    /// both take. See `frame.rs`'s `STACK_DWORD_READS` for why one shared pool is exact.
    pub(crate) dword_reads: u64,
    pub(crate) weighted_fp_clocks: u64,
    pub(crate) mode13_byte_reads: u64,
    pub(crate) mode13_dword_reads: u64,
    pub(crate) ram_byte_writes: u64,
    pub(crate) ram_dword_writes: u64,
    pub(crate) mode13_byte_writes: u64,
    pub(crate) mode13_dword_writes: u64,
    pub(crate) mode13_dirty_pages: u64,
    pub(crate) side_exit: u64,
    pub(crate) side_exit_reason: u32,
    pub(crate) trace_len: u32,
    pub(crate) linked_transfers: u32,
    pub(crate) unresolved_reason: UnresolvedReason,
    pub(crate) trace_ptr: usize,
    pub(crate) dynamic_link_cell: usize,
    pub(crate) dynamic_target_eip: u32,
    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) direct_link_refusal_census_id: u32,
}
