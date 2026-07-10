// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Machine {
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
                if let Some(path) = self.test_snapshot_path.clone() {
                    if let Err(err) = self.write_snapshot_ppm(&path) {
                        eprintln!("unit tester: snapshot to {} failed: {err}", path.display());
                    }
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
        let deadline = self.elapsed_clocks.saturating_add(cycles);
        self.run_until_clock(deadline, cycles)
    }

    pub fn run_until_halt_or_cycles(
        &mut self,
        max_cycles: u64,
    ) -> Result<StopReason, MachineError> {
        let deadline = self.elapsed_clocks.saturating_add(max_cycles);
        self.run_until_clock(deadline, max_cycles)
    }

    fn run_until_clock(
        &mut self,
        deadline: u64,
        requested: u64,
    ) -> Result<StopReason, MachineError> {
        while self.elapsed_clocks < deadline {
            if self.direct_map_changed {
                self.cpu.note_direct_map_changed();
                self.direct_map_changed = false;
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
            // Batch-entry snapshots for the Slice 1 lazy port-read prediction (P4a
            // Task 1.1). Captured here, before the fields below are moved into the
            // destructure, so they reflect live machine state at the moment this
            // batch's MachineBus is built (the one that matters for Slice 1).
            let elapsed_clocks_at_batch_start = self.elapsed_clocks;
            let vga_dots_at_batch_start = self.vga_dots;
            let beam_at_batch_start = self.video.beam_dots();
            let trace_elapsed_at_batch_start = trace_before;
            let bus_rem_at_batch_start = self.bus_rem;
            let inv_clock_at_batch_start = self.timing.inv_clock;
            let pit_clocks_at_batch_start = self.pit_clocks;
            let pit_per_clock_at_batch_start = self.timing.pit_per_clock;
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
            // The batch cap is per timing class:
            // - Accurate (386): exactly one DAC sample of CPU time, so the
            //   per-clock fine-samplers stay in lockstep. BYTE-IDENTICAL
            //   contract (bench cyc/iter + aux, boot suite, device cadence):
            //   do not touch.
            // - Approximate (486/586): up to the next due device event, bounded
            //   by a ~1 ms latency ceiling and floored at the DAC-sample cap;
            //   approx_batch_cap holds the full contract. Batch splits move the
            //   f64 device accumulators through different partial sums, so
            //   device event instants may microshift against the Accurate
            //   splitting; that is licensed in this class (results stay
            //   bit-exact, time is approximate). Computed once
            //   per batch entry: the run loop sits on a measured code-layout
            //   cliff, so nothing here may run per instruction.
            let remaining = deadline - self.elapsed_clocks;
            let cap = if self.active_mode.uses_approximate_timing() {
                self.approx_batch_cap(remaining)
            } else {
                self.timing.clocks_per_audio_sample.min(remaining)
            };
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
                    video,
                    margo,
                    distira,
                    rom,
                    serial,
                    serial2,
                    lpt,
                    lpt2,
                    device_ports,
                    pic,
                    pit,
                    keyboard,
                    speaker,
                    rtc,
                    dma,
                    fdc,
                    floppy,
                    opl,
                    dsp,
                    mixer,
                    wavetable_mpu,
                    midi_input_mpu,
                    wss,
                    wss_base,
                    wss_enabled,
                    ide,
                    ata,
                    trace,
                    pending_soft_int,
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
                    direct_map_changed,
                    #[cfg(test)]
                    test_prior_core_pushes,
                    ..
                } = self;
                let mut bus = MachineBus {
                    memory,
                    ram_lookup,
                    video,
                    margo,
                    distira,
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
                    speaker,
                    rtc,
                    dma,
                    fdc,
                    floppy,
                    opl,
                    dsp,
                    mixer,
                    wavetable_mpu,
                    midi_input_mpu,
                    wss,
                    wss_base: *wss_base,
                    wss_enabled: *wss_enabled,
                    ide,
                    ata,
                    trace,
                    pending_soft_int,
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
                    direct_map_changed,
                    core_clocks_so_far: 0,
                    prior_runs_core_clocks: 0,
                    elapsed_clocks_at_batch_start,
                    vga_dots_at_batch_start,
                    beam_at_batch_start,
                    trace_elapsed_at_batch_start,
                    bus_rem_at_batch_start,
                    inv_clock_at_batch_start,
                    bus_num_at_batch_start,
                    bus_den_at_batch_start,
                    pit_clocks_at_batch_start,
                    pit_per_clock_at_batch_start,
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
                        // toward the cap in the Approximate class, checked at
                        // loop top so an over-budget batch does not enter one
                        // more run. APPROXIMATE ONLY: the Accurate class (frozen
                        // 386) must keep not just the core-only comparison
                        // but the historical batch GEOMETRY - the old post-run
                        // check meant every batch executed at least one
                        // instruction even when the interrupt-service charge
                        // alone met the cap, and review showed the loop-top
                        // relocation changes that (a gate-invisible but real
                        // frozen-class delta). So Accurate skips this break and
                        // relies solely on the restored post-run check below.
                        let spent = u64::from(batch_core) + bus.in_batch_scaled_bus_clocks();
                        if spent >= cap && bus.active_mode.uses_approximate_timing() {
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
                        let remaining = cap.saturating_sub(spent);
                        // Publish the batch-scoped core clocks accumulated so far
                        // (the interrupt-service charge + every prior run of this
                        // batch, exactly the core component the batch-end step
                        // will combine) so a lazy port-read prediction inside the
                        // coming run can add the RUN-scoped core_clocks_so_far on
                        // top and see a batch-total that is monotone across run
                        // boundaries. See MachineBus::prior_runs_core_clocks.
                        bus.prior_runs_core_clocks = u64::from(batch_core);
                        // Logs the bus field itself (not an independent `batch_core`
                        // read) so `batch_loop_publishes_prior_runs_core_clocks_before_every_run`
                        // actually fails if the store above is ever deleted or the
                        // publish drifts from the field a lazy prediction reads.
                        #[cfg(test)]
                        test_prior_core_pushes
                            .last_mut()
                            .expect("opened at batch entry")
                            .push(bus.prior_runs_core_clocks);
                        match cpu.run_straight_line(&mut bus, remaining) {
                            Ok(o) => {
                                batch_core = batch_core.saturating_add(o.core_clocks);
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
                                // Historical post-run core-clock check: the sole
                                // cap break for the Accurate class (preserving
                                // its at-least-one-run batch geometry exactly);
                                // for Approximate the loop-top guest-clock check
                                // above fires first or at the same boundary.
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
                    let step = u64::from(outcome.core_clocks)
                        + self.scale_bus(bus_clocks)
                        + std::mem::take(&mut self.isa_io_batch_clocks);
                    self.elapsed_clocks += step;
                    // Advance the OPL timers so AdLib detection's delay loops see
                    // the overflow flag (the synthesis clock is driven separately
                    // by `render_audio`).
                    let advance_start = self.host_profile.start();
                    self.advance_devices(step);
                    self.host_profile
                        .record(MachineProfilePhaseKind::AdvanceDevices, advance_start);
                    // Charge the CD-ROM's seek + transfer time for a read the
                    // instruction just issued, the way the floppy stalls. The
                    // guest clock jumps; the GUI's realtime pacing turns that into
                    // a visible wait.
                    let cd_secs = self.ide.take_stall_secs();
                    if cd_secs > 0.0 {
                        let cd_start = self.host_profile.start();
                        self.stall_for(cd_secs);
                        self.host_profile
                            .record(MachineProfilePhaseKind::CdStall, cd_start);
                    }
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
                        match self.next_timer_wake(deadline) {
                            Some(wake_step) => {
                                self.elapsed_clocks += wake_step;
                                self.advance_devices(wake_step);
                                self.host_profile
                                    .record(MachineProfilePhaseKind::HaltFastForward, halt_start);
                            }
                            None => {
                                self.host_profile
                                    .record(MachineProfilePhaseKind::HaltFastForward, halt_start);
                                return Ok(StopReason::Halted);
                            }
                        }
                    }
                    // The A20 gate toggled during this step (port 0x92, the 8042, INT 15h, or XMS):
                    // tell the CPU so it drops any prefetch/decoded bytes that A20 now remaps near
                    // the 1 MB wrap, before the next batch executes against the new gate state.
                    if self.keyboard.a20_enabled() != a20_before {
                        self.cpu.note_a20_changed();
                    }
                    // A device wrote guest RAM this step (a DMA disk/floppy transfer or block copy),
                    // bypassing the CPU's SMC tracking; drop the prefetch + decode cache so staged
                    // code is re-decoded rather than replayed stale on a later near branch into it.
                    if self.device_wrote_memory {
                        self.cpu.note_device_memory_write();
                    }
                    if self.direct_map_changed {
                        self.cpu.note_direct_map_changed();
                        self.direct_map_changed = false;
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
