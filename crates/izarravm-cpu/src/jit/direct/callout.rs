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
//! # The abnormal set (exactly three producers)
//!
//! 1. **The permission-checked port, refused up front.** When `is_v86_mode() || CPL > IOPL`,
//!    `check_io_permission` would probe the TSS bitmap through `read_system_linear` -- and under
//!    paging that walk WRITES guest memory (PDE/PTE accessed bits), records `written_pages`, can
//!    set CR2, advances bus clocks mid-run, can evict a TLB entry and invalidate the fast map
//!    whose bases the running block has baked in, and can reach `note_code_write` with this
//!    block's native code live on the stack -- which is the exact situation
//!    `note_code_write_inner`'s "no compiled block is mid-execution" proof rules out. The helper
//!    refuses that state as its FIRST statement, before anything has run. Fail-closed by
//!    construction, not by argument.
//! 2. `check_io_permission` returns `Err` anyway. Unreachable given (1) -- it is the
//!    interpreter's own gate, kept so that if the two predicates ever drift apart this refuses
//!    rather than proceeds. It runs BEFORE `read_io`, so no device has been addressed.
//! 3. `bus.read_io` returns `Err`. For `MachineBus` the sole producer is the `UnsupportedPort`
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
//!
//!   That is the WHOLE charge, and the static lane must contribute NOTHING to it:
//!   `DirectKind::raw_clocks` carries an explicit `CallOut { .. } => 0` arm, the same shape
//!   `X87` carries. Omitting it selects the `_ => 2` default, which lands on top of the runtime
//!   12 and makes every native IN cost 14 raw against the interpreter's 12. That shipped, and
//!   neither the emitter's own `completed_raw` assertion (it sums the same accessor it checks)
//!   nor a single-slot differential (the two-clock error floors away at the 586 dial) could see
//!   it. `cpu_jit_callout_matrix_test.rs` separates them by ACCUMULATION, one to four slots.
//! * **The device's view of time.** `read_io` takes `core_clocks_so_far`, which the interpreter
//!   sets to the run's running total before each instruction. Inside a block that value is stale
//!   by the RUN's prefix -- this block's earlier slots and, on a chained entry, every hop before
//!   it. The helper is handed BOTH accounting lanes, raw and weighted-FP, and folds them exactly
//!   as `run_direct_block` does (FP through `scale_weighted_fp_clocks`, added to raw, then the
//!   persona's long division). Both scalings are previews: neither `fp_rem` nor `timing_rem` is
//!   consumed, so no accounting moves and the charge is still made once, later, by the block's
//!   single batch call. The FP lane is not decoration -- a call-out block never holds an x87
//!   slot, but a float-entered CHAIN can hop into one, and reading only the raw lane would hand
//!   the device a timestamp short by the whole float part of the chain. Without any of this a
//!   mid-block IN samples a beam or a counter in the past.
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
//! A call-out cannot reach `note_code_write`, and after the privilege refusal above the argument
//! needs no caveat: the TSS probe was the only memory access on this path and it is now
//! unreachable. `write_gpr8` is register state and `read_io` writes no guest memory, so the path
//! contains no guest memory access at all. `debug_assert`ed by comparing the written-page
//! bookkeeping across the whole remaining body.
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
pub(crate) type CallOutFn = unsafe extern "C" fn(*mut CpuGsw, u64, u64) -> i64;

/// The live bus and the monomorphised helpers for it, published by `run_direct_block` for the
/// duration of one native entry into a block that carries at least one call-out slot.
///
/// `bus` is a type-ERASED `*mut B`. It is sound only because the same call that publishes it also
/// publishes helpers monomorphised over that exact `B`, and both are cleared when the entry
/// returns; nothing else reads either field.
/// A CLEARED table is not a safety net. `port_read_al_dx` of 0 makes the emitted slot's
/// `call rax` jump to address zero; the `debug_assert` inside the helper is on the far side of
/// that and can never run. What makes the cleared state safe is that emitted code can only reach
/// the load while `run_direct_block` holds the window open, and `run_direct_block` publishes
/// unconditionally on every entry. The zero is a tripwire for a debugger, not a guard.
#[derive(Debug)]
pub(crate) struct CallOutTable {
    pub(crate) bus: *mut (),
    /// `CallOutHelper::PortReadAlDx`, as a `usize` so the emitted load is one plain quadword.
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

/// Excluded from CPU equality and reset on clone, exactly like `DirectRuntimeState` and
/// `FastMapServeGate`: this is a host-side window into a bus that is live for the duration of one
/// native entry and cleared the moment it returns. Two architecturally identical CPUs can hold
/// different erased pointers here (or one can hold a stale value a clone must not inherit), and
/// neither fact is guest state. Deriving over a raw pointer would have made a differential test
/// compare host addresses.
impl Clone for CallOutTable {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for CallOutTable {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for CallOutTable {}

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
unsafe extern "C" fn port_read_al_dx<B: CpuBus>(
    cpu: *mut CpuGsw,
    prefix_raw_clocks: u64,
    prefix_weighted_fp_clocks: u64,
) -> i64 {
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
    // The DENOMINATOR for the two side-exit counters, and the only always-on evidence that the
    // mechanism ran at all. Counted before the refusal below so "abnormal / executed" is a real
    // ratio rather than a count of one arm.
    cpu.jit_direct.note_callout_executed();

    // THE PERMISSION-CHECKED PORT IS REFUSED, BEFORE ANYTHING RUNS. This is the first statement
    // in the body on purpose: at this point the helper has read two fields and done nothing else,
    // so "zero partial effects" is true by construction rather than by argument.
    //
    // `check_io_permission` takes its early return when `!is_v86_mode() && cpl <= iopl`. On the
    // other branch it probes the TSS through `read_system_linear`, and under paging THAT WALK
    // WRITES GUEST MEMORY: PDE/PTE accessed bits go through the page-walk write path, which
    // records `written_pages`, can set CR2, advances bus clocks, can evict a TLB entry and
    // invalidate the fast map whose bases this block has BAKED IN, and can reach
    // `note_code_write` -- with this block's native code live on the stack, which is exactly the
    // situation `note_code_write_inner`'s "no compiled block is mid-execution when this runs"
    // proof (core.rs) says cannot happen.
    //
    // Supporting that would mean unwinding all five of those. Refusing it costs a privileged or
    // V86 guest the call-out and nothing else: the native run ends at the call-out and the
    // INTERPRETER executes the whole instruction, TSS probe included, exactly as it does today
    // for a block that stopped at an IN barrier. Same boundary, same charge, same faults.
    //
    // Neither shipped fixture reaches it (both take the CPL0 early return), which is precisely
    // why it has to be structural rather than tested-by-the-fixtures; `paged_v86_call_out_is_...`
    // in cpu_jit_callout_test.rs is the fixture that does reach it.
    if cpu.is_v86_mode() || cpu.current_privilege_level() > cpu.iopl() {
        return STATUS_ABNORMAL;
    }

    // The SMC assertion, and it now guards a path that has NO memory access left in it: the
    // permission probe was the only one, and the guard above excluded it. `read_io` writes no
    // guest memory and `write_gpr8` is register state, so both counters must come back unchanged.
    // Placed to cover the whole remaining body rather than as a tripwire for a hazard that is
    // still reachable -- the hazard is gone.
    #[cfg(debug_assertions)]
    let written_before = (cpu.written_count, cpu.written_pages_overflow);

    // The device's view of guest time. `core_clocks_so_far` was set to the run's total at block
    // ENTRY; the RUN's prefix has executed since -- this block's earlier slots and, on a chained
    // entry, every hop before it.
    //
    // BOTH lanes are previewed, not just the raw one. A call-out block never holds an x87 slot,
    // but a FLOAT-ENTERED CHAIN can hop into one, and those earlier hops deposited their cost in
    // `STACK_WEIGHTED_FP_CLOCKS`, which only becomes clocks through `scale_weighted_fp_clocks`.
    // Reading only the raw lane would hand the device a timestamp short by the whole float part
    // of the chain. This mirrors `run_direct_block`'s own fold exactly -- FP clocks scaled first,
    // added to raw, then the persona's long division -- and both scalings are read-only: neither
    // `fp_rem` nor `timing_rem` is consumed, so the charge itself is still made once, later, by
    // the block's single batch call.
    let fp =
        crate::jit::native_x87::scale_weighted_fp_clocks(prefix_weighted_fp_clocks, cpu.fp_rem);
    let now = cpu
        .core_clocks_so_far
        .saturating_add(cpu.preview_scale_clocks(prefix_raw_clocks.saturating_add(fp.clocks)));

    let port = cpu.read_gpr16(2);
    let ring0 = cpu.is_ring0_protected();
    // Kept even though the guard above has already established its early-return condition. It is
    // the interpreter's own gate, it is cheap once the TSS branch is unreachable, and if the two
    // predicates ever drift apart this refuses rather than proceeds.
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
    prefix_weighted_fp_clocks: u64,
) -> i64 {
    cpu.native_callout = CallOutTable::publish(bus);
    // SAFETY: the table was just published for this exact `B`, and `cpu` is not otherwise
    // borrowed across the call.
    let status = unsafe {
        port_read_al_dx::<B>(
            cpu as *mut CpuGsw,
            prefix_raw_clocks,
            prefix_weighted_fp_clocks,
        )
    };
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
    // Both accounting lanes are read BEFORE the scratch frame moves RSP, since the constants are
    // RSP-relative. RAX and RDX are the scratch pair (neither is a guest home), so holding the
    // two values across the frame adjust costs nothing.
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_RAW_CLOCKS);
    if static_prefix_raw != 0 {
        e.add_r64_imm32(Reg::RAX, u32::from(static_prefix_raw));
    }
    e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_WEIGHTED_FP_CLOCKS);
    e.sub_r64_imm32(Reg::RSP, CALLOUT_CALL_FRAME);
    // Argument 2 FIRST: on Windows `FLAGS_ARG` IS RDX, so writing argument 1 before reading RDX
    // would destroy the FP lane. On SysV `QUOTA_ARG` is RDX and this degenerates to `mov rdx,rdx`.
    // (Windows' `QUOTA_ARG` is R8, a guest home -- already spilled, and reloaded after the call.)
    e.mov_r64_r64(QUOTA_ARG, Reg::RDX);
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
