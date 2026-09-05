// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_cpu::CpuCycleOutcome;
// Both uses sit inside `#[cfg(feature = "jit")]` items, so the import has to be gated too or a
// no-default-features build of the test targets trips unused_imports.
#[cfg(feature = "jit")]
use izarravm_cpu::TLB_ENTRIES;

#[cfg(feature = "jit")]
fn poll_skip_test_machine(enabled: bool, tracing: TracingMode, mode: GswMode, mask: u8) -> Machine {
    poll_skip_test_machine_at_epoch(enabled, tracing, mode, mask, 1)
}

#[cfg(feature = "jit")]
fn poll_skip_test_machine_at_epoch(
    enabled: bool,
    tracing: TracingMode,
    mode: GswMode,
    mask: u8,
    epoch: u32,
) -> Machine {
    let program = [
        0xba, 0xda, 0x03, // mov dx,3DAh
        0xec, 0xa8, mask, 0x75, 0xfb, // wait while the status bit is set
        0xec, 0xa8, mask, 0x74, 0xfb, // wait until the status bit is set
        0xeb, 0xf4, // repeat both phases
    ];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.set_timing_epoch_for_test(epoch);
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x103;
    machine.cpu.poll_skip_backedge_housekeeping();
    machine.cpu.set_native_backend_enabled(false);
    machine.poll_skip_enabled = enabled;
    machine.trace.set_tracing_mode(tracing);
    machine
}

#[cfg(feature = "jit")]
#[test]
fn epoch_two_poll_skip_matches_executed_iterations() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for protected in [false, true] {
            for mask in [0x01, 0x08] {
                let mut baseline =
                    poll_skip_test_machine_at_epoch(false, TracingMode::Off, mode, mask, 2);
                let mut skipped =
                    poll_skip_test_machine_at_epoch(false, TracingMode::Off, mode, mask, 2);
                for machine in [&mut baseline, &mut skipped] {
                    if protected {
                        machine.cpu.control.cr0 |= 1;
                    }
                    machine.run_cycles(10_000).unwrap();
                    machine.cpu.registers.eip = 0x108;
                    machine.cpu.poll_skip_backedge_housekeeping();
                    let poll = machine.cpu.poll_loop().expect("warm epoch 2 poll");
                    // Seed a valid fractional core carry without changing the loop's price.
                    machine.cpu.commit_poll_skip_core(poll, 1, 1).unwrap();
                    machine.bus_rem = 0;
                    machine.cpu.reset_perf_counters();
                }
                skipped.poll_skip_enabled = true;
                let window = mode.clock_hz() / 30;
                let baseline_stop = baseline.run_cycles(window).unwrap();
                let skipped_stop = skipped.run_cycles(window).unwrap();
                assert_eq!(skipped_stop, baseline_stop);
                let skipped_iterations = skipped.cpu.perf_counters().poll_skip_iterations;
                assert!(
                    skipped_iterations > 1,
                    "mode={mode:?} mask={mask} eip={:#x} eligible={} spans={} instructions={}",
                    skipped.cpu.registers.eip,
                    skipped.cpu.poll_skip_eligible(),
                    skipped.cpu.perf_counters().poll_skip_spans,
                    skipped.cpu.perf_counters().instructions,
                );
                assert_eq!(
                    skipped.cpu.perf_counters().instructions + 3 * skipped_iterations,
                    baseline.cpu.perf_counters().instructions,
                    "mode={mode:?} mask={mask}",
                );
                assert_poll_machine_boundary_eq(&skipped, &baseline);
            }
        }
    }
}

#[cfg(feature = "jit")]
fn assert_poll_machine_boundary_eq(skipped: &Machine, baseline: &Machine) {
    // `core_clocks_so_far` is public-run scratch. The next run resets it before
    // executing its first instruction, and the production path deliberately no
    // longer canonicalizes it on return. Compare the architectural and persistent
    // timing fields directly so this boundary assertion does not turn that scratch
    // partition into production behavior.
    assert_eq!(skipped.cpu.registers, baseline.cpu.registers);
    assert_eq!(skipped.cpu.fpu, baseline.cpu.fpu);
    assert_eq!(skipped.cpu.control, baseline.cpu.control);
    assert_eq!(skipped.cpu.msr, baseline.cpu.msr);
    assert_eq!(skipped.cpu.gdtr, baseline.cpu.gdtr);
    assert_eq!(skipped.cpu.idtr, baseline.cpu.idtr);
    assert_eq!(skipped.cpu.ldtr, baseline.cpu.ldtr);
    assert_eq!(skipped.cpu.tr, baseline.cpu.tr);
    assert_eq!(skipped.cpu.elapsed_clocks, baseline.cpu.elapsed_clocks);
    assert_eq!(skipped.cpu.halted, baseline.cpu.halted);
    assert_eq!(
        skipped.cpu.poll_skip_timing_remainder(),
        baseline.cpu.poll_skip_timing_remainder()
    );
    assert_eq!(skipped.timeline, baseline.timeline);
    assert_eq!(skipped.bus_rem, baseline.bus_rem);
    assert_eq!(skipped.elapsed_clocks, baseline.elapsed_clocks);
    assert_eq!(
        skipped.trace.elapsed_clocks(),
        baseline.trace.elapsed_clocks()
    );
    assert_eq!(skipped.vega.beam_dots(), baseline.vega.beam_dots());
    assert_eq!(
        skipped.vega.frame_sequence(),
        baseline.vega.frame_sequence()
    );
    let beam = skipped.vega.beam_dots();
    assert_eq!(
        skipped.vega.status1_bits(beam),
        baseline.vega.status1_bits(beam)
    );
    assert_eq!(skipped.pit, baseline.pit);
    assert_eq!(skipped.pic, baseline.pic);
    assert_eq!(skipped.serial, baseline.serial);
    assert_eq!(skipped.serial2, baseline.serial2);
    assert_eq!(skipped.lpt, baseline.lpt);
    assert_eq!(skipped.lpt2, baseline.lpt2);
    assert_eq!(skipped.keyboard, baseline.keyboard);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_matches_the_interpreter_at_batch_boundaries() {
    // P2 adds the epoch dimension. Under epoch 2 an elided iteration must project the port's
    // full class charge on the certificate's third lane and the live privilege column's `IN`
    // count on the core lane, so a span of N elided iterations has to land the guest clock in
    // exactly the place N naturally executed iterations land it -- which is what
    // `assert_poll_machine_boundary_eq`'s `elapsed_clocks` / `trace.elapsed_clocks()` /
    // `timeline` / beam comparisons say. This machine runs in REAL mode, which is one of the
    // two privilege columns the skip can actually take (F8: V86 is structurally refused, see
    // `poll_skip_is_refused_in_v86_but_would_price_the_v86_column`), and the epoch-2 leg fails
    // on any per-iteration term that is wrong by a single clock.
    for epoch in [1, 2] {
        for mode in [GswMode::Gsw486, GswMode::Gsw586] {
            for mask in [0x01, 0x08] {
                let mut baseline =
                    poll_skip_test_machine_at_epoch(false, TracingMode::Off, mode, mask, epoch);
                let mut skipped =
                    poll_skip_test_machine_at_epoch(true, TracingMode::Off, mode, mask, epoch);

                skipped.poll_skip_enabled = false;
                baseline.run_cycles(1_000).unwrap();
                skipped.run_cycles(1_000).unwrap();
                assert_eq!(skipped.cpu, baseline.cpu);
                for machine in [&mut baseline, &mut skipped] {
                    machine.cpu.registers.eip = 0x108;
                    machine.cpu.poll_skip_backedge_housekeeping();
                    let poll = machine.cpu.poll_loop().expect("warm direct poll loop");
                    for _ in 0..2 {
                        let raw = if epoch == 2 {
                            1
                        } else {
                            poll.raw_core_clocks()
                        };
                        machine
                            .cpu
                            .commit_poll_skip_core(poll, raw, 1)
                            .expect("one iteration advances the CPU timing remainder");
                        if machine.cpu.poll_skip_timing_remainder() != 0 {
                            break;
                        }
                    }
                    machine.cpu.reset_perf_counters();
                    machine.bus_rem = if epoch == 2 { 0 } else { 2 };
                    assert_ne!(machine.cpu.poll_skip_timing_remainder(), 0);
                    assert!(machine.bus_rem < u64::from(bus_timing(mode.persona(), epoch).1));
                }
                skipped.poll_skip_enabled = true;

                let window = mode.clock_hz() / 30;
                let baseline_stop = baseline.run_cycles(window).unwrap();
                let skipped_stop = skipped.run_cycles(window).unwrap();
                assert_eq!(
                    skipped_stop, baseline_stop,
                    "epoch={epoch} mode={mode:?} mask={mask:#04x}"
                );
                // poll_loop now borrows &mut, so materialize the diagnostic before the
                // assert rather than inside its format args (which hold the other &self
                // borrows). Its only effect is host bookkeeping, invisible to every
                // field assert_poll_machine_boundary_eq compares.
                let loop_diag = skipped.cpu.poll_loop();
                assert!(
                    skipped.cpu.perf_counters().poll_skip_spans > 0,
                    "epoch={epoch} mode={mode:?} mask={mask:#04x} eip={:08x} linear={:08x} dx={:04x} eligible={} loop={loop_diag:?}",
                    skipped.cpu.registers.eip,
                    skipped.cpu.linear_eip(),
                    skipped.cpu.registers.edx() as u16,
                    skipped.cpu.poll_skip_eligible(),
                );
                assert!(skipped.cpu.perf_counters().poll_skip_iterations > 1);
                assert!(
                    skipped.cpu.perf_counters().instructions
                        < baseline.cpu.perf_counters().instructions
                );
                assert_poll_machine_boundary_eq(&skipped, &baseline);
            }
        }
    }
}

/// P2's projection equality, stated as arithmetic rather than as a boundary comparison: an
/// elided run of N iterations must charge EXACTLY N times the per-iteration cost on all three
/// lanes -- core (through `level_timing`), bus (through `scale_bus`) and port (unscaled) -- and
/// the per-iteration figures must be the same ones an executed access pays.
///
/// The lane values are written here as literals, not read back from the code under test:
/// `PciLegacyVga` 56 minus the scaled generic byte cycle (4 raw through the I586 bus dial
/// 16/105, which floors to 0), and Intel's real-mode `IN` 7 x 12 = 84 raw replacing epoch 1's
/// flat 12 inside the shape's own 17.
#[cfg(feature = "jit")]
#[test]
fn an_elided_span_charges_exactly_n_times_one_iteration_on_every_lane() {
    const N: u64 = 7;
    let mut machine =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
    prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
    machine.timing_epoch = 2;
    machine.port_bus_batch_clocks = 0;

    let (raw_per_iteration, port_lane, raw_bus_per_iteration) =
        with_cpu_and_bus(&mut machine, |cpu, bus| {
            let poll = cpu.poll_loop().expect("warm poll descriptor");
            let certificate = bus
                .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
                .expect("0x3DA is admitted under epoch 2");
            (
                cpu.poll_skip_raw_core_clocks(poll, bus),
                certificate.port_bus_clocks_per_iteration(),
                certificate.raw_clocks_per_iteration(),
            )
        });
    assert_eq!(
        raw_per_iteration,
        21 - 12 + 84,
        "the core lane must swap the shape's baked epoch-1 IN (12 raw) for Intel's real-mode          column (7 x 12 = 84 raw), leaving the 5-slot shape's other four slots (21 - 12 = 9          raw) alone -- the rest of the core class table is the recalibration's slice 1, not          this one's"
    );
    assert_eq!(
        port_lane, 52,
        "the port lane must be the PciLegacyVga class charge (56) minus the scaled generic \
         cycle, which slice 2's (1,1) bus ratio no longer scales away"
    );

    let core_before = machine.cpu.elapsed_clocks;
    let bus_before = machine.trace.elapsed_clocks();
    let carry_before = machine.cpu.poll_skip_timing_remainder();
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        let poll = cpu.poll_loop().expect("warm poll descriptor");
        let certificate = bus
            .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
            .expect("0x3DA is admitted under epoch 2");
        cpu.commit_poll_skip_core(poll, raw_per_iteration, N)
            .expect("core commit");
        bus.poll_commit_bus(certificate, N);
    });

    // Core: one scaling of (raw x N), carrying the remainder -- never N separate scalings.
    // `level_timing(I586)` is (1, 12) -- written as a literal here, not read back from the
    // function under test, so a dial change has to come and edit this line deliberately.
    let scaled = raw_per_iteration * N + carry_before;
    assert_eq!(
        machine.cpu.elapsed_clocks - core_before,
        scaled / 12,
        "the core lane must charge one scaling of N x the per-iteration raw"
    );
    assert_eq!(
        machine.trace.elapsed_clocks() - bus_before,
        raw_bus_per_iteration * N,
        "the bus lane must charge exactly N raw certificates"
    );
    assert_eq!(
        machine.port_bus_batch_clocks,
        port_lane * N,
        "the port lane must accrue exactly N x the class charge, unscaled"
    );
}

/// F8's machine half. The poll skip is unavailable in V86 by construction, so the V86 column
/// (Intel's 19 -- the row the design's own §2 arithmetic used) is exactly the row this path
/// never takes; the per-iteration figure is nonetheless DERIVED from the live mode rather than
/// baked, which is what would keep it right if the eligibility rule ever changed. The four
/// columns themselves are pinned in `izarravm-cpu`'s
/// `poll_skip_raw_core_clocks_reads_the_live_privilege_column`; this is the eligibility half.
#[cfg(feature = "jit")]
#[test]
fn the_poll_skip_prices_the_real_mode_column_because_v86_can_never_reach_it() {
    let mut machine =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
    prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
    machine.timing_epoch = 2;
    assert!(!machine.cpu.is_v86_mode(), "the fixture runs in real mode");
    assert!(machine.cpu.poll_skip_eligible());
    let poll = machine.cpu.poll_loop().expect("warm poll descriptor");
    let raw = with_cpu_and_bus(&mut machine, |cpu, bus| {
        cpu.poll_skip_raw_core_clocks(poll, bus)
    });
    assert_eq!(raw, 21 - 12 + 84, "real mode: Intel's IN 7 x 12 = 84 raw");
}

/// F14's instrument, as a RATE. The fixture is the `VL_WaitVBL` shape itself -- spin while the
/// retrace bit is set, then spin until it is set, repeat -- so each of its two loops exits
/// exactly once per displayed frame, and the two edge counters must track the VGA's own frame
/// sequence rather than the number of polls. That is the whole point of the instrument: the
/// poll count moves by design under the reprice and under the skip, and this does not.
///
/// Run across BOTH epochs and BOTH skip arms, because those are the four legs the certifier is
/// read on. The skip arm is the load-bearing one: an elided span's edge solve admits only
/// iterations whose projected instant lands strictly BEFORE the edge the loop is waiting for,
/// so a loop's OWN exit edge can never be elided past, and the exit count survives the elision.
///
/// What this does NOT claim, deliberately: that eliding preserves every edge the guest would
/// have observed. Edges observed are a function of when the guest READS, and a loop spinning on
/// one sense of the bit can be elided across an edge it is not waiting for. Measured on the
/// artificial `setup_poll_machine_case` fixture (whose beam is teleported into place), skip-on
/// and skip-off can differ by one edge for exactly that reason. It does not touch the certifier:
/// a `VL_WaitVBL` loop's exit IS the edge it waits for.
#[cfg(feature = "jit")]
#[test]
fn retrace_poll_exits_track_frames_not_polls() {
    for (epoch, skip) in [(1, false), (1, true), (2, false), (2, true)] {
        let mut machine =
            poll_skip_test_machine_at_epoch(skip, TracingMode::Off, GswMode::Gsw586, 0x08, epoch);
        machine.run_cycles(1_000).unwrap();
        let frames_before = machine.vega.frame_sequence();
        let (rising_before, falling_before) = machine.retrace_poll_exits();
        machine.run_cycles(20_000_000).unwrap();
        let frames = machine.vega.frame_sequence() - frames_before;
        let (rising, falling) = machine.retrace_poll_exits();
        let (rising, falling) = (rising - rising_before, falling - falling_before);
        assert!(
            frames > 2,
            "epoch {epoch} skip={skip}: the fixture must cross frames"
        );
        // One exit per edge per frame, +/- the partial frame at each end of the window.
        assert!(
            rising.abs_diff(frames) <= 1 && falling.abs_diff(frames) <= 1,
            "epoch {epoch} skip={skip}: exits must track frames -- frames={frames}              rising={rising} falling={falling}"
        );
    }
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_declines_when_bus_tracing_is_active() {
    for tracing in [TracingMode::Counts, TracingMode::Full] {
        let mut baseline = poll_skip_test_machine(false, tracing, GswMode::Gsw586, 0x08);
        let mut skipped = poll_skip_test_machine(false, tracing, GswMode::Gsw586, 0x08);
        baseline.run_cycles(1_000).unwrap();
        skipped.run_cycles(1_000).unwrap();
        for machine in [&mut baseline, &mut skipped] {
            machine.cpu.registers.eip = 0x108;
            machine.cpu.poll_skip_backedge_housekeeping();
            machine.cpu.reset_perf_counters();
        }
        skipped.poll_skip_enabled = true;
        let baseline_stop = baseline.run_cycles(20_000).unwrap();
        let skipped_stop = skipped.run_cycles(20_000).unwrap();
        assert_eq!(skipped_stop, baseline_stop);
        assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
        assert_eq!(
            skipped.cpu.perf_counters().instructions,
            baseline.cpu.perf_counters().instructions
        );
        assert_poll_machine_boundary_eq(&skipped, &baseline);
    }
}

#[cfg(feature = "jit")]
#[test]
fn poll_bus_certificate_rejects_instruction_fetches_from_a_device_window() {
    const BASE: u32 = 0x000a_0000;
    const CODE: &[u8] = &[0xec, 0xa8, 0x08, 0x74, 0xfb];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &[0xf4]).unwrap();
    assert!(machine.set_vga_mode(0x13));
    for (offset, byte) in CODE.iter().copied().enumerate() {
        machine.write_physical_u8(BASE + offset as u32, byte);
    }
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        let mut cs = SegmentRegister::real(0xa000);
        cs.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        cpu.registers.set_edx(0x03da);
        cpu.set_native_backend_enabled(false);
        for offset in [0, 1, 3] {
            cpu.registers.eip = offset;
            cpu.poll_skip_backedge_housekeeping();
            cpu.run_budgeted(bus, 0)
                .expect("decode poll from VGA aperture");
        }
        cpu.registers.set_edx(0x03da);
        cpu.registers.eip = 0;
        cpu.poll_skip_backedge_housekeeping();
        let poll = cpu.poll_loop().expect("device-backed shape is structural");
        assert!(
            bus.poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
                .is_none(),
            "device-backed instruction fetches cannot be aggregated"
        );
    });
}

#[cfg(feature = "jit")]
#[test]
fn poll_bus_certificate_admits_only_the_one_port_it_prices() {
    // WAS `poll_bus_certificate_refuses_a_port_the_isa_wait_charge_covers`. P2 retires the ISA
    // refusal's REASON -- the certificate now carries an unscaled third lane
    // (`port_bus_clocks_per_iteration`), so a charged port CAN be priced -- but replaces it
    // with an explicit admission rather than deleting it (review F9). The lane prices a port;
    // it cannot express a port whose read is not idempotent, and an elided span performs the
    // port's side effects ONCE rather than per iteration. 0x40 (the 8254 read-back latch) is
    // exactly such a port, and it must still refuse -- in BOTH epochs and with the ISA knob in
    // either position, because the admission no longer depends on either.
    let mut machine =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
    prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
    for epoch in [1, 2] {
        machine.timing_epoch = epoch;
        // The arm has to be set BEFORE `with_cpu_and_bus`: the bus caches the resolved knob at
        // construction (one bool test per access instead of a lazy-static touch), which is also
        // the production shape -- the environment is read once per process.
        for armed in [false, true] {
            crate::bus::set_isa_io_wait_for_test(Some(armed));
            with_cpu_and_bus(&mut machine, |cpu, bus| {
                let poll = cpu.poll_loop().expect("warm poll descriptor");
                assert!(
                    bus.poll_bus_certificate(poll, 0x0040).is_none(),
                    "epoch={epoch} armed={armed}: a port the admission does not name must                      refuse, whatever the ISA knob and whatever the epoch"
                );
                assert!(
                    bus.poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
                        .is_some(),
                    "epoch={epoch} armed={armed}: 0x3DA is the admitted port in BOTH epochs --                      under epoch 2 it is priced by the third lane, not refused"
                );
            });
        }
    }
    machine.timing_epoch = 1;
    crate::bus::set_isa_io_wait_for_test(None);
}

#[cfg(feature = "jit")]
fn setup_poll_machine(enabled: bool, ecx: bool, paired: bool, source: u32) -> Machine {
    setup_poll_machine_case(enabled, ecx, paired, source, GswMode::Gsw586, 0x08, !paired)
}

#[cfg(feature = "jit")]
fn setup_poll_machine_case(
    enabled: bool,
    ecx: bool,
    paired: bool,
    source: u32,
    mode: GswMode,
    mask: u8,
    jz: bool,
) -> Machine {
    let mut program = Vec::new();
    program.push(if ecx { 0xb9 } else { 0xbb });
    program.extend_from_slice(&source.to_le_bytes());
    program.push(0xb8);
    program.extend_from_slice(&0xdead_beefu32.to_le_bytes());
    program.push(0xf9); // stc: prove the loop's TEST flags replace incoming flags
    program.extend_from_slice(&[
        0x89,
        if ecx { 0xca } else { 0xda },
        0x29,
        0xc0,
        0xec,
        0xa8,
        mask,
        if jz { 0x74 } else { 0x75 },
        if paired { 0x02 } else { 0xf7 },
    ]);
    if paired {
        program.extend_from_slice(&[0xeb, 0xf5]);
    }
    program.push(0xf4);
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.set_native_backend_enabled(false);
    machine.poll_skip_enabled = enabled;
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine
}

#[cfg(feature = "jit")]
fn with_cpu_and_bus<R>(
    machine: &mut Machine,
    f: impl FnOnce(&mut CpuGsw, &mut MachineBus<'_>) -> R,
) -> R {
    let mut cpu = std::mem::take(&mut machine.cpu);
    let result = with_bus(machine, |bus| f(&mut cpu, bus));
    machine.cpu = cpu;
    result
}

#[cfg(feature = "jit")]
fn move_beam_to(machine: &mut Machine, target: u64) {
    let frame = machine.vega.frame_dots();
    assert_ne!(frame, 0);
    let current = machine.vega.beam_dots();
    let advance = (target + frame - current) % frame;
    machine.vega.advance(0, 0, 0, advance);
    assert_eq!(machine.vega.beam_dots(), target % frame);
}

#[cfg(feature = "jit")]
fn set_status1_bit(machine: &mut Machine, mask: u8, target: bool) {
    let bit = mask.trailing_zeros() as u8;
    let beam = machine.vega.beam_dots();
    let current = machine.vega.status1_bits(beam) & mask != 0;
    if current != target {
        let dots = machine
            .vega
            .dots_until_status1_bit_change_from(beam, bit, target)
            .expect("status bit has a geometric edge");
        machine.vega.advance(0, 0, 0, dots);
    }
    let beam = machine.vega.beam_dots();
    assert_eq!(machine.vega.status1_bits(beam) & mask != 0, target);
}

#[cfg(feature = "jit")]
fn prepare_setup_poll_head(
    machine: &mut Machine,
    ecx: bool,
    paired: bool,
    source: u32,
    mask: u8,
    jz: bool,
) {
    const HEAD: u32 = 0x10b;
    let starts: &[u32] = if paired {
        &[0, 2, 4, 5, 7, 9]
    } else {
        &[0, 2, 4, 5, 7]
    };
    with_cpu_and_bus(machine, |cpu, bus| {
        let initial_eflags = cpu.registers.eflags | 1;
        cpu.registers.set_eax(0xdead_beef);
        cpu.registers
            .set_ebx(if ecx { 0xaaaa_1234 } else { source });
        cpu.registers
            .set_ecx(if ecx { source } else { 0xbbbb_5678 });
        cpu.registers.set_edx(0xcccc_9abc);
        cpu.registers.eflags = initial_eflags;
        for offset in starts {
            if *offset == 4 {
                cpu.registers.set_edx(0x03da);
            }
            cpu.registers.eip = HEAD + offset;
            cpu.poll_skip_backedge_housekeeping();
            cpu.run_budgeted(bus, 0)
                .expect("warm exact setup poll phase");
        }
        cpu.registers.set_eax(0xdead_beef);
        cpu.registers
            .set_ebx(if ecx { 0xaaaa_1234 } else { source });
        cpu.registers
            .set_ecx(if ecx { source } else { 0xbbbb_5678 });
        cpu.registers.set_edx(0xcccc_9abc);
        cpu.registers.eflags = initial_eflags;
        cpu.registers.eip = HEAD;
        cpu.poll_skip_backedge_housekeeping();
        assert!(cpu.poll_loop().is_some());
        cpu.reset_perf_counters();
    });
    set_status1_bit(machine, mask, jz == paired);
}

#[cfg(feature = "jit")]
fn projected_poll_total(machine: &mut Machine, iterations: u64, batch_core: u64) -> u64 {
    with_cpu_and_bus(machine, |cpu, bus| {
        let poll = cpu.poll_loop().expect("warm poll descriptor");
        let certificate = bus
            .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
            .expect("RAM poll bus certificate");
        let core = cpu
            .project_poll_skip_core(cpu.poll_skip_raw_core_clocks(poll, bus), iterations)
            .expect("poll core projection");
        let scaled_bus = bus
            .poll_project_scaled_bus_clocks(certificate, iterations)
            .expect("poll bus projection");
        batch_core + core + scaled_bus
    })
}

#[cfg(feature = "jit")]
fn projected_poll_dots(machine: &mut Machine, iterations: u64, batch_core: u64) -> u64 {
    with_cpu_and_bus(machine, |cpu, bus| {
        let poll = cpu.poll_loop().expect("warm poll descriptor");
        let certificate = bus
            .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
            .expect("RAM poll bus certificate");
        let core = cpu
            .project_poll_skip_core(cpu.poll_skip_raw_core_clocks(poll, bus), iterations)
            .expect("poll core projection");
        let scaled_bus = bus
            .poll_project_scaled_bus_clocks(certificate, iterations)
            .expect("poll bus projection");
        bus.poll_project_dot_advance(batch_core + core + scaled_bus)
            .expect("poll dot projection")
    })
}

#[cfg(feature = "jit")]
fn attempt_poll_skip(machine: &mut Machine, batch_core: u64, cap: u64) -> Option<u64> {
    let mut diagnostics = run::PollSkipDiagnostics::default();
    with_cpu_and_bus(machine, |cpu, bus| {
        bus.prior_runs_core_clocks = batch_core;
        bus.core_clocks_so_far = 0;
        let poll = run::classify_poll_skip_boundary(cpu, &mut diagnostics)?;
        run::try_poll_skip(cpu, bus, &mut diagnostics, poll, batch_core, cap)
    })
}

#[cfg(feature = "jit")]
fn assert_poll_skip_classifier_case(
    machine: &mut Machine,
    expected_buckets: [u64; 4],
    expected_rejections: u64,
    expected_structural_hits: u64,
) {
    machine.poll_skip_diagnostics.enable_for_test();
    assert_eq!(
        machine.run_cycles(1).expect("one bounded run boundary"),
        StopReason::CycleLimit { requested: 1 }
    );
    let diagnostics = &machine.poll_skip_diagnostics;
    let (calls, buckets) = diagnostics.classifier_accounting();
    assert_eq!(calls, 1);
    assert_eq!(buckets, expected_buckets);
    assert_eq!(buckets.into_iter().sum::<u64>(), calls);
    assert_eq!(
        diagnostics.admission_accounting(),
        (expected_rejections, expected_structural_hits)
    );
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_run_source_has_one_classifier_call_site() {
    let source = include_str!("run.rs");
    assert_eq!(
        source.matches(".poll_loop()").count(),
        1,
        "run.rs must classify only through classify_poll_skip_boundary"
    );
    assert!(source.contains("let poll = cpu.poll_loop();"));
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_run_boundary_classifies_once_with_distinct_admission_semantics() {
    let mut ineligible =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw386, 0x08, true);
    assert_poll_skip_classifier_case(&mut ineligible, [1, 0, 0, 0], 1, 0);

    let mut structural_miss =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw486, 0x08, true);
    assert_poll_skip_classifier_case(&mut structural_miss, [0, 1, 0, 0], 0, 0);

    let mut non_head =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw486, 0x08, true);
    prepare_setup_poll_head(&mut non_head, false, false, 0x1234_03da, 0x08, true);
    with_cpu_and_bus(&mut non_head, |cpu, _| {
        cpu.registers.set_edx(0x03da);
        cpu.registers.eip = 0x10f;
        cpu.poll_skip_backedge_housekeeping();
    });
    assert_poll_skip_classifier_case(&mut non_head, [0, 0, 1, 0], 0, 0);

    let mut head =
        setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw486, 0x08, true);
    prepare_setup_poll_head(&mut head, false, false, 0x1234_03da, 0x08, true);
    assert_poll_skip_classifier_case(&mut head, [0, 0, 0, 1], 0, 1);
}

#[cfg(feature = "jit")]
#[test]
fn setup_direct_and_paired_poll_skip_preserve_full_registers_flags_and_boundaries() {
    let cases = [
        (GswMode::Gsw486, false, false, 0x01, true),
        (GswMode::Gsw486, false, false, 0x08, false),
        (GswMode::Gsw486, false, true, 0x01, true),
        (GswMode::Gsw486, false, true, 0x08, false),
        (GswMode::Gsw586, true, false, 0x08, true),
        (GswMode::Gsw586, true, false, 0x01, false),
        (GswMode::Gsw586, true, true, 0x08, true),
        (GswMode::Gsw586, true, true, 0x01, false),
    ];
    for (mode, ecx, paired, mask, jz) in cases {
        let source = if ecx { 0x5678_03da } else { 0x1234_03da };
        let mut baseline = setup_poll_machine_case(false, ecx, paired, source, mode, mask, jz);
        let mut skipped = setup_poll_machine_case(true, ecx, paired, source, mode, mask, jz);
        prepare_setup_poll_head(&mut baseline, ecx, paired, source, mask, jz);
        prepare_setup_poll_head(&mut skipped, ecx, paired, source, mask, jz);

        let mut halted = false;
        for boundary in 0..128 {
            let baseline_stop = baseline.run_cycles(100_000).unwrap();
            let skipped_stop = skipped.run_cycles(100_000).unwrap();
            assert_eq!(
                skipped_stop, baseline_stop,
                "mode={mode:?} ecx={ecx} paired={paired} mask={mask:#04x} jz={jz} boundary={boundary}"
            );
            assert_poll_machine_boundary_eq(&skipped, &baseline);
            if baseline_stop == StopReason::Halted {
                halted = true;
                break;
            }
        }
        assert!(
            halted,
            "poll did not reach its edge: mode={mode:?} ecx={ecx} paired={paired} mask={mask:#04x} jz={jz}"
        );
        assert!(
            skipped.cpu.perf_counters().poll_skip_spans > 0,
            "mode={mode:?} ecx={ecx} paired={paired} mask={mask:#04x} jz={jz}"
        );
        assert!(
            skipped.cpu.perf_counters().instructions < baseline.cpu.perf_counters().instructions,
            "bulk span did not replace real iterations"
        );
        assert_eq!(skipped.cpu.registers.edx(), source);
        assert_eq!(skipped.cpu.registers.eax() & !0xff, 0);
        assert_eq!(skipped.cpu.eflags(), baseline.cpu.eflags());
    }
}

#[cfg(feature = "jit")]
#[test]
fn setup_poll_source_mismatch_and_tiny_cap_never_bulk_charge_prefix_state() {
    let mut baseline = setup_poll_machine(false, false, true, 0x1234_1234);
    let mut skipped = setup_poll_machine(true, false, true, 0x1234_1234);
    prepare_setup_poll_head(&mut baseline, false, true, 0x1234_1234, 0x08, false);
    prepare_setup_poll_head(&mut skipped, false, true, 0x1234_1234, 0x08, false);
    baseline.cpu.registers.set_edx(0xaaaa_03da);
    skipped.cpu.registers.set_edx(0xaaaa_03da);
    let baseline_stop = baseline.run_cycles(50_000).unwrap();
    let skipped_stop = skipped.run_cycles(50_000).unwrap();
    assert_eq!(skipped_stop, baseline_stop);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_iterations, 0);
    assert_poll_machine_boundary_eq(&skipped, &baseline);

    let mut baseline = setup_poll_machine(false, false, false, 0x1234_03da);
    let mut skipped = setup_poll_machine(true, false, false, 0x1234_03da);
    skipped.poll_skip_enabled = false;
    baseline.run_cycles(1_000).unwrap();
    skipped.run_cycles(1_000).unwrap();
    skipped.poll_skip_enabled = true;
    baseline.cpu.reset_perf_counters();
    skipped.cpu.reset_perf_counters();
    let baseline_stop = baseline.run_cycles(1).unwrap();
    let skipped_stop = skipped.run_cycles(1).unwrap();
    assert_eq!(skipped_stop, baseline_stop);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
    assert_poll_machine_boundary_eq(&skipped, &baseline);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_reserves_k_plus_one_at_the_exact_cap_with_fractional_carries() {
    const BATCH_CORE: u64 = 7;
    const K: u64 = 2;
    for at_cap in [false, true] {
        let mut machine =
            setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
        prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
        machine.bus_rem = 2;
        with_cpu_and_bus(&mut machine, |cpu, _| {
            let poll = cpu.poll_loop().expect("warm direct setup poll");
            cpu.commit_poll_skip_core(poll, poll.raw_core_clocks(), 1)
                .expect("seed nonzero CPU timing carry");
            assert_ne!(cpu.poll_skip_timing_remainder(), 0);
            cpu.reset_perf_counters();
        });
        assert_ne!(machine.bus_rem, 0);

        let exact_cap = projected_poll_total(&mut machine, K + 1, BATCH_CORE);
        let cap = if at_cap { exact_cap } else { exact_cap - 1 };
        let charged = attempt_poll_skip(&mut machine, BATCH_CORE, cap);
        if at_cap {
            assert!(charged.is_some(), "K+1 reservation must fit at equality");
            assert_eq!(machine.cpu.perf_counters().poll_skip_spans, 1);
            assert_eq!(machine.cpu.perf_counters().poll_skip_iterations, K);
        } else {
            assert!(charged.is_none(), "K+1 reservation crossed the cap");
            assert_eq!(machine.cpu.perf_counters().poll_skip_spans, 0);
            assert_eq!(machine.cpu.perf_counters().poll_skip_iterations, 0);
        }
    }
}

#[cfg(feature = "jit")]
#[test]
fn memory_poll_skip_returns_a_wide_epoch_two_charge_after_precommit() {
    const ITERATIONS: u64 = 1_500_000_000;

    fn prepared_machine() -> Machine {
        let mut machine = memory_poll_machine(
            true,
            false,
            MEMORY_POLL_CELL_DISP,
            MEMORY_POLL_COMPARAND ^ 0x55aa,
        );
        machine.set_timing_epoch_for_test(2);
        with_cpu_and_bus(&mut machine, |cpu, bus| {
            for _ in 0..8 {
                cpu.run_budgeted(bus, 500).expect("warm memory poll spin");
            }
        });
        machine.cpu.registers.eip = MEMORY_POLL_HEAD_EIP;
        machine.cpu.poll_skip_backedge_housekeeping();
        assert!(machine.cpu.poll_skip_eligible());
        assert_eq!(
            machine.cpu.poll_loop().expect("warm memory poll").family(),
            izarravm_cpu::PollFamily::Memory
        );
        machine.cpu.reset_perf_counters();
        machine
    }

    fn memory_projection(machine: &mut Machine, iterations: u64) -> (u64, u64, u64, u64) {
        with_cpu_and_bus(machine, |cpu, bus| {
            let poll = cpu.poll_loop().expect("warm memory poll descriptor");
            let linear = poll.memory_cell_linear().expect("memory poll cell");
            let physical = cpu
                .probe_linear_read_physical(linear)
                .expect("identity-mapped memory poll cell");
            let certificate = bus
                .poll_memory_bus_certificate(poll, physical)
                .expect("plain RAM memory poll certificate");
            let raw_core = cpu.poll_skip_raw_core_clocks(poll, bus);
            let core = cpu
                .project_poll_skip_core(raw_core, iterations)
                .expect("memory poll core projection");
            let bus_total = bus
                .poll_project_scaled_bus_clocks(certificate, iterations)
                .expect("memory poll bus projection");
            (
                raw_core,
                certificate.raw_clocks_per_iteration(),
                core,
                bus_total,
            )
        })
    }

    let mut admitted = prepared_machine();
    let (raw_core, raw_bus, expected_core, expected_bus) =
        memory_projection(&mut admitted, ITERATIONS);
    assert_eq!(raw_core, 40, "epoch-2 I586 memory poll raw core shape");
    let (_, _, reserved_core, reserved_bus) = memory_projection(&mut admitted, ITERATIONS + 1);
    let reserved_total = reserved_core + reserved_bus;
    let elapsed_before = admitted.cpu.elapsed_clocks;
    let remainder_before = admitted.cpu.poll_skip_timing_remainder();
    let trace_before = admitted.trace.elapsed_clocks();
    let cap = reserved_total;
    let charged = attempt_poll_skip(&mut admitted, 0, cap)
        .expect("the full N+1 memory reservation admits N complete iterations");
    assert_eq!(charged, expected_core);
    assert!(charged > u64::from(u32::MAX));
    assert_eq!(admitted.cpu.elapsed_clocks, elapsed_before + expected_core);
    let (num, den) = izarravm_cpu::level_timing_for_test(admitted.cpu.persona());
    assert_eq!(
        expected_core,
        (raw_core * ITERATIONS * u64::from(num) + remainder_before) / u64::from(den)
    );
    let expected_remainder =
        (raw_core * ITERATIONS * u64::from(num) + remainder_before) % u64::from(den);
    assert_eq!(
        admitted.cpu.poll_skip_timing_remainder(),
        expected_remainder
    );
    assert_eq!(
        admitted.trace.elapsed_clocks(),
        trace_before + raw_bus * ITERATIONS
    );
    assert_eq!(
        admitted.port_bus_batch_clocks, 0,
        "memory poll has no ISA lane"
    );
    assert_eq!(
        admitted.cpu.perf_counters().poll_skip_iterations,
        ITERATIONS
    );
    assert_eq!(admitted.cpu.poll_skip_memory().iterations, ITERATIONS);
    assert_eq!(run::checked_batch_core_sum(0, charged), charged);
    assert_eq!(bus_timing(admitted.cpu.persona(), 2), (1, 1));
    assert_eq!(
        expected_bus,
        raw_bus * ITERATIONS,
        "epoch-2 I586 bus projection retains the certified raw memory lane"
    );

    let smaller_cap = cap - 1;
    let mut smaller = prepared_machine();
    let smaller_charged = attempt_poll_skip(&mut smaller, 0, smaller_cap)
        .expect("a lower cap may still admit a smaller complete memory span");
    let smaller_iterations = smaller.cpu.perf_counters().poll_skip_iterations;
    assert!(smaller_iterations < ITERATIONS);
    let mut smaller_projection = prepared_machine();
    let (_, _, smaller_core, smaller_bus) =
        memory_projection(&mut smaller_projection, smaller_iterations + 1);
    assert_eq!(
        smaller_charged,
        memory_projection(&mut smaller_projection, smaller_iterations).2
    );
    assert!(smaller_core + smaller_bus <= smaller_cap);

    let mut minimum = prepared_machine();
    let minimum_cap =
        memory_projection(&mut minimum, 3).2 + memory_projection(&mut minimum, 3).3 - 1;
    let registers_before = minimum.cpu.registers.clone();
    let elapsed_before = minimum.cpu.elapsed_clocks;
    let remainder_before = minimum.cpu.poll_skip_timing_remainder();
    let trace_before = minimum.trace.elapsed_clocks();
    let port_before = minimum.port_bus_batch_clocks;
    assert!(attempt_poll_skip(&mut minimum, 0, minimum_cap).is_none());
    assert_eq!(minimum.cpu.registers, registers_before);
    assert_eq!(minimum.cpu.elapsed_clocks, elapsed_before);
    assert_eq!(minimum.cpu.poll_skip_timing_remainder(), remainder_before);
    assert_eq!(minimum.trace.elapsed_clocks(), trace_before);
    assert_eq!(minimum.port_bus_batch_clocks, port_before);
    assert_eq!(minimum.cpu.perf_counters().poll_skip_spans, 0);
    assert_eq!(minimum.cpu.perf_counters().poll_skip_iterations, 0);
}

#[test]
fn machine_batch_fold_keeps_wide_normal_and_halt_outcomes() {
    let wide = u64::from(u32::MAX) + 29;
    let normal = CpuCycleOutcome {
        core_clocks: wide,
        halted: false,
    };
    let halted = CpuCycleOutcome {
        core_clocks: wide,
        halted: true,
    };

    assert_eq!(
        run::checked_batch_core_sum(11, normal.core_clocks),
        wide + 11
    );
    assert_eq!(
        run::checked_batch_core_sum(13, halted.core_clocks),
        wide + 13
    );
    assert!(!normal.halted);
    assert!(halted.halted);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_edge_is_strict_and_the_reserved_final_iteration_runs_real() {
    const K: u64 = 2;
    fn edge_machine() -> Machine {
        let mut machine =
            setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
        prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
        move_beam_to(&mut machine, 0);
        machine
    }

    let mut exact = edge_machine();
    let candidate_dots = projected_poll_dots(&mut exact, K, 0);
    let edge = exact
        .vega
        .dots_until_status1_bit_change_from(0, 3, true)
        .expect("vertical retrace start");
    assert!(edge > candidate_dots + 1);
    let cap = projected_poll_total(&mut exact, K + 1, 0);
    move_beam_to(&mut exact, edge - candidate_dots);
    assert!(attempt_poll_skip(&mut exact, 0, cap).is_none());
    assert_eq!(exact.cpu.perf_counters().poll_skip_iterations, 0);

    let mut after = edge_machine();
    let cap = projected_poll_total(&mut after, K + 1, 0);
    move_beam_to(&mut after, edge - candidate_dots - 1);
    let mut diagnostics = run::PollSkipDiagnostics::default();
    let (charged, retired, final_eip) = with_cpu_and_bus(&mut after, |cpu, bus| {
        bus.prior_runs_core_clocks = 0;
        bus.core_clocks_so_far = 0;
        let poll = run::classify_poll_skip_boundary(cpu, &mut diagnostics)
            .expect("warm poll before bulk commit");
        let certificate = bus
            .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
            .expect("RAM poll certificate before bulk commit");
        let charged = run::try_poll_skip(cpu, bus, &mut diagnostics, poll, 0, cap)
            .expect("edge one dot after the bulk boundary admits K");
        assert_eq!(cpu.perf_counters().poll_skip_iterations, K);

        let mut batch_core = charged;
        let before = cpu.perf_counters().instructions;
        for _ in 0..6 {
            let spent_bus = bus
                .poll_project_scaled_bus_clocks(certificate, 0)
                .expect("committed bus projection");
            let spent = batch_core + spent_bus;
            let remaining = cap.checked_sub(spent).expect("K+1 reserved tail");
            bus.prior_runs_core_clocks = batch_core;
            let outcome = cpu
                .run_budgeted(bus, remaining)
                .expect("reserved real iteration run");
            batch_core = batch_core
                .checked_add(outcome.consumed_core_clocks)
                .expect("reserved batch core total");
            if cpu.registers.eip == 0x114 || outcome.halted {
                break;
            }
        }
        (
            charged,
            cpu.perf_counters().instructions - before,
            cpu.registers.eip,
        )
    });
    assert_ne!(charged, 0);
    assert!(retired >= 5, "the complete setup poll iteration retired");
    assert_ne!(final_eip, 0x10b, "the real edge iteration exited the loop");
}

#[cfg(feature = "jit")]
#[test]
fn pending_interrupt_preempts_setup_poll_skip_before_any_bulk_commit() {
    const IRQ_HANDLER: u32 = 0x300;
    let mut baseline = setup_poll_machine(false, false, false, 0x1234_03da);
    let mut skipped = setup_poll_machine(true, false, false, 0x1234_03da);
    skipped.poll_skip_enabled = false;
    baseline.run_cycles(1_000).unwrap();
    skipped.run_cycles(1_000).unwrap();
    for machine in [&mut baseline, &mut skipped] {
        machine.cpu.registers.eip = 0x10b;
        machine.cpu.poll_skip_backedge_housekeeping();
        machine.write_physical_u16(8 * 4, IRQ_HANDLER as u16);
        machine.write_physical_u16(8 * 4 + 2, 0);
        machine.write_physical_u8(IRQ_HANDLER, 0xf4);
        machine.pic.write_port(0x20, 0x11);
        machine.pic.write_port(0x21, 0x08);
        machine.pic.write_port(0x21, 0x04);
        machine.pic.write_port(0x21, 0x01);
        machine.pic.set_irq_level(0, true);
        machine.cpu.registers.eflags |= 0x0200; // IF
        machine.cpu.reset_perf_counters();
    }
    skipped.poll_skip_enabled = true;

    let baseline_stop = baseline.run_until_halt_or_cycles(10_000).unwrap();
    let skipped_stop = skipped.run_until_halt_or_cycles(10_000).unwrap();
    assert_eq!(baseline_stop, StopReason::Halted);
    assert_eq!(skipped_stop, baseline_stop);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_iterations, 0);
    assert_poll_machine_boundary_eq(&skipped, &baseline);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_declines_when_the_vga_subsystem_is_disabled() {
    let mut baseline = poll_skip_test_machine(false, TracingMode::Off, GswMode::Gsw586, 0x08);
    let mut skipped = poll_skip_test_machine(true, TracingMode::Off, GswMode::Gsw586, 0x08);
    for machine in [&mut baseline, &mut skipped] {
        with_bus(machine, |bus| {
            bus.write_io(0x3c3, BusWidth::Byte, 0, false).unwrap();
        });
        assert!(!machine.vega.poll_skip_status1_port_active());
        machine.cpu.reset_perf_counters();
    }

    let baseline_stop = baseline.run_cycles(50_000).unwrap();
    let skipped_stop = skipped.run_cycles(50_000).unwrap();
    assert_eq!(skipped_stop, baseline_stop);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_iterations, 0);
    assert_poll_machine_boundary_eq(&skipped, &baseline);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_declines_for_a_hercules_color_status_alias() {
    let mut baseline = poll_skip_test_machine(false, TracingMode::Off, GswMode::Gsw586, 0x08);
    let mut skipped = poll_skip_test_machine(true, TracingMode::Off, GswMode::Gsw586, 0x08);
    for machine in [&mut baseline, &mut skipped] {
        machine.video_mut().set_mono_text_mode();
        with_bus(machine, |bus| {
            bus.write_io(0x3bf, BusWidth::Byte, 0x01, false).unwrap();
            bus.write_io(0x3b8, BusWidth::Byte, 0x0a, false).unwrap();
            bus.write_io(0x3c2, BusWidth::Byte, 0x03, false).unwrap();
        });
        assert!(machine.vega.legacy().is_hercules_personality());
        assert!(machine.vega.legacy().color_status1_port_active());
        assert!(!machine.vega.poll_skip_status1_port_active());
        machine.cpu.reset_perf_counters();
    }

    let baseline_stop = baseline.run_cycles(50_000).unwrap();
    let skipped_stop = skipped.run_cycles(50_000).unwrap();
    assert_eq!(skipped_stop, baseline_stop);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_iterations, 0);
    assert_poll_machine_boundary_eq(&skipped, &baseline);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_preserves_a_live_cs_limit_fault_after_the_in() {
    const GP_HANDLER: u32 = 0x300;
    let mut baseline = poll_skip_test_machine(false, TracingMode::Off, GswMode::Gsw586, 0x08);
    let mut skipped = poll_skip_test_machine(true, TracingMode::Off, GswMode::Gsw586, 0x08);

    skipped.poll_skip_enabled = false;
    baseline.run_cycles(1_000).unwrap();
    skipped.run_cycles(1_000).unwrap();
    for machine in [&mut baseline, &mut skipped] {
        machine.write_physical_u16(13 * 4, GP_HANDLER as u16);
        machine.write_physical_u16(13 * 4 + 2, 0);
        machine.write_physical_u8(GP_HANDLER, 0xf4);
        machine.cpu.registers.eip = 0x108;
        machine.cpu.poll_skip_backedge_housekeeping();
        let mut cs = machine.cpu.registers.cs();
        cs.limit = 0x109;
        machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
        assert!(machine.cpu.poll_loop().is_none());
        machine.cpu.reset_perf_counters();
    }
    skipped.poll_skip_enabled = true;
    let baseline_before = baseline.cpu.elapsed_clocks;
    let skipped_before = skipped.cpu.elapsed_clocks;

    let baseline_stop = baseline.run_until_halt_or_cycles(10_000).unwrap();
    let skipped_stop = skipped.run_until_halt_or_cycles(10_000).unwrap();
    assert_eq!(baseline_stop, StopReason::Halted);
    assert_eq!(skipped_stop, baseline_stop);
    assert!(baseline.cpu.elapsed_clocks > baseline_before);
    assert!(skipped.cpu.elapsed_clocks > skipped_before);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_iterations, 0);
    assert_poll_machine_boundary_eq(&skipped, &baseline);
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_replays_the_attribute_flip_flop_reset_guest_visibly() {
    const PROGRAM: &[u8] = &[
        0xba, 0xc0, 0x03, 0x00, 0x00, // mov edx,3C0h
        0xb0, 0x12, // mov al,12h
        0xee, // out dx,al: select register 12h and leave the data phase armed
        0xba, 0xda, 0x03, 0x00, 0x00, // mov edx,3DAh
        0xec, 0xa8, 0x08, 0x75, 0xfb, // wait while retrace is set
        0xec, 0xa8, 0x08, 0x74, 0xfb, // wait until retrace is set
        0xba, 0xc0, 0x03, 0x00, 0x00, // mov edx,3C0h
        0xb0, 0x05, 0xee, // next write must select attribute register 5
        0xb0, 0x2a, 0xee, // write its data
        0xba, 0xc1, 0x03, 0x00, 0x00, // mov edx,3C1h
        0xec, // in al,dx: read attribute register 5
        0xf4, // hlt
    ];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut baseline = Machine::new_raw_program(profile.clone(), PROGRAM).unwrap();
    let mut skipped = Machine::new_raw_program(profile, PROGRAM).unwrap();
    for machine in [&mut baseline, &mut skipped] {
        let mut cs = machine.cpu.registers.cs();
        cs.default_size_32 = true;
        machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
        machine.cpu.set_native_backend_enabled(false);
        machine.trace.set_tracing_mode(TracingMode::Off);
    }
    baseline.poll_skip_enabled = false;
    skipped.poll_skip_enabled = true;

    let baseline_stop = baseline.run_until_halt_or_cycles(10_000_000).unwrap();
    let skipped_stop = skipped.run_until_halt_or_cycles(10_000_000).unwrap();
    assert_eq!(baseline_stop, StopReason::Halted);
    assert_eq!(skipped_stop, baseline_stop);
    assert!(skipped.cpu.perf_counters().poll_skip_spans > 0);
    assert_eq!(baseline.cpu.registers.eax() as u8, 0x2a);
    assert_eq!(skipped.cpu.registers.eax() as u8, 0x2a);
    assert_poll_machine_boundary_eq(&skipped, &baseline);
}

#[cfg(feature = "jit")]
#[test]
fn natural_event_caps_eventually_offer_a_poll_loop_head() {
    let mut machine = poll_skip_test_machine(true, TracingMode::Off, GswMode::Gsw586, 0x08);
    for _ in 0..32 {
        machine.run_cycles(100_000).unwrap();
        if machine.cpu.perf_counters().poll_skip_spans != 0 {
            return;
        }
    }
    panic!(
        "32 natural caps never offered IN at a warm loop head; final eip={:08x}",
        machine.cpu.registers.eip
    );
}

// Program offset of the self-loop head warmed by the non-poll machine helpers.
// It is the .COM origin, so its linear (CS.base + this) is also its physical byte
// in real mode, which the SMC test in `code_write_retires_negative...` relies on.
#[cfg(feature = "jit")]
const NON_POLL_HEAD_OFFSET: u32 = 0x100;

// Warm a 32-bit self-loop whose head matches no certified poll shape, so
// classifying it is a cacheable structural negative. `poll_skip` stays off so
// warming never classifies: the tests own the first scan. The caller picks the
// loop body so the head opcode lands on either side of the loop-head prefilter.
#[cfg(feature = "jit")]
fn warm_non_poll_loop_machine(program: &[u8]) -> Machine {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, program).unwrap();
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.set_native_backend_enabled(false);
    machine.poll_skip_enabled = false;
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine.cpu.registers.eip = NON_POLL_HEAD_OFFSET;
    machine.cpu.poll_skip_backedge_housekeeping();
    machine.run_cycles(1_000).unwrap();
    machine
}

// A warm `mov edx,ebx; mov eax,ebx; jmp head` loop. This builds a 3-slot
// [0x89, 0x89, 0xEB] is_loop block that matches no certified poll shape, so the
// scan records a cacheable structural negative. The head opcode 0x89 is inside
// the loop-head prefilter set, so the boundary reaches the scan rather than being
// rejected up front. Reused by the negative-cache hit and SMC-retire tests, which
// need a warm non-poll loop whose head clears the prefilter.
#[cfg(feature = "jit")]
fn warm_in_set_non_poll_machine() -> Machine {
    warm_non_poll_loop_machine(&[
        0x89, 0xda, // mov edx, ebx  (0x89 in set)
        0x89, 0xd8, // mov eax, ebx  (0x89 in set)
        0xeb, 0xfa, // jmp -6 back to the head
    ])
}

// A warm `inc eax; inc eax; jmp head` loop. The head opcode 0x40 is outside the
// loop-head prefilter set, so the prefilter rejects the boundary before any scan
// or cache probe. Used by the prefilter-reject tests.
#[cfg(feature = "jit")]
fn warm_out_of_set_non_poll_machine() -> Machine {
    warm_non_poll_loop_machine(&[
        0x40, // inc eax
        0x40, // inc eax
        0xeb, 0xfc, // jmp -4 back to the head
    ])
}

#[cfg(feature = "jit")]
#[test]
fn poll_negative_cache_answers_repeat_scans() {
    // A structural (code-byte-only) negative is recorded on the first scan and
    // answered from the page-generation cache on the next scan at the same
    // linear, without a second full backward scan.
    let mut machine = warm_in_set_non_poll_machine();
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.reset_perf_counters();
    machine.cpu.registers.eip = NON_POLL_HEAD_OFFSET;
    machine.cpu.poll_skip_backedge_housekeeping();

    assert!(machine.cpu.poll_loop().is_none());
    assert_eq!(machine.cpu.perf_counters().poll_head_prefilter_rejects, 0);
    let stores = machine.cpu.perf_counters().poll_neg_cache_stores;
    let hits = machine.cpu.perf_counters().poll_neg_cache_hits;
    assert!(stores >= 1, "the first scan records a structural negative");

    // Same linear, no code write in between, so the live negative answers it.
    assert!(machine.cpu.poll_loop().is_none());
    assert_eq!(
        machine.cpu.perf_counters().poll_neg_cache_stores,
        stores,
        "the repeat scan is answered from the cache, not re-recorded"
    );
    assert_eq!(machine.cpu.perf_counters().poll_neg_cache_hits, hits + 1);
}

#[cfg(feature = "jit")]
#[test]
fn edx_dependent_negative_is_not_cached() {
    // The 3-slot direct shape (IN AL,DX; TEST AL,imm8; Jcc back) is structural,
    // but its port comes from live EDX. A non-3DA EDX is a volatile negative that
    // must never be cached, because the same bytes classify as a poll the moment
    // EDX addresses the 3DA status port.
    let mut machine = poll_skip_test_machine(false, TracingMode::Off, GswMode::Gsw586, 0x08);
    machine.run_cycles(1_000).unwrap();
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.reset_perf_counters();

    // Head of the warm 3-slot phase, with EDX not pointing at the 3DA port.
    machine.cpu.registers.set_edx(0x0000_0100);
    machine.cpu.registers.eip = 0x108;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(machine.cpu.poll_loop().is_none());
    assert_eq!(machine.cpu.perf_counters().poll_neg_cache_stores, 0);
    assert!(machine.cpu.perf_counters().poll_neg_cache_volatile >= 1);

    // The identical bytes classify as a poll once EDX addresses the 3DA port,
    // proving the earlier volatile negative had to stay uncached.
    machine.cpu.registers.set_edx(0x0000_03da);
    machine.cpu.registers.eip = 0x108;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(machine.cpu.poll_loop().is_some());
}

#[cfg(feature = "jit")]
#[test]
fn code_write_retires_negative_and_new_poll_is_recognized() {
    // A negative is keyed on the page's insert generation. A guest code write
    // that installs a real poll shape at the same linear must retire the stale
    // negative (its re-decode bumps the page generation), or a legitimate new
    // poll would be silently suppressed.
    const HEAD: u32 = NON_POLL_HEAD_OFFSET;
    // Certified 5-slot setup-direct poll: mov edx,ebx; sub eax,eax; in al,dx;
    // test al,8; jnz -9 back to the head. EBX supplies the 3DA port downstream;
    // this shape's classification is structural (no register gate).
    const SETUP_DIRECT: &[u8] = &[0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x75, 0xf7];
    const SETUP_STARTS: &[u32] = &[0, 2, 4, 5, 7];

    let mut machine = warm_in_set_non_poll_machine();
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.registers.eip = HEAD;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(
        machine.cpu.poll_loop().is_none(),
        "the non-poll head classifies as a negative"
    );
    assert!(machine.cpu.perf_counters().poll_neg_cache_stores >= 1);

    // SMC: overwrite the head with the setup-direct poll shape through the normal
    // recorded write path, which invalidates the stale decode lines at HEAD.
    let base = machine.cpu.registers.cs().base;
    for (offset, byte) in SETUP_DIRECT.iter().copied().enumerate() {
        machine.write_physical_u8(base + HEAD + offset as u32, byte);
    }
    // Re-warm the new shape so its decode lines exist. Each fresh decode is a
    // `put`, which bumps the head page's insert generation and retires the
    // negative. EBX/EDX address the 3DA port so IN AL,DX does not fault.
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        cpu.registers.set_ebx(0x0000_03da);
        cpu.registers.set_edx(0x0000_03da);
        for offset in SETUP_STARTS {
            cpu.registers.eip = HEAD + offset;
            cpu.poll_skip_backedge_housekeeping();
            cpu.run_budgeted(bus, 0)
                .expect("warm the new setup poll slot");
        }
    });

    // Reclassify at the SAME linear the negative was cached for.
    machine.cpu.registers.set_ebx(0x0000_03da);
    machine.cpu.registers.eip = HEAD;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(
        machine.cpu.poll_loop().is_some(),
        "a stale negative suppressed a legitimate new poll shape"
    );
}

#[cfg(feature = "jit")]
#[test]
fn out_of_set_boundary_is_prefilter_rejected_without_cache_traffic() {
    // A warm head whose opcode (0x40) no certified shape slot can carry is
    // rejected on the opcode peek, before any cache probe or backward scan.
    let mut machine = warm_out_of_set_non_poll_machine();
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.registers.eip = NON_POLL_HEAD_OFFSET;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(machine.cpu.poll_loop().is_none());
    let perf = machine.cpu.perf_counters();
    assert!(perf.poll_head_prefilter_rejects >= 1);
    assert_eq!(perf.poll_neg_cache_stores, 0);
    assert_eq!(perf.poll_neg_cache_hits, 0);
    let rejects = perf.poll_head_prefilter_rejects;
    assert!(machine.cpu.poll_loop().is_none());
    assert_eq!(
        machine.cpu.perf_counters().poll_head_prefilter_rejects,
        rejects + 1
    );
    assert_eq!(machine.cpu.perf_counters().poll_neg_cache_hits, 0);
}

#[cfg(feature = "jit")]
#[test]
fn cold_boundary_is_prefilter_rejected() {
    // EIP at never-executed bytes: the decode line is cold, so no shape can
    // contain it and the prefilter answers without a scan.
    let mut machine = warm_out_of_set_non_poll_machine();
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.registers.eip = NON_POLL_HEAD_OFFSET + 0x40;
    machine.cpu.poll_skip_backedge_housekeeping();
    let before = machine.cpu.perf_counters().poll_head_prefilter_rejects;
    assert!(machine.cpu.poll_loop().is_none());
    assert_eq!(
        machine.cpu.perf_counters().poll_head_prefilter_rejects,
        before + 1
    );
}

#[cfg(feature = "jit")]
#[test]
fn sixteen_bit_code_is_prefilter_rejected() {
    // d = false matches no certified shape; eligibility passes in real mode, so
    // the prefilter is the rejector even when the head opcode (0x89) is in-set.
    // A 16-bit real-mode fixture: same shape as the in-set helper but with the
    // code segment left at its default 16-bit width, so decode d is false.
    let program = [
        0x89, 0xda, // mov dx, bx  (0x89 in set, but 16-bit code)
        0xeb, 0xfc, // jmp -4 back to the head
    ];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.cpu.set_native_backend_enabled(false);
    machine.poll_skip_enabled = false;
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine.cpu.registers.eip = NON_POLL_HEAD_OFFSET;
    machine.cpu.poll_skip_backedge_housekeeping();
    machine.run_cycles(1_000).unwrap();

    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.registers.eip = NON_POLL_HEAD_OFFSET;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(
        machine.cpu.poll_skip_eligible(),
        "real mode with IOPL clearance must stay poll-skip eligible so d=false is the rejector"
    );
    let before = machine.cpu.perf_counters().poll_head_prefilter_rejects;
    assert!(machine.cpu.poll_loop().is_none());
    assert_eq!(
        machine.cpu.perf_counters().poll_head_prefilter_rejects,
        before + 1
    );
}

#[cfg(feature = "jit")]
#[test]
fn cache_on_and_off_commit_identical_spans() {
    // A stale negative may only ever SUPPRESS a skip, never change committed
    // state, so a scenario that really skips must commit the identical span set,
    // iteration count, instruction count, and registers with the cache on or off.
    let run = |cache: bool| {
        let mut machine =
            setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
        prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
        machine.cpu.set_poll_neg_cache_enabled_for_test(cache);
        let mut halted = false;
        for _ in 0..128 {
            if machine.run_cycles(100_000).unwrap() == StopReason::Halted {
                halted = true;
                break;
            }
        }
        assert!(halted, "the setup poll never reached its edge");
        (
            machine.cpu.perf_counters().poll_skip_spans,
            machine.cpu.perf_counters().poll_skip_iterations,
            machine.cpu.perf_counters().instructions,
            machine.cpu.registers.clone(),
        )
    };
    let with_cache = run(true);
    let without_cache = run(false);
    assert!(with_cache.0 > 0, "scenario must actually skip");
    assert_eq!(with_cache, without_cache);
}

#[test]
fn compiled_window_requires_approximate_timing_and_trace_off() {
    let mut machine = test_machine();
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine.set_mode(GswMode::Gsw386);
    assert!(with_bus(&mut machine, |bus| bus.begin_compiled_window()).is_none());

    machine.set_mode(GswMode::Gsw486);
    machine.trace.set_tracing_mode(TracingMode::Full);
    assert!(with_bus(&mut machine, |bus| bus.begin_compiled_window()).is_none());

    machine.trace.set_tracing_mode(TracingMode::Off);
    let epoch = machine.direct_mapping_epoch;
    let window = with_bus(&mut machine, |bus| bus.begin_compiled_window()).unwrap();
    assert_eq!(window.mapping_epoch(), epoch);
    assert_eq!(window.tracing_mode(), TracingMode::Off);
}

#[test]
fn compiled_window_finish_applies_one_exact_aggregate_charge() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    assert!(machine.set_vga_mode(0x13));
    machine.trace.set_tracing_mode(TracingMode::Off);

    with_bus(&mut machine, |bus| {
        bus.read_memory(0x2000, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        let window = bus.begin_compiled_window().unwrap();
        assert_eq!(
            window.batch_raw_clocks(),
            bus.trace.elapsed_clocks() - bus.trace_elapsed_at_batch_start
        );
        let mut delta = CompiledBusDelta::default();
        delta.add_instruction_fetches(5);
        delta.add_ram_accesses(BusWidth::Byte, 2);
        delta.add_ram_accesses(BusWidth::Dword, 3);
        delta.add_vga_reads(BusWidth::Word, 4);
        delta.add_vga_writes(NativeVgaWrites {
            dirty_pages: 0b0010,
            byte_writes: 7,
            word_writes: 1,
            dword_writes: 2,
        });
        let expected = window.delta_raw_clocks(&delta);
        let before = bus.trace.elapsed_clocks();
        bus.finish_compiled_window(window, delta);
        assert_eq!(bus.trace.elapsed_clocks() - before, expected);
    });
}

#[test]
fn empty_compiled_window_finishes_without_a_vga_direct_aperture() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.trace.set_tracing_mode(TracingMode::Off);

    with_bus(&mut machine, |bus| {
        let window = bus.begin_compiled_window().unwrap();
        let before = bus.trace.elapsed_clocks();
        bus.finish_compiled_window(window, CompiledBusDelta::default());
        assert_eq!(bus.trace.elapsed_clocks(), before);
    });
}

#[test]
fn direct_page_epoch_advances_for_a20_and_vga_mapping_changes() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    machine.trace.set_tracing_mode(TracingMode::Off);
    let initial = machine.direct_mapping_epoch;
    let first_page = with_bus(&mut machine, |bus| {
        bus.direct_page(0x2000, BusAccessKind::DataRead)
            .unwrap()
            .unwrap()
    });
    assert_eq!(first_page.mapping_epoch, initial);

    with_bus(&mut machine, |bus| {
        bus.write_io(0x92, BusWidth::Byte, 0, false).unwrap();
    });
    let after_a20 = machine.direct_mapping_epoch;
    assert_ne!(after_a20, initial);
    let second_page = with_bus(&mut machine, |bus| {
        bus.direct_page(0x2000, BusAccessKind::DataRead)
            .unwrap()
            .unwrap()
    });
    assert_eq!(second_page.mapping_epoch, after_a20);

    assert!(machine.set_vga_mode(0x13));
    assert_ne!(machine.direct_mapping_epoch, after_a20);
}

#[test]
fn native_cached_fetch_batch_charges_the_exact_warm_ram_cost() {
    const FETCHES: u64 = 25_000;
    const FETCH_LENS: &[u8] = &[1, 3, 2, 4];
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);

    with_bus(&mut machine, |bus| {
        let clocks_before = bus.trace.elapsed_clocks();
        let fetch_cost = bus.jit_fetch_cost_clocks();
        assert!(bus.charge_native_cached_fetches(0xF_4000, 0x100, FETCH_LENS, FETCHES));
        assert_eq!(
            bus.trace.elapsed_clocks() - clocks_before,
            fetch_cost * FETCHES * FETCH_LENS.len() as u64
        );
    });
}

#[test]
fn native_deadline_bound_uses_the_same_bus_scale_as_batch_accounting() {
    const RAW_CLOCKS: u64 = 301;
    let mut machine = test_machine();

    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        machine.set_mode(mode);
        let (num, den) = bus_timing(mode.persona(), machine.timing_epoch);
        let expected = RAW_CLOCKS
            .saturating_mul(u64::from(num))
            .saturating_add(u64::from(den) - 1)
            / u64::from(den);
        with_bus(&mut machine, |bus| {
            assert_eq!(bus.jit_scale_bus_cost_upper(RAW_CLOCKS), expected);
        });
    }
}

#[test]
fn rep_page_walk_bound_covers_four_scaled_page_table_cycles() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        for address in [0x3000, LOW_BIOS_BASE, 0xA_1000] {
            let mut machine = test_machine();
            machine.set_mode(mode);
            assert!(machine.set_vga_mode(0x13));
            with_bus(&mut machine, |bus| {
                let bound = bus
                    .rep_page_walk_cost_upper()
                    .expect("MachineBus supplies a cold page-walk bound");
                let before = bus.in_batch_scaled_bus_clocks();
                bus.read_memory(address, BusWidth::Dword, BusAccessKind::PageWalkRead)
                    .unwrap();
                bus.write_memory(address, BusWidth::Dword, 0, BusAccessKind::PageWalkWrite)
                    .unwrap();
                bus.read_memory(address, BusWidth::Dword, BusAccessKind::PageWalkRead)
                    .unwrap();
                bus.write_memory(address, BusWidth::Dword, 0, BusAccessKind::PageWalkWrite)
                    .unwrap();
                let growth = bus.in_batch_scaled_bus_clocks() - before;
                assert!(
                    growth <= bound,
                    "{mode:?} address {address:#x}: growth {growth}, bound {bound}"
                );
            });
        }
    }
}

#[test]
fn native_cached_fetch_batch_observes_the_linear_stub_address() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    machine.last_int_vector = Some(0x10);

    with_bus(&mut machine, |bus| {
        assert!(bus.charge_native_cached_fetches(BIOS_LEGACY_IRET_LINEAR, 0x5000, &[1], 4,));
    });

    assert_eq!(machine.pending_soft_int, Some(0x10));
    assert_eq!(machine.last_int_vector, None);
}

const NATIVE_FETCH_LINEAR: u32 = 0xF_4000;
const NATIVE_FETCH_PHYSICAL: u32 = 0x5000;

fn arm_native_fetch_loop(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = NATIVE_FETCH_LINEAR;
    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_esp(0);
    cpu.registers.set_ebp(0);
    cpu.registers.set_esi(0);
    cpu.registers.set_edi(0);
    cpu.registers.eflags = 0x203;
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    cs.access = 0x9b;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
}

fn drive_native_fetch_loop(cpu: &mut CpuGsw, machine: &mut Machine) -> Vec<CpuCycleOutcome> {
    let mut outcomes = Vec::new();
    for _ in 0..64 {
        let outcome = with_bus(machine, |bus| cpu.run_straight_line(bus, u64::MAX).unwrap());
        outcomes.push(outcome);
        if outcome.halted {
            return outcomes;
        }
    }
    panic!("native fetch loop did not halt");
}

#[test]
#[cfg(feature = "jit")]
fn direct_large_self_loop_bulk_fetch_uses_physical_paging_alias_timing() {
    const ITERATIONS: u32 = 1_000;
    const PROGRAM: [u8; 16] = [
        0xb9, 0xe8, 0x03, 0x00, 0x00, // mov ecx,1000
        0x83, 0xc0, 0x03, // add eax,3
        0x89, 0xc2, // mov edx,eax
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz to the loop body
        0xf4,
    ];
    let mut interp_machine = test_machine();
    let mut native_machine = test_machine();
    interp_machine.set_mode(GswMode::Gsw586);
    native_machine.set_mode(GswMode::Gsw586);
    for machine in [&mut interp_machine, &mut native_machine] {
        machine.write_physical_u32(0x1000, 0x2007);
        machine.write_physical_u32(
            0x2000 + ((NATIVE_FETCH_LINEAR >> 12) & 0x3FF) * 4,
            NATIVE_FETCH_PHYSICAL | 7,
        );
    }
    for (offset, byte) in PROGRAM.into_iter().enumerate() {
        interp_machine.write_physical_u8(NATIVE_FETCH_PHYSICAL + offset as u32, byte);
        native_machine.write_physical_u8(NATIVE_FETCH_PHYSICAL + offset as u32, byte);
    }
    let mut interp_cpu = interp_machine.cpu.clone();
    let mut native_cpu = native_machine.cpu.clone();
    for cpu in [&mut interp_cpu, &mut native_cpu] {
        cpu.control.cr0 |= 0x8000_0001;
        cpu.control.cr3 = 0x1000;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x08,
                base: 0,
                limit: u32::MAX,
                access: 0x9b,
                default_size_32: true,
            },
        );
    }
    interp_cpu.set_jit_auto_admit(false);
    native_cpu.set_jit_auto_admit(true);

    for _ in 0..4 {
        arm_native_fetch_loop(&mut interp_cpu);
        arm_native_fetch_loop(&mut native_cpu);
        drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
        drive_native_fetch_loop(&mut native_cpu, &mut native_machine);
    }
    interp_machine.trace = BusTrace::default();
    native_machine.trace = BusTrace::default();
    arm_native_fetch_loop(&mut interp_cpu);
    arm_native_fetch_loop(&mut native_cpu);
    let traced_direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let traced_direct_entries = native_cpu.perf_counters().jit_direct_entries;

    let interp_traced_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_traced_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_traced_outcomes, interp_traced_outcomes);
    assert_eq!(native_machine.trace, interp_machine.trace);
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns,
        traced_direct_insns
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_entries,
        traced_direct_entries
    );

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    arm_native_fetch_loop(&mut interp_cpu);
    arm_native_fetch_loop(&mut native_cpu);
    let direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let direct_entries = native_cpu.perf_counters().jit_direct_entries;

    let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_outcomes, interp_outcomes);
    // THE ARCHITECTURAL FLAGS FIRST, on the roles as they are -- before anything settles them, or
    // the assertion is a tautology -- then the whole structure on SETTLED CLONES.
    //
    // `registers.eflags` plus `pending_flags` is a REPRESENTATION of the flags, and the native role
    // settles that representation on the way into emitted code (`run_direct_block`'s entry clear)
    // where the interpreter role keeps its lazy pair. Both reach the same architectural value; only
    // the split between base and descriptor differs.
    //
    // `CpuGsw::settled` and NOT an inline `registers.eflags = eflags()`: the inline form leaves
    // `pending_flags` standing, and `CpuGsw` derives `PartialEq` over every field, so it would go on
    // byte-comparing the raw descriptor -- the exact invariant this campaign released -- surviving
    // at the only two sites reached across a crate boundary. It would also construct a state no code
    // path produces: a settled base with a live descriptor over it.
    assert_eq!(native_cpu.eflags(), interp_cpu.eflags());
    assert_eq!(native_cpu.settled(), interp_cpu.settled());
    assert_eq!(
        native_machine.trace.elapsed_clocks(),
        interp_machine.trace.elapsed_clocks()
    );
    assert_eq!(native_cpu.registers.eax(), ITERATIONS * 3);
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns - direct_insns,
        u64::from(ITERATIONS) * 4
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_entries - direct_entries,
        1
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn paged_fast_map_tlb_collision_keeps_interpreter_and_native_timing_equal() {
    const PAGE_DIRECTORY: u32 = 0x1000;
    const PAGE_TABLE: u32 = 0x2000;
    // The collision stride is TLB_ENTRIES pages, which is one whole page-directory entry once the
    // TLB has 1024 slots, so B cannot live in A's page table and needs a second one.
    const PAGE_TABLE_B: u32 = 0x3000;
    const WARM_CODE_LINEAR: u32 = 0x000f_4000;
    const WARM_CODE_PHYSICAL: u32 = 0x5000;
    const MEASURE_CODE_LINEAR: u32 = 0x000f_5000;
    const MEASURE_CODE_PHYSICAL: u32 = 0x8000;
    const LINEAR_A: u32 = 0x3000;
    const LINEAR_B: u32 = LINEAR_A + TLB_ENTRIES as u32 * 0x1000;
    const FRAME_A: u32 = 0x6000;
    const FRAME_B: u32 = 0x7000;
    const VALUE_A: u32 = 0x1020_3040;
    const VALUE_B: u32 = 0x5566_7788;
    const PDE_B: u32 = PAGE_DIRECTORY + (LINEAR_B >> 22) * 4;
    const PTE_A: u32 = PAGE_TABLE + ((LINEAR_A >> 12) & 0x3ff) * 4;
    const PTE_B: u32 = PAGE_TABLE_B + ((LINEAR_B >> 12) & 0x3ff) * 4;

    // A COUPLING check between the stride and the slot function, not an independent one: while
    // LINEAR_B is defined as one TLB_ENTRIES stride above LINEAR_A it cannot fail. That is still
    // worth having, because the previous form compared `(LINEAR >> 12) & 63` against a stride
    // hardcoded to 64, so both sides were literals from the same wrong assumption; they agreed
    // with each other while agreeing with nothing, and kept passing after the pages had stopped
    // colliding. Tying both sides to TLB_ENTRIES makes a reintroduced literal fail here, at the
    // cause, instead of downstream on an empty page-walk list. The real teeth are PTE_A != PTE_B
    // and the per-PTE walk assertions below.
    assert_ne!(LINEAR_A, LINEAR_B);
    assert_eq!(
        (LINEAR_A >> 12) as usize % TLB_ENTRIES,
        (LINEAR_B >> 12) as usize % TLB_ENTRIES
    );
    assert_ne!(PTE_A, PTE_B);

    let mut warm_program = vec![0xa1];
    warm_program.extend_from_slice(&LINEAR_A.to_le_bytes());
    warm_program.push(0xa1);
    warm_program.extend_from_slice(&LINEAR_B.to_le_bytes());
    warm_program.push(0xf4);

    let mut program = vec![0x90, 0xa1];
    program.extend_from_slice(&LINEAR_A.to_le_bytes());
    program.extend_from_slice(&[
        0x85, 0xc0, // test eax,eax
        0x74, 0xf7, // jz back to the entry, not taken for VALUE_A
        0xf4, // hlt
    ]);

    let make_fixture = || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw486);
        machine.write_physical_u32(PAGE_DIRECTORY, PAGE_TABLE | 7);
        machine.write_physical_u32(PDE_B, PAGE_TABLE_B | 7);
        machine.write_physical_u32(
            PAGE_TABLE + ((WARM_CODE_LINEAR >> 12) & 0x3ff) * 4,
            WARM_CODE_PHYSICAL | 7,
        );
        machine.write_physical_u32(
            PAGE_TABLE + ((MEASURE_CODE_LINEAR >> 12) & 0x3ff) * 4,
            MEASURE_CODE_PHYSICAL | 7,
        );
        machine.write_physical_u32(PTE_A, FRAME_A | 7);
        machine.write_physical_u32(PTE_B, FRAME_B | 7);
        machine.write_physical_u32(FRAME_A, VALUE_A);
        machine.write_physical_u32(FRAME_B, VALUE_B);
        for (offset, byte) in warm_program.iter().copied().enumerate() {
            machine.write_physical_u8(WARM_CODE_PHYSICAL + offset as u32, byte);
        }
        for (offset, byte) in program.iter().copied().enumerate() {
            machine.write_physical_u8(MEASURE_CODE_PHYSICAL + offset as u32, byte);
        }
        machine.trace = BusTrace::default();
        machine.trace.set_tracing_mode(TracingMode::Full);
        machine
    };
    let mut interp_machine = make_fixture();
    let mut native_machine = make_fixture();

    let configure_cpu = |machine: &Machine| {
        let mut cpu = machine.cpu.clone();
        cpu.control.cr0 |= 0x8000_0001;
        cpu.control.cr3 = PAGE_DIRECTORY;
        cpu.registers
            .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
        for segment in [
            SegmentIndex::Ds,
            SegmentIndex::Ss,
            SegmentIndex::Es,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ] {
            cpu.registers
                .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
        }
        cpu.set_jit_auto_admit(false);
        cpu
    };
    let mut interp_cpu = configure_cpu(&interp_machine);
    let mut native_cpu = configure_cpu(&native_machine);
    let arm = |cpu: &mut CpuGsw, eip: u32| {
        cpu.halted = false;
        cpu.registers.eip = eip;
        cpu.registers.set_eax(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_esp(0);
        cpu.registers.set_ebp(0);
        cpu.registers.set_esi(0);
        cpu.registers.set_edi(0);
        cpu.registers.eflags = 0x202;
    };

    for (cpu, machine) in [
        (&mut interp_cpu, &mut interp_machine),
        (&mut native_cpu, &mut native_machine),
    ] {
        arm(cpu, WARM_CODE_LINEAR);
        let outcomes = drive_native_fetch_loop(cpu, machine);
        assert!(outcomes.last().is_some_and(|outcome| outcome.halted));
        for pte in [PTE_A, PTE_B] {
            assert!(
                machine.trace.cycles().iter().any(|cycle| {
                    cycle.kind == BusAccessKind::PageWalkRead && cycle.address == pte
                }),
                "the cold warmup must walk PTE {pte:#x}"
            );
        }
    }

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    native_cpu.set_jit_auto_admit(true);
    for _ in 0..12 {
        interp_machine.write_physical_u32(FRAME_A, VALUE_A);
        native_machine.write_physical_u32(FRAME_A, VALUE_A);
        arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
        arm(&mut native_cpu, MEASURE_CODE_LINEAR);
        let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
        let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);
        assert_eq!(native_outcomes, interp_outcomes);
    }
    assert!(
        native_cpu.perf_counters().jit_direct_insns >= 3,
        "{:?}",
        native_cpu.perf_counters()
    );

    for (cpu, machine) in [
        (&mut interp_cpu, &mut interp_machine),
        (&mut native_cpu, &mut native_machine),
    ] {
        machine.trace = BusTrace::default();
        machine.trace.set_tracing_mode(TracingMode::Full);
        arm(cpu, WARM_CODE_LINEAR);
        let outcomes = drive_native_fetch_loop(cpu, machine);
        assert!(outcomes.last().is_some_and(|outcome| outcome.halted));
        let data_walks: Vec<_> = machine
            .trace
            .cycles()
            .iter()
            .filter(|cycle| {
                cycle.kind == BusAccessKind::PageWalkRead && matches!(cycle.address, PTE_A | PTE_B)
            })
            .collect();
        assert!(!data_walks.is_empty());
        assert_eq!(data_walks.last().unwrap().address, PTE_B);
        machine
            .memory
            .write_u32(PAGE_DIRECTORY as usize, PAGE_TABLE | 7)
            .unwrap();
        machine
            .memory
            .write_u32(PTE_A as usize, FRAME_A | 7)
            .unwrap();
    }

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    interp_machine.bus_rem = 2;
    native_machine.bus_rem = 2;
    arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
    arm(&mut native_cpu, MEASURE_CODE_LINEAR);
    interp_cpu.elapsed_clocks = 0;
    native_cpu.elapsed_clocks = 0;
    let interp_instructions = interp_cpu.perf_counters().instructions;
    let native_instructions = native_cpu.perf_counters().instructions;
    let side_exits = native_cpu.perf_counters().jit_direct_side_exits;
    let unavailable_exits = native_cpu
        .perf_counters()
        .jit_direct_exit_unavailable_or_kind;
    let direct_loads = native_cpu.perf_counters().jit_native_load_hits;

    let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_outcomes, interp_outcomes);
    // THE ARCHITECTURAL FLAGS FIRST, on the roles as they are -- before anything settles them, or
    // the assertion is a tautology -- then the whole structure on SETTLED CLONES.
    //
    // `registers.eflags` plus `pending_flags` is a REPRESENTATION of the flags, and the native role
    // settles that representation on the way into emitted code (`run_direct_block`'s entry clear)
    // where the interpreter role keeps its lazy pair. Both reach the same architectural value; only
    // the split between base and descriptor differs.
    //
    // `CpuGsw::settled` and NOT an inline `registers.eflags = eflags()`: the inline form leaves
    // `pending_flags` standing, and `CpuGsw` derives `PartialEq` over every field, so it would go on
    // byte-comparing the raw descriptor -- the exact invariant this campaign released -- surviving
    // at the only two sites reached across a crate boundary. It would also construct a state no code
    // path produces: a settled base with a live descriptor over it.
    assert_eq!(native_cpu.eflags(), interp_cpu.eflags());
    assert_eq!(native_cpu.settled(), interp_cpu.settled());
    let interp_raw = interp_machine.trace.elapsed_clocks();
    let native_raw = native_machine.trace.elapsed_clocks();
    assert_eq!(
        native_raw, interp_raw,
        "production aggregate accounting must preserve raw bus clocks"
    );
    assert_eq!(
        interp_cpu.perf_counters().instructions - interp_instructions,
        5
    );
    assert_eq!(
        native_cpu.perf_counters().instructions - native_instructions,
        5
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_side_exits - side_exits,
        1
    );
    assert_eq!(
        native_cpu
            .perf_counters()
            .jit_direct_exit_unavailable_or_kind
            - unavailable_exits,
        1
    );
    assert_eq!(
        native_cpu.perf_counters().jit_native_load_hits - direct_loads,
        0,
        "the evicted first alias must leave native code before the load"
    );
    let interp_scaled = interp_machine.scale_bus(interp_raw);
    let native_scaled = native_machine.scale_bus(native_raw);
    assert_eq!(native_scaled, interp_scaled);
    assert_eq!(native_machine.bus_rem, interp_machine.bus_rem);
    assert_eq!(
        native_scaled,
        (native_raw * u64::from(bus_timing(GswMode::Gsw486.persona(), 1).0) + 2)
            / u64::from(bus_timing(GswMode::Gsw486.persona(), 1).1)
    );
    assert_eq!(
        interp_machine
            .memory
            .read_u32(PAGE_DIRECTORY as usize)
            .unwrap(),
        PAGE_TABLE | 0x27
    );
    assert_eq!(
        interp_machine.memory.read_u32(PTE_A as usize).unwrap(),
        FRAME_A | 0x27
    );
    assert_eq!(
        interp_machine.memory.read_u32(PTE_A as usize).unwrap() & 0x40,
        0,
        "a read must not set the PTE dirty bit"
    );
    assert_eq!(
        native_machine
            .memory
            .read_u32(PAGE_DIRECTORY as usize)
            .unwrap(),
        PAGE_TABLE | 0x27
    );
    assert_eq!(
        native_machine.memory.read_u32(PTE_A as usize).unwrap(),
        FRAME_A | 0x27
    );
    assert_eq!(
        native_machine.memory.as_slice(),
        interp_machine.memory.as_slice()
    );
    assert_eq!(
        interp_machine.memory.read_u32(FRAME_A as usize).unwrap(),
        VALUE_A
    );
    assert_eq!(
        interp_machine.memory.read_u32(FRAME_B as usize).unwrap(),
        VALUE_B
    );
}

// --- Memory-poll (M1) machine-side tests ---
//
// The certified memory shape: `MOV EAX,imm32; CMP EAX,DS:[disp32]; JNZ/JZ rel8`
// back to the CMP, then HLT on exit. Program offsets (CS.base = 0x2000 for a
// raw .COM program): mov at eip 0x100, CMP head at 0x105, Jcc at 0x10b, HLT
// at 0x10d. The polled cell lives at ds.base + MEMORY_POLL_CELL_DISP.

#[cfg(feature = "jit")]
const MEMORY_POLL_HEAD_EIP: u32 = 0x105;
#[cfg(feature = "jit")]
const MEMORY_POLL_CELL_DISP: u32 = 0x3000;
#[cfg(feature = "jit")]
const MEMORY_POLL_COMPARAND: u32 = 0x8765_4321;

#[cfg(feature = "jit")]
fn memory_poll_program(cell_disp: u32, jz: bool) -> Vec<u8> {
    let mut program = vec![0xb8];
    program.extend_from_slice(&MEMORY_POLL_COMPARAND.to_le_bytes());
    program.extend_from_slice(&[0x3b, 0x05]);
    program.extend_from_slice(&cell_disp.to_le_bytes());
    program.push(if jz { 0x74 } else { 0x75 });
    program.push(0xf8);
    program.push(0xf4);
    program
}

#[cfg(feature = "jit")]
fn memory_poll_machine(enabled: bool, jz: bool, cell_disp: u32, cell_value: u32) -> Machine {
    let program = memory_poll_program(cell_disp, jz);
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.set_native_backend_enabled(false);
    machine.poll_skip_enabled = enabled;
    machine.trace.set_tracing_mode(TracingMode::Off);
    let ds_base = machine.cpu.registers.segment(SegmentIndex::Ds).base;
    machine.write_physical_u32(ds_base.wrapping_add(cell_disp), cell_value);
    machine
}

/// State + timing identity at batch boundaries, skip on vs the plain
/// interpreter (spec tests 7 + 8): a spinning tic-wait whose cell never
/// changes must leave
/// every architectural, timing, and device field byte-identical at every batch
/// boundary. `assert_poll_machine_boundary_eq` includes `elapsed_clocks`,
/// `timing_rem`, `trace.elapsed_clocks()`, and the beam, so this is also the
/// timing-identity oracle that PINS `MEMORY_POLL_RAW_CORE_CLOCKS` (a wrong
/// constant shows up as an elapsed-clock divergence on the first skipping
/// boundary) and the R7 caveats (the certificate's per-slot fetch sum plus one
/// warm dword data charge must total exactly the interpreter's per-iteration
/// charge, or these totals split).
#[cfg(feature = "jit")]
#[test]
fn memory_poll_skip_matches_the_interpreter_at_batch_boundaries() {
    for jz in [false, true] {
        // Pick a cell value that SPINS for the sense under test: JNZ (0x75)
        // spins while not equal, JZ (0x74) spins while equal.
        let cell_value = if jz {
            MEMORY_POLL_COMPARAND
        } else {
            MEMORY_POLL_COMPARAND ^ 0x1111
        };
        let mut baseline = memory_poll_machine(false, jz, MEMORY_POLL_CELL_DISP, cell_value);
        let mut skipped = memory_poll_machine(true, jz, MEMORY_POLL_CELL_DISP, cell_value);

        for boundary in 0..8 {
            let baseline_stop = baseline.run_cycles(100_000).unwrap();
            let skipped_stop = skipped.run_cycles(100_000).unwrap();
            assert_eq!(skipped_stop, baseline_stop, "jz={jz} boundary={boundary}");
            assert_poll_machine_boundary_eq(&skipped, &baseline);
        }
        let skipped_memory = skipped.cpu.poll_skip_memory();
        assert!(
            skipped_memory.spans > 0,
            "jz={jz}: the memory shape must have committed spans"
        );
        assert!(skipped_memory.iterations > 1);
        assert_eq!(
            skipped.cpu.perf_counters().poll_skip_spans,
            skipped_memory.spans,
            "no io shape exists in this program"
        );
        assert_eq!(baseline.cpu.poll_skip_memory().spans, 0);
        assert_eq!(baseline.cpu.perf_counters().poll_skip_spans, 0);
        assert!(
            skipped.cpu.perf_counters().instructions < baseline.cpu.perf_counters().instructions
        );
    }
}

/// Spec test 4 (IF=0): a masked-interrupt spin still skips, bounded by the
/// ordinary batch cap, with no special-case branch. Identity against the
/// plain interpreter proves the bound is exactly the cap either way.
#[cfg(feature = "jit")]
#[test]
fn memory_poll_skip_commits_with_interrupts_masked() {
    let cell_value = MEMORY_POLL_COMPARAND ^ 0x22;
    let mut baseline = memory_poll_machine(false, false, MEMORY_POLL_CELL_DISP, cell_value);
    let mut skipped = memory_poll_machine(true, false, MEMORY_POLL_CELL_DISP, cell_value);
    for machine in [&mut baseline, &mut skipped] {
        machine.cpu.registers.eflags &= !0x0200;
    }
    for _ in 0..4 {
        let baseline_stop = baseline.run_cycles(100_000).unwrap();
        let skipped_stop = skipped.run_cycles(100_000).unwrap();
        assert_eq!(skipped_stop, baseline_stop);
        assert_poll_machine_boundary_eq(&skipped, &baseline);
    }
    assert!(skipped.cpu.poll_skip_memory().spans > 0);
    assert_eq!(skipped.cpu.registers.eflags & 0x0200, 0);
}

/// R6a at the executor level: warm the loop spinning, then bump the cell to
/// the exit value (what the timer ISR does), re-enter at the head, and require
/// the executor to DECLINE (no phantom bulk commit) so the interpreter runs
/// the exit iteration with correct flags and clocks. Both senses.
#[cfg(feature = "jit")]
#[test]
fn memory_poll_executor_declines_at_the_head_when_about_to_exit() {
    for jz in [false, true] {
        let (spin_value, exit_value) = if jz {
            (MEMORY_POLL_COMPARAND, MEMORY_POLL_COMPARAND ^ 0x40)
        } else {
            (MEMORY_POLL_COMPARAND ^ 0x40, MEMORY_POLL_COMPARAND)
        };
        let mut baseline = memory_poll_machine(false, jz, MEMORY_POLL_CELL_DISP, spin_value);
        let mut skipped = memory_poll_machine(true, jz, MEMORY_POLL_CELL_DISP, spin_value);
        for machine in [&mut baseline, &mut skipped] {
            machine.run_cycles(2_000).unwrap();
            let ds_base = machine.cpu.registers.segment(SegmentIndex::Ds).base;
            machine.write_physical_u32(ds_base + MEMORY_POLL_CELL_DISP, exit_value);
            machine.cpu.registers.eip = MEMORY_POLL_HEAD_EIP;
            machine.cpu.poll_skip_backedge_housekeeping();
            machine.cpu.reset_perf_counters();
        }
        let baseline_stop = baseline.run_until_halt_or_cycles(100_000).unwrap();
        let skipped_stop = skipped.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(baseline_stop, StopReason::Halted, "jz={jz}");
        assert_eq!(skipped_stop, baseline_stop, "jz={jz}");
        assert_eq!(
            skipped.cpu.poll_skip_memory().spans,
            0,
            "jz={jz}: an about-to-exit head must never bulk-commit"
        );
        assert_eq!(skipped.cpu.eflags(), baseline.cpu.eflags(), "jz={jz}");
        assert_poll_machine_boundary_eq(&skipped, &baseline);
    }
}

/// Spec test 3 (rewritten per R6): a polled cell resolving into the VGA
/// aperture, reached through the real (unpaged-identity) translation, must be
/// rejected by the memory certificate; the loop interprets identically.
#[cfg(feature = "jit")]
#[test]
fn memory_poll_skip_declines_for_an_mmio_polled_cell() {
    let ds_base = 0x2000u32; // raw .COM DS base (asserted below)
    let disp = 0x000a_0000 - ds_base;
    let make = |enabled| {
        let mut machine = memory_poll_machine(enabled, false, disp, 0);
        assert_eq!(
            machine.cpu.registers.segment(SegmentIndex::Ds).base,
            ds_base
        );
        assert!(machine.set_vga_mode(0x13));
        machine
    };
    let mut baseline = make(false);
    let mut skipped = make(true);
    for _ in 0..3 {
        let baseline_stop = baseline.run_cycles(50_000).unwrap();
        let skipped_stop = skipped.run_cycles(50_000).unwrap();
        assert_eq!(skipped_stop, baseline_stop);
        assert_poll_machine_boundary_eq(&skipped, &baseline);
    }
    assert_eq!(skipped.cpu.poll_skip_memory().spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
}

/// Spec test 6 (rewritten per R6): a dword cell crossing a 4 KiB physical page
/// boundary is rejected by the certificate's single-physical-page check.
#[cfg(feature = "jit")]
#[test]
fn memory_poll_skip_declines_for_a_page_crossing_cell() {
    // ds.base 0x2000 + 0x2ffe = linear 0x4ffe: bytes 0x4ffe..=0x5001 straddle
    // the 0x5000 page boundary.
    let disp = 0x2ffe;
    let cell_value = MEMORY_POLL_COMPARAND ^ 0x3333;
    let mut baseline = memory_poll_machine(false, false, disp, cell_value);
    let mut skipped = memory_poll_machine(true, false, disp, cell_value);
    for _ in 0..3 {
        let baseline_stop = baseline.run_cycles(50_000).unwrap();
        let skipped_stop = skipped.run_cycles(50_000).unwrap();
        assert_eq!(skipped_stop, baseline_stop);
        assert_poll_machine_boundary_eq(&skipped, &baseline);
    }
    assert_eq!(skipped.cpu.poll_skip_memory().spans, 0);
    assert_eq!(skipped.cpu.perf_counters().poll_skip_spans, 0);
}

/// Spec test 5: SMC installing a memory-poll shape over a cached structural
/// negative must be recognized after the rewrite (the warm-line re-inserts bump
/// the page generation and retire the negative), mirroring
/// `code_write_retires_negative_and_new_poll_is_recognized` for the io shapes.
#[cfg(feature = "jit")]
#[test]
fn code_write_retires_negative_and_new_memory_poll_is_recognized() {
    const HEAD: u32 = NON_POLL_HEAD_OFFSET;
    let mut machine = warm_in_set_non_poll_machine();
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    machine.cpu.registers.eip = HEAD;
    machine.cpu.poll_skip_backedge_housekeeping();
    assert!(
        machine.cpu.poll_loop().is_none(),
        "the non-poll head classifies as a negative"
    );
    assert!(machine.cpu.perf_counters().poll_neg_cache_stores >= 1);

    // SMC: overwrite the head with the M1 memory-poll shape through the normal
    // recorded write path. CMP EAX,[0x3000]; JNZ -8 (cell in plain RAM).
    let mut shape = vec![0x3b, 0x05];
    shape.extend_from_slice(&MEMORY_POLL_CELL_DISP.to_le_bytes());
    shape.extend_from_slice(&[0x75, 0xf8]);
    let base = machine.cpu.registers.cs().base;
    for (offset, byte) in shape.iter().copied().enumerate() {
        machine.write_physical_u8(base + HEAD + offset as u32, byte);
    }
    // Keep the loop spinning while re-warming (EAX != cell for JNZ).
    let ds_base = machine.cpu.registers.segment(SegmentIndex::Ds).base;
    machine.write_physical_u32(
        ds_base + MEMORY_POLL_CELL_DISP,
        MEMORY_POLL_COMPARAND ^ 0x77,
    );
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        cpu.registers.set_eax(MEMORY_POLL_COMPARAND);
        for offset in [0u32, 6] {
            cpu.registers.eip = HEAD + offset;
            cpu.poll_skip_backedge_housekeeping();
            cpu.run_budgeted(bus, 0)
                .expect("warm the new memory poll slot");
        }
    });

    machine.cpu.registers.eip = HEAD;
    machine.cpu.poll_skip_backedge_housekeeping();
    let poll = machine
        .cpu
        .poll_loop()
        .expect("a stale negative suppressed a legitimate new memory poll shape");
    assert_eq!(poll.family(), izarravm_cpu::PollFamily::Memory);
}

// --- R6b: paged translation fixtures ---
//
// Protected paged 586 with a flat CS/DS: PD at 0x1000 (PD[0] -> PT 0x2000),
// identity PTEs for the low pages, and the polled cell's linear page 5
// (0x5000) mapped NON-identically to frame 0x9000. Code at identity page 3.
// IF stays clear (no IDT is installed).

#[cfg(feature = "jit")]
const PAGED_PD: u32 = 0x1000;
#[cfg(feature = "jit")]
const PAGED_PT: u32 = 0x2000;
#[cfg(feature = "jit")]
const PAGED_CODE: u32 = 0x3000;
#[cfg(feature = "jit")]
const PAGED_CELL_LINEAR: u32 = 0x5000;
#[cfg(feature = "jit")]
const PAGED_CELL_FRAME: u32 = 0x9000;
#[cfg(feature = "jit")]
const PAGED_HEAD: u32 = PAGED_CODE + 5;
/// Second page table, for the TLB-eviction alias once the collision stride reaches a whole
/// page-directory entry. Placed above the identity-mapped low pages so it cannot be confused with
/// guest data.
#[cfg(feature = "jit")]
const PAGED_PT_ALIAS: u32 = 0x2_0000;

#[cfg(feature = "jit")]
fn paged_memory_poll_machine(identity_alias_value: u32, mapped_frame_value: u32) -> Machine {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &[0xf4]).unwrap();
    machine.cpu.set_native_backend_enabled(false);
    machine.poll_skip_enabled = true;
    machine.trace.set_tracing_mode(TracingMode::Off);

    machine.write_physical_u32(PAGED_PD, PAGED_PT | 7);
    for page in 0u32..16 {
        let pte = if page == PAGED_CELL_LINEAR >> 12 {
            PAGED_CELL_FRAME | 7
        } else {
            (page << 12) | 7
        };
        machine.write_physical_u32(PAGED_PT + page * 4, pte);
    }
    // The TLB-eviction alias: linear page 5 + TLB_ENTRIES shares TLB slot 5. Derived from the live
    // entry count, because a hardcoded stride stops evicting (and this test stops testing anything)
    // the moment the TLB is resized. Once the stride reaches 1024 pages the alias sits one
    // page-directory entry higher and needs its own page table.
    let alias_page = 5 + TLB_ENTRIES as u32;
    let alias_dir = alias_page >> 10;
    let alias_table = if alias_dir == 0 {
        PAGED_PT
    } else {
        machine.write_physical_u32(PAGED_PD + alias_dir * 4, PAGED_PT_ALIAS | 7);
        PAGED_PT_ALIAS
    };
    machine.write_physical_u32(alias_table + (alias_page & 0x3ff) * 4, 0x6000 | 7);

    let mut program = vec![0xb8];
    program.extend_from_slice(&MEMORY_POLL_COMPARAND.to_le_bytes());
    program.extend_from_slice(&[0x3b, 0x05]);
    program.extend_from_slice(&PAGED_CELL_LINEAR.to_le_bytes());
    program.extend_from_slice(&[0x75, 0xf8, 0xf4]);
    for (offset, byte) in program.into_iter().enumerate() {
        machine.write_physical_u8(PAGED_CODE + offset as u32, byte);
    }
    // TLB-eviction snippet at identity page 4: mov ebx,[alias]; hlt.
    let alias_linear: u32 = alias_page << 12;
    let mut snippet = vec![0x8b, 0x1d];
    snippet.extend_from_slice(&alias_linear.to_le_bytes());
    snippet.push(0xf4);
    for (offset, byte) in snippet.into_iter().enumerate() {
        machine.write_physical_u8(0x4000 + offset as u32, byte);
    }

    machine.write_physical_u32(PAGED_CELL_LINEAR, identity_alias_value);
    machine.write_physical_u32(PAGED_CELL_FRAME, mapped_frame_value);

    machine.cpu.control.cr0 |= 0x8000_0001;
    machine.cpu.control.cr3 = PAGED_PD;
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        machine
            .cpu
            .registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    machine.cpu.registers.eflags &= !0x0200;
    machine.cpu.registers.eip = PAGED_CODE;
    machine.cpu.poll_skip_backedge_housekeeping();
    machine
}

#[cfg(feature = "jit")]
fn warm_paged_memory_poll(machine: &mut Machine) {
    with_cpu_and_bus(machine, |cpu, bus| {
        // A budgeted run ends early while lines are still cold; several short
        // runs spin the loop enough to warm every slot and the cell's TLB. A
        // fixture whose cell starts at the exit value halts immediately; stop
        // there (the head re-entry below still owns the classify state).
        for _ in 0..8 {
            let outcome = cpu.run_budgeted(bus, 500).expect("warm paged poll spin");
            if outcome.halted {
                cpu.halted = false;
                break;
            }
        }
    });
    machine.cpu.registers.eip = PAGED_HEAD;
    machine.cpu.poll_skip_backedge_housekeeping();
    machine.cpu.reset_perf_counters();
}

/// R6b (non-identity mapping): the certificate and the spin read must use the
/// MAPPED physical frame, not the linear-identity alias. The alias holds the
/// comparand (equal: a wrong linear-identity read would decline as
/// about-to-exit) while the mapped frame differs (spinning), so a committed
/// skip proves the correct physical was used end to end.
#[cfg(feature = "jit")]
#[test]
fn paged_memory_poll_uses_the_mapped_physical_cell() {
    let mut machine =
        paged_memory_poll_machine(MEMORY_POLL_COMPARAND, MEMORY_POLL_COMPARAND ^ 0x5a5a);
    warm_paged_memory_poll(&mut machine);
    let charged = attempt_poll_skip(&mut machine, 0, 200_000);
    assert!(
        charged.is_some(),
        "the mapped frame differs from the comparand, so the loop spins and must skip"
    );
    assert_eq!(machine.cpu.poll_skip_memory().spans, 1);
    assert!(machine.cpu.poll_skip_memory().iterations > 1);

    // The inverse assignment: mapped frame equal (about to exit), identity
    // alias different. A linear-identity read would wrongly see "spinning";
    // the correct mapped read declines.
    let mut machine =
        paged_memory_poll_machine(MEMORY_POLL_COMPARAND ^ 0x5a5a, MEMORY_POLL_COMPARAND);
    warm_paged_memory_poll(&mut machine);
    assert!(
        attempt_poll_skip(&mut machine, 0, 200_000).is_none(),
        "the mapped frame equals the comparand, so the executor must decline"
    );
    assert_eq!(machine.cpu.poll_skip_memory().spans, 0);
}

/// R6b (not-present decline): with the cell's PTE cleared and its TLB entry
/// evicted, the probe declines with ZERO perturbation (CR2, elapsed clocks,
/// trace clocks, timing remainder, and the PTE bytes are untouched), and the
/// interpreted access then takes the #PF path (CR2 = the cell's linear).
#[cfg(feature = "jit")]
#[test]
fn paged_memory_poll_declines_on_a_not_present_page_without_perturbation() {
    let mut machine =
        paged_memory_poll_machine(MEMORY_POLL_COMPARAND ^ 0x11, MEMORY_POLL_COMPARAND ^ 0x22);
    warm_paged_memory_poll(&mut machine);

    // Retire the mapping, then evict the stale TLB entry by touching the
    // aliasing linear page (TLB slot 5) from the eviction snippet.
    let cell_pte = PAGED_PT + (PAGED_CELL_LINEAR >> 12) * 4;
    machine.write_physical_u32(cell_pte, 0);
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        cpu.registers.eip = 0x4000;
        cpu.poll_skip_backedge_housekeeping();
        cpu.run_budgeted(bus, 0).expect("evict the cell's TLB slot");
        cpu.registers.eip = PAGED_HEAD;
        cpu.poll_skip_backedge_housekeeping();
    });
    machine.cpu.reset_perf_counters();
    // **Updated for the CR3 code-cache gate**
    // (`dev_docs/2026-09-02-cr3-code-cache-gate-design.md`). This row used to prove the R2 probe
    // (TLB miss) was the SOLE reason the executor declines, by first showing the head's structural
    // classification was untouched by clearing `cell_pte`. That is no longer true, correctly: the
    // gate's write watch now treats the `cell_pte` store as the live page-table edit it is (it
    // shares `PAGED_PT`'s physical page with the head's own PTE, and `PAGED_PT` is exactly the
    // structure `translate_linear_checked` marked while warming the poll loop), so it retires the
    // whole ring -- `code_pages`/`code_bytes` included, which is what `poll_head_possible`'s
    // prefilter reads. Re-decoding the head to restore the mark is not available cross-crate
    // (`fetch_decoded` is crate-private) and re-EXECUTING it would read the now-not-present cell
    // and triple-fault, so the row can no longer isolate the R2 probe this way. What still holds,
    // and is asserted below exactly as before: the R2-declined `attempt_poll_skip` call touches
    // NOTHING (CR2, elapsed clocks, trace clocks, the timing remainder and the PTE bytes), which
    // is the row's actual "without perturbation" claim.
    assert!(
        machine.cpu.poll_loop().is_none(),
        "the PTE clear is a genuine page-table edit and must retire the ring's structural marks"
    );

    const CR2_SENTINEL: u32 = 0xdead_0000;
    machine.cpu.control.cr2 = CR2_SENTINEL;
    let elapsed_before = machine.cpu.elapsed_clocks;
    let rem_before = machine.cpu.poll_skip_timing_remainder();
    let trace_before = machine.trace.elapsed_clocks();
    let pde_before = machine.memory.read_u32(PAGED_PD as usize).unwrap();

    assert!(
        attempt_poll_skip(&mut machine, 0, 200_000).is_none(),
        "a TLB-missing cell page must decline the skip"
    );
    assert_eq!(machine.cpu.control.cr2, CR2_SENTINEL, "decline set CR2");
    assert_eq!(machine.cpu.elapsed_clocks, elapsed_before);
    assert_eq!(machine.cpu.poll_skip_timing_remainder(), rem_before);
    assert_eq!(machine.trace.elapsed_clocks(), trace_before);
    assert_eq!(machine.memory.read_u32(cell_pte as usize).unwrap(), 0);
    assert_eq!(
        machine.memory.read_u32(PAGED_PD as usize).unwrap(),
        pde_before,
        "decline touched a page-walk entry"
    );
    assert_eq!(machine.cpu.poll_skip_memory().spans, 0);

    // The interpreted iteration then walks and takes the #PF path: CR2 is set
    // to the cell's linear address by the real fault delivery.
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        let _ = cpu.run_budgeted(bus, 10_000);
    });
    assert_eq!(
        machine.cpu.control.cr2, PAGED_CELL_LINEAR,
        "the interpreted access must deliver the #PF for the cell"
    );
}

#[test]
fn ram_lookup_does_not_expose_partial_final_pages_as_full_pages() {
    let vega = Vega::default();
    let lookup = RamPageLookup::new(RAM_LOOKUP_PAGE_SIZE + 17, &vega);
    assert!(lookup.direct_bytes(0, RAM_LOOKUP_PAGE_SIZE).is_some());
    assert!(
        lookup
            .direct_bytes(RAM_LOOKUP_PAGE_SIZE as u32, RAM_LOOKUP_PAGE_SIZE)
            .is_none(),
        "a final partial page cannot back a full direct-page pointer"
    );
}
