// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `vorvek/direct-word-rot1`: the sixteen-bit REGISTER rotate lane, `0xC1`/`0xD1 /0,/1`.
//!
//! `dev_docs/2026-08-31-corpus-lever-plan.md`'s L3: `123-talk-shareware`'s `0xD1 word memory /1`
//! and `0xD0 word register /1` rows are 29,698,883 + 24,856,330 runtime hits, its #1 and #3 census
//! rows and 3.4% of everything it executes; `21-for-1-to-4`'s `0xD0 word register /1` is
//! 13,496,909. The census names are the by-one encoding and the decoded segment default width, not
//! a literal `0xD0` opcode with a rotate sub-opcode -- `0xD0` stays byte-only and its rotate
//! sub-opcodes stay refused, see `jit::direct`'s `0xd0` classify arm. What this file certifies is
//! the `0xc1 | 0xd1` arm's Word admission, `RotateReg { width: MemoryWidth::Word, .. }`.
//!
//! ## The flag contract, and why it is NOT `emit_shift`'s
//!
//! A rotate by 1 defines exactly two flags and leaves the rest exactly where they were:
//!
//! * **CF** is the bit rotated OUT across the boundary -- the MSB for ROL, bit 0 for ROR. At Word
//!   that boundary is bit 15, not bit 31.
//! * **OF** is defined ONLY at a masked count of exactly 1: `MSB(result) XOR CF` for ROL,
//!   `MSB(result) XOR MSB-1(result)` for ROR (`core.rs::shift_rotate`'s `1 | 3 => top ^ ((v &
//!   (msb >> 1)) != 0)`, which is bit 15 XOR bit 14 at Word). Above count 1 it is undefined and this
//!   tree's oracle -- the interpreter -- leaves the PREVIOUS value in place, which the host
//!   reproduces by construction: `emit_rotate_reg` does not capture OF at all past count 1.
//! * **SF, ZF, PF and AF are UNTOUCHED at every count**, including 1. This is the property `Shift`
//!   does NOT have -- a shift redefines SF/ZF/PF at every non-zero count -- and it is why
//!   `RotateReg` is a separate `DirectKind` with a separate emitter rather than a fold into
//!   `Shift`'s Word arm. `emit_rotate_reg`'s count-1 branch captures `CF|OF` only and its 2..=31
//!   branch captures CF alone through `emit_set_cf_only`, which is the whole reason a rotate cannot
//!   share `emit_commit_shift_flags`.
//!
//! Every fixture below therefore seeds EFLAGS with a domain-real prior state (SF, ZF, PF and AF all
//! set, bit 1 forced -- `0x8d7`, the same seed `cpu_jit_word_shift_test.rs` establishes as reachable
//! and non-vacuous) and asserts those four bits survive the rotate UNCHANGED, on top of the
//! ordinary differential against the interpreter.
//!
//! ## High-half preservation
//!
//! `shift_r16_imm8` emits a 66-prefixed `C1 /op` exactly as `emit_shift`'s Word arm does, and the
//! same host property applies: a 16-bit x86-64 instruction writes only its destination's low 16
//! bits and leaves bits 16..=63 of the underlying 64-bit register untouched. Nothing in
//! `emit_rotate_reg` has to ask for that; the seeds below poison every register's high half with
//! `0xdead` so a lowering that ran the Dword rotate here would be caught on `registers`.
//!
//! ## Boundary operands
//!
//! `0x8000` (MSB set, rest clear), `0x0001` (LSB set, rest clear), `0x5555` and `0xaaaa`
//! (alternating bit patterns, so every rotate amount exercises both a 0-to-1 and a 1-to-0 crossing
//! at the boundary) are swept across every count and both directions.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry. An opcode at
/// a block's ENTRY never executes natively, so an entry-position fixture certifies nothing.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The five flag consumers, as `(condition, byte destination)`.
///
/// Between them they read CF, OF, SF, ZF and PF back through EMITTED code, so a wrong flag is a
/// wrong byte register rather than a subtle EFLAGS bit a raw comparison could paper over. The byte
/// destinations avoid CL and CH: the sweeps below rotate CX, and a `SETcc` into either half of it
/// would overwrite the very result being compared.
const CONSUMERS: [(u8, u8); 5] = [
    (0x2, 3), // setc bl  -- CF
    (0x0, 6), // seto dh  -- OF
    (0x8, 4), // sets ah  -- SF
    (0x4, 2), // setz dl  -- ZF
    (0xa, 0), // setp al  -- PF
];

/// `0F 9x /r` with ModRM mod=11, so `rm` is the byte register AL/CL/DL/BL/AH/CH/DH/BH.
fn consumer_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    for (condition, dst) in CONSUMERS {
        bytes.extend_from_slice(&[0x0f, 0x90 | condition, 0xc0 | dst]);
    }
    bytes
}

/// `C1 /op ib` on a register destination, optionally 66-prefixed. `op` 0 is ROL, 1 is ROR.
fn rotate_form(word: bool, op: u8, dst: u8, count: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    if word {
        bytes.push(0x66);
    }
    bytes.extend_from_slice(&[0xc1, 0xc0 | (op << 3) | dst, count]);
    bytes
}

/// `0xD1`, the rotate-by-one encoding. Two bytes plus the prefix, with no immediate: the count is
/// architectural, and `classify` supplies it as `if opcode == 0xd1 { 1 }`.
fn rotate_by_one_form(word: bool, op: u8, dst: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    if word {
        bytes.push(0x66);
    }
    bytes.extend_from_slice(&[0xd1, 0xc0 | (op << 3) | dst]);
    bytes
}

/// A distinct byte at every address, so a stray write of any width is visible in the whole-RAM
/// compare rather than hidden by a zero fill matching a zero store.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// The architectural state both roles start from.
///
/// `gpr` poisons every register's HIGH half. That is the whole point for this slice: a Word rotate
/// that ran as a Dword one clears bits 31..16, and with a zero seed nothing in the comparison would
/// notice.
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

    fn gpr(mut self, index: usize, value: u32) -> Self {
        self.gpr[index] = 0xdead_0000 | u32::from(value as u16);
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
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    slots: u8,
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, and seed both identically.
///
/// `slots` is the EXACT instruction count the block must cover. An exact count rather than a lower
/// bound is what says the tested opcode joined the block instead of ending it -- a `>=` assertion
/// is satisfied by the fillers alone with the form under test refused.
fn build(body: &[u8], slots: u8, seed: Seed) -> Roles {
    let mut code = MOV_ESI_ESI.to_vec();
    let mut starts = vec![ENTRY, ENTRY + code.len() as u32];
    code.extend_from_slice(body);
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(0xf4);

    let mut memory = memory_fill();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
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
        // ESP must be live BEFORE compiling: an unresolvable store page returns the whole block as
        // Retry, which is indistinguishable from the opcode still being a barrier.
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
            panic!("structurally rejected: the sixteen-bit rotate is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the tested opcode really ran natively"
    );
    // A register rotate touches no memory at any width. This is what would catch a Word arm that
    // reached for a memory form of the rotate by mistake.
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
            // A DWORD descriptor produced BEFORE the tested instruction. A count-1 rotate must
            // destroy it and publish live flags; a count-0 rotate, or a count above 1, must leave it
            // exactly as it is (count above 1 only touches CF, which the descriptor path handles
            // through `emit_set_cf_only` rather than by materializing).
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
    // The whole array. A register rotate must write no guest RAM at all, and a window would be the
    // wrong shape to see a stray store.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY: every slot retires in the block and the whole architectural state
/// matches the same number of interpreted steps.
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

/// The rotate under test plus the five flag consumers, as one block body. Seven slots with the two
/// fillers.
fn with_consumers(word: bool, op: u8, dst: u8, count: u8) -> Vec<u8> {
    let mut body = rotate_form(word, op, dst, count);
    body.extend_from_slice(&consumer_bytes());
    body
}

const CONSUMED_SLOTS: u8 = 2 + 1 + CONSUMERS.len() as u8;

/// The two admitted sub-ops, `(label, reg field)`.
const SUB_OPS: [(&str, u8); 2] = [("/0 rol", 0), ("/1 ror", 1)];

/// A domain-real prior EFLAGS state with SF, ZF, PF and AF all SET, and bit 1 forced -- the same
/// `0x8d7` seed `cpu_jit_word_shift_test.rs` establishes as reachable
/// (`an_eflags_image_with_bit_one_clear_is_not_reachable`'s sibling in that file). Used everywhere
/// this file asserts PRESERVATION: a rotate must leave these four bits exactly here.
const SEEDED_EFLAGS: u32 = 0x8d7;

/// The boundary operands named in the module header: MSB-only, LSB-only, and the two alternating
/// patterns, so every rotate amount crosses the boundary both ways.
const BOUNDARY_OPERANDS: [u16; 4] = [0x8000, 0x0001, 0x5555, 0xaaaa];

// ---------------------------------------------------------------------------------------------
// The count axis
// ---------------------------------------------------------------------------------------------

/// Every count from 0 to 31 on both sub-ops, over the boundary operands, with the flag consumers
/// reading CF and OF back through emitted code.
///
/// The count sweep pins the three-way split `emit_rotate_reg` makes on the masked count: 0 emits
/// nothing, 1 captures `CF|OF` and publishes the shadow, and 2..=31 capture CF alone. The boundaries
/// between them (0, 1, 2, 31, and the wrap at 32) are exactly where a mask applied to the raw
/// immediate instead of the architectural one diverges.
#[test]
fn word_rotates_match_the_interpreter_for_every_count() {
    for (label, op) in SUB_OPS {
        for count in 0..=31u8 {
            for operand in BOUNDARY_OPERANDS {
                let seed = Seed::new().gpr(1, u32::from(operand)).flags(SEEDED_EFLAGS);
                let context =
                    format!("{label} cx={operand:#06x} count={count} osz=word (consumed)");
                lowered(
                    &with_consumers(true, op, 1, count),
                    CONSUMED_SLOTS,
                    seed,
                    &context,
                );
            }
        }
    }
}

/// `0xD1`, the rotate-by-one encoding, executed rather than merely admitted.
///
/// The whole argument for `0xd1` costing no emitter work beyond the `width` field is that `0xd1`
/// and `0xc1` with an immediate of 1 produce the SAME `DirectKind::RotateReg`, and that argument has
/// exactly one line of its own to get wrong: `let count = if opcode == 0xd1 { 1 } else { insn.imm
/// as u8 }`. Both widths are swept: `0xD1` at Dword shipped before this slice and must not move.
#[test]
fn rotate_by_one_matches_the_interpreter_at_both_widths() {
    for (label, op) in SUB_OPS {
        for word in [true, false] {
            for operand in [
                0x0001u16, 0x4000, 0x8001, 0x7fff, 0xffff, 0x1234, 0x5555, 0xaaaa,
            ] {
                let seed = Seed::new().gpr(1, u32::from(operand)).flags(SEEDED_EFLAGS);
                let osz = if word { "word" } else { "dword" };
                let context = format!("0xd1 {label} cx={operand:#06x} osz={osz} (consumed)");
                let mut body = rotate_by_one_form(word, op, 1);
                body.extend_from_slice(&consumer_bytes());
                lowered(&body, CONSUMED_SLOTS, seed, &context);
            }
        }
    }
}

/// The count wrap. `rol ax, 32` masks to zero and is a no-op that touches NO flag, and `rol ax, 33`
/// masks to one and therefore DOES define CF and OF.
#[test]
fn the_five_bit_count_mask_is_applied_to_the_architectural_count() {
    for (label, op) in SUB_OPS {
        for count in [32u8, 33, 34, 63, 64, 0xff] {
            for operand in BOUNDARY_OPERANDS {
                for pending in [false, true] {
                    let mut seed = Seed::new().gpr(1, u32::from(operand)).flags(SEEDED_EFLAGS);
                    if pending {
                        seed = seed.pending();
                    }
                    let context =
                        format!("{label} cx={operand:#06x} raw count={count} pending={pending}");
                    lowered(
                        &with_consumers(true, op, 1, count),
                        CONSUMED_SLOTS,
                        seed,
                        &context,
                    );
                }
            }
        }
    }
}

/// A masked count of zero must leave EVERY flag and a live lazy descriptor exactly as they are.
///
/// `shift_rotate` returns before touching the value or a flag, so a zero-count rotate neither
/// creates a descriptor nor destroys one. With a DWORD descriptor live from `0x7fff_ffff + 1`, the
/// five `SETcc` bytes must read that descriptor's flags, not the rotate's.
#[test]
fn a_zero_count_rotate_leaves_every_flag_and_a_live_descriptor_alone() {
    for (label, op) in SUB_OPS {
        for word in [false, true] {
            for operand in BOUNDARY_OPERANDS {
                for pending in [false, true] {
                    for eflags in [0x202u32, SEEDED_EFLAGS] {
                        let mut seed = Seed::new().gpr(1, u32::from(operand)).flags(eflags);
                        if pending {
                            seed = seed.pending();
                        }
                        let context = format!(
                            "{label} count=0 cx={operand:#06x} pending={pending} \
                             eflags={eflags:#x} osz={}",
                            if word { "word" } else { "dword" }
                        );
                        lowered(
                            &with_consumers(word, op, 1, 0),
                            CONSUMED_SLOTS,
                            seed,
                            &context,
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The destination and high-half axis
// ---------------------------------------------------------------------------------------------

/// Every destination register at Word size, and the property that admits the opcode at all: the
/// rotate defines the destination's low 16 bits and PRESERVES its high 16.
///
/// No consumers on this row, deliberately -- a `SETcc` writes a byte register and would overwrite
/// the low byte of whichever destination it collides with, which is exactly the half of the result
/// this row exists to compare. The seeds carry `0xdead` in every high half, so a lowering that ran
/// the Dword rotate here fails on `registers` at the first case.
#[test]
fn word_rotates_write_the_operand_size_and_preserve_what_is_above_it() {
    for (label, op) in SUB_OPS {
        for dst in 0..8u8 {
            for count in [1u8, 2, 3, 8, 15, 16, 31] {
                for operand in BOUNDARY_OPERANDS {
                    let seed = Seed::new().gpr(usize::from(dst), u32::from(operand));
                    let context =
                        format!("{label} dst={dst} operand={operand:#06x} count={count} osz=word");
                    lowered(&rotate_form(true, op, dst, count), 3, seed, &context);
                }
            }
        }
    }
}

/// The DWORD control, and it is not decoration.
///
/// This is the row that says the `width` field would have been caught wired BACKWARDS. With the two
/// arms swapped, the unprefixed form stops defining all 32 bits of its destination and every case
/// here fails on `registers`. The seeds put a value in the whole 32 bits rather than only the low 16
/// for that reason.
#[test]
fn dword_rotates_still_define_the_whole_destination() {
    for (label, op) in SUB_OPS {
        for dst in 0..8u8 {
            for count in [1u8, 3, 16, 31] {
                for operand in [0x8000_0001u32, 0x7fff_ffff, 0xffff_ffff, 0x0001_0000] {
                    let mut seed = Seed::new();
                    seed.gpr[usize::from(dst)] = operand;
                    let context = format!(
                        "{label} dst={dst} operand={operand:#010x} count={count} osz=dword"
                    );
                    lowered(&rotate_form(false, op, dst, count), 3, seed, &context);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The flag axis: CF, OF, and explicit preservation of SF/ZF/PF/AF
// ---------------------------------------------------------------------------------------------

/// The sixteen-bit CF/OF boundaries, over both incoming EFLAGS polarities and both descriptor
/// states, plus an EXPLICIT check that SF, ZF, PF and AF survive from the seed unchanged -- the
/// property that separates a rotate from a shift.
///
/// * `0x8000` at count 1 ROL -- CF set (bit 15 rotated out), result `0x0001`, OF = MSB(result) XOR
///   CF = `0 XOR 1` = 1.
/// * `0x0001` at count 1 ROR -- CF set (bit 0 rotated out), result `0x8000`, OF = MSB(result) XOR
///   MSB-1(result) = `1 XOR 0` = 1.
/// * `0x5555`/`0xaaaa` at count 1 -- CF and OF both defined, and the alternating pattern means every
///   rotate amount up to 15 lands a different bit on the MSB, so the SF a shift would recompute
///   (and a rotate must NOT) would disagree with the seed at some count if this file's own claim
///   were false.
#[test]
fn word_rotates_derive_cf_and_of_from_sixteen_bits_and_preserve_the_rest() {
    let cases: [(u16, u8); 10] = [
        (0x8000, 1),
        (0x8000, 2),
        (0x0001, 1),
        (0x0001, 15),
        (0x0001, 16),
        (0x5555, 1),
        (0x5555, 15),
        (0xaaaa, 1),
        (0xaaaa, 15),
        (0x00ff, 1),
    ];
    for (label, op) in SUB_OPS {
        for (operand, count) in cases {
            for eflags in [0x202u32, SEEDED_EFLAGS] {
                for pending in [false, true] {
                    let mut seed = Seed::new().gpr(1, u32::from(operand)).flags(eflags);
                    if pending {
                        seed = seed.pending();
                    }
                    let context = format!(
                        "{label} cx={operand:#06x} count={count} eflags={eflags:#x} \
                         pending={pending}"
                    );
                    lowered(
                        &with_consumers(true, op, 1, count),
                        CONSUMED_SLOTS,
                        seed,
                        &context,
                    );
                }
            }
        }
    }
}

/// The preservation claim, stated as its own assertion rather than left implicit in the
/// differential above: from `SEEDED_EFLAGS` (SF, ZF, PF and AF all set), every count from 1 to 31
/// leaves those four bits set in the RESULTING eflags, at both sub-ops and every boundary operand.
///
/// This is the row a fold into `Shift`'s Word arm would fail immediately: `emit_shift`'s tail
/// publishes the whole RBP shadow to `eflags` at every non-zero count, which would overwrite SF/ZF/
/// PF with whatever the rotate's OWN result derives them to (and clear AF's descriptor bit), not
/// leave the seed's values in place.
#[test]
fn word_rotates_preserve_sign_zero_parity_and_aux_carry_from_the_seed() {
    const PRESERVED: u32 = crate::FLAG_SF | crate::FLAG_ZF | crate::FLAG_PF | crate::FLAG_AF;
    for (label, op) in SUB_OPS {
        for count in [1u8, 2, 3, 15, 16, 31] {
            for operand in BOUNDARY_OPERANDS {
                let seed = Seed::new().gpr(1, u32::from(operand)).flags(SEEDED_EFLAGS);
                let mut roles = build(&rotate_form(true, op, 1, count), 3, seed);
                assert!(
                    roles
                        .native
                        .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                        .unwrap(),
                    "{label} cx={operand:#06x} count={count}: block did not run natively"
                );
                let after = roles.native.eflags();
                assert_eq!(
                    after & PRESERVED,
                    SEEDED_EFLAGS & PRESERVED,
                    "{label} cx={operand:#06x} count={count}: SF/ZF/PF/AF must survive from the seed"
                );
            }
        }
    }
}

/// The domain claim behind every EFLAGS seed in this file: bit 1 is never zero in a guest, and
/// `SEEDED_EFLAGS` is a real reachable state (SF, ZF, PF, AF all set, bit 1 forced), not an
/// arbitrary bit pattern chosen to make the preservation assertion trivially pass.
#[test]
fn seeded_eflags_is_the_expected_shape() {
    assert_eq!(SEEDED_EFLAGS & 0x2, 0x2, "bit 1 must be set");
    assert_eq!(SEEDED_EFLAGS & crate::FLAG_SF, crate::FLAG_SF);
    assert_eq!(SEEDED_EFLAGS & crate::FLAG_ZF, crate::FLAG_ZF);
    assert_eq!(SEEDED_EFLAGS & crate::FLAG_PF, crate::FLAG_PF);
    assert_eq!(SEEDED_EFLAGS & crate::FLAG_AF, crate::FLAG_AF);
}
