// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::tests::TestBus;
use std::cell::Cell;
use std::time::{Duration, Instant};

thread_local! {
    static CALLS: Cell<[u64; 4]> = const { Cell::new([0; 4]) };
    static NOOP_FINISH: Cell<u64> = const { Cell::new(0) };
}

unsafe extern "C" fn counted<const SLOT: usize>(
    cpu: *mut CpuGsw,
    bus: *mut (),
    frame: *mut Frame,
    operation: *const Operation,
) -> u32 {
    let index = match SLOT {
        0 => 0,
        11 => 1,
        13 => 2,
        14 => 3,
        _ => unreachable!(),
    };
    CALLS.with(|calls| {
        let mut value = calls.get();
        value[index] += 1;
        calls.set(value);
    });
    // SAFETY: the wrappers preserve the real callback's arguments and bus owner.
    unsafe {
        let state = |cpu: *mut CpuGsw, frame: *mut Frame| {
            (
                (*cpu).elapsed_clocks,
                (*cpu).timing_rem,
                (*cpu).perf.instructions,
                (*cpu).perf.monitor_resident_core_clocks,
                (*frame).total,
            )
        };
        let empty_finish = SLOT == 11 && (*frame).pending.is_none();
        let before = state(cpu, frame);
        let result = match SLOT {
            0 => step::<TestBus, 0>(cpu, bus, frame, operation),
            11 => finish::<TestBus>(cpu, bus, frame, operation),
            13 => region::prepare::<TestBus>(cpu, bus, frame, operation),
            14 => region::finish::<TestBus>(cpu, bus, frame, operation),
            _ => unreachable!(),
        };
        if empty_finish && state(cpu, frame) == before && (*frame).pending.is_none() {
            NOOP_FINISH.with(|count| count.set(count.get() + 1));
        }
        result
    }
}

#[cfg(not(any(feature = "int-trace", feature = "timing-class-histogram")))]
impl CpuGsw {
    pub(crate) fn prepare_mkii_singleton_loop_for_test(
        &mut self,
        bus: &mut TestBus,
        legacy: bool,
    ) -> SingletonLoopBench {
        let mut engine = Engine::default();
        engine.trace(self, bus).unwrap();
        let trace = engine.arena.iter_mut().flatten().next().unwrap();
        assert_eq!(trace.operations.len(), 2);
        assert_eq!(trace.operations[0].insn.opcode, 0xb8);
        assert_eq!(trace.operations[0].region_len, 1);
        assert_eq!(trace.operations[1].insn.opcode, 0xeb);
        assert_eq!(trace.operations[1].helper, 5);
        assert!(trace.open_tail.is_none());
        assert!(trace.source_certificate.is_some());
        if legacy {
            trace.operations[0].region_len = 0;
            trace.operations[0].region = None;
            trace.code = super::super::super::native::compile(
                &trace.operations,
                self.persona(),
                self.registers.cs().default_size_32,
            )
            .unwrap();
        }
        let bytes = trace.code.emitted_len;
        let body = trace.code.body_ptr() as usize;
        self.jit_direct.mkii.code_dirty = false;
        self.jit_direct.mkii.mapping_dirty = false;
        SingletonLoopBench {
            engine,
            dispatcher: super::super::super::native::dispatcher().unwrap(),
            initial: self.registers.clone(),
            cpu_owner: std::ptr::from_ref(self) as usize,
            bus_owner: std::ptr::from_ref(bus) as usize,
            bytes,
            body,
        }
    }

    pub(crate) fn check_mkii_singleton_callbacks_for_test(
        &mut self,
        bus: &mut TestBus,
        cap: u64,
        legacy: bool,
    ) -> ([u64; 4], Stats, u64, usize) {
        invoke(self, bus, cap, legacy, false, 0)
    }

    pub(crate) fn check_mkii_singleton_prefix_for_test(
        &mut self,
        bus: &mut TestBus,
        legacy: bool,
    ) -> ([u64; 4], Stats, u64, usize) {
        invoke(self, bus, 100, legacy, true, 0)
    }

    pub(crate) fn check_mkii_singleton_guard_for_test(
        &mut self,
        bus: &mut TestBus,
        guard: u8,
    ) -> ([u64; 4], Stats, u64, usize) {
        invoke(self, bus, 100, false, false, guard)
    }
}

pub(crate) struct SingletonLoopBench {
    engine: Engine,
    dispatcher: super::super::super::native::Code,
    initial: Registers,
    cpu_owner: usize,
    bus_owner: usize,
    pub(crate) bytes: usize,
    body: usize,
}

impl SingletonLoopBench {
    pub(crate) fn run(
        &mut self,
        cpu: &mut CpuGsw,
        bus: &mut TestBus,
        cap: u64,
    ) -> (Duration, Stats) {
        assert_eq!(std::ptr::from_ref(cpu) as usize, self.cpu_owner);
        assert_eq!(std::ptr::from_ref(bus) as usize, self.bus_owner);
        assert!(!cpu.rep_resume_active);
        assert!(!cpu.rep_execution.yielded);
        assert!(cpu.rep_execution.resume.is_none());
        assert!(cpu.rep_execution.budget.is_none());
        cpu.registers = self.initial.clone();
        cpu.set_eip(0);
        cpu.pending_flags = PendingFlags::default();
        cpu.timing_rem = 0;
        cpu.single_step_armed = false;
        cpu.single_step_repeat = false;
        cpu.interrupt_shadow = false;
        cpu.core_clocks_so_far = 0;
        let compiled = self.engine.stats.compiled;
        let invalidations = self.engine.stats.invalidations;
        let mut frame = Frame::new(cpu, bus, cap);
        frame.session = Session::acquire(cpu, bus, std::ptr::from_ref(bus));
        assert_eq!(frame.session.enabled, 1);
        let first = select_next(&mut self.engine, cpu, bus, &mut frame);
        assert_eq!(first, self.body);
        assert_eq!(
            frame.helpers[0] as usize,
            step::<TestBus, 0> as *const () as usize
        );
        assert_eq!(
            frame.helpers[5] as usize,
            step::<TestBus, 5> as *const () as usize
        );
        assert_eq!(
            frame.helpers[11] as usize,
            finish::<TestBus> as *const () as usize
        );
        assert_eq!(
            frame.helpers[13] as usize,
            region::prepare::<TestBus> as *const () as usize
        );
        assert_eq!(
            frame.helpers[14] as usize,
            region::finish::<TestBus> as *const () as usize
        );
        frame.engine = std::ptr::from_mut(&mut self.engine);
        frame.dispatch = self.dispatcher.body_ptr() as usize;
        // SAFETY: the engine owns the operation storage and executable code for the full call.
        let entry: unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame, usize) =
            unsafe { std::mem::transmute(self.dispatcher.entry_ptr()) };
        let start = Instant::now();
        unsafe { entry(cpu, std::ptr::from_mut(bus).cast(), &mut frame, first) };
        let elapsed = start.elapsed();
        assert!(frame.stop);
        assert!(frame.error.is_none());
        assert!(frame.pending.is_none());
        assert!(frame.region_running.is_none());
        assert_eq!(frame.region_inert, 0);
        assert_eq!(frame.stats.cold, 0);
        assert_eq!(frame.stats.mismatches, 0);
        assert_eq!(frame.stats.probe_misses, 1);
        assert!(frame.stats.probe_hits > 0);
        assert_eq!(self.engine.stats.compiled, compiled);
        assert_eq!(self.engine.stats.invalidations, invalidations);
        assert!(!cpu.jit_direct.mkii.code_dirty);
        assert!(!cpu.jit_direct.mkii.mapping_dirty);
        assert!(!cpu.rep_resume_active);
        assert!(!cpu.rep_execution.yielded);
        assert!(cpu.rep_execution.resume.is_none());
        assert!(cpu.rep_execution.budget.is_none());
        (elapsed, frame.stats)
    }
}

fn invoke(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    cap: u64,
    legacy: bool,
    prefix: bool,
    guard: u8,
) -> ([u64; 4], Stats, u64, usize) {
    let mut engine = Engine::default();
    let trace = engine.trace(cpu, bus).unwrap();
    assert_eq!(trace.operations.len(), if prefix { 2 } else { 1 });
    let legacy_code = (legacy || prefix).then(|| {
        let mut operations: Vec<_> = trace
            .operations
            .iter()
            .map(|operation| {
                Operation::lower(operation.eip, operation.physical, operation.insn).unwrap()
            })
            .collect();
        if prefix && !legacy {
            operations[1].region_len = 1;
            operations[1].region = Some(super::super::super::ops::Region::build(
                cpu.class_table(),
                &operations[1..],
            ));
        }
        let code = super::super::super::native::compile(&operations, cpu.persona(), false).unwrap();
        (operations, code)
    });
    let body = legacy_code
        .as_ref()
        .map_or_else(|| trace.code.body_ptr(), |(_, code)| code.body_ptr());
    let emitted_len = legacy_code
        .as_ref()
        .map_or(trace.code.emitted_len, |(_, code)| code.emitted_len);
    let certificate = trace.source_certificate.unwrap();
    cpu.jit_direct.mkii.code_dirty = false;
    cpu.jit_direct.mkii.mapping_dirty = false;
    if guard == 8 {
        cpu.profile.enabled = true;
    }
    let mut frame = Frame::new(cpu, bus, cap);
    frame.session = Session::acquire(cpu, bus, std::ptr::from_ref(bus));
    frame.source_present = 1;
    frame.source_mapping = certificate.0;
    frame.source_cost = certificate.1;
    match guard {
        0 => assert_eq!(frame.session.enabled, 1),
        1 => frame.session.enabled = 0,
        2 => frame.source_present = 0,
        3 => frame.source_mapping ^= 1,
        4 => frame.source_cost ^= 1,
        5 => cpu.jit_direct.mkii.code_dirty = true,
        6 => cpu.jit_direct.mkii.mapping_dirty = true,
        7 => cpu.interrupt_shadow = true,
        8 => {}
        _ => unreachable!(),
    }
    frame.helpers[0] = counted::<0>;
    frame.helpers[11] = counted::<11>;
    frame.helpers[13] = counted::<13>;
    frame.helpers[14] = counted::<14>;
    unsafe extern "C" fn stop(_: *mut CpuGsw, _: *mut (), _: *mut Frame) -> usize {
        0
    }
    let dispatcher = super::super::super::native::dispatcher().unwrap();
    frame.dispatch = dispatcher.body_ptr() as usize;
    frame.resolve = stop;
    CALLS.with(|calls| calls.set([0; 4]));
    NOOP_FINISH.with(|count| count.set(0));
    // SAFETY: all code, operation storage, frame and the unchanged bus owner remain live.
    let entry: unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame, usize) =
        unsafe { std::mem::transmute(dispatcher.entry_ptr()) };
    unsafe {
        entry(
            cpu,
            std::ptr::from_mut(bus).cast(),
            &mut frame,
            body as usize,
        )
    };
    assert!(frame.pending.is_none());
    assert!(frame.error.is_none());
    (
        CALLS.with(Cell::get),
        frame.stats,
        NOOP_FINISH.with(Cell::get),
        emitted_len,
    )
}
