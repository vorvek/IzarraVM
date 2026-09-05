// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn movsb_copies_and_increments_when_df_clear() {
    // movsb (0xa4). [ds:si]=0x42 -> [es:di]; si and di increment (DF=0).
    let mut memory = vec![0; 1024];
    memory[0] = 0xa4;
    memory[0x100] = 0x42;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0x42);
    assert_eq!(cpu.registers.esi(), 0x101);
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn movsb_decrements_when_df_set() {
    // movsb with DF=1: si and di decrement.
    let mut memory = vec![0; 1024];
    memory[0] = 0xa4;
    memory[0x100] = 0x42;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, true);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0x42);
    assert_eq!(cpu.registers.esi(), 0x0ff);
    assert_eq!(cpu.registers.edi(), 0x1ff);
}

#[test]
fn rep_movsb_copies_cx_bytes() {
    // rep movsb (0xf3 0xa4) with cx=3 copies 3 bytes, leaves cx=0, advances si/di by 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x103].copy_from_slice(&[1, 2, 3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(&bus.memory[0x200..0x203], &[1, 2, 3]);
    assert_eq!(cpu.registers.esi(), 0x103);
    assert_eq!(cpu.registers.edi(), 0x203);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_iterations, 3);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 3);
}

#[test]
fn budgeted_rep_movsb_yields_at_restart_eip_and_matches_atomic_timing() {
    const ORIGIN: usize = 0x40;
    let mut memory = vec![0; 0x1000];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let mut budgeted = CpuGsw::default();
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
    ] {
        budgeted.load_segment_real(segment, 0);
    }
    budgeted.registers.eip = ORIGIN as u32;
    budgeted.registers.set_esi(0x100);
    budgeted.registers.set_edi(0x200);
    budgeted.registers.set_ecx(8);
    let mut atomic = budgeted.clone();
    let mut budgeted_bus = TestBus::with_memory(memory.clone());
    budgeted_bus.direct_page_clocks = true;
    budgeted_bus.report_batch_clocks = true;
    let mut atomic_bus = TestBus::with_memory(memory);
    atomic_bus.direct_page_clocks = true;
    atomic_bus.report_batch_clocks = true;

    // Decode fetch (including its lookahead), one conservative core clock, and two MOVSB
    // iterations fit. A third does
    // not. The partial instruction has not retired and exposes its prefix address for an IRQ frame.
    let before_bus = budgeted_bus.trace.elapsed_clocks();
    let first = budgeted.run_budgeted(&mut budgeted_bus, 31).unwrap();
    assert!(
        first.consumed_core_clocks
            + budgeted_bus
                .trace
                .elapsed_clocks()
                .saturating_sub(before_bus)
            <= 31
    );
    assert!(!first.halted);
    assert_eq!(budgeted.registers.eip, ORIGIN as u32);
    assert_eq!(budgeted.registers.ecx(), 6);
    assert_eq!(budgeted.registers.esi(), 0x102);
    assert_eq!(budgeted.registers.edi(), 0x202);
    assert_eq!(
        &budgeted_bus.memory[0x200..0x208],
        &[1, 2, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(budgeted.perf_counters().instructions, 0);

    while budgeted.registers.eip == ORIGIN as u32 {
        let before_bus = budgeted_bus.trace.elapsed_clocks();
        let outcome = budgeted.run_budgeted(&mut budgeted_bus, 31).unwrap();
        assert!(
            outcome.consumed_core_clocks
                + budgeted_bus
                    .trace
                    .elapsed_clocks()
                    .saturating_sub(before_bus)
                <= 31
        );
    }
    atomic.cycle(&mut atomic_bus).unwrap();

    assert_eq!(budgeted, atomic);
    assert_eq!(budgeted_bus.memory, atomic_bus.memory);
    assert_eq!(
        budgeted_bus.trace.elapsed_clocks(),
        atomic_bus.trace.elapsed_clocks()
    );
    assert_eq!(budgeted.perf_counters().instructions, 1);
}

#[test]
fn budgeted_rep_stosb_limits_each_dirty_chunk() {
    const ORIGIN: usize = 0x40;
    let mut memory = vec![0; 0x1000];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xf3, 0xaa]);
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw386);
    for segment in [SegmentIndex::Cs, SegmentIndex::Es, SegmentIndex::Ss] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_eax(0x5a);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(4);
    let mut atomic = cpu.clone();
    let mut bus = TestBus::with_memory(memory.clone());
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;
    let mut atomic_bus = TestBus::with_memory(memory);
    atomic_bus.direct_page_clocks = true;
    atomic_bus.report_batch_clocks = true;

    let first = cpu.run_budgeted(&mut bus, 14).unwrap();
    assert_eq!(first.consumed_core_clocks, 1);
    assert_eq!(cpu.registers.eip, ORIGIN as u32);
    assert_eq!(cpu.registers.ecx(), 3);
    assert_eq!(cpu.registers.edi(), 0x201);
    assert_eq!(&bus.memory[0x200..0x204], &[0x5a, 0, 0, 0]);

    while cpu.registers.eip == ORIGIN as u32 {
        let resumed = cpu.run_budgeted(&mut bus, 14).unwrap();
        assert_eq!(resumed.consumed_core_clocks, 0);
    }
    atomic.cycle(&mut atomic_bus).unwrap();
    assert_eq!(&bus.memory[0x200..0x204], &[0x5a; 4]);
    assert_eq!(cpu.perf_counters().instructions, 1);
    assert_eq!(cpu, atomic);
    assert_eq!(bus.memory, atomic_bus.memory);
    assert_eq!(
        bus.trace.elapsed_clocks(),
        atomic_bus.trace.elapsed_clocks()
    );
}

#[test]
fn resumed_rep_port_returns_only_its_new_port_core_debt() {
    const ORIGIN: usize = 0x40;
    const FIRST_CHUNK_ELEMENTS: usize = 4096;
    let values = (0..=FIRST_CHUNK_ELEMENTS)
        .map(|index| ((index * 73 + 0x19) & 0xff) as u8)
        .collect::<Vec<_>>();

    for (mode, opcode, setup, element) in [
        (GswMode::Gsw486, 0x6c, 11, 3),
        (GswMode::Gsw586, 0x6c, 11, 3),
        (GswMode::Gsw486, 0x6e, 13, 4),
        (GswMode::Gsw586, 0x6e, 13, 4),
    ] {
        let is_ins = opcode == 0x6c;
        let mut memory = vec![0; 0x3000];
        memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xf3, opcode]);
        if !is_ins {
            memory[0x200..0x200 + values.len()].copy_from_slice(&values);
        }
        let mut cpu = CpuGsw::default();
        cpu.set_mode(mode);
        cpu.set_timing_epoch(2);
        for segment in [
            SegmentIndex::Cs,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Ss,
        ] {
            cpu.load_segment_real(segment, 0);
        }
        cpu.registers.eip = ORIGIN as u32;
        cpu.registers.set_edx(0x03da);
        cpu.registers.set_esi(0x200);
        cpu.registers.set_edi(0x200);
        cpu.registers.set_ecx((FIRST_CHUNK_ELEMENTS + 1) as u32);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_page_clocks = true;
        bus.report_batch_clocks = true;
        bus.timing_epoch_two = true;
        bus.lazy_io_reads = true;
        bus.lazy_io_writes = true;
        bus.io_read_sequence = values.iter().map(|value| u32::from(*value)).collect();

        let first_elapsed = cpu.elapsed_clocks;
        let first = cpu.run_budgeted(&mut bus, u64::MAX).unwrap();
        assert_eq!(
            first.consumed_core_clocks,
            setup + FIRST_CHUNK_ELEMENTS as u64 * element,
            "{mode:?} first return has one REP setup and 4096 completed port elements"
        );
        assert_eq!(cpu.registers.eip, ORIGIN as u32);
        assert_eq!(cpu.registers.ecx(), 1);
        assert_eq!(
            first.consumed_core_clocks,
            cpu.elapsed_clocks - first_elapsed
        );

        if is_ins {
            assert_eq!(
                bus.io_reads
                    .iter()
                    .map(|(port, _)| *port)
                    .collect::<Vec<_>>(),
                vec![0x03da; FIRST_CHUNK_ELEMENTS]
            );
            assert_eq!(
                &bus.memory[0x200..0x200 + FIRST_CHUNK_ELEMENTS],
                &values[..FIRST_CHUNK_ELEMENTS]
            );
            assert_eq!(cpu.registers.edi(), 0x200 + FIRST_CHUNK_ELEMENTS as u32);
        } else {
            assert_eq!(
                bus.io_writes
                    .iter()
                    .map(|(port, value, _)| (*port, *value))
                    .collect::<Vec<_>>(),
                values[..FIRST_CHUNK_ELEMENTS]
                    .iter()
                    .map(|value| (0x03da, u32::from(*value)))
                    .collect::<Vec<_>>()
            );
            assert_eq!(cpu.registers.esi(), 0x200 + FIRST_CHUNK_ELEMENTS as u32);
        }

        let resumed_elapsed = cpu.elapsed_clocks;
        let resumed = cpu.run_budgeted(&mut bus, u64::MAX).unwrap();
        assert_eq!(resumed.consumed_core_clocks, element);
        assert_eq!(
            resumed.consumed_core_clocks,
            cpu.elapsed_clocks - resumed_elapsed
        );
        assert_eq!(cpu.registers.ecx(), 0);
        assert_eq!(cpu.registers.eip, (ORIGIN + 2) as u32);
        if is_ins {
            assert_eq!(
                bus.io_reads
                    .iter()
                    .map(|(port, _)| *port)
                    .collect::<Vec<_>>(),
                vec![0x03da; FIRST_CHUNK_ELEMENTS + 1]
            );
            assert_eq!(&bus.memory[0x200..0x200 + values.len()], values.as_slice());
            assert_eq!(cpu.registers.edi(), 0x200 + values.len() as u32);
        } else {
            assert_eq!(
                bus.io_writes
                    .iter()
                    .map(|(port, value, _)| (*port, *value))
                    .collect::<Vec<_>>(),
                values
                    .iter()
                    .map(|value| (0x03da, u32::from(*value)))
                    .collect::<Vec<_>>()
            );
            assert_eq!(cpu.registers.esi(), 0x200 + values.len() as u32);
        }
        assert_eq!(cpu.timing_rem, 0);
    }
}

#[test]
fn epoch_two_rep_port_boundaries_keep_setup_pause_forced_progress_and_resume_separate() {
    struct Case {
        opcode: u8,
        setup: u64,
        element: u64,
        port_entries: fn(&TestBus) -> usize,
        assert_effects: fn(&CpuGsw, &TestBus, usize),
    }

    fn reads(bus: &TestBus) -> usize {
        bus.io_reads.len()
    }

    fn writes(bus: &TestBus) -> usize {
        bus.io_writes.len()
    }

    fn assert_insb_effects(cpu: &CpuGsw, bus: &TestBus, elements: usize) {
        assert_eq!(
            bus.io_reads
                .iter()
                .map(|(port, _)| *port)
                .collect::<Vec<_>>(),
            vec![0x03da; elements]
        );
        assert_eq!(
            bus.memory[0x300..0x300 + elements].to_vec(),
            vec![0xa5; elements]
        );
        assert_eq!(cpu.registers.edi(), 0x300 + elements as u32);
    }

    fn assert_outsb_effects(cpu: &CpuGsw, bus: &TestBus, elements: usize) {
        let values = [0x66, 0x77];
        assert_eq!(
            bus.io_writes
                .iter()
                .map(|(port, value, _)| (*port, *value))
                .collect::<Vec<_>>(),
            values[..elements]
                .iter()
                .map(|value| (0x03da, u32::from(*value)))
                .collect::<Vec<_>>()
        );
        assert_eq!(&bus.memory[0x200..0x202], &values);
        assert_eq!(cpu.registers.esi(), 0x200 + elements as u32);
    }

    let cases = [
        Case {
            opcode: 0x6c,
            setup: 11,
            element: 3,
            port_entries: reads,
            assert_effects: assert_insb_effects,
        },
        Case {
            opcode: 0x6e,
            setup: 13,
            element: 4,
            port_entries: writes,
            assert_effects: assert_outsb_effects,
        },
    ];

    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for case in &cases {
            let fixture = |count: u32, carry: u64| {
                let mut memory = vec![0u8; 0x1000];
                memory[0x40..0x44].copy_from_slice(&[0xf3, case.opcode, 0xb0, 0x5a]);
                memory[0x200] = 0x66;
                memory[0x201] = 0x77;
                let mut cpu = CpuGsw::default();
                cpu.set_mode(mode);
                cpu.set_timing_epoch(2);
                cpu.timing_rem = carry;
                for segment in [
                    SegmentIndex::Cs,
                    SegmentIndex::Ds,
                    SegmentIndex::Es,
                    SegmentIndex::Ss,
                ] {
                    cpu.load_segment_real(segment, 0);
                }
                cpu.registers.eip = 0x40;
                cpu.registers.set_edx(0x03da);
                cpu.registers.set_esi(0x200);
                cpu.registers.set_edi(0x300);
                cpu.registers.set_ecx(count);
                let mut bus = TestBus::with_memory(memory);
                bus.direct_page_clocks = true;
                bus.report_batch_clocks = true;
                bus.timing_epoch_two = true;
                bus.lazy_io_reads = true;
                bus.io_read_value = Some(0xa5);
                (cpu, bus)
            };

            // Count zero still owns the instruction's setup, but no element or port transaction.
            let (mut zero, mut zero_bus) = fixture(0, 7);
            let zero_elapsed = zero.elapsed_clocks;
            let zero_result = zero.run_budgeted(&mut zero_bus, case.setup).unwrap();
            assert_eq!(zero_result.consumed_core_clocks, case.setup);
            assert_eq!(zero.elapsed_clocks - zero_elapsed, case.setup);
            assert_eq!((case.port_entries)(&zero_bus), 0);
            assert_eq!(zero.registers.ecx(), 0);
            assert_eq!(zero.registers.eip, 0x42);
            assert_eq!(zero.timing_rem, 7, "setup is an exact epoch-2 whole clock");
            (case.assert_effects)(&zero, &zero_bus, 0);

            // Count one completes normally under a finite program budget: setup and element
            // appear once in the public return. Its reservation includes bus work beyond core.
            let (mut one, mut one_bus) = fixture(1, 0);
            let one_elapsed = one.elapsed_clocks;
            let one_result = one.run_budgeted(&mut one_bus, u64::MAX).unwrap();
            assert_eq!(one_result.consumed_core_clocks, case.setup + case.element);
            assert_eq!(one.elapsed_clocks - one_elapsed, case.setup + case.element);
            assert_eq!((case.port_entries)(&one_bus), 1);
            assert_eq!(one.registers.ecx(), 0);
            assert_eq!(one.registers.eip, 0x42);
            (case.assert_effects)(&one, &one_bus, 1);

            // This cap is U+12 for INS and U+14 for OUTS. It pauses before the first element,
            // retaining only setup and a restartable EIP. The next exhausted cap takes the real
            // forced-one-element arm; a third call completes the remaining element normally.
            let (mut two, mut two_bus) = fixture(2, 0);
            let paused_elapsed = two.elapsed_clocks;
            let paused = two.run_budgeted(&mut two_bus, case.setup + 1).unwrap();
            assert_eq!(paused.consumed_core_clocks, case.setup);
            assert_eq!(two.elapsed_clocks - paused_elapsed, case.setup);
            assert_eq!((case.port_entries)(&two_bus), 0);
            assert_eq!(two.registers.ecx(), 2);
            assert_eq!(two.registers.eip, 0x40);
            assert!(two.rep_execution.resume.is_some());
            (case.assert_effects)(&two, &two_bus, 0);

            let forced_elapsed = two.elapsed_clocks;
            let forced = two.run_budgeted(&mut two_bus, 0).unwrap();
            assert_eq!(forced.consumed_core_clocks, case.element);
            assert_eq!(two.elapsed_clocks - forced_elapsed, case.element);
            assert_eq!((case.port_entries)(&two_bus), 1);
            assert_eq!(two.registers.ecx(), 1);
            assert_eq!(two.registers.eip, 0x40);
            assert!(two.rep_execution.resume.is_some());
            (case.assert_effects)(&two, &two_bus, 1);

            let resumed_elapsed = two.elapsed_clocks;
            let resumed = two.run_budgeted(&mut two_bus, u64::MAX).unwrap();
            assert_eq!(resumed.consumed_core_clocks, case.element);
            assert_eq!(two.elapsed_clocks - resumed_elapsed, case.element);
            assert_eq!((case.port_entries)(&two_bus), 2);
            assert_eq!(two.registers.ecx(), 0);
            assert_eq!(two.registers.eip, 0x42);
            assert!(two.rep_execution.resume.is_none());
            (case.assert_effects)(&two, &two_bus, 2);

            let later_elapsed = two.elapsed_clocks;
            let later = two.cycle(&mut two_bus).unwrap();
            assert_eq!(later.core_clocks, 1, "later MOV has no REP sidecar debt");
            assert_eq!(two.elapsed_clocks - later_elapsed, 1);
            assert_eq!(two.registers.eax() & 0xff, 0x5a);
        }
    }
}

#[test]
fn i386_epoch_one_rep_ports_keep_first_and_resumed_owners() {
    for (opcode, is_out, first_core, first_carry, later_core, later_carry, resume_cap) in [
        (0x6c, false, 6, 0, 0, 4, 0),
        (0x6c, false, 6, 0, 0, 4, u64::MAX),
        (0x6e, true, 5, 3, 1, 2, 0),
        (0x6e, true, 5, 3, 1, 2, u64::MAX),
    ] {
        let mut memory = vec![0u8; 0x1000];
        memory[0x40..0x44].copy_from_slice(&[0xf3, opcode, 0xb0, 0x5a]);
        if is_out {
            memory[0x200..0x202].copy_from_slice(&[0x11, 0x22]);
        }
        let mut cpu = CpuGsw::default();
        cpu.set_mode(GswMode::Gsw386);
        cpu.set_timing_epoch(1);
        cpu.timing_rem = 0;
        for segment in [
            SegmentIndex::Cs,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Ss,
        ] {
            cpu.load_segment_real(segment, 0);
        }
        cpu.set_flag(FLAG_IF, false);
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.eip = 0x40;
        cpu.registers.set_ecx(2);
        cpu.registers.set_esi(0x200);
        cpu.registers.set_edi(0x200);
        cpu.registers.set_edx(0x03da);
        let mut bus = TestBus::with_memory(memory);
        bus.report_batch_clocks = true;
        bus.batch_bus_scale = (1, 1);
        bus.lazy_io_reads = true;
        bus.lazy_io_writes = true;
        bus.io_read_sequence = vec![0x11, 0x22];

        let first_trace_start = bus.trace.cycles().len();
        let first_elapsed = cpu.elapsed_clocks;
        let first = cpu.run_budgeted(&mut bus, 1).unwrap();
        assert_eq!(first.consumed_core_clocks, first_core);
        assert_eq!(cpu.elapsed_clocks - first_elapsed, first_core);
        assert_eq!(cpu.timing_rem, first_carry);
        assert_eq!(cpu.registers.ecx(), 1);
        assert_eq!(cpu.registers.eip, 0x40);
        assert!(cpu.rep_execution.resume.is_some());
        assert_eq!(bus.trace.elapsed_clocks(), 10);
        assert_eq!(izarravm_bus::CpuBus::in_batch_scaled_bus_clocks(&bus), 10);
        assert_eq!(cpu.perf_counters().instructions, 0);
        assert_eq!(cpu.perf_counters().rep_string_iterations, 1);
        let fetches = bus
            .trace
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::InstructionPrefetch)
            .map(|cycle| cycle.address)
            .collect::<Vec<_>>();
        assert_eq!(fetches, vec![0x40, 0x41, 0x41]);
        assert_eq!(
            bus.trace
                .cycles()
                .iter()
                .skip(first_trace_start)
                .map(|cycle| (cycle.kind, cycle.address))
                .collect::<Vec<_>>(),
            if is_out {
                vec![
                    (BusAccessKind::InstructionPrefetch, 0x40),
                    (BusAccessKind::InstructionPrefetch, 0x41),
                    (BusAccessKind::InstructionPrefetch, 0x41),
                    (BusAccessKind::DataRead, 0x200),
                    (BusAccessKind::IoWrite, 0x03da),
                ]
            } else {
                vec![
                    (BusAccessKind::InstructionPrefetch, 0x40),
                    (BusAccessKind::InstructionPrefetch, 0x41),
                    (BusAccessKind::InstructionPrefetch, 0x41),
                    (BusAccessKind::IoRead, 0x03da),
                    (BusAccessKind::DataWrite, 0x200),
                ]
            }
        );
        if is_out {
            assert_eq!(cpu.registers.esi(), 0x201);
            assert_eq!(cpu.registers.edi(), 0x200);
            assert!(bus.io_reads.is_empty());
            assert_eq!(bus.io_writes, vec![(0x03da, 0x11, 0)]);
            assert_eq!(&bus.memory[0x200..0x202], &[0x11, 0x22]);
        } else {
            assert_eq!(cpu.registers.esi(), 0x200);
            assert_eq!(cpu.registers.edi(), 0x201);
            assert_eq!(bus.io_reads, vec![(0x03da, 0)]);
            assert!(bus.io_writes.is_empty());
            assert_eq!(&bus.memory[0x200..0x202], &[0x11, 0x00]);
        }

        let resumed_trace_start = bus.trace.cycles().len();
        let resumed_elapsed = cpu.elapsed_clocks;
        let resumed = cpu.run_budgeted(&mut bus, resume_cap).unwrap();
        assert_eq!(resumed.consumed_core_clocks, 0);
        assert_eq!(cpu.elapsed_clocks - resumed_elapsed, 0);
        assert_eq!(cpu.timing_rem, first_carry);
        assert_eq!(cpu.registers.ecx(), 0);
        assert_eq!(cpu.registers.eip, 0x42);
        assert!(cpu.rep_execution.resume.is_none());
        assert_eq!(bus.trace.elapsed_clocks(), 14);
        assert_eq!(izarravm_bus::CpuBus::in_batch_scaled_bus_clocks(&bus), 14);
        assert_eq!(cpu.perf_counters().instructions, 1);
        assert_eq!(cpu.perf_counters().rep_string_iterations, 2);
        assert_eq!(
            bus.trace
                .cycles()
                .iter()
                .skip(resumed_trace_start)
                .map(|cycle| (cycle.kind, cycle.address))
                .collect::<Vec<_>>(),
            if is_out {
                vec![
                    (BusAccessKind::DataRead, 0x201),
                    (BusAccessKind::IoWrite, 0x03da),
                ]
            } else {
                vec![
                    (BusAccessKind::IoRead, 0x03da),
                    (BusAccessKind::DataWrite, 0x201),
                ]
            }
        );
        if is_out {
            assert_eq!(cpu.registers.esi(), 0x202);
            assert_eq!(cpu.registers.edi(), 0x200);
            assert!(bus.io_reads.is_empty());
            assert_eq!(bus.io_writes, vec![(0x03da, 0x11, 0), (0x03da, 0x22, 0)]);
            assert_eq!(&bus.memory[0x200..0x202], &[0x11, 0x22]);
        } else {
            assert_eq!(cpu.registers.esi(), 0x200);
            assert_eq!(cpu.registers.edi(), 0x202);
            assert_eq!(bus.io_reads, vec![(0x03da, 0), (0x03da, 0)]);
            assert!(bus.io_writes.is_empty());
            assert_eq!(&bus.memory[0x200..0x202], &[0x11, 0x22]);
        }

        let later_trace_start = bus.trace.cycles().len();
        let later_elapsed = cpu.elapsed_clocks;
        let later_bus = bus.trace.elapsed_clocks();
        let later = cpu.cycle(&mut bus).unwrap();
        assert_eq!(later.core_clocks, later_core);
        assert_eq!(cpu.elapsed_clocks - later_elapsed, later_core);
        assert_eq!(cpu.timing_rem, later_carry);
        assert_eq!(bus.trace.elapsed_clocks() - later_bus, 6);
        assert_eq!(bus.trace.elapsed_clocks(), 20);
        assert_eq!(izarravm_bus::CpuBus::in_batch_scaled_bus_clocks(&bus), 20);
        assert_eq!(
            bus.trace
                .cycles()
                .iter()
                .skip(later_trace_start)
                .map(|cycle| (cycle.kind, cycle.address))
                .collect::<Vec<_>>(),
            vec![
                (BusAccessKind::InstructionPrefetch, 0x42),
                (BusAccessKind::InstructionPrefetch, 0x42),
                (BusAccessKind::InstructionPrefetch, 0x43),
            ]
        );
        assert_eq!(cpu.registers.eip, 0x44);
        assert_eq!(cpu.registers.eax() & 0xff, 0x5a);
        assert_eq!(cpu.perf_counters().instructions, 2);
        assert_eq!(cpu.perf_counters().rep_string_iterations, 2);
    }
}

#[test]
fn rep_port_faults_keep_only_completed_element_debt_and_never_resume() {
    const ORIGIN: usize = 0x40;

    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut out_memory = vec![0; 0x1000];
        out_memory[ORIGIN..ORIGIN + 4].copy_from_slice(&[0xf3, 0x6e, 0xb0, 0x5a]);
        out_memory[0x0fff] = 0x66;
        let mut out_cpu = CpuGsw::default();
        out_cpu.set_mode(mode);
        out_cpu.set_timing_epoch(2);
        out_cpu.timing_rem = 7;
        for segment in [
            SegmentIndex::Cs,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Ss,
        ] {
            out_cpu.load_segment_real(segment, 0);
        }
        out_cpu.registers.eip = ORIGIN as u32;
        out_cpu.registers.set_edx(0x03da);
        out_cpu.registers.set_esi(0x0fff);
        out_cpu.registers.set_ecx(2);
        out_cpu.set_flag(FLAG_IF, false);
        out_cpu.set_flag(FLAG_DF, false);
        let mut out_bus = TestBus::with_memory(out_memory);
        out_bus.timing_epoch_two = true;
        out_bus.lazy_io_reads = true;
        out_bus.lazy_io_writes = true;

        let out_elapsed = out_cpu.elapsed_clocks;
        let out_error = out_cpu.run_budgeted(&mut out_bus, u64::MAX).unwrap_err();
        assert!(matches!(
            out_error.error,
            CpuError::Bus(izarravm_bus::BusError::UnmappedMemory { address: 0x1000 })
        ));
        assert_eq!(
            out_error.consumed_core_clocks, 4,
            "{mode:?} OUTS first element only"
        );
        assert_eq!(out_cpu.elapsed_clocks - out_elapsed, 4);
        assert_eq!(out_cpu.timing_rem, 7);
        assert_eq!(out_cpu.registers.ecx(), 1);
        assert_eq!(out_cpu.registers.esi(), 0x1000);
        assert_eq!(out_cpu.registers.eip, ORIGIN as u32 + 2);
        assert!(out_cpu.rep_execution.resume.is_none());
        assert_eq!(out_cpu.perf_counters().instructions, 0);
        assert_eq!(out_bus.io_writes, vec![(0x03da, 0x66, 0)]);
        assert_eq!(
            out_bus
                .trace
                .cycles()
                .iter()
                .filter(|cycle| cycle.kind == BusAccessKind::DataRead)
                .map(|cycle| cycle.address)
                .collect::<Vec<_>>(),
            vec![0x0fff, 0x1000]
        );
        assert_eq!(out_cpu.cycle(&mut out_bus).unwrap().core_clocks, 1);
        assert_eq!(out_cpu.registers.eax() & 0xff, 0x5a);
        assert_eq!(out_cpu.timing_rem, 7);

        let mut ins_memory = vec![0; 0x1000];
        ins_memory[ORIGIN..ORIGIN + 4].copy_from_slice(&[0xf3, 0x6c, 0xb0, 0x5a]);
        let mut ins_cpu = CpuGsw::default();
        ins_cpu.set_mode(mode);
        ins_cpu.set_timing_epoch(2);
        ins_cpu.timing_rem = 7;
        for segment in [
            SegmentIndex::Cs,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Ss,
        ] {
            ins_cpu.load_segment_real(segment, 0);
        }
        ins_cpu.registers.eip = ORIGIN as u32;
        ins_cpu.registers.set_edx(0x03da);
        ins_cpu.registers.set_edi(0x0fff);
        ins_cpu.registers.set_ecx(2);
        ins_cpu.set_flag(FLAG_IF, false);
        ins_cpu.set_flag(FLAG_DF, false);
        let mut ins_bus = TestBus::with_memory(ins_memory);
        ins_bus.timing_epoch_two = true;
        ins_bus.lazy_io_reads = true;
        ins_bus.lazy_io_writes = true;
        ins_bus.io_read_sequence = vec![0xa5, 0xb6];

        let ins_elapsed = ins_cpu.elapsed_clocks;
        let ins_error = ins_cpu.run_budgeted(&mut ins_bus, u64::MAX).unwrap_err();
        assert!(matches!(
            ins_error.error,
            CpuError::Bus(izarravm_bus::BusError::UnmappedMemory { address: 0x1000 })
        ));
        assert_eq!(
            ins_error.consumed_core_clocks, 3,
            "{mode:?} INS first element only"
        );
        assert_eq!(ins_cpu.elapsed_clocks - ins_elapsed, 3);
        assert_eq!(ins_cpu.timing_rem, 7);
        assert_eq!(ins_cpu.registers.ecx(), 1);
        assert_eq!(ins_cpu.registers.edi(), 0x1000);
        assert_eq!(ins_cpu.registers.eip, ORIGIN as u32 + 2);
        assert!(ins_cpu.rep_execution.resume.is_none());
        assert_eq!(ins_cpu.perf_counters().instructions, 0);
        assert_eq!(ins_bus.io_reads, vec![(0x03da, 0), (0x03da, 3)]);
        assert_eq!(ins_bus.memory[0x0fff], 0xa5);
        assert_eq!(
            ins_bus
                .trace
                .cycles()
                .iter()
                .filter(|cycle| cycle.kind == BusAccessKind::DataWrite)
                .map(|cycle| cycle.address)
                .collect::<Vec<_>>(),
            vec![0x0fff, 0x1000]
        );
        assert_eq!(ins_cpu.cycle(&mut ins_bus).unwrap().core_clocks, 1);
        assert_eq!(ins_cpu.registers.eax() & 0xff, 0x5a);
        assert_eq!(ins_cpu.timing_rem, 7);
    }
}

#[test]
fn interrupt_between_rep_chunks_pushes_the_rep_start_eip() {
    const ORIGIN: usize = 0x40;
    let mut memory = vec![0; 0x1000];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x108].copy_from_slice(&[0x6d; 8]);
    memory[0x20..0x24].copy_from_slice(&[0x00, 0x03, 0x00, 0x00]);
    let mut cpu = CpuGsw::default();
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(8);
    cpu.registers.set_esp(0x800);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;

    cpu.run_budgeted(&mut bus, 31).unwrap();
    cpu.set_flag(FLAG_IF, true);
    bus.pending_irq = Some(8);
    let interrupt = cpu.service_pending_interrupt(&mut bus).unwrap().unwrap();

    assert!(!interrupt.halted);
    assert_eq!(cpu.registers.eip, 0x300);
    assert_eq!(cpu.registers.esp(), 0x7fa);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x7fa], bus.memory[0x7fb]]),
        ORIGIN as u16
    );
    assert!(cpu.rep_execution.resume.is_none());
    assert_eq!(cpu.registers.ecx(), 6);
}

#[test]
fn rep_resume_is_discarded_by_control_flow_cs_and_mode_changes() {
    const ORIGIN: usize = 0x40;
    let mut memory = vec![0; 0x1000];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xf3, 0xaa]);
    let mut cpu = CpuGsw::default();
    for segment in [SegmentIndex::Cs, SegmentIndex::Es, SegmentIndex::Ss] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(8);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;

    cpu.run_budgeted(&mut bus, 13).unwrap();
    assert!(cpu.rep_execution.resume.is_some());
    cpu.set_eip(0x80);
    assert!(cpu.rep_execution.resume.is_none());

    cpu.registers.eip = ORIGIN as u32;
    cpu.run_budgeted(&mut bus, 13).unwrap();
    assert!(cpu.rep_execution.resume.is_some());
    cpu.load_segment_real(SegmentIndex::Cs, 1);
    assert!(cpu.rep_execution.resume.is_none());

    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = ORIGIN as u32;
    cpu.run_budgeted(&mut bus, 13).unwrap();
    assert!(cpu.rep_execution.resume.is_some());
    cpu.set_mode(GswMode::Gsw486);
    assert!(cpu.rep_execution.resume.is_none());
}

#[test]
fn fault_after_resumed_rep_chunk_rewinds_to_original_instruction() {
    const ORIGIN: usize = 0x40;
    let mut memory = vec![0; 0x1000];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
    // Real-mode IVT vector 13 points at 0000:0300.
    memory[13 * 4..13 * 4 + 4].copy_from_slice(&[0x00, 0x03, 0x00, 0x00]);
    let mut cpu = CpuGsw::default();
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    let mut es = cpu.registers.segment(SegmentIndex::Es);
    es.limit = 0x202;
    cpu.registers.set_segment(SegmentIndex::Es, es);
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(4);
    cpu.registers.set_esp(0x800);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;

    cpu.run_budgeted(&mut bus, 31).unwrap();
    assert_eq!(cpu.registers.ecx(), 2);
    cpu.run_budgeted(&mut bus, u64::MAX).unwrap();

    assert_eq!(&bus.memory[0x200..0x204], &[1, 2, 3, 0]);
    assert_eq!(cpu.registers.ecx(), 1);
    assert_eq!(cpu.registers.esi(), 0x103);
    assert_eq!(cpu.registers.edi(), 0x203);
    assert_eq!(cpu.registers.eip, 0x300);
    assert_eq!(cpu.registers.esp(), 0x7fa);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x7fa], bus.memory[0x7fb]]),
        ORIGIN as u16
    );
    assert!(cpu.rep_execution.resume.is_none());
}

const BUDGETED_PAGED_REP_ORIGIN: u32 = 0x100;
const BUDGETED_PAGED_REP_SOURCE: u32 = 0x003f_fff0;
const BUDGETED_PAGED_REP_DESTINATION: u32 = 0x0080_1000;
const BUDGETED_PAGED_REP_COUNT: u32 = 32;
const BUDGETED_PAGED_REP_PD: usize = 0x1000;
const BUDGETED_PAGED_REP_PT0: usize = 0x2000;
const BUDGETED_PAGED_REP_PT1: usize = 0x3000;
const BUDGETED_PAGED_REP_PT2: usize = 0x7000;

fn budgeted_paged_rep_fixture() -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0xa000];
    memory[BUDGETED_PAGED_REP_PD..BUDGETED_PAGED_REP_PD + 4]
        .copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[BUDGETED_PAGED_REP_PD + 4..BUDGETED_PAGED_REP_PD + 8]
        .copy_from_slice(&0x0000_3007u32.to_le_bytes());
    memory[BUDGETED_PAGED_REP_PD + 8..BUDGETED_PAGED_REP_PD + 12]
        .copy_from_slice(&0x0000_7007u32.to_le_bytes());
    memory[BUDGETED_PAGED_REP_PT0..BUDGETED_PAGED_REP_PT0 + 4]
        .copy_from_slice(&0x0000_4007u32.to_le_bytes());
    memory[BUDGETED_PAGED_REP_PT0 + 1023 * 4..BUDGETED_PAGED_REP_PT0 + 1024 * 4]
        .copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[BUDGETED_PAGED_REP_PT1..BUDGETED_PAGED_REP_PT1 + 4]
        .copy_from_slice(&0x0000_6007u32.to_le_bytes());
    memory[BUDGETED_PAGED_REP_PT2 + 4..BUDGETED_PAGED_REP_PT2 + 8]
        .copy_from_slice(&0x0000_8007u32.to_le_bytes());
    let code = 0x4000 + BUDGETED_PAGED_REP_ORIGIN as usize;
    memory[code..code + 3].copy_from_slice(&[0xf3, 0xa4, 0xf4]);
    for index in 0..BUDGETED_PAGED_REP_COUNT as usize {
        let physical = if index < 16 {
            0x5ff0 + index
        } else {
            0x6000 + index - 16
        };
        memory[physical] = index as u8 ^ 0x5a;
    }

    let mut cpu = CpuGsw::default();
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [SegmentIndex::Ds, SegmentIndex::Es, SegmentIndex::Ss] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu.control.cr3 = BUDGETED_PAGED_REP_PD as u32;
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.registers.eip = BUDGETED_PAGED_REP_ORIGIN;
    cpu.registers.set_esi(BUDGETED_PAGED_REP_SOURCE);
    cpu.registers.set_edi(BUDGETED_PAGED_REP_DESTINATION);
    cpu.registers.set_ecx(BUDGETED_PAGED_REP_COUNT);

    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;
    bus.rep_data_byte_cost_override = Some(2);
    (cpu, bus)
}

#[test]
fn budgeted_paged_rep_reserves_the_next_four_mib_page_walk() {
    let (mut cpu, mut bus) = budgeted_paged_rep_fixture();

    let before_bus = bus.trace.elapsed_clocks();
    let outcome = cpu.run_budgeted(&mut bus, 100).unwrap();
    let charged =
        outcome.consumed_core_clocks + bus.trace.elapsed_clocks().saturating_sub(before_bus);

    assert!(charged <= 100, "charged {charged} clocks");
    assert_eq!(cpu.registers.eip, BUDGETED_PAGED_REP_ORIGIN);
    assert_eq!(cpu.registers.ecx(), 16);
    assert_eq!(cpu.registers.esi(), 0x0040_0000);
    assert_eq!(cpu.registers.edi(), BUDGETED_PAGED_REP_DESTINATION + 16);
    assert_eq!(
        &bus.memory[0x8000..0x8010],
        &(0..16).map(|index| index as u8 ^ 0x5a).collect::<Vec<_>>()
    );
    let next_pde = u32::from_le_bytes(
        bus.memory[BUDGETED_PAGED_REP_PD + 4..BUDGETED_PAGED_REP_PD + 8]
            .try_into()
            .unwrap(),
    );
    let next_pte = u32::from_le_bytes(
        bus.memory[BUDGETED_PAGED_REP_PT1..BUDGETED_PAGED_REP_PT1 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(next_pde & 0x20, 0, "the next PDE was walked early");
    assert_eq!(next_pte & 0x60, 0, "the next PTE was walked early");
}

#[test]
fn budgeted_paged_movsw_reserves_both_sides_of_each_split_operand() {
    const SOURCE: u32 = 0x003f_ffff;
    const DESTINATION: u32 = 0x0080_1fff;
    let mut memory = vec![0; 0xa000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[0x1004..0x1008].copy_from_slice(&0x0000_3007u32.to_le_bytes());
    memory[0x1008..0x100c].copy_from_slice(&0x0000_7007u32.to_le_bytes());
    memory[0x2000..0x2004].copy_from_slice(&0x0000_4007u32.to_le_bytes());
    memory[0x2ffc..0x3000].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[0x3000..0x3004].copy_from_slice(&0x0000_6007u32.to_le_bytes());
    memory[0x7004..0x7008].copy_from_slice(&0x0000_8007u32.to_le_bytes());
    memory[0x7008..0x700c].copy_from_slice(&0x0000_9007u32.to_le_bytes());
    memory[0x4100..0x4104].copy_from_slice(&[0xf3, 0x66, 0xa5, 0xf4]);
    memory[0x5fff] = 0xa5;
    memory[0x6000] = 0x5a;

    let mut cpu = CpuGsw::default();
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [SegmentIndex::Ds, SegmentIndex::Es, SegmentIndex::Ss] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.registers.eip = 0x100;
    cpu.registers.set_esi(SOURCE);
    cpu.registers.set_edi(DESTINATION);
    cpu.registers.set_ecx(1);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;
    bus.rep_data_byte_cost_override = Some(2);

    cpu.run_budgeted(&mut bus, 50).unwrap();
    assert_eq!(cpu.registers.eip, 0x100);
    assert_eq!(cpu.registers.ecx(), 1);
    assert_eq!(cpu.registers.esi(), SOURCE);
    assert_eq!(cpu.registers.edi(), DESTINATION);
    assert_eq!(&bus.memory[0x8fff..0x9001], &[0, 0]);
    for entry in [0x2ffc, 0x3000, 0x7004, 0x7008] {
        let value = u32::from_le_bytes(bus.memory[entry..entry + 4].try_into().unwrap());
        assert_eq!(value & 0x60, 0, "page-table entry {entry:#x} was touched");
    }

    cpu.run_budgeted(&mut bus, 1).unwrap();
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.registers.esi(), SOURCE + 2);
    assert_eq!(cpu.registers.edi(), DESTINATION + 2);
    assert_eq!(&bus.memory[0x8fff..0x9001], &[0xa5, 0x5a]);
}

#[test]
fn paged_rep_without_a_walk_bound_advances_once_per_resumed_batch() {
    let (mut cpu, mut bus) = budgeted_paged_rep_fixture();
    bus.page_walk_bound_available = false;

    cpu.run_budgeted(&mut bus, 1).unwrap();
    assert_eq!(cpu.registers.ecx(), BUDGETED_PAGED_REP_COUNT);
    assert_eq!(cpu.registers.eip, BUDGETED_PAGED_REP_ORIGIN);

    for completed in 1..=BUDGETED_PAGED_REP_COUNT {
        cpu.run_budgeted(&mut bus, 1).unwrap();
        assert_eq!(cpu.registers.ecx(), BUDGETED_PAGED_REP_COUNT - completed);
        assert_eq!(cpu.registers.esi(), BUDGETED_PAGED_REP_SOURCE + completed);
        if completed != BUDGETED_PAGED_REP_COUNT {
            assert_eq!(cpu.registers.eip, BUDGETED_PAGED_REP_ORIGIN);
        }
    }
}

/// Code-smell batch 2, S2's `RepLimitPlan` hoist must keep "the bus could not price a page walk"
/// a YIELD outcome, distinct from "the page walk is known to cost zero". The test above pins
/// that with a cap of 1, which is tight enough that `available` saturates to 0 whether or not
/// the paging term is folded in, so it cannot by itself tell the two outcomes apart -- a hoist
/// that silently substituted a zero-cost walk for the unpriced one would still pass it. This
/// gives the yield a budget generous enough to complete the whole REP in one call if the unpriced
/// walk were ever treated as free, so the two outcomes diverge observably.
#[test]
fn paged_rep_without_a_walk_bound_yields_even_with_budget_to_spare() {
    let (mut cpu, mut bus) = budgeted_paged_rep_fixture();
    bus.page_walk_bound_available = false;

    cpu.run_budgeted(&mut bus, 1_000_000).unwrap();
    assert_eq!(
        cpu.registers.ecx(),
        BUDGETED_PAGED_REP_COUNT,
        "an unpriced page walk must force a yield before any progress, not be treated as free"
    );
}

#[test]
fn paged_rep_movsb_bulk_translates_each_page_once_and_keeps_noncontiguous_frames() {
    const PAGE_DIRECTORY: usize = 0x1000;
    const PAGE_TABLE: usize = 0x2000;
    const SRC_LINEAR: u32 = 0x4ff0;
    const DST_LINEAR: u32 = 0x6ff0;
    const COUNT: usize = 32;

    let mut memory = vec![0; 0xe000];
    memory[PAGE_DIRECTORY..PAGE_DIRECTORY + 4].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    for (linear_page, physical_page) in [(4usize, 0x8000u32), (5, 0xa000), (6, 0xc000), (7, 0xd000)]
    {
        let pte = PAGE_TABLE + linear_page * 4;
        memory[pte..pte + 4].copy_from_slice(&(physical_page | 7).to_le_bytes());
    }
    let expected = core::array::from_fn::<_, COUNT, _>(|index| index as u8 ^ 0x5a);
    memory[0x8ff0..0x9000].copy_from_slice(&expected[..16]);
    memory[0xa000..0xa010].copy_from_slice(&expected[16..]);

    let mut cpu = CpuGsw::default();
    for segment in [SegmentIndex::Ds, SegmentIndex::Es] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.base = 0;
        descriptor.limit = u32::MAX;
        descriptor.default_size_32 = true;
        descriptor.access = 0x93;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.control.cr3 = PAGE_DIRECTORY as u32;
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.registers.set_esi(SRC_LINEAR);
    cpu.registers.set_edi(DST_LINEAR);
    cpu.registers.set_ecx(COUNT as u32);
    let mut bus = TestBus::with_memory(memory);

    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Movs,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Dword,
    )
    .unwrap();

    assert_eq!(&bus.memory[0xcff0..0xd000], &expected[..16]);
    assert_eq!(&bus.memory[0xd000..0xd010], &expected[16..]);
    assert_eq!(cpu.registers.esi(), SRC_LINEAR + COUNT as u32);
    assert_eq!(cpu.registers.edi(), DST_LINEAR + COUNT as u32);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, COUNT as u64);
}

#[test]
fn paged_rep_movsb_bulk_fault_keeps_completed_chunk_progress() {
    let mut memory = vec![0; 0xd000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    for (linear_page, physical_page) in [(4usize, 0x8000u32), (5, 0xa000), (6, 0xc000)] {
        let pte = 0x2000 + linear_page * 4;
        memory[pte..pte + 4].copy_from_slice(&(physical_page | 7).to_le_bytes());
    }
    memory[0x8ff0..0x9000].copy_from_slice(&[0x6d; 16]);
    memory[0xa000..0xa010].copy_from_slice(&[0x7e; 16]);

    let mut cpu = CpuGsw::default();
    for segment in [SegmentIndex::Ds, SegmentIndex::Es] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.base = 0;
        descriptor.limit = u32::MAX;
        descriptor.default_size_32 = true;
        descriptor.access = 0x93;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.registers.set_esi(0x4ff0);
    cpu.registers.set_edi(0x6ff0);
    cpu.registers.set_ecx(32);
    let mut bus = TestBus::with_memory(memory);

    let fault = cpu
        .run_string(
            &mut bus,
            &mut InstructionWork::default(),
            StringOp::Movs,
            BusWidth::Byte,
            Prefixes {
                rep: Some(RepKind::Repe),
                ..Default::default()
            },
            AddressSize::Dword,
        )
        .unwrap_err();

    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 14,
            error_code: Some(_)
        }
    ));
    assert_eq!(cpu.control.cr2, 0x7000);
    assert_eq!(&bus.memory[0xcff0..0xd000], &[0x6d; 16]);
    assert_eq!(cpu.registers.esi(), 0x5000);
    assert_eq!(cpu.registers.edi(), 0x7000);
    assert_eq!(cpu.registers.ecx(), 16);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 16);
}

#[test]
fn paged_movs_and_cmps_read_source_before_a_destination_page_fault() {
    const PAGE_DIRECTORY: usize = 0x1000;
    const PAGE_TABLE: usize = 0x2000;
    const SRC_LINEAR: u32 = 0x4000;
    const DST_LINEAR: u32 = 0x6000;
    const SRC_PHYSICAL: usize = 0x8000;
    const SRC_PTE: usize = PAGE_TABLE + 4 * 4;
    const DST_PTE: usize = PAGE_TABLE + 6 * 4;

    for op in [StringOp::Movs, StringOp::Cmps] {
        let mut memory = vec![0; 0x9000];
        memory[PAGE_DIRECTORY..PAGE_DIRECTORY + 4].copy_from_slice(&0x0000_2007u32.to_le_bytes());
        memory[SRC_PTE..SRC_PTE + 4].copy_from_slice(&0x0000_8007u32.to_le_bytes());
        memory[SRC_PHYSICAL] = 0x5a;
        let mut cpu = CpuGsw::default();
        for segment in [SegmentIndex::Ds, SegmentIndex::Es] {
            cpu.registers
                .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
        }
        cpu.control.cr3 = PAGE_DIRECTORY as u32;
        cpu.control.cr0 |= CR0_PE | CR0_PG;
        cpu.registers.set_esi(SRC_LINEAR);
        cpu.registers.set_edi(DST_LINEAR);
        cpu.registers.set_ecx(1);
        let mut bus = TestBus::with_memory(memory);

        let fault = cpu
            .run_string(
                &mut bus,
                &mut InstructionWork::default(),
                op,
                BusWidth::Byte,
                Prefixes {
                    rep: Some(RepKind::Repe),
                    ..Default::default()
                },
                AddressSize::Dword,
            )
            .unwrap_err();

        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 14,
                error_code: Some(_)
            }
        ));
        let source_read = bus
            .trace
            .cycles()
            .iter()
            .position(|cycle| {
                cycle.kind == BusAccessKind::DataRead && cycle.address == SRC_PHYSICAL as u32
            })
            .expect("source element must be read before the destination access");
        let destination_pte_read = bus
            .trace
            .cycles()
            .iter()
            .position(|cycle| {
                cycle.kind == BusAccessKind::PageWalkRead && cycle.address == DST_PTE as u32
            })
            .expect("destination translation must inspect its not-present PTE");
        assert!(source_read < destination_pte_read, "operation {op:?}");
        let source_pte = u32::from_le_bytes(bus.memory[SRC_PTE..SRC_PTE + 4].try_into().unwrap());
        let destination_pte =
            u32::from_le_bytes(bus.memory[DST_PTE..DST_PTE + 4].try_into().unwrap());
        assert_ne!(source_pte & 0x20, 0, "source PTE accessed bit, {op:?}");
        assert_eq!(source_pte & 0x40, 0, "source PTE dirty bit, {op:?}");
        assert_eq!(destination_pte, 0, "faulting destination PTE, {op:?}");
        assert_eq!(cpu.control.cr2, DST_LINEAR);
        assert_eq!(cpu.registers.ecx(), 1);
        assert_eq!(cpu.registers.esi(), SRC_LINEAR);
        assert_eq!(cpu.registers.edi(), DST_LINEAR);
    }
}

#[test]
fn rep_movsb_df_set_uses_correct_slow_path() {
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, true);
    cpu.registers.set_esi(0x103);
    cpu.registers.set_edi(0x203);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(&bus.memory[0x200..0x204], &[1, 2, 3, 4]);
    assert_eq!(cpu.registers.esi(), 0x0ff);
    assert_eq!(cpu.registers.edi(), 0x1ff);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_iterations, 4);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 0);
}

#[test]
fn rep_movsb_with_zero_count_does_nothing() {
    // rep movsb with cx=0 performs no access and leaves si/di/cx unchanged.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100] = 0x42;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(0);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0); // no write
    assert_eq!(cpu.registers.esi(), 0x100);
    assert_eq!(cpu.registers.edi(), 0x200);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn cmpsb_equal_sets_zero_flag() {
    // cmpsb (0xa6). [ds:si]=0x55, [es:di]=0x55 -> equal, ZF set.
    let mut memory = vec![0; 1024];
    memory[0] = 0xa6;
    memory[0x100] = 0x55;
    memory[0x200] = 0x55;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.esi(), 0x101);
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn cmpsb_unequal_clears_zero_flag() {
    // cmpsb. [ds:si]=0x10, [es:di]=0x20 -> 0x10-0x20 borrows: ZF clear, CF set.
    let mut memory = vec![0; 1024];
    memory[0] = 0xa6;
    memory[0x100] = 0x10;
    memory[0x200] = 0x20;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_CF)); // 0x10 < 0x20
    assert_eq!(cpu.registers.esi(), 0x101); // si advances even when unequal
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn scasb_compares_al_with_es_di() {
    // scasb (0xae). al=0x41, [es:di]=0x41 -> ZF set; di increments, si untouched.
    let mut memory = vec![0; 1024];
    memory[0] = 0xae;
    memory[0x200] = 0x41;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x41);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.edi(), 0x201);
    assert_eq!(cpu.registers.esi(), 0x100); // SCAS does not touch SI
}

#[test]
fn rep_fast_paths_cover_stos_lods_cmps_and_scas() {
    let memory = vec![0; 2048];
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    let mut bus = TestBus::with_memory(memory);

    cpu.write_gpr8(0, 0x7e);
    cpu.registers.set_edi(0x300);
    cpu.registers.set_ecx(4);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Stos,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(&bus.memory[0x300..0x304], &[0x7e; 4]);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 4);

    cpu.reset_perf_counters();
    bus.memory[0x400..0x403].copy_from_slice(&[1, 2, 3]);
    cpu.registers.set_esi(0x400);
    cpu.registers.set_ecx(3);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Lods,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(cpu.read_gpr8(0), 3);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 3);

    cpu.reset_perf_counters();
    bus.memory[0x500..0x503].copy_from_slice(&[1, 2, 9]);
    bus.memory[0x600..0x603].copy_from_slice(&[1, 2, 3]);
    cpu.registers.set_esi(0x500);
    cpu.registers.set_edi(0x600);
    cpu.registers.set_ecx(3);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Cmps,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert!(!cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 3);

    cpu.reset_perf_counters();
    bus.memory[0x700..0x703].copy_from_slice(&[1, 2, 3]);
    cpu.write_gpr8(0, 2);
    cpu.registers.set_edi(0x700);
    cpu.registers.set_ecx(3);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Scas,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repne),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.ecx(), 1);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 2);
}

/// T12 (code-smell batch 2, S4). The bulk scratch on `rep_execution.bulk` is REUSED across
/// calls, so poison it by hand before each op and confirm the guest never observes it: neither
/// the destination bytes (MOVS, STOS) nor the accumulator (LODS) may carry `0xCC`.
#[test]
fn bulk_scratch_residue_never_reaches_the_guest() {
    let memory = vec![0; 2048];
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    let mut bus = TestBus::with_memory(memory);

    // MOVS: CX=8 at byte width makes bytes=8 > access=1, so the multi-byte
    // read_memory_bytes_direct fill runs, not just the single first-byte copy.
    cpu.rep_execution.bulk.0.fill(0xcc);
    bus.memory[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x300);
    cpu.registers.set_ecx(8);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Movs,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(
        cpu.perf_counters().rep_string_fast_iterations,
        8,
        "must have taken the bulk path for this to test anything"
    );
    assert_eq!(&bus.memory[0x300..0x308], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(
        !bus.memory[0x300..0x308].contains(&0xcc),
        "MOVS must not leak poisoned scratch into the guest"
    );

    // STOS
    cpu.reset_perf_counters();
    cpu.rep_execution.bulk.0.fill(0xcc);
    cpu.write_gpr8(0, 0x7e);
    cpu.registers.set_edi(0x400);
    cpu.registers.set_ecx(6);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Stos,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 6);
    assert_eq!(&bus.memory[0x400..0x406], &[0x7e; 6]);

    // LODS: the accumulator must be the real last byte read, never poison.
    cpu.reset_perf_counters();
    cpu.rep_execution.bulk.0.fill(0xcc);
    bus.memory[0x500..0x505].copy_from_slice(&[9, 8, 7, 6, 5]);
    cpu.registers.set_esi(0x500);
    cpu.registers.set_ecx(5);
    cpu.run_string(
        &mut bus,
        &mut InstructionWork::default(),
        StringOp::Lods,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 5);
    assert_eq!(
        cpu.read_gpr8(0),
        5,
        "LODS accumulator must be the real last byte, not poison"
    );
}

/// T12b (code-smell batch 2, S4). Two CPUs identical except for a poisoned scratch buffer must
/// still compare equal (the buffer is scratch, not guest state), and a `Clone` must come back
/// zeroed rather than inheriting a previous call's bytes.
#[test]
fn bulk_scratch_does_not_affect_equality_and_clone_zeroes_it() {
    let mut poisoned = CpuGsw::default();
    poisoned.rep_execution.bulk.0.fill(0xcc);
    let clean = CpuGsw::default();
    assert_eq!(
        poisoned, clean,
        "a poisoned scratch buffer must not make two legally-equal CPUs compare unequal"
    );

    let cloned = poisoned.clone();
    assert!(
        cloned.rep_execution.bulk.0.iter().all(|&b| b == 0),
        "a clone must come back zeroed, not inherit a previous call's scratch bytes"
    );
}

/// Two of the three bails that keep the bulk scratch buffer's aliasing surface nil (code-smell
/// batch 2, S4): DF=1 must never reach the bulk path at all, and an overlapping MOVS range must
/// fall back to the single-step path instead of the bulk one. Both pin the OBSERVABLE consequence
/// (`rep_string_fast_iterations` stays 0 while `rep_string_iterations` still makes progress on
/// the slow path), because both bails sit ahead of every touch of the scratch buffer -- if either
/// were ever weakened, this is what would move. The third bail, the one-page clamp on chunk size,
/// needs no bus and is pinned directly in strings_test.rs.
#[test]
fn df_and_overlap_bails_never_reach_the_bulk_path() {
    // DF=1.
    {
        let memory = vec![0u8; 2048];
        let mut cpu = CpuGsw::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        let mut bus = TestBus::with_memory(memory);
        cpu.set_flag(FLAG_DF, true);
        cpu.write_gpr8(0, 0x11);
        cpu.registers.set_edi(0x300);
        cpu.registers.set_ecx(4);
        cpu.run_string(
            &mut bus,
            &mut InstructionWork::default(),
            StringOp::Stos,
            BusWidth::Byte,
            Prefixes {
                rep: Some(RepKind::Repe),
                ..Default::default()
            },
            AddressSize::Word,
        )
        .unwrap();
        assert_eq!(
            cpu.perf_counters().rep_string_fast_iterations,
            0,
            "DF=1 must never take the fast/bulk path"
        );
        assert_eq!(
            cpu.perf_counters().rep_string_iterations,
            4,
            "the slow path must still make progress"
        );
    }

    // Overlapping MOVS: a CORRECTNESS check, not a path-taken check, because the outer
    // run_string loop re-invokes try_run_string_fast after every single-step fallback and the
    // single-byte tail case converges with the bulk path's output anyway, so no perf counter
    // distinguishes "took the bulk path" from "took the single-step fallback four times in a
    // row". What DOES distinguish them is the RESULT: real x86 REP MOVSB with DI = SI + 1
    // processes one byte at a time, so each write becomes the NEXT iteration's read and the
    // first source byte "smears" forward. Reading the whole span first and writing it back in
    // one shot (what the bulk path does, and exactly why it gates on `!ranges_overlap`) would
    // instead copy the ORIGINAL bytes verbatim, one position over -- a different, wrong answer.
    {
        let memory = vec![0u8; 2048];
        let mut cpu = CpuGsw::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x101); // dst = src + 1: forward overlap
        cpu.registers.set_ecx(4);
        cpu.run_string(
            &mut bus,
            &mut InstructionWork::default(),
            StringOp::Movs,
            BusWidth::Byte,
            Prefixes {
                rep: Some(RepKind::Repe),
                ..Default::default()
            },
            AddressSize::Word,
        )
        .unwrap();
        assert_eq!(
            &bus.memory[0x101..0x105],
            &[1, 1, 1, 1],
            "an overlapping forward MOVS must smear byte-by-byte like real hardware, not copy \
             the original bytes verbatim like a memmove-style bulk copy would"
        );
    }
}

#[test]
fn repe_cmpsb_stops_on_first_mismatch() {
    // repe cmpsb (0xf3 0xa6), cx=4. Source "AABB" vs dest "AACC": the third byte
    // (index 2) is the B/C mismatch, so the repeat stops there with ZF clear after
    // 3 iterations; cx counts 4 -> 3 -> 2 -> 1, si/di advance by 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa6]);
    memory[0x100..0x104].copy_from_slice(b"AABB");
    memory[0x200..0x204].copy_from_slice(b"AACC");
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF)); // stopped on the index-2 mismatch (B != C)
    assert_eq!(cpu.registers.ecx(), 1); // 4 -> 3 -> 2 -> 1, then ZF clear stops
    assert_eq!(cpu.registers.esi(), 0x103);
    assert_eq!(cpu.registers.edi(), 0x203);
}

#[test]
fn repne_scasb_stops_on_match() {
    // repne scasb (0xf2 0xae), cx=4, al='C'. Dest "AACA": scans until the match at
    // index 2, stopping with ZF set after 3 iterations; cx counts 4 -> 3 -> 2 -> 1,
    // di advances by 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf2, 0xae]);
    memory[0x200..0x204].copy_from_slice(b"AACA");
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.write_gpr8(0, b'C');
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF)); // matched 'C' at index 2
    assert_eq!(cpu.registers.ecx(), 1); // 4 -> 3 -> 2 -> 1, match stops
    assert_eq!(cpu.registers.edi(), 0x203);
}

#[test]
fn movsb_honors_source_segment_override() {
    // es: movsb (0x26 0xa4). With ds=0 and es base 0x200, the override reads the
    // source from es:si (0x210), not ds:si (0x10); the destination stays es:di (0x230).
    let mut memory = vec![0; 0x400];
    memory[0..2].copy_from_slice(&[0x26, 0xa4]);
    memory[0x210] = 0x99;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0x20); // base 0x200
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x10);
    cpu.registers.set_edi(0x30);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x230], 0x99); // es:di destination
    assert_eq!(bus.memory[0x10], 0); // ds:si source was not used
}

#[test]
fn lea_loads_effective_address() {
    // lea bx, [si+0x10]  (0x8d 0x5c 0x10). bx <- si + 0x10, no memory access:
    // the byte at the computed address must not be loaded.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0x8d, 0x5c, 0x10]);
    memory[0x110] = 0x99;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esi(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0110);
}

#[test]
fn lea_with_register_operand_delivers_ud() {
    // lea ax, ax  (0x8d 0xc0, mod=3) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there and clears IF.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x8d, 0xc0]);
    memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lds_loads_offset_and_ds() {
    // lds bx, [0x0200]  (0xc5 0x1e 0x00 0x02). Loads the far pointer at DS:0x0200:
    // BX <- word[0x0200], DS <- word[0x0202]. No flags change.
    let mut memory = vec![0; 0x1000];
    memory[0..4].copy_from_slice(&[0xc5, 0x1e, 0x00, 0x02]);
    memory[0x0200] = 0x34; // offset low
    memory[0x0201] = 0x12; // offset high -> 0x1234
    memory[0x0202] = 0x00; // selector low
    memory[0x0203] = 0x90; // selector high -> 0x9000
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x9000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0x9000 << 4);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn les_loads_offset_and_es() {
    // les di, [bx]  (0xc4 0x3f). With BX=0x0300 it loads DS:0x0300:
    // DI <- word[0x0300], ES <- word[0x0302]. No flags change.
    let mut memory = vec![0; 0x1000];
    memory[0..2].copy_from_slice(&[0xc4, 0x3f]);
    memory[0x0300] = 0x78; // offset low
    memory[0x0301] = 0x56; // offset high -> 0x5678
    memory[0x0302] = 0x00; // selector low
    memory[0x0303] = 0xb8; // selector high -> 0xb800
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_ebx(0x0300);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Di), 0x5678);
    assert_eq!(cpu.registers.segment(SegmentIndex::Es).selector, 0xb800);
    assert_eq!(cpu.registers.segment(SegmentIndex::Es).base, 0xb800 << 4);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn lds_with_register_operand_delivers_ud() {
    // lds ax, bx  (0xc5 0xc3, mod=3) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xc5, 0xc3]);
    memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lss_real_mode_16bit_loads_offset_and_ss_and_arms_shadow() {
    // lss bx, [0x200]  (0F B2 1E 00 02). Loads the far pointer at DS:0x200:
    // BX <- word[0x200], SS <- word[0x202]. No flags change, but LSS arms the
    // one-instruction interrupt shadow (386 PRM 11-16), exactly like MOV SS/POP SS.
    // No interrupt is pending going in (IF true with nothing pending is the ordinary
    // case); the deferred-delivery behavior itself is `lss_interrupt_shadow_defers_
    // a_pending_irq_by_one_instruction` below.
    let mut memory = vec![0u8; 0x1000];
    memory[0..5].copy_from_slice(&[0x0f, 0xb2, 0x1e, 0x00, 0x02]);
    memory[0x200] = 0x34; // offset low
    memory[0x201] = 0x12; // offset high -> 0x1234
    memory[0x202] = 0x00; // selector low
    memory[0x203] = 0x90; // selector high -> 0x9000
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, true);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x9000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).base, 0x9000 << 4);
    assert_eq!(cpu.registers.eflags, flags_before);
    assert!(
        cpu.interrupt_shadow,
        "LSS must arm the one-instruction interrupt shadow"
    );
}

#[test]
fn lss_interrupt_shadow_defers_a_pending_irq_by_one_instruction() {
    // Same shape as `sti_interrupt_shadow_defers_interrupt_by_one_instruction`, but the
    // shadow is armed by LSS instead of STI: a pending IRQ must not be taken until the
    // instruction AFTER LSS has run.
    let mut memory = vec![0u8; 0x300];
    memory[0..5].copy_from_slice(&[0x0f, 0xb2, 0x1e, 0x00, 0x02]); // lss bx, [0x200]
    memory[5] = 0x90; // NOP -- executes before the interrupt is taken (shadow)
    memory[6] = 0x90; // NOP -- not reached; interrupt taken instead
    memory[0x200] = 0x00; // offset -> 0x0000
    memory[0x201] = 0x00;
    memory[0x202] = 0x00; // selector -> 0x0000 (SS stays flat at base 0 in real mode)
    memory[0x203] = 0x00;
    // IVT entry for vector 0x08 (IRQ0) at byte offset 0x20: offset=0x0208, segment=0.
    memory[0x20..0x22].copy_from_slice(&0x0208u16.to_le_bytes());
    memory[0x22..0x24].copy_from_slice(&0x0000u16.to_le_bytes());
    memory[0x208] = 0xcf; // IRET at the handler target (not reached in this test)

    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.set_flag(FLAG_IF, true);

    let mut bus = TestBus::with_memory(memory);
    // No interrupt pending yet: it "arrives" during LSS's execution window, which is
    // exactly the case the shadow exists to cover -- an IRQ landing between the LSS
    // and the next instruction boundary must wait for that boundary.

    // Cycle 1: LSS. SS reloads; the shadow arms. eip advances normally.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 5,
        "eip must be 5 after LSS -- NOP not yet executed"
    );
    assert!(
        cpu.interrupt_shadow,
        "the shadow must be armed immediately after LSS runs"
    );
    // The IRQ arrives now, after LSS has already committed.
    bus.pending_irq = Some(8);

    // Cycle 2: NOP. Shadow consumed at cycle start -> interrupt check skipped -> NOP
    // executes -> eip advances to 6. IRQ still pending.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 6,
        "eip must be 6 after NOP -- shadow let NOP through"
    );
    assert!(
        bus.pending_irq.is_some(),
        "interrupt must still be pending after NOP (shadow consumed, interrupt check skipped)"
    );

    // Cycle 3: no shadow, IF set, IRQ pending -> interrupt is acknowledged before fetch.
    cpu.cycle(&mut bus).unwrap();
    assert!(
        bus.pending_irq.is_none(),
        "interrupt must be taken after the shadow expires"
    );
}

#[test]
fn lss_32bit_operand_size_loads_esp_wide_offset() {
    // 66 0F B2 1E 00 02 -- lss ebx, [0x200] (operand-size override to 32-bit).
    // EBX <- dword[0x200], SS <- word[0x204].
    let mut memory = vec![0u8; 0x1000];
    memory[0..6].copy_from_slice(&[0x66, 0x0f, 0xb2, 0x1e, 0x00, 0x02]);
    memory[0x200..0x204].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    memory[0x204..0x206].copy_from_slice(&0x9000u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.ebx(), 0x1122_3344);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x9000);
}

#[test]
fn lfs_loads_offset_and_fs() {
    // lfs bx, [0x200]  (0F B4 1E 00 02). No interrupt shadow -- only LSS arms it.
    let mut memory = vec![0u8; 0x1000];
    memory[0..5].copy_from_slice(&[0x0f, 0xb4, 0x1e, 0x00, 0x02]);
    memory[0x200] = 0x34;
    memory[0x201] = 0x12;
    memory[0x202] = 0x00;
    memory[0x203] = 0x70;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    cpu.set_flag(FLAG_IF, true);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).selector, 0x7000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).base, 0x7000 << 4);
    assert!(
        !cpu.interrupt_shadow,
        "LFS must not arm the SS interrupt shadow"
    );
}

#[test]
fn lgs_loads_offset_and_gs() {
    // lgs bx, [0x200]  (0F B5 1E 00 02).
    let mut memory = vec![0u8; 0x1000];
    memory[0..5].copy_from_slice(&[0x0f, 0xb5, 0x1e, 0x00, 0x02]);
    memory[0x200] = 0x34;
    memory[0x201] = 0x12;
    memory[0x202] = 0x00;
    memory[0x203] = 0x60;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0x6000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).base, 0x6000 << 4);
    assert!(
        !cpu.interrupt_shadow,
        "LGS must not arm the SS interrupt shadow"
    );
}

#[test]
fn lss_with_register_operand_delivers_ud() {
    // lss bx, ax encoded with mod=3 (0F B2 C3) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there.
    let mut memory = vec![0u8; 1024];
    memory[0..3].copy_from_slice(&[0x0f, 0xb2, 0xc3]);
    memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lfs_with_register_operand_delivers_ud() {
    // lfs bx, ax (0F B4 C3, mod=3) -> #UD (vector 6).
    let mut memory = vec![0u8; 1024];
    memory[0..3].copy_from_slice(&[0x0f, 0xb4, 0xc3]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lgs_with_register_operand_delivers_ud() {
    // lgs bx, ax (0F B5 C3, mod=3) -> #UD (vector 6).
    let mut memory = vec![0u8; 1024];
    memory[0..3].copy_from_slice(&[0x0f, 0xb5, 0xc3]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lss_null_selector_faults_general_protection() {
    // In protected mode, LSS with a null selector must #GP -- SS can never be null,
    // the same rule any other SS load enforces (`null_selector_into_ss_still_faults`).
    let (mut cpu, mut memory) = protected_cpu(&[0x0f, 0xb2, 0x1e, 0x80, 0x01], 0, 0);
    // Far pointer at 0x180: offset 0x1234, selector 0x0000 (null).
    memory[0x180..0x182].copy_from_slice(&0x1234u16.to_le_bytes());
    memory[0x182..0x184].copy_from_slice(&0x0000u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "a null selector into SS via LSS must #GP(0), got {fault:?}"
    );
}

#[test]
fn lss_protected_mode_refreshes_the_ss_b_bit() {
    // Load SS via LSS from a 32-bit (B=1) data descriptor (selector 0x08, GDT). The cached
    // `default_size_32` (the B bit) must flip to match the new descriptor -- it comes free
    // through `load_segment` -> `descriptor_to_segment`, exactly like any other segment load.
    let descriptor_low = 0x0000_ffffu32; // limit low = 0xffff, base = 0
    let descriptor_high = 0x00cf_9200u32; // access=0x92 (present, data, writable), B=1, G=1
    let (mut cpu, mut memory) = protected_cpu(
        &[0x0f, 0xb2, 0x1e, 0x80, 0x01],
        descriptor_low,
        descriptor_high,
    );
    assert!(
        !cpu.registers.segment(SegmentIndex::Ss).default_size_32,
        "test setup: SS must start 16-bit (B=0) so the flip is observable"
    );
    // Far pointer at 0x180: offset 0x1234, selector 0x08.
    memory[0x180..0x182].copy_from_slice(&0x1234u16.to_le_bytes());
    memory[0x182..0x184].copy_from_slice(&0x0008u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x0008);
    assert!(
        cpu.registers.segment(SegmentIndex::Ss).default_size_32,
        "LSS must refresh SS.B to the loaded descriptor's B bit"
    );
}

#[test]
fn cbw_sign_extends_al_into_ax() {
    // cbw (0x98): al = 0x80 (-128) -> ax = 0xff80.
    let mut memory = vec![0; 64];
    memory[0] = 0x98;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff80);
}

#[test]
fn cwde_sign_extends_ax_into_eax() {
    // 0x66 0x98 (CWDE): ax = 0x8000 -> eax = 0xffff_8000.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x66, 0x98]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0000_8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xffff_8000);
}

#[test]
fn cwd_fills_dx_from_ax_sign() {
    // cwd (0x99): ax = 0x8000 (negative) -> dx = 0xffff, ax unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x99;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
}

#[test]
fn cwd_clears_dx_for_positive_ax() {
    // cwd (0x99): ax = 0x0001 (positive) -> dx = 0.
    let mut memory = vec![0; 64];
    memory[0] = 0x99;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.write_reg16(Reg16::Dx, 0xaaaa);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0000);
}

#[test]
fn cdq_fills_edx_from_eax_sign() {
    // 0x66 0x99 (CDQ): eax = 0x8000_0000 -> edx = 0xffff_ffff.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x66, 0x99]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x8000_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.edx(), 0xffff_ffff);
}

#[test]
fn sti_sets_interrupt_flag() {
    // sti (0xfb) sets IF.
    let mut memory = vec![0; 64];
    memory[0] = 0xfb;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_IF));
}

#[test]
fn lahf_loads_flag_byte_into_ah() {
    // lahf (0x9f). CF=PF=AF=ZF=SF=1 -> AH = 0xD5 | 0x02 = 0xD7; AL unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x9f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_PF, true);
    cpu.set_flag(FLAG_AF, true);
    cpu.set_flag(FLAG_ZF, true);
    cpu.set_flag(FLAG_SF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0xd7);
    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x00);
}

#[test]
fn sahf_loads_flags_from_ah_leaving_overflow() {
    // sahf (0x9e). AH=0xD7 -> CF=PF=AF=ZF=SF=1; OF untouched (a set OF survives).
    let mut memory = vec![0; 64];
    memory[0] = 0x9e;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xd700); // AH=0xD7, AL=0
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_PF, false);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_ZF, false);
    cpu.set_flag(FLAG_SF, false);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_PF));
    assert!(cpu.flag(FLAG_AF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn stc_sets_carry_and_cmc_toggles_it() {
    // stc (0xf9) sets CF; cmc (0xf5) toggles it back to 0.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xf9, 0xf5]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // stc
    assert!(cpu.flag(FLAG_CF));

    cpu.cycle(&mut bus).unwrap(); // cmc
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn pushf_then_popf_restores_flags() {
    // pushf (0x9c) ; popf (0x9d). pushf saves CF=1; CF is perturbed by hand;
    // popf restores it and reserved bit 1 stays set.
    let mut memory = vec![0; 1024];
    memory[0] = 0x9c;
    memory[1] = 0x9d;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // pushf
    cpu.set_flag(FLAG_CF, false); // perturb after the value is on the stack
    cpu.cycle(&mut bus).unwrap(); // popf

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.registers.eflags & 0x2, 0x2);
}

#[test]
fn leave_restores_sp_and_bp() {
    // leave (0xc9): sp <- bp; bp <- pop. bp = 0x0200, [ss:0x0200] = 0x1234.
    // Result: bp = 0x1234, sp = 0x0202 (0x0200 then +2 from the pop).
    let mut memory = vec![0; 1024];
    memory[0] = 0xc9;
    memory[0x200..0x202].copy_from_slice(&0x1234u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0080);
    cpu.write_gpr16(5, 0x0200); // BP
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr16(5), 0x1234);
    assert_eq!(cpu.read_gpr16(4), 0x0202);
}

#[test]
fn pusha_then_popa_round_trips_and_saves_original_sp() {
    // pusha (0x60) ; popa (0x61). All GPRs round-trip; the SP slot holds the
    // pre-pusha SP and popa discards it, so SP returns to its starting value.
    let mut memory = vec![0; 1024];
    memory[0] = 0x60;
    memory[1] = 0x61;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.write_gpr16(0, 0x1111);
    cpu.write_gpr16(1, 0x2222);
    cpu.write_gpr16(2, 0x3333);
    cpu.write_gpr16(3, 0x4444);
    cpu.write_gpr16(5, 0x6666);
    cpu.write_gpr16(6, 0x7777);
    cpu.write_gpr16(7, 0x8888);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // pusha: 8 words, sp 0x0100 -> 0x00f0
    assert_eq!(cpu.read_gpr16(4), 0x00f0);
    // the 5th push (the SP slot) lands at 0x0100 - 2*5 = 0x00f6 and holds 0x0100
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xf6], bus.memory[0xf7]]),
        0x0100
    );

    cpu.cycle(&mut bus).unwrap(); // popa
    assert_eq!(cpu.read_gpr16(0), 0x1111);
    assert_eq!(cpu.read_gpr16(1), 0x2222);
    assert_eq!(cpu.read_gpr16(2), 0x3333);
    assert_eq!(cpu.read_gpr16(3), 0x4444);
    assert_eq!(cpu.read_gpr16(5), 0x6666);
    assert_eq!(cpu.read_gpr16(6), 0x7777);
    assert_eq!(cpu.read_gpr16(7), 0x8888);
    assert_eq!(cpu.read_gpr16(4), 0x0100);
}

#[test]
fn pushfd_pushes_only_defined_eflags_bits() {
    // 0x66 0x9c PUSHFD. EFLAGS carries garbage in the high bits; the 486 pushes
    // the defined low 16 plus AC (bit 18) and ID (bit 21). With every high bit
    // set in the source, the dword on the stack is 0x0024_0493.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0x9c]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.eflags = 0xfffc_0493;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    let pushed = u32::from_le_bytes([
        bus.memory[0xfc],
        bus.memory[0xfd],
        bus.memory[0xfe],
        bus.memory[0xff],
    ]);
    assert_eq!(pushed, 0x0024_0493);
    assert_eq!(cpu.registers.esp(), 0x0000_00fc);
}

#[test]
fn pushad_uses_16bit_sp_and_preserves_high_esp() {
    // 0x66 0x60 PUSHAD on a 16-bit stack: SP wraps within the segment and ESP[31:16]
    // is preserved. ESP = 0x0001_0010 -> SP 0x10 - 32 wraps to 0xfff0.
    let mut memory = vec![0; 0x2_0000];
    memory[0..2].copy_from_slice(&[0x66, 0x60]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0001_0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.esp(), 0x0001_fff0);
}

#[test]
fn popad_leaks_discarded_esp_high_half_on_16bit_stack() {
    // 0x66 0x61 POPAD on a 16-bit stack: the discarded saved-ESP slot's high half
    // lands in ESP[31:16] while SP keeps the advanced value (a 386 quirk).
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0x61]);
    // The discard is the 4th dword, at SP + 12 = 0x20c.
    memory[0x20c..0x210].copy_from_slice(&0x5a04_6b18u32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // SP 0x200 + 32 = 0x220; high half from the discarded slot = 0x5a04.
    assert_eq!(cpu.registers.esp(), 0x5a04_0220);
}

#[test]
fn pop_rm16_into_memory_disp16() {
    // 8F /0 with mod=00 rm=110 disp16: POP word [0x0200]. The encoding the
    // Wizardry III booter uses (with a CS override). Pops the stack top into
    // the memory word and advances SP by 2. Arithmetic flags are untouched.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x8f, 0x06, 0x00, 0x02]);
    // Stack top at ss:0x0100 = 0xbeef.
    memory[0x100..0x102].copy_from_slice(&0xbeefu16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.eflags = 0x0000_0ed7; // all arithmetic flags set
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0xbeef
    );
    assert_eq!(cpu.read_gpr16(4), 0x0102); // SP advanced by 2
    assert_eq!(cpu.registers.eflags, 0x0000_0ed7); // flags unchanged
}

#[test]
fn pop_rm16_into_register() {
    // 8F /0 with mod=11 rm=011: POP BX. Register destination form.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x8f, 0xc3]);
    memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.read_gpr16(4), 0x0102);
}

#[test]
fn pop_rm32_into_register_preserves_high_esp() {
    // 0x66 8F /0 mod=11 rm=001: POP ECX, 32-bit operand on a 16-bit stack.
    // The full dword loads into ECX; SP advances by 4 and ESP[31:16] is kept.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0x66, 0x8f, 0xc1]);
    memory[0x100..0x104].copy_from_slice(&0xcafe_f00du32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0xdead_0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.ecx(), 0xcafe_f00d);
    assert_eq!(cpu.registers.esp(), 0xdead_0104);
}

#[test]
fn pop_rm_reg_nonzero_is_illegal() {
    // 8F with reg != 0 is an illegal group encoding (group 1A reserves only /0), delivered as
    // a #UD through the real-mode IVT. Code is placed away from offset 0 so it doesn't
    // overlap the vector-0 IVT slot this test doesn't use, and vector 6's slot is populated
    // with a distinguishing trap address.
    const ORIGIN: usize = 0x10;
    const UD_TRAP_CS: u16 = 0x0300;
    const UD_TRAP_IP: u16 = 0x0020;
    let mut memory = vec![0; 1024];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0x8f, 0xcb]); // mod=11 reg=001 rm=011
    memory[6 * 4..6 * 4 + 2].copy_from_slice(&UD_TRAP_IP.to_le_bytes());
    memory[6 * 4 + 2..6 * 4 + 4].copy_from_slice(&UD_TRAP_CS.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esp(0x0300);
    let mut bus = TestBus::with_memory(memory);

    let outcome = cpu
        .cycle(&mut bus)
        .expect("a delivered #UD must not error `cycle`");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, UD_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(UD_TRAP_IP));
}

#[test]
fn pop_rm32_esp_relative_destination_uses_post_increment_esp() {
    // The falsifier: push A; push B; pop dword [esp+4] must write B to the
    // POST-increment [esp+4] (the original pre-push top of stack, 0x0200) --
    // not the pre-pop [esp+4] (0x01fc, the slot that holds A), which is what
    // resolving the EA before the pop would compute.
    //
    // 0x66 0x67 8F /0 mod=01 rm=100 (SIB: base=ESP, no index) disp8=0x04:
    // POP dword [esp+4], 32-bit operand + 32-bit address override in real mode.
    let mut memory = vec![0; 1024];
    memory[0..6].copy_from_slice(&[0x66, 0x67, 0x8f, 0x44, 0x24, 0x04]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    // push A; push B (32-bit pushes on the 32-bit-addressed stack).
    cpu.push(&mut bus, 0xaaaa_aaaa, OperandSize::Dword).unwrap();
    cpu.push(&mut bus, 0xbbbb_bbbb, OperandSize::Dword).unwrap();
    assert_eq!(cpu.registers.esp(), 0x01f8);
    // Stack image: [0x01f8]=B (top), [0x01fc]=A.

    cpu.cycle(&mut bus).unwrap();

    // The pop reads B from 0x01f8 and advances esp to 0x01fc first; [esp+4]
    // computed AFTER that lands at 0x0200 (untouched before this instruction),
    // not at the pre-pop [esp+4] == 0x01fc (the slot holding A).
    assert_eq!(cpu.registers.esp(), 0x01fc, "pop advanced esp by 4");
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x0200..0x0204].try_into().unwrap()),
        0xbbbb_bbbb,
        "post-increment EA wrote B to the pre-push top of stack, not A's slot"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x01fc..0x0200].try_into().unwrap()),
        0xaaaa_aaaa,
        "A's slot is untouched: a pre-pop EA would have overwritten it with B"
    );
}

#[test]
fn pop_rm16_esp_relative_destination_uses_post_increment_esp() {
    // 16-bit variant of the falsifier above. 8F /0 mod=01 rm=100 (SIB: base=SP
    // is not directly encodable in 16-bit addressing -- 16-bit ModRM has no SIB
    // byte -- so this uses 32-bit addressing with a 16-bit operand: 0x67 8F /0
    // mod=01 rm=100 disp8=0x02, POP word [esp+2].
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x67, 0x8f, 0x44, 0x24, 0x02]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.push(&mut bus, 0xaaaa, OperandSize::Word).unwrap();
    cpu.push(&mut bus, 0xbbbb, OperandSize::Word).unwrap();
    assert_eq!(cpu.registers.esp(), 0x01fc);
    // Stack image: [0x01fc]=B (top), [0x01fe]=A.

    cpu.cycle(&mut bus).unwrap();

    // The pop reads B from 0x01fc and advances esp to 0x01fe first; [esp+2]
    // computed AFTER that lands at 0x0200 (untouched before this instruction),
    // not at the pre-pop [esp+2] == 0x01fe (the slot holding A).
    assert_eq!(cpu.registers.esp(), 0x01fe, "pop advanced esp by 2");
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x0200..0x0202].try_into().unwrap()),
        0xbbbb,
        "post-increment EA wrote B to the pre-push top of stack, not A's slot"
    );
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x01fe..0x0200].try_into().unwrap()),
        0xaaaa,
        "A's slot is untouched: a pre-pop EA would have overwritten it with B"
    );
}

#[test]
fn pop_rm32_esp_relative_destination_restores_esp_on_page_fault() {
    // (c) A faulting destination write must leave ESP exactly as it was before
    // the instruction started: the pop's ESP advance must be unwound so the
    // instruction is cleanly restartable after the guest's #PF handler fixes up
    // the mapping.
    //
    // PD at 0x1000, PT at 0x2000. Linear page 0 (code + the stack the pop reads
    // from) is identity-mapped present+writable. Linear page 0x3000 (where the
    // post-increment `[esp+4]` destination lands) has NO PTE at all, so the
    // destination write takes a #PF.
    let mut memory = vec![0; 0x4000];
    // Code at linear 0: POP dword [esp+4] (32-bit operand + 32-bit address
    // override in real mode).
    memory[0..6].copy_from_slice(&[0x66, 0x67, 0x8f, 0x44, 0x24, 0x04]);
    // The stack top read by the pop, at linear 0x2ffc: value B.
    memory[0x2ffc..0x3000].copy_from_slice(&0xbbbb_bbbbu32.to_le_bytes());
    // PDE[0] -> PT at 0x2000, present+rw+user.
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    // PTE[0] (linear 0x0000-0x0fff: code + the read side of the stack) -> identity, present+rw.
    memory[0x2000..0x2004].copy_from_slice(&0x0000_0007u32.to_le_bytes());
    // PTE[2] (linear 0x2000-0x2fff, covers 0x2ffc) -> identity, present+rw.
    memory[0x2008..0x200c].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    // PTE[3] (linear 0x3000-0x3fff, the POP destination) intentionally left 0 (not present).
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PG;
    // ESP = 0x2ffc: the pop reads from here (mapped), advances to 0x3000, and
    // the destination EA [esp+4] (post-increment) is 0x3004 -- inside the
    // unmapped page 0x3000, so the write faults.
    cpu.registers.set_esp(0x2ffc);
    let esp_before = cpu.registers.esp();
    let mut bus = TestBus::with_memory(memory);

    // Use the raw decode/execute split (no exception delivery) so the assert
    // below observes ESP exactly as `execute_decoded` left it, not after a
    // real-mode #PF delivery has also pushed flags/CS/IP onto that same stack.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 14,
                error_code: Some(_)
            }
        ),
        "{fault:?}"
    );
    assert_eq!(
        cpu.registers.esp(),
        esp_before,
        "a faulting destination write must leave esp exactly pre-instruction"
    );
}

#[test]
fn push_rm32_esp_source_reads_before_decrement() {
    // (d) PUSH r/m32 with an ESP-based memory source (JEMM's V86_MonitorEx
    // executes `push dword [esp]`) must read the source BEFORE the decrement:
    // the value pushed is the current top of stack, duplicating it, not
    // whatever ends up below the new top.
    //
    // 0x66 0x67 FF /6 mod=00 rm=100 (SIB: base=ESP, no index, no disp): PUSH
    // dword [esp], 32-bit operand + 32-bit address override in real mode.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x66, 0x67, 0xff, 0x34, 0x24]);
    memory[0x0200..0x0204].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.esp(), 0x01fc, "push decremented esp by 4");
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x01fc..0x0200].try_into().unwrap()),
        0xdead_beef,
        "the duplicated top-of-stack value, read before the decrement"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x0200..0x0204].try_into().unwrap()),
        0xdead_beef,
        "the original top-of-stack slot is untouched"
    );
}

#[test]
fn pop_rm32_non_esp_base_is_unchanged() {
    // (e) A non-ESP base (EBX here) is unaffected by the pop-then-resolve
    // reorder: the destination EA never depended on ESP in the first place.
    //
    // 0x66 0x67 8F /0 mod=01 rm=011 disp8=0x10: POP dword [ebx+0x10].
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x66, 0x67, 0x8f, 0x43, 0x10]);
    memory[0x0100..0x0104].copy_from_slice(&0xcafe_babeu32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.set_ebx(0x0300);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.esp(), 0x0104);
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x0310..0x0314].try_into().unwrap()),
        0xcafe_babe,
        "ebx+0x10 destination, unaffected by esp timing"
    );
}

#[test]
fn retf_pops_offset_then_segment() {
    // retf (0xcb). Stack at ss:0x0100 holds ip 0x0100 then cs 0x3000.
    let mut memory = vec![0; 1024];
    memory[0] = 0xcb;
    memory[0x100..0x104].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x0104); // two word pops from 0x0100
}

#[test]
fn far_call_then_retf_round_trips() {
    // call far 0x0000:0x0010 ; the target at 0x10 is retf (0xcb).
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x9a, 0x10, 0x00, 0x00, 0x00]);
    memory[0x10] = 0xcb;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // far call -> cs:0x0000, eip 0x0010
    assert_eq!(cpu.registers.eip, 0x0010);
    cpu.cycle(&mut bus).unwrap(); // retf -> back to cs:0x0000, eip 0x0005
    assert_eq!(cpu.registers.cs().selector, 0x0000);
    assert_eq!(cpu.registers.eip, 0x0005);
    assert_eq!(cpu.read_gpr16(4), 0x0100); // sp restored
}

#[test]
fn ret_near_imm16_pops_and_releases() {
    // ret 0x0004  (0xc2 0x04 0x00). Return ip 0x0100 at ss:0x0100.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xc2, 0x04, 0x00]);
    memory[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0100);
    // sp: 0x0100 -> +2 (word pop) -> +4 (release) = 0x0106
    assert_eq!(cpu.read_gpr16(4), 0x0106);
}

#[test]
fn ret_near_imm16_32bit_preserves_high_esp() {
    // 0x66 0xc2 0x04 0x00 : 32-bit ret, release 4. Pop eip (dword), then release.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x66, 0xc2, 0x04, 0x00]);
    memory[0x100..0x104].copy_from_slice(&0x0000_0100u32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0xdead_0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0000_0100);
    // real-mode 16-bit stack: only SP moves, ESP[31:16] preserved.
    // 0x0100 -> +4 (dword pop) -> +4 (release) = 0x0108
    assert_eq!(cpu.registers.esp(), 0xdead_0108);
}

#[test]
fn retf_imm16_pops_far_and_releases() {
    // retf 0x0004  (0xca 0x04 0x00). Stack: ip 0x0100 then cs 0x3000 at ss:0x0100.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xca, 0x04, 0x00]);
    memory[0x100..0x104].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    // sp: 0x0100 -> +4 (far pop) -> +4 (release) = 0x0108
    assert_eq!(cpu.read_gpr16(4), 0x0108);
}

#[test]
fn release_stack_wraps_sp_and_preserves_high_esp_in_real_mode() {
    // release_stack alone, with no surrounding pop, must move only SP on a
    // real-mode 16-bit stack and wrap at the 16-bit boundary. ESP[31:16] must
    // not absorb the carry: a full-ESP add of 0xbeef_fffe + 4 would carry into
    // 0xbef0_0002, while the SP-only path gives 0xbeef_0002.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(0xbeef_fffe);

    cpu.release_stack(4);

    assert_eq!(cpu.registers.esp(), 0xbeef_0002);
}

/// Load a protected-mode SS segment register directly (bypassing GDT resolution)
/// with the given B bit, for exercising `stack_is_32bit()` in isolation.
fn set_protected_ss(cpu: &mut CpuGsw, base: u32, default_size_32: bool) {
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x10,
            base,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32,
        },
    );
}

#[test]
fn push_dword_on_a_16bit_protected_mode_stack_wraps_sp_and_preserves_high_esp() {
    // The DOS4GW/VCPI scenario: protected mode, a 32-bit push, but SS.B=0 (a
    // 16-bit stack segment). Only SP must wrap; ESP[31:16] survives untouched,
    // and the write lands at SS.base + the wrapped SP, not at SS.base + ESP.
    let memory = vec![0u8; 0x1_0002];
    let mut cpu = CpuGsw::default();
    set_protected_ss(&mut cpu, 0, false);
    cpu.registers.set_esp(0xbeef_0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.push(&mut bus, 0x1122_3344, OperandSize::Dword).unwrap();

    // sp 0x0002 -> wraps to 0xfffe; ESP high half (0xbeef) preserved.
    assert_eq!(cpu.registers.esp(), 0xbeef_fffe);
    let read = bus
        .read_memory_direct(0xfffe, BusWidth::Dword, BusAccessKind::DataRead)
        .unwrap();
    assert_eq!(read.value, 0x1122_3344);
}

#[test]
fn pop_dword_on_a_16bit_protected_mode_stack_wraps_sp_and_preserves_high_esp() {
    // Mirror of the push case: SS.B=0 in protected mode reads from the wrapped
    // SP and advances only SP, leaving ESP[31:16] alone.
    let mut memory = vec![0u8; 0x1_0002];
    memory[0xfffe..0x1_0002].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    set_protected_ss(&mut cpu, 0, false);
    cpu.registers.set_esp(0xbeef_fffe);
    let mut bus = TestBus::with_memory(memory);

    let value = cpu.pop(&mut bus, OperandSize::Dword).unwrap();

    assert_eq!(value, 0x1122_3344);
    // sp 0xfffe -> +4 wraps to 0x0002; ESP high half preserved.
    assert_eq!(cpu.registers.esp(), 0xbeef_0002);
}

#[test]
fn push_dword_on_a_32bit_protected_mode_stack_uses_full_esp() {
    // SS.B=1 (the TOKAEMM monitor's stack shape): full-ESP arithmetic, no wrap
    // at the 16-bit boundary, matching today's protected-mode behavior.
    let memory = vec![0u8; 0x2_0000];
    let mut cpu = CpuGsw::default();
    set_protected_ss(&mut cpu, 0, true);
    cpu.registers.set_esp(0x0001_0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.push(&mut bus, 0xaabb_ccdd, OperandSize::Dword).unwrap();

    assert_eq!(cpu.registers.esp(), 0x0000_fffe);
    let read = bus
        .read_memory_direct(0x0000_fffe, BusWidth::Dword, BusAccessKind::DataRead)
        .unwrap();
    assert_eq!(read.value, 0xaabb_ccdd);
}

#[test]
fn pop_dword_on_a_32bit_protected_mode_stack_uses_full_esp() {
    let mut memory = vec![0u8; 0x2_0000];
    memory[0x0000_fffe..0x0001_0002].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());
    let mut cpu = CpuGsw::default();
    set_protected_ss(&mut cpu, 0, true);
    cpu.registers.set_esp(0x0000_fffe);
    let mut bus = TestBus::with_memory(memory);

    let value = cpu.pop(&mut bus, OperandSize::Dword).unwrap();

    assert_eq!(value, 0xaabb_ccdd);
    assert_eq!(cpu.registers.esp(), 0x0001_0002);
}

#[test]
fn ss_load_populates_the_cached_b_bit_from_the_descriptor() {
    // A GDT-resolved SS load must cache B from descriptor bit 22, and a
    // subsequent real-mode load must clear it back to false.
    let mut memory = vec![0u8; 4096];
    // GDT at 0, entry 1 (selector 0x08): base 0, limit 0xfffff (4K gran), B=1
    // data segment, present, DPL 0. Access byte 0x93 (present, data, r/w).
    // High dword: limit high nibble 0xf | G=1,D/B=1 -> 0xc0 | 0x0f = 0xcf in bits 16-23,
    // access in bits 8-15.
    let low: u32 = 0xffff; // limit low
    let high: u32 = 0x00cf_9300u32; // G=1,B=1,limit_high=0xf,access=0x93
    memory[8..12].copy_from_slice(&low.to_le_bytes());
    memory[12..16].copy_from_slice(&high.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.gdtr.base = 0;
    cpu.gdtr.limit = 0xffff;
    let mut bus = TestBus::with_memory(memory);

    cpu.load_segment(&mut bus, SegmentIndex::Ss, 0x08).unwrap();
    assert!(cpu.stack_is_32bit());

    cpu.load_segment_real(SegmentIndex::Ss, 0);
    assert!(!cpu.stack_is_32bit());
}

/// A flat protected-mode CPU with code at linear 0, CS.D from `code_d32`, and SS.B
/// from `stack_b32` -- for exercising ENTER/LEAVE's SS.B-vs-operand-size split.
fn protected_cpu_with_cs_d_and_ss_b(
    code: &[u8],
    mem_len: usize,
    code_d32: bool,
    stack_b32: bool,
) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; mem_len];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x08,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: code_d32,
        },
    );
    set_protected_ss(&mut cpu, 0, stack_b32);
    cpu.registers.eip = 0;
    (cpu, memory)
}

#[test]
fn leave_on_a_32bit_stack_moves_full_esp_even_with_a_16bit_operand_size() {
    // LEAVE with a 0x66 operand-size prefix (word EBP/BP pop) on an SS.B=1 stack.
    // Per PRM 17-96, StackAddrSize=32 => ESP <- EBP unconditionally: the full
    // register, not the low word, regardless of the operand size. EBP carries a
    // high word (0x0002) distinct from ESP's stale high word (0xdead) that must
    // land in ESP whole; a truncating write would leave ESP's stale 0xdead high
    // half instead of EBP's 0x0002.
    let (mut cpu, memory) = protected_cpu_with_cs_d_and_ss_b(&[0x66, 0xc9], 0x3_0000, true, true);
    cpu.write_gpr32(5, 0x0002_0100); // EBP
    cpu.registers.set_esp(0xdead_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        cpu.registers.esp(),
        0x0002_0100 + 2,
        "ESP <- full EBP, then +2 from the 16-bit pop"
    );
}

#[test]
fn leave_on_a_16bit_stack_moves_only_sp_and_preserves_high_esp() {
    // Mirror on an SS.B=0 stack (real mode's rule, still true in protected mode):
    // only SP takes BP's value; ESP's high word is untouched.
    let (mut cpu, memory) = protected_cpu_with_cs_d_and_ss_b(&[0xc9], 0x1_0000, false, false);
    cpu.write_gpr16(5, 0x0200); // BP
    cpu.registers.set_esp(0xbeef_0080);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // SP <- 0x0200, then +2 from the pop = 0x0202; high half preserved.
    assert_eq!(cpu.registers.esp(), 0xbeef_0202);
}

#[test]
fn enter_op32_on_a_16bit_stack_saves_frame_ptr_from_sp_not_esp() {
    // ENTER imm16,1 (op32) on an SS.B=0 stack: frame-ptr <- eSP is the 16-bit SP
    // (386 PRM 17-62), not the full (garbage-laden) ESP. With nesting level 1 the
    // frame-ptr is pushed once more, so the pushed dword must carry the wrapped SP
    // zero-extended, not ESP's high garbage.
    let (mut cpu, memory) =
        protected_cpu_with_cs_d_and_ss_b(&[0xc8, 0x04, 0x00, 0x01], 0x1_0000, true, false);
    cpu.registers.set_esp(0xbeef_0100);
    cpu.write_gpr32(5, 0); // EBP, arbitrary
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // Push(EBP) at SP 0x0100 -> SP=0x00fc; frame-ptr = SP = 0x00fc (not
    // 0xbeef_00fc); level>0 so frame-ptr is pushed again at SP=0x00f8; final
    // alloc SP -= 4 = 0x00f4. High half of ESP preserved throughout.
    let frame_ptr_slot = u32::from_le_bytes(bus.memory[0xf8..0xfc].try_into().unwrap());
    assert_eq!(
        frame_ptr_slot, 0x00fc,
        "pushed frame-ptr is the 16-bit SP, zero-extended, not ESP-high garbage"
    );
    assert_eq!(cpu.registers.esp(), 0xbeef_00f4);
    assert_eq!(
        cpu.read_gpr32(5),
        0x00fc,
        "EBP <- frame-ptr (zero-extended)"
    );
}

#[test]
fn far_call_pushes_return_and_loads_target() {
    // call far 0x3000:0x0100  (0x9a 0x00 0x01 0x00 0x30), a 5-byte instruction.
    // Pushes CS (0x0000) then the return IP (0x0005), then loads cs:eip.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x9a, 0x00, 0x01, 0x00, 0x30]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x00fc); // two word pushes from 0x0100
    // CS at the higher slot, return IP just below it
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0000
    );
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfc], bus.memory[0xfd]]),
        0x0005
    );
}

#[test]
fn far_call_via_memory_pushes_return_and_transfers() {
    // call far [0x0200]  (0xff 0x1e 0x00 0x02), a 4-byte instruction. The far
    // pointer at ds:0x0200 is offset 0x0100, selector 0x3000.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0xff, 0x1e, 0x00, 0x02]);
    memory[0x200..0x204].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x00fc);
    // return CS 0x0000 at the higher slot, return IP 0x0004 below it
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0000
    );
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfc], bus.memory[0xfd]]),
        0x0004
    );
}

#[test]
fn far_jmp_via_memory_transfers_without_pushing() {
    // jmp far [0x0200]  (0xff 0x2e 0x00 0x02). Pointer = offset 0x0100, selector 0x3000.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0xff, 0x2e, 0x00, 0x02]);
    memory[0x200..0x204].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x0100); // nothing pushed
}

#[test]
fn far_call_via_register_operand_delivers_ud() {
    // 0xff /3 with mod=3 (0xff 0xd8) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there and clears IF.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0xd8]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn far_jmp_via_register_operand_delivers_ud() {
    // 0xff /5 with mod=3 (0xff 0xe8) is an invalid encoding -> #UD (vector 6).
    // The conformance suite pre-skips exception vectors, so this is the only
    // guard that the register form of the far JMP faults rather than transfers.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0xe8]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn far_call_via_memory_wraps_selector_offset_at_64k() {
    // call far [bx+di] (0xff 0x19) with bx+di = 0xfffe. On a 16-bit real-mode
    // segment the IP is read at ds:0xfffe and the selector offset wraps to
    // ds:0x0000 rather than reading past the 0xffff limit; a real 80386
    // completes this without faulting (SingleStepTests FF.3 "call far
    // [ds:bx+di]" with bx=di=0xffff).
    let ds_base = 0x2_0000usize; // ds selector 0x2000
    let mut memory = vec![0; 0x3_0000];
    memory[0..2].copy_from_slice(&[0xff, 0x19]);
    // IP at ds:0xfffe
    memory[ds_base + 0xfffe..ds_base + 0x1_0000].copy_from_slice(&0x0100u16.to_le_bytes());
    // selector at the wrapped ds:0x0000
    memory[ds_base..ds_base + 2].copy_from_slice(&0x3000u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0x2000);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.write_gpr16(3, 0xfffe); // bx
    cpu.write_gpr16(7, 0x0000); // di
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x00fc); // pushed CS then return IP
}

#[test]
fn retf_32bit_pops_full_eip_and_preserves_high_esp() {
    // 0x66 0xcb (32-bit RETF). Pops EIP (dword, not masked to 16) then CS
    // (dword, truncated to the selector). On the real-mode 16-bit stack only
    // SP moves, so ESP[31:16] is preserved.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0xcb]);
    memory[0x100..0x104].copy_from_slice(&0x0001_2345u32.to_le_bytes()); // EIP
    memory[0x104..0x108].copy_from_slice(&0x0000_3000u32.to_le_bytes()); // CS
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0xcafe_0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0001_2345);
    // sp 0x0100 -> +8 (two dword pops) = 0x0108, high half preserved
    assert_eq!(cpu.registers.esp(), 0xcafe_0108);
}

#[test]
fn rep_legacy_invoice_preserves_returns_and_effects() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            for opcode in [0xa4, 0xa5, 0xaa, 0xab] {
                for dword in [false, true] {
                    for address32 in [false, true] {
                        for backwards in [false, true] {
                            for count in [0u32, 1, 2, 4097] {
                                let mut code = vec![0xf3];
                                if dword {
                                    code.push(0x66);
                                }
                                if address32 {
                                    code.push(0x67);
                                }
                                code.push(opcode);
                                let (mut cpu, mut memory) = real_mode_cpu(&code, 0x10000);
                                cpu.set_mode(mode);
                                cpu.set_timing_epoch(epoch);
                                cpu.load_segment_real(SegmentIndex::Es, 0);
                                cpu.registers.set_eax(0x5a5a5a5a);
                                cpu.registers.set_ecx(count);
                                let width = if opcode & 1 == 0 {
                                    1
                                } else if dword {
                                    4
                                } else {
                                    2
                                };
                                let bytes = count as usize * width;
                                memory[0x2000..0x2000 + bytes].fill(0x5a);
                                let offset = if backwards {
                                    bytes.saturating_sub(width)
                                } else {
                                    0
                                } as u32;
                                cpu.registers.set_esi(0x2000 + offset);
                                cpu.registers.set_edi(0x8000 + offset);
                                cpu.set_flag(FLAG_DF, backwards);
                                let mut atomic = cpu.clone();
                                let mut bus = TestBus::with_memory(memory.clone());
                                bus.direct_page_clocks = true;
                                bus.report_batch_clocks = true;
                                let mut atomic_bus = TestBus::with_memory(memory);
                                atomic_bus.direct_page_clocks = true;
                                atomic_bus.report_batch_clocks = true;
                                let start = cpu.elapsed_clocks;
                                let first = cpu.run_budgeted(&mut bus, 0).unwrap();
                                let raw = if epoch == 1 { 4 } else { 48 };
                                assert_eq!(first.consumed_core_clocks, raw / 12);
                                assert_eq!(cpu.timing_rem, raw % 12);
                                assert_eq!(cpu.registers.ecx(), count);
                                let mut total = first.consumed_core_clocks;
                                let mut chunks = 0;
                                while cpu.rep_resume_active {
                                    let before = cpu.elapsed_clocks;
                                    let result = cpu.run_budgeted(&mut bus, 0).unwrap();
                                    assert_eq!(
                                        result.consumed_core_clocks, 0,
                                        "mode={mode:?} epoch={epoch} opcode={opcode:x}"
                                    );
                                    assert_eq!(cpu.elapsed_clocks, before);
                                    assert_eq!(cpu.timing_rem, raw % 12);
                                    total += result.consumed_core_clocks;
                                    chunks += 1;
                                    assert!(chunks <= count);
                                }
                                let atomic_result = atomic.cycle(&mut atomic_bus).unwrap();
                                assert_eq!(total, atomic_result.core_clocks);
                                assert_eq!(cpu.elapsed_clocks - start, total);
                                assert_eq!(cpu.registers, atomic.registers);
                                assert_eq!(bus.memory, atomic_bus.memory);
                                assert_eq!(&bus.memory[0x8000..0x8000 + bytes], vec![0x5a; bytes]);
                                assert_eq!(
                                    bus.trace.elapsed_clocks(),
                                    atomic_bus.trace.elapsed_clocks()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn rep_legacy_history_keeps_initial_count_after_a_zero_quotient_payment() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (mut cpu, memory) = real_mode_cpu(&[0xf3, 0xa4], 0x1000);
        cpu.set_mode(mode);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.set_ecx(2);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_page_clocks = true;
        bus.report_batch_clocks = true;
        let first = cpu.run_budgeted(&mut bus, 0).unwrap();
        assert_eq!(first.consumed_core_clocks, 0);
        assert_eq!(cpu.timing_rem, 4);
        assert_eq!(
            cpu.rep_execution.resume.as_ref().unwrap().price_history,
            Some(RepPriceHistory {
                initial_count: 2,
                startup_paid: true
            })
        );
        let next = cpu.run_budgeted(&mut bus, 0).unwrap();
        assert_eq!(next.consumed_core_clocks, 0);
        assert_eq!(cpu.registers.ecx(), 1);
        assert_eq!(
            cpu.rep_execution.resume.as_ref().unwrap().price_history,
            Some(RepPriceHistory {
                initial_count: 2,
                startup_paid: true
            })
        );
        assert_eq!(cpu.timing_rem, 4);
    }
}

#[test]
fn rep_legacy_partial_fault_invoice_counts_only_completed_elements() {
    for destination_fault in [false, true] {
        let (mut cpu, mut memory) = real_mode_cpu(&[0xf3, 0xa4], 0x1000);
        cpu.set_mode(GswMode::Gsw586);
        cpu.set_timing_epoch(2);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        memory[0x100..0x103].copy_from_slice(&[0x31, 0x42, 0x53]);
        cpu.registers.set_ecx(3);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let segment = if destination_fault {
            SegmentIndex::Es
        } else {
            SegmentIndex::Ds
        };
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.limit = if destination_fault { 0x201 } else { 0x101 };
        cpu.registers.set_segment(segment, descriptor);
        let mut bus = TestBus::with_memory(memory);
        let insn = cpu.decode(&mut bus).unwrap();
        let mut work = InstructionWork::default();
        let result = cpu.execute_decoded(&insn, &mut bus, &mut work);
        assert!(matches!(
            result,
            Err(InternalFault::Exception { vector: 13, .. })
        ));
        let invoice = work.rep.unwrap();
        assert_eq!(invoice.completed, 2);
        assert_eq!(invoice.raw_due, 0);
        assert!(!invoice.history.startup_paid);
        assert_eq!(invoice.history.initial_count, 3);
        assert_eq!(cpu.registers.ecx(), 1);
        assert_eq!(&bus.memory[0x200..0x203], &[0x31, 0x42, 0]);
        assert_eq!(work.committed.total(), 0);
    }
}

#[test]
#[should_panic(expected = "REP resume has unpaid startup")]
fn rep_legacy_resumed_yield_rejects_a_lost_payment() {
    let (mut cpu, memory) = real_mode_cpu(&[0xf3, 0xa4], 0x1000);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.set_mode(GswMode::Gsw586);
    cpu.registers.set_ecx(2);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;
    cpu.run_budgeted(&mut bus, 0).unwrap();
    cpu.rep_execution
        .resume
        .as_mut()
        .unwrap()
        .price_history
        .as_mut()
        .unwrap()
        .startup_paid = false;
    cpu.run_budgeted(&mut bus, 0).unwrap();
}

fn rep_legacy_fixture(mode: GswMode, epoch: u32, prefix: u8, count: u32) -> (CpuGsw, TestBus) {
    let (mut cpu, mut memory) = real_mode_cpu(&[0x90, prefix, 0xa4, 0xb0, 0x5a], 0x10000);
    cpu.set_mode(mode);
    cpu.set_timing_epoch(epoch);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 1;
    cpu.registers.set_ecx(count);
    cpu.registers.set_esi(0x2000);
    cpu.registers.set_edi(0x8000);
    cpu.registers.set_esp(0x1000);
    memory[0x2000..0x2000 + count as usize].fill(0x6d);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;
    (cpu, bus)
}

#[test]
fn rep_legacy_full_chunk_and_cloned_resume_keep_one_startup() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            let (mut cpu, mut bus) = rep_legacy_fixture(mode, epoch, 0xf3, 4097);
            let first = cpu.run_budgeted(&mut bus, u64::MAX).unwrap();
            assert_eq!(cpu.registers.ecx(), 1);
            assert_eq!(cpu.perf.rep_string_iterations, 4096);
            assert_eq!(&bus.memory[0x8000..0x9000], &[0x6d; 4096]);
            let history = cpu
                .rep_execution
                .resume
                .as_ref()
                .unwrap()
                .price_history
                .unwrap();
            assert_eq!(history.initial_count, 4097);
            assert!(history.startup_paid);
            let mut clone = cpu.clone();
            let mut clone_bus = TestBus::with_memory(bus.memory.to_vec());
            clone_bus.direct_page_clocks = true;
            clone_bus.report_batch_clocks = true;
            assert_eq!(clone.rep_execution.resume, cpu.rep_execution.resume);
            assert_eq!(
                clone.canonical_execution_capture().err(),
                Some(CpuCanonicalCaptureError::ActiveRepContinuation)
            );
            let last = cpu.run_budgeted(&mut bus, 0).unwrap();
            let cloned_last = clone.run_budgeted(&mut clone_bus, 0).unwrap();
            assert_eq!(last, cloned_last);
            assert_eq!(last.consumed_core_clocks, 0);
            assert_eq!(cpu.elapsed_clocks, first.consumed_core_clocks);
            assert_eq!(cpu.registers.ecx(), 0);
            assert_eq!(cpu.perf.instructions, 1);
            assert_eq!(cpu.registers, clone.registers);
            assert_eq!(bus.memory, clone_bus.memory);
            assert_eq!(cpu.timing_rem, clone.timing_rem);
        }
    }
}

#[test]
fn rep_legacy_cached_and_profiled_entries_keep_the_invoice() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            for prefix in [0xf2, 0xf3] {
                for profiling in [false, true] {
                    let (mut cpu, mut bus) = rep_legacy_fixture(mode, epoch, prefix, 2);
                    cpu.registers.eip = 0;
                    cpu.cycle(&mut bus).unwrap();
                    cpu.cycle(&mut bus).unwrap();
                    cpu.registers.eip = 0;
                    cpu.registers.set_ecx(2);
                    cpu.registers.set_esi(0x2000);
                    cpu.registers.set_edi(0x8000);
                    cpu.timing_rem = 0;
                    cpu.elapsed_clocks = 0;
                    cpu.reset_perf_counters();
                    cpu.profile.enabled = profiling;
                    #[cfg(feature = "timing-class-histogram")]
                    {
                        cpu.class_histogram = Box::default();
                    }
                    // The warmed ordinary prefix reaches the cached REP tail in this run.
                    let first = cpu.run_budgeted(&mut bus, 21).unwrap();
                    assert_eq!(cpu.registers.eip, 1);
                    assert!(
                        cpu.rep_resume_active,
                        "{mode:?} E{epoch} profile={profiling}"
                    );
                    assert_eq!(
                        cpu.rep_execution.resume.as_ref().unwrap().price_history,
                        Some(RepPriceHistory {
                            initial_count: 2,
                            startup_paid: true
                        })
                    );
                    let carry = cpu.timing_rem;
                    let before = cpu.elapsed_clocks;
                    let mut invocations = 1;
                    while cpu.rep_resume_active {
                        invocations += 1;
                        assert_eq!(
                            cpu.run_budgeted(&mut bus, 0).unwrap().consumed_core_clocks,
                            0
                        );
                    }
                    assert_eq!(cpu.elapsed_clocks, before);
                    assert_eq!(cpu.timing_rem, carry);
                    assert_eq!(cpu.elapsed_clocks, first.consumed_core_clocks);
                    assert_eq!(cpu.perf.instructions, 2);
                    #[cfg(feature = "timing-class-histogram")]
                    assert_eq!(
                        cpu.class_histogram
                            .rows()
                            .into_iter()
                            .find(|(name, _)| *name
                                == crate::timing_class::TimingClass::StringElem.name())
                            .unwrap()
                            .1,
                        invocations
                    );
                    assert!(invocations > 1);
                    assert_eq!(&bus.memory[0x8000..0x8002], &[0x6d; 2]);
                }
            }
        }
    }
}

#[test]
fn rep_legacy_interrupt_iret_refetch_earns_a_fresh_startup() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            let (mut cpu, mut bus) = rep_legacy_fixture(mode, epoch, 0xf3, 2);
            bus.memory[8 * 4..8 * 4 + 4].copy_from_slice(&[0x00, 0x03, 0, 0]);
            bus.memory[0x300] = 0xcf;
            cpu.run_budgeted(&mut bus, 0).unwrap();
            cpu.run_budgeted(&mut bus, 0).unwrap();
            assert_eq!(cpu.registers.ecx(), 1);
            cpu.set_flag(FLAG_IF, true);
            bus.pending_irq = Some(8);
            let before = cpu.elapsed_clocks;
            let interrupt = cpu.service_pending_interrupt(&mut bus).unwrap().unwrap();
            assert_eq!(cpu.elapsed_clocks - before, interrupt.core_clocks);
            assert_eq!(cpu.registers.eip, 0x300);
            assert!(cpu.rep_execution.resume.is_none());
            cpu.cycle(&mut bus).unwrap();
            assert_eq!(cpu.registers.eip, 1);
            let carry = cpu.timing_rem;
            let before = cpu.elapsed_clocks;
            let raw = if epoch == 1 { 4 } else { 48 };
            let restarted = cpu.run_budgeted(&mut bus, 0).unwrap();
            assert_eq!(restarted.consumed_core_clocks, (raw + carry) / 12);
            assert_eq!(cpu.timing_rem, (raw + carry) % 12);
            assert_eq!(cpu.elapsed_clocks - before, restarted.consumed_core_clocks);
            assert_eq!(
                cpu.rep_execution.resume.as_ref().unwrap().price_history,
                Some(RepPriceHistory {
                    initial_count: 1,
                    startup_paid: true
                })
            );
        }
    }
}

#[test]
fn rep_legacy_public_fatal_retains_only_this_entry_and_recovers_cleanly() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            for paused in [false, true] {
                for destination_fault in [false, true] {
                    let (mut cpu, mut bus) = rep_legacy_fixture(mode, epoch, 0xf3, 2);
                    bus = TestBus::with_memory(bus.memory[..0x1000].to_vec());
                    bus.direct_page_clocks = true;
                    bus.report_batch_clocks = true;
                    bus.memory[0x100] = 0x6d;
                    bus.memory[0xfff] = 0x6d;
                    cpu.registers
                        .set_esi(if destination_fault { 0x100 } else { 0xfff });
                    cpu.registers
                        .set_edi(if destination_fault { 0xfff } else { 0x200 });
                    cpu.timing_rem = 7;
                    if paused {
                        cpu.run_budgeted(&mut bus, 0).unwrap();
                        assert!(
                            cpu.rep_execution
                                .resume
                                .as_ref()
                                .unwrap()
                                .price_history
                                .unwrap()
                                .startup_paid
                        );
                    }
                    let before = cpu.elapsed_clocks;
                    let carry = cpu.timing_rem;
                    let error = cpu.run_budgeted(&mut bus, u64::MAX).unwrap_err();
                    assert!(matches!(
                        error.error,
                        CpuError::Bus(izarravm_bus::BusError::UnmappedMemory { address: 0x1000 })
                    ));
                    assert_eq!(error.consumed_core_clocks, 0);
                    assert_eq!(cpu.elapsed_clocks, before);
                    assert_eq!(cpu.timing_rem, carry);
                    assert_eq!(cpu.registers.ecx(), 1);
                    assert!(cpu.rep_execution.resume.is_none());
                    let written = if destination_fault { 0xfff } else { 0x200 };
                    assert_eq!(bus.memory[written], 0x6d);
                    cpu.registers.eip = 3;
                    let next = cpu.cycle(&mut bus).unwrap();
                    let raw = if epoch == 1 { 2 } else { 12 };
                    assert_eq!(next.core_clocks, (raw + carry) / 12);
                    assert_eq!(cpu.timing_rem, (raw + carry) % 12);
                    assert_eq!(cpu.registers.eax() & 0xff, 0x5a);
                }
            }
        }
    }
}

#[test]
fn rep_legacy_delivered_and_triple_faults_settle_after_paid_resumes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            for paused in [false, true] {
                for fatal in [false, true] {
                    let (mut cpu, mut bus) = rep_legacy_fixture(mode, epoch, 0xf3, 2);
                    bus.memory[13 * 4..13 * 4 + 4].copy_from_slice(&[0, 3, 0, 0]);
                    let mut es = cpu.registers.segment(SegmentIndex::Es);
                    es.limit = 0x8000;
                    cpu.registers.set_segment(SegmentIndex::Es, es);
                    cpu.timing_rem = 7;
                    if fatal {
                        cpu.control.cr0 |= CR0_PE;
                        cpu.idtr.limit = 0;
                    }
                    if paused {
                        cpu.run_budgeted(&mut bus, 0).unwrap();
                    }
                    let before = cpu.elapsed_clocks;
                    let carry = cpu.timing_rem;
                    let result = cpu.run_budgeted(&mut bus, u64::MAX);
                    assert_eq!(cpu.registers.ecx(), 1);
                    assert_eq!(&bus.memory[0x8000..0x8002], &[0x6d, 0]);
                    assert!(cpu.rep_execution.resume.is_none());
                    if fatal {
                        let error = result.unwrap_err();
                        assert!(matches!(error.error, CpuError::TripleFault { .. }));
                        assert_eq!(error.consumed_core_clocks, 0);
                        assert_eq!(cpu.elapsed_clocks, before);
                        assert_eq!(cpu.timing_rem, carry);
                    } else {
                        let raw = cpu
                            .class_table()
                            .raw(crate::timing_class::TimingClass::ExceptionDelivery)
                            as u64;
                        assert_eq!(result.unwrap().consumed_core_clocks, (raw + carry) / 12);
                        assert_eq!(cpu.elapsed_clocks - before, (raw + carry) / 12);
                        assert_eq!(cpu.timing_rem, (raw + carry) % 12);
                        assert_eq!(cpu.registers.eip, 0x300);
                        assert_eq!(
                            u16::from_le_bytes([bus.memory[0xffa], bus.memory[0xffb]]),
                            1
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn rep_legacy_invalidation_discards_only_paid_history() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for epoch in [1, 2] {
            for action in 0..4 {
                let (mut cpu, mut bus) = rep_legacy_fixture(mode, epoch, 0xf3, 2);
                let first = cpu.run_budgeted(&mut bus, 0).unwrap();
                assert!(
                    cpu.rep_execution
                        .resume
                        .as_ref()
                        .unwrap()
                        .price_history
                        .unwrap()
                        .startup_paid
                );
                let elapsed = cpu.elapsed_clocks;
                let carry = cpu.timing_rem;
                match action {
                    0 => cpu.set_eip(3),
                    1 => cpu.load_segment_real(SegmentIndex::Cs, 1),
                    2 => cpu.set_mode(if mode == GswMode::Gsw486 {
                        GswMode::Gsw586
                    } else {
                        GswMode::Gsw486
                    }),
                    _ => cpu.reset(),
                }
                assert!(cpu.rep_execution.resume.is_none());
                assert!(!cpu.rep_resume_active);
                assert_eq!(elapsed, first.consumed_core_clocks);
                if action < 2 {
                    assert_eq!(cpu.elapsed_clocks, elapsed);
                    assert_eq!(cpu.timing_rem, carry);
                }
            }
        }
    }
}

#[test]
fn rep_legacy_checked_invoice_bounds_reject_unrepresentable_work() {
    let mut invoice = RepInvocation {
        history: RepPriceHistory {
            initial_count: u32::MAX,
            startup_paid: false,
        },
        completed: 0,
        raw_due: u32::MAX as u64,
    };
    invoice.complete(u32::MAX);
    assert_eq!(invoice.completed, u32::MAX);
    assert_eq!(invoice.legacy_raw(), u32::MAX);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| invoice.complete(1))).is_err()
    );
    invoice.raw_due += 1;
    assert!(std::panic::catch_unwind(|| invoice.legacy_raw()).is_err());
}
