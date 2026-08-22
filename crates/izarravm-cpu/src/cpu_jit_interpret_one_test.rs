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
