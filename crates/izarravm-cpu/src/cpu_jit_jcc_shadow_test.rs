// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_JCC_SHADOW`: the Jcc terminator computed directly out of the RBP EFLAGS shadow
//! instead of through a `push`/`popfq` round trip into the host flags word.
//!
//! # WHAT THESE FIXTURES HAVE TO DO THAT A PLAIN DIFFERENTIAL DOES NOT
//!
//! This slice's failure mode is a GRACEFUL AGREEMENT, not a crash. A shadow predicate that is
//! wrong in a way that happens to coincide with the host-flags answer on the inputs a fixture
//! chose passes every differential while the mechanism is broken. So every row here does three
//! independent things:
//!
//! 1. **Proves the arm ENGAGED.** `Compilation::jcc_shadow_sites()` is an exact oracle: zero on
//!    the OFF arm by construction, and exactly one site in the expected CLASS on the ON arm. A
//!    row that silently ran the base arm cannot pass.
//! 2. **Checks against an INDEPENDENT oracle**, `predicate` below, written from the Intel
//!    condition table rather than from the emitter — so a shared misunderstanding cannot make
//!    both sides of a differential agree.
//! 3. **Checks against the interpreter**, which carries the rest of the architectural state
//!    (registers, `pending_flags`, EFLAGS, core clocks) that the branch outcome alone would miss.
//!
//! On top of that, `assert_discriminating` refuses any condition whose seed set did not produce
//! BOTH a taken and a not-taken outcome. A sweep that only ever falls through proves nothing, and
//! that is exactly the shape a mis-set seed table degenerates into.
//!
//! # WHY THE LOAD-BEARING SHAPE IS PRODUCER-FREE
//!
//! `Shape::Terminal` puts the Jcc at the block's FIRST and ONLY slot, so the seeded EFLAGS reaches
//! RBP unmodified through `materialized_eflags()` and the prologue's shadow load. That is what
//! makes a seed set mean anything. A producer between the seed and the terminator overwrites every
//! flag its own `defined` mask covers -- a `TEST` writes CF = 0 and OF = 0 through `LOGIC_FLAGS`,
//! which would silently collapse JO/JNO and JB/JAE to constants and reduce the signed four to half
//! their input space. The producer-bearing shapes below exist to pin DIFFERENT properties and say
//! so in their own comments.

use super::*;

const ENTRY: u32 = 0x501;
const STACK_TOP: u32 = 0x4000;
/// `jcc rel8` with this displacement, so the taken and fallthrough targets are distinct and both
/// land on a HLT.
const TAKEN_DISP: u8 = 2;

/// The four `JccShadowClass` lanes in order, and the ON-minus-OFF byte cost of each.
///
/// The OFF arm is `emit_load_host_flags` (`mov`/`and`/`push`/`popfq`, ten bytes) plus a six-byte
/// `jcc`: sixteen at every condition. The ON arm is a four-byte `test bpl,imm8` (ten total), a
/// six-byte `test ebp,imm32` (twelve), the ten-byte XOR7 chain (sixteen), or the fifteen-byte XOR6
/// chain (twenty-one).
const CLASS_BYTE_DELTA: [i64; 4] = [-6, -4, 0, 5];
const CLASS_NAMES: [&str; 4] = ["simple", "overflow", "signed_xor", "signed_xor_zf"];

/// Which emission class a condition takes. Written out here rather than imported so a mutation of
/// the emitter's own dispatch cannot move the fixture's expectation with it.
fn expected_class(condition: u8) -> usize {
    match condition >> 1 {
        0 => 1,
        6 => 2,
        7 => 3,
        _ => 0,
    }
}

/// The INDEPENDENT oracle: the guest's own condition table, from the flag bits alone.
///
/// Deliberately not shaped like the emitter. The emitter folds `SF^OF` through a shift chain; this
/// spells it `sf != of`. Both must agree on every input, which is the whole content of the slice.
fn predicate(condition: u8, eflags: u32) -> bool {
    let cf = eflags & crate::FLAG_CF != 0;
    let pf = eflags & crate::FLAG_PF != 0;
    let zf = eflags & crate::FLAG_ZF != 0;
    let sf = eflags & crate::FLAG_SF != 0;
    let of = eflags & crate::FLAG_OF != 0;
    let held = match condition >> 1 {
        0 => of,
        1 => cf,
        2 => zf,
        3 => cf || zf,
        4 => sf,
        5 => pf,
        6 => sf != of,
        _ => zf || (sf != of),
    };
    if condition & 1 == 0 { held } else { !held }
}

/// The eight (SF, OF, ZF) points, every one of them reachable at a block ENTRY even though a
/// producer can only deliver six (ZF = 1 forces SF = 0 for any ADD or SUB, so (SF=1, ZF=1) has no
/// producer -- but POPF, IRET and an interpreted partial write all reach it, and the entry shadow
/// is `materialized_eflags()` verbatim).
///
/// `(SF=0, OF=1)` is load-bearing and must never be pruned as redundant: it is the ONLY point that
/// separates a correct XOR7 from one whose `shr` count is off by one, because on `(SF=1, OF=0)`
/// the misaligned read of shadow bit 12 (IOPL bit 0, clear in every seed here) happens to agree.
const SIGNED_POINTS: [u32; 8] = [
    0x002, // SF=0 OF=0 ZF=0
    0x042, // SF=0 OF=0 ZF=1
    0x082, // SF=1 OF=0 ZF=0
    0x0c2, // SF=1 OF=0 ZF=1  -- producer-unreachable, and M9's killer
    0x802, // SF=0 OF=1 ZF=0  -- M5's only killer
    0x842, // SF=0 OF=1 ZF=1
    0x882, // SF=1 OF=1 ZF=0
    0x8c2, // SF=1 OF=1 ZF=1  -- producer-unreachable
];

/// The full seed set for the sixteen-condition sweep: the eight signed points above, plus single-
/// flag points that separate CF, PF and ZF from each other and from SF, plus the seven seeds
/// `setcc_register_form_matches_the_interpreter_for_every_condition` already justifies flag by
/// flag. Every seed carries bit 1, which real EFLAGS always reads as set -- and which is exactly
/// why no predicate mask may ever carry `0x2`: it would make its condition unconditionally taken.
const SEEDS: [u32; 20] = [
    0x002, 0x042, 0x082, 0x0c2, 0x802, 0x842, 0x882, 0x8c2, // the eight signed points
    0x003, // CF alone
    0x006, // PF alone
    0x043, // CF and ZF together
    0x041, // ZF with CF clear -- JBE's ZF term, with bit 1 deliberately absent
    0x012, // AF alone: no condition may read it
    0x202, 0x206, 0x246, 0x282, 0x8d7, 0xa02, 0xed7, // the SETcc seed set
];

/// What sits in front of the Jcc in the block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// Nothing: the Jcc is the block's first and only slot. The seeded EFLAGS reaches RBP intact.
    Terminal,
    /// `and eax, eax` (0x21 0xC0). Lowers through `emit_capture_flags(e, LOGIC_FLAGS)`, which
    /// writes CF/OF/ZF/SF/PF and leaves AF STALE -- the shape the AF fixture needs.
    AndProducer,
    /// `add eax, ebx` (0x01 0xD8). A full `ARITH_FLAGS` producer, for the six (SF, OF, ZF) points
    /// a real arithmetic result can reach.
    AddProducer,
    /// `fwait` (0x9B). Makes the block x87-BEARING, so the emitter runs `emit_x87_enter` and, on
    /// Windows, spills RSI into the frame as the tag-cache scratch. Touches no flag, so the
    /// seeded shadow still reaches the terminator intact and the independent oracle still applies.
    X87Wait,
    /// `in al, dx` (0xEC). A PORT call-out slot, so the terminator runs after the call-out RESUME
    /// path has republished the guest flags and reloaded RBP from them. `IN` defines no flag, so
    /// the seeded shadow must survive the whole round trip and the independent oracle applies.
    PortCallOut,
}

impl Shape {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Terminal => &[],
            Self::AndProducer => &[0x21, 0xc0],
            Self::AddProducer => &[0x01, 0xd8],
            Self::X87Wait => &[0x9b],
            Self::PortCallOut => &[0xec],
        }
    }

    fn instructions(self) -> u8 {
        match self {
            Self::Terminal => 1,
            Self::AndProducer | Self::AddProducer | Self::X87Wait | Self::PortCallOut => 2,
        }
    }

    /// Whether the seeded entry EFLAGS still describes the shadow at the terminator, i.e. whether
    /// the INDEPENDENT oracle in `predicate` applies. False for the two producer shapes, which
    /// overwrite the flags their own `defined` mask covers and are checked against hand-written
    /// per-condition expectations instead.
    fn preserves_entry_flags(self) -> bool {
        matches!(self, Self::Terminal | Self::X87Wait | Self::PortCallOut)
    }
}

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

fn image(shape: Shape, condition: u8) -> (Vec<u8>, u32, u32) {
    let mut code = shape.bytes().to_vec();
    code.extend_from_slice(&[0x70 | condition, TAKEN_DISP]);
    let fallthrough = ENTRY + code.len() as u32;
    let taken = fallthrough + u32::from(TAKEN_DISP);
    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // Both continuations, and the bytes between them, are HLT so a run that overshoots stops
    // instead of wandering into zeros.
    for byte in memory
        .iter_mut()
        .skip(fallthrough as usize)
        .take(TAKEN_DISP as usize + 2)
    {
        *byte = 0xf4;
    }
    (memory, fallthrough, taken)
}

struct Prepared {
    native: CpuGsw,
    interpreter: CpuGsw,
    native_bus: TestBus,
    interpreter_bus: TestBus,
    block: jit::direct::CompiledBlock,
    sites: [u16; 4],
    code_len: usize,
    code: Vec<u8>,
    fallthrough: u32,
    taken: u32,
}

/// Compile and install one `shape`-then-Jcc block on the CURRENT arm, with the interpreter twin
/// beside it.
///
/// The arm is whatever `set_jcc_shadow_for_test` last named on this thread; every caller sets it
/// explicitly, because a fixture that inherited the ambient knob would silently test one arm twice
/// on an ON-arm suite run.
fn prepare(shape: Shape, condition: u8) -> Prepared {
    let (memory, fallthrough, taken) = image(shape, condition);
    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        if shape == Shape::PortCallOut {
            // A LAZY port read: the served-without-step-break answer, which is the only one that
            // lets the block continue past the call-out into its own terminator. Without it the
            // call-out reports a step break, the block side-exits with EIP on the `IN`, and the
            // Jcc this fixture exists to run never executes at all.
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        }
    }
    let jcc_at = ENTRY + shape.bytes().len() as u32;
    let starts = [ENTRY, jcc_at, fallthrough, taken];
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
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
            panic!("{shape:?}/{condition:#x}: structurally rejected")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("{shape:?}/{condition:#x}: retry"),
    };
    assert_eq!(
        compilation.span.instructions,
        shape.instructions(),
        "{shape:?}/{condition:#x}: the block must end AT the Jcc, covering exactly its own slots"
    );
    assert_eq!(
        compilation.callout_slots,
        u8::from(shape == Shape::PortCallOut),
        "{shape:?}/{condition:#x}: the call-out shape must really carry a call-out slot"
    );
    assert_eq!(
        compilation.has_x87,
        shape == Shape::X87Wait,
        "{shape:?}/{condition:#x}: the x87 shape must really be x87-bearing, or 6.10 tests a \
         plain two-slot block wearing an x87 label"
    );
    let sites = compilation.jcc_shadow_sites();
    let code_len = compilation.code.len();
    let code = compilation.code.clone();
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    // The install-site mirror of `run.rs`, which is where the shipped fold lives. Without it a
    // fixture would read zero sites for a shadow-lowered block.
    if sites != [0; 4] {
        native.jit_direct.direct.note_jcc_shadow_sites(sites);
    }
    let block = native.jit_direct.block(id).expect("live block");
    Prepared {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
        sites,
        code_len,
        code,
        fallthrough,
        taken,
    }
}

/// Arm both roles identically and assert the ARM, then run one native block entry against
/// `shape.instructions()` interpreter cycles and compare everything.
///
/// Returns whether the branch was taken, so the caller can prove its seed set discriminates.
#[allow(clippy::too_many_arguments)]
fn run_row(
    prepared: &mut Prepared,
    shape: Shape,
    condition: u8,
    on: bool,
    eax: u32,
    ebx: u32,
    seed: u32,
    pending: Option<(u32, u32)>,
    context: &str,
) -> bool {
    let class = expected_class(condition);
    let mut expected_sites = [0u16; 4];
    if on {
        expected_sites[class] = 1;
    }
    assert_eq!(
        prepared.sites, expected_sites,
        "{context}: the shadow arm did not engage the way the knob said it would -- a row that \
         silently ran the base arm proves nothing about the shadow lowering"
    );

    let mut effective = 0;
    for (index, cpu) in [&mut prepared.native, &mut prepared.interpreter]
        .into_iter()
        .enumerate()
    {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_eax(eax);
        cpu.registers.set_ebx(ebx);
        if shape == Shape::PortCallOut {
            // The port the call-out fixtures already use. Port 0 is not claimed by this bus, and
            // an unclaimed port takes the ABNORMAL side exit -- which leaves EIP on the `IN` and
            // means the Jcc never ran at all.
            cpu.registers.set_edx(0x03da);
        }
        cpu.registers.eflags = seed;
        cpu.pending_flags = PendingFlags::default();
        if let Some((a, b)) = pending {
            let _ = cpu.alu(0, a, b, BusWidth::Dword);
        }
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        if index == 0 {
            effective = cpu.materialized_eflags();
        }
    }
    prepared.native_bus.trace = BusTrace::default();
    prepared.interpreter_bus.trace = BusTrace::default();

    assert!(
        prepared
            .native
            .try_run_direct_block_for_test(&mut prepared.native_bus, prepared.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    for _ in 0..shape.instructions() {
        prepared
            .interpreter
            .cycle(&mut prepared.interpreter_bus)
            .unwrap();
    }

    // The independent oracle. For the producer shapes `effective` is the ENTRY image, so the
    // expectation is only meaningful where the producer cannot have moved the tested bit; those
    // callers pass `Shape::Terminal`-equivalent expectations of their own instead.
    if shape.preserves_entry_flags() {
        let want = if predicate(condition, effective) {
            prepared.taken
        } else {
            prepared.fallthrough
        };
        assert_eq!(
            prepared.native.registers.eip, want,
            "{context}: branch outcome disagrees with the Intel condition table applied to the \
             materialized shadow {effective:#x}"
        );
    }
    assert_eq!(
        prepared.native.registers, prepared.interpreter.registers,
        "{context}: registers"
    );
    assert_eq!(
        prepared.native.eflags(),
        prepared.interpreter.eflags(),
        "{context}: EFLAGS"
    );
    assert_eq!(
        prepared.native.elapsed_clocks, prepared.interpreter.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        prepared.native_bus.trace.elapsed_clocks(),
        prepared.interpreter_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    prepared.native.registers.eip == prepared.taken
}

/// A condition whose whole sweep fell one way tested nothing about the predicate. This is the
/// anti-vacuity check on the FIXTURE, and it is separate from the arm-engagement check on the
/// SLICE: an emitter that hard-coded "never taken" would pass a differential whose seeds all
/// happened to be not-taken.
fn assert_discriminating(condition: u8, taken: usize, total: usize, context: &str) {
    assert!(
        taken > 0 && taken < total,
        "{context}: condition {condition:#x} produced {taken}/{total} taken -- the seed set does \
         not discriminate it and the rows above prove nothing"
    );
}

/// Sweep a contiguous range of conditions over the full seed set, on both arms.
///
/// Split into two callers (`0x0..0xc` and `0xc..0x10`) deliberately: the twelve simple and
/// overflow conditions and the four signed folds are independently bisectable surfaces, which is
/// how a disappointing ladder is narrowed to one half without a second ladder.
fn sweep(range: std::ops::Range<u8>, label: &str) {
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for condition in range.clone() {
            let mut taken = 0usize;
            let mut total = 0usize;
            for seed in SEEDS {
                for pending in [None, Some((0x7fff_ffff, 1))] {
                    let context = format!(
                        "{label} on={on} cond={condition:#x} seed={seed:#x} pending={}",
                        pending.is_some()
                    );
                    let mut prepared = prepare(Shape::Terminal, condition);
                    if run_row(
                        &mut prepared,
                        Shape::Terminal,
                        condition,
                        on,
                        0,
                        0,
                        seed,
                        pending,
                        &context,
                    ) {
                        taken += 1;
                    }
                    total += 1;
                }
            }
            assert_discriminating(condition, taken, total, label);
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.1, first half. The ten single-bit and CF|ZF conditions plus JO/JNO -- the twelve that lower
/// to one `test` and one branch, and the bulk of the executed volume.
#[test]
fn jcc_shadow_matches_the_interpreter_for_the_twelve_simple_conditions() {
    sweep(0..0xc, "simple");
}

/// 6.1, second half. The four signed folds, which are the only conditions that touch RAX and the
/// only ones whose lowering is arithmetic rather than a mask. Separately bisectable from the
/// twelve above ON PURPOSE.
#[test]
fn jcc_shadow_matches_the_interpreter_for_the_four_signed_conditions() {
    sweep(0xc..0x10, "signed");
}

/// 6.2, seeded shape. The eight (SF, OF, ZF) points against an EXPLICIT expected-taken table,
/// rather than against the interpreter -- so a fault shared by both lowerings cannot hide here.
///
/// This is the fixture that separates XOR7 from XOR6 and that no misaligned shift survives.
#[test]
fn signed_jcc_boundary_enumerates_sf_of_zf() {
    // (seed, JL, JGE, JLE, JG) -- worked out by hand from SF^OF and ZF|(SF^OF).
    const TABLE: [(u32, bool, bool, bool, bool); 8] = [
        (0x002, false, true, false, true), // SF=0 OF=0 ZF=0
        (0x042, false, true, true, false), // SF=0 OF=0 ZF=1
        (0x082, true, false, true, false), // SF=1 OF=0 ZF=0
        (0x0c2, true, false, true, false), // SF=1 OF=0 ZF=1
        (0x802, true, false, true, false), // SF=0 OF=1 ZF=0
        (0x842, true, false, true, false), // SF=0 OF=1 ZF=1
        (0x882, false, true, false, true), // SF=1 OF=1 ZF=0
        (0x8c2, false, true, true, false), // SF=1 OF=1 ZF=1
    ];
    // The table must still enumerate all eight points. A future "these two look redundant" prune
    // would take M5's only killer with it, so the coverage is asserted rather than assumed.
    assert_eq!(
        TABLE.map(|(seed, ..)| seed),
        SIGNED_POINTS,
        "the signed table must cover every (SF, OF, ZF) point, (SF=0, OF=1) above all"
    );
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for (seed, jl, jge, jle, jg) in TABLE {
            for (condition, want) in [(0xcu8, jl), (0xd, jge), (0xe, jle), (0xf, jg)] {
                let context = format!("signed-table on={on} cond={condition:#x} seed={seed:#x}");
                let mut prepared = prepare(Shape::Terminal, condition);
                let taken = run_row(
                    &mut prepared,
                    Shape::Terminal,
                    condition,
                    on,
                    0,
                    0,
                    seed,
                    None,
                    &context,
                );
                assert_eq!(taken, want, "{context}: against the hand-written table");
            }
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.2, producer shape. The six (SF, OF, ZF) points a real ADD can reach -- ZF = 1 forces SF = 0,
/// so (SF=1, ZF=1) has no producer at either OF polarity and lives in the seeded table above.
///
/// Pins that the terminator reads what `emit_capture_flags` merged, on the same axis the seeded
/// table covers, so a narrowed capture mask at the producer is caught here rather than showing up
/// as a wall result.
#[test]
fn signed_jcc_boundary_through_an_add_producer() {
    // (eax, ebx) for `add eax, ebx`, with the (SF, OF, ZF) each produces.
    const POINTS: [(u32, u32); 6] = [
        (1, 1),                     // SF=0 OF=0 ZF=0
        (0, 0),                     // SF=0 OF=0 ZF=1
        (0, 0xffff_ffff),           // SF=1 OF=0 ZF=0
        (0x4000_0000, 0x4000_0000), // SF=1 OF=1 ZF=0
        (0x8000_0000, 0x8000_0001), // SF=0 OF=1 ZF=0
        (0x8000_0000, 0x8000_0000), // SF=0 OF=1 ZF=1
    ];
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for condition in 0xcu8..0x10 {
            let mut taken = 0usize;
            for (eax, ebx) in POINTS {
                // The ENTRY seed contradicts what the producer will write on every flag the
                // producer defines, so a lowering that read the entry shadow instead of the
                // merged one diverges rather than coincidentally agreeing.
                let context = format!("add-producer on={on} cond={condition:#x} eax={eax:#x}");
                let mut prepared = prepare(Shape::AddProducer, condition);
                if run_row(
                    &mut prepared,
                    Shape::AddProducer,
                    condition,
                    on,
                    eax,
                    ebx,
                    0x8d7,
                    None,
                    &context,
                ) {
                    taken += 1;
                }
            }
            assert_discriminating(condition, taken, POINTS.len(), "add-producer");
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.1b. The terminator must read the LAST PRODUCER's merge, not the entry shadow.
///
/// Coverage is the producer's defined mask and no wider, and that is stated here so nobody later
/// mistakes it for the sweep's: `and eax, eax` lowers through `emit_capture_flags(e, LOGIC_FLAGS)`,
/// which writes ZF/SF/PF from the result and forces CF = OF = 0. The entry seed sets all five the
/// other way, so every condition below has a different answer under "read the entry shadow" than
/// under "read the merged shadow".
#[test]
fn jcc_reads_the_last_producers_shadow_not_the_entry_shadow() {
    // eax = 0x8000_0000: the AND leaves SF=1, ZF=0, PF=1 (low byte 0), CF=0, OF=0.
    const EAX: u32 = 0x8000_0000;
    // Entry seed: SF=0, ZF=1, PF=0, CF=1, OF=1 -- the opposite of every one of those.
    const SEED: u32 = 0x843;
    // (condition, taken under the MERGED shadow). Under the entry shadow every one of these flips.
    const EXPECT: [(u8, bool); 6] = [
        (0x0, false), // JO   -- OF cleared by the AND, set in the seed
        (0x2, false), // JB   -- CF cleared by the AND, set in the seed
        (0x4, false), // JZ   -- result non-zero, seed had ZF
        (0x8, true),  // JS   -- result negative, seed had SF clear
        (0xa, true),  // JP   -- low byte 0 is even parity, seed had PF clear
        (0x6, false), // JBE  -- both its terms cleared by the AND
    ];
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for (condition, want) in EXPECT {
            let context = format!("last-producer on={on} cond={condition:#x}");
            let mut prepared = prepare(Shape::AndProducer, condition);
            let taken = run_row(
                &mut prepared,
                Shape::AndProducer,
                condition,
                on,
                EAX,
                0,
                SEED,
                None,
                &context,
            );
            assert_eq!(
                taken, want,
                "{context}: the terminator read the entry shadow, not the producer's merge"
            );
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.3. No Jcc condition reads AF, and this turns that from a claim into a pin.
///
/// A LOGIC producer is the right shape precisely because `LOGIC_FLAGS` EXCLUDES AF: the entry AF
/// survives `and eax, eax` untouched and is live in the shadow at the terminator. Both AF entries
/// must give the same branch outcome at all sixteen conditions. This is the fixture that fails the
/// day a mask table gains `FLAG_AF`.
#[test]
fn jcc_ignores_a_stale_af_left_by_a_logic_producer() {
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for condition in 0u8..16 {
            let mut outcomes = Vec::new();
            for seed in [0x002u32, 0x012] {
                let context = format!("stale-af on={on} cond={condition:#x} seed={seed:#x}");
                let mut prepared = prepare(Shape::AndProducer, condition);
                outcomes.push(run_row(
                    &mut prepared,
                    Shape::AndProducer,
                    condition,
                    on,
                    0x8000_0000,
                    0,
                    seed,
                    None,
                    &context,
                ));
            }
            assert_eq!(
                outcomes[0], outcomes[1],
                "condition {condition:#x} changed its answer when only AF moved (on={on})"
            );
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.4. The block's first and only slot is the Jcc and a live `PendingFlags` descriptor is
/// standing, left by an interpreted producer immediately before entry.
///
/// The descriptor's materialised answer DISAGREES with the raw `eflags` word on every flag it
/// defines, so a lowering that read `registers.eflags` instead of `materialized_eflags()` fails
/// rather than coincidentally passing. `0x7fff_ffff + 1` materialises SF=1, OF=1, ZF=0, CF=0,
/// PF=1; the seed says SF=0, OF=0, ZF=1, CF=1, PF=0.
#[test]
fn jcc_at_the_block_entry_slot_reads_the_materialized_shadow() {
    const SEED: u32 = 0x043;
    const EXPECT: [(u8, bool); 5] = [
        (0x0, true),  // JO  -- 0x7fffffff + 1 overflows
        (0x2, false), // JB  -- no carry out, seed says CF
        (0x4, false), // JZ  -- result non-zero, seed says ZF
        (0x8, true),  // JS  -- result 0x80000000
        (0xa, true),  // JP  -- low byte 0
    ];
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for (condition, want) in EXPECT {
            let context = format!("entry-slot-pending on={on} cond={condition:#x}");
            let mut prepared = prepare(Shape::Terminal, condition);
            let taken = run_row(
                &mut prepared,
                Shape::Terminal,
                condition,
                on,
                0,
                0,
                SEED,
                Some((0x7fff_ffff, 1)),
                &context,
            );
            assert_eq!(
                taken, want,
                "{context}: the entry shadow must be materialized_eflags(), not registers.eflags"
            );
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.7. The byte ledger: `len(ON) - len(OFF)` per condition, against the class table.
///
/// **The DELTA, never the absolute lengths.** The delta is the number the arena-occupancy risk is
/// priced on, and it does not churn when an unrelated emitter change moves the prologue or the
/// completed path; an absolute pin would be edited by the next person who touches
/// `emit_accounting`, and edited without thought, which is how a ledger stops being a ledger.
///
/// This is also the ONLY killer for a mutation of `emit_shadow_test`'s form rule, which is
/// semantically INERT -- `test ebp,0x40` and `test bpl,0x40` set ZF identically, so no
/// differential can see the difference and no differential is pre-registered as if it could.
#[test]
fn jcc_shadow_emission_delta_matches_the_ledger() {
    for condition in 0u8..16 {
        jit::direct::set_jcc_shadow_for_test(Some(false));
        let off = prepare(Shape::Terminal, condition);
        jit::direct::set_jcc_shadow_for_test(Some(true));
        let on = prepare(Shape::Terminal, condition);
        let class = expected_class(condition);
        assert_eq!(
            on.code_len as i64 - off.code_len as i64,
            CLASS_BYTE_DELTA[class],
            "condition {condition:#x} ({}) emission delta",
            CLASS_NAMES[class]
        );
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.8, the OFF-arm direction ONLY. A raw `0x9D` scan is not a sound oracle in the other
/// direction: one unstructured byte occurs by coincidence inside any `disp32`, `imm32`, ModRM or
/// SIB that happens to hold it, so "the ON arm contains none" is a fragile assertion that would
/// eventually fail on an unrelated block. 6.7's delta ledger is the exact oracle for the ON arm
/// and carries that half of the property.
#[test]
fn jcc_shadow_off_arm_emits_popfq() {
    jit::direct::set_jcc_shadow_for_test(Some(false));
    for condition in 0u8..16 {
        let off = prepare(Shape::Terminal, condition);
        assert!(
            off.code.contains(&0x9d),
            "condition {condition:#x}: the OFF arm must still round-trip through popfq"
        );
        assert_eq!(off.sites, [0; 4], "the OFF arm registers no shadow site");
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.9. One site in each lane, and a DISCARDED compile registers none.
///
/// The discarded half is what pins the install-site discipline: a fold moved from `install` into
/// the compile walk would charge every prefix the recovery search throws away and every walk that
/// ends in a `Retry`, and the counter's denominator would stop being "blocks this run installed".
#[test]
fn jcc_shadow_sites_count_by_class_at_install() {
    jit::direct::set_jcc_shadow_for_test(Some(true));
    // One condition per class, in class order.
    let representatives = [0x4u8, 0x0, 0xc, 0xe];
    for (class, condition) in representatives.into_iter().enumerate() {
        let prepared = prepare(Shape::Terminal, condition);
        let mut want = [0u16; 4];
        want[class] = 1;
        assert_eq!(
            prepared.sites, want,
            "condition {condition:#x} must register exactly one {} site",
            CLASS_NAMES[class]
        );
        let snapshot = prepared.native.direct_stall_snapshot();
        assert_eq!(
            [
                snapshot.jcc_sites_simple,
                snapshot.jcc_sites_overflow,
                snapshot.jcc_sites_signed_xor,
                snapshot.jcc_sites_signed_xor_zf,
            ],
            want.map(u64::from),
            "the install fold must reach DirectStallSnapshot, which is what a leg reads"
        );
    }

    // A compile that is never installed charges nothing. Non-vacuous by construction: the walk
    // below really does lower a shadow site, and the tally still does not move.
    let (memory, fallthrough, taken) = image(Shape::Terminal, 0x4);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.registers.set_esp(STACK_TOP);
    for linear in [ENTRY, fallthrough, taken] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("block compiles");
    assert_eq!(
        compilation.jcc_shadow_sites(),
        [1, 0, 0, 0],
        "the discarded walk must itself have lowered a simple shadow site"
    );
    drop(compilation);
    let snapshot = cpu.direct_stall_snapshot();
    assert_eq!(
        [
            snapshot.jcc_sites_simple,
            snapshot.jcc_sites_overflow,
            snapshot.jcc_sites_signed_xor,
            snapshot.jcc_sites_signed_xor_zf,
        ],
        [0u64; 4],
        "a compile that never installs must charge nothing: the counter's denominator is blocks \
         this run INSTALLED"
    );
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.10. An x87-BEARING block terminated by a Jcc, on both arms.
///
/// Pins that neither shadow sequence disturbs RSI -- the Windows x87 tag-cache scratch, spilled
/// into the frame by the prologue and restored on the way out -- and that the x87 boundary work on
/// the completed path still sees what it expects. The two signed folds are the only shadow
/// sequences that name a general register at all, so all four conditions that touch RAX are run
/// here alongside a representative of each of the other two classes.
///
/// `fwait` touches no flag, so the seeded shadow reaches the terminator intact and `predicate`
/// still adjudicates every row.
#[test]
fn jcc_shadow_survives_an_x87_bearing_block() {
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for condition in [0x0u8, 0x4, 0xc, 0xd, 0xe, 0xf] {
            let mut taken = 0usize;
            for seed in SIGNED_POINTS {
                let context = format!("x87 on={on} cond={condition:#x} seed={seed:#x}");
                let mut prepared = prepare(Shape::X87Wait, condition);
                if run_row(
                    &mut prepared,
                    Shape::X87Wait,
                    condition,
                    on,
                    0,
                    0,
                    seed,
                    None,
                    &context,
                ) {
                    taken += 1;
                }
            }
            assert_discriminating(condition, taken, SIGNED_POINTS.len(), "x87");
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// 6.5. A block whose first slot is a PORT call-out, terminated by a Jcc.
///
/// The terminator therefore reads the RBP that the call-out RESUME path reloaded from
/// `registers.eflags`, after `publish_flags` set that word to `materialized_eflags()`. `IN` defines
/// no flag, so the seeded shadow must come out of the round trip byte for byte -- which makes the
/// independent oracle the exact check here, and a lost or truncated republish visible as a wrong
/// branch rather than as a subtly wrong flag word nobody reads.
#[test]
fn jcc_after_a_call_out_resume_reads_the_republished_shadow() {
    for on in [false, true] {
        jit::direct::set_jcc_shadow_for_test(Some(on));
        for condition in 0u8..16 {
            let mut taken = 0usize;
            for seed in SEEDS {
                let context = format!("callout on={on} cond={condition:#x} seed={seed:#x}");
                let mut prepared = prepare(Shape::PortCallOut, condition);
                if run_row(
                    &mut prepared,
                    Shape::PortCallOut,
                    condition,
                    on,
                    0,
                    0,
                    seed,
                    None,
                    &context,
                ) {
                    taken += 1;
                }
            }
            assert_discriminating(condition, taken, SEEDS.len(), "callout");
        }
    }
    jit::direct::set_jcc_shadow_for_test(None);
}

/// The `IZARRAVM_JCC_SHADOW` spelling table. The knob caches its env reading in a process-wide
/// `OnceLock`, so the contract is otherwise assertable exactly once per process and never in an
/// order the harness controls -- hence the parse function is exercised directly.
#[test]
fn jcc_shadow_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_jcc_shadow_arm_for_test;
    assert!(
        !parse(Err(VarError::NotPresent)),
        "unset must name the OFF arm: this knob ships default OFF"
    );
    // UNSET AND THE EMPTY STRING AGREE HERE, which is the OPPOSITE of the default-ON knobs in
    // env_gates.rs (and the opposite of IZARRAVM_SEGMENT_RETIRE_GOVERNOR, whose unset is `cap`
    // and whose empty string is OFF). Nulling a variable in PowerShell leaves it PRESENT and
    // EMPTY; here that spells the same arm as unset, so the trap cannot silently disarm a base
    // leg. The cost is the mirror image: an ON leg must EXPORT `1`.
    assert!(
        !parse(Ok(String::new())),
        "the empty string is the OFF arm, the same arm as unset"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the off arm");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the on arm");
    }
}

/// A typo must PANIC rather than silently run the default. A mistyped ladder leg that fell through
/// would be read as "the arm I asked for changed nothing", the one wrong conclusion an arm ladder
/// exists to avoid.
#[test]
#[should_panic(expected = "IZARRAVM_JCC_SHADOW")]
fn a_mistyped_jcc_shadow_arm_panics() {
    let _ = jit::direct::parse_jcc_shadow_arm_for_test(Ok("yes".to_string()));
}

/// THE DEFAULT PIN, and it is the assertion that decides what a shipped binary does at every Jcc
/// terminator. Catches a flip of `parse_jcc_shadow_arm`'s `NotPresent` arm -- the exact accident
/// changing a default introduces.
///
/// Reads the AMBIENT knob deliberately, so the assertion agrees with the ENVIRONMENT rather than
/// with a constant: this suite is meant to be runnable on both arms, and a fixture that
/// hard-asserted "off" would make an ON-arm suite run impossible by construction. With the
/// variable unset it reduces to "the default is OFF", which is the claim it exists for.
#[test]
fn jcc_shadow_ships_off_by_default() {
    jit::direct::set_jcc_shadow_for_test(None);
    let ambient = std::env::var("IZARRAVM_JCC_SHADOW");
    let expected = jit::direct::parse_jcc_shadow_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::jcc_shadow_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_JCC_SHADOW={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            !expected,
            "IZARRAVM_JCC_SHADOW must default OFF until a ladder prices the shadow arm"
        );
    }
}
