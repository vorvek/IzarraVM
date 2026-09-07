// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn seed_operand(_: &mut CpuGsw, bus: &mut TestBus) {
    bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2]
        .copy_from_slice(&0x8123u16.to_le_bytes());
}

#[test]
fn test_word_helper_preserves_high_address_bits() {
    fn address(cpu: &mut CpuGsw, bus: &mut TestBus) {
        replace_row(cpu, bus, &[0x67, 0xf7, 0x03, 0x23, 0x81]);
        cpu.registers.set_ebx(0x11800);
    }
    let (code, starts) = row_program(&[0xf7, 0x47, 0, 0x23, 0x81]);
    let mut legs = run_both(&code, &starts, address);
    assert_legs_agree(&mut legs);
    assert_eq!(legs.native_insns, 1);
    assert_eq!(
        legs.native
            .direct_stall_snapshot()
            .callout_interpret_one_resync_fault,
        1
    );
}

#[test]
fn test_word_helper_preserves_a_distinct_segment_base() {
    fn segment(cpu: &mut CpuGsw, bus: &mut TestBus) {
        replace_row(cpu, bus, &[0x26, 0xf7, 0x07, 0, 0x80]);
        cpu.load_segment_real(SegmentIndex::Es, 0x10);
        bus.memory[0x1900..0x1902].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    let code = [
        0xb8, 0x11, 0x11, 0xf7, 0x47, 0, 0, 0x80, 0x75, 4, 0xb8, 0x22, 0x22, 0xf4, 0xb8, 0x33,
        0x33, 0xf4,
    ];
    let mut legs = run_both(&code, &[0, 3, 8], segment);
    assert_legs_agree(&mut legs);
    assert_eq!(legs.native.registers.eax() & 0xffff, 0x3333);
    assert!(legs.native_insns >= 3);
}

fn replace_row(cpu: &mut CpuGsw, bus: &mut TestBus, bytes: &[u8]) {
    let at = ENTRY + 3;
    bus.memory[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
    let saved_eip = cpu.registers.eip;
    cpu.set_eip(at);
    let insn = cpu.decode(bus).unwrap();
    let _ = cpu.decode_cache.put(at, insn, false, at);
    cpu.set_eip(saved_eip);
}

#[test]
fn test_word_helper_reads_the_live_immediate_and_displacement() {
    fn replace(cpu: &mut CpuGsw, bus: &mut TestBus) {
        replace_row(cpu, bus, &[0xf7, 0x47, 2, 0, 0x80]);
        bus.memory[POP_TARGET as usize + 2..POP_TARGET as usize + 4]
            .copy_from_slice(&0x8000u16.to_le_bytes());
    }
    let code = [
        0xb8, 0x11, 0x11, 0xf7, 0x47, 0, 0x34, 0x12, 0x75, 4, 0xb8, 0x22, 0x22, 0xf4, 0xb8, 0x33,
        0x33, 0xf4,
    ];
    let mut legs = run_both(&code, &[0, 3, 8], replace);
    assert_legs_agree(&mut legs);
    assert_eq!(legs.native.registers.eax() & 0xffff, 0x3333);
    assert!(legs.native_insns >= 3);
}

#[test]
fn test_word_helper_falls_back_when_the_live_row_changes() {
    fn replace(cpu: &mut CpuGsw, bus: &mut TestBus) {
        replace_row(cpu, bus, &[0x81, 0x27, 0xff, 0x00]);
        seed_operand(cpu, bus);
    }
    let legs = assert_row_resumes(&[0xf7, 0x07, 0x34, 0x12], replace);
    assert_eq!(legs.native_bus.memory[POP_TARGET as usize + 1], 0);
    assert_eq!(legs.native_bus.memory[POP_TARGET as usize], 0x23);
}

#[test]
fn test_word_helper_flags_reach_a_native_conditional_branch() {
    let code = [
        0xb8, 0x11, 0x11, 0xf7, 0x07, 0, 0x80, 0x75, 4, 0xb8, 0x22, 0x22, 0xf4, 0xb8, 0x33, 0x33,
        0xf4,
    ];
    for seed in [seed_operand, |_: &mut CpuGsw, _: &mut TestBus| {}] {
        let mut legs = run_both(&code, &[0, 3, 7], seed);
        assert_legs_agree(&mut legs);
        let expected = if legs.native_bus.memory[POP_TARGET as usize + 1] & 0x80 != 0 {
            0x3333
        } else {
            0x2222
        };
        assert_eq!(legs.native.registers.eax() & 0xffff, expected);
        assert!(legs.native_insns >= 3);
        assert_eq!(
            legs.native
                .direct_stall_snapshot()
                .callout_interpret_one_executed,
            1
        );
    }
}

#[test]
fn test_word_helper_is_selected_only_for_the_specialized_row() {
    let specialized = (core::mem::offset_of!(CpuGsw, native_table_slots)
        + core::mem::offset_of!(jit::direct::NativeTableSlots, interpret_test_word))
        as i32;
    let generic = (core::mem::offset_of!(CpuGsw, native_callout)
        + core::mem::offset_of!(jit::direct::CallOutTable, interpret_one)) as i32;
    for (row, offset, absent) in [
        (&[0xf7, 0x07, 0x34, 0x12][..], specialized, generic),
        (&[0xf7, 0x2f][..], generic, specialized),
    ] {
        let (code, starts) = row_program(row);
        let compilation = compile_fixture(&code, &starts);
        let load = |offset: i32| {
            let mut bytes = vec![0x49, 0x8b, 0x87];
            bytes.extend_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&[0xff, 0xd0]);
            bytes
        };
        assert!(position(&compilation.code, &load(offset)).is_some());
        assert!(position(&compilation.code, &load(absent)).is_none());
    }
}

#[test]
fn test_word_helper_clears_rep_budget_on_success_and_fault() {
    for fault in [false, true] {
        let (code, starts) = row_program(&[0xf7, 0x07, 0x34, 0x12]);
        let (mut cpu, mut bus, block) = build_native(&code, &starts);
        arm_fixture(&mut cpu, &mut bus);
        cpu.rep_execution.budget = Some(RepBudget {
            bus_at_entry: 123,
            cap: 456,
        });
        if fault {
            let mut ds = cpu.registers.segment(SegmentIndex::Ds);
            ds.limit = POP_TARGET - 1;
            cpu.registers.set_segment(SegmentIndex::Ds, ds);
        }
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(cpu.rep_execution.budget, None);
        assert_eq!(cpu.native_table_slots.interpret_test_word, 0);
        assert!(cpu.native_callout.bus.is_null());
        assert_eq!(
            cpu.direct_stall_snapshot().callout_interpret_one_executed,
            1
        );
    }
}

#[test]
fn test_word_helper_publication_is_host_state_and_clone_clears_it() {
    let mut bus = sixteen_bit_bus(vec![0; 0x2000]);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    let (table, helper) = jit::direct::CallOutTable::publish(&mut bus);
    assert_eq!(table.bus, (&mut bus as *mut TestBus).cast::<()>());
    assert_ne!(helper, 0);
    assert_ne!(helper, table.interpret_one);
    cpu.native_callout = table;
    cpu.native_table_slots.interpret_test_word = helper;
    let copy = cpu.clone();
    assert_eq!(copy, cpu);
    assert!(copy.native_callout.bus.is_null());
    assert_eq!(copy.native_table_slots.interpret_test_word, 0);
}

#[test]
fn test_word_helper_segment_fault_keeps_fault_accounting() {
    fn narrow_ds(cpu: &mut CpuGsw, _: &mut TestBus) {
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = POP_TARGET - 1;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    }
    let (code, starts) = row_program(&[0xf7, 0x07, 0x34, 0x12]);
    let mut legs = run_both(&code, &starts, narrow_ds);
    assert_legs_agree(&mut legs);
    assert_eq!(legs.native_insns, 1);
    assert_eq!(
        legs.native
            .direct_stall_snapshot()
            .callout_interpret_one_resync_fault,
        1
    );
}

#[test]
fn test_word_helper_page_fault_matches_interpreter_delivery() {
    fn fixture(code: &[u8], starts: &[u32]) -> (CpuGsw, TestBus) {
        let mut memory = vec![0; 0x5000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
        memory[0x300] = 0xf4;
        memory[0x408..0x410].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9b, 0, 0]);
        memory[0x410..0x418].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x93, 0, 0]);
        memory[0x570..0x578].copy_from_slice(&[0, 3, 8, 0, 0, 0x86, 0, 0]);
        memory[0x3000..0x3004].copy_from_slice(&0x4023u32.to_le_bytes());
        memory[0x4000..0x4004].copy_from_slice(&0x23u32.to_le_bytes());
        memory[0x4004..0x4008].copy_from_slice(&0x1023u32.to_le_bytes());
        let mut bus = sixteen_bit_bus(memory);
        let mut cpu = sixteen_bit_code_cpu(ENTRY);
        cpu.control.cr0 |= CR0_PE | CR0_PG;
        cpu.control.cr3 = 0x3000;
        cpu.gdtr.base = 0x400;
        cpu.gdtr.limit = 0x17;
        cpu.idtr.base = 0x500;
        cpu.idtr.limit = 0x7f;
        for segment in [
            SegmentIndex::Cs,
            SegmentIndex::Ss,
            SegmentIndex::Ds,
            SegmentIndex::Es,
        ] {
            let code = segment == SegmentIndex::Cs;
            cpu.registers.set_segment(
                segment,
                SegmentRegister {
                    selector: if code { 8 } else { 16 },
                    base: 0,
                    limit: 0xffff,
                    access: if code { 0x9b } else { 0x93 },
                    default_size_32: false,
                },
            );
        }
        arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, DATA_PAGE]);
        let starts: Vec<_> = starts.iter().map(|offset| ENTRY + offset).collect();
        warm_sixteen_bit(&mut cpu, &mut bus, &starts);
        arm_fixture(&mut cpu, &mut bus);
        cpu.registers.set_ebx(0x2800);
        (cpu, bus)
    }
    let (code, starts) = row_program(&[0xf7, 0x07, 0x34, 0x12]);
    let (mut interp, mut interp_bus) = fixture(&code, &starts);
    drive(&mut interp, &mut interp_bus);
    let (mut native, mut native_bus) = fixture(&code, &starts);
    let compilation = jit::direct::compile(&mut native, ENTRY, false).expect("TEST block");
    let key = jit::direct::key_for(&native, ENTRY, false).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native.jit_direct.block(id).unwrap();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    assert_eq!(
        native
            .direct_stall_snapshot()
            .callout_interpret_one_resync_fault,
        1
    );
    drive(&mut native, &mut native_bus);
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.control.cr2, 0x2800);
    assert_eq!(native.control.cr2, interp.control.cr2);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions
    );
}
