// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The S-B ALU-rows slice, item 3: `0xD3 /4../7` (SHL/SHR/SAL/SAR by CL), REGISTER destination,
//! at Word operand size, behind `IZARRAVM_WORD_SHIFT_CL_ROWS` (default ON).
//!
//! Modelled on `cpu_jit_word_shift_test.rs`, which is the sixteen-bit REGISTER shift-by-imm8
//! lane's own fixture: the same five-`SETcc` consumer tail, the same `0xdead`-poisoned high
//! halves, and the same count sweep. The one structural difference is that the count here is
//! runtime data in CL rather than a decoded immediate, so `Seed` carries a CL value alongside the
//! destination operand instead of a `count: u8` parameter to the body-builder.
//!
//! `emit_shift_cl`'s width argument selects `shift_r16_cl` in place of `shift_r32_cl`; nothing
//! else in the emitter is width-dependent (see that function's doc). The shift itself is emitted
//! UNCONDITIONALLY before the count test, and a masked count of 0 is a no-op only because the
//! host applies its own five-bit CL mask -- there is no count-0 branch in the emitted code, so
//! `a_zero_count_leaves_every_flag_and_a_live_descriptor_alone` is a fixture concern, not a
//! control-flow one.
//!
//! The carry-seed trap ([[carry-seed-descriptor-swallows-cf]]): `(0x8d5, pending)` never delivers
//! a seeded CF because the live descriptor swallows it. This file uses `0x8d7` with
//! `live_pending: false` for the "a live descriptor survives a zero count" row, plus the
//! no-descriptor case beside it, exactly as `cpu_jit_group2_mem_test.rs:706-714` established.
//!
//! Mutation record, applied by hand and restored:
//!
//! | mutation | caught by | assertion |
//! |---|---|---|
//! | Word arm -> `shift_r32_cl` (i.e. the pre-slice emitter, width ignored) | `word_shift_cl_matches_the_interpreter_for_every_count` | registers: destination high half `0x0000_xxxx` where the interpreter carries `0xdead_xxxx`, and CF taken from bit 31 instead of bit 15 |
//! | drop the `& 0x1f` count mask in `classify`'s `0xd3` arm (none exists here; the mask lives in the emitted `and_r32_imm32(RCX, 0x1f)`, mutated instead) | `word_shift_cl_masks_a_count_above_thirty_one` | registers, at CL=32 (a full-width Dword-style shift instead of a no-op) |

use super::*;

const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The five flag consumers, as `(condition, byte destination)`. Deliberately avoids CL/CH: the
/// count lives in CL and a `SETcc` into it would overwrite the value being read.
const CONSUMERS: [(u8, u8); 5] = [
    (0x2, 3), // setc bl  -- CF
    (0x0, 6), // seto dh  -- OF
    (0x8, 4), // sets ah  -- SF
    (0x4, 2), // setz dl  -- ZF
    (0xa, 0), // setp al  -- PF
];

fn consumer_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    for (condition, dst) in CONSUMERS {
        bytes.extend_from_slice(&[0x0f, 0x90 | condition, 0xc0 | dst]);
    }
    bytes
}

/// `D3 /op` on a register destination, optionally 66-prefixed. CL is implicit and never encoded.
fn shift_cl_form(word: bool, op: u8, dst: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    if word {
        bytes.push(0x66);
    }
    bytes.extend_from_slice(&[0xd3, 0xc0 | (op << 3) | dst]);
    bytes
}

fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            gpr: std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32)),
            eflags: 0x202,
            live_pending: false,
        }
    }

    fn flags(mut self, eflags: u32) -> Self {
        self.eflags = eflags;
        self
    }

    fn pending(mut self) -> Self {
        self.live_pending = true;
        self
    }

    /// `dst` and `cl` set separately when the destination is not ECX itself: the destination's
    /// low 16 bits carry `operand` and ECX's low byte carries `count`, both against a poisoned
    /// high half.
    fn operand(mut self, dst: u8, operand: u16, count: u8) -> Self {
        if dst == 1 {
            // The aliasing case: CL is the low byte of the very register being shifted. The
            // emitter's `mov_r32_r32(RCX, home(1))` must snapshot this BEFORE the shift
            // overwrites `home(1)`, or the count read back would be the shifted result's low
            // byte rather than the pre-shift one.
            self.gpr[1] = 0xdead_0000 | (u32::from(operand) & 0xff00) | u32::from(count);
        } else {
            self.gpr[usize::from(dst)] = 0xdead_0000 | u32::from(operand);
            self.gpr[1] = 0xdead_0000 | u32::from(count);
        }
        self
    }
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    slots: u8,
}

fn build(body: &[u8], slots: u8, seed: Seed) -> Roles {
    let mut code = MOV_ESI_ESI.to_vec();
    let mut starts = vec![ENTRY, ENTRY + code.len() as u32];
    code.extend_from_slice(body);
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interp = flat_cpu();
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
        cpu.registers.set_esp(STACK_TOP);
        for offset in 0..code.len() as u32 {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            let _ = cpu.fetch_decoded(bus, linear);
        }
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        let page = (STACK_TOP - 4) & !0xfff;
        for write in [false, true] {
            let kind = if write {
                BusAccessKind::DataWrite
            } else {
                BusAccessKind::DataRead
            };
            let host = bus.direct_page(page, kind).unwrap().unwrap();
            let ok = if write {
                cpu.jit_fast_map.populate_write(
                    page,
                    page,
                    host,
                    jit::fast_map::PagePermissions::UNPAGED,
                    cpu.physical_page_watched(page),
                )
            } else {
                cpu.jit_fast_map.populate_read(
                    page,
                    page,
                    host,
                    jit::fast_map::PagePermissions::UNPAGED,
                    cpu.physical_page_watched(page),
                )
            };
            assert!(ok, "stack page {page:#x} must map");
        }
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the word shift-by-CL row is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the tested opcode really ran natively"
    );
    // A register shift-by-CL touches no guest memory at any width.
    assert_eq!(compilation.word_reads, 0, "word reads");
    assert_eq!(compilation.word_stores, 0, "word stores");
    assert_eq!(compilation.dword_reads, 0, "dword reads");
    assert_eq!(compilation.dword_stores, 0, "dword stores");
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = seed.gpr;
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.eflags = seed.eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
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
        slots,
    }
}

fn compare_state(roles: &Roles, context: &str) {
    assert_eq!(
        crate::tests::settled_registers(&roles.native),
        crate::tests::settled_registers(&roles.interp),
        "{context}: registers"
    );
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: EFLAGS"
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
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

fn lowered(body: &[u8], slots: u8, seed: Seed, context: &str) {
    let mut roles = build(body, slots, seed);
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        u64::from(roles.slots),
        "{context}: every slot must retire natively"
    );
    for _ in 0..roles.slots {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
}

fn with_consumers(word: bool, op: u8, dst: u8) -> Vec<u8> {
    let mut body = shift_cl_form(word, op, dst);
    body.extend_from_slice(&consumer_bytes());
    body
}

const CONSUMED_SLOTS: u8 = 2 + 1 + CONSUMERS.len() as u8;

/// The four admitted sub-ops, `(label, reg field)`.
const SUB_OPS: [(&str, u8); 4] = [("/4 shl", 4), ("/5 shr", 5), ("/6 sal", 6), ("/7 sar", 7)];

/// Every count from 0 to 31, all four ops, over the operand corners, with the destination's high
/// half seeded `0xdead` and CL carried in a register separate from the destination (`dst=3`, EBX,
/// so this row is not also the aliasing row).
#[test]
fn word_shift_cl_matches_the_interpreter_for_every_count() {
    for (label, op) in SUB_OPS {
        for count in 0..=31u8 {
            for operand in [0x0000u16, 0x0001, 0x8000, 0x7fff, 0xffff] {
                let seed = Seed::new().operand(3, operand, count);
                let context = format!("{label} bx={operand:#06x} cl={count} (consumed)");
                lowered(&with_consumers(true, op, 3), CONSUMED_SLOTS, seed, &context);
            }
        }
    }
}

/// CL 32, 33, 63, 64, 0xff -- the five-bit mask wrap. CL 32 masks to a no-op and must move
/// NOTHING, including the descriptor; the others mask into the range OF is or is not defined for.
#[test]
fn word_shift_cl_masks_a_count_above_thirty_one() {
    for (label, op) in SUB_OPS {
        for count in [32u8, 33, 63, 64, 0xff] {
            for operand in [0x8000u16, 0xffff] {
                for pending in [false, true] {
                    let mut seed = Seed::new().operand(3, operand, count);
                    if pending {
                        seed = seed.pending();
                    }
                    let context =
                        format!("{label} bx={operand:#06x} raw cl={count} pending={pending}");
                    lowered(&with_consumers(true, op, 3), CONSUMED_SLOTS, seed, &context);
                }
            }
        }
    }
}

/// A masked count of zero (CL itself 0, or CL 32/64/... masking to it) must leave every flag and
/// a live descriptor exactly as it is. `0x8d7` with `live_pending: false` is the seed the
/// carry-seed trap requires: `(0x8d5, pending)` never delivers a seeded CF because the descriptor
/// swallows it. Both the pending and the no-descriptor case run.
#[test]
fn a_zero_count_leaves_every_flag_and_a_live_descriptor_alone() {
    for (label, op) in SUB_OPS {
        for operand in [0x0000u16, 0x8000, 0xffff] {
            for (eflags, pending) in [(0x202u32, false), (0x8d7, false), (0x8d7, true)] {
                let mut seed = Seed::new().operand(3, operand, 0).flags(eflags);
                if pending {
                    seed = seed.pending();
                }
                let context =
                    format!("{label} bx={operand:#06x} cl=0 eflags={eflags:#x} pending={pending}");
                lowered(&with_consumers(true, op, 3), CONSUMED_SLOTS, seed, &context);
            }
        }
    }
}

/// The aliasing case: destination is ECX itself, so CL is the low byte of the register being
/// shifted. This is what proves the read of `home(1)` happens before anything writes it -- the
/// emitter's own comment states RCX can never clobber `home(dst)`, and this row exercises the one
/// case where `dst` IS the register CL lives in.
#[test]
fn shl_cx_cl_when_the_destination_is_ecx() {
    for (label, op) in SUB_OPS {
        for count in [0u8, 1, 5, 15, 31] {
            // High byte 0x34 keeps CX distinguishable from the count in the low byte.
            let operand = 0x3400 | u16::from(count);
            let seed = Seed::new().operand(1, operand, count);
            let context = format!("{label} cx={operand:#06x} (=cl) (consumed)");
            lowered(&with_consumers(true, op, 1), CONSUMED_SLOTS, seed, &context);
        }
    }
}

/// `0xD3` reaches no count lane: `count_lane_for` bars on `matches!(insn.opcode, 0xc0 | 0xc1)`,
/// which does not name `0xd3`, so an admitted Word shift-by-CL compiles with no lane attached at
/// all. This is the `0xC1` compiler-panic shape's regression twin: a lane wrongly attaching here
/// would produce a `DirectKind::ShiftCl` with a `lane` field that does not exist, a compile error
/// rather than a runtime one, so this fixture is a shape pin (the block compiles and installs)
/// rather than a field inspection.
#[test]
fn a_word_shift_by_cl_takes_no_count_lane() {
    let seed = Seed::new().operand(3, 0x1234, 3);
    lowered(
        &with_consumers(true, 4, 3),
        CONSUMED_SLOTS,
        seed,
        "/4 shl bx, cl takes no count lane",
    );
}

/// `0xD3 /0../3` (ROL, ROR, RCL, RCR) stay refused at Word: `classify`'s `0xd3` arm narrows to
/// `matches!(m.reg, 4..=7)` before `word_shift_cl_rows_enabled` is even reached, so admitting the
/// opcode under the knob cannot sweep the rotates in.
#[test]
fn the_rotate_sub_ops_stay_refused_at_word() {
    for op in 0u8..4 {
        let body = shift_cl_form(true, op, 3);
        let mut code = MOV_ESI_ESI.to_vec();
        code.extend_from_slice(&body);
        code.extend_from_slice(&MOV_EDI_EDI);
        code.push(0xf4);
        let mut memory = memory_fill();
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_cpu();
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        cpu.registers.set_esp(STACK_TOP);
        for offset in 0..code.len() as u32 {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            let _ = cpu.fetch_decoded(&mut bus, linear);
        }
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, true),
                jit::direct::CompileOutcome::StructuralReject(_)
                    | jit::direct::CompileOutcome::Retry(_)
            ),
            "0xd3 /{op} must stay refused at Word"
        );
    }
}

/// The barrier census control and this row's mutation bite: with `IZARRAVM_WORD_SHIFT_CL_ROWS`
/// off, `0xd3 /4` register word is a `hard_boundary` census row; with it on, no `0xd3` row of
/// this shape survives. Keyed on `operand_form == "register"` for the same reason the ALU-rows
/// census control is (finding 12 gap 1 of the review): a memory-form `0xd3` refusal (still
/// refused either way, no arm reaches it) must not make this flaky.
#[test]
fn the_word_shift_cl_row_flips_with_the_gate() {
    fn fixture_cpu_and_bus() -> (CpuGsw, TestBus) {
        let body = shift_cl_form(true, 4, 3);
        let mut code = MOV_ESI_ESI.to_vec();
        code.extend_from_slice(&body);
        code.extend_from_slice(&MOV_EDI_EDI);
        code.push(0xf4);
        let mut memory = memory_fill();
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

        let mut cpu = flat_cpu();
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        cpu.registers.set_esp(STACK_TOP);
        cpu.enable_direct_barrier_census(true);
        for offset in 0..code.len() as u32 {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            let _ = cpu.fetch_decoded(&mut bus, linear);
        }
        (cpu, bus)
    }

    {
        jit::direct::set_word_shift_cl_rows_for_test(Some(false));
        let (mut cpu, _bus) = fixture_cpu_and_bus();
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
            .expect("the census was enabled");
        let row = snapshot
            .rows
            .iter()
            .find(|row| row.opcode == 0xd3 && row.operand_form == "register")
            .expect("the OFF arm must record its own 0xd3 register-form barrier row");
        assert_eq!(
            row.stop_reason.to_string(),
            "hard_boundary",
            "the OFF arm's row must be a coverage barrier, or this fixture compares the wrong \
             base"
        );
    }
    {
        jit::direct::set_word_shift_cl_rows_for_test(Some(true));
        let (mut cpu, _bus) = fixture_cpu_and_bus();
        assert!(
            matches!(
                jit::direct::compile(&mut cpu, ENTRY, true),
                jit::direct::CompileOutcome::Compiled(_)
            ),
            "the ON arm must admit the Word shift-by-CL row"
        );
        let snapshot = cpu
            .direct_barrier_census_snapshot()
            .expect("the census was enabled");
        assert!(
            snapshot
                .rows
                .iter()
                .all(|row| !(row.opcode == 0xd3 && row.operand_form == "register")),
            "the ON arm compiled the block and must not also have recorded an 0xd3 barrier row"
        );
    }
    jit::direct::set_word_shift_cl_rows_for_test(None);
}
