// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Interpreter CALL-OUT slots: running one instruction through the interpreter from inside a
//! native block, then resuming the same block.
//!
//! Phase 5 carries exactly one opcode here, `0xEC` (IN AL,DX). Every other barrier opcode still
//! ends its block; this module is the mechanism, `classify`'s single `0xec` arm is the policy.
//!
//! # Why a call-out at all
//!
//! Emitted blocks never receive the bus. `DirectEntryFn` takes `(*mut CpuGsw, flags, quota,
//! *mut NativeExit)` and every native memory access goes through the fast map, so a port read --
//! which is a `CpuBus::read_io` call and nothing else -- has no reachable path from emitted code.
//! The helper below closes that by being MONOMORPHISED over the bus type in `run_direct_block`
//! and published, together with the live `&mut B`, through `CpuGsw::native_callout` just before
//! entry. Emitted code loads the function pointer out of the CPU struct and calls it.
//!
//! # The helper contract
//!
//! Written down before a byte was emitted, and the thing an adversarial review should attack.
//!
//! **Synced IN (emitted code, before the call):** the whole `GUEST_HOMES` set, stored to
//! `CpuGsw.registers.gpr` by `emit_store_homes`. Nothing else, and each omission is deliberate:
//!
//! * EFLAGS -- RBP is a SHADOW of `CpuGsw.registers.eflags`, mirrored at every flag-producing
//!   site, so the memory copy is already current at any point in the block body (the same
//!   property `shared_return` relies on when it stores homes but not flags).
//! * EIP -- stays at the block's entry value throughout the body by construction (`emit_advance_eip`
//!   runs only on exit paths), and the helper never reads it. Both call-out exit paths advance it
//!   through the ordinary side-exit machinery, so the block-body invariant is never broken and no
//!   exit can double-advance.
//! * Segment registers, CR*, TR, `pending_flags`, `interrupt_shadow`, the prefetch window and
//!   `written_pages` -- owned by `CpuGsw` and never mirrored into a host register by the block
//!   body, so they are already exactly what the interpreter would see.
//!
//! **Read by the helper:** `registers.gpr[2]` (DX), `registers.eflags` (IOPL and VM, through
//! `iopl`/`is_v86_mode`), CPL, `tr`, `core_clocks_so_far`, `timing_rem`, the persona.
//!
//! **Written by the helper, normal path:** `registers.gpr[0]`'s low byte (AL), plus whatever the
//! device behind the port does. Nothing else -- no clock counter, no `perf`, no EIP.
//!
//! **Written by the helper, abnormal path:** NOTHING. See the enumeration below.
//!
//! **Returned:** an `i64` status.
//! * negative -- ABNORMAL. The caller must end the native run at the call-out with EIP AT the
//!   call-out instruction.
//! * otherwise -- bits 0..32 are the instruction's raw core clocks (the interpreter's own charge,
//!   unscaled), bit 32 is a step-break request.
//!
//! **Synced OUT (emitted code, after the call):** the whole `GUEST_HOMES` set is RELOADED,
//! unconditionally and on every path. Two independent reasons, either one sufficient: R8-R11 are
//! volatile under both host ABIs and the call clobbers them, and AL may have changed. The reload
//! precedes the status branch precisely so the abnormal path reaches `shared_return` -- which
//! stores homes -- with correct values instead of writing clobbered registers over good memory.
//!
//! # The abnormal set (exactly two producers)
//!
//! 1. `check_io_permission` returns `Err`: the TSS I/O-permission bitmap denies the port
//!    (`#GP(0)`), or a TSS probe itself faults. It runs BEFORE `read_io`, so no device has been
//!    addressed.
//! 2. `bus.read_io` returns `Err`. For `MachineBus` the sole producer is the `UnsupportedPort`
//!    fall-through, reached only after every device declined the port -- so no device observed
//!    the access there either.
//!
//! **Zero partial effects.** On both, at the moment of return: `registers.gpr` is byte-identical
//! to the pre-call state (AL is written only after a successful read), EFLAGS and `pending_flags`
//! are untouched (IN writes no flags), EIP still holds the block-entry value, `elapsed_clocks`,
//! `perf`, `timing_rem`, `written_pages` and `prefetch` are untouched, and no clocks are charged
//! (the caller skips the raw-clock add on the negative branch). The native run then ends with EIP
//! at the call-out, which is byte-for-byte the state the run loop sees TODAY when a block ends at
//! an IN barrier -- so the interpreter re-executes the instruction and delivers exactly what it
//! delivers today. FAIL-CLOSED: any status the helper cannot certify is negative.
//!
//! # Timing fidelity
//!
//! * **The charge.** The helper returns the interpreter's raw `clocks(12)`. The caller adds it to
//!   the block's runtime raw-clock lane, which `run_direct_block` puts through
//!   `scale_clocks_batch`. `scale_clocks_batches_exactly` (cpu_test.rs) pins that batch against
//!   summed per-instruction `scale_clocks`, so the guest-visible charge is EQUAL to the
//!   interpreter's, not merely close.
//! * **The device's view of time.** `read_io` takes `core_clocks_so_far`, which the interpreter
//!   sets to the run's running total before each instruction. Inside a block that value is stale
//!   by the block's own prefix, so the helper is handed that prefix's RAW clocks and adds the
//!   scaled prefix on top -- through `preview_scale_clocks`, which performs the same long
//!   division WITHOUT consuming `timing_rem`, so it changes no accounting. By the same batching
//!   identity the result equals the interpreter's running total at this instruction. Without this
//!   a mid-block IN would sample a beam or a counter a few clocks in the past.
//! * **The step break.** After a successful read the helper asks `bus.requires_step_break()` and
//!   reports it. The caller then ends the native run at the boundary AFTER the call-out -- the
//!   same boundary `run_straight_line`'s post-instruction check produces for an interpreted
//!   continuation. This is what preserves `run_direct_block`'s standing "devices advance only
//!   after native return" invariant: a port access that touches time-dependent device state still
//!   ends the run immediately, it just does so one instruction later in the block instead of
//!   refusing to compile the block at all.
//!
//! # Interrupt delivery
//!
//! No new latency class. An IRQ the port read raises (the 8042 data-register read re-levelling
//! IRQ1/IRQ12 is the live example) is delivered where it is delivered for every native block
//! today: at the block boundary, by the machine's batch loop. Every port read that reaches a
//! device sets `io_touched`, which is `requires_step_break`, which ends the native run at the
//! instruction after the IN -- so the delivery boundary is the same one an interpreted
//! continuation ending at that instruction would produce.
//!
//! # Self-modifying code
//!
//! A call-out cannot reach `note_code_write`: `write_gpr8` is register state, `read_io` writes no
//! guest memory, and `check_io_permission` only reads. `debug_assert`ed structurally below by
//! comparing the written-page bookkeeping across the call.
//!
//! # Unwinding
//!
//! The call site moves RSP by `CALLOUT_CALL_FRAME` for the span of the call, while the arena's
//! shared UNWIND_INFO (jit/unwind.rs) asserts a constant RSP for the whole block body -- the same
//! shape as the accepted `pushfq` windows documented there. It is not the same RISK, because
//! nothing ever unwinds through it: the helper is `extern "C"`, whose Rust ABI is nounwind, so a
//! panic inside it aborts at the boundary instead of walking the JIT frame. What remains is the
//! pre-existing sampling gap -- a profiler sampling INSIDE the callee walks back to a call site
//! whose CFA is `CALLOUT_CALL_FRAME` bytes off -- and it is accepted on the same terms.

use super::emit::{emit_store_homes, gpr_offset};
use super::*;
use crate::IN_AL_DX_CORE_CLOCKS;
use izarravm_bus::{BusWidth, CpuBus};

/// The ABI emitted code calls a helper through. `extern "C"` and NOT `extern "C-unwind"`: see the
/// unwinding note in the module docs -- the nounwind guarantee is load-bearing, because a panic
/// walking back through the call site would read the frame against unwind info that does not
/// describe `CALLOUT_CALL_FRAME`.
pub(crate) type CallOutFn = unsafe extern "C" fn(*mut CpuGsw, u64) -> i64;

/// The live bus and the monomorphised helpers for it, published by `run_direct_block` for the
/// duration of one native entry into a block that carries at least one call-out slot.
///
/// `bus` is a type-ERASED `*mut B`. It is sound only because the same call that publishes it also
/// publishes helpers monomorphised over that exact `B`, and both are cleared when the entry
/// returns; nothing else reads either field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CallOutTable {
    pub(crate) bus: *mut (),
    /// `CallOutHelper::PortReadAlDx`. A `usize` rather than an `Option<CallOutFn>` so the emitted
    /// load is one plain quadword and the null case is a null pointer the debug assert catches.
    pub(crate) port_read_al_dx: usize,
}

impl Default for CallOutTable {
    fn default() -> Self {
        Self {
            bus: core::ptr::null_mut(),
            port_read_al_dx: 0,
        }
    }
}

impl CallOutTable {
    /// Publish `bus` and the helpers monomorphised over its type.
    pub(crate) fn publish<B: CpuBus>(bus: &mut B) -> Self {
        Self {
            bus: (bus as *mut B).cast::<()>(),
            port_read_al_dx: port_read_al_dx::<B> as CallOutFn as usize,
        }
    }
}

/// Status encoding. Negative is abnormal; otherwise the low 32 bits are raw core clocks and bit
/// 32 asks the caller to end the native run after this instruction.
const STATUS_ABNORMAL: i64 = -1;
pub(crate) const STATUS_STEP_BREAK_BIT: u32 = 32;

/// Which byte in `CallOutTable` an emitted slot loads its function pointer from.
fn helper_offset(helper: CallOutHelper) -> i32 {
    let field = match helper {
        CallOutHelper::PortReadAlDx => core::mem::offset_of!(CallOutTable, port_read_al_dx),
    };
    (core::mem::offset_of!(CpuGsw, native_callout) + field) as i32
}

/// `0xEC` IN AL,DX through the interpreter's own port path.
///
/// # Safety
///
/// `cpu` must be the live `CpuGsw` the native block was entered with, and its `native_callout`
/// must have been published by `CallOutTable::publish::<B>` for the same `B` this instantiation
/// was selected for. `run_direct_block` is the only publisher and it does both together.
unsafe extern "C" fn port_read_al_dx<B: CpuBus>(cpu: *mut CpuGsw, prefix_raw_clocks: u64) -> i64 {
    // SAFETY: by the contract above, `cpu` is the live CPU and no other reference to it is alive
    // across the call (emitted code holds only its ADDRESS, in R15).
    let cpu = unsafe { &mut *cpu };
    let bus_ptr = cpu.native_callout.bus;
    debug_assert!(
        !bus_ptr.is_null(),
        "a call-out slot ran without a published bus"
    );
    if bus_ptr.is_null() {
        return STATUS_ABNORMAL;
    }
    // SAFETY: `publish::<B>` stored a `*mut B` derived from the `&mut B` that `run_direct_block`
    // holds for the whole native entry, and this instantiation was selected by that same call, so
    // the type matches and the borrow is live and unaliased (the CPU and the bus are disjoint).
    let bus = unsafe { &mut *bus_ptr.cast::<B>() };

    // The written-page bookkeeping is the SMC assertion: nothing on this path may reach
    // `note_code_write`, so both counters must come back unchanged. Sampled here rather than
    // argued in prose so a future helper that does write memory trips a debug build.
    //
    // ONE path could legitimately trip it and it is worth naming rather than excluding: a paged
    // V86 task whose `check_io_permission` TSS probe takes a page walk that sets an accessed bit.
    // That IS a guest memory write, so if it fires the SMC argument genuinely needs the paging
    // case worked through -- the assertion is a tripwire for that question, not a claim that the
    // question is already answered for every privilege state.
    #[cfg(debug_assertions)]
    let written_before = (cpu.written_count, cpu.written_pages_overflow);

    // The device's view of guest time. `core_clocks_so_far` was set to the run's total at block
    // ENTRY; the block's own prefix has run since. `preview_scale_clocks` applies the persona's
    // long division to that prefix WITHOUT consuming `timing_rem`, so this is a pure read that
    // reproduces the interpreter's running total at this instruction.
    let now = cpu
        .core_clocks_so_far
        .saturating_add(cpu.preview_scale_clocks(prefix_raw_clocks));

    let port = cpu.read_gpr16(2);
    let ring0 = cpu.is_ring0_protected();
    if cpu.check_io_permission(bus, port, BusWidth::Byte).is_err() {
        return STATUS_ABNORMAL;
    }
    let Ok(value) = bus.read_io(port, BusWidth::Byte, now, ring0) else {
        return STATUS_ABNORMAL;
    };
    cpu.write_gpr8(0, value as u8);

    #[cfg(debug_assertions)]
    debug_assert_eq!(
        written_before,
        (cpu.written_count, cpu.written_pages_overflow),
        "a call-out slot must never write guest memory"
    );

    // The SAME constant the interpreter's `0xec` arm charges, shared rather than copied, so the
    // exact-clocks claim cannot drift out from under this module.
    i64::from(IN_AL_DX_CORE_CLOCKS)
        | (i64::from(bus.requires_step_break()) << STATUS_STEP_BREAK_BIT)
}

/// Drive the helper exactly as an emitted slot does -- publish, call, clear -- so a test can
/// assert the CONTRACT (status encoding, charge, zero partial effects) without an emitter in the
/// picture. `prefix_raw_clocks` is what a block's prefix would have deposited.
#[cfg(test)]
pub(crate) fn port_read_al_dx_for_test<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &mut B,
    prefix_raw_clocks: u64,
) -> i64 {
    cpu.native_callout = CallOutTable::publish(bus);
    // SAFETY: the table was just published for this exact `B`, and `cpu` is not otherwise
    // borrowed across the call.
    let status = unsafe { port_read_al_dx::<B>(cpu as *mut CpuGsw, prefix_raw_clocks) };
    cpu.native_callout = CallOutTable::default();
    status
}

/// Windows needs 32 bytes of shadow space below the call plus 16-byte alignment at the `call`;
/// SysV needs only the alignment. Both are satisfied by a fixed-size scratch frame, and the
/// assertion below is what proves the alignment rather than a comment claiming it.
#[cfg(target_os = "windows")]
pub(super) const CALLOUT_CALL_FRAME: u32 = 40;
#[cfg(not(target_os = "windows"))]
pub(super) const CALLOUT_CALL_FRAME: u32 = 16;

// RSP is 16-byte aligned at a `call` when the bytes pushed since the ABI-guaranteed entry
// alignment sum to 8 mod 16: the entry `call` itself pushed 8, then `SAVED_HOST_REGS`, then the
// block frame, then this scratch frame. A chained transfer cannot break this -- it jumps into
// another block's BODY, and every block frame has the one shape (`NATIVE_STACK_LEN`).
const _: () =
    assert!((SAVED_HOST_REGS.len() as u32 * 8 + NATIVE_STACK_LEN + CALLOUT_CALL_FRAME) % 16 == 8);
#[cfg(target_os = "windows")]
const _: () = assert!(CALLOUT_CALL_FRAME >= 32);

/// Emit one call-out slot.
///
/// `static_prefix_raw` is this block's compile-time raw-clock total for the slots BEFORE this one;
/// added to the runtime lane so the helper sees the whole prefix (the lane alone carries only
/// what earlier chained blocks and exits deposited).
///
/// `abnormal` and `step_break` are side-exit reason stubs the caller has already registered; this
/// function only branches to them.
pub(super) fn emit_call_out(
    e: &mut Encoder,
    helper: CallOutHelper,
    static_prefix_raw: u16,
    abnormal: Label,
    step_break: Label,
) {
    // Whole-set spill. Deliberately NOT a partial spill keyed on what IN reads and writes: this
    // phase buys correctness, and a partial set would have to be re-derived per helper.
    emit_store_homes(e);
    // Argument 1: the block run's raw clocks so far.
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_RAW_CLOCKS);
    if static_prefix_raw != 0 {
        e.add_r64_imm32(Reg::RAX, u32::from(static_prefix_raw));
    }
    e.sub_r64_imm32(Reg::RSP, CALLOUT_CALL_FRAME);
    e.mov_r64_r64(FLAGS_ARG, Reg::RAX);
    e.mov_r64_r64(CPU_ARG, Reg::R15);
    e.load_r64_disp32(Reg::RAX, Reg::R15, helper_offset(helper));
    e.call_r64(Reg::RAX);
    e.add_r64_imm32(Reg::RSP, CALLOUT_CALL_FRAME);
    // Reload BEFORE the status branch: the abnormal path leaves through `shared_return`, which
    // stores the homes, so a reload placed only on the success path would let a call-clobbered
    // R8-R11 overwrite live guest registers. `mov` writes no flags, so RAX's status survives.
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.load_r32_disp32(home, Reg::R15, gpr_offset(index));
    }
    e.cmp_r64_imm32(Reg::RAX, 0);
    // 0x8 is the x86 sign condition: `js abnormal`.
    e.jcc(0x8, abnormal);
    // The RUNTIME clock lane. `mov r32, r32` zero-extends, which is what isolates the raw-clock
    // field from the step-break bit above it. The mutation record for this slice is this pair
    // deleted: the differential fixture's core-clock comparison then fails by exactly the
    // interpreter's charge for the IN.
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.add_r64_to_mem_disp8(Reg::RSP, STACK_RAW_CLOCKS, Reg::RDX);
    // ModRM /5 is SHR. The shift sets ZF against the step-break bit directly, so no compare.
    e.shift_r64_imm8(5, Reg::RAX, STATUS_STEP_BREAK_BIT as u8);
    e.jnz(step_break);
}
