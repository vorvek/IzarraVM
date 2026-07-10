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
        self.interrupt_shadow = false;
        // This is always either a standalone single-step (no prior instructions in
        // "this run") or run_straight_line's FIRST instruction (total == 0 at that
        // point, by construction): both cases mean core_clocks_so_far is 0 here.
        // Continuations inside run_straight_line go through run_one_cached instead,
        // which does not reset this field; run_straight_line sets it explicitly
        // before each continuation call.
        self.core_clocks_so_far = 0;

        self.begin_instruction();
        let start_eip = self.registers.eip;
        let start_cs = self.registers.cs().selector;
        let lin = self.linear_eip();
        let profiling = self.profile.enabled;
        let profile_start = if profiling {
            self.profile.sample_start()
        } else {
            None
        };
        let mut profile_key = None;
        let result = match self.fetch_decoded(bus, lin) {
            Ok(insn) => {
                if profiling {
                    profile_key = Some((
                        insn.group,
                        insn.opcode,
                        CpuProfileOperandForm::from_insn(&insn),
                    ));
                }
                self.execute_decoded(&insn, bus)
            }
            Err(fault) => Err(fault),
        };
        self.finish_instruction(bus, result, start_eip, start_cs, profile_key, profile_start)
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
    pub(super) fn finish_instruction<B: CpuBus>(
        &mut self,
        bus: &mut B,
        result: ExecResult<CycleOutcome>,
        start_eip: u32,
        start_cs: u16,
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
            self.profile
                .record(group, opcode, form, charged, profile_start, lin);
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
    pub fn run_straight_line<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> Result<CycleOutcome, CpuError> {
        let mut total = 0u64;
        let mut first = true;
        // Guest-clock budget honesty: `cap` is a guest-clock budget (the machine
        // derives it from PIT-edge instants), but `total` counts core clocks
        // only. Track the batch's scaled-bus growth across this run so a
        // bus-heavy run (a framebuffer blit is several bus clocks per core
        // clock) ends at the budget instead of overshooting the next timer
        // edge by the bus:core ratio. Buses without this accounting return 0,
        // which degrades to the core-only comparison.
        let bus_at_entry = bus.in_batch_scaled_bus_clocks();
        self.perf.straight_line_runs += 1;
        loop {
            let can_take_before = self.can_take_interrupt();
            let outcome = if first {
                first = false;
                self.cycle_no_interrupt_check(bus)?
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
                let region_outcome = self.try_region_continuation(
                    bus,
                    lin,
                    cs.default_size_32,
                    total,
                    bus_at_entry,
                    cap,
                )?;
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
                        self.run_one_cached(bus, &insn, lin)?
                    }
                }
            };
            total += u64::from(outcome.core_clocks);
            // The post-instruction break checks run in the SAME ORDER the old per-instruction machine
            // loop used (halted -> step-break -> interrupt-transition -> cap), so the run ends at
            // exactly the boundary that loop would have stopped at.
            if outcome.halted {
                self.perf.brk_halt += 1;
                return Ok(CycleOutcome {
                    core_clocks: total.min(u64::from(u32::MAX)) as u32,
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
        Ok(CycleOutcome {
            core_clocks: total.min(u64::from(u32::MAX)) as u32,
            halted: false,
        })
    }

    /// Enable or disable hotness-driven JIT admission (feature `jit`). Unsupported hosts always
    /// keep it disabled. Independent of the forced-address override.
    /// Lives on the region table (a transparent accelerator excluded from CPU equality), so setting
    /// it never makes an otherwise-identical CPU compare unequal.
    #[cfg(feature = "jit")]
    pub fn set_jit_auto_admit(&mut self, on: bool) {
        self.jit_regions.set_auto_admit(on && jit::HOST_SUPPORTED);
    }

    /// Enable/disable the cost-fold native-LOAD path (env `IZARRAVM_JIT_FOLD`), a process-global toggle
    /// read at region emit time. Off by default so ordinary JIT admission and every bit-identical
    /// test are undisturbed. Associated (no `self`): it sets a global, like
    /// `NATIVE_BOOKKEEPING`. Turning it on makes JIT-block timing approximate (bus cost is folded), so
    /// it is validated by the anchor bands, not the differential timing asserts.
    #[cfg(feature = "jit")]
    pub fn set_jit_fold_timing(on: bool) {
        jit::block::FOLD_TIMING.store(on, std::sync::atomic::Ordering::Relaxed);
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
        total: u64,
        bus_at_entry: u64,
        cap: u64,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        let idx = match self.decode_cache.region_at(lin, d) {
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
                // Auto-admission (hotness) compiles ONLY self-loops: a linear block runs once per
                // entry then returns, so its region prologue/epilogue is pure overhead over the same
                // interpreted instructions and can never win (measured: broad linear-block admission
                // was a ~2.9x Doom wall regression). The forced-address override still admits any
                // block, for the spike/tests. Refusing is always state-correct.
                let Some(idx) = jit::block::try_admit_gated(self, lin, d, !forced) else {
                    return Ok(None);
                };
                self.decode_cache.stamp_region(lin, d, idx);
                idx
            }
        };
        self.run_region(bus, idx, lin, d, total, bus_at_entry, cap)
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
        // For a region with native cost-fold LOAD/STORE slots: DS flatness (and, for stores, DS
        // writability) is a runtime value NOT in the mode key, so re-check it here per entry (like the
        // per-slot CS-limit check below). Computed before the region borrow so `self` is free to read
        // the segment. Cheap; only consulted for regions that emitted a native probe.
        let ds_flat = self.jit_segment_flat(SegmentIndex::Ds);
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
            if region.has_native_fold && !ds_flat {
                // This region's native cost-fold LOAD/STORE slots assume DS is flat (EA == linear ==
                // physical). DS is no longer flat, so the emitted probe would compute the wrong
                // address. Bail to the interpreter (always correct); leave the stamp so a later entry
                // re-uses the region if DS becomes flat again (self-healing, no re-admit churn).
                return Ok(None);
            }
            if region.has_native_store && !ds_writable {
                // A native STORE slot assumes DS permits writes (a write-cache HIT only proves the
                // physical page was writable via some segment). DS is now read-only, so the native store
                // would silently write where the interpreter #GPs — bail to the interpreter, which
                // faults correctly. Self-healing like the flatness check.
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
            ctx.charge_fetch_fn =
                Some(jit::step::jit_charge_fetch::<B> as jit::step::ChargeFetchFn);
            ctx.bus_clocks_fn = Some(jit::step::jit_bus_clocks::<B> as jit::step::BusClocksFn);
            ctx.line_live_fn = Some(jit::step::jit_line_live as jit::step::LineLiveFn);
            ctx.store_finish_fn = Some(unsafe {
                std::mem::transmute::<fn(&mut CpuGsw, u32), jit::step::StoreFinishFn>(
                    Self::jit_store_u8_finish as fn(&mut CpuGsw, u32),
                )
            });
            ctx.entry_eip = eip;
            ctx.raw_clocks = 0;
            ctx.insn_count = 0;
            ctx.run_total_at_entry = total;
            ctx.bus_at_run_start = bus_at_entry;
            ctx.cap = cap;
            ctx.rem0 = rem0;
            ctx.scale_num = num;
            ctx.scale_den = den;
            ctx.d = d;
            ctx.exit = jit::step::RegionExitKind::Boundary;
            ctx.fault = None;
            ctx.halted = false;
            // Cost-fold: start the folded-bus accumulator empty; region_step flushes it. A native
            // MEMORY slot folds ONE instruction-fetch + ONE data-byte cost; a native ALU slot folds the
            // fetch only. Stash both constants from the concrete bus here (THE WRINKLE: the bus-agnostic
            // emitted buffer cannot call a bus method). Zero on buses without bus timing, so a native
            // slot folds nothing there.
            ctx.folded_raw_bus = 0;
            ctx.fetch_cost = bus.jit_fetch_cost_clocks();
            ctx.fold_bus_cost = ctx.fetch_cost + bus.jit_data_byte_cost_clocks();
            (region.entry, std::ptr::from_mut(ctx))
        };
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
        let (raw, count, halted, fault, folded_bus) = {
            let region = self
                .jit_regions
                .get_mut(idx)
                .expect("the region that just ran is still installed");
            let ctx = &mut *region.ctx;
            (
                ctx.raw_clocks,
                ctx.insn_count,
                ctx.halted,
                ctx.fault.take(),
                std::mem::take(&mut ctx.folded_raw_bus),
            )
        };
        // Flush any bus cost the tail native fold slots accumulated but did not hand to a region_step
        // slot: a region that exits directly from a native LOAD's line_live probe (or an inline slot's
        // cap check immediately after one) leaves the last fold unflushed. Bus-timing only (the core
        // term rides raw_clocks below), keeping the device-visible clock total current at region exit.
        if folded_bus > 0 {
            bus.charge_bus_clocks_bulk(folded_bus);
        }
        let charged = self.scale_clocks_batch(raw);
        self.elapsed_clocks += charged;
        self.perf.instructions += u64::from(count);
        self.perf.jit_region_entries += 1;
        self.perf.jit_region_insns += u64::from(count);
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
        let start_cs = self.registers.cs().selector;
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
                    self.finish_instruction(bus, Err(fault), start_eip, start_cs, None, None)
                }
            };
        }
        let profile_start = self.profile.sample_start();
        let result = self
            .charge_cached_fetch(bus, lin, insn.len)
            .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus));
        self.finish_instruction(
            bus,
            result,
            start_eip,
            start_cs,
            profiling.then_some((
                insn.group,
                insn.opcode,
                CpuProfileOperandForm::from_insn(insn),
            )),
            profile_start,
        )
    }

    #[inline]
    pub(super) fn execute_hot_cached_or_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
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
        self.execute_decoded(insn, bus)
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
