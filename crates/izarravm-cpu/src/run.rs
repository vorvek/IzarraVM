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
fn diff_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_DIFF_TRACE").is_some())
}

/// The optional linear address forced JIT admission compiles
/// (`IZARRAVM_JIT_REGION=<hex>`, with or without `0x`). `None` leaves normal hotness admission in
/// control. Cached on first read, like `diff_trace_enabled`.
#[cfg(feature = "jit")]
fn jit_forced_region_lin() -> Option<u32> {
    static FORCED: OnceLock<Option<u32>> = OnceLock::new();
    *FORCED.get_or_init(|| {
        let value = std::env::var("IZARRAVM_JIT_REGION").ok()?;
        u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
    })
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

#[cfg(feature = "jit")]
enum DirectContinuation {
    Run(CycleOutcome),
    Prefix(CycleOutcome),
    Interpret,
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
        if !self.interrupt_shadow && self.flag(FLAG_IF) && bus.interrupt_pending() {
            if let Some(vector) = bus.acknowledge_interrupt() {
                // The interrupt frame sees the paused REP's start EIP. Once delivery begins, the
                // saved host continuation is stale; IRET restarts from guest code and refetches.
                self.rep_resume_active = false;
                self.rep_execution.resume = None;
                self.hardware_interrupt(bus, vector)
                    .map_err(|fault| match fault {
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
                    })?;
                let charged = self.scale_clocks(61);
                self.elapsed_clocks += charged;
                return Ok(Some(CycleOutcome {
                    core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                    halted: false,
                }));
            }
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
        if result.is_ok() {
            if let Some(insn) = &decoded {
                self.unit_sim_observe(
                    insn,
                    lin,
                    start_cs_register.default_size_32,
                    start_cs_register.base,
                );
            }
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
                self.set_eip(start_eip);
                if self.registers.cs().selector != start_cs {
                    self.load_segment_real(SegmentIndex::Cs, start_cs);
                }
                self.deliver_exception(bus, vector, error_code, false)
                    .map_err(|fault| match fault {
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
                    })?;
                CycleOutcome {
                    core_clocks: 59,
                    halted: false,
                }
            }
            Err(InternalFault::Cpu(error)) => return Err(error),
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
        #[cfg(feature = "jit")]
        let mut skip_direct_once = false;
        #[cfg(feature = "jit")]
        let forced_region_lin = jit_forced_region_lin();
        #[cfg(feature = "jit")]
        let native_continuations_active = {
            debug_assert_eq!(
                self.direct_runtime.admission_active,
                self.jit_direct.execution_enabled()
            );
            let legacy_requested = forced_region_lin.is_some()
                || self.jit_regions.auto_admit()
                || self.jit_regions.len() != 0;
            self.direct_runtime.admission_active
                || legacy_requested && self.jit_direct.backend_enabled()
        };
        // Guest-clock budget honesty: `cap` is a guest-clock budget (the machine
        // derives it from PIT-edge instants), but `total` counts core clocks
        // only. Track the batch's scaled-bus growth across this run so a
        // bus-heavy run (a framebuffer blit is several bus clocks per core
        // clock) ends at the budget instead of overshooting the next timer
        // edge by the bus:core ratio. Buses without this accounting return 0,
        // which degrades to the core-only comparison.
        let bus_at_entry = bus.in_batch_scaled_bus_clocks();
        let rep_budget = RepBudget { bus_at_entry, cap };
        self.perf.straight_line_runs += 1;
        loop {
            let can_take_before = self.can_take_interrupt();
            let outcome = if first {
                first = false;
                self.cycle_no_interrupt_check_with_budget(bus, Some(rep_budget))?
            } else {
                let lin = self.linear_eip();
                let cs = self.registers.cs();
                let insn = match self.decode_cache.get(lin, cs.default_size_32) {
                    Some(i) => {
                        if !i.continuable {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_not_continuable += 1;
                            break;
                        }
                        if (lin & 0xfff) + u32::from(i.len) > 0x1000 {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_page_cross += 1;
                            break;
                        }
                        if !Self::fetch_within_limit(self.registers.eip, i.len, cs.limit) {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_not_continuable += 1;
                            break;
                        }
                        i
                    }
                    None => {
                        self.perf.brk_decode_or_branch += 1;
                        self.perf.brk_cont_decode_miss += 1;
                        break;
                    }
                };
                // JIT admission: a compiled region stamped on this line runs natively instead of
                // the interpreted continuation, occupying one loop iteration; the loop's own
                // break checks below then fire at exactly the boundary the region stopped at.
                // The probe read+branch is the whole per-continuation dispatch cost.
                #[cfg(feature = "jit")]
                let region_outcome = if !native_continuations_active {
                    None
                } else {
                    let stamped_region = self.decode_cache.region_at(lin, cs.default_size_32);
                    let continuation_budget = ContinuationBudget {
                        total,
                        bus_at_entry,
                        cap,
                    };
                    if !self.jit_direct.backend_enabled() || std::mem::take(&mut skip_direct_once) {
                        None
                    } else if forced_region_lin == Some(lin)
                        || !self.jit_direct.auto_admit()
                            && (self.jit_regions.auto_admit() || stamped_region.is_some())
                    {
                        self.try_region_continuation(
                            bus,
                            lin,
                            cs.default_size_32,
                            stamped_region,
                            continuation_budget,
                        )?
                    } else if self.mode().uses_approximate_timing() {
                        match self.try_direct_continuation(
                            bus,
                            lin,
                            cs.default_size_32,
                            continuation_budget,
                        )? {
                            DirectContinuation::Run(outcome) => Some(outcome),
                            DirectContinuation::Prefix(outcome) => {
                                skip_direct_once = true;
                                Some(outcome)
                            }
                            DirectContinuation::Interpret => None,
                        }
                    } else {
                        None
                    }
                };
                #[cfg(not(feature = "jit"))]
                let region_outcome: Option<CycleOutcome> = None;
                match region_outcome {
                    Some(outcome) => outcome,
                    None => {
                        // A continuation skips cycle_no_interrupt_check (which resets this
                        // field to 0 for a fresh first instruction), so set it explicitly:
                        // total is exactly the prior instructions' charge in this run, not
                        // including the continuation about to execute.
                        self.core_clocks_so_far = total;
                        if insn.prefixes.rep.is_some() {
                            self.run_one_cached_budgeted(bus, &insn, lin, rep_budget)?
                        } else {
                            self.run_one_cached(bus, &insn, lin)?
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
                #[cfg(feature = "jit")]
                self.flush_direct_cache_stats();
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
            if total + (bus.in_batch_scaled_bus_clocks() - bus_at_entry) >= cap {
                self.perf.brk_cap += 1;
                break;
            }
        }
        #[cfg(feature = "jit")]
        self.flush_direct_cache_stats();
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
        self.unit_sim.0 = on.then(|| Box::new(jit::unit_sim::SimLadder::new()));
    }

    /// Take the unit-simulator ladder's per-rung reports, disabling the sim in the process. `None`
    /// when the sim was not enabled. Each element is `(cfg_label, headline, histogram)` for one
    /// ladder rung (`L0..L4`), where the histogram entries are `(member_count, entry_physical_page)`;
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
    #[cfg(feature = "jit")]
    #[inline]
    fn unit_sim_observe(&mut self, insn: &DecodedInsn, lin: u32, d: bool, cs_base: u32) {
        if self.unit_sim.0.is_none() {
            return;
        }
        // The retired instruction's decode line is live (we just fetched or cached it), so
        // `line_phys_start` is Some and carries the true physical page even under CR0.PG=0. Fall
        // back to the linear page only if the line went unexpectedly cold (an instruction that
        // patched its own decode line via SMC); documented and rare, never on the tested hot code.
        let physical_page = self
            .decode_cache
            .line_phys_start(lin, d)
            .map_or(lin >> 12, |phys| phys >> 12);
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
        };
        if let Some(sim) = self.unit_sim.0.as_mut() {
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
        self.jit_regions.set_auto_admit(false);
        self.jit_direct.set_auto_admit(on && jit::host_supported());
        self.finish_direct_execution_transition(was_enabled);
    }

    /// Enable or disable every native execution path, including forced legacy regions.
    /// Unsupported hosts cannot be enabled.
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
        if was_enabled == enabled {
            return;
        }
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.jit_fast_map.invalidate_all();
        self.jit_direct.invalidate_translation();
    }

    #[cfg(all(feature = "jit", test))]
    #[cfg_attr(
        not(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )),
        allow(dead_code)
    )]
    pub(crate) fn set_legacy_region_auto_admit(&mut self, on: bool) {
        self.set_jit_auto_admit(false);
        self.jit_regions.set_auto_admit(on && jit::host_supported());
    }

    #[cfg(feature = "jit")]
    fn try_direct_continuation<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
        d: bool,
        budget: ContinuationBudget,
    ) -> Result<DirectContinuation, CpuError> {
        if !self.mode().uses_approximate_timing() {
            return Ok(DirectContinuation::Interpret);
        }
        if !self.jit_direct.auto_admit() {
            return Ok(DirectContinuation::Interpret);
        }
        if !self
            .decode_cache
            .direct_hot(lin, d, self.jit_direct.admission_heat())
        {
            return Ok(DirectContinuation::Interpret);
        }
        let Some(key) = jit::direct::key_for(self, lin, d) else {
            return Ok(DirectContinuation::Interpret);
        };
        let probe = self.jit_direct.probe(key);
        let block = match probe {
            jit::direct::BlockProbe::Interpret => return Ok(DirectContinuation::Interpret),
            jit::direct::BlockProbe::Rejected => {
                return Ok(DirectContinuation::Interpret);
            }
            jit::direct::BlockProbe::Ready(id) => self
                .jit_direct
                .block(id)
                .expect("ready direct block must remain live"),
            jit::direct::BlockProbe::Compile => {
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
                        self.jit_direct.dormant(key);
                        return Ok(DirectContinuation::Interpret);
                    }
                };
                let code_page =
                    bus.direct_page(key.physical, BusAccessKind::InstructionPrefetch)?;
                let code_page_covers_block = code_page.is_some_and(|page| {
                    page.physical_page == key.physical & !0x0fff
                        && (key.physical & 0x0fff)
                            .checked_add(u32::from(compilation.span.guest_len))
                            .is_some_and(|end| end as usize <= page.len)
                });
                if !code_page_covers_block {
                    self.jit_direct.dormant(key);
                    return Ok(DirectContinuation::Interpret);
                }
                let Some(id) = self.jit_direct.install(&compilation) else {
                    self.jit_direct.dormant(key);
                    return Ok(DirectContinuation::Interpret);
                };
                self.perf.jit_direct_blocks_installed += 1;
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
            && !self.jit_direct.has_linked_successor(block)
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
        let _ = self.try_direct_continuation(
            bus,
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
        if !block.cs_descriptor_matches(self) {
            self.perf.jit_direct_reject_cs_layout += 1;
            self.jit_direct.retire_key_for_recompile(span.key);
            return Ok(DirectBlockOutcome::NotRun);
        }
        if block.memory_cpl3() != (self.current_privilege_level() == 3) {
            self.perf.jit_direct_reject_cpl += 1;
            self.jit_direct.retire_key_for_recompile(span.key);
            return Ok(DirectBlockOutcome::NotRun);
        }
        let has_link = self.jit_direct.has_linked_successor(block);
        let data_descriptors_match = if has_link {
            block.chain_descriptors_match(self)
        } else {
            block.data_descriptors_match(self)
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
        let fp_core_upper = u64::from(block.weighted_fp_clocks())
            .saturating_add(u64::from(FP_TIMING_DEN) - 1)
            / u64::from(FP_TIMING_DEN);
        let scaled_core_upper = u64::from(block.raw_clocks())
            .saturating_add(fp_core_upper)
            .saturating_mul(u64::from(num))
            .saturating_add(u64::from(den) - 1)
            / u64::from(den);
        let bus_growth = bus
            .in_batch_scaled_bus_clocks()
            .saturating_sub(bus_at_entry);
        let fetch_upper = bus
            .jit_fetch_cost_clocks()
            .saturating_mul(u64::from(span.instructions));
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
        let raw_bus_upper = fetch_upper.saturating_add(data_upper);
        // `cap` and `bus_growth` use the bus's scaled guest-clock domain. Fold the raw
        // fetch/data bound through that same scale before deciding how much native work fits.
        let iteration_upper =
            scaled_core_upper.saturating_add(bus.jit_scale_bus_cost_upper(raw_bus_upper));
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
                // x87-bearing blocks link only to other x87-bearing blocks. This keeps the common
                // integer chain bound tight while covering the 586 FISTP conversion surcharge.
                let unscaled_max_core = if block.has_x87() {
                    jit::direct::MAX_X87_BLOCK_CORE_CLOCKS
                } else {
                    // A block can contain 31 four-clock instructions followed by a ten-clock RET.
                    4u64.saturating_mul(jit::direct::MAX_BLOCK_INSTRUCTIONS as u64) + 6
                };
                let max_core = unscaled_max_core
                    .saturating_mul(u64::from(num))
                    .saturating_add(u64::from(den) - 1)
                    / u64::from(den);
                let max_read = bus
                    .jit_data_cost_clocks(BusWidth::Dword)
                    .max(bus.jit_mode13_data_cost_clocks(BusWidth::Dword))
                    .max(bus.jit_data_cost_clocks(BusWidth::Word))
                    .max(bus.jit_mode13_data_cost_clocks(BusWidth::Word))
                    .max(bus.jit_data_cost_clocks(BusWidth::Byte))
                    .max(bus.jit_mode13_data_cost_clocks(BusWidth::Byte));
                let max_store = max_read;
                let global_raw_bus_upper = (jit::direct::MAX_BLOCK_INSTRUCTIONS as u64)
                    .saturating_mul(
                        bus.jit_fetch_cost_clocks()
                            .saturating_add(max_read)
                            .saturating_add(max_store),
                    );
                let global_block_upper =
                    max_core.saturating_add(bus.jit_scale_bus_cost_upper(global_raw_bus_upper));
                let additional = available
                    .saturating_sub(iteration_upper)
                    .checked_div(global_block_upper)
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
        unsafe {
            entry(
                self as *mut CpuGsw,
                flags,
                quota as u32,
                &mut exit as *mut jit::direct::NativeExit,
            );
        }
        debug_assert!((exit.trace_len as usize) <= trace_capacity);
        debug_assert_eq!(exit.trace_len == 0, uniform_fetches);
        debug_assert!(u64::from(exit.linked_transfers) < quota);
        debug_assert!(exit.mode13_dirty_pages <= u64::from(u16::MAX));
        debug_assert!(exit.side_exit <= 1);
        debug_assert!(
            exit.side_exit != 0
                || exit.side_exit_reason == jit::direct::SideExitReason::None as u32
        );
        debug_assert!(exit.side_exit_reason <= jit::direct::SideExitReason::Other as u32);
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
        self.perf.jit_direct_linked_transfers += u64::from(exit.linked_transfers);
        match exit.unresolved_reason {
            jit::direct::UnresolvedReason::None => {}
            jit::direct::UnresolvedReason::StaticUnbound => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_static_unbound += 1;
            }
            jit::direct::UnresolvedReason::StaticHidden => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_static_hidden += 1;
            }
            jit::direct::UnresolvedReason::DynamicMissOrUnbound => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_dynamic_miss_or_unbound += 1;
            }
            jit::direct::UnresolvedReason::DynamicHidden => {
                self.perf.jit_direct_unresolved_exits += 1;
                self.perf.jit_direct_unresolved_dynamic_hidden += 1;
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
                reason if reason == jit::direct::SideExitReason::CodeWatch as u32 => {
                    self.perf.jit_direct_exit_code_watch += 1;
                }
                _ => self.perf.jit_direct_exit_other += 1,
            }
        }
        if self.is_ring0_protected() {
            self.perf.monitor_resident_core_clocks += charged;
        }
        self.flush_direct_cache_stats();
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
        Ok(!matches!(
            self.run_direct_block(bus, block, 0, bus_at_entry, cap)?,
            DirectBlockOutcome::NotRun
        ))
    }

    /// The JIT dispatch at the continuation seam: run the region stamped on this line, or (on the
    /// forced admission address, or once a line is hot enough) compile/re-stamp one first. `Ok(None)`
    /// means "no region ran"; the caller falls back to the interpreted continuation.
    #[cfg(feature = "jit")]
    fn try_region_continuation<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
        d: bool,
        stamped_region: Option<std::num::NonZeroU32>,
        budget: ContinuationBudget,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        let idx = match stamped_region {
            Some(idx) => idx,
            None => {
                // Admit when either the forced-address override names this line (the spike/test
                // path) or hotness admission is enabled and this line's miss counter just crossed
                // the threshold. Both are cheap: a compare and a counter bump. When neither fires,
                // this branch is the whole per-continuation dispatch cost on the miss path.
                let hot = self.jit_regions.auto_admit() && self.decode_cache.note_hot_miss(lin, d);
                let forced = jit_forced_region_lin() == Some(lin);
                if !forced && !hot {
                    return Ok(None);
                }
                // Hot linear blocks are admitted only when the builder finds a useful all-native
                // interior. Self-loops retain their existing gate, while forced admission remains
                // available for differential tests of any block shape.
                let Some(idx) = jit::block::try_admit_gated(self, lin, d, !forced) else {
                    return Ok(None);
                };
                self.decode_cache.stamp_region(lin, d, idx);
                idx
            }
        };
        self.run_region(
            bus,
            idx,
            lin,
            d,
            budget.total,
            budget.bus_at_entry,
            budget.cap,
        )
    }

    /// Execute a compiled region as one continuation of `run_straight_line`. On return the
    /// loop's own post-checks (halted, step break, interrupt transition, cap) re-fire at the
    /// exact boundary the region stopped at, so break attribution and batch semantics stay
    /// interpreter-identical. `Ok(None)` = an entry precondition failed, interpret instead.
    ///
    /// Deferred-at-exit accounting (one `scale_clocks` batch, `elapsed_clocks`,
    /// `perf.instructions`, ring-0 residency) is sound because no admitted block reads any of
    /// it mid-region (see the builder's invariants in `jit::block`) and the batch equals
    /// the per-instruction sums by the remainder-carry identity.
    #[cfg(feature = "jit")]
    #[allow(clippy::too_many_arguments)]
    fn run_region<B: CpuBus>(
        &mut self,
        bus: &mut B,
        idx: std::num::NonZeroU32,
        lin: u32,
        d: bool,
        total: u64,
        bus_at_entry: u64,
        cap: u64,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        // Preconditions the region cannot honor per instruction: profiling and diff-trace
        // sample every instruction, and a live STI shadow could make an interrupt newly
        // serviceable after the FIRST slot, a mid-region boundary the run loop cannot see.
        if self.profile.enabled || diff_trace_enabled() || self.interrupt_shadow {
            return Ok(None);
        }
        let eip = self.registers.eip;
        let cs_limit = self.registers.cs().limit;
        let epoch = self.decode_cache.jit_smc_epoch;
        let ring0 = self.is_ring0_protected();
        let mode_key = self.jit_mode_key();
        let (num, den) = level_timing(self.persona());
        let rem0 = self.timing_rem;
        // Native byte-memory helpers assume flat DS. Descriptor access rights are runtime values
        // not present in the mode key, so set their per-entry guards before borrowing the table.
        let ds_flat = self.jit_segment_flat(SegmentIndex::Ds);
        let ds_readable = self.jit_segment_readable(SegmentIndex::Ds);
        let ds_writable = self.jit_segment_writable(SegmentIndex::Ds);
        let step_fn = jit::step::region_step::<B> as jit::step::RegionStepFn;
        let (entry, ctx_ptr) = {
            let Some(region) = self.jit_regions.get_mut(idx) else {
                return Ok(None);
            };
            if region.entry_lin != lin || region.d != d {
                return Ok(None);
            }
            if region.mode_key != mode_key {
                // The same phys/d line is being entered in a different CPU mode/size than the block
                // was compiled for (real vs pmode vs V86, or a size/level change). The block key
                // includes the mode bitmask (spec §2.2), so this is a miss: drop the stamp and let
                // the forced-admission path re-build the block for the current mode.
                self.decode_cache.unstamp_region(lin, d);
                return Ok(None);
            }
            if region.valid_epoch != epoch {
                // A narrow SMC kill landed inside this region's span since the slots were last
                // built: the stamp may outlive the killed slot lines, so drop it and let the
                // forced-admission path re-run the builder over the fresh decodes.
                self.decode_cache.unstamp_region(lin, d);
                return Ok(None);
            }
            let ctx = &mut *region.ctx;
            // Every slot must pass the same live CS-limit check its interpreted continuation
            // would have (limits cannot change inside: no CS writer is admitted).
            for slot in &ctx.slots {
                let slot_eip = eip.wrapping_add(slot.lin.wrapping_sub(lin));
                if !Self::fetch_within_limit(slot_eip, slot.insn.len, cs_limit) {
                    return Ok(None);
                }
            }
            let can_native_load = region.has_native_load && ds_flat && ds_readable;
            let can_native_store = region.has_native_store && ds_flat && ds_writable;
            let native_u8_clock_bound = if cap == u64::MAX
                || (!can_native_load && !can_native_store)
            {
                Some(0)
            } else {
                (|| {
                    let mut fetch_max = 0;
                    for slot in &ctx.slots {
                        if matches!(
                            slot.kind,
                            jit::step::SlotKind::MemLoadU8 | jit::step::SlotKind::MemStoreU8
                        ) {
                            fetch_max = fetch_max.max(bus.jit_cached_fetch_run_clocks(
                                slot.physical,
                                u32::from(slot.insn.len),
                            )?);
                        }
                    }
                    let mut data_max = 0;
                    if can_native_load {
                        data_max = data_max.max(bus.jit_direct_memory_max_clocks(
                            BusWidth::Byte,
                            BusAccessKind::DataRead,
                        )?);
                    }
                    if can_native_store {
                        data_max = data_max.max(bus.jit_direct_memory_max_clocks(
                            BusWidth::Byte,
                            BusAccessKind::DataWrite,
                        )?);
                    }
                    let additional_bus = fetch_max.checked_add(data_max)?;
                    let bus_now = bus.in_batch_scaled_bus_clocks();
                    let bus_after = bus.jit_projected_batch_scaled_bus_clocks(additional_bus)?;
                    let bus_bound = bus_after.checked_sub(bus_now)?.saturating_add(1);
                    let num = u64::from(num);
                    let den = u64::from(den);
                    let core_bound = (2 * num).div_ceil(den);
                    core_bound.checked_add(bus_bound)
                })()
            };
            let native_memory_timing = native_u8_clock_bound.is_some();
            ctx.step_fn = Some(step_fn);
            ctx.inline_step_fn =
                Some(jit::step::region_inline_slot::<B> as jit::step::RegionStepFn);
            // Raw fn pointers to the flag helpers. The cast through `as` is sound: each helper is
            // `fn(&mut self, ...)` and we store it as `unsafe extern "C" fn(*mut CpuGsw, ...)`,
            // calling it with the cpu pointer the emitted code already holds; the `&mut` rebind
            // inside is the same disjoint-reborrow pattern region_step uses.
            ctx.set_pending_add_fn = Some(unsafe {
                std::mem::transmute::<fn(&mut CpuGsw, u32, u32), jit::step::SetPendingAddFn>(
                    Self::jit_set_pending_add as fn(&mut CpuGsw, u32, u32),
                )
            });
            ctx.set_shift_flags_fn = Some(unsafe {
                std::mem::transmute::<fn(&mut CpuGsw, u32, u8), jit::step::SetShiftFlagsFn>(
                    Self::jit_set_shift_flags_shr as fn(&mut CpuGsw, u32, u8),
                )
            });
            ctx.native_u8_fn = Some(jit::step::region_native_u8::<B> as jit::step::NativeU8Fn);
            ctx.native_group_guard_fn =
                Some(jit::step::region_native_group_guard::<B> as jit::step::NativeGroupGuardFn);
            ctx.native_group_finish_fn =
                Some(jit::step::region_native_group_finish::<B> as jit::step::NativeGroupFinishFn);
            ctx.entry_eip = eip;
            ctx.raw_clocks = 0;
            ctx.insn_count = 0;
            ctx.native_insn_count = 0;
            ctx.helper_exit_count = 0;
            ctx.native_memory_helper_count = 0;
            ctx.native_load_enabled = u32::from(native_memory_timing && can_native_load);
            ctx.native_store_enabled = u32::from(native_memory_timing && can_native_store);
            ctx.native_u8_clock_bound = native_u8_clock_bound.unwrap_or(0);
            ctx.run_total_at_entry = total;
            ctx.bus_at_run_start = bus_at_entry;
            ctx.cap = cap;
            ctx.rem0 = rem0;
            ctx.scale_num = num;
            ctx.scale_den = den;
            ctx.smc_epoch_at_entry = epoch;
            ctx.d = d;
            ctx.exit = jit::step::RegionExitKind::Boundary;
            ctx.fault = None;
            ctx.halted = false;
            (region.entry, std::ptr::from_mut(ctx))
        };
        let block_start = (self.perf.jit_region_entries & 0x3ff == 0).then(std::time::Instant::now);
        // SAFETY: the emitted code only forwards these pointers to `region_step::<B>`, whose
        // contract this call establishes: `self` and `bus` stay live `&mut` for the whole call
        // (no other reference to either exists here), `ctx` is the running region's boxed
        // mailbox (a separate allocation from `self`, so the step function's two reborrows are
        // disjoint; nothing reachable from the execute dispatch touches `jit_regions`), and `B`
        // is the concrete bus type behind the erased pointer.
        unsafe {
            (entry)(
                std::ptr::from_mut(self),
                (std::ptr::from_mut(bus)).cast(),
                ctx_ptr,
            );
        }
        let (raw, count, native_count, helper_exits, memory_helpers, halted, fault) = {
            let region = self
                .jit_regions
                .get_mut(idx)
                .expect("the region that just ran is still installed");
            let ctx = &mut *region.ctx;
            (
                ctx.raw_clocks,
                ctx.insn_count,
                ctx.native_insn_count,
                ctx.helper_exit_count,
                ctx.native_memory_helper_count,
                ctx.halted,
                ctx.fault.take(),
            )
        };
        if let Some(start) = block_start {
            self.perf.jit_native_block_ns += duration_ns_u64(start.elapsed());
            self.perf.jit_native_block_samples += 1;
        }
        let charged = self.scale_clocks_batch(raw);
        self.elapsed_clocks += charged;
        self.perf.instructions += u64::from(count);
        self.perf.jit_region_entries += 1;
        self.perf.jit_region_insns += u64::from(count);
        self.perf.jit_native_insns += u64::from(native_count);
        self.perf.jit_helper_exits += u64::from(helper_exits);
        self.perf.jit_native_memory_helpers += u64::from(memory_helpers);
        if ring0 {
            self.perf.monitor_resident_core_clocks += charged;
        }
        let mut out = charged;
        if let Some((start_eip, fault)) = fault {
            match fault {
                InternalFault::Cpu(error) => return Err(error),
                // finish_instruction's Exception arm, minus the CS restore (no admitted shape
                // can change CS mid-region): rewind to the faulting instruction, deliver, and
                // charge the interpreter's 59-clock delivery cost.
                InternalFault::Exception { vector, error_code } => {
                    self.set_eip(start_eip);
                    self.deliver_exception(bus, vector, error_code, false)
                        .map_err(|fault| match fault {
                            InternalFault::Cpu(error) => error,
                            InternalFault::Exception {
                                vector: nested_vector,
                                ..
                            } => CpuError::NestedFaultDuringDelivery {
                                original_vector: vector,
                                nested_vector,
                            },
                        })?;
                    let charged_fault = self.scale_clocks(59);
                    self.elapsed_clocks += charged_fault;
                    self.perf.instructions += 1;
                    if self.is_ring0_protected() {
                        self.perf.monitor_resident_core_clocks += charged_fault;
                    }
                    out += charged_fault;
                }
            }
        }
        Ok(Some(CycleOutcome {
            core_clocks: out.min(u64::from(u32::MAX)) as u32,
            halted,
        }))
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
        insn: &DecodedInsn,
        lin: u32,
    ) -> Result<CycleOutcome, CpuError> {
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
                .charge_cached_fetch(bus, lin, insn.len)
                .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus))
            {
                Ok(outcome) => {
                    let charged = self.scale_clocks(outcome.core_clocks);
                    self.elapsed_clocks += charged;
                    self.perf.instructions += 1;
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
            .charge_cached_fetch(bus, lin, insn.len)
            .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus));
        // Profiling path: finish_instruction retires (increments perf.instructions) on Ok; observe
        // the same Ok retirements here so the count stays exact when profiling is enabled.
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

    #[inline]
    fn run_one_cached_budgeted<B: CpuBus>(
        &mut self,
        bus: &mut B,
        insn: &DecodedInsn,
        lin: u32,
        rep_budget: RepBudget,
    ) -> Result<CycleOutcome, CpuError> {
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
                .charge_cached_fetch(bus, lin, insn.len)
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
            .charge_cached_fetch(bus, lin, insn.len)
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

    /// Specialized `mov r8, [mem]` (0x8A) execute for a JIT `MemLoadU8` slot: the exact body of
    /// `execute_hot_cached_datamove`'s 0x8A arm, reached WITHOUT the group/opcode dispatch chain
    /// (`execute_hot_cached_decoded`'s register-only probe + the group match + the datamove opcode
    /// match). Bit-identical to the interpreter by construction — it calls the same
    /// `resolve_memory_addr_mode` / `read_memory_u8` / `write_gpr8` and returns the same `clocks(2)` —
    /// so it inherits every segment/paging/fault/SMC/BusTrace behavior for free, in every CPU mode.
    /// Any shape the classifier did not intend (defensive) falls back to the full dispatch, so the
    /// result is identical regardless.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_load_u8<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x8a {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr8(modrm.reg, value);
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    /// Specialized `mov [mem], r8` (0x88) execute for a JIT `MemStoreU8` slot: the exact body of
    /// `execute_hot_cached_datamove`'s 0x88 arm, reached WITHOUT the group/opcode dispatch chain.
    /// Bit-identical to the interpreter by construction — same `resolve_memory_addr_mode` /
    /// `read_gpr8` / `write_memory_u8` / `clocks(2)`. In particular `write_memory_u8` runs
    /// `note_code_write` on every store, so the SMC code-write watch (Round 3 trap #2) is inherited
    /// for free, along with the segment/paging/fault/BusTrace behavior, in every CPU mode. Any shape
    /// the classifier did not intend falls back to the full dispatch, so the result is identical.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_store_u8<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x88 {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_gpr8(modrm.reg);
                self.write_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    /// Specialized `mov r16/r32, [mem]` (0x8B) execute for a JIT `MemLoadSized` slot: the exact body
    /// of `execute_hot_cached_datamove`'s 0x8B arm, reached WITHOUT the group/opcode dispatch chain.
    /// Bit-identical to the interpreter — same `resolve_memory_addr_mode` / `read_memory_sized`
    /// (which does the alignment/#AC, page-cross and segment/paging checks for the width) /
    /// `write_gpr_sized` / `clocks(2)`. `insn.operand_size` is the captured decode size (unprefixed in
    /// a region, so it is the segment default), so word and dword loads are both correct. Any
    /// unexpected shape falls back to the full dispatch.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_load_sized<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x8b {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    /// Specialized `mov [mem], r16/r32` (0x89) execute for a JIT `MemStoreSized` slot: the exact body
    /// of `execute_hot_cached_datamove`'s 0x89 arm, reached WITHOUT the group/opcode dispatch chain.
    /// Bit-identical — same `resolve_memory_addr_mode` / `read_gpr_sized` / `write_memory_sized`
    /// (which runs `note_code_write`, so the SMC watch and the alignment/page-cross/segment/paging
    /// checks are inherited) / `clocks(2)`, in every mode. Any unexpected shape falls back.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_store_sized<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x89 {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_gpr_sized(modrm.reg, insn.operand_size);
                self.write_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
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
