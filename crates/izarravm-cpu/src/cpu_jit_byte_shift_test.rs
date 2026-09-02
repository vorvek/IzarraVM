// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The BYTE shift/rotate register rows: `0xD0 /4..=7` (a whole opcode that had no classifier arm
//! at all) and `0xC0 /5,/6,/7` -- behind `IZARRAVM_BYTE_SHIFT_ROWS`, plus the `0xC0`/`0xD0` entry
//! on the `OperandSize::Word` allowlist that lets a 16-bit code segment reach either of them.
//!
//! # The rows, from the census that ranked them
//!
//! tyrian-586 at main `0333d956`, `IZARRAVM_DIRECT_BARRIER_CENSUS=1`, plain release build. The
//! static-unbound `rejected` class is 22.23 M runtime hits and two rows of one family are
//! **14,540,543 of them, 65.4%**:
//!
//! | row | form | operand_size | `runtime_hits` |
//! |---|---|---|---:|
//! | `0xD0 /5` SHR r8, 1 | register | word | **13,933,316** |
//! | `0xC0 /5` SHR r8, imm8 | register | word AND dword, MERGED | 607,227 |
//!
//! `0xD0 /5` is the hottest single rejected row on the fixture by 4x. tyrian's code segment is
//! CS.D = 0, so its byte shifts decode at `OperandSize::Word` and were refused by the allowlist
//! BEFORE any arm was reached -- which is why a row with no 16-bit anything about it is tagged
//! `operand_size: word`.
//!
//! # What the lowering is: two `debug_assert`s
//!
//! The Byte emitter lane already existed and was already sub-opcode-generic. `emit_shift_reg8`'s
//! only dependence on `op` was a `debug_assert_eq!(op, 4)`; the encoder call under it,
//! `shift_r8_imm8`, asserts `op < 8` and pushes `op` straight into the ModRM `/op` slot with no
//! translation and no table. The same holds for the count-lane Byte arm. So this slice is a
//! classifier change plus the relaxation of two asserts, and the reason it is CORRECT at each new
//! sub-opcode is the reason the existing `/4` lane is correct: the emitted shift is a genuine
//! 8-bit host shift, so every flag is the host's rather than something the emitter reconstructs.
//!
//! | | interpreter (`shift_rotate`, `BusWidth::Byte`) | host `C0 /op` on DL |
//! |---|---|---|
//! | `/5` CF | bit 0 of the value before each step | bit 0, last bit shifted out |
//! | `/5` OF @1 | msb of the ORIGINAL operand, bit 7 | x86's SHR OF: msb of the original dest |
//! | `/7` value | shifts in bit 7 | arithmetic shift on an 8-bit operand |
//! | `/7` OF @1 | `false` | x86: SAR clears OF |
//! | SF / ZF / PF | bit 7 / 8-bit result / parity of the result | computed against the 8-bit operand |
//!
//! The tempting wrong shape -- a 32-bit host shift over the zero-extended byte -- is wrong in the
//! same five ways `cpu_jit_word_shift_test.rs` enumerates at 16 bits, and for `/7` it is wrong in
//! the VALUE and not merely the flags: a 32-bit arithmetic shift of a zero-extended byte shifts in
//! zeros where the guest shifts in bit 7. That is M12 below.
//!
//! # `/6` is admitted and NORMALISED to `/4` at classify
//!
//! `/6` is the undocumented SAL alias of `/4` and both ends of this tree already say so:
//! `core.rs` answers `4 | 6 => // SHL` and `4 | 6 => top ^ cf` for OF at count 1. The classifier
//! arms therefore emit `op: 4` for `m.reg == 6`, so `DirectKind::Shift` only ever carries a
//! documented sub-opcode and no part of this lowering depends on undocumented host behaviour. The
//! CENSUS still bins by `insn.modrm.reg` and still reports `/6`, which `the_census_rows_disappear_
//! with_the_gate` pins.
//!
//! # Non-vacuity, and why the 16-bit consumer is 66-prefixed
//!
//! A shift DESTROYS a lazy descriptor rather than creating one, so a fixture that only compared
//! `pending_flags` would pass on a lowering that published garbage into a field nothing reads.
//! Every differential row therefore ends with five `SETcc` slots -- `setc`, `seto`, `sets`,
//! `setz`, `setp` -- inside the SAME block, into byte registers that are not the shift's
//! destination, so a wrong CF is a wrong BL.
//!
//! `0x0F9x` is deliberately NOT on the `OperandSize::Word` allowlist, so in a CS.D = 0 segment the
//! UNPREFIXED `SETcc` is a barrier and a consumer frame built from it would end the block at slot
//! 3 rather than 8. The 16-bit frame therefore writes `66 0F 9x /r`, which decodes at
//! `OperandSize::Dword` in a 16-bit segment and so never meets the Word gate at all. The prefix is
//! architecturally inert on `SETcc` (the interpreter's arm calls `write_operand_u8` without
//! consulting `operand_size`), and `build`'s exact slot-count assertion is what would catch it if
//! that stopped being true.
//!
//! # Mutation record
//!
//! Twenty-one applied BY HAND to the committed tree, run, observed, and restored with
//! `git checkout -- <file>` -- which is exactly why each is applied to a COMMITTED tree: that
//! command discards every uncommitted change to the file, not only the mutation, and during this
//! slice it ate an uncommitted edit once before the rule was re-learned. Each was run against the whole `cpu_jit_byte_shift_test` module (its
//! own 15 rows plus the 2,073 the filter excludes are unaffected by a byte-shift edit); the four
//! SURVIVORS were then re-run against the WHOLE `izarravm-cpu` suite -- 2,067 tests, all green on
//! each -- because a survivor is the only outcome a filtered run can get wrong in the direction
//! that matters.
//!
//! | # | mutation | outcome, and the row that fired first |
//! |---|---|---|
//! | M1 | `width: MemoryWidth::Byte` -> `operand_width` in the `0xD0` arm | RED, 3 rows: both differentials + the high-byte row |
//! | M2 | `count: 1` -> `insn.imm as u8` in the `0xD0` arm | RED, the same 3 |
//! | M3 | `matches!(m.reg, 4..=7)` -> `0 \| 1 \| 4..=7` in the `0xD0` arm | RED, `byte_rotates_stay_a_hard_boundary_*` + the neighbour sweep |
//! | M4 | fold `0xd0` into the `0xc1 \| 0xd1` arm | RED, 7 rows |
//! | M5 | drop `0xd0` from the allowlist term | RED, 7 rows incl. `a_sixteen_bit_segment_block_compiles_through_shr_r8_1` |
//! | M6 | drop `0xc0` from the allowlist term | RED, 9 rows |
//! | M7 | drop the sub-opcode key from the allowlist term | **SURVIVES -- see below** |
//! | M8 | delete the `4 if !rotate_rows_enabled()` arm from `0xc0` | RED, `the_two_group_two_knobs_are_independent` ALONE |
//! | M9 | knob read moved below the `m.reg` test in the `0xD0` arm | **SURVIVES -- see below** |
//! | M10 | `emit_shift_reg8` assert reverted to `op == 4` | RED, 8 rows (debug panic) |
//! | M11 | `emit_shift_lane` Byte assert reverted to `op == 4` | RED, 10 rows (debug panic) |
//! | M12 | `emit_shift_reg8` body -> `shift_r32_imm8(op, home(dst), count)` | RED, both differentials + the high-byte row |
//! | M13 | widen `rotate_row_count_byte` to `0xc0 if matches!(reg, 4..=7)` | RED, `the_two_group_two_knobs_are_independent` ALONE |
//! | M14 | `rotate_row_count_byte`'s `0xc0 if reg == 4` -> `_ => None` | RED, the same row alone |
//! | M15 | drop the `count == 0` early return in `emit_shift` | RED, both differentials -- **but only after the fix below** |
//! | M16 | `count = raw_count & 0x1f` -> `raw_count` in `emit_shift` | RED, both differentials -- **same** |
//! | M17 | swap `emit_commit_shift_flags` / `emit_write_gpr8` in `emit_shift_reg8` | RED, 3 rows |
//! | M18 | `op: if m.reg == 6 { 4 }` -> `op: m.reg` (drop the normalisation) | **SURVIVES, by design** |
//! | M19 | `byte_shift_rows_enabled()`'s ENV path returns `true`, override intact | **SURVIVES since the flip -- see below** |
//! | M19b | the WHOLE of `byte_shift_rows_enabled()` returns `true` | RED, 4 rows |
//! | M20 | `"" => DEFAULT_ARM` -> `"" => false` in the parse table | RED since the flip, `byte_shift_rows_spelling_table_names_every_arm` |
//!
//! **M15 and M16 caught a real hole in this file, and the hole is the more useful finding.** On
//! the first run both SURVIVED. `emit_shift`'s count-0 return and its five-bit mask are reachable
//! from the byte rows only through the BAKED emitter, and on the shipped `IZARRAVM_COUNT_LANES`
//! arm every `0xC0` row here was taking a count lane instead -- while `0xD0`'s count is the
//! literal 1 and can never be 0 or 32. So the two rows that the design named as M15's and M16's
//! killers were exercising a different emitter than the mutants edited. The differentials now
//! force the count-lane arm OFF and the laned arm has sweeps of its own, which is
//! `cpu_jit_count_lane_test.rs`'s recorded lesson arriving here a second time.
//!
//! **The four survivors are recorded rather than papered over, and each has a reason.**
//!
//! * **M18 is a hygiene change, not a semantic one.** The host's `C0 /6` and `C0 /4` are the same
//!   instruction, so no differential can separate them. Inventing a test that pinned
//!   `DirectKind::Shift.op == 4` for a `/6` input would be a shape test dressed as a behaviour
//!   test. The design predicted this survivor by name.
//! * **M7 is behaviourally inert on this tree, which the design did NOT predict** -- it expected
//!   the neighbour sweep to kill it. Dropping the sub-opcode key lets `0xC0 /0..=/3` and
//!   `0xD0 /0..=/3` past the Word GATE, and they then hit `return None` inside their own arm one
//!   step later: the same `classify` answer, the same `hard_boundary` census row, the same compile
//!   outcome. There is nothing to assert on. The key stays in the source because it keeps both
//!   halves of the refusal stated in one place, so a later widening of the arms cannot silently
//!   widen what the Word gate admits -- but it buys no behaviour today and this record says so.
//! * **M9 is inert for the same class of reason.** Both orderings return `None` for `/0..=/3`, and
//!   the knob is a pure function of the environment with no side effect but its panic, which still
//!   fires the first time any gated row is classified.
//! * **M19 survives the ENV path alone, and only since the default flipped ON.** The mutant makes
//!   the `OnceLock` read return `true` while leaving the `#[cfg(test)]` override intact, so the
//!   only fixture that can see it is the default pin -- and with the knob unset the pin's expected
//!   value is now `true` as well, so the two agree. It died while the default was OFF and it would
//!   die again at any flip back. This is the blind spot EVERY default-ON knob in this file has,
//!   `IZARRAVM_TEST_WORD_ROWS` included, and it is structural rather than an oversight: a fixture
//!   cannot set the process-wide env the `OnceLock` reads without deciding the arm for every other
//!   test in the process. What covers it instead is the ladder, whose A leg exports an explicit
//!   `IZARRAVM_BYTE_SHIFT_ROWS=0` and records the RESOLVED arm -- a build that ignored the env
//!   would show up there as an A leg that measured the B arm. **M19b is the faithful form the
//!   design named** -- the whole function returns `true`, override included -- and it dies on four
//!   rows, because `force_byte_shift_rows` asserts that its own selection took.
//!
//! **What the record says about where the coverage actually lives.** Three mutants are caught by
//! exactly ONE row each, and two of them are the heat-gate pair: M13 and M14 die only on
//! `the_two_group_two_knobs_are_independent`'s `(HeatGated, *)` cells, which is why that row is a
//! 3x2 matrix over `RotateRowsArm` and not a 2x2 over booleans -- `rotate_row_count_byte` is
//! reached only under the `HeatGated` arm, so a boolean matrix would have been a gate that cannot
//! fail. M19b dies on four rows because `force_byte_shift_rows` asserts that its own selection
//! took, while the narrower M19 reaches only the default pin -- every other row forces the arm
//! through the thread-local override and so never touches the `OnceLock` that mutant edits.
//!
//! **The 2026-08-29 default flip traded two mutants, and neither trade is left silent.** M20 was
//! inert while the default was OFF (`"" => false` and `"" => DEFAULT_ARM` named one arm) and kills
//! cleanly now. The narrow M19 was the reverse: it died on the default pin while the default was
//! OFF and stopped dying at the flip. Both are recorded at their CURRENT outcome rather than at
//! the one the pre-flip run measured, and M19's entry says what covers it instead.

use super::*;

/// `mov esi,esi` / `mov si,si`, the leading slot that keeps the tested opcode off the block entry.
/// An opcode at a block's ENTRY never executes natively, so an entry-position fixture certifies
/// nothing.
const FILL_A: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi` / `mov di,di`, the trailing slot, so the tested opcode is never the last either.
const FILL_B: [u8; 2] = [0x89, 0xff];

/// The five flag consumers, as `(condition, byte destination)`. Between them they read every flag
/// a shift DEFINES. AL/CL/DL/BL/AH/CH/DH/BH are indices 0..7; the destinations here are chosen so
/// a sweep can pick a shift destination that no consumer overwrites.
const CONSUMERS: [(u8, u8); 5] = [
    (0x2, 3), // setc bl  -- CF
    (0x0, 6), // seto dh  -- OF
    (0x8, 5), // sets ch  -- SF
    (0x4, 2), // setz dl  -- ZF
    (0xa, 7), // setp bh  -- PF
];

/// Which code segment a row runs in. Both are required on every behavioural row: the census row
/// this slice claims is the WORD one, and the Dword one is the control that says the admission did
/// not move anything that already worked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seg {
    /// Real mode, CS.D = 0. An unprefixed `0xC0`/`0xD0` decodes at `OperandSize::Word` here, which
    /// is the census row's `operand_size: word, prefix_mask: 0` shape and the half the allowlist
    /// entry exists for.
    Sixteen,
    /// Protected flat, CS.D = 1. An unprefixed `0xC0`/`0xD0` decodes at `OperandSize::Dword`,
    /// which is what the arm's own admission covers.
    ThirtyTwo,
}

impl Seg {
    fn d(self) -> bool {
        self == Seg::ThirtyTwo
    }

    fn cpu(self) -> CpuGsw {
        match self {
            Seg::Sixteen => sixteen_bit_cpu(),
            Seg::ThirtyTwo => flat_cpu(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Seg::Sixteen => "16-bit segment",
            Seg::ThirtyTwo => "32-bit segment",
        }
    }

    /// `0F 9x /r`, 66-prefixed in a 16-bit segment. See the module docs: the unprefixed form is
    /// refused by the Word allowlist, and the prefix is architecturally inert on `SETcc`.
    fn consumer_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (condition, dst) in CONSUMERS {
            if self == Seg::Sixteen {
                bytes.push(0x66);
            }
            bytes.extend_from_slice(&[0x0f, 0x90 | condition, 0xc0 | dst]);
        }
        bytes
    }
}

const SEGMENTS: [Seg; 2] = [Seg::Sixteen, Seg::ThirtyTwo];

/// Real mode with CS.D = 0 and SS.B = 0: the ordinary DOS configuration tyrian runs in.
fn sixteen_bit_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.set_eip(ENTRY);
    cpu
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

// ---------------------------------------------------------------------------------------------
// Encodings. Named rather than spelled inline so a row and its refusal fixture cannot drift.
// ---------------------------------------------------------------------------------------------

/// `C0 /op ib` on a register destination. `dst` is a BYTE-register index: 4..7 name AH/CH/DH/BH.
fn c0_reg(op: u8, dst: u8, count: u8) -> Vec<u8> {
    vec![0xc0, 0xc0 | (op << 3) | dst, count]
}

/// `D0 /op` on a register destination. NO immediate: the count is the literal 1 baked into the
/// opcode.
fn d0_reg(op: u8, dst: u8) -> Vec<u8> {
    vec![0xd0, 0xc0 | (op << 3) | dst]
}

/// The MEMORY forms, `mod == 00` with `rm == 000`, which is `[BX+SI]` at 16-bit address size and
/// `[EAX]` at 32-bit. Both are refused by the shared `DecodedOperand::Reg` bind and both are a
/// census row this slice does not claim.
fn c0_mem(op: u8, count: u8) -> Vec<u8> {
    vec![0xc0, op << 3, count]
}

fn d0_mem(op: u8) -> Vec<u8> {
    vec![0xd0, op << 3]
}

/// `mov eax,ecx` / `mov ax,cx`: the control row, which must compile on BOTH arms in BOTH segment
/// kinds. Without it a refusal assertion could pass because the harness refuses everything.
const CONTROL: [u8; 2] = [0x89, 0xc8];

/// The four admitted sub-opcodes, `(label, /digit)`. Listed rather than ranged, because a range
/// hides a member and `/6` is the alias whose normalisation is stated at classify.
const SUB_OPS: [(&str, u8); 4] = [("/4 shl", 4), ("/5 shr", 5), ("/6 sal", 6), ("/7 sar", 7)];

/// The two sub-opcodes this slice refuses at both byte opcodes: RCL and RCR. ROL and ROR moved OUT
/// as of `vorvek/direct-word-rot1`; see `byte_rotates_are_admitted_at_both_opcodes_and_both_segment_kinds`.
const REFUSED_SUB_OPS: [(&str, u8); 2] = [("/2 rcl", 2), ("/3 rcr", 3)];

/// The two sub-opcodes `vorvek/direct-word-rot1` admits: ROL and ROR.
const ROL_ROR_SUB_OPS: [(&str, u8); 2] = [("/0 rol", 0), ("/1 ror", 1)];

// ---------------------------------------------------------------------------------------------
// Arm selection
// ---------------------------------------------------------------------------------------------

/// Restores every arm this file forces, on the way out of a fixture -- normally OR by panic.
///
/// A plain `set_*_for_test(Some(..))` LEAKS: the overrides are thread-local and the harness reuses
/// threads, so the next fixture on that thread inherits an arm it never asked for.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_byte_shift_rows_for_test(None);
        jit::direct::set_rotate_rows_arm_for_test(None);
        jit::direct::set_count_lanes_for_test(None);
    }
}

/// Force the byte-shift arm and PROVE the selection took. `IZARRAVM_ROTATE_ROWS` is pinned to `On`
/// alongside it, because `0xC0 /4` sits at the intersection of the two axes and a fixture that
/// inherited the ambient rotate arm would be reading a different cell of the matrix than it says.
///
/// **The count-lane arm is left AMBIENT here and forced by the rows that care**, because it
/// selects which of two emitters the `0xC0` rows reach and the two need separate sweeps. That is
/// `cpu_jit_count_lane_test.rs`'s recorded lesson -- when `IZARRAVM_COUNT_LANES` flipped default
/// ON, every unforced group-2 fixture in the tree quietly stopped exercising the baked emitter.
/// It bit this file too: with the arm ambient, `emit_shift`'s `count == 0` early return and its
/// five-bit mask were unreachable from the `0xC0` rows here (`0xD0`'s count is 1 and never zero),
/// and the mutants that delete them both SURVIVED the first run of this suite.
#[must_use]
fn force_byte_shift_rows(on: bool) -> ArmOverride {
    jit::direct::set_byte_shift_rows_for_test(Some(on));
    jit::direct::set_rotate_rows_arm_for_test(Some(jit::direct::RotateRowsArm::On));
    assert_eq!(
        jit::direct::byte_shift_rows_enabled(),
        on,
        "the fixture override must decide the arm, not the ambient IZARRAVM_BYTE_SHIFT_ROWS"
    );
    ArmOverride
}

/// Both group-2 knobs at once, for the fixtures whose claim is about their interaction.
#[must_use]
fn force_both_group_two_arms(rotate: jit::direct::RotateRowsArm, byte_shift: bool) -> ArmOverride {
    jit::direct::set_byte_shift_rows_for_test(Some(byte_shift));
    jit::direct::set_rotate_rows_arm_for_test(Some(rotate));
    ArmOverride
}

// ---------------------------------------------------------------------------------------------
// The compile-only harness, for the admission rows
// ---------------------------------------------------------------------------------------------

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

/// Compile `FILL_A / body / FILL_B / hlt` at `ENTRY` and report the span length, or `None` when the
/// walk refused it.
///
/// Every page is mapped for read and write unconditionally, and not only for the memory rows: with
/// the operand pages absent from the fast map every memory kind is refused, so a negative
/// assertion made without it would pass for the harness's reason rather than the row's.
///
/// `heat_at` seeds one SMC heat record before compiling, which is what the `HeatGated` cell of the
/// two-knob matrix needs. One `bump` is what `note_code_write_inner` leaves after one heat-charged
/// kill.
fn compile_span_with_heat(seg: Seg, body: &[u8], heat_at: Option<u32>) -> Option<u8> {
    let mut code = FILL_A.to_vec();
    code.extend_from_slice(body);
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = seg.cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_esp(STACK_TOP);
    // Every decode line, one byte at a time rather than at the slot boundaries: the body here is
    // up to seven instructions and the compile loop needs a decode for each of them.
    for offset in 0..code.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x5000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    if let Some(physical) = heat_at {
        cpu.sync_smc_heat();
        cpu.jit_direct.smc_heat.bump(physical, 1, 0);
    }
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, seg.d()) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    }
}

fn compile_span(seg: Seg, body: &[u8]) -> Option<u8> {
    compile_span_with_heat(seg, body, None)
}

/// The span a three-slot block reports when the tested opcode JOINED it.
const ADMITTED: Option<u8> = Some(3);
/// A barrier in the body slot. The walk stops one slot in, which is shorter than the minimum
/// installable block, so the outcome is a `StructuralReject` and the harness reports `None` --
/// the same idiom `cpu_jit_test_word_row_test.rs`'s refusal rows use.
///
/// `None` also covers a `Retry`, which is why every fixture that asserts it also asserts the
/// CONTROL row compiles in the same harness: without that a refusal assertion could pass because
/// the fixture refuses everything.
const REFUSED: Option<u8> = None;

// ---------------------------------------------------------------------------------------------
// The differential harness
// ---------------------------------------------------------------------------------------------

/// The architectural state both roles start from.
///
/// `gpr` poisons every register's HIGH half, so a byte shift that ran as a Word or Dword one is
/// visible in the register compare rather than hidden by a zero seed.
#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            gpr: std::array::from_fn(|i| 0xdead_be00 | (0xa0 + i as u32)),
            // Every EFLAGS seed in this file has bit 1 SET. `emit_shift` publishes the shadow
            // without re-asserting the reserved bit where the interpreter's `set_flag_live` ors
            // `0x2` on every write, so a seed with it clear produces a one-bit disagreement on the
            // DWORD lane too. `cpu_jit_word_shift_test.rs` establishes that as a domain fact and
            // pins that the state is unreachable from guest code.
            eflags: 0x202,
            live_pending: false,
        }
    }

    /// Put `value` in the BYTE lane of byte-register index `index`, leaving every other bit of the
    /// home register at its poison. 0..3 are AL/CL/DL/BL and 4..7 are AH/CH/DH/BH.
    fn byte(mut self, index: u8, value: u8) -> Self {
        let home = usize::from(index & 3);
        let shift = if index < 4 { 0 } else { 8 };
        self.gpr[home] = (self.gpr[home] & !(0xff << shift)) | (u32::from(value) << shift);
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

/// Compile `FILL_A / body / FILL_B / hlt` at `ENTRY` on the native role, warm the same decode lines
/// on the interpreter role, and seed both identically.
///
/// `slots` is the EXACT instruction count the block must cover. An exact count rather than a lower
/// bound is what says the tested opcode joined the block instead of ending it -- a `>=` assertion
/// is satisfied by the fillers alone with the form under test refused.
fn build(seg: Seg, body: &[u8], slots: u8, seed: Seed) -> Roles {
    let mut code = FILL_A.to_vec();
    let mut starts = vec![ENTRY, ENTRY + code.len() as u32];
    code.extend_from_slice(body);
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = seg.cpu();
    let mut interp = seg.cpu();
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
        cpu.set_fast_map_enabled_for_test(true);
        for offset in 0..code.len() as u32 {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            cpu.begin_instruction();
            let _ = cpu.fetch_decoded(bus, linear);
        }
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        for page in (0..0x5000u32).step_by(0x1000) {
            map_direct_page(cpu, bus, page);
        }
    }

    let key = jit::direct::key_for(&native, ENTRY, seg.d()).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, seg.d()) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the byte shift is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the tested opcode really ran natively"
    );
    // A register shift touches no memory at any width. This is what would catch a lane that
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

/// How much of the two roles' state a row compares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Compare {
    /// Everything: architectural state, core clocks, bus clocks and the whole of guest RAM.
    Everything,
    /// Architectural state and RAM, but not the two clock columns. Used ONLY by the patched-count
    /// row, and it is `cpu_jit_count_lane_test.rs`'s `assert_agrees` precedent rather than a
    /// weakening invented here: a guest write to a code byte invalidates the interpreter role's
    /// decode line, so it re-FETCHES the patched instruction where the native role runs a block
    /// that is already compiled. That is a two-clock bus difference the patch itself creates and
    /// says nothing about the lowering. Everything the lane can get wrong -- the result, the
    /// flags, the descriptor it publishes and any stray store -- is still compared.
    ArchitecturalState,
}

fn compare_state(roles: &Roles, compare: Compare, context: &str) {
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
    if compare == Compare::Everything {
        assert_eq!(
            roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
            "{context}: core clocks"
        );
        assert_eq!(
            roles.native_bus.trace.elapsed_clocks(),
            roles.interp_bus.trace.elapsed_clocks(),
            "{context}: bus clocks"
        );
    }
    // The whole array. A register shift must write no guest RAM at all, and a window would be the
    // wrong shape to see a stray store.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

fn run_and_compare(roles: &mut Roles, compare: Compare, context: &str) {
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
    compare_state(roles, compare, context);
}

/// A row that completes NATIVELY: every slot retires in the block and the whole architectural
/// state matches the same number of interpreted steps.
fn lowered(seg: Seg, body: &[u8], slots: u8, seed: Seed, context: &str) {
    let mut roles = build(seg, body, slots, seed);
    run_and_compare(&mut roles, Compare::Everything, context);
}

/// The shift under test plus the five flag consumers, as one block body.
fn with_consumers(seg: Seg, shift: &[u8]) -> Vec<u8> {
    let mut body = shift.to_vec();
    body.extend_from_slice(&seg.consumer_bytes());
    body
}

/// Two fillers, the shift, and the five consumers.
const CONSUMED_SLOTS: u8 = 2 + 1 + CONSUMERS.len() as u8;

/// The count sweep for `0xC0`. 0 is the no-op that must touch no flag and no descriptor; 1 is the
/// only count that defines OF; 7 is the last in-width count; 8 shifts the operand entirely away,
/// where the SDM leaves CF undefined and the interpreter does not; 9/16/31 are the undefined
/// range; 32 and 33 test that the five-bit mask is applied to the RAW immediate -- `shr al, 32`
/// must be the no-op and `shr al, 33` a shift by 1.
///
/// **This sweep is the ONLY evidence for counts 8..31 at `/5` and `/7`** -- `emit_shift_reg8`'s
/// header names it as such, because the 48-case host probe that paragraph used to cite covered
/// `/4` alone. It may not be trimmed without rewriting that header.
const COUNTS: [u8; 10] = [0, 1, 2, 7, 8, 9, 16, 31, 32, 33];

/// Operand seeds. `0x80` and `0x01` are the shortest witnesses for SHL's CF/ZF and SHR's CF;
/// `0xf0` is the negative one SAR's sign fill needs; `0x7f` and `0xff` cover the two ends.
const OPERANDS: [u8; 6] = [0x00, 0x01, 0x7f, 0x80, 0xf0, 0xff];

// =============================================================================================
// R1: the allowlist entry is load-bearing
// =============================================================================================

/// The one test that proves the `OperandSize::Word` allowlist entry does something. RED on `main`
/// for the allowlist's reason alone: `0xD0` has no arm there either, but this row is what
/// separates "the arm exists" from "a 16-bit segment can reach it".
#[test]
fn a_sixteen_bit_segment_block_compiles_through_shr_r8_1() {
    let _arm = force_byte_shift_rows(true);
    assert_eq!(
        compile_span(Seg::Sixteen, &d0_reg(5, 0)),
        ADMITTED,
        "`shr al, 1` in a CS.D = 0 segment must join the block: it is the hottest single rejected \
         row on tyrian-586 at 13,933,316 runtime hits"
    );
    assert_eq!(
        compile_span(Seg::Sixteen, &CONTROL),
        ADMITTED,
        "the control row must compile, or this fixture cannot fail"
    );
}

// =============================================================================================
// R2 / R3: the differential, in both segment kinds
// =============================================================================================

/// `0xC0 /4..=7` and `0xD0 /4..=7` against the interpreter in a SIXTEEN-BIT segment, with the flag
/// consumers reading the result back through emitted code.
///
/// This is the census row's own shape: unprefixed, `operand_size: word`, `prefix_mask: 0`. The
/// width must still come from the OPCODE -- M1 is the mutation that reads `operand_width` here and
/// shifts sixteen bits in a byte instruction.
#[test]
fn byte_shift_register_forms_match_the_interpreter_in_a_sixteen_bit_segment() {
    let _arm = force_byte_shift_rows(true);
    // THE BAKED EMITTER, forced. See `force_byte_shift_rows`: on the shipped count-lane arm every
    // `0xC0` row here would reach `emit_shift_lane` instead, and `emit_shift`'s count-0 return and
    // five-bit mask would go untested. The laned arm has its own sweep below.
    jit::direct::set_count_lanes_for_test(Some(false));
    byte_shift_differential(Seg::Sixteen);
}

/// The same matrix at CS.D = 1, which is the arm's own admission rather than the allowlist's.
#[test]
fn byte_shift_register_forms_match_the_interpreter_in_a_thirty_two_bit_segment() {
    let _arm = force_byte_shift_rows(true);
    // The baked emitter, forced, for the reason the sixteen-bit row above states.
    jit::direct::set_count_lanes_for_test(Some(false));
    byte_shift_differential(Seg::ThirtyTwo);
}

fn byte_shift_differential(seg: Seg) {
    // DL, which no consumer writes and which is the value register the byte lane stages through.
    let dst = 2u8;
    for (label, op) in SUB_OPS {
        for operand in OPERANDS {
            // `0xD0`: the count is the literal 1 baked into the opcode.
            let seed = Seed::new().byte(dst, operand);
            let context = format!("{} 0xd0 {label} dl={operand:#04x}", seg.label());
            lowered(
                seg,
                &with_consumers(seg, &d0_reg(op, dst)),
                CONSUMED_SLOTS,
                seed,
                &context,
            );

            for count in COUNTS {
                for pending in [false, true] {
                    for eflags in [0x202u32, 0x8d7] {
                        let mut seed = Seed::new().byte(dst, operand).flags(eflags);
                        if pending {
                            seed = seed.pending();
                        }
                        let context = format!(
                            "{} 0xc0 {label} dl={operand:#04x} count={count} pending={pending} \
                             eflags={eflags:#x}",
                            seg.label()
                        );
                        lowered(
                            seg,
                            &with_consumers(seg, &c0_reg(op, dst, count)),
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

// =============================================================================================
// R4: the high byte registers
// =============================================================================================

/// Destinations AH, CH, DH and BH -- byte-register indices 4..7 -- at both opcodes and in BOTH
/// segment kinds.
///
/// `home(index)` is `GUEST_HOMES[index & 7]`, so index 4 is the guest ESP home and the set is
/// ESP/EBP/ESI/EDI. A byte lane that reached for `home(dst)` would shift the wrong register by 32
/// bits, which is M12. The 16-bit half matters on its own: the byte-register index is UNCHANGED by
/// the Word decode, and a reader who assumed the operand is renumbered there would introduce
/// exactly this bug.
///
/// No consumers on this row: a `SETcc` writes a byte register and would overwrite the half of the
/// result the row exists to compare. The seeds carry a poison in every other lane instead, so a
/// write to the wrong register or the wrong lane fails on `registers`.
#[test]
fn byte_shift_into_a_high_byte_register_matches_the_interpreter() {
    let _arm = force_byte_shift_rows(true);
    // The baked emitter, forced: `emit_shift_reg8` is the function whose `home(dst)` trap this row
    // exists for, and on the ambient arm the `0xC0` half would reach the lane emitter instead.
    jit::direct::set_count_lanes_for_test(Some(false));
    for seg in SEGMENTS {
        for (label, op) in SUB_OPS {
            for dst in 4..8u8 {
                for operand in OPERANDS {
                    let seed = Seed::new().byte(dst, operand);
                    let context = format!(
                        "{} 0xd0 {label} byte-reg {dst} = {operand:#04x}",
                        seg.label()
                    );
                    lowered(seg, &d0_reg(op, dst), 3, seed, &context);
                    for count in [1u8, 3, 8, 31] {
                        let seed = Seed::new().byte(dst, operand);
                        let context = format!(
                            "{} 0xc0 {label} byte-reg {dst} = {operand:#04x} count={count}",
                            seg.label()
                        );
                        lowered(seg, &c0_reg(op, dst, count), 3, seed, &context);
                    }
                }
            }
        }
    }
}

// =============================================================================================
// R5: RCL/RCR stay out; ROL/ROR are admitted, in BOTH segment kinds
// =============================================================================================

/// `0xD0 /2,/3` and `0xC0 /2,/3` (RCL, RCR) stay hard boundaries WITH the knob on, in both segment
/// kinds.
///
/// GREEN on `main`, and it is here for the implementation mistake it catches rather than for a
/// census row: widening the new arms' sub-opcode match to admit `2 | 3` "to mirror the sibling
/// sub-opcodes". RCL and RCR are refused for the standing structural reason `RotateReg`'s doc gives
/// -- both take the incoming CF as a rotate INPUT -- which no emitter here reproduces.
#[test]
fn byte_rcl_rcr_stay_a_hard_boundary_at_both_opcodes_and_both_segment_kinds() {
    let _arm = force_byte_shift_rows(true);
    for seg in SEGMENTS {
        for (label, op) in REFUSED_SUB_OPS {
            assert_eq!(
                compile_span(seg, &d0_reg(op, 0)),
                REFUSED,
                "{} 0xd0 {label} must stay a barrier: no emitter takes the incoming CF",
                seg.label()
            );
            assert_eq!(
                compile_span(seg, &c0_reg(op, 0, 3)),
                REFUSED,
                "{} 0xc0 {label} must stay a barrier: no emitter takes the incoming CF",
                seg.label()
            );
        }
        assert_eq!(
            compile_span(seg, &CONTROL),
            ADMITTED,
            "{}: the control row must compile, or this fixture cannot fail",
            seg.label()
        );
    }
}

/// `0xD0 /0,/1` and `0xC0 /0,/1` (ROL, ROR) are ADMITTED with the knob on, in BOTH segment kinds --
/// the positive half of the guard above, and the ONLY fixture that can detect the reachability gate
/// (`classify`'s top-of-function `OperandSize::Word` allowlist) being unreachable for these two
/// sub-opcodes.
///
/// **The 16-bit-segment case is the one that matters, and it is not decoration.** `123-talk-
/// shareware`'s `0xD0 register /1` and `/0` rows (24.86M and 0.45M runtime hits, its #1 and #3
/// census rows) and `21-for-1-to-4`'s `0xD0 register /1` (13.50M) are UNPREFIXED `0xD0` in a
/// `CS.D = 0` segment, which decodes at `OperandSize::Word` with no `0x66` byte anywhere in sight.
/// `classify`'s reachability gate for `0xc0`/`0xd0` used to admit ONLY `matches!(m.reg, 4..=7)`
/// past that check; a version of this arm that widened the classify MATCH inside the `0xc0`/`0xd0`
/// arms without ALSO widening the gate's own `matches!` would refuse every corpus row this slice
/// claims while a 32-bit-segment (or `0x66`-prefixed) positive test passed regardless, because
/// Dword-default code never reaches that gate at all. `compile_span(Seg::ThirtyTwo, ..)` alone
/// would NOT have caught that mistake; `compile_span(Seg::Sixteen, ..)` is what does.
#[test]
fn byte_rotates_are_admitted_at_both_opcodes_and_both_segment_kinds() {
    let _arm = force_byte_shift_rows(true);
    for seg in SEGMENTS {
        for (label, op) in ROL_ROR_SUB_OPS {
            assert_eq!(
                compile_span(seg, &d0_reg(op, 0)),
                ADMITTED,
                "{} 0xd0 {label} must admit and carry the whole three-slot block",
                seg.label()
            );
            assert_eq!(
                compile_span(seg, &c0_reg(op, 0, 3)),
                ADMITTED,
                "{} 0xc0 {label} must admit and carry the whole three-slot block",
                seg.label()
            );
            // Byte indices 4..=7 name AH/CH/DH/BH, the lane `emit_rotate_reg8` addresses through
            // `emit_read_store_value`/`emit_write_gpr8` rather than through `home()`. Admitted here
            // too, so a lowering that only handled the low four registers would still fail this
            // test rather than passing on `dst=0` alone.
            assert_eq!(
                compile_span(seg, &d0_reg(op, 4)),
                ADMITTED,
                "{} 0xd0 {label} ah-class dst=4 must admit",
                seg.label()
            );
        }
        // The memory forms are a DIFFERENT kind and a different emitter, and they moved with
        // `vorvek/direct-rot-mem-lane`: `RotateRegByte` still binds `DecodedOperand::Reg`, and the
        // `else` branch of that bind now produces `DirectKind::RotateShiftMem`. They are asserted
        // here as admitted rather than dropped, so this file keeps saying what the arm does with
        // BOTH operand forms; their behaviour is certified in `cpu_jit_group2_mem_test.rs`.
        for (label, op) in ROL_ROR_SUB_OPS {
            assert_eq!(
                compile_span(seg, &d0_mem(op)),
                ADMITTED,
                "{} 0xd0 {label} memory form joins the block through RotateShiftMem",
                seg.label()
            );
            assert_eq!(
                compile_span(seg, &c0_mem(op, 3)),
                ADMITTED,
                "{} 0xc0 {label} memory form joins the block through RotateShiftMem",
                seg.label()
            );
        }
    }
}

/// Byte ROL/ROR ride `rotate_rows_enabled` -- the SAME knob the Dword ROL/ROR rows have always
/// used -- rather than `IZARRAVM_BYTE_SHIFT_ROWS`. `force_byte_shift_rows` cannot show that on its
/// own: it always forces `rotate_rows_arm` to `On` alongside whichever `byte_shift_rows` value the
/// caller asks for, so a fixture built only on that helper could not tell "gated on
/// `rotate_rows_enabled`" apart from "gated on `byte_shift_rows_enabled`, and `force_byte_shift_rows`
/// happens to always turn the other knob on too". This sets `rotate_rows_arm` directly, OFF, with
/// `byte_shift_rows` forced ON, which isolates the claim: if ROL/ROR followed
/// `byte_shift_rows_enabled` instead, this fixture would see them admitted and fail to catch it.
#[test]
fn byte_rotates_follow_the_rotate_rows_knob_not_the_byte_shift_rows_knob() {
    jit::direct::set_byte_shift_rows_for_test(Some(true));
    jit::direct::set_rotate_rows_arm_for_test(Some(jit::direct::RotateRowsArm::Off));
    assert!(
        jit::direct::byte_shift_rows_enabled(),
        "byte_shift_rows must be on for this fixture to isolate the other knob"
    );
    assert!(
        !jit::direct::rotate_rows_enabled(),
        "rotate_rows must be off for this fixture to isolate the other knob"
    );
    for seg in SEGMENTS {
        for (label, op) in ROL_ROR_SUB_OPS {
            assert_eq!(
                compile_span(seg, &d0_reg(op, 0)),
                REFUSED,
                "{} 0xd0 {label} must refuse with rotate_rows off, even though byte_shift_rows is on",
                seg.label()
            );
            assert_eq!(
                compile_span(seg, &c0_reg(op, 0, 3)),
                REFUSED,
                "{} 0xc0 {label} must refuse with rotate_rows off, even though byte_shift_rows is on",
                seg.label()
            );
        }
        // `/4..=7` are UNAFFECTED: `/4` needs rotate_rows too (the conjunction row) but `/5,/6,/7`
        // ride byte_shift_rows alone and must still admit.
        assert_eq!(
            compile_span(seg, &d0_reg(5, 0)),
            ADMITTED,
            "{}: 0xd0 /5 shr must still admit on byte_shift_rows alone",
            seg.label()
        );
    }
    jit::direct::set_byte_shift_rows_for_test(None);
    jit::direct::set_rotate_rows_arm_for_test(None);
}

// =============================================================================================
// R5b: the byte rotate differential
// =============================================================================================

/// Operand seeds for the rotate differential. `OPERANDS` covers the shift file's own boundary set;
/// `0x55`/`0xaa` (alternating bit patterns) are added so every rotate amount up to 7 crosses the
/// boundary both ways, mirroring `cpu_jit_word_rotate_test.rs`'s `BOUNDARY_OPERANDS`.
const ROTATE_OPERANDS: [u8; 8] = [0x00, 0x01, 0x7f, 0x80, 0xf0, 0xff, 0x55, 0xaa];

/// A domain-real prior EFLAGS state with SF, ZF, PF and AF all SET, and bit 1 forced -- the byte
/// file's `0x8d7` seed used verbatim, established as reachable by
/// `cpu_jit_word_shift_test.rs::an_eflags_image_with_bit_one_clear_is_not_reachable`. Used
/// everywhere below that asserts PRESERVATION.
const ROTATE_SEEDED_EFLAGS: u32 = 0x8d7;

/// `0xC0 /0,/1` and `0xD0 /0,/1` against the interpreter in a SIXTEEN-BIT segment, with the flag
/// consumers reading CF/OF/SF/ZF/PF back through emitted code.
///
/// This is the census row's own shape: unprefixed, `operand_size: word`, `prefix_mask: 0`. Like
/// `byte_shift_differential`, the width must come from the OPCODE regardless of the decoded
/// segment default.
#[test]
fn byte_rotate_register_forms_match_the_interpreter_in_a_sixteen_bit_segment() {
    let _arm = force_byte_shift_rows(true);
    byte_rotate_differential(Seg::Sixteen);
}

/// The same matrix at CS.D = 1, which is the arm's own admission rather than the allowlist's.
#[test]
fn byte_rotate_register_forms_match_the_interpreter_in_a_thirty_two_bit_segment() {
    let _arm = force_byte_shift_rows(true);
    byte_rotate_differential(Seg::ThirtyTwo);
}

fn byte_rotate_differential(seg: Seg) {
    // DL, which no consumer writes and which is the value register the byte lane stages through.
    let dst = 2u8;
    for (label, op) in ROL_ROR_SUB_OPS {
        for operand in ROTATE_OPERANDS {
            // `0xD0`: the count is the literal 1 baked into the opcode.
            let seed = Seed::new().byte(dst, operand).flags(ROTATE_SEEDED_EFLAGS);
            let context = format!("{} 0xd0 {label} dl={operand:#04x}", seg.label());
            lowered(
                seg,
                &with_consumers(seg, &d0_reg(op, dst)),
                CONSUMED_SLOTS,
                seed,
                &context,
            );

            for count in COUNTS {
                for pending in [false, true] {
                    for eflags in [0x202u32, ROTATE_SEEDED_EFLAGS] {
                        let mut seed = Seed::new().byte(dst, operand).flags(eflags);
                        if pending {
                            seed = seed.pending();
                        }
                        let context = format!(
                            "{} 0xc0 {label} dl={operand:#04x} count={count} pending={pending} \
                             eflags={eflags:#x}",
                            seg.label()
                        );
                        lowered(
                            seg,
                            &with_consumers(seg, &c0_reg(op, dst, count)),
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

/// Raw-count cover for `emit_rotate_reg8`'s five-bit mask on the decoded immediate.
///
/// Given: byte ROL/ROR via `0xC0`, native vs interpreter.
/// When: the count byte is the RAW immediate, not a pre-masked value.
/// Then: every raw count 0..=255 on one representative shape (32-bit CS, ROL,
/// operand 0x55) matches the interpreter, so a high-bit special case
/// (`if raw >= 128 { raw - 128 }`) that agrees at 128/255 still fails at 160.
/// The full (segment × ROL/ROR × operand) matrix runs only the boundary counts
/// `COUNTS` does not already name: 63/64 catch a 6-bit mask, 127/128 a 7-bit
/// mask, 255 an 8-bit mask. 0/1/7/8/9/16/31/32/33 stay so a 3-bit mask or a
/// mask applied to the destination still diverges on ROR of 0x00 and on 0x55.
#[test]
fn byte_rotates_match_the_interpreter_across_every_raw_count() {
    let _arm = force_byte_shift_rows(true);
    const BOUNDARY_COUNTS: [u8; 14] = [0, 1, 7, 8, 9, 16, 31, 32, 33, 63, 64, 127, 128, 255];
    for raw_count in 0u16..=0xff {
        let raw_count = raw_count as u8;
        let seed = Seed::new().byte(2, 0x55).flags(ROTATE_SEEDED_EFLAGS);
        let context = format!("32-bit segment 0xc0 /0 rol dl=0x55 raw_count={raw_count:#04x}");
        lowered(Seg::ThirtyTwo, &c0_reg(0, 2, raw_count), 3, seed, &context);
    }
    for seg in SEGMENTS {
        for (label, op) in ROL_ROR_SUB_OPS {
            for operand in [0x00u8, 0x55, 0xff] {
                for raw_count in BOUNDARY_COUNTS {
                    let seed = Seed::new().byte(2, operand).flags(ROTATE_SEEDED_EFLAGS);
                    let context = format!(
                        "{} 0xc0 {label} dl={operand:#04x} raw_count={raw_count:#04x}",
                        seg.label()
                    );
                    lowered(seg, &c0_reg(op, 2, raw_count), 3, seed, &context);
                }
            }
        }
    }
}

/// Destinations AH, CH, DH and BH -- byte-register indices 4..7 -- at both rotate opcodes and in
/// BOTH segment kinds, mirroring `byte_shift_into_a_high_byte_register_matches_the_interpreter`.
///
/// `home(index)` is `GUEST_HOMES[index & 7]`, so index 4 is the guest ESP home and the set is
/// ESP/EBP/ESI/EDI. A rotate lane that reached for `home(dst)` at this width would rotate the wrong
/// register by 32 bits, which is exactly the hazard `RotateRegByte`'s own doc names. No consumers
/// on this row: a `SETcc` writes a byte register and would overwrite the half of the result the row
/// exists to compare. The seeds carry a poison in every other lane instead.
#[test]
fn byte_rotate_into_a_high_byte_register_matches_the_interpreter() {
    let _arm = force_byte_shift_rows(true);
    for seg in SEGMENTS {
        for (label, op) in ROL_ROR_SUB_OPS {
            for dst in 4..8u8 {
                for operand in ROTATE_OPERANDS {
                    let seed = Seed::new().byte(dst, operand);
                    let context = format!(
                        "{} 0xd0 {label} byte-reg {dst} = {operand:#04x}",
                        seg.label()
                    );
                    lowered(seg, &d0_reg(op, dst), 3, seed, &context);
                    for count in [1u8, 3, 8, 31] {
                        let seed = Seed::new().byte(dst, operand);
                        let context = format!(
                            "{} 0xc0 {label} byte-reg {dst} = {operand:#04x} count={count}",
                            seg.label()
                        );
                        lowered(seg, &c0_reg(op, dst, count), 3, seed, &context);
                    }
                }
            }
        }
    }
}

/// A masked count of zero must leave EVERY flag and a live lazy descriptor exactly as they are, at
/// both rotate opcodes and both segment kinds -- the byte sibling of
/// `a_zero_count_rotate_leaves_every_flag_and_a_live_descriptor_alone` in
/// `cpu_jit_word_rotate_test.rs`.
///
/// `shift_rotate` returns before touching the value or a flag, so a zero-count rotate neither
/// creates a descriptor nor destroys one. With a DWORD descriptor live from `0x7fff_ffff + 1`, the
/// five `SETcc` bytes must read that descriptor's flags, not the rotate's.
#[test]
fn a_zero_count_byte_rotate_leaves_every_flag_and_a_live_descriptor_alone() {
    let _arm = force_byte_shift_rows(true);
    for seg in SEGMENTS {
        for (label, op) in ROL_ROR_SUB_OPS {
            for operand in ROTATE_OPERANDS {
                for pending in [false, true] {
                    for eflags in [0x202u32, ROTATE_SEEDED_EFLAGS] {
                        let mut seed = Seed::new().byte(2, operand).flags(eflags);
                        if pending {
                            seed = seed.pending();
                        }
                        let context = format!(
                            "{} 0xc0 {label} count=0 dl={operand:#04x} pending={pending} \
                             eflags={eflags:#x}",
                            seg.label()
                        );
                        lowered(
                            seg,
                            &with_consumers(seg, &c0_reg(op, 2, 0)),
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

/// The preservation claim, stated as its own assertion rather than left implicit in the
/// differential above: from `ROTATE_SEEDED_EFLAGS` (SF, ZF, PF and AF all set), every count from 1
/// to 31 leaves those four bits set in the resulting eflags, and every byte of the destination
/// register OUTSIDE the rotated lane -- including the full upper 24 bits for a low register and the
/// low 8 plus upper 16 for a high one -- survives from the seed untouched.
///
/// This is the row a fold into `Shift`'s byte arm (`emit_shift_reg8`, via `emit_commit_shift_flags`)
/// would fail immediately: that path publishes the whole RBP shadow to `eflags` at every non-zero
/// count, overwriting SF/ZF/PF with whatever the rotate's OWN 8-bit result derives them to.
#[test]
fn byte_rotates_preserve_flags_above_the_lane_and_bits_outside_it() {
    const PRESERVED: u32 = crate::FLAG_SF | crate::FLAG_ZF | crate::FLAG_PF | crate::FLAG_AF;
    let _arm = force_byte_shift_rows(true);
    for seg in SEGMENTS {
        for (label, op) in ROL_ROR_SUB_OPS {
            for dst in [0u8, 1, 4, 5] {
                for count in [1u8, 2, 3, 7, 31] {
                    for operand in ROTATE_OPERANDS {
                        let seed = Seed::new().byte(dst, operand).flags(ROTATE_SEEDED_EFLAGS);
                        let lane_mask: u32 = if dst < 4 { 0xff } else { 0xff00 };
                        let home = usize::from(dst & 3);
                        let before = seed.gpr[home];
                        let mut roles = build(seg, &c0_reg(op, dst, count), 3, seed);
                        assert!(
                            roles
                                .native
                                .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                                .unwrap(),
                            "{} {label} dst={dst} count={count} operand={operand:#04x}: block \
                             did not run natively",
                            seg.label()
                        );
                        let after_eflags = roles.native.eflags();
                        assert_eq!(
                            after_eflags & PRESERVED,
                            ROTATE_SEEDED_EFLAGS & PRESERVED,
                            "{} {label} dst={dst} count={count} operand={operand:#04x}: \
                             SF/ZF/PF/AF must survive from the seed",
                            seg.label()
                        );
                        let after = roles.native.registers.gpr[home];
                        assert_eq!(
                            after & !lane_mask,
                            before & !lane_mask,
                            "{} {label} dst={dst} count={count} operand={operand:#04x}: every \
                             bit outside the rotated lane, including the register's high half, \
                             must survive untouched",
                            seg.label()
                        );
                    }
                }
            }
        }
    }
}

// =============================================================================================
// R6 / R7: count lanes
// =============================================================================================

/// Compile the fixture body with the count-lane arm forced ON and report how many COUNT lanes the
/// block took.
fn count_lanes_for(seg: Seg, body: &[u8], slots: u8) -> usize {
    let mut code = FILL_A.to_vec();
    code.extend_from_slice(body);
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = seg.cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_esp(STACK_TOP);
    for offset in 0..code.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x5000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, seg.d()) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("fixture block did not compile: structurally rejected")
        }
        jit::direct::CompileOutcome::Retry(_) => {
            panic!("fixture block did not compile: the walk asked for a retry")
        }
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "fixture block shape changed; a lane assertion on a truncated block is vacuous"
    );
    compilation.count_lane_count()
}

/// `0xC0 /5,/6,/7` DO take a count lane, at both segment kinds, and the laned block still equals
/// the interpreter.
///
/// This is why the `emit_shift_lane` assert relaxation is not optional. `count_lane_for` bars on
/// `matches!(insn.opcode, 0xc0 | 0xc1)`, `imm_len == 1` and `len == 3`, all of which the new rows
/// pass -- so on the SHIPPED count-lane arm a debug build reaches the Byte arm's
/// `debug_assert_eq!(op, 4)` and panics the compiler on ordinary DOS code, while a release build
/// emits code that is already correct. A debug-only panic on a release-correct path is exactly the
/// class of bug the `emit_shift_lane` Word `unreachable!` incident shipped once.
///
/// The 16-bit row is a second claim on top: an unprefixed `c0 e8 03` in a CS.D = 0 segment has
/// `prefixes == default`, `disp_len == 0`, `imm_len == 1` and `len == 3`, which is the SAME shape
/// that shipped a compiler panic for `0xC1` at Word. The outcome is opposite and safe here, because
/// the kind's width is `Byte` rather than `Word` and `count_lane_for`'s width bar admits Byte
/// deliberately -- pinned rather than left as an inference.
#[test]
fn a_byte_shr_takes_a_count_lane_at_every_admitted_sub_opcode() {
    let _arm = force_byte_shift_rows(true);
    jit::direct::set_count_lanes_for_test(Some(true));
    for seg in SEGMENTS {
        for (label, op) in SUB_OPS {
            assert_eq!(
                count_lanes_for(seg, &c0_reg(op, 2, 3), 3),
                1,
                "{} 0xc0 {label} must take a COUNT lane on the shipped arm",
                seg.label()
            );
        }
    }
}

/// The laned block equals the interpreter at every count shape, in both segment kinds.
///
/// `0x20` masks to 0 and `0x21` masks to 1, which is what turns "the mask is applied to the loaded
/// byte before the shape test" into a measurement: an implementation that selected on the raw byte
/// would run them as counts 32 and 33 and diverge on both the result and the descriptor.
#[test]
fn a_sixteen_bit_byte_shift_takes_a_count_lane_and_stays_byte_wide() {
    let _arm = force_byte_shift_rows(true);
    jit::direct::set_count_lanes_for_test(Some(true));
    for seg in SEGMENTS {
        for (label, op) in SUB_OPS {
            for count in [0u8, 1, 3, 31, 0x20, 0x21, 0xff] {
                for operand in [0x01u8, 0x80, 0xf0] {
                    let seed = Seed::new().byte(2, operand).pending();
                    let context = format!(
                        "{} laned 0xc0 {label} dl={operand:#04x} count={count}",
                        seg.label()
                    );
                    lowered(
                        seg,
                        &with_consumers(seg, &c0_reg(op, 2, count)),
                        CONSUMED_SLOTS,
                        seed,
                        &context,
                    );
                }
            }
        }
    }
}

/// A count patched IN GUEST RAM changes the result without killing the block, which is the whole
/// point of a lane. The block is compiled at one count and run at another.
#[test]
fn a_patched_byte_shift_count_keeps_the_block_and_changes_the_result() {
    let _arm = force_byte_shift_rows(true);
    jit::direct::set_count_lanes_for_test(Some(true));
    for seg in SEGMENTS {
        for (label, op) in SUB_OPS {
            for patched in [0u8, 1, 4, 0x21] {
                let seed = Seed::new().byte(2, 0xf0);
                // Compiled at 3, run at `patched`, so a lane that quietly re-used its compile-time
                // immediate fails instead of agreeing.
                let mut roles = build(
                    seg,
                    &with_consumers(seg, &c0_reg(op, 2, 3)),
                    CONSUMED_SLOTS,
                    seed,
                );
                let lane = ENTRY + FILL_A.len() as u32 + 2;
                for (cpu, bus) in [
                    (&mut roles.native, &mut roles.native_bus),
                    (&mut roles.interp, &mut roles.interp_bus),
                ] {
                    cpu.write_memory_bus_width(
                        bus,
                        SegmentIndex::Ds,
                        lane,
                        BusWidth::Byte,
                        u32::from(patched),
                        BusAccessKind::DataWrite,
                    )
                    .expect("fixture patch store");
                }
                // The patch is a code write; both roles must start the run from the same state.
                for cpu in [&mut roles.native, &mut roles.interp] {
                    cpu.halted = false;
                    cpu.registers.gpr = seed.gpr;
                    cpu.registers.set_esp(STACK_TOP);
                    cpu.registers.eflags = seed.eflags;
                    cpu.pending_flags = PendingFlags::default();
                    cpu.set_eip(ENTRY);
                    cpu.elapsed_clocks = 0;
                    cpu.timing_rem = 0;
                    cpu.core_clocks_so_far = 0;
                }
                roles.native_bus.trace = BusTrace::default();
                roles.interp_bus.trace = BusTrace::default();
                let context = format!("{} patched 0xc0 {label} count={patched}", seg.label());
                run_and_compare(&mut roles, Compare::ArchitecturalState, &context);
            }
        }
    }
}

/// `0xD0` takes NO count lane, and the refusal is over-determined by three independent bars: the
/// opcode bar (`0xc0 | 0xc1`), `imm_len == 1` (it is 0) and `len == 3` (it is 2).
///
/// Pinned rather than inferred, because the opcode bar's own comment admits it is redundant today
/// -- so a later widening that dropped it would leave the row's refusal resting on two bars nobody
/// stated.
#[test]
fn a_byte_shift_by_one_takes_no_count_lane() {
    let _arm = force_byte_shift_rows(true);
    jit::direct::set_count_lanes_for_test(Some(true));
    for seg in SEGMENTS {
        for (label, op) in SUB_OPS {
            assert_eq!(
                count_lanes_for(seg, &d0_reg(op, 2), 3),
                0,
                "{} 0xd0 {label} has no immediate byte and must take no count lane",
                seg.label()
            );
        }
        // The positive control in the same harness, or a zero-lane assertion could pass because
        // nothing takes a lane here at all.
        assert_eq!(
            count_lanes_for(seg, &c0_reg(5, 2, 3), 3),
            1,
            "{}: the control row must take a lane",
            seg.label()
        );
    }
}

// =============================================================================================
// R8: the rows flip with the gate
// =============================================================================================

/// Every row this slice claims is a boundary on the OFF arm and admitted on the ON arm, in both
/// segment kinds -- and `0xC0 /4` at Word is the row that pins the two-axis rule.
///
/// Catches, in the `false` direction: an arm that forgot its `byte_shift_rows_enabled()` guard,
/// i.e. a row that ships admitted while the knob says off, which would destroy the A/B base.
/// Catches, in the `true` direction: a missing or wrongly-keyed allowlist term.
#[test]
fn the_byte_shift_rows_flip_with_the_gate() {
    for on in [false, true] {
        let _arm = force_byte_shift_rows(on);
        let admitted = if on { ADMITTED } else { REFUSED };
        for seg in SEGMENTS {
            for (label, op) in SUB_OPS {
                assert_eq!(
                    compile_span(seg, &d0_reg(op, 0)),
                    admitted,
                    "{} 0xd0 {label} on the {on} arm",
                    seg.label()
                );
            }
            for (label, op) in [("/5 shr", 5u8), ("/6 sal", 6), ("/7 sar", 7)] {
                assert_eq!(
                    compile_span(seg, &c0_reg(op, 0, 3)),
                    admitted,
                    "{} 0xc0 {label} on the {on} arm",
                    seg.label()
                );
            }
            assert_eq!(
                compile_span(seg, &CONTROL),
                ADMITTED,
                "{}: the control row must compile on the {on} arm",
                seg.label()
            );
        }
        // `0xC0 /4` is the CONJUNCTION row. It predates this slice at Dword and is gated there on
        // IZARRAVM_ROTATE_ROWS alone, which `force_byte_shift_rows` pins to `On`; at Word it needs
        // the reachability axis as well, so it is a NEW admission that this slice names rather
        // than smuggles.
        assert_eq!(
            compile_span(Seg::ThirtyTwo, &c0_reg(4, 0, 3)),
            ADMITTED,
            "0xc0 /4 at Dword predates this slice and must compile on the {on} arm"
        );
        assert_eq!(
            compile_span(Seg::Sixteen, &c0_reg(4, 0, 3)),
            admitted,
            "0xc0 /4 at Word needs BOTH knobs: it is a boundary on the byte-shift OFF arm even \
             with IZARRAVM_ROTATE_ROWS on"
        );
    }
}

// =============================================================================================
// R9: the gate does not sweep in its neighbours
// =============================================================================================

/// With the knob ON, the adjacent group-2 opcodes are exactly where they were, and the memory
/// forms of the two claimed opcodes stay boundaries.
///
/// `0xD3` is the shift-by-CL group. It compiles at Dword unconditionally and, as of the S-B
/// ALU-rows slice, also at Word under its own `IZARRAVM_WORD_SHIFT_CL_ROWS` knob (default ON,
/// unaffected by `IZARRAVM_BYTE_SHIFT_ROWS`) -- this row now pins that the byte-shift knob does
/// not gate it either way. `0xD2` is its byte twin and has no arm at any width.
#[test]
fn the_gate_does_not_sweep_in_its_neighbours() {
    for on in [false, true] {
        let _arm = force_byte_shift_rows(on);
        for seg in SEGMENTS {
            // The MEMORY forms of both claimed opcodes ride the SAME knob as their register
            // siblings, which is the property this row now pins. They used to be barriers at both
            // arms, refused by the shared `Reg` bind; `vorvek/direct-rot-mem-lane` lowers them
            // through `DirectKind::RotateShiftMem` and reads the knob ABOVE the operand bind, so
            // the off arm still refuses them and the on arm admits them. A memory form that
            // compiled on the OFF arm would mean the knob had stopped gating the whole opcode.
            for (label, op) in SUB_OPS {
                // The SAME cell of the two-knob matrix the register row occupies, which is not
                // uniform across the sub-opcodes: `/4` is on `IZARRAVM_ROTATE_ROWS` inside the
                // arm (it predates the byte-shift slice) and needs the byte-shift reachability
                // term only in a 16-bit segment, while `/5..=7` are on
                // `IZARRAVM_BYTE_SHIFT_ROWS` in the arm itself and refuse at both segment kinds.
                // `force_byte_shift_rows` pins the rotate knob to `On` at both arms, so `/4` in a
                // 32-bit segment is admitted either way -- exactly what the conjunction-row
                // assertion above says about the register form.
                //
                // `0xD0` does NOT share that exception: its own arm puts `/4..=7` on
                // `byte_shift_rows_enabled()` with no rotate-knob term at all, so every one of its
                // sub-opcodes refuses at both segment kinds on the off arm. The two opcodes really
                // do sit in different cells here, and one shared expression for both would be
                // wrong for one of them.
                let c0_expected = if on || (op == 4 && seg == Seg::ThirtyTwo) {
                    ADMITTED
                } else {
                    REFUSED
                };
                let d0_expected = if on { ADMITTED } else { REFUSED };
                assert_eq!(
                    compile_span(seg, &c0_mem(op, 3)),
                    c0_expected,
                    "{} 0xc0 {label} MEMORY form on the {on} arm",
                    seg.label()
                );
                assert_eq!(
                    compile_span(seg, &d0_mem(op)),
                    d0_expected,
                    "{} 0xd0 {label} MEMORY form on the {on} arm",
                    seg.label()
                );
            }
            // The byte rotates at Word, which the allowlist term screens on the SUB-OPCODE as well
            // as the opcode.
            for (label, op) in REFUSED_SUB_OPS {
                assert_eq!(
                    compile_span(seg, &c0_reg(op, 0, 3)),
                    REFUSED,
                    "{} 0xc0 {label} must stay a barrier on the {on} arm",
                    seg.label()
                );
                assert_eq!(
                    compile_span(seg, &d0_reg(op, 0)),
                    REFUSED,
                    "{} 0xd0 {label} must stay a barrier on the {on} arm",
                    seg.label()
                );
            }
            // `0xD2`, the byte shift by CL: no arm at any width.
            assert_eq!(
                compile_span(seg, &[0xd2, 0xe8]),
                REFUSED,
                "{} 0xd2 /5 must stay a barrier on the {on} arm: emit_shift_cl is Dword-only",
                seg.label()
            );
        }
        // The wide siblings, which were admitted before this slice and must not move.
        for (label, bytes) in [
            ("0xc1 /5", vec![0xc1, 0xe8, 0x03]),
            ("0xd1 /5", vec![0xd1, 0xe8]),
        ] {
            for seg in SEGMENTS {
                assert_eq!(
                    compile_span(seg, &bytes),
                    ADMITTED,
                    "{} {label} predates this slice and must still compile on the {on} arm",
                    seg.label()
                );
            }
        }
        // `0xD3 /5` is admitted at Dword unconditionally and, since the S-B ALU-rows slice, at
        // Word too under `IZARRAVM_WORD_SHIFT_CL_ROWS` (default ON) -- independent of
        // `IZARRAVM_BYTE_SHIFT_ROWS`, so both arms of THIS knob admit it.
        assert_eq!(
            compile_span(Seg::ThirtyTwo, &[0xd3, 0xe8]),
            ADMITTED,
            "0xd3 /5 at Dword must still compile on the {on} arm"
        );
        assert_eq!(
            compile_span(Seg::Sixteen, &[0xd3, 0xe8]),
            ADMITTED,
            "0xd3 /5 at Word must compile on the {on} arm: IZARRAVM_WORD_SHIFT_CL_ROWS is its own knob"
        );
    }
}

// =============================================================================================
// R10: the two group-2 knobs are independent
// =============================================================================================

/// The 3x2 matrix over `IZARRAVM_ROTATE_ROWS`'s three arms and this slice's two.
///
/// **It is 3x2 rather than 2x2 because `rotate_row_count_byte` is reached ONLY under
/// `rotate_rows_arm() == RotateRowsArm::HeatGated`** (the compile walk and the census suffix scan
/// each guard it that way). A 2x2 over {0, 1} would be a gate that cannot fail for the one claim
/// this file makes about the heat gate: that it is deliberately NOT widened to the new rows.
///
/// The `(HeatGated, on)` cell is the whole of that argument. Over a count byte that carries a heat
/// record, `0xC0 /4` is downgraded to `HardBoundary` and `0xC0 /5` is NOT, because
/// `rotate_row_count_byte` matches `0xc0 if reg == 4` and nothing else. That is the cell M13 dies
/// on.
#[test]
fn the_two_group_two_knobs_are_independent() {
    use jit::direct::RotateRowsArm;
    // The count byte of a three-byte `0xC0` in the body slot: two filler bytes, then the opcode
    // and the ModRM.
    let count_byte = ENTRY + FILL_A.len() as u32 + 2;

    for byte_shift in [false, true] {
        let admitted = if byte_shift { ADMITTED } else { REFUSED };

        // --- Off ---------------------------------------------------------------------------
        {
            let _arm = force_both_group_two_arms(RotateRowsArm::Off, byte_shift);
            for seg in SEGMENTS {
                assert_eq!(
                    compile_span(seg, &c0_reg(4, 0, 3)),
                    REFUSED,
                    "{} (Off, {byte_shift}): 0xc0 /4 is gated on IZARRAVM_ROTATE_ROWS alone",
                    seg.label()
                );
                assert_eq!(
                    compile_span(seg, &c0_reg(5, 0, 3)),
                    admitted,
                    "{} (Off, {byte_shift}): 0xc0 /5 is gated on IZARRAVM_BYTE_SHIFT_ROWS alone",
                    seg.label()
                );
                assert_eq!(
                    compile_span(seg, &d0_reg(5, 0)),
                    admitted,
                    "{} (Off, {byte_shift}): 0xd0 /5 is gated on IZARRAVM_BYTE_SHIFT_ROWS alone",
                    seg.label()
                );
            }
        }

        // --- On ----------------------------------------------------------------------------
        {
            let _arm = force_both_group_two_arms(RotateRowsArm::On, byte_shift);
            assert_eq!(
                compile_span(Seg::ThirtyTwo, &c0_reg(4, 0, 3)),
                ADMITTED,
                "(On, {byte_shift}): 0xc0 /4 at Dword is the pre-slice admission"
            );
            assert_eq!(
                compile_span(Seg::Sixteen, &c0_reg(4, 0, 3)),
                admitted,
                "(On, {byte_shift}): 0xc0 /4 at Word is the CONJUNCTION row and needs both arms"
            );
        }

        // --- HeatGated ---------------------------------------------------------------------
        {
            let _arm = force_both_group_two_arms(RotateRowsArm::HeatGated, byte_shift);
            for seg in SEGMENTS {
                // Over an UNRECORDED count byte the heat arm reads as admitting, so this cell is
                // the `On` cell.
                let four = if seg == Seg::ThirtyTwo {
                    ADMITTED
                } else {
                    admitted
                };
                assert_eq!(
                    compile_span(seg, &c0_reg(4, 0, 3)),
                    four,
                    "{} (HeatGated, {byte_shift}): an unrecorded count byte admits 0xc0 /4",
                    seg.label()
                );
                assert_eq!(
                    compile_span(seg, &c0_reg(5, 0, 3)),
                    admitted,
                    "{} (HeatGated, {byte_shift}): an unrecorded count byte admits 0xc0 /5",
                    seg.label()
                );

                // And over a RECORDED one, the gate separates the two rows. This is the cell
                // §5.2's decision is observable in, and the only one that can kill M13.
                assert_eq!(
                    compile_span_with_heat(seg, &c0_reg(4, 0, 3), Some(count_byte)),
                    REFUSED,
                    "{} (HeatGated, {byte_shift}): a heat record on the count byte must downgrade \
                     0xc0 /4 to a HardBoundary",
                    seg.label()
                );
                assert_eq!(
                    compile_span_with_heat(seg, &c0_reg(5, 0, 3), Some(count_byte)),
                    admitted,
                    "{} (HeatGated, {byte_shift}): 0xc0 /5 is NOT in rotate_row_count_byte and \
                     must be admitted over a heat-recorded count byte",
                    seg.label()
                );
                assert_eq!(
                    compile_span_with_heat(seg, &d0_reg(5, 0), Some(count_byte)),
                    admitted,
                    "{} (HeatGated, {byte_shift}): 0xd0 has no count byte to gate",
                    seg.label()
                );
            }
        }
    }
}

// =============================================================================================
// R11: the knob's own contract
// =============================================================================================

/// The spelling table, every arm and the refusal.
///
/// The `""` assertion is the one this knob needs most: `""` names the SAME arm as UNSET -- the
/// default -- deliberately NOT `parse_rotate_rows_arm`'s shape, which folds `""` in with `0`/`off`.
/// A wrapper that computes a leg value and produces `""` has MISSED a lookup; it has not said
/// "off". It is asserted against `byte_shift_rows_default_arm_for_test()` rather than against a
/// literal, so it keeps meaning what it says on the day the default flips -- which is the commit
/// that makes M20 (`"" => false`) a real defect rather than a synonym.
#[test]
fn byte_shift_rows_spelling_table_names_every_arm() {
    use std::env::VarError;
    let default = jit::direct::byte_shift_rows_default_arm_for_test();
    let unset = jit::direct::parse_byte_shift_rows_arm_for_test(Err(VarError::NotPresent));
    let empty = jit::direct::parse_byte_shift_rows_arm_for_test(Ok(String::new()));
    assert_eq!(unset, default, "unset must name the shipped default arm");
    assert_eq!(
        empty, default,
        "\"\" must name the SAME arm as unset -- the default -- not `0`/`off`"
    );
    for off in ["0", "off", "OFF", " off ", "Off"] {
        assert!(
            !jit::direct::parse_byte_shift_rows_arm_for_test(Ok(off.to_string())),
            "{off:?} must name the off arm"
        );
    }
    for on in ["1", "on", "ON", " on ", "On"] {
        assert!(
            jit::direct::parse_byte_shift_rows_arm_for_test(Ok(on.to_string())),
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "byte", "shift", "heat_gated"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_byte_shift_rows_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_BYTE_SHIFT_ROWS={typo:?} names no arm and must panic rather than silently \
             running the base"
        );
    }
}

/// THE DEFAULT PIN, and it is the one assertion that decides what a shipped binary admits.
///
/// It reads the AMBIENT knob deliberately -- no override -- and must agree with the ENVIRONMENT
/// rather than with a constant, because this suite is run on BOTH arms and a fixture that
/// hard-asserted one of them would make that impossible by construction. With the variable unset
/// the assertion reduces to "the default is what the parse table says", which is the claim.
#[test]
fn byte_shift_rows_ship_the_default_arm() {
    jit::direct::set_byte_shift_rows_for_test(None);
    let ambient = std::env::var("IZARRAVM_BYTE_SHIFT_ROWS");
    let expected = jit::direct::parse_byte_shift_rows_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::byte_shift_rows_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_BYTE_SHIFT_ROWS={ambient:?}"
    );
    if ambient.is_err() {
        assert_eq!(
            expected,
            jit::direct::byte_shift_rows_default_arm_for_test(),
            "an unset knob must resolve to the shipped default arm"
        );
    }
}

// =============================================================================================
// R12: the census row disappears
// =============================================================================================

/// The row the campaign grades against, from both ends of the gate.
///
/// With the knob OFF a `0xD0 /5` in a 16-bit segment produces a census row with
/// `stop_reason == "hard_boundary"`, `modrm_reg == Some(5)`, `operand_form == "register"` and
/// `operand_size == "word"` -- verbatim the tyrian-586 row this slice claims. With it ON, no row
/// for that shape exists.
///
/// The `/6` half is here because §0.2's normalisation is a KIND-level fact and not a census-level
/// one: the census bins by `insn.modrm.reg`, so a `/6` site keeps reporting `modrm_reg: 6` even
/// though the kind it produces carries `op: 4`.
#[test]
fn the_census_rows_disappear_with_the_gate() {
    for (reg, bytes) in [
        (5u8, d0_reg(5, 0)),
        (6, d0_reg(6, 0)),
        (5, c0_reg(5, 0, 3)),
        (6, c0_reg(6, 0, 3)),
    ] {
        let opcode = u16::from(bytes[0]);
        for on in [false, true] {
            let _arm = force_byte_shift_rows(on);
            let row = census_row_for(&bytes, opcode, reg);
            if on {
                assert!(
                    row.is_none(),
                    "{opcode:#04x} /{reg} must have NO census row on the ON arm; it is lowered"
                );
            } else {
                let row = row.unwrap_or_else(|| {
                    panic!("{opcode:#04x} /{reg} must record a census row on the OFF arm")
                });
                assert_eq!(row.operand_form, "register");
                assert_eq!(row.operand_size, "word");
                assert_eq!(row.prefix_mask, 0);
                assert_eq!(
                    row.stop_reason, "hard_boundary",
                    "the refusal must land in the SAME census arm the tyrian row was ranked in"
                );
            }
        }
    }
}

/// Compile the body in a 16-bit segment with the barrier census on and return the row for this
/// shape, if the walk recorded one.
fn census_row_for(body: &[u8], opcode: u16, reg: u8) -> Option<crate::DirectBarrierCensusRow> {
    let mut code = FILL_A.to_vec();
    code.extend_from_slice(body);
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = sixteen_bit_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_esp(STACK_TOP);
    cpu.enable_direct_barrier_census(true);
    for offset in 0..code.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x5000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    let _ = jit::direct::compile(&mut cpu, ENTRY, false);
    cpu.direct_barrier_census_snapshot()
        .expect("enabled census snapshot")
        .rows
        .into_iter()
        .find(|row| row.opcode == opcode && row.modrm_reg == Some(reg))
}
