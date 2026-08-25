// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `0x85` TEST r/m16, r16, REGISTER form, at Word operand size -- behind `IZARRAVM_TEST_WORD_ROWS`.
//!
//! The row is the top of the duke3d-586 rejected table at main `49c7ad97`, the first duke census
//! taken after `IZARRAVM_ROTATE_ROWS`, `IZARRAVM_COUNT_LANES`, `IZARRAVM_FPU_LOOP_ROWS` and
//! `IZARRAVM_V86_LOOP_ROWS` all became the shipped default
//! (`.bench/results/duke-census-slice-20260821/`). Ranked by `runtime_hits`, twelve `0x85` rows
//! carry **53,583,389 of the table's 126,933,336 hits -- 42.2%**, and 42,642,774 static unbound
//! exits behind them. The next row is `0x8E /0` at 22,638,814, which this slice deliberately
//! leaves alone; see `test_word_rows_enabled` for the suffix measurement that separates them.
//!
//! **What the width field changes, and what it deliberately does not.** `DirectKind::Test` grew a
//! `width` exactly as `MovImm`, `AluImm`, `Load`, `Store` and `MovExtendReg` each did in their own
//! slice, which is what discharges `classify`'s header entry naming `0x85` as a Dword sibling with
//! no width field. The Dword arm still calls the ORIGINAL `emit_test` with the original arguments,
//! so a gate-OFF binary emits byte-identical code for every TEST it has ever emitted; the Word arm
//! is `emit_test_byte`'s shape at the other narrow width, reading both register operands through
//! `emit_read_store_value` and handing them to the `emit_test_preloaded` that has been emitting
//! the Byte form in production.
//!
//! **Every fixture here states its arm through `set_test_word_rows_for_test`, in both directions.**
//! The default is OFF, so a positive fixture that read the ambient knob would be testing the
//! refusal and calling it a lowering; and a refusal fixture that inherited the arm would go vacuous
//! the day the default moves. The default pin is the one assertion that reads the ambient knob, and
//! it is supposed to.
//!
//! The differential rows run the same guest bytes natively and through a block-free interpreter
//! from identical state, and compare registers (segment registers and EIP included), the raw lazy
//! flags descriptor, materialized EFLAGS, the halt latch, core clocks, bus clocks and the whole of
//! guest RAM. The tested opcode is always MID-BLOCK: an opcode at a block's entry slot side-exits
//! having retired nothing, so an entry-position fixture cannot tell a lowering from a refusal.
//!
//! **TEST WRITES NO REGISTER, so the usual Word hazard is not the one to pin here.** What
//! discriminates a correct Word lowering from a Dword one is entirely FLAGS, and the seed is built
//! for that: every GPR carries `0xdead` in its high half, so `AND` over 32 bits and `AND` over 16
//! bits disagree about ZF on the commonest case in the census (`test ax,ax` with a zero AX), about
//! SF whenever bit 15 and bit 31 differ, and about PF never -- PF is the low byte at both widths,
//! which is why ZF and SF are the columns the operand table is chosen to exercise.

// MUTATION EVIDENCE (2026-08-21). Each mutation below was applied BY HAND to the committed tree,
// the fixtures were run, the failure text was read, and the tree was restored with
// `git checkout -- <file>` (which is why the slice was committed first: that command discards
// every uncommitted change to the file, not only the mutation). A mutation nobody catches is a
// fixture bug, not a free pass, so the failing ASSERTION is quoted rather than the test name alone.
//
// | # | mutation | caught by | the assertion that fired |
// |---|---|---|---|
// | M1 | the Word emit arm routed to `emit_test`, i.e. the Dword emitter reached through the new gate | `test_word_register_form_matches_the_interpreter_*` (both) | raw lazy-flags descriptor: `tag 0x8000_0202, result 0xdead_0000` against `tag 0x8000_0102, result 0` |
// | M2 | the classify arm's `width: operand_width` reverted to `MemoryWidth::Dword` | the same two | the same descriptor pair, from the other end of the same path |
// | M3 | the allowlist term widened from `insn.opcode == 0x85` to `matches!(.., 0x85 \| 0xa9)` | `the_gate_does_not_sweep_in_its_neighbours` | "0xA9 TEST AX,imm16 must stay a barrier at Word on the true arm": `Some(3)` against `None` |
// | M4 | the allowlist term's `test_word_rows_enabled()` guard dropped | `the_word_test_row_flips_with_the_gate` | "unprefixed 85 /r ... must stay a barrier with the gate off": `Some(3)` against `None` -- the mutation that would destroy the A/B base |
// | M5 | the arm's `DecodedOperand::Reg` bind relaxed to fall back to register 0 on memory | `the_gate_does_not_sweep_in_its_neighbours` | "85 /r MEMORY form must stay a barrier at Dword on the false arm": `Some(3)` against `None` |
//
// All five were RE-RUN against the reworked `operand_seed` sweep on 2026-08-21 and all five still
// die. M1's surviving `result` moved `0xbeef_0000` -> `0xdead_0000` on the first failing case,
// which is the N1 fix visible in the failure text itself: on an aliasing pair the value that now
// survives into the register is `a`, where before it was `b` overwriting `a`.
//
// M1 and M2 are the same defect entered from the two ends of the width field, and BOTH were run
// because the emitter and the classifier are separately editable; the shared failure text is the
// point rather than a duplication. Note what killed them: not the materialized EFLAGS but the RAW
// pending descriptor, which is the earlier and stricter of the two. A Word TEST publishes a
// descriptor a later reader materializes, so a wrong width tag is a defect that outlives the
// instruction, and `compare_state` reads it directly for that reason.

use super::*;

/// EIP of the block entry. CS base is 0 in the 16-bit fixture, so this is also its linear address.
const ENTRY: u32 = 0x100;
/// The word operand's OFFSET for the MEMORY form, which must stay refused. 2-aligned.
const OPERAND: u16 = 0x0220;
/// The 16-bit fixture's segment bases, chosen distinct so a dropped override would be observable.
const CS_BASE: u32 = 0x0000;
const DS_BASE: u32 = 0x0400;
const ES_BASE: u32 = 0x0300;
const STACK_SP: u32 = 0x0700;
/// The stack pointer with a poisoned high half, the shape `cpu_jit_v86_loop_rows_test.rs` uses.
const STACK_ESP: u32 = 0xdead_0000 | STACK_SP;

/// A distinct byte at every address, so a read of the wrong WIDTH or through the wrong SEGMENT is
/// observable. A constant fill would make a four-byte read indistinguishable from a two-byte one.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x1_0000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// Select the arm for this thread and PROVE the selection took.
fn select_test_word_rows(enabled: bool) {
    jit::direct::set_test_word_rows_for_test(Some(enabled));
    assert_eq!(
        jit::direct::test_word_rows_enabled(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_TEST_WORD_ROWS"
    );
}

/// Real mode, CS.D = 0 and SS.B = 0. The unprefixed `85 /r` decodes at Word here, which is the
/// census's `operand_size=word, prefix_mask=0` half (15,743,159 runtime hits over four rows).
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

/// Protected mode, flat, CS.D = 1. A `66`-prefixed `85 /r` decodes at Word here, which is the
/// census's `operand_size=word, prefix_mask=1` half -- 37,840,230 runtime hits over eight rows and
/// the larger of the two halves by 2.4 to one. duke3d runs DOS4GW, so this is the mode that
/// carries the row.
fn flat_protected_cpu() -> CpuGsw {
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

/// `mov si,si` / `mov esi,esi`: the leading slot that keeps the tested opcode off the block entry.
const FILL_A: [u8; 2] = [0x89, 0xf6];
/// `mov di,di` / `mov edi,edi`: the trailing slot, so the tested opcode is never last either.
const FILL_B: [u8; 2] = [0x89, 0xff];

/// Compile `FILL_A / body / FILL_B / hlt` at `ENTRY` and report the span length, or `None` when the
/// walk refused it.
///
/// Every decode line is warmed, not only the entry one: warming the entry alone makes slot 1 miss,
/// the walk stops at `Retry`, and a negative assertion then passes whether or not the opcode is
/// lowered.
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
    // Unconditional, and not only for the memory row: with the operand pages absent from the fast
    // map every memory kind is refused, so a negative assertion made without it would pass for the
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

/// `TEST r/m, r` register form. `modrm` is `0b11_rrr_mmm`, so `reg` is the second operand's
/// register number and `rm` the first's -- both are plain register numbers on this opcode, which
/// is why the census reports twelve `modrm_reg` rows for one shape rather than a `/digit` group.
fn test_reg(reg: u8, rm: u8) -> Vec<u8> {
    vec![0x85, 0xc0 | (reg << 3) | rm]
}

/// The same with a `0x66` operand-size prefix.
fn test_reg_66(reg: u8, rm: u8) -> Vec<u8> {
    [vec![0x66], test_reg(reg, rm)].concat()
}

/// `TEST [disp16], AX` -- the MEMORY form, which has no kind at either width and must stay a
/// barrier on both arms.
fn test_mem16() -> Vec<u8> {
    [vec![0x85, 0x06], OPERAND.to_le_bytes().to_vec()].concat()
}

/// `TEST AX, imm16` (0xA9) -- the family member this slice deliberately does NOT admit, because
/// the duke census measures zero rows for it at any width.
fn test_ax_imm16() -> Vec<u8> {
    vec![0xa9, 0x34, 0x12]
}

/// `mov ax,cx` / `mov eax,ecx`: the control row, which must compile on BOTH arms. Without it a
/// refusal assertion could pass because the harness refuses everything.
const CONTROL: [u8; 2] = [0x89, 0xc8];

// ---------------------------------------------------------------------------------------------
// The gate itself
// ---------------------------------------------------------------------------------------------

/// THE DEFAULT PIN, and it is the one assertion that decides what a shipped binary admits.
///
/// Catches: a flip of `parse_test_word_rows_arm`'s `NotPresent` arm. The row is DEFAULT OFF and a
/// default that moved without a ladder would change every shipped binary's admission silently.
///
/// It reads the AMBIENT knob deliberately -- no override -- and it must therefore agree with the
/// ENVIRONMENT rather than with a constant, because this suite is run on BOTH arms: the whole point
/// of the `DIRECT_BARRIER` episode is that a knob's ON arm has to be green too, and a fixture that
/// hard-asserted "off" would make that impossible by construction. With the variable unset the
/// assertion reduces to "the default is OFF", which is the claim this fixture exists for.
#[test]
fn test_word_rows_ship_off_by_default() {
    jit::direct::set_test_word_rows_for_test(None);
    let ambient = std::env::var("IZARRAVM_TEST_WORD_ROWS");
    let expected = jit::direct::parse_test_word_rows_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::test_word_rows_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_TEST_WORD_ROWS={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            !expected,
            "IZARRAVM_TEST_WORD_ROWS must default OFF; the row has not been priced on a wall \
             ladder that authorized a flip"
        );
    }
}

/// The spelling table, both arms and the refusal.
///
/// Catches: a `_ => false` fallthrough replacing the panic. A mistyped ladder leg
/// (`IZARRAVM_TEST_WORD_ROWS=yes`) that fell through would run the BASE and be read as "the row I
/// asked for changed nothing", which is the single wrong conclusion an arm ladder exists to avoid.
#[test]
fn test_word_rows_spelling_table_names_both_arms() {
    use std::env::VarError;
    assert!(
        !jit::direct::parse_test_word_rows_arm_for_test(Err(VarError::NotPresent)),
        "unset must name the OFF arm: this row is default-off"
    );
    // The empty string and unset happen to agree HERE, and the case is pinned anyway because the
    // family's other four knobs default ON, where the two differ: nulling an environment variable
    // in PowerShell leaves it PRESENT and EMPTY, and three earlier evidence directories ran their
    // default-ON knobs off believing they had left them at the default.
    assert!(
        !jit::direct::parse_test_word_rows_arm_for_test(Ok(String::new())),
        "the empty string is the OFF arm"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(
            !jit::direct::parse_test_word_rows_arm_for_test(Ok(off.to_string())),
            "{off:?} must name the off arm"
        );
    }
    for on in ["1", "on", "ON", " on ", "On"] {
        assert!(
            jit::direct::parse_test_word_rows_arm_for_test(Ok(on.to_string())),
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "test", "word"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_test_word_rows_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_TEST_WORD_ROWS={typo:?} names no arm and must panic rather than silently \
             running the base"
        );
    }
}

/// The row flips with the gate, in BOTH of the modes the census measures, and nothing else does.
///
/// Catches, in the `false` direction: an arm that forgot its `test_word_rows_enabled()` guard, i.e.
/// a row that ships admitted while the knob says off -- which would make the gate-OFF census leg
/// disagree with main and destroy the A/B base.
///
/// Catches, in the `true` direction: a missing or wrongly-keyed allowlist term.
///
/// The DWORD forms are asserted on both arms and in both modes, because the width field this slice
/// adds touches the Dword path's construction even though it must not touch its emission. A Dword
/// TEST that stopped compiling would be a regression this gate has no business causing.
#[test]
fn the_word_test_row_flips_with_the_gate() {
    for &(reg, rm) in &[(0u8, 0u8), (0, 1), (1, 1), (3, 6), (6, 6), (7, 2)] {
        select_test_word_rows(false);
        assert_eq!(
            compile16(&test_reg(reg, rm)),
            None,
            "unprefixed 85 /r in a 16-bit segment must stay a barrier with the gate off \
             (reg={reg} rm={rm})"
        );
        assert_eq!(
            compile32(&test_reg_66(reg, rm)),
            None,
            "66 85 /r in a 32-bit segment must stay a barrier with the gate off \
             (reg={reg} rm={rm})"
        );
        // The Dword forms, which this gate must leave exactly where they were.
        assert_eq!(
            compile32(&test_reg(reg, rm)),
            Some(3),
            "85 /r at Dword must compile on the OFF arm (reg={reg} rm={rm})"
        );
        assert_eq!(
            compile16(&test_reg_66(reg, rm)),
            Some(3),
            "66 85 /r in a 16-bit segment is Dword and must compile on the OFF arm \
             (reg={reg} rm={rm})"
        );

        select_test_word_rows(true);
        assert_eq!(
            compile16(&test_reg(reg, rm)),
            Some(3),
            "unprefixed 85 /r in a 16-bit segment must be admitted with the gate on \
             (reg={reg} rm={rm})"
        );
        assert_eq!(
            compile32(&test_reg_66(reg, rm)),
            Some(3),
            "66 85 /r in a 32-bit segment must be admitted with the gate on (reg={reg} rm={rm})"
        );
        assert_eq!(
            compile32(&test_reg(reg, rm)),
            Some(3),
            "85 /r at Dword must still compile on the ON arm (reg={reg} rm={rm})"
        );
        assert_eq!(
            compile16(&test_reg_66(reg, rm)),
            Some(3),
            "66 85 /r in a 16-bit segment is Dword and must still compile on the ON arm \
             (reg={reg} rm={rm})"
        );
    }

    for arm in [false, true] {
        select_test_word_rows(arm);
        assert_eq!(
            compile16(&CONTROL),
            Some(3),
            "the control row must compile on the {arm} arm, or this fixture cannot fail"
        );
        assert_eq!(
            compile32(&CONTROL),
            Some(3),
            "the 32-bit control row must compile on the {arm} arm"
        );
    }
    jit::direct::set_test_word_rows_for_test(None);
}

/// The neighbours this slice must NOT have swept in, on BOTH arms.
///
/// * The **memory form** of `0x85`. It has no kind at either width, no emitter and no census row on
///   duke3d-586 (zero memory rows against 53.6M register hits). The classify arm refuses it by
///   binding `DecodedOperand::Reg`, and a relaxed bind would reach an emitter expecting a register.
/// * **`0xA9` TEST AX, imm16.** Its kind already carries a width and `emit_test_imm_reg` is already
///   width-parameterised, so it is ONE allowlist entry from being admitted -- and it stays out
///   because the census measures zero `0xA9` rows at any width, and an unmeasured admission is the
///   campaign's standing refusal. This assertion is what keeps a later reader from adding it for
///   symmetry instead of for a row.
/// * **`0xF7 /0` TEST r/m16, imm16** is NOT here: it was already admitted before this slice, by
///   sub-opcode, and asserting it as a refusal would be wrong.
#[test]
fn the_gate_does_not_sweep_in_its_neighbours() {
    for arm in [false, true] {
        select_test_word_rows(arm);
        assert_eq!(
            compile16(&test_mem16()),
            None,
            "85 /r MEMORY form must stay a barrier at Word on the {arm} arm"
        );
        assert_eq!(
            compile32(&[vec![0x85, 0x05], 0x0000_0220u32.to_le_bytes().to_vec()].concat()),
            None,
            "85 /r MEMORY form must stay a barrier at Dword on the {arm} arm"
        );
        assert_eq!(
            compile16(&test_ax_imm16()),
            None,
            "0xA9 TEST AX,imm16 must stay a barrier at Word on the {arm} arm"
        );
    }
    jit::direct::set_test_word_rows_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// The differential harness
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            // 0xdead in every high half, so an AND taken over 32 bits and one taken over 16 bits
            // disagree about ZF and (for the right pairs) SF. This is the ONLY discriminator this
            // row has: TEST writes no register, so the usual "did the high half survive" assertion
            // is trivially true down both paths.
            gpr: std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32)),
            eflags: 0x202,
            live_pending: false,
        }
    }

    fn gpr(mut self, index: usize, value: u32) -> Self {
        self.gpr[index] = value;
        self
    }

    fn pending(mut self) -> Self {
        self.live_pending = true;
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
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
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
        cpu.registers.eflags = seed.eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
            // A descriptor produced BEFORE the tested instruction. TEST publishes a descriptor of
            // its own, so this is what proves the new one REPLACES the old rather than merging
            // with it -- and it is the state a wrong width tag corrupts silently.
            let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
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
        crate::tests::settled_registers(&roles.native),
        crate::tests::settled_registers(&roles.interp),
        "{context}: registers (segment registers and EIP included)"
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

/// The operand pairs, chosen for the flag columns a Word/Dword confusion actually moves.
///
/// `0x0000` against a poisoned high half is the ZF discriminator and it is the commonest form in
/// real code (`test ax,ax`). `0x8000` and `0x00ff` split SF and PF. `0xffff` is the all-ones
/// saturation. Each is written into the LOW half only, so the seed's `0xdead` high half survives.
const OPERANDS: [u32; 6] = [0x0000, 0x0001, 0x00ff, 0x7fff, 0x8000, 0xffff];

/// Seed the two guest operands of `TEST rm, reg`, or `None` when the iteration is redundant.
///
/// **When `reg == rm` there is only ONE guest register.** The first version of this file wrote
/// both values into it —
///
/// ```ignore
/// Seed::new().gpr(rm, 0xdead_0000 | a).gpr(reg, 0xbeef_0000 | b)
/// ```
///
/// — so on every aliasing pair the second call overwrote the first, `a` was discarded, and the
/// advertised 6 x 36 sweep quietly collapsed to 6 distinct machine states. The assertions still
/// fired (the surviving high half is nonzero, so the ZF discriminator survived) which is exactly
/// why nothing caught it: it was coverage loss, not a hole. Found by the 2026-08-21 adversarial
/// review (N1).
///
/// The fix sweeps the aliased pairs over `a` alone and skips the thirty duplicate iterations,
/// rather than pretending to a second operand that does not exist.
fn operand_seed(reg: u8, rm: u8, a: u32, b: u32) -> Option<Seed> {
    if reg == rm {
        // `test ax,ax`: the guest ANDs the register with itself, so `b` is architecturally
        // meaningless here. One state per `a`, and the 0xdead high half is what makes a Dword
        // lowering of `test ax,ax` with AX = 0 disagree about ZF.
        if a != b {
            return None;
        }
        return Some(Seed::new().gpr(usize::from(rm), 0xdead_0000 | a));
    }
    Some(
        Seed::new()
            .gpr(usize::from(rm), 0xdead_0000 | a)
            .gpr(usize::from(reg), 0xbeef_0000 | b),
    )
}

// ---------------------------------------------------------------------------------------------
// 0x85 TEST r/m16, r16 at Word
// ---------------------------------------------------------------------------------------------

/// The census row itself, in a 16-bit segment: unprefixed `85 /r`.
///
/// Catches: a Word arm routed to the Dword emitter (ZF and SF differ on the poisoned high half); a
/// descriptor tagged `0x8000_0202` instead of `0x8000_0102`, which recomputes the lazy flags at a
/// 32-bit width and is invisible until a later reader materializes them; and a lowering that wrote
/// a register, since `compare_state` compares the whole register file.
///
/// Both operand REGISTERS are swept, including the `reg == rm` case (`test ax,ax`), which is the
/// shape the census's largest row is: `emit_read_store_value` is called twice into two different
/// host registers, and an aliasing bug there would only show when the two guest operands are the
/// same register.
///
/// **The sweep is 36 operand pairs on the four DISTINCT-register pairs and 6 on the two aliased
/// ones**, because an aliased pair has one register and therefore one value; `operand_seed` says so
/// and skips the redundant iterations rather than overwriting one value with the other. Do not
/// restore the "6 x 36" reading — it was never true for `(0,0)` and `(6,6)`.
#[test]
fn test_word_register_form_matches_the_interpreter_in_a_sixteen_bit_segment() {
    select_test_word_rows(true);
    for &(reg, rm) in &[(0u8, 0u8), (0, 1), (1, 0), (2, 3), (6, 6), (7, 4)] {
        for a in OPERANDS {
            for b in OPERANDS {
                let Some(base) = operand_seed(reg, rm, a, b) else {
                    continue;
                };
                for live_pending in [false, true] {
                    let seed = if live_pending { base.pending() } else { base };
                    lowered_on(
                        sixteen_bit_cpu,
                        false,
                        &test_reg(reg, rm),
                        seed,
                        &format!("test r{rm},r{reg} a={a:#06x} b={b:#06x} pending={live_pending}"),
                    );
                }
            }
        }
    }
    jit::direct::set_test_word_rows_for_test(None);
}

/// The same row in the mode duke3d actually runs: 32-bit flat protected mode with a `0x66` prefix.
/// This is 37,840,230 of the row's 53,583,389 runtime hits, against 15,743,159 unprefixed.
#[test]
fn test_word_register_form_matches_the_interpreter_in_a_thirty_two_bit_segment() {
    select_test_word_rows(true);
    for &(reg, rm) in &[(0u8, 0u8), (1, 1), (0, 6), (3, 2), (6, 0), (7, 7)] {
        for a in OPERANDS {
            for b in OPERANDS {
                let Some(base) = operand_seed(reg, rm, a, b) else {
                    continue;
                };
                for live_pending in [false, true] {
                    let seed = if live_pending { base.pending() } else { base };
                    lowered_on(
                        flat_protected_cpu,
                        true,
                        &test_reg_66(reg, rm),
                        seed,
                        &format!(
                            "66 test r{rm},r{reg} a={a:#06x} b={b:#06x} pending={live_pending}"
                        ),
                    );
                }
            }
        }
    }
    jit::direct::set_test_word_rows_for_test(None);
}

/// THE DWORD REGRESSION, on BOTH arms.
///
/// The width field this slice adds is constructed on the Dword path too, and the whole safety
/// argument for the gate-OFF binary is that Dword still reaches `emit_test` with the same
/// arguments. That is an argument about the emitter, and this is the assertion that makes it a
/// measured fact: the same operand sweep at Dword, unprefixed in a 32-bit segment, must match the
/// interpreter whether the gate is on or off.
#[test]
fn test_dword_register_form_is_unchanged_on_both_arms() {
    for arm in [false, true] {
        select_test_word_rows(arm);
        for &(reg, rm) in &[(0u8, 0u8), (1, 2), (5, 3)] {
            for a in OPERANDS {
                for b in OPERANDS {
                    let Some(seed) = operand_seed(reg, rm, a, b) else {
                        continue;
                    };
                    lowered_on(
                        flat_protected_cpu,
                        true,
                        &test_reg(reg, rm),
                        seed,
                        &format!("dword test r{rm},r{reg} a={a:#06x} b={b:#06x} arm={arm}"),
                    );
                }
            }
        }
    }
    jit::direct::set_test_word_rows_for_test(None);
}
