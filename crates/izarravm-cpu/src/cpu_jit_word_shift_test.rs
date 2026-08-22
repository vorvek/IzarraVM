// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The rejected-row campaign's Slice 3b: the sixteen-bit REGISTER shift lane, `0xC1 /4../7`.
//!
//! This slice is a REPAIR rather than an opportunity. Slice 3 lowered quake's `0x0FB6` MOVZX
//! memory-word row and 30,692 of its exits RELOCATED one instruction along, onto
//! `0xC1 /4` SHL r16,imm8 -- the census names the pair to the unit as `movzx cx, byte [..]`
//! followed by `shl cx, imm8`. Quake's blocks now stop on a barrier the old boundary was hiding,
//! which the Slice 3 ladder priced at +8.78% blocks installed, +48.08% arena compactions and
//! roughly 1% of wall.
//!
//! | row | quake exits (post-slice-3 census) |
//! |---|---:|
//! | `0xC1 /7` SAR r16, imm8 | 63,039 |
//! | `0xC1 /4` SHL r16, imm8 | 62,934 |
//! | `0xC1 /5` SHR r16, imm8 | 0 exits, 1 runtime hit |
//!
//! Doom measures no `0xC1` row at any width.
//!
//! ## What the lowering is, and the one thing that could have gone wrong
//!
//! `emit_shift` had no sixteen-bit lane. The tempting shape -- a 32-bit host shift over a
//! zero-extended operand -- is wrong in five separate ways at once, and each of them is a rule
//! `CpuGsw::shift_rotate` derives from its `BusWidth` argument:
//!
//! * **CF** is the last bit shifted OUT: bit 15 for SHL/SAL, bit 0 for SHR/SAR. A 32-bit shift
//!   takes it from bit 31.
//! * **OF** is defined only at a masked count of exactly 1. SHL is `msb(result) ^ CF` with the msb
//!   at bit 15; SHR is the msb of the ORIGINAL operand; SAR is always false.
//! * **SF** is bit 15 of the result, **ZF** is the 16-bit result being zero, **PF** is the parity
//!   of its low byte.
//! * **SAR** shifts in bit 15, so a 32-bit arithmetic shift of a zero-extended operand shifts in
//!   zeros and produces a different value, not merely different flags.
//! * The **destination write** is `write_gpr16`, a MERGE into the low 16 that preserves the high
//!   16. A 32-bit shift defines all 32.
//!
//! The fix is one instruction: `66 C1 /op` narrows all five at once, because an x86-64 16-bit
//! shift computes every flag against its own 16 bits and writes only the low 16 of the register.
//! So the Word arm differs from the Dword arm by the encoder call and by nothing else -- the
//! `defined` mask, the eager publish to `eflags` and the `emit_clear_pending` are width-invariant,
//! because `set_shift_result_flags` materializes and writes live at either width.
//!
//! ## Non-vacuity: the descriptor is CONSUMED, not merely written
//!
//! A shift DESTROYS a lazy descriptor rather than creating one, so a fixture that only compared
//! `pending_flags` at the end would pass on a lowering that published garbage into a field nothing
//! reads. Every row here therefore ends with FIVE `SETcc` slots -- `setc`, `seto`, `sets`, `setz`,
//! `setp` -- inside the SAME block, into five byte registers that are not the shift's destination.
//! `SETcc` lowers through `emit_load_host_flags`, which reloads the host flags from RBP, so those
//! five slots read the shift's published flags back through EMITTED code and turn each one into an
//! architectural byte the differential compares. A wrong CF is a wrong BL, not a subtle EFLAGS
//! bit.
//!
//! ## Counts of 16 to 31 are architecturally undefined and are lowered anyway
//!
//! Intel documents result and flags as undefined once a shift count reaches the operand size, so
//! `shl ax, 20` is outside the architecture. The reference this tree matches is its own
//! interpreter, and `word_shifts_match_the_interpreter_for_every_count` sweeps all 32 counts for
//! exactly that reason: the host and `shift_rotate`'s single-bit loop agree across the whole range
//! on this box, and pinning it as a test means a host that disagreed would fail the suite loudly
//! rather than miscompile quietly. Refusing the range instead would have been safe but blind --
//! the census cannot say which immediates quake's `shl cx, imm8` sites use, so a count-gated
//! admission could have lowered nothing at all.
//!
//! ## Every EFLAGS seed here has bit 1 set, and that is a domain fact rather than a convenience
//!
//! `emit_shift` publishes the shadow with `store_r32_disp32(eflags, RBP)` and does NOT `or` in the
//! reserved bit 1, where the interpreter's `set_flag_live` ends every write with
//! `self.registers.eflags |= 0x2`. Seeding `registers.eflags` with bit 1 CLEAR therefore produces
//! a one-bit disagreement -- and it does so on the DWORD lane identically, which is how this was
//! established as pre-existing rather than as something this slice introduced: the first draft of
//! the flag row used `0x8d5`, and a probe row built from the same seed against the unprefixed form
//! failed the same way (2065 against 2067).
//!
//! It is not reachable from guest code. x86 hardwires EFLAGS bit 1 to 1, and this tree reproduces
//! that at both writers: `set_flag_live` ors `0x2` on every single-flag write, and
//! `control.rs::load_flags` ors `0x2` into the merged image for POPF, POPFD, IRET and every other
//! flag load at both operand sizes. `an_eflags_image_with_bit_one_clear_is_not_reachable` pins
//! both, so the seeds below are the guest's real domain rather than an assumption about it.
//!
//! Mutation record. Six, all applied by hand, observed, and restored, and every one re-checked
//! against the WHOLE crate rather than under a test filter -- Slice 3 recorded that a filtered
//! mutation run understates the blast radius as readily as it understates the catch, and this file
//! took that lesson as a precondition rather than rediscovering it.
//!
//! The `failing` column is the whole-suite count against a clean 1324-pass baseline.
//!
//! | mutation | failing | caught by | first failing assertion |
//! |---|---:|---|---|
//! | `emit_shift`'s Word arm -> `shift_r32_imm8` (the pre-slice emitter verbatim) | 4 | all four `word_shifts_*` / count rows; NO dword row moves | registers, at `/4 shl cx=0x8000 count=1` |
//! | `emit_shift`'s Dword arm -> `shift_r16_imm8` (the field wired backwards) | 7 | `dword_shifts_still_define_*` plus SIX pre-existing rows | the differential generator's `GeneratedCase` |
//! | `emit_shift`'s `count == 0` early return deleted | 3 | `a_zero_count_*`, both count rows | registers, at `/4 shl count=0 osz=dword` |
//! | `emit_shift`'s `count == 1` OF arm -> unconditional `defined \|= FLAG_OF` | 8 | all five rows here plus the three generator rows | the generator's `GeneratedCase` |
//! | `emit_shift`'s five-bit mask `raw_count & 0x1f` -> `raw_count` | 1 | `the_five_bit_count_mask_*` alone | registers, at `/4 shl cx=0x8001 raw count=32` |
//! | classify's Word ROR refusal deleted | 2 | `the_word_size_group_two_shapes_*` + the pre-existing `group2_non_lowered_rotates_remain_interpreter_only` | span instruction count, 4 where 3 is the refusal |
//!
//! The first row is the one that matters: it is the pre-slice emitter, so it says this fixture
//! would have refused the admission had it been written before the `width` field rather than with
//! it. The second is its mirror -- it says the fixture would also have caught the field wired the
//! wrong way round, which is the failure mode `ExtendWidths` was introduced for one slice earlier.
//!
//! Two readings the numbers force, and both were guesses before the runs:
//!
//! * **Only the mask mutation is caught by this file alone**, and it is caught by exactly one row.
//!   Everything else here has co-catchers, most of them pre-existing: swapping the Dword arm or
//!   widening the OF mask breaks paths the differential generator has covered since long before
//!   this slice. The blast radius is the honest measure and it is wider than the intent.
//! * **The Word arm mutation moves NO dword row**, which is the separation this slice's `width`
//!   field is for. Had the two lanes shared a rounding of any kind, a Word-only edit would have
//!   shown up on the Dword coverage, and it does not.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry. An opcode at
/// a block's ENTRY never executes natively, so an entry-position fixture certifies nothing.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The five flag consumers, as `(condition, byte destination)`.
///
/// Between them they read every flag a shift DEFINES. The byte destinations deliberately avoid CL
/// and CH: the sweeps that use consumers shift CX, and a `SETcc` into either half of it would
/// overwrite the result being compared.
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

/// `C1 /op ib` on a register destination, optionally 66-prefixed.
fn shift_form(word: bool, op: u8, dst: u8, count: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    if word {
        bytes.push(0x66);
    }
    bytes.extend_from_slice(&[0xc1, 0xc0 | (op << 3) | dst, count]);
    bytes
}

/// `0xD1`, the shift-by-one encoding. Two bytes plus the prefix, with no immediate: the count is
/// architectural, and `classify` supplies it as `if opcode == 0xd1 { 1 }`.
fn shift_by_one_form(word: bool, op: u8, dst: u8) -> Vec<u8> {
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
/// `gpr` poisons every register's HIGH half. That is the whole point for this slice: a Word shift
/// that ran as a Dword one clears bits 31..16, and with a zero seed nothing in the comparison
/// would notice.
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
/// `slots` is the EXACT instruction count the block must cover. An exact count rather than a
/// lower bound is what says the tested opcode joined the block instead of ending it -- a `>=`
/// assertion is satisfied by the fillers alone with the form under test refused.
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
        // Warm every decode line the block covers, one byte at a time rather than at the three
        // slot boundaries: the body here is up to seven instructions and the compile loop needs a
        // decode for each of them.
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
            panic!("structurally rejected: the sixteen-bit shift is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the tested opcode really ran natively"
    );
    // A register shift touches no memory at any width. This is what would catch a Word arm that
    // reached for a memory form of the shift by mistake.
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
            // A DWORD descriptor produced BEFORE the tested instruction. A shift must destroy it
            // and publish live flags; a zero-count shift must leave it exactly as it is.
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
        roles.native.registers, roles.interp.registers,
        "{context}: registers"
    );
    assert_eq!(
        roles.native.pending_flags, roles.interp.pending_flags,
        "{context}: lazy flags"
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
    // The whole array. A register shift must write no guest RAM at all, and a window would be the
    // wrong shape to see a stray store.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY: every slot retires in the block and the whole architectural
/// state matches the same number of interpreted steps.
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

/// The shift under test plus the five flag consumers, as one block body. Seven slots with the two
/// fillers.
fn with_consumers(word: bool, op: u8, dst: u8, count: u8) -> Vec<u8> {
    let mut body = shift_form(word, op, dst, count);
    body.extend_from_slice(&consumer_bytes());
    body
}

const CONSUMED_SLOTS: u8 = 2 + 1 + CONSUMERS.len() as u8;

/// The four admitted sub-ops, `(label, reg field)`. Listed rather than ranged, for the reason the
/// allowlist tests give: a range hides a member, and `/6` is the undocumented SAL alias of `/4`
/// that the host encodes identically and the interpreter treats as `4 | 6` in one arm.
const SUB_OPS: [(&str, u8); 4] = [("/4 shl", 4), ("/5 shr", 5), ("/6 sal", 6), ("/7 sar", 7)];

// ---------------------------------------------------------------------------------------------
// The count axis
// ---------------------------------------------------------------------------------------------

/// Every count from 0 to 31 on every admitted sub-op, with the flag consumers reading the result
/// back through emitted code.
///
/// The count sweep is not decoration. Three of `emit_shift`'s four shapes are selected on the
/// MASKED count -- 0 emits nothing, 1 adds OF to the defined mask, and 2..=31 do not -- and the
/// boundaries between them (0, 1, 2, 31, and the wrap at 32) are where a mask applied to the raw
/// immediate instead of the architectural one diverges.
///
/// Counts 16..=31 are architecturally UNDEFINED for a 16-bit operand and are swept anyway, because
/// the reference here is the interpreter and the point of the row is to pin that the host agrees
/// with it. See the module docs.
#[test]
fn word_shifts_match_the_interpreter_for_every_count() {
    for (label, op) in SUB_OPS {
        for count in 0..=31u8 {
            for operand in [0x0001u16, 0x8001, 0x7fff, 0xffff, 0x1234] {
                let seed = Seed::new().gpr(1, u32::from(operand));
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

/// `0xD1`, the shift-by-one encoding, executed rather than merely admitted.
///
/// The 16-bit campaign's third slice put `0xd1` on the Word allowlist, and the whole argument for
/// it costing no emitter work is that `0xd1` and `0xc1` with an immediate of 1 produce the same
/// `DirectKind::Shift`. That argument has exactly one line of its own to get wrong,
/// `let count = if opcode == 0xd1 { 1 } else { insn.imm as u8 }`, and until this test nothing
/// executed it: every other `0xD1` fixture asserts block length only.
///
/// The failure it guards is silent. `decode` leaves `insn.imm` at its default for `0xd0..=0xd3`,
/// so a lowering that read the immediate instead of supplying 1 gets count 0, `emit_shift`
/// returns without emitting anything, and the shift disappears with every flag left untouched.
/// Both roles would agree on a wrong answer if the interpreter shared the mistake; it does not,
/// so the two diverge and this fails.
///
/// Count 1 is also the one count where OF is architecturally DEFINED for a shift, so the flag
/// consumers matter here as much as the register result. Both widths are swept: `0xD1` at Dword
/// shipped before this slice and must not move.
#[test]
fn shift_by_one_matches_the_interpreter_at_both_widths() {
    for (label, op) in SUB_OPS {
        for word in [true, false] {
            for operand in [0x0001u16, 0x4000, 0x8001, 0x7fff, 0xffff, 0x1234] {
                let seed = Seed::new().gpr(1, u32::from(operand));
                let osz = if word { "word" } else { "dword" };
                let context = format!("0xd1 {label} cx={operand:#06x} osz={osz} (consumed)");
                let mut body = shift_by_one_form(word, op, 1);
                body.extend_from_slice(&consumer_bytes());
                lowered(&body, CONSUMED_SLOTS, seed, &context);
            }
        }
    }
}

/// The count wrap. `shl ax, 32` masks to zero and is a no-op that touches NO flag, and `shl ax,
/// 33` masks to one and therefore DOES define OF.
///
/// `classify` stores the immediate raw, so this is what catches a mask applied at the wrong place:
/// testing `raw_count` instead of `raw_count & 0x1f` reads 32 as a shift by 32 rather than the
/// no-op it is, and reads 33 as a multi-bit shift rather than the single-bit one that defines OF.
#[test]
fn the_five_bit_count_mask_is_applied_to_the_architectural_count() {
    for (label, op) in SUB_OPS {
        for count in [32u8, 33, 34, 63, 64, 0xff] {
            for operand in [0x8001u16, 0xffff] {
                for pending in [false, true] {
                    let mut seed = Seed::new().gpr(1, u32::from(operand));
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
/// `shift_rotate` returns before touching the value or a flag, so a zero-count shift neither
/// creates a descriptor nor destroys one. The consumers here are the assertion that matters: with
/// a DWORD descriptor live from `0x7fff_ffff + 1`, the five `SETcc` bytes must read that
/// descriptor's flags, not the shift's. An emitter that published RBP to `eflags` and cleared the
/// descriptor would produce the same `eflags()` in most seeds and a different `pending_flags`, so
/// both are compared.
#[test]
fn a_zero_count_shift_leaves_every_flag_and_a_live_descriptor_alone() {
    for (label, op) in SUB_OPS {
        for word in [false, true] {
            for operand in [0x0000u16, 0x8001, 0xffff] {
                for pending in [false, true] {
                    for eflags in [0x202u32, 0x8d7] {
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
// The destination axis
// ---------------------------------------------------------------------------------------------

/// Every destination register at Word size, and the property that admits the opcode at all: the
/// shift defines the destination's low 16 bits and PRESERVES its high 16.
///
/// No consumers on this row, deliberately -- a `SETcc` writes a byte register and would overwrite
/// the low byte of whichever destination it collides with, which is exactly the half of the
/// result this row exists to compare. The seeds carry `0xdead` in every high half, so a lowering
/// that ran the Dword shift here fails on `registers` at the first case.
#[test]
fn word_shifts_write_the_operand_size_and_preserve_what_is_above_it() {
    for (label, op) in SUB_OPS {
        for dst in 0..8u8 {
            for count in [1u8, 2, 3, 8, 15, 16, 31] {
                for operand in [0x8001u16, 0x7fff, 0xffff] {
                    let seed = Seed::new().gpr(usize::from(dst), u32::from(operand));
                    let context =
                        format!("{label} dst={dst} operand={operand:#06x} count={count} osz=word");
                    lowered(&shift_form(true, op, dst, count), 3, seed, &context);
                }
            }
        }
    }
}

/// The DWORD control, and it is not decoration.
///
/// This is the row that says the `width` field would have been caught wired BACKWARDS. With the
/// two arms swapped, the unprefixed form stops defining all 32 bits of its destination and every
/// case here fails on `registers`. The seeds put a value in the whole 32 bits rather than only
/// the low 16 for that reason.
#[test]
fn dword_shifts_still_define_the_whole_destination() {
    for (label, op) in SUB_OPS {
        for dst in 0..8u8 {
            for count in [1u8, 3, 16, 31] {
                for operand in [0x8000_0001u32, 0x7fff_ffff, 0xffff_ffff, 0x0001_0000] {
                    let mut seed = Seed::new();
                    seed.gpr[usize::from(dst)] = operand;
                    let context = format!(
                        "{label} dst={dst} operand={operand:#010x} count={count} osz=dword"
                    );
                    lowered(&shift_form(false, op, dst, count), 3, seed, &context);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The flag axis
// ---------------------------------------------------------------------------------------------

/// The sixteen-bit flag boundaries, swept against both incoming EFLAGS polarities and both
/// descriptor states.
///
/// The operands are chosen so that CF, OF, SF and ZF each disagree between a 16-bit and a 32-bit
/// derivation on at least one of them:
///
/// * `0x8000` at count 1 -- SHL carries out of bit 15 (CF set at Word, clear at Dword) and the
///   result is zero at Word but not at Dword.
/// * `0x0001` at count 15 -- SHL lands the bit on bit 15, so SF is set at Word and clear at Dword.
/// * `0xffff` under SAR -- the guest shifts in bit 15, so the result stays `0xffff`; a Dword
///   arithmetic shift over the same zero-extended value shifts in zeros.
/// * `0x4000` at count 1 -- SHL sets OF at Word (msb changes) and clears it at Dword.
#[test]
fn word_shifts_derive_every_flag_from_sixteen_bits() {
    let cases: [(u16, u8); 8] = [
        (0x8000, 1),
        (0x8000, 2),
        (0x0001, 15),
        (0x0001, 16),
        (0x4000, 1),
        (0xffff, 1),
        (0xffff, 15),
        (0x00ff, 1),
    ];
    for (label, op) in SUB_OPS {
        for (operand, count) in cases {
            for eflags in [0x202u32, 0x8d7] {
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

/// The domain claim behind every EFLAGS seed in this file: bit 1 is never zero in a guest.
///
/// `emit_shift` publishes RBP straight into `eflags` without re-asserting the reserved bit, so a
/// state with bit 1 clear would diverge -- at BOTH widths, which is what makes this a property of
/// the seed rather than of this slice. The two writers that could produce such a state are pinned
/// here instead of the claim being taken on trust: a single-flag write goes through
/// `set_flag_live`, and every wholesale load (POPF, POPFD, IRET) goes through `load_flags`.
#[test]
fn an_eflags_image_with_bit_one_clear_is_not_reachable() {
    let mut cpu = flat_cpu();

    // A single-flag write, from a state whose bit 1 has been forced clear behind the interpreter's
    // back. `set_flag` must restore it whether the flag it is asked to write is set or cleared.
    for enabled in [false, true] {
        cpu.registers.eflags = 0x8d5;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_flag(crate::FLAG_CF, enabled);
        assert_eq!(
            cpu.registers.eflags & 0x2,
            0x2,
            "set_flag(CF, {enabled}) must leave the reserved bit set"
        );
    }

    // A wholesale load at both operand sizes, with bit 1 clear in the popped image.
    for operand_size in [OperandSize::Word, OperandSize::Dword] {
        cpu.registers.eflags = 0x8d5;
        cpu.load_flags(0x0000_0845, operand_size, false);
        assert_eq!(
            cpu.registers.eflags & 0x2,
            0x2,
            "load_flags({operand_size:?}) must force the reserved bit set"
        );
    }
}
