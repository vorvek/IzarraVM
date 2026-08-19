// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Mutable imm8 lanes: `op r8, imm8` (`0x80 /r`, register destination) whose one immediate byte is
//! read out of guest RAM on every execution instead of being baked into host code, so a guest
//! patch of that byte keeps the compiled block.
//!
//! This is L2 arm 1 of the 2026-08-19 duke re-profile. The duke3d-586 SMC shape trace measures
//! `0x80` at 4.73 M patch events on the long row — the largest of the three `imm_len == 1` shapes
//! that together own 79% of all block kills — and it is the one whose emitted form has no
//! compile-time flag split, so it ships first. See `imm8_lane_for` for the full admission argument
//! and `rotate_rows_enabled` for why `0xC1`/`0xC0` and `0x0FA4` are NOT in this arm.
//!
//! **The arm is default OFF.** Every positive fixture here forces it on through
//! `set_imm8_lanes_for_test`, which is thread-local, so a fixture that forgot would test the
//! refusal and call it a lowering. `imm8_lane_is_refused_on_the_default_arm` is the fixture that
//! proves the forcing is doing something.
//!
//! The ALU sits at slot 1, never at the block's entry, for `cpu_jit_imm_lane_test`'s reason: an
//! opcode at the entry position is not reached by the emitted body on this fixture path, so an
//! entry-position slot would leave the lane emitter completely untested while every assertion
//! still passed.

use super::*;

/// Block entry.
const ENTRY: u32 = 0x500;
/// Offset of the `op r8, imm8` inside the block: after the two-byte `mov esi, esi`.
const ALU_OFFSET: u32 = 2;
/// The lane: the ALU's immediate byte, two bytes into a three-byte instruction.
const LANE: u32 = ENTRY + ALU_OFFSET + 2;

/// A `0x80 /r` fixture is described by its sub-opcode and its BYTE-register destination. Index
/// 0..=3 are AL/CL/DL/BL and 4..=7 are AH/CH/DH/BH — the high-lane read/write-back path the
/// emitter reaches through `emit_read_store_value`/`emit_write_gpr8`, which the lane arm shares
/// verbatim with the baked arm.
#[derive(Clone, Copy)]
struct Shape {
    op: u8,
    dst: u8,
}

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_mode(GswMode::Gsw486);
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

/// `mov esi, esi` / `op r8, imm8` / `mov edi, edi` / `hlt`. The HLT is a hard boundary, so the
/// block is exactly the three instructions before it.
fn image(shape: Shape, imm: u8) -> Vec<u8> {
    image_from(&[0x80, 0xc0 | (shape.op << 3) | shape.dst, imm])
}

/// The same frame around an arbitrary middle instruction, so the refusal fixtures can put a
/// near-miss shape in the admitted slot's place.
fn image_from(middle: &[u8]) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6];
    code.extend_from_slice(middle);
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

fn decode_at(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).unwrap();
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// Instruction starts for a block whose middle instruction is `middle_len` bytes long.
fn block_starts(middle_len: u32) -> [u32; 3] {
    [ENTRY, ENTRY + ALU_OFFSET, ENTRY + ALU_OFFSET + middle_len]
}

fn install(cpu: &mut CpuGsw, entry: u32, instructions: u8) -> jit::direct::BlockId {
    let key = jit::direct::key_for(cpu, entry, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, entry, true).expect("fixture block compiles");
    assert_eq!(
        compilation.span.instructions, instructions,
        "fixture block shape changed"
    );
    // Mirrors the production install site in `run.rs`, which is where both registration counters
    // are bumped; a fixture that installed without them would read zero lanes for a lane-bearing
    // block.
    let lanes = compilation.imm_lane_count() as u64;
    let imm8 = compilation.imm8_lane_count() as u64;
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("fixture block installs");
    cpu.perf.smc_lane_registrations += lanes;
    if imm8 != 0 {
        cpu.jit_direct.direct.note_imm8_lane_registrations(imm8);
    }
    id
}

fn arm(cpu: &mut CpuGsw, eax: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(eax);
    // Every byte-register index has a distinct, non-trivial home so a lane that read the wrong
    // one diverges rather than agreeing by accident.
    cpu.registers.set_ecx(0x1122_3344);
    cpu.registers.set_edx(0x55aa_7f80);
    cpu.registers.set_ebx(0xfedc_ba98);
    cpu.registers.set_esp(0xc000);
    cpu.registers.eflags = 0x8d7;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn test_bus(memory: Vec<u8>) -> TestBus {
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus
}

fn guest_store_byte(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u8) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Byte,
        u32::from(value),
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

fn guest_store_word(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Word,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

fn guest_store_dword(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Dword,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

/// Compile and install a lane block for `shape`, with the arm forced ON, and hand back everything
/// a patch-then-run needs.
fn lane_fixture(shape: Shape, imm: u8) -> (CpuGsw, TestBus, jit::direct::BlockId) {
    jit::direct::set_imm8_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(shape, imm));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "the fixture ALU did not take a lane; every assertion below would be vacuous"
    );
    assert_eq!(
        cpu.jit_direct.stall_snapshot().imm8_lane_registrations,
        1,
        "the lane must be counted as a ONE-BYTE lane, not folded into the dword class"
    );
    (cpu, bus, id)
}

/// Compile the fixture block for `shape` on the AMBIENT arm and report how many lanes it took.
///
/// `warm` is a list of data addresses to touch before compiling. A slot with a memory operand
/// needs its target page in the data caches or the walk returns `Retry` rather than a block, and a
/// `Retry` would make a zero-lane assertion vacuous for the wrong reason.
fn lanes_registered_for_warm(middle: &[u8], instructions: u8, warm: &[u32]) -> usize {
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image_from(middle));
    decode_at(&mut cpu, &mut bus, &block_starts(middle.len() as u32));
    for &address in warm {
        guest_store_dword(&mut cpu, &mut bus, address, 0);
    }
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    let compilation = match outcome {
        jit::direct::CompileOutcome::Compiled(c) => c,
        jit::direct::CompileOutcome::Retry => {
            panic!("fixture block did not compile: {middle:02x?} -> Retry")
        }
        jit::direct::CompileOutcome::StructuralReject(r) => {
            panic!("fixture block did not compile: {middle:02x?} -> reject {r:?}")
        }
    };
    assert_eq!(
        compilation.span.instructions, instructions,
        "fixture block shape changed; a refusal assertion on a truncated block is vacuous"
    );
    compilation.imm_lane_count()
}

fn lanes_registered_for(middle: &[u8], instructions: u8) -> usize {
    lanes_registered_for_warm(middle, instructions, &[])
}

/// THE EMITTER, and the file's central claim: a lane block's result must equal the interpreter's
/// for every one of the eight `/r` operations, at both byte-register lanes, and after the guest
/// patches the immediate between executions. The interpreter side runs the same bytes with no
/// block at all, so it re-decodes the patched instruction and is the reference by construction.
///
/// Flags are asserted three ways — architectural EFLAGS, the lazy `pending_flags` descriptor, and
/// full CPU equality — because the lane arm reaches `emit_alu_byte_preloaded` on a path the baked
/// arm does not (the operand arrives from memory rather than from a materialised constant), and a
/// descriptor that recorded the wrong `b` would agree on EFLAGS until something later consumed it.
#[test]
fn imm8_lane_matches_the_interpreter_across_patches_for_every_alu_op() {
    jit::direct::set_imm8_lanes_for_test(Some(true));
    // Values chosen to cross every byte boundary the flags can see: zero, the sign bit, the
    // all-ones borrow case, an odd/even parity pair, and a carry-out pair.
    let patches = [0x01u8, 0xff, 0x80, 0x7f, 0x00, 0x10, 0x81];
    for op in 0..8u8 {
        // AL (low lane) and AH (high lane): the emitter reaches AH through shift-and-mask on the
        // way in and mask-shift-or on the way out, so a lane that staged its operand into the
        // wrong half diverges here and only here.
        for dst in [0u8, 4] {
            let shape = Shape { op, dst };
            let mut native = flat_cpu();
            let mut native_bus = test_bus(image(shape, patches[0]));
            decode_at(&mut native, &mut native_bus, &block_starts(3));
            let id = install(&mut native, ENTRY, 3);
            assert_eq!(
                native.perf_counters().smc_lane_registrations,
                1,
                "op {op} dst {dst}: no lane, so this round would be vacuous"
            );

            let mut interpreter = flat_cpu();
            let mut interpreter_bus = test_bus(image(shape, patches[0]));
            decode_at(&mut interpreter, &mut interpreter_bus, &block_starts(3));

            for (round, &imm) in patches.iter().enumerate() {
                if round != 0 {
                    guest_store_byte(&mut native, &mut native_bus, LANE, imm);
                    guest_store_byte(&mut interpreter, &mut interpreter_bus, LANE, imm);
                }
                let eax = 0x1234_5678u32.wrapping_mul(round as u32 + 1);
                arm(&mut native, eax);
                arm(&mut interpreter, eax);

                let block = native
                    .jit_direct
                    .block(id)
                    .expect("the lane block survives every patch");
                assert!(
                    native
                        .try_run_direct_block_for_test(&mut native_bus, block)
                        .unwrap(),
                    "op {op} dst {dst}: native block did not run in round {round}"
                );
                for _ in 0..3 {
                    interpreter.cycle(&mut interpreter_bus).unwrap();
                }

                let label = format!("op {op} dst {dst} patch {imm:#04x}");
                assert_eq!(
                    native.registers, interpreter.registers,
                    "{label}: registers differ"
                );
                assert_eq!(
                    native.eflags(),
                    interpreter.eflags(),
                    "{label}: EFLAGS differ"
                );
                assert_eq!(
                    native.pending_flags, interpreter.pending_flags,
                    "{label}: lazy flags differ"
                );
                assert_eq!(
                    native_bus.memory, interpreter_bus.memory,
                    "{label}: guest memory differs"
                );
            }
        }
    }
}

/// The lane really is READ at run time rather than baked, stated as an arithmetic fact rather than
/// as agreement with a second model. An `ADD AL, imm8` block compiled at 1 and patched to 0x20
/// must add 0x20.
///
/// This is what separates "the lane emitter works" from "the lane emitter re-used the compile-time
/// immediate and the interpreter comparison above re-decoded the same stale line".
#[test]
fn imm8_lane_uses_the_current_immediate_not_the_compiled_one() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    guest_store_byte(&mut cpu, &mut bus, LANE, 0x20);
    arm(&mut cpu, 0x0000_0005);
    let block = cpu.jit_direct.block(id).expect("the block survives");
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eax() & 0xff, 0x25);
}

/// The accept case: exactly ONE byte at exactly the lane start. The block stays installed, the
/// next entry is still native, and — the property the whole class exists for — the interpreter's
/// decode line is still killed.
#[test]
fn imm8_lane_write_preserves_the_owning_block_and_its_native_entry() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    let before_kills = cpu.perf_counters().smc_narrow_kills;
    guest_store_byte(&mut cpu, &mut bus, LANE, 0x20);

    let after = cpu.perf_counters();
    assert_eq!(after.smc_lane_accepts, 1);
    assert_eq!(after.smc_lane_reject_width, 0);
    assert_eq!(after.smc_lane_reject_address, 0);
    assert!(
        after.smc_narrow_kills > before_kills,
        "the interpreter's decode line must still be killed"
    );

    let block = cpu
        .jit_direct
        .block(id)
        .expect("a lane write must not retire the owning block");
    arm(&mut cpu, 1);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the block must still be entered natively"
    );
    assert_eq!(cpu.registers.eax() & 0xff, 1 + 0x20);
}

/// The SLOW byte door. `write_linear_u8`'s direct-page arm called the value-less
/// `note_code_write` until this slice, which refuses the lane exemption outright — so on a persona
/// with no FastMap the lane would have registered, absorbed nothing, and read as a dead mechanism.
///
/// This is the fixture for that plumbing: same store, same lane, FastMap off. Reverting
/// `write_linear_u8` to `note_code_write` makes it fail on the accept counter and on block
/// liveness together.
#[test]
fn imm8_lane_absorbs_a_patch_on_the_slow_byte_write_path() {
    jit::direct::set_imm8_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    cpu.set_fast_map_enabled_for_test(false);
    let mut bus = test_bus(image(Shape { op: 0, dst: 0 }, 1));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(cpu.perf_counters().smc_lane_registrations, 1);

    guest_store_byte(&mut cpu, &mut bus, LANE, 0x20);

    assert_eq!(
        cpu.perf_counters().smc_lane_accepts,
        1,
        "the slow byte path must reach the value-aware door"
    );
    let block = cpu
        .jit_direct
        .block(id)
        .expect("the block must survive a slow-path lane patch");
    arm(&mut cpu, 1);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eax() & 0xff, 1 + 0x20);
}

/// A lane write is not code churn. Enough patches to cross the heat threshold several times over
/// must leave the heat map untouched, because that demotion pressure is what the lane exists to
/// remove — `lane_only` at `core.rs`'s heat block is the suppression, and it is keyed on the
/// accept COUNT rather than on any lane's width, so the new class is covered by construction.
/// This fixture is what turns "by construction" into a measurement.
///
/// **THE DECODE LINE IS RE-FETCHED BETWEEN PATCHES, and without that this test cannot fail.**
/// `heat_hit` is set by a narrow decode kill as well as by a block kill, and the first patch kills
/// the line — so a fixture that just stores 64 times charges heat at most once, never reaches the
/// threshold of 4, and passes with the suppression deleted. Re-decoding restores the line each
/// round, so every round produces a narrow kill and `smc_narrow_kills` climbs with the loop; the
/// assertion below pins that, which is what makes the zero-heat assertion beside it mean
/// "suppressed" rather than "never charged".
#[test]
fn imm8_lane_writes_contribute_no_smc_heat() {
    // The compiled immediate is deliberately outside the patch sequence below: a repatch to the
    // value already in memory is elided by G2 before it reaches the choke, so a colliding first
    // round would silently cost one accept.
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 0xee);
    const ROUNDS: u32 = 64;
    for round in 0..ROUNDS {
        cpu.set_eip(ENTRY + ALU_OFFSET);
        cpu.fetch_decoded(&mut bus, ENTRY + ALU_OFFSET).unwrap();
        // Every value distinct from the last: G2 same-value elision never reaches the choke, so
        // an identical repatch would not count.
        guest_store_byte(&mut cpu, &mut bus, LANE, (round + 1) as u8);
    }
    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, u64::from(ROUNDS));
    assert!(
        perf.smc_narrow_kills >= u64::from(ROUNDS),
        "every round must produce a decode-line kill, or the heat assertion below is vacuous:          narrow kills {}",
        perf.smc_narrow_kills
    );
    assert_eq!(
        perf.smc_heat_chunks_hot, 0,
        "lane patches must not heat the chunk"
    );
    assert_eq!(perf.smc_heat_demotions, 0);
    assert!(
        cpu.jit_direct.block(id).is_some(),
        "the block must survive all of them"
    );
}

/// The width check, fail-closed, from the wide side: a FOUR-byte store starting at the one-byte
/// lane rewrites the immediate and the two instruction bytes after it, so it is not the admitted
/// shape and takes the normal invalidation path.
///
/// Part of the slice's mutation record. Relaxing the per-lane width test to "any width that starts
/// at a lane" makes this fail on both the counter and the block-liveness assertion.
#[test]
fn dword_write_at_the_imm8_lane_start_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    guest_store_dword(&mut cpu, &mut bus, LANE, 0x1122_3344);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 1);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a wider write over the immediate must retire the block"
    );
}

/// The same from the two-byte side, which also crosses out of the instruction.
#[test]
fn word_write_at_the_imm8_lane_start_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    guest_store_word(&mut cpu, &mut bus, LANE, 0x1234);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 1);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(cpu.jit_direct.block(id).is_none());
}

/// `smc_lane_reject_width` MUST NOT CHANGE MEANING now that a second width class exists, and this
/// is the fixture that pins it: a one-byte store landing exactly on a DWORD lane is still a
/// width rejection and still kills the block. Only a lane that registered at width one accepts a
/// width-one store.
///
/// The fixture is the `0x81` shape, deliberately, with the imm8 arm forced ON — so the block holds
/// a four-byte lane while the one-byte class is live, which is exactly the state a naive
/// "accept width 1 anywhere a lane starts" implementation would get wrong.
#[test]
fn byte_write_at_a_dword_lane_start_still_rejects_on_width() {
    jit::direct::set_imm8_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    // `81 c0 ii ii ii ii` — ADD EAX, imm32, the `imm_lane_for` shape, in the same block frame.
    let mut bus = test_bus(image_from(&[0x81, 0xc0, 0x78, 0x56, 0x34, 0x12]));
    decode_at(&mut cpu, &mut bus, &block_starts(6));
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(cpu.perf_counters().smc_lane_registrations, 1);
    assert_eq!(
        cpu.jit_direct.stall_snapshot().imm8_lane_registrations,
        0,
        "a 0x81 slot must not be counted as a one-byte lane"
    );

    guest_store_byte(&mut cpu, &mut bus, ENTRY + ALU_OFFSET + 2, 0x99);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(
        perf.smc_lane_reject_width, 1,
        "a byte patch of a dword lane is a WIDTH rejection, as it always was"
    );
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a partial patch of a dword immediate must still retire the block"
    );
}

/// THE OFF ARM'S COUNTERS DO NOT MOVE, and this is the fixture the adversarial review demanded.
///
/// The slow byte path's door is arm-selected (`note_code_byte_write_hit`, core.rs). Taking the
/// value-aware door unconditionally is the obvious simplification and it is WRONG: the permitting
/// door also runs the lane-rejection accounting, so a byte store landing on a DWORD lane would
/// start counting in `smc_lane_reject_width` on the SHIPPED arm — a counter
/// `dev_docs/duke-reprofile-2026-08-19.md` reads as a baseline ("reads 0 today") and compares the
/// 08-16 census against. The kill is identical either way; only the counters and the choke's
/// per-block lane walk differ, which is exactly what makes the mistake invisible without this
/// fixture.
///
/// So: same block, same store, same dword lane as
/// `byte_write_at_a_dword_lane_start_still_rejects_on_width` — but with the arm OFF, both
/// rejection counters must stay at zero while the block still dies.
#[test]
fn the_off_arm_moves_no_rejection_counter_on_a_byte_write() {
    jit::direct::set_imm8_lanes_for_test(Some(false));
    let mut cpu = flat_cpu();
    cpu.set_fast_map_enabled_for_test(false);
    // `81 c0 ii ii ii ii` — ADD EAX, imm32, which takes a four-byte lane on every arm.
    let mut bus = test_bus(image_from(&[0x81, 0xc0, 0x78, 0x56, 0x34, 0x12]));
    decode_at(&mut cpu, &mut bus, &block_starts(6));
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "the dword lane must exist, or the counters below are zero for the wrong reason"
    );

    guest_store_byte(&mut cpu, &mut bus, ENTRY + ALU_OFFSET + 2, 0x99);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(
        perf.smc_lane_reject_width, 0,
        "the shipped arm must not start counting byte writes as lane rejections"
    );
    assert_eq!(
        perf.smc_lane_reject_address, 0,
        "the shipped arm must not start counting byte writes as lane rejections"
    );
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "the block still dies; only the counters differ between the arms"
    );
}

/// A write to the instruction's OTHER bytes — its opcode and ModRM — is structural and retires the
/// block. It overlaps no lane byte, so it is not even a lane rejection.
#[test]
fn structural_write_to_the_same_instruction_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    guest_store_byte(&mut cpu, &mut bus, ENTRY + ALU_OFFSET, 0x81);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 0);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a write to the opcode bytes must retire the block"
    );
}

/// A two-byte write that overlaps the lane but starts one byte early: it rewrites the ModRM as
/// well, so the resulting instruction is not the one that compiled. An ADDRESS rejection, not a
/// width one, because it does not start at the lane.
#[test]
fn straddling_write_over_the_imm8_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    guest_store_word(&mut cpu, &mut bus, LANE - 1, 0x1122);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 0);
    assert_eq!(perf.smc_lane_reject_address, 1);
    assert!(cpu.jit_direct.block(id).is_none());
}

/// Device and HLE writes never take the exemption, even when their range is byte-for-byte the
/// lane. They arrive through the value-less choke with no store path behind them.
#[test]
fn device_write_at_the_imm8_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(Shape { op: 0, dst: 0 }, 1);
    cpu.note_device_memory_write_range(LANE, 1);

    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a device write must take the normal invalidation path"
    );
    let _ = &mut bus;
}

/// THE DEFAULT ARM. Without the thread-local forcing, the very same block registers no lane and
/// the very same patch retires it — which is what makes every fixture above a statement about the
/// slice rather than about the backend it was already.
#[test]
fn imm8_lane_is_refused_on_the_default_arm() {
    jit::direct::set_imm8_lanes_for_test(Some(false));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(Shape { op: 0, dst: 0 }, 5));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        0,
        "the default arm registers no one-byte lane"
    );

    // The baked immediate still applies, so the off arm is a real lowering and not a refusal to
    // compile.
    arm(&mut cpu, 100);
    let block = cpu.jit_direct.block(id).unwrap();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eax() & 0xff, 105);

    guest_store_byte(&mut cpu, &mut bus, LANE, 9);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "with no lane, the patch a lane would have absorbed retires the block"
    );
}

/// The admission bars, one fixture per bar, each a near-miss of the admitted shape. Every one of
/// these compiles a three-instruction block — `lanes_registered_for` asserts that — so a zero lane
/// count is a REFUSAL and not a truncated walk.
#[test]
fn near_miss_shapes_take_no_imm8_lane() {
    jit::direct::set_imm8_lanes_for_test(Some(true));
    // The AL-accumulator short form `04 ii`: `AluByteImm` too, but its immediate is at offset ONE,
    // so a lane at `physical + 2` would name the NEXT instruction's first byte. The opcode test is
    // what excludes it, and this is that test's fixture.
    assert_eq!(lanes_registered_for(&[0x04, 0x11], 3), 0, "0x04 AL, imm8");
    // A segment override moves the immediate off offset 2. Refused rather than re-derived.
    assert_eq!(
        lanes_registered_for(&[0x26, 0x80, 0xc0, 0x11], 3),
        0,
        "prefixed 0x80"
    );
    // An operand-size override does not change what `0x80` does — byte width is a property of the
    // form — but it is still a prefix byte and still moves the immediate.
    assert_eq!(
        lanes_registered_for(&[0x66, 0x80, 0xc0, 0x11], 3),
        0,
        "0x66-prefixed 0x80"
    );
    // `0x83 /r`, the SIGN-EXTENDED dword group: `imm_len == 1` like the admitted shape, but the
    // kind is `AluImm` and the emitted code reads a dword. Neither matcher takes it, and a lane
    // that did would name one byte of a four-byte read.
    assert_eq!(
        lanes_registered_for(&[0x83, 0xc0, 0x11], 3),
        0,
        "0x83 sign-extended imm8"
    );
    // The MEMORY destination form of `0x80`: `AluMemDest`, whose emitter has no lane arm.
    assert_eq!(
        lanes_registered_for_warm(&[0x80, 0x05, 0x00, 0x20, 0x00, 0x00, 0x11], 3, &[0x2000]),
        0,
        "0x80 with a memory destination"
    );
}

/// The page-kind guard, stated positively and verbatim from the dword class: a lane is created
/// ONLY from the fetch-page cache, the one direct-page cache that cannot hold a device-aperture
/// pointer. With the fetch entry gone but the data caches warm for the same page, the qualifying
/// `0x80` must compile with a BAKED immediate — correct as ever, just not parameterized.
#[test]
fn a_page_the_fetch_cache_cannot_see_gets_no_imm8_lane() {
    jit::direct::set_imm8_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(Shape { op: 0, dst: 0 }, 5));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    // Warm the data-write cache for the code page (well past the block's bytes), then drop the
    // fetch entry — the state every code write already leaves behind.
    guest_store_dword(&mut cpu, &mut bus, ENTRY + 0x40, 0x1234_5678);
    cpu.fetch_page.invalidate();

    // `install` only accepts a key the cache has already SEEN, which is what `probe` records.
    let key = jit::direct::key_for(&cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut cpu, ENTRY, true).expect("the block still compiles");
    assert_eq!(
        compilation.imm_lane_count(),
        0,
        "no fetch-cached page, no lane"
    );
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("the baked-immediate block installs");
    arm(&mut cpu, 100);
    let block = cpu.jit_direct.block(id).unwrap();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.registers.eax() & 0xff,
        105,
        "the baked immediate still applies"
    );
}

/// Compile `middle` in the fixture frame under a forced arm and hand back the emitted code's
/// LENGTH.
///
/// The length rather than the bytes, and that is a limitation of the fixture rather than a
/// weakening of the claim: two compilations bake different host pointers (link-cell and portal
/// addresses, and a lane's own host pointer), so a byte comparison across them fails on
/// allocation noise whatever the arm. The length is stable, and it is exactly the quantity a
/// stray lane moves — the lane arm emits `mov r64, imm64` plus `movzx r32, byte [r64]` where the
/// baked arm emits one `mov r32, imm32`.
fn emitted_len_under_arm(middle: &[u8], arm: bool) -> usize {
    jit::direct::set_imm8_lanes_for_test(Some(arm));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image_from(middle));
    decode_at(&mut cpu, &mut bus, &block_starts(middle.len() as u32));
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    // Restored before the match, so a `panic!` below cannot leave this thread's override forced.
    // The override is per-thread and the harness reuses threads across tests.
    jit::direct::set_imm8_lanes_for_test(None);
    match outcome {
        jit::direct::CompileOutcome::Compiled(c) => c.code.len(),
        jit::direct::CompileOutcome::Retry => panic!("fixture block did not compile: Retry"),
        jit::direct::CompileOutcome::StructuralReject(r) => {
            panic!("fixture block did not compile: reject {r:?}")
        }
    }
}

/// THE OFF ARM PAYS NOTHING, stated as emitted code rather than as an argument.
///
/// The admitted shape's two arms must differ — otherwise the knob is decorative — and every OTHER
/// shape must emit the same code under both arms, which is what says the arm is a lane admission
/// and not a second code path that a non-`0x80` block also walks through.
#[test]
fn the_off_arm_emits_the_same_code_for_everything_it_does_not_admit() {
    let admitted = &[0x80, 0xc0, 0x11];
    assert_ne!(
        emitted_len_under_arm(admitted, false),
        emitted_len_under_arm(admitted, true),
        "the admitted shape must lower differently under the two arms, or the knob does nothing"
    );
    for middle in [
        // The AL short form, an `AluByteImm` the arm must not touch.
        &[0x04, 0x11][..],
        // The dword immediate family, which has its own lane and its own knob.
        &[0x81, 0xc0, 0x78, 0x56, 0x34, 0x12][..],
        // A byte ALU with a register source: the emitter the lane arm shares its tail with.
        &[0x00, 0xc4][..],
        // A plain register move, the frame's own instruction repeated.
        &[0x89, 0xd8][..],
    ] {
        assert_eq!(
            emitted_len_under_arm(middle, false),
            emitted_len_under_arm(middle, true),
            "{middle:02x?} must emit the same code under both arms"
        );
    }
}

/// The `IZARRAVM_IMM8_LANES` spelling table. `imm8_lanes_enabled` caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls — hence the parse function is exercised directly.
#[test]
fn imm8_lanes_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_imm8_lanes_arm_for_test;
    assert!(!parse(Err(VarError::NotPresent)), "unset is the base");
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must be the base");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must be the slice");
    }
}

/// A typo must not silently run the base. See `parse_imm8_lanes_arm` for why guessing is worse
/// than failing: a leg that quietly ran the base would be read as the slice doing nothing.
#[test]
#[should_panic(expected = "names no arm")]
fn an_unrecognised_imm8_lanes_spelling_panics() {
    jit::direct::parse_imm8_lanes_arm_for_test(Ok("true".to_string()));
}
