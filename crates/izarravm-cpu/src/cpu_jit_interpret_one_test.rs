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
    (cpu, bus, block)
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
    assert!(snapshot.allows_resume(&cpu, end_eip));
}

/// R1: the step must leave EIP exactly past the instruction. A transfer that moved it resyncs, and
/// so does a fault the step swallowed.
#[test]
fn interpret_one_resync_on_eip_move() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.set_eip(end_eip + 2);
    assert!(!snapshot.allows_resume(&cpu, end_eip));
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
    assert!(!snapshot.allows_resume(&cpu, end_eip));
}

/// R3, the CS half of R1: the block's whole compilation is keyed on CS.
#[test]
fn interpret_one_resync_on_cs_change() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    let mut cs = cpu.registers.cs();
    cs.selector = cs.selector.wrapping_add(1);
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(!snapshot.allows_resume(&cpu, end_eip));
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
            !snapshot.allows_resume(&cpu, end_eip),
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
    assert!(!snapshot.allows_resume(&cpu, ENTRY + 5));
}

/// Design item M8, the resuming direction: IF going 1 to 0 has no delivery point, so refusing it
/// would cost the block for nothing. CLI is on the S3 list precisely because of this.
#[test]
fn interpret_one_resumes_on_if_1_to_0() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.registers.eflags &= !FLAG_IF;
    assert!(snapshot.allows_resume(&cpu, end_eip));
}

/// R3: the one-instruction interrupt shadow. The seam the helper uses does not clear it on the way
/// in (design item M6), so the clause is a plain test of the flag after the step.
#[test]
fn interpret_one_resync_on_interrupt_shadow() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.interrupt_shadow = true;
    assert!(!snapshot.allows_resume(&cpu, end_eip));
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
    assert!(!snapshot.allows_resume(&cpu, end_eip));
}

/// R4: a LIVE mapping epoch that moved. The paging generation changed under the block.
#[test]
fn interpret_one_resync_on_mapping_epoch_change() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.set_data_write_mapping_epoch_for_test(7);
    assert!(
        snapshot.allows_resume(&cpu, end_eip),
        "0 to n is a cold fill, not a mapping change"
    );
    let snapshot = jit::direct::ResumeSnapshot::capture(&cpu);
    cpu.set_data_write_mapping_epoch_for_test(8);
    assert!(
        !snapshot.allows_resume(&cpu, end_eip),
        "n to m is a mapping change and must resync"
    );
}

/// R7 and R8: the run loop's own state. A halted step or a paused REP is not a boundary the block
/// may run past.
#[test]
fn interpret_one_resync_on_halt() {
    let (mut cpu, snapshot, end_eip) = snapshot_fixture();
    cpu.halted = true;
    assert!(!snapshot.allows_resume(&cpu, end_eip));
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
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpret_one_demotes_after_three_resyncs() {
    let (mut native, mut native_bus, block) = build_native(CODE, STARTS);
    let mut executed = Vec::new();
    for _ in 0..4 {
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

/// STI stays refused, which is the other half of design review M8.
///
/// It takes the IF 0-to-1 edge AND arms the interrupt shadow, so it fails two clauses of R3 on
/// every execution. Admitting it would spend a call-out and a governor execution to reach the
/// boundary it already produces. The census measures it at 486 k hits, larger than CLI's 244 k,
/// which is exactly why the refusal has to be stated rather than left to look like an oversight.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn sti_stays_a_boundary_beside_the_admitted_cli() {
    assert_row_is_a_call_out(&[0xFA]);

    let mut code = vec![0xB8, 0x11, 0x11, 0x40, 0x41];
    code.push(0xFB);
    code.extend_from_slice(&[0x40, 0xF4]);
    let starts = vec![0, 3, 4, 5, 6];
    let (_, _, block) = build_native(&code, &starts);
    assert_eq!(
        block.span().instructions,
        3,
        "STI must still end the block where CLI no longer does"
    );
    assert_eq!(block.callout_interpret_one_slots(), 0);
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
    // mov ax,0x1111 | mov fs,ax | inc ax | hlt
    let row = [0x8Eu8, 0xE0];
    assert_row_is_a_call_out(&row);
    let (code, starts) = row_program(&row);
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

/// SS and the three illegal reg fields stay refused, from the side that can see them.
///
/// `/2` fails R3's interrupt-shadow clause on every execution and `/1`, `/6` and `/7` can only
/// fault, so each would be a call-out that never resumes. They are one bit apart from the four
/// admitted values, which is why they are asserted rather than argued.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn mov_sreg_refuses_ss_and_the_illegal_reg_fields() {
    for reg in [1u8, 2, 6, 7] {
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
/// One row per shape the widening admits: a memory form with a ModRM, a register form, and a
/// two-byte opcode routed above the `u8::try_from` truncation. Each is encoded with 32-bit
/// addressing inside the 16-bit code segment, which is exactly what the 0x67 prefix means there.
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
