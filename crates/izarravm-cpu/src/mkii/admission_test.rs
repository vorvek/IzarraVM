// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::mkii::runtime::Session;
use izarravm_bus::{MkiiBusSessionParts, MkiiCounterPath};
use std::mem::offset_of;

struct Probe {
    trace: u64,
    isa: u64,
    mapping: u64,
}

fn probe_code(op: &Operation, persona: crate::CpuPersona) -> ExecutableBuffer {
    let mut e = Encoder::new();
    let unwind = entry(&mut e);
    for (index, register) in SAVED.into_iter().enumerate() {
        e.store_r64_disp32(Reg::RSP, index as i32 * 8, register);
    }
    let prepared = e.label();
    let check = e.label();
    let corrupt = e.label();
    admission::emit(&mut e, op, persona, prepared);
    e.mov_r32_imm32(Reg::R9, 0);
    e.jmp(check);
    e.place(prepared);
    e.mov_r32_imm32(Reg::R9, 1);
    e.place(check);
    for (index, register) in SAVED.into_iter().enumerate() {
        e.load_r64_disp32(Reg::RAX, Reg::RSP, index as i32 * 8);
        e.cmp_r64_r64(Reg::RAX, register);
        e.jcc(5, corrupt);
    }
    e.mov_r64_r64(Reg::RAX, Reg::R9);
    epilogue(&mut e);
    e.place(corrupt);
    e.mov_r32_imm32(Reg::RAX, 2);
    epilogue(&mut e);
    ExecutableBuffer::new_with_unwind(&e.finish(), &unwind).unwrap()
}

#[test]
fn mkii_shared_admission_uses_each_calls_address_and_cost() {
    use crate::GswMode::{Gsw486, Gsw586};
    for (mode, raw, eip, remainder, core, next_remainder) in [
        (Gsw586, 24, 0, 3, 2, 3),
        (Gsw586, 25, 16, 11, 3, 0),
        (Gsw586, 95, 32, 10, 8, 9),
        (Gsw486, 25, 48, 11, 3, 0),
        (Gsw586, (1u64 << 40) * 12 + 5, 112, 11, (1u64 << 40) + 1, 4),
    ] {
        let (mut cpu, mut frame, mut probe) = fixture(mode);
        cpu.registers.eip = eip;
        cpu.timing_rem = remainder;
        frame.cap = core + 100;
        let mut ops = operations(&[0x90, 0x90]);
        ops[0].eip = eip;
        ops[0].region_len = 2;
        ops[0].region = Some(crate::mkii::ops::Region::build(cpu.class_table(), &ops));
        ops[0].region.as_mut().unwrap().prefixes[2].raw_core = raw;
        let shared = admission::code_for(cpu.persona(), 0).unwrap().entry_ptr();
        let code = probe_code(&ops[0], cpu.persona());
        assert_eq!(
            admission::code_for(cpu.persona(), 0).unwrap().entry_ptr(),
            shared
        );
        let invoke: unsafe extern "C" fn(*mut CpuGsw, *mut Probe, *mut Frame) -> u32 =
            unsafe { std::mem::transmute(code.entry_ptr()) };
        assert_eq!(
            unsafe { invoke(&mut cpu, &mut probe, &mut frame) },
            1,
            "{mode:?} eip={eip}"
        );
        assert_eq!(
            (frame.region_full_core, frame.region_full_rem),
            (core, next_remainder)
        );
        assert_eq!(cpu.registers.eip, eip);
        assert_eq!(cpu.timing_rem, remainder);
        assert_eq!(cpu.elapsed_clocks, 71);
    }
}

#[test]
fn mkii_shared_admission_refuses_386_without_publication() {
    let (mut cpu, mut frame, mut probe) = fixture(crate::GswMode::Gsw386);
    assert!(!cpu.fast_map_serve_enabled.enabled);
    let mut ops = operations(&[0x90, 0x90]);
    ops[0].region_len = 2;
    ops[0].region = Some(crate::mkii::ops::Region::build(cpu.class_table(), &ops));
    let code = probe_code(&ops[0], cpu.persona());
    let before = published(&cpu, &frame);
    let invoke: unsafe extern "C" fn(*mut CpuGsw, *mut Probe, *mut Frame) -> u32 =
        unsafe { std::mem::transmute(code.entry_ptr()) };
    assert_eq!(unsafe { invoke(&mut cpu, &mut probe, &mut frame) }, 0);
    assert_eq!(published(&cpu, &frame), before);
}

#[test]
fn mkii_shared_admission_separates_segment_masks() {
    for segment in [crate::SegmentIndex::Es, crate::SegmentIndex::Ds] {
        let (mut cpu, mut frame, mut probe) = fixture(crate::GswMode::Gsw586);
        cpu.registers.segments[crate::SegmentIndex::Ds.index()].access = 0x98;
        let biases = [0usize];
        let epochs = [55u64];
        frame.session.load_biases = biases.as_ptr() as usize;
        frame.session.mapping_epochs = epochs.as_ptr() as usize;
        let mut ops = operations(&[0x90, 0x90]);
        ops[0].region_len = 2;
        ops[0].region = Some(crate::mkii::ops::Region::build(cpu.class_table(), &ops));
        ops[0].region.as_mut().unwrap().segments = 1 << segment.index();
        let code = probe_code(&ops[0], cpu.persona());
        let before = published(&cpu, &frame);
        let invoke: unsafe extern "C" fn(*mut CpuGsw, *mut Probe, *mut Frame) -> u32 =
            unsafe { std::mem::transmute(code.entry_ptr()) };
        let accepted = unsafe { invoke(&mut cpu, &mut probe, &mut frame) };
        assert_eq!(accepted, u32::from(segment == crate::SegmentIndex::Es));
        if segment == crate::SegmentIndex::Ds {
            assert_eq!(published(&cpu, &frame), before);
        }
    }
}

#[test]
fn mkii_shared_admission_failed_lookup_emits_no_call() {
    let (cpu, _, _) = fixture(crate::GswMode::Gsw586);
    let mut ops = operations(&[0x90, 0x90]);
    ops[0].region_len = 2;
    ops[0].region = Some(crate::mkii::ops::Region::build(cpu.class_table(), &ops));
    let mut e = Encoder::new();
    let prepared = e.label();
    let mut lookups = 0;
    admission::emit_with(&mut e, &ops[0], cpu.persona(), prepared, |_, _| {
        lookups += 1;
        None
    });
    e.place(prepared);
    assert_eq!(lookups, 1);
    assert!(e.finish().is_empty());
}

fn published(cpu: &CpuGsw, frame: &Frame) -> [u64; 15] {
    [
        frame.region_epoch,
        frame.region_full_core,
        frame.region_full_rem,
        frame.region_load_biases as u64,
        frame.region_mapping_epochs as u64,
        u64::from(frame.region_completed),
        u64::from(frame.region_guard_miss),
        u64::from(frame.branch_taken),
        u64::from(frame.region_inert),
        u64::from(frame.region_user),
        u64::from(frame.region_monitor),
        u64::from(frame.region_can_take),
        cpu.core_clocks_so_far,
        frame.stats.regions,
        frame.stats.native_admissions,
    ]
}

fn fixture(mode: crate::GswMode) -> (CpuGsw, Frame, Probe) {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    cpu.set_jit_auto_admit(true);
    cpu.control.cr0 |= crate::CR0_PE;
    cpu.load_segment_real(crate::SegmentIndex::Cs, 0);
    cpu.set_eip(0);
    cpu.jit_direct.mkii.code_dirty = false;
    cpu.jit_direct.mkii.mapping_dirty = false;
    cpu.timing_rem = 3;
    cpu.elapsed_clocks = 71;
    cpu.core_clocks_so_far = 9;
    assert_eq!(
        cpu.fast_map_serve_enabled.enabled,
        mode.uses_approximate_timing()
    );
    let mut bus = crate::tests::TestBus::with_memory(Vec::new());
    let mut frame = Frame::new(&cpu, &mut bus, 100);
    frame.bus_at_entry = 3;
    frame.total = 7;
    frame.source_present = 1;
    frame.source_mapping = 55;
    frame.source_cost = 71;
    frame.stats.regions = 17;
    frame.stats.native_admissions = 19;
    frame.region_epoch = 21;
    frame.region_full_core = 22;
    frame.region_full_rem = 23;
    frame.region_completed = 24;
    frame.region_guard_miss = 25;
    frame.branch_taken = 1;
    frame.region_user = 1;
    frame.region_load_biases = 26;
    frame.region_mapping_epochs = 27;
    frame.session = Session {
        enabled: 1,
        bus: MkiiBusSessionParts {
            trace_clocks: MkiiCounterPath::direct(offset_of!(Probe, trace) as u32),
            isa_clocks: MkiiCounterPath::direct(offset_of!(Probe, isa) as u32),
            mapping_epoch: MkiiCounterPath::direct(offset_of!(Probe, mapping) as u32),
            trace_origin: 11,
            cost_epoch: 71,
            bus_numerator: 33,
            bus_denominator: 105,
        },
        ..Session::default()
    };
    let probe = Probe {
        trace: 16,
        isa: 3,
        mapping: 55,
    };
    frame.session.raw_limit = u64::MAX / frame.session.bus.bus_numerator;
    frame.session.threshold_limit = u64::MAX / frame.session.bus.bus_denominator;
    (cpu, frame, probe)
}

#[test]
fn mkii_native_admission_refuses_live_guards_before_publication() {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(crate::GswMode::Gsw586);
    let mut ops = operations(&[0x90, 0x90]);
    ops[0].region_len = 2;
    ops[0].region = Some(crate::mkii::ops::Region::build(cpu.class_table(), &ops));
    assert_eq!(ops[0].region.as_ref().unwrap().prefixes[2].raw_core, 24);
    let code = probe_code(&ops[0], cpu.persona());
    let invoke: unsafe extern "C" fn(*mut CpuGsw, *mut Probe, *mut Frame) -> u32 =
        unsafe { std::mem::transmute(code.entry_ptr()) };
    for case in 0..37 {
        let (mut cpu, mut frame, mut probe) = fixture(crate::GswMode::Gsw586);
        match case {
            0 => {}
            1 => frame.stop = true,
            2 => frame.source_present = 0,
            3 => cpu.jit_direct.mkii.code_dirty = true,
            4 => cpu.jit_direct.mkii.mapping_dirty = true,
            5 => cpu.registers.eip = 1,
            6 => cpu.registers.segments[crate::SegmentIndex::Cs.index()].base = 16,
            7 => frame.table = 1,
            8 => cpu.control.cr0 &= !crate::CR0_PE,
            9 => cpu.registers.eflags |= crate::FLAG_VM,
            10 => cpu.registers.eflags |= crate::FLAG_TF,
            11 => cpu.registers.segments[crate::SegmentIndex::Cs.index()].default_size_32 = true,
            12 => cpu.rep_resume_active = true,
            13 => cpu.interrupt_shadow = true,
            14 => cpu.written_count = 1,
            15 => cpu.written_pages_overflow = true,
            16 => cpu.last_written_page = 0,
            17 => cpu.fast_map_serve_enabled.enabled = false,
            18 => cpu.rmw_census_enabled = true,
            19 => cpu.slot_census_enabled = true,
            20 => {
                cpu.cpl = 3;
                cpu.alignment_armed = true;
            }
            21 => probe.mapping = 56,
            22 => frame.source_cost = 72,
            23 => probe.trace = 10,
            24 => {
                probe.trace = u64::MAX;
                probe.isa = 12;
            }
            25 => probe.trace = u64::MAX / 33 + 12,
            26 => cpu.timing_rem = 12,
            27 => cpu.elapsed_clocks = u64::MAX - 1,
            28 => frame.cap = 9,
            29 => frame.bus_at_entry = 4,
            30 => {
                frame.session.bus.bus_numerator = 1;
                frame.session.bus.bus_denominator = 1;
                frame.session.bus.trace_origin = 0;
                probe.trace = u64::MAX - 3;
                probe.isa = 0;
                frame.bus_at_entry = u64::MAX - 3;
            }
            31 => {
                frame.session.bus.bus_numerator = 1;
                frame.session.bus.bus_denominator = 2;
                frame.session.bus.trace_origin = 0;
                probe.trace = u64::MAX - 1;
                probe.isa = 0;
                frame.bus_at_entry = u64::MAX / 2 + 2;
            }
            32 => {
                frame.session.bus.bus_numerator = 1;
                frame.session.bus.bus_denominator = 2;
                frame.bus_at_entry = 4;
                frame.cap = u64::MAX - 10;
            }
            33 => frame.total = u64::MAX - 1,
            34 => frame.session.enabled = 0,
            35 => {
                #[cfg(feature = "reflected-call-memo")]
                {
                    cpu.reflected_call_journal = true;
                }
                #[cfg(not(feature = "reflected-call-memo"))]
                {
                    frame.source_mapping = 56;
                }
            }
            36 => {
                #[cfg(feature = "reflected-call-diagnostic")]
                {
                    cpu.retire_gates.reflected_call_diag_armed = true;
                }
                #[cfg(not(feature = "reflected-call-diagnostic"))]
                {
                    frame.source_cost = 0;
                }
            }
            _ => unreachable!(),
        }
        frame.session.raw_limit = u64::MAX / frame.session.bus.bus_numerator;
        frame.session.threshold_limit = u64::MAX / frame.session.bus.bus_denominator;
        let before = published(&cpu, &frame);
        let registers = cpu.registers.clone();
        let pending = cpu.pending_flags;
        let elapsed = cpu.elapsed_clocks;
        // SAFETY: the emitted probe only traverses these live counters and state;
        // neither path calls a bus helper or executes a guest instruction.
        let accepted = unsafe { invoke(&mut cpu, &mut probe, &mut frame) };
        assert_eq!(accepted, u32::from(case == 0), "case={case}");
        assert_eq!(cpu.registers, registers, "case={case}");
        assert_eq!(cpu.pending_flags, pending, "case={case}");
        assert_eq!(cpu.elapsed_clocks, elapsed, "case={case}");
        if case == 0 {
            assert_eq!(
                published(&cpu, &frame),
                [
                    55,
                    2,
                    3,
                    0,
                    0,
                    0,
                    0,
                    0,
                    1,
                    0,
                    1,
                    u64::from(cpu.can_take_interrupt()),
                    7,
                    18,
                    20
                ]
            );
        } else {
            assert_eq!(published(&cpu, &frame), before, "case={case}");
        }
    }
}
