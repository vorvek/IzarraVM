// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Group-2 shift by CL (0xd3 /4../7, register destination) against the interpreter.
//!
//! The count is runtime data, so the classes that matter are the MASK classes, not magnitudes:
//! 0 (no flags may move — the merge must be skipped entirely, see `emit_shift_cl`), 1 (OF is
//! defined), 2..=31 (OF preserved), and 32/33 (masked back to 0/1). CL garbage above bit 4 and
//! above bit 7 must be ignored. `shift ecx, cl` pins that the count is captured before the
//! destination write. The seeds run with LIVE pending flags (`arm` leaves an ALU descriptor
//! pending), so a lowering that dropped or double-materialized lazy flags diverges here.

use super::*;

const ENTRY: u32 = 0x501;
const DEST: u32 = 0x8123_4567;

fn flat_cpu(entry: u32) -> CpuGsw {
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
    cpu.set_eip(entry);
    cpu
}

fn decode_fixture(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).unwrap();
    }
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn install_block(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, linear, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, linear, true).expect("shift-by-CL compiles");
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("shift-by-CL installs");
    cpu.jit_direct.block(id).unwrap()
}

fn arm(cpu: &mut CpuGsw, ecx: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(DEST);
    cpu.registers.set_ecx(ecx);
    cpu.registers.set_esp(0xc000);
    // All arithmetic flags SET, so a count of zero that leaked the merge would clear some of
    // them, and a shift that failed to write one would leave a stale 1 the interpreter clears.
    cpu.registers.eflags = 0x8d7;
    cpu.pending_flags = PendingFlags::default();
    // Leave a LIVE lazy-flag descriptor, exactly as cpu_jit_double_shift_test's arm does.
    let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

/// `d3 /op` on `dst`, then the two register-move filler slots and a HLT boundary.
fn run_case(op: u8, dst: u8, ecx: u32, context: &str) {
    let modrm = 0b1100_0000 | (op << 3) | dst;
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + 7]
        .copy_from_slice(&[0xd3, modrm, 0x89, 0xf6, 0x89, 0xff, 0xf4]);

    let mut native = flat_cpu(ENTRY);
    let mut interpreter = flat_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 2, ENTRY + 4];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native, ENTRY);
    arm(&mut native, ecx);
    arm(&mut interpreter, ecx);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();

    let retired = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap(),
        "native block did not run: {context}"
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        native.registers, interpreter.registers,
        "registers differ: {context}"
    );
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "lazy flags differ: {context}"
    );
    assert_eq!(
        native.eflags(),
        interpreter.eflags(),
        "EFLAGS differ: {context}"
    );
    assert_eq!(
        native.elapsed_clocks, interpreter.elapsed_clocks,
        "clock charge differs: {context}"
    );
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "bus timing differs: {context}"
    );
    assert_eq!(native.perf_counters().jit_direct_insns - retired, 3);
}

#[test]
fn register_shifts_by_cl_match_the_interpreter_for_every_count_class() {
    for op in [4u8, 5, 6, 7] {
        // 0xffff_ff05 and 0x0000_2101: garbage above CL and above the five-bit mask must both
        // be ignored; 32 and 33 wrap to 0 and 1, picking the no-merge and the OF-defined merge.
        for ecx in [0u32, 1, 5, 31, 32, 33, 0xffff_ff05, 0x0000_2101] {
            run_case(op, 0, ecx, &format!("d3 /{op} eax, cl ecx={ecx:#x}"));
        }
    }
}

#[test]
fn cl_count_is_captured_before_ecx_destination_changes() {
    for ecx in [1u32, 4, 31] {
        run_case(5, 1, ecx, &format!("d3 /5 ecx, cl ecx={ecx:#x}"));
    }
}
