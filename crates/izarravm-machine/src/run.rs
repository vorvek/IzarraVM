// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
#[cfg(feature = "jit")]
use izarravm_cpu::{PollFamily, PollLoop};

pub(super) fn jit_auto_admit_policy(
    value: Option<&str>,
    jit_available: bool,
    backend: ExecutionBackend,
) -> bool {
    backend == ExecutionBackend::Automatic && jit_available && !matches!(value, Some("" | "0"))
}

pub(super) fn jit_auto_admit_default(backend: ExecutionBackend) -> bool {
    let value = std::env::var("IZARRAVM_JIT").ok();
    jit_auto_admit_policy(
        value.as_deref(),
        izarravm_cpu::native_backend_available(),
        backend,
    )
}

// Poll skipping defaults on for the interpreter backend; it is never engaged on any
// other backend regardless of the env var.
#[cfg(feature = "jit")]
pub(super) fn poll_skip_policy(value: Option<&str>, backend: ExecutionBackend) -> bool {
    backend == ExecutionBackend::Interpreter && poll_skip_requested(value)
}

// Default on: unset means enabled. "0" or empty explicitly disables it.
#[cfg(feature = "jit")]
fn poll_skip_requested(value: Option<&str>) -> bool {
    !matches!(value, Some("" | "0"))
}

#[cfg(feature = "jit")]
pub(super) fn poll_skip_default(backend: ExecutionBackend) -> bool {
    let value = std::env::var("IZARRAVM_POLL_SKIP").ok();
    poll_skip_policy(value.as_deref(), backend)
}

#[cfg(feature = "jit")]
#[derive(Debug, Default)]
pub(super) struct PollSkipDiagnostics {
    enabled: bool,
    policy_backend_rejections: u64,
    cpu_eligibility_rejections: u64,
    structural_hits_direct3: u64,
    structural_hits_setup_direct: u64,
    structural_hits_setup_paired: u64,
    source_port_mismatches: u64,
    vga_bus_certificate_rejections: u64,
    edge_cap_rejections: u64,
    committed_spans: u64,
    committed_iterations: u64,
    // Memory-family-only diagnostics (own certification and spin predicate,
    // no port/vega involvement; see try_poll_skip_memory).
    memory_structural_hits: u64,
    memory_translate_or_certificate_rejections: u64,
    memory_spin_rejections: u64,
    memory_cap_rejections: u64,
    #[cfg(test)]
    classifier_calls: u64,
    #[cfg(test)]
    classifier_ineligible_none: u64,
    #[cfg(test)]
    classifier_eligible_none: u64,
    #[cfg(test)]
    classifier_non_head: u64,
    #[cfg(test)]
    classifier_head: u64,
}

#[cfg(feature = "jit")]
impl PollSkipDiagnostics {
    pub(super) fn new(backend: ExecutionBackend) -> Self {
        let requested_value = std::env::var("IZARRAVM_POLL_SKIP").ok();
        let explicitly_requested = matches!(
            requested_value.as_deref(),
            Some(v) if !matches!(v, "" | "0")
        );
        let enabled = std::env::var("IZARRAVM_POLL_SKIP_DIAG")
            .ok()
            .is_some_and(|value| !matches!(value.as_str(), "" | "0"));
        let backend_rejected = explicitly_requested && backend != ExecutionBackend::Interpreter;
        if backend_rejected {
            eprintln!(
                "IZARRAVM_POLL_SKIP requested with a non-interpreter backend; poll skipping is disabled"
            );
        }
        Self {
            enabled,
            policy_backend_rejections: u64::from(backend_rejected),
            ..Self::default()
        }
    }

    fn increment(enabled: bool, counter: &mut u64) {
        if enabled {
            *counter = counter.saturating_add(1);
        }
    }

    fn cpu_eligibility_rejection(&mut self) {
        Self::increment(self.enabled, &mut self.cpu_eligibility_rejections);
    }

    fn structural_hit(&mut self, class: u8) {
        let counter = match class {
            0 => &mut self.structural_hits_direct3,
            1 => &mut self.structural_hits_setup_direct,
            2 => &mut self.structural_hits_setup_paired,
            _ => return,
        };
        Self::increment(self.enabled, counter);
    }

    fn source_port_mismatch(&mut self) {
        Self::increment(self.enabled, &mut self.source_port_mismatches);
    }

    fn vga_bus_certificate_rejection(&mut self) {
        Self::increment(self.enabled, &mut self.vga_bus_certificate_rejections);
    }

    fn edge_cap_rejection(&mut self) {
        Self::increment(self.enabled, &mut self.edge_cap_rejections);
    }

    fn committed(&mut self, iterations: u64) {
        if self.enabled {
            self.committed_spans = self.committed_spans.saturating_add(1);
            self.committed_iterations = self.committed_iterations.saturating_add(iterations);
        }
    }

    #[cold]
    #[inline(never)]
    fn memory_structural_hit(&mut self) {
        Self::increment(self.enabled, &mut self.memory_structural_hits);
    }

    #[cold]
    #[inline(never)]
    fn memory_translate_or_certificate_rejection(&mut self) {
        Self::increment(
            self.enabled,
            &mut self.memory_translate_or_certificate_rejections,
        );
    }

    #[cold]
    #[inline(never)]
    fn memory_spin_rejection(&mut self) {
        Self::increment(self.enabled, &mut self.memory_spin_rejections);
    }

    #[cold]
    #[inline(never)]
    fn memory_cap_rejection(&mut self) {
        Self::increment(self.enabled, &mut self.memory_cap_rejections);
    }

    #[cfg(test)]
    fn classifier_observation(&mut self, poll: Option<PollLoop>, eligible: bool) {
        self.classifier_calls = self.classifier_calls.saturating_add(1);
        let counter = match poll {
            None if eligible => &mut self.classifier_eligible_none,
            None => &mut self.classifier_ineligible_none,
            Some(poll) if poll.at_head() => &mut self.classifier_head,
            Some(_) => &mut self.classifier_non_head,
        };
        *counter = counter.saturating_add(1);
    }

    #[cfg(test)]
    pub(super) fn enable_for_test(&mut self) {
        self.enabled = true;
    }

    #[cfg(test)]
    pub(super) fn classifier_accounting(&self) -> (u64, [u64; 4]) {
        (
            self.classifier_calls,
            [
                self.classifier_ineligible_none,
                self.classifier_eligible_none,
                self.classifier_non_head,
                self.classifier_head,
            ],
        )
    }

    #[cfg(test)]
    pub(super) fn admission_accounting(&self) -> (u64, u64) {
        (
            self.cpu_eligibility_rejections,
            self.structural_hits_direct3
                .saturating_add(self.structural_hits_setup_direct)
                .saturating_add(self.structural_hits_setup_paired),
        )
    }
}

#[cfg(feature = "jit")]
impl Drop for PollSkipDiagnostics {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        eprintln!(
            "poll-skip diag: policy_backend_rejections={} cpu_eligibility_rejections={} structural_hits_direct3={} structural_hits_setup_direct={} structural_hits_setup_paired={} source_port_mismatches={} vga_bus_certificate_rejections={} edge_cap_rejections={} committed_spans={} committed_iterations={} memory_structural_hits={} memory_translate_or_certificate_rejections={} memory_spin_rejections={} memory_cap_rejections={}",
            self.policy_backend_rejections,
            self.cpu_eligibility_rejections,
            self.structural_hits_direct3,
            self.structural_hits_setup_direct,
            self.structural_hits_setup_paired,
            self.source_port_mismatches,
            self.vga_bus_certificate_rejections,
            self.edge_cap_rejections,
            self.committed_spans,
            self.committed_iterations,
            self.memory_structural_hits,
            self.memory_translate_or_certificate_rejections,
            self.memory_spin_rejections,
            self.memory_cap_rejections,
        );
    }
}

#[cfg(feature = "jit")]
pub(super) fn classify_poll_skip_boundary(
    cpu: &mut CpuGsw,
    diagnostics: &mut PollSkipDiagnostics,
) -> Option<PollLoop> {
    let poll = cpu.poll_loop();
    let eligible = poll.is_some() || cpu.poll_skip_eligible();
    #[cfg(test)]
    diagnostics.classifier_observation(poll, eligible);
    if poll.is_none() && !eligible {
        diagnostics.cpu_eligibility_rejection();
    }
    poll
}

#[cfg(feature = "jit")]
pub(super) fn try_poll_skip(
    cpu: &mut CpuGsw,
    bus: &mut MachineBus<'_>,
    diagnostics: &mut PollSkipDiagnostics,
    poll: PollLoop,
    batch_core: u32,
    cap: u64,
) -> Option<u32> {
    if !cpu.poll_skip_eligible() {
        diagnostics.cpu_eligibility_rejection();
        return None;
    }
    // Family dispatch (R4): the memory shape is a parallel executor with its
    // own certification, spin predicate, and cap-only binary search, no port
    // or vega calls. Everything below this branch is the io path, BYTE-
    // IDENTICAL to before the memory-poll shape existed.
    if poll.family() == PollFamily::Memory {
        return try_poll_skip_memory(cpu, bus, diagnostics, poll, batch_core, cap);
    }
    diagnostics.structural_hit(poll.diagnostic_class());
    if !poll.at_head() {
        return None;
    }
    if poll.resolved_port(cpu) != 0x03da {
        diagnostics.source_port_mismatch();
        return None;
    }
    if !bus.vega.poll_skip_status1_port_active() {
        diagnostics.vga_bus_certificate_rejection();
        return None;
    }
    let Some(certificate) = bus.poll_bus_certificate(poll) else {
        diagnostics.vga_bus_certificate_rejection();
        return None;
    };
    let beam = bus.predicted_beam();
    let status = bus.vega.status1_bits(beam);
    if !poll.fresh_iteration_spins(status) {
        diagnostics.edge_cap_rejection();
        return None;
    }
    let mask = poll.status_mask();
    let bit = mask.trailing_zeros() as u8;
    let current = status & mask != 0;
    let Some(edge_dots) = bus
        .vega
        .dots_until_status1_bit_change_from(beam, bit, !current)
    else {
        diagnostics.edge_cap_rejection();
        return None;
    };

    let current_bus = bus.poll_project_scaled_bus_clocks(certificate, 0)?;
    let spent = u64::from(batch_core).checked_add(current_bus)?;
    let upper = cap
        .checked_sub(spent)?
        .min(u64::from(u32::MAX))
        .saturating_sub(1);
    if upper < 2 {
        diagnostics.edge_cap_rejection();
        return None;
    }

    let admissible = |iterations: u64| -> bool {
        let Some(reserved) = iterations.checked_add(1) else {
            return false;
        };
        let Some(reserved_core) = cpu.project_poll_skip_core(poll, reserved) else {
            return false;
        };
        let Some(reserved_bus) = bus.poll_project_scaled_bus_clocks(certificate, reserved) else {
            return false;
        };
        let Some(reserved_total) = u64::from(batch_core)
            .checked_add(reserved_core)
            .and_then(|total| total.checked_add(reserved_bus))
        else {
            return false;
        };
        if reserved_total > cap {
            return false;
        }

        let Some(skipped_core) = cpu.project_poll_skip_core(poll, iterations) else {
            return false;
        };
        let Some(skipped_bus) = bus.poll_project_scaled_bus_clocks(certificate, iterations) else {
            return false;
        };
        let Some(candidate_total) = u64::from(batch_core)
            .checked_add(skipped_core)
            .and_then(|total| total.checked_add(skipped_bus))
        else {
            return false;
        };
        bus.poll_project_dot_advance(candidate_total)
            .is_some_and(|dots| dots < edge_dots)
    };

    let mut low = 2u64;
    let mut high = upper;
    let mut best = 0u64;
    while low <= high {
        let mid = low + (high - low) / 2;
        if admissible(mid) {
            best = mid;
            low = mid.saturating_add(1);
        } else {
            high = mid.saturating_sub(1);
        }
    }
    if best < 2 {
        diagnostics.edge_cap_rejection();
        return None;
    }

    let charged = cpu.project_poll_skip_core(poll, best)?;
    let charged_u32 = u32::try_from(charged).ok()?;
    bus.poll_project_scaled_bus_clocks(certificate, best)?;

    let committed = cpu
        .commit_poll_skip_core(poll, best)
        .expect("projected poll core commit must succeed");
    debug_assert_eq!(committed, charged);
    cpu.poll_skip_backedge_housekeeping();
    bus.poll_commit_bus(certificate, best);
    diagnostics.committed(best);
    Some(charged_u32)
}

/// The memory-family poll-skip executor (R4): certifies the polled cell's
/// data address through the real translation seam, checks the spin predicate
/// (R1) before committing anything, and bounds the skip by `cap` alone (no
/// device-specific edge exists for a plain-RAM cell: its only possible writer
/// is a device advance, and every device advance runs at batch end, after
/// `cap`; see the design doc's R3). No vega or port calls anywhere in this
/// function.
#[cfg(feature = "jit")]
#[cold]
#[inline(never)]
fn try_poll_skip_memory(
    cpu: &mut CpuGsw,
    bus: &mut MachineBus<'_>,
    diagnostics: &mut PollSkipDiagnostics,
    poll: PollLoop,
    batch_core: u32,
    cap: u64,
) -> Option<u32> {
    diagnostics.memory_structural_hit();
    if !poll.at_head() {
        return None;
    }
    let linear = poll.memory_cell_linear()?;
    let Some(physical) = cpu.probe_linear_read_physical(linear) else {
        diagnostics.memory_translate_or_certificate_rejection();
        return None;
    };
    let Some(certificate) = bus.poll_memory_bus_certificate(poll, physical) else {
        diagnostics.memory_translate_or_certificate_rejection();
        return None;
    };
    // R1: read the polled cell through the plain, uncharged backing-store
    // read (never CpuBus::read_memory/read_memory_direct/charge_direct_memory,
    // which all record trace clocks and would break timing identity), then
    // require the loop to actually be spinning before committing anything.
    // The read uses the A20-gated physical so it agrees with both the
    // certificate's checks and the interpreter's own access (identity today:
    // the M1 shape requires 32-bit code, where A20 is open in practice).
    let cell_value = bus.memory.read_u32(bus.apply_a20(physical) as usize).ok()?;
    let comparand = poll.memory_comparand(cpu)?;
    if !poll.memory_spin_predicate(cell_value, comparand)? {
        diagnostics.memory_spin_rejection();
        return None;
    }

    let current_bus = bus.poll_project_scaled_bus_clocks(certificate, 0)?;
    let spent = u64::from(batch_core).checked_add(current_bus)?;
    let upper = cap
        .checked_sub(spent)?
        .min(u64::from(u32::MAX))
        .saturating_sub(1);
    if upper < 2 {
        diagnostics.memory_cap_rejection();
        return None;
    }

    // Cap-only admissibility: the same one-iteration-headroom convention as
    // the io executor, minus the vretrace edge term (there is none for this
    // shape; see the design doc's "no new device query is needed" section).
    let admissible = |iterations: u64| -> bool {
        let Some(reserved) = iterations.checked_add(1) else {
            return false;
        };
        let Some(reserved_core) = cpu.project_poll_skip_core(poll, reserved) else {
            return false;
        };
        let Some(reserved_bus) = bus.poll_project_scaled_bus_clocks(certificate, reserved) else {
            return false;
        };
        let Some(reserved_total) = u64::from(batch_core)
            .checked_add(reserved_core)
            .and_then(|total| total.checked_add(reserved_bus))
        else {
            return false;
        };
        reserved_total <= cap
    };

    let mut low = 2u64;
    let mut high = upper;
    let mut best = 0u64;
    while low <= high {
        let mid = low + (high - low) / 2;
        if admissible(mid) {
            best = mid;
            low = mid.saturating_add(1);
        } else {
            high = mid.saturating_sub(1);
        }
    }
    if best < 2 {
        diagnostics.memory_cap_rejection();
        return None;
    }

    let charged = cpu.project_poll_skip_core(poll, best)?;
    let charged_u32 = u32::try_from(charged).ok()?;
    bus.poll_project_scaled_bus_clocks(certificate, best)?;

    let committed = cpu
        .commit_poll_skip_core(poll, best)
        .expect("projected poll core commit must succeed");
    debug_assert_eq!(committed, charged);
    cpu.poll_skip_backedge_housekeeping();
    bus.poll_commit_memory_bus(certificate, best);
    diagnostics.committed(best);
    Some(charged_u32)
}

impl Machine {
    /// Enable or disable the trace-driven unit-growth simulator on the CPU (feature `jit`,
    /// diagnostic). A no-op without feature `jit`. See `CpuGsw::set_unit_sim_enabled`.
    pub fn set_unit_sim_enabled(&mut self, on: bool) {
        #[cfg(feature = "jit")]
        self.cpu.set_unit_sim_enabled(on);
        #[cfg(not(feature = "jit"))]
        let _ = on;
    }

    /// Enable or disable the CPU's off-by-default SMC trace (diagnostic). See
    /// `CpuGsw::set_smc_trace_enabled`.
    pub fn set_smc_trace_enabled(&mut self, on: bool) {
        self.cpu.set_smc_trace_enabled(on);
    }

    /// Take the SMC trace's report lines, disabling the trace. `None` when it was never enabled.
    /// See `CpuGsw::take_smc_trace_report`.
    pub fn take_smc_trace_report(&mut self) -> Option<Vec<String>> {
        self.cpu.take_smc_trace_report()
    }

    /// Take the unit-simulator ladder's per-rung reports, disabling the sim in the process. Each
    /// element is `(cfg_label, headline, histogram)` for one ladder rung (the measurement set
    /// `{L0, L4, L6, P}`), where the histogram entries are `(member_count, entry_physical_page)`.
    /// `None` when the sim was not enabled. Only present with feature `jit`; see
    /// `CpuGsw::take_unit_sim_report`.
    #[cfg(feature = "jit")]
    #[allow(clippy::type_complexity)] // Signature fixed by the Track C task 3 reporting contract.
    pub fn take_unit_sim_report(
        &mut self,
    ) -> Option<Vec<(&'static str, izarravm_cpu::SimReport, Vec<(usize, u32)>)>> {
        self.cpu.take_unit_sim_report()
    }

    /// The per-port io-read histogram (behind `IZARRAVM_IO_HIST=1`), sorted by count descending.
    /// `None` without the histogram. Must be read before `take_unit_sim_report` (it borrows the sim);
    /// only present with feature `jit`. See `CpuGsw::unit_sim_io_hist`.
    #[cfg(feature = "jit")]
    pub fn unit_sim_io_hist(&self) -> Option<Vec<(u16, u64)>> {
        self.cpu.unit_sim_io_hist()
    }

    fn consume_pending_device_memory_write_range(&mut self) {
        if let Some((physical, width)) = self.pending_device_memory_write_range.take() {
            self.cpu.note_device_memory_write_range(physical, width);
        }
    }

    /// Preload the Neurketa benchmark selector the guest reads at start to pick
    /// its payload. Call before `run_until_halt_or_cycles`.
    pub fn set_bench_selector(&mut self, selector: u8) {
        self.unittester
            .set_reg_u8(unittester::REG_SELECTOR, selector);
    }

    /// The iteration count the Neurketa payload reported before `CMD_EXIT`.
    pub fn bench_iterations(&self) -> u32 {
        self.unittester.reg_u32(unittester::REG_RESULT_ITER)
    }

    /// The payload-specific auxiliary value (the Sieve reports its prime count).
    pub fn bench_aux(&self) -> u32 {
        self.unittester.reg_u32(unittester::REG_RESULT_AUX)
    }

    /// The payload status byte (1 once the payload ran to completion).
    pub fn bench_status(&self) -> u8 {
        self.unittester.reg_u8(unittester::REG_RESULT_STATUS)
    }

    /// Execute a unit-tester command deferred from a 0xE6 write. Returns the exit
    /// code for `CMD_EXIT` so the run loop can stop; `None` otherwise.
    fn perform_unittester(&mut self, cmd: u8) -> Option<u8> {
        match cmd {
            unittester::CMD_CRC => {
                let (x, y, w, h) = self.unittester.rect();
                let crc = self.screen_crc32(x, y, w, h);
                self.unittester.set_crc(crc);
                None
            }
            unittester::CMD_SNAPSHOT => {
                if let Some(path) = self.test_snapshot_path.clone()
                    && let Err(err) = self.write_snapshot_ppm(&path)
                {
                    eprintln!("unit tester: snapshot to {} failed: {err}", path.display());
                }
                None
            }
            unittester::CMD_EXIT => {
                // Diagnostic trace only (IZARRAVM_FAULT_TRACE=1): the Doom repro
                // needs to know whether the exit was a deliberate port write from
                // the running guest or a stray fetch. The run loop's OUT to 0xE6
                // always ends the batch before this deferred command executes
                // (write_io sets io_touched unconditionally), so CS:IP here is the
                // guest instruction right after the OUT, the closest reachable
                // point to the origin without threading CS:IP through CpuBus.
                if fault_trace_enabled() {
                    let cs = self.cpu.registers.cs().selector;
                    let eip = self.cpu.registers.eip;
                    eprintln!(
                        "fault trace: OUT 0xE6 CMD_EXIT val={cmd:#04x} \
                         next-guest-CS:IP={cs:#06x}:{eip:#010x} v86={} ring0={}",
                        self.cpu.is_v86_mode(),
                        self.cpu.is_ring0_protected(),
                    );
                }
                Some(self.unittester.exit_code())
            }
            _ => None, // unknown command: ignore, like an unused port write
        }
    }

    /// Log a fatal `CpuError` that stopped the run loop (env-gated, see
    /// `fault_trace_enabled`). Reports whatever CS:IP the CPU shows at the
    /// error site: for the V86-sensitive-op / selector-load faults this is the
    /// faulting guest instruction directly (the error is raised before any
    /// exception delivery runs), and for a fault raised while the TOKAEMM
    /// monitor is running ring-0 PM code it is the monitor's own CS:IP (the
    /// V86 guest CS:IP the monitor was servicing is on its stack, not
    /// reachable here without walking the ring-0 stack frame -- noted as the
    /// gap rather than adding a paging-aware stack walk to this trace).
    fn log_fault_trace(&mut self, error: &CpuError) {
        let cs = self.cpu.registers.cs().selector;
        let eip = self.cpu.registers.eip;
        let cs_base = self.cpu.registers.cs().base;
        eprintln!(
            "fault trace: {error} at CS:IP={cs:#06x}:{eip:#010x} v86={} ring0={}",
            self.cpu.is_v86_mode(),
            self.cpu.is_ring0_protected(),
        );
        eprintln!(
            "fault trace: CS base={cs_base:#010x} limit={:#010x} linear EIP={:#010x}",
            self.cpu.registers.cs().limit,
            cs_base.wrapping_add(eip),
        );
        let linear_eip = cs_base.wrapping_add(eip);
        let mut bytes_before = String::new();
        let start = linear_eip.saturating_sub(32);
        for addr in start..linear_eip {
            bytes_before.push_str(&format!("{:02x} ", self.read_physical_u8(addr)));
        }
        eprintln!(
            "fault trace: bytes before EIP [{start:#010x}..{linear_eip:#010x}): {bytes_before}"
        );
        let mut bytes_after = String::new();
        for addr in linear_eip..linear_eip.saturating_add(32) {
            bytes_after.push_str(&format!("{:02x} ", self.read_physical_u8(addr)));
        }
        eprintln!("fault trace: bytes at/after EIP [{linear_eip:#010x}..): {bytes_after}");
        // Dump the guest stack (128 bytes each direction) using SS base + ESP.
        let ss_base = self
            .cpu
            .registers
            .segment(izarravm_cpu::SegmentIndex::Ss)
            .base;
        let esp = self.cpu.registers.esp();
        let stack_linear = ss_base.wrapping_add(esp);
        let mut stack_before = String::new();
        let sb_start = stack_linear.saturating_sub(128);
        for addr in (sb_start..stack_linear).step_by(4) {
            stack_before.push_str(&format!("{:08x} ", self.read_physical_u32(addr)));
        }
        eprintln!(
            "fault trace: SS:ESP={:#06x}:{esp:#010x} linear={stack_linear:#010x}",
            self.cpu
                .registers
                .segment(izarravm_cpu::SegmentIndex::Ss)
                .selector
        );
        eprintln!("fault trace: stack before ESP: {stack_before}");
        let mut stack_after = String::new();
        for addr in (stack_linear..stack_linear.saturating_add(128)).step_by(4) {
            stack_after.push_str(&format!("{:08x} ", self.read_physical_u32(addr)));
        }
        eprintln!("fault trace: stack at/after ESP: {stack_after}");
    }

    pub fn run_cycles(&mut self, cycles: u64) -> Result<StopReason, MachineError> {
        let deadline_ticks = self
            .timeline
            .now_ticks()
            .saturating_add(self.timeline.master_ticks_for_cpu_clocks(cycles));
        self.run_until_tick(deadline_ticks, cycles)
    }

    /// Run against a fixed master-tick deadline. The CPU-clock count reported
    /// in `CycleLimit` is the causal quantum selected at this call boundary;
    /// live mode changes do not reinterpret the deadline.
    pub fn run_master_ticks(&mut self, master_ticks: u64) -> Result<StopReason, MachineError> {
        let requested = self.timeline.cpu_clocks_for_master_ticks_ceil(master_ticks);
        let deadline_ticks = self.timeline.now_ticks().saturating_add(master_ticks);
        self.run_until_tick(deadline_ticks, requested)
    }

    pub fn run_until_halt_or_cycles(
        &mut self,
        max_cycles: u64,
    ) -> Result<StopReason, MachineError> {
        let deadline_ticks = self
            .timeline
            .now_ticks()
            .saturating_add(self.timeline.master_ticks_for_cpu_clocks(max_cycles));
        self.run_until_tick(deadline_ticks, max_cycles)
    }

    fn run_until_tick(
        &mut self,
        deadline_ticks: u64,
        requested: u64,
    ) -> Result<StopReason, MachineError> {
        self.consume_pending_device_memory_write_range();
        if std::mem::take(&mut self.device_wrote_memory) {
            self.cpu.note_device_memory_write();
        }
        while self.timeline.now_ticks() < deadline_ticks {
            if self.direct_map_changed {
                self.cpu.note_direct_map_changed();
                self.direct_map_changed = false;
                self.direct_data_map_changed = false;
            } else if self.direct_data_map_changed {
                self.note_vga_wipe_apply();
                self.cpu.note_direct_data_map_changed();
                self.direct_data_map_changed = false;
            }
            // pending_soft_int is posted at a stub LANDING (V86 or real mode), so
            // for a monitor-reflected V86 INT it is set only after the monitor has
            // IRETed back into V86 with the real-mode frame in place, and serviced
            // at that same batch's end. The ring-0 guard is kept defensively: if a
            // pending vector ever survives into a ring-0 monitor batch (a landing
            // interrupted before its break), preserve it until V86 resumes.
            if !self.cpu.is_ring0_protected() {
                self.pending_soft_int = None;
            }
            self.io_touched = false;
            self.device_wrote_memory = false;
            let trace_before = self.trace.elapsed_clocks();
            // Capture live timing state before the fields move into MachineBus.
            let timeline_at_batch_start = self.timeline;
            let master_ticks_at_batch_start = self.timeline.now_ticks();
            let beam_at_batch_start = self.vega.beam_dots();
            let trace_elapsed_at_batch_start = trace_before;
            let bus_rem_at_batch_start = self.bus_rem;
            // bus_timing's (num, den), read from the same authoritative CPU mode
            // that scale_bus uses. Machine's active_mode copy exists for Lotura
            // register readback and is updated in the same set_mode call.
            let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(self.cpu.level());
            // Test seam: open this batch's per-run prior_runs_core_clocks push log.
            #[cfg(test)]
            self.test_prior_core_pushes.push(Vec::new());
            // A20 is a machine-layer event the CPU never sees directly, yet toggling it changes
            // which physical bytes back a linear address near the 1 MB wrap. Any A20 write (port
            // 0x92, the 8042, INT 15h, XMS) sets io_touched or is an HLE INT, so it ends this step;
            // a before/after compare here is the one seam that catches every source and lets the CPU
            // invalidate its prefetch + decode cache before the next batch runs.
            let a20_before = self.keyboard.a20_enabled();
            // Run a batch of straight-line instructions against one MachineBus,
            // then service devices once; a port access, an HLE INT, a HLT, or a
            // fault ends the batch sooner. This is the global-TSC / event-batched
            // model (research item 2.3): it drops the per-instruction bus rebuild
            // + 14-device fan-out that dominated the old loop.
            //
            // End every batch at the next known PIT, DSP, or WSS deadline. A
            // 1 ms fallback bounds the fast modes; a DAC-period fallback keeps
            // the 386 paths fine-grained. Either may be shortened by an earlier
            // event. Compute this once at batch entry because the run loop is
            // layout-sensitive.
            let remaining_ticks = deadline_ticks - self.timeline.now_ticks();
            let remaining = self
                .timeline
                .cpu_clocks_for_master_ticks_ceil(remaining_ticks)
                .max(1);
            let cap = self.event_batch_cap(remaining);
            #[cfg(feature = "jit")]
            let poll_skip_enabled = self.poll_skip_enabled;
            let cpu_batch_start = self.host_profile.start();
            let outcome = {
                let Machine {
                    profile,
                    active_mode,
                    pending_mode,
                    cpu,
                    cache_model,
                    memory,
                    ram_lookup,
                    vega,
                    rom,
                    serial,
                    serial2,
                    lpt,
                    lpt2,
                    device_ports,
                    pic,
                    pit,
                    keyboard,
                    gameport,
                    speaker,
                    rtc,
                    dma,
                    fdc,
                    opl,
                    sb16,
                    wavetable_mpu,
                    midi_mpu,
                    wss,
                    wss_base,
                    wss_enabled,
                    ide,
                    ata,
                    bmide,
                    trace,
                    pending_soft_int,
                    pending_bios32,
                    last_int_vector,
                    fast_post,
                    booter_inert,
                    program_runtime,
                    pending_toka_service,
                    toka_service_status,
                    unittester,
                    pci,
                    io_touched,
                    isa_io_batch_clocks,
                    device_wrote_memory,
                    pending_device_memory_write_range,
                    direct_map_changed,
                    direct_data_map_changed,
                    #[cfg(feature = "jit")]
                    poll_skip_diagnostics,
                    #[cfg(test)]
                    test_prior_core_pushes,
                    ..
                } = self;
                let mut bus = MachineBus {
                    memory,
                    ram_lookup,
                    vega,
                    pci,
                    rom,
                    serial,
                    serial2,
                    lpt,
                    lpt2,
                    device_ports,
                    pic,
                    pit,
                    keyboard,
                    gameport,
                    speaker,
                    rtc,
                    dma,
                    fdc,
                    opl,
                    sb16,
                    wavetable_mpu,
                    midi_mpu,
                    wss,
                    wss_base: *wss_base,
                    wss_enabled: *wss_enabled,
                    ide,
                    ata,
                    bmide,
                    trace,
                    pending_soft_int,
                    pending_bios32,
                    last_int_vector,
                    active_mode: *active_mode,
                    pending_mode,
                    fast_post: *fast_post,
                    booter_inert: *booter_inert,
                    program_runtime: *program_runtime,
                    pending_toka_service,
                    toka_service_status: *toka_service_status,
                    unittester,
                    wait_states: profile.wait_states,
                    cache: cache_model,
                    flat_data_cost: active_mode.uses_approximate_timing(),
                    lazy_port_reads: active_mode.uses_approximate_timing(),
                    io_touched,
                    isa_io_clocks: isa_io_batch_clocks,
                    device_wrote_memory,
                    pending_device_memory_write_range,
                    direct_map_changed,
                    direct_data_map_changed,
                    direct_mapping_epoch: &mut self.direct_mapping_epoch,
                    vga_wipe_census: &mut self.vga_wipe_census,
                    core_clocks_so_far: 0,
                    prior_runs_core_clocks: 0,
                    timeline_at_batch_start,
                    master_ticks_at_batch_start,
                    beam_at_batch_start,
                    trace_elapsed_at_batch_start,
                    bus_rem_at_batch_start,
                    bus_num_at_batch_start,
                    bus_den_at_batch_start,
                };
                // Collapse the batch into one CycleOutcome so every downstream
                // service step (device advance, CD stall, pending INT/mode/Toka/
                // unittester, console flush, HLT fast-forward) is unchanged:
                // core_clocks is the batch sum, halted is set iff the batch ended
                // on a HLT. core_clocks can't overflow u32 (the cap is at most
                // ~1 ms of guest clocks in the Approximate class, a few hundred
                // thousand at 586).
                let mut batch_core = 0u32;
                let mut halted = false;
                let mut fault = None;
                // Service a pending interrupt / halt-wake ONCE per batch.
                // interrupt_pending() cannot change mid-batch (devices advance only
                // after the batch, and any guest PIC access ends the batch via
                // io_touched), so a per-batch check is equivalent to the old
                // per-instruction one. The STI one-instruction shadow is still
                // honored per instruction inside cycle_no_interrupt_check.
                match cpu.service_pending_interrupt(&mut bus) {
                    Ok(Some(o)) => {
                        batch_core = batch_core.saturating_add(o.core_clocks);
                        if o.halted {
                            halted = true;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        fault = Some(e);
                    }
                }
                if fault.is_none() && !halted {
                    loop {
                        // Watch the "a maskable interrupt is now serviceable" edge
                        // (IF set AND no STI shadow pending). When an instruction
                        // raises it - POPF/IRET enabling IF, or the instruction after
                        // STI consuming the shadow - end the batch so the next batch
                        // entry re-checks interrupts at exactly that boundary. The
                        // interrupt-pending check is per-batch, not per-instruction, so
                        // without this an IF-enable whose window closes inside the same
                        // batch loses its pending interrupt. Two load-bearing cases:
                        // the HLE WaitForKey retry (the IRET stub restores IF, then the
                        // re-run INT 21h clears it again in the same batch, so IRQ1
                        // would never run), and an `STI; poll; jz` idle loop whose
                        // cap boundary always lands right after the STI (the shadow
                        // would block the per-batch check forever).
                        let can_take_before = cpu.can_take_interrupt();
                        // The batch cap's contract is GUEST clocks (its PIT terms
                        // are "clocks until the next OUT edge"), but core_clocks
                        // alone under-counts a bus-heavy stretch: a framebuffer
                        // blit can be several bus clocks per core clock, so a
                        // core-only cap overshoots the next IRQ0 edge by that
                        // ratio and the PIC coalesces the missed edges - a guest
                        // timer ISR then loses ticks that a real PIT delivers
                        // (each edge interrupts long before the next at any
                        // realistic rate). Count the in-batch SCALED bus clocks
                        // toward the cap in every mode. Check at loop top so an
                        // over-budget batch does not enter one more run.
                        let spent = u64::from(batch_core) + bus.in_batch_scaled_bus_clocks();
                        if spent >= cap {
                            break;
                        }
                        // Run a straight-line run of instructions inside the CPU in one call (the
                        // first via the normal single path, then cached straight-line continuations)
                        // instead of bouncing here per instruction. The run ends on a fault, halt, a
                        // non-straight-line / un-cached / page-crossing terminator, an interrupt-
                        // serviceable transition, or its cap. The batch-break checks below still run
                        // on the collapsed outcome: the executor's internal transition check ends the
                        // RUN at the edge, and the machine's check below ends the BATCH so the next
                        // batch services the interrupt. Both are needed.
                        #[cfg_attr(not(feature = "jit"), allow(unused_mut))]
                        let mut remaining = cap.saturating_sub(spent);
                        // Publish the batch-scoped core clocks accumulated so far
                        // (the interrupt-service charge + every prior run of this
                        // batch, exactly the core component the batch-end step
                        // will combine) so a lazy port-read prediction inside the
                        // coming run can add the RUN-scoped core_clocks_so_far on
                        // top and see a batch-total that is monotone across run
                        // boundaries. See MachineBus::prior_runs_core_clocks.
                        bus.prior_runs_core_clocks = u64::from(batch_core);
                        #[cfg(feature = "jit")]
                        let align_poll_head = if poll_skip_enabled {
                            // The CPU resets its run-scoped offset before the first
                            // real instruction. Poll projection happens before that
                            // public call, so canonicalize the matching bus scratch
                            // only inside the poll-skip-enabled path.
                            bus.core_clocks_so_far = 0;
                            let poll = classify_poll_skip_boundary(cpu, poll_skip_diagnostics);
                            let align = poll.is_some_and(|poll| !poll.at_head());
                            if !align
                                && let Some(poll) = poll
                                && let Some(skipped_core) = try_poll_skip(
                                    cpu,
                                    &mut bus,
                                    poll_skip_diagnostics,
                                    poll,
                                    batch_core,
                                    cap,
                                )
                            {
                                batch_core = batch_core
                                    .checked_add(skipped_core)
                                    .expect("poll projection bounded the batch core total");
                                bus.prior_runs_core_clocks = u64::from(batch_core);
                                let spent = u64::from(batch_core)
                                    .saturating_add(bus.in_batch_scaled_bus_clocks());
                                remaining = cap.saturating_sub(spent);
                            }
                            align
                        } else {
                            false
                        };
                        // Logs the bus field itself (not an independent `batch_core`
                        // read) so `batch_loop_publishes_prior_runs_core_clocks_before_every_run`
                        // actually fails if the store above is ever deleted or the
                        // publish drifts from the field a lazy prediction reads.
                        #[cfg(test)]
                        test_prior_core_pushes
                            .last_mut()
                            .expect("opened at batch entry")
                            .push(bus.prior_runs_core_clocks);
                        #[cfg(feature = "jit")]
                        let run_budget = if align_poll_head { 0 } else { remaining };
                        #[cfg(not(feature = "jit"))]
                        let run_budget = remaining;
                        match cpu.run_budgeted(&mut bus, run_budget) {
                            Ok(o) => {
                                batch_core = batch_core.saturating_add(o.consumed_core_clocks);
                                if o.halted {
                                    halted = true;
                                    break;
                                }
                                // A port access read or changed time-dependent device
                                // state; an HLE INT (pending_soft_int) needs &mut self.
                                // Stop so the run loop services them at this instant.
                                if *bus.io_touched || bus.pending_soft_int.is_some() {
                                    break;
                                }
                                if !can_take_before && cpu.can_take_interrupt() {
                                    break;
                                }
                                // A core-only fast exit avoids another loop when
                                // this run consumed the full budget. Bus-heavy
                                // runs are caught by the combined check above.
                                if u64::from(batch_core) >= cap {
                                    break;
                                }
                            }
                            Err(e) => {
                                fault = Some(e);
                                break;
                            }
                        }
                    }
                }
                match fault {
                    Some(e) => Err(e),
                    None => Ok(CycleOutcome {
                        core_clocks: batch_core,
                        halted,
                    }),
                }
            };
            self.consume_pending_device_memory_write_range();
            self.host_profile
                .record(MachineProfilePhaseKind::CpuBatch, cpu_batch_start);

            match outcome {
                Ok(outcome) => {
                    // Test seam: the final core total the batch-end step consumes,
                    // parallel to this batch's test_prior_core_pushes entry.
                    #[cfg(test)]
                    self.test_batch_core_totals
                        .push(u64::from(outcome.core_clocks));
                    let bus_clocks = self.trace.elapsed_clocks() - trace_before;
                    // Scale the bus portion per mode (B-T10). core_clocks is already
                    // scaled by the CPU's level_timing; this applies the third lever
                    // to the fetch + data-access bus clocks so a fast part pulls away
                    // from the flat per-access floor.
                    // ISA I/O bus time for the OPL status poll (Approximate class
                    // only), accumulated per access in read_io. The ISA bus runs at a
                    // fixed ~8 MHz, so an OPL status poll costs about a microsecond of
                    // wall time no matter how fast the CPU is.
                    // The per-mode bus scaler (scale_bus) instead prices the whole bus
                    // portion DOWN in the fast modes (586 x7/30), driving a port access
                    // toward zero guest-clocks, so a tight poll loop retires thousands
                    // of iterations per microsecond. That silently breaks the AdLib
                    // timer detection Doom runs before enabling FM music: the poll
                    // outruns the 80 us OPL timer, the overflow bit never appears, and
                    // music is disabled. Charging the real ISA period per poll lets the
                    // timer overflow within the poll. This is added OUTSIDE the
                    // io_touched batch-end gate on purpose: under TOKAEMM the poll runs
                    // in the V86 monitor (ring-0 PM), where the monitor's own device
                    // pokes are deliberately exempted from io_touched, so gating on it
                    // would miss exactly the case that fails. The Accurate class
                    // (386) never accumulates this (see read_io), so it stays
                    // byte-identical; its slower clock already spans the 80 us window.
                    let scaled_bus_clocks = self.scale_bus(bus_clocks);
                    let step = u64::from(outcome.core_clocks)
                        + scaled_bus_clocks
                        + std::mem::take(&mut self.isa_io_batch_clocks);
                    self.scaled_bus_clocks =
                        self.scaled_bus_clocks.saturating_add(scaled_bus_clocks);
                    // Advance the OPL timers so AdLib detection's delay loops see
                    // the overflow flag (the synthesis clock is driven separately
                    // by `render_audio`).
                    let advance_start = self.host_profile.start();
                    self.advance_cpu_work(step, u64::from(outcome.core_clocks));
                    self.host_profile
                        .record(MachineProfilePhaseKind::AdvanceDevices, advance_start);
                    let service_start = self.host_profile.start();
                    let mut serviced = false;
                    let mut service_stop = None;
                    if let Some(mode) = self.pending_mode.take() {
                        serviced = true;
                        self.set_mode(mode); // live Lotura switch takes effect next instruction
                    }
                    if let Some(cmd) = self.pending_toka_service.take() {
                        serviced = true;
                        self.perform_toka_service(cmd); // Repair (cmd 0x01)
                    }
                    if let Some(cmd) = self.unittester.take_pending() {
                        serviced = true;
                        if let Some(code) = self.perform_unittester(cmd) {
                            service_stop = Some(StopReason::TestExit { code });
                        }
                    }
                    if let Some(call) = self.pending_bios32.take() {
                        serviced = true;
                        match call {
                            Bios32Call::Directory => self.handle_bios32_directory(),
                            Bios32Call::Pci => self.handle_pci_bios(true),
                        }
                    }
                    // A software INT taken by a V86 guest faults to the TOKAEMM monitor
                    // (ring-0 PM) before its frame is reflected onto the guest stack. The
                    // HLE BIOS services assume that real-mode-style frame at SS:SP+4 (see
                    // `set_int_frame_carry`), so defer them while the monitor runs; they
                    // fire once it IRETs back into V86 with the frame in place.
                    if service_stop.is_none()
                        && !self.cpu.is_ring0_protected()
                        && let Some(vector) = self.pending_soft_int
                    {
                        serviced = true;
                        match vector {
                            0x10 | 0x42 => self.handle_int10(),
                            0x11 => self.handle_int11(),
                            0x12 => self.handle_int12(),
                            0x13 | 0x40 => self.handle_int13(),
                            0x14 => self.handle_int14(),
                            0x15 => self.handle_int15(),
                            0x17 => self.handle_int17(),
                            0x18 => self.handle_int18(),
                            0x19 => self.handle_int19(),
                            0x1A => self.handle_int1a(),
                            0x5C => self.handle_absent_resident_api(0x5C),
                            0x60 => self.handle_absent_resident_api(0x60),
                            0x68 => self.handle_absent_resident_api(0x68),
                            0x6F => self.handle_absent_resident_api(0x6F),
                            0x7A => self.handle_absent_resident_api(0x7A),
                            0x86 => self.handle_absent_resident_api(0x86),
                            0xE4 => self.handle_absent_resident_api(0xE4),
                            0x2F => {
                                self.handle_int2f();
                            }
                            0x20 | 0x21 | 0x27 if self.program_runtime => {
                                match self.handle_raw_program_int(vector) {
                                    Ok(Some(code)) => {
                                        service_stop = Some(StopReason::DosExit { code });
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        service_stop = Some(StopReason::CpuError(format!(
                                            "raw program INT {vector:#04x}: {error}"
                                        )));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if serviced {
                        self.host_profile
                            .record(MachineProfilePhaseKind::SoftInt, service_start);
                    }
                    if let Some(stop) = service_stop {
                        return Ok(stop);
                    }
                    // Mirror any DOS console output onto the VGA text screen.
                    let console_start = self.host_profile.start();
                    self.flush_dos_console_to_screen();
                    self.host_profile
                        .record(MachineProfilePhaseKind::ConsoleFlush, console_start);
                    if outcome.halted {
                        let halt_start = self.host_profile.start();
                        match self.next_timer_wake(deadline_ticks) {
                            Some(wake_step) => {
                                self.advance_halted_cpu_clocks(wake_step);
                            }
                            None => {
                                let remaining = deadline_ticks - self.timeline.now_ticks();
                                if let Some(ticks) = self.next_timed_io_deadline() {
                                    self.advance_halted_ticks(ticks.min(remaining));
                                } else {
                                    self.host_profile.record(
                                        MachineProfilePhaseKind::HaltFastForward,
                                        halt_start,
                                    );
                                    return Ok(StopReason::Halted);
                                }
                            }
                        }
                        self.host_profile
                            .record(MachineProfilePhaseKind::HaltFastForward, halt_start);
                    }
                    // The A20 gate toggled during this step (port 0x92, the 8042, INT 15h, or XMS):
                    // tell the CPU so it drops any prefetch/decoded bytes that A20 now remaps near
                    // the 1 MB wrap, before the next batch executes against the new gate state.
                    if self.keyboard.a20_enabled() != a20_before {
                        self.cpu.note_a20_changed();
                    }
                    // A bus-side DMA copy without a reported destination range wrote guest RAM.
                    // Range-aware HLE, floppy, and bus-master IDE paths notify the CPU directly.
                    if std::mem::take(&mut self.device_wrote_memory) {
                        self.cpu.note_device_memory_write();
                    }
                    if self.direct_map_changed {
                        self.cpu.note_direct_map_changed();
                        self.direct_map_changed = false;
                        self.direct_data_map_changed = false;
                    } else if self.direct_data_map_changed {
                        self.note_vga_wipe_apply();
                        self.cpu.note_direct_data_map_changed();
                        self.direct_data_map_changed = false;
                    }
                }
                Err(error) => {
                    if fault_trace_enabled() {
                        self.log_fault_trace(&error);
                    }
                    return Ok(StopReason::CpuError(error.to_string()));
                }
            }
        }

        Ok(StopReason::CycleLimit { requested })
    }
}
