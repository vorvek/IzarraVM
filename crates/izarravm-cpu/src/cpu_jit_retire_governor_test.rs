// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The data-segment reject governor (`IZARRAVM_SEGMENT_RETIRE_GOVERNOR`).
//!
//! Design: `dev_docs/specs/2026-08-23-data-segment-reject-treadmill-design.md` (rev 3), section 6.
//! Every fixture here is named for the mutant it kills, and the mutant is stated in its own doc
//! comment rather than left to be inferred from the assertions.
//!
//! THE SHAPE EVERY FIXTURE IS BUILT FROM. A block bakes six segment descriptors at compile time.
//! The dispatcher entry check compares them against the live records and refuses on a mismatch --
//! all six when the block has a live outbound link (the STRICT arm, because a chained transfer
//! runs the successor's body without ever re-entering the dispatcher), only the block's own pinned
//! set when it does not (the MASKED arm). Today's refusal also RETIRES the key, betting that the
//! new layout is the steady one. Under a record that alternates it never is, and the block
//! recompiles forever while running natively never.
//!
//! Arms are forced through `set_segment_retire_governor_for_test`, which is per-thread, and every
//! fixture that forces one holds an `ArmGuard` so a panicking assertion still restores the ambient
//! reading for the next test on that thread.

use super::*;

use jit::direct::{DATA_SEGMENT_RETIRE_CAP, SegmentRetireGovernor};

const ALL_ARMS: [SegmentRetireGovernor; 3] = [
    SegmentRetireGovernor::Off,
    SegmentRetireGovernor::Cap,
    SegmentRetireGovernor::On,
];

/// Restores the ambient `IZARRAVM_SEGMENT_RETIRE_GOVERNOR` reading on unwind as well as on
/// return. Without the `Drop` a failing assertion would leak a forced arm into whatever test the
/// harness runs next on this thread, and the failure would move.
struct ArmGuard;

impl Drop for ArmGuard {
    fn drop(&mut self) {
        jit::direct::set_segment_retire_governor_for_test(None);
    }
}

#[must_use]
fn force_arm(arm: SegmentRetireGovernor) -> ArmGuard {
    jit::direct::set_segment_retire_governor_for_test(Some(arm));
    ArmGuard
}

const HEAD_PRED: u32 = 0x1e0;
const ENTRY: u32 = 0x200;
const SECOND: u32 = 0x220;
const DONE: u32 = 0x240;
const DATA: u32 = 0x3000;
const SHIFT: u32 = 0x100;
const BAKED_VALUE: u32 = 0xdead_beef;
const SHIFTED_VALUE: u32 = 0xcafe_babe;

/// The measured shape, in three blocks.
///
/// `HEAD_PRED` and `ENTRY` neither write nor use any data segment; `SECOND` reads through ES. So
/// `ENTRY`'s own pinned mask is EMPTY and it can only ever reject on the strict arm -- which is
/// the population stage 2 exists for, and the one a masked-arm-only reading of the mechanism
/// would silently miss.
fn strict_memory() -> Vec<u8> {
    let mut memory = vec![0; 0x5000];
    memory[HEAD_PRED as usize..HEAD_PRED as usize + 10].copy_from_slice(&[
        0xb8, 0x02, 0x00, 0x00, 0x00, // mov eax,2
        0xe9, 0x16, 0x00, 0x00, 0x00, // jmp 0x200
    ]);
    memory[ENTRY as usize..ENTRY as usize + 10].copy_from_slice(&[
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xe9, 0x16, 0x00, 0x00, 0x00, // jmp 0x220
    ]);
    memory[SECOND as usize..SECOND as usize + 12].copy_from_slice(&[
        0x26, 0x8b, 0x15, 0x00, 0x30, 0x00, 0x00, // mov edx,[es:0x3000]
        0xe9, 0x14, 0x00, 0x00, 0x00, // jmp 0x240
    ]);
    memory[DONE as usize] = 0xf4;
    memory[DATA as usize..DATA as usize + 4].copy_from_slice(&BAKED_VALUE.to_le_bytes());
    memory[(DATA + SHIFT) as usize..(DATA + SHIFT) as usize + 4]
        .copy_from_slice(&SHIFTED_VALUE.to_le_bytes());
    memory
}

/// The other arm's shape: one block that reads through DS ITSELF, with no successor installed, so
/// it is entered unlinked and rejects on the MASKED arm.
fn masked_memory() -> Vec<u8> {
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + 11].copy_from_slice(&[
        0x8b, 0x15, 0x00, 0x30, 0x00, 0x00, // mov edx,[ds:0x3000]
        0xe9, 0x35, 0x00, 0x00, 0x00, // jmp 0x240
    ]);
    memory[DONE as usize] = 0xf4;
    memory[DATA as usize..DATA as usize + 4].copy_from_slice(&BAKED_VALUE.to_le_bytes());
    memory[(DATA + SHIFT) as usize..(DATA + SHIFT) as usize + 4]
        .copy_from_slice(&SHIFTED_VALUE.to_le_bytes());
    memory
}

fn fixture(memory: Vec<u8>, starts: &[u32]) -> (CpuGsw, TestBus) {
    let mut cpu = flat_stack_cpu(ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, starts);
    map_direct_page(
        &mut cpu,
        &mut bus,
        DATA,
        DATA,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );
    (cpu, bus)
}

fn strict_fixture() -> (CpuGsw, TestBus) {
    fixture(
        strict_memory(),
        &[
            HEAD_PRED,
            HEAD_PRED + 5,
            ENTRY,
            ENTRY + 5,
            SECOND,
            SECOND + 7,
        ],
    )
}

fn masked_fixture() -> (CpuGsw, TestBus) {
    fixture(masked_memory(), &[ENTRY, ENTRY + 6])
}

fn set_base(cpu: &mut CpuGsw, segment: SegmentIndex, base: u32) {
    let mut descriptor = cpu.registers.segment(segment);
    descriptor.base = base;
    cpu.registers.set_segment(segment, descriptor);
}

fn key_at(cpu: &CpuGsw, linear: u32) -> jit::direct::BlockKey {
    jit::direct::key_for(cpu, linear, true).expect("a key for the fixture block")
}

/// Compile and install `linear` unless its key is already `Compiled`, and hand back the block.
///
/// The "unless" is the whole point: under the cap a key that has spent its budget STAYS
/// `Compiled` through a reject, so this stops recompiling exactly when the governor starts
/// working, and `jit_direct_blocks_installed` becomes a direct reading of how many
/// re-specializations the treadmill actually paid for.
fn ensure_compiled(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = key_at(cpu, linear);
    if let jit::direct::BlockProbe::Ready(id) = cpu.jit_direct.probe(key) {
        return cpu
            .jit_direct
            .block(id)
            .expect("a live block for a ready key");
    }
    let compilation =
        jit::direct::compile(cpu, linear, true).expect("the fixture block must compile");
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    cpu.jit_direct.block(id).expect("the block must be live")
}

fn refusal_count(cpu: &CpuGsw, label: &str) -> u64 {
    cpu.jit_direct
        .stall_snapshot()
        .link_refusals
        .iter()
        .find(|(name, _)| *name == label)
        .map(|(_, count)| *count)
        .expect("named link refusal")
}

fn clears_by_cause(cpu: &CpuGsw, label: &str) -> u64 {
    cpu.jit_direct
        .stall_snapshot()
        .links_cleared
        .iter()
        .find(|(name, _)| *name == label)
        .map(|(_, count)| *count)
        .expect("named link clear cause")
}

/// One turn of the treadmill on the STRICT shape.
///
/// `SECOND` bakes ES at whatever base it was first compiled under and is never retired, so the
/// CHAIN requirement is permanently that base. The head therefore has to be (re)compiled under
/// that base to link at all, and is then entered under the OTHER one -- which is precisely the
/// alternating record the design measured, reproduced in two statements.
fn strict_turn(cpu: &mut CpuGsw, bus: &mut TestBus, baked_base: u32) -> bool {
    set_base(cpu, SegmentIndex::Es, baked_base);
    let block = ensure_compiled(cpu, ENTRY);
    set_base(cpu, SegmentIndex::Es, baked_base ^ SHIFT);
    cpu.set_eip(ENTRY);
    cpu.try_run_direct_block_for_test(bus, block)
        .expect("a refused entry is not a machine stop")
}

/// The same turn on the MASKED shape: the block reads through DS itself, so it rejects on its own
/// pinned set and never on the chain's.
fn masked_turn(cpu: &mut CpuGsw, bus: &mut TestBus, baked_base: u32) -> bool {
    set_base(cpu, SegmentIndex::Ds, baked_base);
    let block = ensure_compiled(cpu, ENTRY);
    set_base(cpu, SegmentIndex::Ds, baked_base ^ SHIFT);
    cpu.set_eip(ENTRY);
    cpu.try_run_direct_block_for_test(bus, block)
        .expect("a refused entry is not a machine stop")
}

/// FIXTURE 1. After the cap the block is no longer retired -- and the REFUSAL is untouched.
///
/// Kills the miscompile "cap the refusal too": a governor that let the entry through once the
/// budget was spent would run the body under a record its snapshot does not match, which is the
/// one thing no arm of this design may ever do. Also kills CALLER DRIFT, by proving that the cap
/// lives in the data-segment caller and not inside `retire_key_for_recompile`, whose four other
/// callers must keep retiring a capped key.
#[test]
fn data_segment_reject_still_refuses_after_the_retire_cap() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = masked_fixture();
    let key = key_at(&cpu, ENTRY);
    let turns = u32::from(DATA_SEGMENT_RETIRE_CAP) + 3;

    for turn in 0..turns {
        assert!(
            !masked_turn(&mut cpu, &mut bus, 0),
            "turn {turn}: the entry must be REFUSED on every arm, capped or not"
        );
        assert_eq!(
            cpu.registers.edx(),
            0,
            "turn {turn}: nothing may have run under a mismatched descriptor"
        );
        assert_eq!(cpu.registers.eip, ENTRY, "turn {turn}: eip must not move");
    }

    let stalls = cpu.jit_direct.stall_snapshot();
    assert_eq!(
        stalls.data_segment_retires_suppressed,
        u64::from(turns - u32::from(DATA_SEGMENT_RETIRE_CAP)),
        "every reject past the cap suppresses exactly one retire"
    );
    assert_eq!(
        stalls.data_segment_sticky_crossings, 1,
        "one key, one crossing"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment,
        u64::from(turns),
        "the refusal itself is ungoverned"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment_masked,
        u64::from(turns),
        "this shape pins DS itself, so every reject is on the masked arm"
    );
    assert_eq!(cpu.perf_counters().jit_direct_reject_data_segment_strict, 0);
    assert!(
        cpu.jit_direct.direct.key_is_compiled_for_test(key),
        "a capped key keeps its block: that is what stops the recompile"
    );

    // The unchanged-callers control. `retire_key_for_recompile` is the shared implementation of
    // the CS-layout retire (`run.rs`, above the data-segment check), the CPL retire, the call-out
    // demotion latch and the x87 cap, and NONE of them may inherit this cap.
    assert!(
        cpu.jit_direct.retire_key_for_recompile(key),
        "a capped key is still retirable by every other caller"
    );
    assert!(
        !cpu.jit_direct.direct.key_is_compiled_for_test(key),
        "and that retire really did demote it"
    );
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some(),
        "the BUDGET survives another caller's retire: it is a statement about the guest's records \
         at this address, not about one block's edges"
    );
}

/// FIXTURE 2. The capped block is not a dead block: it runs natively on the layout it froze on.
///
/// Kills "a cap that also kills the block" -- a governor that stopped the recompile by leaving the
/// key permanently interpreted would move every counter this slice grades on while buying nothing.
#[test]
fn capped_key_still_runs_natively_on_the_layout_it_froze_on() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = masked_fixture();

    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!masked_turn(&mut cpu, &mut bus, 0));
    }
    // The block is frozen on the base the last turn compiled it under.
    let frozen_id = match cpu.jit_direct.probe(key_at(&cpu, ENTRY)) {
        jit::direct::BlockProbe::Ready(id) => id,
        other => panic!("the capped key must still be compiled, got {other:?}"),
    };
    let frozen = cpu
        .jit_direct
        .segment_layout(frozen_id)
        .expect("a capped block keeps its layout")
        .data_segment_base_for_test(SegmentIndex::Ds);

    let installs_before = cpu.jit_direct.direct.len();
    let block = ensure_compiled(&mut cpu, ENTRY);
    assert_eq!(
        cpu.jit_direct.direct.len(),
        installs_before,
        "control: no recompile happened, so what follows is the FROZEN block"
    );

    set_base(&mut cpu, SegmentIndex::Ds, frozen);
    cpu.set_eip(ENTRY);
    cpu.registers.set_edx(0);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block)
            .expect("a matching entry runs"),
        "an entry carrying the frozen layout must run NATIVELY"
    );
    let expected = if frozen == 0 {
        BAKED_VALUE
    } else {
        SHIFTED_VALUE
    };
    assert_eq!(
        cpu.registers.edx(),
        expected,
        "and it must read through the base it actually holds"
    );

    // The other value still interprets, and still pays no compile.
    let attempts_before = cpu.jit_direct.direct.len();
    set_base(&mut cpu, SegmentIndex::Ds, frozen ^ SHIFT);
    cpu.set_eip(ENTRY);
    cpu.registers.set_edx(0);
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, block)
            .expect("a refused entry is not a machine stop")
    );
    assert_eq!(cpu.registers.edx(), 0);
    assert_eq!(
        cpu.jit_direct.direct.len(),
        attempts_before,
        "and the refusal costs no re-specialization"
    );
}

/// FIXTURE 3. THE OBLIGATION FIXTURE. The measured shape, driven PAST the cap on all three arms,
/// against a fresh interpreter, with canonical state compared every round.
///
/// The head neither writes nor uses ES; its bound successor reads through ES; ES alternates. On
/// the `on` arm the head is DECLINED, drops to the masked arm and RUNS -- and this fixture is what
/// says that is sound. Section 2's shipped strict-arm fixture cannot carry the obligation: it
/// enters below the cap, so it never reaches the decline, and "it passes on all three arms" there
/// is an artifact of the entry count.
///
/// Kills the whole design if the decline is unsound: a declined head running its successor's baked
/// ES would read `SHIFTED_VALUE` where the interpreter reads `BAKED_VALUE`, or the reverse.
#[test]
fn alternating_es_with_a_successor_that_bakes_es_is_state_identical_on_both_arms() {
    for arm in ALL_ARMS {
        let _guard = force_arm(arm);
        let (mut native, mut native_bus) = strict_fixture();
        let (mut interp, mut interp_bus) = strict_fixture();

        // Form the chain under ES base 0, the base `SECOND` bakes and keeps.
        set_base(&mut native, SegmentIndex::Es, 0);
        let head = ensure_compiled(&mut native, ENTRY);
        ensure_compiled(&mut native, SECOND);
        assert!(
            native.jit_direct.has_linked_successor(head.id()),
            "{arm:?}: non-vacuity -- the chain must actually form, or the strict arm is never taken"
        );

        // PAST the cap, on the strict arm, before a single state comparison is taken. This is the
        // whole difference between this fixture and the shipped section 2 one, which enters below
        // the cap and therefore never reaches the decline at all.
        for turn in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
            assert!(
                !strict_turn(&mut native, &mut native_bus, 0),
                "{arm:?} turn {turn}: the entry must be refused on every arm"
            );
        }
        let key = key_at(&native, ENTRY);
        let declined = native
            .jit_direct
            .direct
            .data_segment_link_declined_for_test(key);
        assert_eq!(
            declined,
            matches!(arm, SegmentRetireGovernor::On),
            "{arm:?}: the head is declined on the `on` arm and on no other"
        );
        if declined {
            let head_id = match native.jit_direct.probe(key) {
                jit::direct::BlockProbe::Ready(id) => id,
                other => panic!("a declined key stays compiled, got {other:?}"),
            };
            assert!(
                !native.jit_direct.has_linked_successor(head_id),
                "a declined head must be on the MASKED arm: no live outbound edge"
            );
        }

        // Now the state comparison, ABOVE the cap, through the real dispatcher: the declined head
        // takes the masked arm and RUNS, and its successor must be entered under its own check.
        native.set_jit_auto_admit(true);
        let insns_before = native.perf_counters().jit_direct_insns;
        let rounds = 8u32;
        for round in 0..rounds {
            let es_base = if round.is_multiple_of(2) { SHIFT } else { 0 };
            for cpu in [&mut native, &mut interp] {
                cpu.halted = false;
                cpu.registers.gpr = [0; 8];
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.elapsed_clocks = 0;
                cpu.timing_rem = 0;
                cpu.core_clocks_so_far = 0;
                set_base(cpu, SegmentIndex::Es, es_base);
                cpu.set_eip(ENTRY);
            }
            drive(&mut native, &mut native_bus);
            drive(&mut interp, &mut interp_bus);

            assert_eq!(
                native.registers.gpr, interp.registers.gpr,
                "{arm:?} round {round}: GPRs"
            );
            assert_eq!(
                native.registers.eip, interp.registers.eip,
                "{arm:?} round {round}: eip"
            );
            assert_eq!(
                native.registers.eflags, interp.registers.eflags,
                "{arm:?} round {round}: eflags"
            );
            assert_eq!(
                native.pending_flags, interp.pending_flags,
                "{arm:?} round {round}: pending flags"
            );
            assert_eq!(
                native.elapsed_clocks, interp.elapsed_clocks,
                "{arm:?} round {round}: elapsed clocks"
            );
            for segment in jit::direct::SEGMENT_ORDER {
                assert_eq!(
                    native.registers.segment(segment),
                    interp.registers.segment(segment),
                    "{arm:?} round {round}: {segment:?} record"
                );
            }
            assert_eq!(
                native.registers.edx(),
                if es_base == 0 {
                    BAKED_VALUE
                } else {
                    SHIFTED_VALUE
                },
                "{arm:?} round {round}: the read must follow the LIVE base, never the baked one"
            );
            assert_eq!(
                native_bus.memory, interp_bus.memory,
                "{arm:?} round {round}: memory"
            );
        }
        assert!(
            native.perf_counters().jit_direct_insns > insns_before,
            "{arm:?}: non-vacuity -- the native side must actually have run guest code"
        );
        if !matches!(arm, SegmentRetireGovernor::Off) {
            assert!(
                native
                    .jit_direct
                    .stall_snapshot()
                    .data_segment_retires_suppressed
                    > 0,
                "{arm:?}: the cap must be doing work"
            );
        }
    }
}

/// FIXTURE 4. A declined block is never re-linked and is never re-parked.
///
/// Two mutants in one row. The REBIND HOLE: a decline that only cleared the cells would be undone
/// by the next `resolve_successors`, and the mechanism would be inert while every counter still
/// looked right. The PARK TREADMILL (review M4): a declined source left in `waiting` is retried by
/// every later install at that `LinkTarget` and refused every time -- bounded in size, unbounded in
/// count, and `link_refusals["declined"]` would stop meaning "stage 2 trips".
#[test]
fn link_declined_block_is_never_relinked_and_is_never_re_parked() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);

    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    let key = key_at(&cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "non-vacuity: the decline must have fired"
    );
    assert_eq!(
        clears_by_cause(&cpu, "data_segment_decline"),
        1,
        "one edge cut, under its own cause"
    );
    let head_id = match cpu.jit_direct.probe(key) {
        jit::direct::BlockProbe::Ready(id) => id,
        other => panic!("a declined key stays compiled, got {other:?}"),
    };
    assert!(
        !cpu.jit_direct.direct.waiting_holds_source_for_test(head_id),
        "the decline must also unpark the source"
    );

    let refused_before = refusal_count(&cpu, "declined");
    cpu.jit_direct.direct.resolve_successors_for_test(head_id);
    assert_eq!(
        refusal_count(&cpu, "declined") - refused_before,
        1,
        "exactly one refusal per attempted edge -- more means the source was re-parked and \
         re-tried, which is the treadmill M4 names"
    );
    assert!(
        !cpu.jit_direct.direct.waiting_holds_source_for_test(head_id),
        "and it must still not be parked"
    );
    assert!(
        !cpu.jit_direct.has_linked_successor(head_id),
        "nothing may have rebound the cell"
    );
}

/// FIXTURE 5. The decline cuts OUTBOUND edges only.
///
/// Kills "clear all cells, inbound too" -- which would cost the predecessor its chaining for its
/// successor's judgement -- and, from the other side, the unsound relaxation that drops the
/// predecessor to its own masked arm while its cone still bakes ES. `HEAD_PRED` keeps its edge
/// into the declined block, so it keeps a live outbound cell, so it is still entered on the STRICT
/// arm and still refuses when ES moves.
#[test]
fn link_decline_leaves_inbound_edges_alone() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    let predecessor = ensure_compiled(&mut cpu, HEAD_PRED);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    assert!(
        cpu.jit_direct.has_linked_successor(predecessor.id()),
        "non-vacuity: the predecessor must chain into the block that gets declined"
    );

    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key_at(&cpu, ENTRY)),
        "non-vacuity: the decline must have fired"
    );
    assert!(
        cpu.jit_direct.has_linked_successor(predecessor.id()),
        "the predecessor's OUTBOUND edge is not the declined block's to cut"
    );

    // Still on the strict arm: with ES moved, entering the predecessor must be refused, because
    // its chain requirement still carries the ES its cone baked.
    set_base(&mut cpu, SegmentIndex::Es, SHIFT);
    cpu.set_eip(HEAD_PRED);
    cpu.registers.set_edx(0);
    let before = cpu.perf_counters().jit_direct_reject_data_segment_strict;
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, predecessor)
            .expect("a refused entry is not a machine stop"),
        "the predecessor must still take the STRICT arm and refuse"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment_strict - before,
        1
    );
    assert_eq!(cpu.registers.edx(), 0, "and nothing may have run");
}

/// FIXTURE 6. A masked-arm reject reaches the cap and does NOT decline the link.
///
/// Kills stage 2 applied to the wrong arm. A block that fails `data_matches` on a record it uses
/// ITSELF gains nothing from losing its edges: the masked check it would fall back to refuses too,
/// and the block would have paid its chaining for nothing. This is also what makes the shipped
/// masked-arm section 2 fixture genuinely arm-invariant.
#[test]
fn masked_arm_reject_does_not_decline_the_link() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = masked_fixture();
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 3 {
        assert!(!masked_turn(&mut cpu, &mut bus, 0));
    }
    let stalls = cpu.jit_direct.stall_snapshot();
    assert!(
        stalls.data_segment_retires_suppressed > 0,
        "non-vacuity: the cap must have been reached"
    );
    assert_eq!(
        stalls.data_segment_link_declines, 0,
        "the masked arm never declines"
    );
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key_at(&cpu, ENTRY))
    );
    assert_eq!(clears_by_cause(&cpu, "data_segment_decline"), 0);
}

/// FIXTURE 7. Both governor structures die with their key.
///
/// Kills the stale ban: a rewritten page hands the NEW code at that address a fresh budget and a
/// clean slate, instead of its predecessor's stickiness and its predecessor's decline.
#[test]
fn data_segment_state_dies_with_its_key() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    let key = key_at(&cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some()
            && cpu
                .jit_direct
                .direct
                .data_segment_link_declined_for_test(key),
        "non-vacuity: both structures must hold this key before the write"
    );

    assert!(
        cpu.note_code_write(ENTRY, 4),
        "the SMC door must report a hit on compiled code"
    );
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_none(),
        "the budget must not outlive its key"
    );
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "and neither may the decline"
    );
}

/// FIXTURE 7a. `invalidate_translation` forgets a decline, and the edge re-forms.
///
/// Review H2. That function drops every link, clears `inbound` / `waiting` / `linear_blocks`, bumps
/// the link epoch and RESETS EVERY CHAIN REQUIREMENT wholesale, with the rule in its own comment:
/// a flushed block that keeps demanding a segment nothing live reaches is "safe, but permanently
/// over-strict". A decline earned against a chain requirement the flush then erased is that same
/// failure one level up, and it would bar re-linking for the rest of the cache's life.
///
/// The COUNT is asserted to SURVIVE, so the asymmetry is deliberate and visible rather than an
/// omission: a stale count costs at most bounded interpretation, a stale decline costs the link
/// graph.
#[test]
fn invalidate_translation_forgets_a_decline_and_the_edge_re_forms() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    let key = key_at(&cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "non-vacuity: the decline must have fired"
    );

    cpu.jit_direct.invalidate_translation();
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "the flush erased what the decline was earned against, so the decline goes with it"
    );
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some(),
        "the BUDGET survives: it is a statement about the guest's records at this address"
    );

    // Republish both ends -- `revalidate_translation` is the door root dispatch uses once it has
    // revalidated a block's canonical key, and it is what calls `make_link_visible` -- and the
    // edge must bind again.
    set_base(&mut cpu, SegmentIndex::Es, 0);
    let second_key = key_at(&cpu, SECOND);
    cpu.jit_direct
        .revalidate_translation(second_key)
        .expect("the successor is still compiled across a link flush");
    let head = cpu
        .jit_direct
        .revalidate_translation(key)
        .expect("and so is the head");
    assert!(
        cpu.jit_direct.has_linked_successor(head.id()),
        "a block whose decline was forgotten must be able to chain again"
    );
}

/// FIXTURE 7b. Another caller's retire clears the decline, so the recompile is not BORN declined.
///
/// Review H4. The CS-layout and CPL checks sit ABOVE the data-segment check and the loader moves
/// CS, so those retires are reachable on a declined key. Without the hook the key goes `Seen`,
/// recompiles into a possibly different slot, and `make_link_visible` -> `resolve_successors`
/// refuses every edge as `Declined`: a block that would have chained perfectly is permanently
/// unchained for its predecessor incarnation's judgement.
///
/// Driven through `retire_key_for_recompile` itself, which is the shared implementation every one
/// of those callers reaches and the only place the hook could correctly live.
#[test]
fn a_cs_layout_retire_clears_the_decline_so_the_recompile_is_not_born_declined() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    let key = key_at(&cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "non-vacuity: the decline must have fired"
    );

    assert!(
        cpu.jit_direct.retire_key_for_recompile(key),
        "the other callers still retire a capped, declined key"
    );
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "and that retire takes the decline with it"
    );
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some(),
        "the budget is still spent: records, not edges"
    );

    set_base(&mut cpu, SegmentIndex::Es, 0);
    let head = ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    assert!(
        cpu.jit_direct.has_linked_successor(head.id()),
        "the fresh block must chain normally: it is not its predecessor incarnation"
    );
    let _ = &mut bus;
}

/// FIXTURE 8. The governor's maps are empty when `entries` is.
///
/// Exercises the containment `debug_assert` in `BlockCache::clear` -- the bound the memory
/// argument rests on, since neither structure has an eviction policy. The first `clear` takes the
/// full reset path; the second takes the early-return path where the assertion lives, so removing
/// the reset hook makes this fixture panic rather than pass quietly.
#[test]
fn data_segment_retire_map_is_empty_when_entries_is() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key_at(&cpu, ENTRY))
            .is_some(),
        "non-vacuity: there must be state to outlive its entries"
    );
    cpu.jit_direct.clear();
    cpu.jit_direct.clear();
    assert!(cpu.jit_direct.direct.data_segment_state_is_empty());
}

/// FIXTURE 9. The knob's spelling table, both directions.
///
/// Kills a silently inert ladder leg. Mirrors `parse_retry_lift_arm_for_test`: unset and the empty
/// string are OFF (the PowerShell nulling trap), `cap` is stage 1, `1` and `on` are both stages,
/// and case and surrounding whitespace do not matter.
#[test]
fn segment_retire_governor_knob_spellings() {
    let parse =
        |raw: &str| jit::direct::parse_segment_retire_governor_arm_for_test(Ok(raw.to_string()));
    assert_eq!(
        jit::direct::parse_segment_retire_governor_arm_for_test(Err(
            std::env::VarError::NotPresent
        )),
        SegmentRetireGovernor::Off,
        "unset must be OFF, or an ON leg that merely nulls the variable measures the default"
    );
    for raw in ["", "0", "off", "OFF", "  off  "] {
        assert_eq!(parse(raw), SegmentRetireGovernor::Off, "{raw:?}");
    }
    for raw in ["cap", "CAP", " cap "] {
        assert_eq!(parse(raw), SegmentRetireGovernor::Cap, "{raw:?}");
    }
    for raw in ["1", "on", "ON", " on "] {
        assert_eq!(parse(raw), SegmentRetireGovernor::On, "{raw:?}");
    }
}

/// The other direction: a spelling the table does not name must PANIC rather than fall back to the
/// default, which would run a ladder leg as OFF while its report named an arm.
#[test]
#[should_panic(expected = "names no arm")]
fn segment_retire_governor_refuses_an_unknown_spelling() {
    let _ = jit::direct::parse_segment_retire_governor_arm_for_test(Ok("stage2".to_string()));
}

/// FIXTURE 10. The two shipped section 2 fixtures, re-run on all three arms UNMODIFIED.
///
/// They are the invariant this whole slice is measured against: a block or a chain must never
/// execute under a baked record that differs from the live one. Running them under each arm is the
/// cheapest check that no arm weakens a refusal below the cap.
///
/// THE CAVEAT, recorded here rather than left to be discovered (review M6). The masked-arm fixture
/// is genuinely arm-invariant, and `masked_arm_reject_does_not_decline_the_link` above is what
/// pins why. The strict-arm one is NOT: it enters BELOW the cap, so it never reaches the decline,
/// and its passing on all three arms is an artifact of its entry count.
/// `alternating_es_with_a_successor_that_bakes_es_is_state_identical_on_both_arms` carries the
/// above-cap obligation, and it is the fixture to read if this one ever looks like enough.
#[test]
fn section_two_refusal_fixtures_hold_on_every_governor_arm() {
    for arm in ALL_ARMS {
        let _guard = force_arm(arm);
        super::direct_mov_reg_sreg_bakes_pinned_selectors_and_repins_when_a_segment_moves();
        super::execution::direct_chain_entry_validates_a_segment_only_the_successor_uses();
    }
}

/// FIXTURE 11. The layout census counts the MASKED tuple, not the live one.
///
/// Review H3. Unmasked, a key on this loader saturates at eight within a few hundred rejects on
/// records nothing in the chain pins, and slice (a) -- per-layout block variants -- would read a
/// false no-go off it. Here DS alternates between exactly two values while FS, which the block's
/// mask does NOT cover, moves to a fresh base on every single entry: more than eight of them, so
/// an unmasked fingerprint saturates and this fixture FAILS. That is the point; as first written
/// it could not.
#[test]
fn distinct_layout_census_counts_two_while_an_unpinned_record_wobbles() {
    const OTHER: u32 = 2 * SHIFT;
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = masked_fixture();
    let key = key_at(&cpu, ENTRY);
    // DS takes exactly TWO values at the reject, and neither is the base the block freezes on, so
    // every entry below really does reject and really is censused.
    let rejecting = |turn: u32| if turn.is_multiple_of(2) { SHIFT } else { OTHER };

    let mut block = ensure_compiled(&mut cpu, ENTRY);
    for turn in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) {
        set_base(&mut cpu, SegmentIndex::Ds, rejecting(turn));
        set_base(&mut cpu, SegmentIndex::Fs, turn * 0x10);
        cpu.set_eip(ENTRY);
        assert!(
            !cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a refused entry is not a machine stop"),
            "turn {turn}: the entry must reject"
        );
        // Each of those retired the key; recompile it under the base it will freeze on.
        set_base(&mut cpu, SegmentIndex::Ds, 0);
        block = ensure_compiled(&mut cpu, ENTRY);
    }

    let turns = 16u32;
    assert!(
        turns > jit::direct::DATA_SEGMENT_LAYOUT_CENSUS_CAP as u32,
        "the wobble has to be able to saturate an unmasked census, or the row proves nothing"
    );
    for turn in 0..turns {
        set_base(&mut cpu, SegmentIndex::Ds, rejecting(turn));
        set_base(
            &mut cpu,
            SegmentIndex::Fs,
            (turn + u32::from(DATA_SEGMENT_RETIRE_CAP)) * 0x10,
        );
        cpu.set_eip(ENTRY);
        assert!(
            !cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a refused entry is not a machine stop"),
            "post-cap turn {turn}: the entry must still reject"
        );
    }

    let record = cpu
        .jit_direct
        .direct
        .data_segment_retire_record_for_test(key)
        .expect("the capped key must carry its census");
    assert!(
        !record.layouts_saturated(),
        "an unmasked fingerprint would have filled the census on the FS wobble"
    );
    assert_eq!(
        record.distinct_layouts(),
        2,
        "DS took exactly two values at the reject; FS is outside the block's mask and must \
         contribute nothing"
    );

    let histogram = cpu
        .jit_direct
        .stall_snapshot()
        .data_segment_distinct_layouts;
    assert_eq!(histogram[2], 1, "one key, two distinct masked layouts");
    assert_eq!(
        histogram[jit::direct::DATA_SEGMENT_LAYOUT_CENSUS_CAP + 1],
        0
    );
}

/// FIXTURE 12. A differential sweep over segment-moving programs with the governor ON.
///
/// The hand fixtures above each pin one mechanism; this one pins the composition. Both shapes are
/// driven for many rounds with ES, DS and FS moved on a deterministic pseudo-random schedule that
/// crosses the cap many times over, native against a fresh interpreter, comparing canonical state
/// every round. Anything the hand fixtures missed -- an ordering, an interaction between the
/// decline and a recompile, a wrong base surviving one round in twenty -- shows up as a state
/// difference here.
#[test]
fn governor_on_is_state_identical_to_the_interpreter_over_a_segment_moving_sweep() {
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut native, mut native_bus) = strict_fixture();
    let (mut interp, mut interp_bus) = strict_fixture();
    set_base(&mut native, SegmentIndex::Es, 0);
    ensure_compiled(&mut native, HEAD_PRED);
    ensure_compiled(&mut native, ENTRY);
    ensure_compiled(&mut native, SECOND);
    // Drive the treadmill past the cap FIRST, so the sweep below runs against a cache that has
    // already declined an edge rather than one that might get there by luck.
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut native, &mut native_bus, 0));
    }
    native.set_jit_auto_admit(true);

    // xorshift, so the schedule is reproducible and the failure is too.
    let mut state = 0x1234_5678u32;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };

    for round in 0..48u32 {
        let roll = next();
        let es_base = (roll & 1) * SHIFT;
        let ds_base = ((roll >> 1) & 1) * SHIFT;
        let fs_base = (roll >> 8) & 0xff0;
        let start = if roll & 2 == 0 { HEAD_PRED } else { ENTRY };
        for cpu in [&mut native, &mut interp] {
            cpu.halted = false;
            cpu.registers.gpr = [0; 8];
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
            set_base(cpu, SegmentIndex::Es, es_base);
            set_base(cpu, SegmentIndex::Ds, ds_base);
            set_base(cpu, SegmentIndex::Fs, fs_base);
            cpu.set_eip(start);
        }
        drive(&mut native, &mut native_bus);
        drive(&mut interp, &mut interp_bus);

        assert_eq!(
            native.registers.gpr, interp.registers.gpr,
            "round {round}: GPRs (es={es_base:#x} ds={ds_base:#x} start={start:#x})"
        );
        assert_eq!(native.registers.eip, interp.registers.eip, "round {round}");
        assert_eq!(
            native.registers.eflags, interp.registers.eflags,
            "round {round}"
        );
        assert_eq!(native_bus.memory, interp_bus.memory, "round {round}");
    }
    assert!(
        native.perf_counters().jit_direct_insns > 0,
        "non-vacuity: the native side must actually have run guest code"
    );

    assert!(
        native
            .jit_direct
            .stall_snapshot()
            .data_segment_link_declines
            > 0,
        "non-vacuity: the sweep must actually have exercised stage 2"
    );
}
