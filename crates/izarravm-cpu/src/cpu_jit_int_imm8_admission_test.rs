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
