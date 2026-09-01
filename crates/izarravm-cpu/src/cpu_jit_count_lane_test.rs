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
//! **The arm is DEFAULT ON since the 2026-08-20 ladder** (-5.73% short, -4.94% long; see
//! `count_lanes_enabled`). Every fixture here states its arm through `force_count_lanes` /
//! `force_both_lane_arms` anyway, whose `ArmOverride` guard restores the ambient reading on the way
//! out even if the fixture panics. Stating it is not ceremony and the direction that matters
//! flipped with the default: it is now the REFUSAL fixtures that must force `Some(false)`, because
//! one that read the ambient arm would compile a lane and pass for the wrong reason.
//! `count_lane_is_refused_on_the_off_arm` is that fixture, and
//! `the_shipped_count_lanes_default_is_the_on_arm` is the one test here that deliberately reads the
//! ambient arm.
//!
//! **AND IT IS TWO ARMS, NOT ONE — `force_count_lanes` alone is not enough for a refusal
//! fixture.** `lanes_registered_for` reads `smc_lane_registrations`, which counts EVERY lane class,
//! so a shape this file calls a "near miss" for the count lane can still register an IMM8 lane and
//! read non-zero. That was harmless while `IZARRAVM_IMM8_LANES` was default-OFF and became a red
//! test the moment it flipped default-ON on 2026-08-21:
//! `near_miss_shapes_take_no_count_lane` now uses `force_both_lane_arms(true, false)`. Any fixture
//! here whose assertion is a lane COUNT rather than a count-lane-specific counter must state both
//! arms; `count_lane_registrations` from the stall snapshot is the class-specific alternative where
//! only the count lane is the subject.
//!
//! **THE BAKED EMITTERS KEEP THEIR OWN SWEEP, and the flip is why.** Before 2026-08-20 every
//! unforced group-2 fixture in the tree exercised `emit_rotate_reg` and `emit_shift`; on today's
//! default those same fixtures take a lane, so the baked path would have quietly lost its coverage
//! at the moment the default moved. `the_baked_arm_matches_the_interpreter_for_every_form_and_count`
//! is that coverage, forced off and held here rather than left to the ambient arm of some other
//! file.
//!
//! **THE FRAME CARRIES A LIVE LAZY-FLAGS DESCRIPTOR into the laned instruction**, and that is not
//! decoration. The three count shapes differ mostly in what they do to the DESCRIPTOR: count 0 must
//! leave it untouched, 2..31 must rewrite its CF override in place, and 1 must clear it after
//! publishing. A frame with no descriptor live collapses all three onto the same observable state
//! and every assertion below would pass with the branch deleted. So slot 1 of every fixture block
//! is `add ebx, ebx`, whose emitted form leaves a descriptor, and the group-2 instruction is slot
//! 2. `a_zero_count_preserves_the_live_descriptor` is the fixture that pins the frame itself.
//!
//! **AND THE FRAME CONSUMES THE FLAG SHADOW AFTERWARDS, at slot 3, which is a second thing the
//! first version of this file got wrong.** The 2..31 rotate arm's contract is that it captures CF
//! and FREEZES every other bit of RBP, because a rotate architecturally preserves SF, ZF, PF and
//! AF and the live descriptor still owns them. With nothing downstream reading the shadow, that
//! freeze is unobservable: an adversarial review widened the capture mask to `CF|ZF|SF` and the
//! whole suite stayed green, because RBP is never published wholesale at block exit.
//!
//! Slot 3 is therefore `ror edi, 1` — the `0xD1 /1` two-byte form, which takes NO lane (no
//! immediate byte) and is outside the `IZARRAVM_ROTATE_ROWS` gate, so it adds no dependency on
//! either arm. As a count-1 rotate it PUBLISHES RBP WHOLESALE to `eflags` with only `CF|OF`
//! redefined, so every other bit of the architectural result comes straight out of the shadow the
//! laned instruction left behind. A widened capture mask lands the lane preamble's own
//! `cmp ecx, 1` flags in RBP's SF and ZF, and they reach guest EFLAGS from here. That is what
//! turns the freeze into a measurement.
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
/// Instructions in a fixture block: the two leading frame slots, the group-2 slot, the shadow
/// consumer, and the trailing `mov edi, edi` before the HLT boundary.
const BLOCK_INSNS: u8 = 5;

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

/// Force both one-byte lane arms for the length of one fixture and restore the AMBIENT reading
/// when it ends -- normally or by panic.
///
/// A plain `set_*_for_test(Some(..))` at the top of a test LEAKS: the overrides are thread-local
/// and the harness reuses threads, so the next fixture on that thread inherits an arm it never
/// asked for. That was tolerable while the knob only selected lane admission. It is not tolerable
/// now: `count_lanes_enabled()` also selects which door `note_code_byte_write_hit` takes, so a
/// leaked ON override changes the SMC write path and the lane-rejection counters of every later
/// test on the thread -- including the fixtures whose whole point is that those counters stay at
/// zero on the shipped arm.
///
/// `Drop` rather than a trailing statement, because a `panic!` inside a fixture (an assertion
/// failure, which is the normal way these end when something is wrong) skips trailing statements
/// and would leak exactly when the state is least expected.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_count_lanes_for_test(None);
        jit::direct::set_imm8_lanes_for_test(None);
    }
}

/// Force the count arm and leave the imm8 arm ambient. Bind the result for the fixture's lifetime.
#[must_use]
fn force_count_lanes(on: bool) -> ArmOverride {
    jit::direct::set_count_lanes_for_test(Some(on));
    ArmOverride
}

/// Force BOTH one-byte arms, for the fixtures whose claim is about their interaction.
#[must_use]
fn force_both_lane_arms(count: bool, imm8: bool) -> ArmOverride {
    jit::direct::set_count_lanes_for_test(Some(count));
    jit::direct::set_imm8_lanes_for_test(Some(imm8));
    ArmOverride
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

/// `mov esi, esi` / `add ebx, ebx` / `<group-2>` / `ror edi, 1` / `mov edi, edi` / `hlt`. The HLT
/// is a hard boundary, so the block is exactly the five instructions before it.
fn image(shape: Shape, count: u8) -> Vec<u8> {
    image_from(&[shape.opcode, 0xc0 | (shape.op << 3) | shape.dst, count])
}

/// The same frame around an arbitrary middle instruction, so the refusal fixtures can put a
/// near-miss shape in the admitted slot's place.
fn image_from(middle: &[u8]) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    // `add ebx, ebx` is the descriptor seed and `ror edi, 1` (the `0xD1 /1` form, which takes no
    // lane and is outside the rotate-rows gate) is the shadow consumer; see the module comment for
    // why every fixture needs both around the laned instruction.
    let mut code = vec![0x89, 0xf6, 0x01, 0xdb];
    code.extend_from_slice(middle);
    code.extend_from_slice(&[0xd1, 0xcf, 0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// The frame WITHOUT the shadow consumer: `mov esi, esi` / `add ebx, ebx` / `<group-2>` /
/// `mov edi, edi` / `hlt`, four instructions.
///
/// **BOTH FRAMES ARE NEEDED AND EACH HIDES EXACTLY WHAT THE OTHER EXPOSES.** This is not
/// belt-and-braces; it was measured, one mutation at a time, and neither frame alone is sufficient:
///
/// * The CONSUMER frame publishes the shadow wholesale at slot 3, which is the only way the 2..31
///   arm's RBP freeze becomes observable at all. But that publish also OVERWRITES the descriptor
///   and re-defines CF and OF, so on this frame alone two real defects survive: deleting the
///   count-0 branch (the seed's CF happens to match what a zero-count rotate captures, and the
///   consumer overwrites the override anyway) and giving the count-1 arm the 2..31 flag path (the
///   consumer publishes and clears a moment later either way).
/// * The CONSUMER-FREE frame ends with whatever the group-2 slot left, so the un-republished
///   architectural EFLAGS are directly comparable — which catches those two. But nothing
///   reads RBP after the laned slot, so on this frame alone the RBP freeze is unobservable and a
///   widened capture mask survives.
///
/// So the interpreter sweeps run over both, and the mutation record in the commit message lists
/// which frame catches which defect.
fn image_without_consumer(shape: Shape, count: u8) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x01, 0xdb];
    code.extend_from_slice(&[shape.opcode, 0xc0 | (shape.op << 3) | shape.dst, count]);
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// Instruction starts for the consumer-free frame, whose group-2 slot is always three bytes.
fn block_starts_without_consumer() -> [u32; 4] {
    [
        ENTRY,
        ENTRY + SEED_OFFSET,
        ENTRY + OP_OFFSET,
        ENTRY + OP_OFFSET + 3,
    ]
}

/// Which of the two frames a sweep round is running on. See `image_without_consumer` for why every
/// interpreter comparison runs on both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Frame {
    /// `.. / <group-2> / ror edi, 1 / mov edi, edi / hlt` — the shadow is published downstream.
    WithConsumer,
    /// `.. / <group-2> / mov edi, edi / hlt` — the descriptor the slot left survives to block exit.
    WithoutConsumer,
}

impl Frame {
    fn image(self, shape: Shape, count: u8) -> Vec<u8> {
        match self {
            Frame::WithConsumer => image(shape, count),
            Frame::WithoutConsumer => image_without_consumer(shape, count),
        }
    }

    /// Decode starts, for a group-2 slot that is always three bytes long.
    fn starts(self) -> Vec<u32> {
        match self {
            Frame::WithConsumer => block_starts(3).to_vec(),
            Frame::WithoutConsumer => block_starts_without_consumer().to_vec(),
        }
    }

    fn instructions(self) -> u8 {
        match self {
            Frame::WithConsumer => BLOCK_INSNS,
            Frame::WithoutConsumer => BLOCK_INSNS - 1,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Frame::WithConsumer => "consumer frame",
            Frame::WithoutConsumer => "consumer-free frame",
        }
    }
}

const FRAMES: [Frame; 2] = [Frame::WithConsumer, Frame::WithoutConsumer];

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
fn block_starts(middle_len: u32) -> [u32; 5] {
    [
        ENTRY,
        ENTRY + SEED_OFFSET,
        ENTRY + OP_OFFSET,
        ENTRY + OP_OFFSET + middle_len,
        ENTRY + OP_OFFSET + middle_len + 2,
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
fn lane_fixture(shape: Shape, count: u8) -> (CpuGsw, TestBus, jit::direct::BlockId, ArmOverride) {
    let arm_override = force_count_lanes(true);
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
    (cpu, bus, id, arm_override)
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
        jit::direct::CompileOutcome::Retry(_) => {
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
#[allow(clippy::too_many_arguments)]
fn assert_agrees(
    native: &mut CpuGsw,
    native_bus: &mut TestBus,
    interpreter: &mut CpuGsw,
    interpreter_bus: &mut TestBus,
    id: jit::direct::BlockId,
    eax: u32,
    frame: Frame,
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
    for _ in 0..frame.instructions() {
        interpreter.cycle(interpreter_bus).unwrap();
    }
    assert_eq!(
        crate::tests::settled_registers(native),
        crate::tests::settled_registers(interpreter),
        "{label}: registers differ"
    );
    assert_eq!(
        native.eflags(),
        interpreter.eflags(),
        "{label}: EFLAGS differ"
    );
    // The raw `pending_flags` word is deliberately NOT compared: it is a REPRESENTATION of the
    // flags, and two roles at the same architectural state are free to carry different
    // (base, descriptor) pairs for it. What still fails is a count-2 rotate that gets the
    // architectural answer wrong -- `eflags()` above is what turns a descriptor into flags, and
    // the CONSUMER-BEARING frame reads the flags back through a real instruction.
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
    let _arm = force_count_lanes(true);
    for frame in FRAMES {
        for shape in SHAPES {
            for (round, &count) in COUNTS.iter().enumerate() {
                // Compiled at a count that is NOT the one under test, so a lane that quietly
                // re-used its compile-time immediate fails instead of agreeing.
                let mut native = flat_cpu();
                let mut native_bus = test_bus(frame.image(shape, 0x11));
                decode_at(&mut native, &mut native_bus, &frame.starts());
                let id = install(&mut native, ENTRY, frame.instructions());
                assert_eq!(
                    native.perf_counters().smc_lane_registrations,
                    1,
                    "{} on the {}: no lane, so this round would be vacuous",
                    shape.label(),
                    frame.label()
                );

                let mut interpreter = flat_cpu();
                let mut interpreter_bus = test_bus(frame.image(shape, 0x11));
                decode_at(&mut interpreter, &mut interpreter_bus, &frame.starts());

                guest_store_byte(&mut native, &mut native_bus, LANE, count);
                guest_store_byte(&mut interpreter, &mut interpreter_bus, LANE, count);

                let label = format!(
                    "{} count {count:#04x} on the {}",
                    shape.label(),
                    frame.label()
                );
                assert_agrees(
                    &mut native,
                    &mut native_bus,
                    &mut interpreter,
                    &mut interpreter_bus,
                    id,
                    0x1234_5678u32.wrapping_mul(round as u32 + 1) | 1,
                    frame,
                    &label,
                );
            }
        }
    }
}

/// THE BAKED EMITTERS, swept exactly as the lane emitters are, with the arm forced OFF.
///
/// **This test exists because of the 2026-08-20 default flip and would have been redundant before
/// it.** While the arm was default-off, every unforced group-2 fixture in the tree -- the
/// rotate-rows admission fixtures, the lowering sweeps, the timing cases -- ran `emit_rotate_reg`
/// and `emit_shift`. On today's default those same fixtures attach a lane and run
/// `emit_rotate_reg_lane` / `emit_shift_lane` instead. Nothing failed when the default moved,
/// because the two paths agree; what changed silently is WHICH path the tree covers. A future edit
/// to the baked compile-time three-way split would then be caught by nothing.
///
/// So the baked path gets its own sweep, over the same forms, the same counts and both frames, and
/// the count is BAKED into the image rather than patched in -- on this arm a patch retires the
/// block instead of being absorbed, which is the whole point of the arm.
#[test]
fn the_baked_arm_matches_the_interpreter_for_every_form_and_count() {
    let _arm = force_count_lanes(false);
    for frame in FRAMES {
        for shape in SHAPES {
            for (round, &count) in COUNTS.iter().enumerate() {
                let mut native = flat_cpu();
                let mut native_bus = test_bus(frame.image(shape, count));
                decode_at(&mut native, &mut native_bus, &frame.starts());
                let id = install(&mut native, ENTRY, frame.instructions());
                assert_eq!(
                    native.perf_counters().smc_lane_registrations,
                    0,
                    "{} on the {}: the off arm must bake the count, or this sweep is a second copy of the lane sweep",
                    shape.label(),
                    frame.label()
                );

                let mut interpreter = flat_cpu();
                let mut interpreter_bus = test_bus(frame.image(shape, count));
                decode_at(&mut interpreter, &mut interpreter_bus, &frame.starts());

                assert_agrees(
                    &mut native,
                    &mut native_bus,
                    &mut interpreter,
                    &mut interpreter_bus,
                    id,
                    0x1234_5678u32.wrapping_mul(round as u32 + 1) | 1,
                    frame,
                    &format!(
                        "BAKED {} count {count:#04x} on the {}",
                        shape.label(),
                        frame.label()
                    ),
                );
            }
        }
    }
}

/// THE COUNT-0 CONTRACT, ON THE CONSUMER-FREE FRAME: a masked count of zero moves no flag, creates
/// no descriptor and destroys none, and the block ends carrying exactly the descriptor the seed ALU
/// left.
///
/// **This fixture runs on `image_without_consumer` and that is load-bearing.** On the main frame
/// slot 3 publishes the shadow and clears the descriptor, which overwrites the evidence: with only
/// that frame, deleting the count-0 branch outright leaves the entire suite green, because a
/// zero-count rotate captures the `cmp` flags' CF, which for this seed matches the descriptor's,
/// and the consumer then overwrites the override anyway. Comparing the architectural `eflags()`
/// at the end of a block that has nothing after the group-2 slot is what makes the count-0 arm's
/// deletion visible: with no consumer to republish, the un-materialised difference reaches the
/// comparison.
///
/// The liveness half is asserted from the interpreter two instructions in, which is where the
/// claim is actually made: a descriptor is live ENTERING the group-2 slot. Without that, "leave it
/// untouched" and "there was nothing to touch" are the same assertion.
#[test]
fn a_zero_count_preserves_the_live_descriptor() {
    let _arm = force_count_lanes(true);
    for count in [0u8, 0x20] {
        let mut native = flat_cpu();
        let mut native_bus = test_bus(image_without_consumer(ROL, 3));
        decode_at(
            &mut native,
            &mut native_bus,
            &block_starts_without_consumer(),
        );
        let id = install(&mut native, ENTRY, 4);
        assert_eq!(
            native.perf_counters().smc_lane_registrations,
            1,
            "count {count:#04x}: no lane, so this round would be vacuous"
        );

        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus(image_without_consumer(ROL, 3));
        decode_at(
            &mut interpreter,
            &mut interpreter_bus,
            &block_starts_without_consumer(),
        );

        guest_store_byte(&mut native, &mut native_bus, LANE, count);
        guest_store_byte(&mut interpreter, &mut interpreter_bus, LANE, count);

        arm(&mut native, 0x8000_0001);
        arm(&mut interpreter, 0x8000_0001);
        let before = native.registers.eax();

        let block = native.jit_direct.block(id).expect("the block survives");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        // Two instructions in is the group-2 slot's entry: the descriptor must be live THERE, or
        // "preserved" is a statement about nothing.
        interpreter.cycle(&mut interpreter_bus).unwrap();
        interpreter.cycle(&mut interpreter_bus).unwrap();
        assert_ne!(
            interpreter.pending_flags,
            PendingFlags::default(),
            "count {count:#04x}: the seed ALU must leave a live descriptor entering the group-2 \
             slot, or the count-0 contract is unobservable"
        );
        let entering = interpreter.pending_flags;
        for _ in 0..2 {
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }

        assert_eq!(
            interpreter.pending_flags, entering,
            "count {count:#04x}: the INTERPRETER must carry the seed's descriptor through a \
             zero-count rotate untouched; if it does not, the oracle moved and not the emitter"
        );
        assert_eq!(native.eflags(), interpreter.eflags(), "count {count:#04x}");
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interpreter),
            "count {count:#04x}"
        );
        assert_eq!(
            native.registers.eax(),
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
    let (mut cpu, mut bus, id, _arm) = lane_fixture(ROL, 3);
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
    let _arm = force_count_lanes(true);
    for frame in FRAMES {
        for shape in SHAPES {
            let mut native = flat_cpu();
            let mut native_bus = test_bus(frame.image(shape, 3));
            decode_at(&mut native, &mut native_bus, &frame.starts());
            let id = install(&mut native, ENTRY, frame.instructions());
            assert_eq!(native.perf_counters().smc_lane_registrations, 1);

            let mut interpreter = flat_cpu();
            let mut interpreter_bus = test_bus(frame.image(shape, 3));
            decode_at(&mut interpreter, &mut interpreter_bus, &frame.starts());

            let before_kills = native.perf_counters().smc_narrow_kills;
            // Every value distinct from the last: G2 same-value elision never reaches the choke,
            // so an identical repatch would not count as an accept.
            let patches = [1u8, 0, 2, 1, 31, 0, 0x21, 0x20, 7];
            for (round, &count) in patches.iter().enumerate() {
                guest_store_byte(&mut native, &mut native_bus, LANE, count);
                guest_store_byte(&mut interpreter, &mut interpreter_bus, LANE, count);
                assert_eq!(
                    native.perf_counters().smc_lane_accepts,
                    round as u64 + 1,
                    "{} on the {}: patch {count:#04x} must be absorbed as a lane accept",
                    shape.label(),
                    frame.label()
                );
                assert_agrees(
                    &mut native,
                    &mut native_bus,
                    &mut interpreter,
                    &mut interpreter_bus,
                    id,
                    0x89ab_cdefu32.wrapping_mul(round as u32 + 1) | 1,
                    frame,
                    &format!(
                        "{} patch {count:#04x} on the {}",
                        shape.label(),
                        frame.label()
                    ),
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
}

/// The SLOW byte door, arm-selected at `note_code_byte_write_hit` (core.rs). That test is an OR
/// over the two one-byte arms since this slice, and this is the fixture for the half this slice
/// added: with FastMap off and `IZARRAVM_IMM8_LANES` explicitly OFF, only the count arm can open
/// the value-aware door. Deleting the `count_lanes_enabled()` half makes this fail on the accept
/// counter and on block liveness together.
#[test]
fn the_count_arm_alone_opens_the_slow_byte_door() {
    let _arm = force_both_lane_arms(true, false);
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
    let (mut cpu, mut bus, id, _arm) = lane_fixture(ROL, 0xee);
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
    let (mut cpu, mut bus, id, _arm) = lane_fixture(ROL, 3);
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
    let (mut cpu, mut bus, id, _arm) = lane_fixture(ROL, 3);
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
        let (mut cpu, mut bus, id, _arm) = lane_fixture(ROL, 3);
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
    let (mut cpu, mut bus, id, _arm) = lane_fixture(ROL, 3);
    cpu.note_device_memory_write_range(LANE, 1);

    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a device write must take the normal invalidation path"
    );
    let _ = &mut bus;
}

/// THE OFF ARM, which since the 2026-08-20 default flip must be FORCED rather than inherited. On
/// this arm the very same block registers no lane, the baked count still applies, and the very same
/// patch retires the block -- which is what makes every fixture above a statement about the slice
/// rather than about the backend it was already, and what keeps the escape a true pre-slice world
/// for the next A/B to be read against.
///
/// The two rejection counters are pinned at zero on this arm for
/// `the_off_arm_moves_no_rejection_counter_on_a_byte_write`'s reason (cpu_jit_imm8_lane_test): with
/// both one-byte arms off, `note_code_byte_write_hit` must keep the value-less door it always had,
/// counters included. FastMap is off here because that is the path the door selects on.
#[test]
fn count_lane_is_refused_on_the_off_arm() {
    let _arm = force_both_lane_arms(false, false);
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
}

/// The admission bars, one fixture per bar, each a near-miss of the admitted shape. Every one of
/// these compiles a four-instruction block -- `lanes_registered_for` asserts that -- so a zero lane
/// count is a REFUSAL and not a truncated walk.
#[test]
fn near_miss_shapes_take_no_count_lane() {
    // BOTH arms are stated, not one. `lanes_registered_for` counts `smc_lane_registrations`, which
    // is EVERY lane class, so a `0x80`-shaped near miss registers an IMM8 lane and this fixture
    // reads non-zero the moment `IZARRAVM_IMM8_LANES` is on ambiently. It was written when that
    // arm was default-OFF and could never happen; it is default-ON as of the 2026-08-21 flip.
    // See the note at the top of this file.
    let _arm = force_both_lane_arms(true, false);
    // `0xD1 /0` ROL r32, 1: the SAME `RotateReg` kind, but its count is the literal 1 baked into
    // the opcode and it carries no immediate at all, so `physical + 2` would name the NEXT
    // instruction's first byte. TWO bars refuse it -- the opcode test and `imm_len == 1` -- and
    // this fixture cannot tell them apart; see `count_lane_for`, which says outright that the
    // opcode bar is redundant today and kept as defence in depth.
    assert_eq!(
        lanes_registered_for(&[0xd1, 0xc0], BLOCK_INSNS),
        0,
        "0xD1 /0 ROL"
    );
    // `0xD3 /4` SHL r32, CL: a `ShiftCl` kind, whose count is already runtime data out of guest CL
    // and which has no immediate byte to lane. Excluded by kind.
    assert_eq!(
        lanes_registered_for(&[0xd3, 0xe0], BLOCK_INSNS),
        0,
        "0xD3 /4 SHL CL"
    );
    // A segment override moves the count byte off offset 2. Refused rather than re-derived.
    assert_eq!(
        lanes_registered_for(&[0x26, 0xc1, 0xc0, 0x03], BLOCK_INSNS),
        0,
        "prefixed 0xC1"
    );
    // An operand-size override makes this a WORD shift, whose emitter has no CL-form lane at all.
    // THREE bars refuse it here -- the prefix bar, `len == 3`, and the kind's own width -- and only
    // the last of those is load-bearing: in a 16-bit code segment the same instruction needs no
    // prefix at all and satisfies the first two. See
    // `a_word_group_two_shift_in_a_sixteen_bit_segment_takes_no_count_lane` in
    // cpu_jit_sixteen_bit_test, which is the fixture for the case this one cannot reach.
    assert_eq!(
        lanes_registered_for(&[0x66, 0xc1, 0xe0, 0x03], BLOCK_INSNS),
        0,
        "0x66-prefixed 0xC1 shift"
    );
    // The MEMORY destination form of `0xC1` takes NO lane. It used to be pinned as a compile
    // OUTCOME, because classify's group-2 arms bound `DecodedOperand::Reg` only and the block did
    // not exist to count lanes in. `vorvek/direct-rot-mem-lane` gives it
    // `DirectKind::RotateShiftMem`, so the block compiles now and the claim is pinned where it
    // belongs: `count_lane_for` matches `RotateReg` and `Shift` BY NAME and ends in a catch-all
    // `return None`, so the new kind cannot reach a lane whatever its encoding looks like. This is
    // the stronger assertion of the two -- the old one would have gone green again for the wrong
    // reason the moment the memory form was admitted.
    assert_eq!(
        lanes_registered_for_warm(&[0xc1, 0x05, 0x00, 0x20, 0x00, 0x00, 0x03], 5, &[0x2000]),
        0,
        "0xC1 with a memory destination takes no count lane"
    );
    // `0x80 /r`, the other one-byte immediate family: `imm_len == 1` and `len == 3` like the
    // admitted shape, but its kind is `AluByteImm` and its lane belongs to the OTHER arm, which is
    // off here. A count lane that matched on encoding shape instead of on kind would take it.
    assert_eq!(
        lanes_registered_for(&[0x80, 0xc0, 0x11], BLOCK_INSNS),
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
    let _arm = force_count_lanes(true);
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
    // Restored before the match rather than through `ArmOverride`, because this helper is called
    // TWICE per comparison and must not hold an override across its own second call.
    jit::direct::set_count_lanes_for_test(None);
    match outcome {
        jit::direct::CompileOutcome::Compiled(c) => c.code.len(),
        jit::direct::CompileOutcome::Retry(_) => panic!("fixture block did not compile: Retry"),
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

/// The `IZARRAVM_COUNT_LANES` spelling table, and THE DEFAULT PIN, TWO-SIDED.
///
/// `count_lanes_enabled` caches its env reading in a process-wide `OnceLock`, so the contract is
/// otherwise assertable exactly once per process and never in an order the harness controls --
/// hence the parse function is exercised directly.
///
/// **Both directions are pinned here and each fails a different mutation**, which is what makes
/// this a pin rather than a restatement of the code:
///
/// * `unset -> ON` is the 2026-08-20 flip. Restoring `unset -> false` (the pre-flip mapping) fails
///   the first assertion. Without it, a default that silently reverted would leave the whole class
///   dormant in the shipped binary while every forced fixture in this file kept passing.
/// * `0` / `off` / empty `-> OFF` is the escape. Making the off spellings select ON -- the obvious
///   "simplify the table now that on is the default" edit -- fails the second loop. That escape is
///   the base every future A/B on this class is read against, so it has to keep reproducing the
///   pre-slice world or the base stops being one.
///
/// `1` / `on` stays pinned to ON rather than merely accepted, because that is the spelling every
/// leg in `.bench/results/duke-l2-count-lane-20260820/` used.
#[test]
fn count_lanes_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_count_lanes_arm_for_test;
    assert!(
        parse(Err(VarError::NotPresent)),
        "unset must select ON -- the shipped default since the 2026-08-20 ladder"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(
            !parse(Ok(off.to_string())),
            "{off:?} must select the pre-slice world; it is the escape and the A/B base"
        );
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(
            parse(Ok(on.to_string())),
            "{on:?} must select the lane class"
        );
    }
}

/// THE SHIPPED DEFAULT, asserted through the live reader rather than the parse table.
///
/// Everything else in this file runs on the thread-local override, so without this nothing would
/// notice `count_lanes_enabled` growing a different default from the one `parse_count_lanes_arm`
/// spells -- a `#[cfg]`, a stray `OnceLock` seed, an override that failed to clear. It also makes
/// the NEXT flip a deliberate edit rather than a side effect, which is exactly the job this test
/// did for `IZARRAVM_ROTATE_ROWS`.
///
/// Reads the AMBIENT arm, with the override explicitly cleared first. That is the one place in
/// this file that depends on the process environment: a developer running the suite with
/// `IZARRAVM_COUNT_LANES=0` exported is asking for the escape and will see this fail, which is the
/// correct and legible outcome for a deliberate default pin.
#[test]
fn the_shipped_count_lanes_default_is_the_on_arm() {
    jit::direct::set_count_lanes_for_test(None);
    assert!(
        jit::direct::count_lanes_enabled(),
        "the shipped default must be ON since the 2026-08-20 ladder (-5.73% short, -4.94% long); \
         see count_lanes_enabled for the evidence"
    );
}

/// A typo must not silently run the default. See `parse_count_lanes_arm` for why guessing is worse
/// than failing -- and why the argument is STRONGER since the flip: a leg that quietly fell through
/// would run exactly what an unset environment runs, and be read as the arm it named doing nothing.
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
    let _arm = force_both_lane_arms(true, true);
    let mut cpu = flat_cpu();
    // `80 c0 11` ADD AL, 0x11 then `c1 c0 03` ROL EAX, 3, both in the frame.
    let mut bus = test_bus(image_from(&[0x80, 0xc0, 0x11, 0xc1, 0xc0, 0x03]));
    decode_at(
        &mut cpu,
        &mut bus,
        &[
            ENTRY,
            ENTRY + 2,
            ENTRY + 4,
            ENTRY + 7,
            ENTRY + 10,
            ENTRY + 12,
        ],
    );
    let key = jit::direct::key_for(&cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("the block compiles");
    assert_eq!(
        compilation.span.instructions,
        BLOCK_INSNS + 1,
        "the frame plus BOTH laned slots; a truncated walk would make the lane counts vacuous"
    );
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
}
