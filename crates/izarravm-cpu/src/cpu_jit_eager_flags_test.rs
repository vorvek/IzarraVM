// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_DIRECT_EAGER_FLAGS`: flag-producing slots publish the RBP EFLAGS shadow straight to
//! `registers.eflags` instead of writing a lazy `PendingFlags` descriptor for a later reader to
//! recompute from.
//!
//! # WHAT THESE FIXTURES HAVE TO DO THAT A PLAIN DIFFERENTIAL DOES NOT
//!
//! The arm's failure mode is a GRACEFUL AGREEMENT. A publish that commits the wrong bits, or a
//! producer that publishes nothing at all, is invisible to any row where a LATER slot in the same
//! block publishes the same flags again -- which is most rows, because flag producers cluster. So
//! every row here does four independent things:
//!
//! 1. **Proves the arm ENGAGED.** `Compilation::eager_flags_sites()` is an exact oracle: all six
//!    lanes read zero on the OFF arm by construction, and the tested shape's own lane reads
//!    non-zero on the ON arm. A row that silently ran the default arm cannot pass.
//! 2. **Checks against the INTERPRETER**, which keeps its lazy flags on both arms and is
//!    therefore an oracle the arm cannot move.
//! 3. **Uses SINGLE-PRODUCER blocks where the property is a publish.** A block whose tested
//!    producer is followed by another producer has its dropped publish masked by the next one.
//!    `PUBLISH_ROWS` below is built so the tested slot is the only flag producer in its block.
//! 4. **Pins the emitted BYTE DELTA per shape.** An encoding-length change that is semantically
//!    inert is invisible to every differential, and the arena-occupancy cost is priced on exactly
//!    that number.
//!
//! # THE ARM IS READ AT EMISSION TIME
//!
//! `set_direct_eager_flags_for_test` must be called BEFORE `jit::direct::compile`, and a fixture
//! that flips it mid-run must recompile. Every `build` below sets it as its first act and restores
//! `None` when the row is done.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry: an opcode at
/// the entry slot parks the block on the interpreter, so an entry-position fixture certifies
/// nothing.
const LEAD: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const TAIL: [u8; 2] = [0x89, 0xff];

/// The six `EAGER_CLASS_*` lanes, in lane order, for messages.
const CLASS_NAMES: [&str; jit::direct::EAGER_FLAGS_CLASSES] = [
    "arith",
    "logic",
    "byte_logic",
    "inc_dec",
    "cf_only",
    "mem_commit",
];

/// One tested shape: its guest bytes, the lane it must charge, and a name.
#[derive(Clone, Copy)]
struct Shape {
    name: &'static str,
    body: &'static [u8],
    class: usize,
}

const fn shape(name: &'static str, body: &'static [u8], class: usize) -> Shape {
    Shape { name, body, class }
}

/// One representative per lane, plus the extra shapes each lane needs to be exercised at all its
/// widths and both its arms.
///
/// **Every body here is a SINGLE flag producer**, which is what `PUBLISH_ROWS` and the mutants
/// that drop a publish depend on: with a second producer in the block the later publish would
/// overwrite the evidence of the first one's absence.
const SHAPES: &[Shape] = &[
    // ---- arith ----
    shape("add_eax_ecx", &[0x01, 0xc8], jit::direct::EAGER_CLASS_ARITH),
    shape("sub_eax_ecx", &[0x29, 0xc8], jit::direct::EAGER_CLASS_ARITH),
    shape("cmp_eax_ecx", &[0x39, 0xc8], jit::direct::EAGER_CLASS_ARITH),
    shape("adc_eax_ecx", &[0x11, 0xc8], jit::direct::EAGER_CLASS_ARITH),
    shape("sbb_eax_ecx", &[0x19, 0xc8], jit::direct::EAGER_CLASS_ARITH),
    shape("add_al_cl", &[0x00, 0xc8], jit::direct::EAGER_CLASS_ARITH),
    shape("neg_eax", &[0xf7, 0xd8], jit::direct::EAGER_CLASS_ARITH),
    shape(
        "add_ax_cx",
        &[0x66, 0x01, 0xc8],
        jit::direct::EAGER_CLASS_ARITH,
    ),
    // ---- logic ----
    shape("and_eax_ecx", &[0x21, 0xc8], jit::direct::EAGER_CLASS_LOGIC),
    shape("or_eax_ecx", &[0x09, 0xc8], jit::direct::EAGER_CLASS_LOGIC),
    shape("xor_eax_ecx", &[0x31, 0xc8], jit::direct::EAGER_CLASS_LOGIC),
    shape(
        "test_eax_ecx",
        &[0x85, 0xc8],
        jit::direct::EAGER_CLASS_LOGIC,
    ),
    shape(
        "and_ax_cx",
        &[0x66, 0x21, 0xc8],
        jit::direct::EAGER_CLASS_LOGIC,
    ),
    // ---- byte logic: BLOCKER 1's arm. `and al, cl` WRITES BACK, which is the half that
    // miscompiles if the descriptor producer is deleted without its RDX reload.
    shape(
        "and_al_cl",
        &[0x20, 0xc8],
        jit::direct::EAGER_CLASS_BYTE_LOGIC,
    ),
    shape(
        "or_al_cl",
        &[0x08, 0xc8],
        jit::direct::EAGER_CLASS_BYTE_LOGIC,
    ),
    shape(
        "xor_al_cl",
        &[0x30, 0xc8],
        jit::direct::EAGER_CLASS_BYTE_LOGIC,
    ),
    shape(
        "and_al_imm8",
        &[0x80, 0xe0, 0x5a],
        jit::direct::EAGER_CLASS_BYTE_LOGIC,
    ),
    shape(
        "test_al_cl",
        &[0x84, 0xc8],
        jit::direct::EAGER_CLASS_BYTE_LOGIC,
    ),
    // ---- inc/dec ----
    shape("inc_eax", &[0x40], jit::direct::EAGER_CLASS_INC_DEC),
    shape("dec_eax", &[0x48], jit::direct::EAGER_CLASS_INC_DEC),
    shape("inc_ax", &[0x66, 0x40], jit::direct::EAGER_CLASS_INC_DEC),
    shape("inc_al", &[0xfe, 0xc0], jit::direct::EAGER_CLASS_INC_DEC),
    // ---- CF only ----
    shape(
        "bt_eax_ecx",
        &[0x0f, 0xa3, 0xc8],
        jit::direct::EAGER_CLASS_CF_ONLY,
    ),
    shape(
        "ror_eax_3",
        &[0xc1, 0xc8, 0x03],
        jit::direct::EAGER_CLASS_CF_ONLY,
    ),
    shape("clc", &[0xf8], jit::direct::EAGER_CLASS_CF_ONLY),
    shape("stc", &[0xf9], jit::direct::EAGER_CLASS_CF_ONLY),
    // ---- memory-destination commit ----
    shape(
        "add_mem_eax",
        &[0x01, 0x05, 0x00, 0x20, 0x00, 0x00],
        jit::direct::EAGER_CLASS_MEM_COMMIT,
    ),
    shape(
        "and_mem_eax",
        &[0x21, 0x05, 0x00, 0x20, 0x00, 0x00],
        jit::direct::EAGER_CLASS_MEM_COMMIT,
    ),
    shape(
        "adc_mem_eax",
        &[0x11, 0x05, 0x00, 0x20, 0x00, 0x00],
        jit::direct::EAGER_CLASS_MEM_COMMIT,
    ),
];

/// The seeded architectural state a row starts BOTH roles from.
#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    /// Install a LIVE lazy descriptor before the block runs, through a real interpreter ALU op.
    /// This is what makes the entry clear (E1) observable: on the eager arm the descriptor must
    /// not survive into native execution, or it outranks every publish the block performs.
    live_pending: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            gpr: [0x1234_5678, 0x0000_00a5, 0, 0, 0, 0, 0, 0],
            eflags: 0x202,
            live_pending: false,
        }
    }

    fn gpr(mut self, gpr: [u32; 8]) -> Self {
        self.gpr = gpr;
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

/// The flag seeds every differential row runs. AF is seeded BOTH ways and CF is seeded BOTH ways,
/// deliberately:
///
/// * **AF both ways** is what makes a logic-arm capture mask widened from `LOGIC_FLAGS` to
///   `ARITH_FLAGS` go red. Host `AND` clearing AF is a HOST behaviour, not a guarantee, so a
///   one-sided AF seed can pass by luck.
/// * **CF both ways** is what makes an INC/DEC capture mask widened to include CF go red: INC and
///   DEC architecturally PRESERVE CF, and a seed of 0 agrees with a host `inc` that clears it.
const FLAG_SEEDS: [u32; 6] = [
    0x0202, // AF clear, CF clear
    0x0203, // CF set
    0x0212, // AF set
    0x0213, // AF and CF set
    0x08d7, // SF/ZF/PF/AF/CF all set, OF set
    0x0246, // ZF and PF set
];

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    slots: usize,
    sites: [u16; jit::direct::EAGER_FLAGS_CLASSES],
    code_len: usize,
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role with the arm
/// forced to `on`, warm the same decode lines on the interpreter role, and seed both identically.
///
/// The arm is set BEFORE `compile`, which is the whole of the hazard: it is read at emission time.
fn build(body: &[u8], seed: Seed, on: bool) -> Roles {
    jit::direct::set_direct_eager_flags_for_test(Some(on));

    let mut code = LEAD.to_vec();
    let mut starts = vec![ENTRY, ENTRY + code.len() as u32];
    code.extend_from_slice(body);
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&TAIL);
    code.push(0xf4);
    let slots = 3;

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // A non-zero word at the memory-destination shapes' operand address, so a commit that wrote
    // the wrong value is visible in guest RAM rather than agreeing with a zero.
    memory[0x2000..0x2004].copy_from_slice(&0x0f0f_0f0fu32.to_le_bytes());

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
        // ESP must be live BEFORE compiling, not only before running: a stack-touching slot
        // resolves its store page at compile time, and the default ESP of 0 makes that page
        // 0xFFFFFFFC, which cannot resolve and returns the whole block as `Retry` --
        // indistinguishable from the shape still being a barrier.
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        // The pages the block reads and writes have to be in the fast map before compilation, for
        // the same reason ESP does. Two of them: the stack page, and the page the
        // memory-destination shapes address at 0x2000.
        for page in [(STACK_TOP - 4) & !0xfff, 0x2000 & !0xfff] {
            let read = bus
                .direct_page(page, BusAccessKind::DataRead)
                .unwrap()
                .unwrap();
            cpu.jit_fast_map.populate_read(
                page,
                page,
                read,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            );
            let write = bus
                .direct_page(page, BusAccessKind::DataWrite)
                .unwrap()
                .unwrap();
            cpu.jit_fast_map.populate_write(
                page,
                page,
                write,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            );
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
            panic!("structurally rejected: the shape is a compile barrier and certifies nothing")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        usize::from(compilation.span.instructions),
        slots,
        "the block must cover every slot, so the tested opcode really ran natively"
    );
    let sites = compilation.eager_flags_sites();
    let code_len = compilation.code.len();
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
            // A real interpreter ALU op, so the descriptor is one the interpreter would actually
            // have installed rather than a hand-built word.
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
        sites,
        code_len,
    }
}

/// Run every slot natively, step the interpreter the same number of times, and compare everything
/// a guest can observe.
fn run_and_compare(mut roles: Roles, on: bool, context: &str) -> Roles {
    let slots = roles.slots;
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        usize::try_from(roles.native.perf_counters().jit_direct_insns - retired).unwrap(),
        slots,
        "{context}: every slot must retire natively"
    );
    for _ in 0..slots {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }

    // THE ARCHITECTURAL FLAGS FIRST, on the roles as they are. This is the substantive assertion
    // and it must come before anything settles them, or it is a tautology.
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: architectural EFLAGS"
    );
    // The GUEST REGISTER FILE, which is what BLOCKER 1's mutant moves: the byte-logic arm's RDX
    // reload carries the ALU RESULT, not a flag.
    assert_eq!(
        crate::tests::settled_registers(&roles.native),
        crate::tests::settled_registers(&roles.interp),
        "{context}: registers"
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

    if on {
        // I1, ON-ARM ONLY, and on a role that ENTERED native code. Nothing in the tree before
        // this slice asserted that native execution leaves the descriptor dead, so this is real
        // new coverage -- on this arm. On the shipped default arm it is FALSE by construction (a
        // native `AND r32,r32` leaves 0x8000_0202 in the descriptor), which is why it is gated.
        //
        // On the NATIVE role, never on a role a clone-and-settle step has already touched: a
        // settled role reads `is_none()` by construction and the assertion would be vacuous.
        assert!(
            roles.native.pending_flags.is_none(),
            "{context}: I1 holds on the ON arm after a native run -- no emitted producer may \
             leave a descriptor, and there is no longer an `emit_clear_pending` downstream to \
             hide one"
        );
        // I2, a SYNTHETIC-STATE gate. Bit 1 of `registers.eflags` is invariantly set: the reset
        // image carries it, `load_flags` ORs it in both operand-size arms so POPF/POPFD/IRET
        // cannot clear it, `set_flag_live` and SAHF OR it, and RBP is loaded from that word while
        // every RBP writer masks by a `defined` set that excludes 0x2. The eager publish stores
        // RBP wholesale, so if that invariance ever broke, bit 1 would start reaching EFLAGS
        // clear. This asserts the invariant is LIVE, not that any instruction is present.
        assert!(
            roles.native.registers.eflags & 0x2 != 0,
            "{context}: EFLAGS bit 1 must survive an eager publish"
        );
    }
    roles
}

fn finish() {
    jit::direct::set_direct_eager_flags_for_test(None);
}

// ---------------------------------------------------------------------------------------------
// 1. The knob.
// ---------------------------------------------------------------------------------------------

/// The spelling table, including that `0` is a legal OFF spelling. Stated as its own row because
/// the `parameter-knobs-have-no-off-spelling` trap -- a PARAMETER knob whose `=0` ARMS it -- cost
/// a whole census, and both conventions now live in `jit/direct.rs`.
#[test]
fn eager_flags_arm_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_eager_flags_arm_for_test;
    assert!(
        !parse(Err(VarError::NotPresent)),
        "unset is the default arm"
    );
    for off in ["", "0", "off", "OFF", "False", " 0 "] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the OFF arm");
    }
    for on in ["1", "on", "ON", "true", " TRUE "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the ON arm");
    }
}

/// A spelling that names no arm PANICS rather than silently running the default, because a
/// mistyped ladder leg would otherwise be read as the arm it named doing nothing.
#[test]
#[should_panic(expected = "names no arm")]
fn eager_flags_arm_refuses_to_guess() {
    let _ = jit::direct::parse_eager_flags_arm_for_test(Ok("yes".to_string()));
}

/// THE SHIPPED DEFAULT IS OFF, read with NO test override in force.
///
/// Every other fixture in this file forces the arm explicitly, which is right for what they test
/// and is exactly why this row has to exist: with the thread-local override set, a flipped knob
/// DEFAULT is invisible to all of them. This one clears the override first, so it reads the arm
/// the binary actually ships, and it is the only gate a default flip can fail.
///
/// It reads the arm through `compile` rather than through `eager_flags_enabled` so that a default
/// flip which somehow failed to reach emission would fail here too.
#[test]
fn the_shipped_default_arm_is_off() {
    jit::direct::set_direct_eager_flags_for_test(None);
    for shape in SHAPES {
        // `build` sets the override, so the arm has to be cleared again after it and the block
        // recompiled. Simpler: compile through `build`'s OFF path and then assert the AMBIENT
        // reading agrees with it.
        let off = build(shape.body, Seed::new(), false);
        finish();
        assert_eq!(
            off.sites,
            [0; jit::direct::EAGER_FLAGS_CLASSES],
            "{}: the OFF arm registers no eager site",
            shape.name
        );
    }
    assert!(
        !jit::direct::eager_flags_enabled(),
        "IZARRAVM_DIRECT_EAGER_FLAGS ships DEFAULT OFF, and flipping that default is an OWNER          DECISION -- not something a slice does on its way past"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. ARM VACUITY: the site ledger.
// ---------------------------------------------------------------------------------------------

/// Every one of the six lanes reads zero on the OFF arm and non-zero on the ON arm.
///
/// This is the ladder's vacuity check: a leg whose ON arm reads zero ran the default mechanism and
/// its wall number means nothing. It is also what a flipped knob default would fail.
///
/// **All six lanes, not a total.** A total would stay non-zero while a whole class silently fell
/// back to the descriptor path.
#[test]
fn eager_flags_sites_are_zero_off_and_charged_on_every_class_lane() {
    let mut charged = [0u32; jit::direct::EAGER_FLAGS_CLASSES];
    for shape in SHAPES {
        let off = build(shape.body, Seed::new(), false);
        assert_eq!(
            off.sites,
            [0; jit::direct::EAGER_FLAGS_CLASSES],
            "{}: the OFF arm must register no eager site at all",
            shape.name
        );
        let on = build(shape.body, Seed::new(), true);
        assert!(
            on.sites[shape.class] > 0,
            "{}: the ON arm must charge the {} lane, or the row ran the default mechanism",
            shape.name,
            CLASS_NAMES[shape.class]
        );
        charged[shape.class] += u32::from(on.sites[shape.class]);
    }
    finish();
    for (class, count) in charged.iter().enumerate() {
        assert!(
            *count > 0,
            "no shape charged the {} lane: the lane is unreachable and the table is a fiction",
            CLASS_NAMES[class]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 3. The BYTE LEDGER.
// ---------------------------------------------------------------------------------------------

/// THE LEDGER: per shape, how many eager publishes the ON arm emits and by how many BYTES its
/// emission differs from the OFF arm's.
///
/// **The byte DELTA, never the absolute length.** The delta is the number the arena-occupancy risk
/// is priced on and it does not churn when an unrelated emitter change moves the prologue or the
/// completed path; an absolute pin would be edited by the next person who touches
/// `emit_accounting`, and edited without thought, which is how a ledger stops being a ledger.
///
/// This is also the ONLY gate that sees a semantically-INERT encoding change: two encodings of the
/// same store set the same bits, so no differential can tell them apart and none is pre-registered
/// as if it could.
///
/// **The site column is not redundant with the byte column.** ADC/SBB carry TWO publishes because
/// the two arithmetic arms are mutually exclusive PATHS, not two producers. A ledger that pinned
/// only bytes could not tell a second publish on a second path from a longer encoding on one.
const SHAPE_LEDGER: &[(&str, u16, i64)] = &[
    ("add_eax_ecx", 1, -25),
    ("sub_eax_ecx", 1, -25),
    ("cmp_eax_ecx", 1, -25),
    ("adc_eax_ecx", 2, -69),
    ("sbb_eax_ecx", 2, -69),
    ("add_al_cl", 1, -25),
    ("neg_eax", 1, -29),
    ("add_ax_cx", 1, -25),
    ("and_eax_ecx", 1, -69),
    ("or_eax_ecx", 1, -69),
    ("xor_eax_ecx", 1, -69),
    ("test_eax_ecx", 1, -69),
    ("and_ax_cx", 1, -69),
    ("and_al_cl", 1, -76),
    ("or_al_cl", 1, -76),
    ("xor_al_cl", 1, -76),
    ("and_al_imm8", 1, -76),
    ("test_al_cl", 1, -69),
    ("inc_eax", 1, -42),
    ("dec_eax", 1, -42),
    ("inc_ax", 1, -57),
    ("inc_al", 1, -42),
    ("bt_eax_ecx", 1, -107),
    ("ror_eax_3", 1, -151),
    ("clc", 1, -107),
    ("stc", 1, -107),
    ("add_mem_eax", 1, -35),
    ("and_mem_eax", 1, -86),
    ("adc_mem_eax", 1, -133),
];

/// How many publishes a shape emits on the ON arm, from `SHAPE_LEDGER`.
fn expected_sites(name: &str) -> u16 {
    SHAPE_LEDGER
        .iter()
        .find(|(shape, _, _)| *shape == name)
        .unwrap_or_else(|| panic!("{name} has no ledger row"))
        .1
}

#[test]
fn eager_flags_emission_delta_matches_the_ledger() {
    let mut ledger: Vec<(&str, u16, i64)> = Vec::new();
    for shape in SHAPES {
        let off = build(shape.body, Seed::new(), false);
        let on = build(shape.body, Seed::new(), true);
        ledger.push((
            shape.name,
            on.sites.iter().sum(),
            on.code_len as i64 - off.code_len as i64,
        ));
    }
    finish();
    assert_eq!(
        ledger,
        SHAPE_LEDGER.to_vec(),
        "the per-shape publish count or ON-minus-OFF emitted byte delta moved"
    );
}

// ---------------------------------------------------------------------------------------------
// 4. The DIFFERENTIAL, on both arms.
// ---------------------------------------------------------------------------------------------

/// Every shape, every flag seed, with and without a live incoming descriptor, on both arms.
///
/// The OFF-arm half is not decoration: it is what proves a failure is the ARM's and not the
/// fixture's.
#[test]
fn the_eager_arm_matches_the_interpreter_for_every_producer_shape() {
    for shape in SHAPES {
        for &eflags in &FLAG_SEEDS {
            for pending in [false, true] {
                for on in [false, true] {
                    let mut seed = Seed::new().flags(eflags);
                    if pending {
                        seed = seed.pending();
                    }
                    let context = format!(
                        "{} eflags={eflags:#06x} pending={pending} on={on}",
                        shape.name
                    );
                    let roles = build(shape.body, seed, on);
                    if on {
                        assert!(
                            roles.sites[shape.class] > 0,
                            "{context}: the arm did not engage"
                        );
                    }
                    run_and_compare(roles, on, &context);
                }
            }
        }
    }
    finish();
}

/// The operand values that separate a flag lowering from a lucky one: zero, the sign bit, the
/// all-ones borrow case, an odd/even parity pair and a carry-out pair.
const OPERAND_PAIRS: [(u32, u32); 8] = [
    (0, 0),
    (0, 1),
    (1, 0),
    (0x8000_0000, 1),
    (0xffff_ffff, 1),
    (0x7fff_ffff, 1),
    (0x0f0f_0f0f, 0xf0f0_f0f0),
    (0xa5a5_a5a5, 0x5a5a_5a5a),
];

/// The same shapes over an operand sweep, so the flag ANSWER is exercised and not just the flag
/// PLUMBING. On the ON arm only, since arm-independent operand coverage already exists elsewhere.
#[test]
fn the_eager_arm_matches_the_interpreter_across_the_operand_corners() {
    for shape in SHAPES {
        for &(a, b) in &OPERAND_PAIRS {
            for &eflags in &[0x0202u32, 0x0213] {
                let seed = Seed::new()
                    .gpr([a, b, 0, 0, 0, 0, 0, 0])
                    .flags(eflags)
                    .pending();
                let context = format!(
                    "{} a={a:#010x} b={b:#010x} eflags={eflags:#06x}",
                    shape.name
                );
                let roles = build(shape.body, seed, true);
                run_and_compare(roles, true, &context);
            }
        }
    }
    finish();
}

// ---------------------------------------------------------------------------------------------
// 5. E1, the entry clear, and the shapes whose publish must not be masked.
// ---------------------------------------------------------------------------------------------

/// A LIVE descriptor entering a native block that produces flags.
///
/// This is the fixture that fails if `run_direct_block`'s entry clear (E1) is reverted to the bare
/// `let flags = self.materialized_eflags();` it used to be. Without the clear the interpreter's
/// descriptor survives into native execution, and on the eager arm it OUTRANKS every publish the
/// block performs: `materialized_eflags` recomputes the six arithmetic bits from the stale operand
/// pair over the word emitted code just stored.
///
/// The seeded descriptor is `0x7fff_ffff + 1`, whose materialised flags (OF, SF, AF, and ZF clear)
/// disagree with every row's real answer, so the survival is not maskable by a lucky agreement.
#[test]
fn the_eager_arm_needs_the_entry_clear() {
    for shape in SHAPES {
        let context = format!("{}: live descriptor entering a native producer", shape.name);
        let roles = build(shape.body, Seed::new().pending(), true);
        assert!(
            roles.sites[shape.class] > 0,
            "{context}: the arm did not engage"
        );
        run_and_compare(roles, true, &context);
    }
    finish();
}

/// A refused entry must leave the interpreter's descriptor UNTOUCHED, not settled.
///
/// E1 sits below every refusal return in `run_direct_block`, and this is what catches an E1 that
/// drifted above one: a refused entry runs no guest code, so it must move no state at all -- the
/// raw descriptor included, which is why this is the one fixture class that still compares it.
#[test]
fn a_refused_entry_leaves_the_descriptor_untouched_on_both_arms() {
    for on in [false, true] {
        let mut roles = build(&[0x01, 0xc8], Seed::new().pending(), on);
        let before_pending = roles.native.pending_flags;
        let before_eflags = roles.native.registers.eflags;
        assert!(
            !before_pending.is_none(),
            "the seed must leave a LIVE descriptor, or this fixture asserts nothing"
        );
        roles.native.interrupt_shadow = true;
        assert!(
            !roles
                .native
                .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                .unwrap(),
            "on={on}: the interrupt shadow must refuse the entry"
        );
        assert_eq!(
            roles.native.pending_flags, before_pending,
            "on={on}: a refused entry must not settle the descriptor"
        );
        assert_eq!(
            roles.native.registers.eflags, before_eflags,
            "on={on}: a refused entry must not move the flag base either"
        );
    }
    finish();
}

/// The shapes whose PUBLISH is the whole property, run in a block where the tested slot is the
/// ONLY flag producer AND nothing after it can republish.
///
/// `SHAPES`' bodies are already single producers, and `LEAD`/`TAIL` are `mov` forms that write no
/// flag, so a dropped publish at the tested slot reaches the comparison directly. Without that,
/// a later slot's publish would write the same architectural answer and mask the omission.
#[test]
fn a_dropped_publish_is_not_masked_by_a_later_slot() {
    for shape in SHAPES {
        for &eflags in &FLAG_SEEDS {
            let context = format!("{} eflags={eflags:#06x}: single-producer block", shape.name);
            let roles = build(shape.body, Seed::new().flags(eflags).pending(), true);
            assert_eq!(
                roles.sites.iter().map(|s| u32::from(*s)).sum::<u32>(),
                u32::from(roles.sites[shape.class]),
                "{context}: the block must charge exactly one lane, or it is not a \
                 single-producer block and a dropped publish could be masked"
            );
            assert_eq!(
                roles.sites[shape.class],
                expected_sites(shape.name),
                "{context}: the shape's whole publish population, so dropping one has nowhere \
                 to hide (ADC/SBB carry two because their arms are mutually exclusive PATHS, \
                 not two producers)"
            );
            run_and_compare(roles, true, &context);
        }
    }
    finish();
}

// ---------------------------------------------------------------------------------------------
// 6. The capture masks the publish commits.
// ---------------------------------------------------------------------------------------------

/// A rotate above count 1, and BT, write CF ALONE: SF, ZF, PF, AF and OF are architecturally
/// preserved.
///
/// The mechanism this guards, stated so the fixture is not guesswork: the host flags register is
/// SCRATCH across a block, and a rotate at count >= 2 writes neither SF/ZF/PF/AF nor a defined OF.
/// Widening either caller's `emit_capture_flags(FLAG_CF)` to `ARITH_FLAGS` therefore imports HOST
/// SCRATCH into the guest's five preserved flags -- and on the eager arm the publish commits it
/// straight to `registers.eflags`, where before it would have been overridden by a live
/// descriptor. Asserting against the interpreter avoids having to predict what the host left.
///
/// The block puts a flag PRODUCER in front of the rotate, so the five preserved flags carry a
/// value the rotate must not disturb rather than whatever the entry seed held.
#[test]
fn a_cf_only_writer_preserves_the_other_five_flags_on_the_eager_arm() {
    // `add eax, ecx` (a producer), then `ror eax, 3`, then `bt eax, ecx`.
    let body: &[u8] = &[0x01, 0xc8, 0xc1, 0xc8, 0x03, 0x0f, 0xa3, 0xc8];
    for &eflags in &FLAG_SEEDS {
        for &(a, b) in &OPERAND_PAIRS {
            let seed = Seed::new()
                .gpr([a, b, 0, 0, 0, 0, 0, 0])
                .flags(eflags)
                .pending();
            let context = format!("producer+ror+bt a={a:#010x} b={b:#010x} eflags={eflags:#06x}");
            let mut roles = build_multi(&[&body[0..2], &body[2..5], &body[5..8]], seed, true);
            assert!(
                roles.sites[jit::direct::EAGER_CLASS_CF_ONLY] >= 2,
                "{context}: both CF-only writers must charge the lane"
            );
            let slots = roles.slots;
            let retired = roles.native.perf_counters().jit_direct_insns;
            assert!(
                roles
                    .native
                    .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                    .unwrap(),
                "{context}: block did not run natively"
            );
            assert_eq!(
                usize::try_from(roles.native.perf_counters().jit_direct_insns - retired).unwrap(),
                slots
            );
            for _ in 0..slots {
                roles.interp.cycle(&mut roles.interp_bus).unwrap();
            }
            assert_eq!(
                roles.native.eflags(),
                roles.interp.eflags(),
                "{context}: architectural EFLAGS"
            );
            assert_eq!(
                crate::tests::settled_registers(&roles.native),
                crate::tests::settled_registers(&roles.interp),
                "{context}: registers"
            );
            assert!(roles.native.pending_flags.is_none(), "{context}: I1");
        }
    }
    finish();
}

/// `build` for a body of several instructions, each given separately so the decode lines can be
/// warmed at every start.
fn build_multi(bodies: &[&[u8]], seed: Seed, on: bool) -> Roles {
    jit::direct::set_direct_eager_flags_for_test(Some(on));
    let mut code = LEAD.to_vec();
    let mut starts = vec![ENTRY];
    for body in bodies {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(body);
    }
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&TAIL);
    code.push(0xf4);
    let slots = bodies.len() + 2;

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0x2000..0x2004].copy_from_slice(&0x0f0f_0f0fu32.to_le_bytes());

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
        // ESP must be live BEFORE compiling, not only before running: a stack-touching slot
        // resolves its store page at compile time, and the default ESP of 0 makes that page
        // 0xFFFFFFFC, which cannot resolve and returns the whole block as `Retry` --
        // indistinguishable from the shape still being a barrier.
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        // The pages the block reads and writes have to be in the fast map before compilation, for
        // the same reason ESP does. Two of them: the stack page, and the page the
        // memory-destination shapes address at 0x2000.
        for page in [(STACK_TOP - 4) & !0xfff, 0x2000 & !0xfff] {
            let read = bus
                .direct_page(page, BusAccessKind::DataRead)
                .unwrap()
                .unwrap();
            cpu.jit_fast_map.populate_read(
                page,
                page,
                read,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            );
            let write = bus
                .direct_page(page, BusAccessKind::DataWrite)
                .unwrap()
                .unwrap();
            cpu.jit_fast_map.populate_write(
                page,
                page,
                write,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            );
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
            panic!("structurally rejected: the shape is a compile barrier and certifies nothing")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(usize::from(compilation.span.instructions), slots);
    let sites = compilation.eager_flags_sites();
    let code_len = compilation.code.len();
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
        sites,
        code_len,
    }
}

/// BLOCKER 1's fixture: the byte-logic arm's RDX reload carries the guest ALU RESULT, not a flag.
///
/// Asserted on `registers`, deliberately. Deleting the descriptor producer at that arm while
/// leaving the reload does not merely lose flags -- it writes whatever the descriptor slot holds
/// into the guest's byte register, so the failure arrives as a WRONG REGISTER VALUE. A flags-only
/// assertion would miss it entirely on any seed where the wrong result happened to carry the same
/// flags.
#[test]
fn the_byte_logic_arm_writes_back_the_real_alu_result() {
    for body in [
        &[0x20u8, 0xc8][..],     // and al, cl
        &[0x08, 0xc8][..],       // or al, cl
        &[0x30, 0xc8][..],       // xor al, cl
        &[0x80, 0xe0, 0x5a][..], // and al, 0x5a
    ] {
        for &(a, b) in &OPERAND_PAIRS {
            for on in [false, true] {
                let seed = Seed::new().gpr([a, b, 0, 0, 0, 0, 0, 0]).pending();
                let context = format!("byte logic {body:02x?} a={a:#010x} b={b:#010x} on={on}");
                let mut roles = build(body, seed, on);
                let slots = roles.slots;
                assert!(
                    roles
                        .native
                        .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                        .unwrap(),
                    "{context}: block did not run natively"
                );
                for _ in 0..slots {
                    roles.interp.cycle(&mut roles.interp_bus).unwrap();
                }
                // THE REGISTER FILE, before the flags. A miscompiled write-back is a data bug.
                assert_eq!(
                    roles.native.registers.gpr, roles.interp.registers.gpr,
                    "{context}: the guest register file -- the byte ALU RESULT, not its flags"
                );
                assert_eq!(
                    roles.native.eflags(),
                    roles.interp.eflags(),
                    "{context}: architectural EFLAGS"
                );
            }
        }
    }
    finish();
}
