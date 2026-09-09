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

fn install(engine: &mut Engine, cpu: &mut CpuGsw, eip: u32, physical: u32) -> Key {
    let insn = DecodedInsn {
        len: 1,
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
    let operations = vec![Operation::lower(eip, physical, insn).unwrap()].into_boxed_slice();
    let code = super::super::native::compile(&operations, cpu.persona()).unwrap();
    cpu.mkii_watch_source(physical, 1);
    let index = engine.free.pop().unwrap_or_else(|| {
        engine.arena.push(None);
        engine.arena.len() - 1
    });
    let key = Key {
        table: cpu.class_table() as *const _ as usize,
        physical,
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
