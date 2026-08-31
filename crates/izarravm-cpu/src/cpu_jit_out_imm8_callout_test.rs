// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `0xE6` OUT imm8,AL -- the `PortWriteAlImm8` call-out, behind `IZARRAVM_OUT_IMM8_ROWS`
//! (`dev_docs/gp2-out-e6-research-2026-08-30.md` §5 Option B, gp2's 59,968,424-exit row).
//!
//! This file covers the KNOB and the `classify` ARM: whether `0xE6` reaches the compile walk at
//! all, and with what `port` baked into the slot. The HELPER's own contract (the UNCONDITIONAL
//! step break, the exact charge, zero partial effects, the privilege refusals, the engagement
//! counter) is covered in `cpu_jit_callout_test.rs`, beside `PortReadAlImm8`'s equivalent
//! fixtures, per the placement the `0xE4` slice established.
//!
//! **Every fixture here states its arm through `set_out_imm8_rows_for_test`, in both
//! directions.** The knob has shipped default ON since the 2026-08-30 gp2-586 ladder
//! (`parse_out_imm8_rows_arm`); stating the arm explicitly rather than reading the ambient one is
//! what keeps a refusal fixture from going vacuous now that the default is the arm it used to test
//! against, and what keeps a positive fixture from silently asserting the shipped behavior instead
//! of the one it names.

use super::*;

const ENTRY: u32 = 0x401;
const STACK_TOP: u32 = 0x4000;
/// The PIT control port, and not an arbitrary choice: it is the port gp2's single `0xE6` site
/// writes 59,968,424 times, and the one whose control-word write can call `pic.request(0)` from
/// inside `MachineBus::write_io`. A fixture on any quieter port would exercise the arm without
/// naming the hazard the forced step break exists for.
const PORT: u8 = 0x43;

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

/// Resets the thread-local override to the ambient reading when dropped -- including on unwind,
/// which is what a plain trailing `set_out_imm8_rows_for_test(None)` does not do: a panic partway
/// through a fixture would skip it, and under `--test-threads=1` a leaked `Some(true)` could
/// silently arm a later fixture that meant to read the shipped default.
#[must_use]
struct OutImm8RowsGuard;

impl Drop for OutImm8RowsGuard {
    fn drop(&mut self) {
        jit::direct::set_out_imm8_rows_for_test(None);
    }
}

/// Select the arm for this thread and PROVE the selection took.
fn select_out_imm8_rows(enabled: bool) -> OutImm8RowsGuard {
    jit::direct::set_out_imm8_rows_for_test(Some(enabled));
    assert_eq!(
        jit::direct::out_imm8_rows_armed(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_OUT_IMM8_ROWS"
    );
    OutImm8RowsGuard
}

// ---------------------------------------------------------------------------------------------
// The knob
// ---------------------------------------------------------------------------------------------

/// THE DEFAULT PIN. This row ships default ON since the 2026-08-30 flip: the gp2-586 ladder read
/// min-wall ratio 1.2127 with 4/4 rounds valid, and the two-arm census reconciliation removed the
/// whole 59,968,424-exit row with zero relocation. A default that moved back without equivalent
/// evidence would change every shipped binary's admission silently.
#[test]
fn out_imm8_rows_ship_on_since_the_20260830_ladder() {
    jit::direct::set_out_imm8_rows_for_test(None);
    let ambient = std::env::var("IZARRAVM_OUT_IMM8_ROWS");
    let expected = jit::direct::parse_out_imm8_rows_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::out_imm8_rows_armed(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_OUT_IMM8_ROWS={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            expected,
            "IZARRAVM_OUT_IMM8_ROWS must default ON: the 2026-08-30 gp2-586 ladder (1.2127, 4/4 \
             rounds) is the flip's evidence, recorded on the parse arm and in the flip commit"
        );
    }
}

/// The spelling table: unset and `""` must BOTH parse `true` (the default, ON since 2026-08-30),
/// `1`/`on` must name the same arm stated, `0`/`off` the pre-slice base, and anything else must
/// PANIC.
///
/// Catches: a `_ => false` fallthrough replacing the panic -- a mistyped ladder leg would silently
/// run the base and be read as "the slice under test changed nothing".
#[test]
fn out_imm8_rows_spelling_table_names_both_arms() {
    use std::env::VarError;
    let unset = jit::direct::parse_out_imm8_rows_arm_for_test(Err(VarError::NotPresent));
    let empty = jit::direct::parse_out_imm8_rows_arm_for_test(Ok(String::new()));
    assert!(
        unset,
        "unset must name the ON arm: the default flipped ON with the 2026-08-30 ladder"
    );
    assert_eq!(
        unset, empty,
        "unset and \"\" must name the SAME arm (the campaign's convention), not \
         IZARRAVM_ATA_POLL_SKIP's inverted one"
    );
    for off in ["0", "off", "OFF", " off ", "Off"] {
        assert!(
            !jit::direct::parse_out_imm8_rows_arm_for_test(Ok(off.to_string())),
            "{off:?} must name the off arm"
        );
    }
    for on in ["1", "on", "ON", " on ", "On"] {
        assert!(
            jit::direct::parse_out_imm8_rows_arm_for_test(Ok(on.to_string())),
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "e6", "out"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_out_imm8_rows_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_OUT_IMM8_ROWS={typo:?} names no arm and must panic rather than silently \
             running the base"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The `classify` arm
// ---------------------------------------------------------------------------------------------

/// Build `mov esi,esi` / `out PORT,al` / `mov edi,edi` at `ENTRY`, mid-block exactly as the `0xE4`
/// fixtures do, and try to compile it.
fn compile_out_imm8_block(port: u8) -> Option<jit::direct::Compilation> {
    let code = [0x89, 0xf6, 0xe6, port, 0x89, 0xff];
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.registers.set_esp(STACK_TOP);
    for &linear in &[ENTRY, ENTRY + 2, ENTRY + 4] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation),
        jit::direct::CompileOutcome::StructuralReject(_)
        | jit::direct::CompileOutcome::Retry(_) => None,
    }
}

/// With the knob OFF, `0xE6` must stay a barrier -- byte-identical to the pre-slice tree, which is
/// what makes the census row PRESENT on the OFF arm and ABSENT on the ON arm the campaign's proof
/// the admission took.
///
/// Catches: a `jit_admits_non_continuable` or `classify` arm that forgot its `out_imm8_rows_armed()`
/// guard, which would destroy the A/B base.
#[test]
fn out_imm8_stays_a_barrier_with_the_gate_off() {
    let _guard = select_out_imm8_rows(false);
    assert!(
        compile_out_imm8_block(PORT).is_none(),
        "0xE6 must not compile into a block with the gate off"
    );
}

/// With the knob ON, `0xE6` joins the block as a `PortWriteAlImm8` call-out slot AND ends it.
///
/// BOTH edits are required for this to pass, and neither alone: `jit_admits_non_continuable` gets
/// the compile walk past `block_continuable`'s port refusal (`decode.rs`, which admits only the IN
/// forms and which this slice deliberately does NOT touch, so the interpreter's batch structure
/// stays byte-identical), and the `classify` arm turns it into a slot rather than a
/// `HardBoundary` one census arm to the left.
///
/// TWO of the three slots, not three: the helper's step break is unconditional, so a third slot
/// could never execute, and `DirectKind::is_terminal` stops the walk at the OUT rather than
/// emitting it dead. Catches an `is_terminal` arm dropped or written against the wrong helper.
#[test]
fn out_imm8_joins_the_block_and_terminates_it_with_the_gate_on() {
    let _guard = select_out_imm8_rows(true);
    let compilation = compile_out_imm8_block(PORT).expect("0xE6 must compile with the gate on");
    assert_eq!(
        compilation.span.instructions, 2,
        "the walk must stop AT the call-out: a slot after an unconditional step break is dead code"
    );
    assert_eq!(
        compilation.callout_slots, 1,
        "the call-out must be counted for the budget bound"
    );
    assert_eq!(
        compilation.callout_port_slots, 1,
        "PortWriteAlImm8 is in the PORT class: it reaches check_io_permission exactly as the two \
         IN helpers do, which is what arms run_direct_block's privilege gate for it"
    );
}

/// `classify` admits `0xE6` for ANY immediate port, unconditionally -- there is no per-port
/// allowlist, because the forced step break discharges the pendency obligation for every port at
/// once rather than per device. Four ports, including the PIT control port and the PIC command
/// port, the two whose writes can move an IRQ line from inside `write_io`.
#[test]
fn out_imm8_admits_any_port_unconditionally() {
    let _guard = select_out_imm8_rows(true);
    for port in [0x00u8, 0x20, 0x43, 0xff] {
        assert!(
            compile_out_imm8_block(port).is_some(),
            "port {port:#04x} must be admitted -- the arm is not port-gated"
        );
    }
}

/// The Word gate, RE-OPENED by the corpus campaign's L2 slice: `0xE6` is now on `classify`'s
/// Word-size allowlist, so a 16-bit code segment refuses `0xE6` on the OFF arm (the knob's
/// pre-slice base, `non_continuable`) and ADMITS it as a call-out slot on the ON arm, exactly as
/// the 32-bit form already does. The standing refusal this used to pin is gone: the corpus
/// supplied the missing measurement (123-talk-shareware 238,768 unbound exits on this exact row,
/// its #1 `non_continuable` row; 1000-miglia 173,344; 100-000-pyramid 11,530).
///
/// Catches: the ON arm compiling on neither leg (the admission silently regressing) or the OFF
/// arm compiling on either leg (a Word admission that leaked past the knob).
#[test]
fn out_imm8_is_barred_off_and_admitted_on_in_a_sixteen_bit_segment() {
    let code = [0x89, 0xf6, 0xe6, PORT, 0x89, 0xff];
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(STACK_TOP);
    cpu.set_eip(ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    for &linear in &[ENTRY, ENTRY + 2, ENTRY + 4] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }

    {
        let _guard = select_out_imm8_rows(false);
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, false),
                jit::direct::CompileOutcome::StructuralReject(_)
                    | jit::direct::CompileOutcome::Retry(_)
            ),
            "the OFF arm must still refuse a 16-bit `0xE6`"
        );
    }
    {
        let _guard = select_out_imm8_rows(true);
        let compilation = match jit::direct::compile(&mut cpu, ENTRY, false) {
            jit::direct::CompileOutcome::Compiled(compilation) => compilation,
            _ => panic!("the ON arm must admit a 16-bit `0xE6`"),
        };
        assert_eq!(
            compilation.span.instructions, 2,
            "the walk must stop AT the call-out: its step break is unconditional"
        );
        assert_eq!(compilation.callout_slots, 1);
    }
}

/// **THE OFF ARM'S ROW MUST NOT RELOCATE, and the ON arm's row must DISAPPEAR (0 exits), not
/// move.** The test above says the OFF arm still refuses a 16-bit `0xE6` and the ON arm now
/// compiles it; this fixture is the barrier-census half of that claim, read with the census
/// enabled on the OFF arm alone (the ON arm produces no barrier row at all, because it compiles).
///
/// Before the corpus L2 slice, the historical defect this fixture guarded against was admitting
/// `0xE6` on the opcode ALONE and having a 16-bit site stop with `BarrierStop::HardBoundary` on
/// the ON arm where the OFF arm stopped it with `NonContinuable` -- a relocated row rather than a
/// vanished one, which the two-arm reconciliation
/// ([[overlapping-slices-measure-leftovers]]) would have misread. Now that the Word admission is
/// real and knob-gated in `classify` itself (not just in `jit_admits_non_continuable`), the ON
/// arm's row does not relocate, it is retired: `compile` returns `Compiled`, not
/// `StructuralReject`, so no barrier is recorded for it at all. This fixture pins the OFF arm's
/// row identity, which is what a revert of the Word allowlist entry (leaving the
/// `jit_admits_non_continuable` term behind) would move.
#[test]
fn out_imm8_off_arm_keeps_its_pre_slice_barrier_row() {
    fn fixture_cpu_and_bus() -> (CpuGsw, TestBus) {
        let code = [0x89, 0xf6, 0xe6, PORT, 0x89, 0xff];
        let mut memory = vec![0u8; 0x5000];
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

        let mut cpu = CpuGsw::default();
        cpu.set_mode(GswMode::Gsw586);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.enable_direct_barrier_census(true);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        for &linear in &[ENTRY, ENTRY + 2, ENTRY + 4] {
            cpu.set_eip(linear);
            cpu.fetch_decoded(&mut bus, linear).unwrap();
        }
        (cpu, bus)
    }

    {
        let _guard = select_out_imm8_rows(false);
        let (mut cpu, _bus) = fixture_cpu_and_bus();
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, false),
                jit::direct::CompileOutcome::StructuralReject(_)
                    | jit::direct::CompileOutcome::Retry(_)
            ),
            "the OFF arm must refuse"
        );
        let snapshot = cpu
            .direct_barrier_census_snapshot()
            .expect("the census was enabled");
        let row = snapshot
            .rows
            .iter()
            .find(|row| row.opcode == 0xe6)
            .expect("the OFF arm must record its own 0xE6 barrier row");
        assert_eq!(
            row.stop_reason.to_string(),
            "non_continuable",
            "the OFF arm must be main's own row, or this fixture is comparing against a moved \
             base"
        );
    }
    {
        // The ON arm's counterpart: it must not leave a barrier row behind AT ALL, because it
        // compiles. A stray row here would mean the Word admission is real in `classify` but the
        // compile walk still stopped short of it on some path -- the exact relocation defect this
        // fixture used to detect by comparing two barrier rows, now detected instead by there
        // being only one to find.
        let _guard = select_out_imm8_rows(true);
        let (mut cpu, _bus) = fixture_cpu_and_bus();
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, false),
                jit::direct::CompileOutcome::Compiled(_)
            ),
            "the ON arm must admit a 16-bit `0xE6`"
        );
        let snapshot = cpu
            .direct_barrier_census_snapshot()
            .expect("the census was enabled");
        assert!(
            snapshot.rows.iter().all(|row| row.opcode != 0xe6),
            "the ON arm compiled the block and must not also have recorded an 0xE6 barrier row"
        );
    }
}
