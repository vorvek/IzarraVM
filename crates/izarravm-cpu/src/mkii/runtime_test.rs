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
