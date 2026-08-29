// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `0xD1 /2` RCL and `0xD1 /3` RCR at Dword, REGISTER form, lowered natively behind
//! `IZARRAVM_RCL_ROWS` (default OFF).
//!
//! # The row, from the census that ranked it
//!
//! nascar-586 at main `f777010c`, `IZARRAVM_DIRECT_BARRIER_CENSUS=1`, plain release build. The
//! `rejected` class is 40,666,390 unbound exits and `0xD1 /2` RCL r32,1 register is its
//! **8,704,380**-runtime-hit head, third behind the two string primitives, with a native prefix of
//! 10 and a suffix of 4. It is the other half of the `shl eax,1` / `rcl edx,1` idiom that shifts a
//! 64-bit quantity through two 32-bit registers, so it sits directly behind rows this backend has
//! lowered since the group-2 slices -- which is why the exits pile onto it.
//!
//! # The flag contract is the whole of this slice
//!
//! `shift_rotate` (`core.rs`) takes the `matches!(op, 4..=7)` FALSE branch for every rotate:
//! `set_flag(FLAG_CF, cf)` unconditionally, and at count 1 ONLY `set_flag(FLAG_OF, top ^ cf)`.
//! **SF, ZF, PF and AF are untouched**, so a live lazy descriptor keeps its authority across the
//! rotate.
//!
//! The design's first revision cited `emit_carry_alu_preloaded` as the shape to copy, because the
//! code's own comment on the `0xc1 | 0xd1` arm did. That emitter does
//! `emit_capture_flags(ARITH_FLAGS)` and then publishes the whole arithmetic class -- right for
//! ADC/SBB and **wrong for a rotate by four flags**. The adversarial review blocked the design on
//! it (B4), and `a_live_descriptor_survives_a_lowered_rotate` is the fixture that would have
//! caught the shipped bug: `cmp`, then a lowered RCL, then `setz`/`sets`/`setp` reading flags the
//! interpreter preserves and that emitter would have overwritten. **The misleading comment is
//! corrected in the same commit**, because it is where the wrong idea came from.
//!
//! What IS worth taking from `emit_carry_alu_preloaded` is one line: `emit_load_host_flags`,
//! emitted BEFORE the host rotate, because CF is a rotate INPUT. Its two-arm branch on CF is not
//! needed and would not be sound here -- there is no CF=0 arithmetic variant of RCL with this flag
//! contract.
//!
//! # RCL/RCR are outside the L1 heat gate, and nothing was done to put them there
//!
//! `rotate_row_count_byte` is keyed on `(opcode, reg)` with arms `0xc0 reg == 4`, `0xc1 reg == 0`
//! and `0xd1 reg == 0`; `0xd1 reg == 2` falls to `_ => None`. The reason is NOT that `0xD1` has no
//! patchable count byte -- the `0xd1 reg == 0` arm returns `physical + len - 2`, the OPCODE byte,
//! precisely because for `0xD1` the count IS the opcode -- and stating it that way would tell a
//! future editor to delete a live arm. `the_heat_gate_keys_on_the_sub_opcode_as_well_as_the_opcode`
//! pins both halves in one fixture: over a count byte carrying a heat record, `0xD1 /0` ROL is
//! downgraded and `0xD1 /2` RCL is not.
//!
//! # Mutation record
//!
//! Applied BY HAND to the committed tree, run, observed, and restored with `git checkout --`.
//! Each was run against the whole `cpu_jit_rcl_rows_test` module; survivors were re-run against
//! the whole `izarravm-cpu` suite.
//!
//! | # | mutation | outcome |
//! |---|---|---|
//! | M1 | drop `emit_load_host_flags` from the `2 \| 3` arm of `emit_rotate_reg` | RED |
//! | M2 | move `emit_load_host_flags` to AFTER the host rotate | RED |
//! | M3 | `emit_rotate_reg`'s count-1 arm -> `emit_capture_flags(ARITH_FLAGS)` + eager publish (the wrong emitter's contract) | RED |
//! | M4 | capture `CF` only at count 1, dropping `OF` | RED |
//! | M5 | drop the `opcode == 0xd1` term from the classify guard | RED |
//! | M6 | drop the `OperandSize::Word` refusal for `2 \| 3` | RED |
//! | M7 | widen `rotate_row_count_byte` with a `0xd1 if reg == 2` arm | RED |
//! | M8 | `rcl_rows_enabled()`'s ENV path returns `true` | RED |
//! | M9 | `"" => false` -> `"" => true` in the parse table | RED |
//!
//! The recorded outcomes and the rows each killed are in the PR body; every one of the nine kills.

use super::*;

/// `mov edi,edi`: the filler slot that keeps the tested opcode off the block entry. EDI is the one
/// register neither the rotate, the `cmp` nor any consumer touches.
const FILL: [u8; 2] = [0x89, 0xff];

/// `cmp ebp, esi`, the descriptor producer. It defines CF as well as SF/ZF/PF/AF, so it is also
/// what feeds the rotate's CARRY-IN through the RBP shadow -- which is the half of this slice a
/// fixture without it could not reach.
const CMP: [u8; 2] = [0x39, 0xf5];

/// The five flag consumers, as `(condition, byte-register index)`. Between them they read every
/// flag a rotate defines AND every flag it must preserve. The destinations are chosen so that none
/// of them is the rotate's destination (EDX) or either `cmp` operand (EBP, ESI): AL and AH are one
/// register's two halves, CL and CH the other's, BL a third.
const CONSUMERS: [(u8, u8); 5] = [
    (0x2, 0), // setc al  -- CF, DEFINED by the rotate
    (0x0, 3), // seto bl  -- OF, defined at count 1 only
    (0x8, 1), // sets cl  -- SF, PRESERVED
    (0x4, 4), // setz ah  -- ZF, PRESERVED
    (0xa, 5), // setp ch  -- PF, PRESERVED
];

/// The rotate destination for every consumer-bearing row: EDX, which no consumer and neither `cmp`
/// operand names.
const ROTATE_DST: u8 = 2;

/// `(label, /digit)` for the two rows this slice adds and the two it shares an emitter with.
const ROTATES: [(&str, u8); 4] = [("rol", 0), ("ror", 1), ("rcl", 2), ("rcr", 3)];
/// The two this slice ADDS. `/3` RCR has no census row of its own and rides `/2` on the closure
/// rule -- one emitter, one interpreter branch, one flag contract at both counts.
const CARRY_ROTATES: [(&str, u8); 2] = [("rcl", 2), ("rcr", 3)];

/// `D1 /op` on a register destination, the whole of what this slice lowers.
fn d1_reg(op: u8, dst: u8) -> Vec<u8> {
    vec![0xd1, 0xc0 | (op << 3) | dst]
}

/// `C1 /op ib` -- the IMMEDIATE-count form, which this slice deliberately does NOT admit for the
/// carry rotates.
fn c1_reg(op: u8, dst: u8, count: u8) -> Vec<u8> {
    vec![0xc1, 0xc0 | (op << 3) | dst, count]
}

/// `D1 /op` on a MEMORY destination (`mod = 00`, `rm = 000` is `[eax]` at 32-bit address size).
fn d1_mem(op: u8) -> Vec<u8> {
    vec![0xd1, op << 3]
}

/// `D0 /op` and `C0 /op ib`, the BYTE forms, which stay out at every arm.
fn d0_reg(op: u8, dst: u8) -> Vec<u8> {
    vec![0xd0, 0xc0 | (op << 3) | dst]
}

fn c0_reg(op: u8, dst: u8, count: u8) -> Vec<u8> {
    vec![0xc0, 0xc0 | (op << 3) | dst, count]
}

/// `0F 9x /r`, register form, in a 32-bit segment.
fn consumer_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    for (condition, dst) in CONSUMERS {
        bytes.extend_from_slice(&[0x0f, 0x90 | condition, 0xc0 | dst]);
    }
    bytes
}

/// `mov eax,ecx`: the control row, which must compile on BOTH arms. Without it a refusal assertion
/// could pass because the harness refuses everything.
const CONTROL: [u8; 2] = [0x89, 0xc8];

/// A distinct byte at every address, so a stray write of any width shows up in the whole-RAM
/// compare rather than hiding behind a zero fill.
fn rotate_memory() -> Vec<u8> {
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

/// Restores every arm this file forces, on the way out of a fixture -- normally OR by panic. A
/// plain `set_*_for_test(Some(..))` LEAKS: the overrides are thread-local and the harness reuses
/// threads, so the next fixture on that thread would inherit an arm it never asked for.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_rcl_rows_for_test(None);
        jit::direct::set_rotate_rows_arm_for_test(None);
        jit::direct::set_count_lanes_for_test(None);
    }
}

/// Force the carry-rotate arm and PROVE the selection took. `IZARRAVM_ROTATE_ROWS` is pinned to
/// `On` alongside it, because `0xD1 /0` ROL rides that other axis and this file's neighbour rows
/// compare the two: a fixture that inherited the ambient rotate arm would be reading a different
/// cell of the matrix than it says.
#[must_use]
fn force_rcl_rows(on: bool) -> ArmOverride {
    jit::direct::set_rcl_rows_for_test(Some(on));
    jit::direct::set_rotate_rows_arm_for_test(Some(jit::direct::RotateRowsArm::On));
    assert_eq!(
        jit::direct::rcl_rows_enabled(),
        on,
        "the fixture override must decide the arm, not the ambient IZARRAVM_RCL_ROWS"
    );
    ArmOverride
}

// ---------------------------------------------------------------------------------------------
// The compile-only harness
// ---------------------------------------------------------------------------------------------

/// Compile `FILL / body / FILL / hlt` at `ENTRY` and report the span length, or `None` when the
/// walk refused it.
///
/// `word` prefixes the whole body with `0x66`, which is how a Dword-default segment reaches the
/// Word decode of a group-2 form. `heat_at` seeds one SMC heat record before compiling, which is
/// what the heat-gate row needs.
fn compile_span_full(body: &[u8], word: bool, heat_at: Option<u32>) -> Option<u8> {
    let mut code = FILL.to_vec();
    let body_at = ENTRY + code.len() as u32;
    if word {
        code.push(0x66);
    }
    code.extend_from_slice(body);
    code.extend_from_slice(&FILL);
    code.push(0xf4);

    let mut memory = rotate_memory();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_esp(STACK_TOP);
    // A resolvable operand address for the memory forms, so their refusal is the arm's and not the
    // fast map's.
    cpu.registers.set_eax(0x2000);
    for offset in 0..code.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x5000u32).step_by(0x1000) {
        map_page(&mut cpu, &mut bus, page);
    }
    if let Some(offset) = heat_at {
        cpu.sync_smc_heat();
        cpu.jit_direct.smc_heat.bump(body_at + offset, 1, 0);
    }
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    }
}

fn compile_span(body: &[u8]) -> Option<u8> {
    compile_span_full(body, false, None)
}

fn compile_span_word(body: &[u8]) -> Option<u8> {
    compile_span_full(body, true, None)
}

/// A three-slot block: the two fillers and the form under test.
const ADMITTED: Option<u8> = Some(3);
/// A barrier in the body slot stops the walk one slot in, which is shorter than the minimum
/// installable block, so the outcome is a `StructuralReject` and the harness reports `None`.
/// `None` also covers a `Retry`, which is why every fixture asserting it also asserts the CONTROL
/// row compiles in the same harness.
const REFUSED: Option<u8> = None;

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

/// The architectural state both roles start from. Every register's HIGH half is poisoned, so a
/// rotate that ran at the wrong width shows up in the register compare rather than hiding behind a
/// zero seed.
#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
}

impl Seed {
    fn new() -> Self {
        Self {
            gpr: std::array::from_fn(|i| 0xdead_be00 | (0xa0 + i as u32)),
            // Bit 1 is the reserved bit the interpreter ors on every flag write; a seed with it
            // clear produces a one-bit disagreement that says nothing about this slice.
            eflags: 0x202,
        }
    }

    fn reg(mut self, index: u8, value: u32) -> Self {
        self.gpr[usize::from(index)] = value;
        self
    }

    /// The rotate's CARRY-IN. Set through EFLAGS rather than through a preceding instruction, for
    /// the rows whose block has no descriptor producer in it.
    fn carry(mut self, set: bool) -> Self {
        if set {
            self.eflags |= crate::FLAG_CF;
        } else {
            self.eflags &= !crate::FLAG_CF;
        }
        self
    }

    fn flags(mut self, eflags: u32) -> Self {
        self.eflags = eflags | 0x2;
        self
    }
}

/// Compile `program` on the native role, warm the same decode lines on the interpreter role, and
/// seed both identically.
///
/// `slots` is the EXACT instruction count the block must cover. An exact count rather than a lower
/// bound is what says the rotate joined the block instead of ending it -- a `>=` assertion is
/// satisfied by the fillers alone with the form under test refused.
fn build(program: &[u8], slots: u8, seed: Seed) -> Roles {
    let mut memory = rotate_memory();
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

    // The probe is what moves the entry to `Seen`, and `install` refuses any key that is not in
    // that state. Without it every row here fails at the install rather than at its own assertion.
    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the rotate is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the rotate really ran natively"
    );
    // A register rotate touches no memory at any width. This is what would catch a lane that
    // reached for a memory form by mistake.
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
        "{context}: every slot must retire natively"
    );
    for _ in 0..roles.slots {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
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
    assert_eq!(
        roles.native.timing_rem, roles.interp.timing_rem,
        "{context}: scaled-clock remainder"
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

/// `FILL / <rotate> / <five consumers> / hlt`: the rotate with no descriptor producer ahead of it,
/// so CF comes from the seeded EFLAGS.
fn plain_program(rotate: &[u8]) -> (Vec<u8>, u8) {
    let mut code = FILL.to_vec();
    code.extend_from_slice(rotate);
    code.extend_from_slice(&consumer_bytes());
    code.push(0xf4);
    (code, 2 + CONSUMERS.len() as u8)
}

/// `FILL / cmp ebp,esi / <rotate> / <five consumers> / hlt`: the B4 shape. The `cmp` leaves a LIVE
/// lazy descriptor owning SF/ZF/PF/AF and also supplies the rotate's carry-in.
fn descriptor_program(rotate: &[u8]) -> (Vec<u8>, u8) {
    let mut code = FILL.to_vec();
    code.extend_from_slice(&CMP);
    code.extend_from_slice(rotate);
    code.extend_from_slice(&consumer_bytes());
    code.push(0xf4);
    (code, 3 + CONSUMERS.len() as u8)
}

/// Operand seeds. `0x8000_0000` and `0x0000_0001` are the shortest witnesses for a left and a
/// right rotate's CF; `0x7fff_ffff` and `0xffff_ffff` cover the two ends; `0x4000_0000` is the one
/// that makes RCL's count-1 OF disagree with its CF.
const OPERANDS: [u32; 6] = [
    0x0000_0000,
    0x0000_0001,
    0x4000_0000,
    0x7fff_ffff,
    0x8000_0000,
    0xffff_ffff,
];

// =============================================================================================
// Admission
// =============================================================================================

/// The anti-vacuity gate: `rcl edx,1` and `rcr edx,1` compile only with the knob on, and the
/// control row compiles either way.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_carry_rotates_compile_only_with_the_gate() {
    for on in [false, true] {
        let _arm = force_rcl_rows(on);
        assert_eq!(
            compile_span(&CONTROL),
            ADMITTED,
            "control: mov eax,ecx must compile on the {on} arm"
        );
        for (label, op) in CARRY_ROTATES {
            for dst in 0..8u8 {
                assert_eq!(
                    compile_span(&d1_reg(op, dst)),
                    if on { ADMITTED } else { REFUSED },
                    "0xd1 {label} r{dst},1 on the {on} arm"
                );
            }
        }
        // The two rotates that predate this slice must be unmoved by it in either direction.
        for (label, op) in [("rol", 0u8), ("ror", 1)] {
            assert_eq!(
                compile_span(&d1_reg(op, ROTATE_DST)),
                ADMITTED,
                "0xd1 {label} predates this slice and must compile on the {on} arm"
            );
        }
    }
}

/// The three refusals the design states, each of which the code already warned about, plus the
/// memory form and the shift siblings as controls.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_refusals_hold_on_both_arms() {
    for on in [false, true] {
        let _arm = force_rcl_rows(on);
        assert_eq!(
            compile_span(&CONTROL),
            ADMITTED,
            "control: mov eax,ecx must compile on the {on} arm"
        );
        for (label, op) in CARRY_ROTATES {
            // (1) WORD. `RotateReg` has no width field and its emitter is `shift_r32_imm8`, so a
            // 66-prefixed form would rotate 32 bits where the guest rotates 16 -- and would seed
            // bit 16 of the destination from CF, writing into a half a 16-bit rotate preserves.
            assert_eq!(
                compile_span_word(&d1_reg(op, ROTATE_DST)),
                REFUSED,
                "66 D1 /{op} {label} at Word must stay a barrier on the {on} arm"
            );
            // (2) BYTE forms. A byte rotate through this emitter rotates 32 bits, takes CF from
            // bit 31 instead of bit 7, and for indices 4..7 reaches the wrong guest home.
            assert_eq!(
                compile_span(&d0_reg(op, ROTATE_DST)),
                REFUSED,
                "0xd0 /{op} {label} must stay a barrier on the {on} arm"
            );
            assert_eq!(
                compile_span(&c0_reg(op, ROTATE_DST, 3)),
                REFUSED,
                "0xc0 /{op} {label} must stay a barrier on the {on} arm"
            );
            // (3) `0xC1` does NOT come along for free: the admission gates on `opcode == 0xd1`.
            for count in [0u8, 1, 2, 3, 31, 32] {
                assert_eq!(
                    compile_span(&c1_reg(op, ROTATE_DST, count)),
                    REFUSED,
                    "0xc1 /{op} {label}, {count} must stay a barrier on the {on} arm"
                );
            }
            // The MEMORY form, refused by the shared `DecodedOperand::Reg` bind.
            assert_eq!(
                compile_span(&d1_mem(op)),
                REFUSED,
                "0xd1 /{op} {label} MEMORY form must stay a barrier on the {on} arm"
            );
            // The shift-by-CL group is a different arm entirely and stays out.
            assert_eq!(
                compile_span(&[0xd3, 0xc0 | (op << 3) | ROTATE_DST]),
                REFUSED,
                "0xd3 /{op} {label} must stay a barrier on the {on} arm"
            );
        }
        // The wide SHIFT siblings, admitted before this slice, must still compile.
        for (label, bytes) in [
            ("0xd1 /4 shl", vec![0xd1, 0xe2]),
            ("0xc1 /5 shr", vec![0xc1, 0xea, 0x03]),
        ] {
            assert_eq!(
                compile_span(&bytes),
                ADMITTED,
                "{label} predates this slice and must compile on the {on} arm"
            );
        }
    }
}

// =============================================================================================
// The flag contract
// =============================================================================================

/// **The B4 fixture.** A live lazy descriptor must survive a lowered rotate.
///
/// `cmp ebp,esi` leaves a descriptor owning SF, ZF, PF and AF; the rotate defines CF (and OF at
/// count 1) and must leave the other four exactly where they were; the five `SETcc` slots then read
/// all of them inside the SAME block. `emit_carry_alu_preloaded`'s contract --
/// `emit_capture_flags(ARITH_FLAGS)` and a full arithmetic publish -- writes all six and would
/// disagree with the interpreter on four of them.
///
/// The `cmp` also supplies the rotate's CARRY-IN, so this row is simultaneously the proof that the
/// preload reads a CURRENT CF: RBP is the running materialized shadow, and a descriptor live over
/// the rotate does not make it stale.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_live_descriptor_survives_a_lowered_rotate() {
    let _arm = force_rcl_rows(true);
    for (label, op) in ROTATES {
        for operand in OPERANDS {
            // Every (ebp, esi) pair the cmp can produce a different flag word from: equal, below,
            // above, and the two that differ in sign.
            for (ebp, esi) in [
                (0u32, 0u32),
                (1, 0),
                (0, 1),
                (0x8000_0000, 0x7fff_ffff),
                (0x7fff_ffff, 0x8000_0000),
                (0xffff_ffff, 0xffff_ffff),
            ] {
                let (program, slots) = descriptor_program(&d1_reg(op, ROTATE_DST));
                let seed = Seed::new().reg(ROTATE_DST, operand).reg(5, ebp).reg(6, esi);
                let mut roles = build(&program, slots, seed);
                run_and_compare(
                    &mut roles,
                    &format!("{label} r{ROTATE_DST},1 after cmp {ebp:#x},{esi:#x} on {operand:#x}"),
                );
            }
        }
    }
}

/// The same five consumers with NO descriptor producer ahead of the rotate, so CF comes from the
/// seeded EFLAGS word and every preserved flag comes from it too.
///
/// Both carry seeds on every operand. That pair is what makes the row non-vacuous for the PRELOAD:
/// `rcl` of the same operand with CF=0 and CF=1 differ in bit 0 of the result, so an emitter that
/// never loaded the host flags produces the same answer twice and one of the two legs diverges.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_rotate_flag_contract_matches_the_interpreter_without_a_descriptor() {
    let _arm = force_rcl_rows(true);
    for (label, op) in ROTATES {
        for operand in OPERANDS {
            for carry in [false, true] {
                // Every preserved flag seeded BOTH ways, so a row that cleared SF/ZF/PF/AF and one
                // that set them are both visible.
                for preserved in [0x002u32, 0x8d6] {
                    let (program, slots) = plain_program(&d1_reg(op, ROTATE_DST));
                    let seed = Seed::new()
                        .reg(ROTATE_DST, operand)
                        .flags(preserved)
                        .carry(carry);
                    let mut roles = build(&program, slots, seed);
                    run_and_compare(
                        &mut roles,
                        &format!(
                            "{label} r{ROTATE_DST},1 on {operand:#x}, carry-in {carry}, \
                             preserved {preserved:#x}"
                        ),
                    );
                }
            }
        }
    }
}

/// CF is an INPUT, stated as its own claim rather than left to the sweep.
///
/// Two runs of the SAME program on the SAME operand differing only in the seeded CF must produce
/// DIFFERENT destinations. Without this, an emitter that dropped `emit_load_host_flags` could pass
/// the sweeps above on any host whose flags happened to carry the right CF into the block.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_carry_is_a_rotate_input() {
    let _arm = force_rcl_rows(true);
    for (label, op) in CARRY_ROTATES {
        let mut results = Vec::new();
        for carry in [false, true] {
            let (program, slots) = plain_program(&d1_reg(op, ROTATE_DST));
            let seed = Seed::new().reg(ROTATE_DST, 0).carry(carry);
            let mut roles = build(&program, slots, seed);
            run_and_compare(&mut roles, &format!("{label} carry-in {carry}"));
            results.push(roles.native.registers.gpr[usize::from(ROTATE_DST)]);
        }
        assert_ne!(
            results[0], results[1],
            "{label} of zero must differ between carry-in 0 and 1, or the preload is not being \
             read and the sweeps above are passing for the host's reason"
        );
        // The bit the carry lands in, named rather than inferred: RCL shifts it into bit 0 and RCR
        // into bit 31.
        let expected = if op == 2 { 1u32 } else { 0x8000_0000 };
        assert_eq!(
            results[1], expected,
            "{label} of zero with carry-in 1 must be {expected:#x}"
        );
        assert_eq!(
            results[0], 0,
            "{label} of zero with carry-in 0 must be zero"
        );
    }
}

/// OF is defined at count 1 and its definition is the DIRECTION's, not the carry form's.
///
/// `0x4000_0000` is the witness: `rcl` of it leaves the top two bits differing, so OF is set, while
/// `rcl` of `0x8000_0000` with carry-in 0 leaves them equal and OF clear. An emitter that dropped
/// `OF` from the count-1 capture would report the stale value for both.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn overflow_is_defined_at_count_one() {
    let _arm = force_rcl_rows(true);
    for (label, op) in CARRY_ROTATES {
        let mut seen = Vec::new();
        for operand in OPERANDS {
            for carry in [false, true] {
                // Seed OF the WRONG way for each case, so a missing capture is visible as the seed
                // surviving rather than as a coincidence.
                for seeded_of in [0x002u32, 0x802] {
                    let (program, slots) = plain_program(&d1_reg(op, ROTATE_DST));
                    let seed = Seed::new()
                        .reg(ROTATE_DST, operand)
                        .flags(seeded_of)
                        .carry(carry);
                    let mut roles = build(&program, slots, seed);
                    run_and_compare(
                        &mut roles,
                        &format!("{label} OF on {operand:#x}, carry {carry}, seed {seeded_of:#x}"),
                    );
                    seen.push(roles.native.eflags() & crate::FLAG_OF != 0);
                }
            }
        }
        assert!(
            seen.contains(&true) && seen.contains(&false),
            "{label}: the sweep must reach both OF polarities, or the row cannot see a capture \
             that dropped OF"
        );
    }
}

// =============================================================================================
// The heat gate, and the count lane
// =============================================================================================

/// The L1 heat gate keys on `(opcode, reg)`, so `0xD1 /0` ROL is inside it and `0xD1 /2` RCL is
/// not -- and the reason is the MATCH, not the absence of a count byte.
///
/// `rotate_row_count_byte`'s `0xd1 if reg == 0` arm returns `physical + len - 2`, the OPCODE byte,
/// precisely because for `0xD1` the count IS the opcode. So `0xD1` very much has a gate site; what
/// keeps RCL out of the gate is that `0xd1 reg == 2` falls to `_ => None`. This fixture pins both
/// halves against one heat record on the same byte offset: with `IZARRAVM_ROTATE_ROWS=heat_gated`,
/// ROL is downgraded to a barrier and RCL is not.
///
/// It is a `HeatGated`-arm fixture rather than a 2x2, because `rotate_row_count_byte` is reached
/// ONLY under that arm; a matrix over {on, off} alone would be a gate that cannot fail for the one
/// claim this file makes about the heat gate.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_heat_gate_keys_on_the_sub_opcode_as_well_as_the_opcode() {
    let _arm = force_rcl_rows(true);
    jit::direct::set_rotate_rows_arm_for_test(Some(jit::direct::RotateRowsArm::HeatGated));
    // `0xD1` is two bytes and the gate site is `len - 2`, i.e. the OPCODE byte at offset 0.
    let heat = Some(0);
    assert_eq!(
        compile_span_full(&d1_reg(0, ROTATE_DST), false, heat),
        REFUSED,
        "0xd1 /0 ROL over a heat-carrying count byte must be downgraded under the HeatGated arm; \
         without this the RCL half below proves nothing"
    );
    for (label, op) in CARRY_ROTATES {
        assert_eq!(
            compile_span_full(&d1_reg(op, ROTATE_DST), false, heat),
            ADMITTED,
            "0xd1 /{op} {label} is OUTSIDE the heat gate: rotate_row_count_byte's match is keyed \
             on (opcode, reg) and has no `0xd1 reg == {op}` arm"
        );
    }
    // Without the heat record the ROL row compiles, which is what says the row above measured the
    // gate rather than a broken harness.
    assert_eq!(
        compile_span_full(&d1_reg(0, ROTATE_DST), false, None),
        ADMITTED,
        "control: 0xd1 /0 ROL with no heat record must compile under the HeatGated arm"
    );
}

/// A `0xD1` rotate can never carry a count lane, on either arm of `IZARRAVM_COUNT_LANES`.
///
/// `count_lane_for` keys on `insn.opcode` being `0xC0` or `0xC1`, and these rows are `0xD1` only,
/// so `emit_rotate_reg_lane` -- whose `debug_assert` refuses `/2` and `/3` -- is unreachable for
/// them. Asserted through behaviour rather than through the kind: with the lane arm forced ON, a
/// guest patch of the `0xD1` opcode byte must NOT change what an already-compiled block does,
/// because the count is baked.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_d1_carry_rotate_never_takes_a_count_lane() {
    let _arm = force_rcl_rows(true);
    for lanes in [false, true] {
        jit::direct::set_count_lanes_for_test(Some(lanes));
        for (label, op) in CARRY_ROTATES {
            for operand in OPERANDS {
                let (program, slots) = plain_program(&d1_reg(op, ROTATE_DST));
                let seed = Seed::new().reg(ROTATE_DST, operand).carry(true);
                let mut roles = build(&program, slots, seed);
                run_and_compare(&mut roles, &format!("{label} with count lanes {lanes}"));
            }
        }
    }
}

// =============================================================================================
// The knob
// =============================================================================================

/// Every accepted spelling, and the panic on a typo. The panic is the load-bearing half: a
/// mistyped ladder leg that fell through to the default would run the OFF arm and be read as "the
/// rows I asked for changed nothing".
#[test]
fn rcl_rows_spelling_table_names_every_arm() {
    use jit::direct::parse_rcl_rows_arm_for_test as parse;
    assert!(
        !parse(Err(std::env::VarError::NotPresent)),
        "unset is the shipped default, which is OFF"
    );
    for spelling in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(
            !parse(Ok(spelling.to_string())),
            "{spelling:?} must name the OFF arm"
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
/// The ONE row in this file that does not force its arm, and it is supposed to be: it is what
/// would catch `rcl_rows_enabled`'s env path being changed to return `true` while every forcing
/// fixture kept passing.
#[test]
fn rcl_rows_ships_the_off_arm_by_default() {
    assert!(
        std::env::var("IZARRAVM_RCL_ROWS").is_err(),
        "this row reads the ambient knob, so the harness must not have it set"
    );
    assert!(
        !jit::direct::rcl_rows_enabled(),
        "the slice ships default OFF; the flip is a separate commit with its own ladder leg"
    );
}
