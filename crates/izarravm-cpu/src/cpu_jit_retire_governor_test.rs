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
//! **The default arm is `cap` as of the 2026-08-23 ladder** (loader phase +27.1%, lower95 1.263;
//! duke3d-586 short +4.3%). `off` is the escape and the A/B base; `on` is opt-in, because it beats
//! `cap` on the loader and LOSES to it on duke. EVERY fixture in this file forces its arm
//! explicitly through `set_segment_retire_governor_for_test` and none reads the ambient default,
//! which is what kept the whole file valid across the flip -- a fixture that had relied on "unset
//! means off" would have started measuring `cap` and passing for the wrong reason.
//!
//! The override is per-thread, and every fixture that forces an arm holds an `ArmGuard` so a
//! panicking assertion still restores the ambient reading for the next test on that thread.

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
    // TWO successors, deliberately. `xor eax,eax` sets ZF, so the branch is always taken and the
    // executed path is exactly what an unconditional jump would give -- but the block now carries
    // a SECOND successor slot, the fallthrough at 0x204, that nothing ever installs. That is what
    // makes the head simultaneously LINKED (slot 0, into the ES-baking successor, so it is entered
    // on the strict arm) and PARKED in `waiting` (slot 1, unresolved), which is the only state in
    // which the decline's `remove_waiting_sources` is observable. With a single-successor head the
    // do-not-park contract's second half cannot be reached and asserting on it is vacuous.
    memory[ENTRY as usize..ENTRY as usize + 9].copy_from_slice(&[
        0x31, 0xc0, // xor eax,eax   (sets ZF)
        0x74, 0x1c, // jz 0x220
        0xe9, 0x37, 0x00, 0x00, 0x00, // jmp 0x240 -- the fallthrough, never executed
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
            ENTRY + 2,
            ENTRY + 4,
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

/// FIXTURE 1. Other callers of `retire_key_for_recompile` still demote a capped key.
///
/// The "still refuses after the cap" half is now the (A) live-data rows: a capped DS-pinning
/// key is promoted and then runs natively on both layouts. This half stays, because the cap
/// lives in the data-segment caller and not inside `retire_key_for_recompile`.
#[test]
fn data_segment_reject_still_refuses_after_the_retire_cap() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = masked_fixture();
    let key = key_at(&cpu, ENTRY);

    for turn in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) {
        assert!(
            !masked_turn(&mut cpu, &mut bus, 0),
            "turn {turn}: every reject below the cap retires"
        );
        assert_eq!(cpu.registers.edx(), 0, "turn {turn}: edx must not move");
        assert_eq!(cpu.registers.eip, ENTRY, "turn {turn}: eip must not move");
    }
    set_base(&mut cpu, SegmentIndex::Ds, 0);
    ensure_compiled(&mut cpu, ENTRY);
    let stalls = cpu.jit_direct.stall_snapshot();
    assert_eq!(
        stalls.data_segment_sticky_crossings, 1,
        "one key, one crossing"
    );
    assert_eq!(
        stalls.data_segment_live_promotions, 0,
        "the promoting entry has not fired yet"
    );
    assert!(
        cpu.jit_direct.direct.key_is_compiled_for_test(key),
        "a just-capped key can still be compiled"
    );

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

/// The OFF arm is pre-slice `main`: it touches neither governor structure, and it pays for neither
/// of the governor's two inputs.
///
/// **This fixture got MORE load-bearing when the default flipped to `cap`, not less.** OFF is no
/// longer what everybody runs by accident -- it is the ESCAPE and the A/B BASE, the leg every
/// future ladder on this mechanism subtracts from. A base that has quietly acquired a map write or
/// an extra six-descriptor compare is a base that hides the thing it is supposed to isolate, and
/// nothing else in the suite would notice.
///
/// EMITTED CODE is untouched by every arm of this knob, and that is a property of WHERE the
/// governor lives rather than something a fixture re-proves per arm: nothing in this slice is
/// reachable from `jit/direct/emit*`, the governor is read only on the dispatcher's reject path,
/// and the block bytes come out of the same compile walk on all three arms. What the arms change
/// is WHICH blocks exist and for how long. The executable half of that claim is
/// `section_two_refusal_fixtures_hold_on_every_governor_arm`, which runs the two shipped refusal
/// fixtures -- both of which compare native execution against the interpreter instruction for
/// instruction -- unmodified on all three arms.
///
/// The first half is assertable directly -- the cap map and the declined set stay empty, and all
/// three stall counters read zero, through more rejects than the cap would ever allow. The second
/// half is the reason `run_direct_block` tests the knob at the reject site rather than inside
/// `retire_key_for_data_segment`: the second `data_matches` (a six-descriptor compare) and the
/// 96-byte `live` copy are work `main` does not do, and building them above the branch would have
/// charged every OFF leg for a governor it is not running. That is pinned from the other side, by
/// the `debug_assert` at the top of `retire_key_for_data_segment` -- reachable only if some caller
/// routes the OFF arm through the governor after all, and this fixture is the one that would then
/// trip it.
///
/// The ARM SPLIT deliberately still counts here. It is the instrument the shipped-arm split had
/// never been measured with, and the OFF leg is where it reads.
#[test]
fn the_off_arm_touches_no_governor_state() {
    let _guard = force_arm(SegmentRetireGovernor::Off);
    let (mut cpu, mut bus) = masked_fixture();
    let turns = u32::from(DATA_SEGMENT_RETIRE_CAP) + 5;
    for turn in 0..turns {
        assert!(
            !masked_turn(&mut cpu, &mut bus, 0),
            "turn {turn}: the refusal is ungoverned, so it fires on the OFF arm too"
        );
    }
    assert!(
        cpu.jit_direct.direct.data_segment_state_is_empty(),
        "the OFF arm must write neither the cap map nor the declined set"
    );
    let stalls = cpu.jit_direct.stall_snapshot();
    assert_eq!(stalls.data_segment_retires_suppressed, 0);
    assert_eq!(stalls.data_segment_sticky_crossings, 0);
    assert_eq!(stalls.data_segment_link_declines, 0);
    assert_eq!(stalls.data_segment_live_promotions, 0);
    assert_eq!(
        stalls.data_segment_distinct_layouts,
        [0; jit::direct::DATA_SEGMENT_LAYOUT_CENSUS_CAP + 2],
        "and the census is empty with it"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment_masked,
        u64::from(turns),
        "the ARM SPLIT is not governed: the OFF leg is where it first reads"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment,
        u64::from(turns),
        "and every reject still retires, so this is pre-slice main's treadmill unchanged"
    );
    // The counter-identity half, stated as the identity rather than as a list: on the OFF arm the
    // key leaves `Compiled` after EVERY reject. That is what "every reject retires" means, and it
    // is the single fact that separates this arm from the shipped `cap` one.
    assert!(
        !cpu.jit_direct
            .direct
            .key_is_compiled_for_test(key_at(&cpu, ENTRY)),
        "the last reject must have retired the key: on the OFF arm nothing is ever suppressed"
    );
}

/// FIXTURE 2. After promotion the live variant runs natively on both layouts.
///
/// Mutant: restoring the baked immediate on the sticky emit path, or leaving `used` unstripped.
#[test]
fn capped_key_still_runs_natively_on_the_layout_it_froze_on() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = masked_fixture();

    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!masked_turn(&mut cpu, &mut bus, 0));
    }
    assert_eq!(
        cpu.jit_direct.stall_snapshot().data_segment_live_promotions,
        1
    );
    let key = key_at(&cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "the promoting entry sets live_data before it retires"
    );

    let block = ensure_compiled(&mut cpu, ENTRY);
    let layout = cpu
        .jit_direct
        .segment_layout(block.id())
        .expect("the live compile is installed");
    assert_eq!(
        layout.used_for_test() & jit::direct::LIVE_DATA_BITS,
        0,
        "the installed snapshot must drop DS/ES so the entry compare matches both layouts"
    );
    let _ = layout.data_segment_base_for_test(SegmentIndex::Ds);
    let installs_after_live = cpu.jit_direct.direct.len();

    for (base, expected) in [(0, BAKED_VALUE), (SHIFT, SHIFTED_VALUE)] {
        set_base(&mut cpu, SegmentIndex::Ds, base);
        cpu.set_eip(ENTRY);
        cpu.registers.set_edx(0);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, block)
                .expect("a live entry runs"),
            "base {base:#x}: both layouts retire native"
        );
        assert_eq!(
            cpu.registers.edx(),
            expected,
            "base {base:#x}: the read must follow the LIVE base"
        );
    }
    assert_eq!(
        cpu.jit_direct.direct.len(),
        installs_after_live,
        "compile_attempts stay flat after the one live compile"
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
        assert!(
            !declined,
            "{arm:?}: promote retires and forgets any decline; leaf-ness rides live_data"
        );
        if !matches!(arm, SegmentRetireGovernor::Off) {
            assert!(
                native
                    .jit_direct
                    .direct
                    .data_segment_retire_record_for_test(key)
                    .is_some_and(|record| record.live_data()),
                "{arm:?}: the promoting entry sets live_data"
            );
            let head = ensure_compiled(&mut native, ENTRY);
            assert!(
                native.jit_direct.direct.link_source_declined(head.id()),
                "{arm:?}: a live key is a leaf"
            );
            assert!(
                !native.jit_direct.has_linked_successor(head.id()),
                "{arm:?}: no live outbound edge"
            );
            assert_eq!(
                native.jit_direct.outbound_targets_for_test(head.id()),
                [None, None],
                "{arm:?}: both LinkCells stay empty"
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
                native.eflags(),
                interp.eflags(),
                "{arm:?} round {round}: eflags"
            );
            assert_eq!(
                native.eflags(),
                interp.eflags(),
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
                    .data_segment_live_promotions
                    > 0,
                "{arm:?}: the cap must promote the key"
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

    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    // The cap-th turn retired the key, so put it back the way the next turn would.
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    let key = key_at(&cpu, ENTRY);
    let parked_id = match cpu.jit_direct.probe(key) {
        jit::direct::BlockProbe::Ready(id) => id,
        other => panic!("the head must be compiled at the cap, got {other:?}"),
    };
    assert!(
        cpu.jit_direct
            .direct
            .waiting_holds_source_for_test(parked_id),
        "non-vacuity for the unpark half: the head's second successor slot never resolves, \
         so it must be PARKED at the moment the decline fires"
    );
    assert!(
        cpu.jit_direct.has_linked_successor(parked_id),
        "non-vacuity for the strict arm: and it must be linked on the other slot"
    );

    // The turn that crosses into promotion. live_data is the leaf-ness, not a decline.
    assert!(!strict_turn(&mut cpu, &mut bus, 0));
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "non-vacuity: the promoting entry sets live_data"
    );
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "promote retires and forgets any decline"
    );
    let head = ensure_compiled(&mut cpu, ENTRY);
    let head_id = head.id();
    assert!(
        cpu.jit_direct.direct.link_source_declined(head_id),
        "a live key is a leaf"
    );
    assert!(
        !cpu.jit_direct.direct.waiting_holds_source_for_test(head_id),
        "resolve_successors must not park a live source"
    );
    assert!(
        !cpu.jit_direct.has_linked_successor(head_id),
        "nothing may have bound the cell"
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
    let entry_key = key_at(&cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(entry_key)
            .is_some_and(|record| record.live_data()),
        "non-vacuity: ENTRY promotes"
    );
    ensure_compiled(&mut cpu, ENTRY);
    // live_data bars ENTRY as a SOURCE, not as a TARGET: the predecessor may re-link into it.
    set_base(&mut cpu, SegmentIndex::Es, 0);
    cpu.jit_direct
        .direct
        .resolve_successors_for_test(predecessor.id());
    assert!(
        cpu.jit_direct.has_linked_successor(predecessor.id()),
        "the predecessor's OUTBOUND edge is not the live key's to bar"
    );
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
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!masked_turn(&mut cpu, &mut bus, 0));
    }
    let stalls = cpu.jit_direct.stall_snapshot();
    assert_eq!(
        stalls.data_segment_live_promotions, 1,
        "non-vacuity: the cap must have promoted"
    );
    assert_eq!(
        stalls.data_segment_link_declines, 0,
        "the masked arm never declines"
    );
    let key = key_at(&cpu, ENTRY);
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key)
    );
    assert_eq!(clears_by_cause(&cpu, "data_segment_decline"), 0);
    ensure_compiled(&mut cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data())
    );
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
    ensure_compiled(&mut cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "non-vacuity: the budget (and live_data) must hold this key before the write"
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
    let key = key_at(&cpu, ENTRY);
    cpu.jit_direct
        .direct
        .decline_links_for_data_segment_for_test(key);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "non-vacuity: the decline must have fired"
    );
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "this row is the non-live decline; live_data must not be the bar"
    );

    cpu.jit_direct.invalidate_translation();
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "the flush erased what the decline was earned against, so the decline goes with it"
    );

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
    let _ = &mut bus;
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
    let key = key_at(&cpu, ENTRY);
    cpu.jit_direct
        .direct
        .decline_links_for_data_segment_for_test(key);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "non-vacuity: the decline must have fired"
    );

    assert!(
        cpu.jit_direct.retire_key_for_recompile(key),
        "the other callers still retire a declined key"
    );
    assert!(
        !cpu.jit_direct
            .direct
            .data_segment_link_declined_for_test(key),
        "and that retire takes the decline with it"
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

/// FIXTURE 9. The knob's spelling table, both directions, with **unset and `""` on DIFFERENT
/// arms**.
///
/// Kills a silently inert ladder leg, and one thing more since the 2026-08-23 ladder flipped the
/// default to `cap`: the nulling trap is INVERTED for this knob. Everywhere else in `env_gates.rs`
/// unset means OFF and `env-null-empty-is-off-trap` bites the ON leg. Here unset means `cap` and
/// `""` means OFF, so PowerShell's `SetEnvironmentVariable($null)` DISARMS the shipped default
/// instead of failing to arm an opt-in one. The two cases are asserted apart, deliberately, with
/// that in the message: collapsing them is the mutation that would make a "default" leg silently
/// measure the escape.
#[test]
fn segment_retire_governor_knob_spellings() {
    let parse =
        |raw: &str| jit::direct::parse_segment_retire_governor_arm_for_test(Ok(raw.to_string()));
    assert_eq!(
        jit::direct::parse_segment_retire_governor_arm_for_test(Err(
            std::env::VarError::NotPresent
        )),
        SegmentRetireGovernor::Cap,
        "UNSET is the shipped default `cap` since the 2026-08-23 ladder"
    );
    assert_eq!(
        parse(""),
        SegmentRetireGovernor::Off,
        "and the EMPTY STRING is the escape, NOT the default: a leg that nulls this variable is \
         measuring `off`, which is the one place this knob's nulling trap runs backwards"
    );
    for raw in ["0", "off", "OFF", "  off  "] {
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
/// default. That got worse, not better, when the default became an ARMED arm: a typo used to run
/// a leg as `off` while its report named an arm, and now it runs the leg as `cap` while its report
/// names `on` -- a wrong number that looks like a plausible one.
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
        assert_eq!(native.eflags(), interp.eflags(), "round {round}");
        assert_eq!(
            native.registers.edx(),
            interp.registers.edx(),
            "round {round}: the ES read, which is the only value in this sweep a wrong baked base \
             could corrupt"
        );
        // NOT a memory compare. Neither shape in this file writes a byte of guest memory, so
        // `native_bus.memory == interp_bus.memory` is true for every implementation of everything
        // and cannot fail. The register comparisons above are what carry the differential.

        // The split identity, on a sweep that takes BOTH arms: `has_link` is the only thing
        // separating the two counters, and this pins that neither one can be double-charged or
        // dropped. Non-vacuous because the sweep starts at `HEAD_PRED` and at `ENTRY` at random
        // and declines edges as it goes, so both arms are genuinely populated.
        let perf = native.perf_counters();
        assert_eq!(
            perf.jit_direct_reject_data_segment_strict + perf.jit_direct_reject_data_segment_masked,
            perf.jit_direct_reject_data_segment,
            "round {round}: the arm split must partition the reject counter"
        );
        assert_eq!(
            perf.jit_direct_reject_data_segment_real
                + perf.jit_direct_reject_data_segment_v86
                + perf.jit_direct_reject_data_segment_pm16
                + perf.jit_direct_reject_data_segment_pm32,
            perf.jit_direct_reject_data_segment,
            "round {round}: the mode-key buckets must partition the reject counter"
        );
    }
    let perf = native.perf_counters().clone();
    assert!(
        perf.jit_direct_reject_data_segment_strict > 0
            && perf.jit_direct_reject_data_segment_masked > 0,
        "non-vacuity for the split: the sweep must have taken BOTH arms, got strict={} masked={}",
        perf.jit_direct_reject_data_segment_strict,
        perf.jit_direct_reject_data_segment_masked
    );
    assert!(
        native.perf_counters().jit_direct_insns > 0,
        "non-vacuity: the native side must actually have run guest code"
    );

    assert!(
        native
            .jit_direct
            .stall_snapshot()
            .data_segment_live_promotions
            > 0,
        "non-vacuity: the sweep must actually have promoted a sticky key"
    );
}

// ---------------------------------------------------------------------------------------------
// The governor's two inputs under `IZARRAVM_CHAIN_ENTRY_CHECK`. Repair 1 (the decline's EFFECT)
// and Repair 2 (its TRIGGER, and the census mask that round 2's R2-3 found one site short of).
// ---------------------------------------------------------------------------------------------

/// Restores the ambient entry-check arm. Forced BEFORE the fixture builds its CPU, because the
/// arm is read once per `BlockCache`.
struct EntryCheckGuard;

#[must_use]
fn force_chain_entry_check() -> EntryCheckGuard {
    jit::direct::set_chain_entry_check_for_test(Some(true));
    EntryCheckGuard
}

impl Drop for EntryCheckGuard {
    fn drop(&mut self) {
        jit::direct::set_chain_entry_check_for_test(None);
    }
}

/// REPAIR 2, the decline's TRIGGER. `own_mask_matches` must be built from the block's OWN mask.
///
/// `ENTRY` pins nothing at all and `SECOND` bakes ES, so on the armed arm the head rejects
/// against the CHAIN's `{ES}` while its own (empty) mask passes everything. Compute
/// `own_mask_matches` from the layout the entry check just used -- the chain one -- and it asks
/// "would the chain check have passed?", which is what the reject answered NO to. It would be
/// identically false, `decline_links_for_data_segment` would never fire on ANY row, and the
/// decline is the only shipped mechanism that reaches the own_pass - chain_pass residual.
///
/// REPAIR 1, the decline's EFFECT, rides the same row. The decline's promise is "the next entry
/// takes the check this block would have had if it had never linked". Under the chain entry check
/// that is only true if cutting the last cell also narrows the requirement -- otherwise the
/// declined block is re-checked against its stale-wide `{ES}` and rejects again, and the decline
/// buys nothing. The final third of this fixture is that claim, stated as a run.
#[test]
fn governor_own_mask_input_is_the_blocks_own_mask() {
    let _knob = force_chain_entry_check();
    let _guard = force_arm(SegmentRetireGovernor::On);
    let (mut cpu, mut bus) = strict_fixture();
    assert!(cpu.jit_direct.chain_entry_check_armed());
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);

    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    let key = key_at(&cpu, ENTRY);
    let head_id = match cpu.jit_direct.probe(key) {
        jit::direct::BlockProbe::Ready(id) => id,
        other => panic!("the head must be compiled at the cap, got {other:?}"),
    };
    assert!(
        cpu.jit_direct.has_linked_successor(head_id),
        "non-vacuity: the head must be LINKED, or the reject lands on the masked arm"
    );
    assert_ne!(
        cpu.jit_direct.chain_requirement_used_for_test(head_id),
        0,
        "non-vacuity: the head pins NOTHING itself, so a non-empty requirement can only be the          ES its successor pins"
    );

    // The turn that crosses into promotion. own_pass still counts on that reject.
    assert!(!strict_turn(&mut cpu, &mut bus, 0));
    assert!(
        cpu.jit_direct.stall_snapshot().entry_chain_reject_own_pass > 0,
        "the own_pass - chain_pass residual counter must have seen the promoting reject"
    );
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "the promoting entry sets live_data"
    );
    let head = ensure_compiled(&mut cpu, ENTRY);
    let head_id = head.id();
    assert_eq!(
        cpu.jit_direct.outbound_targets_for_test(head_id),
        [None, None],
        "live_data bars both cells; that is what makes the block a leaf"
    );
    assert_eq!(
        cpu.jit_direct.chain_requirement_used_for_test(head_id),
        0,
        "and a leaf's requirement is its own layout, which for this block pins nothing"
    );
    let block = cpu.jit_direct.block(head_id).expect("the block is live");
    set_base(&mut cpu, SegmentIndex::Es, SHIFT);
    cpu.set_eip(ENTRY);
    let before = cpu.perf_counters().clone();
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the live leaf must RUN under the moved ES"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment - before.jit_direct_reject_data_segment,
        0
    );
}

/// R2-3: the census mask the BUDGET is fingerprinted under must be the mask the reject was taken
/// under, and on the armed arm that is the chain requirement on BOTH arms of the reject.
///
/// The shape is the residual the narrowing deliberately cannot remove. `SECOND`'s portal is
/// cleared while the edge itself stays in the link graph -- what `suspend_decode_slot` and
/// `compact_arena` do -- so the head is entered with `has_link == false` and rejects on the MASKED
/// arm while still carrying its successor's `{ES}`. Leave that arm reading `segment_layouts` and
/// the mask is EMPTY, so every live ES folds to the same fingerprint and the per-key census
/// collapses to one layout. Two entries under two different ES bases separate the two readings.
#[test]
fn the_masked_arm_census_mask_follows_the_entry_check() {
    let _knob = force_chain_entry_check();
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);

    for live_es in [SHIFT, SHIFT * 2] {
        // Compile the head under the base `SECOND` baked, or the edge cannot form at all and the
        // head's requirement stays empty.
        set_base(&mut cpu, SegmentIndex::Es, 0);
        let block = ensure_compiled(&mut cpu, ENTRY);
        let second_id = match cpu.jit_direct.probe(key_at(&cpu, SECOND)) {
            jit::direct::BlockProbe::Ready(id) => id,
            other => panic!("the successor must stay compiled, got {other:?}"),
        };
        assert_ne!(
            cpu.jit_direct.chain_requirement_used_for_test(block.id()),
            0,
            "non-vacuity: the head must carry the successor's ES before the portal is hidden"
        );

        cpu.jit_direct.hide_block_portal_for_test(second_id);
        assert!(
            !cpu.jit_direct.has_linked_successor(block.id()),
            "non-vacuity: hiding the portal must make the VISIBILITY predicate read false"
        );
        assert_ne!(
            cpu.jit_direct.outbound_targets_for_test(block.id()),
            [None, None],
            "and the LINK GRAPH must still hold the edge -- this is the residual, not a leaf"
        );

        set_base(&mut cpu, SegmentIndex::Es, live_es);
        cpu.set_eip(ENTRY);
        let before = cpu.perf_counters().clone();
        assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(
            cpu.perf_counters().jit_direct_reject_data_segment_masked
                - before.jit_direct_reject_data_segment_masked,
            1,
            "the hidden successor puts this reject on the MASKED arm, against the CHAIN's mask"
        );
    }

    let record = cpu
        .jit_direct
        .direct
        .data_segment_retire_record_for_test(key_at(&cpu, ENTRY))
        .expect("the rejecting key must carry its census");
    assert_eq!(
        record.distinct_layouts(),
        2,
        "the two entries moved ES, and ES is in the mask the reject was taken under; a census \
         taken under the block's own (empty) mask folds both to one fingerprint"
    );
}

// ---------------------------------------------------------------------------------------------
// Sticky live DS/ES (problem (A)).
// ---------------------------------------------------------------------------------------------

#[must_use]
fn force_entry_check(armed: bool) -> EntryCheckGuard {
    jit::direct::set_chain_entry_check_for_test(Some(armed));
    EntryCheckGuard
}

fn jmp16(from: u32, to: u32) -> [u8; 3] {
    let next = from + 3;
    let rel = (to as i32 - next as i32) as i16;
    let bytes = rel.to_le_bytes();
    [0xe9, bytes[0], bytes[1]]
}

fn compile_sixteen(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, linear, false).expect("a 16-bit key");
    match cpu.jit_direct.probe(key) {
        jit::direct::BlockProbe::Ready(id) => {
            return cpu.jit_direct.block(id).expect("a live 16-bit block");
        }
        jit::direct::BlockProbe::Interpret | jit::direct::BlockProbe::Compile => {}
        other => panic!("unexpected 16-bit probe {other:?}"),
    }
    let compilation = match jit::direct::compile(cpu, linear, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the 16-bit block became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry(cause) => {
            panic!("the 16-bit block requested a retry: {cause:?}")
        }
    };
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the 16-bit block");
    cpu.jit_direct
        .block(id)
        .expect("the 16-bit block must be live")
}

fn sixteen_load_memory(with_successor: bool) -> Vec<u8> {
    let mut memory = vec![0; 0x5000];
    let load = [0x8b, 0x16, 0x00, 0x30];
    memory[ENTRY as usize..ENTRY as usize + 4].copy_from_slice(&load);
    let jmp_target = if with_successor { SECOND } else { DONE };
    memory[ENTRY as usize + 4..ENTRY as usize + 7].copy_from_slice(&jmp16(ENTRY + 4, jmp_target));
    if with_successor {
        memory[SECOND as usize..SECOND as usize + 4].copy_from_slice(&[0x8b, 0x0e, 0x00, 0x30]);
        memory[SECOND as usize + 4..SECOND as usize + 7].copy_from_slice(&jmp16(SECOND + 4, DONE));
    }
    memory[DONE as usize] = 0xf4;
    memory[DATA as usize..DATA as usize + 2].copy_from_slice(&0xbeefu16.to_le_bytes());
    memory[(DATA + SHIFT) as usize..(DATA + SHIFT) as usize + 2]
        .copy_from_slice(&0xcafeu16.to_le_bytes());
    memory
}

fn sixteen_load_cpu(with_successor: bool) -> (CpuGsw, TestBus) {
    let memory = sixteen_load_memory(with_successor);
    let mut bus = super::sixteen_bit::sixteen_bit_bus(memory);
    let mut cpu = super::sixteen_bit::sixteen_bit_code_cpu(ENTRY);
    super::sixteen_bit::arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, DATA]);
    let mut starts = vec![ENTRY, ENTRY + 4];
    if with_successor {
        starts.extend_from_slice(&[SECOND, SECOND + 4]);
    }
    super::sixteen_bit::warm_sixteen_bit(&mut cpu, &mut bus, &starts);
    (cpu, bus)
}

fn sixteen_key(cpu: &CpuGsw, linear: u32) -> jit::direct::BlockKey {
    jit::direct::key_for(cpu, linear, false).expect("a 16-bit key")
}

fn promote_sixteen_masked(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        set_base(cpu, SegmentIndex::Ds, 0);
        let block = compile_sixteen(cpu, ENTRY);
        set_base(cpu, SegmentIndex::Ds, SHIFT);
        cpu.set_eip(ENTRY);
        cpu.registers.set_edx(0);
        assert!(
            !cpu.try_run_direct_block_for_test(bus, block)
                .expect("a refused 16-bit entry is not a machine stop"),
            "every reject through the promoting entry is NotRun"
        );
        assert_eq!(cpu.registers.edx(), 0);
        assert_eq!(cpu.registers.eip, ENTRY);
    }
}

/// MASKED (A). A 16-bit real-mode DS-relative load block, no MOV DS, no successor.
///
/// Mutant: restoring the baked immediate on the sticky emit path, or leaving `used` unstripped.
#[test]
fn live_data_masked_sixteen_bit_load_runs_both_layouts() {
    for armed in [true, false] {
        let _check = force_entry_check(armed);
        let _guard = force_arm(SegmentRetireGovernor::Cap);
        let (mut cpu, mut bus) = sixteen_load_cpu(false);
        promote_sixteen_masked(&mut cpu, &mut bus);
        let key = sixteen_key(&cpu, ENTRY);
        assert_eq!(
            cpu.jit_direct.stall_snapshot().data_segment_live_promotions,
            1
        );
        assert!(
            cpu.jit_direct
                .direct
                .data_segment_retire_record_for_test(key)
                .is_some_and(|record| record.live_data())
        );
        let installs_before_live = cpu.jit_direct.direct.len();
        let block = compile_sixteen(&mut cpu, ENTRY);
        assert_eq!(
            cpu.jit_direct.direct.len(),
            installs_before_live + 1,
            "CHAIN_ENTRY_CHECK={armed}: one live compile"
        );
        let layout = cpu
            .jit_direct
            .segment_layout(block.id())
            .expect("live layout");
        assert_eq!(
            layout.used_for_test() & jit::direct::LIVE_DATA_BITS,
            0,
            "CHAIN_ENTRY_CHECK={armed}: installed used must drop DS/ES"
        );
        for (base, expected) in [(0, 0xbeefu32), (SHIFT, 0xcafeu32)] {
            set_base(&mut cpu, SegmentIndex::Ds, base);
            cpu.set_eip(ENTRY);
            cpu.registers.set_edx(0);
            assert!(
                cpu.try_run_direct_block_for_test(&mut bus, block)
                    .expect("live 16-bit entry"),
                "CHAIN_ENTRY_CHECK={armed} base {base:#x}: both layouts retire native"
            );
            assert_eq!(
                cpu.registers.edx(),
                expected,
                "CHAIN_ENTRY_CHECK={armed} base {base:#x}"
            );
        }
        assert_eq!(
            cpu.jit_direct.direct.len(),
            installs_before_live + 1,
            "CHAIN_ENTRY_CHECK={armed}: compile_attempts stay flat"
        );
        let _ = armed;
    }
}

/// STRICT (A). A 16-bit load root with a live linked successor that also pins DS.
///
/// Mutant: live_data read missing from link_source_declined, so the edge re-forms.
#[test]
fn live_data_strict_sixteen_bit_load_is_a_leaf() {
    for armed in [true, false] {
        let _check = force_entry_check(armed);
        let _guard = force_arm(SegmentRetireGovernor::Cap);
        let (mut cpu, mut bus) = sixteen_load_cpu(true);
        set_base(&mut cpu, SegmentIndex::Ds, 0);
        compile_sixteen(&mut cpu, SECOND);
        let root = compile_sixteen(&mut cpu, ENTRY);
        assert!(
            cpu.jit_direct.has_linked_successor(root.id()),
            "CHAIN_ENTRY_CHECK={armed}: the chain must form before promotion"
        );
        for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
            set_base(&mut cpu, SegmentIndex::Ds, 0);
            compile_sixteen(&mut cpu, SECOND);
            let block = compile_sixteen(&mut cpu, ENTRY);
            set_base(&mut cpu, SegmentIndex::Ds, SHIFT);
            cpu.set_eip(ENTRY);
            cpu.registers.set_edx(0);
            assert!(
                !cpu.try_run_direct_block_for_test(&mut bus, block)
                    .expect("a refused 16-bit entry is not a machine stop")
            );
        }
        assert_eq!(
            cpu.jit_direct.stall_snapshot().data_segment_live_promotions,
            1
        );
        let installs_before_live = cpu.jit_direct.direct.len();
        compile_sixteen(&mut cpu, SECOND);
        let root = compile_sixteen(&mut cpu, ENTRY);
        assert_eq!(cpu.jit_direct.direct.len(), installs_before_live + 1);
        let layout = cpu
            .jit_direct
            .segment_layout(root.id())
            .expect("live root layout");
        assert_eq!(layout.used_for_test() & jit::direct::LIVE_DATA_BITS, 0);
        assert!(
            cpu.jit_direct.direct.link_source_declined(root.id()),
            "CHAIN_ENTRY_CHECK={armed}: live_data bars the outbound edge"
        );
        assert!(
            !cpu.jit_direct.has_linked_successor(root.id()),
            "CHAIN_ENTRY_CHECK={armed}: no live outbound edge"
        );
        assert_eq!(
            cpu.jit_direct.outbound_targets_for_test(root.id()),
            [None, None]
        );
        for (base, expected) in [(0, 0xbeefu32), (SHIFT, 0xcafeu32)] {
            set_base(&mut cpu, SegmentIndex::Ds, base);
            cpu.set_eip(ENTRY);
            cpu.registers.set_edx(0);
            assert!(
                cpu.try_run_direct_block_for_test(&mut bus, root)
                    .expect("live 16-bit root"),
                "CHAIN_ENTRY_CHECK={armed} base {base:#x}"
            );
            assert_eq!(cpu.registers.edx(), expected);
        }
        assert_eq!(cpu.jit_direct.direct.len(), installs_before_live + 1);
        let _ = armed;
    }
}

fn protected16_at(entry: u32) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    let mut cs = SegmentRegister::flat(0x08, 0x9b);
    cs.default_size_32 = false;
    cs.base = 0;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        let mut data = SegmentRegister::flat(0x10, 0x93);
        data.default_size_32 = false;
        data.base = 0;
        cpu.registers.set_segment(segment, data);
    }
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(entry);
    cpu
}

fn stamp_ds(cpu: &mut CpuGsw, selector: u16, base: u32, limit: u32, access: u8) {
    let mut descriptor = cpu.registers.segment(SegmentIndex::Ds);
    descriptor.selector = selector;
    descriptor.base = base;
    descriptor.limit = limit;
    descriptor.access = access;
    descriptor.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ds, descriptor);
}

/// 16-bit PM twin. Compile live under a writable selector, then enter read-only.
///
/// Mutant: live-base plus baked limit/access.
#[test]
fn live_data_pm16_store_live_loads_limit_and_access() {
    for armed in [true, false] {
        let _check = force_entry_check(armed);
        let _guard = force_arm(SegmentRetireGovernor::Cap);
        let mut memory = vec![0; 0x5000];
        memory[ENTRY as usize..ENTRY as usize + 4].copy_from_slice(&[0x89, 0x16, 0x00, 0x30]);
        memory[ENTRY as usize + 4..ENTRY as usize + 7].copy_from_slice(&jmp16(ENTRY + 4, DONE));
        memory[DONE as usize] = 0xf4;
        let mut bus = super::sixteen_bit::sixteen_bit_bus(memory);
        let mut cpu = protected16_at(ENTRY);
        super::sixteen_bit::arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, DATA]);
        super::sixteen_bit::warm_sixteen_bit(&mut cpu, &mut bus, &[ENTRY, ENTRY + 4]);

        const WRITABLE: u8 = 0x93;
        const READ_ONLY: u8 = 0x91;
        stamp_ds(&mut cpu, 0x10, 0, 0x4000, WRITABLE);
        for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
            stamp_ds(&mut cpu, 0x10, 0, 0x4000, WRITABLE);
            let block = compile_sixteen(&mut cpu, ENTRY);
            stamp_ds(&mut cpu, 0x18, SHIFT, 0x5000, WRITABLE);
            cpu.set_eip(ENTRY);
            cpu.registers.set_edx(0x00aa);
            assert!(
                !cpu.try_run_direct_block_for_test(&mut bus, block)
                    .expect("a refused PM16 entry is not a machine stop")
            );
        }
        assert_eq!(
            cpu.jit_direct.stall_snapshot().data_segment_live_promotions,
            1
        );
        stamp_ds(&mut cpu, 0x10, 0, 0x4000, WRITABLE);
        let block = compile_sixteen(&mut cpu, ENTRY);
        let layout = cpu
            .jit_direct
            .segment_layout(block.id())
            .expect("live PM16 layout");
        assert_eq!(layout.used_for_test() & jit::direct::LIVE_DATA_BITS, 0);

        stamp_ds(&mut cpu, 0x10, 0, 0x4000, WRITABLE);
        cpu.set_eip(ENTRY);
        cpu.registers.set_edx(0x1111);
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(
            &bus.memory[DATA as usize..DATA as usize + 2],
            &0x1111u16.to_le_bytes()
        );

        stamp_ds(&mut cpu, 0x18, SHIFT, 0x5000, WRITABLE);
        cpu.set_eip(ENTRY);
        cpu.registers.set_edx(0x2222);
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(
            &bus.memory[(DATA + SHIFT) as usize..(DATA + SHIFT) as usize + 2],
            &0x2222u16.to_le_bytes()
        );

        let before_exits = cpu.perf_counters().jit_direct_exit_unavailable_or_kind;
        let stored = bus.memory[DATA as usize..DATA as usize + 2].to_vec();
        stamp_ds(&mut cpu, 0x20, 0, 0x4000, READ_ONLY);
        cpu.set_eip(ENTRY);
        cpu.registers.set_edx(0x3333);
        let ran = cpu
            .try_run_direct_block_for_test(&mut bus, block)
            .expect("a side-exit is not a machine stop");
        assert!(
            ran,
            "CHAIN_ENTRY_CHECK={armed}: used-strip must let the read-only entry reach the body"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_unavailable_or_kind - before_exits,
            1,
            "CHAIN_ENTRY_CHECK={armed}: store to a read-only selector side-exits"
        );
        assert_eq!(
            &bus.memory[DATA as usize..DATA as usize + 2],
            stored.as_slice(),
            "CHAIN_ENTRY_CHECK={armed}: the read-only entry must not store"
        );

        let before_limit = cpu.jit_direct.stall_snapshot().side_exit_segment_limit;
        stamp_ds(&mut cpu, 0x28, 0, 0x1000, WRITABLE);
        cpu.set_eip(ENTRY);
        cpu.registers.set_edx(0x4444);
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(
            cpu.jit_direct.stall_snapshot().side_exit_segment_limit - before_limit,
            1,
            "CHAIN_ENTRY_CHECK={armed}: a too-small live limit must side-exit"
        );
        let _ = armed;
    }
}

/// 16-bit `mov bx, ds` still refuses when DS's selector moves, sticky or not.
#[test]
fn sixteen_bit_mov_reg_sreg_keeps_the_selector_pin() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize..ENTRY as usize + 2].copy_from_slice(&[0x8c, 0xdb]);
    memory[ENTRY as usize + 2..ENTRY as usize + 5].copy_from_slice(&jmp16(ENTRY + 2, DONE));
    memory[DONE as usize] = 0xf4;
    let mut bus = super::sixteen_bit::sixteen_bit_bus(memory);
    let mut cpu = super::sixteen_bit::sixteen_bit_code_cpu(ENTRY);
    super::sixteen_bit::arm_native_sixteen_bit(&mut cpu, &mut bus, &[0]);
    super::sixteen_bit::warm_sixteen_bit(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2]);
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.selector = 0x1234;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    let block = compile_sixteen(&mut cpu, ENTRY);
    ds.selector = 0x5678;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu.set_eip(ENTRY);
    cpu.registers.set_ebx(0);
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "mov bx, ds must refuse when the selector moves"
    );
    assert_eq!(cpu.registers.ebx(), 0);
    let layout = cpu
        .jit_direct
        .segment_layout(block.id())
        .expect("selector-pin layout");
    assert_ne!(
        layout.used_for_test() & jit::direct::LIVE_DATA_BITS,
        0,
        "a selector pin keeps DS in used even if the key later promotes"
    );
}

/// `mov ds, ax` then a DS-relative load still ends the block at the load.
#[test]
fn dirty_segment_still_splits_mov_ds_then_load() {
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + 6].copy_from_slice(&[
        0x89, 0xf6, // mov si, si
        0x89, 0xff, // mov di, di
        0x8e, 0xd8, // mov ds, ax
    ]);
    let load_at = ENTRY + 6;
    memory[load_at as usize..load_at as usize + 4].copy_from_slice(&[0x8b, 0x16, 0x00, 0x30]);
    memory[load_at as usize + 4..load_at as usize + 7].copy_from_slice(&jmp16(load_at + 4, DONE));
    memory[DONE as usize] = 0xf4;
    let mut bus = super::sixteen_bit::sixteen_bit_bus(memory);
    let mut cpu = super::sixteen_bit::sixteen_bit_code_cpu(ENTRY);
    super::sixteen_bit::arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, DATA]);
    super::sixteen_bit::warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 2, ENTRY + 4, load_at, load_at + 4],
    );
    let write_block = compile_sixteen(&mut cpu, ENTRY);
    assert_eq!(
        write_block.span().instructions,
        3,
        "the load after MOV DS must not join the write block"
    );
    let load_block = compile_sixteen(&mut cpu, load_at);
    assert_eq!(load_block.span().instructions, 2);
    assert_ne!(write_block.id(), load_block.id());
}

/// Fixture 7 live twin: after promote, install the live compile, then flush; the edge must not
/// re-form. Mutant: `link_source_declined` still keyed only on the declined set.
#[test]
fn live_promoted_key_stays_a_leaf_across_a_flush() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    let key = key_at(&cpu, ENTRY);
    let head = ensure_compiled(&mut cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data())
    );
    assert!(cpu.jit_direct.direct.link_source_declined(head.id()));
    assert!(!cpu.jit_direct.has_linked_successor(head.id()));

    cpu.jit_direct.invalidate_translation();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    let second_key = key_at(&cpu, SECOND);
    cpu.jit_direct
        .revalidate_translation(second_key)
        .expect("the successor is still compiled across a link flush");
    let head = cpu
        .jit_direct
        .revalidate_translation(key)
        .expect("the live key is Compiled, not Seen");
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "live_data survives the flush"
    );
    assert!(
        cpu.jit_direct.direct.link_source_declined(head.id()),
        "link_source_declined must read live_data with the declined set empty"
    );
    assert!(
        !cpu.jit_direct.has_linked_successor(head.id()),
        "the edge must not re-form"
    );
}

/// Fixture 7b live half: a CS retire of a live-promoted key keeps leaf-ness via live_data.
#[test]
fn a_cs_layout_retire_of_a_live_key_keeps_leaf_ness() {
    let _guard = force_arm(SegmentRetireGovernor::Cap);
    let (mut cpu, mut bus) = strict_fixture();
    set_base(&mut cpu, SegmentIndex::Es, 0);
    ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    for _ in 0..u32::from(DATA_SEGMENT_RETIRE_CAP) + 1 {
        assert!(!strict_turn(&mut cpu, &mut bus, 0));
    }
    let key = key_at(&cpu, ENTRY);
    ensure_compiled(&mut cpu, ENTRY);
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data())
    );
    assert!(cpu.jit_direct.retire_key_for_recompile(key));
    assert!(
        cpu.jit_direct
            .direct
            .data_segment_retire_record_for_test(key)
            .is_some_and(|record| record.live_data()),
        "live_data survives retire_key_for_recompile"
    );
    set_base(&mut cpu, SegmentIndex::Es, 0);
    let head = ensure_compiled(&mut cpu, ENTRY);
    ensure_compiled(&mut cpu, SECOND);
    assert!(
        cpu.jit_direct.direct.link_source_declined(head.id()),
        "the recompile is born a leaf because live_data survived"
    );
    assert!(!cpu.jit_direct.has_linked_successor(head.id()));
}
