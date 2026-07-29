// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Differential cover for the lowerings added by the runtime-weighted reject sweep
//! (`dev_docs/2026-07-30-dispatch-architecture-audit.md` §5d).
//!
//! Every case runs the same guest bytes natively and interpreted from identical state and
//! compares registers, lazy flags, EFLAGS, core clocks and bus clocks. The tested opcode is
//! placed MID-BLOCK, never at the entry: an opcode at a block's entry slot parks the block on
//! the interpreter, so an entry-position fixture silently tests nothing.

use super::*;

const ENTRY: u32 = 0x401;

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
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
    cpu.set_eip(ENTRY);
    cpu
}

/// Run `body` mid-block against the interpreter. `seed_eflags` is applied to both roles, so a
/// lowering that reads a flag it should have ignored, or drops one it should have kept, diverges.
fn differential(body: &[u8], seed_eflags: u32, live_pending: bool, context: &str) {
    // A leading `mov esi,esi` keeps the tested opcode off the entry slot; the two trailing
    // register moves and the HLT give the block a tail and a terminator.
    let mut code = vec![0x89, 0xf6];
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, body_at, tail_at];
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut native, ENTRY, true).unwrap_or_else(|| {
        panic!(
            "{context}: the \
             opcode under test did not compile; it is still a barrier"
        )
    });
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");
    assert_eq!(
        compilation.span.instructions, 3,
        "{context}: block must cover all three slots, so the tested opcode really ran natively"
    );

    for cpu in [&mut native, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0xc000);
        cpu.registers.eflags = seed_eflags;
        cpu.pending_flags = PendingFlags::default();
        if live_pending {
            let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
        }
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();

    let retired = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        native.perf_counters().jit_direct_insns - retired,
        3,
        "{context}: all three slots must retire natively"
    );
    assert_eq!(
        native.registers, interpreter.registers,
        "{context}: registers"
    );
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "{context}: lazy flags"
    );
    assert_eq!(native.eflags(), interpreter.eflags(), "{context}: EFLAGS");
    assert_eq!(
        native.elapsed_clocks, interpreter.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
}

#[test]
fn direction_flag_matches_the_interpreter_from_both_polarities() {
    // 0x202 has DF clear, 0x602 has DF (bit 10) set, so each opcode is exercised both as a
    // no-op and as a real transition. Live pending flags on every case: DF is outside the lazy
    // descriptor's ARITH mask, so a lowering that cleared or rebuilt the descriptor diverges.
    for (opcode, name) in [(0xfcu8, "CLD"), (0xfd, "STD")] {
        for seed in [0x202u32, 0x602] {
            for pending in [false, true] {
                differential(
                    &[opcode],
                    seed,
                    pending,
                    &format!("{name} seed={seed:#x} pending={pending}"),
                );
            }
        }
    }
}
