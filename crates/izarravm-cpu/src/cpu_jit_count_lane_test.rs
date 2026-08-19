// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Mutable COUNT lanes: the group-2 shift/rotate count byte (`0xC1 /0` ROL, `0xC1 /1` ROR,
//! `0xC1 /4..=7` the dword shifts, and `0xC0 /4` SHL r8 -- register forms, no prefixes) read out of
//! guest RAM on every execution instead of being baked into host code, so a guest patch of that
//! byte keeps the compiled block.
//!
//! This is L2 arm 2 of the 2026-08-19 duke re-profile, and it is the RE-TEST TRIGGER
//! `rotate_rows_enabled` names: duke3d patches the count byte of its group-2 shifts, and since the
//! `IZARRAVM_ROTATE_ROWS` default flip those sites are admitted, so every count patch kills a
//! compiled block. See `count_lane_for` for the admission argument and `emit_rotate_reg_lane` /
//! `emit_shift_lane` for the runtime three-way branch that is this slice's whole correctness cost.
//!
//! **The arm is default OFF.** Every positive fixture here forces it on through
//! `set_count_lanes_for_test`, which is thread-local, so a fixture that forgot would test the
//! refusal and call it a lowering. `count_lane_is_refused_on_the_default_arm` is the fixture that
//! proves the forcing is doing something.
//!
//! **THE FRAME CARRIES A LIVE LAZY-FLAGS DESCRIPTOR into the laned instruction**, and that is not
//! decoration. The three count shapes differ mostly in what they do to the DESCRIPTOR: count 0 must
//! leave it untouched, 2..31 must rewrite its CF override in place, and 1 must clear it after
//! publishing. A frame with no descriptor live collapses all three onto the same observable state
//! and every assertion below would pass with the branch deleted. So slot 1 of every fixture block
//! is `add ebx, ebx`, whose emitted form leaves a descriptor, and the group-2 instruction is slot
//! 2. `a_zero_count_preserves_the_live_descriptor` is the fixture that pins the frame itself.
//!
//! The group-2 instruction is never at the block's entry, for `cpu_jit_imm_lane_test`'s reason: an
//! opcode at the entry position is not reached by the emitted body on this fixture path, so an
//! entry-position slot would leave the lane emitter untested while every assertion still passed.

use super::*;

/// Block entry.
const ENTRY: u32 = 0x500;
/// Offset of the descriptor-producing `add ebx, ebx`: after the two-byte `mov esi, esi`.
const SEED_OFFSET: u32 = 2;
/// Offset of the group-2 instruction inside the block.
const OP_OFFSET: u32 = 4;
/// The lane: the group-2 count byte, two bytes into a three-byte instruction.
const LANE: u32 = ENTRY + OP_OFFSET + 2;
/// Instructions in a fixture block: the two frame slots, the group-2 slot, and the trailing
/// `mov edi, edi` before the HLT boundary.
const BLOCK_INSNS: u8 = 4;

/// The count shapes every emitter fixture sweeps. `0x20` masks to 0 and `0x21` masks to 1, which is
/// what turns "the mask is applied to the loaded byte before the shape test" into a measurement:
/// an implementation that selected on the raw byte would run them as counts 32 and 33 and diverge
/// on both the result and the descriptor.
const COUNTS: [u8; 8] = [0, 1, 2, 3, 31, 0x20, 0x21, 0xff];

/// A group-2 fixture is described by its opcode and its `/op` sub-opcode and destination.
///
/// `0xC1` reaches ROL (/0), ROR (/1) and the four dword shifts (/4..=7) with a 32-bit register
/// destination; `0xC0 /4` reaches SHL r8, where `dst` is a BYTE-register index and 4..=7 name
/// AH/CH/DH/BH rather than the homes of EBP/ESI/EDI.
#[derive(Clone, Copy)]
struct Shape {
    opcode: u8,
    op: u8,
    dst: u8,
}

impl Shape {
    fn label(self) -> String {
        format!("{:#04x} /{} dst {}", self.opcode, self.op, self.dst)
    }
}

/// Every mandatory form: both rotates, all four dword shifts, and the byte shift at both a low and
/// a high byte-register lane.
const SHAPES: [Shape; 10] = [
    Shape {
        opcode: 0xc1,
        op: 0,
        dst: 0,
    },
    Shape {
        opcode: 0xc1,
        op: 0,
        dst: 3,
    },
    Shape {
        opcode: 0xc1,
        op: 1,
        dst: 0,
    },
    Shape {
        opcode: 0xc1,
        op: 4,
        dst: 0,
    },
    Shape {
        opcode: 0xc1,
        op: 5,
        dst: 0,
    },
    Shape {
        opcode: 0xc1,
        op: 6,
        dst: 2,
    },
    Shape {
        opcode: 0xc1,
        op: 7,
        dst: 2,
    },
    Shape {
        opcode: 0xc0,
        op: 4,
        dst: 0,
    },
    Shape {
        opcode: 0xc0,
        op: 4,
        dst: 4,
    },
    Shape {
        opcode: 0xc0,
        op: 4,
        dst: 3,
    },
];

/// The plain ROL fixture the single-purpose tests use.
const ROL: Shape = Shape {
    opcode: 0xc1,
    op: 0,
    dst: 0,
};

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

/// `mov esi, esi` / `add ebx, ebx` / `<group-2>` / `mov edi, edi` / `hlt`. The HLT is a hard
/// boundary, so the block is exactly the four instructions before it.
fn image(shape: Shape, count: u8) -> Vec<u8> {
    image_from(&[shape.opcode, 0xc0 | (shape.op << 3) | shape.dst, count])
}

/// The same frame around an arbitrary middle instruction, so the refusal fixtures can put a
/// near-miss shape in the admitted slot's place.
fn image_from(middle: &[u8]) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    // `add ebx, ebx` is the descriptor seed; see the module comment for why every fixture needs
    // one live across the laned instruction.
    let mut code = vec![0x89, 0xf6, 0x01, 0xdb];
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
fn block_starts(middle_len: u32) -> [u32; 4] {
    [
        ENTRY,
        ENTRY + SEED_OFFSET,
        ENTRY + OP_OFFSET,
        ENTRY + OP_OFFSET + middle_len,
    ]
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
    let counts = compilation.count_lane_count() as u64;
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("fixture block installs");
    cpu.perf.smc_lane_registrations += lanes;
    if counts != 0 {
        cpu.jit_direct.direct.note_count_lane_registrations(counts);
    }
    id
}

/// Register and flag state before every run. Values are chosen so that every destination the
/// shapes reach carries a distinct, non-trivial pattern: a lane that read or wrote the wrong
/// register diverges rather than agreeing by accident.
fn arm(cpu: &mut CpuGsw, eax: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(eax);
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

/// Compile and install a count-lane block for `shape`, with the arm forced ON, and hand back
/// everything a patch-then-run needs.
fn lane_fixture(shape: Shape, count: u8) -> (CpuGsw, TestBus, jit::direct::BlockId) {
    jit::direct::set_count_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(shape, count));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    let id = install(&mut cpu, ENTRY, BLOCK_INSNS);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "the fixture group-2 slot did not take a lane; every assertion below would be vacuous"
    );
    assert_eq!(
        cpu.jit_direct.stall_snapshot().count_lane_registrations,
        1,
        "the lane must be counted as a COUNT lane, not folded into another class"
    );
    (cpu, bus, id)
}

/// Compile the fixture block for `middle` on the AMBIENT arm and report how many lanes it took.
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

/// Run the fixture block natively and the same bytes interpreted, and assert they agree on
/// everything the campaign compares.
fn assert_agrees(
    native: &mut CpuGsw,
    native_bus: &mut TestBus,
    interpreter: &mut CpuGsw,
    interpreter_bus: &mut TestBus,
    id: jit::direct::BlockId,
    eax: u32,
    label: &str,
) {
    arm(native, eax);
    arm(interpreter, eax);
    let block = native
        .jit_direct
        .block(id)
        .expect("the lane block survives every patch");
    assert!(
        native
            .try_run_direct_block_for_test(native_bus, block)
            .unwrap(),
        "{label}: native block did not run"
    );
    for _ in 0..BLOCK_INSNS {
        interpreter.cycle(interpreter_bus).unwrap();
    }
    assert_eq!(
        native.registers, interpreter.registers,
        "{label}: registers differ"
    );
    assert_eq!(
        native.eflags(),
        interpreter.eflags(),
        "{label}: EFLAGS differ"
    );
    // The RAW descriptor, not just the materialised word. A count-2 rotate that published RBP
    // wholesale instead of rewriting the CF override in place would agree on `eflags()` here and
    // differ on every byte of this, until something later consumed it.
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "{label}: lazy flags differ"
    );
    assert_eq!(
        native_bus.memory, interpreter_bus.memory,
        "{label}: guest memory differs"
    );
}

/// THE EMITTER, and the file's central claim: a count-lane block must equal the interpreter for
/// every admitted form at every count shape, with a live descriptor crossing the instruction.
///
/// The interpreter side runs the same bytes with no block at all, so it re-decodes the patched
/// instruction and is the reference by construction. `shift_rotate` is the oracle here, not the
/// SDM: the manual leaves OF undefined above count 1 and CF undefined at counts past the operand
/// width, and this tree matches its own interpreter across the whole range.
#[test]
fn count_lane_matches_the_interpreter_for_every_form_and_count() {
    jit::direct::set_count_lanes_for_test(Some(true));
    for shape in SHAPES {
        for (round, &count) in COUNTS.iter().enumerate() {
            // Compiled at a count that is NOT the one under test, so a lane that quietly re-used
            // its compile-time immediate fails instead of agreeing.
            let mut native = flat_cpu();
            let mut native_bus = test_bus(image(shape, 0x11));
            decode_at(&mut native, &mut native_bus, &block_starts(3));
            let id = install(&mut native, ENTRY, BLOCK_INSNS);
            assert_eq!(
                native.perf_counters().smc_lane_registrations,
                1,
                "{}: no lane, so this round would be vacuous",
                shape.label()
            );

            let mut interpreter = flat_cpu();
            let mut interpreter_bus = test_bus(image(shape, 0x11));
            decode_at(&mut interpreter, &mut interpreter_bus, &block_starts(3));

            guest_store_byte(&mut native, &mut native_bus, LANE, count);
            guest_store_byte(&mut interpreter, &mut interpreter_bus, LANE, count);

            let label = format!("{} count {count:#04x}", shape.label());
            assert_agrees(
                &mut native,
                &mut native_bus,
                &mut interpreter,
                &mut interpreter_bus,
                id,
                0x1234_5678u32.wrapping_mul(round as u32 + 1) | 1,
                &label,
            );
        }
    }
}

/// THE FRAME IS NON-VACUOUS. A descriptor really is live when the group-2 slot runs, and a masked
/// count of zero really does leave it exactly as it found it.
///
/// Without this the whole count-0 contract is unobservable: with no descriptor live, "publish RBP
/// and clear the descriptor" and "touch nothing" settle to the same architectural state, and the
/// count-0 arm of the runtime branch could be deleted with every other fixture still passing.
#[test]
fn a_zero_count_preserves_the_live_descriptor() {
    for count in [0u8, 0x20] {
        let (mut cpu, mut bus, id) = lane_fixture(ROL, 3);
        guest_store_byte(&mut cpu, &mut bus, LANE, count);
        arm(&mut cpu, 0x8000_0001);
        let before = cpu.registers.eax();
        let block = cpu.jit_direct.block(id).expect("the block survives");
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_ne!(
            cpu.pending_flags,
            PendingFlags::default(),
            "count {count:#04x}: the seed ALU must leave a live descriptor, or the count-0 \
             contract is unobservable"
        );
        assert_eq!(
            cpu.registers.eax(),
            before,
            "count {count:#04x}: a masked count of zero must not touch the destination"
        );
    }
}

/// The lane really is READ at run time rather than baked, stated as an arithmetic fact rather than
/// as agreement with a second model. A `ROL EAX, 3` block patched to 8 must rotate by 8.
///
/// This is what separates "the lane emitter works" from "the lane emitter re-used the compile-time
/// count and the interpreter comparison above re-decoded the same stale line".
#[test]
fn count_lane_uses_the_current_count_not_the_compiled_one() {
    let (mut cpu, mut bus, id) = lane_fixture(ROL, 3);
    guest_store_byte(&mut cpu, &mut bus, LANE, 8);
    arm(&mut cpu, 0x0000_00ff);
    let block = cpu.jit_direct.block(id).expect("the block survives");
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eax(), 0x0000_ff00);
}

/// PATCH THEN RE-EXECUTE, the property the class exists for: the guest rewrites the count byte
/// through its own store path and the SAME installed block keeps running, with no recompile, at
/// the new count's semantics.
///
/// **The sequence deliberately includes patches to 0 and to 1**, the two shapes the compile-time
/// split could not survive: a block compiled at count 3 bakes the 2..31 flag path, so a patch to 1
/// must start publishing `CF|OF` and clearing the descriptor, and a patch to 0 must stop touching
/// flags altogether. An implementation that laned the count but kept the compile-time flag shape
/// passes every "does it rotate by the right amount" assertion and fails here.
#[test]
fn a_patched_count_re_executes_without_recompiling() {
    for shape in SHAPES {
        jit::direct::set_count_lanes_for_test(Some(true));
        let mut native = flat_cpu();
        let mut native_bus = test_bus(image(shape, 3));
        decode_at(&mut native, &mut native_bus, &block_starts(3));
        let id = install(&mut native, ENTRY, BLOCK_INSNS);
        assert_eq!(native.perf_counters().smc_lane_registrations, 1);

        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus(image(shape, 3));
        decode_at(&mut interpreter, &mut interpreter_bus, &block_starts(3));

        let before_kills = native.perf_counters().smc_narrow_kills;
        // Every value distinct from the last: G2 same-value elision never reaches the choke, so an
        // identical repatch would not count as an accept.
        let patches = [1u8, 0, 2, 1, 31, 0, 0x21, 0x20, 7];
        for (round, &count) in patches.iter().enumerate() {
            guest_store_byte(&mut native, &mut native_bus, LANE, count);
            guest_store_byte(&mut interpreter, &mut interpreter_bus, LANE, count);
            assert_eq!(
                native.perf_counters().smc_lane_accepts,
                round as u64 + 1,
                "{}: patch {count:#04x} must be absorbed as a lane accept",
                shape.label()
            );
            assert_agrees(
                &mut native,
                &mut native_bus,
                &mut interpreter,
                &mut interpreter_bus,
                id,
                0x89ab_cdefu32.wrapping_mul(round as u32 + 1) | 1,
                &format!("{} patch {count:#04x}", shape.label()),
            );
        }
        let after = native.perf_counters();
        assert_eq!(after.smc_lane_reject_width, 0);
        assert_eq!(after.smc_lane_reject_address, 0);
        assert!(
            after.smc_narrow_kills > before_kills,
            "{}: the interpreter's decode line must still be killed",
            shape.label()
        );
        assert!(
            native.jit_direct.block(id).is_some(),
            "{}: the block must survive every patch",
            shape.label()
        );
    }
}

/// The SLOW byte door, arm-selected at `note_code_byte_write_hit` (core.rs). That test is an OR
/// over the two one-byte arms since this slice, and this is the fixture for the half this slice
/// added: with FastMap off and `IZARRAVM_IMM8_LANES` explicitly OFF, only the count arm can open
/// the value-aware door. Deleting the `count_lanes_enabled()` half makes this fail on the accept
/// counter and on block liveness together.
#[test]
fn the_count_arm_alone_opens_the_slow_byte_door() {
    jit::direct::set_count_lanes_for_test(Some(true));
    jit::direct::set_imm8_lanes_for_test(Some(false));
    let mut cpu = flat_cpu();
    cpu.set_fast_map_enabled_for_test(false);
    let mut bus = test_bus(image(ROL, 3));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    let id = install(&mut cpu, ENTRY, BLOCK_INSNS);
    assert_eq!(cpu.perf_counters().smc_lane_registrations, 1);

    guest_store_byte(&mut cpu, &mut bus, LANE, 8);

    assert_eq!(
        cpu.perf_counters().smc_lane_accepts,
        1,
        "the slow byte path must reach the value-aware door on the count arm alone"
    );
    let block = cpu
        .jit_direct
        .block(id)
        .expect("the block must survive a slow-path lane patch");
    arm(&mut cpu, 0x0000_00ff);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eax(), 0x0000_ff00);
    jit::direct::set_imm8_lanes_for_test(None);
}

/// A lane write is not code churn. Enough patches to cross the heat threshold several times over
/// must leave the heat map untouched, because that demotion pressure is what the lane exists to
/// remove -- `lane_only` at `core.rs`'s heat block is the suppression, and it is keyed on the
/// accept COUNT rather than on any lane's class, so the count lane inherits it by construction.
/// This fixture is what turns "by construction" into a measurement.
///
/// **THE DECODE LINE IS RE-FETCHED BETWEEN PATCHES, and without that this test cannot fail.**
/// `heat_hit` is set by a narrow decode kill as well as by a block kill, and the first patch kills
/// the line -- so a fixture that just stores 64 times charges heat at most once, never reaches the
/// threshold, and passes with the suppression deleted.
#[test]
fn count_lane_writes_contribute_no_smc_heat() {
    // The compiled count is deliberately outside the patch sequence below: a repatch to the value
    // already in memory is elided by G2 before it reaches the choke, so a colliding first round
    // would silently cost one accept.
    let (mut cpu, mut bus, id) = lane_fixture(ROL, 0xee);
    const ROUNDS: u32 = 64;
    for round in 0..ROUNDS {
        cpu.set_eip(ENTRY + OP_OFFSET);
        cpu.fetch_decoded(&mut bus, ENTRY + OP_OFFSET).unwrap();
        guest_store_byte(&mut cpu, &mut bus, LANE, (round + 1) as u8);
    }
    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, u64::from(ROUNDS));
    assert!(
        perf.smc_narrow_kills >= u64::from(ROUNDS),
        "every round must produce a decode-line kill, or the heat assertion below is vacuous: \
         narrow kills {}",
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
/// lane rewrites the count and the two instruction bytes after it, so it is not the admitted shape
/// and takes the normal invalidation path.
#[test]
fn dword_write_at_the_count_lane_start_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(ROL, 3);
    guest_store_dword(&mut cpu, &mut bus, LANE, 0x1122_3344);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 1);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a wider write over the count must retire the block"
    );
}

/// A two-byte write that overlaps the lane but starts one byte early: it rewrites the ModRM as
/// well, so the resulting instruction is not the one that compiled. An ADDRESS rejection, not a
/// width one, because it does not start at the lane.
#[test]
fn straddling_write_over_the_count_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(ROL, 3);
    guest_store_word(&mut cpu, &mut bus, LANE - 1, 0x1122);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 0);
    assert_eq!(perf.smc_lane_reject_address, 1);
    assert!(cpu.jit_direct.block(id).is_none());
}

/// A write to the instruction's OTHER bytes -- its opcode and ModRM -- is structural and retires
/// the block. It overlaps no lane byte, so it is not even a lane rejection. This is the bar that
/// keeps a `C1 -> D1` or a `/0 -> /2` patch (ROL to RCL, which is not lowered at all) from being
/// absorbed by a lane that only ever meant the count.
#[test]
fn structural_write_to_the_same_instruction_retires_the_block() {
    for offset in [0u32, 1] {
        let (mut cpu, mut bus, id) = lane_fixture(ROL, 3);
        guest_store_byte(&mut cpu, &mut bus, ENTRY + OP_OFFSET + offset, 0xd1);

        let perf = cpu.perf_counters();
        assert_eq!(perf.smc_lane_accepts, 0, "offset {offset}");
        assert_eq!(perf.smc_lane_reject_width, 0, "offset {offset}");
        assert_eq!(perf.smc_lane_reject_address, 0, "offset {offset}");
        assert!(
            cpu.jit_direct.block(id).is_none(),
            "offset {offset}: a write to the opcode bytes must retire the block"
        );
    }
}

/// Device and HLE writes never take the exemption, even when their range is byte-for-byte the
/// lane. They arrive through the value-less choke with no store path behind them.
#[test]
fn device_write_at_the_count_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(ROL, 3);
    cpu.note_device_memory_write_range(LANE, 1);

    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a device write must take the normal invalidation path"
    );
    let _ = &mut bus;
}

/// THE OFF ARM. Without the thread-local forcing, the very same block registers no lane, the baked
/// count still applies, and the very same patch retires the block -- which is what makes every
/// fixture above a statement about the slice rather than about the backend it was already.
///
/// The two rejection counters are pinned at zero on this arm for
/// `the_off_arm_moves_no_rejection_counter_on_a_byte_write`'s reason (cpu_jit_imm8_lane_test): with
/// both one-byte arms off, `note_code_byte_write_hit` must keep the value-less door it always had,
/// counters included. FastMap is off here because that is the path the door selects on.
#[test]
fn count_lane_is_refused_on_the_default_arm() {
    jit::direct::set_count_lanes_for_test(Some(false));
    jit::direct::set_imm8_lanes_for_test(Some(false));
    let mut cpu = flat_cpu();
    cpu.set_fast_map_enabled_for_test(false);
    let mut bus = test_bus(image(ROL, 4));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    let id = install(&mut cpu, ENTRY, BLOCK_INSNS);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        0,
        "the default arm registers no count lane"
    );
    assert_eq!(cpu.jit_direct.stall_snapshot().count_lane_registrations, 0);

    // The baked count still applies, so the off arm is a real lowering and not a refusal to
    // compile.
    arm(&mut cpu, 0x0000_00ff);
    let block = cpu.jit_direct.block(id).unwrap();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eax(), 0x0000_0ff0);

    guest_store_byte(&mut cpu, &mut bus, LANE, 8);
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
        "with no lane, the patch a lane would have absorbed retires the block"
    );
    jit::direct::set_imm8_lanes_for_test(None);
}

/// The admission bars, one fixture per bar, each a near-miss of the admitted shape. Every one of
/// these compiles a four-instruction block -- `lanes_registered_for` asserts that -- so a zero lane
/// count is a REFUSAL and not a truncated walk.
#[test]
fn near_miss_shapes_take_no_count_lane() {
    jit::direct::set_count_lanes_for_test(Some(true));
    // `0xD1 /0` ROL r32, 1: the SAME `RotateReg` kind, but its count is the literal 1 baked into
    // the opcode and it carries no immediate at all, so `physical + 2` would name the NEXT
    // instruction's first byte. TWO bars refuse it -- the opcode test and `imm_len == 1` -- and
    // this fixture cannot tell them apart; see `count_lane_for`, which says outright that the
    // opcode bar is redundant today and kept as defence in depth.
    assert_eq!(lanes_registered_for(&[0xd1, 0xc0], 4), 0, "0xD1 /0 ROL");
    // `0xD3 /4` SHL r32, CL: a `ShiftCl` kind, whose count is already runtime data out of guest CL
    // and which has no immediate byte to lane. Excluded by kind.
    assert_eq!(lanes_registered_for(&[0xd3, 0xe0], 4), 0, "0xD3 /4 SHL CL");
    // A segment override moves the count byte off offset 2. Refused rather than re-derived.
    assert_eq!(
        lanes_registered_for(&[0x26, 0xc1, 0xc0, 0x03], 4),
        0,
        "prefixed 0xC1"
    );
    // An operand-size override makes this a WORD shift, whose emitter has no CL-form lane at all.
    // The prefix bar and the `len == 3` bar both refuse it, which is what makes `shift_r16_cl` a
    // helper this tree does not owe.
    assert_eq!(
        lanes_registered_for(&[0x66, 0xc1, 0xe0, 0x03], 4),
        0,
        "0x66-prefixed 0xC1 shift"
    );
    // The MEMORY destination form of `0xC1` never reaches the lane matcher at all: classify's
    // group-2 arms take `DecodedOperand::Reg` only, so the walk breaks at the instruction and the
    // whole fixture span is rejected rather than compiled with a baked count. Pinned as the
    // OUTCOME rather than as a lane count, because a lane count would be a statement about a block
    // that does not exist.
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image_from(&[0xc1, 0x05, 0x00, 0x20, 0x00, 0x00, 0x03]));
    decode_at(&mut cpu, &mut bus, &block_starts(7));
    guest_store_dword(&mut cpu, &mut bus, 0x2000, 0);
    assert!(
        matches!(
            jit::direct::compile(&mut cpu, ENTRY, true),
            jit::direct::CompileOutcome::StructuralReject(_)
        ),
        "0xC1 with a memory destination must be refused before the lane matcher"
    );
    // `0x80 /r`, the other one-byte immediate family: `imm_len == 1` and `len == 3` like the
    // admitted shape, but its kind is `AluByteImm` and its lane belongs to the OTHER arm, which is
    // off here. A count lane that matched on encoding shape instead of on kind would take it.
    assert_eq!(
        lanes_registered_for(&[0x80, 0xc0, 0x11], 4),
        0,
        "0x80 belongs to the imm8 arm"
    );
}

/// The page-kind guard, stated positively and verbatim from the other two classes: a lane is
/// created ONLY from the fetch-page cache, the one direct-page cache that cannot hold a
/// device-aperture pointer. With the fetch entry gone but the data caches warm for the same page,
/// the qualifying `0xC1` must compile with a BAKED count -- correct as ever, just not
/// parameterized.
#[test]
fn a_page_the_fetch_cache_cannot_see_gets_no_count_lane() {
    jit::direct::set_count_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(ROL, 4));
    decode_at(&mut cpu, &mut bus, &block_starts(3));
    // Warm the data-write cache for the code page (well past the block's bytes), then drop the
    // fetch entry -- the state every code write already leaves behind.
    guest_store_dword(&mut cpu, &mut bus, ENTRY + 0x40, 0x1234_5678);
    cpu.fetch_page.invalidate();

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
        .expect("the baked-count block installs");
    arm(&mut cpu, 0x0000_00ff);
    let block = cpu.jit_direct.block(id).unwrap();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.registers.eax(),
        0x0000_0ff0,
        "the baked count still applies"
    );
}

/// Compile `middle` in the fixture frame under a forced arm and hand back the emitted code's
/// LENGTH.
///
/// The length rather than the bytes, and that is a limitation of the fixture rather than a
/// weakening of the claim: two compilations bake different host pointers (link-cell and portal
/// addresses, and a lane's own host pointer), so a byte comparison across them fails on allocation
/// noise whatever the arm. The length is exactly the quantity a stray lane moves -- the lane arm
/// emits a pointer load, a `movzx`, a mask and a three-way branch where the baked arm emits one
/// shift and one flag path.
fn emitted_len_under_arm(middle: &[u8], arm: bool) -> usize {
    jit::direct::set_count_lanes_for_test(Some(arm));
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image_from(middle));
    decode_at(&mut cpu, &mut bus, &block_starts(middle.len() as u32));
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    // Restored before the match, so a `panic!` below cannot leave this thread's override forced.
    // The override is per-thread and the harness reuses threads across tests.
    jit::direct::set_count_lanes_for_test(None);
    match outcome {
        jit::direct::CompileOutcome::Compiled(c) => c.code.len(),
        jit::direct::CompileOutcome::Retry => panic!("fixture block did not compile: Retry"),
        jit::direct::CompileOutcome::StructuralReject(r) => {
            panic!("fixture block did not compile: reject {r:?}")
        }
    }
}

/// THE DEFAULT PIN, TWO-SIDED, and the mutation record for the knob itself.
///
/// One direction: the admitted shapes must lower DIFFERENTLY under the two arms, or the knob is
/// decorative and a build that hard-wired the lane off would still pass every refusal fixture.
/// The other: every shape the arm does not admit must emit the SAME code under both arms, which is
/// what says the arm is a lane admission and not a second code path that an unrelated block also
/// walks through -- the failure a default flip would produce.
#[test]
fn the_off_arm_emits_the_same_code_for_everything_it_does_not_admit() {
    for admitted in [
        &[0xc1, 0xc0, 0x03][..],
        &[0xc1, 0xc8, 0x03][..],
        &[0xc1, 0xe0, 0x03][..],
        &[0xc0, 0xe0, 0x03][..],
    ] {
        assert_ne!(
            emitted_len_under_arm(admitted, false),
            emitted_len_under_arm(admitted, true),
            "{admitted:02x?} must lower differently under the two arms, or the knob does nothing"
        );
    }
    for middle in [
        // `0xD1 /0`, the same kind with no immediate.
        &[0xd1, 0xc0][..],
        // The CL-count form, whose count is runtime data already.
        &[0xd3, 0xe0][..],
        // The one-byte ALU immediate family, which has its own lane and its own knob.
        &[0x80, 0xc0, 0x11][..],
        // The dword immediate family, likewise.
        &[0x81, 0xc0, 0x78, 0x56, 0x34, 0x12][..],
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

/// The `IZARRAVM_COUNT_LANES` spelling table. `count_lanes_enabled` caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls -- hence the parse function is exercised directly.
///
/// The `unset -> false` assertion is the OTHER half of the default pin: flipping the default to on
/// fails here, and hard-wiring the arm off fails the emitted-code fixture above.
#[test]
fn count_lanes_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_count_lanes_arm_for_test;
    assert!(!parse(Err(VarError::NotPresent)), "unset is the base");
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must be the base");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must be the slice");
    }
}

/// A typo must not silently run the base. See `parse_count_lanes_arm` for why guessing is worse
/// than failing: a leg that quietly ran the base would be read as the slice doing nothing.
#[test]
#[should_panic(expected = "names no arm")]
fn an_unrecognised_count_lanes_spelling_panics() {
    jit::direct::parse_count_lanes_arm_for_test(Ok("true".to_string()));
}

/// The two knobs are INDEPENDENT LEVERS, pinned as a test because the obvious simplification --
/// hang the count lane off `IZARRAVM_IMM8_LANES`, they are both one-byte lanes -- would destroy the
/// 2x2 the ladder needs (see `rotate_rows_enabled`'s cross-term paragraph). Forcing one arm must
/// leave the other's reading alone, in both directions.
#[test]
fn the_count_arm_and_the_imm8_arm_are_separate_levers() {
    jit::direct::set_count_lanes_for_test(Some(true));
    jit::direct::set_imm8_lanes_for_test(Some(false));
    assert!(jit::direct::count_lanes_enabled());
    assert!(!jit::direct::imm8_lanes_enabled());
    jit::direct::set_count_lanes_for_test(Some(false));
    jit::direct::set_imm8_lanes_for_test(Some(true));
    assert!(!jit::direct::count_lanes_enabled());
    assert!(jit::direct::imm8_lanes_enabled());
    jit::direct::set_count_lanes_for_test(None);
    jit::direct::set_imm8_lanes_for_test(None);
}

/// The two classes COEXIST in one block and do not absorb each other's stores. Both arms on, a
/// `0x80` byte ALU and a `0xC1` rotate in the same span: two lanes, both one byte wide, and a
/// patch of either must be absorbed while the other keeps working.
///
/// This is the fixture for the shared-budget and shared-width-class plumbing. A count lane that
/// registered under the wrong class counter, or a write choke that matched an address without its
/// width, shows up here and nowhere else.
#[test]
fn a_count_lane_and_an_imm8_lane_coexist_in_one_block() {
    jit::direct::set_count_lanes_for_test(Some(true));
    jit::direct::set_imm8_lanes_for_test(Some(true));
    let mut cpu = flat_cpu();
    // `80 c0 11` ADD AL, 0x11 then `c1 c0 03` ROL EAX, 3, both in the frame.
    let mut bus = test_bus(image_from(&[0x80, 0xc0, 0x11, 0xc1, 0xc0, 0x03]));
    decode_at(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 2, ENTRY + 4, ENTRY + 7, ENTRY + 10],
    );
    let key = jit::direct::key_for(&cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("the block compiles");
    assert_eq!(compilation.span.instructions, 5);
    assert_eq!(compilation.imm_lane_count(), 2, "both lanes must register");
    assert_eq!(compilation.imm8_lane_count(), 1);
    assert_eq!(compilation.count_lane_count(), 1);
    let lanes = compilation.imm_lane_count() as u64;
    let id = cpu.jit_direct.install(&compilation).expect("it installs");
    cpu.perf.smc_lane_registrations += lanes;

    // Patch the ALU's immediate and the rotate's count, one each.
    guest_store_byte(&mut cpu, &mut bus, ENTRY + 4 + 2, 0x20);
    guest_store_byte(&mut cpu, &mut bus, ENTRY + 7 + 2, 8);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 2);
    assert_eq!(cpu.perf_counters().smc_lane_reject_width, 0);
    assert_eq!(cpu.perf_counters().smc_lane_reject_address, 0);

    let block = cpu
        .jit_direct
        .block(id)
        .expect("neither patch may retire the block");
    arm(&mut cpu, 0x0000_0001);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    // AL = 1 + 0x20 = 0x21, then ROL EAX, 8.
    assert_eq!(cpu.registers.eax(), 0x0000_2100);
    jit::direct::set_imm8_lanes_for_test(None);
}
