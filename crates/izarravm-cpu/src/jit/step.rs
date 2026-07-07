//! The per-instruction step function a compiled loop-region calls, and the `RegionCtx` it
//! communicates through. One call per guest instruction slot: the step does exactly what one
//! `run_straight_line` continuation does (the `run_one_cached` prologue, the cached-fetch charge,
//! the execute dispatch, and the post-instruction break checks, in the same order), so guest
//! semantics, bus traffic, and clock accounting are interpreter-identical by construction. The
//! emitted native code contributes only the call sequencing and the loop back-edge; it never
//! computes guest state itself.
//!
//! What the region defers to its exit (see `Cpu386::run_region`): `scale_clocks` (one batch call,
//! sound by the remainder-carry identity the batch-identity test pins), `elapsed_clocks`,
//! `perf.instructions`, and ring-0 residency. Nothing executed inside an admitted region reads
//! any of those mid-run: the shape matcher admits no RDTSC/WRMSR (the `elapsed_clocks` readers)
//! and no IF-writer (so `can_take_interrupt` cannot transition inside, which is what makes the
//! run loop's whole-region interrupt-transition check equivalent to the interpreter's per-
//! instruction one). The dispatch additionally refuses entry while `interrupt_shadow` is set, so
//! the first slot cannot consume a shadow the interpreter would have broken on.

use std::ffi::c_void;

use izarravm_bus::CpuBus;

use crate::{Cpu386, DecodedInsn, InternalFault};

/// One guest instruction slot of a compiled region: the decoded instruction (refreshed wholesale
/// on every re-stamp, which is how self-patched immediates stay current) and the linear address
/// of its first byte (fixed relative to the region entry; a region is only entered through the
/// decode line it was stamped on, so the absolute linears cannot have moved).
pub(crate) struct Slot {
    pub insn: DecodedInsn,
    pub lin: u32,
    /// How the emitted code handles this slot. The register-only kinds (mov/add/shr) are inlined
    /// natively against `gpr[]` plus a flag-helper call, skipping the interpreter's full decode
    /// dispatch; the Memory kind reuses the v1 per-slot step (the bus-bound memory operand
    /// resolution cannot be inlined without a trampoline per access, out of scope for v2).
    pub kind: SlotKind,
}

/// The emitted-code strategy for one slot. Set once by the matcher from the captured decode; the
/// emitter reads it to decide between a native inline op and a `region_step` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotKind {
    /// `mov r32, r32` (opcode 0x8B, modrm mode 3): native gpr load + store, no flags, no call.
    RegMov { dst: u8, src: u8 },
    /// `add r32, imm32` (opcode 0x81 /0, mode 3): native add + `jit_set_pending_add` call.
    /// `imm` is read from the captured decode (self-patched slots refresh on re-stamp).
    RegAddImm { dst: u8, imm: u32 },
    /// `shr r32, imm8` (opcode 0xC1 /5, mode 3): native shr + `jit_set_shift_flags_shr` call.
    RegShrImm { dst: u8, count: u8 },
    /// A memory-operand slot (load/store/r-m-w): the full v1 `region_step` (decode dispatch +
    /// bus-bound memory resolution + fault handling).
    Memory,
    /// The final rel8 Jcc back-edge (taken = loop, not-taken = LoopDone).
    BackEdge,
}

/// Why the region returned. `Boundary` covers every exit the run loop's own post-checks
/// reproduce by re-testing live state (step break, cap, generation bump); the loop then breaks
/// with the same `brk_*` attribution the interpreter would have used, or keeps interpreting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RegionExitKind {
    #[default]
    Boundary,
    /// The final conditional branch fell through: the guest loop is done. The run continues
    /// interpreted at the fall-through instruction, exactly as after a not-taken branch.
    LoopDone,
}

/// The signature of `region_step` behind the ctx's function-pointer field. `Option` for the
/// not-yet-entered state: the null-pointer optimization guarantees `Option<fn>` is one pointer
/// wide with `None` as 0, so the emitted code's raw 8-byte load of `[ctx + 0]` reads the
/// address directly.
pub(crate) type RegionStepFn =
    unsafe extern "C" fn(cpu: *mut Cpu386, bus: *mut c_void, ctx: *mut RegionCtx, k: u32) -> u8;

/// Signature of the `jit_set_pending_add` flag helper, stored as a raw fn pointer the inline
/// emitter loads and calls indirectly (the helper is a Rust method the emitter cannot address by
/// offset). `Option` for the same null-pointer-optimization reason as `RegionStepFn`.
#[cfg(feature = "jit")]
pub(crate) type SetPendingAddFn = unsafe extern "C" fn(cpu: *mut Cpu386, a: u32, b: u32);

/// Signature of `jit_set_shift_flags_shr`, the SHR flag helper.
#[cfg(feature = "jit")]
pub(crate) type SetShiftFlagsFn = unsafe extern "C" fn(cpu: *mut Cpu386, value: u32, count: u8);

/// Signature of the charge-cached-fetch call-out. Advances eip by `len` and charges the bus for
/// the instruction fetch. Returns 0 on success, 1 on fault (the fault is recorded in ctx).
#[cfg(feature = "jit")]
pub(crate) type ChargeFetchFn = unsafe extern "C" fn(
    cpu: *mut Cpu386,
    bus: *mut c_void,
    ctx: *mut RegionCtx,
    lin: u32,
    len: u32,
) -> u8;

/// Signature of the scaled-bus-clocks call-out. Returns the live in-batch scaled bus clock count.
#[cfg(feature = "jit")]
pub(crate) type BusClocksFn = unsafe extern "C" fn(bus: *const c_void) -> u64;

/// Signature of the decode-line liveness probe. Returns 1 if the slot's line is live, 0 otherwise.
#[cfg(feature = "jit")]
pub(crate) type LineLiveFn = unsafe extern "C" fn(cpu: *const Cpu386, lin: u32, d: bool) -> bool;

/// The mailbox between the dispatch (`Cpu386::run_region`), the emitted code, and the step fns.
/// `step_fn` MUST stay the first field: the emitted prologue loads it from `[ctx + 0]`;
/// `inline_step_fn` is the second field, loaded from `[ctx + 8]`. Every other field is Rust-only.
/// Boxed by the compile driver so its address is stable and disjoint from the `Cpu386` allocation
/// (the step function holds `&mut` to both at once).
#[repr(C)]
pub(crate) struct RegionCtx {
    /// `region_step::<B>` monomorphized for the live bus type, written by the dispatch on every
    /// entry (a CPU can run under different bus types across calls; the compiled bytes are
    /// bus-agnostic because they only forward this pointer). Used for Memory and BackEdge slots.
    pub step_fn: Option<RegionStepFn>,
    /// `region_inline_slot::<B>` for the register-only inline slots, written by the dispatch on
    /// every entry alongside `step_fn`. Loaded by the prologue from `[ctx + 8]` (the second
    /// `Option<fn>` field; the null-pointer optimization keeps each one pointer-wide at 8 bytes).
    pub inline_step_fn: Option<RegionStepFn>,
    /// Raw fn pointer to `Cpu386::jit_set_pending_add`, loaded by the inline add slot's helper
    /// call. At `[ctx + 16]` (third pointer field).
    #[cfg(feature = "jit")]
    pub set_pending_add_fn: Option<SetPendingAddFn>,
    /// Raw fn pointer to `Cpu386::jit_set_shift_flags_shr`, loaded by the inline shr slot's helper
    /// call. At `[ctx + 24]` (fourth pointer field).
    #[cfg(feature = "jit")]
    pub set_shift_flags_fn: Option<SetShiftFlagsFn>,
    /// Fn pointer to the charge-cached-fetch call-out (advances eip + charges bus). At `[ctx+32]`.
    #[cfg(feature = "jit")]
    pub charge_fetch_fn: Option<ChargeFetchFn>,
    /// Fn pointer to the scaled-bus-clocks call-out. At `[ctx+40]`.
    #[cfg(feature = "jit")]
    pub bus_clocks_fn: Option<BusClocksFn>,
    /// Fn pointer to the decode-line liveness probe. At `[ctx+48]`.
    #[cfg(feature = "jit")]
    pub line_live_fn: Option<LineLiveFn>,
    pub slots: Vec<Slot>,
    /// Index of the final slot (the back-edge Jcc). Taken = native back-edge; not taken =
    /// `LoopDone`.
    pub jnz_slot: u32,
    /// eip of the region's first instruction at this entry (the back-edge target).
    pub entry_eip: u32,

    // Per-entry state, reset by the dispatch before each region call.
    /// Raw (unscaled) core clocks accumulated by completed slots this entry.
    pub raw_clocks: u64,
    /// Instructions retired this entry (folded into `perf.instructions` at exit).
    pub insn_count: u32,
    /// `run_straight_line`'s scaled `total` at region entry.
    pub run_total_at_entry: u64,
    /// The run's `bus_at_entry` (NOT the region's): cap checking mirrors the loop exactly.
    pub bus_at_run_start: u64,
    pub cap: u64,
    /// `timing_rem` at region entry plus the live `level_timing` factors: `scaled_prefix` is the
    /// pure form of the interpreter's per-instruction `scale_clocks` sums (remainder-carry
    /// identity), computed without mutating `timing_rem` mid-region.
    pub rem0: u64,
    pub scale_num: u32,
    pub scale_den: u32,
    /// The region's decode-line D bit, for the next-line liveness probe below.
    pub d: bool,

    // Exit report.
    pub exit: RegionExitKind,
    /// A fault raised by a slot: the faulting instruction's start eip (for the
    /// `finish_instruction`-mirror rewind) and the fault itself. The slot's execution already
    /// unwound without committing (interpreter fault contract); it is NOT re-executed.
    pub fault: Option<(u32, InternalFault)>,
    pub halted: bool,
}

impl RegionCtx {
    /// Scaled guest clocks for `raw` core clocks charged from this entry's starting remainder:
    /// `floor((rem0 + raw*num) / den)`. Equals the sum the interpreter's per-instruction
    /// `scale_clocks` calls would have produced, by the exact-division remainder carry.
    #[inline]
    pub(crate) fn scaled_prefix(&self, raw: u64) -> u64 {
        (self.rem0 + raw * u64::from(self.scale_num)) / u64::from(self.scale_den)
    }
}

/// The signature the emitted region code is called through.
pub(crate) type RegionEntryFn =
    unsafe extern "C" fn(cpu: *mut Cpu386, bus: *mut c_void, ctx: *mut RegionCtx) -> i64;

/// Status protocol with the emitted code: 0 = proceed (next slot, or the back-edge from the
/// final slot); nonzero = return to the dispatch, which reads the details from the ctx.
const CONTINUE: u8 = 0;
const STOP: u8 = 1;

/// Execute one region slot. Mirrors, in order: `run_straight_line`'s pre-continuation
/// `core_clocks_so_far` update, `run_one_cached`'s prologue (shadow consume, `begin_instruction`)
/// and body (cached-fetch charge + execute dispatch), then the run loop's post-instruction
/// checks (halted, step break, cap) plus the generation re-check that stands in for the next
/// continuation's decode probe. The interrupt-enable transition check is intentionally absent:
/// no admitted shape contains an IF writer and entry requires a clear shadow, so the transition
/// cannot occur inside a region (the run loop still performs its own check across the whole
/// region call).
///
/// SAFETY: called only from region code invoked by `Cpu386::run_region`, which guarantees `cpu`
/// and `bus` are live, non-aliased `&mut` for the whole region call, `ctx` is the boxed
/// `RegionCtx` of the running region (a separate allocation from `Cpu386`, so the two `&mut`
/// reborrows are disjoint; nothing reachable from the execute dispatch touches `jit_regions`),
/// and `B` is the concrete bus type behind `bus`.
pub(crate) unsafe extern "C" fn region_step<B: CpuBus>(
    cpu: *mut Cpu386,
    bus: *mut c_void,
    ctx: *mut RegionCtx,
    k: u32,
) -> u8 {
    // SAFETY: see the function-level contract.
    let cpu = unsafe { &mut *cpu };
    let bus = unsafe { &mut *(bus as *mut B) };
    let ctx = unsafe { &mut *ctx };
    let slot = &ctx.slots[k as usize];
    let insn = slot.insn;
    let lin = slot.lin;

    cpu.interrupt_shadow = false;
    cpu.begin_instruction();
    let start_eip = cpu.registers.eip;
    cpu.core_clocks_so_far = ctx.run_total_at_entry + ctx.scaled_prefix(ctx.raw_clocks);

    match cpu
        .charge_cached_fetch(bus, lin, insn.len)
        .and_then(|()| cpu.execute_hot_cached_or_decoded(&insn, bus))
    {
        Ok(outcome) => {
            ctx.raw_clocks += u64::from(outcome.core_clocks);
            ctx.insn_count += 1;
            // The run loop's break checks, in its exact order (halted, step, cap). All are
            // re-derivable from live state at exit, so the exit kind stays `Boundary` and the
            // loop re-attributes the break itself.
            if outcome.halted {
                ctx.halted = true;
                return STOP;
            }
            if bus.requires_step_break() {
                return STOP;
            }
            let total = ctx.run_total_at_entry + ctx.scaled_prefix(ctx.raw_clocks);
            if total + (bus.in_batch_scaled_bus_clocks() - ctx.bus_at_run_start) >= ctx.cap {
                return STOP;
            }
            // The next continuation's `decode_cache` probe, for real: a global invalidation
            // (generation bump) or a NARROW SMC kill of exactly the next slot's line makes the
            // interpreter's probe miss and end the run at this boundary, so the region must
            // stop here too. Nothing can re-decode a line mid-region (no decode runs inside),
            // so line-live implies the line still holds the insn the slot table captured.
            if k == ctx.jnz_slot {
                if cpu.registers.eip == ctx.entry_eip {
                    if !cpu.decode_cache.line_live(ctx.slots[0].lin, ctx.d) {
                        return STOP;
                    }
                    return CONTINUE; // taken: the emitted code loops to slot 0
                }
                ctx.exit = RegionExitKind::LoopDone;
                return STOP;
            }
            if !cpu
                .decode_cache
                .line_live(ctx.slots[k as usize + 1].lin, ctx.d)
            {
                return STOP;
            }
            CONTINUE
        }
        Err(fault) => {
            ctx.fault = Some((start_eip, fault));
            STOP
        }
    }
}

/// The bookkeeping for one register-only inline slot (mov r,r / add r,imm / shr r,imm): the emitted
/// native code has already performed the guest computation (gpr read/compute/write and, for add and
/// shr, the flag-helper call) BEFORE calling this. This function does the part that cannot be
/// inlined because it is bus-trait-bound or touches run-loop state: the pre-instruction prologue
/// (shadow consume, `begin_instruction`, `core_clocks_so_far`), the cached-fetch charge (the eip
/// advance plus the bus's instruction-fetch clock charge), the per-slot clock accumulation (these
/// opcodes are all 2 core_clocks), and the run loop's post-instruction break checks (halted, step,
/// cap) plus the next-line liveness probe.
///
/// ORDERING NOTE: v1's `region_step` charges the fetch BEFORE executing; this is called AFTER the
/// native execute. That reordering is observably identical for the admitted register-only shape:
/// mov/add/shr read neither eip nor the fetch result, the slots are matcher-verified warm-cached
/// (so `charge_cached_fetch` cannot fault mid-region), and the eip advance lands at the same value.
/// The differential suite's state+trace identity test pins this.
///
/// Returns CONTINUE (0) to proceed to the next slot, STOP (1) to exit to the dispatch. The fault
/// path is unreachable for a warm cached fetch in an admitted region but is handled for soundness.
///
/// SAFETY: same contract as `region_step`: called only from region code invoked by
/// `Cpu386::run_region`; `cpu`/`bus` are live non-aliased `&mut`, `ctx` is the boxed RegionCtx, `B`
/// is the concrete bus type.
pub(crate) unsafe extern "C" fn region_inline_slot<B: CpuBus>(
    cpu: *mut Cpu386,
    bus: *mut c_void,
    ctx: *mut RegionCtx,
    k: u32,
) -> u8 {
    // SAFETY: see the function-level contract.
    let cpu = unsafe { &mut *cpu };
    let bus = unsafe { &mut *(bus as *mut B) };
    let ctx = unsafe { &mut *ctx };
    let slot = &ctx.slots[k as usize];
    let insn = slot.insn;
    let lin = slot.lin;

    cpu.interrupt_shadow = false;
    cpu.begin_instruction();
    let start_eip = cpu.registers.eip;
    cpu.core_clocks_so_far = ctx.run_total_at_entry + ctx.scaled_prefix(ctx.raw_clocks);

    // The native code already executed the guest op (gpr compute + flags); charge the fetch (eip
    // advance + bus clocks) and, if it faults (unreachable for a warm cached line, handled for
    // soundness), report exactly as region_step would.
    if let Err(fault) = cpu.charge_cached_fetch(bus, lin, insn.len) {
        ctx.fault = Some((start_eip, fault));
        return STOP;
    }

    // The opcodes inlined here (mov/add/shr register forms) are all 2 core_clocks (the same value
    // execute_hot_cached_decoded returns for them); accumulate it and the retired-instruction count.
    ctx.raw_clocks += 2;
    ctx.insn_count += 1;

    // The run loop's break checks. Halted is always false for these opcodes (mov/add/shr do not
    // halt). The step-break check (requires_step_break) is elided: register-only slots do no port
    // I/O (the only thing that sets io_touched), and no admitted shape contains an INT (the only
    // thing that sets pending_soft_int), so requires_step_break is provably false here. The
    // charge_cached_fetch above touches the bus for an instruction-fetch, which never sets
    // io_touched (it goes through charge_instruction_fetch_run, not read_io/write_io). The cap
    // check stays: the fetch charge can add bus clocks that push over the threshold.
    let total = ctx.run_total_at_entry + ctx.scaled_prefix(ctx.raw_clocks);
    if total + (bus.in_batch_scaled_bus_clocks() - ctx.bus_at_run_start) >= ctx.cap {
        return STOP;
    }
    // The next continuation's decode probe: an inline slot can never be the back-edge slot (the
    // matcher classifies the final Jcc as BackEdge, handled by region_step), so this is always the
    // "is the next slot's line still live" check.
    if !cpu
        .decode_cache
        .line_live(ctx.slots[k as usize + 1].lin, ctx.d)
    {
        return STOP;
    }
    CONTINUE
}

// -------- Native cap-check call-outs (feature `jit`) --------
//
// These are the bus-trait-bound and cpu-bound operations the emitted native code calls via fn
// pointers from the RegionCtx. They replace the per-slot `region_inline_slot` trampoline: the
// emitted code does the native gpr op, then calls these for the bus-bound parts (fetch charge,
// bus-clock read, line liveness), and does the cap-check arithmetic natively between calls.

/// Charge the cached fetch for one slot: advance eip by `len` and charge the bus for the
/// instruction-fetch clocks. Returns 0 on success, 1 on fault (records the fault in ctx).
/// SAFETY: same contract as `region_step` — cpu/bus live non-aliased &mut, ctx is the boxed
/// RegionCtx.
#[cfg(feature = "jit")]
pub(crate) unsafe extern "C" fn jit_charge_fetch<B: CpuBus>(
    cpu: *mut Cpu386,
    bus: *mut c_void,
    ctx: *mut RegionCtx,
    lin: u32,
    len: u32,
) -> u8 {
    let cpu = unsafe { &mut *cpu };
    let bus = unsafe { &mut *(bus as *mut B) };
    let ctx = unsafe { &mut *ctx };
    cpu.interrupt_shadow = false;
    cpu.begin_instruction();
    cpu.core_clocks_so_far = ctx.run_total_at_entry + ctx.scaled_prefix(ctx.raw_clocks);
    if let Err(fault) = cpu.charge_cached_fetch(bus, lin, len as u8) {
        ctx.fault = Some((cpu.registers.eip, fault));
        return 1;
    }
    0
}

/// Read the live in-batch scaled bus clock count from the bus. A pure `&self` read.
/// SAFETY: `bus` must be the live bus pointer the region was entered with.
#[cfg(feature = "jit")]
pub(crate) unsafe extern "C" fn jit_bus_clocks<B: CpuBus>(bus: *const c_void) -> u64 {
    let bus = unsafe { &*(bus as *const B) };
    bus.in_batch_scaled_bus_clocks()
}

/// Probe whether a decode line is live (generation-current, matching tag and D bit).
/// SAFETY: `cpu` must be the live Cpu386 pointer the region was entered with.
#[cfg(feature = "jit")]
pub(crate) unsafe extern "C" fn jit_line_live(cpu: *const Cpu386, lin: u32, d: bool) -> bool {
    let cpu = unsafe { &*cpu };
    cpu.decode_cache.line_live(lin, d)
}
