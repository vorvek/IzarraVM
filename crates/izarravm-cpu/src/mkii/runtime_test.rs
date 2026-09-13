// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[path = "session_test.rs"]
mod session;

#[cfg(not(any(feature = "int-trace", feature = "timing-class-histogram")))]
impl CpuGsw {
    pub(crate) fn check_mkii_adjacent_pending_for_test<B: CpuBus>(
        &mut self,
        bus: &mut B,
        adjacent: bool,
    ) {
        let mut operations = Vec::new();
        self.set_eip(0);
        while self.registers.eip < 8 {
            let eip = self.registers.eip;
            let insn = self.fetch_decoded(bus, eip).unwrap();
            operations.push(Operation::lower(eip, eip, insn).unwrap());
        }
        assert_eq!(operations.len(), 6);
        operations[0].region_len = 4;
        operations[0].span_len = 4;
        operations[0].region = Some(super::super::ops::Region::build(
            self.class_table(),
            &operations[..4],
        ));
        operations[0].region.as_mut().unwrap().segments = 1 << SegmentIndex::Ds.index();
        operations[4].region_len = 2;
        operations[4].region = Some(super::super::ops::Region::build(
            self.class_table(),
            &operations[4..],
        ));
        assert_eq!(operations[4].span_len, 1);
        assert_eq!(operations[5].span_len, 1);
        self.registers.segments[SegmentIndex::Ds.index()].access = 0x98;
        self.set_eip(if adjacent { 0 } else { 4 });
        self.timing_rem = 0;
        self.registers.set_eax(0xabcd_5678);
        self.jit_direct.mkii.code_dirty = false;
        self.jit_direct.mkii.mapping_dirty = false;
        let elapsed = self.elapsed_clocks;
        let instructions = self.perf.instructions;
        let mut frame = Frame::new(self, bus, 5);
        frame.session = Session::acquire(self, bus, std::ptr::from_ref(bus));
        assert_eq!(frame.session.enabled, 1);
        let (mapping, cost) = bus.owned_code_replay_epochs().unwrap();
        frame.source_present = 1;
        frame.source_mapping = mapping;
        frame.source_cost = cost;
        unsafe extern "C" fn stop(_: *mut CpuGsw, _: *mut (), _: *mut Frame) -> usize {
            0
        }
        let dispatcher = super::super::native::dispatcher().unwrap();
        frame.dispatch = dispatcher.body_ptr() as usize;
        frame.resolve = stop;
        let code = super::super::native::compile(
            &operations[if adjacent { 0 } else { 4 }..],
            self.persona(),
        )
        .unwrap();
        // SAFETY: operations, bus, frame and both generated allocations outlive this call.
        let invoke: unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame) =
            unsafe { std::mem::transmute(code.entry_ptr()) };
        unsafe { invoke(self, std::ptr::from_mut(bus).cast(), &mut frame) };
        assert_eq!(
            self.registers.eax(),
            if adjacent { 0xabcd_5678 } else { 0xabcd_1234 }
        );
        assert_eq!(self.registers.eip, if adjacent { 5 } else { 8 });
        let clocks = if adjacent { 5 } else { 2 };
        assert_eq!(frame.total, clocks);
        assert_eq!(self.elapsed_clocks - elapsed, clocks);
        assert_eq!(self.perf.instructions - instructions, clocks);
        assert_eq!(self.timing_rem, 0);
        assert_eq!(frame.stats.spans, u64::from(adjacent));
        assert_eq!(frame.stats.native_admissions, u64::from(!adjacent));
        assert_eq!(frame.stop, adjacent);
        assert!(frame.pending.is_none());
        assert!(frame.error.is_none());
        assert_eq!(frame.stats.cold, 0);
        assert!(!self.jit_direct.mkii.code_dirty);
        assert!(!self.jit_direct.mkii.mapping_dirty);
    }
}

#[test]
fn mkii_selection_clears_a_previous_source_certificate() {
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    let mut bus = crate::tests::TestBus::with_memory(vec![0x90; 65536]);
    let mut engine = Engine::default();
    let mut bodies = Vec::new();
    for eip in [0, 16] {
        cpu.set_eip(eip);
        cpu.fetch_decoded(&mut bus, eip).unwrap();
        let key = install(&mut engine, &mut cpu, eip, eip);
        let index = engine.lookup(key).unwrap();
        engine.arena[index].as_mut().unwrap().source_certificate = (eip == 0).then_some((55, 71));
        bodies.push(engine.arena[index].as_ref().unwrap().code.body_ptr() as usize);
    }
    let mut frame = Frame::new(&cpu, &mut bus, 100);
    cpu.set_eip(0);
    assert_eq!(
        select_next(&mut engine, &mut cpu, &mut bus, &mut frame),
        bodies[0]
    );
    assert_eq!(
        (
            frame.source_present,
            frame.source_mapping,
            frame.source_cost
        ),
        (1, 55, 71)
    );
    cpu.set_eip(16);
    assert_eq!(
        select_next(&mut engine, &mut cpu, &mut bus, &mut frame),
        bodies[1]
    );
    assert_eq!(
        (
            frame.source_present,
            frame.source_mapping,
            frame.source_cost
        ),
        (0, 0, 0)
    );
}

fn test_operation(eip: u32, physical: u32, len: u8) -> Operation {
    let insn = DecodedInsn {
        len,
        prefixes: Prefixes::default(),
        opcode: 0x90,
        operand_size: OperandSize::Word,
        address_size: AddressSize::Word,
        modrm: None,
        operand: None,
        imm: 0,
        imm2: 0,
        group: DecodeGroup::DataMove,
        continuable: true,
        disp_len: 0,
        imm_len: 0,
    };
    Operation::lower(eip, physical, insn).unwrap()
}

fn install_operations(
    engine: &mut Engine,
    cpu: &mut CpuGsw,
    eip: u32,
    ranges: &[(u32, u8)],
) -> Key {
    let operations = ranges
        .iter()
        .scan(eip, |operation_eip, &(physical, len)| {
            let operation = test_operation(*operation_eip, physical, len);
            *operation_eip = operation_eip.wrapping_add(u32::from(len));
            Some(operation)
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let code = super::super::native::compile(&operations, cpu.persona()).unwrap();
    for operation in &operations {
        cpu.mkii_watch_source(operation.physical, u32::from(operation.insn.len));
    }
    let index = engine.free.pop().unwrap_or_else(|| {
        engine.arena.push(None);
        engine.arena.len() - 1
    });
    let key = Key {
        table: cpu.class_table() as *const _ as usize,
        physical: ranges[0].0,
        eip,
        cs_base: 0,
        cs_limit: 0xffff,
        cs_selector: 0,
        cpl: 0,
    };
    engine.traces.insert(key, index);
    engine.arena[index] = Some(Trace {
        operations,
        code,
        open_tail: None,
        source_certificate: None,
    });
    key
}

fn install(engine: &mut Engine, cpu: &mut CpuGsw, eip: u32, physical: u32) -> Key {
    install_operations(engine, cpu, eip, &[(physical, 1)])
}

fn source_contains(state: &super::super::State, physical: u32) -> bool {
    state
        .sources
        .range(..=physical)
        .next_back()
        .is_some_and(|(_, &end)| end > u64::from(physical))
}

fn decoded_operation(code: &[u8]) -> Option<Operation> {
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.set_eip(0);
    let mut memory = vec![0; 65536];
    memory[..code.len()].copy_from_slice(code);
    let mut bus = crate::tests::TestBus::with_memory(memory);
    let insn = cpu.fetch_decoded(&mut bus, 0).ok()?;
    Operation::lower(0, 0, insn)
}

#[test]
fn mkii_trace_continuation_admits_only_owned_ff6_push() {
    for extension in 0..8 {
        for mode in [0x00, 0xc0] {
            let operation = decoded_operation(&[0xff, mode | (extension << 3)]);
            assert_eq!(
                operation.as_ref().is_some_and(is_ff6_fallthrough),
                extension == 6,
                "extension={extension} mode={mode:02x} operation={operation:?}"
            );
        }
    }
    let non_ff6 = decoded_operation(&[0xfe, 0xf0]).unwrap();
    assert!(!is_ff6_fallthrough(&non_ff6));
    assert!(trace_can_continue_after(&non_ff6));
    for code in [&[0xe8, 0, 0][..], &[0xeb, 0], &[0xc3]] {
        assert!(
            decoded_operation(code)
                .as_ref()
                .is_none_or(|operation| !trace_can_continue_after(operation)),
            "{code:02x?}"
        );
    }
    for code in [&[0xf0, 0xff, 0xf0][..], &[0xf3, 0xff, 0xf0]] {
        assert!(decoded_operation(code).is_none(), "{code:02x?}");
    }
}

#[test]
fn mkii_trace_owns_ff6_push_and_its_sequential_successor() {
    let code = [0xff, 0xf0, 0x43, 0xeb, 0xfe];
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.set_eip(0);
    let mut memory = vec![0; 65536];
    memory[..code.len()].copy_from_slice(&code);
    let mut bus = crate::tests::TestBus::with_memory(memory);
    while cpu.registers.eip < code.len() as u32 {
        let eip = cpu.registers.eip;
        cpu.fetch_decoded(&mut bus, eip).unwrap();
    }
    cpu.set_eip(0);
    let mut engine = Engine::default();
    let trace = engine.trace(&mut cpu, &mut bus).unwrap();
    assert_eq!(
        trace
            .operations
            .iter()
            .map(|operation| (operation.eip, operation.helper))
            .collect::<Vec<_>>(),
        [(0, 8), (2, 6), (3, 5)]
    );
    assert!(trace.operations[0].pure.is_none());
    assert_eq!(trace.operations[0].span_len, 1);
    assert_eq!(trace.operations[0].region_len, 0);
}

#[test]
fn mkii_ff6_source_or_backing_change_refuses_guarded_suffixes() {
    for source_dirty in [false, true] {
        for suffix in 0..3 {
            let code: &[u8] = match suffix {
                0 => &[0xff, 0xf0, 0x43],
                1 => &[0xff, 0xf0, 0x90, 0x90],
                2 => &[0xff, 0xf0, 0xb8, 1, 0, 0x03, 0x06, 0, 0x20],
                _ => unreachable!(),
            };
            let mut cpu = CpuGsw::default();
            cpu.set_mode(crate::GswMode::Gsw586);
            cpu.control.cr0 |= crate::CR0_PE;
            cpu.load_segment_real(SegmentIndex::Cs, 0);
            cpu.load_segment_real(SegmentIndex::Ds, 0);
            cpu.load_segment_real(SegmentIndex::Ss, 0);
            cpu.registers.set_esp(0x9000);
            cpu.registers.set_ebx(0xa5a5_0000);
            cpu.set_jit_auto_admit(true);
            cpu.set_dynarec_mkii_enabled(true);
            cpu.set_eip(0);
            let mut memory = vec![0; 65536];
            memory[..code.len()].copy_from_slice(code);
            let mut bus = crate::tests::TestBus::with_memory(memory);
            let mut operations = Vec::new();
            while cpu.registers.eip < code.len() as u32 {
                let eip = cpu.registers.eip;
                let insn = cpu.fetch_decoded(&mut bus, eip).unwrap();
                operations.push(Operation::lower(eip, eip, insn).unwrap());
            }
            if suffix == 1 {
                operations[1].span_len = 2;
            } else if suffix == 2 {
                operations[1].region_len = 2;
                operations[1].region = Some(super::super::ops::Region::build(
                    cpu.class_table(),
                    &operations[1..],
                ));
            }
            let _code = super::super::native::compile(&operations, cpu.persona()).unwrap();
            cpu.set_eip(0);
            cpu.jit_direct.mkii.code_dirty = false;
            cpu.jit_direct.mkii.mapping_dirty = false;
            let mut frame = Frame::new(&cpu, &mut bus, 1000);
            let first = unsafe {
                step::<crate::tests::TestBus, 8>(
                    &mut cpu,
                    std::ptr::from_mut(&mut bus).cast(),
                    &mut frame,
                    &operations[0],
                )
            };
            assert_eq!(first, 1);
            assert_eq!(cpu.registers.eip, 2);
            assert_eq!(cpu.perf.instructions, 1);
            assert!(frame.pending.is_none());
            let before = (
                cpu.perf.instructions,
                cpu.elapsed_clocks,
                cpu.core_clocks_so_far,
                bus.bus_cycles_for_test(),
            );
            if source_dirty {
                cpu.jit_direct.mkii.code_dirty = true;
            } else {
                cpu.note_direct_map_changed();
                assert!(cpu.jit_direct.mkii.mapping_dirty);
            }
            let result = match suffix {
                0 => unsafe {
                    step::<crate::tests::TestBus, 6>(
                        &mut cpu,
                        std::ptr::from_mut(&mut bus).cast(),
                        &mut frame,
                        &operations[1],
                    )
                },
                1 => unsafe {
                    prepare_span::<crate::tests::TestBus>(
                        &mut cpu,
                        std::ptr::from_mut(&mut bus).cast(),
                        &mut frame,
                        &operations[1],
                    )
                },
                2 => unsafe {
                    super::region::prepare::<crate::tests::TestBus>(
                        &mut cpu,
                        std::ptr::from_mut(&mut bus).cast(),
                        &mut frame,
                        &operations[1],
                    )
                },
                _ => unreachable!(),
            };
            assert_eq!(result, if suffix == 0 { 0 } else { 2 });
            assert_eq!(cpu.registers.ebx(), 0xa5a5_0000);
            assert_eq!(cpu.registers.eax(), 0);
            assert_eq!(
                (
                    cpu.perf.instructions,
                    cpu.elapsed_clocks,
                    cpu.core_clocks_so_far,
                    bus.bus_cycles_for_test(),
                ),
                before
            );
            assert!(frame.pending.is_none());
            assert_eq!(frame.stats.helpers, 1);
        }
    }
}

#[test]
fn mkii_ff6_fallthrough_uses_the_existing_64_operation_trace_cap() {
    let mut code = vec![0xff, 0xf0].repeat(65);
    code.extend_from_slice(&[0xeb, 0xfe]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.set_eip(0);
    let mut memory = vec![0; 65536];
    memory[..code.len()].copy_from_slice(&code);
    let mut bus = crate::tests::TestBus::with_memory(memory);
    while cpu.registers.eip < code.len() as u32 {
        let eip = cpu.registers.eip;
        cpu.fetch_decoded(&mut bus, eip).unwrap();
    }
    cpu.set_eip(0);
    let mut engine = Engine::default();
    let first = engine.trace(&mut cpu, &mut bus).unwrap();
    assert_eq!(first.operations.len(), 64);
    assert!(first.operations.iter().all(is_ff6_fallthrough));
    assert_eq!(first.operations.last().unwrap().eip, 126);
    cpu.set_eip(128);
    let second = engine.trace(&mut cpu, &mut bus).unwrap();
    assert_eq!(second.operations.len(), 2);
    assert!(is_ff6_fallthrough(&second.operations[0]));
    assert_eq!(second.operations[1].insn.opcode, 0xeb);
}

#[test]
fn mkii_ff6_cold_tail_growth_retries_and_preserves_source_ownership() {
    for invalidated in [1, 3] {
        let code = [0x90, 0xff, 0xf0, 0x43, 0xeb, 0xfe];
        let mut cpu = CpuGsw::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.set_eip(0);
        let mut memory = vec![0; 65536];
        memory[..code.len()].copy_from_slice(&code);
        let mut bus = crate::tests::TestBus::with_memory(memory);
        for eip in [0, 1] {
            cpu.set_eip(eip);
            cpu.fetch_decoded(&mut bus, eip).unwrap();
        }
        cpu.set_eip(0);
        let mut engine = Engine::default();
        let before = (
            cpu.registers.eip,
            cpu.elapsed_clocks,
            cpu.core_clocks_so_far,
            cpu.perf.instructions,
            cpu.perf.decode_misses,
            bus.bus_cycles_for_test(),
        );
        {
            let trace = engine.trace(&mut cpu, &mut bus).unwrap();
            assert_eq!(trace.operations.len(), 2);
            assert_eq!(trace.open_tail, Some(3));
        }
        assert_eq!(
            (
                cpu.registers.eip,
                cpu.elapsed_clocks,
                cpu.core_clocks_so_far,
                cpu.perf.instructions,
                cpu.perf.decode_misses,
                bus.bus_cycles_for_test(),
            ),
            before
        );
        assert_eq!(cpu.jit_direct.code_watch.refcount(0), 1);
        assert_eq!(cpu.jit_direct.code_watch.refcount(3), 0);
        cpu.decode_cache.kill_line_at(1);
        cpu.set_eip(3);
        cpu.fetch_decoded(&mut bus, 3).unwrap();
        cpu.set_eip(0);
        engine.fail_next_compile = true;
        let before = (
            cpu.registers.eip,
            cpu.elapsed_clocks,
            cpu.core_clocks_so_far,
            cpu.perf.instructions,
            cpu.perf.decode_misses,
            bus.bus_cycles_for_test(),
        );
        {
            let trace = engine.trace(&mut cpu, &mut bus).unwrap();
            assert_eq!(trace.operations.len(), 2);
            assert_eq!(trace.open_tail, Some(3));
        }
        assert_eq!(
            (
                cpu.registers.eip,
                cpu.elapsed_clocks,
                cpu.core_clocks_so_far,
                cpu.perf.instructions,
                cpu.perf.decode_misses,
                bus.bus_cycles_for_test(),
            ),
            before
        );
        assert_eq!(cpu.jit_direct.code_watch.refcount(0), 1);
        assert_eq!(cpu.jit_direct.code_watch.refcount(3), 0);
        let before = (
            cpu.registers.eip,
            cpu.elapsed_clocks,
            cpu.core_clocks_so_far,
            cpu.perf.instructions,
            cpu.perf.decode_misses,
            bus.bus_cycles_for_test(),
        );
        {
            let trace = engine.trace(&mut cpu, &mut bus).unwrap();
            assert_eq!(trace.operations.len(), 3);
            assert_eq!(trace.open_tail, Some(4));
            assert!(is_ff6_fallthrough(&trace.operations[1]));
        }
        assert_eq!(
            (
                cpu.registers.eip,
                cpu.elapsed_clocks,
                cpu.core_clocks_so_far,
                cpu.perf.instructions,
                cpu.perf.decode_misses,
                bus.bus_cycles_for_test(),
            ),
            before
        );
        assert_eq!(engine.stats.expansions, 1);
        assert_eq!(cpu.jit_direct.code_watch.refcount(0), 1);
        assert_eq!(cpu.jit_direct.code_watch.refcount(3), 1);
        assert!(cpu.jit_direct.mkii.note_write(invalidated, 1));
        engine.drain_writes(&mut cpu);
        assert!(engine.traces.is_empty());
        assert_eq!(cpu.jit_direct.code_watch.refcount(0), 0);
        assert_eq!(cpu.jit_direct.code_watch.refcount(3), 0);
    }
}

#[test]
fn mkii_ff6_fallthrough_keeps_cs_page_straddle_and_runoff_bounds() {
    for (start, limit, warm_successor, expected) in [
        (0x0800, 0xffff, true, Some(3)),
        (0x0ffe, 0xffff, true, Some(1)),
        (0xfffc, 0xfffd, false, Some(1)),
        (0xfffe, 0x1ffff, false, Some(1)),
        (0x0fff, 0xffff, false, None),
    ] {
        let code = [0xff, 0xf0, 0x43, 0xeb, 0xfe];
        let mut cpu = CpuGsw::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.segments[SegmentIndex::Cs.index()].limit = limit;
        cpu.set_eip(start);
        let mut memory = vec![0; 0x11000];
        memory[start as usize..start as usize + code.len()].copy_from_slice(&code);
        let mut bus = crate::tests::TestBus::with_memory(memory);
        cpu.fetch_decoded(&mut bus, start).unwrap();
        if warm_successor {
            cpu.set_eip(start + 2);
            cpu.fetch_decoded(&mut bus, start + 2).unwrap();
            cpu.set_eip(start + 3);
            cpu.fetch_decoded(&mut bus, start + 3).unwrap();
        }
        cpu.set_eip(start);
        let mut engine = Engine::default();
        let trace = engine.trace(&mut cpu, &mut bus);
        assert_eq!(
            trace.map(|trace| trace.operations.len()),
            expected,
            "start={start:#x} limit={limit:#x}"
        );
    }
}

#[test]
fn mkii_source_rebuild_preserves_exact_union_for_adjacency_gaps_aliases_and_wrap() {
    let layouts: &[&[(u32, u8)]] = &[
        &[(0x100, 1), (0x101, 2), (0x103, 1)],
        &[(0x200, 1), (0x202, 1)],
        &[(0x300, 2), (0x301, 2), (0x300, 1), (0x2ff, 1)],
        &[(0xffe, 2), (0x1000, 2)],
        &[(u32::MAX - 1, 3)],
    ];
    let mut expected = super::super::State::default();
    let mut actual = super::super::State::default();
    for (index, layout) in layouts.iter().enumerate() {
        let operations = layout
            .iter()
            .scan(index as u32 * 0x100, |eip, &(physical, len)| {
                let operation = test_operation(*eip, physical, len);
                *eip += u32::from(len);
                Some(operation)
            })
            .collect::<Vec<_>>();
        Engine::rebuild_trace_sources(&mut actual, &operations);
        for &(physical, len) in *layout {
            expected.add_source(physical, u32::from(len));
        }
    }
    Engine::rebuild_trace_sources(&mut actual, &[]);
    assert_eq!(actual.sources, expected.sources);
    for (physical, present) in [
        (0, true),
        (1, false),
        (0xff, false),
        (0x100, true),
        (0x103, true),
        (0x104, false),
        (0x200, true),
        (0x201, false),
        (0x202, true),
        (0x2fe, false),
        (0x2ff, true),
        (0x303, false),
        (0xffd, false),
        (0xffe, true),
        (0x1001, true),
        (0x1002, false),
        (u32::MAX - 2, false),
        (u32::MAX - 1, true),
        (u32::MAX, true),
    ] {
        assert_eq!(source_contains(&actual, physical), present, "{physical:#x}");
    }
}

#[test]
fn mkii_source_rebuild_after_retirement_keeps_union_and_watch_references_exact() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    let survivors: &[&[(u32, u8)]] = &[
        &[(0x100, 2), (0x102, 2)],
        &[(0x101, 2), (0x105, 1)],
        &[(0x200, 1), (0x202, 1)],
        &[(u32::MAX, 2)],
    ];
    for (index, layout) in survivors.iter().enumerate() {
        install_operations(&mut engine, &mut cpu, 0x1000 + index as u32 * 0x10, layout);
    }
    install(&mut engine, &mut cpu, 0x2000, 0x500);

    let mut expected = super::super::State::default();
    let mut expected_refs = std::collections::BTreeMap::<u32, u32>::new();
    for layout in survivors {
        for &(physical, len) in *layout {
            expected.add_source(physical, u32::from(len));
            for (start, end) in super::super::physical_ranges(physical, u32::from(len))
                .into_iter()
                .flatten()
            {
                for byte in u64::from(start)..end {
                    *expected_refs.entry(byte as u32).or_default() += 1;
                }
            }
        }
    }
    assert!(cpu.jit_direct.mkii.note_write(0x500, 1));
    engine.drain_writes(&mut cpu);

    assert_eq!(cpu.jit_direct.mkii.sources, expected.sources);
    assert_eq!(engine.traces.len(), survivors.len());
    assert_eq!(engine.stats.invalidations, 1);
    assert_eq!(engine.stats.retired_artifacts, 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x500), 0);
    for (&physical, &refs) in &expected_refs {
        assert_eq!(cpu.jit_direct.code_watch.refcount(physical), refs);
    }
    for physical in [0x104, 0x106, 0x1ff, 0x201, 0x203] {
        assert!(!source_contains(&cpu.jit_direct.mkii, physical));
        assert_eq!(cpu.jit_direct.code_watch.refcount(physical), 0);
    }
}

#[test]
fn mkii_dispatch_cache_checks_full_keys_and_forgets_retired_arena_slots() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    assert_eq!(engine.dispatch.capacity(), 0);
    let first = install(&mut engine, &mut cpu, 0x100, 0x100);
    let same_slot = (0x101u32..0x10000)
        .find(|physical| {
            physical.wrapping_mul(0x9e37_79b9) >> 20
                == first.physical.wrapping_mul(0x9e37_79b9) >> 20
        })
        .unwrap();
    let second = install(&mut engine, &mut cpu, same_slot, same_slot);
    let first_index = engine.lookup(first).unwrap();
    assert_eq!(engine.lookup(first), Some(first_index));
    let hits = engine.stats.dispatch_hits;
    let second_index = engine.lookup(second).unwrap();
    assert_ne!(first_index, second_index);
    assert_eq!(engine.stats.dispatch_hits, hits);
    assert_eq!(engine.lookup(first), Some(first_index));
    for alias in [
        Key { cpl: 3, ..first },
        Key {
            cs_selector: 8,
            ..first
        },
        Key {
            cs_base: 16,
            ..first
        },
        Key {
            cs_limit: 256,
            ..first
        },
        Key { table: 1, ..first },
    ] {
        assert_eq!(engine.lookup(alias), None);
        assert_eq!(engine.lookup(first), Some(first_index));
    }
    assert!(cpu.jit_direct.mkii.note_write(first.physical, 1));
    engine.drain_writes(&mut cpu);
    assert_eq!(engine.lookup(first), None);
    let replacement = install(&mut engine, &mut cpu, 0x300, 0x300);
    assert_eq!(engine.lookup(replacement), Some(first_index));
    assert_eq!(engine.lookup(first), None);
    assert_eq!(engine.lookup(second), Some(second_index));
    engine.clear(&mut cpu);
    assert!(engine.free.is_empty());
    assert!(engine.arena.is_empty());
    assert_eq!(engine.lookup(replacement), None);
    let _leased = std::mem::take(&mut engine);
    assert_eq!(engine.dispatch.capacity(), 0);
}

#[test]
fn mkii_narrow_retirement_preserves_shared_pages_and_removes_aliases() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    install(&mut engine, &mut cpu, 0x100, 0x100);
    install(&mut engine, &mut cpu, 0x200, 0x100);
    install(&mut engine, &mut cpu, 0x101, 0x101);
    install(&mut engine, &mut cpu, 0x300, 0x300);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x100), 2);
    assert!(cpu.jit_direct.mkii.note_write(0x100, 1));
    assert!(cpu.jit_direct.mkii.note_write(0x300, 1));
    engine.drain_writes(&mut cpu);
    assert_eq!(engine.traces.len(), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x101), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x300), 0);
    assert!(!cpu.jit_direct.mkii.note_write(0x100, 1));
    assert!(cpu.jit_direct.mkii.note_write(0x101, 1));
    assert_eq!(engine.stats.retired_artifacts, 3);
    cpu.jit_direct.mkii.invalidate_code();
    engine.drain_writes(&mut cpu);
    engine.clear(&mut cpu);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x101), 0);
    assert!(cpu.jit_direct.mkii.sources.is_empty());
    assert!(cpu.jit_direct.mkii.dirty_writes.is_empty());
    assert!(!cpu.jit_direct.mkii.full_flush);
}

#[test]
fn mkii_dirty_queue_overflow_promotes_to_a_bounded_full_flush() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    install(&mut engine, &mut cpu, 0, 0x100);
    for _ in 0..1000 {
        assert!(cpu.jit_direct.mkii.note_write(0x100, 1));
    }
    assert!(cpu.jit_direct.mkii.full_flush);
    assert!(cpu.jit_direct.mkii.dirty_writes.is_empty());
    engine.drain_writes(&mut cpu);
    assert!(engine.traces.is_empty());
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x100), 0);
    assert!(!cpu.jit_direct.mkii.code_dirty);
}
