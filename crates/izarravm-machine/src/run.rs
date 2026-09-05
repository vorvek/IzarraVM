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

/// Whether the device-armed ATA/ATAPI clock skip is engaged.
///
/// **DEFAULT ON SINCE THE 2026-08-21 FLIP.** Unset admits the skip; `0` / `off`
/// is the escape, the pre-slice behaviour, and the A/B base every measurement
/// here was read against. Landed default-off first and flipped in its own
/// commit, exactly as `IZARRAVM_FPU_LOOP_ROWS` (`bc55e87b` then `a0d2123f`),
/// `IZARRAVM_COUNT_LANES` and `IZARRAVM_V86_LOOP_ROWS` (`f844134b`) did.
///
/// Deliberately NOT named `IZARRAVM_POLL_SKIP_ATA`: this is a different
/// mechanism from `IZARRAVM_POLL_SKIP` (a device-armed clock skip, not an
/// instruction-eliding shape skip) and must not read as a sub-flag of it.
///
/// THE SPELLING TABLE, trimmed and case-folded on the way in, matching the lane
/// family exactly:
///
/// * **unset** or `1` / `on` -> ON. The shipped default. **Every OFF leg must
///   now EXPORT `0`** -- the same trap `ROTATE_ROWS`, `COUNT_LANES`,
///   `FPU_LOOP_ROWS` and `V86_LOOP_ROWS` all carry, and any leg recorded before
///   this flip that merely left the variable alone is an OFF leg.
/// * `` (empty), `0` or `off` -> OFF. The escape and the A/B base. **Empty is
///   spelled OFF here on purpose**, because OFF is a real arm for this gate:
///   nulling a variable in PowerShell leaves it PRESENT AND EMPTY, which is how
///   three earlier evidence directories came to measure their default-on knobs
///   off. (The numeric sweep knobs below spell empty as THE DEFAULT instead,
///   because a threshold has no "off" value -- see `sweep_knob`.)
/// * **anything else PANICS.** A mistyped ladder leg that fell through to the
///   default would be read as "the arm I asked for changed nothing".
///
/// WHAT PRICED THE FLIP, in the order the evidence was taken
/// (`.bench/results/atapi-poll-skip-20260821/`):
///
/// * **tombraid-586 wall ladder**, one binary, A B B A A B, full 28e9 row:
///   **-29.33% min-wall** (121.287 s against 171.618 s), arms fully
///   non-overlapping with B's worst leg 49.4 s faster than A's best. Row rt
///   0.9843 -> 1.3938, dispatcher entries 786.6M -> 341.4M, retired
///   instructions 19.49G -> 16.82G. A second, earlier ladder on the
///   pre-mitigation binary read -28.55% with **byte-identical counters**, which
///   is what proves the interactive mitigation inert on the headless path.
/// * **Mechanism assertions**, which are what actually carry the acceptance --
///   the wall numbers corroborate them, not the other way round.
///   `spans / atapi_packet_commands` = **2.9273** against the 3 that TOKACD's
///   three separately-scheduled per-sector poll phases predict, so all three
///   clear the 20 us floor; `ticks` / modelled ATAPI wait = **0.807**, a lower
///   bound since `media_delay` is skipped too; `io_stall_ticks` moves by
///   **exactly** `ata_poll_skip.ticks`; `halted_ticks` identical on both arms;
///   `cd_pio_bytes` **byte-identical** (22,310,936) on all twelve ladder legs
///   across both ladders, and `atapi_packet_commands` identical at 14,135.
/// * **Windows**: the load window (guest 142-145) -68.6% wall and -89.3%
///   dispatcher entries, the FMV window (guest 4-124) -33.0% wall.
/// * **The interactive path**, which no headless leg can grade. A windowed run
///   measured the GUI slice at a ~13 us median -- not the 1 ms the design
///   assumed, because `execution_budget` is `min(credit, quantum)` and the
///   credit binds in a paced window -- so most interactive slices sit below the
///   floor. Before the batch-entry slice test the skip armed 538,305 times to
///   commit 170,564 spans, with 364,753 clamped declines; after it, 169,467 arms
///   for 167,806 spans and **385** clamped declines, with the skipped guest time
///   unchanged. Crucially `ata_poll_skip_blocks` never tracked the clamped
///   cause -- 2,988 blocks against 364,753 clamped declines, and blocks equals
///   `declines_below_floor` exactly -- which is the R2-B split holding under a
///   load no fixture could reproduce.
/// * **Board, gate ON, main's scoreboard against this branch's binary**: 11 of
///   12 passed before the pin move, the twelfth being the retired-instruction
///   pin this flip moves. All eleven non-CD rows read `ata_poll_skip.arms == 0`
///   with the gate ARMED -- inert because nothing arms, not because nothing ran.
///   The board at the OFF default was 12/12 with all nine counters zero.
/// * **The 0.5G exact-frame anchor is byte-identical with the skip active**, and
///   that is counted rather than argued: the anchor run commits two spans
///   (146 us) and returns the pinned hash on both arms.
///
/// THE PIN THIS FLIP MOVES, and where it physically lives. `tombraid-586`'s
/// `final_instructions` centre goes 19,491,752,775 -> **16,822,094,052**,
/// tolerance untouched at 5%, because the lever cuts retired instructions 13.696%
/// BY CONSTRUCTION -- the elided poll iterations are not executed. The centre
/// moved, never the band. **That edit is NOT in this repository's history and
/// cannot be**: `scripts/fixture-scoreboard-invariants.json` is in
/// `.git/info/exclude` and exists only in the main checkout's working tree, so
/// there is exactly one pin set on the box and a worktree cannot carry its own.
/// It was edited in main's checkout by the coordinator; git log will never show
/// it. The compensating pins that now carry the weight, all checked rather than
/// only the ones that pass: the 0.5G anchor, the non-black coverage and
/// distinct-colour bands, the display class, the `cycle_limit` stop,
/// `cd_pio_bytes` byte-identity whole-run, and `io_stall_ticks` moving by the
/// predicted amount.
pub(super) fn ata_poll_skip_default() -> bool {
    parse_ata_poll_skip_arm(std::env::var("IZARRAVM_ATA_POLL_SKIP"))
}

fn parse_ata_poll_skip_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic
        // as a typo rather than the same silence as "unset": someone set the
        // variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_ATA_POLL_SKIP is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default since 2026-08-21: the \
                 device-armed ATA/ATAPI clock skip) and `0` / `off` (the escape, under which \
                 the skip stays disarmed)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_ATA_POLL_SKIP={other:?} names no arm; accepted spellings are unset or \
             `1` / `on` (the shipped default since 2026-08-21: the device-armed ATA/ATAPI \
             clock skip) and `0` / `off` (the escape, under which the skip stays disarmed). \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be \
             read as the arm it named doing nothing"
        ),
    }
}

/// A sweep knob's value, or a PANIC.
///
/// **THE KNOBS' FIRST REAL USE WILL BE A SWEEP, WHICH IS EXACTLY THE RUN A
/// SILENT FALLBACK POISONS.** These two started life as
/// `.ok().and_then(|r| r.parse().ok())`, so `IZARRAVM_ATA_POLL_RUN=sixteen` ran
/// 16 without a word -- while the main gate one screen up panics on
/// `IZARRAVM_ATA_POLL_SKIP=yes` for the stated reason that "a mistyped ladder
/// leg that fell through to the default would be read as 'the arm I asked for
/// changed nothing'". A mistyped sweep leg is the same trap wearing the same
/// clothes: it reads as "that threshold changed nothing".
///
/// **UNSET AND EMPTY BOTH MEAN "THE DEFAULT", and only those two are silent.**
///
/// The empty string has to be a spelling of unset here, and this is not a
/// convenience: `[Environment]::SetEnvironmentVariable($name, $null, "Process")`
/// — how every harness in this repo clears a variable it does not want — leaves
/// the variable PRESENT AND EMPTY on Windows. That is the same
/// nulling-is-not-unsetting trap that made three earlier evidence directories
/// measure their default-ON knobs off, and it bit this branch's own re-ladder
/// script on the first run after these panics landed: six legs died in 60 ms
/// each on `IZARRAVM_ATA_POLL_FLOOR_US=""`.
///
/// A numeric knob has no "off" value the way the main gate does, so empty
/// cannot mean anything but "use the default". A NON-EMPTY unparseable value is
/// still a typo and still panics, which is the case this exists for.
fn sweep_knob(name: &str, value: Result<String, std::env::VarError>, what: &str) -> Option<u64> {
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return None,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("{name} is set to a value that is not valid UTF-8; it takes {what}")
        }
        Ok(raw) => raw,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<u64>() {
        Ok(parsed) => Some(parsed),
        Err(_) => panic!(
            "{name}={raw:?} is not a number; it takes {what}. \
             Refusing to guess: a mistyped sweep leg would silently run the DEFAULT and be \
             read as the value it named changing nothing"
        ),
    }
}

/// `IZARRAVM_ATA_POLL_RUN`, default 16. A sweep override, not a supported knob.
pub(super) fn ata_poll_run_default() -> u32 {
    match sweep_knob(
        "IZARRAVM_ATA_POLL_RUN",
        std::env::var("IZARRAVM_ATA_POLL_RUN"),
        "a positive alt-status read count",
    ) {
        None => ide::ATA_POLL_RUN,
        // Zero would arm on every read and is never what a sweep means; it is
        // refused rather than clamped, for the same reason a typo is.
        Some(0) => panic!(
            "IZARRAVM_ATA_POLL_RUN=0 would arm the skip on every alt-status read; \
             it takes a positive alt-status read count"
        ),
        Some(run) => u32::try_from(run).unwrap_or(u32::MAX),
    }
}

/// `IZARRAVM_ATA_POLL_FLOOR_US`, default 20 us. A sweep override.
pub(super) fn ata_poll_floor_ticks_default() -> u64 {
    match sweep_knob(
        "IZARRAVM_ATA_POLL_FLOOR_US",
        std::env::var("IZARRAVM_ATA_POLL_FLOOR_US"),
        "a minimum-skip floor in microseconds",
    ) {
        None => ide::ATA_POLL_FLOOR_TICKS,
        Some(micros) => (u128::from(micros) * u128::from(izarravm_core::MASTER_CLOCK_HZ)
            / 1_000_000)
            .min(u128::from(u64::MAX)) as u64,
    }
}

/// `IZARRAVM_ATA_POLL_SKIP_DIAG`. Same spelling table as the main gate, and for
/// the same reason: the mirror-image wart of a silent fallback is a knob that
/// reads `=off` as ON, which is what an "anything but empty or 0 is on" test
/// does.
pub(super) fn ata_poll_skip_diag_default() -> bool {
    let raw = match std::env::var("IZARRAVM_ATA_POLL_SKIP_DIAG") {
        Err(std::env::VarError::NotPresent) => return false,
        Err(std::env::VarError::NotUnicode(_)) => panic!(
            "IZARRAVM_ATA_POLL_SKIP_DIAG is set to a value that is not valid UTF-8; \
             accepted spellings are unset or `0` / `off`, and `1` / `on`"
        ),
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_ATA_POLL_SKIP_DIAG={other:?} names no arm; accepted spellings are \
             unset or `0` / `off`, and `1` / `on`. Refusing to guess: the previous test \
             accepted anything but empty or `0` as ON, so `=off` turned it ON"
        ),
    }
}

#[cfg(test)]
pub(crate) fn sweep_knob_for_test(
    name: &str,
    value: Result<String, std::env::VarError>,
) -> Option<u64> {
    sweep_knob(name, value, "a number")
}

/// The spelling table, reachable from the fixtures. The shipped reading happens
/// once per machine at construction, so the contract is otherwise assertable
/// only by mutating the process environment.
#[cfg(test)]
pub(crate) fn parse_ata_poll_skip_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_ata_poll_skip_arm(value)
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
    batch_core: u64,
    cap: u64,
) -> Option<u64> {
    // I-D1b clause 2: no register-mask (D1b) shape can reach the interpreter. It is true by
    // double gating -- `build_poll_loop` passes `sixteen_bit_ok: false` at every interpreter
    // call site, and `poll_head_possible` still refuses a 16-bit head -- and it is load-bearing
    // rather than decorative: `fresh_iteration_spins` below and `req.status_mask
    // .trailing_zeros()` in the seam both assume a mask that already passed the
    // `0x01 | 0x08` check, which for an `Ah` shape happens only at the Direct call-out.
    debug_assert!(
        poll.mask_is_resolved(),
        "an unresolved register-mask poll shape reached the interpreter's poll skip"
    );
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
    if poll.resolved_port(cpu) != crate::bus::POLL_SKIP_IO_PORT {
        diagnostics.source_port_mismatch();
        return None;
    }
    if !bus.vega.poll_skip_status1_port_active() {
        diagnostics.vga_bus_certificate_rejection();
        return None;
    }
    // The certificate is told which port it is pricing, because it refuses outright on a
    // port the ISA wait-state charge covers (see `poll_bus_certificate_from`). Passing the
    // constant is sound here and not an assumption: the check ten lines up has already
    // rejected every shape whose `resolved_port` is not `POLL_SKIP_IO_PORT`, so the constant
    // and the live polled port are the same value by then.
    let Some(certificate) = bus.poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT) else {
        diagnostics.vga_bus_certificate_rejection();
        return None;
    };
    // P2: the per-iteration RAW core charge, with the shape's baked epoch-1 `IN` term replaced
    // by the live privilege column under epoch 2 (F8). Read ONCE, before the binary search, so
    // every projection in this call and the commit that follows price the same iteration --
    // nothing between here and the commit can change the mode.
    let raw_per_iteration = cpu.poll_skip_raw_core_clocks(poll, bus);
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
    let spent = batch_core.checked_add(current_bus)?;
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
        let Some(reserved_core) = cpu.project_poll_skip_core(raw_per_iteration, reserved) else {
            return false;
        };
        let Some(reserved_bus) = bus.poll_project_scaled_bus_clocks(certificate, reserved) else {
            return false;
        };
        let Some(reserved_total) = batch_core
            .checked_add(reserved_core)
            .and_then(|total| total.checked_add(reserved_bus))
        else {
            return false;
        };
        if reserved_total > cap {
            return false;
        }

        let Some(skipped_core) = cpu.project_poll_skip_core(raw_per_iteration, iterations) else {
            return false;
        };
        let Some(skipped_bus) = bus.poll_project_scaled_bus_clocks(certificate, iterations) else {
            return false;
        };
        let Some(candidate_total) = batch_core
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

    let charged = cpu.project_poll_skip_core(raw_per_iteration, best)?;
    bus.poll_project_scaled_bus_clocks(certificate, best)?;

    let committed = cpu
        .commit_poll_skip_core(poll, raw_per_iteration, best)
        .expect("projected poll core commit must succeed");
    debug_assert_eq!(committed, charged);
    cpu.poll_skip_backedge_housekeeping();
    bus.poll_commit_bus(certificate, best);
    diagnostics.committed(best);
    Some(charged)
}

#[inline]
pub(crate) fn checked_batch_core_sum(total: u64, added: u64) -> u64 {
    total
        .checked_add(added)
        .expect("machine batch core total exceeded u64")
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
    batch_core: u64,
    cap: u64,
) -> Option<u64> {
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
    // The memory family has no port slot, so this is `poll.raw_core_clocks()` in both epochs
    // (`poll_skip_raw_core_clocks` returns it unchanged for a non-`Io` family). Taken through
    // the same function anyway so the two executors cannot drift.
    let raw_per_iteration = cpu.poll_skip_raw_core_clocks(poll, bus);
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
    let spent = batch_core.checked_add(current_bus)?;
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
        let Some(reserved_core) = cpu.project_poll_skip_core(raw_per_iteration, reserved) else {
            return false;
        };
        let Some(reserved_bus) = bus.poll_project_scaled_bus_clocks(certificate, reserved) else {
            return false;
        };
        let Some(reserved_total) = batch_core
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

    let charged = cpu.project_poll_skip_core(raw_per_iteration, best)?;
    bus.poll_project_scaled_bus_clocks(certificate, best)?;

    let committed = cpu
        .commit_poll_skip_core(poll, raw_per_iteration, best)
        .expect("projected poll core commit must succeed");
    debug_assert_eq!(committed, charged);
    cpu.poll_skip_backedge_housekeeping();
    bus.poll_commit_memory_bus(certificate, best);
    diagnostics.committed(best);
    Some(charged)
}

impl Machine {
    /// The device-armed ATA/ATAPI clock skip, ACTUATION HALF.
    ///
    /// Called once per batch from `run_until_tick`, from the halt
    /// fast-forward's position: after `advance_cpu_work`, after every pending
    /// service arm, and BEFORE the device-edge cache invalidation -- where
    /// `io_touched` is already true (the arm let the lazy clear stand), so the
    /// cache is dropped for free.
    ///
    /// A NAMED METHOD rather than an inline block, and not only for tidiness:
    /// the fixtures call THIS, so they exercise the production decision tree
    /// instead of a re-implementation of it. A fixture that re-derives the
    /// branch it is testing is a fixture that cannot fail.
    pub(crate) fn actuate_ata_poll_skip(&mut self, deadline_ticks: u64, halted: bool) {
        // The flag is TAKEN unconditionally, not tested under a
        // condition, so it can never be stranded into the next batch.
        // The halt case is excluded explicitly because by the time
        // `run_until_tick` calls this, the halt fast-forward has
        // already advanced the timeline for that batch.
        let armed = std::mem::take(&mut self.ata_poll_skip_armed);
        if armed && !halted {
            // THE MANDATORY ATA TERM. `next_device_edge_ticks`'s own
            // ATA term is OPTIONAL -- it chains `next_ata_deadline`
            // alongside PIT ch0/ch2, DSP, WSS, RTC and timed I/O and
            // takes the `min` over whatever is present -- so with no
            // pending command the `min` falls through to an
            // unrelated edge, and PIT channel 0 at the default
            // 18.2 Hz is up to 54.9 ms away. Stalling there would
            // grant the guest that whole span with the drive READY,
            // charged as `io_stall_clocks`: invisible to
            // `cd_pio_bytes`, invisible to the frame anchor (guest
            // time still advances), and visible only as wall no
            // ladder could attribute.
            //
            // Two independent routes reach the no-pending state --
            // this batch's own end-of-batch advance crossing the
            // completion, and a ring-0 SRST landing mid-batch (the
            // write-side `skip_io_touched` carve-out does not name
            // 0x376, so such a write does not end the batch) -- so
            // the guard is explicit, read straight off the channel,
            // never inferred from the `min`.
            match self.ide.ticks_until_completion() {
                None => {
                    self.ata_poll_skip.counters.declines_not_pending = self
                        .ata_poll_skip
                        .counters
                        .declines_not_pending
                        .saturating_add(1);
                }
                Some(ata_ticks) => {
                    // The DEVICE-bounded target. The block decision
                    // is made on THIS and nothing else.
                    //
                    // THE `min` IS PROVABLY REDUNDANT TODAY, and the
                    // line is kept anyway. `next_cacheable_edge_ticks`
                    // chains `next_ata_deadline()` unconditionally,
                    // which chains this very
                    // `ide.ticks_until_completion()`, so for the
                    // secondary channel the edge set SUBSUMES the
                    // direct read and `ata_ticks` can never bind
                    // tighter than the `min` it is duplicating. (The
                    // design's "neither subsumes the other" is wrong
                    // about this half; it is right about the `None`
                    // arm above, which is not redundant at all and is
                    // the load-bearing part of the precondition.)
                    //
                    // Kept as defence in depth because it is free and
                    // because it makes the precondition and the bound
                    // the SAME fact rather than two facts that happen
                    // to agree -- so a future change that gates the
                    // ATA term inside the edge set cannot silently
                    // unbound this.
                    let device_target = self
                        .next_device_edge_ticks()
                        .map_or(ata_ticks, |edge| ata_ticks.min(edge));
                    if device_target < self.ata_poll_floor_ticks {
                        // A device edge truncated it: the pathology
                        // the latch exists for. Bound it to one
                        // wasted break per pending command.
                        self.ata_poll_skip.counters.declines_below_floor = self
                            .ata_poll_skip
                            .counters
                            .declines_below_floor
                            .saturating_add(1);
                        self.ata_poll_skip.counters.blocks =
                            self.ata_poll_skip.counters.blocks.saturating_add(1);
                        self.ide.block_poll_skip();
                    } else {
                        // Only NOW clamp to the caller's run
                        // deadline. THE TWO TRUNCATION CAUSES ARE
                        // KEPT APART, and that is load-bearing on
                        // the interactive path: the GUI slice is
                        // FAST_EMU_QUANTUM_TICKS = 1 ms against a
                        // 1.111 ms `sector_transfer_ticks`, so the
                        // slice boundary lands inside the wait
                        // essentially every time. The latch is
                        // cleared only by `schedule` or a committed
                        // skip, neither of which happens until the
                        // sector completes, so blocking on this
                        // cause would make the guest spin out the
                        // rest of every sector at full interpreted
                        // cost -- in the GUI only, invisible to
                        // every headless leg on the board.
                        let remaining = deadline_ticks.saturating_sub(self.timeline.now_ticks());
                        let ticks = device_target.min(remaining);
                        if ticks < self.ata_poll_floor_ticks {
                            // The CALLER's slice ran out, not a
                            // device edge. Different cause,
                            // different disposition: decline and DO
                            // NOT block, so the next slice re-arms
                            // after another run of reads.
                            let clamped =
                                &mut self.ata_poll_skip.counters.declines_deadline_clamped;
                            *clamped = clamped.saturating_add(1);
                        } else {
                            // The same primitive `stall_for_hdd_sectors_cached`
                            // uses for the INT 13h path: it advances
                            // the master timeline with the full
                            // device fan-out, charges
                            // `elapsed_clocks` and `io_stall_clocks`,
                            // and advances the TSC.
                            //
                            // I/O STALL, NOT HALTED TIME. A spinning
                            // guest is not idle; `halted_ticks` is
                            // the "guest asked to be parked" metric
                            // and must stay that.
                            //
                            // A clamped target still COMMITS
                            // whenever it clears the floor -- a
                            // 500 us residue of a 1.11 ms wait is a
                            // 500 us skip, the latch is cleared, and
                            // the following slice takes the rest.
                            self.stall_for_master_ticks(ticks);
                            self.ata_poll_skip.counters.spans =
                                self.ata_poll_skip.counters.spans.saturating_add(1);
                            self.ata_poll_skip.counters.ticks =
                                self.ata_poll_skip.counters.ticks.saturating_add(ticks);
                            self.ide.clear_poll_skip_block();
                        }
                    }
                }
            }
        } else if armed {
            self.ata_poll_skip.counters.declines_halted = self
                .ata_poll_skip
                .counters
                .declines_halted
                .saturating_add(1);
        }
    }

    /// Slice 9C-pre: the primary-channel (fixed-disk) analogue of
    /// `actuate_ata_poll_skip`, above -- same shape, same seam, a SEPARATE
    /// armed flag and counter set so this stays entirely independent of the
    /// shipped ATAPI mechanism (no shared mutable state, no risk of
    /// interference either way).
    ///
    /// Reuses `next_device_edge_ticks()` unmodified: it already chains
    /// `next_ata_deadline()` ungated, which folds `self.ata`, `bmide` and
    /// `self.ide` together (see that function's doc comment), so a skip armed
    /// by the primary channel cannot outrun the secondary's boundary or a DMA
    /// transfer's, exactly as the ATAPI skip cannot outrun the primary's.
    /// Reuses `stall_for_master_ticks` to commit: the same whole-machine
    /// advance `stall_for_hdd_sectors_cached` uses for the INT 13h path, so
    /// every device -- including `self.ata` itself -- advances in lockstep
    /// with the master clock the skip moves.
    ///
    /// A reduced mechanism relative to the ATAPI one BY DESIGN, not by
    /// oversight: no interactive "slice too short" pre-arm mitigation (§12 of
    /// the ATAPI test file) and no `monitor_exempt` counter, because this
    /// whole path is dark unless `DeviceTimingProfile::ata` is armed, and
    /// nothing yet arms it in production. Reopen both if 9C ships a real
    /// non-zero `ata::COMMAND_LATENCY_TICKS` and the interactive path shows
    /// the same waste the ATAPI confirmation run found.
    pub(crate) fn actuate_ata_hdd_poll_skip(&mut self, deadline_ticks: u64, halted: bool) {
        let armed = std::mem::take(&mut self.ata_hdd_poll_skip_armed);
        if !armed || halted {
            return;
        }
        let Some(ata_ticks) = self
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion)
        else {
            // THE MANDATORY ATA PRECONDITION, same rationale as the ATAPI
            // actuation: with no command pending, `next_device_edge_ticks`'s
            // `min` falls through to an unrelated edge and stalling there
            // would grant the guest that whole span with the drive READY.
            return;
        };
        // THE `min` IS DEFENCE IN DEPTH, same as the ATAPI actuation: it is
        // provably redundant today because `next_device_edge_ticks` already
        // chains this exact `ticks_until_completion()` call, but keeping it
        // makes the precondition and the bound the SAME fact.
        let device_target = self
            .next_device_edge_ticks()
            .map_or(ata_ticks, |edge| ata_ticks.min(edge));
        if device_target < ata::ATA_POLL_FLOOR_TICKS {
            // A device edge truncated it: bound to one wasted break per
            // pending command.
            if let Some(disk) = self.ata.as_mut() {
                disk.block_poll_skip();
            }
            return;
        }
        let remaining = deadline_ticks.saturating_sub(self.timeline.now_ticks());
        let ticks = device_target.min(remaining);
        if ticks < ata::ATA_POLL_FLOOR_TICKS {
            // The CALLER's slice ran out, not a device edge: decline and do
            // NOT block, so the next slice re-arms after another run of reads.
            return;
        }
        self.stall_for_master_ticks(ticks);
        self.ata_hdd_poll_skip_counters.skips =
            self.ata_hdd_poll_skip_counters.skips.saturating_add(1);
        self.ata_hdd_poll_skip_counters.skipped_ticks = self
            .ata_hdd_poll_skip_counters
            .skipped_ticks
            .saturating_add(ticks);
        if let Some(disk) = self.ata.as_mut() {
            disk.clear_poll_skip_block();
        }
    }

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

    /// The non-direct data-read page histogram (behind `IZARRAVM_SLOW_READ_HISTO=1`), sorted by
    /// count descending. `None` without it. See `CpuGsw::slow_read_histo`.
    pub fn slow_read_histo(&self) -> Option<Vec<(u32, u64)>> {
        self.cpu.slow_read_histo()
    }

    /// `(misaligned, total)` over the same reads. See `CpuGsw::slow_read_alignment`.
    pub fn slow_read_alignment(&self) -> Option<(u64, u64)> {
        self.cpu.slow_read_alignment()
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
            unittester::CMD_MARK => {
                // Records a boot-profiler boundary and returns None, so the
                // machine keeps running: the guest has to be able to say
                // "Toka-DOS is up" from inside AUTOEXEC and then carry on into
                // the workload being measured.
                let id = self.unittester.mark_id();
                self.note_phase_mark(id);
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
    /// `fault_trace_enabled`). Anchored on the CPU's recorded raise site, so the
    /// address and the byte window are the faulting instruction rather than the
    /// one after it.
    ///
    /// Which CONTEXT that instruction belongs to is a separate question this
    /// does not answer: for a fault raised while the TOKAEMM monitor is running
    /// ring-0 PM code it is the monitor's own instruction, and the V86 guest
    /// CS:IP the monitor was servicing is on its stack, not reachable here
    /// without a paging-aware stack walk. Noted as the gap rather than papered
    /// over.
    fn log_fault_trace(&mut self, error: &CpuError) {
        eprint!("{}", self.fault_trace_report(error));
    }

    /// One line naming a fatal error, where it was raised, and the instruction
    /// bytes there. Not env-gated, unlike the full dump above.
    ///
    /// An undecoded I/O port is fatal on purpose so a hardware gap stays
    /// visible, which makes this line the whole diagnosis for the class of bug
    /// it exists to catch. Gating it behind a variable meant a stop said only
    /// which port, and every occurrence then cost a bespoke investigation.
    /// `IZARRAVM_FAULT_TRACE` was there the whole time during the Prince of
    /// Persia work and was never reached for.
    ///
    /// Latched on the SITE, not on a count. A fatal error does not stop the
    /// machine and the GUI resumes it, so an unlatched line is an unbounded
    /// flood from a re-faulting loop. A plain "once" would be worse than it
    /// sounds in the other direction: the guest steps past and keeps faulting
    /// elsewhere, and during bring-up the first fault is routinely a benign
    /// probe while the one worth seeing is the third.
    fn report_fatal_fault(&mut self, error: &CpuError) {
        const DISTINCT_SITE_CAP: usize = 16;

        let site = self.cpu.fault_site();
        // The error is part of the key, not just the address. A driver-detect
        // sweep is one instruction walking a port range (`in al,dx; inc dx; jmp`)
        // and faults at a SINGLE address on every port it touches, so keying on
        // the address alone would name the first port and hide the rest, which
        // is the same failure this latch was chosen over a plain print-once to
        // avoid. The cap below still bounds the flood.
        let key = ReportedFault {
            site: site.map(|record| (record.cs.selector, record.eip)),
            error: error.to_string(),
        };
        if self.reported_fault_sites.contains(&key) {
            return;
        }
        if self.reported_fault_sites.len() >= DISTINCT_SITE_CAP {
            if self.reported_fault_sites.len() == DISTINCT_SITE_CAP {
                // One past the cap, so this branch is reachable exactly once.
                self.reported_fault_sites.push(ReportedFault::sentinel());
                eprintln!("fault: further fault sites suppressed after {DISTINCT_SITE_CAP}");
            }
            return;
        }
        self.reported_fault_sites.push(key);

        let line = match site {
            Some(record) => {
                let linear = record.cs.base.wrapping_add(record.eip);
                let moved = if record.cs_moved {
                    " (CS moved during the instruction, bytes may not be its code)"
                } else {
                    ""
                };
                // "faulting CS:IP", not "CS:IP": the CLI prints its own CS:IP
                // line for every stop, and that one is the live register, which
                // for a fault is the NEXT instruction. Two unlabelled addresses
                // differing by an instruction length is how this bug gets made
                // a second time.
                format!(
                    "fault: {error} at faulting CS:IP={:#06x}:{:#010x} linear={linear:#010x} \
                     bytes=[{}]{moved}. Set IZARRAVM_FAULT_TRACE for the full dump.",
                    record.cs.selector,
                    record.eip,
                    self.linear_bytes(linear, 8).trim_end(),
                )
            }
            // The three sites that raise a fatal CpuError all record one, so
            // this is not expected. Say the site is missing rather than
            // printing live registers dressed up as the raise point.
            None => format!("fault: {error} (no raise site recorded)"),
        };
        self.last_fault_line = Some(line.clone());
        eprintln!("{line}");
    }

    /// The body of `log_fault_trace`, returning what it would print. Split out
    /// so the report can be asserted: eight bare `eprintln!` calls had no seam,
    /// so nothing about this output was under test, including whether it was
    /// reading memory correctly. It was not (see `read_linear_u8`).
    pub(crate) fn fault_trace_report(&mut self, error: &CpuError) -> String {
        use std::fmt::Write as _;

        // Anchor on the recorded raise site, not on the live registers. EIP has
        // already advanced past the faulting instruction by the time a fatal
        // error propagates, so anchoring on it puts the whole dump one
        // instruction late: the byte window would start after the instruction
        // being investigated and leave it buried at an unknown offset in the
        // "before" block, which x86 gives no way to parse backwards. This is
        // only reached on the fatal arm, which is what makes reading the field
        // safe (nothing clears it, and a fatal error leaves the machine
        // resumable).
        let site = self.cpu.fault_site();
        let (cs_register, eip) = match site {
            Some(record) => (record.cs, record.eip),
            None => (self.cpu.registers.cs(), self.cpu.registers.eip),
        };
        let cs = cs_register.selector;
        let cs_base = cs_register.base;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "fault trace: {error} at CS:IP={cs:#06x}:{eip:#010x} v86={} ring0={}",
            self.cpu.is_v86_mode(),
            self.cpu.is_ring0_protected(),
        );
        if site.is_some_and(|record| record.cs_moved) {
            let _ = writeln!(
                out,
                "fault trace: CS CHANGED during the faulting instruction, so the \
                 base below describes the destination and the byte window may not \
                 be the code that faulted"
            );
        }
        let _ = writeln!(
            out,
            "fault trace: CS base={cs_base:#010x} limit={:#010x} linear EIP={:#010x}",
            cs_register.limit,
            cs_base.wrapping_add(eip),
        );
        // The descriptor tables. A delivery fault names a vector and nothing
        // else, which leaves the reader unable to tell WHOSE tables are loaded --
        // the monitor's, a VCPI client's, or the ones a guest built for a V86
        // task of its own. Added 2026-08-30 while diagnosing a Zone 66 crash on
        // INT 0FDh, where exactly that question was the whole investigation: the
        // IDTR read 49 vectors at an address beside the guest's own code, which
        // is what identified the tables as the game's rather than TOKAEMM's.
        let _ = writeln!(
            out,
            "fault trace: IDTR base={:#010x} limit={:#06x} (covers {} vectors)               GDTR base={:#010x} limit={:#06x}",
            self.cpu.idtr.base,
            self.cpu.idtr.limit,
            (u32::from(self.cpu.idtr.limit) + 1) / 8,
            self.cpu.gdtr.base,
            self.cpu.gdtr.limit,
        );
        let linear_eip = cs_base.wrapping_add(eip);
        let start = linear_eip.saturating_sub(32);
        // Count derived from the clamped start, not a fixed 32. Near the bottom
        // of the address space `saturating_sub` shortens the window, and a fixed
        // count would print bytes past the header's own end and into the
        // at/after window, so the label would stop describing the payload.
        let before_len = linear_eip - start;
        let _ = writeln!(
            out,
            "fault trace: bytes before EIP [{start:#010x}..{linear_eip:#010x}): {}",
            self.linear_bytes(start, before_len)
        );
        let _ = writeln!(
            out,
            "fault trace: bytes at/after EIP [{linear_eip:#010x}..): {}",
            self.linear_bytes(linear_eip, 32)
        );
        // Dump the guest stack (128 bytes each direction) using SS base + ESP.
        let ss_base = self
            .cpu
            .registers
            .segment(izarravm_cpu::SegmentIndex::Ss)
            .base;
        let esp = self.cpu.registers.esp();
        let stack_linear = ss_base.wrapping_add(esp);
        let sb_start = stack_linear.saturating_sub(128);
        let stack_before_dwords = (stack_linear - sb_start) / 4;
        let _ = writeln!(
            out,
            "fault trace: SS:ESP={:#06x}:{esp:#010x} linear={stack_linear:#010x}",
            self.cpu
                .registers
                .segment(izarravm_cpu::SegmentIndex::Ss)
                .selector
        );
        let _ = writeln!(
            out,
            "fault trace: stack before ESP: {}",
            self.linear_dwords(sb_start, stack_before_dwords)
        );
        let _ = writeln!(
            out,
            "fault trace: stack at/after ESP: {}",
            self.linear_dwords(stack_linear, 32)
        );
        out
    }

    /// `count` bytes from a LINEAR address, hex, `--` where the address is not
    /// mapped. An unmapped byte has to be visibly absent: printing a zero for it
    /// puts a plausible value in front of someone about to make a decision.
    fn linear_bytes(&mut self, linear: u32, count: u32) -> String {
        let mut out = String::with_capacity(count as usize * 3);
        for index in 0..count {
            match self.read_linear_u8(linear.wrapping_add(index)) {
                Some(byte) => out.push_str(&format!("{byte:02x} ")),
                None => out.push_str("-- "),
            }
        }
        out
    }

    /// `count` dwords from a LINEAR address, assembled byte by byte rather than
    /// through a dword read: a dword can straddle a page boundary, and its two
    /// halves can then live in unrelated frames, which a single translation of
    /// the base address would silently get wrong.
    fn linear_dwords(&mut self, linear: u32, count: u32) -> String {
        let mut out = String::with_capacity(count as usize * 9);
        for index in 0..count {
            let base = linear.wrapping_add(index * 4);
            let bytes: Vec<Option<u8>> = (0..4)
                .map(|offset| self.read_linear_u8(base.wrapping_add(offset)))
                .collect();
            if let [Some(b0), Some(b1), Some(b2), Some(b3)] = bytes[..] {
                let value = u32::from_le_bytes([b0, b1, b2, b3]);
                out.push_str(&format!("{value:08x} "));
            } else {
                out.push_str("-------- ");
            }
        }
        out
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
        // The device-edge deadline cache is only maintained INSIDE this loop. Every
        // host-side mutator that can move a device schedule -- key/mouse/joystick
        // injection, media mount and eject, RTC/CMOS seeding, audio rendering, a
        // mode change from the GUI -- runs between run calls on the machine
        // thread, so dropping it once here covers all of them
        // at the cost of one pull-scan per run call (~1 ms of guest time).
        self.invalidate_device_edge_cache();
        // D5 (slice0b review §2, the reflected-call diagnostic's
        // `on_batch_boundary` tag): persists ACROSS iterations of the loop
        // below, so the batch-entry call each iteration can tell whether the
        // PREVIOUS batch ended for a reason independent of the currently
        // open trip's own instructions (`true`) or purely because that
        // trip's own nested `IRET` just re-enabled IF (`false`, the
        // `can_take_before` check below). `true` at the very first iteration
        // is correct: there is no prior batch to blame here. The
        // default-true/mark-false-only-on-the-IF-edge bookkeeping is pulled
        // out into `BatchBoundaryRealTag` (bottom of this file) so it has its
        // own plain unit test (merge-review nit 7) independent of the
        // surrounding CPU/bus machinery this loop needs.
        #[cfg(feature = "reflected-call-diagnostic")]
        let mut reflected_call_batch_tag = BatchBoundaryRealTag::new();
        while self.timeline.now_ticks() < deadline_ticks {
            // Periodic sampling, gated on a sentinel that is `u64::MAX` when disarmed so this
            // costs one compare against an already-live value. See `fire_periodic_phase_mark`.
            if self.next_phase_mark_ticks <= self.timeline.now_ticks() {
                self.fire_periodic_phase_mark();
            }
            // Windowed IPE trace, gated on the same kind of `u64::MAX` sentinel: one load of a
            // counter the direct backend already maintains and one compare, per batch. See
            // `arm_ipe_window_trace` for why the boundary is approximate.
            if self.cpu.perf_counters().jit_direct_entries >= self.next_ipe_window_entries {
                self.close_ipe_window();
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
            // AFTER the two direct-map arms, so a coarse invalidation that already ran leaves
            // this a no-op (the flush cleared the aperture flag). One bool test per batch when
            // nothing was raised.
            if self.aperture_content_changed {
                self.cpu.note_aperture_content_changed();
                self.aperture_content_changed = false;
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
            self.exempt_io_touched = false;
            // Cleared HERE, at batch entry, so no arm can ever survive into a
            // batch that did not raise it; and the channel's run counter is
            // zeroed alongside, which is what makes "armed" mean "N alt-status
            // reads inside ONE batch with no other I/O to the channel" rather
            // than the much weaker "N reads eventually".
            self.ata_poll_skip_armed = false;
            self.ide.reset_alt_status_run();
            // Slice 9C-pre: the primary-channel analogue, cleared at the same
            // batch-entry point and for the same reason -- see the ATAPI
            // comment above.
            self.ata_hdd_poll_skip_armed = false;
            if let Some(disk) = self.ata.as_mut() {
                disk.reset_alt_status_run();
            }
            // DO NOT ARM INTO A SLICE THAT CANNOT PAY FOR A SKIP.
            //
            // Measured on the interactive confirmation run, not modelled: the
            // GUI's real slice is ~13 us, not the 1 ms
            // `FAST_EMU_QUANTUM_TICKS` the design assumed.
            // `execution_budget` is `min(credit, quantum)`, and in a paced
            // window the CREDIT binds -- a machine running on time refills it
            // in tiny wall-clock increments and spends it immediately. So the
            // typical interactive slice is BELOW the 20 us floor, a skip armed
            // in it can never commit, and it declines as `_deadline_clamped`:
            // 364,753 of them in one 328 s window, 2.14 per committed span,
            // each costing a forced batch break plus a device-edge-cache
            // invalidation and its pull-scan.
            //
            // Harmless to guest state -- the latch correctly never fired, which
            // is the R2-B split holding under real load -- but pure waste, and
            // a paced run at rt 1.0 has no observable margin in which to see
            // its cost. On a slower host that margin is what runs out first.
            //
            // MONOTONE WITHIN A RUN CALL, which is what makes one test at batch
            // entry sound: `deadline_ticks` is fixed for the call and `now`
            // only advances, so a slice that cannot pay stays unable to pay for
            // every later batch in the same call.
            //
            // INERT HEADLESS except in the run tail, where the arm already
            // declines: one `run_until_halt_or_cycles` spans the whole run, so
            // `remaining` is astronomically above the floor until the very end.
            //
            // A MITIGATION, NOT AN ELIMINATION: a batch entered with 25 us
            // remaining still arms and still clamps. `_deadline_clamped` is to
            // be re-read on the confirmation run, never assumed to reach zero.
            self.ata_poll_skip_slice_too_short = deadline_ticks
                .saturating_sub(self.timeline.now_ticks())
                < self.ata_poll_floor_ticks;
            self.device_wrote_memory = false;
            let trace_before = self.trace.elapsed_clocks();
            #[cfg(test)]
            let elapsed_at_batch_start = self.elapsed_clocks;
            // Capture live timing state before the fields move into MachineBus.
            let timeline_at_batch_start = self.timeline;
            let master_ticks_at_batch_start = self.timeline.now_ticks();
            let beam_at_batch_start = self.scanout_beam_dots();
            let margo_scanout_at_batch_start = self.vega.margo_scanout().is_some();
            let trace_elapsed_at_batch_start = trace_before;
            let bus_rem_at_batch_start = self.bus_rem;
            // bus_timing's (num, den), read from the same authoritative CPU mode
            // that scale_bus uses. Machine's active_mode copy exists for Lotura
            // register readback and is updated in the same set_mode call.
            let (bus_num_at_batch_start, bus_den_at_batch_start) =
                bus_timing(self.cpu.level(), self.timing_epoch);
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
            // 1 ms fallback bounds every mode; the 386 paths drop to a
            // DAC-period fallback while a consumer that can observe the
            // difference is active (see `fine_batch_grain_required`). Either may be
            // shortened by an earlier event. Compute this once at batch entry
            // because the run loop is layout-sensitive.
            let remaining_ticks = deadline_ticks - self.timeline.now_ticks();
            let remaining = self
                .timeline
                .cpu_clocks_for_master_ticks_ceil(remaining_ticks)
                .max(1);
            // The reflected-call memo's interpreter forcing (slice1 plan section 4.2: the
            // batch loop owns this toggle, never the call-out). A learn cycle's two JOURNALED
            // trips, and any audit trip, must retire through the interpreter's write seams --
            // a compiled block's stores reach none of them. Read once per batch and acted on
            // only when the answer CHANGES, so a run with the knob off pays one bool compare.
            #[cfg(all(feature = "jit", feature = "reflected-call-memo"))]
            {
                let want = self.cpu.reflected_call_wants_interpreter();
                if want != self.reflected_call_interpreter_forced {
                    self.cpu
                        .set_native_backend_enabled(!want && self.native_backend_configured);
                    self.reflected_call_interpreter_forced = want;
                }
                if want {
                    self.cpu.reflected_call_note_learn_batch();
                }
            }
            let cap = self.event_batch_cap_cached(remaining);
            #[cfg(test)]
            let cap = self
                .test_next_batch_cap
                .take()
                .map_or(cap, |requested| requested.min(cap));
            #[cfg(test)]
            self.test_effective_batch_caps.push(cap);
            // Published to `MachineBus` below so the reflected-call memo's answer gate
            // bounds a lump by THIS cap rather than a second, drift-prone derivation;
            // the bool only names the refusal lane (`DeviceEdge` vs `Cap`).
            let reflected_call_cap_is_device_edge = self.batch_cap_is_device_edge(cap, remaining);
            // Slice 0b's batch-straddle counter (plan §5, Q6/B6; §14 Q2): one
            // cfg-gated call marking a batch boundary on the reflected-call
            // diagnostic's currently open trip, if any and if armed. The
            // ONLY reason this diagnostic feature reaches the machine crate.
            // `take_and_reset` reads whether the PREVIOUS batch ended for a
            // real (cap/deadline/fault/HLT/HLE-post) reason, then immediately
            // resets the tag to "real" as the default for the batch about to
            // run; only the `can_take_before` IF-edge break below can mark it
            // `false` again before the NEXT iteration's call here reads it.
            #[cfg(feature = "reflected-call-diagnostic")]
            izarravm_cpu::on_batch_boundary(reflected_call_batch_tag.take_and_reset());
            #[cfg(feature = "jit")]
            let poll_skip_enabled = self.poll_skip_enabled;
            // Read before the destructure below, the same way `poll_skip_enabled`
            // is: a `Copy` policy bool, resolved once per machine.
            let ata_poll_skip_enabled = self.ata_poll_skip_enabled;
            let ata_poll_skip_slice_too_short = self.ata_poll_skip_slice_too_short;
            // Read before the destructure for the same reason: a `Copy` policy
            // value resolved once per machine at construction.
            let device_timing = self.device_timing;
            // Read before the destructure for the same reason, and because both sources are
            // moved into the bus below as mutable borrows. See `MachineBus::a20_open` and
            // `MachineBus::device_free_extended_floor` for what keeps them live.
            let a20_open = self.keyboard.a20_enabled();
            let device_free_extended_floor = self.vega.device_free_extended_floor();
            let cpu_batch_start = self.host_profile.start();
            let outcome = {
                let Machine {
                    timing_epoch,
                    poll_skip_certificate,
                    retrace_poll,
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
                    open_bus,
                    pic,
                    pit,
                    keyboard,
                    gameport,
                    speaker,
                    rtc,
                    dma,
                    inta_diag,
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
                    pending_cd_doorbell,
                    cd_doorbell_status,
                    cd_redirector_dos_ds,
                    unittester,
                    pci,
                    io_touched,
                    exempt_io_touched,
                    ata_poll_skip_armed,
                    ata_poll_skip,
                    port_bus_batch_clocks,
                    port_accesses_by_class,
                    ata_hdd_poll_skip_armed,
                    pit_observer_fine_until,
                    opl_probe,
                    shadow_l1,
                    device_wrote_memory,
                    pending_device_memory_write_range,
                    direct_map_changed,
                    direct_data_map_changed,
                    aperture_content_changed,
                    #[cfg(feature = "jit")]
                    poll_skip_diagnostics,
                    #[cfg(test)]
                    test_prior_core_pushes,
                    #[cfg(test)]
                    test_string_port_observations,
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
                    open_bus,
                    pic,
                    pit,
                    keyboard,
                    gameport,
                    speaker,
                    rtc,
                    dma,
                    device_timing,
                    inta_diag,
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
                    timing_epoch: *timing_epoch,
                    poll_skip_certificate: &*poll_skip_certificate,
                    retrace_poll,
                    string_port_element_bytes: 0,
                    #[cfg(test)]
                    test_string_port_observations,
                    pending_mode,
                    fast_post: *fast_post,
                    booter_inert: *booter_inert,
                    program_runtime: *program_runtime,
                    pending_toka_service,
                    toka_service_status: *toka_service_status,
                    pending_cd_doorbell,
                    cd_doorbell_status,
                    cd_redirector_armed: cd_redirector_dos_ds.is_some(),
                    unittester,
                    wait_states: profile.wait_states,
                    icache_fetch_clocks: if crate::bus::l1_charges_folded(
                        *active_mode,
                        *timing_epoch,
                    ) {
                        0
                    } else {
                        u64::from(izarravm_bus::BusCycle::clocks_for(
                            BusWidth::Byte,
                            cache_model.code_fetch_wait_states(),
                        ))
                    },
                    cache: cache_model,
                    l1_charges_folded: crate::bus::l1_charges_folded(*active_mode, *timing_epoch),
                    flat_data_cost: active_mode.uses_approximate_timing(),
                    a20_open,
                    device_free_extended_floor,
                    extended_ram_screen: crate::bus::extended_ram_screen_enabled(),
                    lazy_port_reads: active_mode.uses_approximate_timing(),
                    isa_io_wait: crate::bus::isa_io_wait_armed(),
                    lazy_ports_386: crate::bus::lazy_ports_386_for(*active_mode),
                    io_touched,
                    exempt_io_touched,
                    ata_poll_skip_enabled,
                    ata_poll_skip_armed,
                    ata_poll_skip_slice_too_short,
                    ata_poll_skip,
                    isa_io_clocks: port_bus_batch_clocks,
                    port_accesses_by_class,
                    ata_hdd_poll_skip_armed,
                    pit_observer_fine_until,
                    opl_probe,
                    shadow_l1,
                    device_wrote_memory,
                    pending_device_memory_write_range,
                    direct_map_changed,
                    direct_data_map_changed,
                    aperture_content_changed,
                    direct_mapping_epoch: &mut self.direct_mapping_epoch,
                    vga_wipe_census: &mut self.vga_wipe_census,
                    core_clocks_so_far: 0,
                    prior_runs_core_clocks: 0,
                    timeline_at_batch_start,
                    master_ticks_at_batch_start,
                    beam_at_batch_start,
                    margo_scanout_at_batch_start,
                    trace_elapsed_at_batch_start,
                    bus_rem_at_batch_start,
                    bus_num_at_batch_start,
                    bus_den_at_batch_start,
                    reflected_call_batch_cap: cap,
                    reflected_call_cap_is_device_edge,
                    reflected_call_answered: false,
                };
                // Collapse the batch into one CpuCycleOutcome so every downstream
                // service step (device advance, CD stall, pending INT/mode/Toka/
                // unittester, console flush, HLT fast-forward) is unchanged:
                // core_clocks is the batch sum, halted is set iff the batch ended
                // on a HLT.
                let mut batch_core = 0u64;
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
                        batch_core = checked_batch_core_sum(batch_core, o.core_clocks);
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
                        let spent = batch_core
                            .checked_add(bus.in_batch_scaled_bus_clocks())
                            .expect("machine batch spent total exceeded u64");
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
                        bus.prior_runs_core_clocks = batch_core;
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
                                batch_core = checked_batch_core_sum(batch_core, skipped_core);
                                bus.prior_runs_core_clocks = batch_core;
                                let spent = batch_core
                                    .checked_add(bus.in_batch_scaled_bus_clocks())
                                    .expect("machine poll batch spent total exceeded u64");
                                remaining = cap.saturating_sub(spent);
                            }
                            align
                        } else {
                            // The classifier is gated off here, which is the whole point: this
                            // is the Direct configuration every scoreboard fixture runs in, and
                            // it is why every poll counter reads zero on all ten of them. Tally
                            // what the classifier WOULD have said, read-only, so the zeros can be
                            // told apart from "there was nothing to find".
                            #[cfg(feature = "poll-head-probe")]
                            cpu.probe_poll_head();
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
                        // A committed span can land `spent` EXACTLY on the cap.
                        // The loop-top check has already run this pass, so
                        // without this break the machine would enter
                        // `run_budgeted(0)`, which always executes at least one
                        // instruction, and drift one instruction past the plain
                        // interpreter at the same scaled-clock boundary (found
                        // by `memory_poll_skip_matches_the_interpreter_at_batch_boundaries`
                        // when the 586 bus ratio moved for the PC100 spec). The
                        // deliberate zero-budget alignment run is different: it
                        // must still execute to reach the poll head.
                        #[cfg(feature = "jit")]
                        if !align_poll_head && remaining == 0 {
                            break;
                        }
                        #[cfg(feature = "jit")]
                        let run_budget = if align_poll_head { 0 } else { remaining };
                        #[cfg(not(feature = "jit"))]
                        let run_budget = remaining;
                        match cpu.run_budgeted(&mut bus, run_budget) {
                            Ok(o) => {
                                batch_core =
                                    checked_batch_core_sum(batch_core, o.consumed_core_clocks);
                                if o.halted {
                                    halted = true;
                                    break;
                                }
                                // A port access read or changed time-dependent device
                                // state; an HLE INT (pending_soft_int) needs &mut self.
                                // Stop so the run loop services them at this instant.
                                // The reflected-call memo's batch-end mitigation (slice1
                                // plan section 6): an ANSWERED trip ends the batch. A real
                                // trip contains 6-8 IF-enable batch boundaries and an
                                // answered one contains none, so without this term an IRQ
                                // raised during the lump would wait for the batch cap
                                // instead of being re-checked at the trip's first `IRET`.
                                // One boundary per answer against 6-8 per real trip is
                                // strictly FEWER boundaries, and the cap is unchanged, so
                                // `irq0_edges` cannot move; what it buys is that no
                                // interrupt is ever deferred across a lump.
                                if *bus.io_touched
                                    || bus.pending_soft_int.is_some()
                                    || std::mem::take(&mut bus.reflected_call_answered)
                                {
                                    break;
                                }
                                if !can_take_before && cpu.can_take_interrupt() {
                                    // D5: this break is the trip's OWN
                                    // instructions re-enabling IF, not a
                                    // cap/deadline/device reason -- the next
                                    // `on_batch_boundary` call must NOT count
                                    // toward `batch_straddle_trips`.
                                    #[cfg(feature = "reflected-call-diagnostic")]
                                    reflected_call_batch_tag.mark_if_edge();
                                    break;
                                }
                                // A core-only fast exit avoids another loop when
                                // this run consumed the full budget. Bus-heavy
                                // runs are caught by the combined check above.
                                if batch_core >= cap {
                                    break;
                                }
                            }
                            Err(e) => {
                                fault = Some(CpuRunError {
                                    error: e.error,
                                    consumed_core_clocks: checked_batch_core_sum(
                                        batch_core,
                                        e.consumed_core_clocks,
                                    ),
                                });
                                break;
                            }
                        }
                    }
                }
                match fault {
                    Some(e) => Err(e),
                    None => Ok(CpuCycleOutcome {
                        core_clocks: batch_core,
                        halted,
                    }),
                }
            };
            self.consume_pending_device_memory_write_range();
            self.host_profile
                .record(MachineProfilePhaseKind::CpuBatch, cpu_batch_start);

            let settled_core = match &outcome {
                Ok(outcome) => outcome.core_clocks,
                Err(error) => error.consumed_core_clocks,
            };
            let bus_clocks = self.trace.elapsed_clocks() - trace_before;
            let scaled_bus_clocks = self.scale_bus(bus_clocks);
            let isa_clocks = std::mem::take(&mut self.port_bus_batch_clocks);
            let step = settled_core
                .checked_add(scaled_bus_clocks)
                .and_then(|total| total.checked_add(isa_clocks))
                .expect("machine batch step exceeded u64");
            self.scaled_bus_clocks = self.scaled_bus_clocks.saturating_add(scaled_bus_clocks);
            let advance_start = self.host_profile.start();
            self.advance_cpu_work(step, settled_core);
            self.host_profile
                .record(MachineProfilePhaseKind::AdvanceDevices, advance_start);

            // The same settlement owner serves a completed batch and a fatal
            // return. Keep the observation at that common boundary so tests do
            // not have to infer fatal core from device time.
            #[cfg(test)]
            {
                self.test_batch_observations.push(TestBatchObservation {
                    raw_bus_clocks: bus_clocks,
                    scaled_bus_clocks,
                    core_clocks: settled_core,
                    isa_clocks,
                    step,
                    bus_rem_at_entry: bus_rem_at_batch_start,
                    bus_rem_at_exit: self.bus_rem,
                    timeline_ticks_at_entry: master_ticks_at_batch_start,
                    timeline_ticks_at_exit: self.timeline.now_ticks(),
                    elapsed_at_entry: elapsed_at_batch_start,
                    elapsed_at_exit: self.elapsed_clocks,
                    effective_cap: cap,
                    fatal: outcome.is_err(),
                    device_wrote_memory_before_reconcile: self.device_wrote_memory,
                    direct_map_changed_before_reconcile: self.direct_map_changed,
                    direct_data_map_changed_before_reconcile: self.direct_data_map_changed,
                });
                self.test_batch_core_totals.push(settled_core);
                self.test_batch_steps.push(step);
                self.test_batch_isa_clocks.push(isa_clocks);
                self.test_batch_registers.push((
                    self.cpu.registers.eip,
                    self.cpu.registers.ecx(),
                    self.cpu.registers.edi(),
                ));
            }

            match outcome {
                Ok(outcome) => {
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
                    if let Some(cmd) = self.pending_cd_doorbell.take() {
                        serviced = true;
                        self.perform_cd_doorbell(cmd);
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
                        // The reflected-call memo's HLE seam (Fable review 2026-09-05,
                        // BLOCKING F2). This is the instant a posted software interrupt is
                        // SERVICED, and the machine writes guest registers and memory here,
                        // outside every CPU seam. A reflected trip straddles 6-8 batch
                        // boundaries, so a nested `INT 1Ah`/`INT 10h`/`INT 13h`/`INT 2Fh`
                        // inside a trip is serviced mid-trip and the flag is cleared at the
                        // next batch entry -- long before `finish_trip` or the answer screen
                        // could ask the bus whether one was posted. Refusing the open trip
                        // HERE is the only place that sees it.
                        #[cfg(feature = "reflected-call-memo")]
                        self.cpu.reflected_call_note_soft_int_serviced();
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
                                // Saturating because a batch can end PAST the
                                // deadline, so `now > deadline` is a normal state
                                // here, not a broken invariant. The batch was
                                // granted `remaining_ticks` worth of clocks at
                                // loop top, and four separate routes carry the
                                // timeline beyond that grant:
                                //   (a) `run_budgeted` always retires at least one
                                //       instruction, so the last one straddles the
                                //       budget;
                                //   (b) the batch-entry `service_pending_interrupt`
                                //       is charged before any cap test runs;
                                //   (c) under EPOCH 1 `port_bus_batch_clocks` joins
                                //       the batch-end `step` without ever having
                                //       been counted against the cap (`spent` is
                                //       core plus in-batch bus only). Under epoch 2
                                //       it IS counted: review finding F2 folded the
                                //       lane into `in_batch_scaled_bus_clocks()`,
                                //       which is what `spent` is built from, and
                                //       turned the CPU's per-instruction cap screen
                                //       off so the exact test runs after every
                                //       retired instruction. That route's
                                //       contribution to the overshoot is then one
                                //       access, not a whole batch's worth -- which
                                //       matters because epoch 2 charges every port,
                                //       not seven of them;
                                //   (d) the grant itself rounds UP --
                                //       `cpu_clocks_for_master_ticks_ceil(..).max(1)`
                                //       -- so even a batch that spends exactly what
                                //       it was granted lands up to
                                //       `ticks_per_cpu_clock - 1` master ticks past
                                //       the deadline (249 at the 386 quantum), with
                                //       no instruction-level explanation at all.
                                // The overshoot is small but NOT bounded by a
                                // handful of clocks: measured at 3 clocks on the
                                // 386 fixture (`tokaemm_v86_iopl_is_always_three_
                                // across_a_boot` hits it once per boot), while the
                                // true bound is one instruction's core clocks plus
                                // the batch's uncounted ISA charge -- `isa_io_clocks`
                                // is `clock_hz / 1_000_000`, i.e. 166 clocks per ISA
                                // access at the 586 persona, so an Approximate-class
                                // EPOCH-1 batch can overshoot by hundreds. Under
                                // epoch 2 route (c) contributes one access's class
                                // charge and no more.
                                // `next_timer_wake` already clamps the same quantity
                                // the same way, and it is exactly what returned None
                                // here.
                                //
                                // Clamping to zero IS the semantic: `remaining`
                                // exists only to stop the halted fast-forward
                                // from running past the caller's deadline, and a
                                // deadline already reached leaves zero halted
                                // time to grant. `advance_halted_ticks(0)` is the
                                // same no-op the exact-landing case already takes,
                                // and the loop condition then ends the run. The
                                // pending device edge is not lost -- the next run
                                // call re-derives it.
                                #[cfg(test)]
                                if self.timeline.now_ticks() > deadline_ticks {
                                    self.test_halt_deadline_clamps += 1;
                                }
                                let remaining =
                                    deadline_ticks.saturating_sub(self.timeline.now_ticks());
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
                    self.actuate_ata_poll_skip(deadline_ticks, outcome.halted);
                    self.actuate_ata_hdd_poll_skip(deadline_ticks, outcome.halted);
                    // Keep the cached device edge across this batch only if the batch
                    // was provably quiet. Anything else could have rearmed a device:
                    // a guest port access (io_touched -- NOT a Margo blit arm, which
                    // is a memory write and arms only the uncached `vega_edge_ticks`
                    // terms; see `Machine::device_edge_cache`), a bus-side DMA write
                    // into guest RAM, any serviced HLE / mode-switch / Toka / BIOS32
                    // / unittester step, or the HLT fast-forward's device advance.
                    // Over-invalidating here
                    // costs one pull-scan; under-invalidating would hand the next
                    // batch a deadline LATER than the truth, so the test is
                    // deliberately one-sided. The batch-entry check in
                    // `event_batch_cap_cached` handles the remaining case, a cached
                    // edge that simply came due.
                    if self.io_touched
                        || self.exempt_io_touched
                        || self.device_wrote_memory
                        || serviced
                        || outcome.halted
                    {
                        self.invalidate_device_edge_cache();
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
                Err(fatal) => {
                    let error = fatal.error;
                    self.invalidate_device_edge_cache();
                    if self.keyboard.a20_enabled() != a20_before {
                        self.cpu.note_a20_changed();
                    }
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
                    self.report_fatal_fault(&error);
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

/// D5's real/IF-edge bookkeeping (slice0b review §2), pulled out of
/// `run_until_tick`'s batch loop so it has its own plain unit test
/// (merge-review nit 7) independent of the surrounding CPU/bus machinery: the
/// interesting half of D5 is that this tag defaults to "real" every
/// iteration and is marked "not real" ONLY by the trip's own IF-enable edge
/// (`can_take_before` false, then true) -- a wrong default, or a mark that
/// leaks across iterations, would silently restore the 100%-tautological
/// `batch_straddle_trips` 0c exists to fix, and `cargo check` alone (the only
/// gate this file gets under `-p izarravm-machine` in CI's quartet) cannot
/// catch that.
#[cfg(feature = "reflected-call-diagnostic")]
#[derive(Debug, Default)]
struct BatchBoundaryRealTag(bool);

#[cfg(feature = "reflected-call-diagnostic")]
impl BatchBoundaryRealTag {
    /// `true`: there is no prior batch to blame at the very first iteration,
    /// so the first `on_batch_boundary` call correctly reports "real".
    fn new() -> Self {
        Self(true)
    }

    /// Read the tag left by the batch that just ended, then reset it to
    /// `true` -- the default for the batch about to run, overridden only if
    /// THAT batch's own `can_take_before` edge calls `mark_if_edge` before
    /// the next `take_and_reset`.
    fn take_and_reset(&mut self) -> bool {
        let real = self.0;
        self.0 = true;
        real
    }

    /// The trip's own IF-enable edge, not a cap/deadline/fault/HLT/HLE-post
    /// reason: the NEXT `take_and_reset` must report `false`.
    fn mark_if_edge(&mut self) {
        self.0 = false;
    }
}

#[cfg(all(test, feature = "reflected-call-diagnostic"))]
#[path = "run_batch_boundary_tag_test.rs"]
mod batch_boundary_real_tag_tests;
