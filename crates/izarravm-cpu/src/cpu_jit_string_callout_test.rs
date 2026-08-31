// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The four UNPREFIXED STRING FAMILIES as `InterpretOne` call-out rows -- `0xA4`/`0xA5` MOVS,
//! `0xA6`/`0xA7` CMPS, `0xAA`/`0xAB` STOS, `0xAC`/`0xAD` LODS and `0xAE`/`0xAF` SCAS -- behind
//! `IZARRAVM_GENERIC_CALLOUT`, **default ON since the 2026-08-29 ladder** (nascar-586 min-wall
//! -2.9%, 56.16 s -> 54.55 s, entries -10%, unbound exits -15.4 M; duke3d-586 the expected-zero
//! control at +0.16% wall with identical guest instruction counts).
//!
//! # The rows, from the census that ranked them
//!
//! nascar-586 at main `f777010c`, `IZARRAVM_DIRECT_BARRIER_CENSUS=1`, plain release build. Entries
//! 162.1 M, unbound 115.8 M (71% of entries). The `rejected` class is 40,666,390 unbound exits and
//! its two largest heads are string primitives:
//!
//! | head | `runtime_hits` | native prefix / suffix |
//! |---|---:|---|
//! | `0xAD` LODSD dword | **17,202,631** | 9 / 8 |
//! | `0xAB` STOSD dword | **7,984,903** | 12 / 3 |
//!
//! 25.2 M of the 28.1 M call-out-shaped mass on that fixture, 89.7%. The rest is out of reach on
//! purpose: LOOP/LOOPE (9.2 M) are control transfers and R1 refuses resume for any of them, and
//! `0xD1 /2` RCL r32,1 (8.7 M) wants a native lowering rather than a ~40-host-instruction frame.
//!
//! # SLOT 0 is the production shape
//!
//! `0xAD`'s prefix of 9 means the block BEFORE it installs fine and the damage is the SECOND
//! compile unit -- LODSD plus the eight instructions behind it -- which forms fewer than three
//! slots and rejects wholesale. So with the gate ON the converted call-out is at slot 0 in 100% of
//! the measured population. Every behavioural row here runs BOTH positions: slot 0 because that is
//! what production does, and mid-block because that is what proves the emitter
//! ([[jit-fixture-entry-position-trap]] is right about the emitter and wrong about which shape the
//! census measures, so this file refuses to choose).
//!
//! # What is NOT admitted, and how many ways
//!
//! `0x6C`-`0x6F` INS/OUTS are members of the same `StringOp` family and would be swept in by any
//! rule phrased as "the string opcodes". They perform PORT I/O, bypassing both the port helper's
//! two-phase TSS-bitmap probe and `run_direct_block`'s entry privilege gate on
//! `callout_port_slots()`. FOUR independent bars stop them, and the mutation table below is what
//! established the ORDER -- which matters, because the first bar is not the one the design named:
//!
//! 1. **`block_continuable`, above `classify`.** `route_group` puts INS/OUTS in
//!    `DecodeGroup::Misc`, whose `block_continuable` arm admits only `0xa8`/`0xa9`, so the compile
//!    walk refuses them one test EARLIER than the classifier and their census arm is
//!    `non_continuable`. This is the bar that actually holds today.
//! 2. `classify`'s two explicit byte ranges cannot reach `0x6C..=0x6F`. Real, and currently
//!    unreachable -- M2 widens it and nothing changes.
//! 3. `execute_string_decoded`'s match is `unreachable!` outside `0xa4..=0xaf`, so they could
//!    never pick up the flat `clocks(4)` even if admitted.
//! 4. Their charge (12/10 through the port arm, 15/14 through the REP model) would raise the fold
//!    past 7 and move `INTERPRET_ONE_MAX_CORE_CLOCKS`.
//!
//! `the_denied_neighbours_stay_barriers_in_their_own_census_arm` and
//! `interpret_one_fold_is_unmoved_by_the_string_rows` catch the same hole from opposite ends.
//!
//! REP forms never reach `classify` at all: `prefixes_supported_for` refuses the prefix upstream,
//! and the census arm is `prefix_unsupported`, NOT `hard_boundary`. That distinction is pinned per
//! row rather than asserted once for the whole deny set, because a fixture rewritten to match
//! whatever the code did after its first red is the [[fixtures-that-cannot-fail]] shape.
//!
//! # Mutation record
//!
//! Applied BY HAND to the committed tree, run, observed, and restored with `git checkout --`.
//! Each was run against the whole `cpu_jit_string_callout_test` module; survivors were re-run
//! against the whole `izarravm-cpu` suite, because a survivor is the only outcome a filtered run
//! can get wrong in the direction that matters.
//!
//! | # | mutation | outcome, and the rows that fired |
//! |---|---|---|
//! | M1 | the `0xac \| 0xad` classify arm returns `None` | RED, 8 rows, led by `a_lodsd_at_slot_zero_compiles_only_with_the_gate` |
//! | M2 | widen the `0xa4 \| 0xa5` arm to `0x6c..=0x6f` | **SURVIVES -- and the survival is a FINDING, see below** |
//! | M2b | M2 **plus** `0x6c..=0x6f` added to `jit_admits_non_continuable` | RED, `the_denied_neighbours_...` and `the_gate_admits_exactly_the_ten_string_opcodes` |
//! | M3 | `STRING_CORE_CLOCKS` 4 -> 8 | RED **at compile time**: T6's `const` block refuses to evaluate |
//! | M3b | `POP_SS_CORE_CLOCKS` 7 -> 9 (the fold moves, the string term does not) | RED, `interpret_one_fold_is_unmoved_by_the_string_rows`, with the "re-ladder" message |
//! | M4 | drop the `STRING_CORE_CLOCKS` term from the `INTERPRET_ONE_MAX_CORE_CLOCKS` fold | SURVIVES, by design -- see below |
//! | M5 | give `StringStore` `may_write_segment() == true` | RED, `the_resume_matrix_holds_for_every_string_row` |
//! | M6 | delete the memory-eflags half of `emit_direction_flag` | RED, `a_string_slot_reads_the_direction_flag_from_a_cld_std_slot` |
//! | M7 | delete the call-out's runtime clock add | RED, 5 rows incl. BOTH differential positions -- **but only after the `timing_rem` fix below** |
//! | M8 | the `0xa6/0xa7/0xae/0xaf` arm charges `StringLoad` instead | RED, `each_string_execution_is_attributed_to_its_own_row` |
//! | M9 | `generic_callout_enabled()`'s ENV path returns a literal `true` | **SURVIVES since the flip** -- see below |
//! | M9b | the WHOLE of `generic_callout_enabled()` returns `true`, override bypassed | RED, every refusal row |
//! | M10 | `"" => GENERIC_CALLOUT_DEFAULT_ARM` -> `"" => false` | RED, `generic_callout_spelling_table_names_every_arm` |
//!
//! **M2 SURVIVES, and it refutes the mutation the design specified for T2.** The design's T2 says
//! *"widen the classify arm to `0x6C..=0x6F` -> INS/OUTS compile -> red"*. They do not compile,
//! and the classify arm is not what stops them: `route_group` puts INS/OUTS in
//! `DecodeGroup::Misc`, `block_continuable`'s `Misc` arm admits only `0xa8`/`0xa9`, and the
//! compile walk refuses a non-continuable shape ABOVE `classify` -- so a widened classify arm for
//! them is unreachable code and no fixture can see it. The exclusion is enforced FOUR ways, not
//! three, and the byte-range bar is the *fourth* rather than the first. M2b is the mutation that
//! is actually discriminating: widen the arm AND make the shape continuable, which is the shape a
//! future editor unifying the two ranges into a `StringOp` match would have to produce before the
//! hole opened. Recorded rather than quietly substituted, because "the deny is enforced three
//! ways" was a claim this file made and one of the three turned out to be inert.
//!
//! **M4 SURVIVES and is recorded rather than papered over.** `STRING_CORE_CLOCKS` is 4 and the
//! fold's other terms already reach 7, so removing the term changes no value today. That is
//! precisely the shape the fold exists to prevent LATER -- a row admitted without a term is an
//! under-budgeted chain hop the day its arm's charge rises -- and no fixture can separate a
//! present term from an absent one while the term is dominated. Pinning it would mean asserting
//! the text of the constant, which is a shape test dressed as a behaviour test. The term is in the
//! fold, the reason is in its doc, and M3/M3b cover the two ways the VALUE can move.
//!
//! **The 2026-08-29 default flip traded two mutants, and neither trade is left silent.** M9 was
//! written against a default-OFF knob, where a literal `true` on the env path contradicted it and
//! the default pin died. Now that the default IS `true` the two coincide and the mutant is inert:
//! it changes no behaviour any fixture can observe. It is recorded at its CURRENT outcome rather
//! than at the one the pre-flip run measured, and M9b is what covers the ground it used to --
//! bypassing the thread-local override entirely, which every refusal row then catches, because
//! `force_generic_callout` asserts its own selection took. M10 was the mirror case: before the
//! flip the parse table spelled `"" => false` and the mutant flipping it to `true` died on the
//! spelling row; the flip inverted the arm, so the mutant is now the reverse edit and dies on the
//! same row. That row asserts against `generic_callout_default_arm_for_test()` rather than a
//! literal, which is why it moved with the default instead of being rewritten to match it.
//!
//! **A worked example of [[worktree-outputs-die-with-the-worktree]]'s sibling trap, paid for
//! during this slice.** The mutation loop restores with `git checkout -- <file>`, which discards
//! EVERY uncommitted change to that file and not only the mutation. Running the flip's mutants
//! against an UNCOMMITTED flip ate the whole flip out of `direct.rs` mid-loop; the fixture file
//! survived only because it was a different path. The rule the byte-shift header already states --
//! apply mutations to a COMMITTED tree - is the reason, and it now has two independent scars
//! behind it.
//!
//! **M7 caught a real hole in this file on its first run, and the hole is the more useful
//! finding.** With only `elapsed_clocks` compared, deleting the runtime clock add survived every
//! three- and four-slot row here and died on ONE longer block by luck. `level_timing` at 586 is
//! **1/12**, so a whole string primitive's four raw clocks scale to ZERO elapsed clocks and land
//! entirely in `timing_rem`, which nothing was comparing. `compare_state` now compares the
//! remainder as well as the quotient; both roles start it at zero. Any differential in this tree
//! that compares `elapsed_clocks` alone has the same blind spot for any charge under twelve
//! clocks.

use super::*;

// ---------------------------------------------------------------------------------------------
// The fixture's geometry
// ---------------------------------------------------------------------------------------------

/// The string source, on a page the block's code is not on. A code page would make every row an
/// accidental self-modifying-store case, which is a REAL case with a fixture of its own.
const SRC: u32 = 0x2000;
/// The string destination, on a third page.
const DST: u32 = 0x3000;

/// `mov ebp,ebp`: the filler slot. EBP is chosen because it is the one register no string
/// primitive reads or writes -- ESI, EDI, ECX, EAX and the flags are all in play, and a filler
/// that touched any of them would make the differential compare the filler.
const FILL: [u8; 2] = [0x89, 0xed];

/// The ten opcodes this slice admits, with the census row each is expected to charge and the
/// register the differential seeds to make it non-vacuous.
const STRING_ROWS: [(&str, &[u8], &str); 10] = [
    ("movsb", &[0xa4], "0xa4_a5_movs"),
    ("movsd", &[0xa5], "0xa4_a5_movs"),
    ("cmpsb", &[0xa6], "0xa6_a7_cmps"),
    ("cmpsd", &[0xa7], "0xa6_a7_cmps"),
    ("stosb", &[0xaa], "0xaa_ab_stos"),
    ("stosd", &[0xab], "0xaa_ab_stos"),
    ("lodsb", &[0xac], "0xac_ad_lods"),
    ("lodsd", &[0xad], "0xac_ad_lods"),
    ("scasb", &[0xae], "0xa6_a7_cmps"),
    ("scasd", &[0xaf], "0xa6_a7_cmps"),
];

/// The four rows, for the per-row sweeps that do not care about the encoding.
const ROWS: [(&str, jit::direct::InterpretOneRow); 4] = [
    ("0xa4_a5_movs", jit::direct::InterpretOneRow::StringMove),
    ("0xa6_a7_cmps", jit::direct::InterpretOneRow::StringCompare),
    ("0xaa_ab_stos", jit::direct::InterpretOneRow::StringStore),
    ("0xac_ad_lods", jit::direct::InterpretOneRow::StringLoad),
];

/// Every segment record compared, which is the STRICT reading of R2 and the one a row with
/// `may_write_segment() == false` gets.
const ALL_SEGMENTS: u8 = u8::MAX;

/// A distinct byte at every address, so a stray write of any width is visible in the whole-RAM
/// compare rather than hidden by a zero fill matching a zero store.
fn string_memory() -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

fn map_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
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

// ---------------------------------------------------------------------------------------------
// Arm selection
// ---------------------------------------------------------------------------------------------

/// Restores the arm on the way out of a fixture -- normally OR by panic. A plain
/// `set_generic_callout_for_test(Some(..))` LEAKS: the override is thread-local and the harness
/// reuses threads, so the next fixture on that thread would inherit an arm it never asked for.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_generic_callout_for_test(None);
    }
}

/// Force the string-call-out arm and PROVE the selection took. Every row states its arm rather
/// than inheriting one, which is why the 2026-08-29 default flip moved no fixture in this file:
/// while the default was OFF it was the POSITIVE rows that needed this, and now it is the REFUSAL
/// rows that do.
#[must_use]
fn force_generic_callout(on: bool) -> ArmOverride {
    jit::direct::set_generic_callout_for_test(Some(on));
    assert_eq!(
        jit::direct::generic_callout_enabled(),
        on,
        "the fixture override must decide the arm, not the ambient IZARRAVM_GENERIC_CALLOUT"
    );
    ArmOverride
}

// ---------------------------------------------------------------------------------------------
// Where the tested opcode sits in the block
// ---------------------------------------------------------------------------------------------

/// Which slot the string opcode occupies.
///
/// BOTH are required on every behavioural row, and the reason is in the module docs: slot 0 is
/// 100% of the production population (the barred instruction becomes the ENTRY of the next compile
/// unit), and mid-block is what proves the emitter rather than the dispatcher.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Position {
    /// `<string> FILL FILL FILL hlt` -- the shape the census measures.
    SlotZero,
    /// `FILL <string> FILL hlt` -- the shape [[jit-fixture-entry-position-trap]] asks for.
    MidBlock,
}

impl Position {
    fn label(self) -> &'static str {
        match self {
            Self::SlotZero => "slot 0",
            Self::MidBlock => "mid-block",
        }
    }

    /// The block body, and the number of slots it must compile to.
    fn program(self, body: &[u8]) -> (Vec<u8>, u8) {
        let mut code = Vec::new();
        match self {
            Self::SlotZero => {
                code.extend_from_slice(body);
                for _ in 0..3 {
                    code.extend_from_slice(&FILL);
                }
                code.push(0xf4);
                (code, 4)
            }
            Self::MidBlock => {
                code.extend_from_slice(&FILL);
                code.extend_from_slice(body);
                code.extend_from_slice(&FILL);
                code.push(0xf4);
                (code, 3)
            }
        }
    }
}

const POSITIONS: [Position; 2] = [Position::SlotZero, Position::MidBlock];

// ---------------------------------------------------------------------------------------------
// The compile-only harness
// ---------------------------------------------------------------------------------------------

/// What a compile of one program produced.
struct Compiled {
    /// Slots the block covered, or `None` when the walk refused it.
    slots: Option<u8>,
    /// `InterpretOne` call-out slots in the block.
    interpret_one_slots: u8,
}

/// Compile `program` at `ENTRY` with every page mapped for read and write.
///
/// The unconditional page mapping is not incidental: with the operand pages absent from the fast
/// map every memory-touching kind is refused, so a negative assertion made without it would pass
/// for the harness's reason rather than the row's.
fn compile_program(program: &[u8], census: bool) -> (Compiled, CpuGsw) {
    let mut memory = string_memory();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + program.len()].copy_from_slice(program);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_esp(STACK_TOP);
    cpu.registers.set_esi(SRC);
    cpu.registers.set_edi(DST);
    if census {
        cpu.enable_direct_barrier_census(true);
    }
    // Every decode line, one byte at a time: the compile loop needs a decode for each slot and the
    // slot boundaries are not known here.
    for offset in 0..program.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x5000u32).step_by(0x1000) {
        map_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    let compiled = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => Compiled {
            slots: Some(compilation.span.instructions),
            interpret_one_slots: compilation.callout_interpret_one_slots,
        },
        _ => Compiled {
            slots: None,
            interpret_one_slots: 0,
        },
    };
    (compiled, cpu)
}

/// A body placed mid-block, compiled. The common shape for the refusal rows: a barrier in the body
/// slot stops the walk one slot in, which is shorter than the minimum installable block, so the
/// outcome is a `StructuralReject` and `slots` is `None`.
fn compile_body(body: &[u8]) -> Compiled {
    let (program, _) = Position::MidBlock.program(body);
    compile_program(&program, false).0
}

/// `mov eax,ecx`: the control row, which must compile on BOTH arms. Without it a refusal assertion
/// could pass because the harness refuses everything.
const CONTROL: [u8; 2] = [0x89, 0xc8];

// ---------------------------------------------------------------------------------------------
// The differential harness
// ---------------------------------------------------------------------------------------------

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    slots: u8,
}

/// The architectural state both roles start from. ESI and EDI point at the two data pages, ECX is
/// poisoned (a non-REP string primitive must not touch it), EAX carries a recognisable value for
/// STOS to write and for LODS to overwrite, and DF is whatever the seed says.
#[derive(Clone, Copy)]
struct Seed {
    eflags: u32,
    eax: u32,
    live_pending: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            // Bit 1 is the reserved bit the interpreter ors on every flag write; a seed with it
            // clear produces a one-bit disagreement that says nothing about this slice.
            eflags: 0x202,
            eax: 0x1234_5678,
            live_pending: false,
        }
    }

    fn direction_down(mut self) -> Self {
        self.eflags |= FLAG_DF;
        self
    }

    /// A lazy arithmetic descriptor live at block entry. MOVS/STOS/LODS define NO flags, so a
    /// call-out that published a settled word where the interpreter kept a descriptor would be
    /// visible only through `eflags()` -- which `compare_state` reads.
    fn pending(mut self) -> Self {
        self.live_pending = true;
        self
    }
}

/// Compile `program` on the native role, warm the same decode lines on the interpreter role, and
/// seed both identically.
fn build(program: &[u8], slots: u8, seed: Seed) -> Roles {
    let mut memory = string_memory();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + program.len()].copy_from_slice(program);

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
        cpu.set_fast_map_enabled_for_test(true);
        for offset in 0..program.len() as u32 {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            cpu.begin_instruction();
            let _ = cpu.fetch_decoded(bus, linear);
        }
        for page in (0..0x5000u32).step_by(0x1000) {
            map_page(cpu, bus, page);
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
            panic!("structurally rejected: the string opcode is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the string opcode really ran through the call-out"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_esi(SRC);
        cpu.registers.set_edi(DST);
        // A non-REP string primitive must not touch ECX at all. Poisoned so that a helper that
        // reached the REP path would be visible in the register compare.
        cpu.registers.set_ecx(0xdead_c04e);
        cpu.registers.set_eax(seed.eax);
        cpu.registers.set_ebp(0xdead_be5e);
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

fn compare_state(roles: &mut Roles, context: &str) {
    // Settle BOTH before the register comparison: `Registers.eflags` is the RAW field and two CPUs
    // at the same architectural state are free to hold different (raw, descriptor) representations
    // of it. That is exactly what happens here, because the helper publishes a settled word.
    roles.native.materialize_flags();
    roles.interp.materialize_flags();
    assert_eq!(
        crate::tests::settled_registers(&roles.native),
        crate::tests::settled_registers(&roles.interp),
        "{context}: registers or EIP"
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
    // The REMAINDER as well as the quotient, and this is not belt-and-braces: `level_timing` at
    // 586 is 1/12, so a whole string primitive's four raw clocks scale to ZERO elapsed clocks and
    // land entirely in this carry. Both roles start it at 0. Without this line the mutation that
    // deletes the call-out's runtime clock add SURVIVES every three-and-four-slot row in this
    // file -- which is what the first run of the mutation table measured, and the reason the
    // deletion was caught by one longer block by luck rather than by design.
    assert_eq!(
        roles.native.timing_rem, roles.interp.timing_rem,
        "{context}: scaled-clock remainder"
    );
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    // The WHOLE array: a string primitive writes at most one element and a window would be the
    // wrong shape to see a stray store.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// Enter the block, step the interpreter role the same number of instructions, and compare.
///
/// Returns the native role's `InterpretOne` execution/resync counts for the row under test, so a
/// caller can also assert WHICH row was charged.
fn run_and_compare(roles: &mut Roles, context: &str) {
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .expect("the fixture block must not stop the machine"),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        u64::from(roles.slots),
        "{context}: every slot must retire natively, so the call-out really resumed"
    );
    for _ in 0..roles.slots {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(roles, context);
}

/// One row's census counts out of a snapshot, by label.
fn row_counts(cpu: &CpuGsw, label: &str) -> crate::InterpretOneRowCounts {
    let snapshot = cpu.direct_stall_snapshot();
    *snapshot
        .callout_interpret_one_rows
        .iter()
        .find(|counts| counts.row == label)
        .unwrap_or_else(|| panic!("no census row labelled {label}"))
}

// =============================================================================================
// T1: the conversion row, at SLOT 0
// =============================================================================================

/// The anti-vacuity gate for the whole slice, in the position production takes.
///
/// With the gate OFF a `lodsd` at slot 0 stops the walk at slot 0, which cannot form a block, so
/// the outcome is a `StructuralReject` and the harness reports `None` -- verbatim the mechanism
/// that produces nascar's 40.7 M `rejected` class. With it ON the block covers all four slots and
/// carries exactly one `InterpretOne` slot.
///
/// SLOT 0 is deliberate and it is 100% of the production shape: the barred instruction becomes the
/// ENTRY of the next compile unit, so a fixture that only tested mid-block would certify the
/// emitter and none of the population.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_lodsd_at_slot_zero_compiles_only_with_the_gate() {
    for position in POSITIONS {
        let (program, slots) = position.program(&[0xad]);
        for on in [false, true] {
            let _arm = force_generic_callout(on);
            let (compiled, _) = compile_program(&program, false);
            if on {
                assert_eq!(
                    compiled.slots,
                    Some(slots),
                    "{}: lodsd must join the block on the ON arm",
                    position.label()
                );
                assert_eq!(
                    compiled.interpret_one_slots,
                    1,
                    "{}: lodsd must be an InterpretOne slot, not a lowering",
                    position.label()
                );
            } else {
                assert_eq!(
                    compiled.slots,
                    None,
                    "{}: lodsd must still end its block on the OFF arm",
                    position.label()
                );
            }
        }
    }
}

/// Every one of the ten opcodes joins the block on the ON arm and none of them does on the OFF
/// arm, with the CONTROL row compiling on both so the refusal half cannot pass vacuously.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn every_string_opcode_is_admitted_by_the_gate_and_by_nothing_else() {
    for on in [false, true] {
        let _arm = force_generic_callout(on);
        assert_eq!(
            compile_body(&CONTROL).slots,
            Some(3),
            "control: mov eax,ecx must compile on the {on} arm"
        );
        for (label, bytes, _) in STRING_ROWS {
            let compiled = compile_body(bytes);
            if on {
                assert_eq!(
                    compiled.slots,
                    Some(3),
                    "{label} must join the block on the ON arm"
                );
                assert_eq!(compiled.interpret_one_slots, 1, "{label} slot count");
            } else {
                assert_eq!(
                    compiled.slots, None,
                    "{label} must stay a barrier on the OFF arm"
                );
            }
        }
    }
}

// =============================================================================================
// T2: the deny rows, each pinned at ITS OWN census arm
// =============================================================================================

/// With the gate ON, every neighbour of the admitted set stays a barrier -- and stays a barrier in
/// the census arm the CODE puts it in, not in one arm asserted for all of them.
///
/// The arms differ and the difference is the point (review round 2, R2-m2). Each expectation below
/// is READ OFF THE CODE, opcode by opcode, rather than asserted once for the set:
///
/// * **`prefix_unsupported`** -- `REP MOVSD`, `REPNE SCASD`. `prefixes_supported_for` refuses the
///   prefix in the compile walk, which is ABOVE `classify`, so a REP string never reaches the arms
///   at all. It is a different census row from the unprefixed one and a slice that read the two
///   together would double-count its own population.
/// * **`non_continuable`** -- `0x6C`-`0x6F` INS/OUTS and `0xC4`/`0xC5` LES/LDS. `route_group`
///   (decode.rs) puts INS/OUTS in `DecodeGroup::Misc` and LES/LDS in `DecodeGroup::SystemSeg`;
///   `block_straight_line` names neither, and `block_continuable`'s `Misc` arm admits only
///   `0xa8`/`0xa9`. So the walk refuses both one test EARLIER than `classify`, in the same
///   condition as the prefix refusal and with prefix winning the tie.
/// * **`hard_boundary`** -- `0x9D` POPFD (`DecodeGroup::Stack`) and `0xE2` LOOP
///   (`DecodeGroup::Branch`). Both groups ARE straight-line, so both are continuable, both reach
///   `classify`, and both are ordinary opcode-coverage refusals -- the same arm the ten admitted
///   opcodes leave when the gate is off.
///
///   This fixture compiles at `d = true` (`Position::MidBlock.program` below), so `0x9D` decodes
///   at `OperandSize::Dword` here -- POPFD, not POPF. POPFD still has no `classify` arm at any
///   width and stays a barrier, unmeasured and out of scope (N2). The WORD form left this deny
///   set on N2: it is now an `InterpretOne` call-out (`InterpretOneRow::Popf`,
///   `cpu_jit_interpret_one_test.rs`), and this generic-callout gate has no opinion on it one way
///   or the other -- the row's admission lives in `classify`'s Word allowlist, upstream of the
///   knob this fixture is about.
///
/// The first run of this fixture had LES/LDS pinned at `hard_boundary` and went red on exactly
/// that row. The expectation was corrected against `route_group`, not against the failure message.
///
/// The set is the one an adversarial reviewer attacks first: the port-string hole this design
/// found (`0x6C`-`0x6F`), the far-pointer segment loads that break both the interrupt-shadow latch
/// and the four-access budget proof, POPF's IOPL hazard, and a control transfer.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_denied_neighbours_stay_barriers_in_their_own_census_arm() {
    // (label, bytes, opcode as the census keys it, the arm the code puts it in)
    let denied: [(&str, &[u8], u16, &str); 10] = [
        // REP/REPNE: refused by the prefix gate, a DIFFERENT arm from the admitted rows'.
        ("rep movsd", &[0xf3, 0xa5], 0xa5, "prefix_unsupported"),
        ("repne scasd", &[0xf2, 0xaf], 0xaf, "prefix_unsupported"),
        // The port-string hole. Same `StringOp` family, port I/O, no TSS probe, no privilege gate.
        ("insb", &[0x6c], 0x6c, "non_continuable"),
        ("insd", &[0x6d], 0x6d, "non_continuable"),
        ("outsb", &[0x6e], 0x6e, "non_continuable"),
        ("outsd", &[0x6f], 0x6f, "non_continuable"),
        // The far-pointer segment loads. `mod=00, reg=0, rm=1` is `les eax,[ecx]`.
        ("les eax,[ecx]", &[0xc4, 0x01], 0xc4, "non_continuable"),
        ("lds eax,[ecx]", &[0xc5, 0x01], 0xc5, "non_continuable"),
        // POPFD (Dword; the Word form is an InterpretOne call-out since N2, tested in
        // cpu_jit_interpret_one_test.rs, not here): one stack read, no transfer, not privileged
        // -- and it writes IOPL.
        ("popfd (dword only)", &[0x9d], 0x9d, "hard_boundary"),
        // A control transfer. R1 refuses resume for every one of them, forever -- so LOOP can
        // never be a CALL-OUT, which is this fixture's claim. It stopped being a BARRIER on
        // 2026-08-30: `IZARRAVM_LOOP_ROWS` (default ON) lowers it as a native TERMINAL through
        // `DirectKind::Loop`, a different mechanism from the one this gate guards. The arm is
        // forced OFF below so this row keeps asserting what it was written to assert -- that the
        // STRING gate does not admit it -- rather than silently testing the other slice's knob.
        ("loop", &[0xe2, 0x00], 0xe2, "hard_boundary"),
    ];
    let _arm = force_generic_callout(true);
    jit::direct::set_loop_rows_for_test(Some(false));
    assert_eq!(
        compile_body(&CONTROL).slots,
        Some(3),
        "control: mov eax,ecx must compile with the gate ON"
    );
    for (label, bytes, opcode, expected_arm) in denied {
        assert_eq!(
            compile_body(bytes).slots,
            None,
            "{label} must stay a barrier with the gate ON"
        );
        let (program, _) = Position::MidBlock.program(bytes);
        let (_, cpu) = compile_program(&program, true);
        let row = cpu
            .direct_barrier_census_snapshot()
            .expect("enabled census snapshot")
            .rows
            .into_iter()
            .find(|row| row.opcode == opcode)
            .unwrap_or_else(|| panic!("{label} must record a census row"));
        assert_eq!(
            row.stop_reason, expected_arm,
            "{label} stops in the {expected_arm} arm; a fixture that asserted one arm for the \
             whole deny set would have been rewritten to match whatever the code did"
        );
    }
}

/// The 0x0F escape half of the far-pointer deny, kept separate because a two-byte opcode is a
/// different key and the single-byte sweep above cannot see it.
///
/// `0x0F B2` LSS is the one that ARMS the interrupt shadow. Carried as a non-arming
/// `InterpretOne` row it would leave `cpu.interrupt_shadow` set, R3 would resync, and the boundary
/// would then clear a shadow an EARLIER `Sti` slot latched -- delivery one instruction early,
/// which the arming-count `debug_assert` at the boundary cannot see.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_two_byte_far_pointer_loads_stay_barriers() {
    let _arm = force_generic_callout(true);
    for (label, bytes) in [
        ("lss eax,[ecx]", &[0x0fu8, 0xb2, 0x01][..]),
        ("lfs eax,[ecx]", &[0x0f, 0xb4, 0x01]),
        ("lgs eax,[ecx]", &[0x0f, 0xb5, 0x01]),
    ] {
        assert_eq!(
            compile_body(bytes).slots,
            None,
            "{label} must stay a barrier with the gate ON"
        );
    }
}

// =============================================================================================
// T3: why the control transfers can never be call-outs
// =============================================================================================

/// R1, the clause that makes LOOP/LOOPE structurally unreachable for this mechanism, asserted
/// against every one of the four new rows rather than against the row the clause was written for.
///
/// The block body holds EIP at the block's ENTRY value throughout and only exits advance it, so
/// the helper compares the post-step EIP against `start + len`. A TAKEN transfer always leaves a
/// different value, resyncs on every execution, and the governor demotes it within eight. That is
/// 9.2 M of nascar's rejected class permanently out of reach of any allowlist widening -- the
/// LOOP rows need a `DirectKind` lowering, not a call-out -- and this row is why.
#[test]
fn a_transferred_eip_can_never_resume_on_any_string_row() {
    for (label, row) in ROWS {
        let mut cpu = flat_cpu();
        cpu.registers.eflags = 0x202;
        cpu.set_eip(ENTRY + 5);
        let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
        assert!(
            snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: the control leg must resume, or every refusal below is vacuous"
        );
        // What a taken LOOP leaves behind: an EIP that is not `start + len`.
        cpu.set_eip(ENTRY - 0x20);
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: a moved EIP must resync -- this is why LOOP can never take a call-out slot"
        );
    }
}

// =============================================================================================
// T4: the resume matrix, per new row
// =============================================================================================

/// Each of the four rows resumes on the clean path and resyncs on each machine event it can meet.
///
/// The list is exhaustive over what these rows can actually reach, which is the argument that the
/// sparse-resync failure mode is not available to them: a segment record moved (R2), a paging
/// generation moved (R4), TF set, a halt, or a paused REP. NONE of those is a property of the
/// instruction's operand path, so a string slot cannot have the "over budget on one operand path
/// in twenty" shape that permanently admits a bad slot after its governor window freezes.
///
/// R2 is the row that matters most here. `may_write_segment()` is false for all four, so R2
/// compares ALL SIX records -- the strict side -- and a moved DS record resyncs. A row that leaked
/// into `may_write_segment` would relax that compare to the mask and the moved-DS leg would
/// resume, which is the mutation this row exists for.
#[test]
fn the_resume_matrix_holds_for_every_string_row() {
    for (label, row) in ROWS {
        assert!(
            !row.may_write_segment(),
            "{label} writes no segment register, so R2 must stay at its strict reading"
        );
        assert!(
            !row.arms_interrupt_shadow(),
            "{label} does not arm the interrupt shadow"
        );
        assert!(
            !row.takes_interrupt_enable_edge(),
            "{label} does not write IF"
        );

        let clean = || {
            let mut cpu = flat_cpu();
            cpu.registers.eflags = 0x202;
            cpu.set_eip(ENTRY + 5);
            let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
            (cpu, snapshot)
        };
        let (cpu, snapshot) = clean();
        assert!(
            snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: the clean path must resume"
        );

        // R2: the guest moved a record the block baked. DS is the string ops' own default segment.
        let (mut cpu, snapshot) = clean();
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.base = ds.base.wrapping_add(0x10);
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: a moved DS record must resync"
        );

        // R2 again, through ES, which STOS/MOVS/SCAS address through and cannot override.
        let (mut cpu, snapshot) = clean();
        let mut es = cpu.registers.segment(SegmentIndex::Es);
        es.base = es.base.wrapping_add(0x10);
        cpu.registers.set_segment(SegmentIndex::Es, es);
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: a moved ES record must resync"
        );

        // R4: the paging generation moved under the block.
        let (mut cpu, _) = clean();
        cpu.set_data_write_mapping_epoch_for_test(7);
        let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
        cpu.set_data_write_mapping_epoch_for_test(8);
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: a moved mapping epoch must resync"
        );

        // R3: the trap flag. A block cannot produce the instruction boundary single-step wants.
        let (mut cpu, snapshot) = clean();
        cpu.registers.eflags |= FLAG_TF;
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: TF must resync"
        );

        // R7 and R8: a halted step and a paused REP are the run loop's own state.
        let (mut cpu, snapshot) = clean();
        cpu.halted = true;
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: a halted step must resync"
        );
        let (mut cpu, snapshot) = clean();
        cpu.rep_resume_active = true;
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{label}: a paused REP must resync"
        );
    }
}

/// R5 end to end, on the one row that can reach it: a STOS into the running block's OWN page.
///
/// The store commits through the interpreter's ordinary path, `note_code_write_inner` records it
/// instead of invalidating (the block's native frame is still on the host stack), R5 sees a
/// non-empty deferred list and the block RESYNCS, and `run_direct_block` drains the list on the way
/// out -- which is what kills the block. The assertions are ordered accordingly: the block ran and
/// returned, and only afterwards is it gone.
///
/// This is one of exactly two resyncs the design expects these rows to take at all, and it is
/// correct behaviour rather than a cost.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_stos_into_the_blocks_own_page_resyncs_and_retires_it() {
    let _arm = force_generic_callout(true);
    // `stosd` mid-block, with EDI pointed at the block's own trailing filler.
    let (program, slots) = Position::MidBlock.program(&[0xab]);
    let mut roles = build(&program, slots, Seed::new());
    for cpu in [&mut roles.native, &mut roles.interp] {
        cpu.registers.set_edi(ENTRY + 2 + 1);
    }
    let live_before = roles.native.jit_direct.len();
    assert_eq!(live_before, 1, "the fixture must have installed one block");

    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .expect("a self-modifying store must not stop the machine"),
        "the block must run"
    );
    let counts = row_counts(&roles.native, "0xaa_ab_stos");
    assert_eq!(
        (counts.executed, counts.resync, counts.resync_fault),
        (1, 1, 0),
        "the store row must record its own execution AND its own resync"
    );
    assert_eq!(
        roles.native.jit_direct.len(),
        0,
        "the deferred code write must have retired the block on the way out"
    );
}

// =============================================================================================
// T5 / T5b: DF currency
// =============================================================================================

/// T5: `cld; lodsd` and `std; lodsd` in ONE block, in both slot positions.
///
/// DF is the class's one implicit flag INPUT and it is the one proof obligation the design could
/// not discharge with new code: `string_step` reads DF out of `registers.eflags`, and
/// `materialize_flags` settles only the six arithmetic flags, so the currency of that location is
/// provided by `emit_direction_flag`, which writes DF to BOTH the RBP shadow and memory `eflags`
/// on every CLD/STD slot. Deleting the memory half is the mutation this row kills.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_string_slot_reads_the_direction_flag_from_a_cld_std_slot() {
    let _arm = force_generic_callout(true);
    for (label, df) in [("cld", 0xfcu8), ("std", 0xfd)] {
        // `<df>; lodsd; FILL; hlt` -- the DF writer at slot 0, the string slot behind it.
        let mut program = vec![df, 0xad];
        program.extend_from_slice(&FILL);
        program.push(0xf4);
        let mut roles = build(&program, 3, Seed::new());
        run_and_compare(&mut roles, &format!("{label}; lodsd"));
        // Non-vacuity: the two polarities must move ESI in OPPOSITE directions, or the row would
        // pass against an emitter that ignored DF entirely.
        let esi = roles.native.registers.esi();
        if df == 0xfc {
            assert_eq!(esi, SRC + 4, "cld; lodsd must walk FORWARD");
        } else {
            assert_eq!(esi, SRC - 4, "std; lodsd must walk BACKWARD");
        }
    }
}

/// T5b (review round 2, R2-m3): the leg that is load-bearing for the PRODUCTION population.
///
/// The converted spans nascar measures contain no `cld`/`std` at all -- DF was set by some earlier
/// instruction, outside this block. So the block has NO `DirectionFlag` slot to keep the two
/// copies in step, and the invariant that has to hold instead is **RBP.DF == memory.DF at every
/// point inside a block**: RBP is loaded at entry from the materialized eflags, DF included.
///
/// The block also carries a WHOLESALE RBP PUBLISH ahead of the string slot (`adc eax,ecx`, which
/// does `store_r32_disp32(R15, eflags_offset(), RBP)`), because that publish is what would
/// resurrect a stale DF if the entry load had dropped it. Mutation: mask DF out of RBP's entry
/// load and the publish writes DF=0 back over the guest's DF=1, the string slot walks forward, and
/// this row goes red where T5 stays green.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_string_slot_reads_a_direction_flag_set_outside_the_block() {
    let _arm = force_generic_callout(true);
    for position in POSITIONS {
        // `adc eax,ecx` is the wholesale publish; the string slot follows it.
        let mut body = vec![0x11u8, 0xc8];
        body.extend_from_slice(&[0xad]);
        let (program, slots) = position.program(&body);
        // `program` counts the two-instruction body as one slot, so the block is one longer.
        let slots = slots + 1;
        let mut roles = build(&program, slots, Seed::new().direction_down());
        run_and_compare(
            &mut roles,
            &format!("{}: DF set outside the block", position.label()),
        );
        assert_eq!(
            roles.native.registers.esi(),
            SRC - 4,
            "{}: the entry DF must survive the wholesale publish and walk the string slot \
             BACKWARD",
            position.label()
        );
    }
}

// =============================================================================================
// T6: the budget fold
// =============================================================================================

/// The OFF leg is MAIN, and this is the fixture that says so.
///
/// `INTERPRET_ONE_MAX_CORE_CLOCKS` and `MAX_CALL_OUT_CORE_CLOCKS` are `const` and no knob can gate
/// them. They feed `compute_iteration_upper`, whose result is the chain quota's DIVISOR, so a row
/// whose charge raised either fold would change how much native work fits in EVERY block on EVERY
/// fixture -- with the knob OFF. The string arm charges 4 against an existing 7, so neither moves.
///
/// The property pinned is "the OFF leg is main", NOT "7 is the correct charge". The assertion
/// message says so, because a future row that legitimately raises the fold must read this failure
/// as "re-ladder" rather than as a stale constant to update.
#[test]
fn interpret_one_fold_is_unmoved_by_the_string_rows() {
    assert_eq!(
        crate::INTERPRET_ONE_MAX_CORE_CLOCKS,
        7,
        "the OFF leg is no longer main; re-ladder. INTERPRET_ONE_MAX_CORE_CLOCKS is the chain \
         quota's divisor through compute_iteration_upper and is NOT behind any knob, so moving it \
         changes the admission of every block on every fixture with IZARRAVM_GENERIC_CALLOUT off. \
         Do not update this number: measure the OFF leg against main first"
    );
    assert_eq!(
        crate::MAX_CALL_OUT_CORE_CLOCKS,
        18,
        "the OFF leg is no longer main; re-ladder. MAX_CALL_OUT_CORE_CLOCKS is what prices a \
         call-out slot when the budget cannot tell the helpers apart, and it is not behind any \
         knob. Do not update this number: measure the OFF leg against main first"
    );
    assert_eq!(
        crate::INTERPRET_ONE_MAX_DATA_ACCESSES,
        4,
        "the OFF leg is no longer main; re-ladder. INTERPRET_ONE_MAX_DATA_ACCESSES multiplies the \
         call-out bus term in both budget bounds and is not behind any knob"
    );
    // The string rows' own terms, stated so that a change to either is a change to a number this
    // file names rather than one it infers.
    assert_eq!(
        crate::STRING_CORE_CLOCKS,
        4,
        "execute_string_decoded has ONE exit, Ok(clocks(4)), for all ten opcodes"
    );
    // A CONST assertion, in the style of
    // `max_x87_block_core_clocks_dominates_every_shape_in_the_metadata_table`: both sides are
    // `const`, so this is a compile-time bar rather than a runtime one and a violating edit does
    // not build at all.
    const {
        assert!(
            crate::STRING_CORE_CLOCKS < crate::INTERPRET_ONE_MAX_CORE_CLOCKS,
            "the string arm must stay DOMINATED by the fold, which is what lets it ship behind a \
             knob at all: a term that raised the fold would change the chain quota for every \
             block on every fixture with the knob OFF"
        )
    };
}

// =============================================================================================
// T7: row exhaustiveness
// =============================================================================================

/// The four new rows are in `ALL`, at their discriminants, with distinct labels, and none of them
/// claims a relaxation it cannot use.
///
/// `interpret_one_row_labels_cover_every_variant` derives its length from `COUNT` and so
/// self-adjusts from 12 to 16 -- VERIFIED rather than assumed, which is what this row is for: it
/// names the new count explicitly, so a variant added without an `ALL` entry fails here as well as
/// there.
///
/// 18 rather than 16 as of N2: `Pushf` and `Popf` joined afterwards, and this count is a floor
/// for THIS slice's four rows, not a ceiling on the enum -- it moves again the next time a row is
/// added, and this comment moves with it.
#[test]
fn the_four_string_rows_are_on_the_allowlist() {
    assert_eq!(
        jit::direct::InterpretOneRow::COUNT,
        18,
        "twelve rows before this slice plus the four string families plus N2's two"
    );
    for (label, row) in ROWS {
        assert_eq!(row.label(), label);
        assert_eq!(
            jit::direct::InterpretOneRow::ALL[row.index()],
            row,
            "{label} is not at its discriminant's position in ALL"
        );
    }
    let labels: Vec<&'static str> = jit::direct::InterpretOneRow::ALL
        .iter()
        .map(|row| row.label())
        .collect();
    let mut sorted = labels.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), labels.len(), "two rows share a census label");
}

// =============================================================================================
// T8: the execution-level differential, BOTH positions
// =============================================================================================

/// Every one of the ten opcodes, at slot 0 and mid-block, against the interpreter.
///
/// The comparison is the whole of it: registers, EIP, EFLAGS, the WHOLE of guest RAM, core clocks
/// and bus clocks. The clock columns are what catch a call-out that forgot to add the helper's
/// returned `core_clocks` -- deleting that add fails by exactly `STRING_CORE_CLOCKS` per slot
/// execution -- and the RAM compare is what catches a MOVS that moved the wrong width or walked
/// the wrong way.
///
/// Both DF polarities and a live lazy descriptor on every row. MOVS/STOS/LODS define NO flags, so
/// a descriptor that entered the block must leave it intact; CMPS/SCAS define the ordinary
/// arithmetic set and must destroy it. One sweep covers both claims because `eflags()` settles
/// whichever representation each role holds.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn every_string_opcode_matches_the_interpreter_in_both_positions() {
    let _arm = force_generic_callout(true);
    for position in POSITIONS {
        for (label, bytes, _) in STRING_ROWS {
            for (df, seed) in [
                ("df=0", Seed::new()),
                ("df=1", Seed::new().direction_down()),
                ("df=0 pending", Seed::new().pending()),
                ("df=1 pending", Seed::new().direction_down().pending()),
            ] {
                let (program, slots) = position.program(bytes);
                let mut roles = build(&program, slots, seed);
                run_and_compare(&mut roles, &format!("{} {label} {df}", position.label()));
            }
        }
    }
}

/// A MOVS whose source and destination are the SAME dword, and one whose destination is the byte
/// before the source, so a lowering that read after it wrote would produce a different RAM image.
///
/// The overlapping shape is what a hand-written emitter gets wrong; the helper runs the
/// interpreter's own arm and so cannot, which is exactly the argument for a call-out over a
/// lowering here. Pinned anyway, because "cannot by construction" is the claim a fixture is for.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn overlapping_movs_matches_the_interpreter() {
    let _arm = force_generic_callout(true);
    for position in POSITIONS {
        for (label, delta) in [
            ("same address", 0i32),
            ("dst one byte below", -1),
            ("dst one byte above", 1),
        ] {
            for seed in [Seed::new(), Seed::new().direction_down()] {
                let (program, slots) = position.program(&[0xa5]);
                let mut roles = build(&program, slots, seed);
                for cpu in [&mut roles.native, &mut roles.interp] {
                    cpu.registers.set_edi(SRC.wrapping_add_signed(delta));
                }
                run_and_compare(
                    &mut roles,
                    &format!("{} overlapping movsd, {label}", position.label()),
                );
            }
        }
    }
}

// =============================================================================================
// The per-row census
// =============================================================================================

/// An execution lands on the row that was ADMITTED, and on no other.
///
/// The whole point of four rows rather than one is that a family the governor gives back cannot
/// hide behind three that it does not, so the assertion is not "the right row moved" but "the
/// right row moved AND every other row is zero". SCAS is in the sweep specifically because it
/// shares `StringCompare` with CMPS: a fifth row invented for it, or CMPS folded into LODS, both
/// fail here.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn each_string_execution_is_attributed_to_its_own_row() {
    let _arm = force_generic_callout(true);
    for (label, bytes, expected_row) in STRING_ROWS {
        let (program, slots) = Position::MidBlock.program(bytes);
        let mut roles = build(&program, slots, Seed::new());
        run_and_compare(&mut roles, label);
        let counts = row_counts(&roles.native, expected_row);
        assert_eq!(
            (
                counts.executed,
                counts.resync,
                counts.resync_fault,
                counts.demoted
            ),
            (1, 0, 0, 0),
            "{label} must charge {expected_row}, resume, and not be given back"
        );
        let total: u64 = roles
            .native
            .direct_stall_snapshot()
            .callout_interpret_one_rows
            .iter()
            .map(|counts| counts.executed)
            .sum();
        assert_eq!(
            total, 1,
            "{label}'s execution must not have landed on a second row as well"
        );
    }
}

/// The census rows the campaign grades against, from both ends of the gate.
///
/// With the knob OFF a `0xAD` in a 32-bit segment produces a `hard_boundary` row with
/// `operand_size: dword` and `prefix_mask: 0` -- verbatim the nascar-586 row this slice claims.
/// With it ON no row for that shape exists, because the walk no longer stops there.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_census_rows_disappear_with_the_gate() {
    for (label, bytes, _) in STRING_ROWS {
        let opcode = u16::from(bytes[0]);
        for on in [false, true] {
            let _arm = force_generic_callout(on);
            let (program, _) = Position::MidBlock.program(bytes);
            let (_, cpu) = compile_program(&program, true);
            let row = cpu
                .direct_barrier_census_snapshot()
                .expect("enabled census snapshot")
                .rows
                .into_iter()
                .find(|row| row.opcode == opcode);
            if on {
                assert!(
                    row.is_none(),
                    "{label} must have NO census row on the ON arm; it is a call-out"
                );
            } else {
                let row = row
                    .unwrap_or_else(|| panic!("{label} must record a census row on the OFF arm"));
                assert_eq!(row.prefix_mask, 0, "{label} prefix mask");
                assert_eq!(row.operand_size, "dword", "{label} operand size");
                assert_eq!(
                    row.stop_reason, "hard_boundary",
                    "{label}: the refusal must land in the SAME census arm the nascar rows were \
                     ranked in"
                );
            }
        }
    }
}

// =============================================================================================
// T9: the gate admits exactly ten opcodes
// =============================================================================================

/// A single-byte sweep over the whole opcode space with the gate OFF and ON. The set of opcodes
/// whose outcome CHANGED must be exactly `0xA4`-`0xA7` and `0xAA`-`0xAF`.
///
/// A sweep rather than a list of neighbours, because the failure this catches is a widened arm
/// reaching something nobody thought to name -- `0x6C..=0x6F` is the one this design found, and
/// the whole point is that the next one will not be predicted either. Single-byte space suffices
/// for this slice: both classify arms are single-byte ranges below the `u8::try_from` truncation,
/// so no two-byte opcode can reach them.
///
/// Every opcode is compiled mid-block with a zero-filled tail, which is the coverage matrix's own
/// idiom: the decoder consumes whatever displacement or immediate bytes the form wants, and zero
/// keeps relative branch targets at the fall-through.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_gate_admits_exactly_the_ten_string_opcodes() {
    let mut moved: Vec<u8> = Vec::new();
    for opcode in 0x00u8..=0xff {
        let mut bytes = vec![opcode];
        // A zero-filled tail long enough for any single-byte form's ModRM, displacement and
        // immediate. `mod=00, rm=000` is `[eax]` at 32-bit address size and needs no SIB.
        bytes.extend_from_slice(&[0x00; 6]);
        let off = {
            let _arm = force_generic_callout(false);
            compile_body(&bytes).slots
        };
        let on = {
            let _arm = force_generic_callout(true);
            compile_body(&bytes).slots
        };
        if off != on {
            moved.push(opcode);
        }
    }
    let expected: Vec<u8> = (0xa4u8..=0xa7).chain(0xaa..=0xaf).collect();
    assert_eq!(
        moved, expected,
        "the gate must move exactly the four string families and nothing else; \
         0x6C-0x6F INS/OUTS in this list would be the port-string hole"
    );
}

// =============================================================================================
// T10: the knob's spelling table
// =============================================================================================

/// Every accepted spelling, and the panic on a typo.
///
/// The panic is the load-bearing half: a mistyped ladder leg that fell through to the default
/// would run the arm it did not name and be read as "the rows I asked for changed nothing", which
/// is the one wrong conclusion an A/B exists to avoid.
///
/// **`""` tracks the DEFAULT rather than restating it.** The 2026-08-29 flip moved that arm from
/// OFF to ON, and the assertion reads `generic_callout_default_arm_for_test()` so it moved with it
/// instead of going stale -- which is also what keeps the row honest the next time the default
/// moves in either direction.
#[test]
fn generic_callout_spelling_table_names_every_arm() {
    use jit::direct::parse_generic_callout_arm_for_test as parse;
    let default = jit::direct::generic_callout_default_arm_for_test();
    assert_eq!(
        parse(Err(std::env::VarError::NotPresent)),
        default,
        "unset must name the shipped default arm"
    );
    assert_eq!(
        parse(Ok(String::new())),
        default,
        "the EMPTY string names the same arm as unset, not the OFF arm; nulling the variable is          not unsetting it, and this is the assertion that moves when the default does"
    );
    for spelling in ["0", "off", "OFF", " off ", "Off"] {
        assert!(
            !parse(Ok(spelling.to_string())),
            "{spelling:?} must name the OFF arm -- the escape and the A/B base, which the flip              did NOT move"
        );
    }
    for spelling in ["1", "on", "ON", " On "] {
        assert!(
            parse(Ok(spelling.to_string())),
            "{spelling:?} must name the ON arm"
        );
    }
    for typo in ["yes", "true", "enabled", "2", "of"] {
        let panicked = std::panic::catch_unwind(|| parse(Ok(typo.to_string()))).is_err();
        assert!(
            panicked,
            "{typo:?} names no arm and must PANIC rather than silently run the default"
        );
    }
}

/// The shipped default arm, read through the ambient knob rather than through the override.
///
/// This is the ONE row in the file that does not force its arm, and it is supposed to be: it is
/// what would catch `generic_callout_enabled`'s env path disagreeing with the parse table while
/// every forcing fixture kept passing.
///
/// **ON since the 2026-08-29 ladder** -- nascar-586 min-wall -2.9% (56.16 s -> 54.55 s), entries
/// -10%, unbound exits -15.4 M, with duke3d-586 holding its expected-near-zero control at +0.16%
/// wall and IDENTICAL guest instruction counts. Asserted against the named constant rather than
/// against a literal, so this row states the default instead of duplicating it.
#[test]
fn generic_callout_ships_the_default_arm() {
    assert!(
        std::env::var("IZARRAVM_GENERIC_CALLOUT").is_err(),
        "this row reads the ambient knob, so the harness must not have it set"
    );
    assert_eq!(
        jit::direct::generic_callout_enabled(),
        jit::direct::generic_callout_default_arm_for_test(),
        "the ambient reading must agree with the parse table's default arm"
    );
    assert!(
        jit::direct::generic_callout_default_arm_for_test(),
        "the slice ships default ON since the 2026-08-29 ladder"
    );
}
