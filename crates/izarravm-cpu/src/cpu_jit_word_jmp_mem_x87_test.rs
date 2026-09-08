// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::tests::jit_x87_direct::{arm, direct_memory, run_to_halt, x87_cpu};

#[test]
fn word_memory_jump_chains_float_source_to_integer_target_and_spills_x87() {
    const SOURCE: u32 = 0x100;
    const TARGET: u32 = 0x300;
    const POINTER: u32 = 0x800;

    let mut memory = vec![0; 0x2000];
    memory[SOURCE as usize - 1] = 0x90;
    memory[SOURCE as usize..SOURCE as usize + 13].copy_from_slice(&[
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // three mov ax,ax
        0x66, 0xd9, 0xe8, // fld1 with the Direct x87 Dword admission size
        0xff, 0x26, 0x00, 0x08, // jmp word [0x800]
    ]);
    memory[TARGET as usize..TARGET as usize + 7].copy_from_slice(&[
        0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, // three mov bx,bx
        0xf4,
    ]);
    memory[POINTER as usize..POINTER as usize + 4].copy_from_slice(&[
        TARGET as u8,
        (TARGET >> 8) as u8,
        0xa5,
        0x5a,
    ]);
    assert_ne!(
        u32::from_le_bytes(
            memory[POINTER as usize..POINTER as usize + 4]
                .try_into()
                .unwrap()
        ),
        TARGET,
        "the high-byte poison must make a Dword target observably wrong"
    );

    let mut native = x87_cpu(GswMode::Gsw586);
    let mut interpreter = x87_cpu(GswMode::Gsw586);
    for cpu in [&mut native, &mut interpreter] {
        let mut cs = cpu.registers.cs();
        cs.default_size_32 = false;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
    }
    let mut native_bus = direct_memory(memory.clone());
    let mut interpreter_bus = direct_memory(memory.clone());

    arm(&mut native, 0x0f7f);
    run_to_halt(&mut native, &mut native_bus);
    arm(&mut interpreter, 0x0f7f);
    run_to_halt(&mut interpreter, &mut interpreter_bus);
    native
        .read_memory_u8(
            &mut native_bus,
            SegmentIndex::Ds,
            0,
            BusAccessKind::DataRead,
        )
        .expect("initialize direct map");

    arm(&mut native, 0x0f7f);
    native.registers.eip = TARGET;
    let target_key = jit::direct::key_for(&native, TARGET, false).expect("target key");
    assert!(matches!(
        native.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Interpret
    ));
    let target_compilation =
        jit::direct::compile(&mut native, TARGET, false).expect("integer target block");
    assert!(!target_compilation.has_x87);
    assert_eq!(target_compilation.span.instructions, 3);
    let target_id = native
        .jit_direct
        .install(&target_compilation)
        .expect("target install");

    native.registers.eip = SOURCE;
    let source_key = jit::direct::key_for(&native, SOURCE, false).expect("source key");
    assert!(matches!(
        native.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let source_compilation =
        jit::direct::compile(&mut native, SOURCE, false).expect("float source block");
    assert!(
        source_compilation.has_x87,
        "source lost x87 slot: span={}, fp_clocks={}, code_len={}",
        source_compilation.span.instructions,
        source_compilation.weighted_fp_clocks,
        source_compilation.code.len()
    );
    assert!(source_compilation.dynamic_successor);
    assert_eq!(source_compilation.successors, [None, None]);
    assert_eq!(source_compilation.byte_reads, 0);
    assert_eq!(source_compilation.word_reads, 1);
    assert_eq!(source_compilation.dword_reads, 0);
    assert_eq!(source_compilation.span.instructions, 5);
    let source_id = native
        .jit_direct
        .install(&source_compilation)
        .expect("source install");
    let source_block = native
        .jit_direct
        .block(source_id)
        .expect("source block remains live");
    let cells_before = [0, 1].map(|slot| {
        native
            .jit_direct
            .link_cell_state_for_test(source_id, slot)
            .expect("source link cell")
    });
    assert!(cells_before.iter().all(|state| !state.2));

    arm(&mut native, 0x0f7f);
    native.registers.eip = SOURCE;
    native_bus.memory.copy_from_slice(&memory);
    let installs = native.perf_counters().jit_direct_blocks_installed;
    let invalidations = native.perf_counters().code_invalidations;
    let transfers = native.perf_counters().jit_direct_linked_transfers;
    let first_insns = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source_block)
            .unwrap()
    );
    assert_eq!(native.registers.eip, TARGET);
    assert_eq!(native.perf_counters().jit_direct_insns - first_insns, 5);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers,
        transfers
    );
    assert!(native.jit_direct.block(source_id).is_some());
    assert!(native.jit_direct.block(target_id).is_some());
    assert_eq!(native.perf_counters().jit_direct_blocks_installed, installs);
    assert_eq!(native.perf_counters().code_invalidations, invalidations);
    let cells_bound = [0, 1].map(|slot| {
        native
            .jit_direct
            .link_cell_state_for_test(source_id, slot)
            .expect("same source link cell")
    });
    assert!(cells_bound.iter().any(|state| state.1 == TARGET && state.2));
    for (before, bound) in cells_before.iter().zip(cells_bound.iter()) {
        assert_eq!(bound.0, before.0, "link cell storage changed during bind");
    }

    arm(&mut native, 0x0f7f);
    arm(&mut interpreter, 0x0f7f);
    native.registers.eip = SOURCE;
    interpreter.registers.eip = SOURCE;
    native_bus.memory.copy_from_slice(&memory);
    interpreter_bus.memory.copy_from_slice(&memory);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    let direct_insns = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source_block)
            .unwrap()
    );
    for _ in 0..8 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        1,
        "the second entry must cross into the integer target natively"
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - direct_insns,
        8,
        "all five source and three target instructions must retire natively"
    );
    assert_eq!(native.registers.eip, TARGET + 6);
    assert!(native.jit_direct.block(source_id).is_some());
    assert!(native.jit_direct.block(target_id).is_some());
    assert_eq!(native.perf_counters().jit_direct_blocks_installed, installs);
    assert_eq!(native.perf_counters().code_invalidations, invalidations);
    let cells_after_chain = [0, 1].map(|slot| {
        native
            .jit_direct
            .link_cell_state_for_test(source_id, slot)
            .expect("live source link cell after chain")
    });
    assert_eq!(cells_after_chain, cells_bound);
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interpreter)
    );
    assert_eq!(
        native.fpu, interpreter.fpu,
        "the float source must spill its x87 register, status and tag state before chaining"
    );
    assert_eq!(native.fpu.get(0), 1.0, "the native FLD1 must reach CpuGsw");
    assert_eq!(native.eflags(), interpreter.eflags());
    assert_eq!(native.pending_flags, interpreter.pending_flags);
    assert_eq!(native.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(native.timing_rem, interpreter.timing_rem);
    assert_eq!(native.fp_rem, interpreter.fp_rem);
    assert_eq!(native_bus.memory, interpreter_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks()
    );
}
