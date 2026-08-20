// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The SIX 16-bit rows of the tombraid FMV census's loop A, behind `IZARRAVM_V86_LOOP_ROWS`.
//!
//! `dev_docs/tombraid-reprofile-2026-08-20.md` §4.1 names the loop and
//! `.bench/results/tomb-fmv-admission-20260819/census-fpu-loop-rows-1-summary.json` carries the
//! rows. Interpreted hits over the 20e9 boot+FMV prefix, and the static unbound exits behind them:
//!
//! | row | hits | unbound exits | kind |
//! |---|---:|---:|---|
//! | `0x07` POP ES | 97,347,816 | 95,057,524 | `DirectKind::PopSegReal` |
//! | `0x3d` CMP AX,imm16 | 96,182,170 | 587,085 | `DirectKind::AluImm` at Word |
//! | `0xa1` MOV AX,es:moffs16 | 95,614,884 | 95,502,528 | `DirectKind::Load` at Word |
//! | `0xf8` CLC | 95,090,745 | 94,883,220 | `DirectKind::CarryFlag` |
//! | `0xff /1` DEC word cs:[m] | 95,055,642 | 95,020,029 | `DirectKind::RmwIncDec`, already lowered |
//! | `0x2b /0` SUB ax,cs:[m] | 95,055,326 | 0 | `DirectKind::AluMemSource`, already lowered |
//!
//! **The last two rows' prefix is CS, not a data segment.** The re-profile calls them
//! "`prefix_unsupported` word-memory forms (both prefix mask 64)"; mask 64 is
//! `(segment_index(Cs) + 1) << 5`. See `v86_loop_rows_enabled` for the probe that settled it and
//! for the loop's disassembly. That makes the slice's third change a REVERSAL of an explicit
//! refusal rather than a widening, which is why `prefixes_supported_for` carries the argument and
//! why this file pins both the admission and what stays refused with it (a protected-mode CS
//! WRITE).
//!
//! Two collateral forms ride the same gate because they sit in the SAME straight-line run and
//! landing the rows without them only relocates the exits one instruction along: `0xa3`
//! `mov cs:[m], ax` and `0xc7 /0` `mov word cs:[m], imm16`.
//!
//! **Every fixture here states its arm through `set_v86_loop_rows_for_test`, in both directions.**
//! The shipped default is OFF, so a positive fixture that read the ambient knob would be testing
//! the refusal and calling it a lowering; the refusal fixtures force `Some(false)` rather than
//! leaning on the default, so they keep meaning what they say after a flip.
//!
//! The differential rows run the same guest bytes natively and through a BLOCK-FREE interpreter
//! from identical state, and compare registers (segment registers and EIP included), the raw lazy
//! flags descriptor, materialized EFLAGS, the halt latch, core clocks, bus clocks and the WHOLE of
//! guest RAM. The tested opcode is always MID-BLOCK: an opcode at a block's entry slot side-exits
//! having retired nothing, so an entry-position fixture cannot tell a lowering from a refusal.
//!
//! The 16-bit CPU here gives every segment a DIFFERENT base (CS 0, DS 0x400, ES 0x300) on purpose.
//! With the usual all-zero real-mode layout `cs:[0x220]`, `es:[0x220]` and `[0x220]` are the same
//! linear address, so a lowering that dropped the override, or took the wrong one, would agree
//! with the interpreter byte for byte and every assertion would pass.

// MUTATION EVIDENCE (2026-08-20, applied by hand, run, restored). Each row names the fixture that
// caught it; a mutation nobody catches is a fixture bug, not a free pass.
//
// | mutation | caught by | assertion |
// |---|---|---|
// | `0xa1` width back to `MemoryWidth::Dword`, i.e. the pre-slice emitter | `the_moffs_pair_*`, `the_cs_override_*`, `the_tombraid_loop_body_*` | registers: EAX `0x1af9_1234` against the interpreter's `0xdead_1234` |
// | the CERTAIN-EXIT rule disabled | `a_statically_misaligned_*`, `the_loop_compiles_into_the_units_*` | span length: `Some(3)` where `None` is required |
// | `CarryFlag` drops `emit_set_cf_only` and writes only the flag shadow | `clc_and_stc_*`, `the_carry_*_adc` | raw lazy-flags descriptor, then registers on the ADC read-back |
// | the ADC/SBB guard narrowed back to `matches!(form, 1 \| 3)` | `the_gate_does_not_sweep_in_*` | `0x15` ADC AX,imm16 compiles where it must be a barrier |
// | `PopSegReal`'s `alu_r16_imm16(0, home(4), 2)` widened to `add_r32_imm32` | `pop_segment_preserves_the_high_half_of_esp_across_the_sixteen_bit_wrap` | registers: ESP `0xdeae_0000` against `0xdead_0000` |
// | `PopSegReal` drops the access / `default_size_32` store | `pop_segment_matches_*`, the stale-descriptor row | registers: the segment's access byte |
// | `PopSegReal`'s `shift_r32_imm8(4, RAX, 4)` -> shift by 3 | three POP fixtures | registers: the segment base |
//
// TWO OF THESE SURVIVED THE FIRST VERSION OF THIS FILE, and both survivals were the same kind of
// mistake: a seed that already held the value the emitter was failing to write.
//
// * The widened pointer advance survived a poisoned ESP high half, because `add r32, 2` and
//   `add r16, 2` agree on every value that does not carry out of bit 15. `STACK_ESP` alone was not
//   enough; the WRAP row is what discriminates.
// * The dropped segment-field store survived because both roles started from a descriptor
//   `load_segment_real` had already made real, so "write the real-mode access byte" and "write
//   nothing" produced the same bytes. `Seed::stale_segments` is what closed it.
//
// Neither was caught by adding an assertion. Both were caught by changing the STATE the assertion
// runs against, which is the general shape worth remembering: a differential compares two machines,
// and it can only see a field the two machines were ever going to disagree about.

use super::*;

/// EIP of the block entry. CS base is 0 in the 16-bit fixture, so this is also its linear address.
const ENTRY: u32 = 0x100;
/// The word operand's OFFSET, applied against whichever segment the row names. 2-aligned.
const OPERAND: u16 = 0x0220;
/// The 16-bit fixture's segment bases, chosen distinct so an override is observable.
const CS_BASE: u32 = 0x0000;
const DS_BASE: u32 = 0x0400;
const ES_BASE: u32 = 0x0300;
/// SS is base 0 with SP here, clear of every operand above.
const STACK_SP: u32 = 0x0700;
/// The same stack pointer with a POISONED high half, which is what every role is seeded with.
///
/// SS.B is 0 in this fixture, so the guest's stack pointer is SP and bits 31..16 of ESP are
/// architecturally untouched by a push or a pop: `alu_r16_imm16` preserves them where an `add r32`
/// clears them, and with a ZERO high half the two are indistinguishable. A mutation that widened
/// `PopSegReal`'s pointer advance to 32 bits SURVIVED the first version of this file for exactly
/// that reason, and this constant is what closed it.
const STACK_ESP: u32 = 0xdead_0000 | STACK_SP;

/// A distinct byte at every address, so a store of the wrong WIDTH or through the wrong SEGMENT
/// changes guest RAM even when it writes the right value. A constant fill would make a four-byte
/// store indistinguishable from a two-byte one whenever the upper half happened to match.
fn memory_fill() -> Vec<u8> {
    // A full 64K, so a stack pointer at the top of the 16-bit wrap has real memory under it.
    let mut memory = vec![0u8; 0x1_0000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// Select the arm for this thread and PROVE the selection took, the shape
/// `cpu_jit_fpu_loop_rows_test.rs`'s `select_fpu_loop_rows` uses.
fn select_v86_loop_rows(enabled: bool) {
    jit::direct::set_v86_loop_rows_for_test(Some(enabled));
    assert_eq!(
        jit::direct::v86_loop_rows_enabled(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_V86_LOOP_ROWS"
    );
}

/// Real mode, CS.D = 0 and SS.B = 0: the ordinary DOS configuration the census measured.
fn sixteen_bit_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, (CS_BASE >> 4) as u16);
    cpu.load_segment_real(SegmentIndex::Ds, (DS_BASE >> 4) as u16);
    cpu.load_segment_real(SegmentIndex::Es, (ES_BASE >> 4) as u16);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(STACK_ESP);
    cpu.set_eip(ENTRY);
    cpu
}

/// The same machine in V86: `load_segment_real` for every segment, CR0.PE set and EFLAGS.VM with
/// IOPL 3. That is the mode loop A actually runs in, and it is the arm on which
/// `segment_access_supported` short-circuits `true` for a CS-based WRITE.
fn v86_cpu() -> CpuGsw {
    let mut cpu = sixteen_bit_cpu();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    // The CACHED CPL, which `current_privilege_level` debug-asserts is 3 in V86 rather than
    // deriving from the V86 CS's arbitrary real-mode-style RPL bits (core.rs). IOPL is 3 in the
    // flags above, which is the configuration the IOPL-3 V86 monitor runs the guest at.
    cpu.cpl = 3;
    assert!(cpu.is_v86_mode(), "the V86 fixture must actually be in V86");
    assert_eq!(cpu.current_privilege_level(), 3);
    cpu
}

/// Protected mode, flat, CS.D = 1: the population the CS refusal was written against, and the one
/// where a CS-based write must STAY refused.
fn flat_protected_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    // 0x9b: present, code, EXECUTE/READ. A code segment that is not readable would refuse the
    // read row for a second reason and make the read fixture below vacuous.
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
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    cpu
}

fn map_direct_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
    let permissions = jit::fast_map::PagePermissions::UNPAGED;
    let read = bus
        .direct_page(page, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        page,
        page,
        read,
        permissions,
        cpu.physical_page_watched(page)
    ));
    let write = bus
        .direct_page(page, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        page,
        page,
        write,
        permissions,
        cpu.physical_page_watched(page)
    ));
}

/// `mov si,si` at Word / `mov esi,esi` at Dword: the leading slot that keeps the tested opcode off
/// the block entry.
const FILL_A: [u8; 2] = [0x89, 0xf6];
/// `mov di,di` / `mov edi,edi`: the trailing slot, so the tested opcode is never last either.
const FILL_B: [u8; 2] = [0x89, 0xff];

/// Compile `FILL_A / body / FILL_B / hlt` at `ENTRY` and report the span length, or `None` when the
/// walk refused it.
///
/// The three-slot shape is load-bearing for `cpu_jit_fpu_loop_rows_test.rs`'s reason: warming only
/// the entry decode line makes slot 1 miss, the walk stops at `Retry`, and a negative assertion
/// then passes whether or not the opcode is lowered. Every decode line is warmed here.
fn compile_leading_block_on(builder: fn() -> CpuGsw, d: bool, body: &[u8]) -> Option<u8> {
    let mut code = FILL_A.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = builder();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    for &linear in &[ENTRY, body_at, tail_at] {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    // Unconditional, and not only for the memory rows: with the operand pages absent from the fast
    // map EVERY memory kind is refused, so a negative assertion made without it would pass for the
    // harness's reason rather than the row's.
    for page in (0..0x1_0000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    }
}

fn compile16(body: &[u8]) -> Option<u8> {
    compile_leading_block_on(sixteen_bit_cpu, false, body)
}

fn compile32(body: &[u8]) -> Option<u8> {
    compile_leading_block_on(flat_protected_cpu, true, body)
}

// ---------------------------------------------------------------------------------------------
// Encodings. Named rather than spelled inline so a row and its refusal fixture cannot drift.
// ---------------------------------------------------------------------------------------------

fn w(value: u16) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// The six census rows, in the encodings the loop actually uses.
fn loop_a_rows() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("0x07 POP ES", vec![0x07]),
        ("0x3D CMP AX,imm16", [vec![0x3d], w(0x00b6)].concat()),
        (
            "0xA1 MOV AX,es:moffs16",
            [vec![0x26, 0xa1], w(OPERAND)].concat(),
        ),
        ("0xF8 CLC", vec![0xf8]),
        (
            "0xFF /1 DEC word cs:[m]",
            [vec![0x2e, 0xff, 0x0e], w(OPERAND)].concat(),
        ),
        (
            "0x2B /0 SUB ax,cs:[m]",
            [vec![0x2e, 0x2b, 0x06], w(OPERAND)].concat(),
        ),
    ]
}

/// The two forms the same straight-line run needs, which the gate carries with the rows.
fn loop_a_collateral() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "0xA3 MOV cs:moffs16,AX",
            [vec![0x2e, 0xa3], w(OPERAND)].concat(),
        ),
        (
            "0xC7 /0 MOV word cs:[m],imm16",
            [vec![0x2e, 0xc7, 0x06], w(OPERAND), w(0xffff)].concat(),
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// The gate itself
// ---------------------------------------------------------------------------------------------

/// THE DEFAULT PIN, and it is the one assertion that decides what a shipped binary admits.
///
/// Catches: a flip of `parse_v86_loop_rows_arm`'s `NotPresent` arm. The slice ships OFF and is
/// unpriced; a default that moved without a ladder would change every shipped binary's admission
/// silently, and the CS-override reversal in particular would ride along unmeasured.
///
/// It reads the AMBIENT knob deliberately -- no override -- so it also fails if
/// `IZARRAVM_V86_LOOP_ROWS=1` is exported in the environment running the suite, which is the
/// correct outcome: the rest of this file states its arm, and a fixture that means to pin the
/// shipped default has nowhere else to read it from.
#[test]
fn v86_loop_rows_ship_off_by_default() {
    jit::direct::set_v86_loop_rows_for_test(None);
    assert!(
        !jit::direct::v86_loop_rows_enabled(),
        "IZARRAVM_V86_LOOP_ROWS must default OFF until a tombraid-586 ladder prices it"
    );
}

/// The spelling table, both arms and the refusal.
///
/// Catches: a `_ => false` fallthrough replacing the panic. A mistyped ladder leg
/// (`IZARRAVM_V86_LOOP_ROWS=yes`) that fell through would run exactly what an unset environment
/// runs and be read as "the arm I asked for changed nothing", which is the single wrong conclusion
/// an arm ladder exists to avoid.
#[test]
fn v86_loop_rows_spelling_table_names_both_arms() {
    use std::env::VarError;
    assert!(
        !jit::direct::parse_v86_loop_rows_arm_for_test(Err(VarError::NotPresent)),
        "unset must name the OFF arm while the slice is unpriced"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(
            !jit::direct::parse_v86_loop_rows_arm_for_test(Ok(off.to_string())),
            "{off:?} must name the off arm"
        );
    }
    for on in ["1", "on", "ON", " on ", "On"] {
        assert!(
            jit::direct::parse_v86_loop_rows_arm_for_test(Ok(on.to_string())),
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "v86", "rows"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_v86_loop_rows_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_V86_LOOP_ROWS={typo:?} names no arm and must panic rather than silently \
             running the default"
        );
    }
}

/// EVERY row flips with the gate, and NOTHING ELSE DOES.
///
/// Catches, in the `false` direction: an arm that forgot its `v86_loop_rows_enabled()` guard, i.e.
/// a row that ships admitted while the knob says off -- which would make the gate-off census leg
/// disagree with main and destroy the A/B base. The CS half of that is the one that matters most,
/// because `classify` ALREADY lowers `0x2b /0` and `0xff /1` and only `prefixes_supported_for`
/// stands between them and a compiled block.
///
/// Catches, in the `true` direction: a missing or wrongly-keyed classify arm. Against a tree with
/// any one of the changes missing the `true` half fails on that row by name.
///
/// The control row is a plain `mov ax,cx`, which must compile on BOTH arms: it proves the
/// three-slot harness is not simply refusing everything, which is how this test would go vacuous.
#[test]
fn every_v86_loop_row_flips_with_the_gate() {
    select_v86_loop_rows(false);
    for (name, code) in loop_a_rows().into_iter().chain(loop_a_collateral()) {
        assert_eq!(
            compile16(&code),
            None,
            "{name} must stay a barrier with IZARRAVM_V86_LOOP_ROWS off"
        );
    }
    assert_eq!(
        compile16(&[0x89, 0xc8]),
        Some(3),
        "the control row must compile on the off arm, or this fixture cannot fail"
    );

    select_v86_loop_rows(true);
    for (name, code) in loop_a_rows().into_iter().chain(loop_a_collateral()) {
        assert_eq!(
            compile16(&code),
            Some(3),
            "{name} must be admitted with IZARRAVM_V86_LOOP_ROWS on"
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The neighbours this slice must NOT have swept in. Each is one bit away from an admitted row.
///
/// * `0x15` ADC AX,imm16 and `0x1d` SBB AX,imm16 -- ALU form 5's carry members. They share the arm
///   `0x3d` reaches, and `emit_alu_preloaded`'s Word lane masks both operands with `and`, which
///   CLEARS host CF, then tags the descriptor as the SUB class. An admitted `15 iw` would compute
///   without the carry in and then evaluate its lazy CF as `a < b`. The forms-1|3|5 guard is what
///   refuses them.
/// * `0x17` POP SS -- `PopSegReal`'s sibling in the same neighbourhood. It arms the
///   one-instruction interrupt shadow, which a native block never passes through.
/// * `0xf5` CMC -- CLC/STC's neighbour. It needs the INCOMING carry rather than a constant.
/// * `0xfc` CLD -- an existing kind whose opcode is deliberately still off the Word allowlist.
/// * `0xc7 /0` REGISTER form at Word -- refused inside its own arm since before this slice, and
///   the `0xa1`/`0xa3` width work must not have disturbed it.
#[test]
fn the_gate_does_not_sweep_in_the_neighbouring_encodings() {
    select_v86_loop_rows(true);
    for (name, code) in [
        ("0x15 ADC AX,imm16", [vec![0x15], w(0x1234)].concat()),
        ("0x1D SBB AX,imm16", [vec![0x1d], w(0x1234)].concat()),
        ("0x17 POP SS", vec![0x17]),
        ("0xF5 CMC", vec![0xf5]),
        ("0xFC CLD", vec![0xfc]),
        (
            "0xC7 /0 MOV AX,imm16",
            [vec![0xc7, 0xc0], w(0x1234)].concat(),
        ),
    ] {
        assert_eq!(
            compile16(&code),
            None,
            "{name} is not part of this slice and must stay a barrier"
        );
    }
    // ...and the rows that were admitted BEFORE this slice and must not have been disturbed.
    for (name, code) in [
        (
            "0x3B CMP AX,[m] (word memory, pre-slice)",
            [vec![0x3b, 0x06], w(OPERAND)].concat(),
        ),
        (
            "0x89 MOV [m],AX (word memory, pre-slice)",
            [vec![0x89, 0x06], w(OPERAND)].concat(),
        ),
        (
            "0xFF /1 DEC word [m], NO override",
            [vec![0xff, 0x0e], w(OPERAND)].concat(),
        ),
        (
            "0x2B /0 SUB ax,[m], NO override",
            [vec![0x2b, 0x06], w(OPERAND)].concat(),
        ),
    ] {
        assert_eq!(
            compile16(&code),
            Some(3),
            "{name} was admitted before this slice and must still be"
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The DATA-segment overrides must still be admitted with the gate OFF, and the CS one must not.
///
/// Catches a `prefixes_supported_for` edit that gated the whole segment-override admission behind
/// the new knob instead of only its CS clause. That would be invisible to every other fixture here
/// (they all run CS) and would quietly un-ship the rejected-row campaign's slice 6.
#[test]
fn the_gate_moves_only_the_cs_clause_of_the_prefix_admission() {
    for arm in [false, true] {
        select_v86_loop_rows(arm);
        for (name, prefix) in [("es:", 0x26u8), ("ds:", 0x3e), ("ss:", 0x36)] {
            assert_eq!(
                compile16(&[vec![prefix, 0x2b, 0x06], w(OPERAND)].concat()),
                Some(3),
                "the {name} override was admitted before this slice and must be on both arms"
            );
        }
        assert_eq!(
            compile16(&[vec![0x2e, 0x2b, 0x06], w(OPERAND)].concat()),
            if arm { Some(3) } else { None },
            "the cs: override is the clause this gate moves"
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// A protected-mode CS-based WRITE must STAY refused; a protected-mode CS-based READ is admitted.
///
/// This is the exact split `prefixes_supported_for`'s reversal claims, and it is enforced by
/// `segment_access_supported` rather than by the gate: its first line short-circuits `true` in real
/// mode and V86, and below that a code segment refuses `write` outright. Without this fixture the
/// reversal's whole safety argument would be a comment.
#[test]
fn a_protected_mode_cs_write_stays_refused_while_the_read_is_admitted() {
    select_v86_loop_rows(true);
    assert_eq!(
        compile32(&[vec![0x2e, 0x2b, 0x05], 0x3010u32.to_le_bytes().to_vec()].concat()),
        Some(3),
        "a protected-mode read through a readable CS is what the admission buys"
    );
    for (name, code) in [
        (
            "0xFF /1 DEC dword cs:[m]",
            [vec![0x2e, 0xff, 0x0d], 0x3010u32.to_le_bytes().to_vec()].concat(),
        ),
        (
            "0x89 MOV cs:[m],EAX",
            [vec![0x2e, 0x89, 0x05], 0x3010u32.to_le_bytes().to_vec()].concat(),
        ),
    ] {
        assert_eq!(
            compile32(&code),
            None,
            "{name} writes through a code segment in protected mode and must stay refused"
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// POP ES is REAL MODE and V86 only.
///
/// Catches the `stack_width_kind` arm being written for `LoadSegReal` alone. In protected mode a
/// segment load is a GDT/LDT fetch with type, privilege and present checks that can raise #GP or
/// #NP; the emitted form computes `base = selector << 4` and skips every one of them.
///
/// The second half is the width bar: at Dword the interpreter pops FOUR bytes and loads the low 16,
/// which is a different stack movement and a different bus charge, and `classify` refuses it.
#[test]
fn pop_segment_is_refused_outside_real_mode_and_at_dword() {
    select_v86_loop_rows(true);
    assert_eq!(
        compile16(&[0x07]),
        Some(3),
        "POP ES must be admitted in real mode, or the refusals below are vacuous"
    );
    assert_eq!(
        compile_leading_block_on(v86_cpu, false, &[0x07]),
        Some(3),
        "POP ES must be admitted in V86, which is the mode the census measured"
    );
    assert_eq!(
        compile32(&[0x07]),
        None,
        "POP ES in protected mode needs a descriptor fetch and must stay a barrier"
    );
    assert_eq!(
        compile16(&[0x66, 0x07]),
        None,
        "the Dword POP ES form pops four bytes and is not lowered"
    );
    jit::direct::set_v86_loop_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// The differential harness
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
    /// Written little-endian at each of the three segment bases plus `OPERAND`, so whichever
    /// segment the row names finds a value there.
    operand: u16,
    /// Pushed at SS:SP before the run, for the POP rows.
    stacked: u16,
    /// The stack pointer at run time. Its LOW half decides where `stacked` is written (SS is base 0
    /// here), and its HIGH half is poisoned so a 32-bit pointer advance is distinguishable.
    esp: u32,
    /// When set, ES and DS are given a STALE descriptor before the run: an unreal-mode limit, a
    /// code-segment access byte and `default_size_32` true, none of which a fresh real-mode segment
    /// carries.
    ///
    /// This is what makes each field of the segment write observable, and it is not decoration:
    /// with both roles starting from an already-real descriptor, an emitter that dropped the
    /// access/`default_size_32` store entirely would agree with the interpreter on every byte,
    /// because the value it failed to write was already there. That mutation SURVIVED the first
    /// version of this file.
    ///
    /// The limit is the opposite assertion: a real-mode load leaves the cached limit ALONE (that is
    /// what unreal mode IS), and the emitted form stores none, so the stale limit must SURVIVE the
    /// pop in real mode. In V86 the entry has already canonicalized every segment, so the fixture's
    /// V86 rows run without it.
    stale_segments: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            // 0xdead in every high half, so a lowering that writes 32 bits where the operand size
            // says 16 is a distinguishable failure. The low half is the register index.
            gpr: std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32)),
            eflags: 0x202,
            live_pending: false,
            operand: 0x1234,
            stacked: 0x0050,
            esp: STACK_ESP,
            stale_segments: false,
        }
    }

    fn gpr(mut self, index: usize, value: u32) -> Self {
        self.gpr[index] = value;
        self
    }

    fn flags(mut self, eflags: u32) -> Self {
        self.eflags = eflags;
        self
    }

    fn pending(mut self) -> Self {
        self.live_pending = true;
        self
    }

    fn operand(mut self, operand: u16) -> Self {
        self.operand = operand;
        self
    }

    fn esp(mut self, esp: u32) -> Self {
        self.esp = esp;
        self
    }

    fn stacked(mut self, stacked: u16) -> Self {
        self.stacked = stacked;
        self
    }

    fn stale_segments(mut self) -> Self {
        self.stale_segments = true;
        self
    }
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
}

/// Compile `FILL_A / body / FILL_B / hlt` on the native role, warm the same decode lines on the
/// interpreter role, and seed both identically.
fn build(builder: fn() -> CpuGsw, d: bool, body: &[u8], seed: Seed) -> Roles {
    let mut code = FILL_A.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // The operand, under every segment base a row here can name, plus the flat 32-bit fixture's.
    for base in [CS_BASE, DS_BASE, ES_BASE] {
        let at = (base + u32::from(OPERAND)) as usize;
        memory[at..at + 2].copy_from_slice(&seed.operand.to_le_bytes());
    }
    let stack_at = (seed.esp & 0xffff) as usize;
    memory[stack_at..stack_at + 2].copy_from_slice(&seed.stacked.to_le_bytes());

    let mut native = builder();
    let mut interp = builder();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.set_fast_map_enabled_for_test(true);
        for &linear in &[ENTRY, body_at, tail_at] {
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).expect("fixture decode");
        }
        for page in (0..0x1_0000u32).step_by(0x1000) {
            map_direct_page(cpu, bus, page);
        }
    }

    let compilation = match jit::direct::compile(&mut native, ENTRY, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the row under test is still a barrier")
        }
        jit::direct::CompileOutcome::Retry => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must cover all three slots, so the tested opcode really ran natively"
    );
    let key = jit::direct::key_for(&native, ENTRY, d).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = seed.gpr;
        cpu.registers.set_esp(seed.esp);
        let saved_eflags = cpu.registers.eflags & (FLAG_VM | (3 << 12));
        cpu.registers.eflags = seed.eflags | saved_eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
            // A descriptor produced BEFORE the tested instruction: the state that separates a
            // correct CF publish from a bare RBP write, and the one `emit_set_cf_only`'s two
            // branches split on.
            let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
        }
        if seed.stale_segments {
            for segment in [SegmentIndex::Es, SegmentIndex::Ds] {
                let mut stale = cpu.registers.segment(segment);
                stale.limit = 0xffff_ffff;
                stale.access = 0x9b;
                stale.default_size_32 = true;
                cpu.registers.set_segment(segment, stale);
            }
        }
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    Roles {
        native,
        native_bus,
        interp,
        interp_bus,
        block,
    }
}

fn compare_state(roles: &Roles, context: &str) {
    assert_eq!(
        roles.native.registers, roles.interp.registers,
        "{context}: registers (segment registers and EIP included)"
    );
    assert_eq!(
        roles.native.pending_flags, roles.interp.pending_flags,
        "{context}: raw lazy-flags descriptor"
    );
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: materialized EFLAGS"
    );
    assert_eq!(
        roles.native.halted, roles.interp.halted,
        "{context}: halt latch"
    );
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        roles.native.timing_rem, roles.interp.timing_rem,
        "{context}: timing remainder"
    );
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    // The WHOLE array, not a window. A store that widened, or that took the wrong segment base,
    // writes bytes the interpreter never touched, and a window sized to the intended access is
    // exactly the wrong shape to see that.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY: all three slots retire in the block and the whole architectural
/// state matches three interpreted steps.
fn lowered_on(builder: fn() -> CpuGsw, d: bool, body: &[u8], seed: Seed, context: &str) {
    let mut roles = build(builder, d, body, seed);
    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        3,
        "{context}: all three slots must retire natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
}

fn lowered16(body: &[u8], seed: Seed, context: &str) {
    lowered_on(sixteen_bit_cpu, false, body, seed, context);
}

// ---------------------------------------------------------------------------------------------
// 0x3D CMP AX,imm16 (ALU form 5 at Word)
// ---------------------------------------------------------------------------------------------

/// The whole of ALU form 5 at Word, against every flag-relevant operand pair.
///
/// Catches: an `AluImm` Word lane that wrote all 32 bits of the accumulator (the seed poisons every
/// high half with 0xdead); a descriptor tagged 0x200 instead of 0x100, which recomputes the lazy
/// flags at a 32-bit width and is invisible until a later reader; and a truncated immediate, since
/// `decode` zero-extends a Word immediate into the same `insn.imm` a Dword form fills.
///
/// CMP (`0x3d`) is the census row; the other five non-carry members ride the same arm and are here
/// because the closure rule put them on the allowlist together.
#[test]
fn alu_accumulator_immediate_forms_match_the_interpreter_at_word() {
    select_v86_loop_rows(true);
    for (name, opcode) in [
        ("add", 0x05u8),
        ("or", 0x0d),
        ("and", 0x25),
        ("sub", 0x2d),
        ("xor", 0x35),
        ("cmp", 0x3d),
    ] {
        for ax in [0x0000u32, 0x00b6, 0x00b7, 0x8000, 0xffff, 0x7fff] {
            for imm in [0x00b6u16, 0x0000, 0xffff, 0x8000] {
                for live_pending in [false, true] {
                    let seed = Seed::new().gpr(0, 0xdead_0000 | ax);
                    let seed = if live_pending { seed.pending() } else { seed };
                    lowered16(
                        &[vec![opcode], w(imm)].concat(),
                        seed,
                        &format!("{name} ax,{imm:#06x} ax={ax:#06x} pending={live_pending}"),
                    );
                }
            }
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The Dword form is UNCHANGED by the slice.
///
/// Catches a width field wired from the wrong source. `0x3d` at Dword has been lowered since before
/// this slice; if the form-5 arm started reading Word from somewhere it should not, this is what
/// notices.
#[test]
fn the_dword_accumulator_immediate_form_is_unchanged() {
    for arm in [false, true] {
        select_v86_loop_rows(arm);
        lowered_on(
            flat_protected_cpu,
            true,
            &[vec![0x3d], 0x1234_5678u32.to_le_bytes().to_vec()].concat(),
            Seed::new().gpr(0, 0x1234_5678),
            &format!("cmp eax,imm32 gate={arm}"),
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// 0xA1 / 0xA3 moffs, and the width trap they were kept off the allowlist for
// ---------------------------------------------------------------------------------------------

/// The moffs pair at Word, through EVERY segment a row can name and with no override at all.
///
/// Catches the trap this slice exists downstream of: both arms hard-coded `MemoryWidth::Dword`
/// until now, so a Word admission that reused them would move FOUR bytes where the guest moves
/// two. Two independent assertions see it -- the 0xdead high half on the load (`emit_write_gpr16`
/// merges, a 32-bit write clobbers) and the whole-RAM compare on the store.
///
/// It also catches an arm that dropped the segment override on the way to `DirectAddr`: the three
/// segment bases differ here, so a lowering that read DS where the guest reads ES lands on a
/// different linear address and a different byte.
#[test]
fn the_moffs_pair_matches_the_interpreter_at_word_through_every_segment() {
    select_v86_loop_rows(true);
    for (name, prefix) in [
        ("no override (ds)", None),
        ("es:", Some(0x26u8)),
        ("cs:", Some(0x2e)),
        ("ds:", Some(0x3e)),
        ("ss:", Some(0x36)),
    ] {
        for operand in [0x1234u16, 0x0000, 0xffff] {
            let mut load = prefix.map_or_else(Vec::new, |p| vec![p]);
            load.push(0xa1);
            load.extend_from_slice(&w(OPERAND));
            lowered16(
                &load,
                Seed::new().operand(operand),
                &format!("mov ax, {name}[{OPERAND:#06x}] operand={operand:#06x}"),
            );

            let mut store = prefix.map_or_else(Vec::new, |p| vec![p]);
            store.push(0xa3);
            store.extend_from_slice(&w(OPERAND));
            lowered16(
                &store,
                Seed::new().gpr(0, 0xdead_0000 | u32::from(operand)),
                &format!("mov {name}[{OPERAND:#06x}], ax value={operand:#06x}"),
            );
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The Dword moffs forms are UNCHANGED, on both arms.
///
/// This is the other half of the width work and the one a Word-only fixture cannot reach: the arms
/// now read `operand_width` instead of a literal, so a mis-wired field would narrow the 32-bit form
/// to two bytes. The seed's high halves and the whole-RAM compare are what see it.
#[test]
fn the_dword_moffs_pair_is_unchanged() {
    for arm in [false, true] {
        select_v86_loop_rows(arm);
        let at = 0x1010u32;
        lowered_on(
            flat_protected_cpu,
            true,
            &[vec![0xa1], at.to_le_bytes().to_vec()].concat(),
            Seed::new(),
            &format!("mov eax,[{at:#x}] gate={arm}"),
        );
        lowered_on(
            flat_protected_cpu,
            true,
            &[vec![0xa3], at.to_le_bytes().to_vec()].concat(),
            Seed::new().gpr(0, 0x1122_3344),
            &format!("mov [{at:#x}],eax gate={arm}"),
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// 0xF8 / 0xF9 CLC / STC
// ---------------------------------------------------------------------------------------------

/// CLC and STC against BOTH branches of `emit_set_cf_only`, both incoming CF polarities, and a
/// following reader.
///
/// Catches:
///
/// * an emitter that wrote the flag SHADOW alone -- the in-memory EFLAGS word stays stale for the
///   next reader that goes to memory rather than to the shadow;
/// * an emitter that wrote EFLAGS alone and left a LIVE descriptor in place -- CF is inside
///   `ARITH_FLAGS`, so the descriptor would recompute it from the pre-CLC operand pair at the next
///   reader and silently overwrite what the guest just set. `pending_flags` is compared as a RAW
///   descriptor, not just through `eflags()`;
/// * a polarity inversion, since both opcodes run against both incoming values;
/// * `emit_direction_flag`'s shape copied here, which would touch the wrong bit entirely.
///
/// The `adc ax, 0` row is the descriptor-consuming reader: it reads CF as an OPERAND, so a CF that
/// is right in EFLAGS but wrong in the descriptor produces a different AX.
#[test]
fn clc_and_stc_match_the_interpreter_in_every_descriptor_state() {
    select_v86_loop_rows(true);
    for (name, opcode) in [("clc", 0xf8u8), ("stc", 0xf9)] {
        for incoming in [0u32, FLAG_CF] {
            for live_pending in [false, true] {
                let seed = Seed::new().flags(0x202 | incoming);
                let seed = if live_pending { seed.pending() } else { seed };
                lowered16(
                    &[opcode],
                    seed,
                    &format!("{name} incoming_cf={incoming:#x} pending={live_pending}"),
                );
            }
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The carry the block just wrote, READ BACK by an instruction the block does not contain.
///
/// The fixture above compares the descriptor as a raw struct, which already catches a CF written to
/// EFLAGS while a live descriptor kept its own answer. This one closes the loop the way the guest
/// does: `adc ax, cx` consumes CF as an OPERAND, so a descriptor that disagrees with the published
/// EFLAGS produces a different AX, whichever of the two the reader happens to consult.
///
/// `adc` is refused as a lowering by the forms-1|3|5 guard, which is exactly what makes it a useful
/// tail: the block ends at CLC/STC and the SAME interpreter runs the consumer on both roles.
#[test]
fn the_carry_clc_and_stc_write_is_read_back_by_a_later_adc() {
    select_v86_loop_rows(true);
    for (name, opcode) in [("clc", 0xf8u8), ("stc", 0xf9)] {
        for incoming in [0u32, FLAG_CF] {
            for live_pending in [false, true] {
                let context =
                    format!("{name} incoming_cf={incoming:#x} pending={live_pending} then adc");
                // `mov si,si` / CLC-or-STC / `mov di,di` / `adc ax,cx` / hlt. The block covers
                // the first three; the compile walk refuses a block shorter than three slots, so
                // the trailing filler is what makes the two-instruction shape expressible at all.
                let code = [
                    FILL_A.to_vec(),
                    vec![opcode],
                    FILL_B.to_vec(),
                    vec![0x11, 0xc8, 0xf4],
                ]
                .concat();
                let mut memory = memory_fill();
                memory[(ENTRY - 1) as usize] = 0x90;
                memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

                let mut native = sixteen_bit_cpu();
                let mut interp = sixteen_bit_cpu();
                let mut native_bus = TestBus::with_memory(memory.clone());
                let mut interp_bus = TestBus::with_memory(memory);
                for bus in [&mut native_bus, &mut interp_bus] {
                    bus.direct_pages_enabled = true;
                    bus.direct_page_clocks = true;
                }
                for (cpu, bus) in [
                    (&mut native, &mut native_bus),
                    (&mut interp, &mut interp_bus),
                ] {
                    cpu.set_fast_map_enabled_for_test(true);
                    for &linear in &[ENTRY, ENTRY + 2, ENTRY + 3, ENTRY + 5] {
                        cpu.set_eip(linear);
                        cpu.begin_instruction();
                        cpu.fetch_decoded(bus, linear).expect("fixture decode");
                    }
                    map_direct_page(cpu, bus, 0x0000);
                    cpu.registers.gpr = std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32));
                    cpu.registers.set_esp(STACK_ESP);
                    cpu.registers.eflags = 0x202 | incoming;
                    cpu.pending_flags = PendingFlags::default();
                    if live_pending {
                        let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
                    }
                    cpu.set_eip(ENTRY);
                    cpu.elapsed_clocks = 0;
                    cpu.timing_rem = 0;
                    cpu.core_clocks_so_far = 0;
                }

                let compilation = match jit::direct::compile(&mut native, ENTRY, false) {
                    jit::direct::CompileOutcome::Compiled(compilation) => compilation,
                    _ => panic!("{context}: the two-slot block must compile"),
                };
                assert_eq!(
                    compilation.span.instructions, 3,
                    "{context}: the block must stop AT the adc, or the consumer ran natively too"
                );
                let key = jit::direct::key_for(&native, ENTRY, false).expect("entry key");
                assert!(matches!(
                    native.jit_direct.probe(key),
                    jit::direct::BlockProbe::Interpret
                ));
                let id = native
                    .jit_direct
                    .install(&compilation)
                    .expect("block installs");
                let block = native.jit_direct.block(id).expect("live block");
                native_bus.trace = BusTrace::default();
                interp_bus.trace = BusTrace::default();

                assert!(
                    native
                        .try_run_direct_block_for_test(&mut native_bus, block)
                        .unwrap(),
                    "{context}: block did not run natively"
                );
                // The consumer, interpreted on BOTH roles.
                native.cycle(&mut native_bus).unwrap();
                for _ in 0..4 {
                    interp.cycle(&mut interp_bus).unwrap();
                }
                let roles = Roles {
                    native,
                    native_bus,
                    interp,
                    interp_bus,
                    block,
                };
                compare_state(&roles, &context);
            }
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// 0x07 / 0x1F POP Sreg
// ---------------------------------------------------------------------------------------------

/// POP ES and POP DS in real mode and in V86, against several selectors.
///
/// Catches:
///
/// * a 32-bit pointer advance -- `alu_r16_imm16` preserves ESP's high half where `add r32` does
///   not, and the seed poisons it with 0xdead;
/// * a missing or wrong `access` / `default_size_32` store, both of which live in `SegmentRegister`
///   and are compared through `registers`;
/// * a base computed as anything other than `selector << 4`;
/// * a segment register written before the stack read's guards, which would show as a state
///   difference on the guarded row below.
///
/// The `stale_segments` row is what makes each stored FIELD observable, and it carries two
/// assertions at once. The access byte and `default_size_32` must be OVERWRITTEN with the real-mode
/// values, which an emitter that dropped that store would fail (that mutation survived the first
/// version of this file, because both roles already held a real-mode descriptor and the value it
/// failed to write was already there). The LIMIT must SURVIVE, because a real-mode segment load
/// leaves the cached limit alone -- that is what unreal mode is -- and a lowering that helpfully
/// wrote 0xFFFF would disagree with the interpreter.
#[test]
fn pop_segment_matches_the_interpreter_in_real_mode_and_v86() {
    select_v86_loop_rows(true);
    for (name, opcode) in [("pop es", 0x07u8), ("pop ds", 0x1f)] {
        for builder in [
            (sixteen_bit_cpu as fn() -> CpuGsw, "real"),
            (v86_cpu as fn() -> CpuGsw, "v86"),
        ] {
            for selector in [0x0000u16, 0x0040, 0xb800, 0xffff] {
                lowered_on(
                    builder.0,
                    false,
                    &[opcode],
                    Seed::new().stacked(selector),
                    &format!("{name} selector={selector:#06x} mode={}", builder.1),
                );
            }
        }
        lowered16(
            &[opcode],
            Seed::new().stacked(0x0040).stale_segments(),
            &format!("{name} over a stale unreal-mode descriptor"),
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// POP Sreg across the SIXTEEN-BIT STACK WRAP, which is the only state that separates a 16-bit
/// pointer advance from a 32-bit one.
///
/// Catches `alu_r16_imm16(0, home(4), 2)` widened to `add_r32_imm32(home(4), 2)`. THIS MUTATION
/// SURVIVED the first version of this file, and the reason is worth keeping: with SP anywhere below
/// 0xFFFE the two instructions produce the same 32-bit value, because the add never carries out of
/// bit 15. Poisoning ESP's high half was not enough on its own; the wrap is what makes the carry
/// happen, and only then does the widened form clobber bits 31..16.
///
/// The interpreter's 16-bit stack pointer wraps within SP (`memory.rs`'s `pop` advances at the
/// operand size), so the correct answer here is `0xdead_0000` and the mutation's is `0xdeae_0000`.
#[test]
fn pop_segment_preserves_the_high_half_of_esp_across_the_sixteen_bit_wrap() {
    select_v86_loop_rows(true);
    for (name, opcode) in [("pop es", 0x07u8), ("pop ds", 0x1f)] {
        for builder in [
            (sixteen_bit_cpu as fn() -> CpuGsw, "real"),
            (v86_cpu as fn() -> CpuGsw, "v86"),
        ] {
            lowered_on(
                builder.0,
                false,
                &[opcode],
                Seed::new().esp(0xdead_fffe).stacked(0x0040),
                &format!("{name} at SP=0xfffe mode={}", builder.1),
            );
        }
    }
    // The register pops share the same advance and the same wrap, so they are the control that says
    // the assertion is about the POINTER rather than about this one kind.
    lowered16(
        &[0x58],
        Seed::new().esp(0xdead_fffe).stacked(0x1234),
        "pop ax at SP=0xfffe",
    );
    jit::direct::set_v86_loop_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// The CS-override memory forms
// ---------------------------------------------------------------------------------------------

/// The four CS-override word-memory forms the loop uses, in real mode and in V86.
///
/// Catches a `DirectAddr` that lost the override on its way through `direct_addr`, which is the
/// whole mechanism the prefix gate is trusting: with CS at base 0 and DS at base 0x400 the two
/// answers are different linear addresses and different bytes, and the whole-RAM compare sees it.
///
/// The two WRITE rows are the ones the pre-slice comment on `prefixes_supported_for` believed could
/// not exist. They can, in V86 and real mode, and the fixture runs them there.
#[test]
fn the_cs_override_word_memory_forms_match_the_interpreter() {
    select_v86_loop_rows(true);
    let rows: Vec<(&str, Vec<u8>)> = vec![
        (
            "sub ax, cs:[m]",
            [vec![0x2e, 0x2b, 0x06], w(OPERAND)].concat(),
        ),
        (
            "dec word cs:[m]",
            [vec![0x2e, 0xff, 0x0e], w(OPERAND)].concat(),
        ),
        ("mov cs:[m], ax", [vec![0x2e, 0xa3], w(OPERAND)].concat()),
        (
            "mov word cs:[m], 0xffff",
            [vec![0x2e, 0xc7, 0x06], w(OPERAND), w(0xffff)].concat(),
        ),
        ("mov ax, cs:[m]", [vec![0x2e, 0xa1], w(OPERAND)].concat()),
    ];
    for (name, code) in rows {
        for builder in [
            (sixteen_bit_cpu as fn() -> CpuGsw, "real"),
            (v86_cpu as fn() -> CpuGsw, "v86"),
        ] {
            for operand in [0x0001u16, 0x0000, 0xffff, 0x8000] {
                lowered_on(
                    builder.0,
                    false,
                    &code,
                    Seed::new().operand(operand).gpr(0, 0xdead_00b6),
                    &format!("{name} operand={operand:#06x} mode={}", builder.1),
                );
            }
        }
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// The CERTAIN-EXIT rule
// ---------------------------------------------------------------------------------------------

/// A read-modify-write whose operand address is a compile-time constant that FAILS the alignment
/// guard must stay a BARRIER, not become a slot that exits on every execution.
///
/// This is the finding the design review blocked on, and it is the one class of defect the
/// differential rows above cannot see: a certain-exit slot is perfectly CORRECT -- the exit reports
/// zero completed work and the interpreter runs the instruction -- and it is strictly slower than
/// the rejected span it replaces, because a rejected span short-circuits through the decline memo
/// while this pays a dispatcher lookup, a segment check, a native entry, the address, the
/// page-cross bound, the alignment test and an exit stub first.
///
/// `emit_rmw_inc_dec` is one of the memory sites the one-lookup relaxation never reached: it guards
/// with `emit_wide_page_guard`, whose alignment test side-exits rather than falling into a split
/// charge. The tombraid row is exactly this shape -- `dec word cs:[0xf3]` resolves to linear
/// `0xc8113` -- and its block is two slots ending in a terminal `Jcc`, which the walk admits.
///
/// The ALIGNED counterpart must still compile, or the rule is a blanket refusal wearing a
/// measurement's clothes. And the two SERVED misaligned shapes must still compile: a misaligned
/// `Store` and a misaligned `AluMemSource` dispatch to relaxed lean sites that split the charge and
/// keep running, so refusing them would cost the slice two of its rows for nothing.
#[test]
fn a_statically_misaligned_read_modify_write_stays_a_barrier() {
    select_v86_loop_rows(true);
    // CS base is 0 in this fixture, so the operand's parity is the displacement's.
    let odd = OPERAND | 1;
    assert_eq!(
        compile16(&[vec![0x2e, 0xff, 0x0e], w(odd)].concat()),
        None,
        "dec word cs:[odd] would exit at that slot on every execution and must stay a barrier"
    );
    assert_eq!(
        compile16(&[vec![0xff, 0x0e], w(odd)].concat()),
        None,
        "the rule is about the ADDRESS, not the override: an unprefixed odd RMW is the same slot"
    );
    assert_eq!(
        compile16(&[vec![0x2e, 0xff, 0x0e], w(OPERAND)].concat()),
        Some(3),
        "the ALIGNED read-modify-write must still compile, or the rule is a blanket refusal"
    );
    // The two shapes that ARE served misaligned, through the relaxed lean sites.
    assert_eq!(
        compile16(&[vec![0x2e, 0x2b, 0x06], w(odd)].concat()),
        Some(3),
        "a misaligned AluMemSource is served by the relaxed lean read site and must not be refused"
    );
    assert_eq!(
        compile16(&[vec![0x2e, 0xa3], w(odd)].concat()),
        Some(3),
        "a misaligned moffs store is served by the relaxed lean store site and must not be refused"
    );
    // A register-relative address is NOT decidable here and must be left alone, whatever the
    // displacement's parity: the operand moves at run time.
    assert_eq!(
        compile16(&[vec![0x2e, 0xff, 0x4f], vec![0x01]].concat()),
        Some(3),
        "dec word cs:[bx+1] has a run-time address and is outside the rule"
    );
    // ...and with the gate OFF the rule must not fire at all: the aligned CS form is refused by the
    // prefix gate, and the unprefixed odd form keeps compiling exactly as it does on main.
    select_v86_loop_rows(false);
    assert_eq!(
        compile16(&[vec![0xff, 0x0e], w(odd)].concat()),
        Some(3),
        "the certain-exit rule must be inert on the off arm, or the A/B base is not main"
    );
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The REAL block shapes of loop A, which are four compile units rather than one walk.
///
/// `DirectKind::Jcc` is terminal, so the loop's conditional branches cut it into units, and the
/// census's own numbers agree: `0x2b cs:` carries ZERO unbound exits (it is interior) while `0x07`,
/// `0xa1` and `0xf8` each carry ~95M (each is an entry target).
///
/// This fixture is what says the slice actually forms those units, and it fails by SPAN LENGTH
/// naming the unit that is short rather than by a state difference. The `0xc901c` unit is expected
/// to stay refused: its only non-terminal slot is the statically misaligned read-modify-write above.
#[test]
fn the_loop_compiles_into_the_units_the_census_predicts() {
    select_v86_loop_rows(true);
    // Unit 1, entered at 0xc900e: mov ax,es:[m] / sub ax,cs:[m] / cmp ax,imm16 / jnb.
    let unit1 = [
        vec![0x26, 0xa1],
        w(OPERAND),
        vec![0x2e, 0x2b, 0x06],
        w(OPERAND),
        vec![0x3d],
        w(0x00b6),
        vec![0x73, 0x10],
    ]
    .concat();
    assert_eq!(
        compile_unit(&unit1),
        Some(4),
        "unit 1 must cover the load, the CS-override subtract, the compare and the branch"
    );
    // Unit 2, entered at 0xc901c: dec word cs:[m] / jne. REFUSED, by the certain-exit rule on the
    // misaligned operand the driver actually uses.
    assert_eq!(
        compile_unit(&[vec![0x2e, 0xff, 0x0e], w(OPERAND | 1), vec![0x75, 0x08]].concat()),
        None,
        "unit 2's read-modify-write is statically misaligned and the unit stays refused"
    );
    // The same unit with an ALIGNED operand does compile, which is what says the refusal above is
    // the operand's and not the shape's.
    assert_eq!(
        compile_unit(&[vec![0x2e, 0xff, 0x0e], w(OPERAND), vec![0x75, 0x08]].concat()),
        Some(2),
        "a two-slot unit ending in a terminal Jcc is admitted by the fewer-than-three rule"
    );
    // Unit 4, entered at 0xc9031: pop es / pop cx / pop bx / pop ax / clc, stopping before the ret
    // on MAX_BLOCK_STACK_ACCESSES.
    assert_eq!(
        compile_unit(&[0x07, 0x59, 0x5b, 0x58, 0xf8, 0xc3]),
        Some(5),
        "unit 4 must cover the segment pop, the three register pops and the clc"
    );
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// Compile `code` at `ENTRY` with no filler at all, so the span length is the unit's own shape.
fn compile_unit(code: &[u8]) -> Option<u8> {
    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    let mut block = code.to_vec();
    block.push(0xf4);
    memory[ENTRY as usize..ENTRY as usize + block.len()].copy_from_slice(&block);

    let mut cpu = v86_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    // Warm every byte offset that can start an instruction. Over-warming is harmless: a decode
    // line the walk never asks for costs nothing, and an unwarmed one stops the walk at `Retry`,
    // which reads as a refusal and would make every negative assertion here vacuous.
    for offset in 0..block.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x1_0000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The loop itself
// ---------------------------------------------------------------------------------------------

/// THE WHOLE LOOP BODY, as one block, against the interpreter.
///
/// This is the slice's own red demonstration: the bytes are the tombraid driver's hot body from
/// `0xc900e` (see `v86_loop_rows_enabled` for the probe), and with any one of the six rows missing
/// the block is shorter than the assertion below and the test fails by span length rather than by
/// state. It runs in V86, which is where the census measured it.
///
/// The `mov es, ax` head is deliberately NOT included: the compile walk's dirty-segment rule ends
/// a block at the first slot that pins a segment an earlier slot wrote, so a walk that starts
/// before it stops at the `mov ax, es:[m]` whatever this slice does. That is the row's own note in
/// the design, and the block the exits actually land on is this one.
#[test]
fn the_tombraid_loop_body_compiles_as_one_block_and_matches_the_interpreter() {
    select_v86_loop_rows(true);
    let body = [
        vec![0x26, 0xa1],
        w(OPERAND), // mov ax, es:[m]
        vec![0x2e, 0x2b, 0x06],
        w(OPERAND), // sub ax, cs:[m]
        vec![0x3d],
        w(0x00b6), // cmp ax, 0xb6
        vec![0x2e, 0xff, 0x0e],
        w(OPERAND), // dec word cs:[m]
        vec![0x2e, 0xc7, 0x06],
        w(OPERAND),
        w(0xffff),              // mov word cs:[m], 0xffff
        vec![0x07],             // pop es
        vec![0x59, 0x5b, 0x58], // pop cx / pop bx / pop ax
        vec![0xf8],             // clc
    ]
    .concat();

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    let mut code = body.clone();
    code.push(0xf4);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    for base in [CS_BASE, DS_BASE, ES_BASE] {
        let at = (base + u32::from(OPERAND)) as usize;
        memory[at..at + 2].copy_from_slice(&0x0100u16.to_le_bytes());
    }
    for (i, word) in [0x0040u16, 0x1111, 0x2222, 0x3333].into_iter().enumerate() {
        let at = STACK_SP as usize + i * 2;
        memory[at..at + 2].copy_from_slice(&word.to_le_bytes());
    }

    let mut native = v86_cpu();
    let mut interp = v86_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    // Every instruction start in the body, so the compile walk's `decode_cache.get` never misses.
    let mut starts = Vec::new();
    let mut at = ENTRY;
    let lengths = [4u32, 5, 3, 5, 7, 1, 1, 1, 1, 1];
    for len in lengths {
        starts.push(at);
        at += len;
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.set_fast_map_enabled_for_test(true);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).expect("fixture decode");
        }
        for page in (0..0x1_0000u32).step_by(0x1000) {
            map_direct_page(cpu, bus, page);
        }
        cpu.registers.gpr = std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32));
        cpu.registers.set_esp(STACK_ESP);
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }

    let compilation = match jit::direct::compile(&mut native, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("the loop body must compile as one block"),
    };
    assert_eq!(
        compilation.span.instructions,
        u8::try_from(lengths.len()).unwrap(),
        "every instruction of the loop body must be in the block; a short span names the row that \
         is still a barrier"
    );
    let key = jit::direct::key_for(&native, ENTRY, false).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    let before = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap(),
        "the loop-body block must run natively"
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - before,
        lengths.len() as u64,
        "every slot must retire natively"
    );
    for _ in 0..lengths.len() {
        interp.cycle(&mut interp_bus).unwrap();
    }
    let roles = Roles {
        native,
        native_bus,
        interp,
        interp_bus,
        block,
    };
    compare_state(&roles, "the whole tombraid loop body");
    jit::direct::set_v86_loop_rows_for_test(None);
}
