// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[cfg(feature = "jit")]
fn poll_skip_test_machine(enabled: bool, tracing: TracingMode, mode: GswMode, mask: u8) -> Machine {
    let program = [
        0xba, 0xda, 0x03, // mov dx,3DAh
        0xec, 0xa8, mask, 0x75, 0xfb, // wait while the status bit is set
        0xec, 0xa8, mask, 0x74, 0xfb, // wait until the status bit is set
        0xeb, 0xf4, // repeat both phases
    ];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
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
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for mask in [0x01, 0x08] {
            let mut baseline = poll_skip_test_machine(false, TracingMode::Off, mode, mask);
            let mut skipped = poll_skip_test_machine(true, TracingMode::Off, mode, mask);

            skipped.poll_skip_enabled = false;
            baseline.run_cycles(1_000).unwrap();
            skipped.run_cycles(1_000).unwrap();
            assert_eq!(skipped.cpu, baseline.cpu);
            for machine in [&mut baseline, &mut skipped] {
                machine.cpu.registers.eip = 0x108;
                machine.cpu.poll_skip_backedge_housekeeping();
                let poll = machine.cpu.poll_loop().expect("warm direct poll loop");
                for _ in 0..2 {
                    machine
                        .cpu
                        .commit_poll_skip_core(poll, 1)
                        .expect("one iteration advances the CPU timing remainder");
                    if machine.cpu.poll_skip_timing_remainder() != 0 {
                        break;
                    }
                }
                machine.cpu.reset_perf_counters();
                machine.bus_rem = 2;
                assert_ne!(machine.cpu.poll_skip_timing_remainder(), 0);
                assert_ne!(machine.bus_rem, 0);
            }
            skipped.poll_skip_enabled = true;

            let baseline_stop = baseline.run_cycles(100_000).unwrap();
            let skipped_stop = skipped.run_cycles(100_000).unwrap();
            assert_eq!(
                skipped_stop, baseline_stop,
                "mode={mode:?} mask={mask:#04x}"
            );
            assert!(
                skipped.cpu.perf_counters().poll_skip_spans > 0,
                "mode={mode:?} mask={mask:#04x} eip={:08x} linear={:08x} dx={:04x} eligible={} loop={:?}",
                skipped.cpu.registers.eip,
                skipped.cpu.linear_eip(),
                skipped.cpu.registers.edx() as u16,
                skipped.cpu.poll_skip_eligible(),
                skipped.cpu.poll_loop()
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
            bus.poll_bus_certificate(poll).is_none(),
            "device-backed instruction fetches cannot be aggregated"
        );
    });
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
fn projected_poll_total(machine: &mut Machine, iterations: u64, batch_core: u32) -> u64 {
    with_cpu_and_bus(machine, |cpu, bus| {
        let poll = cpu.poll_loop().expect("warm poll descriptor");
        let certificate = bus
            .poll_bus_certificate(poll)
            .expect("RAM poll bus certificate");
        let core = cpu
            .project_poll_skip_core(poll, iterations)
            .expect("poll core projection");
        let scaled_bus = bus
            .poll_project_scaled_bus_clocks(certificate, iterations)
            .expect("poll bus projection");
        u64::from(batch_core) + core + scaled_bus
    })
}

#[cfg(feature = "jit")]
fn projected_poll_dots(machine: &mut Machine, iterations: u64, batch_core: u32) -> u64 {
    with_cpu_and_bus(machine, |cpu, bus| {
        let poll = cpu.poll_loop().expect("warm poll descriptor");
        let certificate = bus
            .poll_bus_certificate(poll)
            .expect("RAM poll bus certificate");
        let core = cpu
            .project_poll_skip_core(poll, iterations)
            .expect("poll core projection");
        let scaled_bus = bus
            .poll_project_scaled_bus_clocks(certificate, iterations)
            .expect("poll bus projection");
        bus.poll_project_dot_advance(u64::from(batch_core) + core + scaled_bus)
            .expect("poll dot projection")
    })
}

#[cfg(feature = "jit")]
fn attempt_poll_skip(machine: &mut Machine, batch_core: u32, cap: u64) -> Option<u32> {
    let mut diagnostics = run::PollSkipDiagnostics::default();
    with_cpu_and_bus(machine, |cpu, bus| {
        bus.prior_runs_core_clocks = u64::from(batch_core);
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
    const BATCH_CORE: u32 = 7;
    const K: u64 = 2;
    for at_cap in [false, true] {
        let mut machine =
            setup_poll_machine_case(true, false, false, 0x1234_03da, GswMode::Gsw586, 0x08, true);
        prepare_setup_poll_head(&mut machine, false, false, 0x1234_03da, 0x08, true);
        machine.bus_rem = 2;
        with_cpu_and_bus(&mut machine, |cpu, _| {
            let poll = cpu.poll_loop().expect("warm direct setup poll");
            cpu.commit_poll_skip_core(poll, 1)
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
            .poll_bus_certificate(poll)
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
            let spent = u64::from(batch_core) + spent_bus;
            let remaining = cap.checked_sub(spent).expect("K+1 reserved tail");
            bus.prior_runs_core_clocks = u64::from(batch_core);
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
        let (num, den) = bus_timing(mode.persona());
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

fn drive_native_fetch_loop(cpu: &mut CpuGsw, machine: &mut Machine) -> Vec<CycleOutcome> {
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
    assert_eq!(native_cpu, interp_cpu);
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
    const WARM_CODE_LINEAR: u32 = 0x000f_4000;
    const WARM_CODE_PHYSICAL: u32 = 0x5000;
    const MEASURE_CODE_LINEAR: u32 = 0x000f_5000;
    const MEASURE_CODE_PHYSICAL: u32 = 0x8000;
    const LINEAR_A: u32 = 0x3000;
    const LINEAR_B: u32 = LINEAR_A + 64 * 0x1000;
    const FRAME_A: u32 = 0x6000;
    const FRAME_B: u32 = 0x7000;
    const VALUE_A: u32 = 0x1020_3040;
    const VALUE_B: u32 = 0x5566_7788;
    const PTE_A: u32 = PAGE_TABLE + ((LINEAR_A >> 12) & 0x3ff) * 4;
    const PTE_B: u32 = PAGE_TABLE + ((LINEAR_B >> 12) & 0x3ff) * 4;

    assert_eq!((LINEAR_A >> 12) & 63, (LINEAR_B >> 12) & 63);

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
    assert_eq!(native_cpu, interp_cpu);
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
        (native_raw * u64::from(bus_timing(GswMode::Gsw486.persona()).0) + 2)
            / u64::from(bus_timing(GswMode::Gsw486.persona()).1)
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
