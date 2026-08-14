// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Interpreter CALL-OUT slots: running one instruction through the interpreter from inside a
//! native block, then resuming the same block.
//!
//! Three opcodes live here: `0xEC` (IN AL,DX), `0x60` (PUSHAD) and `0x61` (POPAD). Every other
//! barrier opcode still ends its block; this module is the mechanism, `classify`'s three arms are
//! the policy.
//!
//! The three fall into TWO CLASSES with different reachable sets, and almost every design decision
//! below is really a decision about which class an opcode is in:
//!
//! | class | member | why a call-out rather than a lowering | touches |
//! |---|---|---|---|
//! | port | `0xEC` | emitted code cannot reach `CpuBus::read_io` at all | one device, one GPR byte |
//! | memory | `0x60`, `0x61` | code size: eight guarded accesses per instruction | eight stack dwords, eight GPRs |
//!
//! The memory class is the one that justifies the mechanism beyond port IO, and the justification
//! is a SIZE argument, not a reachability one: emitted code can reach guest memory perfectly well.
//! A lowered PUSHAD is eight guarded stores -- eight address computations, eight fast-map probes,
//! eight permission checks, eight code-watch guards and eight side-exit stub sets -- against a
//! one-host-page block budget that `MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS` already caps at four
//! instructions for slots of that shape. A call-out is a fixed ~60 bytes whatever the instruction
//! does. See the module-level note at the end of this comment for the shape that WOULD beat it.
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
//! `iopl`/`is_v86_mode`), CPL, `tr`, `core_clocks_so_far`, `timing_rem`, the persona, and -- on
//! the TSS-bitmap arm only -- the TLB (read-only, `resident_translate_system`) and two bytes of
//! plain guest RAM (uncharged, `CpuBus::peek_direct_ram`).
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
//! # The reload contract, RE-DERIVED for a helper that mutates the register file
//!
//! `0xEC` writes one byte of one register. `POPAD` writes EIGHT registers including ESP, and
//! `PUSHAD` writes ESP. The reload above was written for the first and has to be shown sufficient
//! for the second rather than assumed to carry over. It is, and here is the whole derivation:
//!
//! 1. **Coverage.** `GUEST_HOMES` is all eight GPRs (R8-R14 plus RBX, indices 0..8), the reload
//!    loop is `for (index, home) in GUEST_HOMES.into_iter().enumerate()` with no filter, and it
//!    runs on every path before the status branch. So every register a helper can write is
//!    reloaded. There is no partial-set optimisation to get wrong, and `emit_store_homes` on the
//!    way IN is the same whole set, so the helper reads current values for all eight -- which
//!    `0xEC` did not need (it read only DX) and `PUSHAD` does.
//! 2. **Nothing else is cached.** The only other guest state a block body holds in a host register
//!    is EFLAGS, shadowed in RBP. Neither PUSHAD nor POPAD writes a flag (386 PRM: both are
//!    flag-transparent), so the shadow stays current and must NOT be reloaded -- reloading it
//!    would be the bug, because RBP is the authority mid-block, not memory.
//! 3. **The lazy-flag descriptor survives.** `PendingFlags` carries VALUES (`tag`, `a`, `b`,
//!    `result`), not register references, so a pending descriptor produced before the call still
//!    evaluates to the same flags after POPAD has overwritten the registers those values came
//!    from. Had it carried register indices, POPAD would have silently corrupted every lazy flag
//!    in flight and no reload could have fixed it.
//! 4. **Stack addressing follows.** Later slots' stack addresses are emitted relative to
//!    `home(4)`, which the reload has just refreshed from `registers.gpr[4]`, so a `push` after a
//!    `PUSHAD` in the same block addresses the post-PUSHAD ESP. Nothing bakes an ESP value.
//! 5. **EIP is untouched** by both helpers, so the block-body invariant (`EIP` holds the entry
//!    value throughout, advanced only on exit paths) is unbroken and both exits still advance it
//!    exactly once.
//!
//! Conclusion: the existing contract is sufficient, unchanged, for a register-file-mutating
//! helper. The mutation that proves it is dropping any one entry from the reload loop, which now
//! fails the POPAD differential on `registers` as well as the port tests on AL.
//!
//! # The abnormal set, memory class (`0x60`, `0x61`)
//!
//! ONE producer, and it is a pre-check rather than a discovered failure:
//! `CpuGsw::call_out_stack_frame_resident` (memory.rs) refuses unless all eight dwords of the
//! frame can be moved through the FastMap serve path with no page walk, no fault and no
//! code-watch hit. Its clause-by-clause enumeration -- 16-bit stack, unarmed FastMap, unaligned
//! ESP, SS limit (including an ESP WRAP), absent/read-only/wrong-CPL page, stale mapping epoch, a
//! frame CROSSING A PAGE BOUNDARY onto a second page that is not resident, the Mode13h aperture,
//! and watched code -- lives at that function, next to the predicates it evaluates.
//!
//! Two things make the refusal cheap rather than a loss. It is `&self` and touches nothing, so a
//! refused call-out has ZERO partial effects by construction, exactly like the port class's
//! privilege refusal. And the cost of being wrong is one call-out: the native run ends at the
//! instruction, the interpreter executes PUSHAD or POPAD whole -- page walk, fault delivery,
//! partial frame and all -- at the same boundary a pre-slice barrier produced.
//!
//! The privilege gate the port class carries is DELIBERATELY ABSENT here, and the difference is
//! the TSS. `0xEC`'s two-phase probe exists because `check_io_permission` reads the IO-permission
//! bitmap through `read_system_linear`, which page-walks. PUSHAD and POPAD are unprivileged
//! instructions
//! with no such probe: their only privilege-sensitive decision is page protection, and that is
//! made by `FastMap::lookup_access` against the LIVE CPL and CR0.WP inside the pre-check, which
//! fails closed to a refusal. So a CPL-3 or IOPL-restricted guest keeps its PUSHAD call-outs; only
//! a block carrying a PORT call-out is refused entry by `run_direct_block`, which is why the
//! dispatch gate is keyed on `callout_port_slots()` rather than on `callout_slots()`.
//!
//! # The abnormal set, port class (`0xEC`) -- exactly three producers
//!
//! 1. **The permission-checked port, refused by PHASE P.** When `is_v86_mode() || CPL > IOPL`,
//!    `check_io_permission` reads the TSS bitmap through `read_system_linear` -- and under paging
//!    a TLB MISS there walks, and the walk WRITES guest memory (PDE/PTE accessed bits), records
//!    `written_pages`, can set CR2, advances bus clocks mid-run, can evict a TLB entry and
//!    invalidate the fast map whose bases the running block has baked in, and can reach
//!    `note_code_write` with this block's native code live on the stack -- which is the exact
//!    situation `note_code_write_inner`'s "no compiled block is mid-execution" proof rules out.
//!    The helper therefore answers that state with a PURE probe first: TLB hits only
//!    (`resident_translate_system`), an uncharged RAM read (`CpuBus::peek_direct_ram`), and a
//!    refusal on anything it cannot settle -- a miss, a non-RAM or misaligned physical, a TSS
//!    limit overrun, or a bitmap bit that denies the port. Only then does phase C charge. Every
//!    refusal is taken before ANY effect, so fail-closed is by construction, not by argument.
//! 2. `check_io_permission` returns `Err` anyway. Runs only on the OTHER arm, where it is the
//!    interpreter's own early return and is kept so that if the two predicates ever drift apart
//!    this refuses rather than proceeds. It runs BEFORE `read_io`, so no device has been
//!    addressed.
//! 3. `bus.read_io` returns `Err`. For `MachineBus` the sole producer is the `UnsupportedPort`
//!    fall-through, reached only after every device declined the port -- so no device observed
//!    the access there either.
//!
//! Phase C's two charged re-reads are NOT a fourth producer: each address survived
//! `direct_page_ram_bytes` inside the peek, so `read_memory_direct` takes its aligned direct-RAM
//! arm and cannot fail. They are asserted, not handled.
//!
//! **Zero partial effects.** On producers 1 and 2, and on producer 3 from the CPL0 arm, at the
//! moment of return: `registers.gpr` is byte-identical
//! to the pre-call state (AL is written only after a successful read), EFLAGS and `pending_flags`
//! are untouched (IN writes no flags), EIP still holds the block-entry value, `elapsed_clocks`,
//! `perf`, `timing_rem`, `written_pages` and `prefetch` are untouched, and no clocks are charged
//! (the caller skips the raw-clock add on the negative branch). The native run then ends with EIP
//! at the call-out, which is byte-for-byte the state the run loop sees TODAY when a block ends at
//! an IN barrier -- so the interpreter re-executes the instruction and delivers exactly what it
//! delivers today. FAIL-CLOSED: any status the helper cannot certify is negative.
//!
//! **The one exception, stated rather than hidden: producer 3 reached from the BITMAP arm.**
//! There, C1 and C2 have already charged the two TSS data reads before `read_io` is called, so an
//! `Err` returns after a charge. The interpreter then re-executes the whole instruction and
//! charges those two reads again: the run is short by nothing and long by two data-read cycles.
//! Nothing guest-visible other than timing moves -- no register, no flag, no guest memory, no
//! device.
//!
//! It is unreachable on the production bus and OPT-IN everywhere else. `MachineBus::read_io` has
//! exactly one `Err` producer, `self.open_bus.note(port, false)?`, reached only when nothing
//! decoded the port; `note` errs only for a port in the fatal set, which is populated solely by
//! `OpenBusPorts::from_env` reading `IZARRAVM_PORT_FATAL` and by a test-only `set_fatal`. `0x3DA`
//! is decoded by the VGA status path and never reaches that fall-through at all. So a guest has
//! to be run with `IZARRAVM_PORT_FATAL` naming the polled port before the discrepancy can occur,
//! and it is bounded at two data-read cycles per occurrence.
//!
//! This is the price of putting `read_io` LAST, and charge identity is what requires that: the
//! interpreter's `check_io_permission` reads the TSS before it addresses the device, so any
//! ordering that hoisted `read_io` above C1/C2 would charge the three accesses in a different
//! order than the interpreter and forfeit the whole claim of section 2 of the design.
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
//! **Port class.** `0xEC` cannot reach `note_code_write`, and after the privilege refusal above
//! the argument needs no caveat: the TSS probe was the only memory access on this path and it is
//! now unreachable. `write_gpr8` is register state and `read_io` writes no guest memory, so the
//! path contains no guest memory access at all. `debug_assert`ed by comparing the written-page
//! bookkeeping across the whole remaining body.
//!
//! **Memory class.** `0x60` really does write thirty-two bytes of guest memory, so the argument
//! cannot be "no access" and is instead "no WATCHED access". `call_out_stack_frame_resident`
//! evaluates `code_write_watched(physical, 4)` for each of the eight stores and refuses the whole
//! frame if any is watched, which makes `finish_fast_map_write`'s `changed && watched` gate
//! provably false for all eight -- so `note_code_write_hit` is not called and the "no compiled
//! block is mid-execution" proof in `note_code_write_inner` is not put to the test. The same
//! pre-check excludes the page walk, which is the OTHER way this path could have written guest
//! memory (accessed/dirty bits) and reached that proof by the back door. `record_write_page` still
//! runs, exactly as it does for the interpreter's own PUSHAD, and is bookkeeping rather than
//! invalidation.
//!
//! # Why not lower PUSHAD natively, and what would change the answer
//!
//! Recorded because it is the honest alternative rather than to close it off. A lowered PUSHAD as
//! eight ordinary `Store` slots is not close: it would blow both the one-host-page budget and
//! `MAX_BLOCK_STACK_ACCESSES`. But PUSHAD writes a CONTIGUOUS, 4-aligned 32-byte range, so the
//! shape that would win is ONE guard over the whole range -- the same all-or-nothing trick
//! `MemoryWidth::Tbyte` already plays for a ten-byte x87 store -- followed by eight plain `mov`s
//! and one counter add, which is of the order of two hundred bytes. What that needs and this slice
//! does not build: a 32-byte guard width, a code-watch guard over a range wider than one watch
//! chunk, and a native answer for PUSHAD's restore-(E)SP-on-fault rule. If this family is ever
//! measured to be worth more wall than the call-out buys, that is the shape to build, not a bigger
//! call-out.
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
use crate::{IN_AL_DX_CORE_CLOCKS, POP_ALL_CORE_CLOCKS, PUSH_ALL_CORE_CLOCKS};
use izarravm_bus::{BusAccessKind, BusWidth, CpuBus};

/// How many dwords `PUSHAD`/`POPAD` move. Eight registers, one dword each -- the SP slot included,
/// which POPAD reads and discards.
pub(crate) const CALL_OUT_STACK_FRAME_DWORDS: u32 = 8;

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
    /// `CallOutHelper::PushAllDword`.
    pub(crate) push_all_dword: usize,
    /// `CallOutHelper::PopAllDword`.
    pub(crate) pop_all_dword: usize,
}

impl Default for CallOutTable {
    fn default() -> Self {
        Self {
            bus: core::ptr::null_mut(),
            port_read_al_dx: 0,
            push_all_dword: 0,
            pop_all_dword: 0,
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
            push_all_dword: push_all_dword::<B> as CallOutFn as usize,
            pop_all_dword: pop_all_dword::<B> as CallOutFn as usize,
        }
    }
}

/// Which interpreter path a `DirectKind::CallOut` slot routes through. One variant per admitted
/// opcode, never a catch-all: `callout::helper_offset` matches this exhaustively, so adding an
/// opcode to `classify` without an execute path is a compile error rather than a silent misroute.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallOutHelper {
    /// `0xEC` IN AL,DX.
    PortReadAlDx,
    /// `0x60` PUSHAD.
    PushAllDword,
    /// `0x61` POPAD.
    PopAllDword,
}

impl CallOutHelper {
    /// True for the PORT class: helpers that can reach `check_io_permission`, and therefore the
    /// TSS bitmap probe whose page walk is why `run_direct_block` refuses to enter such a block in
    /// the privileged state. False for the memory class, which has no privileged probe at all --
    /// its page-protection decision is made inside `call_out_stack_frame_resident` against the
    /// live CPL, and fails closed to a refusal. Keeping this a method rather than a `matches!` at
    /// the two call sites is what makes a fourth helper choose its class deliberately.
    pub(crate) fn probes_io_permission(self) -> bool {
        match self {
            Self::PortReadAlDx => true,
            Self::PushAllDword | Self::PopAllDword => false,
        }
    }

    /// True for the MEMORY class: helpers that move `CALL_OUT_STACK_FRAME_DWORDS` dwords of guest
    /// stack, which `compute_iteration_upper` has to price as bus traffic the block's static
    /// access counters do not contain.
    pub(crate) fn moves_a_stack_frame(self) -> bool {
        match self {
            Self::PortReadAlDx => false,
            Self::PushAllDword | Self::PopAllDword => true,
        }
    }
}

/// The per-class interpreter call-out slot counts, packed into ONE byte.
///
/// Two counts, each bounded by `MAX_BLOCK_CALLOUT_SLOTS` (4), so each fits a nibble. Packed rather
/// than carried as two `u8` fields because `CompiledBlock` is memcpy'd several times per Direct
/// entry and `compiled_block_stays_small_enough_to_copy_per_entry` pins its size at 120 bytes --
/// that byte budget is exactly full, so a second field costs eight bytes of alignment padding on
/// every one of ~47 M entries in a Quake run. The packing is a size decision rather than a
/// cleverness: each accessor is one mask or one shift, and the total is one add.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CallOutSlotCounts(u8);

impl CallOutSlotCounts {
    pub(super) fn new(port: u8, memory: u8) -> Self {
        debug_assert!(
            port <= 0xf && memory <= 0xf,
            "MAX_BLOCK_CALLOUT_SLOTS bounds both counts well below a nibble"
        );
        Self((port & 0xf) | ((memory & 0xf) << 4))
    }

    /// Slots whose helper can reach `check_io_permission`; see `CallOutHelper::probes_io_permission`.
    pub(super) fn port(self) -> u32 {
        u32::from(self.0 & 0xf)
    }

    /// Slots whose helper moves a guest stack frame; see `CallOutHelper::moves_a_stack_frame`.
    pub(super) fn memory(self) -> u32 {
        u32::from(self.0 >> 4)
    }

    /// The two counts summed. TEST-ONLY, and gated for the same reason
    /// `CompiledBlock::callout_slots` is: nothing in the budget path reads a total, because
    /// `compute_iteration_upper` prices the two classes separately, and asserting this against the
    /// pair it sums is vacuous. See that accessor's comment for where the real check lives.
    #[cfg(test)]
    pub(super) fn total(self) -> u32 {
        self.port() + self.memory()
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
        CallOutHelper::PushAllDword => core::mem::offset_of!(CallOutTable, push_all_dword),
        CallOutHelper::PopAllDword => core::mem::offset_of!(CallOutTable, pop_all_dword),
    };
    (core::mem::offset_of!(CpuGsw, native_callout) + field) as i32
}

/// What phase P learned, and the only thing phase C is allowed to use. Physicals rather than
/// linears on purpose: re-translating in phase C could take a different answer (and, on a miss, a
/// page walk), so the translation happens exactly once, in the pure phase.
#[derive(Clone, Copy)]
struct PortPermissionProbe {
    io_base_physical: u32,
    /// The peeked values, carried only so phase C's charged re-read can be asserted equal.
    io_base: u32,
    bitmap_physical: u32,
    bits: u32,
}

/// PHASE P for `port_read_al_dx`: the interpreter's `check_io_permission` TSS walk, redone with
/// no effect of any kind, refusing wherever it cannot answer purely.
///
/// `&CpuGsw` and `&B` are the contract, not a convenience: neither reference can mutate, so "this
/// runs before the helper is allowed to commit anything" is enforced by the compiler rather than
/// argued. Each `None` is one lane of the design's abnormal set:
///
/// * P1 the TSS is too small to hold the io_base word;
/// * P2 that word straddles a page (so one translate could not cover it);
/// * P3/P7 a TLB MISS -- the interpreter would walk here, which is the whole hazard;
/// * P4/P8 the physical is not aligned, page-local, A20-clean plain RAM;
/// * P5 the bitmap byte is past the TSS limit -- the interpreter's `#GP(0)`;
/// * P9 the bit is set -- the interpreter's other `#GP(0)`.
///
/// The two `#GP` lanes refuse rather than fault for the reason the caller states: raising from
/// inside a live block is the hazard class this design will not open, and the interpreter raises
/// the identical fault one instruction boundary later.
///
/// Byte width only, matching `0xEC`'s single-byte port access: the interpreter's loop over
/// `port..port + width.bytes()` collapses to one iteration.
fn port_permission_resident<B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    port: u16,
) -> Option<PortPermissionProbe> {
    if cpu.tr.limit < 0x67 {
        return None;
    }
    let io_base_linear = cpu.tr.base.wrapping_add(0x66);
    if io_base_linear & 0xfff > 0xffe {
        return None;
    }
    let io_base_physical = cpu.resident_translate_system(io_base_linear)?;
    let io_base = bus.peek_direct_ram(io_base_physical, BusWidth::Word)?;

    let byte_index = io_base + u32::from(port) / 8;
    if byte_index > cpu.tr.limit {
        return None;
    }
    let bitmap_physical = cpu.resident_translate_system(cpu.tr.base.wrapping_add(byte_index))?;
    let bits = bus.peek_direct_ram(bitmap_physical, BusWidth::Byte)?;
    if bits & (1 << (u32::from(port) % 8)) != 0 {
        return None;
    }

    Some(PortPermissionProbe {
        io_base_physical,
        io_base,
        bitmap_physical,
        bits,
    })
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
    #[cfg(feature = "direct-callout-attribution")]
    let original_port = cpu.read_gpr16(2);

    let port = cpu.read_gpr16(2);

    // PHASE P. Pure: no charge, no guest write, no `trace.record`, no device. It runs only in the
    // state `check_io_permission` does NOT early-return from -- V86, or CPL > IOPL -- and it
    // answers one question: can this instruction's TSS I/O-permission probe be satisfied without
    // any effect the helper is not allowed to have?
    //
    // The effect it must avoid is the PAGE WALK. `read_system_linear` translates through
    // `translate_linear_system`, and on a TLB miss that walks, and `write_page_walk_entry` sets
    // accessed bits through `bus.write_memory` + `record_write_page` + `note_code_write` -- with
    // this block's native code live on the stack, which is exactly the situation
    // `note_code_write_inner`'s "no compiled block is mid-execution when this runs" proof
    // (core.rs) says cannot happen. `resident_translate_system` therefore serves TLB HITS ONLY
    // and has no fallthrough to the walk at all, and `peek_direct_ram` reads plain RAM without
    // charging or recording. Every step of the probe can only answer or refuse.
    //
    // Refusing costs the guest the call-out and nothing else: the native run ends here and the
    // INTERPRETER executes the whole instruction, TSS probe, page walk and `#GP` included,
    // exactly as it does for a block that stopped at an IN barrier. Same boundary, same charge,
    // same faults. The miss case is self-healing -- the interpreted IN refills the TLB and the
    // next one is served natively.
    //
    // The DENIED cases (P5 limit overrun, P9 bitmap bit set) refuse for the same reason rather
    // than raising the fault here: `#GP(0)` from inside a live block is the whole hazard class
    // this design refuses to open, and the interpreter raises exactly that fault one boundary
    // later. Design doc section 1.2 enumerates the lanes; each has its own fixture.
    let permission = if cpu.is_v86_mode() || cpu.current_privilege_level() > cpu.iopl() {
        let Some(probe) = port_permission_resident(cpu, bus, port) else {
            #[cfg(feature = "direct-callout-attribution")]
            cpu.jit_direct.note_callout_attribution(
                CallOutHelper::PortReadAlDx,
                Some(original_port),
                CallOutOutcome::Abnormal,
            );
            return STATUS_ABNORMAL;
        };
        Some(probe)
    } else {
        None
    };

    // The SMC assertion. Phase P wrote nothing by construction; phase C's two reads are DATA
    // reads through the same aligned direct-RAM arm the interpreter takes, and `read_io` writes
    // no guest memory, so both counters must come back unchanged whichever arm ran.
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

    let ring0 = cpu.is_ring0_protected();
    // PHASE C. Committed: the charges happen here, in the interpreter's own order, and nothing
    // below may refuse.
    //
    // The two arms are the two arms of `check_io_permission` itself. With `permission` absent the
    // interpreter took its early return and charged NOTHING for the check, so the gate is re-run
    // here verbatim: it is cheap when it early-returns, and if the two predicates ever drift
    // apart this refuses rather than proceeds. With `permission` present the interpreter read the
    // io_base word and one bitmap byte through `read_system_linear`, and those two charges are
    // replayed at C1/C2 -- SAME addresses, SAME widths, SAME `DataRead` kind, SAME order, and
    // before `read_io`, which is row 9 of the design's charge table.
    //
    // Phase P's peek read the same bytes with no charge; these read them again with one. They
    // cannot disagree -- nothing between them writes guest memory -- and the two assertions pin
    // it. The physical survived `direct_page_ram_bytes`, whose `should_split` term rejects
    // exactly the misaligned case, so C1 is guaranteed the ALIGNED arm of `read_memory_direct`
    // and `charge_direct_ram_split` is unreachable from here; C2 is a byte and cannot split at
    // all. An `Err` is likewise unreachable for the same reason -- the aligned arm returns `Ok`
    // unconditionally -- and is asserted; the return and its attribution keep the helper honest
    // for a bus that violates the trait's promise, and keep `attempts == callout_executed` true
    // on the attribution snapshot whichever way this goes.
    match permission {
        None => {
            if cpu.check_io_permission(bus, port, BusWidth::Byte).is_err() {
                #[cfg(feature = "direct-callout-attribution")]
                cpu.jit_direct.note_callout_attribution(
                    CallOutHelper::PortReadAlDx,
                    Some(original_port),
                    CallOutOutcome::Abnormal,
                );
                return STATUS_ABNORMAL;
            }
        }
        Some(probe) => {
            let Ok(io_base) = bus.read_memory_direct(
                probe.io_base_physical,
                BusWidth::Word,
                BusAccessKind::DataRead,
            ) else {
                debug_assert!(false, "phase C re-read of a peeked io_base word failed");
                #[cfg(feature = "direct-callout-attribution")]
                cpu.jit_direct.note_callout_attribution(
                    CallOutHelper::PortReadAlDx,
                    Some(original_port),
                    CallOutOutcome::Abnormal,
                );
                return STATUS_ABNORMAL;
            };
            debug_assert_eq!(
                io_base.value, probe.io_base,
                "the charged io_base re-read disagreed with the pure peek"
            );
            let Ok(bits) = bus.read_memory_direct(
                probe.bitmap_physical,
                BusWidth::Byte,
                BusAccessKind::DataRead,
            ) else {
                debug_assert!(false, "phase C re-read of a peeked bitmap byte failed");
                #[cfg(feature = "direct-callout-attribution")]
                cpu.jit_direct.note_callout_attribution(
                    CallOutHelper::PortReadAlDx,
                    Some(original_port),
                    CallOutOutcome::Abnormal,
                );
                return STATUS_ABNORMAL;
            };
            debug_assert_eq!(
                bits.value, probe.bits,
                "the charged bitmap re-read disagreed with the pure peek"
            );
        }
    }
    let Ok(value) = bus.read_io(port, BusWidth::Byte, now, ring0) else {
        #[cfg(feature = "direct-callout-attribution")]
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PortReadAlDx,
            Some(original_port),
            CallOutOutcome::Abnormal,
        );
        return STATUS_ABNORMAL;
    };
    cpu.write_gpr8(0, value as u8);
    // The numerator the acceptance gate needs, and the reason it is separate from
    // `callout_executed`: on a mixed guest that count sums the CPL0 arm and this one, so it
    // cannot say whether the NEW arm ever served anything. Bumped last, so an abnormal return
    // from any lane -- including the unreachable `read_io` error -- is excluded by construction.
    if permission.is_some() {
        cpu.jit_direct.note_callout_port_v86_served();
    }

    #[cfg(debug_assertions)]
    debug_assert_eq!(
        written_before,
        (cpu.written_count, cpu.written_pages_overflow),
        "a call-out slot must never write guest memory"
    );

    // The SAME constant the interpreter's `0xec` arm charges, shared rather than copied, so the
    // exact-clocks claim cannot drift out from under this module.
    #[cfg(feature = "direct-callout-attribution")]
    {
        let step_break = bus.requires_step_break();
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PortReadAlDx,
            Some(original_port),
            if step_break {
                CallOutOutcome::StepBreak
            } else {
                CallOutOutcome::Continued
            },
        );
        i64::from(IN_AL_DX_CORE_CLOCKS) | (i64::from(step_break) << STATUS_STEP_BREAK_BIT)
    }
    #[cfg(not(feature = "direct-callout-attribution"))]
    {
        i64::from(IN_AL_DX_CORE_CLOCKS)
            | (i64::from(bus.requires_step_break()) << STATUS_STEP_BREAK_BIT)
    }
}

/// Resolve the published bus for a helper instantiation, or `None` if the window is not open.
///
/// # Safety
///
/// Same contract as each helper's: `cpu.native_callout` must have been published by
/// `CallOutTable::publish::<B>` for this exact `B`, and the returned reference must not outlive
/// the native entry.
#[inline]
unsafe fn published_bus<'a, B: CpuBus>(cpu: &CpuGsw) -> Option<&'a mut B> {
    let bus_ptr = cpu.native_callout.bus;
    debug_assert!(
        !bus_ptr.is_null(),
        "a call-out slot ran without a published bus"
    );
    if bus_ptr.is_null() {
        return None;
    }
    // SAFETY: by the caller's contract.
    Some(unsafe { &mut *bus_ptr.cast::<B>() })
}

/// `0x60` PUSHAD through the interpreter's own stack path.
///
/// TWO PHASES, and the split is the whole design. Phase one PROVES the eight-dword frame movable
/// (`call_out_stack_frame_resident`) without touching anything; phase two runs `push_all_gpr`, the
/// interpreter's own body, which phase one has made incapable of walking a page table, faulting,
/// or reaching `note_code_write`. A refusal in phase one is `STATUS_ABNORMAL` with literally
/// nothing done, so the native run ends at the instruction and the interpreter executes PUSHAD
/// whole -- the same boundary a pre-slice PUSHAD barrier produced.
///
/// The `Err` arm of phase two is UNREACHABLE given phase one and is written as a restore anyway.
/// What phase one cannot exclude by construction is a `CpuBus::charge_direct_ram_memory` that
/// errors on a resident RAM page; no bus in tree can (`MachineBus` returns `Ok` unconditionally,
/// and so does the trait default). If one ever did, the restore puts (E)SP and the whole register
/// file back, so the interpreter's re-execution writes the same bytes to the same addresses and
/// the only residue is a duplicated bus charge for the sub-pushes that completed -- a timing
/// divergence on an unreachable path, disclosed rather than hidden.
///
/// # Safety
///
/// See `port_read_al_dx`.
unsafe extern "C" fn push_all_dword<B: CpuBus>(
    cpu: *mut CpuGsw,
    _prefix_raw_clocks: u64,
    _prefix_weighted_fp_clocks: u64,
) -> i64 {
    // SAFETY: by the contract above, `cpu` is the live CPU and no other reference to it is alive
    // across the call (emitted code holds only its ADDRESS, in R15).
    let cpu = unsafe { &mut *cpu };
    // SAFETY: `publish::<B>` stored a `*mut B` for the same `B` this instantiation was selected
    // for, and the CPU and the bus are disjoint.
    let Some(bus) = (unsafe { published_bus::<B>(cpu) }) else {
        return STATUS_ABNORMAL;
    };
    cpu.jit_direct.note_callout_executed();

    // The clock-prefix arguments are ignored, and that is a fact about the reachable set rather
    // than an omission. `port_read_al_dx` previews them because `CpuBus::read_io` takes a guest
    // timestamp; nothing on this helper's path does. Phase one refuses the Mode13h aperture, so
    // the only bus entry point phase two reaches is `charge_direct_ram_memory`, which takes an
    // address, a width and a kind. No device observes the time at which these stores happen, so
    // there is no timestamp to get wrong.
    if !cpu.call_out_stack_frame_resident(CALL_OUT_STACK_FRAME_DWORDS, true) {
        #[cfg(feature = "direct-callout-attribution")]
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PushAllDword,
            None,
            CallOutOutcome::Abnormal,
        );
        return STATUS_ABNORMAL;
    }
    let registers = cpu.registers.clone();
    if cpu.push_all_gpr(bus, OperandSize::Dword).is_err() {
        debug_assert!(
            false,
            "a resident PUSHAD frame faulted inside a call-out slot"
        );
        cpu.registers = registers;
        #[cfg(feature = "direct-callout-attribution")]
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PushAllDword,
            None,
            CallOutOutcome::Abnormal,
        );
        return STATUS_ABNORMAL;
    }

    // The SAME constant the interpreter's `0x60` arm charges, and the same step-break question
    // `run_straight_line` asks after every interpreted instruction. Both are shared rather than
    // restated so the exact-clocks claim cannot drift out from under this module.
    #[cfg(feature = "direct-callout-attribution")]
    {
        let step_break = bus.requires_step_break();
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PushAllDword,
            None,
            if step_break {
                CallOutOutcome::StepBreak
            } else {
                CallOutOutcome::Continued
            },
        );
        i64::from(PUSH_ALL_CORE_CLOCKS) | (i64::from(step_break) << STATUS_STEP_BREAK_BIT)
    }
    #[cfg(not(feature = "direct-callout-attribution"))]
    {
        i64::from(PUSH_ALL_CORE_CLOCKS)
            | (i64::from(bus.requires_step_break()) << STATUS_STEP_BREAK_BIT)
    }
}

/// `0x61` POPAD through the interpreter's own stack path. Same two-phase shape as
/// `push_all_dword`, with `write = false`: the frame is READ, so there is no code-watch clause to
/// satisfy and no `note_code_write` to reach -- what phase one still has to exclude is the page
/// walk (which WRITES accessed bits, and can therefore reach `note_code_write` by that route) and
/// the fault, because `pop_all_gpr` does not restore on fault at all and a partial POPAD would
/// leave eight registers half loaded.
///
/// This helper MUTATES THE GUEST REGISTER FILE -- eight registers plus ESP, where `0xEC` wrote one
/// byte of one. That is carried entirely by the emitted slot's unconditional whole-set reload; see
/// the reload derivation in the module docs.
///
/// # Safety
///
/// See `port_read_al_dx`.
unsafe extern "C" fn pop_all_dword<B: CpuBus>(
    cpu: *mut CpuGsw,
    _prefix_raw_clocks: u64,
    _prefix_weighted_fp_clocks: u64,
) -> i64 {
    // SAFETY: as `push_all_dword`.
    let cpu = unsafe { &mut *cpu };
    // SAFETY: as `push_all_dword`.
    let Some(bus) = (unsafe { published_bus::<B>(cpu) }) else {
        return STATUS_ABNORMAL;
    };
    cpu.jit_direct.note_callout_executed();

    if !cpu.call_out_stack_frame_resident(CALL_OUT_STACK_FRAME_DWORDS, false) {
        #[cfg(feature = "direct-callout-attribution")]
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PopAllDword,
            None,
            CallOutOutcome::Abnormal,
        );
        return STATUS_ABNORMAL;
    }
    let registers = cpu.registers.clone();
    if cpu.pop_all_gpr(bus, OperandSize::Dword).is_err() {
        debug_assert!(
            false,
            "a resident POPAD frame faulted inside a call-out slot"
        );
        cpu.registers = registers;
        #[cfg(feature = "direct-callout-attribution")]
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PopAllDword,
            None,
            CallOutOutcome::Abnormal,
        );
        return STATUS_ABNORMAL;
    }

    #[cfg(feature = "direct-callout-attribution")]
    {
        let step_break = bus.requires_step_break();
        cpu.jit_direct.note_callout_attribution(
            CallOutHelper::PopAllDword,
            None,
            if step_break {
                CallOutOutcome::StepBreak
            } else {
                CallOutOutcome::Continued
            },
        );
        i64::from(POP_ALL_CORE_CLOCKS) | (i64::from(step_break) << STATUS_STEP_BREAK_BIT)
    }
    #[cfg(not(feature = "direct-callout-attribution"))]
    {
        i64::from(POP_ALL_CORE_CLOCKS)
            | (i64::from(bus.requires_step_break()) << STATUS_STEP_BREAK_BIT)
    }
}

/// Drive `push_all_dword` exactly as an emitted slot does, for the helper-level tests.
#[cfg(test)]
pub(crate) fn push_all_dword_for_test<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B) -> i64 {
    cpu.native_callout = CallOutTable::publish(bus);
    // SAFETY: the table was just published for this exact `B`, and `cpu` is not otherwise
    // borrowed across the call.
    let status = unsafe { push_all_dword::<B>(cpu as *mut CpuGsw, 0, 0) };
    cpu.native_callout = CallOutTable::default();
    status
}

/// Drive `pop_all_dword` exactly as an emitted slot does, for the helper-level tests.
#[cfg(test)]
pub(crate) fn pop_all_dword_for_test<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B) -> i64 {
    cpu.native_callout = CallOutTable::publish(bus);
    // SAFETY: as `push_all_dword_for_test`.
    let status = unsafe { pop_all_dword::<B>(cpu as *mut CpuGsw, 0, 0) };
    cpu.native_callout = CallOutTable::default();
    status
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
