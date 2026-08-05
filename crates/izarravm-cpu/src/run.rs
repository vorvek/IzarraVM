// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Gate for the differential-oracle per-instruction trace prototype
/// (`IZARRAVM_DIFF_TRACE`). Separate env var from `IZARRAVM_FAULT_TRACE`
/// deliberately: that one fires on a handful of cold fault paths, this one
/// fires on every retired instruction, so it must never share a code path
/// that could accidentally widen the fault trace's cost. Cached after the
/// first check, same pattern as `ud_trace_enabled`, so the steady-state cost
/// when unset is one relaxed load per instruction, not a syscall.
pub(super) fn diff_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_DIFF_TRACE").is_some())
}

/// Whether an opcode is a port-I/O instruction (IN / OUT / INS / OUTS), for the unit simulator's
/// `touches_io` fact. IN (0xE4/0xE5 imm, 0xEC/0xED DX) and the string forms INS (0x6C/0x6D) / OUTS
/// (0x6E/0x6F) plus OUT (0xE6/0xE7 imm, 0xEE/0xEF DX). OUT/INS/OUTS are also non-continuable, so the
/// sim closes them as terminators before the I/O flag matters; IN is the Approximate-class interior
/// form whose observation this flag actually drives.
#[cfg(feature = "jit")]
fn unit_sim_touches_io(opcode: u16) -> bool {
    matches!(
        opcode,
        0xe4 | 0xe5 | 0xe6 | 0xe7 | 0xec | 0xed | 0xee | 0xef | 0x6c | 0x6d | 0x6e | 0x6f
    )
}

/// Whether an opcode is a port IN, for the unit simulator's `io_read` fact (rung P): the immediate
/// forms 0xE4/0xE5 and the DX forms 0xEC/0xED. OUT and the string I/O forms are excluded - only a
/// port READ can be the side-effect-free device poll that P models.
#[cfg(feature = "jit")]
fn unit_sim_io_read(opcode: u16) -> bool {
    matches!(opcode, 0xe4 | 0xe5 | 0xec | 0xed)
}

/// The port number an IN reads: the imm8 for 0xE4/0xE5, else the live DX (0xEC/0xED). Used only by
/// the `IZARRAVM_IO_HIST` per-port histogram.
#[cfg(feature = "jit")]
fn unit_sim_io_port(insn: &DecodedInsn, dx: u16) -> u16 {
    match insn.opcode {
        0xe4 | 0xe5 => (insn.imm & 0xff) as u16,
        _ => dx,
    }
}

/// True when `IZARRAVM_IO_HIST` requests the per-port io-read histogram (any value other than "" or
/// "0"). Off by default and read once; the histogram is a diagnostic that never touches guest state.
#[cfg(feature = "jit")]
fn io_hist_requested() -> bool {
    static REQUESTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REQUESTED.get_or_init(|| {
        std::env::var("IZARRAVM_IO_HIST")
            .ok()
            .as_deref()
            .is_some_and(|value| !matches!(value, "" | "0"))
    })
}

#[cfg(feature = "jit")]
enum DirectContinuation {
    Run(CycleOutcome),
    Prefix(CycleOutcome),
    Interpret,
}

/// What the single admission dispatcher decided for one continuation. The two non-native answers
/// are kept apart because only one of them is a DECLINE: `Declined` means the JIT was actually
/// consulted about this boundary and chose the interpreter (the `jit_direct_dispatch_declines`
/// seam counter), while `Skipped` means a gate upstream of the JIT meant it was never asked.
#[cfg(feature = "jit")]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) enum ContinuationDispatch {
    Native(CycleOutcome),
    Declined,
    Skipped,
}

#[cfg(feature = "jit")]
enum DirectBlockOutcome {
    NotRun,
    Complete(CycleOutcome),
    Prefix(CycleOutcome),
}

#[cfg(feature = "jit")]
#[derive(Clone, Copy)]
struct ContinuationBudget {
    total: u64,
    bus_at_entry: u64,
    cap: u64,
}

impl CpuGsw {
    pub fn cycle<B: CpuBus>(&mut self, bus: &mut B) -> Result<CycleOutcome, CpuError> {
        // `cycle` is the per-instruction prologue (halt-wake + hardware interrupt
        // service) followed by one instruction. The two halves are split so the
        // machine batch loop can run the prologue once per batch and then run a
        // sequence of instructions through `cycle_no_interrupt_check`:
        // `interrupt_pending()` is driven by the PIC and cannot change mid-batch
        // (devices advance only after the batch, and any guest PIC access ends the
        // batch), so a per-batch interrupt check is equivalent to the old
        // per-instruction one. Every existing caller keeps the old behavior because
        // this thin wrapper composes the two halves exactly as before.
        match self.service_pending_interrupt(bus)? {
            Some(outcome) => Ok(outcome),
            None => self.cycle_no_interrupt_check(bus),
        }
    }

    /// The interrupt prologue of `cycle`: wake from HLT if an enabled interrupt is
    /// pending, then service one pending hardware interrupt. Returns `Some(outcome)`
    /// when this call produced a complete cycle (it stayed halted or it took an
    /// interrupt) and `None` when the caller should run an instruction next. The
    /// STI one-instruction shadow is *tested* here but NOT consumed: the consume
    /// belongs to a running instruction (`cycle_no_interrupt_check`), so that when
    /// the machine runs this once per batch a `STI; HLT` idle loop still eventually
    /// takes its interrupt instead of spinning forever.
    pub fn service_pending_interrupt<B: CpuBus>(
        &mut self,
        bus: &mut B,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        // Wake from HLT only when a maskable interrupt can actually be taken. The 386
        // exits HLT on an enabled interrupt; a masked or IF-disabled request leaves it
        // halted. NMI and the other non-maskable wake sources are out of scope.
        if self.halted {
            if self.flag(FLAG_IF) && bus.interrupt_pending() {
                self.halted = false;
            } else {
                return Ok(Some(CycleOutcome {
                    core_clocks: 1,
                    halted: true,
                }));
            }
        }

        // Test the one-instruction shadow set by STI without consuming it. While the
        // shadow is active the interrupt check is skipped so the instruction after STI
        // always executes before an interrupt can be taken; the consume happens in
        // `cycle_no_interrupt_check` when that next instruction runs.
        if !self.interrupt_shadow
            && self.flag(FLAG_IF)
            && bus.interrupt_pending()
            && let Some(vector) = bus.acknowledge_interrupt()
        {
            // The interrupt frame sees the paused REP's start EIP. Once delivery begins, the
            // saved host continuation is stale; IRET restarts from guest code and refetches.
            self.rep_resume_active = false;
            self.rep_execution.resume = None;
            // No faulting instruction to name here: the fault is in delivery of
            // an asynchronous interrupt taken at an instruction boundary, so the
            // boundary itself IS the right answer and must not be rewound.
            let boundary_eip = self.registers.eip;
            if let Err(fault) = self.hardware_interrupt(bus, vector) {
                // cs_moved is false by construction: an IRQ is taken at a
                // boundary, so no instruction was mid-flight to move CS.
                self.record_fault_site(boundary_eip, false);
                return Err(match fault {
                    InternalFault::Cpu(error) => error,
                    // A fault raised while `hardware_interrupt` (which calls
                    // `deliver_exception`) was building the IRQ's own frame is a
                    // genuinely nested fault, not an IDT-limit violation on `vector`
                    // itself -- report it truthfully instead of relabeling it.
                    InternalFault::Exception {
                        vector: nested_vector,
                        ..
                    } => CpuError::NestedFaultDuringDelivery {
                        original_vector: vector,
                        nested_vector,
                    },
                });
            }
            let charged = self.scale_clocks(61);
            self.elapsed_clocks += charged;
            return Ok(Some(CycleOutcome {
                core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                halted: false,
            }));
        }

        Ok(None)
    }

    /// Fetch and execute exactly one instruction with NO halt handling and NO
    /// interrupt check. The caller (`cycle`, or the machine batch loop) is
    /// responsible for having run `service_pending_interrupt` first. This consumes
    /// the STI one-instruction shadow: a running instruction uses up the one-cycle
    /// delay, so the instruction after STI runs here and the shadow is clear by the
    /// next interrupt check.
    pub fn cycle_no_interrupt_check<B: CpuBus>(
        &mut self,
        bus: &mut B,
    ) -> Result<CycleOutcome, CpuError> {
        self.cycle_no_interrupt_check_with_budget(bus, None)
    }

    fn cycle_no_interrupt_check_with_budget<B: CpuBus>(
        &mut self,
        bus: &mut B,
        rep_budget: Option<RepBudget>,
    ) -> Result<CycleOutcome, CpuError> {
        self.interrupt_shadow = false;
        // This is always either a standalone single-step (no prior instructions in
        // "this run") or run_straight_line's FIRST instruction (total == 0 at that
        // point, by construction): both cases mean core_clocks_so_far is 0 here.
        // Continuations inside run_straight_line go through run_one_cached instead,
        // which does not reset this field; run_straight_line sets it explicitly
        // before each continuation call.
        self.core_clocks_so_far = 0;

        if self.rep_resume_active {
            return self.resume_rep_instruction(bus, rep_budget);
        }

        self.begin_instruction();
        let start_eip = self.registers.eip;
        let start_cs_register = self.registers.cs();
        let start_cs = start_cs_register.selector;
        let lin = self.linear_eip();
        let profiling = self.profile.enabled;
        let profile_start = if profiling {
            self.profile.sample_start()
        } else {
            None
        };
        let mut profile_key = None;
        let mut decoded = None;
        self.rep_execution.yielded = false;
        let result = match self.fetch_decoded(bus, lin) {
            Ok(insn) => {
                decoded = Some(insn);
                if profiling {
                    profile_key = Some((
                        insn.group,
                        cpu_profile_opcode(&insn),
                        CpuProfileOperandForm::from_insn(&insn),
                    ));
                }
                self.execute_decoded_with_rep_budget(&insn, bus, rep_budget)
            }
            Err(fault) => Err(fault),
        };
        if self.rep_execution.yielded {
            let insn = decoded.expect("only a decoded REP instruction can yield");
            let outcome = result.expect("a faulting REP chunk cannot also yield");
            return Ok(self.pause_rep_instruction(insn, start_eip, start_cs_register, outcome));
        }
        // Observe the retired instruction (this is the FIRST-instruction / standalone retire path;
        // continuations retire through the run_one_cached fast tails). `finish_instruction`
        // increments perf.instructions on Ok and on a delivered Exception; the unit simulator
        // measures fault-free hot code, so observe only the Ok retirements (a decode miss leaves
        // `decoded` None and never observes). `d` is the pre-execution decode key.
        #[cfg(feature = "jit")]
        if result.is_ok()
            && let Some(insn) = &decoded
        {
            self.unit_sim_observe(
                insn,
                lin,
                start_cs_register.default_size_32,
                start_cs_register.base,
            );
        }
        self.finish_instruction(
            bus,
            result,
            start_eip,
            start_cs,
            0,
            profile_key,
            profile_start,
        )
    }

    fn pause_rep_instruction(
        &mut self,
        insn: DecodedInsn,
        start_eip: u32,
        cs: SegmentRegister,
        outcome: CycleOutcome,
    ) -> CycleOutcome {
        debug_assert!(insn.prefixes.rep.is_some());
        debug_assert!(!outcome.halted);
        let post_eip = self.registers.eip;
        let charged = self.scale_clocks(outcome.core_clocks);
        self.elapsed_clocks += charged;
        if self.is_ring0_protected() {
            self.perf.monitor_resident_core_clocks += charged;
        }
        // Do not call set_eip here. A budget yield is not guest control flow and must retain the
        // instruction's prefetch snapshot for the no-interrupt continuation.
        self.registers.eip = start_eip;
        self.rep_execution.resume = Some(RepResume {
            insn,
            start_eip,
            post_eip,
            cs,
            precharged_core: charged,
        });
        self.rep_resume_active = true;
        self.rep_execution.yielded = false;
        CycleOutcome {
            core_clocks: charged.min(u64::from(u32::MAX)) as u32,
            halted: false,
        }
    }

    fn resume_rep_instruction<B: CpuBus>(
        &mut self,
        bus: &mut B,
        rep_budget: Option<RepBudget>,
    ) -> Result<CycleOutcome, CpuError> {
        let resume = self
            .rep_execution
            .resume
            .expect("resume path requires REP state");
        if self.registers.eip != resume.start_eip || self.registers.cs() != resume.cs {
            self.rep_resume_active = false;
            self.rep_execution.resume = None;
            return self.cycle_no_interrupt_check_with_budget(bus, rep_budget);
        }

        self.registers.eip = resume.post_eip;
        self.rep_execution.yielded = false;
        let profiling = self.profile.enabled;
        let profile_start = if profiling {
            self.profile.sample_start()
        } else {
            None
        };
        let result = self.execute_decoded_with_rep_budget(&resume.insn, bus, rep_budget);
        if self.rep_execution.yielded {
            let outcome = result.expect("a faulting REP chunk cannot also yield");
            debug_assert!(!outcome.halted);
            self.registers.eip = resume.start_eip;
            self.rep_execution.yielded = false;
            return Ok(CycleOutcome {
                core_clocks: 0,
                halted: false,
            });
        }

        self.rep_resume_active = false;
        self.rep_execution.resume = None;
        let result = result.map(|outcome| CycleOutcome {
            core_clocks: 0,
            halted: outcome.halted,
        });
        // The REP resume completes the paused instruction; observe it once here (the paused chunks
        // yielded without retiring). `.map` above preserves the Ok/Err split, so the same
        // Ok-only rule as the other retire sites holds.
        #[cfg(feature = "jit")]
        if result.is_ok() {
            let lin = resume.cs.base.wrapping_add(resume.start_eip);
            self.unit_sim_observe(&resume.insn, lin, resume.cs.default_size_32, resume.cs.base);
        }
        self.finish_instruction(
            bus,
            result,
            resume.start_eip,
            resume.cs.selector,
            resume.precharged_core,
            profiling.then_some((
                resume.insn.group,
                cpu_profile_opcode(&resume.insn),
                CpuProfileOperandForm::from_insn(&resume.insn),
            )),
            profile_start,
        )
    }

    fn execute_decoded_with_rep_budget<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        rep_budget: Option<RepBudget>,
    ) -> ExecResult<CycleOutcome> {
        if insn.prefixes.rep.is_none() || rep_budget.is_none() {
            return self.execute_decoded(insn, bus);
        }
        self.rep_execution.budget = rep_budget;
        let result = self.execute_decoded(insn, bus);
        self.rep_execution.budget = None;
        result
    }

    /// Emit one differential-oracle trace line for the instruction that just retired at
    /// `start_cs:start_eip`, in the common trace format: `CS:IP EAX EBX ECX EDX ESI EDI EBP
    /// ESP EFLAGS CS DS ES SS FS GS` (space-separated hex, no leading 0x). Gated on
    /// `diff_trace_enabled()`; callers check the gate themselves so the common case (env
    /// var unset) costs exactly one cached bool load and this function is never called.
    /// Reads `self.registers` fresh, i.e. AFTER the instruction retired, matching a
    /// reference emulator's post-step register dump.
    ///
    /// Prototype-only note: writes go through a process-wide buffered, mutex-guarded
    /// stderr handle (`diff_trace_writer`) rather than a bare `eprintln!`. An unbuffered
    /// `eprintln!` here cost one syscall per retired instruction, which measured at only
    /// ~3300 lines/sec against a redirected file -- far too slow to trace even a single
    /// BIOS POST, let alone anything DOS4GW-shaped. This is a diagnostic tool, not a hot
    /// path, so a mutex per line is an acceptable cost; the buffering is what actually
    /// matters for throughput.
    #[cold]
    fn emit_diff_trace_line(&self, start_cs: u16, start_eip: u32) {
        let r = &self.registers;
        let mut w = diff_trace_writer()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(
            w,
            "{start_cs:04x}:{start_eip:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} \
             {:08x} {:08x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x}",
            r.eax(),
            r.ebx(),
            r.ecx(),
            r.edx(),
            r.esi(),
            r.edi(),
            r.ebp(),
            r.esp(),
            r.eflags,
            r.cs().selector,
            r.segment(SegmentIndex::Ds).selector,
            r.segment(SegmentIndex::Es).selector,
            r.segment(SegmentIndex::Ss).selector,
            r.segment(SegmentIndex::Fs).selector,
            r.segment(SegmentIndex::Gs).selector,
        );
    }

    /// The shared rewind / deliver / scale tail of a single instruction's execution. It owns ONLY
    /// what happens after `result` is produced: on a delivered exception it rewinds eip (and CS, if a
    /// far transfer moved it) to the faulting instruction and delivers the fault through
    /// `deliver_exception` exactly as the per-instruction path always did, charging the architectural
    /// 59 core clocks for the dispatch; on a CPU-level error it propagates; otherwise it scales the
    /// retired clocks (the single per-mode timing dial) and accumulates them. Callers charge their own
    /// fetch BEFORE calling this, so it never touches fetch clocks and never double-charges.
    /// `start_eip` / `start_cs` are captured before the fetch so the rewind lands on the instruction's
    /// first byte.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_instruction<B: CpuBus>(
        &mut self,
        bus: &mut B,
        result: ExecResult<CycleOutcome>,
        start_eip: u32,
        start_cs: u16,
        precharged_core: u64,
        profile_key: Option<(DecodeGroup, u16, CpuProfileOperandForm)>,
        profile_start: Option<std::time::Instant>,
    ) -> Result<CycleOutcome, CpuError> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(InternalFault::Exception { vector, error_code }) => {
                // Captured BEFORE the rewind, because the rewind destroys the
                // evidence. `load_segment_real` installs a fabricated real-mode
                // descriptor (base = selector << 4), which is wrong in protected
                // mode, and it makes the selectors match, so comparing them
                // afterwards would report "CS did not move" while handing the
                // byte dump an invented base. That is the exact plausible-but-
                // wrong-hex failure this whole change exists to remove.
                let cs_was_moved = self.registers.cs().selector != start_cs;
                self.set_eip(start_eip);
                if cs_was_moved {
                    self.load_segment_real(SegmentIndex::Cs, start_cs);
                }
                // The rewind above already put CS:EIP back on the faulting
                // instruction, and deliver_exception loads CS and sets EIP last,
                // after the IDT read, the stack switch and every push, so every
                // error path out of it still has the rewound value. That makes
                // start_eip/start_cs the site directly, with no snapshot needed.
                // Caveat for whoever reads the report: cpl and SS:ESP have moved
                // by then, so the surrounding ring0/stack context is the inner
                // stack mid-delivery, not the faulting code's.
                if let Err(fault) = self.deliver_exception(bus, vector, error_code, false) {
                    self.record_fault_site(start_eip, cs_was_moved);
                    return Err(match fault {
                        InternalFault::Cpu(error) => error,
                        // As above: a fault raised while building `vector`'s own frame
                        // (e.g. the ring-0 stack access that was the actual dossier bug)
                        // is a nested fault, not an IDT-limit violation on `vector`.
                        InternalFault::Exception {
                            vector: nested_vector,
                            ..
                        } => CpuError::NestedFaultDuringDelivery {
                            original_vector: vector,
                            nested_vector,
                        },
                    });
                }
                CycleOutcome {
                    core_clocks: 59,
                    halted: false,
                }
            }
            Err(InternalFault::Cpu(error)) => {
                // Note start_eip, NOT self.registers.eip: EIP has already
                // advanced past the instruction by fetch time, so the live value
                // names the next instruction. The architectural EIP is left
                // alone on purpose. Rewinding it the way the Exception arm does
                // would be wrong here, because a fatal error does not stop the
                // machine (the run loop returns Ok(StopReason::CpuError) with
                // everything intact) and the GUI resumes it, so a rewind would
                // pin the guest on the faulting instruction instead of stepping
                // past it.
                let cs_moved = self.registers.cs().selector != start_cs;
                self.record_fault_site(start_eip, cs_moved);
                return Err(error);
            }
        };

        let charged = self.scale_clocks(outcome.core_clocks);
        self.elapsed_clocks += charged;
        self.perf.instructions += 1;
        // V86 trap tax residency: see PerfCounters::monitor_resident_core_clocks.
        if self.is_ring0_protected() {
            self.perf.monitor_resident_core_clocks += charged;
        }
        if let Some((group, opcode, form)) = profile_key {
            // The hot-address histogram wants the linear address of the instruction START.
            // A far transfer already moved the CS base by now, mis-attributing that one
            // sample; histogram noise, accepted (this is a host-side loop finder, not
            // architectural state).
            let lin = self.registers.cs().base.wrapping_add(start_eip);
            self.profile.record(
                group,
                opcode,
                form,
                precharged_core.saturating_add(charged),
                profile_start,
                lin,
            );
        }
        if diff_trace_enabled() {
            self.emit_diff_trace_line(start_cs, start_eip);
        }
        Ok(CycleOutcome {
            core_clocks: charged.min(u64::from(u32::MAX)) as u32,
            halted: outcome.halted,
        })
    }

    /// Run a straight-line run of instructions in one cross-crate call instead of bouncing to the
    /// machine batch loop once per instruction. The first instruction always goes through the normal
    /// single-instruction path (`cycle_no_interrupt_check`), which handles a decode miss, a fault, and
    /// halt. Each continuation runs ONLY when the next instruction is already in the decode cache,
    /// passes the `block_continuable` gate (the straight-line groups plus the near RET / near
    /// indirect CALL/JMP transfers), and stays inside the current 4 KB page; any miss, gated
    /// opcode, or page cross ends the run, and that terminator then runs through the normal path on
    /// the next machine-loop entry. `cap` bounds the run in scaled core clocks.
    ///
    /// This is the lean recompiler path: it needs no block cache. Self-modifying code is handled for
    /// free because every continuation re-peeks `decode_cache.get(lin)`. A guest write that modifies a
    /// later instruction bumps the decode-cache generation (via `note_code_write`), so the next `get`
    /// misses and the run ends; the modified instruction re-decodes through the normal path. The
    /// per-instruction `scale_clocks` + `charge_cached_fetch` calls are kept, so cyc/iter and the bus
    /// clock metric stay bit-identical to the per-instruction loop.
    ///
    /// Interrupt semantics match the per-batch model exactly: every instruction (the first AND each
    /// continuation) has its "a maskable interrupt just became serviceable" transition checked
    /// uniformly, so the run ends at precisely the boundary the old per-instruction loop would have
    /// stopped at (the post-STI instruction consuming the shadow, or POPF/IRET enabling IF). The
    /// machine's own per-batch transition check then services the interrupt at the next batch entry.
    pub fn run_budgeted<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> Result<BudgetedRunOutcome, CpuError> {
        let result = self.run_budgeted_inner(bus, cap);
        // Close the unit simulator's batch on EVERY return path, including the `?` error
        // propagations inside the loop, so an open sim entry never leaks across batches.
        #[cfg(feature = "jit")]
        self.unit_sim_batch_end();
        // Fold the Direct block cache's stats into `perf` once per batch rather than once per
        // dispatcher entry. The twelve fields are accumulate-only and nothing reads them between
        // an entry and the end of a batch (the only readers are the end-of-run reporters in
        // `izarravm`), so the totals are unchanged while the work drops from 88 million calls to
        // 27.6 million. Sitting in the wrapper rather than in the body also covers the six `?`
        // propagations inside the loop, which the two calls this replaces did not.
        #[cfg(feature = "jit")]
        self.flush_direct_cache_stats();
        result
    }

    /// The `run_budgeted` body; the public wrapper owns the per-return batch bookkeeping.
    fn run_budgeted_inner<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> Result<BudgetedRunOutcome, CpuError> {
        let mut total = 0u64;
        let mut first = true;
        // Run-scoped latch, cleared on every entry so it can never carry across a batch. It used
        // to be a local here; it moved onto `direct_runtime` so the whole admission decision is one
        // call, and this clear is what keeps that move behaviour-preserving.
        #[cfg(feature = "jit")]
        {
            self.direct_runtime.skip_direct_once = false;
        }
        #[cfg(feature = "jit")]
        let native_continuations_active = {
            debug_assert_eq!(
                self.direct_runtime.admission_active,
                self.jit_direct.execution_enabled()
            );
            self.direct_runtime.admission_active
        };
        // Guest-clock budget honesty: `cap` is a guest-clock budget (the machine
        // derives it from PIT-edge instants), but `total` counts core clocks
        // only. Track the batch's scaled-bus growth across this run so a
        // bus-heavy run (a framebuffer blit is several bus clocks per core
        // clock) ends at the budget instead of overshooting the next timer
        // edge by the bus:core ratio. Buses without this accounting return 0,
        // which degrades to the core-only comparison.
        let bus_at_entry = bus.in_batch_scaled_bus_clocks();
        // Screen inputs for the per-instruction cap test, read ONCE per run: the batch's raw
        // clock count at entry and the batch-constant growth bound `F`. Both are batch-scoped
        // (a run never spans a batch boundary, and a mode change is staged in `pending_mode` and
        // applied after the batch), so hoisting them here is not a snapshot of stale state. See
        // the cap test below for the derivation.
        let raw_at_entry = bus.in_batch_raw_bus_clocks();
        let cap_screen_scale = bus.in_batch_scaled_bus_clocks_screen_scale();
        let rep_budget = RepBudget { bus_at_entry, cap };
        self.perf.straight_line_runs += 1;
        // Seam counters, folded once per run (never per-instruction on the hot path).
        let mut seam_probes: u64 = 0;
        // Only the jit-gated Direct dispatch arm below increments this; a no-jit build folds it
        // unconditionally (always 0) so the fold site does not need its own cfg split.
        #[cfg_attr(not(feature = "jit"), allow(unused_mut))]
        let mut seam_declines: u64 = 0;
        loop {
            let can_take_before = self.can_take_interrupt();
            let outcome = if first {
                first = false;
                self.cycle_no_interrupt_check_with_budget(bus, Some(rep_budget))?
            } else {
                let lin = self.linear_eip();
                let cs = self.registers.cs();
                seam_probes += 1;
                // ONE decode-line fetch for the whole iteration. The probe, the admission hotness
                // bump, the block key's physical start and the warm-fetch charge all wanted the
                // same line and each used to index the table and repeat its tag check; they now
                // read this view.
                //
                // STALENESS: the view is a snapshot taken BEFORE dispatch, and the interpreter
                // below deliberately executes what it read. This is the same argument the decode
                // cache's insn copy rests on (`.bench/results/decodecache-20260731/RESULTS.md`):
                // a borrow would be unsound because lines get refilled, a copy simply commits to
                // the instruction the probe saw. Nothing on the path between here and the consumers
                // can invalidate this line anyway — admission only READS the decode cache (its
                // compile walk uses `get`, never `put`), and every arm that returns to the
                // interpreter does so before any guest instruction executes.
                let view = match self.decode_cache.get_view(lin, cs.default_size_32) {
                    Some(view) => {
                        if !view.insn.continuable {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_not_continuable += 1;
                            break;
                        }
                        if (lin & 0xfff) + u32::from(view.insn.len) > 0x1000 {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_page_cross += 1;
                            break;
                        }
                        if !Self::fetch_within_limit(self.registers.eip, view.insn.len, cs.limit) {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_not_continuable += 1;
                            break;
                        }
                        view
                    }
                    None => {
                        self.perf.brk_decode_or_branch += 1;
                        self.perf.brk_cont_decode_miss += 1;
                        break;
                    }
                };
                // JIT admission: a compiled Direct block covering this line runs natively instead
                // of the interpreted continuation, occupying one loop iteration; the loop's own
                // break checks below then fire at exactly the boundary the block stopped at.
                // One call answers the whole question (see `dispatch_continuation`).
                #[cfg(feature = "jit")]
                let direct_outcome = match self.dispatch_continuation(
                    bus,
                    native_continuations_active,
                    &view,
                    lin,
                    cs.default_size_32,
                    ContinuationBudget {
                        total,
                        bus_at_entry,
                        cap,
                    },
                )? {
                    ContinuationDispatch::Native(outcome) => Some(outcome),
                    ContinuationDispatch::Declined => {
                        seam_declines += 1;
                        None
                    }
                    ContinuationDispatch::Skipped => None,
                };
                #[cfg(not(feature = "jit"))]
                let direct_outcome: Option<CycleOutcome> = None;
                match direct_outcome {
                    Some(outcome) => outcome,
                    None => {
                        // A continuation skips cycle_no_interrupt_check (which resets this
                        // field to 0 for a fresh first instruction), so set it explicitly:
                        // total is exactly the prior instructions' charge in this run, not
                        // including the continuation about to execute.
                        self.core_clocks_so_far = total;
                        if view.insn.prefixes.rep.is_some() {
                            self.run_one_cached_budgeted(bus, &view, lin, rep_budget)?
                        } else {
                            self.run_one_cached(bus, &view, lin)?
                        }
                    }
                }
            };
            total += u64::from(outcome.core_clocks);
            // A budgeted REP exposes its restart EIP and returns after every bounded chunk so the
            // machine can service an event or interrupt before any further iteration.
            if self.rep_resume_active {
                break;
            }
            // The post-instruction break checks run in the SAME ORDER the old per-instruction machine
            // loop used (halted -> step-break -> interrupt-transition -> cap), so the run ends at
            // exactly the boundary that loop would have stopped at.
            if outcome.halted {
                self.perf.brk_halt += 1;
                // This early return is a second lexical exit from the loop (the other is the
                // fall-through after `break`, folded below the loop). Fold here too so a run that
                // ends in HLT does not lose its seam counts.
                self.perf.decode_probes += seam_probes;
                self.perf.jit_direct_dispatch_declines += seam_declines;
                return Ok(BudgetedRunOutcome {
                    consumed_core_clocks: total.min(u64::from(u32::MAX)) as u32,
                    halted: true,
                });
            }
            // A port access touched time-dependent device state, or an HLE software interrupt is
            // pending (e.g. an INT n, or an x87 #MF routed to vector 0x10). End the run so the machine
            // services it now, at the old per-instruction boundary. Checked after the FIRST
            // instruction too: a port OUT/IN as the run's first instruction must end the run after
            // that one instruction, exactly like the old loop.
            if bus.requires_step_break() {
                self.perf.brk_step += 1;
                break;
            }
            // End the run the instant an instruction makes a maskable interrupt serviceable (the
            // post-STI instruction consuming the shadow, or POPF/IRET enabling IF), so the machine
            // batch loop services it at the next batch entry, at exactly the old per-instruction
            // boundary. Checked after the FIRST instruction too: an IF-on-then-off-within-a-run
            // sequence would otherwise lose the interrupt.
            if !can_take_before && self.can_take_interrupt() {
                self.perf.brk_interrupt += 1;
                break;
            }
            // Was `total + (bus.in_batch_scaled_bus_clocks() - bus_at_entry) >= cap`, which put a
            // 64-bit divide in this loop for a test that fires 22,903 times out of 553.6M in a
            // Quake/586 run (2.99% of wall, measured with an inline barrier on the accessor).
            //
            // Exactly equivalent, not an approximation. `total >= cap` makes the original true on
            // its own because the bus term is non-negative (the scaled figure is monotone in the
            // batch's raw clocks). Otherwise `cap - total > 0`, and
            // `total + (S - bus_at_entry) >= cap` iff `S >= bus_at_entry + (cap - total)`, which
            // `in_batch_scaled_bus_clocks_at_least` answers with two multiplies.
            //
            // An effectively-unbounded `cap` (uncapped runs pass one) makes `bus_at_entry +
            // (cap - total)` exceed u64. That is not an edge case to assert away: the target is
            // then unreachable by any attainable scaled figure, so the original comparison is
            // false and the run must NOT break. `checked_add` returning None IS that answer.
            //
            // The exact question still costs two u128 multiplies plus three loads off the bus,
            // and it answers "no" for all but a handful of the instructions that ask it (see
            // `cap_screen_matches_the_exact_test_at_the_boundary`, which pins the boundary the
            // screen below must not move). So screen it first with one 64-bit compare.
            //
            // Derivation. Let `S(raw)` be the bus's scaled figure, `raw_e`/`raw` the batch's raw
            // clocks at run entry and now, and `F = in_batch_scaled_bus_clocks_screen_scale()`,
            // a per-batch constant with `S(raw) - S(raw_e) <= (raw - raw_e) * F` (the bus owns
            // that bound; for `MachineBus` it is `ceil(num/den)` over the batch-start snapshot,
            // and a mode change never lands mid-batch, so it is constant here). Then
            //
            //     total + (S(raw) - S(raw_e)) >= cap   =>   total + (raw - raw_e) * F >= cap
            //
            // by substituting the upper bound on the left. Contrapositive: when
            // `total + (raw - raw_e) * F < cap` the exact test is CERTAINLY false, so skipping it
            // cannot move a run boundary or a `brk_cap` count. The screen only ever admits extra
            // work, never removes a break.
            //
            // Rounding and overflow both resolve toward "screen passes", i.e. toward asking the
            // exact question: `F` is a CEILING (rounding the bound up keeps it an upper bound),
            // and if either the product or the sum overflows u64 the screen is treated as passed
            // rather than wrapped. `F == 0` means the bus offers no bound at all — then there is
            // no screen and every ask goes to the exact test, which is the pre-screen behaviour.
            let hit_cap = if total >= cap {
                true
            } else if cap_screen_scale != 0
                && bus
                    .in_batch_raw_bus_clocks()
                    .wrapping_sub(raw_at_entry)
                    .checked_mul(cap_screen_scale)
                    .and_then(|scaled_upper| scaled_upper.checked_add(total))
                    .is_some_and(|bound| bound < cap)
            {
                false
            } else {
                match bus_at_entry.checked_add(cap - total) {
                    None => false,
                    Some(target) => bus.in_batch_scaled_bus_clocks_at_least(target),
                }
            };
            if hit_cap {
                self.perf.brk_cap += 1;
                break;
            }
        }
        // Fold once per run, not per instruction. A propagated hard `CpuError` (a `?` inside the
        // loop above) skips this fold, which is acceptable: a hard error aborts the entire run
        // and no gate run produces one.
        self.perf.decode_probes += seam_probes;
        self.perf.jit_direct_dispatch_declines += seam_declines;
        Ok(BudgetedRunOutcome {
            consumed_core_clocks: total.min(u64::from(u32::MAX)) as u32,
            halted: false,
        })
    }

    /// Compatibility wrapper for callers that still consume a single-cycle outcome.
    pub fn run_straight_line<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> Result<CycleOutcome, CpuError> {
        let outcome = self.run_budgeted(bus, cap)?;
        Ok(CycleOutcome {
            core_clocks: outcome.consumed_core_clocks,
            halted: outcome.halted,
        })
    }

    /// Enable or disable the trace-driven unit simulator (feature `jit`, diagnostic). Enabling
    /// installs a fresh `UnitSim`; disabling drops any accumulated state. The sim only observes
    /// retired interpreter instructions and never influences execution, so toggling it leaves
    /// architectural state unchanged (it is excluded from `CpuGsw` equality).
    #[cfg(feature = "jit")]
    pub fn set_unit_sim_enabled(&mut self, on: bool) {
        // The C-pre-2.5 measurement set is {L0, L2, L3, L4}: the L0 anchor, call/ret linking at
        // L2, strict re-stamp at L3, and the full mechanism set at L4. The complete ladder stays
        // available via `SimLadder::new()` for tests.
        self.unit_sim.0 = on.then(|| {
            let mut ladder = jit::unit_sim::SimLadder::with_rungs(&[0, 2, 3, 4]);
            if io_hist_requested() {
                ladder.enable_io_hist();
            }
            Box::new(ladder)
        });
    }

    /// Take the unit-simulator ladder's per-rung reports, disabling the sim in the process. `None`
    /// when the sim was not enabled. Each element is `(cfg_label, headline, histogram)` for one
    /// ladder rung (the measurement set `L0, L4, L5, L6`), where the histogram entries are
    /// `(member_count, entry_physical_page)`;
    /// see `SimReport` and `jit::unit_sim` for the counter meanings. Consumed by the measurement
    /// tests and Track C tooling.
    #[cfg(feature = "jit")]
    #[allow(clippy::type_complexity)] // Signature fixed by the Track C task 3 reporting contract.
    pub fn take_unit_sim_report(
        &mut self,
    ) -> Option<Vec<(&'static str, SimReport, Vec<(usize, u32)>)>> {
        let ladder = self.unit_sim.0.take()?;
        Some(ladder.reports())
    }

    /// Enable or disable the SMC trace (diagnostic, off by default). Enabling installs a fresh
    /// recorder; disabling drops what it collected. The trace only observes the invalidation
    /// choke and never influences execution, so toggling it leaves architectural state and every
    /// perf counter unchanged -- the campaign protocol pins that with a trace-off probe.
    pub fn set_smc_trace_enabled(&mut self, on: bool) {
        self.smc_trace.0 = on.then(|| Box::new(smc_trace::SmcTrace::default()));
    }

    /// Take the SMC trace's report lines, disabling the trace in the process. `None` when the
    /// trace was never enabled. See `smc_trace` for the line format.
    pub fn take_smc_trace_report(&mut self) -> Option<Vec<String>> {
        let trace = self.smc_trace.0.take()?;
        Some(trace.report_lines())
    }

    /// The per-port io-read histogram (behind `IZARRAVM_IO_HIST=1`), sorted by count descending.
    /// `None` when the sim or the histogram was not enabled. Borrows (does not take) the sim, so it
    /// must be read BEFORE `take_unit_sim_report`. Consumed by the headless reporter.
    #[cfg(feature = "jit")]
    pub fn unit_sim_io_hist(&self) -> Option<Vec<(u16, u64)>> {
        self.unit_sim.0.as_ref()?.io_hist_sorted()
    }

    /// Feed one retired interpreter instruction into the optional unit simulator. A no-op (one
    /// `Option` test) when the sim is disabled, the production default, so the hot retire paths pay
    /// almost nothing. Called exactly once per retired instruction at every interpreter retire site
    /// (the `cycle_no_interrupt_check` first-instruction path, both `run_one_cached` fast tails, the
    /// profiling `finish_instruction` calls, and the REP resume path) so the observed count equals
    /// the `perf.instructions` delta. `d` is the pre-execution decode key (`CS.default_size_32`)
    /// and `cs_base` the pre-execution CS base, both captured by the caller before the instruction
    /// runs so the decode-line lookup and the branch-target arithmetic match the instruction that
    /// retired (a near branch never changes either, but capturing before execution is
    /// unconditionally correct).
    /// Split guard/body on purpose. The body below is far too large for the inliner to take at
    /// the call site, so a plain `#[inline]` left a real call on the interpreted-retire path —
    /// a RIP profile of Quake/586 still showed `unit_sim_observe` as its own 0.645% symbol with
    /// the sim disabled, which is ~507M calls that do nothing but test one `Option` and return.
    /// `#[inline(always)]` here folds that test into the caller and keeps the body out of line.
    #[cfg(feature = "jit")]
    #[inline(always)]
    fn unit_sim_observe(&mut self, insn: &DecodedInsn, lin: u32, d: bool, cs_base: u32) {
        if self.unit_sim.0.is_none() {
            return;
        }
        self.unit_sim_observe_enabled(insn, lin, d, cs_base);
    }

    #[cfg(feature = "jit")]
    #[inline(never)]
    fn unit_sim_observe_enabled(&mut self, insn: &DecodedInsn, lin: u32, d: bool, cs_base: u32) {
        // The retired instruction's decode line is live (we just fetched or cached it), so
        // `line_phys_start` is Some and carries the true physical page even under CR0.PG=0. Fall
        // back to the linear page only if the line went unexpectedly cold (an instruction that
        // patched its own decode line via SMC); documented and rare, never on the tested hot code.
        let physical_page = self
            .decode_cache
            .line_phys_start(lin, d)
            .map_or(lin >> 12, |phys| phys >> 12);
        // `writes_memory` / `io_read` are derived HERE, after the sim-disabled early return, so the
        // production (sim-off) path never runs the classifier; only rung P reads either fact.
        let io_read = unit_sim_io_read(insn.opcode);
        let observed = jit::unit_sim::ObservedInsn {
            linear: lin,
            len: insn.len,
            physical_page,
            mode_key: self.jit_mode_key(),
            transfer: jit::block::observed_transfer(insn, lin, cs_base),
            is_terminator: !insn.continuable
                || jit::block::changes_interrupt_visibility(insn)
                || jit::block::changes_native_memory_context(insn),
            touches_io: unit_sim_touches_io(insn.opcode),
            writes_memory: jit::block::writes_memory(insn),
            io_read,
        };
        // The per-port io histogram is counted once per retirement (on the ladder, not per rung); the
        // DX-form port is a runtime register value read here, so resolve it before the sim borrow.
        let io_port = (io_read && io_hist_requested())
            .then(|| unit_sim_io_port(insn, self.registers.edx() as u16));
        if let Some(sim) = self.unit_sim.0.as_mut() {
            if let Some(port) = io_port {
                sim.record_io_read(port);
            }
            sim.observe(observed);
        }
    }

    /// Close the current unit-simulator batch at a `run_budgeted` return, if the sim is enabled.
    /// Mirrors the real backend where each budget yield is a fresh dispatcher round trip, so any
    /// open entry ends without charging an exit counter.
    #[cfg(feature = "jit")]
    #[inline]
    fn unit_sim_batch_end(&mut self) {
        if let Some(sim) = self.unit_sim.0.as_mut() {
            sim.note_batch_end();
        }
    }

    /// Enable or disable hotness-driven JIT admission (feature `jit`). Unsupported hosts always
    /// keep it disabled. Independent of the forced-address override.
    /// Lives on the direct block cache, a transparent accelerator excluded from CPU equality, so
    /// setting it never makes an otherwise-identical CPU compare unequal.
    #[cfg(feature = "jit")]
    pub fn set_jit_auto_admit(&mut self, on: bool) {
        let was_enabled = self.direct_runtime.admission_active;
        self.jit_direct.set_auto_admit(on && jit::host_supported());
        self.finish_direct_execution_transition(was_enabled);
    }

    /// Enable or disable the native execution path. Unsupported hosts cannot be enabled.
    #[cfg(feature = "jit")]
    pub fn set_native_backend_enabled(&mut self, on: bool) {
        let was_enabled = self.direct_runtime.admission_active;
        self.jit_direct.set_backend_enabled(on);
        self.finish_direct_execution_transition(was_enabled);
    }

    #[cfg(feature = "jit")]
    fn finish_direct_execution_transition(&mut self, was_enabled: bool) {
        let enabled = self.jit_direct.execution_enabled();
        self.direct_runtime.admission_active = enabled;
        debug_assert_eq!(
            self.direct_runtime.admission_active,
            self.jit_direct.execution_enabled()
        );
        // `admission_active` is one of the inputs to `fast_map_population_enabled()`
        // (memory.rs); refresh the interpreter serve gate's cached mirror unconditionally, not
        // only on a real transition below, since a caller can reach this after changing another
        // of that predicate's inputs.
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.refresh_fast_map_serve_gate();
        if was_enabled == enabled {
            return;
        }
        self.jit_direct.fast_map_audit.wipes_admission += 1;
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.record_fast_map_wipe_extent();
        self.jit_direct.invalidate_translation();
    }

    /// G1: shared demotion tail of both admission gates. Parks the key Dormant, stamps its entry
    /// chunk for cool-down recovery, and counts the demotion.
    #[cfg(feature = "jit")]
    fn smc_heat_demote(&mut self, key: jit::direct::BlockKey, epoch: u32) {
        // Split borrow on the jit state: the cache mutates its entry states while stamping
        // the shared map.
        self.sync_smc_heat();
        let jit = &mut *self.jit_direct;
        jit.direct.demote_smc_hot(&mut jit.smc_heat, key, epoch);
        self.perf.smc_heat_demotions += 1;
    }

    /// The WHOLE admission decision for one continuation, in one call. The run loop used to spell
    /// the gate chain out inline and own the `skip_direct_once` latch as a local; both live behind
    /// this boundary now, so the loop asks one question and gets one of three answers.
    ///
    /// The decision tree is EXACTLY the one the inline form ran, gate for gate and in the same
    /// order. Two properties of that order are load-bearing rather than incidental, and neither is
    /// safe to "tidy":
    ///
    /// - The latch is consumed by a SHORT-CIRCUITED `||`. With the backend disabled the take never
    ///   runs, so a pending skip SURVIVES a disabled stretch instead of being spent on a boundary
    ///   the JIT was never going to take.
    /// - The approximate-timing test is asked here AND again as `try_direct_continuation`'s second
    ///   gate. The duplicate is not redundant at the seam: refusing here is a `Skipped`, while
    ///   refusing inside would be an `Interpret` and would count a decline. The counter gate on
    ///   `jit_direct_dispatch_declines` is what pins this.
    #[cfg(feature = "jit")]
    fn dispatch_continuation<B: CpuBus>(
        &mut self,
        bus: &mut B,
        native_continuations_active: bool,
        view: &DecodeLineView,
        lin: u32,
        d: bool,
        budget: ContinuationBudget,
    ) -> Result<ContinuationDispatch, CpuError> {
        // Hoisted by the caller and passed in: it is run-invariant, and re-reading it per
        // continuation is exactly the per-iteration cost this task is removing.
        if !native_continuations_active {
            return Ok(ContinuationDispatch::Skipped);
        }
        if !self.jit_direct.backend_enabled()
            || std::mem::take(&mut self.direct_runtime.skip_direct_once)
        {
            return Ok(ContinuationDispatch::Skipped);
        }
        if !self.mode().uses_approximate_timing() {
            return Ok(ContinuationDispatch::Skipped);
        }
        match self.try_direct_continuation(bus, view, lin, d, budget)? {
            DirectContinuation::Run(outcome) => Ok(ContinuationDispatch::Native(outcome)),
            DirectContinuation::Prefix(outcome) => {
                self.direct_runtime.skip_direct_once = true;
                Ok(ContinuationDispatch::Native(outcome))
            }
            DirectContinuation::Interpret => Ok(ContinuationDispatch::Declined),
        }
    }

    #[cfg(feature = "jit")]
    fn try_direct_continuation<B: CpuBus>(
        &mut self,
        bus: &mut B,
        view: &DecodeLineView,
        lin: u32,
        d: bool,
        budget: ContinuationBudget,
    ) -> Result<DirectContinuation, CpuError> {
        // A 16-bit code segment can NEVER produce a block: `key_for` refuses on `!d`, its very
        // first test alongside `host_supported`. So on base this function already returned
        // Interpret for every such boundary, but only after a decode-cache line lookup, a hotness
        // mutation and the probe itself. Real mode, V86 and 16-bit protected mode are the whole
        // population, and it is the whole population of a real-mode DOS guest.
        //
        // Placed BEFORE `direct_hot` so the bookkeeping goes too. That is observationally
        // equivalent, and the reason is worth stating because it is the entire correctness case:
        // `direct_hot` only ever increments a line whose `d` already matches, so a 16-bit boundary
        // can only heat a line with `d == false`; and `DecodeCache::put` REPLACES the whole
        // `DecodeLine` with `jit_direct_hotness: 0` rather than merging, so the moment that linear
        // address is executed as 32-bit code the line is re-inserted and the counter is zeroed.
        // The heating removed here is therefore write-only state that is always destroyed before
        // any 32-bit consumer can read it. The one in-place invalidator that preserves the counter,
        // `narrow_invalidate`, sets `generation = 0`, and the live generation is never 0, so such a
        // line can only come back through `put`.
        //
        // IZARRAVM_JIT16 (default 1 since the 486 measurement) gates this; see
        // `jit::direct::sixteen_bit_admission_level`. `key_for_phys` still refuses `!d` on every
        // persona but 586, and `jit_mode_key` bit 0 is CS.D, so a 16-bit block and a 32-bit block
        // at one linear address can never collide on a key.
        if !d && self.jit_direct.sixteen_bit_level == 0 {
            return Ok(DirectContinuation::Interpret);
        }
        if !self.mode().uses_approximate_timing() {
            return Ok(DirectContinuation::Interpret);
        }
        if !self.jit_direct.auto_admit() {
            return Ok(DirectContinuation::Interpret);
        }
        if !self
            .decode_cache
            .direct_hot_at(view.slot, self.jit_direct.admission_heat())
        {
            return Ok(DirectContinuation::Interpret);
        }
        let Some(key) = jit::direct::key_for_phys(self, lin, d, view.phys_start) else {
            return Ok(DirectContinuation::Interpret);
        };
        let probe = self.jit_direct.probe(key);
        let block = match probe {
            jit::direct::BlockProbe::Interpret => return Ok(DirectContinuation::Interpret),
            jit::direct::BlockProbe::Rejected => {
                // G1 recovery: a heat-demoted Dormant whose entry-chunk stamp has aged out lifts
                // back to Seen here, so the next encounter re-admits through the normal path.
                // Dormants without a heat stamp (Retry, G4 cover failure) stay parked. On the cold
                // Rejected path only, so Ready hits never pay the lookup.
                let heat_epoch = self.smc_heat_epoch();
                self.sync_smc_heat();
                let jit = &mut *self.jit_direct;
                jit.direct
                    .lift_cold_smc_dormant(&mut jit.smc_heat, key, heat_epoch);
                return Ok(DirectContinuation::Interpret);
            }
            jit::direct::BlockProbe::Ready(id) => self
                .jit_direct
                .block(id)
                .expect("ready direct block must remain live"),
            jit::direct::BlockProbe::Compile => {
                // G1 pre-compile gate (cheap, entry chunk only): if the block's first 16-byte
                // chunk is churning this heat epoch, park it Dormant and interpret without paying a
                // compile. Dormant (not Rejected) because Rejected would acquire watch ranges and
                // keep the demoted page alive; existing valid blocks keep running and links only
                // form to installed blocks, so a demoted region starves naturally.
                let heat_epoch = self.smc_heat_epoch();
                self.sync_smc_heat();
                if self.jit_direct.smc_heat.chunk_hot(key.physical, heat_epoch) {
                    self.smc_heat_demote(key, heat_epoch);
                    return Ok(DirectContinuation::Interpret);
                }
                let compile_start = std::time::Instant::now();
                let outcome = jit::direct::compile(self, lin, d);
                self.perf.jit_direct_compile_attempts += 1;
                self.perf.jit_direct_compile_ns +=
                    compile_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
                let compilation = match outcome {
                    jit::direct::CompileOutcome::Compiled(compilation) => compilation,
                    jit::direct::CompileOutcome::StructuralReject(span) => {
                        self.jit_direct.reject(span);
                        return Ok(DirectContinuation::Interpret);
                    }
                    jit::direct::CompileOutcome::Retry => {
                        self.jit_direct
                            .dormant(key, jit::direct::DormantReason::CompileRetry);
                        return Ok(DirectContinuation::Interpret);
                    }
                };
                // G4 guarantee (dev_docs/specs/2026-07-15-smc-hardening-design.md): a block only
                // installs when a real RAM direct page covers its whole physical span. The kind
                // MUST stay InstructionPrefetch: the production bus yields a direct page under that
                // kind ONLY for true RAM, so video/MMIO windows (the mode-13 window answers Data
                // kinds only), ROM, and A20-gated aliases all return None here and can never host
                // compiled code. Switching this to a Data kind would let the VGA window pass and is
                // pinned against by the G4 CPU test.
                let code_page =
                    bus.direct_page(key.physical, BusAccessKind::InstructionPrefetch)?;
                let code_page_covers_block = code_page.is_some_and(|page| {
                    page.physical_page == key.physical & !0x0fff
                        && (key.physical & 0x0fff)
                            .checked_add(u32::from(compilation.span.guest_len))
                            .is_some_and(|end| end as usize <= page.len)
                });
                if !code_page_covers_block {
                    self.jit_direct
                        .dormant(key, jit::direct::DormantReason::PageCoverFailed);
                    return Ok(DirectContinuation::Interpret);
                }
                // G1 pre-install gate (full span): the compiled block may cover chunks past its
                // entry that are churning even when the entry chunk is cold. Refuse installation
                // and park it Dormant so the whole span runs on the interpreter.
                if self.jit_direct.smc_heat.span_hot(
                    key.physical,
                    u32::from(compilation.span.guest_len),
                    heat_epoch,
                ) {
                    self.smc_heat_demote(key, heat_epoch);
                    return Ok(DirectContinuation::Interpret);
                }
                let Some(id) = self.jit_direct.install(&compilation) else {
                    self.jit_direct
                        .dormant(key, jit::direct::DormantReason::InstallFailed);
                    return Ok(DirectContinuation::Interpret);
                };
                self.perf.jit_direct_blocks_installed += 1;
                self.perf.smc_lane_registrations += compilation.imm_lane_count() as u64;
                // Mode-key bit 0 is CS.D (`jit_mode_key`), so a clear bit is a 16-bit code
                // segment. Cold path, so a branch is free here; the two hot counterparts at the
                // block-entry site are written branchlessly.
                if key.mode_key & 1 == 0 {
                    self.perf.jit_direct_blocks_installed_sixteen_bit += 1;
                }
                self.jit_direct
                    .block(id)
                    .expect("installed direct block must be live")
            }
        };
        if self.decode_cache.line_count() != self.jit_direct.decode_slot_count() {
            self.jit_direct.invalidate_translation();
        }
        let block = if self.jit_direct.is_link_visible(block.id()) {
            block
        } else {
            let mut slot_lin = block.span().key.linear;
            if block.fetch_lens().iter().any(|&len| {
                let live = self.decode_cache.line_live(slot_lin, d);
                slot_lin = slot_lin.wrapping_add(u32::from(len));
                !live
            }) {
                return Ok(DirectContinuation::Interpret);
            }
            let Some(block) = self.jit_direct.revalidate_translation(block.span().key) else {
                return Ok(DirectContinuation::Interpret);
            };
            block
        };
        // A hidden short block must pass the canonical decode scan above before it becomes a link
        // target again. Once current, avoid the heavier native-entry validation until one of its
        // own successor cells is live.
        if self.jit_direct.defer_short_enabled()
            && !block.is_self_loop()
            && block.span().instructions < jit::direct::MIN_STANDALONE_INSTRUCTIONS
            && !self.jit_direct.has_linked_successor(block.id())
        {
            self.perf.jit_direct_deferred_short += 1;
            return Ok(DirectContinuation::Interpret);
        }
        match self.run_direct_block(bus, block, budget.total, budget.bus_at_entry, budget.cap)? {
            DirectBlockOutcome::Complete(outcome) => Ok(DirectContinuation::Run(outcome)),
            DirectBlockOutcome::Prefix(outcome) => Ok(DirectContinuation::Prefix(outcome)),
            DirectBlockOutcome::NotRun => Ok(DirectContinuation::Interpret),
        }
    }

    #[cfg(all(
        test,
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn try_direct_continuation_for_test<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
        d: bool,
    ) -> Result<(), CpuError> {
        // The fixtures drive a decoded, live line; a miss here would have reached `key_for`'s
        // `line_phys_start` and returned `Interpret`, which this helper discards either way.
        let Some(view) = self.decode_cache.get_view(lin, d) else {
            return Ok(());
        };
        let _ = self.try_direct_continuation(
            bus,
            &view,
            lin,
            d,
            ContinuationBudget {
                total: 0,
                bus_at_entry: 0,
                cap: u64::MAX,
            },
        )?;
        Ok(())
    }

    /// Drive `dispatch_continuation` (the gate chain plus the latch) on a decoded fixture line.
    #[cfg(all(
        test,
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn dispatch_continuation_for_test<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
        d: bool,
        native_continuations_active: bool,
    ) -> Result<ContinuationDispatch, CpuError> {
        let view = self
            .decode_cache
            .get_view(lin, d)
            .expect("fixture decode line must be live");
        self.dispatch_continuation(
            bus,
            native_continuations_active,
            &view,
            lin,
            d,
            ContinuationBudget {
                total: 0,
                bus_at_entry: 0,
                cap: u64::MAX,
            },
        )
    }

    #[cfg(all(test, feature = "jit"))]
    pub(super) fn skip_direct_once_for_test(&self) -> bool {
        self.direct_runtime.skip_direct_once
    }

    #[cfg(all(test, feature = "jit"))]
    pub(super) fn set_skip_direct_once_for_test(&mut self, on: bool) {
        self.direct_runtime.skip_direct_once = on;
    }

    #[cfg(feature = "jit")]
    fn flush_direct_cache_stats(&mut self) {
        let stats = self.jit_direct.take_stats();
        self.perf.jit_direct_hot_hits += stats.hot_hits;
        self.perf.jit_direct_hash_hits += stats.hash_hits;
        self.perf.jit_direct_lookup_misses += stats.lookup_misses;
        self.perf.jit_direct_cache_resets += stats.cache_resets;
        self.perf.jit_direct_arena_compactions += stats.arena_compactions;
        self.perf.jit_direct_arena_compaction_live_blocks += stats.arena_compaction_live_blocks;
        self.perf.jit_direct_arena_compaction_bytes += stats.arena_compaction_bytes;
        self.perf.jit_direct_arena_compaction_failures += stats.arena_compaction_failures;
        self.perf.jit_direct_links_created += stats.links;
        self.perf.jit_direct_links_cleared += stats.unlinks;
        self.perf.jit_direct_decode_dependencies_scanned += stats.decode_dependencies_scanned;
        self.perf.jit_direct_portals_hidden += stats.portals_hidden;
    }

    /// The worst-case cost of one chain hop, used as the divisor that decides how many blocks a
    /// chain may run before returning to the dispatcher. Depends on the persona timing pair, the
    /// bus cost dials, and one bit of the block: whether it is an x87 block.
    ///
    /// A float entry may cross into integer blocks partway through the chain; charging every hop
    /// at the x87 rate over-estimates those integer hops but stays conservative in the safe
    /// direction, and it still covers the 586 FISTP conversion surcharge for the hops that really
    /// are x87.
    ///
    /// An integer entry USED to be unable to reach a float block, and this bound said so. With the
    /// shared x87 re-entry pad that is no longer true, so the integer bound must now dominate a
    /// float hop as well. It is computed as a `max` rather than asserted, and the max is taken
    /// against the float class's TRUE cost: `has_x87` is `x87_slots != 0` and the block builder
    /// caps such a block at `MAX_X87_BLOCK_INSTRUCTIONS`, so a float hop can never present the 32
    /// instructions of bus traffic the float ENTRY bound charges.
    ///
    /// On both personas the Direct backend runs on this changes nothing: 586 gives an integer
    /// bound of 1,177 against a true float hop of 874, and 486 gives 1,036 against 821, so the
    /// `max` returns the integer term unchanged and the slice stays byte-identical here. It is not
    /// decoration: on a bus whose data dials are all zero the two terms are 12 and 437, the
    /// inequality reverses, and an integer-headed chain would under-budget every float hop.
    /// The float figures move whenever `MAX_X87_BLOCK_CORE_CLOCKS` does (they contain
    /// `scale_core` of it, ceil(5,240 / 12) = 437 at the 1/12 persona pair); the integer figures
    /// do not.
    #[cfg(feature = "jit")]
    fn compute_global_block_upper<B: CpuBus>(bus: &B, num: u32, den: u32, has_x87: bool) -> u64 {
        let scale_core = |unscaled: u64| {
            unscaled
                .saturating_mul(u64::from(num))
                .saturating_add(u64::from(den) - 1)
                / u64::from(den)
        };
        // A block can contain 31 four-clock instructions followed by a ten-clock RET, PLUS its
        // call-out slots. The call-out term is not covered by the four-clock-per-instruction
        // shape: an `IN AL,DX` charges three times that, and the charge is deposited at runtime
        // rather than baked into `raw_clocks`. Without it `iteration_upper` can legitimately
        // exceed this "global maximum" on a bus whose cost dials are all zero (the `CpuBus` trait
        // defaults), where the bus terms that normally swamp the core term vanish -- and the
        // chain-pricing `debug_assert` that `per_hop_estimate <= global_block_upper` would trip
        // on a bound that is supposed to dominate by construction.
        // `MAX_CALL_OUT_CORE_CLOCKS`, not `IN_AL_DX_CORE_CLOCKS`: this bound cannot see which
        // helper a slot carries, so it prices every slot at the largest charge any admitted helper
        // returns. That is 18 (PUSHAD/POPAD) rather than 12 (IN AL,DX) as of the memory class, and
        // the constant is derived from the three per-opcode constants so a fourth helper raises it
        // by construction.
        let callout_core = u64::from(jit::direct::MAX_BLOCK_CALLOUT_SLOTS)
            .saturating_mul(u64::from(MAX_CALL_OUT_CORE_CLOCKS));
        let integer_core = scale_core(
            4u64.saturating_mul(jit::direct::MAX_BLOCK_INSTRUCTIONS as u64) + 6 + callout_core,
        );
        // The x87 class needs no call-out term: x87 and call-out slots never share a block
        // (the compile walk refuses the mix in either order), so an x87 block's `callout_slots`
        // is zero by construction.
        let x87_core = scale_core(jit::direct::MAX_X87_BLOCK_CORE_CLOCKS);
        let max_core = if has_x87 { x87_core } else { integer_core };
        let max_read = bus
            .jit_data_cost_clocks(BusWidth::Dword)
            .max(bus.jit_mode13_data_cost_clocks(BusWidth::Dword))
            .max(bus.jit_data_cost_clocks(BusWidth::Word))
            .max(bus.jit_mode13_data_cost_clocks(BusWidth::Word))
            .max(bus.jit_data_cost_clocks(BusWidth::Byte))
            .max(bus.jit_mode13_data_cost_clocks(BusWidth::Byte));
        let max_store = max_read;
        let per_instruction_bus = bus
            .jit_fetch_cost_clocks()
            .saturating_add(max_read)
            .saturating_add(max_store)
            // One byte-wide port access per instruction covers a block that is nothing but PORT
            // call-out slots, and keeps this bound above `compute_iteration_upper`'s matching
            // term for every slot count. Same reachable set as that term: no TSS probe, because
            // the helper refuses the privilege state that would reach one.
            .saturating_add(bus.jit_io_cost_clocks(BusWidth::Byte));
        // The MEMORY class's traffic, added ONCE for the whole block rather than folded into the
        // per-instruction term, and the difference is not cosmetic.
        //
        // Why a term at all: `max_read + max_store` budgets ONE read and ONE store per instruction,
        // and a PUSHAD or POPAD slot makes `CALL_OUT_STACK_FRAME_DWORDS` accesses in a single
        // instruction. Four of them in a 32-instruction block present 32 accesses where the other
        // 28 instructions leave only 8 unclaimed, so the old bound no longer dominated
        // `compute_iteration_upper`. That is a correctness bug -- the chain-pricing
        // `debug_assert!(per_hop_estimate <= global_block_upper)` would trip on a zero-dial bus --
        // and not a perf question.
        //
        // Why ONCE and not per instruction: `MAX_BLOCK_CALLOUT_SLOTS` bounds the slots per BLOCK,
        // so `MAX_BLOCK_CALLOUT_SLOTS * CALL_OUT_STACK_FRAME_DWORDS` store-costs is already the
        // worst any block can present, and it dominates `compute_iteration_upper`'s
        // `memory_slots * CALL_OUT_STACK_FRAME_DWORDS * dword_data_upper` term by construction
        // (`memory_slots <= MAX_BLOCK_CALLOUT_SLOTS`, and `max_store` is the max over every width
        // and both the RAM and Mode13h dials, so it is `>= dword_data_upper`). Per instruction
        // would multiply it by `MAX_BLOCK_INSTRUCTIONS` -- 256 store-costs of headroom rather than
        // 32 -- for a term whose whole job is to be the smallest value that still dominates.
        //
        // What that choice does NOT buy, corrected by review because the first version of this
        // comment claimed it did: this bound does not throttle anything. `global_block_upper` is
        // read at exactly two places (`run_direct_block`), the memo store and the
        // `debug_assert!(per_hop_estimate <= global_block_upper)` beside it. The chain quota's
        // DIVISOR is `per_hop_estimate`, which is `iteration_upper` -- the block's own cost -- and
        // has been since the per-hop re-pricing landed. So in a release build inflating this eight
        // times over would change no guest-visible behaviour at all; it would only make the debug
        // assertion weaker. Keeping it tight is hygiene, not throughput: the assertion is the only
        // thing standing between a future budget term and a silently unbounded hop, and a bound
        // with 8x slack in it stops catching the thing it exists to catch.
        //
        // Zero for the x87 class, and that is the same by-construction fact the core term uses:
        // x87 and call-out slots never share a block (the compile walk refuses the mix in either
        // order), so an x87 block's `callout_memory_slots` is zero and pricing it would be pure
        // inflation of a bound that already assumes 32 instructions of traffic for a 12-slot cap.
        let callout_memory_bus = if has_x87 {
            0
        } else {
            u64::from(jit::direct::MAX_BLOCK_CALLOUT_SLOTS)
                .saturating_mul(u64::from(jit::direct::CALL_OUT_STACK_FRAME_DWORDS))
                .saturating_mul(max_store)
        };
        let global_raw_bus_upper = (jit::direct::MAX_BLOCK_INSTRUCTIONS as u64)
            .saturating_mul(per_instruction_bus)
            .saturating_add(callout_memory_bus);
        let own_class = max_core.saturating_add(bus.jit_scale_bus_cost_upper(global_raw_bus_upper));
        if has_x87 {
            return own_class;
        }
        // The float hop an integer chain can now reach, at ITS true instruction cap. No call-out
        // memory term for the reason above: the hop being priced is an x87 block, which cannot
        // hold one.
        let x87_raw_bus_upper =
            (jit::direct::MAX_X87_BLOCK_INSTRUCTIONS as u64).saturating_mul(per_instruction_bus);
        let x87_hop = x87_core.saturating_add(bus.jit_scale_bus_cost_upper(x87_raw_bus_upper));
        own_class.max(x87_hop)
    }

    /// THIS block's own worst-case cost for one iteration: its scaled core clocks (integer plus
    /// the FP-class weighted term) plus its own fetch and data traffic folded through the bus
    /// scale, so the result lives in the same scaled guest-clock domain as `cap` and the in-batch
    /// bus growth it is compared against.
    ///
    /// Every input is fixed for the life of the block under one set of cost dials: the counts and
    /// clock totals are block metadata sealed at compile time, and the rest is the persona timing
    /// pair plus the same bus dials `compute_global_block_upper` reads. See `iteration_upper`
    /// below for the memo that exploits this.
    #[cfg(feature = "jit")]
    fn compute_iteration_upper<B: CpuBus>(
        bus: &B,
        block: &jit::direct::CompiledBlock,
        num: u32,
        den: u32,
    ) -> u64 {
        let fp_core_upper = u64::from(block.weighted_fp_clocks())
            .saturating_add(u64::from(FP_TIMING_DEN) - 1)
            / u64::from(FP_TIMING_DEN);
        // Every call-out slot belongs to exactly ONE class, so the two class terms below cover the
        // whole population. A helper that was neither -- the shape a fourth `CallOutHelper` takes
        // if someone adds it without choosing a class -- would be charged NOTHING here and would
        // silently under-budget its block.
        //
        // That invariant is checked at `BlockCache::install` (jit/direct.rs) and NOT here, and the
        // difference is whether the check can fail. Install compares the two class counts against
        // `Compilation::callout_slots`, which the compile walk accumulates INDEPENDENTLY from
        // `kind.is_call_out()` while the class counts come from `kind.call_out_helper()` -- two
        // derivations, so they can disagree. Any check at this level cannot: `CompiledBlock` stores
        // only the packed pair and `callout_slots()` is defined as `port() + memory()`, so
        // asserting their sum against it compares a value with itself. A `debug_assert_eq!` doing
        // exactly that shipped here and was deleted by review; do not reintroduce it.
        // CALL-OUT DOMINANCE. A call-out slot's charge is RUNTIME, so `block.raw_clocks()` --
        // which is the sum of the baked per-kind constants -- does not contain it and the bound
        // would not cover it.
        //
        // Priced BY CLASS rather than at the worst helper, and that is not an optimisation, it is
        // exactness. Every port slot charges exactly `IN_AL_DX_CORE_CLOCKS` and every memory slot
        // exactly `PUSH_ALL_CORE_CLOCKS` (= `POP_ALL_CORE_CLOCKS`); both are the ONLY case for
        // their class, not a worst case, so the sum is exact. Pricing both classes at the maximum
        // of the two would inflate every port-only block -- which is what doom's 20 M call-outs
        // are -- by six core clocks a slot for traffic it cannot generate, and a budget bound
        // decides admission at the margin.
        //
        // Keep this in step with `classify`'s call-out admission: a new helper needs a class here,
        // and one whose charge is not a constant needs its maximum.
        let callout_core_upper = u64::from(block.callout_port_slots())
            .saturating_mul(u64::from(IN_AL_DX_CORE_CLOCKS))
            .saturating_add(
                u64::from(block.callout_memory_slots())
                    .saturating_mul(u64::from(PUSH_ALL_CORE_CLOCKS.max(POP_ALL_CORE_CLOCKS))),
            );
        let scaled_core_upper = u64::from(block.raw_clocks())
            .saturating_add(fp_core_upper)
            .saturating_add(callout_core_upper)
            .saturating_mul(u64::from(num))
            .saturating_add(u64::from(den) - 1)
            / u64::from(den);
        let fetch_upper = bus
            .jit_fetch_cost_clocks()
            .saturating_mul(u64::from(block.span().instructions));
        let byte_data_upper = bus
            .jit_data_cost_clocks(BusWidth::Byte)
            .max(bus.jit_mode13_data_cost_clocks(BusWidth::Byte));
        let word_data_upper = bus
            .jit_data_cost_clocks(BusWidth::Word)
            .max(bus.jit_mode13_data_cost_clocks(BusWidth::Word));
        let dword_data_upper = bus
            .jit_data_cost_clocks(BusWidth::Dword)
            .max(bus.jit_mode13_data_cost_clocks(BusWidth::Dword));
        let data_upper = byte_data_upper
            .saturating_mul(u64::from(block.byte_reads()))
            .saturating_add(word_data_upper.saturating_mul(u64::from(block.word_reads())))
            .saturating_add(dword_data_upper.saturating_mul(u64::from(block.dword_reads())))
            .saturating_add(byte_data_upper.saturating_mul(u64::from(block.byte_stores())))
            .saturating_add(word_data_upper.saturating_mul(u64::from(block.word_stores())))
            .saturating_add(dword_data_upper.saturating_mul(u64::from(block.dword_stores())));
        // The call-out's BUS term, the other half of the dominance argument, and split by class for
        // the reason the core term above is.
        //
        // PORT: ONE access, a byte-wide port read. There is deliberately no TSS-probe term. The
        // helper refuses `is_v86_mode() || CPL > IOPL` as its first statement
        // (jit/direct/callout.rs), which is exactly the condition under which `check_io_permission`
        // would leave its early return and touch memory -- so the probe's word and byte reads are
        // not in the call-out's reachable set at all, and pricing them here would inflate every
        // call-out block's bound for traffic that cannot happen. If that refusal is ever relaxed,
        // this term comes back with it.
        //
        // MEMORY: exactly `CALL_OUT_STACK_FRAME_DWORDS` dword accesses, no more and no fewer.
        // `call_out_stack_frame_resident` refuses the frame unless all eight resolve through the
        // FastMap, so there is no page-walk traffic to price either -- the same shape of argument,
        // from the same kind of pre-check. `dword_data_upper` is reused rather than
        // `jit_data_cost_clocks(Dword)` alone so a Mode13h dial that exceeds the RAM dial cannot
        // make this an under-estimate; the aperture itself is refused by the pre-check, so the
        // `max` is slack rather than reachable traffic.
        let callout_bus_upper = u64::from(block.callout_port_slots())
            .saturating_mul(bus.jit_io_cost_clocks(BusWidth::Byte))
            .saturating_add(
                u64::from(block.callout_memory_slots())
                    .saturating_mul(u64::from(jit::direct::CALL_OUT_STACK_FRAME_DWORDS))
                    .saturating_mul(dword_data_upper),
            );
        let raw_bus_upper = fetch_upper
            .saturating_add(data_upper)
            .saturating_add(callout_bus_upper);
        // `cap` and the in-batch bus growth use the bus's scaled guest-clock domain. Fold the raw
        // fetch/data bound through that same scale before deciding how much native work fits.
        scaled_core_upper.saturating_add(bus.jit_scale_bus_cost_upper(raw_bus_upper))
    }

    /// `compute_iteration_upper` memoised per block, keyed on the bus's cost-dial epoch. Same key
    /// and same shape as the `global_block_upper` memo in `run_direct_block`, one level down: that
    /// one collapses to two entries because it reads only `has_x87()` off the block, this one has
    /// to be per block because it reads the block's own clock and access counts.
    ///
    /// This ran on EVERY direct-block entry, not just the chain-eligible ones, and it is the more
    /// expensive of the two: six bus accessor calls, three `max`, eight multiplies and two
    /// divisions with runtime denominators, all to rederive a number that cannot move until the
    /// dials do. The `debug_assert_eq!` recomputes and compares on every entry, so a debug or test
    /// build proves the memo continuously and a release build pays nothing for it.
    #[cfg(feature = "jit")]
    fn iteration_upper<B: CpuBus>(
        &mut self,
        bus: &B,
        block: &jit::direct::CompiledBlock,
        num: u32,
        den: u32,
    ) -> u64 {
        let epoch = bus.jit_cost_dial_epoch();
        let cached = self.jit_direct.iteration_upper_cached(block.id(), epoch);
        let value = if cached != 0 {
            cached
        } else {
            let computed = Self::compute_iteration_upper(bus, block, num, den);
            self.jit_direct
                .set_iteration_upper_cached(block.id(), epoch, computed);
            computed
        };
        debug_assert_eq!(
            value,
            Self::compute_iteration_upper(bus, block, num, den),
            "cached iteration_upper went stale"
        );
        value
    }

    /// Classify the successor a `StaticUnbound` exit just failed to reach. The CPU's EIP at this
    /// point IS that successor's address, so the key it would have been compiled under is
    /// recoverable without threading the exiting slot out of the native frame.
    #[cfg(feature = "jit")]
    #[inline(never)]
    fn classify_unbound_exit(&mut self) {
        let lin = self.linear_eip();
        let d = self.registers.cs().default_size_32;
        // The `key_for` refusal is CLASSIFIED, not dropped. Returning early here would make the
        // class totals fall short of `jit_direct_unresolved_static_unbound` by an unknown amount,
        // and the whole point of the table is that it closes on that counter — see
        // `unbound_target_classes_are_exhaustive` (jit/direct_test.rs).
        let (kind, linear) = match jit::direct::key_for(self, lin, d) {
            Some(key) => (self.jit_direct.classify_unbound_target(key), key.linear()),
            None => (jit::direct::UnboundTarget::NoKey, 0),
        };
        self.jit_direct.note_unbound_target(kind, linear);
    }

    /// The dynamic-successor counterpart of `classify_unbound_exit`. Same recovery of the key
    /// from the live EIP, separate lane, because a dynamic miss and a static unbound have
    /// different fixes: a static unbound wants the target compiled, a dynamic miss whose target
    /// is already `CompiledButUnlinked` wants a wider inline cache.
    #[cfg(feature = "jit")]
    #[inline(never)]
    fn classify_dynamic_miss_exit(&mut self) {
        let lin = self.linear_eip();
        let d = self.registers.cs().default_size_32;
        // Same closure requirement as `classify_unbound_exit`: this lane closes on
        // `jit_direct_unresolved_dynamic_miss_or_unbound`.
        //
        // The entry linear is now recovered alongside the class, exactly as the static lane has
        // always done. Discarding it left every dynamic miss into a REJECTED block attributed to
        // no row at all -- 2.86M exits on quake, larger than its whole attributed static row set,
        // and the lane Slice 4 found was 65% the size of the static one for the row it lowered.
        let (kind, linear) = match jit::direct::key_for(self, lin, d) {
            Some(key) => (self.jit_direct.classify_unbound_target(key), key.linear()),
            None => (jit::direct::UnboundTarget::NoKey, 0),
        };
        self.jit_direct.note_dynamic_miss_target(kind, linear);
    }

    #[cfg(feature = "jit")]
    fn run_direct_block<B: CpuBus>(
        &mut self,
        bus: &mut B,
        block: jit::direct::CompiledBlock,
        total: u64,
        bus_at_entry: u64,
        cap: u64,
    ) -> Result<DirectBlockOutcome, CpuError> {
        if self.profile.enabled || diff_trace_enabled() {
            self.perf.jit_direct_reject_observer += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }
        if self.interrupt_shadow {
            self.perf.jit_direct_reject_interrupt_shadow += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }
        if !bus.native_aggregate_accounting_allowed() {
            self.perf.jit_direct_reject_aggregate_accounting += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }
        let span = block.span();
        if span.key.mode_key != self.jit_mode_key() {
            self.perf.jit_direct_reject_mode_key += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }
        if block
            .x87_entry_top()
            .is_some_and(|expected| self.fpu.top() != expected)
        {
            self.perf.jit_direct_reject_x87_top += 1;
            self.jit_direct.retire_key_for_recompile(span.key);
            return Ok(DirectBlockOutcome::NotRun);
        }
        // Fetched once and held in a local across all three descriptor checks. It used to ride
        // every `CompiledBlock` copy at 116 bytes a piece; the checks only ever read it.
        let Some(segments) = self.jit_direct.segment_layout(block.id()) else {
            return Ok(DirectBlockOutcome::NotRun);
        };
        if !segments.cs_matches(self) {
            self.perf.jit_direct_reject_cs_layout += 1;
            self.jit_direct.retire_key_for_recompile(span.key);
            return Ok(DirectBlockOutcome::NotRun);
        }
        if block.memory_cpl3() != (self.current_privilege_level() == 3) {
            self.perf.jit_direct_reject_cpl += 1;
            self.jit_direct.retire_key_for_recompile(span.key);
            return Ok(DirectBlockOutcome::NotRun);
        }
        // A block carrying an interpreter call-out slot does not run in the privilege state whose
        // port reads consult the TSS bitmap. THIS is the load-bearing gate; the matching refusal
        // inside the helper (jit/direct/callout.rs) is the second line of defence, kept because it
        // is what makes the helper's zero-partial-effects property hold on its own terms.
        //
        // Correctness is settled by the helper. What this site buys is COST. Without it, a paged
        // V86 or CPL>IOPL guest -- EMM386 and VCPI DOS, first-class targets here -- would pay, on
        // every single execution of a compiled IN: the whole-set spill, the scratch frame, the
        // indirect call, the guard's refusal, the whole-set reload, the abnormal side exit, AND
        // then the dispatcher trip back to the interpreter it would have taken anyway. Strictly
        // worse than the pre-slice barrier, and unconditionally so. Refusing here returns the
        // block to the interpreter, which is exactly the pre-slice behaviour.
        //
        // Two field reads at a site that already reads privilege state for the check above, and
        // both are behind `callout_slots() != 0`, so a block without a slot pays one compare.
        // NOT retired: the state is transient (a V86 task returns to ring 0 and back), and the
        // block is perfectly good -- it is the privilege level that is wrong, like the alignment
        // and budget refusals below rather than the layout ones above.
        // Keyed on the PORT class alone as of the memory class. The reason a call-out block is
        // refused here is the TSS bitmap probe that `check_io_permission` would take, and PUSHAD
        // and POPAD do not probe it: they are unprivileged instructions whose only
        // privilege-sensitive decision is page protection, which `call_out_stack_frame_resident`
        // makes against the LIVE CPL inside the helper and fails closed on. Refusing a PUSHAD block
        // here would cost every ring-3 protected-mode guest the lowering for nothing.
        if block.callout_port_slots() != 0
            && (self.is_v86_mode() || self.current_privilege_level() > self.iopl())
        {
            self.jit_direct.note_reject_callout_privileged();
            return Ok(DirectBlockOutcome::NotRun);
        }
        let has_link = self.jit_direct.has_linked_successor(block.id());
        let data_descriptors_match = if has_link {
            segments.all_data_matches(self)
        } else {
            segments.data_matches(self)
        };
        if !data_descriptors_match {
            self.perf.jit_direct_reject_data_segment += 1;
            self.jit_direct.retire_key_for_recompile(span.key);
            return Ok(DirectBlockOutcome::NotRun);
        }
        if block.has_wide_accesses() && self.alignment_armed && self.current_privilege_level() == 3
        {
            self.perf.jit_direct_reject_alignment += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }
        let eip = self.registers.eip;
        let fetch_last = u32::from(span.guest_len) - 1;
        if self
            .registers
            .cs()
            .limit
            .checked_sub(fetch_last)
            .is_none_or(|last_start| eip > last_start)
        {
            self.perf.jit_direct_reject_fetch_limit += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }

        let chain_eligible =
            has_link && !(self.alignment_armed && self.current_privilege_level() == 3);
        if self.jit_direct.defer_short_enabled()
            && !block.is_self_loop()
            && span.instructions < jit::direct::MIN_STANDALONE_INSTRUCTIONS
            && !chain_eligible
        {
            self.perf.jit_direct_deferred_short += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }

        let (num, den) = level_timing(self.persona());
        let bus_growth = bus
            .in_batch_scaled_bus_clocks()
            .saturating_sub(bus_at_entry);
        let iteration_upper = self.iteration_upper(bus, &block, num, den);
        let used = total.saturating_add(bus_growth);
        let available = cap.saturating_sub(used).saturating_sub(1);
        let budget_quota = available.checked_div(iteration_upper).unwrap_or(u64::MAX);
        const MAX_NATIVE_SELF_LOOP_ITERATIONS: u64 = 4_096;
        let quota = if block.is_self_loop() {
            budget_quota.min(MAX_NATIVE_SELF_LOOP_ITERATIONS)
        } else if budget_quota == 0 {
            0
        } else {
            // Devices advance only after native return. I/O, flag-control operations, segment
            // changes, and interrupt-shadow boundaries are compiler barriers, so budget and the
            // bounded block count are the only steady-state boundary checks needed here.
            if !chain_eligible {
                1
            } else {
                // An integer entry never reaches a float block, so its bound only has to cover
                // integer hops. A float entry may cross into integer blocks partway through the
                // chain; charging every hop at the x87 rate over-estimates those integer hops but
                // stays conservative in the safe direction, and it still covers the 586 FISTP
                // conversion surcharge for the hops that are actually x87.
                self.perf.jit_direct_chain_quota_entries += 1;
                // `global_block_upper` reads exactly one thing from the block, `has_x87()`, and
                // otherwise only the persona and the bus cost dials. The dials move only when the
                // persona does: `CacheModel::set_mode` is their sole writer, its only caller is
                // `Machine::set_mode`, and that calls `CpuGsw::set_mode` first, which clears this
                // cache along with every compiled block. So the whole computation collapses to a
                // two-entry table, and recomputing it on every chain-eligible entry was spending
                // six bus accessor calls, five `max`, three multiplies and a division to rederive
                // one of two numbers.
                //
                // 0 means unset, and it can never collide with a real value: `global_block_upper`
                // is at least `max_core`, which is `ceil(unscaled_max_core * num / den)` with
                // `unscaled_max_core` either 134 or 5,240 and `num >= 1` on every persona, so it
                // is at least 1 on every bus including the trait defaults.
                let x87_index = usize::from(block.has_x87());
                let epoch = bus.jit_cost_dial_epoch();
                let cached = self.jit_direct.global_block_upper_cached(x87_index, epoch);
                let global_block_upper = if cached != 0 {
                    cached
                } else {
                    self.perf.jit_direct_chain_quota_cache_misses += 1;
                    let computed = Self::compute_global_block_upper(bus, num, den, block.has_x87());
                    self.jit_direct
                        .set_global_block_upper_cached(x87_index, epoch, computed);
                    computed
                };
                debug_assert_eq!(
                    global_block_upper,
                    Self::compute_global_block_upper(bus, num, den, block.has_x87()),
                    "cached global_block_upper went stale"
                );
                // CHAIN PRICING. `global_block_upper` is a SOUND bound — it prices every hop as
                // 32 four-clock instructions plus RET plus 32 worst-case bus accesses — and that
                // soundness is what makes it useless. The measured average Direct block is 6.4
                // instructions at roughly 2 clocks, so the sound bound overprices a hop by 10-40x
                // and cuts chains that far short of the budget they were allowed. The dispatch
                // audit (dev_docs/2026-07-30-dispatch-architecture-audit.md) attributes ~13.4M of
                // 46.7M stint-ends to this, against a PIT-edge cap that fires 22,903 times.
                //
                // It is also FAKE precision. It guarantees native code never overshoots the edge
                // by one block while the interpreter arm of the same loop overshoots by an
                // instruction and a REP fast path by a whole string chunk. The owner's ruling
                // (2026-07-30) is that sub-perceptual timing exactness is not worth this, and
                // real parts of the same stepping vary between packages anyway.
                //
                // `iteration_upper` is THIS block's actual cost, already computed above for the
                // first hop. Chains are loop bodies, so the entry block is a far better estimate
                // of the next hop than the global maximum. Overshoot is now possible and bounded
                // by MAX_CHAIN_BLOCKS hops of (real hop cost - entry cost); `brk_cap` and the
                // scaled-bus term still end the run, one block late instead of 10-40x early.
                //
                // `global_block_upper` stays live below as the memoised two-entry table and as the
                // input to the `debug_assert!` on the next line, and as NOTHING ELSE -- those two
                // are its complete consumer set. An earlier version of this comment also called it
                // "the x87 crossing guard's input"; that consumer does not exist and the claim was
                // deleted by review. The practical consequence is worth stating where someone
                // tuning `compute_global_block_upper` will read it: in a RELEASE build that
                // function's result is dead except for the memo, so changing it moves no guest
                // number and no wall number. Only the divisor below does.
                let per_hop_estimate = iteration_upper.max(1);
                debug_assert!(per_hop_estimate <= global_block_upper.max(1));
                let additional = available
                    .saturating_sub(iteration_upper)
                    .checked_div(per_hop_estimate)
                    .unwrap_or(jit::direct::MAX_CHAIN_BLOCKS as u64 - 1);
                1 + additional.min(jit::direct::MAX_CHAIN_BLOCKS as u64 - 1)
            }
        };
        if quota == 0 {
            self.perf.jit_direct_reject_zero_budget += 1;
            return Ok(DirectBlockOutcome::NotRun);
        }

        let uniform_fetches = bus.native_fetches_are_uniform();
        let trace_capacity = if uniform_fetches {
            0
        } else if block.is_self_loop() {
            1
        } else {
            quota.min(jit::direct::MAX_CHAIN_BLOCKS as u64) as usize
        };
        let mut trace = Vec::<std::mem::MaybeUninit<jit::direct::NativeBlockTrace>>::with_capacity(
            trace_capacity,
        );
        let mut exit = jit::direct::NativeExit {
            trace_ptr: if uniform_fetches {
                0
            } else {
                trace.as_mut_ptr() as usize
            },
            ..jit::direct::NativeExit::default()
        };
        // Arena compaction can relocate code while callers still hold a copied block descriptor.
        // Resolve its generational ID at the last safe point before entering native code.
        let Some(current_block) = self.jit_direct.block(block.id()) else {
            return Ok(DirectBlockOutcome::NotRun);
        };
        self.begin_instruction();
        self.core_clocks_so_far = total;
        let flags = self.materialized_eflags();
        // SAFETY: direct::emit produced this page using the exact four-argument ABI, the arena
        // sealed it executable, and the current generational lookup keeps that arena entry live.
        let entry: jit::direct::DirectEntryFn =
            unsafe { std::mem::transmute(current_block.entry_ptr()) };
        // The call-out window: two stores in, two out, so the erased `*mut B` is never reachable
        // outside the call that owns the borrow. `publish::<B>` also picks the helper
        // instantiations for THIS bus type, which is what makes the erasure sound; see
        // `jit/direct/callout.rs`.
        //
        // UNCONDITIONAL, deliberately. Gating it on `block.callout_slots() != 0` would be wrong,
        // not merely different: a chained native transfer jumps into a SUCCESSOR block's body
        // without returning here, so an entry block with no call-out can reach a chained block
        // that has one. The entry block's own slot count says nothing about the chain.
        self.native_callout = jit::direct::CallOutTable::publish(bus);
        unsafe {
            entry(
                self as *mut CpuGsw,
                flags,
                quota as u32,
                &mut exit as *mut jit::direct::NativeExit,
            );
        }
        self.native_callout = jit::direct::CallOutTable::default();
        debug_assert!((exit.trace_len as usize) <= trace_capacity);
        debug_assert_eq!(exit.trace_len == 0, uniform_fetches);
        debug_assert!(u64::from(exit.linked_transfers) < quota);
        debug_assert!(exit.mode13_dirty_pages <= u64::from(u16::MAX));
        debug_assert!(exit.side_exit <= 1);
        debug_assert!(
            exit.side_exit != 0
                || exit.side_exit_reason == jit::direct::SideExitReason::None as u32
        );
        debug_assert!(exit.side_exit_reason <= jit::direct::SideExitReason::MAX);
        let side_exit = exit.side_exit != 0;

        let final_eip = self.registers.eip;
        let cs_base = self.registers.cs().base;
        if exit.dynamic_link_cell != 0 {
            debug_assert_eq!(exit.dynamic_target_eip, final_eip);
            self.jit_direct.bind_dynamic_successor(
                exit.dynamic_link_cell,
                exit.dynamic_target_eip,
                cs_base.wrapping_add(exit.dynamic_target_eip),
                span.key.mode_key,
            );
        }
        let instructions = exit.instructions;
        let fp = jit::native_x87::scale_weighted_fp_clocks(exit.weighted_fp_clocks, self.fp_rem);
        self.fp_rem = fp.remainder;
        let raw_clocks = exit.raw_clocks.saturating_add(fp.clocks);
        let byte_reads = exit.byte_reads & u64::from(u32::MAX);
        let word_reads = exit.byte_reads >> 32;
        let dword_reads = exit.dword_reads;
        let mode13_byte_reads = exit.mode13_byte_reads & u64::from(u32::MAX);
        let mode13_word_reads = exit.mode13_byte_reads >> 32;
        let ram_byte_writes = exit.ram_byte_writes & u64::from(u32::MAX);
        let ram_word_writes = exit.ram_byte_writes >> 32;
        let mode13_byte_writes = exit.mode13_byte_writes & u64::from(u32::MAX);
        let mode13_word_writes = exit.mode13_byte_writes >> 32;
        if uniform_fetches {
            bus.charge_bus_clocks_bulk(bus.jit_fetch_cost_clocks().saturating_mul(instructions));
        } else {
            let trace = unsafe {
                std::slice::from_raw_parts(
                    trace.as_ptr().cast::<jit::direct::NativeBlockTrace>(),
                    exit.trace_len as usize,
                )
            };
            let mut traced_instructions = 0u64;
            for trace in trace {
                let traced = self
                    .jit_direct
                    .block_for_trace(trace.linear, trace.physical, span.key.mode_key)
                    .expect("resident native trace must name a live block");
                debug_assert!(trace.prefix_instructions <= u32::from(traced.span().instructions));
                let repetitions = u64::from(trace.repetitions);
                traced_instructions = traced_instructions
                    .saturating_add(
                        repetitions.saturating_mul(u64::from(traced.span().instructions)),
                    )
                    .saturating_add(u64::from(trace.prefix_instructions));
                self.registers.eip = trace.linear.wrapping_sub(cs_base);
                if !bus.charge_native_cached_fetches(
                    trace.linear,
                    trace.physical,
                    traced.fetch_lens(),
                    repetitions,
                ) {
                    for _ in 0..repetitions {
                        let mut fetch_lin = trace.linear;
                        for &len in traced.fetch_lens() {
                            self.charge_cached_fetch(bus, fetch_lin, len)
                                .expect("validated direct-block fetch charge cannot fault");
                            fetch_lin = fetch_lin.wrapping_add(u32::from(len));
                        }
                    }
                }
                let mut fetch_lin = trace.linear;
                for &len in traced
                    .fetch_lens()
                    .iter()
                    .take(trace.prefix_instructions as usize)
                {
                    self.charge_cached_fetch(bus, fetch_lin, len)
                        .expect("validated direct-block prefix fetch charge cannot fault");
                    fetch_lin = fetch_lin.wrapping_add(u32::from(len));
                }
            }
            debug_assert_eq!(traced_instructions, instructions);
        }
        self.registers.eip = final_eip;

        debug_assert!(mode13_byte_reads <= byte_reads);
        debug_assert!(mode13_word_reads <= word_reads);
        debug_assert!(exit.mode13_dword_reads <= dword_reads);
        let ram_byte_reads = byte_reads - mode13_byte_reads;
        let ram_word_reads = word_reads - mode13_word_reads;
        let ram_dword_reads = dword_reads - exit.mode13_dword_reads;

        let data_clocks = bus
            .jit_data_cost_clocks(BusWidth::Byte)
            .saturating_mul(ram_byte_reads)
            .saturating_add(
                bus.jit_data_cost_clocks(BusWidth::Word)
                    .saturating_mul(ram_word_reads),
            )
            .saturating_add(
                bus.jit_data_cost_clocks(BusWidth::Dword)
                    .saturating_mul(ram_dword_reads),
            )
            .saturating_add(
                bus.jit_mode13_data_cost_clocks(BusWidth::Byte)
                    .saturating_mul(mode13_byte_reads),
            )
            .saturating_add(
                bus.jit_mode13_data_cost_clocks(BusWidth::Word)
                    .saturating_mul(mode13_word_reads),
            )
            .saturating_add(
                bus.jit_mode13_data_cost_clocks(BusWidth::Dword)
                    .saturating_mul(exit.mode13_dword_reads),
            )
            .saturating_add(
                bus.jit_data_cost_clocks(BusWidth::Byte)
                    .saturating_mul(ram_byte_writes),
            )
            .saturating_add(
                bus.jit_data_cost_clocks(BusWidth::Word)
                    .saturating_mul(ram_word_writes),
            )
            .saturating_add(
                bus.jit_data_cost_clocks(BusWidth::Dword)
                    .saturating_mul(exit.ram_dword_writes),
            );
        bus.charge_bus_clocks_bulk(data_clocks);
        bus.charge_native_mode13_writes(izarravm_bus::NativeMode13Writes {
            dirty_pages: exit.mode13_dirty_pages as u16,
            byte_writes: mode13_byte_writes,
            word_writes: mode13_word_writes,
            dword_writes: exit.mode13_dword_writes,
        });

        let charged = self.scale_clocks_batch(raw_clocks);
        self.elapsed_clocks += charged;
        let reads = byte_reads + word_reads + dword_reads;
        let writes = ram_byte_writes
            + ram_word_writes
            + exit.ram_dword_writes
            + mode13_byte_writes
            + mode13_word_writes
            + exit.mode13_dword_writes;
        if writes != 0
            && let Some(page) = self.prefetch.physical_page()
        {
            // Native stores are reported in one batch, without an address per write. Mark the
            // current prefetch page conservatively so the next instruction drops any stale bytes.
            self.record_write_page(page << 12);
        }
        self.perf.instructions += instructions;
        self.perf.jit_direct_entries += 1;
        self.perf.jit_direct_insns += instructions;
        // The CS.D = 0 split of the two lines above. Mode-key bit 0 is CS.D, so a clear bit is a
        // 16-bit code segment. Branchless because this is the hottest path in the backend: the
        // predicate is a compare into a flag and the add is unconditional, so a 32-bit block
        // pays two arithmetic ops and no misprediction.
        let sixteen_bit = u64::from(block.span().key.mode_key & 1 == 0);
        self.perf.jit_direct_entries_sixteen_bit += sixteen_bit;
        self.perf.jit_direct_insns_sixteen_bit += sixteen_bit * instructions;
        // Same shape and the same reasoning as the pair above: a widened predicate and two
        // unconditional adds, because this is the hottest path in the backend. Lands in
        // `DirectStallTally` rather than `PerfCounters`, which sits ahead of `pending_flags` in
        // `CpuGsw` at an offset emitted code bakes.
        self.jit_direct.note_segment_write_block_entry(
            u64::from(block.is_segment_write_block()),
            instructions,
        );
        self.perf.jit_direct_linked_transfers += u64::from(exit.linked_transfers);
        match exit.unresolved_reason {
            jit::direct::UnresolvedReason::None => {}
            jit::direct::UnresolvedReason::StaticUnbound => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_static_unbound += 1;
                // Gated at the CALL SITE, not inside the classifier: this is 56% of all stint
                // ends and `key_for` reads segment state and builds a three-word key.
                if self.jit_direct.barrier_census_active() {
                    self.classify_unbound_exit();
                }
            }
            jit::direct::UnresolvedReason::StaticHidden => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_static_hidden += 1;
            }
            jit::direct::UnresolvedReason::DynamicMissOrUnbound => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_dynamic_miss_or_unbound += 1;
                // The same classification the StaticUnbound arm runs, into its own lane. This is
                // the only remaining exit pool large enough to move wall (20% of entries) and
                // nothing distinguished "the target was never compiled" from "the target is
                // compiled and the two-way inline cache missed it". `classify_unbound_target`
                // answers exactly that: CompiledButUnlinked means the block exists and the PIC
                // could not name it, anything else means it does not exist yet.
                //
                // Gated at the CALL SITE for the reason the static arm documents: `key_for`
                // reads segment state and builds a three-word key, and this fires 6.9M times.
                if self.jit_direct.barrier_census_active() {
                    self.classify_dynamic_miss_exit();
                }
            }
            jit::direct::UnresolvedReason::DynamicHidden => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_dynamic_hidden += 1;
            }
            jit::direct::UnresolvedReason::X87TopMismatch => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_x87_pad_bails += 1;
            }
        }
        self.perf.jit_native_load_hits += reads;
        self.perf.data_direct_reads += reads;
        self.perf.direct_data_pointer_reads += reads;
        self.perf.jit_native_store_hits += writes;
        self.perf.data_direct_writes += writes;
        self.perf.direct_data_pointer_writes += writes;
        if side_exit {
            self.perf.jit_direct_side_exits += 1;
            match exit.side_exit_reason {
                reason if reason == jit::direct::SideExitReason::CrossPageOrAlignment as u32 => {
                    self.perf.jit_direct_exit_cross_page_or_alignment += 1;
                }
                reason if reason == jit::direct::SideExitReason::UnavailableOrKind as u32 => {
                    self.perf.jit_direct_exit_unavailable_or_kind += 1;
                }
                reason if reason == jit::direct::SideExitReason::Permission as u32 => {
                    self.perf.jit_direct_exit_permission += 1;
                }
                reason if reason == jit::direct::SideExitReason::SegmentLimit as u32 => {
                    self.jit_direct.note_side_exit_segment_limit();
                }
                reason if reason == jit::direct::SideExitReason::X87Eligibility as u32 => {
                    self.jit_direct.note_side_exit_x87_eligibility();
                }
                reason if reason == jit::direct::SideExitReason::DivideGuard as u32 => {
                    self.jit_direct.note_side_exit_divide_guard();
                }
                reason if reason == jit::direct::SideExitReason::CodeWatch as u32 => {
                    self.perf.jit_direct_exit_code_watch += 1;
                }
                reason if reason == jit::direct::SideExitReason::CallOutStepBreak as u32 => {
                    self.jit_direct.note_side_exit_callout_step_break();
                }
                reason if reason == jit::direct::SideExitReason::CallOutAbnormal as u32 => {
                    self.jit_direct.note_side_exit_callout_abnormal();
                }
                _ => self.perf.jit_direct_exit_other += 1,
            }
        }
        if self.is_ring0_protected() {
            self.perf.monitor_resident_core_clocks += charged;
        }
        let outcome = CycleOutcome {
            core_clocks: charged.min(u64::from(u32::MAX)) as u32,
            halted: false,
        };
        if side_exit {
            Ok(DirectBlockOutcome::Prefix(outcome))
        } else {
            Ok(DirectBlockOutcome::Complete(outcome))
        }
    }

    /// The memoised `iteration_upper` for one block, at the live persona. Pairs with
    /// `recompute_iteration_upper_for_test` so a test can assert the memo against a fresh
    /// computation without entering native code.
    #[cfg(all(feature = "jit", test))]
    pub(crate) fn iteration_upper_for_test<B: CpuBus>(
        &mut self,
        bus: &B,
        block: &jit::direct::CompiledBlock,
    ) -> u64 {
        let (num, den) = level_timing(self.persona());
        self.iteration_upper(bus, block, num, den)
    }

    #[cfg(all(feature = "jit", test))]
    pub(crate) fn recompute_iteration_upper_for_test<B: CpuBus>(
        &self,
        bus: &B,
        block: &jit::direct::CompiledBlock,
    ) -> u64 {
        let (num, den) = level_timing(self.persona());
        Self::compute_iteration_upper(bus, block, num, den)
    }

    #[cfg(all(feature = "jit", test))]
    #[cfg_attr(
        not(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )),
        allow(dead_code)
    )]
    pub(crate) fn try_run_direct_block_for_test<B: CpuBus>(
        &mut self,
        bus: &mut B,
        block: jit::direct::CompiledBlock,
    ) -> Result<bool, CpuError> {
        self.try_run_direct_block_with_cap_for_test(bus, block, u64::MAX)
    }

    #[cfg(all(feature = "jit", test))]
    #[cfg_attr(
        not(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )),
        allow(dead_code)
    )]
    pub(crate) fn try_run_direct_block_with_cap_for_test<B: CpuBus>(
        &mut self,
        bus: &mut B,
        block: jit::direct::CompiledBlock,
        cap: u64,
    ) -> Result<bool, CpuError> {
        let bus_at_entry = bus.in_batch_scaled_bus_clocks();
        let outcome = self.run_direct_block(bus, block, 0, bus_at_entry, cap)?;
        // This helper bypasses `run_budgeted`, which is where the per-batch flush now lives, so a
        // fixture that entered a block here would never see the cache's stats reach `perf`.
        self.flush_direct_cache_stats();
        Ok(!matches!(outcome, DirectBlockOutcome::NotRun))
    }

    /// Execute one already-decoded cached instruction as a straight-line continuation. Consumes the
    /// one-instruction STI shadow (a running instruction uses up the one-cycle delay), charges the
    /// cached-hit fetch (without re-decoding, so no double charge), runs the decoded form, and uses a
    /// small profiling-off success tail. Faults and profiling route through `finish_instruction`, so a
    /// mid-run fault still rewinds eip to the faulting instruction and delivers normally.
    #[inline]
    fn run_one_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        view: &DecodeLineView,
        lin: u32,
    ) -> Result<CycleOutcome, CpuError> {
        let insn = &view.insn;
        self.interrupt_shadow = false;
        self.begin_instruction();
        let start_eip = self.registers.eip;
        // One pre-execution CS snapshot: the selector for the fault rewind, and (jit) the decode
        // key + base for the sim's decode-line lookup and branch-target arithmetic, captured
        // before the instruction runs so they match the instruction that is about to retire.
        let start_cs_register = self.registers.cs();
        let start_cs = start_cs_register.selector;
        let profiling = self.profile.enabled;
        if !profiling {
            return match self
                .charge_cached_fetch_at(bus, lin, insn.len, view.phys_start)
                .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus))
            {
                Ok(outcome) => {
                    let charged = self.scale_clocks(outcome.core_clocks);
                    self.elapsed_clocks += charged;
                    self.perf.instructions += 1;
                    // Gated at the CALL SITE, not inside the hook: this is the common
                    // interpreted-retire tail (506.85M instructions in a Quake/586 run) and the
                    // census, off by default, never consumes it otherwise.
                    #[cfg(feature = "jit")]
                    if self.jit_direct.barrier_census_active() {
                        self.jit_direct.note_barrier_census_interpreted(insn);
                    }
                    // This non-profiling fast tail is the COMMON continuation retire path; observe
                    // the instruction here (once) so the sim count tracks perf.instructions.
                    #[cfg(feature = "jit")]
                    self.unit_sim_observe(
                        insn,
                        lin,
                        start_cs_register.default_size_32,
                        start_cs_register.base,
                    );
                    if self.is_ring0_protected() {
                        self.perf.monitor_resident_core_clocks += charged;
                    }
                    if diff_trace_enabled() {
                        self.emit_diff_trace_line(start_cs, start_eip);
                    }
                    Ok(CycleOutcome {
                        core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                        halted: outcome.halted,
                    })
                }
                Err(fault) => {
                    self.finish_instruction(bus, Err(fault), start_eip, start_cs, 0, None, None)
                }
            };
        }
        let profile_start = self.profile.sample_start();
        let result = self
            .charge_cached_fetch_at(bus, lin, insn.len, view.phys_start)
            .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus));
        // Profiling path: finish_instruction retires (increments perf.instructions) on Ok; observe
        // the same Ok retirements here so the count stays exact when profiling is enabled.
        #[cfg(feature = "jit")]
        if result.is_ok() {
            if self.jit_direct.barrier_census_active() {
                self.jit_direct.note_barrier_census_interpreted(insn);
            }
            self.unit_sim_observe(
                insn,
                lin,
                start_cs_register.default_size_32,
                start_cs_register.base,
            );
        }
        self.finish_instruction(
            bus,
            result,
            start_eip,
            start_cs,
            0,
            profiling.then_some((
                insn.group,
                cpu_profile_opcode(insn),
                CpuProfileOperandForm::from_insn(insn),
            )),
            profile_start,
        )
    }

    #[inline]
    fn run_one_cached_budgeted<B: CpuBus>(
        &mut self,
        bus: &mut B,
        view: &DecodeLineView,
        lin: u32,
        rep_budget: RepBudget,
    ) -> Result<CycleOutcome, CpuError> {
        let insn = &view.insn;
        debug_assert!(insn.prefixes.rep.is_some());
        self.interrupt_shadow = false;
        self.begin_instruction();
        let start_eip = self.registers.eip;
        let start_cs_register = self.registers.cs();
        let start_cs = start_cs_register.selector;
        let profiling = self.profile.enabled;
        self.rep_execution.yielded = false;
        if !profiling {
            return match self
                .charge_cached_fetch_at(bus, lin, insn.len, view.phys_start)
                .and_then(|()| self.execute_hot_cached_or_decoded_budgeted(insn, bus, rep_budget))
            {
                Ok(outcome) if self.rep_execution.yielded => {
                    Ok(self.pause_rep_instruction(*insn, start_eip, start_cs_register, outcome))
                }
                Ok(outcome) => {
                    let charged = self.scale_clocks(outcome.core_clocks);
                    self.elapsed_clocks += charged;
                    self.perf.instructions += 1;
                    // A budgeted REP retires (does not yield) here; observe it once. The yielding
                    // arm above returns without retiring, so it is deliberately not observed.
                    #[cfg(feature = "jit")]
                    self.unit_sim_observe(
                        insn,
                        lin,
                        start_cs_register.default_size_32,
                        start_cs_register.base,
                    );
                    // V86 trap tax residency: the monitor's own straight-line
                    // instructions chain through this cached fast tail, not
                    // finish_instruction, so the residency attribution must
                    // live here too or the monitor body goes uncounted.
                    if self.is_ring0_protected() {
                        self.perf.monitor_resident_core_clocks += charged;
                    }
                    // Same reasoning as the residency counter above: this fast
                    // tail bypasses finish_instruction, so the diff-trace hook
                    // must be duplicated here or every cached-path instruction
                    // (the common case) goes untraced.
                    if diff_trace_enabled() {
                        self.emit_diff_trace_line(start_cs, start_eip);
                    }
                    Ok(CycleOutcome {
                        core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                        halted: outcome.halted,
                    })
                }
                Err(fault) => {
                    self.finish_instruction(bus, Err(fault), start_eip, start_cs, 0, None, None)
                }
            };
        }
        let profile_start = self.profile.sample_start();
        let result = self
            .charge_cached_fetch_at(bus, lin, insn.len, view.phys_start)
            .and_then(|()| self.execute_hot_cached_or_decoded_budgeted(insn, bus, rep_budget));
        if self.rep_execution.yielded {
            let outcome = result.expect("a faulting REP chunk cannot also yield");
            return Ok(self.pause_rep_instruction(*insn, start_eip, start_cs_register, outcome));
        }
        // Profiling path: past the yield check, an Ok result retires through finish_instruction;
        // observe the same Ok retirements so the count stays exact when profiling is enabled.
        #[cfg(feature = "jit")]
        if result.is_ok() {
            self.unit_sim_observe(
                insn,
                lin,
                start_cs_register.default_size_32,
                start_cs_register.base,
            );
        }
        self.finish_instruction(
            bus,
            result,
            start_eip,
            start_cs,
            0,
            profiling.then_some((
                insn.group,
                cpu_profile_opcode(insn),
                CpuProfileOperandForm::from_insn(insn),
            )),
            profile_start,
        )
    }

    fn execute_hot_cached_or_decoded_budgeted<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        rep_budget: RepBudget,
    ) -> ExecResult<CycleOutcome> {
        self.execute_hot_cached_or_decoded_inner(insn, bus, Some(rep_budget))
    }

    #[inline]
    #[cfg_attr(not(feature = "jit"), allow(dead_code))]
    pub(super) fn execute_hot_cached_or_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        self.execute_hot_cached_or_decoded_inner(insn, bus, None)
    }

    #[inline]
    fn execute_hot_cached_or_decoded_inner<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        rep_budget: Option<RepBudget>,
    ) -> ExecResult<CycleOutcome> {
        if let Some(outcome) = self.execute_hot_cached_decoded(insn) {
            return Ok(outcome);
        }
        match insn.group {
            DecodeGroup::Alu => {
                if let Some(outcome) = self.execute_hot_cached_alu_memory(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::DataMove => {
                if let Some(outcome) = self.execute_hot_cached_datamove(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::FlagsMisc => {
                if let Some(outcome) = self.execute_hot_cached_flags_misc(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::Group => {
                if let Some(outcome) = self.execute_hot_cached_group1_memory(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::Stack => {
                if let Some(outcome) = self.execute_hot_cached_stack(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::Branch => {
                if let Some(outcome) = self.execute_hot_cached_branch(insn, bus)? {
                    return Ok(outcome);
                }
            }
            _ => {}
        }
        match rep_budget {
            Some(rep_budget) => self.execute_decoded_with_rep_budget(insn, bus, Some(rep_budget)),
            None => self.execute_decoded(insn, bus),
        }
    }

    /// Hot cached-instruction subset that never touches the bus and cannot fault. EIP has already
    /// advanced past the instruction by `charge_cached_fetch`, matching the normal decoded executor.
    #[inline]
    fn execute_hot_cached_decoded(&mut self, insn: &DecodedInsn) -> Option<CycleOutcome> {
        if insn.opcode <= 0xff {
            let opcode = insn.opcode as u8;
            if opcode < 0x40 && (opcode & 0x07) < 6 {
                return self.execute_hot_cached_alu(insn, opcode);
            }
            if matches!(opcode, 0x80..=0x83) {
                return self.execute_hot_cached_group1(insn, opcode);
            }
            if matches!(opcode, 0xc0 | 0xc1 | 0xd0..=0xd3) {
                return self.execute_hot_cached_group2(insn, opcode);
            }
        }

        match insn.opcode {
            0x0fb6 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                self.write_gpr_sized(
                    modrm.reg,
                    insn.operand_size,
                    u32::from(self.read_gpr8(index)),
                );
                Some(clocks(3))
            }
            0x0fb7 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                self.write_gpr_sized(
                    modrm.reg,
                    insn.operand_size,
                    self.read_gpr_sized(index, OperandSize::Word),
                );
                Some(clocks(3))
            }
            0x0fbe => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let value = self.read_gpr8(index) as i8 as i32 as u32;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                Some(clocks(3))
            }
            0x0fbf => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let value = self.read_gpr_sized(index, OperandSize::Word) as i16 as i32 as u32;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                Some(clocks(3))
            }
            0x70..=0x7f | 0x0f80..=0x0f8f => {
                let cc = (insn.opcode & 0x0f) as u8;
                let taken = match cc {
                    0x4 => self.flag(FLAG_ZF),
                    0x5 => !self.flag(FLAG_ZF),
                    _ => self.condition(cc),
                };
                if taken {
                    self.relative_jump(insn.imm as i32, insn.operand_size);
                }
                Some(clocks(3))
            }
            opcode if opcode <= 0xff => match opcode as u8 {
                0x40..=0x4f => {
                    let index = insn.opcode as u8 & 0x07;
                    let value = self.read_gpr_sized(index, insn.operand_size);
                    let result = self.inc_dec(
                        value,
                        insn.opcode as u8 >= 0x48,
                        insn.operand_size.bus_width(),
                    );
                    self.write_gpr_sized(index, insn.operand_size, result);
                    Some(clocks(2))
                }
                0x84 => {
                    let modrm = insn.modrm?;
                    if modrm.mode == 3 && modrm.reg == modrm.rm {
                        let value = self.read_gpr8(modrm.rm);
                        self.alu_logic(u32::from(value), BusWidth::Byte);
                        return Some(clocks(2));
                    }
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    let value = self.read_gpr8(index);
                    let result = value & self.read_gpr8(modrm.reg);
                    self.alu_logic(u32::from(result), BusWidth::Byte);
                    Some(clocks(2))
                }
                0x85 => {
                    let modrm = insn.modrm?;
                    if modrm.mode == 3 && modrm.reg == modrm.rm {
                        let value = self.read_gpr_sized(modrm.rm, insn.operand_size);
                        self.alu_logic(value, insn.operand_size.bus_width());
                        return Some(clocks(2));
                    }
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    let value = self.read_gpr_sized(index, insn.operand_size);
                    let result = value & self.read_gpr_sized(modrm.reg, insn.operand_size);
                    self.alu_logic(result, insn.operand_size.bus_width());
                    Some(clocks(2))
                }
                0x88 => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    self.write_gpr8(index, self.read_gpr8(modrm.reg));
                    Some(clocks(2))
                }
                0x89 => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    if insn.operand_size == OperandSize::Word {
                        self.write_gpr16(index, self.read_gpr16(modrm.reg));
                    } else {
                        self.write_gpr32(index, self.read_gpr32(modrm.reg));
                    }
                    Some(clocks(2))
                }
                0x8a => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    self.write_gpr8(modrm.reg, self.read_gpr8(index));
                    Some(clocks(2))
                }
                0x8b => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    if insn.operand_size == OperandSize::Word {
                        self.write_gpr16(modrm.reg, self.read_gpr16(index));
                    } else {
                        self.write_gpr32(modrm.reg, self.read_gpr32(index));
                    }
                    Some(clocks(2))
                }
                0x8d => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Mem(addr) = insn.operand? else {
                        return None;
                    };
                    let memory = self.resolve_memory_addr_mode(&addr);
                    self.write_gpr_sized(modrm.reg, insn.operand_size, memory.offset);
                    Some(clocks(2))
                }
                0x90 => Some(clocks(3)),
                0x91..=0x97 => {
                    let reg = opcode as u8 & 0x07;
                    let acc = self.read_gpr_sized(0, insn.operand_size);
                    let other = self.read_gpr_sized(reg, insn.operand_size);
                    self.write_gpr_sized(0, insn.operand_size, other);
                    self.write_gpr_sized(reg, insn.operand_size, acc);
                    Some(clocks(3))
                }
                0xb0..=0xb7 => {
                    self.write_gpr8(insn.opcode as u8 - 0xb0, insn.imm as u8);
                    Some(clocks(2))
                }
                0xb8..=0xbf => {
                    self.write_gpr_sized(insn.opcode as u8 - 0xb8, insn.operand_size, insn.imm);
                    Some(clocks(2))
                }
                0xe0 | 0xe1 => {
                    let count_nonzero = match insn.address_size {
                        AddressSize::Word => {
                            let next = self.read_gpr16(1).wrapping_sub(1);
                            self.write_gpr16(1, next);
                            next != 0
                        }
                        AddressSize::Dword => {
                            let next = self.registers.ecx().wrapping_sub(1);
                            self.registers.set_ecx(next);
                            next != 0
                        }
                    };
                    let zf = self.flag(FLAG_ZF);
                    if count_nonzero && (if insn.opcode as u8 == 0xe1 { zf } else { !zf }) {
                        self.relative_jump(insn.imm as i32, insn.operand_size);
                    }
                    Some(clocks(11))
                }
                0xe2 => {
                    let taken = match insn.address_size {
                        AddressSize::Word => {
                            let next = self.read_gpr16(1).wrapping_sub(1);
                            self.write_gpr16(1, next);
                            next != 0
                        }
                        AddressSize::Dword => {
                            let next = self.registers.ecx().wrapping_sub(1);
                            self.registers.set_ecx(next);
                            next != 0
                        }
                    };
                    if taken {
                        self.relative_jump(insn.imm as i32, insn.operand_size);
                    }
                    Some(clocks(11))
                }
                0xe3 => {
                    let taken = match insn.address_size {
                        AddressSize::Word => self.read_gpr16(1) == 0,
                        AddressSize::Dword => self.registers.ecx() == 0,
                    };
                    if taken {
                        self.relative_jump(insn.imm as i32, insn.operand_size);
                    }
                    Some(clocks(9))
                }
                0xe9 | 0xeb => {
                    self.relative_jump(insn.imm as i32, insn.operand_size);
                    Some(clocks(7))
                }
                _ => None,
            },
            _ => None,
        }
    }

    #[inline]
    fn execute_hot_cached_alu(&mut self, insn: &DecodedInsn, opcode: u8) -> Option<CycleOutcome> {
        let op = (opcode >> 3) & 0x07;
        let form = opcode & 0x07;
        let write_back = op != 7;
        let operand_size = insn.operand_size;

        match form {
            0 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    u32::from(self.read_gpr8(index)),
                    u32::from(self.read_gpr8(modrm.reg)),
                    BusWidth::Byte,
                ) as u8;
                if write_back {
                    self.write_gpr8(index, result);
                }
            }
            1 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    self.read_gpr_sized(index, operand_size),
                    self.read_gpr_sized(modrm.reg, operand_size),
                    operand_size.bus_width(),
                );
                if write_back {
                    self.write_gpr_sized(index, operand_size, result);
                }
            }
            2 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    u32::from(self.read_gpr8(modrm.reg)),
                    u32::from(self.read_gpr8(index)),
                    BusWidth::Byte,
                ) as u8;
                if write_back {
                    self.write_gpr8(modrm.reg, result);
                }
            }
            3 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    self.read_gpr_sized(modrm.reg, operand_size),
                    self.read_gpr_sized(index, operand_size),
                    operand_size.bus_width(),
                );
                if write_back {
                    self.write_gpr_sized(modrm.reg, operand_size, result);
                }
            }
            4 => {
                let result =
                    self.alu(op, u32::from(self.read_gpr8(0)), insn.imm, BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(0, result);
                }
            }
            5 => {
                let result = self.alu(
                    op,
                    self.read_gpr_sized(0, operand_size),
                    insn.imm,
                    operand_size.bus_width(),
                );
                if write_back {
                    self.write_gpr_sized(0, operand_size, result);
                }
            }
            _ => return None,
        }

        Some(clocks(2))
    }

    #[inline]
    fn execute_hot_cached_alu_memory<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        if opcode >= 0x40 || (opcode & 0x07) >= 4 {
            return Ok(None);
        }
        let Some(modrm) = insn.modrm else {
            return Ok(None);
        };
        let Some(DecodedOperand::Mem(addr)) = insn.operand else {
            return Ok(None);
        };
        let memory = self.resolve_memory_addr_mode(&addr);

        let op = (opcode >> 3) & 0x07;
        let write_back = op != 7;
        match opcode & 0x07 {
            0 => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr8(modrm.reg);
                let result = self.alu(op, u32::from(value), u32::from(reg), BusWidth::Byte) as u8;
                if write_back {
                    self.write_memory_u8(
                        bus,
                        memory.segment,
                        memory.offset,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            1 => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr_sized(modrm.reg, insn.operand_size);
                let result = self.alu(op, value, reg, insn.operand_size.bus_width());
                if write_back {
                    self.write_memory_sized(
                        bus,
                        memory.segment,
                        memory.offset,
                        insn.operand_size,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            2 => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr8(modrm.reg);
                let result = self.alu(op, u32::from(reg), u32::from(value), BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(modrm.reg, result);
                }
                Ok(Some(clocks(2)))
            }
            3 => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr_sized(modrm.reg, insn.operand_size);
                let result = self.alu(op, reg, value, insn.operand_size.bus_width());
                if write_back {
                    self.write_gpr_sized(modrm.reg, insn.operand_size, result);
                }
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_group1(
        &mut self,
        insn: &DecodedInsn,
        opcode: u8,
    ) -> Option<CycleOutcome> {
        let modrm = insn.modrm?;
        let DecodedOperand::Reg(index) = insn.operand? else {
            return None;
        };

        match opcode {
            0x80 | 0x82 => {
                let result = self.alu(
                    modrm.reg,
                    u32::from(self.read_gpr8(index)),
                    insn.imm,
                    BusWidth::Byte,
                ) as u8;
                if modrm.reg != 7 {
                    self.write_gpr8(index, result);
                }
            }
            0x81 | 0x83 => {
                let result = self.alu(
                    modrm.reg,
                    self.read_gpr_sized(index, insn.operand_size),
                    insn.imm,
                    insn.operand_size.bus_width(),
                );
                if modrm.reg != 7 {
                    self.write_gpr_sized(index, insn.operand_size, result);
                }
            }
            _ => return None,
        }

        Some(clocks(2))
    }

    #[inline]
    fn execute_hot_cached_group2(
        &mut self,
        insn: &DecodedInsn,
        opcode: u8,
    ) -> Option<CycleOutcome> {
        let modrm = insn.modrm?;
        let DecodedOperand::Reg(index) = insn.operand? else {
            return None;
        };
        let count = match opcode {
            0xc0 | 0xc1 => insn.imm as u8,
            0xd0 | 0xd1 => 1,
            _ => (self.registers.ecx() & 0xff) as u8,
        };

        if opcode & 1 == 0 {
            let result = self.shift_rotate(
                modrm.reg,
                u32::from(self.read_gpr8(index)),
                count,
                BusWidth::Byte,
            ) as u8;
            self.write_gpr8(index, result);
        } else {
            let result = self.shift_rotate(
                modrm.reg,
                self.read_gpr_sized(index, insn.operand_size),
                count,
                insn.operand_size.bus_width(),
            );
            self.write_gpr_sized(index, insn.operand_size, result);
        }

        Some(clocks(2))
    }

    #[inline]
    fn execute_hot_cached_group1_memory<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        if !matches!(opcode, 0x80..=0x83) {
            return Ok(None);
        }
        let Some(modrm) = insn.modrm else {
            return Ok(None);
        };
        let Some(DecodedOperand::Mem(addr)) = insn.operand else {
            return Ok(None);
        };
        let memory = self.resolve_memory_addr_mode(&addr);

        match opcode {
            0x80 | 0x82 => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let result = self.alu(modrm.reg, u32::from(value), insn.imm, BusWidth::Byte) as u8;
                if modrm.reg != 7 {
                    self.write_memory_u8(
                        bus,
                        memory.segment,
                        memory.offset,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            0x81 | 0x83 => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let result = self.alu(modrm.reg, value, insn.imm, insn.operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_memory_sized(
                        bus,
                        memory.segment,
                        memory.offset,
                        insn.operand_size,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_datamove<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        if !matches!(opcode, 0x88..=0x8b) {
            return Ok(None);
        }
        let Some(modrm) = insn.modrm else {
            return Ok(None);
        };
        let Some(DecodedOperand::Mem(addr)) = insn.operand else {
            return Ok(None);
        };
        let memory = self.resolve_memory_addr_mode(&addr);

        match opcode {
            0x88 => {
                let value = self.read_gpr8(modrm.reg);
                self.write_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(Some(clocks(2)))
            }
            0x89 => {
                let value = self.read_gpr_sized(modrm.reg, insn.operand_size);
                self.write_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(Some(clocks(2)))
            }
            0x8a => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr8(modrm.reg, value);
                Ok(Some(clocks(2)))
            }
            0x8b => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_flags_misc<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        match insn.opcode as u8 {
            0x84 => {
                let Some(modrm) = insn.modrm else {
                    return Ok(None);
                };
                let Some(DecodedOperand::Mem(addr)) = insn.operand else {
                    return Ok(None);
                };
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr8(modrm.reg);
                self.alu(4, u32::from(value), u32::from(reg), BusWidth::Byte);
                Ok(Some(clocks(2)))
            }
            0x85 => {
                let Some(modrm) = insn.modrm else {
                    return Ok(None);
                };
                let Some(DecodedOperand::Mem(addr)) = insn.operand else {
                    return Ok(None);
                };
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr_sized(modrm.reg, insn.operand_size);
                self.alu(4, value, reg, insn.operand_size.bus_width());
                Ok(Some(clocks(2)))
            }
            0x98 => {
                match insn.operand_size {
                    OperandSize::Word => {
                        let ax = i16::from(self.read_gpr8(0) as i8) as u16;
                        self.write_gpr16(0, ax);
                    }
                    OperandSize::Dword => {
                        let eax = i32::from(self.read_gpr16(0) as i16) as u32;
                        self.write_gpr32(0, eax);
                    }
                }
                Ok(Some(clocks(3)))
            }
            0x99 => {
                match insn.operand_size {
                    OperandSize::Word => {
                        let dx = if (self.read_gpr16(0) as i16) < 0 {
                            0xffff
                        } else {
                            0
                        };
                        self.write_gpr16(2, dx);
                    }
                    OperandSize::Dword => {
                        let edx = if (self.read_gpr32(0) as i32) < 0 {
                            0xffff_ffff
                        } else {
                            0
                        };
                        self.write_gpr32(2, edx);
                    }
                }
                Ok(Some(clocks(2)))
            }
            0x9e => {
                self.materialize_flags();
                let ah = u32::from(self.read_gpr8(4));
                self.registers.eflags = (self.registers.eflags & !0xd5) | (ah & 0xd5) | 0x02;
                Ok(Some(clocks(3)))
            }
            0x9f => {
                self.materialize_flags();
                let ah = ((self.registers.eflags as u8) & 0xd5) | 0x02;
                self.write_gpr8(4, ah);
                Ok(Some(clocks(2)))
            }
            0xf5 => {
                self.set_flag(FLAG_CF, !self.flag(FLAG_CF));
                Ok(Some(clocks(2)))
            }
            0xf8 => {
                self.set_flag(FLAG_CF, false);
                Ok(Some(clocks(2)))
            }
            0xf9 => {
                self.set_flag(FLAG_CF, true);
                Ok(Some(clocks(2)))
            }
            0xfc => {
                self.set_flag(FLAG_DF, false);
                Ok(Some(clocks(2)))
            }
            0xfd => {
                self.set_flag(FLAG_DF, true);
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_stack<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        let operand_size = insn.operand_size;
        match opcode {
            0x50..=0x57 => {
                let value = self.read_gpr_sized(opcode - 0x50, operand_size);
                self.push(bus, value, operand_size)?;
                Ok(Some(clocks(2)))
            }
            0x58..=0x5f => {
                let value = self.pop(bus, operand_size)?;
                self.write_gpr_sized(opcode - 0x58, operand_size, value);
                Ok(Some(clocks(4)))
            }
            0x68 => {
                self.push(bus, insn.imm, operand_size)?;
                Ok(Some(clocks(2)))
            }
            0x6a => {
                self.push(bus, sign_extend_u8(insn.imm as u8), operand_size)?;
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_branch<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        if insn.opcode as u8 != 0xe8 {
            return Ok(None);
        }

        self.push(bus, self.registers.eip, insn.operand_size)?;
        self.relative_jump(insn.imm as i32, insn.operand_size);
        Ok(Some(clocks(7)))
    }
}
