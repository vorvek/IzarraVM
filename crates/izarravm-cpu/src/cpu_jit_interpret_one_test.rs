// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The S2 generic call-out: `InterpretOne`, with one opcode admitted (0x8F POP r/m).
//!
//! The mechanism runs ONE interpreter instruction from inside a live native block and then either
//! resumes the block or ends the run. Everything here is about the seam between those two worlds,
//! so the fixtures are built to make the seam visible rather than to exercise POP: every execution
//! case runs the same bytes twice, once wholly interpreted and once with the block installed, and
//! compares registers, EIP, EFLAGS, guest RAM, `elapsed_clocks`, `perf.instructions` and the side
//! exit. A lowering bug and an accounting bug both show up as a difference between the two legs.
//!
//! **Why some clauses are pinned at the predicate and not through a fixture.** With 0x8F as the
//! whole allowlist, no admitted instruction can move a segment record, a control register or IF.
//! Those clauses are therefore evaluated against a CPU the test moved by hand, through
//! `ResumeSnapshot::allows_resume`, which is the same function the helper calls and not a copy of
//! it. S3's rows reach them for real; until then a fixture claiming to test them would be testing
//! nothing. The clauses that 0x8F CAN reach -- the watched-code write, the fault, the missing
//! decode view, the governor -- are pinned end to end.
//!
//! **Every fixture puts its stack and its POP target on a page the block's code is not on.**
//! Installing a block arms a code watch over the pages it spans, so a store into the code page is
//! a watched-code write. That is a real case and it has its own test; it must not be the accidental
//! shape of the others.

use super::sixteen_bit::{
    arm_native_sixteen_bit, sixteen_bit_bus, sixteen_bit_code_cpu, warm_sixteen_bit,
};
use super::*;

const ENTRY: u32 = 0x100;
/// A page the block's code is not on, for the stack and for the POP's destination.
const DATA_PAGE: u32 = 0x1000;
const STACK_TOP: u32 = 0x1700;
const POP_TARGET: u32 = 0x1800;
/// What the fixture seeds at the top of the stack, so the popped value is identifiable.
const POPPED: u16 = 0x4321;

// ---------------------------------------------------------------------------
// The shared fixture.
// ---------------------------------------------------------------------------

/// `mov ax, 0x1111; pop word [bx]; inc ax; hlt` in a 16-bit code segment.
///
/// The slot under test sits in the MIDDLE. That is the whole point: a call-out at the head would
/// pass with a resume predicate that never resumed (the block would have nothing left to run) and
/// a call-out at the tail would pass with a broken EIP restore (the exit's delta would be the last
/// one). With a slot on each side, a resume that does not resume loses `inc ax`, and an EIP
/// restore that does not restore lands the final exit somewhere else.
const CODE: &[u8] = &[0xB8, 0x11, 0x11, 0x8F, 0x07, 0x40, 0xF4];
const STARTS: &[u32] = &[0, 3, 5];
/// Instructions in the compiled block: the three before the HLT, which is unclassifiable.
const BLOCK_INSTRUCTIONS: u8 = 3;

/// Put a HLT where a real-mode fault delivery lands.
///
/// The IVT is zeroed, so every vector points at 0000:0000. Without an instruction there the
/// interpreted leg of a faulting fixture runs `add [bx+si], al` forever and the harness's
/// "guest did not halt" panic is what a test failure looks like. Both legs get the same byte, so
/// the fault path still has an oracle.
fn seed_fault_handler(program: &mut [u8]) {
    program[0] = 0xF4;
}

fn arm_fixture(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(STACK_TOP);
    cpu.registers.set_ebx(POP_TARGET);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[STACK_TOP as usize..STACK_TOP as usize + 2].copy_from_slice(&POPPED.to_le_bytes());
    bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2].fill(0);
    bus.trace = BusTrace::default();
}

struct Legs {
    interp: CpuGsw,
    interp_bus: TestBus,
    native: CpuGsw,
    native_bus: TestBus,
    /// The block's own side-exit reason, or `None` when it ran to completion.
    exit_reason: Option<u32>,
    /// Instructions the block reported retiring natively on the entry under test.
    native_insns: u64,
}

/// Build the two CPUs and the installed block without entering it yet, so a caller can perturb
/// state between the arm and the entry.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn build_native(code: &[u8], starts: &[u32]) -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
    let (cpu, bus, block, _) = build_native_keeping_compilation(code, starts);
    (cpu, bus, block)
}

/// `build_native`, and the `Compilation` it installed.
///
/// The compilation is worth keeping for exactly one caller: installing it AGAIN produces a block
/// that shares the SAME `InterpretOneCell` allocations, because `install` clones a
/// `Vec<Arc<InterpretOneCell>>` and an `Arc` clone is the same cell. That is the only way to enter
/// a demoted slot on purpose now that a demotion retires its block.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn build_native_keeping_compilation(
    code: &[u8],
    starts: &[u32],
) -> (
    CpuGsw,
    TestBus,
    jit::direct::CompiledBlock,
    jit::direct::Compilation,
) {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    seed_fault_handler(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    let linears: Vec<u32> = starts.iter().map(|offset| ENTRY + offset).collect();
    warm_sixteen_bit(&mut cpu, &mut bus, &linears);
    let compilation =
        jit::direct::compile(&mut cpu, ENTRY, false).expect("the fixture must compile as a block");
    let key = jit::direct::key_for(&cpu, ENTRY, false).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");
    (cpu, bus, block, compilation)
}

/// Run `code` interpreted and again with its leading block installed, and hand back both worlds.
///
/// `perturb` runs on BOTH cpus, right after the shared arm. It is fixture state and not a native
/// condition: applying it to one leg only would leave the other running a different program, and
/// every comparison below would then be measuring the fixture rather than the mechanism.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn run_both(code: &[u8], starts: &[u32], perturb: fn(&mut CpuGsw, &mut TestBus)) -> Legs {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    seed_fault_handler(&mut program);
    let mut interp_bus = sixteen_bit_bus(program);
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut interp, &mut interp_bus);
    perturb(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = build_native(code, starts);
    arm_fixture(&mut native, &mut native_bus);
    perturb(&mut native, &mut native_bus);

    let before = native.perf_counters().jit_direct_insns;
    let entries = native.perf_counters().jit_direct_entries;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture block must not stop the machine"),
        "the installed block must actually run"
    );
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 1);
    let native_insns = native.perf_counters().jit_direct_insns - before;
    let exit_reason = native.jit_direct.last_side_exit_reason_for_test();
    // The tail past the block is interpreted on both legs, so the two CPUs end at the same
    // architectural point and the whole-struct comparison below is exact.
    drive(&mut native, &mut native_bus);

    Legs {
        interp,
        interp_bus,
        native,
        native_bus,
        exit_reason,
        native_insns,
    }
}

/// Every quantity the two legs must agree on. Named rather than inlined because "the same" is the
/// claim of this whole file, and a case that checks four of the seven is the shape that ships a
/// bug: a wrong `elapsed_clocks` or a double-counted `perf.instructions` moves nothing a register
/// comparison can see.
fn assert_legs_agree(legs: &mut Legs) {
    // SETTLE BOTH before the register comparison. `Registers.eflags` is the RAW field, and two
    // CPUs at the same architectural state are free to hold different (raw eflags, pending
    // descriptor) representations of it -- which is exactly what happens here, because the block
    // publishes a settled word where the interpreter leaves a descriptor. Comparing the raw field
    // would fail on a difference that `eflags()` says is not one.
    legs.native.materialize_flags();
    legs.interp.materialize_flags();
    assert_eq!(
        legs.native.registers, legs.interp.registers,
        "registers or EIP differ between the native and interpreted legs"
    );
    assert_eq!(legs.native.eflags(), legs.interp.eflags(), "EFLAGS");
    assert_eq!(
        legs.native_bus.memory, legs.interp_bus.memory,
        "guest RAM differs"
    );
    assert_eq!(
        legs.native.elapsed_clocks, legs.interp.elapsed_clocks,
        "guest clocks differ"
    );
    assert_eq!(
        legs.native.perf_counters().instructions,
        legs.interp.perf_counters().instructions,
        "retirement counts differ, so some instruction was counted twice or not at all"
    );
}

fn no_perturb(_: &mut CpuGsw, _: &mut TestBus) {}

// ---------------------------------------------------------------------------
// 1. The mechanism: the block admits the row, runs it, and CARRIES ON.
// ---------------------------------------------------------------------------

/// The anti-vacuity gate for the whole slice: `pop word [bx]` compiles into a call-out slot rather
/// than ending the block.
///
/// Without this every other test here could pass with the row still a hard boundary, because a
/// two-instruction block that stops at the POP produces identical architectural state.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn pop_rm16_compiles_into_an_interpret_one_slot() {
    let (_, _, block) = build_native(CODE, STARTS);
    assert_eq!(
        block.span().instructions,
        BLOCK_INSTRUCTIONS,
        "the block stopped early, so the POP is still a boundary"
    );
    assert_eq!(
        block.callout_interpret_one_slots(),
        1,
        "the POP must be an InterpretOne slot"
    );
    assert_eq!(block.callout_port_slots(), 0);
    assert_eq!(block.callout_memory_slots(), 0);
}

/// The whole mechanism end to end: the slot runs, the predicate resumes, and the instruction AFTER
/// it retires natively in the same entry.
///
/// Pins design item B3 (`perf.instructions` charged exactly once) through `assert_legs_agree`, and
/// the resume itself through the retired count: a RESYNC would report two instructions, not three.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_pop_rm16_resumes() {
    let mut legs = run_both(CODE, STARTS, no_perturb);
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native_insns,
        u64::from(BLOCK_INSTRUCTIONS),
        "the block did not resume past the call-out"
    );
    assert_eq!(legs.exit_reason, None, "the block should have completed");
    assert_eq!(
        legs.native.registers.eax() & 0xffff,
        0x1112,
        "the instruction after the call-out must have run"
    );
    assert_eq!(
        u16::from_le_bytes(
            legs.native_bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2]
                .try_into()
                .unwrap()
        ),
        POPPED,
        "the POP must have stored the popped word"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_eq!(stalls.callout_interpret_one_resync_fault, 0);
    assert_eq!(stalls.callout_interpret_one_abnormal, 0);
}

/// Design item B1: the helper restores the block-entry EIP before it resumes.
///
/// Every exit in the emitter advances EIP RELATIVELY -- `emit_advance_eip` loads `cpu.eip`, adds a
/// compile-time delta and stores it back -- so a helper that leaves EIP at the instruction after
/// the slot makes the block's completed path advance from the wrong base. The mutation that this
/// catches is deleting the `cpu.registers.eip = entry_eip` on the resume path: the final EIP then
/// runs ahead by the slot's offset and this assertion fails while every register still matches.
///
/// It is a separate test from the resume above even though `assert_legs_agree` compares EIP too,
/// because it names the quantity: a reader looking for the B1 pin should find one.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_restores_entry_eip_on_resume() {
    let legs = run_both(CODE, STARTS, no_perturb);
    // The HLT is the last instruction, so both legs stop with EIP just past it.
    let expected = ENTRY + CODE.len() as u32;
    assert_eq!(legs.interp.registers.eip, expected);
    assert_eq!(
        legs.native.registers.eip, expected,
        "the native leg's EIP moved, so the entry value was not restored before the exit"
    );
}

/// Design item B4: the helper settles a pending flag descriptor on the way IN and republishes the
/// whole architectural EFLAGS on the way OUT, and the emitted slot reloads its RBP shadow from
/// there before the block reads a flag natively.
///
/// The fixture runs ONE interpreted instruction first (`add ax, cx`), which leaves a LAZY
/// descriptor -- `pending_flags` live and `registers.eflags` stale in the six arithmetic bits --
/// and only then enters a block whose first slot is the call-out and whose second is a `jz`.
/// A native ALU slot would not do: emitted flag sites compute eagerly into RBP and store the
/// "no pending" tag, so a block that produces its own flags never carries a descriptor into a
/// call-out. The descriptor has to come from outside the block, which is exactly the state design
/// review B4 says the memory copy is stale in.
///
/// The failure it catches is the flag sync being dropped: `registers.eflags` then keeps the
/// pre-`add` value, the slot's RBP reload picks that up, and the `jz` branches on the wrong ZF.
/// AX ends one apart and nothing else moves.
///
/// It pins the CONJUNCTION of the two halves rather than each one, and that is a fact about the
/// S2 allowlist rather than a gap in the fixture. POP changes no flag and leaves no descriptor, so
/// sync IN (`materialize_flags`) and sync OUT (`publish_flags`) compute the same value here and
/// either one alone is enough; deleting both is what this fails on. S3's rows -- the 0xF7 group,
/// the BT family -- change flags inside the step, and there the two stop being interchangeable.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_flags_exact_after_pending_descriptor() {
    // add ax, cx | pop word [bx] | jz +1 | hlt | inc ax | hlt
    const FLAG_CODE: &[u8] = &[0x01, 0xC8, 0x8F, 0x07, 0x74, 0x01, 0xF4, 0x40, 0xF4];
    // The block starts at the POP, so the ADD stays interpreted and its descriptor is live when
    // the block is entered.
    const BLOCK_ENTRY: u32 = ENTRY + 2;

    fn arm(cpu: &mut CpuGsw, bus: &mut TestBus) {
        arm_fixture(cpu, bus);
        // AX + CX wraps to zero at sixteen bits, so the ADD sets ZF...
        cpu.registers.set_eax(0x0001);
        cpu.registers.set_ecx(0xffff);
        // ...while the eflags word says the opposite, so a stale read branches the other way.
        cpu.registers.eflags = 0x202;
    }

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + FLAG_CODE.len()].copy_from_slice(FLAG_CODE);
    seed_fault_handler(&mut program);

    let mut interp_bus = sixteen_bit_bus(program.clone());
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let mut native_bus = sixteen_bit_bus(program);
    let mut native = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut native, &mut native_bus, &[0x0000, DATA_PAGE]);
    warm_sixteen_bit(
        &mut native,
        &mut native_bus,
        &[ENTRY, BLOCK_ENTRY, ENTRY + 4],
    );
    let compilation = jit::direct::compile(&mut native, BLOCK_ENTRY, false)
        .expect("the flag fixture must compile as a block");
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "the POP must be the block's call-out slot"
    );
    assert_eq!(
        compilation.span.instructions, 2,
        "the block is the call-out and the conditional that reads its flags"
    );
    let key = jit::direct::key_for(&native, BLOCK_ENTRY, false).expect("a key for the block");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("install the flag fixture block");
    let block = native.jit_direct.block(id).expect("the block must be live");

    arm(&mut native, &mut native_bus);
    native.cycle(&mut native_bus).expect("the interpreted ADD");
    assert!(
        !native.pending_flags.is_none(),
        "the fixture is vacuous unless the ADD left a live descriptor"
    );
    assert_eq!(native.registers.eip, BLOCK_ENTRY);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the flag fixture must not stop the machine")
    );
    drive(&mut native, &mut native_bus);

    // Settle both, for the reason `assert_legs_agree` states: the raw eflags field is a
    // representation, not the architectural value.
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(
        native.registers, interp.registers,
        "the conditional after the call-out branched on different flags"
    );
    assert_eq!(native.eflags(), interp.eflags(), "EFLAGS");
    assert_eq!(native_bus.memory, interp_bus.memory, "guest RAM");
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions
    );
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native.registers.eax() & 0xffff,
        0x0001,
        "ZF was set, so the branch must have skipped the first HLT and run the INC"
    );
}

/// The RESUME path, and only it, reloads RBP from the published EFLAGS.
///
/// A byte pin rather than a behavioural one, and the reason is worth writing down: NO row on the
/// S2 allowlist changes a flag, so with 0x8F alone the reload is provably a no-op -- RBP already
/// holds `materialized_eflags()` and that is exactly what the helper republishes. It is emitted
/// because S3's rows (the 0xF7 group, the BT family) do change flags, and because putting it in
/// with the mechanism is what keeps design review B4 satisfied by construction instead of by a
/// promise to come back. The pin says it exists and that it is on the fall-through, past every
/// status branch.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_reloads_the_flag_shadow_on_resume() {
    let compilation = compile_fixture(CODE, STARTS);
    let code = compilation.code;
    // `shr rax, 32` then `jnz rel32`, then `mov ebp, [r15 + disp32]`.
    let step_break = [0x48u8, 0xC1, 0xE8, 0x20, 0x0F, 0x85];
    let at = position(&code, &step_break).expect("the step-break test must be emitted");
    let reload_at = at + step_break.len() + 4;
    assert_eq!(
        &code[reload_at..reload_at + 3],
        &[0x41, 0x8B, 0xAF],
        "the resume path must reload RBP from the CPU's eflags; code={code:02x?}"
    );
}

// ---------------------------------------------------------------------------
// 2. The resume predicate, clause by clause.
// ---------------------------------------------------------------------------

/// The suffix mask a clause test hands the predicate: everything, which is what a slot with no
/// segment-writing row is compared against anyway and what keeps these cases about their own
/// clause rather than about the mask.
const ALL_SEGMENTS: u8 = u8::MAX;

/// The row every clause test below is written against. `0x8F` POP r/m is the mechanism's first
/// and plainest row and it arms nothing, so passing it here keeps each clause at its STRICT
/// reading. The two clauses that a row can loosen (the IF 0-to-1 edge and a step-armed
/// `interrupt_shadow`) have their own fixtures further down, and those pass `STI`.
const POP_RM: jit::direct::InterpretOneRow = jit::direct::InterpretOneRow::PopRm;

/// A CPU and the snapshot taken from it, for the predicate-level clauses. `end_eip` is the EIP a
/// well-behaved step would have left.
fn snapshot_fixture() -> (CpuGsw, jit::direct::ResumeSnapshot, u32) {
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    cpu.registers.eflags = 0x202;
    cpu.set_eip(ENTRY + 5);
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    (cpu, snapshot, ENTRY + 5)
}

/// The control: an untouched CPU resumes. Without it every refusal below could pass against a
/// predicate that refuses unconditionally.
#[test]
fn interpret_one_resumes_when_nothing_moved() {
    let (cpu, snapshot, end_eip) = snapshot_fixture();
    assert!(snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R1: the step must leave EIP exactly past the instruction. A transfer that moved it resyncs, and
/// so does a fault the step swallowed.
#[test]
fn interpret_one_resync_on_eip_move() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.set_eip(end_eip + 2);
    assert!(!snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R2: a segment record the block baked moved under it.
///
/// Predicate-level because 0x8F cannot load a segment; S3's `0x8E /4 /5` can. The block bakes the
/// base of every segment it addresses through, so a changed base makes every later memory slot
/// address the wrong linear page -- silently, because the fast map would happily serve it.
#[test]
fn interpret_one_resync_on_segment_change() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.base = ds.base.wrapping_add(0x10);
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    assert!(!snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R3, the CS half of R1: the block's whole compilation is keyed on CS.
#[test]
fn interpret_one_resync_on_cs_change() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    let mut cs = cpu.registers.cs();
    cs.selector = cs.selector.wrapping_add(1);
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(!snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R3: a control register the mode key or the paging state depends on.
#[test]
fn interpret_one_resync_on_control_register_change() {
    for (name, apply) in [
        (
            "cr0",
            (|cpu: &mut CpuGsw| cpu.control.cr0 ^= CR0_PE) as fn(&mut CpuGsw),
        ),
        ("cr3", |cpu: &mut CpuGsw| cpu.control.cr3 = 0x1000),
        ("cr4", |cpu: &mut CpuGsw| cpu.control.cr4 |= 1),
    ] {
        let (mut cpu, snapshot, end_eip) = snapshot_fixture();
        apply(&mut cpu);
        assert!(
            !snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS),
            "a moved {name} must resync"
        );
    }
}

/// Design item M8, the refusing direction: IF going 0 to 1 is exactly where the run loop would
/// deliver a pending interrupt, so the block must end there.
#[test]
fn interpret_one_resync_on_if_0_to_1() {
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    cpu.registers.eflags = 0x002;
    cpu.set_eip(ENTRY + 5);
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    cpu.registers.eflags |= FLAG_IF;
    assert!(!snapshot.allows_resume(&cpu, ENTRY + 5, POP_RM, ALL_SEGMENTS));
}

/// Design item M8, the resuming direction: IF going 1 to 0 has no delivery point, so refusing it
/// would cost the block for nothing. CLI is on the S3 list precisely because of this.
#[test]
fn interpret_one_resumes_on_if_1_to_0() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.registers.eflags &= !FLAG_IF;
    assert!(snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R3: the one-instruction interrupt shadow. The seam the helper uses does not clear it on the way
/// in (design item M6), so the clause is a plain test of the flag after the step.
#[test]
fn interpret_one_resync_on_interrupt_shadow() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.interrupt_shadow = true;
    assert!(!snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R3: the trap flag. A block cannot produce the instruction boundary single-step delivery wants,
/// so a step that leaves TF set has to hand the boundary back to the run loop.
///
/// Predicate-level because no row on the S2 allowlist can SET TF; the clause exists for the block
/// that was ENTERED with it set, which the Direct dispatcher has no refusal against.
#[test]
fn interpret_one_resync_on_trap_flag() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.registers.eflags |= FLAG_TF;
    assert!(!snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

/// R4: a LIVE mapping epoch that moved. The paging generation changed under the block.
#[test]
fn interpret_one_resync_on_mapping_epoch_change() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.set_data_write_mapping_epoch_for_test(7);
    assert!(
        snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS),
        "0 to n is a cold fill, not a mapping change"
    );
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    cpu.set_data_write_mapping_epoch_for_test(8);
    assert!(
        !snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS),
        "n to m is a mapping change and must resync"
    );
}

/// R7 and R8: the run loop's own state. A halted step or a paused REP is not a boundary the block
/// may run past.
#[test]
fn interpret_one_resync_on_halt() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.halted = true;
    assert!(!snapshot.allows_resume(&cpu, end_eip, POP_RM, ALL_SEGMENTS));
}

// ---------------------------------------------------------------------------
// 3. The clauses 0x8F reaches for real.
// ---------------------------------------------------------------------------

/// Design item B2, end to end: a store from inside the call-out onto the block's OWN watched page
/// must not retire the block while its native frame is live, and must still invalidate before the
/// guest runs again.
///
/// The fixture points the POP at the code page. The store commits (the interpreter's own path,
/// with nothing suppressed), `note_code_write_inner` records it instead of invalidating, R5 sees a
/// non-empty list and the block RESYNCs, and `run_direct_block` drains the list on the way out --
/// which is what actually kills the block. The assertions are therefore ordered: the block ran and
/// returned, and only afterwards is it gone.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_resync_on_watched_code_write() {
    // The POP's destination is the block's own second instruction.
    fn aim_at_the_code(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_ebx(ENTRY + 3);
    }
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    seed_fault_handler(&mut program);
    let mut interp_bus = sixteen_bit_bus(program);
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut interp, &mut interp_bus);
    aim_at_the_code(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = build_native(CODE, STARTS);
    arm_fixture(&mut native, &mut native_bus);
    aim_at_the_code(&mut native, &mut native_bus);
    let live_before = native.jit_direct.len();
    assert_eq!(live_before, 1);

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("a self-modifying store must not stop the machine"),
        "the block must run"
    );
    // The DRAIN happened inside `run_direct_block`, after the native return: the block is gone by
    // the time control is back here, and it was alive while the helper was running.
    assert_eq!(
        native.jit_direct.len(),
        0,
        "the deferred code write must have retired the block after the native return"
    );
    assert_eq!(
        native.jit_direct.last_side_exit_reason_for_test(),
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "a watched-code write must RESYNC, not complete and not abnormal"
    );
    let stalls = native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync, 1);
    drive(&mut native, &mut native_bus);
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions
    );
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
}

/// Design item M9, the not-retired stub: a step that FAULTED is counted and charged by
/// `finish_instruction`, so the block must report the prefix and add nothing.
///
/// The fixture puts the POP's destination across the real-mode DS limit, which is a `#GP(0)` the
/// interpreter raises from inside `write_operand_sized`. The interpreted leg delivers the same
/// fault at the same point, so `perf.instructions`, `elapsed_clocks` and the handler EIP all have
/// an oracle. Reporting `prefix + 1` here would double-count the faulting instruction, which
/// `assert_legs_agree`'s retirement comparison is what catches.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_resync_fault_reports_prefix_only() {
    fn aim_past_the_limit(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_ebx(0xffff);
    }
    let mut legs = run_both(CODE, STARTS, aim_past_the_limit);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResyncFault as u32),
        "a faulting step must take the not-retired RESYNC stub"
    );
    assert_eq!(
        legs.native_insns, 1,
        "the block must report the PREFIX only: the fault path already counted the POP"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync_fault, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_legs_agree(&mut legs);
}

/// Design item M10: no resident decode view is ABNORMAL, not a re-decode.
///
/// A full re-decode from inside a live block reaches the code fetch path, which page-walks on a
/// TLB miss, and a walk writes accessed bits with native code on the host stack. The helper
/// therefore fails closed: nothing runs, EIP is left at the call-out, and the interpreter takes
/// the instruction at the boundary exactly as it did before this slice.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_abnormal_when_decode_view_missing() {
    let (mut native, mut native_bus, block) = build_native(CODE, STARTS);
    arm_fixture(&mut native, &mut native_bus);
    // Kill the decode LINES only. `invalidate_code_caches` would be wrong here for a reason that
    // is the point of the test: it also drops the compiled block, so the entry would be refused
    // before the helper ever ran and the fixture would pass while proving nothing.
    native.decode_cache.invalidate_and_clear_code_marks();
    let before = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("an abnormal call-out must not stop the machine"),
        "the block must run"
    );
    assert_eq!(
        native.jit_direct.last_side_exit_reason_for_test(),
        Some(jit::direct::SideExitReason::CallOutAbnormal as u32)
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - before,
        1,
        "only the slot before the call-out may retire"
    );
    assert_eq!(
        native.registers.eip,
        ENTRY + 3,
        "EIP must sit AT the call-out, so the interpreter re-runs it"
    );
    assert_eq!(
        native
            .direct_stall_snapshot()
            .callout_interpret_one_abnormal,
        1
    );
}

/// Design item M11, the governor: three RESYNCs inside the first eight executions demote the slot,
/// and a demoted slot takes the abnormal exit without calling the helper at all.
///
/// The faulting fixture is the driver because it resyncs on EVERY execution while leaving the
/// block installed -- the watched-code case retires the block on its first, so it cannot count to
/// three. Each iteration re-arms the CPU and re-enters, which is what the dispatcher would do.
///
/// The FOURTH entry needs a block the demotion did not take away, and the demotion now retires the
/// one it was learned on (`a_demoted_slot_recompiles_as_a_hard_boundary`). Re-installing the same
/// `Compilation` gives one: `install` clones the cell `Arc`s, so the new block reads the SAME
/// governor byte, and entering it is the only way to reach the emitted prologue on purpose.
///
/// That prologue is not made dead by the retire, which is why this claim is still worth pinning. A
/// SELF-LOOP block runs its slots more than once inside one native entry, so a slot demoted on the
/// first iteration is met again on the second, before any retire can happen -- and the demotion
/// can land on a resume path, where the block keeps running after it.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_demotes_after_three_resyncs() {
    let (mut native, mut native_bus, block, compilation) =
        build_native_keeping_compilation(CODE, STARTS);
    let mut executed = Vec::new();
    for entry in 0..4 {
        // The demotion lands on the third entry and retires the block, so the fourth needs its
        // own installation of the same code -- and of the same cells.
        let block = if entry < 3 {
            block
        } else {
            let id = native
                .jit_direct
                .install(&compilation)
                .expect("re-install the fixture block");
            native
                .jit_direct
                .block(id)
                .expect("the re-installed block must be live")
        };
        arm_fixture(&mut native, &mut native_bus);
        native.registers.set_ebx(0xffff);
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .expect("the fixture must not stop the machine")
        );
        executed.push(
            native
                .direct_stall_snapshot()
                .callout_interpret_one_executed,
        );
    }
    let stalls = native.direct_stall_snapshot();
    assert_eq!(
        executed,
        vec![1, 2, 3, 3],
        "the fourth entry must not reach the helper: the slot is demoted"
    );
    assert_eq!(
        stalls.callout_interpret_one_demoted, 1,
        "the demotion counter fires once per cell, not once per later abnormal"
    );
    assert_eq!(
        native.jit_direct.last_side_exit_reason_for_test(),
        Some(jit::direct::SideExitReason::CallOutAbnormal as u32),
        "a demoted slot is the pre-slice hard boundary: abnormal, EIP at the instruction"
    );
    assert_eq!(
        native.registers.eip,
        ENTRY + 3,
        "the demoted slot must leave EIP at the call-out"
    );
}

/// `mov ax,0x1111; inc ax; inc ax; pop word [bx]; inc ax; inc ax; inc ax; hlt`.
///
/// Three instructions on EACH side of the call-out, and that is what the fixture is for rather
/// than the shared `CODE`'s one. It makes both halves of the demoted-site claim non-vacuous:
///
/// * the block that ends BEFORE the slot still has three slots, so it installs and can be run,
///   instead of dying on `compile_with_instruction_limit`'s short-block return for a reason that
///   has nothing to do with the demotion;
/// * a walk STARTING at the slot has three more instructions behind it, so before the demotion it
///   compiles into a real block carrying the call-out. After it, the same walk stops on its first
///   slot. A two-instruction tail would have refused that entry either way.
const DEMOTED_SITE_CODE: &[u8] = &[
    0xB8, 0x11, 0x11, // mov ax, 0x1111        +0
    0x40, // inc ax                            +3
    0x40, // inc ax                            +4
    0x8F, 0x07, // pop word [bx]               +5   <- the call-out slot
    0x40, // inc ax                            +7
    0x40, // inc ax                            +8
    0x40, // inc ax                            +9
    0xF4, // hlt                               +10
];
const DEMOTED_SITE_STARTS: &[u32] = &[0, 3, 4, 5, 7, 8, 9];
/// Where the call-out sits in `DEMOTED_SITE_CODE`.
const DEMOTED_SITE_SLOT: u32 = ENTRY + 5;

/// A demoted slot must stop being a slot: the demotion retires the block and marks the SITE, and
/// the recompile ends its block before the instruction.
///
/// The thing this exists to prevent, measured on the tombraid loader before it existed: 405
/// demoted cells produced 1,965,674 abnormal side exits, because a demoted slot keeps its place in
/// the block and every later execution runs `test byte [cell], 0x80; jnz abnormal` and pays a
/// dispatcher round trip to reach the boundary the governor already decided on. The block also
/// keeps carrying the slots after it, which that exit guarantees are unreachable.
///
/// The site is keyed on the instruction and not on the block, so the assertion that matters is the
/// SECOND compile: a walk entering AT the slot -- a different key, a different block -- stops too.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_demoted_slot_recompiles_as_a_hard_boundary() {
    let (mut native, mut native_bus, block) = build_native(DEMOTED_SITE_CODE, DEMOTED_SITE_STARTS);
    assert_eq!(
        block.callout_interpret_one_slots(),
        1,
        "control: the fresh block carries the slot"
    );
    let at_slot = jit::direct::compile(&mut native, DEMOTED_SITE_SLOT, false)
        .expect("control: the slot leads a compilable block before the demotion");
    assert_eq!(
        at_slot.callout_interpret_one_slots, 1,
        "control: a walk entering at the slot admits it too"
    );
    let key = jit::direct::key_for(&native, ENTRY, false).expect("a key for the fixture block");

    // Three faulting executions: the same driver `interpret_one_demotes_after_three_resyncs`
    // uses, and the only one that resyncs every time while leaving the block installed.
    for _ in 0..3 {
        arm_fixture(&mut native, &mut native_bus);
        native.registers.set_ebx(0xffff);
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .expect("the fixture must not stop the machine")
        );
    }
    assert_eq!(
        native.direct_stall_snapshot().callout_interpret_one_demoted,
        1
    );
    assert_eq!(
        native.jit_direct.demoted_callout_site_count_for_test(),
        1,
        "the demotion must name the code SITE, not only the cell"
    );
    assert!(
        !native.jit_direct.retire_key_for_recompile(key),
        "the demotion must already have retired the block, so this second retire finds nothing"
    );

    let recompiled = jit::direct::compile(&mut native, ENTRY, false)
        .expect("the prefix before the slot must still compile");
    assert_eq!(
        recompiled.callout_interpret_one_slots, 0,
        "the recompile must not re-admit the demoted slot"
    );
    assert_eq!(
        recompiled.span.instructions, 3,
        "the block ends BEFORE the slot: mov ax, inc ax, inc ax"
    );
    assert_eq!(
        u32::from(recompiled.span.guest_len),
        DEMOTED_SITE_SLOT - ENTRY,
        "so the successor starts AT the instruction the boundary hands to the interpreter"
    );
    assert!(
        matches!(
            jit::direct::compile(&mut native, DEMOTED_SITE_SLOT, false),
            jit::direct::CompileOutcome::StructuralReject(_)
        ),
        "the site is a boundary for EVERY walk, so the one that starts on it has no first slot          and lands on the same structural reject an unlowered opcode there would have"
    );
    let after = jit::direct::compile(&mut native, DEMOTED_SITE_SLOT + 2, false)
        .expect("the instruction after the slot leads the next block");
    assert_eq!(after.span.instructions, 3, "inc ax, inc ax, inc ax");

    // And the exit the storm was made of is gone: the recompiled block has no call-out prologue to
    // take it from, so no number of later executions can grow the counter.
    let abnormal_before = native.direct_stall_snapshot().side_exit_callout_abnormal;
    let id = native
        .jit_direct
        .install(&recompiled)
        .expect("install the recompiled block");
    let installed = native.jit_direct.block(id).expect("the block must be live");
    for _ in 0..4 {
        arm_fixture(&mut native, &mut native_bus);
        native.registers.set_ebx(0xffff);
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, installed)
                .expect("the boundary block must not stop the machine")
        );
        assert_eq!(
            native.jit_direct.last_side_exit_reason_for_test(),
            None,
            "a block that ENDS at the boundary completes; it has no exit to take"
        );
        assert_eq!(
            native.registers.eip, DEMOTED_SITE_SLOT,
            "and it leaves EIP at the instruction, exactly as the pre-slice barrier did"
        );
    }
    assert_eq!(
        native.direct_stall_snapshot().side_exit_callout_abnormal,
        abnormal_before,
        "no later execution may pay an abnormal exit"
    );
}

/// Design review round 2, BLOCKER 1: the call-out window stays OPEN across the fault delivery.
///
/// `deliver_exception` pushes three words in real mode, and a TINY-MODEL guest -- SS and CS on one
/// page, which is what the loader is -- puts those pushes on the running block's own bytes. If the
/// window closes before `finish_instruction`, they reach `invalidate_physical_range` with the
/// block's native frame still on the host stack and retire the block the helper has to RETURN
/// THROUGH. Closing after is the whole fix.
///
/// The fixture aims the frame at the block deliberately: SP starts at 0x104, the POP takes the
/// word there and leaves SP at 0x106, and the three pushes then land at 0x104, 0x102 and 0x100 --
/// the block's own three instructions.
///
/// `callout_deferred_code_writes` is what makes it a test rather than a hope. With the window
/// closed too early the delivery's writes invalidate immediately and the counter reads ZERO,
/// because the POP itself faulted before it could store; with the window held open it reads the
/// frame. That counter is also the only always-on evidence design review B2 can have, since the
/// drain makes the outcome identical either way.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_window_stays_open_across_fault_delivery() {
    /// SP inside the block, so all three words of the delivery frame land on the code.
    ///
    /// The POP does NOT move it: its `0x8f` arm restores the pre-pop (E)SP when the store faults,
    /// so the instruction is restartable. The frame therefore starts here and grows down through
    /// 0x104, 0x102 and 0x100 -- the block's whole span. Two bytes lower and the last word would
    /// fall below the block and only two of the three would be watched, which is what the first
    /// version of this fixture measured.
    const TINY_MODEL_SP: u32 = ENTRY + 6;

    fn arm_tiny_model(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_esp(TINY_MODEL_SP);
        // Past the real-mode DS limit at a word width, which is the #GP the POP's store takes.
        cpu.registers.set_ebx(0xffff);
    }

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    seed_fault_handler(&mut program);

    let mut interp_bus = sixteen_bit_bus(program.clone());
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut interp, &mut interp_bus);
    arm_tiny_model(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = build_native(CODE, STARTS);
    arm_fixture(&mut native, &mut native_bus);
    arm_tiny_model(&mut native, &mut native_bus);
    assert_eq!(native.jit_direct.len(), 1);
    let deferred_before = native.direct_stall_snapshot().callout_deferred_code_writes;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("a delivery onto the block's own page must not stop the machine"),
        "the block must run"
    );

    let stalls = native.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_deferred_code_writes - deferred_before,
        3,
        "the three words of the real-mode delivery frame must all have been DEFERRED, which they \
         are only if the window was still open across `finish_instruction`"
    );
    assert_eq!(
        stalls.callout_interpret_one_resync_fault, 1,
        "the step faulted, so this is the not-retired RESYNC"
    );
    // The drain ran on the way out of `run_direct_block`, so the block is gone by the time control
    // is back here. That it went through the DRAIN rather than through the live frame is what the
    // counter above says.
    assert_eq!(
        native.jit_direct.len(),
        0,
        "the deferred delivery writes must have retired the block at the drain"
    );

    drive(&mut native, &mut native_bus);
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native_bus.memory, interp_bus.memory, "guest RAM");
    assert_eq!(
        native.perf_counters().instructions,
        interp.perf_counters().instructions
    );
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
}

/// Design review round 2, MAJOR 2: the helper puts `core_clocks_so_far` back.
///
/// The field is the base a device sees for guest time. `run_direct_block` sets it ONCE per entry
/// and `port_read_al_dx` previews on top of it without writing it back, because it can compute
/// into a local. `interpret_one` cannot -- the interpreter reads the field -- so it writes the
/// preview and must restore the entry value on the way out. Leaving it advanced makes the NEXT
/// call-out in the same block preview a prefix that already contained this one's.
///
/// The fixture is exactly that block: a POP call-out followed by a port call-out, and the oracle
/// is the timestamp the interpreted leg's `read_io` receives. A leaked preview shows up there and
/// nowhere else -- no register moves, no clock total changes, and the block still completes.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_restores_the_device_timestamp_base() {
    // EIGHT `mov ax,imm16` | pop word [bx] | in al,dx | inc ax | hlt.
    //
    // The eight are not padding. The leak this pins is the POP slot's own clock PREVIEW left in
    // the field, and that preview is `prefix_raw * num / den` -- one twelfth on both Approximate
    // personas. With a two-instruction prefix it rounds to zero and a leaked zero is invisible, so
    // the fixture puts sixteen raw clocks of prefix in front of the call-out and the preview
    // becomes a clock the port slot's timestamp either carries twice or does not.
    const PORT_CODE: &[u8] = &[
        0xB8, 0x11, 0x11, 0xB8, 0x22, 0x22, 0xB8, 0x33, 0x33, 0xB8, 0x44, 0x44, 0xB8, 0x55, 0x55,
        0xB8, 0x66, 0x66, 0xB8, 0x77, 0x77, 0xB8, 0x88, 0x88, 0x8F, 0x07, 0xEC, 0x40, 0xF4,
    ];
    const PORT_STARTS: &[u32] = &[0, 3, 6, 9, 12, 15, 18, 21, 24, 26, 27];

    fn arm_port(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_edx(0x0201);
    }

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + PORT_CODE.len()].copy_from_slice(PORT_CODE);
    seed_fault_handler(&mut program);

    let mut interp_bus = sixteen_bit_bus(program.clone());
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    // WARMED like the native role, and that is part of the oracle rather than setup. The timestamp
    // a device sees is the clocks retired SO FAR IN THIS RUN, so a cold interpreted leg that broke
    // its straight-line run on a decode miss would restart the count and read zero at the port for
    // reasons that have nothing to do with the call-out.
    let interp_linears: Vec<u32> = PORT_STARTS.iter().map(|offset| ENTRY + offset).collect();
    warm_sixteen_bit(&mut interp, &mut interp_bus, &interp_linears);
    arm_fixture(&mut interp, &mut interp_bus);
    arm_port(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = build_native(PORT_CODE, PORT_STARTS);
    assert_eq!(
        block.callout_interpret_one_slots(),
        1,
        "the fixture needs the POP slot"
    );
    assert_eq!(
        block.callout_port_slots(),
        1,
        "and the port slot after it, or the leak has nothing to show up in"
    );
    arm_fixture(&mut native, &mut native_bus);
    arm_port(&mut native, &mut native_bus);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the port fixture must not stop the machine")
    );
    drive(&mut native, &mut native_bus);

    assert_eq!(
        native_bus.io_reads.len(),
        1,
        "the fixture is vacuous unless the port slot served"
    );
    assert_eq!(
        native_bus.io_reads, interp_bus.io_reads,
        "the port slot saw a different guest timestamp than the interpreter did, so the call-out \
         before it left `core_clocks_so_far` advanced"
    );
    native.materialize_flags();
    interp.materialize_flags();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
}

/// Design review round 2, MAJOR 3: the step settles the write record it leaves behind.
///
/// The interpreter clears the record at the head of every instruction (`begin_instruction`). A
/// call-out's successor is a NATIVE slot and never runs one, so without `settle_write_record` the
/// call-out's writes ride the rest of the block: the prefetch invalidation they owe runs at the
/// first interpreted instruction after the BLOCK, and the record itself -- four `CpuGsw` fields
/// the whole-CPU differential compares -- accumulates.
///
/// Compared against the interpreter stepped to the SAME boundary, which is the only place the two
/// can be asked to agree: by the time both halt, the interpreted HLT has cleared the record on
/// both roles and the difference is gone. That is what makes this a latent flake rather than a
/// visible failure, and why it needs a boundary assertion rather than an end-state one.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_settles_the_write_record_for_the_next_slot() {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    seed_fault_handler(&mut program);

    let mut interp_bus = sixteen_bit_bus(program.clone());
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut interp, &mut interp_bus);
    // The three instructions the block covers, one at a time, so the comparison lands on the
    // boundary the block exits at.
    for _ in 0..BLOCK_INSTRUCTIONS {
        interp.cycle(&mut interp_bus).expect("interpreted step");
    }

    let (mut native, mut native_bus, block) = build_native(CODE, STARTS);
    arm_fixture(&mut native, &mut native_bus);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture must not stop the machine")
    );

    assert_eq!(
        native.written_count, interp.written_count,
        "the call-out's write record outlived its own instruction"
    );
    assert_eq!(native.written_pages, interp.written_pages);
    assert_eq!(native.written_pages_overflow, interp.written_pages_overflow);
    assert_eq!(native.last_written_page, interp.last_written_page);
    // Non-vacuity: the POP really did store, so there was a record to settle.
    assert_eq!(
        u16::from_le_bytes(
            native_bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2]
                .try_into()
                .unwrap()
        ),
        POPPED
    );
}

/// The other half of `settle_write_record`, on its own: a write onto the page the prefetch queue
/// is holding drops the queue.
///
/// A unit test rather than a fixture, and that is the honest shape for it. The 486 prefetch queue
/// is a SNAPSHOT of already-fetched bytes, so the invalidation only becomes guest-visible when
/// something later fetches through the queue over bytes the call-out overwrote -- and with S2's
/// allowlist every slot after a call-out is native and fetches through nothing. The clause is
/// emitted because it is what makes the CLEAR beside it safe: clearing the record without acting
/// on it would lose the invalidation entirely rather than delay it.
#[test]
fn settle_write_record_invalidates_the_prefetch_queue() {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    // A bus WITHOUT direct pages, deliberately. The prefetch queue is the slow fetch path's
    // snapshot; a direct-RAM page is read in place and never fills it, so on the fixtures' usual
    // bus this clause has nothing to act on and the test would be vacuous.
    let mut bus = TestBus::with_memory(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut cpu, &mut bus);
    // EXECUTED, not merely decoded: the queue is filled by the fetch path, and a decode that hits
    // the cache never touches it.
    cpu.cycle(&mut bus)
        .expect("the fixture must execute one instruction");
    let page = cpu
        .prefetch
        .physical_page()
        .expect("the fixture must leave the prefetch queue hot");

    // A write somewhere else leaves it alone.
    cpu.record_write_page((page + 1) << 12);
    cpu.settle_write_record();
    assert_eq!(
        cpu.prefetch.physical_page(),
        Some(page),
        "an unrelated page must not drop the queue"
    );
    assert_eq!(cpu.written_count, 0, "the record is cleared either way");

    // A write ON the page drops it.
    cpu.record_write_page(page << 12);
    cpu.settle_write_record();
    assert_eq!(
        cpu.prefetch.physical_page(),
        None,
        "a write onto the queue's own page must drop it"
    );
}

/// Design review round 2, MAJOR 5: `POP_RM_CORE_CLOCKS` is what the interpreter actually charges.
///
/// The constant is the budget bound's input (`INTERPRET_ONE_MAX_CORE_CLOCKS`) and, since this
/// round, the literal in `execute.rs`'s `0x8f` arm as well, so the two cannot drift apart. The pin
/// is a pair of roles: one CPU RUNS the instruction, the other scales the constant from the same
/// fresh state, and they agree only if the arm charges the constant.
///
/// TWENTY-FOUR of them, not one. Both Approximate personas scale core clocks by one twelfth with a
/// carried remainder, so a single POP charges zero however wrong the constant is; the difference
/// only leaves the rounding after a couple of dozen. That is the same reason
/// `cpu_jit_callout_matrix_test.rs` separates the call-out clock lanes by ACCUMULATION.
#[test]
fn pop_rm_core_clocks_is_what_the_interpreter_charges() {
    const REPEATS: usize = 24;
    let mut program = vec![0u8; 0x2000];
    for index in 0..REPEATS {
        // pop word [bx]
        let at = ENTRY as usize + index * 2;
        program[at..at + 2].copy_from_slice(&[0x8F, 0x07]);
    }
    program[ENTRY as usize + REPEATS * 2] = 0xF4;
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut cpu, &mut bus);
    let mut oracle = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut oracle, &mut bus);

    let mut expected = 0u64;
    for _ in 0..REPEATS {
        cpu.cycle(&mut bus).expect("the POP must execute");
        expected += oracle.scale_clocks(crate::POP_RM_CORE_CLOCKS);
    }
    assert!(
        expected > 0,
        "the fixture must clear the timing dial's rounding"
    );
    assert_eq!(
        cpu.elapsed_clocks, expected,
        "the 0x8f arm charges something other than POP_RM_CORE_CLOCKS"
    );
}

/// The deferred list records WATCHED writes only, and the byte door is why that has to be checked
/// here rather than inherited from the caller.
///
/// `write_linear_fragment` pre-gates its call on `code_write_watched`, so for a sized store the
/// probe inside the window is redundant. `write_linear_u8` does NOT: it calls its door on
/// `changed` alone so that a one-byte immediate patch can be absorbed as a lane. With the window
/// open and no probe, every changed byte store made from inside a call-out lands in the list, R5
/// reads it as a code write and the block RESYNCs -- on every execution, until the governor
/// demotes the slot. That is a per-execution loss for traffic that touches no code at all, and it
/// is invisible to every S2 fixture because no S2 row stores a byte.
///
/// Both directions are pinned here. An unwatched byte write must leave the list empty and report
/// no hit; a write onto the block's own page must still be recorded, which is the whole reason the
/// window exists.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_call_out_window_defers_watched_writes_only() {
    let (mut cpu, _bus, _block) = build_native(CODE, STARTS);

    // A page the fixture's code and decode lines are not on.
    cpu.deferred_code_writes.open();
    assert!(
        !cpu.note_code_write_hit(POP_TARGET, 1),
        "an unwatched byte write must not report a code hit"
    );
    cpu.deferred_code_writes.close();
    assert!(
        cpu.deferred_code_writes.is_empty(),
        "an unwatched write must not be deferred, or every byte-storing row resyncs"
    );

    // The block's own second instruction, which installing the block armed a watch over.
    cpu.deferred_code_writes.open();
    assert!(
        cpu.note_code_write_hit(ENTRY + 3, 1),
        "a write onto the running block must still report a hit"
    );
    cpu.deferred_code_writes.close();
    assert!(
        !cpu.deferred_code_writes.is_empty(),
        "a watched write must be deferred rather than invalidating under the live block"
    );
    assert_eq!(
        cpu.jit_direct.len(),
        1,
        "the block must still be installed: the window defers, it does not invalidate"
    );
}

/// An UNWATCHED write made inside the window still reaches the body's diagnostics.
///
/// The window branch's own comment says an unwatched write falls through to the ordinary body, so
/// the unit-sim feed, the SMC trace and the smc-census choke keep observing while a call-out runs.
/// It shipped as a nested early return, which made that sentence false: for as long as a window
/// was open those three stopped seeing anything. Nothing was unsound -- none of them invalidates --
/// but a diagnostic that goes blind during exactly the mechanism under measurement is worse than
/// one that was never turned on.
///
/// The SMC trace is the observer because it is the one that records EVERY write reaching the body,
/// watched or not: `traced` is built from `smc_trace.0.is_some()` alone, and `trace.record` runs
/// unconditionally at the end. Its `events=` line is therefore a direct count of writes that got
/// past the window branch.
///
/// MUTATION: put the early return back and the unwatched case reads `events=0`.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn an_unwatched_write_inside_the_window_still_reaches_the_diagnostics() {
    let (mut cpu, _bus, _block) = build_native(CODE, STARTS);
    cpu.set_smc_trace_enabled(true);

    // A page the fixture's code and decode lines are not on.
    cpu.deferred_code_writes.open();
    assert!(!cpu.note_code_write_hit(POP_TARGET, 1));
    cpu.deferred_code_writes.close();
    assert!(
        cpu.deferred_code_writes.is_empty(),
        "an unwatched write must not be deferred"
    );
    let report = cpu
        .take_smc_trace_report()
        .expect("the trace was enabled above");
    assert!(
        report[0].starts_with("smc_trace events=1 "),
        "the unwatched write must have reached the body: {}",
        report[0]
    );

    // The WATCHED write is the other direction: it is recorded and returns BEFORE the body, so the
    // trace must not see it. Without this half the assertion above would also pass for a branch
    // that deferred nothing at all.
    cpu.set_smc_trace_enabled(true);
    cpu.deferred_code_writes.open();
    assert!(cpu.note_code_write_hit(ENTRY + 3, 1));
    cpu.deferred_code_writes.close();
    assert!(!cpu.deferred_code_writes.is_empty());
    let report = cpu
        .take_smc_trace_report()
        .expect("the trace was enabled above");
    assert!(
        report[0].starts_with("smc_trace events=0 "),
        "a deferred write must not reach the body until the drain: {}",
        report[0]
    );
}

// ---------------------------------------------------------------------------
// 4. Emitted shape.
// ---------------------------------------------------------------------------

/// The demotion prologue is emitted, and it is a BIT test rather than a compare against zero.
///
/// The plan sketched `cmp byte [cell], 0; jnz abnormal`, which is wrong against the cell layout the
/// same plan specifies: the low bits are the execution and RESYNC counts, so that sequence would
/// refuse the slot from its second execution. `F6 /0 80` is `test byte [rax], 0x80`, the demoted
/// bit alone.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_slot_encodes_demotion_prologue() {
    let compilation = compile_fixture(CODE, STARTS);
    assert_eq!(
        compilation.interpret_one_cells.len(),
        1,
        "one cell for one InterpretOne slot"
    );
    let cell = compilation.interpret_one_cells[0].address();
    // `mov rax, imm64 <cell>` then `test byte [rax], 0x80`.
    let mut needle = vec![0x48u8, 0xB8];
    needle.extend_from_slice(&(cell as u64).to_le_bytes());
    // `F6 /0 ib` with mod=01 and a zero displacement byte: the encoder has one byte-test form
    // and it always emits the disp8.
    needle.extend_from_slice(&[0xF6, 0x40, 0x00, 0x80]);
    let code = &compilation.code;
    assert_eq!(
        occurrences(code, &needle),
        1,
        "the demotion prologue is missing or duplicated; code={code:02x?}"
    );
    // The SAME address is the helper's fourth argument, so the cell is baked twice and only
    // twice: once for the prologue's test and once for the call.
    let mut argument = vec![
        0x48u8 | u8::from(EXIT_ARG_IS_EXTENDED),
        0xB8 | EXIT_ARG_LOW3,
    ];
    argument.extend_from_slice(&(cell as u64).to_le_bytes());
    assert_eq!(
        occurrences(code, &argument),
        1,
        "the fourth argument must carry the cell address"
    );
}

/// `mov <EXIT_ARG>, imm64` encoding pieces, so the assertion above names the register rather than
/// a magic byte. `EXIT_ARG` is R9 on Windows and RCX on SysV.
#[cfg(target_os = "windows")]
const EXIT_ARG_IS_EXTENDED: bool = true;
#[cfg(target_os = "windows")]
const EXIT_ARG_LOW3: u8 = 1;
#[cfg(not(target_os = "windows"))]
const EXIT_ARG_IS_EXTENDED: bool = false;
#[cfg(not(target_os = "windows"))]
const EXIT_ARG_LOW3: u8 = 1;

/// The two RESYNC stubs exist and are reached by `bt` on bits 34 and 33, in that order.
///
/// `48 0F BA E0 22` is `bt rax, 34` and `48 0F BA E0 21` is `bt rax, 33`. The ORDER is the pin:
/// the fault bit has to be tested first, because it is the one arm that must not report a
/// retirement, and a single shift would fold the two bits together.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_status_bits_are_tested_fault_first() {
    let compilation = compile_fixture(CODE, STARTS);
    let code = compilation.code;
    let fault = [0x48u8, 0x0F, 0xBA, 0xE0, 0x22];
    let retired = [0x48u8, 0x0F, 0xBA, 0xE0, 0x21];
    let fault_at = position(&code, &fault).expect("bt rax, 34 must be emitted");
    let retired_at = position(&code, &retired).expect("bt rax, 33 must be emitted");
    assert!(
        fault_at < retired_at,
        "the fault bit must be tested before the retired bit"
    );
}

/// Design item M9's other half, at the emitter: the RESYNC stubs advance EIP by ZERO.
///
/// Every other side exit in the emitter adds a compile-time delta to `cpu.eip`, because the block
/// body never moved it. These two are the exception -- the helper already left the architectural
/// next address, which for a fault is the handler's entry. The pin is behavioural rather than a
/// byte pattern, because "no advance" emits nothing to match: the fault fixture's final EIP is the
/// interpreter's, and any non-zero delta moves it.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_resync_stub_leaves_eip_delta_zero() {
    fn aim_past_the_limit(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_ebx(0xffff);
    }
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    seed_fault_handler(&mut program);
    let mut interp_bus = sixteen_bit_bus(program);
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut interp, &mut interp_bus);
    aim_past_the_limit(&mut interp, &mut interp_bus);
    // ONE interpreted instruction at a time, up to and including the POP, so the comparison is
    // against the exact boundary the block exits at rather than against the end of the run.
    for _ in 0..2 {
        interp.cycle(&mut interp_bus).expect("interpreted step");
    }
    let expected = (interp.registers.cs().selector, interp.registers.eip);

    let (mut native, mut native_bus, block) = build_native(CODE, STARTS);
    arm_fixture(&mut native, &mut native_bus);
    aim_past_the_limit(&mut native, &mut native_bus);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture must not stop the machine")
    );
    assert_eq!(
        (native.registers.cs().selector, native.registers.eip),
        expected,
        "the RESYNC stub moved EIP away from where the fault delivery left it"
    );
}

/// The three pre-existing call-out arms emit exactly the bytes they emitted before this slice.
///
/// `emit_call_out` grew two optional arguments and a whole status-branch tail. `None` has to emit
/// NOTHING, not a zero and not a skipped branch, or the port and stack-frame slots change shape on
/// a slice that is supposed to leave them alone. The pin is the `0xEC` slot's whole sequence from
/// the call through the step-break test.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_leaves_the_port_slot_bytes_unchanged() {
    // inc ax; in al, dx; inc cx; hlt. The port slot is SECOND because the compile walk refuses a
    // block whose first slot is a call-out, which is a pre-existing rule this slice does not touch.
    const PORT_CODE: &[u8] = &[0x40, 0xEC, 0x41, 0xF4];
    let compilation = compile_fixture(PORT_CODE, &[0, 1, 2]);
    assert_eq!(compilation.callout_port_slots, 1);
    assert_eq!(compilation.callout_interpret_one_slots, 0);
    let code = compilation.code;
    // `call rax` (FF D0), `add rsp, CALLOUT_CALL_FRAME`, the eight home reloads, `cmp rax, 0`,
    // `js`, `mov edx, eax`, `add [rsp+104], rdx`, `shr rax, 32`, `jnz`. The tail from `cmp` on is
    // what the new status bits sit in the middle of, so it is the part that must be contiguous.
    let tail = [
        0x48u8, 0x81, 0xF8, 0x00, 0x00, 0x00, 0x00, // cmp rax, imm32 0
        0x0F, 0x88, // js rel32
    ];
    let cmp_at = position(&code, &tail).expect("the port slot's status compare must be emitted");
    // Immediately after the `js rel32` displacement: `mov edx, eax`, `add [rsp+disp8], rdx`,
    // `shr rax, 32`. No `bt` may appear between them, which is the whole assertion: the two
    // RESYNC tests belong to the `InterpretOne` class alone.
    let after = cmp_at + tail.len() + 4;
    assert_eq!(
        &code[after..after + 2],
        &[0x89, 0xC2],
        "mov edx, eax must follow the status compare"
    );
    assert_eq!(
        &code[after + 2..after + 6],
        &[0x48, 0x01, 0x54, 0x24],
        "add [rsp+disp8], rdx must follow"
    );
    assert_eq!(
        &code[after + 7..after + 11],
        &[0x48, 0xC1, 0xE8, 0x20],
        "shr rax, 32 must follow with no bit test between: code={code:02x?}"
    );
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Compile the fixture WITHOUT installing it, for the tests that read emitted bytes. Reading them
/// out of the arena would work too, but `Compilation` already owns the vector and the cells, and
/// the cell addresses are what the byte patterns are keyed on.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn compile_fixture(code: &[u8], starts: &[u32]) -> jit::direct::Compilation {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    seed_fault_handler(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    let linears: Vec<u32> = starts.iter().map(|offset| ENTRY + offset).collect();
    warm_sixteen_bit(&mut cpu, &mut bus, &linears);
    jit::direct::compile(&mut cpu, ENTRY, false).expect("the fixture must compile as a block")
}

// ---------------------------------------------------------------------------
// 5. The S3 policy widening: one row per commit, each with its own resume fixture.
// ---------------------------------------------------------------------------

/// `mov ax,0x1111; <row>; inc ax; hlt`, plus the instruction starts the decode warm-up needs.
///
/// The same shape as `CODE` and for the same reason: the row under test sits in the MIDDLE, so a
/// resume that does not resume loses `inc ax`, and an EIP restore that does not restore lands the
/// final exit somewhere else. Every S3 row is dropped into this one program rather than getting a
/// fixture of its own, which is what makes the anti-vacuity gate below a shared assertion instead
/// of eight bespoke ones that can each be wrong in their own way.
fn row_program(row: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut code = vec![0xB8, 0x11, 0x11];
    code.extend_from_slice(row);
    code.push(0x40);
    code.push(0xF4);
    let starts = vec![0, 3, 3 + row.len() as u32];
    (code, starts)
}

/// `row_program` with a slot behind the row that BAKES FS, so a segment load in the row is compared
/// by R2 rather than relaxed away.
/// A `mov ax, Sreg` behind a segment-loading call-out, which is what puts that segment in the
/// slot's SUFFIX MASK.
///
/// Since S4f, R2 compares only the segments the slots STRICTLY AFTER a segment-writing slot
/// depend on (plus CS and SS), so a fixture whose tail is `inc ax` sees a changed record RESUME.
/// Every resync and demotion fixture in this file therefore ends with a slot that bakes the
/// segment the row loads. `0x8C` register form is the cheapest one: it reports through
/// `selector_segment`, so it pins the segment while touching no memory.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn row_program_with_an_fs_user(row: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut code = vec![0xB8, 0x11, 0x11];
    code.extend_from_slice(row);
    // mov ax, fs
    code.extend_from_slice(&[0x8C, 0xE0]);
    code.push(0xF4);
    let starts = vec![0, 3, 3 + row.len() as u32];
    (code, starts)
}

/// The anti-vacuity gate every S3 row owes: the row compiles into an `InterpretOne` slot with a
/// native slot on each side, rather than ending the block where it used to.
///
/// Without it every state comparison below would pass with the row still a hard boundary, because
/// a one-instruction block that stops at the row produces identical architectural state.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn assert_row_is_a_call_out(row: &[u8]) {
    let (code, starts) = row_program(row);
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        BLOCK_INSTRUCTIONS,
        "the block stopped early, so row {row:02x?} is still a boundary"
    );
    assert_eq!(
        block.callout_interpret_one_slots(),
        1,
        "row {row:02x?} must be an InterpretOne slot"
    );
    assert_eq!(block.callout_port_slots(), 0);
    assert_eq!(block.callout_memory_slots(), 0);
}

/// Run a row interpreted and again with the block installed, and assert the two worlds agree AND
/// that the block carried on past the row.
///
/// The retirement equality is the resume proof and it is stronger than any register check: a
/// RESYNC reports two instructions where a resume reports three, whatever the row did to the
/// register file. That matters because several S3 rows write AX, so "AX ended at 0x1112" is not a
/// claim this helper can make for all of them.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn assert_row_resumes(row: &[u8], perturb: fn(&mut CpuGsw, &mut TestBus)) -> Legs {
    assert_row_is_a_call_out(row);
    let (code, starts) = row_program(row);
    let mut legs = run_both(&code, &starts, perturb);
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.exit_reason, None,
        "row {row:02x?} should have completed the block"
    );
    assert_eq!(
        legs.native_insns,
        u64::from(BLOCK_INSTRUCTIONS),
        "the block did not resume past row {row:02x?}"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_eq!(stalls.callout_interpret_one_resync_fault, 0);
    assert_eq!(stalls.callout_interpret_one_abnormal, 0);
    legs
}

/// Distinct real-mode selectors in the three segments nothing in the fixture addresses through.
///
/// CS, SS and DS keep selector 0: the code, the stack and the `[bx]` operand all resolve through
/// them, so moving their bases would move the fixture rather than the thing under test. ES, FS and
/// GS are free, and giving them three different values is what makes the stored selector
/// observable at all -- with every segment at zero the `0x8c` store writes the zero the fixture
/// already seeded and could not be told from no store at all.
fn spread_segment_selectors(cpu: &mut CpuGsw, _: &mut TestBus) {
    cpu.load_segment_real(SegmentIndex::Es, 0x1234);
    cpu.load_segment_real(SegmentIndex::Fs, 0x5678);
    cpu.load_segment_real(SegmentIndex::Gs, 0x9abc);
}

/// Row 1: `MOV r/m16, Sreg` memory form, every Sreg the arm names.
///
/// All six of `0..=5` in one test, because they are one classifier arm and one interpreter arm:
/// the helper runs the decode line, so there is no per-segment lowering that could be right for
/// four values and wrong for two. Three of them (ES, FS, GS) carry an observable selector; the
/// other three resolve the fixture's own addressing and stay at zero, where the store is proved by
/// the RAM comparison against the interpreted leg rather than by a literal.
///
/// MUTATION: return `None` instead of the call-out for the memory form (the pre-slice behaviour)
/// and `assert_row_is_a_call_out` fails on the block shape for all six.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_mov_rm16_sreg_memory_resumes_for_every_segment() {
    for (reg, expected) in [
        (0u8, 0x1234u16),
        (1, 0x0000),
        (2, 0x0000),
        (3, 0x0000),
        (4, 0x5678),
        (5, 0x9abc),
    ] {
        // `8C /r` with mod 00 and r/m 111: `mov [bx], <sreg>`.
        let row = [0x8C, 0x07 | (reg << 3)];
        let legs = assert_row_resumes(&row, spread_segment_selectors);
        assert_eq!(
            u16::from_le_bytes(
                legs.native_bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2]
                    .try_into()
                    .unwrap()
            ),
            expected,
            "reg {reg} stored the wrong selector"
        );
    }
}

/// `/6` and `/7` stay refused, so the block still ends there.
///
/// They are not a segment at all: `segment_from_reg_field` answers them with a catch-all that
/// happens to say GS, and a block compiled around an accident is worse than the boundary it
/// replaced. This is the same refusal the register form already makes, restated from the memory
/// side because the memory arm is new.
///
/// The row sits AFTER three native slots rather than in the middle of `row_program`, because a
/// block that stops at the row would otherwise be one instruction long and `compile` refuses
/// anything under three. The `/0` control on the same program is what says the shape can hold a
/// call-out at that position, so the refusal is a refusal and not the fixture running out of
/// block.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn mov_rm16_sreg_memory_refuses_the_unnamed_reg_fields() {
    /// `mov ax,0x1111; inc ax; inc cx; <row>; inc ax; hlt`.
    fn padded(reg: u8) -> (Vec<u8>, Vec<u32>) {
        let code = vec![
            0xB8,
            0x11,
            0x11,
            0x40,
            0x41,
            0x8C,
            0x07 | (reg << 3),
            0x40,
            0xF4,
        ];
        (code, vec![0, 3, 4, 5, 7])
    }

    let (code, starts) = padded(0);
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        5,
        "the control must carry the call-out and everything after it"
    );
    assert_eq!(block.callout_interpret_one_slots(), 1);

    for reg in [6u8, 7] {
        let (code, starts) = padded(reg);
        let (_, _, block) = build_native(&code, &starts);
        assert_eq!(
            block.span().instructions,
            3,
            "0x8c /{reg} must still end the block after the three native slots"
        );
        assert_eq!(block.callout_interpret_one_slots(), 0);
    }
}

/// Run a row twenty-four times interpreted and compare `elapsed_clocks` against its named core
/// charge scaled the same number of times.
///
/// The generalisation of `pop_rm_core_clocks_is_what_the_interpreter_charges`, and it exists for
/// the reason that test states: both Approximate personas scale core clocks by one twelfth with a
/// carried remainder, so a single execution charges zero however wrong the constant is and the
/// difference only leaves the rounding after a couple of dozen. `TestBus` charges nothing for
/// fetch or data, so `elapsed_clocks` here is the core lane alone.
///
/// This is the per-row half of the `INTERPRET_ONE_MAX_CORE_CLOCKS` check: the constant is the
/// budget bound's input, and the bound and the interpreter must agree on it.
fn assert_row_charges(row: &[u8], expected: u32, seed: fn(&mut CpuGsw, &mut TestBus)) {
    const REPEATS: usize = 24;
    let mut program = vec![0u8; 0x2000];
    for index in 0..REPEATS {
        let at = ENTRY as usize + index * row.len();
        program[at..at + row.len()].copy_from_slice(row);
    }
    program[ENTRY as usize + REPEATS * row.len()] = 0xF4;
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut cpu, &mut bus);
    seed(&mut cpu, &mut bus);
    let mut oracle = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut oracle, &mut bus);

    let mut wanted = 0u64;
    for _ in 0..REPEATS {
        cpu.cycle(&mut bus).expect("the row must execute");
        wanted += oracle.scale_clocks(expected);
    }
    assert!(
        wanted > 0,
        "the fixture must clear the timing dial's rounding"
    );
    assert_eq!(
        cpu.elapsed_clocks, wanted,
        "row {row:02x?} charges something other than its named constant"
    );
}

/// Row 2: the XCHG family, all four forms.
///
/// `0x86` is the byte exchange, `0x87` the operand-width one, and `0x91..=0x97` the accumulator
/// forms; each is tested in both the shapes it has. `0x94` is in the list on purpose: it writes
/// the STACK POINTER, which is the case the call-out reload contract has to cover and the one the
/// module docs derive for POPAD.
///
/// MUTATION: drop `0x86 | 0x87 | 0x91..=0x97` from the classifier arm and every case fails on the
/// block shape; drop them from the Word allowlist instead and the same happens, because the
/// fixture runs in a 16-bit code segment where every one of them decodes at Word.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_xchg_family_resumes() {
    // xchg bl, al | xchg [bx], al | xchg bx, ax | xchg [bx], ax | xchg ax, cx | xchg ax, sp |
    // xchg ax, di
    for row in [
        &[0x86u8, 0xC3][..],
        &[0x86, 0x07],
        &[0x87, 0xC3],
        &[0x87, 0x07],
        &[0x91],
        &[0x94],
        &[0x97],
    ] {
        assert_row_resumes(row, no_perturb);
    }
}

/// The memory forms really exchange: the word the fixture seeded and the accumulator swap places.
///
/// Separate from the sweep above because it names the semantics rather than the seam. The whole
/// point of a cross-write is that BOTH sides move, and a helper that ran only the read half would
/// still resume and still agree on every clock.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_xchg_memory_form_exchanges_both_sides() {
    let legs = assert_row_resumes(&[0x87, 0x07], no_perturb);
    assert_eq!(
        u16::from_le_bytes(
            legs.native_bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2]
                .try_into()
                .unwrap()
        ),
        0x1111,
        "the accumulator must have reached memory"
    );
    // AX took the seeded zero and then `inc ax` ran.
    assert_eq!(legs.native.registers.eax() & 0xffff, 0x0001);
}

/// `0x90` keeps its native `Nop` lowering rather than joining the family it belongs to
/// architecturally.
///
/// XCHG (E)AX,(E)AX is a no-op, and an emitter that emits nothing beats a helper call that does
/// nothing. This is the one member of the encoding family the S3 arm must NOT swallow, so it is
/// pinned from the other side.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn xchg_eax_eax_stays_a_native_nop() {
    let (code, starts) = row_program(&[0x90]);
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(block.span().instructions, BLOCK_INSTRUCTIONS);
    assert_eq!(
        block.callout_interpret_one_slots(),
        0,
        "0x90 must stay a native Nop, not become a call-out"
    );
}

/// `XCHG_CORE_CLOCKS` is what the interpreter charges, on the register form and the memory form
/// alike.
#[test]
fn xchg_core_clocks_is_what_the_interpreter_charges() {
    for row in [&[0x87u8, 0xC3][..], &[0x87, 0x07], &[0x91]] {
        assert_row_charges(row, crate::XCHG_CORE_CLOCKS, |_, _| {});
    }
}

/// The other half of `assert_row_is_a_call_out`: the row is still lowered NATIVELY, so the block
/// carries it without a helper at all.
///
/// Needed wherever a slice splits one opcode between the two answers, which the bit-string family
/// is the first to do: `0F A3` register keeps its emitter at Dword and becomes a call-out at Word.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn assert_row_is_native(row: &[u8]) {
    let (code, starts) = row_program(row);
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        BLOCK_INSTRUCTIONS,
        "the block stopped early, so row {row:02x?} is a boundary rather than a lowering"
    );
    assert_eq!(
        block.callout_interpret_one_slots(),
        0,
        "row {row:02x?} must keep its native emitter, not become a call-out"
    );
}

/// Row 3: the bit-string family, every form the S3 arm takes.
///
/// `0F BA` carries the immediate index and `0F A3`/`AB`/`B3`/`BB` carry it in a register; the four
/// sub-operations differ only in whether and how they write the bit back. Both operand shapes of
/// each, because the memory shape is the one whose effective address moves with the index at
/// runtime and the register shape is the one whose index is masked to the operand width.
///
/// The memory index is `AX`, which the block's first slot sets to 0x1111, so the row addresses 546
/// bytes above `[bx]`: on the mapped data page, inside the real-mode DS limit, and away from the
/// stack. That is deliberate rather than incidental, and it is what makes these cases exercise the
/// runtime address adjustment instead of a zero offset.
///
/// MUTATION: delete the `0F AB | 0F B3 | 0F BB` opcodes from the routing arm and their four cases
/// fail on the block shape while the `0F A3` and `0F BA` cases still pass.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_bit_string_family_resumes() {
    for row in [
        // 0F BA /4../7 with mod 00 r/m 111: bt/bts/btr/btc word [bx], 3.
        &[0x0Fu8, 0xBA, 0x27, 0x03][..],
        &[0x0F, 0xBA, 0x2F, 0x03],
        &[0x0F, 0xBA, 0x37, 0x03],
        &[0x0F, 0xBA, 0x3F, 0x03],
        // The same four with mod 11 r/m 011: bt/bts/btr/btc bx, 3.
        &[0x0F, 0xBA, 0xE3, 0x03],
        &[0x0F, 0xBA, 0xEB, 0x03],
        &[0x0F, 0xBA, 0xF3, 0x03],
        &[0x0F, 0xBA, 0xFB, 0x03],
        // 0F A3/AB/B3/BB memory form: bt/bts/btr/btc [bx], ax.
        &[0x0F, 0xA3, 0x07],
        &[0x0F, 0xAB, 0x07],
        &[0x0F, 0xB3, 0x07],
        &[0x0F, 0xBB, 0x07],
        // The register forms of the three that write back, plus the Word BT the native arm
        // refuses.
        &[0x0F, 0xA3, 0xC3],
        &[0x0F, 0xAB, 0xC3],
        &[0x0F, 0xB3, 0xC3],
        &[0x0F, 0xBB, 0xC3],
    ] {
        assert_row_resumes(row, no_perturb);
    }
}

/// The width split: `0F A3` register keeps its native lowering at Dword and becomes a call-out at
/// Word.
///
/// This is the pin the routing arm's whole placement exists for. `DirectKind::Bt` carries no width
/// and the emitter masks the index with `& 31`, while at Word the interpreter masks with `& 15`.
/// Before this slice the Word form was kept out by the allowlist's silence; now the allowlist
/// admits the family and the width test lives in the arm, so it has to be visible from a test.
///
/// The Dword case is a 66-prefixed encoding inside the SAME 16-bit code segment, which is what
/// `prefixes_supported_for` admits for an operand-size override under CS.D = 0. No second harness.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn bt_register_form_splits_native_dword_from_call_out_word() {
    assert_row_is_native(&[0x66, 0x0F, 0xA3, 0xC3]);
    assert_row_is_a_call_out(&[0x0F, 0xA3, 0xC3]);
}

/// `0F BA /0../3` are not bit-test operations and stay refused.
///
/// The interpreter answers them with #UD before the operation runs. Admitting them would compile a
/// block around an instruction that can only ever fault, burning a call-out and a governor
/// execution on each one.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn bit_string_immediate_form_refuses_the_undefined_extensions() {
    for reg in 0u8..4 {
        // 0F BA /reg with mod 11 r/m 011, then three native slots ahead of it so the block that
        // stops here is long enough to compile.
        let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
        code.extend_from_slice(&[0x0F, 0xBA, 0xC3 | (reg << 3), 0x03]);
        code.extend_from_slice(&[0x40, 0xF4]);
        let starts = vec![0, 3, 4, 5, 9];
        let (_, _, block) = build_native(&code, &starts);
        assert_eq!(
            block.span().instructions,
            3,
            "0F BA /{reg} must still end the block"
        );
        assert_eq!(block.callout_interpret_one_slots(), 0);
    }
}

/// `BIT_STRING_CORE_CLOCKS` is what the interpreter charges, on both arms of the family and both
/// operand forms.
#[test]
fn bit_string_core_clocks_is_what_the_interpreter_charges() {
    for row in [
        &[0x0Fu8, 0xBA, 0xE3, 0x03][..],
        &[0x0F, 0xBA, 0x27, 0x03],
        &[0x0F, 0xA3, 0xC3],
        &[0x0F, 0xA3, 0x07],
    ] {
        assert_row_charges(row, crate::BIT_STRING_CORE_CLOCKS, |_, _| {});
    }
}

/// Row 4: group 3 at Word, `/2../7` in both operand forms plus the `/0` memory form.
///
/// The register and memory shapes of NOT, NEG, MUL, IMUL, DIV and IDIV, and TEST r/m16,imm16
/// through memory. The divides run on DX:AX over BX, which the fixture leaves at the data pointer
/// 0x1800, so the quotient fits and the row retires; the faulting case is a fixture of its own.
///
/// MUTATION: delete the Word interception at the head of the `0xf6 | 0xf7` arm and the first case
/// (`not bx`, which has no lowering at any width) fails on the block shape. The cases behind it
/// are the ones that motivate the arm's PLACEMENT rather than its existence: without the
/// interception a Word NEG reaches `NegReg` and is emitted as a 32-bit operation, which is a
/// miscompile rather than a missed lowering.
/// `group3_word_subops_join_as_call_outs_not_lowerings` (cpu_jit_test_imm_test.rs) is the
/// assertion that names that directly, by slot class rather than by state.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_group3_word_forms_resume() {
    fn seed_divisor(cpu: &mut CpuGsw, bus: &mut TestBus) {
        // DX:AX / BX with DX clear, so no divide overflows. The MEMORY divides take their divisor
        // from the word at [bx], which the shared arm seeds to zero, so it is seeded here as well:
        // without it the two memory divide cases would be a divide-by-zero fixture wearing this
        // test's name, and the fault has one of its own below.
        cpu.registers.set_edx(0);
        bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2]
            .copy_from_slice(&3u16.to_le_bytes());
    }
    for row in [
        // F7 /2../7 with mod 11 r/m 011: not/neg/mul/imul/div/idiv bx.
        &[0xF7u8, 0xD3][..],
        &[0xF7, 0xDB],
        &[0xF7, 0xE3],
        &[0xF7, 0xEB],
        &[0xF7, 0xF3],
        &[0xF7, 0xFB],
        // The same six with mod 00 r/m 111: the word at [bx].
        &[0xF7, 0x17],
        &[0xF7, 0x1F],
        &[0xF7, 0x27],
        &[0xF7, 0x2F],
        &[0xF7, 0x37],
        &[0xF7, 0x3F],
        // F7 /0 with mod 00 r/m 111: test word [bx], 0x1234.
        &[0xF7, 0x07, 0x34, 0x12],
    ] {
        assert_row_resumes(row, seed_divisor);
    }
}

/// `/0` TEST keeps its native lowering in the REGISTER form and takes the call-out only through
/// memory.
///
/// The two answers sit in one classifier arm, so a slice that widened the memory case by deleting
/// the width test rather than by routing it would silently move the register case too.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn group3_test_word_splits_native_register_from_call_out_memory() {
    // F7 /0 with mod 11 r/m 011: test bx, 0x1234.
    assert_row_is_native(&[0xF7, 0xC3, 0x34, 0x12]);
    assert_row_is_a_call_out(&[0xF7, 0x07, 0x34, 0x12]);
}

/// `/1` is not a group-3 operation and stays refused.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn group3_word_refuses_the_undefined_extension() {
    let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
    // F7 /1 with mod 11 r/m 011.
    code.extend_from_slice(&[0xF7, 0xCB]);
    code.extend_from_slice(&[0x40, 0xF4]);
    let starts = vec![0, 3, 4, 5, 7];
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        3,
        "0xF7 /1 must still end the block"
    );
    assert_eq!(block.callout_interpret_one_slots(), 0);
}

/// The RESYNC-after-fault path on a row that faults on ORDINARY DATA.
///
/// Every fault fixture before this one aimed an address past a limit, which is a property of where
/// the operand lives. A divide by zero is a property of the VALUE, so it is the first case where a
/// perfectly ordinary block, compiled around perfectly ordinary addresses, faults inside the
/// helper. The interpreted leg delivers the same #DE at the same point, so `perf.instructions`,
/// `elapsed_clocks` and the handler EIP all have an oracle.
///
/// MUTATION: report `prefix + 1` from the fault stub instead of `prefix` and the retirement
/// comparison in `assert_legs_agree` fails, because `finish_instruction` already counted the DIV.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_divide_by_zero_takes_the_fault_stub() {
    fn zero_divisor(cpu: &mut CpuGsw, bus: &mut TestBus) {
        cpu.registers.set_edx(0);
        cpu.registers.set_ebx(0);
        // Vector 0's IVT entry IS the four bytes at address zero, which is exactly where
        // `seed_fault_handler` parks the HLT it gives every OTHER vector. So #DE is the one fault
        // that cannot use the shared handler: reading it as a far pointer would send both legs to
        // 0000:00F4 and neither would ever halt. Point the vector at a handler of its own.
        bus.memory[0..4].copy_from_slice(&[0x00, 0x05, 0x00, 0x00]);
        bus.memory[0x500] = 0xF4;
    }
    // div bx
    let row = [0xF7u8, 0xF3];
    assert_row_is_a_call_out(&row);
    let (code, starts) = row_program(&row);
    let mut legs = run_both(&code, &starts, zero_divisor);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResyncFault as u32),
        "a #DE inside the helper must take the not-retired RESYNC stub"
    );
    assert_eq!(
        legs.native_insns, 1,
        "the block must report the PREFIX only: the fault path already counted the DIV"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync_fault, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_legs_agree(&mut legs);
}

/// `GROUP3_CORE_CLOCKS` is what the interpreter charges, across the sub-opcodes and both operand
/// forms.
#[test]
fn group3_core_clocks_is_what_the_interpreter_charges() {
    for row in [
        &[0xF7u8, 0xD3][..],
        &[0xF7, 0xE3],
        &[0xF7, 0x17],
        &[0xF7, 0x07, 0x34, 0x12],
    ] {
        assert_row_charges(row, crate::GROUP3_CORE_CLOCKS, |cpu, _| {
            cpu.registers.set_edx(0);
        });
    }
}

/// Row 5: INC and DEC r/m8 through memory.
///
/// The first admitted row that STORES A BYTE, which is why the deferred-code-write probe had to be
/// fixed before it could be worth anything: a byte store reaches the invalidation choke on
/// `changed` alone, so without that probe every execution here would have recorded a write, failed
/// R5 and RESYNCed. This test would have passed anyway on state, which is what the resume count in
/// `assert_row_resumes` is for.
///
/// MUTATION: put the memory arm back to `None` and both cases fail on the block shape. Revert the
/// `code_write_watched` probe in `note_code_write_inner` instead and both cases still agree on
/// every register and every byte of RAM, and fail on the resume alone. That second mutant is
/// caught HERE and by nothing else in the suite -- the generated sweep's byte store takes the
/// sized door, which pre-gates on `code_write_watched` and never reaches the window's own probe.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_inc_dec_byte_memory_resumes() {
    for (row, expected) in [(&[0xFEu8, 0x07][..], 1u8), (&[0xFE, 0x0F], 0xff)] {
        let legs = assert_row_resumes(row, no_perturb);
        assert_eq!(
            legs.native_bus.memory[POP_TARGET as usize], expected,
            "row {row:02x?} must have written the byte back"
        );
    }
}

/// The REGISTER form keeps its native lowering, and `/2../7` stay refused.
///
/// `0xFE` has three answers now -- a lowering, a call-out and a refusal -- and the arm decides
/// between them on two independent fields, so both boundaries are pinned rather than one.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn inc_dec_byte_splits_native_register_from_call_out_memory() {
    // FE /0 with mod 11 r/m 011: inc bl.
    assert_row_is_native(&[0xFE, 0xC3]);
    assert_row_is_a_call_out(&[0xFE, 0x07]);

    for reg in 2u8..8 {
        let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
        code.extend_from_slice(&[0xFE, 0x07 | (reg << 3)]);
        code.extend_from_slice(&[0x40, 0xF4]);
        let starts = vec![0, 3, 4, 5, 7];
        let (_, _, block) = build_native(&code, &starts);
        assert_eq!(
            block.span().instructions,
            3,
            "0xFE /{reg} is #UD and must still end the block"
        );
        assert_eq!(block.callout_interpret_one_slots(), 0);
    }
}

/// `INC_DEC_RM8_CORE_CLOCKS` is what the interpreter charges.
#[test]
fn inc_dec_rm8_core_clocks_is_what_the_interpreter_charges() {
    for row in [&[0xFEu8, 0x07][..], &[0xFE, 0xC3]] {
        assert_row_charges(row, crate::INC_DEC_RM8_CORE_CLOCKS, |_, _| {});
    }
}

/// Row 6: PUSH r/m16 through memory.
///
/// The first admitted row whose store goes to the STACK rather than to an address the block can
/// see, so the resume has to survive a moved pointer: `emit_store_homes` hands the helper the live
/// (E)SP and the unconditional reload picks up the decremented one, which is what lets the slot
/// after it address the stack correctly. Nothing bakes an SP value.
///
/// MUTATION: return `PushMem` for the Word form as well and the block shape assertion fails,
/// because `PushMem` is a stack kind and the compile loop's stack-width matrix has no Word cell
/// for it in either stack width.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_push_rm16_memory_resumes() {
    // FF /6 with mod 00 r/m 111: push word [bx].
    let legs = assert_row_resumes(&[0xFF, 0x37], no_perturb);
    assert_eq!(
        legs.native.registers.esp() & 0xffff,
        STACK_TOP - 2,
        "the push must have moved the stack pointer"
    );
    assert_eq!(
        u16::from_le_bytes(
            legs.native_bus.memory[STACK_TOP as usize - 2..STACK_TOP as usize]
                .try_into()
                .unwrap()
        ),
        0,
        "the pushed word is the zero the fixture seeded at [bx]"
    );
}

/// The row is WORD only: the Dword form keeps `PushMem` and is decided by the stack-width matrix,
/// not here.
///
/// In this 16-bit fixture the 66-prefixed form has no cell in that matrix (a four-byte push on a
/// 16-bit pointer is not built), so it ends the block. What the assertion pins is that it did NOT
/// become a call-out, which is the half this slice could have got wrong.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn push_rm_memory_takes_the_call_out_at_word_size_only() {
    let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
    // 66 FF /6 with mod 00 r/m 111: push dword [bx].
    code.extend_from_slice(&[0x66, 0xFF, 0x37]);
    code.extend_from_slice(&[0x40, 0xF4]);
    let starts = vec![0, 3, 4, 5, 8];
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        3,
        "the Dword form must stay with PushMem and its stack-width matrix"
    );
    assert_eq!(block.callout_interpret_one_slots(), 0);
}

/// `PUSH_RM_CORE_CLOCKS` is what the interpreter charges.
#[test]
fn push_rm_core_clocks_is_what_the_interpreter_charges() {
    assert_row_charges(&[0xFF, 0x37], crate::PUSH_RM_CORE_CLOCKS, |_, _| {});
}

/// Row 7: CLI, on both of the edges it can take.
///
/// IF 1 to 0 is the interesting one and it RESUMES, by design review M8: disabling interrupts
/// cannot make one serviceable, so the run loop has no delivery point on that edge. IF 0 to 0
/// resumes for the same reason, and it is a separate case rather than an obvious corollary because
/// R3's clause is written as "IF did not go 0 to 1" rather than as "IF did not change" -- a
/// predicate that compared IF for equality would resume on the first and resync on neither, and
/// only running both says which one is implemented.
///
/// MUTATION: change R3's IF clause to an equality (`interrupt_enable != live IF`) and the first
/// case resyncs while the second still passes.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_cli_resumes_on_both_edges() {
    fn clear_interrupt_flag(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.eflags = 0x002;
    }
    for perturb in [
        no_perturb as fn(&mut CpuGsw, &mut TestBus),
        clear_interrupt_flag,
    ] {
        let legs = assert_row_resumes(&[0xFA], perturb);
        assert_eq!(
            legs.native.eflags() & crate::FLAG_IF,
            0,
            "CLI must have cleared IF on the native leg"
        );
    }
}

/// STI JOINS the block beside CLI, which is the S4d admission.
///
/// The pin MOVED here on 2026-08-22 rather than being deleted. It used to assert the opposite,
/// on design review M8's reasoning: STI takes the IF 0-to-1 edge AND arms the interrupt shadow,
/// so it failed two clauses of R3 on every execution. Both clauses are now scoped to the row
/// (`InterpretOneRow::arms_interrupt_shadow`), and the row pays for the relaxation with a
/// pendency test the other rows do not run. The census measures it at 486 k hits against CLI's
/// 244 k, which is why it was worth reopening.
///
/// The block shape is the whole assertion: five slots means the STI joined AND the `inc` behind
/// it retired natively, which is the extension the row exists for. A block that stopped at three
/// would be the old answer.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn sti_joins_the_block_beside_the_admitted_cli() {
    assert_row_is_a_call_out(&[0xFA]);
    assert_row_is_a_call_out(&[0xFB]);

    let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
    code.push(0xFB);
    code.extend_from_slice(&[0x40, 0xF4]);
    let starts = vec![0, 3, 4, 5, 6];
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        5,
        "STI must join the block and let the instruction behind it retire natively"
    );
    assert_eq!(block.callout_interpret_one_slots(), 1);
}

/// `CLI_CORE_CLOCKS` is what the interpreter charges.
#[test]
fn cli_core_clocks_is_what_the_interpreter_charges() {
    assert_row_charges(&[0xFA], crate::CLI_CORE_CLOCKS, |_, _| {});
}

/// Row 8: MOV Sreg, r/m, the form the resume predicate actually decides.
///
/// R2 compares all six cached segment records, so this is the first admitted row whose answer
/// depends on the VALUE it loads rather than on its shape. A load that leaves the record identical
/// resumes; a load that changes it resyncs. Both are pinned here, because a predicate that always
/// resumed and one that always resynced would each pass half of this test.
///
/// The resuming cases load selector 0 into ES and DS, which the fixture already holds:
/// `sixteen_bit_code_cpu` installs them through `load_segment_real`, and `load_segment_real_mode`
/// (the ordinary real-mode `MOV Sreg` path) rebuilds the same record from the same selector while
/// preserving the limit, so the byte comparison finds nothing moved. That is the re-establishing
/// `mov ds, ax` a 16-bit C runtime emits at every function that could have changed it, which is
/// the shape the census row is made of.
///
/// MUTATION: delete the FS/GS arm and the two FS cases fail on the block shape; delete R2's
/// segment comparison instead and the resync case reports a completed block where the interpreted
/// leg is one instruction further along.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_mov_sreg_resumes_on_an_unchanged_record() {
    fn zero_the_source(cpu: &mut CpuGsw, bus: &mut TestBus) {
        cpu.registers.set_edx(0);
        bus.memory[POP_TARGET as usize..POP_TARGET as usize + 2].fill(0);
    }
    for row in [
        // 8E /0 and /3 with mod 00 r/m 111: mov es,[bx] and mov ds,[bx], both reading zero.
        &[0x8Eu8, 0x07][..],
        &[0x8E, 0x1F],
        // 8E /4 and /5 with mod 11 r/m 010: mov fs,dx and mov gs,dx, both loading zero.
        &[0x8E, 0xE2],
        &[0x8E, 0xEA],
    ] {
        assert_row_resumes(row, zero_the_source);
    }
}

/// The other side of R2: a load that MOVES the record ends the run at the slot.
///
/// FS starts at its default record (base 0, limit 0, access 0) and the row loads 0x1111, so the
/// real-mode path rebuilds base and access and the comparison sees a different segment. The block
/// stops there; the interpreter finishes the program, and the two legs still agree on everything.
///
/// This is the row's expected behaviour on a guest that really changes segments, and the governor
/// is what keeps it from being a loss: three of the first eight executions demote the slot back to
/// the boundary it replaced.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_mov_sreg_resyncs_on_a_changed_record() {
    // mov ax,0x1111 | mov fs,ax | mov ax,fs | hlt
    let row = [0x8Eu8, 0xE0];
    assert_row_is_a_call_out(&row);
    let (code, starts) = row_program_with_an_fs_user(&row);
    let mut legs = run_both(&code, &starts, no_perturb);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "a segment record that moved must RESYNC"
    );
    assert_eq!(
        legs.native_insns, 2,
        "the block must report the prefix plus the retired segment load"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync, 1);
    assert_eq!(stalls.callout_interpret_one_resync_fault, 0);
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Fs).selector,
        0x1111,
        "the load itself must have happened: a RESYNC is not an undo"
    );
}

/// The other half of the split (S4f): a changed record NO OTHER SLOT IN THE BLOCK USES resumes.
///
/// Same row, same moved record, and the only difference is what surrounds it. `mov ax, 0x1111` in
/// front and `inc ax` behind bake no segment, so FS is in neither half of the slot's mask, R2 does
/// not compare it, and the block carries on. That is 2.23 M of the S4 loader's remaining barrier
/// hits: DS and ES far-pointer reloads whose new record nothing in the block reads.
///
/// NO EARLIER SLOT either, which the first loader gate is the reason for. A mask built from the
/// suffix alone let a block that bakes FS in its PREFIX resume, and the block then failed its own
/// `data_matches` at the next entry and recompiled every visit. The sibling below is that shape.
///
/// MUTATION: drop `used_by_others` from the mask (compare all six) and this resyncs, reporting two
/// instructions instead of three.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_mov_sreg_resumes_on_a_changed_record_no_slot_uses() {
    // mov ax,0x1111 | mov fs,ax | inc ax | hlt
    let (code, starts) = row_program(&[0x8E, 0xE0]);
    let mut legs = run_both(&code, &starts, no_perturb);
    assert_eq!(legs.exit_reason, None, "the block should have completed");
    assert_eq!(
        legs.native_insns,
        u64::from(BLOCK_INSTRUCTIONS),
        "no later slot bakes FS, so the moved record cannot reach anything"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_eq!(stalls.callout_interpret_one_demoted, 0);
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Fs).selector,
        0x1111
    );
}

/// An EARLIER slot using the segment refuses the resume, and the counter says what that cost.
///
/// `mov ax, fs` in front of `mov fs, bx` is the shape the first loader gate measured as
/// `jit_direct_reject_data_segment` 307,714 -> 514,327: the suffix behind the load bakes nothing,
/// so a suffix-only mask resumed -- and then the block failed its own entry check on the next
/// visit, because `used` is the BLOCK-WIDE pinned set and the prefix put FS in it. Retire,
/// recompile, every visit.
///
/// `callout_interpret_one_resume_refused_prefix_use` is the other side of that trade, and it is
/// asserted here rather than merely exported: it is the number the `IZARRAVM_CALLOUT_SEGMENT_RESUME`
/// A/B is read against, so a counter that never fired would make the two arms look free.
///
/// MUTATION: mask with `suffix_used` instead of `used_by_others` and this resumes, reporting three
/// instructions and leaving the counter at zero.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_mov_sreg_resyncs_when_an_earlier_slot_uses_the_segment() {
    // mov ax,fs | mov fs,bx | inc ax | hlt -- the user is slot 0 and the call-out is slot 1.
    let code = [0x8C, 0xE0, 0x8E, 0xE3, 0x40, 0xF4];
    let starts = [0, 2, 4];
    let mut legs = run_both(&code, &starts, no_perturb);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "the block bakes FS in its prefix, so the moved record must RESYNC"
    );
    assert_eq!(
        legs.native_insns, 2,
        "the prefix slot plus the retired load"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync, 1);
    assert_eq!(
        stalls.callout_interpret_one_resume_refused_prefix_use, 1,
        "the suffix-only mask would have carried this one, and the ladder needs to know"
    );
    let counts = row_counts(&legs.native, "0x8e_mov_sreg");
    assert_eq!(counts.resume_refused_prefix_use, 1, "and on its own row");
    assert_legs_agree(&mut legs);
}

/// The knob's OFF arm is the pre-S4f behaviour, both halves of it.
///
/// The same fixture that resumes on the ON arm resyncs here, and the block that publishes no
/// successors on the ON arm publishes its fallthrough again. Those are the two things S4f changed,
/// and an escape that restored one without the other would be an arm nobody measured.
///
/// The arm is read ONCE PER COMPILE, so the override has to be set before `build_native` runs, not
/// before the entry.
///
/// MUTATION: read the knob at resume time instead of baking it into the cell and the first
/// assertion still passes while the successor one does not, which is the split this test exists to
/// catch.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_segment_resume_knob_restores_the_strict_rule_and_the_successors() {
    let (code, starts) = row_program(&[0x8E, 0xE0]);

    jit::direct::set_callout_segment_resume_for_test(Some(false));
    let (cpu, _, block) = build_native(&code, &starts);
    assert!(
        !block.is_segment_write_block(),
        "off arm: the block must publish its successors again"
    );
    assert_eq!(
        cpu.jit_direct.waiting_len_for_test(),
        1,
        "off arm: and queue its fallthrough"
    );
    let mut legs = run_both(&code, &starts, no_perturb);
    jit::direct::set_callout_segment_resume_for_test(None);

    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "off arm: any changed record must RESYNC"
    );
    assert_eq!(legs.native_insns, 2, "off arm");
    assert_eq!(
        legs.native
            .direct_stall_snapshot()
            .callout_interpret_one_resume_refused_prefix_use,
        0,
        "off arm: the prefix counter is about the ON arm's mask and must stay silent"
    );
    assert_legs_agree(&mut legs);

    // Control: the SAME program on the ON arm resumes and bars the successors.
    let (cpu, _, block) = build_native(&code, &starts);
    assert!(block.is_segment_write_block());
    assert_eq!(cpu.jit_direct.waiting_len_for_test(), 0);
    let legs = run_both(&code, &starts, no_perturb);
    assert_eq!(legs.exit_reason, None);
    assert_eq!(legs.native_insns, u64::from(BLOCK_INSTRUCTIONS));
}

/// The knob's spelling table, unset included. Default ON, and `0` / `off` / empty are the escape.
#[test]
fn the_segment_resume_knob_spellings() {
    use jit::direct::parse_callout_segment_resume_arm_for_test as parse;
    assert!(parse(Err(std::env::VarError::NotPresent)), "unset is ON");
    for on in ["1", "on", "ON", " on "] {
        assert!(parse(Ok(on.to_string())), "{on}");
    }
    for off in ["0", "off", "OFF", "", "  "] {
        assert!(!parse(Ok(off.to_string())), "{off}");
    }
}

/// The mask is built by SLOT INDEX and covers the whole suffix, not just the next slot.
///
///
/// The call-out sits at index 0 of three, which is the case an off-by-one gets wrong in the
/// direction that matters: a union started at `i + 2` would leave FS out of the first slot's mask
/// even though the last slot bakes it, and a changed record would resume against a stale baked
/// selector.
///
/// MUTATION: make the mask the NEXT slot's pinned set alone and this resumes, reporting three.
/// The other off-by-one -- a union that SKIPS the immediately next slot -- is killed by
/// `interpret_one_mov_sreg_resyncs_on_a_changed_record` and its protected-mode sibling, whose
/// user sits directly behind the load. The pair is what covers both directions.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_suffix_mask_reaches_past_the_next_slot() {
    // mov fs,bx | inc ax | mov ax,fs | hlt -- the call-out is slot 0 and the user is slot 2.
    //
    // BX rather than CX because the record has to MOVE: `arm_fixture` clears CX, and FS starts at
    // selector zero, so `mov fs,cx` is the re-establishing shape that resumes under any mask.
    let code = [0x8E, 0xE3, 0x40, 0x8C, 0xE0, 0xF4];
    let starts = [0, 2, 3];
    let mut legs = run_both(&code, &starts, no_perturb);
    assert_eq!(
        legs.native_insns, 1,
        "the call-out is the first slot, so a resync reports it alone"
    );
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "FS is baked two slots later, so the moved record must RESYNC"
    );
    assert_legs_agree(&mut legs);
}

/// The three illegal reg fields stay refused, from the side that can see them.
///
/// `/1`, `/6` and `/7` can only fault, so each would be a call-out that never resumes. They are
/// one bit apart from the admitted values, which is why they are asserted rather than argued.
///
/// `/2` left this list on 2026-08-22: it is the `MovSsReg` row now, with its own section below.
/// The claim it used to carry -- that a shadow-arming load cannot be a plain `MovSreg` -- is kept
/// by the row split rather than by a refusal, and `only_the_arming_rows_may_resume_with_a_step_armed_shadow`
/// is where it is pinned.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn mov_sreg_refuses_the_illegal_reg_fields() {
    for reg in [1u8, 6, 7] {
        let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
        // 8E /reg with mod 11 r/m 010 (DX).
        code.extend_from_slice(&[0x8E, 0xC2 | (reg << 3)]);
        code.extend_from_slice(&[0x40, 0xF4]);
        let starts = vec![0, 3, 4, 5, 7];
        let (_, _, block) = build_native(&code, &starts);
        assert_eq!(
            block.span().instructions,
            3,
            "0x8E /{reg} must still end the block"
        );
        assert_eq!(block.callout_interpret_one_slots(), 0);
    }
}

/// ES and DS keep their REAL-MODE register lowering, which emits no helper call at all.
///
/// The row splits four ways and this is the cell that must not move: `LoadSegReal` is three stores
/// and no call, so turning it into a call-out would be a loss on the fixture that measured it.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn mov_sreg_keeps_the_real_mode_register_lowering() {
    for row in [&[0x8Eu8, 0xC2][..], &[0x8E, 0xDA]] {
        assert_row_is_native(row);
    }
}

/// `MOV_SREG_CORE_CLOCKS` is what the interpreter charges, on the register form and the memory
/// form alike.
#[test]
fn mov_sreg_core_clocks_is_what_the_interpreter_charges() {
    for row in [&[0x8Eu8, 0xE2][..], &[0x8E, 0x07]] {
        assert_row_charges(row, crate::MOV_SREG_CORE_CLOCKS, |cpu, _| {
            cpu.registers.set_edx(0);
        });
    }
}

/// Every S3 row still refuses a 0x67 address-size prefix, at the gate that runs BEFORE `classify`.
///
/// The policy widening added arms to `classify`, and `classify` is reached only after
/// `prefixes_supported_for` and the persona gate have both passed, so the address-size refusal is
/// inherited rather than restated per row. That inheritance is the thing worth pinning: the
/// refusal is what keeps `MemoryEmitContext::address_wrap` -- a BLOCK property derived from CS.D
/// alone -- true of every slot, and a row admitted at the wrong layer would falsify it for the
/// whole block rather than for itself.
///
/// One case per shape the widening admits: memory forms with a ModRM, a two-byte opcode routed
/// above the `u8::try_from` truncation, a group-3 sub-opcode intercepted at the head of its arm,
/// and REGISTER forms, which carry no memory operand at all and are here because the refusal is
/// on the PREFIX rather than on the operand: `prefixes_supported_for` compares the whole
/// `Prefixes` struct against one that permits only the operand-size override, so an address-size
/// override refuses a row that would not have used it. Each memory case is encoded with 32-bit
/// addressing inside the 16-bit code segment, which is what the 0x67 prefix means there.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_widened_rows_still_refuse_an_address_size_prefix() {
    // THE POSITIVE CONTROL, first: the same shape unprefixed joins the block. Without it "the
    // block is three instructions" is satisfied identically by the prefix being refused and by the
    // fixture being unable to compile a fourth slot at all.
    {
        let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
        // 8C /0 with mod 00 r/m 111: mov [bx], es, the unprefixed sibling of the first row below.
        code.extend_from_slice(&[0x8C, 0x07]);
        code.extend_from_slice(&[0x40, 0xF4]);
        let (_, _, block) = build_native(&code, &[0, 3, 4, 5, 7]);
        assert_eq!(
            block.span().instructions,
            5,
            "the control must carry the whole block, or the refusals below prove nothing"
        );
        assert_eq!(block.callout_interpret_one_slots(), 1);
    }

    // Each row with a 0x67 prefix and a dword-addressed operand where it has one.
    for row in [
        // 67 8C 05 <disp32>: mov [disp32], es.
        &[0x67u8, 0x8C, 0x05, 0x00, 0x18, 0x00, 0x00][..],
        // 67 87 05 <disp32>: xchg [disp32], ax.
        &[0x67, 0x87, 0x05, 0x00, 0x18, 0x00, 0x00],
        // 67 0F BA 2D <disp32> 03: bts word [disp32], 3, a two-byte opcode.
        &[0x67, 0x0F, 0xBA, 0x2D, 0x00, 0x18, 0x00, 0x00, 0x03],
        // 67 FE 05 <disp32>: inc byte [disp32].
        &[0x67, 0xFE, 0x05, 0x00, 0x18, 0x00, 0x00],
        // 67 FF 35 <disp32>: push word [disp32].
        &[0x67, 0xFF, 0x35, 0x00, 0x18, 0x00, 0x00],
        // 67 8E 05 <disp32>: mov es, [disp32].
        &[0x67, 0x8E, 0x05, 0x00, 0x18, 0x00, 0x00],
        // 67 F7 5D 00: neg word [ebp+0], the group-3 row, intercepted at the head of its arm
        // rather than by the allowlist, so it is the one that could most easily have been routed
        // ahead of the prefix gate.
        &[0x67, 0xF7, 0x5D, 0x00],
        // 67 0F A3 05 <disp32>: bt [disp32], ax, a TWO-BYTE opcode keyed above the u8 truncation.
        &[0x67, 0x0F, 0xA3, 0x05, 0x00, 0x18, 0x00, 0x00],
        // 67 91: xchg ax, cx. A REGISTER form, which has no address to override; it refuses
        // because the prefix is present at all, which is the property under test.
        &[0x67, 0x91],
    ] {
        let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
        code.extend_from_slice(row);
        code.extend_from_slice(&[0x40, 0xF4]);
        let starts = vec![0, 3, 4, 5, 5 + row.len() as u32];
        let (_, _, block) = build_native(&code, &starts);
        assert_eq!(
            block.span().instructions,
            3,
            "row {row:02x?} carries a 0x67 prefix and must end the block"
        );
        assert_eq!(block.callout_interpret_one_slots(), 0);
    }
}

// ---------------------------------------------------------------------------
// 6. The protected-mode segment load, end to end.
// ---------------------------------------------------------------------------
//
// Row 8's protected-mode arm had compile-shape coverage only: `stack_width_kind` routes
// `LoadSegReal` to a call-out when `is_protected_mode()`, and the s5 allowlist test asserts the
// slot class. Nothing ran one. This section builds the smallest machine that can: CR0.PE, a
// hand-built GDT with four usable descriptors, and an IDT whose #GP gate lands on a HLT so a
// faulting load has somewhere to go and both legs can be driven to a halt.
//
// It is what makes `load_protected_segment` -- a descriptor fetch with type, privilege and present
// checks, an Accessed-bit write-back and three fault vectors -- an executed path rather than an
// argument about which arm was chosen.

/// The GDT and the IDT live on the DATA page, clear of the block's code.
const GDT_BASE: u32 = 0x1200;
const IDT_BASE: u32 = 0x1400;
/// Where the #GP gate points. On the code page but far below `ENTRY`, so the delivery does not
/// disturb the block and the guest still halts.
const FAULT_HANDLER: u32 = 0x0800;

/// Index 1: flat 32-bit code, the segment the fixture runs in.
const SEL_CODE: u16 = 0x08;
/// Index 2: flat 32-bit data. FS starts holding exactly this descriptor's record, so RELOADING it
/// is the case R2 admits.
const SEL_DATA: u16 = 0x10;
/// Index 3: data again, but with G clear and a 64 KB limit, so its record DIFFERS from `SEL_DATA`
/// in the one field R2 compares byte for byte.
const SEL_OTHER: u16 = 0x18;
/// Index 4: `SEL_DATA`'s twin with the Accessed bit CLEAR, so loading it makes
/// `load_protected_segment` write the descriptor back.
const SEL_UNACCESSED: u16 = 0x20;
/// Index 5: a writable data descriptor with the PRESENT bit clear. Loading it into SS raises #SS
/// (vector 12) rather than the #NP every other segment gets, which is the 386 PRM 9.3 carve-out
/// and the second fault vector the SS rows owe a test.
const SEL_NOT_PRESENT: u16 = 0x28;
/// Index 6: a writable data descriptor with D/B and G CLEAR, so loading it into SS gives a
/// 16-BIT stack. Its record differs from `SEL_DATA`'s in `default_size_32`, which is the field
/// `jit_mode_key` bit 3 keys every block on and the reason SS is in R2's mask unconditionally.
const SEL_SS16: u16 = 0x30;
/// Past the table limit, so the load is a #GP with no descriptor to blame.
const SEL_BAD: u16 = 0x38;
/// Six usable entries: `index + 7 > limit` refuses `SEL_BAD` and admits every other selector.
const GDT_LIMIT: u16 = 0x37;

/// One 8-byte descriptor, in the layout `descriptor_to_segment` reads back.
fn descriptor(low: u32, high: u32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&low.to_le_bytes());
    bytes[4..].copy_from_slice(&high.to_le_bytes());
    bytes
}

/// A 32-bit interrupt gate: present, DPL 0, through `SEL_CODE`.
fn interrupt_gate(offset: u32) -> [u8; 8] {
    let low = (u32::from(SEL_CODE) << 16) | (offset & 0xffff);
    let high = (offset & 0xffff_0000) | (0x8e << 8);
    descriptor(low, high)
}

/// Seed the descriptor tables and the fault handler into a program image.
fn seed_protected_tables(program: &mut [u8]) {
    fn put(program: &mut [u8], at: u32, bytes: [u8; 8]) {
        let at = at as usize;
        program[at..at + 8].copy_from_slice(&bytes);
    }
    // 0x00CF9B00 / 0x00CF9300: base 0, limit 0xFFFFF with G, D set. The classic flat pair.
    put(
        program,
        GDT_BASE + u32::from(SEL_CODE),
        descriptor(0x0000_ffff, 0x00cf_9b00),
    );
    put(
        program,
        GDT_BASE + u32::from(SEL_DATA),
        descriptor(0x0000_ffff, 0x00cf_9300),
    );
    // G clear, so `descriptor_to_segment` leaves the limit at 0xFFFF instead of scaling it.
    put(
        program,
        GDT_BASE + u32::from(SEL_OTHER),
        descriptor(0x0000_ffff, 0x0040_9300),
    );
    // Access 0x92 rather than 0x93: the Accessed bit is clear, which is what makes
    // `load_protected_segment` take its write-back branch.
    put(
        program,
        GDT_BASE + u32::from(SEL_UNACCESSED),
        descriptor(0x0000_ffff, 0x00cf_9200),
    );
    // Present clear, S set, type 2 (data, writable): legal for SS but not loadable.
    put(
        program,
        GDT_BASE + u32::from(SEL_NOT_PRESENT),
        descriptor(0x0000_ffff, 0x0040_1200),
    );
    // 0x00009300: access 0x93 with D/B and G clear, so `descriptor_to_segment` reports a 16-bit
    // stack with a 64 KB limit.
    put(
        program,
        GDT_BASE + u32::from(SEL_SS16),
        descriptor(0x0000_ffff, 0x0000_9300),
    );
    program[FAULT_HANDLER as usize] = 0xF4;
    // 13 is #GP and 12 is #SS. Both land on the same handler: what the fixtures compare is the two
    // LEGS of one vector against each other, not one vector against another.
    put(program, IDT_BASE + 12 * 8, interrupt_gate(FAULT_HANDLER));
    put(program, IDT_BASE + 13 * 8, interrupt_gate(FAULT_HANDLER));
}

/// A protected-mode CPU whose FS already holds `SEL_DATA`'s record.
///
/// `SegmentRegister::flat(SEL_DATA, 0x93)` is not an approximation of what the GDT entry decodes
/// to, it is the same value: `flat` gives base 0, limit 0xFFFF_FFFF, `default_size_32` true, and
/// `descriptor_to_segment` of 0x00CF9300 gives exactly those three. That equality is the whole
/// reason the reload case can resume, so it is stated rather than assumed.
fn protected_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(SEL_CODE, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(SEL_DATA, 0x93));
    }
    cpu.gdtr = DescriptorTable {
        base: GDT_BASE,
        limit: GDT_LIMIT,
    };
    cpu.idtr = DescriptorTable {
        base: IDT_BASE,
        limit: 0xff,
    };
    cpu.set_eip(ENTRY);
    cpu
}

/// `mov eax,0x1111; mov fs,dx; mov ax,fs; hlt`, with the segment load in the MIDDLE for the reason
/// `CODE` states.
///
/// The tail is `mov ax, fs` rather than `inc eax` since S4f: it BAKES FS through
/// `selector_segment`, which is what puts FS in the call-out slot's suffix mask and keeps R2
/// comparing the record these fixtures move. With `inc eax` behind it the mask is empty and a
/// changed descriptor resumes, which is the relaxation and has its own fixture.
const PROTECTED_CODE: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xE2, 0x8C, 0xE0, 0xF4];
const PROTECTED_STARTS: &[u32] = &[0, 5, 7];

/// The same fixture with `inc eax` behind the load instead, so the block pins FS NOWHERE.
///
/// Two fixtures need it, and both for the same reason: they PERTURB FS before the entry, and the
/// `mov ax, fs` tail puts FS in the block's `used` set, which `data_matches` compares at the entry
/// check. A block whose entry check refuses never runs at all, and neither of those two is about
/// the record compare -- their resync comes from R5, the deferred code write the accessed-bit
/// write-back produces.
const PROTECTED_CODE_PLAIN: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xE2, 0x40, 0xF4];
const PROTECTED_STARTS_PLAIN: &[u32] = &[0, 5, 7];

fn arm_protected(cpu: &mut CpuGsw, bus: &mut TestBus, selector: u16) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(STACK_TOP);
    cpu.registers.set_edx(u32::from(selector));
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// Build and install the protected-mode fixture over an arbitrary program.
///
/// Parameterised for the SS rows, which need `/2` and `0x17` in the middle slot where
/// `PROTECTED_CODE` puts `mov fs,dx`. The block-shape assertions stay: they are what stops a
/// fixture from silently measuring a block that ended at the row it was written for.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn build_protected_program(
    code: &[u8],
    starts: &[u32],
) -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    let linears: Vec<u32> = starts.iter().map(|offset| ENTRY + offset).collect();
    for &linear in &linears {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    cpu.set_eip(ENTRY);
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the protected-mode fixture must compile as a block");
    assert_eq!(
        compilation.span.instructions, BLOCK_INSTRUCTIONS,
        "the block stopped early, so the protected-mode segment load is still a boundary"
    );
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "the protected-mode segment load must be an InterpretOne slot"
    );
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");
    (cpu, bus, block)
}

/// Run the protected-mode fixture interpreted and again with its block installed.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn run_both_protected(selector: u16, perturb: fn(&mut CpuGsw, &mut TestBus)) -> Legs {
    run_both_protected_program(PROTECTED_CODE, PROTECTED_STARTS, selector, perturb)
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn run_both_protected_program(
    code: &[u8],
    starts: &[u32],
    selector: u16,
    perturb: fn(&mut CpuGsw, &mut TestBus),
) -> Legs {
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    seed_protected_tables(&mut program);
    let mut interp_bus = sixteen_bit_bus(program);
    let mut interp = protected_cpu();
    arm_protected(&mut interp, &mut interp_bus, selector);
    perturb(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let (mut native, mut native_bus, block) = build_protected_program(code, starts);
    arm_protected(&mut native, &mut native_bus, selector);
    perturb(&mut native, &mut native_bus);

    let before = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture block must not stop the machine"),
        "the installed block must actually run"
    );
    let native_insns = native.perf_counters().jit_direct_insns - before;
    let exit_reason = native.jit_direct.last_side_exit_reason_for_test();
    drive(&mut native, &mut native_bus);

    Legs {
        interp,
        interp_bus,
        native,
        native_bus,
        exit_reason,
        native_insns,
    }
}

/// Reloading the SAME selector onto the same descriptor resumes.
///
/// This is the shape the census row is made of and the only one that is a win: a 16-bit C runtime
/// re-establishes its data segment at every function that could have changed it, and the record
/// R2 compares does not move.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_protected_mode_segment_reload_resumes() {
    let mut legs = run_both_protected(SEL_DATA, no_perturb);
    assert_legs_agree(&mut legs);
    assert_eq!(legs.exit_reason, None, "the block should have completed");
    assert_eq!(
        legs.native_insns,
        u64::from(BLOCK_INSTRUCTIONS),
        "the block did not resume past the descriptor fetch"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Fs).selector,
        SEL_DATA
    );
}

/// A DIFFERENT descriptor moves the record, so R2 refuses and the run ends at the slot.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_protected_mode_segment_change_resyncs() {
    let mut legs = run_both_protected(SEL_OTHER, no_perturb);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "a descriptor with a different limit must RESYNC"
    );
    assert_eq!(legs.native_insns, 2, "the prefix plus the retired load");
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Fs).limit,
        0xffff,
        "the load itself must have happened, and taken the unscaled limit"
    );
}

/// A selector past the table limit is a #GP raised from inside `load_protected_segment`, delivered
/// through the IDT with the block reporting the prefix only.
///
/// The three checks that make it more than "it did not crash": the exit is the NOT-RETIRED stub,
/// the retirement count is the prefix, and every quantity in `assert_legs_agree` matches an
/// interpreted leg that delivered the same fault from the same instruction.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_protected_mode_bad_selector_takes_the_fault_stub() {
    let mut legs = run_both_protected(SEL_BAD, no_perturb);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResyncFault as u32),
        "a #GP inside the helper must take the not-retired RESYNC stub"
    );
    assert_eq!(
        legs.native_insns, 1,
        "the block must report the PREFIX only: the fault path already counted the load"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync_fault, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Fs).selector,
        SEL_DATA,
        "a faulting load must leave the old segment in place"
    );
}

/// Put `SEL_UNACCESSED`'s own record into FS before the row runs, so RELOADING it moves nothing
/// R2 compares.
///
/// Without this both Accessed-bit fixtures below would resync for the WRONG reason: FS starts on
/// `SEL_DATA`, so any load of a different index changes the selector field and R2 refuses before
/// the write-back is ever the question. The record here is what `descriptor_to_segment` produces
/// for that entry, access byte included -- 0x92, the PRE-write-back value, because the cached
/// record is built from the descriptor as it was READ and not as it was left.
fn hold_the_unaccessed_descriptor(cpu: &mut CpuGsw, _: &mut TestBus) {
    cpu.registers.set_segment(
        SegmentIndex::Fs,
        SegmentRegister::flat(SEL_UNACCESSED, 0x92),
    );
}

/// Review MAJOR 2: the descriptor's Accessed-bit WRITE-BACK is a guest memory write, and R5 has to
/// see it.
///
/// `load_protected_segment` sets bit 0 of the type field when it was clear, through
/// `write_system_linear`. That function reported nothing -- no `record_write_page`, no
/// `note_code_write` -- so a GDT entry sharing a watched page with the running block was invisible
/// to R5 and the block resumed over code the descriptor write had just changed.
///
/// The fixture marks the descriptor's own bytes as watched code. That is the honest way to build
/// it: the byte the write-back lands on has to be watched, and marking the range directly says so
/// without corrupting an instruction either leg is going to execute.
///
/// MUTATION: drop the `note_code_write` from `write_system_linear` and this resumes instead of
/// resyncing, with every register, every flag and every byte of RAM still matching the interpreted
/// leg. Only the resume count and the exit reason move, which is why both are asserted.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_accessed_bit_write_onto_watched_code_resyncs() {
    fn watch_the_descriptor(cpu: &mut CpuGsw, bus: &mut TestBus) {
        hold_the_unaccessed_descriptor(cpu, bus);
        cpu.mark_block_code_for_test(GDT_BASE + u32::from(SEL_UNACCESSED), 8);
    }
    let mut legs = run_both_protected_program(
        PROTECTED_CODE_PLAIN,
        PROTECTED_STARTS_PLAIN,
        SEL_UNACCESSED,
        watch_the_descriptor,
    );
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "the Accessed-bit write-back landed on watched code and must RESYNC"
    );
    assert_eq!(legs.native_insns, 2, "the prefix plus the retired load");
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync, 1);
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native_bus.memory[(GDT_BASE + u32::from(SEL_UNACCESSED) + 5) as usize] & 0x01,
        0x01,
        "the write-back itself must have happened: a RESYNC is not an undo"
    );
}

/// The control for the test above: the SAME descriptor, unwatched, resumes.
///
/// Without it, "the block resynced" is satisfied identically by the Accessed-bit write being seen
/// and by the descriptor simply differing from FS's record. It also pins the half of MAJOR 2 that
/// is easy to overshoot: reporting the write must not make EVERY protected-mode load resync.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_accessed_bit_write_off_watched_code_resumes() {
    let mut legs = run_both_protected_program(
        PROTECTED_CODE_PLAIN,
        PROTECTED_STARTS_PLAIN,
        SEL_UNACCESSED,
        hold_the_unaccessed_descriptor,
    );
    assert_eq!(
        legs.exit_reason, None,
        "an unwatched Accessed-bit write-back must not end the run"
    );
    assert_eq!(legs.native_insns, u64::from(BLOCK_INSTRUCTIONS));
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native_bus.memory[(GDT_BASE + u32::from(SEL_UNACCESSED) + 5) as usize] & 0x01,
        0x01,
        "the fixture is vacuous unless the write-back actually ran"
    );
}

// ---------------------------------------------------------------------------
// 7. The per-row census.
// ---------------------------------------------------------------------------

/// One row's counts out of a snapshot, by census label.
fn row_counts(cpu: &CpuGsw, label: &str) -> crate::InterpretOneRowCounts {
    let snapshot = cpu.direct_stall_snapshot();
    *snapshot
        .callout_interpret_one_rows
        .iter()
        .find(|counts| counts.row == label)
        .unwrap_or_else(|| panic!("no census row labelled {label}"))
}

/// Every `InterpretOneRow` variant is in `ALL`, and every label is distinct.
///
/// The census array is indexed by `index()`, which is the discriminant, so a variant missing from
/// `ALL` would leave a row that increments a slot nothing ever reports. A duplicated label would
/// merge two rows in the probe JSON, which is the same failure one step later.
#[test]
fn interpret_one_row_labels_cover_every_variant() {
    let labels: Vec<&'static str> = jit::direct::InterpretOneRow::ALL
        .iter()
        .map(|row| row.label())
        .collect();
    assert_eq!(labels.len(), jit::direct::InterpretOneRow::COUNT);
    let mut sorted = labels.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), labels.len(), "two rows share a census label");
    for (index, row) in jit::direct::InterpretOneRow::ALL.iter().enumerate() {
        assert_eq!(
            row.index(),
            index,
            "{} is not at its discriminant's position in ALL",
            row.label()
        );
    }
}

/// Review MAJOR 4: an execution lands on the row that was ADMITTED, and on no other.
///
/// The whole point of the split is that one bad row cannot hide behind eight good ones, so the
/// assertion is not "the right row moved" but "the right row moved AND the rest are zero". Three
/// families, each run on its own fixture: `0x8C` through memory, the XCHG family, and CLI, which
/// between them cover a store row, a register row and a row that touches no memory at all.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_executions_are_attributed_to_their_own_row() {
    for (row, label) in [
        (&[0x8Cu8, 0x07][..], "0x8c_mov_rm_sreg"),
        (&[0x87, 0x07], "0x86_87_91_97_xchg"),
        (&[0xFA], "0xfa_cli"),
    ] {
        let legs = assert_row_resumes(row, no_perturb);
        let counts = row_counts(&legs.native, label);
        assert_eq!(
            (
                counts.executed,
                counts.resync,
                counts.resync_fault,
                counts.demoted
            ),
            (1, 0, 0, 0),
            "row {label} must carry the execution"
        );
        let total: u64 = legs
            .native
            .direct_stall_snapshot()
            .callout_interpret_one_rows
            .iter()
            .map(|counts| counts.executed)
            .sum();
        assert_eq!(
            total, 1,
            "row {label}'s execution must not have landed on a second row as well"
        );
        assert_eq!(
            legs.native
                .direct_stall_snapshot()
                .callout_interpret_one_executed,
            total,
            "the scalar must be the sum of the per-row column"
        );
    }
}

/// A RESYNC is attributed too, which is the column the plan's refutation rule actually reads.
///
/// `0x8E` is the row whose resync rate decides whether it stays on the allowlist, so it is the one
/// worth pinning: a segment load that moves the record must charge `0x8e_mov_sreg` and nothing
/// else.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_resyncs_are_attributed_to_their_own_row() {
    let (code, starts) = row_program_with_an_fs_user(&[0x8E, 0xE0]);
    let legs = run_both(&code, &starts, no_perturb);
    let counts = row_counts(&legs.native, "0x8e_mov_sreg");
    assert_eq!(
        (counts.executed, counts.resync, counts.resync_fault),
        (1, 1, 0),
        "the segment load's resync must land on its own row"
    );
    let snapshot = legs.native.direct_stall_snapshot();
    let resyncs: u64 = snapshot
        .callout_interpret_one_rows
        .iter()
        .map(|counts| counts.resync)
        .sum();
    assert_eq!(
        resyncs, snapshot.callout_interpret_one_resync,
        "the scalar must be the sum of the per-row column"
    );
    assert_eq!(resyncs, 1, "no other row may have been charged");
}

/// A #DE inside the helper is attributed to the group-3 row, not to the family at large.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_faults_are_attributed_to_their_own_row() {
    fn zero_divisor(cpu: &mut CpuGsw, bus: &mut TestBus) {
        cpu.registers.set_edx(0);
        cpu.registers.set_ebx(0);
        bus.memory[0..4].copy_from_slice(&[0x00, 0x05, 0x00, 0x00]);
        bus.memory[0x500] = 0xF4;
    }
    let (code, starts) = row_program(&[0xF7, 0xF3]);
    let legs = run_both(&code, &starts, zero_divisor);
    let counts = row_counts(&legs.native, "0xf7_group3_word");
    assert_eq!(
        (counts.executed, counts.resync, counts.resync_fault),
        (1, 0, 1),
        "the divide fault must land on the group-3 row"
    );
}

// ---------------------------------------------------------------------------
// 8. CLI in V86, where it is the one admitted row that can fault on its own.
// ---------------------------------------------------------------------------

/// CLI below IOPL 3 in a V86 task raises #GP from inside the helper, and the window stays open
/// across the delivery.
///
/// This is the case the classifier's `0xfa` arm claims and nothing ran: `check_v86_iopl` is the
/// FIRST statement of the interpreter's own arm, which is the arm the helper runs, so a V86 guest
/// below IOPL 3 must raise the same #GP from inside a call-out that it raised at the barrier. Every
/// other CLI fixture is real mode, where CLI cannot fault at all.
///
/// The block holds no memory slot -- `mov ax,imm`, the call-out, `inc ax` -- which is what makes a
/// V86 fixture affordable here: nothing needs the fast map, so the whole machine is
/// `v86_world`'s paging, GDT, IDT and TSS with the block compiled at `d = false`.
///
/// THE WINDOW EVIDENCE is `callout_deferred_code_writes`, the same counter
/// `interpret_one_window_stays_open_across_fault_delivery` uses and for the same reason: the
/// deferral is invisible from outside, because the drain makes the outcome identical either way.
/// The fixture marks the ring-0 stack the delivery frame lands on as watched code, so those pushes
/// have to be deferred rather than reach `invalidate_physical_range` with the block's native frame
/// live on the host stack. A real V86 monitor's stack is not code; marking it is how the hazard is
/// made reachable without contriving a tiny-model layout.
///
/// MUTATION: close the window before `finish_instruction` on the fault arm and the counter reads
/// zero, which is the shape design review round 2's BLOCKER 1 had.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_cli_faults_in_v86_with_the_window_open() {
    /// `mov ax,0x1111; cli; inc ax; hlt` at the V86 guest's CS:0.
    const V86_CODE: &[u8] = &[0xB8, 0x11, 0x11, 0xFA, 0x40, 0xF4];
    /// Where `v86_world` puts the guest image, which is `enter_v86_direct`'s CS base.
    const V86_BASE: u32 = 0xA000;
    /// The monitor's HLT, so the delivered #GP halts instead of running off.
    const MONITOR: &[u8] = &[0xF4];

    let (mut cpu, mut bus) = super::super::v86_world(MONITOR, V86_CODE, &[0x00]);
    super::super::enter_v86_direct(&mut cpu, 0, 0x1000);
    // IOPL 0, which `enter_v86_direct` already sets, is the whole condition: at IOPL 3 the same
    // instruction would succeed and resume. Stated rather than inherited.
    assert_eq!(cpu.registers.eflags & 0x3000, 0, "the fixture needs IOPL 0");
    assert!(cpu.is_v86_mode());

    for offset in [0u32, 3, 4] {
        let linear = V86_BASE + offset;
        cpu.set_eip(offset);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    cpu.set_eip(0);

    let compilation = jit::direct::compile(&mut cpu, V86_BASE, false)
        .expect("the V86 fixture must compile as a block");
    assert_eq!(
        compilation.span.instructions, BLOCK_INSTRUCTIONS,
        "the block must carry the CLI and the slot after it"
    );
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "CLI must be the block's call-out slot"
    );
    let key = jit::direct::key_for(&cpu, V86_BASE, false).expect("a key for the V86 block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the V86 block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");

    // The delivery frame is ten dwords below ESP0 on the ring-0 stack. Marking it watched is what
    // makes the deferral observable; see the note above.
    cpu.mark_block_code_for_test(0x7000 - 40, 40);

    cpu.set_eip(0);
    cpu.registers.set_eax(0);
    let before = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block)
            .expect("a delivered #GP must not stop the machine"),
        "the block must run"
    );

    assert_eq!(
        cpu.jit_direct.last_side_exit_reason_for_test(),
        Some(jit::direct::SideExitReason::CallOutResyncFault as u32),
        "a V86 CLI below IOPL 3 must take the not-retired RESYNC stub"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_insns - before,
        1,
        "the block must report the PREFIX only: the fault path already counted the CLI"
    );
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_resync_fault, 1);
    assert_eq!(
        row_counts(&cpu, "0xfa_cli").resync_fault,
        1,
        "the fault must be attributed to the CLI row"
    );
    assert!(
        stalls.callout_deferred_code_writes > 0,
        "the delivery's pushes must have been DEFERRED, so the window was still open"
    );

    // The fault really was delivered, and to the monitor.
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(
        cpu.registers.cs().selector,
        0x08,
        "the ring-0 code selector"
    );
    assert_eq!(cpu.registers.eip, 0x8000, "the monitor's entry point");
    assert_eq!(
        cpu.registers.eflags & crate::FLAG_IF,
        0,
        "an interrupt gate clears IF on entry, so the guest's own CLI never ran"
    );
}

/// The demoted-site boundary on the row that actually demotes: the protected-mode segment load,
/// through `MOV ES,r16` and not `MOV FS,r16`.
///
/// The register choice is the whole point, and it is not interchangeable. `classify` admits FS and
/// GS as call-outs on its own, so a FS fixture reaches the mechanism through the same path the
/// 16-bit `POP r/m` one does and proves nothing new. ES and DS in REGISTER form arrive as
/// `LoadSegReal` and only become a `CallOut` inside `stack_width_kind`, once the mode is in hand --
/// which is below where a demoted-site check naturally wants to sit, beside the
/// `PlannedInsn::HardBoundary` arm.
///
/// That is where it was written first, and the tombraid loader then measured 402,264 demotions and
/// 716,777 compile attempts with the mechanism "on", byte-identical to the run without it, because
/// every site it demotes is this form. This fixture is the one that fails on that placement.
///
/// `SEL_OTHER` is the driver: a descriptor whose record differs in the field R2 compares, so the
/// load retires and the block resyncs, every time.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_demoted_protected_mode_segment_load_recompiles_as_a_hard_boundary() {
    /// `mov eax,0x1111; mov es,dx; mov ax,es; hlt`. The tail BAKES ES, which is what keeps
    /// the moved record inside the slot's suffix mask; see `PROTECTED_CODE`.
    const CODE_ES: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xC2, 0x8C, 0xC0, 0xF4];
    const STARTS_ES: &[u32] = &[0, 5, 7];

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE_ES.len()].copy_from_slice(CODE_ES);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    let warm = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        for offset in STARTS_ES {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).expect("fixture decode");
        }
        cpu.set_eip(ENTRY);
    };
    warm(&mut cpu, &mut bus);

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the ES fixture must compile as a block");
    assert_eq!(
        compilation.span.instructions, BLOCK_INSTRUCTIONS,
        "control: the load joins the block instead of ending it"
    );
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "control: and it joins as an InterpretOne slot"
    );
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");

    for _ in 0..3 {
        arm_protected(&mut cpu, &mut bus, SEL_OTHER);
        // Put ES back where `protected_cpu` seeds it. Without this the SECOND load of `SEL_OTHER`
        // is a re-establishing load, which is exactly the case R2 RESUMES, and the governor would
        // never see its third resync.
        cpu.registers
            .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a resyncing load must not stop the machine")
        );
        assert_eq!(
            cpu.jit_direct.last_side_exit_reason_for_test(),
            Some(jit::direct::SideExitReason::CallOutResync as u32),
            "a descriptor with a different limit must RESYNC"
        );
    }
    assert_eq!(
        cpu.direct_stall_snapshot().callout_interpret_one_demoted,
        1,
        "three resyncs in the first eight executions demote the slot"
    );
    assert_eq!(
        cpu.jit_direct.demoted_callout_site_count_for_test(),
        1,
        "and the demotion must reach the site map from the protected-mode arm too"
    );
    assert!(
        !cpu.jit_direct.retire_key_for_recompile(key),
        "the demotion must already have retired the block"
    );

    // Put the CPU back exactly where the control compile ran from, decode lines included, so the
    // ONLY difference between that Compiled outcome and this one is the demoted site.
    arm_protected(&mut cpu, &mut bus, SEL_OTHER);
    cpu.registers
        .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
    warm(&mut cpu, &mut bus);
    assert!(
        matches!(
            jit::direct::compile(&mut cpu, ENTRY, true),
            jit::direct::CompileOutcome::StructuralReject(_)
        ),
        "the recompile must end before the demoted slot. One instruction is left in front of it,          which is under the walk's three-instruction floor, so the boundary shows up as the whole          key refusing -- the same structural reject this key produced before 0x8E was admitted.          What matters is that it is not Compiled: with the site check disabled, or placed above          stack_width_kind where this call-out does not exist yet, the same call returns a          three-slot block carrying the slot again"
    );
}

/// A demotion inside a CHAINED successor must retire that successor, not the block the dispatcher
/// entered.
///
/// The defect this pins: the retire latch used to be a bool, and `run_direct_block` acted on it
/// with its OWN `span.key` -- the ROOT of the chain. A linked transfer runs the successor without
/// returning to Rust, so a slot demoted in the successor retired the root instead, and the
/// successor kept its slot with the cell already latched. `note_execution` fires once per cell and
/// never asks again, so that block would exit abnormally for the rest of its life while the
/// recompile of an innocent root learned nothing. The key is now baked into the cell at compile
/// time (`InterpretOneCell::key`) and the latch carries it.
///
/// Two fixture choices, each forced:
///
/// * PROTECTED 32-bit, not the 16-bit world most of this file uses. A chain needs a static link,
///   and `classify` admits no control transfer at Word operand size -- which is every instruction
///   in a CS.D = 0 segment -- so block A could not end in a jump at all.
/// * `MOV ES,dx` with `SEL_OTHER`, so the resync is R2 refusing a record that moved. A FAULTING
///   driver reloads CS through the IDT and invalidates the code caches, which clears the block
///   cache and the links with it: the chain would be rebuilt every traversal and never taken.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_demotion_in_a_chained_successor_retires_the_successor_and_not_the_root() {
    // Block A: three `inc eax` and a jump into B. Block B: three `inc eax`, the segment load, a
    // `mov ax,es` behind it, then HLT. `jmp +0` lands on the instruction after it, which is B's
    // entry. The tail reads ES back deliberately: since S4f that is what keeps the moved record
    // inside the slot's suffix mask and therefore inside R2's compare.
    const CODE: &[u8] = &[
        0x40, 0x40, 0x40, // block A                    +0 +1 +2
        0xEB, 0x00, // jmp +0, terminal, static link     +3
        0x40, 0x40, 0x40, // block B                     +5 +6 +7
        0x8E, 0xC2, // mov es,dx -- the call-out slot    +8
        0x8C, 0xC0, // mov ax,es -- mid-block, and BAKES ES so the suffix mask holds it  +10
        0xF4, //                                         +12
    ];
    const STARTS: &[u32] = &[0, 1, 2, 3, 5, 6, 7, 8, 10, 12];
    let entry_b = ENTRY + 5;
    let slot = ENTRY + 8;

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    for &offset in STARTS {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }

    let key_a = jit::direct::key_for(&cpu, ENTRY, true).expect("block A key");
    let key_b = jit::direct::key_for(&cpu, entry_b, true).expect("block B key");
    let mut blocks = Vec::new();
    for (entry, key, slots, call_outs) in [(ENTRY, key_a, 4u8, 0u8), (entry_b, key_b, 5, 1)] {
        let compilation =
            jit::direct::compile(&mut cpu, entry, true).expect("both fixture blocks must compile");
        assert_eq!(compilation.span.instructions, slots);
        assert_eq!(compilation.callout_interpret_one_slots, call_outs);
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let id = cpu
            .jit_direct
            .install(&compilation)
            .expect("install the fixture block");
        blocks.push(cpu.jit_direct.block(id).expect("the block must be live"));
    }
    let (block_a, block_b) = (blocks[0], blocks[1]);

    let arm = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        arm_protected(cpu, bus, SEL_OTHER);
        cpu.registers
            .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
    };

    // Three traversals of A. Its jump is a STATIC link and both blocks are installed, so the
    // transfer into B happens inside the native run on every one of them: the entry that demotes
    // therefore has A's key on its span and B's key on the cell.
    let transfers = cpu.perf_counters().jit_direct_linked_transfers;
    for _ in 0..3 {
        arm(&mut cpu, &mut bus);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block_a)
                .unwrap()
        );
        assert_eq!(
            cpu.registers.eip,
            slot + 2,
            "the run must end where B's call-out resynced, not where A ends"
        );
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers,
        3,
        "every traversal must reach B through the CHAIN, not a second entry"
    );
    assert_eq!(
        cpu.direct_stall_snapshot().callout_interpret_one_resync,
        3,
        "three resyncs is what the governor demotes on"
    );
    assert_eq!(cpu.direct_stall_snapshot().callout_interpret_one_demoted, 1);
    let _ = block_b;

    assert!(
        !cpu.jit_direct.key_is_compiled_for_test(key_b),
        "the demoted slot's OWN block must be the one retired"
    );
    assert!(
        cpu.jit_direct.key_is_compiled_for_test(key_a),
        "and the root of the chain must be untouched: it carries no call-out at all"
    );
    assert_eq!(
        cpu.jit_direct.demoted_callout_site_count_for_test(),
        1,
        "the site must be filed under block B's key, which is what the recompile asks with"
    );

    // And B re-walks with the boundary: three `inc eax` in front of the slot, then it ends.
    arm(&mut cpu, &mut bus);
    for &offset in STARTS {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    let recompiled = jit::direct::compile(&mut cpu, entry_b, true)
        .expect("block B must still compile as its prefix");
    assert_eq!(recompiled.callout_interpret_one_slots, 0);
    assert_eq!(recompiled.span.instructions, 3);
    assert_eq!(u32::from(recompiled.span.guest_len), slot - entry_b);
}

/// The demoted site must be filed under the mode key the BLOCK was compiled with, not under
/// whatever the CPU holds when the demotion is noticed.
///
/// V86 CLI is the fixture because it is the one admitted row whose demotion path CHANGES the mode
/// key before the demotion is recorded: the #GP is delivered through the monitor's interrupt gate
/// from inside the helper, so by the time `note_demotion` runs, VM is clear and CS is the ring-0
/// 32-bit selector -- `jit_mode_key` bits 2 and 0, both moved. A live read files the site under a
/// key no compile walk ever asks for, which is an inert entry AND, because the retire still fires,
/// a demote/retire/recompile treadmill on every later execution.
///
/// Nothing INSIDE a block can move the mode key on the non-faulting path, and that is not an
/// accident worth relying on: `classify`'s `0x8e` arm refuses `/2` (SS) outright, so no admitted
/// row moves SS.B, and none moves CS.D, PE or PG either. The fault arm is the reachable case, and
/// it is the one this fixture takes.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_demoted_site_is_filed_under_the_blocks_mode_key_and_not_the_live_one() {
    /// `mov ax,0x1111; cli; inc ax; hlt` at the V86 guest's CS:0.
    const V86_CODE: &[u8] = &[0xB8, 0x11, 0x11, 0xFA, 0x40, 0xF4];
    const V86_BASE: u32 = 0xA000;
    const MONITOR: &[u8] = &[0xF4];

    let (mut cpu, mut bus) = super::super::v86_world(MONITOR, V86_CODE, &[0x00]);
    super::super::enter_v86_direct(&mut cpu, 0, 0x1000);
    let warm = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        for offset in [0u32, 3, 4] {
            cpu.set_eip(offset);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, V86_BASE + offset)
                .expect("fixture decode");
        }
        cpu.set_eip(0);
    };
    warm(&mut cpu, &mut bus);

    let compilation = jit::direct::compile(&mut cpu, V86_BASE, false)
        .expect("the V86 fixture must compile as a block");
    assert_eq!(compilation.span.instructions, BLOCK_INSTRUCTIONS);
    assert_eq!(
        compilation.callout_interpret_one_slots, 1,
        "control: CLI is the block's call-out slot"
    );
    let key = jit::direct::key_for(&cpu, V86_BASE, false).expect("a key for the V86 block");
    let v86_mode_key = key.mode_key;
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the V86 block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");

    // Three faulting executions. Each one delivers the #GP to the monitor, so the guest has to be
    // put back into V86 before the next.
    for _ in 0..3 {
        super::super::enter_v86_direct(&mut cpu, 0, 0x1000);
        warm(&mut cpu, &mut bus);
        cpu.registers.set_eax(0);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a delivered #GP must not stop the machine")
        );
        assert_eq!(
            cpu.jit_direct.last_side_exit_reason_for_test(),
            Some(jit::direct::SideExitReason::CallOutResyncFault as u32)
        );
        assert!(
            !cpu.is_v86_mode(),
            "the delivery must have left V86, which is what moves the mode key under the demotion"
        );
        assert_ne!(
            cpu.jit_mode_key(),
            v86_mode_key,
            "and the live mode key must therefore differ from the block's, or this fixture is \
             asserting nothing"
        );
    }
    assert_eq!(cpu.direct_stall_snapshot().callout_interpret_one_demoted, 1);
    assert_eq!(cpu.jit_direct.demoted_callout_site_count_for_test(), 1);

    // The recompile asks with the V86 key, and must find the site.
    super::super::enter_v86_direct(&mut cpu, 0, 0x1000);
    warm(&mut cpu, &mut bus);
    assert_eq!(
        jit::direct::key_for(&cpu, V86_BASE, false)
            .expect("the V86 key")
            .mode_key,
        v86_mode_key,
        "the guest is back where it was, so the walk asks with the same key it compiled under"
    );
    assert!(
        matches!(
            jit::direct::compile(&mut cpu, V86_BASE, false),
            jit::direct::CompileOutcome::StructuralReject(_)
        ),
        "the recompile must end before the demoted CLI. One instruction is left in front of it, \
         under the walk's three-instruction floor, so the boundary shows up as the whole key \
         refusing. Filed under the monitor's mode key instead, this call returns a block carrying \
         the call-out again"
    );
}

/// A demotion the site cap REFUSES to record must not retire anything.
///
/// The defect this pins: the retire fired unconditionally while `note_demoted_callout_site`
/// returned silently at `DEMOTED_CALLOUT_SITE_CAP`. Past the cap the recompile has nothing to
/// consult, so it puts the slot straight back, the fresh cell demotes, and the block is retired
/// again -- a compile per three executions, for ever. What the cap is supposed to buy is a bounded
/// loss: the block keeps its slot and pays the emitted prologue test, which is the cost the
/// mechanism was invented to remove but is still finite.
///
/// The precedent is `X87_TOP_RETIRE_CAP`, which suppresses the RETIRE and not the refusal, for the
/// same reason: a retire whose recompile undoes it is a treadmill.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_demotion_past_the_site_cap_keeps_its_block_instead_of_recompiling_for_ever() {
    /// `mov eax,0x1111; mov es,dx; inc eax; hlt`, the shape `PROTECTED_CODE` uses with FS.
    const CODE_ES: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xC2, 0x8C, 0xC0, 0xF4];
    const STARTS_ES: &[u32] = &[0, 5, 7];

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE_ES.len()].copy_from_slice(CODE_ES);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    for offset in STARTS_ES {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    cpu.set_eip(ENTRY);

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the ES fixture must compile as a block");
    assert_eq!(compilation.callout_interpret_one_slots, 1);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");

    cpu.jit_direct.fill_demoted_callout_sites_for_test();
    let filled = cpu.jit_direct.demoted_callout_site_count_for_test();

    let run = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        arm_protected(cpu, bus, SEL_OTHER);
        cpu.registers
            .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
        assert!(
            cpu.try_run_direct_block_for_test(bus, block)
                .expect("a resyncing load must not stop the machine")
        );
    };
    for _ in 0..3 {
        run(&mut cpu, &mut bus);
    }
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_interpret_one_demoted, 1,
        "the governor still demotes: the cap is about what happens NEXT"
    );
    assert_eq!(
        stalls.demoted_callout_sites_refused, 1,
        "and the cap must have refused the site, or this fixture is testing the ordinary path"
    );
    assert_eq!(
        cpu.jit_direct.demoted_callout_site_count_for_test(),
        filled,
        "a refused site must not be inserted"
    );
    assert!(
        cpu.jit_direct.key_is_compiled_for_test(key),
        "and the block must NOT have been retired: its recompile would put the slot straight back"
    );

    // Later executions take the demoted prologue and exit abnormally. That is the bounded loss the
    // cap accepts. What must not happen is another demotion, which is what a retire-and-recompile
    // treadmill looks like from here.
    let abnormal = cpu.direct_stall_snapshot().side_exit_callout_abnormal;
    for _ in 0..4 {
        run(&mut cpu, &mut bus);
        assert_eq!(
            cpu.jit_direct.last_side_exit_reason_for_test(),
            Some(jit::direct::SideExitReason::CallOutAbnormal as u32)
        );
    }
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        stalls.side_exit_callout_abnormal - abnormal,
        4,
        "the prologue test is the whole of what a capped-out demotion costs"
    );
    assert_eq!(
        stalls.callout_interpret_one_demoted, 1,
        "one demotion, not one per three executions"
    );
    assert_eq!(stalls.demoted_callout_sites_refused, 1);
    assert!(cpu.jit_direct.key_is_compiled_for_test(key));
}

/// A demotion on an entry that ends in a machine-stopping `CpuError` must not leave its retire
/// request behind for the NEXT entry to act on.
///
/// The defect this pins: `run_direct_block` returned `Err` on `callout_error` before it took the
/// retire latch, so the latch survived the return. The following entry -- any entry, in any block,
/// possibly much later -- then found it set and retired on it. With the pre-fix bool latch that
/// retired whatever block happened to be running; with the key it retires the right block at the
/// wrong time, which is still a block cache that changes shape for a reason nothing at that call
/// site can explain.
///
/// The error is reached by breaking the IDT under a faulting segment load: the #GP raised inside
/// `load_protected_segment` has no gate to be delivered through, which escalates until the machine
/// stops. Two clean resyncs first, so the third -- the faulting one -- is the execution that
/// demotes.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_demotion_on_a_stopping_entry_does_not_leak_its_retire_to_the_next_one() {
    /// `mov eax,0x1111; mov es,dx; mov ax,es; hlt`. The tail BAKES ES, which is what keeps
    /// the moved record inside the slot's suffix mask; see `PROTECTED_CODE`.
    const CODE_ES: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xC2, 0x8C, 0xC0, 0xF4];
    const STARTS_ES: &[u32] = &[0, 5, 7];

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE_ES.len()].copy_from_slice(CODE_ES);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    for offset in STARTS_ES {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    cpu.set_eip(ENTRY);

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the ES fixture must compile as a block");
    assert_eq!(compilation.callout_interpret_one_slots, 1);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");

    let arm = |cpu: &mut CpuGsw, bus: &mut TestBus, selector: u16| {
        arm_protected(cpu, bus, selector);
        cpu.registers
            .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
    };
    for _ in 0..2 {
        arm(&mut cpu, &mut bus, SEL_OTHER);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a resyncing load must not stop the machine")
        );
    }
    assert_eq!(cpu.direct_stall_snapshot().callout_interpret_one_demoted, 0);

    // The third execution: a selector past the table limit raises #GP inside the helper, and an
    // empty IDT gives that #GP nowhere to go.
    arm(&mut cpu, &mut bus, SEL_BAD);
    cpu.idtr.limit = 0;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).is_err(),
        "the fixture must actually reach the machine-stopping path"
    );
    assert_eq!(
        cpu.direct_stall_snapshot().callout_interpret_one_demoted,
        1,
        "and it must be the execution that demoted the slot"
    );
    assert!(
        cpu.jit_direct.key_is_compiled_for_test(key),
        "the stopping entry drops its retire: the machine is going down, not recompiling"
    );

    // The entry that must not inherit it. The slot is demoted, so this one exits from the emitted
    // prologue and touches nothing else.
    cpu.idtr.limit = 0xff;
    arm(&mut cpu, &mut bus, SEL_OTHER);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block)
            .expect("a demoted slot exits abnormally, it does not stop the machine")
    );
    assert_eq!(
        cpu.jit_direct.last_side_exit_reason_for_test(),
        Some(jit::direct::SideExitReason::CallOutAbnormal as u32)
    );
    assert!(
        cpu.jit_direct.key_is_compiled_for_test(key),
        "this entry demoted nothing, so it must retire nothing"
    );
}

/// Overwriting a demoted site's code must clear the judgement with it.
///
/// The defect this pins: `invalidate_physical_range` dropped `entries` and `top_mismatch_retires`
/// for a genuinely killed key and left the demoted-site map alone, so an overlay written over that
/// address inherited the previous occupant's ban. Nothing lifts it short of a whole-cache wipe, and
/// nothing reports it -- the new instruction simply never becomes a call-out.
///
/// The retain lives at the top of `invalidate_physical_range` and not in its genuine-kill arm,
/// which is what makes this fixture meaningful: the map is keyed on the INSTRUCTION and outlives
/// the block, so a site whose block has already been retired -- exactly what a demotion leaves
/// behind -- has no key left for that arm to visit.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn overwriting_a_demoted_sites_code_lets_it_be_a_call_out_again() {
    /// `mov eax,0x1111; mov es,dx; mov ax,es; hlt`. The tail BAKES ES, which is what keeps
    /// the moved record inside the slot's suffix mask; see `PROTECTED_CODE`.
    const CODE_ES: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xC2, 0x8C, 0xC0, 0xF4];
    const STARTS_ES: &[u32] = &[0, 5, 7];
    let slot = ENTRY + 5;

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE_ES.len()].copy_from_slice(CODE_ES);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    let warm = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        for offset in STARTS_ES {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).expect("fixture decode");
        }
        cpu.set_eip(ENTRY);
    };
    warm(&mut cpu, &mut bus);

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the ES fixture must compile as a block");
    assert_eq!(compilation.callout_interpret_one_slots, 1);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");

    for _ in 0..3 {
        arm_protected(&mut cpu, &mut bus, SEL_OTHER);
        cpu.registers
            .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a resyncing load must not stop the machine")
        );
    }
    assert_eq!(cpu.jit_direct.demoted_callout_site_count_for_test(), 1);
    arm_protected(&mut cpu, &mut bus, SEL_OTHER);
    cpu.registers
        .set_segment(SegmentIndex::Es, SegmentRegister::flat(SEL_DATA, 0x93));
    warm(&mut cpu, &mut bus);
    assert!(
        matches!(
            jit::direct::compile(&mut cpu, ENTRY, true),
            jit::direct::CompileOutcome::StructuralReject(_)
        ),
        "control: the site is banned before the overwrite"
    );

    // The overlay: the same two bytes written back over the segment load. `note_code_write` is the
    // door every SMC store reaches, and the block it would have killed is already gone.
    assert!(cpu.note_code_write(slot, 2));
    assert_eq!(
        cpu.jit_direct.demoted_callout_site_count_for_test(),
        0,
        "the write must have taken the judgement about that code with it"
    );

    warm(&mut cpu, &mut bus);
    let recompiled = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the code at that address is new, so the walk starts over");
    assert_eq!(
        recompiled.callout_interpret_one_slots, 1,
        "and the instruction there may be a call-out again"
    );
}

// ---------------------------------------------------------------------------
// 10. The STI row (S4d), and the interrupt shadow it leaves behind.
//
// Every other row must find `interrupt_shadow` clear after its step. STI always arms it, and a
// native block has no point at which the interpreter would consume it. The mechanism is that the
// helper NEVER clears the flag: it latches the EIP its slot left behind and `run_direct_block`
// decides at the boundary. The two fixtures that matter are the two answers that decision can
// give, and design review 10.1's blocker B1 is the second one.
// ---------------------------------------------------------------------------

/// Run a fixture's block ONCE and stop at the boundary, before the interpreted tail.
///
/// `run_both` drives past the block on both legs, which is right for a state comparison and wrong
/// for every question here: the interpreted leg's next instruction consumes the shadow, so by the
/// time it stops the two legs agree whatever the boundary did. These tests have to look at the
/// CPU while the block's own answer is still the last thing that happened.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn run_block_to_boundary(
    code: &[u8],
    starts: &[u32],
    perturb: fn(&mut CpuGsw, &mut TestBus),
) -> (CpuGsw, TestBus, u64, jit::direct::CompiledBlock) {
    let (mut cpu, mut bus, block) = build_native(code, starts);
    arm_fixture(&mut cpu, &mut bus);
    perturb(&mut cpu, &mut bus);
    let before = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block)
            .expect("the fixture block must not stop the machine"),
        "the installed block must actually run"
    );
    let native_insns = cpu.perf_counters().jit_direct_insns - before;
    (cpu, bus, native_insns, block)
}

fn clear_if(cpu: &mut CpuGsw, _: &mut TestBus) {
    cpu.registers.eflags = 0x002;
}

/// STI resumes across the IF 0-to-1 edge, and the whole-state comparison holds.
///
/// This is the row's identity pin: `assert_row_resumes` compares registers, EFLAGS, guest RAM,
/// `elapsed_clocks` and `perf.instructions` against a wholly interpreted leg, and asserts the
/// block retired all three of its slots rather than resyncing at the STI.
///
/// MUTATION: make `arms_interrupt_shadow` return false for `Sti` and the block resyncs, so
/// `native_insns` is two instead of three.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_sti_resumes_across_the_interrupt_flag_edge() {
    for perturb in [no_perturb as fn(&mut CpuGsw, &mut TestBus), clear_if] {
        let legs = assert_row_resumes(&[0xFB], perturb);
        assert_ne!(
            legs.native.eflags() & crate::FLAG_IF,
            0,
            "STI must have set IF on the native leg"
        );
    }
}

/// The boundary CLEARS the shadow when the block ran past the STI.
///
/// `mov ax,0x1111; sti; inc ax` is three native slots, so the `inc` is the one shadowed
/// instruction and it retired inside the block. The flag must not survive the boundary: leaving
/// it armed would hand the guest a second reprieve the hardware never gives.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_block_that_runs_past_the_sti_clears_the_shadow_at_its_boundary() {
    let (code, starts) = row_program(&[0xFB]);
    let (cpu, _, native_insns, _) = run_block_to_boundary(&code, &starts, clear_if);
    assert_eq!(native_insns, 3, "the block must have resumed past the STI");
    assert!(
        !cpu.interrupt_shadow,
        "the shadowed instruction retired inside the block, so the flag must be clear"
    );
    assert!(cpu.can_take_interrupt());
}

/// Design review 10.1, B1: a block that ENDS at the STI exits with the shadow still armed, and the
/// interrupt lands after exactly one further instruction.
///
/// `mov ax,0x1111; inc ax; sti` are the three slots; the `cmc` behind them is unclassifiable and
/// is what makes the STI last. Nothing retired after the STI, so the one-instruction reprieve has
/// not been used and the interpreter owes it. A helper that cleared the flag itself would deliver
/// here, one instruction EARLY, which is outside the caveat the owner approved.
///
/// MUTATION: clear `interrupt_shadow` unconditionally at the boundary instead of comparing the
/// latched EIP, and the first `service_pending_interrupt` below delivers.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_block_that_ends_at_the_sti_leaves_the_shadow_for_the_interpreter() {
    // mov ax,0x1111; inc ax; sti; cmc; hlt
    let code = [0xB8, 0x11, 0x11, 0x40, 0xFB, 0xF5, 0xF4];
    let starts = [0, 3, 4, 5];
    let (mut cpu, mut bus, native_insns, block) = run_block_to_boundary(&code, &starts, clear_if);
    assert_eq!(
        native_insns, 3,
        "the block must end AT the STI, or this fixture is testing the other branch"
    );
    assert!(
        cpu.interrupt_shadow,
        "nothing retired after the STI, so its reprieve is still owed"
    );
    assert!(!cpu.can_take_interrupt());

    // And the dispatcher refuses one entry while it stands, which is the counter design review
    // 10.1 M6 pre-registers as this slice's one expected counter RISE. The refusal is not a
    // regression: it is the shadow doing its job, and it costs exactly one block.
    let refused = cpu.perf_counters().jit_direct_reject_interrupt_shadow;
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, block)
            .expect("a refused entry is not a machine stop"),
        "a block must not be entered while the shadow is armed"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_interrupt_shadow - refused,
        1
    );

    // The IRQ arrives after the block. The shadow defers it by exactly one instruction, which is
    // the `cmc`, and the vector then lands: the fixture's IVT is all zeros and address zero holds
    // a HLT, so a delivered interrupt puts EIP at 0.
    bus.pending_irq = Some(0x08);
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("the shadow must defer rather than fault")
            .is_none(),
        "the shadow must defer the interrupt by one instruction"
    );
    cpu.cycle_no_interrupt_check(&mut bus)
        .expect("the shadowed instruction runs");
    assert!(!cpu.interrupt_shadow, "the cmc consumed the reprieve");
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("delivery must not fault")
            .is_some(),
        "the interrupt lands on the instruction after the shadowed one"
    );
    assert_eq!(cpu.registers.eip, 0, "delivery jumped through the IVT");
}

/// Design review 10.1, B2: STI does not resume while an interrupt is pending.
///
/// The premise the row rests on is that pendency cannot change inside a batch, so "nothing pending
/// at the STI" means the delivery point does not move at all. When something IS pending the row
/// resyncs with the shadow armed, which is bit-identical to the interpreted path, and that is what
/// keeps the identity pins holding for this slice.
///
/// MUTATION: drop the `bus.interrupt_pending()` term from the resume expression and the block
/// resumes here, reporting three instructions instead of two.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_sti_row_resyncs_while_an_interrupt_is_pending() {
    fn pend(cpu: &mut CpuGsw, bus: &mut TestBus) {
        clear_if(cpu, bus);
        bus.pending_irq = Some(0x08);
    }
    let (code, starts) = row_program(&[0xFB]);
    let (cpu, _, native_insns, _) = run_block_to_boundary(&code, &starts, pend);
    assert_eq!(
        native_insns, 2,
        "the STI retired but the block must not have continued past it"
    );
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 1);
    assert!(
        cpu.interrupt_shadow,
        "a resync leaves the interpreter exactly the state it would have produced"
    );
}

/// Design review 10.1, B3: the relaxation is about a shadow the STEP armed. A shadow the block was
/// ENTERED with refuses for every row, `Sti` included.
///
/// Without the split, a block entered under a shadow would resume and the reprieve would be spent
/// on an instruction that never consumed it.
///
/// The clause reads the shadow `run_direct_block` PUBLISHED at the entry rather than the live
/// flag, which is review finding F2: read live it would also refuse every call-out sitting behind
/// an arming one in the same block, and the governor would demote those slots for it.
#[test]
fn an_entry_interrupt_shadow_refuses_resume_for_every_row() {
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    cpu.registers.eflags = 0x202;
    cpu.set_eip(ENTRY + 5);
    cpu.jit_direct.set_block_entry_interrupt_shadow(true);
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    for row in jit::direct::InterpretOneRow::ALL {
        assert!(
            !snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            "{} resumed under an entry shadow",
            row.label()
        );
    }
}

/// Only the ARMING rows may resume with the shadow the step armed. Every other row must refuse.
///
/// Written against `arms_interrupt_shadow` rather than against a literal list, so admitting a row
/// without deciding its shadow answer is a compile-time question rather than a silent pass. The
/// set is `Sti`, `MovSsReg` and `PopSs`.
#[test]
fn only_the_arming_rows_may_resume_with_a_step_armed_shadow() {
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    cpu.registers.eflags = 0x202;
    cpu.set_eip(ENTRY + 5);
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    cpu.interrupt_shadow = true;
    for row in jit::direct::InterpretOneRow::ALL {
        assert_eq!(
            snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            row.arms_interrupt_shadow(),
            "{} disagreed with its own arming answer",
            row.label()
        );
    }
}

/// `STI_CORE_CLOCKS` is what the interpreter charges.
#[test]
fn sti_core_clocks_is_what_the_interpreter_charges() {
    assert_row_charges(&[0xFB], crate::STI_CORE_CLOCKS, |_, _| {});
}

/// The IF 0-to-1 relaxation is NARROWER than the arming set, and the two must not be merged.
///
/// `arms_interrupt_shadow` names three rows; `takes_interrupt_enable_edge` names one. The SS rows
/// arm the shadow and write no flag at all, so giving them the IF pass would be a relaxation
/// nothing asked for, extended automatically to any future arming row that does write IF.
///
/// MUTATION: make `takes_interrupt_enable_edge` an alias of `arms_interrupt_shadow` and this
/// fails on both SS rows.
#[test]
fn the_interrupt_enable_relaxation_is_the_sti_row_alone() {
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    // IF CLEAR before the step, SET after it: the edge the clause is about.
    cpu.registers.eflags = 0x002;
    cpu.set_eip(ENTRY + 5);
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    cpu.registers.eflags = 0x202;
    for row in jit::direct::InterpretOneRow::ALL {
        assert_eq!(
            snapshot.allows_resume(&cpu, ENTRY + 5, row, ALL_SEGMENTS),
            row.takes_interrupt_enable_edge(),
            "{} disagreed with its own interrupt-enable answer",
            row.label()
        );
        assert!(
            !row.takes_interrupt_enable_edge() || row.arms_interrupt_shadow(),
            "{}: a row that takes the IF edge must also arm the shadow",
            row.label()
        );
    }
}

/// Design review 10.1, M5: the SS-load measurement splits same-record loads from record-moving
/// ones, at both interpreter arms.
///
/// The S4d slice admits STI alone. The two SS rows behind it can only resume when R2 finds the
/// segment records unchanged, so whether they are worth building at all is a question about a real
/// guest's mix, and this pair of counters is what supplies it on the loader phase. The test is
/// what makes the number trustworthy: a counter that classified every load the same way would
/// answer the question with a constant.
#[test]
fn the_ss_load_measurement_splits_same_record_from_changed_record() {
    // `mov ax, imm16; mov ss, ax; hlt`, and `mov ax, 0; push ax; pop ss; hlt`.
    let cases: &[(&str, &[u8], u64, u64)] = &[
        // Reloading the selector SS already holds leaves every field of the record alone.
        ("mov ss, same", &[0xB8, 0x00, 0x00, 0x8E, 0xD0, 0xF4], 1, 0),
        // A different real-mode selector moves the base with it.
        ("mov ss, other", &[0xB8, 0x00, 0x01, 0x8E, 0xD0, 0xF4], 0, 1),
        // The 0x17 arm, which is the other site the measurement has to cover.
        ("pop ss, same", &[0xB8, 0x00, 0x00, 0x50, 0x17, 0xF4], 1, 0),
    ];
    for (label, code, same, changed) in cases {
        let mut program = vec![0u8; 0x2000];
        program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
        let mut bus = sixteen_bit_bus(program);
        let mut cpu = sixteen_bit_code_cpu(ENTRY);
        // The measurement is barrier-census gated since the S4 review round.
        cpu.enable_direct_barrier_census(true);
        cpu.registers.set_esp(STACK_TOP);
        while !cpu.halted {
            cpu.cycle(&mut bus).expect("the fixture must run");
        }
        let stalls = cpu.direct_stall_snapshot();
        assert_eq!(
            (stalls.ss_load_same_record, stalls.ss_load_changed_record),
            (*same, *changed),
            "{label}"
        );
    }
}

/// The measurement must not fire for a segment that is not SS. Without this the "same record"
/// column could be inflated by every `mov ds, ax` a guest runs, and the ratio the SS rows are
/// judged on would be meaningless.
#[test]
fn the_ss_load_measurement_ignores_the_other_segments() {
    // mov ax, 0; mov ds, ax; mov es, ax; hlt
    let code = [0xB8, 0x00, 0x00, 0x8E, 0xD8, 0x8E, 0xC0, 0xF4];
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    cpu.enable_direct_barrier_census(true);
    while !cpu.halted {
        cpu.cycle(&mut bus).expect("the fixture must run");
    }
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        (stalls.ss_load_same_record, stalls.ss_load_changed_record),
        (0, 0)
    );
}

/// The measurement costs a plain build nothing: with the census off both columns stay at zero
/// however many SS loads the guest runs.
///
/// The counters live in two interpreter arms, which is why the gate exists at all. Without it the
/// `0x8e` arm pays a segment compare and the `0x17` arm an unconditional record read on every
/// execution, in every build, for a number that is read on census legs only.
///
/// MUTATION: delete the `ss_load_census_active` term from either arm and this reads one instead
/// of zero.
#[cfg(feature = "jit")]
#[test]
fn the_ss_load_measurement_is_off_without_the_census() {
    // mov ax, 0; mov ss, ax; push ax; pop ss; hlt
    let code = [0xB8, 0x00, 0x00, 0x8E, 0xD0, 0x50, 0x17, 0xF4];
    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    cpu.registers.set_esp(STACK_TOP);
    while !cpu.halted {
        cpu.cycle(&mut bus).expect("the fixture must run");
    }
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        (stalls.ss_load_same_record, stalls.ss_load_changed_record),
        (0, 0),
        "the measurement must be silent on a plain build"
    );
}

// ---------------------------------------------------------------------------
// 11. The SS rows (S4 part 2): `0x8E /2` MOV SS,r/m and `0x17` POP SS.
//
// They are STI's siblings in the mechanism and its opposites in the measurement. STI arms the
// shadow and always resumes; these arm the shadow and resume only when R2 finds the SS record
// unchanged, which the tombraid loader split at 484,385 same-record loads against 488,498
// record-moving ones (design review 10.1 M5). Both halves are here: the reload that carries on,
// and the stack switch that stops the run exactly where it stopped before the rows existed.
// ---------------------------------------------------------------------------

/// `mov ss, cx` with CX zero, which is the selector SS already holds in the 16-bit fixture.
///
/// CX rather than AX because the shared program opens with `mov ax, 0x1111`: loading THAT into SS
/// is the record-moving case, and it has its own fixture below.
const MOV_SS_SAME: &[u8] = &[0x8E, 0xD1];
/// `mov ss, ax`, which the same program has already loaded with 0x1111.
const MOV_SS_CHANGED: &[u8] = &[0x8E, 0xD0];

/// Put the selector SS already holds on top of the stack, so `pop ss` reloads the same record.
///
/// `arm_fixture` seeds `POPPED` there for the 0x8F row, which for POP SS would be a stack switch
/// to segment 0x4321 and therefore the OTHER case.
fn same_selector_on_the_stack(_: &mut CpuGsw, bus: &mut TestBus) {
    bus.memory[STACK_TOP as usize..STACK_TOP as usize + 2].fill(0);
}

/// Both SS rows resume when the load leaves the record alone.
///
/// This is the half of the census split that is a win, and it is the whole reason the rows exist:
/// a 16-bit runtime that re-establishes the stack it is already on pays a call-out instead of a
/// block boundary. `assert_row_resumes` compares registers, EFLAGS, guest RAM, `elapsed_clocks`
/// and `perf.instructions` against a wholly interpreted leg and asserts all three slots retired.
///
/// MUTATION: return `None` for `/2` in the `0x8e` arm, or delete the `0x17` arm, and
/// `assert_row_is_a_call_out` fails on the block shape.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_ss_rows_resume_on_an_unchanged_record() {
    let legs = assert_row_resumes(MOV_SS_SAME, no_perturb);
    assert_eq!(legs.native.registers.segment(SegmentIndex::Ss).selector, 0);
    let legs = assert_row_resumes(&[0x17], same_selector_on_the_stack);
    assert_eq!(legs.native.registers.segment(SegmentIndex::Ss).selector, 0);
    assert_eq!(
        legs.native.registers.esp(),
        STACK_TOP + 2,
        "the pop must have moved the stack pointer"
    );
}

/// The other half of the split: a load that MOVES the record resyncs, and it leaves the shadow
/// armed for the interpreter exactly as an interpreted load would.
///
/// Three claims, and the third is the one that is easy to get wrong. The run ends at the row (two
/// instructions retired, not three). The block's own state is the interpreter's. And
/// `interrupt_shadow` is still ARMED at the boundary, so the dispatcher refuses the successor for
/// exactly one instruction and the interpreter consumes the reprieve, which is what hardware does
/// after any SS load. The helper never clears the flag and a RESYNC latches nothing, so the
/// boundary has nothing to compare and leaves it alone.
///
/// MUTATION: drop the SS entry from R2's record compare and the run reports three instructions.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_ss_rows_resync_on_a_changed_record_with_the_shadow_armed() {
    // `mov ax, 0x4321; push ax` is not available inside `row_program`, so the POP case takes the
    // value `arm_fixture` already seeds at the top of the stack.
    for (label, row, perturb) in [
        (
            "mov ss, ax",
            MOV_SS_CHANGED,
            no_perturb as fn(&mut CpuGsw, &mut TestBus),
        ),
        ("pop ss", &[0x17][..], no_perturb),
    ] {
        let (code, starts) = row_program(row);
        let (mut cpu, mut bus, native_insns, block) =
            run_block_to_boundary(&code, &starts, perturb);
        assert_eq!(
            native_insns, 2,
            "{label}: the prefix plus the retired load, and nothing after it"
        );
        let stalls = cpu.direct_stall_snapshot();
        assert_eq!(stalls.callout_interpret_one_executed, 1, "{label}");
        assert_eq!(stalls.callout_interpret_one_resync, 1, "{label}");
        assert!(
            cpu.interrupt_shadow,
            "{label}: a resync leaves the interpreter the state it would have produced"
        );
        assert!(!cpu.can_take_interrupt(), "{label}");

        // The entry gate refuses ONE block while the reprieve stands, which is the counter design
        // review 10.1 M6 pre-registers.
        let refused = cpu.perf_counters().jit_direct_reject_interrupt_shadow;
        assert!(
            !cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a refused entry is not a machine stop"),
            "{label}: a block must not be entered while the shadow is armed"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_reject_interrupt_shadow - refused,
            1,
            "{label}"
        );

        // One interpreted instruction consumes it, and the refusal does not repeat.
        cpu.cycle_no_interrupt_check(&mut bus)
            .expect("the shadowed instruction runs");
        assert!(
            !cpu.interrupt_shadow,
            "{label}: one instruction, one reprieve"
        );
        let refused = cpu.perf_counters().jit_direct_reject_interrupt_shadow;
        let _ = cpu.try_run_direct_block_for_test(&mut bus, block);
        assert_eq!(
            cpu.perf_counters().jit_direct_reject_interrupt_shadow - refused,
            0,
            "{label}: the refusal must cost exactly one instruction"
        );
    }
}

/// Drive a CPU the way `run_budgeted_inner` does and answer how many instructions retire before
/// the pending vector lands.
///
/// The interrupt is asked ONLY where the run would end, which is the first boundary at which
/// `can_take_interrupt` has turned true across an instruction: that is the interpreter's own
/// delivery point (run.rs, the `!can_take_before && can_take_interrupt()` break), and the machine
/// then services at the next batch entry. Calling `cycle` instead would service before the first
/// instruction and measure the fixture rather than the mechanism.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn instructions_until_the_vector_lands(cpu: &mut CpuGsw, bus: &mut TestBus) -> u64 {
    let start = cpu.perf_counters().instructions;
    for _ in 0..16 {
        let can_take_before = cpu.can_take_interrupt();
        cpu.cycle_no_interrupt_check(bus)
            .expect("the fixture must not stop the machine");
        if !can_take_before && cpu.can_take_interrupt() {
            let retired = cpu.perf_counters().instructions - start;
            assert!(
                cpu.service_pending_interrupt(bus)
                    .expect("delivery must not fault")
                    .is_some(),
                "the run ended on the interrupt transition, so the vector must land here"
            );
            assert_eq!(cpu.registers.eip, 0, "delivery jumped through the IVT");
            return retired;
        }
        assert!(
            !cpu.halted,
            "the guest halted with the interrupt still undelivered"
        );
    }
    panic!("the vector never landed");
}

/// An IRQ pending across `mov ss, cx; mov sp, bx` is delivered at the SAME instruction the
/// interpreter delivers it at.
///
/// The owner's caveat allows a LATER delivery when a block carried on past an arming row. It never
/// allows an earlier one, and here it does not even allow a later one: the pendency clause makes
/// the row resync while an interrupt is pending, so the block ends at the SS load and the
/// interpreter runs the one shadowed instruction itself. Both legs deliver after three retired
/// instructions.
///
/// MUTATION: drop the `bus.interrupt_pending()` term and the block resumes, reporting three
/// native instructions instead of two.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn an_irq_across_an_ss_load_lands_where_the_interpreter_lands_it() {
    fn pend(_: &mut CpuGsw, bus: &mut TestBus) {
        bus.pending_irq = Some(0x08);
    }
    // mov ax,0x1111; mov ss,cx; mov sp,bx; hlt
    let code = [0xB8, 0x11, 0x11, 0x8E, 0xD1, 0x8B, 0xE3, 0xF4];
    let starts = [0, 3, 5, 7];

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    seed_fault_handler(&mut program);
    let mut interp_bus = sixteen_bit_bus(program);
    let mut interp = sixteen_bit_code_cpu(ENTRY);
    arm_fixture(&mut interp, &mut interp_bus);
    pend(&mut interp, &mut interp_bus);
    let interpreted = instructions_until_the_vector_lands(&mut interp, &mut interp_bus);
    assert_eq!(interpreted, 3, "the interpreted leg is the oracle");

    let (mut cpu, mut bus, native_insns, _) = run_block_to_boundary(&code, &starts, pend);
    assert_eq!(
        native_insns, 2,
        "the block must stop at the SS load while something is pending"
    );
    let native = native_insns + instructions_until_the_vector_lands(&mut cpu, &mut bus);
    assert_eq!(
        native, interpreted,
        "the vector landed on a different instruction than the interpreter puts it on"
    );
}

/// The pendency clause is what stops a later IF-clearing slot from LOSING the interrupt, which is
/// a bigger error than the latency the caveat covers.
///
/// `mov ss, cx; nop; cli`: interpreted, the `nop` consumes the shadow, the run breaks on the
/// interrupt transition and the vector lands BEFORE the CLI. A block that resumed past the SS load
/// would run the CLI inside itself, reach its boundary with IF clear, and the interrupt would then
/// wait for the next IF rise, which the fixture never performs.
///
/// MUTATION: drop the `bus.interrupt_pending()` term and this fixture halts with the interrupt
/// still pending, which `instructions_until_the_vector_lands` reports as a panic.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn an_ss_load_does_not_lose_an_irq_to_a_later_cli_slot() {
    fn pend(_: &mut CpuGsw, bus: &mut TestBus) {
        bus.pending_irq = Some(0x08);
    }
    // mov ax,0x1111; mov ss,cx; nop; cli; hlt
    let code = [0xB8, 0x11, 0x11, 0x8E, 0xD1, 0x90, 0xFA, 0xF4];
    let starts = [0, 3, 5, 6, 7];
    let (mut cpu, mut bus, native_insns, _) = run_block_to_boundary(&code, &starts, pend);
    assert_eq!(native_insns, 2, "the block must stop at the SS load");
    assert_eq!(
        native_insns + instructions_until_the_vector_lands(&mut cpu, &mut bus),
        3,
        "the vector must land on the instruction after the SS load, before the CLI"
    );
}

/// The IF term narrows the pendency clause: with interrupts disabled the block carries on.
///
/// Nothing is deliverable at the boundary or anywhere else while IF is clear, so continuing cannot
/// move a delivery point, and refusing here would demote every SS slot a guest runs inside an
/// interrupt handler. STI is unaffected by the term because a retired STI always leaves IF set.
///
/// MUTATION: delete the `FLAG_IF` term and this resyncs, reporting two instructions.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn an_ss_load_resumes_while_interrupts_are_disabled_and_an_irq_is_pending() {
    fn pend_with_if_clear(cpu: &mut CpuGsw, bus: &mut TestBus) {
        clear_if(cpu, bus);
        bus.pending_irq = Some(0x08);
    }
    let (code, starts) = row_program(MOV_SS_SAME);
    let (cpu, _, native_insns, _) = run_block_to_boundary(&code, &starts, pend_with_if_clear);
    assert_eq!(native_insns, 3, "the block must have carried on");
    assert_eq!(cpu.direct_stall_snapshot().callout_interpret_one_resync, 0);
}

/// `POP_SS_CORE_CLOCKS` and `MOV_SREG_CORE_CLOCKS` are what the interpreter charges for the two
/// rows, which is what the block budget bound is derived from.
#[test]
fn the_ss_rows_charge_what_the_interpreter_charges() {
    // A stack clear of everything `arm_fixture` seeds, so twenty-four pops all load selector zero
    // and the segment base stays inside the fixture's memory.
    fn quiet_stack(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_esp(0x1900);
    }
    assert_row_charges(&[0x17], crate::POP_SS_CORE_CLOCKS, quiet_stack);
    assert_row_charges(MOV_SS_SAME, crate::MOV_SREG_CORE_CLOCKS, |_, _| {});
}

// The protected-mode half. A real-mode SS load is `base = selector << 4`; a protected-mode one is
// a descriptor fetch with a type check the other segments do not run (a stack must be a WRITABLE
// data segment), a privilege check, and two fault vectors instead of one.

/// `mov eax,0x1111; mov ss,dx; inc eax; hlt`, the protected-mode SS fixture.
const PROTECTED_MOV_SS: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xD2, 0x40, 0xF4];
const PROTECTED_MOV_SS_STARTS: &[u32] = &[0, 5, 7];
/// `mov eax,0x1111; pop ss; inc eax; hlt`, on the 32-BIT stack `protected_cpu` gives SS.
const PROTECTED_POP_SS: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x17, 0x40, 0xF4];
const PROTECTED_POP_SS_STARTS: &[u32] = &[0, 5, 6];

/// Put `SEL_DATA` on top of the 32-bit stack, so the protected-mode POP SS reloads the record SS
/// already holds. Four bytes, because CS.D is 1 here and the 386 PRM has a Dword POP SS move a
/// full dword of stack and load the low sixteen.
fn same_selector_on_the_dword_stack(_: &mut CpuGsw, bus: &mut TestBus) {
    bus.memory[STACK_TOP as usize..STACK_TOP as usize + 4]
        .copy_from_slice(&u32::from(SEL_DATA).to_le_bytes());
}

/// Reloading the same descriptor into SS resumes in protected mode, through both rows and
/// therefore both stack widths: `mov ss,dx` on the 16-bit fixture above and `pop ss` here on a
/// 32-bit stack, which is what `protected_cpu` gives SS.
///
/// The descriptor fetch, the writable-data type check, the privilege check and the accessed-bit
/// branch are all the interpreter's, run from inside a live native frame.
///
/// MUTATION: refuse `0x17` in `stack_width_kind` the way `PopSegReal` is refused in protected mode
/// and the block shape assertion inside `build_protected_program` fails.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_protected_mode_ss_reload_resumes() {
    let mut legs = run_both_protected_program(
        PROTECTED_MOV_SS,
        PROTECTED_MOV_SS_STARTS,
        SEL_DATA,
        no_perturb,
    );
    assert_eq!(legs.exit_reason, None, "mov ss: the block should complete");
    assert_eq!(legs.native_insns, u64::from(BLOCK_INSTRUCTIONS), "mov ss");
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Ss).selector,
        SEL_DATA
    );

    let mut legs = run_both_protected_program(
        PROTECTED_POP_SS,
        PROTECTED_POP_SS_STARTS,
        SEL_DATA,
        same_selector_on_the_dword_stack,
    );
    assert_eq!(legs.exit_reason, None, "pop ss: the block should complete");
    assert_eq!(legs.native_insns, u64::from(BLOCK_INSTRUCTIONS), "pop ss");
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.esp(),
        STACK_TOP + 4,
        "a Dword POP SS moves four bytes of stack"
    );
}

/// A protected-mode load of a DIFFERENT descriptor moves the record, so the run ends at the row.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_protected_mode_ss_change_resyncs() {
    let mut legs = run_both_protected_program(
        PROTECTED_MOV_SS,
        PROTECTED_MOV_SS_STARTS,
        SEL_OTHER,
        no_perturb,
    );
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32)
    );
    assert_eq!(legs.native_insns, 2, "the prefix plus the retired load");
    assert_legs_agree(&mut legs);
    assert_eq!(
        legs.native.registers.segment(SegmentIndex::Ss).limit,
        0xffff,
        "the load itself must have happened"
    );
}

/// Both SS fault vectors, delivered from inside the helper through the not-retired stub.
///
/// #GP(13) for a selector past the table limit, and #SS(12) for a present-bit-clear descriptor,
/// which is the 386 PRM 9.3 carve-out no other segment takes: every other register would get #NP.
/// A row whose fault path was only ever argued about would pass every other test in this file.
///
/// MUTATION: return `STATUS_ABNORMAL` instead of the fault stub and the retirement count reads 0
/// instead of the prefix, and `assert_legs_agree` fails on `perf.instructions`.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_protected_mode_ss_faults_take_the_fault_stub() {
    for (label, selector) in [
        ("#GP bad selector", SEL_BAD),
        ("#SS not present", SEL_NOT_PRESENT),
        // The NULL selector, which is the third vector and the one that is a rule rather than a
        // table lookup: `load_protected_segment` short-circuits index 0 into a legal unusable
        // segment for ES/DS/FS/GS and carves SS out of that with CS, so SS gets #GP(0) where every
        // data segment would have been loaded without a fault. It reaches the same fault stub with
        // the code-write window open, and the two legs agree on the delivery.
        ("#GP null selector", 0x0000),
    ] {
        let mut legs = run_both_protected_program(
            PROTECTED_MOV_SS,
            PROTECTED_MOV_SS_STARTS,
            selector,
            no_perturb,
        );
        assert_eq!(
            legs.exit_reason,
            Some(jit::direct::SideExitReason::CallOutResyncFault as u32),
            "{label}"
        );
        assert_eq!(legs.native_insns, 1, "{label}: the prefix only");
        assert_eq!(
            legs.native
                .direct_stall_snapshot()
                .callout_interpret_one_resync_fault,
            1,
            "{label}"
        );
        assert_legs_agree(&mut legs);
        assert_eq!(
            legs.native.registers.segment(SegmentIndex::Ss).selector,
            SEL_DATA,
            "{label}: a faulting load must leave the old segment in place"
        );
    }
}

// ---------------------------------------------------------------------------
// 12. The S4 review round. Three findings about the shadow, each with the fixture that would
// have caught it.
// ---------------------------------------------------------------------------

/// F1: the LAST arming slot owns the latch, on every path it can leave by.
///
/// Two arming slots in one run, and the second RESYNCS. `mov ss,cx` reloads the record SS already
/// holds and resumes, latching its end; `mov ss,ax` then loads a different selector, so R2 refuses
/// and the block ends there with a shadow that slot has just armed and nothing has consumed. The
/// `cmc` behind it is unclassifiable, which is what makes the second SS load the block's last
/// slot, and it is also the one instruction the reprieve is spent on.
///
/// Latched on the resume path alone, the boundary compares the block's final EIP against the
/// FIRST slot's address, finds them different, and clears that fresh shadow: the interrupt is
/// delivered one instruction EARLY, which is outside the caveat the owner approved. With the latch
/// written by whichever arming slot ran last, the address matches and the flag survives for the
/// interpreter.
///
/// MUTATION: move the latch back below the `if !resume` return and this fails on
/// `interrupt_shadow`; in a debug build the boundary's `debug_assert` on the arm count fires
/// first, because the stamp is guarded by the SAME predicate as the latch and so still moves for
/// the second slot. Guarded on "this step armed it" instead -- which is how it was written first
/// -- the stamp went missing along with the latch and the assertion agreed with itself.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn the_last_arming_slot_owns_the_shadow_latch() {
    // mov ax,0x0010; mov ss,cx; nop; mov ss,ax; cmc; hlt
    //
    // Selector 0x0010 rather than the shared program's 0x1111: the record has to MOVE, and it
    // also has to leave a stack the interrupt frame below can be pushed onto.
    let code = [0xB8, 0x10, 0x00, 0x8E, 0xD1, 0x90, 0x8E, 0xD0, 0xF5, 0xF4];
    let starts = [0, 3, 5, 6, 8];
    // IF stays SET (the shared arm's 0x202): neither SS load writes it, and the delivery below
    // needs it. `clear_if` belongs to the STI fixtures, which are about the 0-to-1 edge.
    let (mut cpu, mut bus, native_insns, _) = run_block_to_boundary(&code, &starts, no_perturb);
    assert_eq!(
        native_insns, 4,
        "three native slots plus the SS load that retired and then resynced"
    );
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        (
            stalls.callout_interpret_one_executed,
            stalls.callout_interpret_one_resync
        ),
        (2, 1),
        "the first SS load must resume and the second must resync"
    );
    assert!(
        cpu.interrupt_shadow,
        "the LAST SS load armed a reprieve nothing has consumed"
    );

    // And it is spent on exactly one instruction, not on none.
    bus.pending_irq = Some(0x08);
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("the shadow must defer rather than fault")
            .is_none(),
        "the reprieve must defer the interrupt by one instruction"
    );
    cpu.cycle_no_interrupt_check(&mut bus)
        .expect("the shadowed instruction runs");
    assert!(!cpu.interrupt_shadow, "the cmc consumed the reprieve");
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("delivery must not fault")
            .is_some()
    );
    assert_eq!(cpu.registers.eip, 0, "delivery jumped through the IVT");
}

/// The same claim for two STI slots, which is the shape the review named.
///
/// Both resume here -- nothing is pending and neither moves a record -- so the latch is written
/// twice and the second write is the one that counts. The block ends AT the second STI (the `cmc`
/// behind it is unclassifiable), so nothing retired after it and the flag survives.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn two_sti_slots_leave_the_second_ones_shadow_armed() {
    // mov ax,0x1111; sti; inc ax; sti; cmc; hlt
    let code = [0xB8, 0x11, 0x11, 0xFB, 0x40, 0xFB, 0xF5, 0xF4];
    let starts = [0, 3, 4, 5, 6];
    let (mut cpu, mut bus, native_insns, _) = run_block_to_boundary(&code, &starts, clear_if);
    assert_eq!(native_insns, 4, "the block must end AT the second STI");
    assert!(cpu.interrupt_shadow, "nothing retired after the second STI");
    assert!(!cpu.can_take_interrupt());

    bus.pending_irq = Some(0x08);
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("deferral must not fault")
            .is_none()
    );
    cpu.cycle_no_interrupt_check(&mut bus)
        .expect("the cmc runs");
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("delivery must not fault")
            .is_some(),
        "the interrupt lands on the instruction after the shadowed one"
    );
}

/// F2: a shadow an EARLIER slot armed must not refuse the call-outs behind it.
///
/// `sti; pop word [bx]; cli` is three call-out slots in one block, and only the first arms
/// anything. Read live, R3's entry-shadow clause is true for the two behind it -- the helper never
/// clears the flag and no native slot consumes it -- so both would resync, and three resyncs in
/// the governor's first eight executions would demote a slot that did nothing wrong. The clause
/// reads the shadow the BLOCK was entered with instead.
///
/// MUTATION: capture `cpu.interrupt_shadow` instead of the published block-entry value and the
/// block reports two instructions instead of four.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_call_out_behind_an_arming_slot_still_resumes() {
    // mov ax,0x1111; sti; pop word [bx]; cli; hlt
    let code = [0xB8, 0x11, 0x11, 0xFB, 0x8F, 0x07, 0xFA, 0xF4];
    let starts = [0, 3, 4, 6];
    let mut legs = run_both(&code, &starts, clear_if);
    assert_eq!(legs.exit_reason, None, "the block should have completed");
    assert_eq!(
        legs.native_insns, 4,
        "all four slots must retire natively; a resync at the POP reports two"
    );
    let stalls = legs.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 3);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_eq!(stalls.callout_interpret_one_demoted, 0);
    assert_legs_agree(&mut legs);
}

/// F3: `perf.brk_interrupt` is PRESERVED by this slice, not lowered by it.
///
/// The exit-path re-check in `run_budgeted_inner` fires on every block that resumes past an arming
/// row: `can_take_interrupt` never consults the bus, so the break is about the TRANSITION, and
/// interpreted that transition happens on the instruction after the arming one. The comment at
/// that site used to claim the fold could not fire in production, and design section 10.1 M6
/// pre-registered the counter as FALLING. Both were wrong, and this is the equality that says so.
///
/// The block is entered through the run loop rather than the direct seam, because the counter
/// lives in the loop: a NOP one byte below the entry makes the block a CONTINUATION, which is the
/// only way the dispatcher reaches it.
///
/// IF is already SET at the entry, which is the REDUNDANT-STI shape and the only one where the
/// fold does any work. With IF clear the iteration that runs the block already answers
/// `can_take_interrupt` false before it starts, so the break fires from the ordinary transition
/// test and the fixture would pass with the fold deleted.
///
/// The `mov ss, cx` leg is the same claim without the flag write: that row changes no flag at all,
/// so IF is set before the block and set after it, and the consumed-shadow fold is the only thing
/// in the loop that can make the transition test fire. Nothing is pending on either leg -- the
/// break is about the TRANSITION and never consults the bus, which is the half of the finding the
/// old comment at this site had backwards.
///
/// MUTATION: delete the `take_interrupt_shadow_consumed` fold and the native leg reports zero
/// against the interpreter's one.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_block_that_resumes_past_the_sti_breaks_the_run_where_the_interpreter_does() {
    fn breaks(mut cpu: CpuGsw, mut bus: TestBus, native: bool) -> u64 {
        // A NOP below the entry, so the block is reached as a continuation. The dispatcher only
        // takes a block on a CONTINUATION, so a run that starts AT the entry interprets it.
        bus.memory[(ENTRY - 1) as usize] = 0x90;
        // The oracle leg must stay interpreted; the other has to have the dispatcher armed, which
        // `build_native` does not do because every other fixture in this file enters its block
        // through the direct seam instead.
        cpu.set_jit_auto_admit(native);
        cpu.set_eip(ENTRY - 1);
        let before = cpu.perf_counters().brk_interrupt;
        for _ in 0..64 {
            if cpu
                .run_budgeted(&mut bus, 4_096)
                .expect("the fixture must not stop the machine")
                .halted
            {
                break;
            }
        }
        assert_eq!(
            cpu.perf_counters().jit_direct_entries > 0,
            native,
            "the native leg must enter its block and the oracle must not"
        );
        cpu.perf_counters().brk_interrupt - before
    }

    // BOTH arming shapes. STI is the row the finding was written against; `mov ss, cx` is the one
    // that makes the fixture discriminating on its own terms, because it changes no flag at all --
    // IF is already set at the entry and stays set, so the ONLY thing that can end the run at the
    // block boundary is the consumed-shadow fold.
    // The SS leg FIRST, so a failure names the discriminating one: it writes no flag, so the fold
    // is the only thing in the loop that can end the run at the block boundary.
    for (label, row) in [("mov ss,cx", &[0x8Eu8, 0xD1][..]), ("sti", &[0xFB])] {
        let (code, starts) = row_program(row);
        let mut program = vec![0u8; 0x2000];
        program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        seed_fault_handler(&mut program);
        let mut interp_bus = sixteen_bit_bus(program);
        let mut interp = sixteen_bit_code_cpu(ENTRY);
        arm_fixture(&mut interp, &mut interp_bus);
        let interpreted = breaks(interp, interp_bus, false);
        assert_eq!(
            interpreted, 1,
            "{label}: the interpreted leg is the oracle and it must break once"
        );

        let (mut native, mut native_bus, _) = build_native(&code, &starts);
        arm_fixture(&mut native, &mut native_bus);
        let native_breaks = breaks(native, native_bus, true);
        assert_eq!(
            native_breaks, interpreted,
            "{label}: the block must end the run at the same transition the interpreter breaks on"
        );
    }
}

// ---------------------------------------------------------------------------
// 13. S4f: the suffix-used relaxation of R2, and the successor bar that pays for it.
//
// A segment-writing call-out compares only the records the slots BEHIND it depend on, plus CS and
// SS. What makes that sound is not an argument about the block's own slots -- R2 covers those --
// but the bar on publishing successors: a chained transfer jumps into a successor's body without
// returning to `run_direct_block`, so its `data_matches` never runs, and nothing in the mask says
// anything about what a successor bakes.
// ---------------------------------------------------------------------------

/// A block holding a segment-writing call-out publishes NO successors, and registers no waiting
/// link either.
///
/// The two assertions are one claim seen from both ends. `is_segment_write_block` is what
/// `run_direct_block` reads to clamp the quota to one; the `waiting` map is what `install` fills
/// when a successor is not compiled yet, and a block that names no successor must add nothing to
/// it. A predicate that said "no successors" while the install still queued the target would link
/// the moment the target appeared.
///
/// MUTATION: leave `callout_segment_writes` out of `segment_write_block` and both fail.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn segment_writing_callout_block_publishes_no_successors() {
    // mov ax,0x1111; mov fs,ax; inc ax; hlt -- the HLT is the barrier, so the block would
    // otherwise publish its fallthrough.
    let (code, starts) = row_program(&[0x8E, 0xE0]);
    let (cpu, _, block) = build_native(&code, &starts);
    assert!(
        block.is_segment_write_block(),
        "an InterpretOne row that can write a segment must bar the block's successors"
    );
    assert_eq!(
        cpu.jit_direct.waiting_len_for_test(),
        0,
        "a block with no successors must queue no waiting link"
    );

    // The control differs by the row alone: `xchg ax,cx` is a call-out that writes no segment.
    let (code, starts) = row_program(&[0x91]);
    let (cpu, _, block) = build_native(&code, &starts);
    assert!(
        !block.is_segment_write_block(),
        "control: a call-out that writes no segment must keep its successors"
    );
    assert_eq!(
        cpu.jit_direct.waiting_len_for_test(),
        1,
        "control: and it must queue its fallthrough"
    );
}

/// The chained shape, which is the one the bar exists for.
///
/// `A -> B -> C`, all three installed. A's `jmp +0` is a static link into B and the transfer
/// happens inside the native run. B holds a segment-writing call-out that RESUMES -- no later slot
/// in B bakes FS, so the moved record cannot reach anything -- and B's own `jmp +0` into C is
/// published as nothing. C is therefore reached by returning to the dispatcher, which re-runs the
/// entry check that a chained transfer would have skipped.
///
/// MUTATION: publish B's successors anyway and the linked-transfer count reads two instead of one,
/// which is C entered against a base the call-out was free to move.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_resumed_segment_write_ends_the_chain_at_its_own_block() {
    const CODE: &[u8] = &[
        0x40, 0x40, 0x40, // A: inc eax x3                    +0 +1 +2
        0xEB, 0x00, // jmp +0, static link into B             +3
        0x40, 0x40, // B: inc eax x2                          +5 +6
        0x8E, 0xE2, // mov fs,dx -- the call-out that resumes +7
        0x40, // inc eax; bakes NO segment              +9
        0xEB, 0x00, // jmp +0, would link into C              +10
        0x40, 0x40, 0x40, // C: inc eax x3                    +12 +13 +14
        0xF4, //                                              +15
    ];
    const STARTS: &[u32] = &[0, 1, 2, 3, 5, 6, 7, 9, 10, 12, 13, 14, 15];
    let entry_b = ENTRY + 5;
    let entry_c = ENTRY + 12;

    let mut program = vec![0u8; 0x2000];
    program[ENTRY as usize..ENTRY as usize + CODE.len()].copy_from_slice(CODE);
    seed_protected_tables(&mut program);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = protected_cpu();
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    for &offset in STARTS {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }

    let mut blocks = Vec::new();
    // C is THREE: its `hlt` is unclassifiable, so it is the barrier rather than a fourth slot.
    for (entry, slots) in [(ENTRY, 4u8), (entry_b, 5), (entry_c, 3)] {
        let key = jit::direct::key_for(&cpu, entry, true).expect("fixture key");
        let compilation =
            jit::direct::compile(&mut cpu, entry, true).expect("every fixture block compiles");
        assert_eq!(compilation.span.instructions, slots);
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let id = cpu
            .jit_direct
            .install(&compilation)
            .expect("install the fixture block");
        blocks.push(cpu.jit_direct.block(id).expect("the block must be live"));
    }
    assert!(
        blocks[1].is_segment_write_block(),
        "B carries the call-out that can write a segment"
    );
    assert!(!blocks[0].is_segment_write_block());
    assert!(!blocks[2].is_segment_write_block());

    arm_protected(&mut cpu, &mut bus, SEL_OTHER);
    let transfers = cpu.perf_counters().jit_direct_linked_transfers;
    let insns = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, blocks[0])
            .expect("the chain must not stop the machine")
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers,
        1,
        "A links into B and B links into nothing"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_insns - insns,
        9,
        "A's four and B's five: the call-out resumed rather than ending the run"
    );
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_interpret_one_executed, 1);
    assert_eq!(stalls.callout_interpret_one_resync, 0);
    assert_eq!(
        cpu.registers.eip, entry_c,
        "the run must return to the dispatcher at C's entry"
    );

    // And C is reached from there, through the entry check a chained transfer would have skipped.
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, blocks[2])
            .expect("C must not stop the machine")
    );
    assert_eq!(cpu.registers.eip, ENTRY + 15);
}

/// SS is in the mask ALWAYS, however empty the suffix is.
///
/// `mov ss,dx` with a 16-bit stack descriptor moves `default_size_32`, which is `jit_mode_key`
/// bit 3 and the width every stack slot in every block is emitted against. The suffix here is a
/// single `inc eax`, which bakes nothing at all, so this is exactly the case a mask built from the
/// suffix alone would wave through.
///
/// MUTATION: drop the SS bit from the mask and this resumes, reporting three instructions.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_stack_width_change_resyncs_even_with_an_empty_suffix() {
    // mov eax,0x1111; mov ss,dx; inc eax; hlt
    const CODE: &[u8] = &[0xB8, 0x11, 0x11, 0x00, 0x00, 0x8E, 0xD2, 0x40, 0xF4];
    const STARTS: &[u32] = &[0, 5, 7];
    let mut legs = run_both_protected_program(CODE, STARTS, SEL_SS16, no_perturb);
    assert_eq!(
        legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "a stack width change must RESYNC whatever the suffix uses"
    );
    assert_eq!(legs.native_insns, 2, "the prefix plus the retired load");
    assert_legs_agree(&mut legs);
    assert!(
        !legs
            .native
            .registers
            .segment(SegmentIndex::Ss)
            .default_size_32,
        "the load itself must have happened, and taken the 16-bit stack"
    );
}
