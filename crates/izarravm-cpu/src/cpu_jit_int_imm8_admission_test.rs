// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `0xCD` INT imm8 as a compile-walk admission.
//!
//! Before this slice the opcode had no admission path anywhere in `jit/direct.rs`: it was in
//! neither `non_continuable_walk_candidate` nor `non_continuable_break_probe_candidate`, and
//! `classify` had no arm for it at any operand size. A compile walk that reached one stopped with
//! `BarrierStop::NonContinuable` and the whole block rejected structurally.
//!
//! The corpus measured what that costs. On `100-000-pyramid` the `INT 16h` keyboard-poll head at
//! guest linear `0x10E3D` carries 25,750,933 static unbound exits, 87.2% of that game's whole
//! `absent` class, and the per-edge link census records the edge `0x00010E37 -> 0x00010E3D` as
//! `not_attempted` -- the predecessor block ends three instructions short of the INT and its
//! successor is never keyed at all. The other three 486 laggards pay the same instruction through
//! the `0xCD` `non_continuable` census row instead: `15-move-hole-puzzle` 23,474,587,
//! `21-for-1-to-4` 4,858,277 static plus 11,869,917 dynamic, `10rogue` 1,703,556. Same
//! instruction, two classifications, about 56 M static exits between them.
//!
//! # Scope
//!
//! ADMISSION ONLY. The slot is TERMINAL: `DirectKind::is_terminal` stops the walk at the INT, so
//! the block ends there and no slot after it is emitted. Letting a block CONTINUE past an INT is a
//! separate design and is deliberately not attempted here.
//!
//! `0xCC` INT3, `0xCE` INTO and `0xCF` IRET are untouched and stay barriers; the fixtures below
//! pin that, because a rule phrased as "the interrupt opcodes" would sweep all four in.

use super::sixteen_bit::{
    arm_native_sixteen_bit, sixteen_bit_bus, sixteen_bit_code_cpu, warm_sixteen_bit,
};
use super::*;

const ENTRY: u32 = 0x401;
const STACK_TOP: u32 = 0x4000;

/// `INT 16h`, and not an arbitrary vector: it is the exact instruction at pyramid's `0x10E3D`,
/// the single address this slice is measured on.
const INT_16H: [u8; 2] = [0xcd, 0x16];

/// `MOV ESI,ESI` then `MOV EDI,EDI`: two register moves that lower natively, so a block built
/// around the INT has a real prefix and a real tail and the span assertions can tell "the walk
/// stopped AT the INT" apart from "the walk never started".
const LEAD: [u8; 2] = [0x89, 0xf6];
const TAIL: [u8; 2] = [0x89, 0xff];

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

/// Stage a program at `ENTRY`, warm the decode line for every instruction start in it, and hand
/// back the CPU and bus ready for a compile walk.
///
/// Every start is warmed rather than only the entry, because `compile_with_budget` reads each
/// slot's line out of the decode cache and a cold line ends the walk with a `Retry` that would
/// read like a structural refusal.
fn staged(code: &[u8], starts: &[u32]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.registers.set_esp(STACK_TOP);
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    cpu.set_eip(ENTRY);
    (cpu, bus)
}

/// `LEAD` / `body` / `TAIL` / `HLT`, with the body's own start warmed. The shape
/// `cpu_jit_slice7_test.rs`'s `still_a_barrier` uses, so a reader comparing the two files sees
/// one fixture shape rather than two.
fn staged_around(body: &[u8]) -> (CpuGsw, TestBus) {
    let mut code = LEAD.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&TAIL);
    code.push(0xf4);
    staged(&code, &[ENTRY, body_at, tail_at])
}

// ------------------------------------------------------------------------------------------
// The admission itself
// ------------------------------------------------------------------------------------------

/// THE RED PROOF, mid-block. Before the slice this walk stops at the INT with
/// `BarrierStop::NonContinuable` and `compile` returns `StructuralReject`; after it, the INT is a
/// call-out slot and the walk stops right AFTER it because the slot is terminal.
///
/// The span is the whole assertion. `2` means the `MOV ESI,ESI` prefix plus the INT and nothing
/// else: a `3` would mean the tail was emitted behind a slot that never returns to it, and a `1`
/// would mean the INT was dropped rather than admitted.
#[test]
fn an_int_imm8_is_admitted_as_a_terminal_call_out_slot_mid_block() {
    let (mut cpu, _bus) = staged_around(&INT_16H);
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: INT imm8 was not admitted")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 2,
        "the walk must carry the prefix and the INT and STOP there: the INT loads CS:EIP through \
         the IDT, so any slot after it is emitted dead"
    );
    assert_eq!(
        compilation.callout_slots, 1,
        "the INT must produce a call-out slot, not a silent lowering"
    );
}

/// The same admission with the INT as the block ENTRY, which is the shape three of the four
/// laggards pay it in (`15-move-hole-puzzle`'s 23,474,587-exit `0xCD` census row is a keyed
/// address that refuses, not an unkeyed one).
///
/// A separate fixture rather than a second arm of the one above, because the entry position is a
/// different gate: `walk_admits_non_continuable_entry` decides it, and a walk that admits an
/// opcode mid-block while refusing it at slot 0 would pass the fixture above and still leave the
/// measured population untouched.
#[test]
fn an_int_imm8_is_admitted_as_a_block_entry() {
    let mut code = INT_16H.to_vec();
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&TAIL);
    code.push(0xf4);
    let (mut cpu, _bus) = staged(&code, &[ENTRY, tail_at]);

    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("an INT imm8 block entry was structurally rejected")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 1,
        "the INT is the whole block"
    );
    assert_eq!(compilation.callout_slots, 1);
}

/// The WORD form, which is the only form the measured population has: every corpus guest is
/// 16-bit real-mode or V86 code, so `operand_size` is `Word` at all four laggard sites. A slice
/// that admitted only the Dword form would pass both fixtures above and move nothing.
#[test]
fn an_int_imm8_is_admitted_in_a_sixteen_bit_segment() {
    let mut code = LEAD.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&INT_16H);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&TAIL);
    code.push(0xf4);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.load_segment_real(segment, 0);
    }
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.registers.set_esp(STACK_TOP);
    for &linear in &[ENTRY, body_at, tail_at] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }

    let compilation = match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("a 16-bit INT imm8 was structurally rejected")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(compilation.span.instructions, 2);
    assert_eq!(compilation.callout_slots, 1);
}

// ------------------------------------------------------------------------------------------
// What is NOT admitted
// ------------------------------------------------------------------------------------------

/// The neighbours stay barriers, each for its own reason: `0xCC` INT3 and `0xCE` INTO carry no
/// immediate vector and were never measured, and `0xCF` IRET pops CS and is refused everywhere.
///
/// This is the fixture a rule phrased as "the interrupt opcodes" fails. The slice admits ONE
/// opcode, and the set membership below is what says so at the predicate level.
#[test]
fn the_neighbouring_interrupt_opcodes_stay_barriers() {
    for (bytes, label) in [
        (&[0xccu8][..], "0xCC INT3"),
        (&[0xce][..], "0xCE INTO"),
        (&[0xcf][..], "0xCF IRETD"),
    ] {
        let (mut cpu, _bus) = staged_around(bytes);
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, true),
                jit::direct::CompileOutcome::StructuralReject(_)
                    | jit::direct::CompileOutcome::Retry(_)
            ),
            "{label} must stay a structural barrier"
        );
    }
}

/// The two opcode sets, and the fact that they answer different questions.
///
/// `0xCD` joins the WALK-candidate set, which is a capability statement. Whether it also joins the
/// BREAK-PROBE set is a measured POPULATION decision, and that predicate's own doc carries the
/// duke3d-586 post-mortem for what happens when the two are conflated: 2.79 M break-site probes
/// for 92 installs moved `jit_direct_blocks_installed` 200,711 -> 235,235 and cost 5.7% of that
/// row. The membership asserted here is the one this slice ships, in both directions, so a later
/// edit that moves it has to move this fixture too.
#[test]
fn int_imm8_joins_the_walk_candidate_set_only() {
    assert!(
        jit::direct::non_continuable_walk_candidate(0xcd),
        "a compile walk must be able to carry INT imm8"
    );
    assert!(
        !jit::direct::non_continuable_break_probe_candidate(0xcd),
        "INT imm8 stays out of the break-probe set: the predecessor block absorbs the INT into \
         its own span, so the address needs no key of its own and the probe would be pure residue"
    );
    for opcode in [0xccu16, 0xce, 0xcf] {
        assert!(
            !jit::direct::non_continuable_walk_candidate(opcode),
            "{opcode:#04x} must stay out of the walk-candidate set"
        );
    }
}

// ------------------------------------------------------------------------------------------
// The knob, in both directions
// ------------------------------------------------------------------------------------------

/// Restore the ambient reading when dropped, INCLUDING on unwind -- a trailing
/// `set_int_imm8_rows_for_test(None)` would be skipped by a panicking fixture and could leak a
/// forced arm into a later one under `--test-threads=1`.
#[must_use]
struct IntImm8RowsGuard;

impl Drop for IntImm8RowsGuard {
    fn drop(&mut self) {
        jit::direct::set_int_imm8_rows_for_test(None);
    }
}

fn select_int_imm8_rows(enabled: bool) -> IntImm8RowsGuard {
    jit::direct::set_int_imm8_rows_for_test(Some(enabled));
    assert_eq!(
        jit::direct::int_imm8_rows_armed(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_INT_IMM8_ROWS"
    );
    IntImm8RowsGuard
}

/// THE RED STATE, kept runnable rather than described.
///
/// The OFF arm is main: `int_imm8_admitted_here` is the only thing that admits `0xCD`, so with it
/// false the compile walk's `continuable` term is false and the block stops at the INT exactly as
/// it did before the slice. Every positive fixture above is red on this arm, which is what makes
/// them non-vacuous.
#[test]
fn the_off_arm_keeps_int_imm8_a_structural_barrier() {
    let _guard = select_int_imm8_rows(false);
    let (mut cpu, _bus) = staged_around(&INT_16H);
    assert!(
        matches!(
            jit::direct::compile(&mut cpu, ENTRY, true),
            jit::direct::CompileOutcome::StructuralReject(_)
                | jit::direct::CompileOutcome::Retry(_)
        ),
        "the OFF arm must refuse INT imm8, or the knob gates nothing"
    );
}

/// **THE ROW MUST NOT RELOCATE.** The fixture above says the OFF arm refuses, and that assertion
/// would pass whether the refusal is made by `int_imm8_admitted_here` or one step later by
/// `classify` returning `None` -- so on its own it cannot see the defect this exists for.
///
/// The defect: refuse `0xCD` in `classify` instead, and the OFF arm stops with
/// `BarrierStop::HardBoundary` where main stopped it with `NonContinuable`. Neither arm compiles
/// the block, so nothing about the guest moves, but the barrier census row MOVES BETWEEN STOP
/// ARMS -- and a two-arm reconciliation reading "exactly one row removed, zero rows new" reads a
/// relocated row as a new one. `retf_admitted_here`'s own doc states the rule this enforces.
///
/// So the OFF arm's recorded `stop_reason` is pinned, and the ON arm is required to leave no row
/// at all rather than a moved one.
#[test]
fn the_off_arm_keeps_mains_own_barrier_row() {
    fn fixture() -> (CpuGsw, TestBus) {
        let mut code = LEAD.to_vec();
        let body_at = ENTRY + code.len() as u32;
        code.extend_from_slice(&INT_16H);
        let tail_at = ENTRY + code.len() as u32;
        code.extend_from_slice(&TAIL);
        code.push(0xf4);

        let mut memory = vec![0u8; 0x5000];
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

        // A FRESH CPU per arm: the census accumulates, so two arms sharing one would compare a
        // row against itself.
        let mut cpu = flat_cpu();
        cpu.jit_direct.enable_barrier_census_for_test();
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &[ENTRY, body_at, tail_at] {
            cpu.set_eip(linear);
            cpu.fetch_decoded(&mut bus, linear).unwrap();
        }
        (cpu, bus)
    }

    {
        let _guard = select_int_imm8_rows(false);
        let (mut cpu, _bus) = fixture();
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, true),
                jit::direct::CompileOutcome::StructuralReject(_)
                    | jit::direct::CompileOutcome::Retry(_)
            ),
            "the OFF arm must refuse"
        );
        let snapshot = cpu
            .direct_barrier_census_snapshot()
            .expect("the census is enabled for this fixture");
        let row = snapshot
            .rows
            .iter()
            .find(|row| row.opcode == 0xcd)
            .expect("the OFF arm must record its own 0xCD barrier row");
        assert_eq!(
            row.stop_reason.to_string(),
            "non_continuable",
            "the OFF arm must be main's own row, or this fixture compares against a moved base"
        );
    }
    {
        let _guard = select_int_imm8_rows(true);
        let (mut cpu, _bus) = fixture();
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, true),
                jit::direct::CompileOutcome::Compiled(_)
            ),
            "the ON arm must admit INT imm8"
        );
        let snapshot = cpu
            .direct_barrier_census_snapshot()
            .expect("the census is enabled for this fixture");
        assert!(
            snapshot.rows.iter().all(|row| row.opcode != 0xcd),
            "the ON arm compiled the block and must not also have recorded a 0xCD barrier row: \
             the row is RETIRED, not relocated"
        );
    }
}

/// The spelling table, exhaustively. `int_imm8_rows_armed` caches its env reading in a
/// process-wide `OnceLock`, so this is the only place the contract is assertable.
#[test]
fn int_imm8_rows_spelling_table_names_every_arm() {
    use std::env::VarError;
    let parse = jit::direct::parse_int_imm8_rows_arm_for_test;
    assert!(parse(Err(VarError::NotPresent)), "unset is the ON default");
    assert!(
        parse(Ok(String::new())),
        "empty names the SAME arm as unset"
    );
    for on in ["1", "on", "ON", " on "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the ON arm");
    }
    for off in ["0", "off", "OFF", " off "] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the OFF arm");
    }
}

#[test]
#[should_panic(expected = "names no arm")]
fn int_imm8_rows_refuses_to_guess_a_mistyped_arm() {
    let _ = jit::direct::parse_int_imm8_rows_arm_for_test(Ok("yes".to_string()));
}

// ------------------------------------------------------------------------------------------
// Fidelity: the emitted slot against the interpreter
// ------------------------------------------------------------------------------------------
//
// The claim this slice rests on is that the call-out is guest-visibly indistinguishable from the
// interpreted instruction. It is structural rather than argued: the helper runs
// `execute_hot_cached_or_decoded` over the decoded `0xCD`, which IS the interpreter's own arm. But
// "structural" is a reason to expect these to pass, not a reason to skip them. What they can still
// catch is everything AROUND the step: the EIP the block leaves behind, the frame the guest sees
// on its stack, the clocks, and the retirement count.

const INT_ENTRY: u32 = 0x100;
const INT_DATA_PAGE: u32 = 0x1000;
const INT_STACK_TOP: u32 = 0x1700;
/// `mov ax,0x1111` then `int 0x21`. The INT sits at slot 1 so the fixture proves the block carries
/// a real native prefix INTO the call-out rather than consisting of the call-out alone.
const INT_CODE: &[u8] = &[0xb8, 0x11, 0x11, 0xcd, 0x21];
const INT_STARTS: &[u32] = &[0, 3];

/// Real mode with a ZEROED IVT, so vector 0x21 reads CS:IP = 0000:0000 and the delivery lands on
/// the `HLT` seeded at linear 0. Both legs get the same byte, so the interpreted leg has somewhere
/// to land too and the comparison has an oracle rather than a runaway.
fn int_program() -> Vec<u8> {
    let mut program = vec![0u8; 0x2000];
    program[INT_ENTRY as usize..INT_ENTRY as usize + INT_CODE.len()].copy_from_slice(INT_CODE);
    program[0] = 0xf4;
    program
}

fn arm_int_fixture(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(INT_STACK_TOP);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(INT_ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn drive_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..64 {
        if cpu.halted {
            return;
        }
        cpu.cycle(bus)
            .expect("the fixture must not stop the machine");
    }
    panic!("the guest never halted, stuck at {:#x}", cpu.linear_eip());
}

/// Build the native leg: compile at `INT_ENTRY`, install, and hand back the runnable block.
fn int_native_leg() -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
    let mut bus = sixteen_bit_bus(int_program());
    let mut cpu = sixteen_bit_code_cpu(INT_ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, INT_DATA_PAGE]);
    let linears: Vec<u32> = INT_STARTS.iter().map(|offset| INT_ENTRY + offset).collect();
    warm_sixteen_bit(&mut cpu, &mut bus, &linears);
    let compilation = match jit::direct::compile(&mut cpu, INT_ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the 16-bit INT block became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("the 16-bit INT block requested a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 2,
        "the block must be the MOV plus the INT, and stop there"
    );
    assert_eq!(
        compilation.callout_int_imm8_slots, 1,
        "the INT must be counted in its own slot class"
    );
    let key = jit::direct::key_for(&cpu, INT_ENTRY, false).expect("a 16-bit key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the INT block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");
    (cpu, bus, block)
}

/// THE DIFFERENTIAL. Same guest bytes, same start state; one leg runs the compiled block and the
/// other never compiles at all. Every quantity the two must agree on is named, because a case that
/// compares registers alone is the shape that ships a wrong clock charge or a double retirement.
#[test]
fn the_int_call_out_matches_the_interpreter() {
    let _guard = select_int_imm8_rows(true);

    let mut interp_bus = sixteen_bit_bus(int_program());
    let mut interp = sixteen_bit_code_cpu(INT_ENTRY);
    arm_int_fixture(&mut interp, &mut interp_bus);
    drive_to_halt(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = int_native_leg();
    arm_int_fixture(&mut native, &mut native_bus);
    let before = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture block must not stop the machine"),
        "the installed block must actually run natively"
    );
    let native_insns = native.perf_counters().jit_direct_insns - before;
    drive_to_halt(&mut native, &mut native_bus);

    assert_eq!(
        native_insns, 2,
        "the MOV and the INT must both retire natively; the block ends at the INT"
    );
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp),
        "registers or EIP differ between the native and interpreted legs"
    );
    assert_eq!(native.eflags(), interp.eflags(), "EFLAGS");
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "guest RAM differs, and the pushed interrupt frame lives in here"
    );
    assert_eq!(
        native.elapsed_clocks, interp.elapsed_clocks,
        "guest clocks differ: the call-out must charge exactly the interpreter clocks(37)"
    );
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions,
        "retirement counts differ, so some instruction was counted twice or not at all"
    );
}

/// The frame itself, read out of guest RAM rather than inferred from the differential above.
///
/// The differential proves the two legs AGREE; this proves they agree on the right thing. A
/// call-out that pushed nothing and an interpreter that pushed nothing would pass that fixture and
/// fail this one.
#[test]
fn the_int_call_out_pushes_the_real_mode_frame_and_lands_on_the_vector() {
    let _guard = select_int_imm8_rows(true);
    let (mut native, mut native_bus, block) = int_native_leg();
    arm_int_fixture(&mut native, &mut native_bus);
    let flags_before = native.eflags() as u16;
    native
        .try_run_direct_block_for_test(&mut native_bus, block)
        .expect("the block must run");

    assert_eq!(
        native.registers.esp() & 0xffff,
        (INT_STACK_TOP - 6) & 0xffff,
        "a real-mode INT pushes exactly three words"
    );
    let sp = (INT_STACK_TOP - 6) as usize;
    let word = |at: usize| u16::from_le_bytes([native_bus.memory[at], native_bus.memory[at + 1]]);
    assert_eq!(
        word(sp + 4),
        flags_before,
        "FLAGS is pushed first, as it stood BEFORE the delivery cleared IF and TF"
    );
    assert_eq!(
        word(sp + 2),
        0,
        "CS is pushed next, and the fixture CS is 0"
    );
    assert_eq!(
        word(sp),
        (INT_ENTRY + INT_CODE.len() as u32) as u16,
        "the return offset is the address AFTER the INT, which is what proves the helper moved \
         EIP to the slot before stepping rather than leaving it at the block entry"
    );
    assert_eq!(
        native.linear_eip(),
        0,
        "the zeroed IVT sends vector 0x21 to 0000:0000, and the block exit must advance EIP by \
         ZERO or the handler address would not survive"
    );
    assert_eq!(
        native.registers.eflags & FLAG_IF,
        0,
        "the delivery must clear IF exactly as the interpreter delivery does"
    );
}

// ------------------------------------------------------------------------------------------
// The two refusals that keep the row honest
// ------------------------------------------------------------------------------------------

/// **V86 BELOW IOPL 3 IS REFUSED AT ADMISSION**, not cleaned up afterwards by the governor.
///
/// The interpreter arm for `0xCD` notifies the bus and then raises #GP through `check_v86_iopl`
/// on EVERY execution in that state. Admitted there, the slot would take the call-out fault path
/// on every single visit -- spill, call, delivery, side exit, dispatcher trip -- buying the guest
/// nothing, and it would charge a permanent demoted-site entry for every distinct INT site in an
/// EMM386-style guest before the governor caught up.
///
/// The population this row is measured on is unaffected: the four 486 laggards run under TOKAEMM
/// at real IOPL 3, which is the arm the second half of this fixture covers.
#[test]
fn v86_below_iopl_three_refuses_the_admission_and_iopl_three_keeps_it() {
    for (iopl, admitted) in [(0u32, false), (3u32, true)] {
        let _guard = select_int_imm8_rows(true);
        let mut code = LEAD.to_vec();
        let body_at = ENTRY + code.len() as u32;
        code.extend_from_slice(&INT_16H);
        let tail_at = ENTRY + code.len() as u32;
        code.extend_from_slice(&TAIL);
        code.push(0xf4);

        let mut memory = vec![0u8; 0x5000];
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

        let mut cpu = CpuGsw::default();
        cpu.set_mode(GswMode::Gsw586);
        for segment in [
            SegmentIndex::Cs,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Ss,
        ] {
            cpu.load_segment_real(segment, 0);
        }
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.eflags = 0x202 | FLAG_VM | (iopl << 12);
        cpu.cpl = 3;
        assert!(cpu.is_v86_mode(), "the fixture must actually be in V86");
        assert_eq!(
            u32::from(cpu.iopl()),
            iopl,
            "the fixture must set the IOPL it names"
        );

        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &[ENTRY, body_at, tail_at] {
            cpu.set_eip(linear);
            cpu.fetch_decoded(&mut bus, linear).unwrap();
        }

        let compiled = matches!(
            jit::direct::compile(&mut cpu, ENTRY, false),
            jit::direct::CompileOutcome::Compiled(_)
        );
        assert_eq!(
            compiled, admitted,
            "V86 at IOPL {iopl}: admission must be {admitted}, because the interpreter faults on \
             every execution below IOPL 3 and waves the task through at 3"
        );
    }
}

/// **THE GOVERNOR MUST NOT DEMOTE THIS ROW ON ITS RESYNCS**, or the lever undoes itself after
/// three visits.
///
/// `note_execution` demotes a slot after `GOVERNOR_RESYNC_LIMIT` resyncs inside
/// `GOVERNOR_WINDOW` executions. That is right for a row whose resume predicate fails for reasons
/// the compile walk could not see; it is wrong for a row that can never resume BY CONSTRUCTION,
/// because its resync is the design rather than a symptom. Without the exemption the block would
/// be retired and recompiled without the slot, putting the instruction straight back at a barrier.
///
/// The loop runs well past the governor window, so a demotion would be visible in the counter and
/// in the block going away.
#[test]
fn the_governor_does_not_demote_the_int_row_for_resyncing() {
    let _guard = select_int_imm8_rows(true);
    let (mut cpu, mut bus, block) = int_native_leg();

    for visit in 0..12 {
        arm_int_fixture(&mut cpu, &mut bus);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("the block must not stop the machine"),
            "visit {visit}: the block stopped running natively, so the slot was demoted"
        );
    }

    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_interpret_one_demoted, 0,
        "the INT row resyncs by construction and must never be demoted for it"
    );
    assert!(
        stalls.callout_interpret_one_resync >= 12,
        "the fixture is vacuous unless the slot really did resync every visit, got {}",
        stalls.callout_interpret_one_resync
    );
}

/// The fault arm keeps its governor, which is the other half of the exemption being NARROW.
///
/// A row exempted from demotion on every path would be a Trojan horse: a site that faults on every
/// execution would keep paying a call-out frame forever. The exemption covers the RETIRED path
/// only, so `note_execution(true)` on the fault arm still runs. This fixture states the predicate
/// rather than driving a faulting guest, because the state that produces a systematic fault --
/// V86 below IOPL 3 -- is refused at admission by the fixture above and is therefore unreachable
/// from a compiled slot at all. Both bounds are real and they are independent.
#[test]
fn only_the_int_row_claims_the_never_resumes_property() {
    for row in jit::direct::InterpretOneRow::ALL {
        assert_eq!(
            row.always_resyncs(),
            row == jit::direct::InterpretOneRow::IntImm8,
            "{} claims always_resyncs; only the INT row may, because only it is guaranteed to \
             transfer control",
            row.label()
        );
    }
}

// ------------------------------------------------------------------------------------------
// The `always_resyncs` gate, on the one guest shape that needs it
// ------------------------------------------------------------------------------------------

/// The IVT slot for vector `0x21`, in a program image.
fn seed_ivt(program: &mut [u8], vector: u8, segment: u16, offset: u16) {
    let at = usize::from(vector) * 4;
    program[at..at + 2].copy_from_slice(&offset.to_le_bytes());
    program[at + 2..at + 4].copy_from_slice(&segment.to_le_bytes());
}

/// The fall-through address: the byte right after the `INT` in `INT_CODE`.
const INT_FALL_THROUGH: u32 = INT_ENTRY + INT_CODE.len() as u32;

/// The same program, but with vector `0x21` pointing at `CS:INT_FALL_THROUGH` -- the very address
/// the block would have carried on to. A `HLT` lives there so both legs still halt.
fn fall_through_handler_program() -> Vec<u8> {
    let mut program = int_program();
    program[INT_FALL_THROUGH as usize] = 0xf4;
    seed_ivt(&mut program, 0x21, 0, INT_FALL_THROUGH as u16);
    program
}

/// **THE SHAPE `always_resyncs` EXISTS FOR, EXECUTED.** A guest whose interrupt vector lands on the
/// address right after the `INT`, in the same code segment.
///
/// This is the one guest for which `ResumeSnapshot::allows_resume` answers TRUE after an `INT`, and
/// every clause is satisfied for an ordinary reason rather than a contrived one:
///
/// * R1 compares `cpu.registers.eip` against `end_eip` and `cpu.registers.cs()` against the
///   snapshot. The handler IS `end_eip` and the handler's segment IS the snapshot's, so both halves
///   pass -- which is the whole point: R1 is a reading of the guest's IVT, not a fact about the
///   mechanism.
/// * R2 compares CS and SS (the row answers `may_write_segment`), and a same-CS delivery with no
///   privilege change moves neither.
/// * R3's TF clause passes because `INT` CLEARS TF, and its IF clause is DIRECTIONAL -- it refuses a
///   0-to-1 edge, and `INT` clears IF, which is the arm that resumes.
///
/// So without the structural gate the resume tail would restore the block's ENTRY EIP and carry on
/// through the block's remaining slots, keeping the pushed frame and the cleared IF while undoing
/// the control transfer. With the gate the slot resyncs and the guest lands on its handler.
///
/// A `debug_assert` used to stand at the gate claiming `allows_resume` would answer FALSE here.
/// Review N1 caught it: it asserted the negation of its own doc comment, and this fixture is the
/// guest that would have tripped it. It is gone, and this test is what owns the claim now.
///
/// RED-FIRST, measured: change `InterpretOneRow::always_resyncs` to name any other row and this
/// fixture goes red at the resync assertion below with `left: 0` -- the slot RESUMED, the block
/// carried on from its entry EIP, and the control transfer was undone. The real-mode differential
/// above stays GREEN under that same mutation, because its zeroed IVT sends the handler to
/// `0000:0000` and R1 refuses on its own. That contrast is the fixture's whole reason to exist.
#[test]
fn a_handler_at_the_fall_through_address_still_resyncs() {
    let _guard = select_int_imm8_rows(true);

    let mut interp_bus = sixteen_bit_bus(fall_through_handler_program());
    let mut interp = sixteen_bit_code_cpu(INT_ENTRY);
    arm_int_fixture(&mut interp, &mut interp_bus);
    drive_to_halt(&mut interp, &mut interp_bus);

    let mut native_bus = sixteen_bit_bus(fall_through_handler_program());
    let mut native = sixteen_bit_code_cpu(INT_ENTRY);
    arm_native_sixteen_bit(&mut native, &mut native_bus, &[0x0000, INT_DATA_PAGE]);
    let linears: Vec<u32> = INT_STARTS.iter().map(|offset| INT_ENTRY + offset).collect();
    warm_sixteen_bit(&mut native, &mut native_bus, &linears);
    let compilation = match jit::direct::compile(&mut native, INT_ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the fall-through fixture became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry(_) => {
            panic!("the fall-through fixture asked for a retry")
        }
    };
    assert_eq!(compilation.callout_int_imm8_slots, 1);
    let key = jit::direct::key_for(&native, INT_ENTRY, false).expect("a 16-bit key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("install the fall-through block");
    let block = native.jit_direct.block(id).expect("the block must be live");

    arm_int_fixture(&mut native, &mut native_bus);
    let cs_before = native.registers.cs();
    let before = native.perf_counters().jit_direct_insns;
    let elapsed_before = native.elapsed_clocks;
    let outcome = native
        .run_direct_block_accounted_for_test(&mut native_bus, block, u64::MAX)
        .expect("the fixture block must not stop the machine")
        .expect("the installed terminal INT block must actually run natively");
    let native_insns = native.perf_counters().jit_direct_insns - before;

    assert!(
        outcome.core_clocks > 0,
        "the terminal native INT entry must retain nonzero committed core work"
    );
    assert_eq!(
        outcome.core_clocks,
        native.elapsed_clocks - elapsed_before,
        "the terminal native INT return carries its prefix and delivered-interrupt core work"
    );

    // R1's two halves, read off the machine rather than argued. This is the assertion that makes
    // the fixture NON-VACUOUS: a delivery landing anywhere else would fail R1 and the gate would
    // never have been the thing that refused.
    assert_eq!(
        native.registers.eip, INT_FALL_THROUGH,
        "the handler must be at the fall-through address, or R1 refuses and the gate is untested"
    );
    assert_eq!(
        native.registers.cs(),
        cs_before,
        "the handler must be in the SAME code segment, or R1's second half refuses"
    );

    // The gate did the refusing. `resync` counts the slot leaving through the RESYNC stub; a
    // resumed slot would have counted nothing here and would have run the rest of the block.
    let stalls = native.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_interpret_one_resync, 1,
        "the slot must resync; a resumed slot would restore the block's entry EIP"
    );
    assert_eq!(
        native_insns, 2,
        "the MOV and the INT retire natively and the block ends at the INT"
    );

    // The control transfer survived: a resumed slot would have kept this frame AND rewound EIP.
    assert_eq!(
        native.registers.esp() & 0xffff,
        (INT_STACK_TOP - 6) & 0xffff,
        "the three-word real-mode frame must still be on the stack"
    );

    drive_to_halt(&mut native, &mut native_bus);
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp),
        "registers or EIP differ from the interpreted leg on the fall-through-handler guest"
    );
    assert_eq!(native.eflags(), interp.eflags(), "EFLAGS");
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "guest RAM differs, and the pushed frame lives in here"
    );
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks, "guest clocks");
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions,
        "retirement counts differ"
    );
}

// ------------------------------------------------------------------------------------------
// Protected mode, and the fault mid-delivery
// ------------------------------------------------------------------------------------------
//
// The differentials above run in a 16-bit real-mode segment against a zeroed IVT. That is the
// population the row is measured on, and it exercises the SHORTEST delivery the interpreter has:
// four words read, three pushed, no descriptor walked.
//
// Review N3 asked for the other two. Protected mode walks an IDT gate AND a GDT descriptor, writes
// an Accessed bit back into the table, and pushes dwords -- and it is what
// `INT_IMM8_MAX_DATA_ACCESSES`' derivation is about, which until now was arithmetic on paper with
// nothing executing it. And a delivery that FAILS is the one path where the call-out has bespoke
// code of its own rather than the interpreter's arm: `interpret_one_step`'s fault tail names the
// SLOT's `start_eip` and `start_cs` to `finish_instruction`, charges the fetch by hand, reports
// zero clocks and leaves through a different stub. Every one of those is a place to get a call-out
// wrong in a way the interpreter cannot be wrong, so both are differentials rather than
// self-checks.

const PM_ENTRY: u32 = 0x100;
const PM_DATA_PAGE: u32 = 0x1000;
const PM_GDT_BASE: u32 = 0x1200;
const PM_IDT_BASE: u32 = 0x1400;
/// On the code page, far below `PM_ENTRY`, so a delivery does not disturb the block's own bytes.
const PM_HANDLER: u32 = 0x800;
const PM_STACK_TOP: u32 = 0x1700;
/// Index 1: flat 32-bit code. The Accessed bit is CLEAR (`0x9A`, not `0x9B`), so the gate's CS load
/// writes the descriptor back and the RAM comparison covers that write too.
const PM_SEL_CODE: u16 = 0x08;
/// Index 2: flat 32-bit data.
const PM_SEL_DATA: u16 = 0x10;
/// Two usable entries.
const PM_GDT_LIMIT: u16 = 0x17;
/// Big enough for vector `0x21`, whose descriptor occupies `0x108..=0x10F`.
const PM_IDT_LIMIT_OK: u16 = 0x1ff;
/// TOO SMALL for vector `0x21` and big enough for vector 13, which is the whole fault fixture:
/// the delivery raises `#GP(0x21 * 8 + 2)` and that #GP has a gate to be delivered through.
const PM_IDT_LIMIT_SHORT: u16 = 0xff;
/// `#GP`'s vector, whose gate has to stay inside even the short limit.
const PM_VECTOR_GP: u32 = 13;
/// The vector the fixture's `INT` names.
const PM_VECTOR: u32 = 0x21;

/// `mov eax,0x11111111` then `int 0x21`, at 32-bit operand size. The INT is at slot 1 for the same
/// reason `INT_CODE` puts it there: the block must carry a real native prefix into the call-out.
const PM_CODE: &[u8] = &[0xb8, 0x11, 0x11, 0x11, 0x11, 0xcd, 0x21];
const PM_STARTS: &[u32] = &[0, 5];
/// Where the faulting `INT` itself begins. The #GP frame must name THIS address, not the block's
/// entry, and that is the assertion the fault fixture is built around.
const PM_INT_AT: u32 = PM_ENTRY + 5;

fn pm_descriptor(low: u32, high: u32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&low.to_le_bytes());
    bytes[4..].copy_from_slice(&high.to_le_bytes());
    bytes
}

/// A 32-bit interrupt gate: present, DPL 0, through `PM_SEL_CODE`.
fn pm_interrupt_gate(offset: u32) -> [u8; 8] {
    pm_descriptor(
        (u32::from(PM_SEL_CODE) << 16) | (offset & 0xffff),
        (offset & 0xffff_0000) | (0x8e << 8),
    )
}

fn pm_program() -> Vec<u8> {
    let mut program = vec![0u8; 0x2000];
    program[PM_ENTRY as usize..PM_ENTRY as usize + PM_CODE.len()].copy_from_slice(PM_CODE);
    program[PM_HANDLER as usize] = 0xf4;
    let mut put = |at: u32, bytes: [u8; 8]| {
        let at = at as usize;
        program[at..at + 8].copy_from_slice(&bytes);
    };
    put(
        PM_GDT_BASE + u32::from(PM_SEL_CODE),
        pm_descriptor(0x0000_ffff, 0x00cf_9a00),
    );
    put(
        PM_GDT_BASE + u32::from(PM_SEL_DATA),
        pm_descriptor(0x0000_ffff, 0x00cf_9300),
    );
    put(PM_IDT_BASE + PM_VECTOR * 8, pm_interrupt_gate(PM_HANDLER));
    put(
        PM_IDT_BASE + PM_VECTOR_GP * 8,
        pm_interrupt_gate(PM_HANDLER),
    );
    program
}

fn pm_cpu(idt_limit: u16) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(PM_SEL_CODE, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(PM_SEL_DATA, 0x93));
    }
    cpu.gdtr = DescriptorTable {
        base: PM_GDT_BASE,
        limit: PM_GDT_LIMIT,
    };
    cpu.idtr = DescriptorTable {
        base: PM_IDT_BASE,
        limit: idt_limit,
    };
    cpu.set_eip(PM_ENTRY);
    cpu
}

fn arm_pm_fixture(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(PM_STACK_TOP);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(PM_ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// Compile and install the protected-mode INT block. `d = true`: `PM_CODE` is 32-bit.
fn pm_native_leg(idt_limit: u16) -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
    let mut bus = sixteen_bit_bus(pm_program());
    let mut cpu = pm_cpu(idt_limit);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, PM_DATA_PAGE]);
    for &offset in PM_STARTS {
        let linear = PM_ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    cpu.set_eip(PM_ENTRY);
    let compilation = match jit::direct::compile(&mut cpu, PM_ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the protected-mode INT block became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry(_) => {
            panic!("the protected-mode INT block requested a retry")
        }
    };
    assert_eq!(
        compilation.span.instructions, 2,
        "the block must be the MOV plus the INT, and stop there"
    );
    assert_eq!(
        compilation.callout_int_imm8_slots, 1,
        "the INT must be counted in its own slot class"
    );
    let key = jit::direct::key_for(&cpu, PM_ENTRY, true).expect("a protected-mode key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the protected-mode INT block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");
    (cpu, bus, block)
}

/// The two legs, plus the two quantities that only exist BEFORE the guest is driven on to its
/// `HLT`: the EIP the block exit left behind, and the ESP under the frame it pushed.
struct PmLegs {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    /// Where the block exit put EIP. Read here rather than after the drive, because the handler's
    /// `HLT` retires and moves it.
    native_eip_after_block: u32,
    native_esp_after_block: u32,
}

/// Run both legs of the protected-mode fixture and hand back the two settled machines.
///
/// `native_insns` is a PARAMETER and not a constant, because the two fixtures differ in it and the
/// difference is the point: a successful delivery retires the `INT`, and a delivery that faults
/// does not. `interpret_one_step`'s fault arm reports the PREFIX ONLY -- `finish_instruction` has
/// already counted the faulting instruction in `perf.instructions`, so the block must not count it
/// again. A fault leg reporting 2 here would be that double count.
fn pm_both_legs(idt_limit: u16, native_insns: u64) -> PmLegs {
    let mut interp_bus = sixteen_bit_bus(pm_program());
    let mut interp = pm_cpu(idt_limit);
    arm_pm_fixture(&mut interp, &mut interp_bus);
    drive_to_halt(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = pm_native_leg(idt_limit);
    arm_pm_fixture(&mut native, &mut native_bus);
    let before = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture block must not stop the machine"),
        "the installed block must actually run natively"
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - before,
        native_insns,
        "the block reported the wrong number of natively retired instructions"
    );
    let native_eip_after_block = native.registers.eip;
    let native_esp_after_block = native.registers.esp();
    drive_to_halt(&mut native, &mut native_bus);
    PmLegs {
        native,
        native_bus,
        interp,
        interp_bus,
        native_eip_after_block,
        native_esp_after_block,
    }
}

fn pm_assert_legs_agree(
    native: &mut CpuGsw,
    native_bus: &TestBus,
    interp: &mut CpuGsw,
    interp_bus: &TestBus,
) {
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(
        crate::tests::settled_registers(native),
        crate::tests::settled_registers(interp),
        "registers or EIP differ between the native and interpreted legs"
    );
    assert_eq!(native.eflags(), interp.eflags(), "EFLAGS");
    assert_eq!(
        native.registers.cs(),
        interp.registers.cs(),
        "CS selector differs, and the delivery is what loads it"
    );
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "guest RAM differs: the pushed frame AND the descriptor write-backs live in here"
    );
    assert_eq!(
        native.elapsed_clocks, interp.elapsed_clocks,
        "guest clocks differ"
    );
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions,
        "retirement counts differ, so some instruction was counted twice or not at all"
    );
}

fn pm_dword(bus: &TestBus, at: u32) -> u32 {
    let at = at as usize;
    u32::from_le_bytes([
        bus.memory[at],
        bus.memory[at + 1],
        bus.memory[at + 2],
        bus.memory[at + 3],
    ])
}

/// **THE PROTECTED-MODE DIFFERENTIAL.** A real IDT gate, a real GDT descriptor, dword pushes, and
/// the Accessed-bit write-back the gate's CS load performs -- against the interpreter running the
/// identical guest.
///
/// The RAM comparison is the load-bearing one here rather than the register comparison: the
/// descriptor write-back is a guest-memory side effect that no register carries, and it happens
/// inside the `deferred_code_writes` window the helper opens around the step.
#[test]
fn the_int_call_out_matches_the_interpreter_in_protected_mode() {
    let _guard = select_int_imm8_rows(true);
    let legs = pm_both_legs(PM_IDT_LIMIT_OK, 2);
    let native_bus = &legs.native_bus;

    // Non-vacuity: the delivery really happened and really went through the gate.
    assert_eq!(
        legs.native_eip_after_block, PM_HANDLER,
        "the gate must have transferred control to the handler, and the block exit must advance \
         EIP by ZERO or the handler address would not survive"
    );
    let sp = PM_STACK_TOP - 12;
    assert_eq!(
        legs.native_esp_after_block, sp,
        "a same-privilege 32-bit gate pushes exactly three dwords"
    );
    assert_eq!(
        pm_dword(native_bus, sp),
        PM_INT_AT + 2,
        "the pushed EIP is the address AFTER the INT"
    );
    assert_eq!(
        pm_dword(native_bus, sp + 4),
        u32::from(PM_SEL_CODE),
        "the pushed CS is the running code selector"
    );
    assert_eq!(
        legs.native.registers.eflags & FLAG_IF,
        0,
        "an interrupt gate clears IF"
    );
    assert_eq!(
        native_bus.memory[(PM_GDT_BASE + u32::from(PM_SEL_CODE) + 5) as usize] & 0x01,
        0x01,
        "the gate's CS load must have written the Accessed bit back into the GDT"
    );

    let PmLegs {
        mut native,
        native_bus,
        mut interp,
        interp_bus,
        ..
    } = legs;
    pm_assert_legs_agree(&mut native, &native_bus, &mut interp, &interp_bus);
}

/// **THE FAULT PATH.** The IDT limit refuses vector `0x21`, so the delivery itself raises
/// `#GP(0x21 * 8 + 2)` instead of transferring control -- and the call-out's fault tail has to
/// produce exactly the frame the interpreter's own tail produces.
///
/// This is the one shape where the call-out is NOT simply the interpreter's arm.
/// `interpret_one_step` advances EIP past the `INT` before stepping, so its fault tail has to hand
/// `finish_instruction` the SLOT's own `start_eip` and `start_cs` for the rewind; the block's entry
/// EIP is a plausible wrong answer that only a fixture like this can see, because it differs from
/// the right one by the prefix's five bytes. The hand-rolled fetch charge, the zero-clock return
/// and the separate stub are the other three pieces of bespoke tail code, and the clock and
/// retirement comparisons below cover them.
///
/// RED-FIRST, and the mutation is named so the next reader can re-run it: pass `entry_eip` in place
/// of `start_eip` at `direct.rs`'s
/// `cpu.finish_instruction(bus, result, start_eip, start_cs, 0, None, None)`. Measured, not
/// assumed: with that one-word edit the pushed EIP becomes `PM_ENTRY` (256) instead of `PM_INT_AT`
/// (261) and this fixture goes red on the frame read below.
///
/// WHAT THE MUTATION ALSO SHOWED, and it is worth stating rather than hiding: seven existing
/// `interpret_one` fault fixtures go red on it too. The fault tail is SHARED by every
/// `InterpretOne` row, so there is no INT-specific fault code to mutate -- which is exactly review
/// N3's structural point and the reason this is a nit rather than a hole. Mutating the delivery
/// itself (skipping the IDT-limit check in `execute_extended.rs`) would move BOTH legs identically
/// and prove nothing, which is why this fixture does not rest on the differential alone: the frame
/// reads above it are absolute values, not a comparison, and they are what stays red when both legs
/// move together. What the fixture adds over the existing seven is the `IntImm8` row driving that
/// tail at all, in protected mode, through a delivery that fails with an ERROR CODE -- the shape
/// the row's own `INT_IMM8_MAX_DATA_ACCESSES` bound is derived against.
#[test]
fn a_failed_int_delivery_faults_exactly_as_the_interpreter_does() {
    let _guard = select_int_imm8_rows(true);
    let legs = pm_both_legs(PM_IDT_LIMIT_SHORT, 1);
    let native_bus = &legs.native_bus;

    // Non-vacuity: this really is the fault path and not a successful delivery.
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_interpret_one_resync_fault, 1,
        "the slot must have left through the resync-FAULT stub, or the delivery succeeded"
    );
    assert_eq!(
        legs.native_eip_after_block, PM_HANDLER,
        "#GP is delivered through vector 13's gate, which is inside even the short limit"
    );

    // The #GP frame: the error code, then EIP, CS and EFLAGS above it.
    let sp = PM_STACK_TOP - 16;
    assert_eq!(
        legs.native_esp_after_block, sp,
        "a #GP with an error code pushes four dwords"
    );
    assert_eq!(
        pm_dword(native_bus, sp),
        PM_VECTOR * 8 + 2,
        "the error code names the IDT entry that was out of limit, with the IDT bit set"
    );
    assert_eq!(
        pm_dword(native_bus, sp + 4),
        PM_INT_AT,
        "the fault frame must name the INT ITSELF, not the block's entry: a fault rewinds onto \
         the faulting instruction, and the call-out's tail is what supplies that address"
    );
    assert_eq!(
        pm_dword(native_bus, sp + 8),
        u32::from(PM_SEL_CODE),
        "the pushed CS is the faulting code selector"
    );

    let PmLegs {
        mut native,
        native_bus,
        mut interp,
        interp_bus,
        ..
    } = legs;
    pm_assert_legs_agree(&mut native, &native_bus, &mut interp, &interp_bus);
}
