// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The FIVE dword rows of the tombraid FMV census's loop B, behind `IZARRAVM_FPU_LOOP_ROWS`.
//!
//! `dev_docs/tombraid-reprofile-2026-08-20.md` §4.1 and
//! `.bench/results/tombraid-reprofile-20260820/census-fmv-summary.json` between them name the
//! rows and their interpreted-hit counts over the 20e9 boot+FMV prefix:
//!
//! | row | form | hits | kind |
//! |---|---|---:|---|
//! | `0x9B` WAIT/FWAIT | none | 165,587,061 | `NativeX87Insn::Wait` |
//! | `0x9E` SAHF | none | 55,203,044 | `DirectKind::Sahf` |
//! | `0xD9 /7` rm=4 FRNDINT | register | 55,195,911 | `NativeX87Insn::RoundToInt` |
//! | `0xF7 /7` IDIV | memory | 27,602,949 | `DirectKind::DivMem` |
//! | `0x0F94 /0` SETE | memory | 27,602,402 | `DirectKind::SetCcMem` |
//!
//! **Every positive fixture here forces the arm through `set_fpu_loop_rows_for_test`.** The
//! shipped default is OFF, so a fixture that read the ambient knob would be testing the refusal
//! and calling it a lowering -- the mistake the count-lane slice's module doc records from the
//! other side of a default flip. The refusal fixtures force `Some(false)` for the same reason
//! rather than leaning on the default, so they keep meaning what they say after a flip.
//!
//! Each test's doc comment names WHAT IT CATCHES. The paired admission test
//! (`every_fpu_loop_row_flips_with_the_gate`) is also the red demonstration for the whole slice:
//! its `false` arm is the pre-slice world and its `true` arm fails against a tree with any one of
//! the five classify arms missing.

// MUTATION EVIDENCE (2026-08-19, applied by hand, run, restored). Each row names the fixture that
// caught it; a mutation nobody catches is a fixture bug, not a free pass. The two x87 rows are
// caught by fixtures in `cpu_jit_x87_direct_test.rs` and are listed here so the slice's whole
// record is in one place.
//
// | mutation | caught by |
// |---|---|
// | `emit_sahf` drops `emit_clear_pending` | `sahf_matches_the_interpreter_across_ah_descriptor_and_overflow_state` |
// | `SAHF_MASK` widened to include `FLAG_OF` | same |
// | `DirectKind::Sahf` raw clocks 3 -> 2 (the `_ => 2` default) | same |
// | `SetCcMem` stores at `MemoryWidth::Dword` | `setcc_memory_matches_...` AND `setcc_memory_exits_before_the_store_...` |
// | `e.setcc(condition ^ 1, ..)` (condition inverted) | `setcc_memory_matches_the_interpreter_for_every_condition` |
// | `emit_div_mem`'s `emit_mode13_read_completion` moved ABOVE the guards | `a_mode13_divide_guard_exit_deposits_no_read` AND `div_and_idiv_memory_match_...` |
// | the memory divisor's `movsxd` dropped for IDIV | `div_and_idiv_memory_match_the_interpreter_across_the_guard_classes` |
// | `NativeX87Insn::Wait` metadata `FpOpClass::Wait` -> `Register` | `wait_retires_natively_...` AND `wait_delivers_...` (x87 file) |
// | `RoundToInt`'s RC compare chain never matches (baked nearest) | `frndint_matches_the_interpreter_under_every_rounding_mode` (x87 file) |
// | `RoundToInt` drops `emit_store_physical` | same |
//
// One finding worth keeping. The mode-13 completion mutation fails FIRST inside `run.rs` on
// `debug_assert!(exit.mode13_dword_reads <= dword_reads)` rather than on any comparison this file
// makes -- a guard exit reports zero completed dword reads for a slot that deposited one. A
// release build would see it only through the bus-timing equality, which is why `finish` asserts
// that too rather than settling for the register comparison.

use super::*;

const ENTRY: u32 = 0x501;
const RAM_TARGET: u32 = 0x3000;
const MODE13_TARGET: u32 = 0x000a_1000;
/// The byte pattern seeded around every memory target. A `SetCcMem` that stored four bytes, or a
/// `DivMem` that wrote where it should only read, shows up as one of these disappearing.
const POISON: [u8; 8] = [0xa5, 0x5a, 0xc3, 0x3c, 0xf0, 0x0f, 0x99, 0x66];

/// Select the arm for this thread and PROVE the selection took, the shape
/// `cpu_jit_test_imm_test.rs`'s `select_rotate_rows` uses.
fn select_fpu_loop_rows(enabled: bool) {
    jit::direct::set_fpu_loop_rows_for_test(Some(enabled));
    assert_eq!(
        jit::direct::fpu_loop_rows_enabled(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_FPU_LOOP_ROWS"
    );
}

fn flat_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_mode(mode);
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

/// Real mode with a SIXTEEN-BIT code segment (CS.D = 0), which is what `flat_cpu` deliberately is
/// not. Every instruction decodes at `OperandSize::Word` there, prefix or no prefix, which is the
/// half of the width bar a prefix test cannot reach.
fn sixteen_bit_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
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

fn map_direct_page(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, physical: u32) {
    let permissions = jit::fast_map::PagePermissions::UNPAGED;
    let read = bus
        .direct_page(physical, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        linear,
        physical,
        read,
        permissions,
        cpu.physical_page_watched(physical)
    ));
    let write = bus
        .direct_page(physical, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        linear,
        physical,
        write,
        permissions,
        cpu.physical_page_watched(physical)
    ));
}

/// Compile `code` as slot 0 of a THREE-slot block and report the span length, or `None` when the
/// walk refused it.
///
/// The three-slot shape is `cpu_jit_test_imm_test.rs`'s and is load-bearing for the same reason it
/// is there: warming only the entry decode line makes slot 1 miss, the walk stops at `Retry`, and
/// the fewer-than-three-slots gate reports the same `None` as a genuine reject -- so a negative
/// assertion on an unwarmed shape passes whether or not the opcode is lowered.
fn compile_leading_block_on(cpu_builder: fn() -> CpuGsw, code: &[u8]) -> Option<u8> {
    let mut memory = vec![0; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    let mut block = code.to_vec();
    block.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + block.len()].copy_from_slice(&block);
    let mut cpu = cpu_builder();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let starts = [
        ENTRY,
        ENTRY + code.len() as u32,
        ENTRY + code.len() as u32 + 2,
    ];
    decode_fixture(&mut cpu, &mut bus, &starts);
    // Unconditional, and not only for the memory rows: with `RAM_TARGET`'s page absent from the
    // fast map EVERY memory kind is refused, `mov eax, [disp32]` included, so a negative
    // assertion made without it would pass for the harness's reason rather than the row's. That
    // is the "fixture that cannot fail" trap in its exact local form -- the first draft of this
    // file asserted the Word-size refusal of two memory rows against a CPU that would have
    // refused their DWORD forms too.
    map_direct_page(&mut cpu, &mut bus, RAM_TARGET, RAM_TARGET);
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    outcome
        .is_some()
        .then(|| outcome.unwrap().span.instructions)
}

fn flat_586() -> CpuGsw {
    flat_cpu(GswMode::Gsw586)
}

fn compile_leading_block(code: &[u8]) -> Option<u8> {
    compile_leading_block_on(flat_586, code)
}

/// The five rows as they appear in the census, each with the operand shape its row reports.
///
/// `0xF7 /7` and `0x0F94 /0` use a bare disp32 (`mod = 00, rm = 101`), the addressing form the
/// FMV loop's own sites use and the one `direct_addr` lowers with no base or index register.
fn fpu_loop_row_encodings() -> [(&'static str, Vec<u8>); 5] {
    [
        ("0x9B WAIT", vec![0x9b]),
        ("0x9E SAHF", vec![0x9e]),
        ("0xD9 FC FRNDINT", vec![0xd9, 0xfc]),
        (
            "0xF7 /7 IDIV dword [disp32]",
            [vec![0xf7, 0x3d], RAM_TARGET.to_le_bytes().to_vec()].concat(),
        ),
        (
            "0x0F94 /0 SETE byte [disp32]",
            [vec![0x0f, 0x94, 0x05], RAM_TARGET.to_le_bytes().to_vec()].concat(),
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// The gate itself
// ---------------------------------------------------------------------------------------------

/// THE DEFAULT PIN, and it is the one assertion that decides whether this slice is inert.
///
/// Catches: a flip of `parse_fpu_loop_rows_arm`'s `NotPresent` arm. The slice ships OFF and the ON
/// arm is a separate decision priced by a wall ladder; a default that moved without that ladder
/// would change every shipped binary's admission silently.
///
/// It reads the AMBIENT knob deliberately -- no override -- so it also fails if
/// `IZARRAVM_FPU_LOOP_ROWS=1` is exported in the environment running the suite, which is the
/// correct outcome: the rest of this file states its arm, and a fixture that means to pin the
/// shipped default has nowhere else to read it from.
#[test]
fn fpu_loop_rows_ships_off_by_default() {
    jit::direct::set_fpu_loop_rows_for_test(None);
    assert!(
        !jit::direct::fpu_loop_rows_enabled(),
        "IZARRAVM_FPU_LOOP_ROWS must default OFF in this slice; see fpu_loop_rows_enabled for \
         why the flip is a separate, ladder-priced decision"
    );
}

/// The spelling table, both arms and the refusal.
///
/// Catches: a `_ => false` fallthrough replacing the panic. A mistyped ladder leg
/// (`IZARRAVM_FPU_LOOP_ROWS=yes`) that fell through would run exactly what an unset environment
/// runs and be read as "the arm I asked for changed nothing", which is the single wrong conclusion
/// an arm ladder exists to avoid.
#[test]
fn fpu_loop_rows_spelling_table_names_both_arms() {
    use std::env::VarError;
    assert!(!jit::direct::parse_fpu_loop_rows_arm_for_test(Err(
        VarError::NotPresent
    )));
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(
            !jit::direct::parse_fpu_loop_rows_arm_for_test(Ok(off.to_string())),
            "{off:?} must name the off arm"
        );
    }
    for on in ["1", "on", "ON", " on ", "On"] {
        assert!(
            jit::direct::parse_fpu_loop_rows_arm_for_test(Ok(on.to_string())),
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "fpu", "rows"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_fpu_loop_rows_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_FPU_LOOP_ROWS={typo:?} names no arm and must panic rather than silently \
             running the default"
        );
    }
}

/// EVERY row flips with the gate, and NOTHING ELSE DOES.
///
/// Catches, in the `false` direction: an arm that forgot its `fpu_loop_rows_enabled()` guard, i.e.
/// a row that ships admitted while the knob says off -- which would make the gate-off census leg
/// disagree with main and destroy the A/B base.
///
/// Catches, in the `true` direction: a missing or wrongly-keyed classify arm. This is the red
/// demonstration for the slice: against a tree without the five arms the `true` half fails five
/// times, one per row.
///
/// The control row is a plain `mov eax, ecx`, which must compile on BOTH arms: it proves the
/// three-slot harness is not simply refusing everything, which is how this test would go vacuous.
#[test]
fn every_fpu_loop_row_flips_with_the_gate() {
    select_fpu_loop_rows(false);
    for (name, code) in fpu_loop_row_encodings() {
        assert_eq!(
            compile_leading_block(&code),
            None,
            "{name} must stay a hard boundary with IZARRAVM_FPU_LOOP_ROWS off"
        );
    }
    assert_eq!(
        compile_leading_block(&[0x89, 0xc8]),
        Some(3),
        "the control row must compile on the off arm, or this fixture cannot fail"
    );

    select_fpu_loop_rows(true);
    for (name, code) in fpu_loop_row_encodings() {
        assert_eq!(
            compile_leading_block(&code),
            Some(3),
            "{name} must be admitted with IZARRAVM_FPU_LOOP_ROWS on"
        );
    }
    select_fpu_loop_rows(false);
}

// ---------------------------------------------------------------------------------------------
// The width bars
// ---------------------------------------------------------------------------------------------

/// THE WIDTH BAR, both directions, for every row that has one.
///
/// Catches the failure the count-lane slice shipped in the mirror image: barring an admission on
/// the ABSENCE OF A PREFIX rather than on the KIND's own width, which panicked the compiler on an
/// unprefixed word form in a 16-bit segment. Both halves are asserted here:
///
/// * a 66-prefixed encoding in a 32-bit segment (`OperandSize::Word`, prefix mask 1), and
/// * an UNPREFIXED encoding in a CS.D = 0 segment (`OperandSize::Word`, prefix mask 0).
///
/// Both must be REFUSED, and neither may panic -- `compile_leading_block` runs the whole compile
/// walk, so a `unreachable!` or an `expect` reached through a Word path fails this test loudly
/// rather than in production.
///
/// The gate is forced ON throughout: with it off the rows are refused for the wrong reason and the
/// assertion would pass vacuously.
#[test]
fn every_fpu_loop_row_stays_a_barrier_at_word_operand_size() {
    select_fpu_loop_rows(true);
    // 16-bit code segment, unprefixed. The whole row set, including the two that carry no ModRM.
    for (name, code) in fpu_loop_row_encodings() {
        // The disp32 memory rows re-encode with a word displacement in a 16-bit segment, so use
        // the register-adjacent `[BX+SI]` form (mod = 00, rm = 00) instead: what is under test is
        // the operand SIZE, not the address form.
        let code = match code.first() {
            Some(0xf7) => vec![0xf7, 0x38],
            Some(0x0f) => vec![0x0f, 0x94, 0x00],
            _ => code,
        };
        assert_eq!(
            compile_leading_block_on(sixteen_bit_cpu, &code),
            None,
            "{name} unprefixed in a CS.D = 0 segment decodes at Word and must stay a barrier"
        );
    }
    // 32-bit code segment, 66-prefixed. WAIT and FRNDINT are covered by the FPU branch's blanket
    // `operand_size != Dword` refusal; SAHF, IDIV and SETcc by the Word allowlist.
    for (name, code) in [
        ("66 9B", vec![0x66, 0x9b]),
        ("66 9E", vec![0x66, 0x9e]),
        ("66 D9 FC", vec![0x66, 0xd9, 0xfc]),
        (
            "66 F7 /7 mem",
            [vec![0x66, 0xf7, 0x3d], RAM_TARGET.to_le_bytes().to_vec()].concat(),
        ),
        (
            "66 0F94 /0 mem",
            [
                vec![0x66, 0x0f, 0x94, 0x05],
                RAM_TARGET.to_le_bytes().to_vec(),
            ]
            .concat(),
        ),
    ] {
        assert_eq!(
            compile_leading_block(&code),
            None,
            "{name} decodes at Word in a 32-bit segment and must stay a barrier"
        );
    }
    select_fpu_loop_rows(false);
}

/// The neighbours this slice must NOT have swept in.
///
/// Catches a widened classify arm. Each of these is one bit away from an admitted row and each
/// would be a miscompile rather than a missed lowering:
///
/// * `0x9F` LAHF -- SAHF's sibling in the same interpreter arm, writing AH from the flag byte. No
///   emitter, no census row.
/// * `0xD9 F8` FPREM, `0xD9 FE` FSIN -- two of the six `0xD9 /7` register encodings that stay
///   rejected. They share the ModRM `reg` field with FRNDINT and differ only in `rm`.
/// * `0xF7 /4` MUL memory and `0xF7 /5` IMUL-accumulator memory -- `DivMem`'s neighbours in the
///   group-3 arm. `/4`'s register arm returns from `classify` rather than from the match, so a
///   memory `/4` was already unreachable; `/5`'s memory form was already admitted and must stay
///   admitted, which is what its entry in the ADMITTED list below pins.
#[test]
fn the_gate_does_not_sweep_in_the_neighbouring_encodings() {
    select_fpu_loop_rows(true);
    for (name, code) in [
        ("0x9F LAHF", vec![0x9f]),
        ("0xD9 F8 FPREM", vec![0xd9, 0xf8]),
        ("0xD9 FE FSIN", vec![0xd9, 0xfe]),
        (
            "0xF7 /4 MUL dword [disp32]",
            [vec![0xf7, 0x25], RAM_TARGET.to_le_bytes().to_vec()].concat(),
        ),
    ] {
        assert_eq!(
            compile_leading_block(&code),
            None,
            "{name} is not one of the five rows and must stay a hard boundary"
        );
    }
    // ...and the two that WERE already admitted and must not have been disturbed.
    for (name, code) in [
        ("0xD9 FA FSQRT", vec![0xd9, 0xfa]),
        (
            "0xF7 /5 IMUL dword [disp32]",
            [vec![0xf7, 0x2d], RAM_TARGET.to_le_bytes().to_vec()].concat(),
        ),
    ] {
        assert_eq!(
            compile_leading_block(&code),
            Some(3),
            "{name} was admitted before this slice and must still be"
        );
    }
    select_fpu_loop_rows(false);
}

// ---------------------------------------------------------------------------------------------
// The differential harness for the three INTEGER rows
//
// WAIT and FRNDINT are x87 slots and are covered in `cpu_jit_x87_direct_test.rs`, beside the
// FSQRT and control-word rows they share an emitter file with; that file's
// `assert_program_matches_exact_insns` already carries the x87 state, TOP and fp_rem comparisons
// this harness has no way to make.
// ---------------------------------------------------------------------------------------------

/// A three-slot block with the row under test as slot ONE, never slot zero.
///
/// Slot 0 is a flag-neutral `mov edi, edi`. It is not padding: a guarded row placed at block entry
/// side-exits having retired NOTHING, so `jit_direct_insns` does not move and an assertion on
/// "retired > 0" cannot tell a guard exit from a block that never ran. With the row at slot 1 a
/// guard exit retires exactly one instruction, which is a positive observation.
struct Fixture {
    native: CpuGsw,
    interpreter: CpuGsw,
    native_bus: TestBus,
    interpreter_bus: TestBus,
    block: jit::direct::CompiledBlock,
    slots: u8,
}

fn install_block(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, linear, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, linear, true).expect("the fixture block compiles");
    assert_eq!(
        compilation.span.instructions, 3,
        "the fixture block must lower all three slots, or the row under test never ran natively"
    );
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("the fixture block installs");
    cpu.jit_direct.block(id).unwrap()
}

/// `row` is the instruction under test; it is framed by two flag-neutral register moves.
fn prepare(row: &[u8], target: u32, arm: &dyn Fn(&mut CpuGsw)) -> Fixture {
    let mut code = vec![0x89, 0xff]; // mov edi,edi
    code.extend_from_slice(row);
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]); // mov esi,esi ; hlt
    let mut pristine = vec![0; (target as usize + 0x2000).max(0x5000)];
    pristine[(ENTRY - 1) as usize] = 0x90;
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    pristine[target as usize..target as usize + POISON.len()].copy_from_slice(&POISON);

    let mut native = flat_cpu(GswMode::Gsw586);
    let mut interpreter = flat_cpu(GswMode::Gsw586);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + 2,
        ENTRY + 2 + row.len() as u32,
        ENTRY + 4 + row.len() as u32,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts[..3]);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts[..3]);
    map_direct_page(&mut native, &mut native_bus, target, target);
    let block = install_block(&mut native, ENTRY);
    arm(&mut native);
    arm(&mut interpreter);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
        slots: 3,
    }
}

/// Run the native block once, run the interpreter `expect_retired` instructions, and compare
/// everything the campaign compares.
///
/// `expect_retired` is the whole guard contract in one number. It is 3 when the row runs
/// natively and 1 when the row's guard exits -- one for the leading `mov edi, edi` and nothing
/// for the row itself, which is what "the exit leaves the instruction un-started" means in
/// counters. The interpreter is stepped the SAME number of instructions, so a comparison after a
/// guard exit is still a comparison of the same guest program point.
fn finish(mut fixture: Fixture, expect_retired: u8, context: &str) -> Fixture {
    let before = fixture.native.perf_counters().jit_direct_insns;
    let side_exits_before = fixture.native.perf_counters().jit_direct_side_exits;
    fixture
        .native
        .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
        .unwrap();
    for _ in 0..expect_retired {
        fixture
            .interpreter
            .cycle(&mut fixture.interpreter_bus)
            .unwrap();
    }

    let retired = fixture.native.perf_counters().jit_direct_insns - before;
    assert_eq!(
        retired,
        u64::from(expect_retired),
        "native retirement differs: {context}"
    );
    let side_exits = fixture.native.perf_counters().jit_direct_side_exits - side_exits_before;
    if expect_retired < fixture.slots {
        assert_eq!(side_exits, 1, "expected exactly one side exit: {context}");
    }
    assert_eq!(
        fixture.native.registers, fixture.interpreter.registers,
        "registers differ: {context}"
    );
    assert_eq!(
        fixture.native.pending_flags, fixture.interpreter.pending_flags,
        "the raw lazy-flags descriptor differs: {context}"
    );
    assert_eq!(
        fixture.native.eflags(),
        fixture.interpreter.eflags(),
        "materialized EFLAGS differ: {context}"
    );
    assert_eq!(
        fixture.native_bus.memory, fixture.interpreter_bus.memory,
        "memory differs: {context}"
    );
    assert_eq!(
        fixture.native.elapsed_clocks, fixture.interpreter.elapsed_clocks,
        "clock charge differs: {context}"
    );
    assert_eq!(
        fixture.native.timing_rem, fixture.interpreter.timing_rem,
        "timing remainder differs: {context}"
    );
    assert_eq!(
        fixture.native_bus.trace.elapsed_clocks(),
        fixture.interpreter_bus.trace.elapsed_clocks(),
        "bus timing differs, which is where a double-counted mode-13 read shows up: {context}"
    );
    assert_eq!(
        fixture.native_bus.mode13_dirty_pages, fixture.interpreter_bus.mode13_dirty_pages,
        "Mode13 dirty pages differ: {context}"
    );
    fixture
}

fn base_arm(cpu: &mut CpuGsw, eflags: u32, live_descriptor: bool) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(0xc000);
    cpu.registers.eflags = eflags;
    cpu.pending_flags = PendingFlags::default();
    if live_descriptor {
        // One deferred ALU, exactly as `cpu_jit_double_shift_test.rs` arms one: the descriptor is
        // still LIVE at block entry, which is the state that separates a correct publish from a
        // bare RBP write.
        let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

// ---------------------------------------------------------------------------------------------
// 0x9E SAHF
// ---------------------------------------------------------------------------------------------

/// SAHF across every AH bit pattern that matters, both descriptor states, and both polarities of
/// the flag it must NOT touch.
///
/// Catches four separate mistakes, each of which passes a naive "does CF come out right" check:
///
/// * **A missing `emit_clear_pending`.** With a live descriptor the five loaded bits would be
///   recomputed from the pre-SAHF operand pair at the next reader, silently overwriting what the
///   guest just loaded. `pending_flags` is compared as a RAW descriptor, not just through
///   `eflags()`, so this fails even when the materialized word happens to agree.
/// * **A mask widened to `ARITH_FLAGS`.** That clears OF whenever AH's bit 11 is clear -- and AH
///   has no bit 11. The `of` loop is what sees it: the 0x00 and 0xff AH rows both keep OF.
/// * **A missing publish to `CpuGsw.eflags`.** RBP alone leaves the in-memory word stale for the
///   next reader that goes to memory rather than to the shadow.
/// * **Reading AL instead of AH.** Every AH pattern here differs from the AL byte beneath it.
#[test]
fn sahf_matches_the_interpreter_across_ah_descriptor_and_overflow_state() {
    select_fpu_loop_rows(true);
    for ah in [0x00u32, 0xff, 0xd5, 0x2a, 0x55, 0xaa, 0x10, 0x80] {
        for live_descriptor in [false, true] {
            for of in [0, crate::FLAG_OF] {
                let context = format!("ah={ah:#04x} live_descriptor={live_descriptor} of={of:#x}");
                let eflags = 0x2 | of | crate::FLAG_DF | crate::FLAG_IF;
                let eax = (ah << 8) | 0x5c;
                finish(
                    prepare(&[0x9e], RAM_TARGET, &move |cpu: &mut CpuGsw| {
                        base_arm(cpu, eflags, live_descriptor);
                        cpu.registers.set_eax(eax);
                    }),
                    3,
                    &context,
                );
            }
        }
    }
    select_fpu_loop_rows(false);
}

// ---------------------------------------------------------------------------------------------
// 0x0F9x SETcc m8
// ---------------------------------------------------------------------------------------------

fn setcc_mem(condition: u8, target: u32) -> Vec<u8> {
    [
        vec![0x0f, 0x90 | condition, 0x05],
        target.to_le_bytes().to_vec(),
    ]
    .concat()
}

/// All SIXTEEN conditions against several flag words, in RAM and in the Mode-13 aperture.
///
/// Catches:
///
/// * a condition code translated on its way to `Encoder::setcc` (the register form's own tests
///   would not see a memory-only mistranslation);
/// * a store WIDER than one byte -- `POISON` fills the seven bytes after the target and the whole
///   memory image is compared;
/// * the value being computed inside `emit_store`'s scratch instead of parked, which produces
///   whatever the address path left in RDX rather than the condition;
/// * a Mode-13 write not registering its dirty page or its dword count, which shows up in the
///   bus timing comparison and in `mode13_dirty_pages`.
#[test]
fn setcc_memory_matches_the_interpreter_for_every_condition() {
    select_fpu_loop_rows(true);
    for target in [RAM_TARGET, MODE13_TARGET] {
        for eflags in [
            0x2,
            0x2 | crate::FLAG_ZF,
            0x2 | crate::FLAG_CF | crate::FLAG_SF,
            0x2 | crate::FLAG_OF | crate::FLAG_PF | crate::FLAG_AF,
            0x2 | crate::FLAG_ZF | crate::FLAG_SF | crate::FLAG_OF,
        ] {
            for condition in 0u8..16 {
                for live_descriptor in [false, true] {
                    let context = format!(
                        "condition={condition:#x} eflags={eflags:#x} target={target:#x} \
                         live_descriptor={live_descriptor}"
                    );
                    finish(
                        prepare(
                            &setcc_mem(condition, target),
                            target,
                            &move |cpu: &mut CpuGsw| base_arm(cpu, eflags, live_descriptor),
                        ),
                        3,
                        &context,
                    );
                }
            }
        }
    }
    select_fpu_loop_rows(false);
}

/// The SETcc byte must survive a MEMORY GUARD on the store, and the guard must fire before it.
///
/// Catches a store whose guard was placed after the value was committed. The target straddles a
/// page boundary into an unmapped page, so the wide-page guard exits; the row must retire nothing
/// and leave memory untouched.
#[test]
fn setcc_memory_exits_before_the_store_when_the_page_guard_fires() {
    select_fpu_loop_rows(true);
    // One byte inside the last mapped page, but on a page the fixture never mapped.
    let unmapped = RAM_TARGET + 0x1000;
    let fixture = prepare(&setcc_mem(4, unmapped), RAM_TARGET, &|cpu: &mut CpuGsw| {
        base_arm(cpu, 0x2 | crate::FLAG_ZF, false)
    });
    finish(fixture, 1, "SETcc into an unmapped page");
    select_fpu_loop_rows(false);
}

// ---------------------------------------------------------------------------------------------
// 0xF7 /6 and /7 DIV / IDIV m32
// ---------------------------------------------------------------------------------------------

fn div_mem(signed: bool, target: u32) -> Vec<u8> {
    let modrm = if signed { 0x3d } else { 0x35 };
    [vec![0xf7, modrm], target.to_le_bytes().to_vec()].concat()
}

fn seed_divisor(cpu: &mut CpuGsw, eax: u32, edx: u32) {
    base_arm(cpu, 0x2 | crate::FLAG_CF | crate::FLAG_ZF, true);
    cpu.registers.set_eax(eax);
    cpu.registers.set_edx(edx);
}

/// Write the divisor into both buses' memory images before the run.
fn with_divisor(fixture: &mut Fixture, target: u32, divisor: u32) {
    for bus in [&mut fixture.native_bus, &mut fixture.interpreter_bus] {
        bus.memory[target as usize..target as usize + 4].copy_from_slice(&divisor.to_le_bytes());
    }
}

fn run_div_mem(
    signed: bool,
    target: u32,
    eax: u32,
    edx: u32,
    divisor: u32,
    expect_retired: u8,
    context: &str,
) -> Fixture {
    let mut fixture = prepare(
        &div_mem(signed, target),
        target,
        &move |cpu: &mut CpuGsw| seed_divisor(cpu, eax, edx),
    );
    with_divisor(&mut fixture, target, divisor);
    finish(fixture, expect_retired, context)
}

/// The whole DIV/IDIV memory matrix: both sub-opcodes, both page kinds, and every operand class
/// the guards split on.
///
/// The `expect_retired` column IS the guard contract:
///
/// * 3 -- the divide ran natively;
/// * 1 -- a guard exited with the instruction un-started, and the interpreter (stepped one
///   instruction, i.e. only the leading `mov edi, edi`) agrees on every register, flag and byte.
///
/// Catches: a guard that admits a faulting divide (the host would fault inside emitted code, which
/// is not a recoverable state); a guard that exits AFTER writing EAX or EDX; the signed form's
/// 64-bit assembly done in the wrong order (`cqo` before the high half is consumed); and the
/// unsigned `cmp edx, divisor` written as a signed compare.
#[test]
fn div_and_idiv_memory_match_the_interpreter_across_the_guard_classes() {
    select_fpu_loop_rows(true);
    for target in [RAM_TARGET, MODE13_TARGET] {
        // Unsigned DIV (/6). The guard is `edx >= divisor`, which subsumes the zero divisor.
        for (eax, edx, divisor, retired, name) in [
            (17u32, 0u32, 5u32, 3u8, "17/5"),
            (0xffff_ffff, 0, 1, 3, "max/1"),
            (0, 0, 7, 3, "0/7"),
            (17, 0, 0, 1, "divide by zero"),
            (0, 1, 1, 1, "quotient overflow"),
            (0, 5, 5, 1, "edx == divisor"),
            (
                0xffff_ffff,
                0xffff_fffe,
                0xffff_ffff,
                3,
                "just inside the range",
            ),
        ] {
            let context = format!("DIV {name} target={target:#x}");
            run_div_mem(false, target, eax, edx, divisor, retired, &context);
        }
        // Signed IDIV (/7). Three guards: zero, minus one (conservative), and the post-divide
        // i32 range check.
        for (eax, edx, divisor, retired, name) in [
            (17u32, 0u32, 5u32, 3u8, "17/5"),
            (0xffff_ffef, 0xffff_ffff, 5, 3, "-17/5"),
            (17, 0, 0xffff_fffb, 3, "17/-5"),
            (0xffff_ffef, 0xffff_ffff, 0xffff_fffb, 3, "-17/-5"),
            (17, 0, 0, 1, "divide by zero"),
            (17, 0, 0xffff_ffff, 1, "divisor -1, legal but refused"),
            (0, 0x8000_0000, 0xffff_ffff, 1, "i64::MIN / -1"),
            (0, 1, 1, 1, "quotient overflow"),
            (0x8000_0000, 0xffff_ffff, 1, 3, "i32::MIN / 1"),
        ] {
            let context = format!("IDIV {name} target={target:#x}");
            run_div_mem(true, target, eax, edx, divisor, retired, &context);
        }
    }
    select_fpu_loop_rows(false);
}

/// THE DEFERRED MODE-13 COMPLETION, which is the one thing this row's emitter exists to get right.
///
/// `emit_ram_read_pointer` deposits `mode13_dword_reads` into the frame before it returns, and
/// `emit_return` copies that lane out on every exit. A divide-guard exit taken after the deposit
/// therefore charges one guest read TWICE -- once natively and once when the interpreter
/// re-executes the instruction whole.
///
/// Catches it in two independent ways, and the first fires even in a build that ignores bus
/// timing: `run.rs`'s `debug_assert!(exit.mode13_dword_reads <= dword_reads)` panics outright,
/// because a guard exit reports `completed_dword_reads = 0` for a slot that deposited one. The
/// second is the bus-timing equality inside `finish`, which is what a release build would see.
///
/// Swapping `emit_div_mem`'s `emit_mode13_read_completion` back above the guards is the mutation
/// this test is written against.
#[test]
fn a_mode13_divide_guard_exit_deposits_no_read() {
    select_fpu_loop_rows(true);
    for signed in [false, true] {
        let context = format!("divide by zero out of the Mode-13 aperture, signed={signed}");
        run_div_mem(signed, MODE13_TARGET, 17, 0, 0, 1, &context);
    }
    select_fpu_loop_rows(false);
}

/// The read's OWN guard must fire before anything else in the slot.
///
/// Catches a divisor load emitted ahead of the page guards. The operand straddles into a page the
/// fixture never mapped, so the read cannot be served; the row must retire nothing, and EAX/EDX
/// must be exactly what they were.
#[test]
fn div_memory_exits_on_the_read_guard_before_the_divide() {
    select_fpu_loop_rows(true);
    for signed in [false, true] {
        let unmapped = RAM_TARGET + 0x1000;
        let mut fixture = prepare(
            &div_mem(signed, unmapped),
            RAM_TARGET,
            &|cpu: &mut CpuGsw| seed_divisor(cpu, 17, 0),
        );
        with_divisor(&mut fixture, RAM_TARGET, 5);
        finish(
            fixture,
            1,
            &format!("unmapped divisor page, signed={signed}"),
        );
    }
    select_fpu_loop_rows(false);
}
