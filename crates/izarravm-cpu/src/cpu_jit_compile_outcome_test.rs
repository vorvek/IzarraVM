// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::run::ContinuationDispatch;

const ENTRY: u32 = 0x100;

fn fresh() -> CpuGsw {
    fresh_in_mode(GswMode::Gsw586)
}

fn fresh_in_mode(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    for segment in [SegmentIndex::Cs, SegmentIndex::Ds, SegmentIndex::Ss] {
        cpu.load_segment_real(segment, 0);
    }
    for segment in [SegmentIndex::Cs, SegmentIndex::Ss] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.default_size_32 = true;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.registers.eip = ENTRY;
    cpu
}

fn fixture(code: &[u8]) -> (CpuGsw, TestBus) {
    fixture_at(ENTRY, code)
}

fn fixture_in_mode(code: &[u8], mode: GswMode) -> (CpuGsw, TestBus) {
    fixture_at_in_mode(ENTRY, code, mode)
}

fn fixture_at(entry: u32, code: &[u8]) -> (CpuGsw, TestBus) {
    fixture_at_in_mode(entry, code, GswMode::Gsw586)
}

fn fixture_at_in_mode(entry: u32, code: &[u8], mode: GswMode) -> (CpuGsw, TestBus) {
    let memory_len = (entry as usize + code.len()).max(0x2000);
    let mut memory = vec![0; memory_len];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh_in_mode(mode);
    cpu.registers.eip = entry;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

#[cfg(feature = "direct-admission-census")]
fn admission_declines(cpu: &CpuGsw) -> [u64; 4] {
    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("admission census must be enabled");
    assert_eq!(
        snapshot
            .admission_declines
            .iter()
            .map(|(label, _)| *label)
            .collect::<Vec<_>>(),
        [
            "heat_refusal",
            "key_failure",
            "dormant_probe",
            "rejected_probe",
        ]
    );
    std::array::from_fn(|index| snapshot.admission_declines[index].1)
}

fn warm(cpu: &mut CpuGsw, bus: &mut TestBus, addresses: &[u32]) {
    for &linear in addresses {
        cpu.registers.eip = linear;
        cpu.begin_instruction();
        cpu.fetch_decoded(bus, linear).expect("fixture decode");
    }
}

fn structural(outcome: jit::direct::CompileOutcome) -> jit::direct::RejectedSpan {
    match outcome {
        jit::direct::CompileOutcome::StructuralReject(span) => span,
        jit::direct::CompileOutcome::Compiled(_) => panic!("fixture unexpectedly compiled"),
        jit::direct::CompileOutcome::Retry(_) => panic!("fixture unexpectedly requested a retry"),
    }
}

fn compiled(outcome: jit::direct::CompileOutcome) -> jit::direct::Compilation {
    match outcome {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("fixture unexpectedly became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("fixture unexpectedly requested a retry"),
    }
}

#[test]
fn structural_rejection_includes_the_complete_short_prefix_and_barrier() {
    let cases: &[(&[u8], &[u32], u16)] = &[
        (&[DIRECT_BARRIER], &[ENTRY], 1),
        (&[0x40, DIRECT_BARRIER], &[ENTRY, ENTRY + 1], 2),
        (
            &[0x40, 0x41, DIRECT_BARRIER],
            &[ENTRY, ENTRY + 1, ENTRY + 2],
            3,
        ),
    ];

    for &(code, addresses, expected_len) in cases {
        let (mut cpu, mut bus) = fixture(code);
        warm(&mut cpu, &mut bus, addresses);

        let span = structural(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(span.key().linear, ENTRY);
        assert_eq!(span.key().physical, ENTRY);
        assert_eq!(span.guest_len(), expected_len);
    }
}

#[test]
fn three_supported_slots_compile_before_an_unsupported_barrier() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, DIRECT_BARRIER]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}

#[test]
fn barrier_census_is_opt_in_and_counts_runtime_hits_for_an_interior_shape() {
    // Not 0xFC (CLD, natively lowered already) and not 0x99/CDQ (the CHOICE THIS TEST USED TO
    // MAKE, until the adversarial review on the PreciseHelper-scaffolding deletion caught that
    // Task 4 -- the very next slice in this phase -- teaches `classify` to admit 0x98/0x99,
    // which would silently turn this fixture's span from 4 instructions to 8, drop its census
    // row, and panic the `.expect` below. That is the exact 0xFC failure class the sibling
    // `DIRECT_BARRIER` constant's own doc comment (`cpu_test.rs`) narrates: "it will eventually
    // be lowered too". `DIRECT_BARRIER` is durable FOR THIS TEST specifically because, unlike
    // 0x99, it carries its own certifying test (`direct_barrier_opcode_is_still_unclassifiable`,
    // below in this file) that fails LOUDLY the day it stops being a barrier, rather than degrading
    // silently -- so reusing it here means a future re-classification is caught upstream, not
    // discovered here first. This is the same shape the old
    // `..._scores_an_interior_helper_shape` test used before the `PreciseHelper` scaffolding
    // (`HelperFamily`/`eligible_shapes`/`selected`) it exercised was deleted: `runtime_hits`,
    // ungated from helper families in `cd24b945`, is the one column that survives that deletion.
    let code = [
        0x40,
        0x41,
        0x42,
        0x43,
        DIRECT_BARRIER,
        0x44,
        0x45,
        0x46,
        0x47,
    ];
    let addresses: Vec<_> = (0..code.len())
        .map(|offset| ENTRY + offset as u32)
        .collect();

    let (mut disabled_cpu, mut disabled_bus) = fixture(&code);
    warm(&mut disabled_cpu, &mut disabled_bus, &addresses);
    let _ = compiled(jit::direct::compile(&mut disabled_cpu, ENTRY, true));
    assert!(disabled_cpu.direct_barrier_census_snapshot().is_none());

    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    warm(&mut cpu, &mut bus, &addresses);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 4);

    // Simulate the barrier opcode retiring once through the interpreter: this is the hook
    // `run.rs` calls on every interpreted retirement, and `runtime_hits` is the census's only
    // per-execution column.
    let barrier_linear = ENTRY + 4;
    cpu.registers.eip = barrier_linear;
    cpu.begin_instruction();
    let insn = cpu
        .fetch_decoded(&mut bus, barrier_linear)
        .expect("re-decode the barrier instruction");
    cpu.jit_direct.note_barrier_census_interpreted(&insn);

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == u16::from(DIRECT_BARRIER))
        .expect("recorded structural stop");
    assert_eq!(row.hits, 1);
    assert_eq!(row.native_prefix_instructions, 4);
    assert_eq!(row.native_suffix_instructions, 4);
    assert_eq!(row.runtime_hits, 1);
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_attributes_a_heat_refusal_at_the_production_seam() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    cpu.enable_direct_barrier_census(true);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(8);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("heat refusal");

    assert_eq!(admission_declines(&cpu), [1, 0, 0, 0]);
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_attributes_a_key_failure_at_the_production_seam() {
    const BIOS_ENTRY: u32 = 0x000f_0000;
    let (mut cpu, mut bus) = fixture_at(BIOS_ENTRY, &[0x40, 0x41, 0x42]);
    cpu.load_segment_real(SegmentIndex::Cs, 0xf000);
    let mut cs = cpu.registers.segment(SegmentIndex::Cs);
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.registers.eip = 0;
    cpu.enable_direct_barrier_census(true);
    cpu.set_sixteen_bit_admission_level(1);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(1);
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, BIOS_ENTRY)
        .expect("fixture decode");

    cpu.try_direct_continuation_for_test(&mut bus, BIOS_ENTRY, true)
        .expect("key refusal");

    assert_eq!(admission_declines(&cpu), [0, 1, 0, 0]);
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_attributes_a_cooled_dormant_before_lifting_it() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    cpu.enable_direct_barrier_census(true);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(1);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation");
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.sync_smc_heat();
    let jit = &mut *cpu.jit_direct;
    jit.direct.demote_smc_hot(&mut jit.smc_heat, key, 0);
    cpu.perf.instructions = 1u64 << jit::direct::SMC_HEAT_EPOCH_SHIFT;

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("cooled dormant probe");

    assert_eq!(admission_declines(&cpu), [0, 0, 1, 0]);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Compile
    ));
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_attributes_a_rejected_probe_at_the_production_seam() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    cpu.enable_direct_barrier_census(true);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(1);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation");
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.jit_direct
        .reject(jit::direct::RejectedSpan::new(key, 1).expect("rejected fixture span"));
    cpu.sweep_block_watch_edges();

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("rejected probe");

    assert_eq!(admission_declines(&cpu), [0, 0, 0, 1]);
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_leaves_first_touch_and_seen_compile_unattributed() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    cpu.enable_direct_barrier_census(true);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(1);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first touch");
    assert_eq!(admission_declines(&cpu), [0; 4]);
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("seen compile");
    assert_eq!(admission_declines(&cpu), [0; 4]);
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_is_a_partial_subset_of_consulted_declines() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    cpu.enable_direct_barrier_census(true);
    cpu.jit_direct.set_admission_heat_for_test(8);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    let mut consulted_declines = 0u64;

    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("auto-admit refusal"),
        ContinuationDispatch::Declined
    );
    consulted_declines += 1;
    cpu.set_jit_auto_admit(true);
    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("heat refusal"),
        ContinuationDispatch::Declined
    );
    consulted_declines += 1;

    let attributed: u64 = admission_declines(&cpu).into_iter().sum();
    assert_eq!(attributed, 1);
    assert!(attributed < consulted_declines);
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_off_keeps_the_snapshot_absent_on_a_refusal() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(8);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("unprofiled heat refusal");

    assert!(cpu.direct_barrier_census_snapshot().is_none());
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn admission_census_preserves_the_cooled_dormant_dispatch_and_state() {
    fn drive(enabled: bool) -> (ContinuationDispatch, CpuGsw, jit::direct::BlockKey) {
        let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
        cpu.enable_direct_barrier_census(enabled);
        cpu.set_jit_auto_admit(true);
        cpu.jit_direct.set_admission_heat_for_test(1);
        warm(&mut cpu, &mut bus, &[ENTRY]);
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("first observation");
        let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
        cpu.sync_smc_heat();
        let jit = &mut *cpu.jit_direct;
        jit.direct.demote_smc_hot(&mut jit.smc_heat, key, 0);
        cpu.perf.instructions = 1u64 << jit::direct::SMC_HEAT_EPOCH_SHIFT;

        let dispatch = cpu
            .dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("cooled dormant dispatch");
        (dispatch, cpu, key)
    }

    let (off_dispatch, mut off, off_key) = drive(false);
    let (on_dispatch, mut on, on_key) = drive(true);
    assert_eq!(off_dispatch, ContinuationDispatch::Declined);
    assert_eq!(on_dispatch, off_dispatch);
    assert_eq!(on.registers, off.registers);
    assert_eq!(
        format!("{:?}", on.perf_counters()),
        format!("{:?}", off.perf_counters())
    );
    assert!(matches!(
        off.jit_direct.probe(off_key),
        jit::direct::BlockProbe::Compile
    ));
    assert!(matches!(
        on.jit_direct.probe(on_key),
        jit::direct::BlockProbe::Compile
    ));
    assert!(off.direct_barrier_census_snapshot().is_none());
    assert_eq!(admission_declines(&on), [0, 0, 1, 0]);
}

#[test]
fn barrier_census_does_not_admit_an_unaudited_structural_stop() {
    let code = [
        0x40,
        0x41,
        0x42,
        0x43,
        DIRECT_BARRIER,
        0x44,
        0x45,
        0x46,
        0x47,
    ];
    let addresses: Vec<_> = (0..code.len())
        .map(|offset| ENTRY + offset as u32)
        .collect();
    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    warm(&mut cpu, &mut bus, &addresses);

    let _ = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot.rows.first().expect("recorded structural stop");
    assert_eq!(row.opcode, u16::from(DIRECT_BARRIER));
    assert_eq!(row.runtime_hits, 0);
}

// Mutation record for the suffix-agreement slice, each verified by hand against this tree. Every
// fix is covered by exactly ONE test, and no mutation moved a test other than its own:
//
//  * dropping the loop-top memory-ALU cap makes `census_suffix_respects_the_memory_alu_block_cap`
//    read 5 where it must read 0;
//  * dropping the call-out slot cap makes `census_suffix_respects_the_call_out_slot_cap` read 2
//    where it must read 0;
//  * restoring the bare `!insn.continuable` makes
//    `census_suffix_admits_the_non_continuable_imul_forms` read 0 where it must read 3.
//
//  * deleting the page-budget mirror makes `census_suffix_stops_where_the_page_budget_does` read
//    31 where it must read 9. That divergence is the one the S3 page budget introduced, and it is
//    not bounded by a constant: it grows with how memory-heavy the path is, which is the axis the
//    suffix column ranks on.
//
//  * deleting the demoted-site mirror makes `census_suffix_stops_at_a_demoted_call_out_site` read
//    5 where it must read 2, which is a demoted row claiming the coverage it just gave back.
//
//  * deleting the L1 heat-gate mirror makes `census_suffix_mirrors_the_l1_heat_gate` read 4 on its
//    hot leg where it must read 2. That is the seventh divergence, and it is the only one whose
//    over-report is CORRELATED with the quantity the arm measures rather than merely bounded.
//
// The x87 divergence has NO test here, deliberately and not by omission: the forward scan refuses
// every x87 kind outright, so the compile walk's x87 caps are unreachable from it and any mirror
// of them would be dead code no fixture could make fire. It is left as a conservative floor and
// documented at `census_native_suffix`.
//
// Mutation record for the dirty-stop slice, same method:
//
//  * flipping the `DirtySegment` call site to `model_dirty: true` makes
//    `a_dirty_segment_stop_is_censused_and_prices_its_own_removal` read 0 where it must read 3,
//    which is the arm pricing its own fix at nothing;
//  * dropping either the `barred_segment_write` fold-in OR the dirty test inside the scan makes
//    `the_suffix_seed_carries_a_barred_segment_write` read 2 where it must read 0. The two share
//    a test on purpose: a seed nothing reads and a reader with nothing seeded are the same bug
//    seen from opposite ends, and neither is observable alone;
//  * making `installs_rejected_span` return true for every arm makes
//    `a_dirty_segment_row_does_not_claim_the_rejected_span_map` credit an exit it has no claim on.
//
// Mutation record for the segment-write entry counter. All three fail
// `a_segment_write_block_is_counted_at_the_dispatcher_entry`, and the last two are why that test
// carries three cases rather than two:
//
//  * zeroing the increment in `run.rs` leaves the counter flat;
//  * dropping `!self.dynamic_successor` from `is_segment_write_block` counts the RET-terminated
//    block, which publishes `[None, None]` for an entirely different reason;
//  * dropping `self.successors == [None, None]` counts nothing and misses the real one.

/// Compile `code` with the census on and return the single barrier row's suffix.
///
/// `warm_offsets` is the instruction start list, and it is what BOUNDS the suffix: the forward
/// scan reads `decode_cache`, so an address that was never warmed ends the walk. That makes the
/// expected value in each caller an exact number rather than a floor.
fn barrier_suffix(code: &[u8], warm_offsets: &[u32]) -> u64 {
    census_row_for(code, warm_offsets, u16::from(DIRECT_BARRIER)).native_suffix_instructions
}

/// The same, for a fixture whose barrier is a real opcode rather than the synthetic one.
fn census_row_for(code: &[u8], warm_offsets: &[u32], opcode: u16) -> crate::DirectBarrierCensusRow {
    let (mut cpu, mut bus) = fixture(code);
    cpu.enable_direct_barrier_census(true);
    let addresses: Vec<_> = warm_offsets.iter().map(|offset| ENTRY + offset).collect();
    warm(&mut cpu, &mut bus, &addresses);
    let _ = jit::direct::compile(&mut cpu, ENTRY, true);
    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == opcode)
        .unwrap_or_else(|| panic!("no census row for opcode {opcode:#x}"));
    assert_eq!(row.hits, 1, "fixture must record exactly one barrier hit");
    row.clone()
}

/// The L1 heat gate's suffix mirror, and it is the seventh divergence in the scan's ledger.
///
/// The gate is the ONLY admission rule in the backend that is not a `classify` answer: on the
/// `heat_gated` arm a group-2 row whose count byte carries a heat record classifies Native and is
/// downgraded to `HardBoundary` afterwards, in the compile walk, where the physical address and
/// the heat map are in scope. `census_native_suffix` calls `classify` directly, so without the
/// mirror it walks straight through the instruction the compile walk stops at.
///
/// That over-report is the worst possible shape for this particular slice rather than a generic
/// inaccuracy: it appears only at PATCHED sites, so its size is proportional to `(1 - u)` — and
/// `u`, the unpatched share, is the single number the arm exists to measure. A suffix column that
/// moved with `(1 - u)` would corrupt the heat-vs-off census difference the ladder's non-vacuity
/// check reads (dev_docs/duke-reprofile-2026-08-19.md §6.2).
///
/// One fixture, two legs, differing ONLY in whether the count byte carries a record:
///
/// * cold count byte — the ROL is admitted by both the compile walk and the scan, so the scan
///   reaches every warmed instruction after the barrier: 4;
/// * hot count byte — the compile walk stops at the ROL, so the scan must too: 2, the two `inc`s
///   between the barrier and the ROL.
///
/// Deleting the mirror in `census_native_suffix` makes the second leg read 4, which is exactly the
/// silent over-report described above.
#[test]
fn census_suffix_mirrors_the_l1_heat_gate() {
    // ENTRY+0 barrier, +1 inc eax, +2 inc ecx, +3..+5 rol ebx,16, +6 inc edx. The ROL's count byte
    // is its imm8 at ENTRY + 5. ENTRY+7 is deliberately left unwarmed so the cold leg's expected
    // value is an exact number rather than a floor, the same discipline as `barrier_suffix`.
    let code = [DIRECT_BARRIER, 0x40, 0x41, 0xc1, 0xc3, 0x10, 0x42];
    let offsets = [0, 1, 2, 3, 6];

    assert_eq!(
        heat_gated_barrier_suffix(&code, &offsets, None),
        4,
        "control: with no record on the count byte the heat gate admits the ROL and the scan \
         reaches every warmed instruction after the barrier"
    );
    assert_eq!(
        heat_gated_barrier_suffix(&code, &offsets, Some(ENTRY + 5)),
        2,
        "a record on the count byte stops the COMPILE WALK at the ROL, so the suffix scan must \
         stop there too; without the mirror this reads 4 and the over-report tracks (1 - u)"
    );
}

/// `barrier_suffix` on the `heat_gated` arm, with an optional SMC heat record seeded at one
/// physical byte first. The arm is forced through the thread-local override rather than the
/// process-wide `OnceLock`, and restored before returning.
fn heat_gated_barrier_suffix(code: &[u8], warm_offsets: &[u32], heat_at: Option<u32>) -> u64 {
    let (mut cpu, mut bus) = fixture(code);
    cpu.enable_direct_barrier_census(true);
    let addresses: Vec<_> = warm_offsets.iter().map(|offset| ENTRY + offset).collect();
    warm(&mut cpu, &mut bus, &addresses);
    // One `bump` is what `note_code_write_inner` leaves after one heat-charged kill, the same
    // seeding the disp-lane and classify-side fixtures use on the other sides of this probe.
    if let Some(physical) = heat_at {
        cpu.sync_smc_heat();
        cpu.jit_direct.smc_heat.bump(physical, 1, 0);
    }
    jit::direct::set_rotate_rows_arm_for_test(Some(jit::direct::RotateRowsArm::HeatGated));
    let _ = jit::direct::compile(&mut cpu, ENTRY, true);
    jit::direct::set_rotate_rows_arm_for_test(None);
    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == u16::from(DIRECT_BARRIER))
        .expect("no census row for the barrier");
    assert_eq!(row.hits, 1, "fixture must record exactly one barrier hit");
    row.native_suffix_instructions
}

/// The memory-ALU BLOCK cap, and it is the largest of the six divergences the suffix audit found.
///
/// `compile_with_instruction_limit` breaks at its LOOP TOP on `memory_alu_slots != 0 &&
/// slots.len() == MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS`, before the next instruction is decoded and
/// regardless of what that instruction turns out to be. The forward scan applied the same bound
/// only when the next kind was ITSELF memory-ALU, so any barrier whose prefix held one
/// read-modify-write slot over-reported its suffix by up to 28 instructions against a
/// 32-instruction ceiling — on the very column the campaign ranks rows by.
///
/// The two fixtures are `add [ebx], eax` and `add eax, ecx`: SAME opcode, SAME length, differing
/// only in the ModRM mod field. So the pair isolates `is_memory_alu` and nothing else, and a
/// lowering change that moved either form would fail here rather than silently re-diverge.
#[test]
fn census_suffix_respects_the_memory_alu_block_cap() {
    let offsets = [0, 2, 3, 4, 5, 6, 7, 8, 9];
    let memory = [
        0x01,
        0x03,
        0x40,
        0x41,
        DIRECT_BARRIER,
        0x42,
        0x43,
        0x44,
        0x45,
        0x46,
    ];
    let register = [
        0x01,
        0xc8,
        0x40,
        0x41,
        DIRECT_BARRIER,
        0x42,
        0x43,
        0x44,
        0x45,
        0x46,
    ];

    assert_eq!(
        barrier_suffix(&register, &offsets),
        5,
        "control: with no memory-ALU slot in the prefix the cap is not armed and the scan reaches \
         every warmed instruction after the barrier"
    );
    // Three prefix slots plus the barrier itself is already MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS, so
    // the counterfactual block is full before the scan takes a single step. The cap bounds
    // `prefix + 1 + suffix`, NOT the suffix alone.
    assert_eq!(
        barrier_suffix(&memory, &offsets),
        0,
        "one memory-ALU slot in the prefix arms the loop-top cap"
    );
}

/// The compile walk admits two non-continuable opcodes that the forward scan refused.
///
/// `block_continuable` (decode.rs) says no to `0x69`/`0x6b`, and the compile walk overrides it
/// with `insn.continuable || jit_admits_non_continuable(insn.opcode)`. The scan took the bare
/// flag, so any suffix that reached an IMUL-with-immediate was truncated there. This
/// UNDER-reported, which is the safer direction but still a disagreement.
///
/// A refusal would stop the walk rather than skip the instruction, so the failing value is 0 and
/// not 2. That is what makes one assertion enough.
#[test]
fn census_suffix_admits_the_non_continuable_imul_forms() {
    // `inc eax / inc ecx / inc edx / <barrier> / imul eax, eax, 5 / inc ebx / inc esp`
    let offsets = [0, 1, 2, 3, 4, 7, 8];
    let code = [
        0x40,
        0x41,
        0x42,
        DIRECT_BARRIER,
        0x6b,
        0xc0,
        0x05,
        0x43,
        0x44,
    ];

    assert_eq!(
        barrier_suffix(&code, &offsets),
        3,
        "the IMUL must be counted AND walked through to the two instructions behind it"
    );
}

/// The call-out slot cap.
///
/// PUSHA and POPA are `DirectKind::CallOut` and are deliberately NOT `uses_stack`, which is what
/// lets this fixture exercise `MAX_BLOCK_CALLOUT_SLOTS` in isolation: both caps are 4, so a kind
/// that counted against each would leave the test unable to say which one fired.
#[test]
fn census_suffix_respects_the_call_out_slot_cap() {
    let offsets = [0, 1, 2, 3, 4, 5, 6];
    let full = [0x60, 0x61, 0x60, 0x61, DIRECT_BARRIER, 0x60, 0x40];
    let spare = [0x60, 0x61, 0x60, 0x40, DIRECT_BARRIER, 0x60, 0x40];

    assert_eq!(
        barrier_suffix(&spare, &offsets),
        2,
        "control: three call-out slots in the prefix leaves room for the one behind the barrier"
    );
    assert_eq!(
        barrier_suffix(&full, &offsets),
        0,
        "a full call-out budget must stop the scan at the next call-out"
    );
}

/// The dirty-segment rule now leaves a census row, and that row's suffix prices the rule's own
/// removal rather than re-applying it.
///
/// Before this, admitting `MOV DS,r16` looked like it deleted 18.4M census hits while the census
/// showed nothing gained anywhere. The hits had not gone: the block now ends at the first later
/// slot that wants the overwritten segment, which was a `CompileStop::Boundary` and so recorded
/// nothing at all.
///
/// The suffix assertion is the whole test and it is a COUNTERFACTUAL, which is what lets it stand
/// in for a comparison against a value the public surface cannot produce. Every instruction after
/// the barrier reads through DS, so with `model_dirty` left true the scan would stop at the first
/// of them and report 0. Reporting 3 is only possible with the rule disabled.
#[test]
fn a_dirty_segment_stop_is_censused_and_prices_its_own_removal() {
    // inc eax / inc ecx / mov ds, ax / mov eax,[ebx] / mov ecx,[ebx] / mov edx,[ebx] / inc eax
    //                                  ^ barred: DS is dirty and this slot pins it
    let code = [
        0x40, 0x41, 0x8e, 0xd8, 0x8b, 0x03, 0x8b, 0x0b, 0x8b, 0x13, 0x40,
    ];
    let offsets = [0, 1, 2, 4, 6, 8, 10];

    let row = census_row_for(&code, &offsets, 0x8b);
    assert_eq!(
        row.stop_reason, "dirty_segment",
        "the 0x8b row must be attributed to the dirty rule, not to opcode coverage"
    );
    assert_eq!(
        row.native_prefix_instructions, 3,
        "the two increments and the segment load are kept; the rule ends the block after them"
    );
    assert_eq!(
        row.native_suffix_instructions, 3,
        "with the dirty rule disabled the scan walks through every later DS reader; with it \
         applied this reads 0, which is the counterfactual this number stands against"
    );
}

/// The COMPILE WALK's Word refusal moves with the flag.
///
/// Distinct from the census test below and from the `key_for_phys` test in the sixteen-bit file,
/// and all three are needed: the three gates are separate code sites and reverting any one of
/// them alone must fail something. This one is the walk itself, exercised by putting the Word
/// instruction MID-BLOCK in a 32-bit segment at I486, where `key_for_phys` admits the key and the
/// walk is the only thing that can refuse the slot.
#[test]
fn the_compile_walk_word_refusal_moves_with_the_flag() {
    // inc eax / inc ecx / inc edx / mov cx,ax / inc ebx / inc esp
    let code = [0x40, 0x41, 0x42, 0x66, 0x89, 0xc1, 0x43, 0x44];
    let offsets = [0u32, 1, 2, 3, 6, 7];

    for (label, admitted, expected) in [("refused", false, 3), ("admitted", true, 6)] {
        let (mut cpu, mut bus) = fixture_in_mode(&code, GswMode::Gsw486);
        cpu.set_word_operands_at_486(admitted);
        let addresses: Vec<_> = offsets.iter().map(|offset| ENTRY + offset).collect();
        warm(&mut cpu, &mut bus, &addresses);
        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, expected,
            "{label}: the walk must stop at the Word slot only while the flag refuses it"
        );
    }
}

/// The flag never admits the 386 class, whatever it is set to.
///
/// `key_for_phys` refuses every persona below I486 a few lines above the clause this slice
/// touches, so the 386 case is already dead. Pinned anyway: the predicate spells I386 out
/// explicitly so that a future 386 enablement cannot silently inherit Word admission, and that
/// intent is worth a test rather than a comment.
#[test]
fn the_word_flag_never_admits_the_386_class() {
    let code = [0x40, 0x41, 0x42, 0x66, 0x89, 0xc1, 0x43, 0x44];
    let (mut cpu, mut bus) = fixture_in_mode(&code, GswMode::Gsw386);
    cpu.set_word_operands_at_486(true);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    assert!(
        jit::direct::key_for(&cpu, ENTRY, true).is_none(),
        "the 386 class must stay refused with the flag on"
    );
}

/// Key admission follows a LIVE persona switch, in both directions.
///
/// `key_for_phys`'s host/persona screen is no longer evaluated on the spot; it reads
/// `JitState::native_keys_admitted`, refreshed by `set_mode`. That cache is what this pins. Delete
/// the `refresh_native_key_admission()` call in `CpuGsw::set_mode` and the second iteration below
/// fails: the CPU keeps answering with the persona it was constructed at, so a 386 CPU still keys
/// blocks the Direct backend must never compile for it. (The `debug_assert` inside `key_for_phys`
/// trips first in a debug build; this test is the one that still fails when asserts are off.)
///
/// The oracle is the predicate itself rather than a literal `true`, so the test says the same
/// thing on a host without AVX2 — where every arm is refused and the whole Direct suite is
/// vacuous anyway — while still separating the personas on the hosts that run the campaign.
#[test]
fn key_admission_follows_a_live_persona_switch() {
    let code = [0x40, 0x41, 0x42];
    let (mut cpu, mut bus) = fixture_in_mode(&code, GswMode::Gsw586);
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw386Slow,
        GswMode::Gsw586,
    ] {
        cpu.set_mode(mode);
        // `set_mode` invalidates the decode caches, so the line has to be re-warmed before
        // `key_for` can find its physical start.
        warm(&mut cpu, &mut bus, &[ENTRY]);
        assert_eq!(
            jit::direct::key_for(&cpu, ENTRY, true).is_some(),
            jit::direct::native_keys_admitted(mode),
            "{mode:?}: key admission must track the persona the CPU is running RIGHT NOW"
        );
    }
}

/// The census suffix scan carries the SAME Word predicate as the compile walk.
///
/// There are three copies of this policy: the compile walk, `key_for_phys`, and the forward scan
/// in `census_native_suffix`. A slice that lifted the first two and forgot the third would
/// re-open a seventh divergence between the two walks, days after six were closed, and on the
/// exact arm the A/B is measuring.
///
/// It has to be tested at I486 with the flag flipped, because at I586 the predicate is true either
/// way and the two arms are indistinguishable. The suffix instructions are 66-prefixed so their
/// operand size is Word in a 32-bit segment, which is the same `OperandSize::Word` a CS.D = 0
/// segment produces for every instruction.
#[test]
fn the_census_suffix_scan_shares_the_word_predicate() {
    // inc eax / inc ecx / inc edx / <barrier> / mov cx,ax / mov dx,ax / inc eax
    let code = [
        0x40,
        0x41,
        0x42,
        DIRECT_BARRIER,
        0x66,
        0x89,
        0xc1,
        0x66,
        0x89,
        0xc2,
        0x40,
    ];
    let offsets = [0u32, 1, 2, 3, 4, 7, 10];

    for (label, admitted, expected) in [("refused", false, 0), ("admitted", true, 3)] {
        let (mut cpu, mut bus) = fixture_in_mode(&code, GswMode::Gsw486);
        cpu.enable_direct_barrier_census(true);
        cpu.set_word_operands_at_486(admitted);
        let addresses: Vec<_> = offsets.iter().map(|offset| ENTRY + offset).collect();
        warm(&mut cpu, &mut bus, &addresses);
        let _ = jit::direct::compile(&mut cpu, ENTRY, true);
        let snapshot = cpu
            .direct_barrier_census_snapshot()
            .expect("enabled census snapshot");
        let row = snapshot
            .rows
            .iter()
            .find(|row| row.opcode == u16::from(DIRECT_BARRIER))
            .expect("recorded structural stop");
        assert_eq!(
            row.native_suffix_instructions, expected,
            "{label}: the scan must apply the same Word predicate the compile walk does"
        );
    }
}

/// The V86-sensitive opcodes stay compile barriers at `OperandSize::Word`, pinned per opcode.
///
/// V86 code is always CS.D = 0, so every instruction it executes decodes at Word. That makes the
/// Word-size path the one that matters for V86 safety, and for five of these six opcodes the ONLY
/// remaining gate is "no `classify` arm exists" -- a gate this campaign's whole method is to
/// widen. A list defended by nothing but the absence of code needs a test that names each member,
/// or the arm that admits one of them lands without anything failing.
///
/// PUSHF (0x9c) is the deliberate exception and the reason this is a table rather than a loop
/// over interchangeable bytes: it HAS a classify arm (PUSHFD, a runtime-weighted top-five reject
/// before it was lowered), and its V86 cover is `stack_width_kind`, which refuses
/// `StoreSource::Flags` whenever `cpu.is_v86_mode()` because PUSHF checks IOPL in V86 and can
/// raise #GP. Its Word arm is still refused by the allowlist, and the Dword control here proves
/// the arm is live so this test cannot rot into "refused at every size" vacuity the way the port
/// test once did.
///
/// CLI (0xfa) LEFT the barrier list with the S3 policy widening and is asserted from the other
/// side in the same table: it joins the block at both widths as an `InterpretOne` call-out. Its
/// V86 cover is not a compile-time refusal at all but the helper's fault arm -- `check_v86_iopl`
/// is the interpreter's own first statement in that opcode's arm, so a V86 task below IOPL 3
/// raises the same #GP from inside the call-out that it raises at a barrier, delivered by
/// `finish_instruction` with the block reporting the prefix only. STI stays refused beside it,
/// and the pair is the point: they are one instruction apart in the encoding and get opposite
/// answers, so a widening that swept STI in with CLI fails here.
#[test]
fn v86_sensitive_opcodes_keep_their_word_answers() {
    // (bytes, admitted_at_dword, call_out_at_every_width)
    let table: &[(&[u8], bool, bool)] = &[
        (&[0x9c], true, false),  // PUSHF: lowered at Dword, allowlist-refused at Word
        (&[0x9d], false, false), // POPF: no classify arm
        (&[0xfa], false, true),  // CLI: an InterpretOne call-out since the S3 widening
        (&[0xfb], false, true),  // STI: an InterpretOne call-out since S4d
        (&[0xcd, 0x20], false, false), // INT imm8: no classify arm
        (&[0xcf], false, false), // IRET: no classify arm
    ];
    for (op, admitted_at_dword, call_out_at_every_width) in table {
        for prefixed in [false, true] {
            let mut code = vec![0x40, 0x41, 0x42];
            let mut offsets = vec![0u32, 1, 2, 3];
            if prefixed {
                code.push(0x66);
            }
            code.extend_from_slice(op);
            let tail_at = code.len() as u32;
            code.extend_from_slice(&[0x43, 0x44, 0x45]);
            offsets.extend_from_slice(&[tail_at, tail_at + 1, tail_at + 2]);

            let (mut cpu, mut bus) = fixture(&code);
            let addresses: Vec<_> = offsets.iter().map(|offset| ENTRY + offset).collect();
            warm(&mut cpu, &mut bus, &addresses);
            let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
            let first = op[0];
            if *call_out_at_every_width {
                assert!(
                    compilation.span.instructions > 3,
                    "{first:#04x} (prefixed={prefixed}) must join the block as a call-out"
                );
                assert_eq!(
                    compilation.callout_interpret_one_slots, 1,
                    "{first:#04x} (prefixed={prefixed}) must join as a call-out, not a lowering"
                );
            } else if !prefixed && *admitted_at_dword {
                assert!(
                    compilation.span.instructions > 3,
                    "{first:#04x} at Dword has a classify arm and must not end the block;                      if this fails the Dword control is dead and the Word assertions below                      can no longer distinguish Word-refusal from always-refusal"
                );
            } else {
                assert_eq!(
                    compilation.span.instructions, 3,
                    "{first:#04x} (prefixed={prefixed}) must stop the block at three slots"
                );
            }
        }
    }
}

/// The census suffix scan honours the x87 loop-top cap the compile walk applies.
///
/// Both x87 gates in the compile walk are armed by the PREFIX (`x87_slots != 0`), so the scan's
/// blanket refusal of x87 kinds in the suffix never made them unreachable, and before the seed
/// carried `x87_slots` a barrier whose prefix held one `FLD` reported a suffix bounded only by the
/// 32-instruction ceiling instead of the 12-instruction x87 one. `max_native_suffix` is a ranking
/// column, and the over-report landed on exactly the x87-adjacent population the campaign ranks.
///
/// The program is `FLD dword [ebx]`, three integer slots, the barrier, then twenty more admissible
/// integer slots. Prefix is 4, so the x87 cap stops the scan at 12 - 4 - 1 = 7; without the cap it
/// reads all 20. Proven non-vacuous by exactly that flip: with the seed's `x87_slots` forced to
/// zero this assertion reads 20 and fails.
#[test]
fn the_census_suffix_scan_applies_the_x87_block_cap() {
    let mut code = vec![0xd9, 0x03, 0x40, 0x41, 0x42, DIRECT_BARRIER];
    let mut offsets = vec![0u32, 2, 3, 4, 5];
    for extra in 0..20u32 {
        code.push(0x40);
        offsets.push(6 + extra);
    }

    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    let addresses: Vec<_> = offsets.iter().map(|offset| ENTRY + offset).collect();
    warm(&mut cpu, &mut bus, &addresses);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "the prefix must be FLD plus three integer slots, or the seed under test is not armed"
    );
    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == u16::from(DIRECT_BARRIER))
        .expect("recorded structural stop");
    assert_eq!(
        row.native_suffix_instructions, 7,
        "an x87 slot in the prefix must bound the suffix by MAX_X87_BLOCK_INSTRUCTIONS"
    );
}

/// The port opcodes at `OperandSize::Word`: `0xED`/`0xEE`/`0xEF` are never admitted, `0xEC` now
/// is. Load-bearing rather than defensive, in both directions.
///
/// `key_for_phys`'s V86 safety argument used to rest on three gates, "any ONE sufficient". One of
/// them (`try_direct_continuation` refusing every 16-bit boundary) is already conditional on
/// `IZARRAVM_JIT16`, so the argument really rests on two, and the Word allowlist was one of the
/// two: an `IN` in a V86 16-bit segment stayed a barrier, because operand size follows CS.D
/// opcode-independently and V86 code is always CS.D = 0.
///
/// `0xEC` LEFT that argument on purpose, and its safety is now the helper's rather than the
/// list's: `port_read_al_dx` proves the TSS I/O-permission answer purely before it commits
/// anything, and refuses whatever it cannot prove. The Word admission and that helper arm are ONE
/// change and must revert together -- the admission alone is measured negative
/// (`classify.rs`, the 2026-08-11 note). This row pins the admission so a revert of one half
/// cannot pass silently.
///
/// The other three keep the original claim. That gate is a LIST under active change by this very
/// campaign, and a list defended by a git-ignored findings doc nobody reads is not defended.
#[test]
fn only_the_call_out_port_opcode_is_admitted_at_word() {
    for opcode in [0xecu8, 0xed, 0xee, 0xef] {
        // The un-prefixed CONTROL, and it is asserted, not just built. An earlier revision
        // constructed, configured and warmed this pair and then dropped it, so the test could not
        // distinguish "refused at Word" from "refused at every size" -- it compiled cleanly and
        // read like a positive control while asserting nothing.
        //
        // The control's meaning differs by opcode, and pretending otherwise is how the vacuity
        // crept in. `0xEC` has a call-out helper (`CallOutHelper::PortReadAlDx`), so at Dword it
        // is ADMITTED mid-block and the whole seven-slot program compiles as one span; its Word
        // arm below therefore isolates the Word gate exactly. `0xED`/`0xEE`/`0xEF` have no
        // `classify` arm at any size, so their un-prefixed arm stops at the same three slots and
        // proves the stronger fact that carries their V86 safety: refused everywhere, with the
        // Word gate as redundant cover rather than the only gate.
        let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, opcode, 0x43, 0x44, 0x45]);
        let addresses: Vec<_> = (0..7u32).map(|offset| ENTRY + offset).collect();
        warm(&mut cpu, &mut bus, &addresses);
        let dword = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        if opcode == 0xec {
            assert_eq!(
                dword.span.instructions, 7,
                "IN AL,DX at Dword is a call-out slot and must not end the block"
            );
        } else {
            assert_eq!(
                dword.span.instructions, 3,
                "{opcode:#04x} has no classify arm at any operand size"
            );
        }
        // The 0x66 prefix in a 32-bit segment is what makes the decoded operand size Word here;
        // in real 16-bit code CS.D does it, and `classify` cannot tell the two apart. The
        // `set_word_operands_at_486` calls the earlier revision made were inert -- `fixture()`
        // builds a 586, where Word operands are admitted unconditionally -- and are gone.
        let (mut word_cpu, mut word_bus) =
            fixture(&[0x40, 0x41, 0x42, 0x66, opcode, 0x43, 0x44, 0x45]);
        let word_addresses: Vec<_> = [0u32, 1, 2, 3, 5, 6, 7]
            .iter()
            .map(|offset| ENTRY + offset)
            .collect();
        warm(&mut word_cpu, &mut word_bus, &word_addresses);
        let compilation = compiled(jit::direct::compile(&mut word_cpu, ENTRY, true));
        if opcode == 0xec {
            assert_eq!(
                compilation.span.instructions, 7,
                "IN AL,DX at Word is the V86 port call-out slice and must join the block"
            );
            assert_eq!(
                compilation.callout_slots, 1,
                "the Word form must produce a call-out slot, not a silent lowering"
            );
        } else {
            assert_eq!(
                compilation.span.instructions, 3,
                "{opcode:#04x} at Word must stop the block at three slots, not be lowered"
            );
        }
    }
}

/// A block that overwrites a segment register is counted at the dispatcher entry.
///
/// `segment_writes != 0` makes `compile` publish `successors = [None, None]`, which makes
/// `chain_eligible` false and clamps the quota to 1: the block can never chain, so every entry
/// runs it alone and returns through the full prologue and epilogue. That is the cost the
/// dirty-stop census cannot see, because it applies to EVERY block containing a segment write and
/// not only to the ones the dirty rule stopped.
///
/// Driven through `try_run_direct_block_for_test` and NOT `invoke_native_entry`: the latter jumps
/// straight to the block's entry pointer and never reaches `run_direct_block`'s exit accounting,
/// so it would leave the counter at zero while the block ran perfectly.
///
/// The control is what makes this non-vacuous. It differs by ONE byte pair, `mov ds,ax` against a
/// third increment, so nothing but the segment write can explain the difference.
#[test]
fn a_segment_write_block_is_counted_at_the_dispatcher_entry() {
    // inc eax / inc ecx / mov ds,ax / <barrier>, against inc eax / inc ecx / inc edx / <barrier>.
    let writes = [0x40, 0x41, 0x8e, 0xd8, DIRECT_BARRIER, 0x43, 0x44];
    let control = [0x40, 0x41, 0x42, DIRECT_BARRIER, 0x43, 0x44, 0x45];
    // The OTHER arm that publishes `[None, None]`: a terminal whose successor is dynamic. It is
    // what makes the `!dynamic_successor` conjunct load-bearing, because a predicate written as
    // `successors == [None, None]` alone would count this block and be wrong.
    let ret = [0x40, 0x41, 0x42, 0xc3, 0x43, 0x44, 0x45];

    for (label, code, offsets, expected) in [
        (
            "segment write",
            writes.as_slice(),
            [0, 1, 2, 4, 5, 6].as_slice(),
            1,
        ),
        (
            "fallthrough control",
            control.as_slice(),
            [0, 1, 2, 3, 4, 5].as_slice(),
            0,
        ),
        (
            "dynamic terminal control",
            ret.as_slice(),
            [0, 1, 2, 3, 4, 5].as_slice(),
            0,
        ),
    ] {
        let (mut cpu, mut bus) = fixture(code);
        cpu.registers.set_esp(0x1000);
        let addresses: Vec<_> = offsets.iter().map(|offset| ENTRY + offset).collect();
        warm(&mut cpu, &mut bus, &addresses);
        // `install` refuses a key that is not already `Seen`, and `probe` is what registers it.
        let key = jit::direct::key_for(&cpu, ENTRY, true).expect("entry key");
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        let id = cpu
            .jit_direct
            .install(&compilation)
            .expect("fixture block installs");
        let block = cpu.jit_direct.block(id).expect("live block");
        assert_eq!(
            block.is_segment_write_block(),
            expected == 1,
            "{label}: the derived predicate disagrees with the fixture"
        );

        cpu.registers.eip = ENTRY;
        let ran = cpu
            .try_run_direct_block_for_test(&mut bus, block)
            .expect("fixture block runs");
        assert!(ran, "{label}: the block must actually be entered");

        let stalls = cpu.direct_stall_snapshot();
        assert_eq!(
            stalls.segment_write_block_head_entries, expected,
            "{label}: segment-write head entries"
        );
        if expected == 1 {
            assert!(
                stalls.segment_write_block_head_insns >= 3,
                "{label}: the instruction lane must carry the block's retired count, got {}",
                stalls.segment_write_block_head_insns
            );
        } else {
            assert_eq!(
                stalls.segment_write_block_head_insns, 0,
                "{label}: control must not deposit into the instruction lane"
            );
        }
    }
}

/// A dirty-segment row must NOT claim its entry linear in the rejected-span map.
///
/// `rejected_barrier` is keyed on entry linear alone and answers "which barrier refused the block
/// living here", so that a runtime exit into a rejected block can be charged back to an opcode.
/// A dirty stop is a `CompileStop::Boundary`: the key it leaves behind is Compiled, or Dormant
/// when the break landed with too few slots to install, but never Rejected. Letting it write that
/// map would hand a genuinely rejected block's exits to whichever of the two happened to be
/// recorded second.
///
/// Driven through `note_unbound_target`, which is the hook that reads the map, rather than by
/// inspecting the map directly. That is the path the mis-attribution would actually take.
#[test]
fn a_dirty_segment_row_does_not_claim_the_rejected_span_map() {
    let code = [
        0x40, 0x41, 0x8e, 0xd8, 0x8b, 0x03, 0x8b, 0x0b, 0x8b, 0x13, 0x40,
    ];
    let offsets = [0, 1, 2, 4, 6, 8, 10];
    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    let addresses: Vec<_> = offsets.iter().map(|offset| ENTRY + offset).collect();
    warm(&mut cpu, &mut bus, &addresses);
    let _ = jit::direct::compile(&mut cpu, ENTRY, true);

    // An exit reporting a REJECTED target at the dirty block's own entry linear. If the dirty stop
    // had registered there, this credits its row.
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::Rejected, ENTRY);

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.stop_reason == "dirty_segment")
        .expect("recorded dirty-segment stop");
    assert_eq!(
        row.unbound_exits, 0,
        "the dirty row took credit for a rejected-target exit it has no claim on"
    );
}

/// The seed carries the BARRED instruction's own segment write.
///
/// `POP DS` is not lowered, so it is an ordinary barrier row, and the compile walk never reached
/// its write. But the suffix prices "what if this barrier were lowered", and lowering a `POP DS`
/// makes DS dirty for everything behind it. Without the seed the row over-reports, and `0x1f` is
/// 10.3% of the 16-bit census.
///
/// The control changes ONLY the barrier byte, so the pair isolates `barred_segment_write` from
/// everything else the scan does.
#[test]
fn the_suffix_seed_carries_a_barred_segment_write() {
    // inc eax / inc ecx / inc edx / <barrier> / mov eax,[ebx] / inc eax
    let pop_ds = [0x40, 0x41, 0x42, 0x1f, 0x8b, 0x03, 0x40];
    let control = [0x40, 0x41, 0x42, DIRECT_BARRIER, 0x8b, 0x03, 0x40];
    let offsets = [0, 1, 2, 3, 4, 6];

    assert_eq!(
        barrier_suffix(&control, &offsets),
        2,
        "control: a barrier that writes no segment leaves the scan free to walk the DS reader"
    );
    assert_eq!(
        census_row_for(&pop_ds, &offsets, 0x1f).native_suffix_instructions,
        0,
        "POP DS dirties DS for the scan behind it, so the very next slot stops it"
    );
}

/// The prefix arm of the compile walk is ATTRIBUTED, not silent — and, since slice 6, the arm is
/// reached by a CS override rather than by any segment override.
///
/// Before the completeness slice, `record_barrier` fired on the `HardBoundary` arm alone, so a
/// block stopped by an unsupported prefix installed a rejected span with no census row.
///
/// TWO controls, and together they pin the whole admitted/refused split at ONE code site:
///  * the SAME opcode at the SAME position with no prefix is lowered, so the row is the prefix's
///    doing and not the load's;
///  * the SAME opcode at the SAME position behind an **SS** override is ALSO lowered, so the row
///    is specifically the CS override's doing and not "a segment override's". That is the slice-6
///    decision expressed as a fixture: had the admission been written to take every segment, this
///    test would fail on the CS half; had it been written to take none, it would fail on the SS
///    half.
#[test]
fn barrier_census_attributes_the_prefix_refusal_arm() {
    // The CS half is an OFF-ARM statement as of 2026-08-20: `IZARRAVM_V86_LOOP_ROWS` admits the CS
    // override, so the arm is stated rather than inherited and the ON arm is asserted at the end
    // of this test rather than left to chance. The slice-6 decision this fixture was written for
    // is unchanged on the shipped default, which is what the OFF arm here pins.
    jit::direct::set_v86_loop_rows_for_test(Some(false));
    // `mov eax, [eax]`, behind a CS override, behind an SS override, and bare.
    let prefixed = [0x40, 0x41, 0x42, 0x43, 0x2e, 0x8b, 0x00, 0x44, 0x45];
    let ss_override = [0x40, 0x41, 0x42, 0x43, 0x36, 0x8b, 0x00, 0x44, 0x45];
    let bare = [0x40, 0x41, 0x42, 0x43, 0x8b, 0x00, 0x44, 0x45, 0x46];
    let addresses: Vec<_> = (0..9u32).map(|offset| ENTRY + offset).collect();

    for (label, code) in [("bare", bare.as_slice()), ("ss override", &ss_override)] {
        let (mut control_cpu, mut control_bus) = fixture(code);
        control_cpu.enable_direct_barrier_census(true);
        warm(&mut control_cpu, &mut control_bus, &addresses);
        let control = compiled(jit::direct::compile(&mut control_cpu, ENTRY, true));
        assert!(
            control.span.instructions > 5,
            "control {label}: the load must be lowered so the walk runs past it, got {} slots",
            control.span.instructions
        );
        assert!(
            control_cpu
                .direct_barrier_census_snapshot()
                .expect("enabled census snapshot")
                .rows
                .iter()
                .all(|row| row.opcode != 0x8b),
            "control {label}: the load must not be a barrier at all"
        );
    }

    let (mut cpu, mut bus) = fixture(&prefixed);
    cpu.enable_direct_barrier_census(true);
    warm(&mut cpu, &mut bus, &addresses);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "the CS-prefixed load must stop the walk at the four INCs"
    );

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == 0x8b)
        .expect("the prefix refusal must produce a census row");
    assert_eq!(row.stop_reason, "prefix_unsupported");
    // `(segment_index(Cs) + 1) << 5` = 64. Asserted to the value rather than as "non-zero": the
    // census reader decodes this mask to name the segment, and doom's surviving CS-override row is
    // read off it.
    assert_eq!(
        row.prefix_mask, 64,
        "the row must name CS as the prefix that refused it"
    );
    assert_eq!(row.hits, 1);
    assert_eq!(row.native_prefix_instructions, 4);
    assert_eq!(row.unbound_exits, 0, "no exit has happened yet");
    assert_eq!(row.dynamic_unbound_exits, 0);

    // The ON arm: the same CS-prefixed load is LOWERED, so it produces no census row at all and the
    // walk runs past it exactly as the SS control above does. Without this half the test would keep
    // passing if the gate stopped moving the CS clause, which is the thing it now describes.
    jit::direct::set_v86_loop_rows_for_test(Some(true));
    let (mut on_cpu, mut on_bus) = fixture(&prefixed);
    on_cpu.enable_direct_barrier_census(true);
    warm(&mut on_cpu, &mut on_bus, &addresses);
    let on = compiled(jit::direct::compile(&mut on_cpu, ENTRY, true));
    assert!(
        on.span.instructions > 5,
        "with IZARRAVM_V86_LOOP_ROWS on, the CS-prefixed load must be lowered and the walk must \
         run past it, got {} slots",
        on.span.instructions
    );
    assert!(
        on_cpu
            .direct_barrier_census_snapshot()
            .expect("enabled census snapshot")
            .rows
            .iter()
            .all(|row| row.opcode != 0x8b),
        "with the gate on the load must not be a barrier at all"
    );
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// The non-continuable arm, same shape of claim. HLT is the durable choice: `block_continuable`
/// names it explicitly as staying a terminator, and the assertion below re-derives that from the
/// decoded instruction rather than trusting the comment, so the day HLT becomes continuable this
/// test says so instead of silently measuring a different arm.
#[test]
fn barrier_census_attributes_the_non_continuable_arm() {
    let code = [0x40, 0x41, 0x42, 0x43, 0xf4, 0x44, 0x45, 0x46, 0x47];
    let addresses: Vec<_> = (0..code.len() as u32)
        .map(|offset| ENTRY + offset)
        .collect();

    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    warm(&mut cpu, &mut bus, &addresses);

    cpu.registers.eip = ENTRY + 4;
    cpu.begin_instruction();
    let halt = cpu
        .fetch_decoded(&mut bus, ENTRY + 4)
        .expect("re-decode the halt");
    assert!(
        !halt.continuable,
        "this fixture measures the non-continuable arm, so HLT must still be non-continuable"
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 4);

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == 0xf4)
        .expect("the non-continuable refusal must produce a census row");
    assert_eq!(row.stop_reason, "non_continuable");
    assert_eq!(row.prefix_mask, 0);
    assert_eq!(row.hits, 1);
    assert_eq!(row.native_prefix_instructions, 4);
}

/// The Word-persona arm, which reads ZERO on both shipped fixtures because they run 586.
///
/// It would have been cheap to disclose this arm as instrumented-but-untested. The campaign's own
/// ledger says the opposite: "no fixture can see this" is a claim requiring evidence, and it is
/// cheaper to add the fixture than to justify its absence. It is reachable — a 66-prefixed Word
/// instruction in a 32-bit code segment passes the prefix arm above it (a 66 override IS what
/// `prefixes_supported_for` expects for Word under `d == true`) and lands here on I486.
#[test]
fn barrier_census_attributes_the_word_persona_arm() {
    // `mov cx, ax` behind the operand-size override, after four INCs.
    let code = [0x40, 0x41, 0x42, 0x43, 0x66, 0x89, 0xc1, 0x44, 0x45];
    let addresses: Vec<_> = [0, 1, 2, 3, 4, 7, 8].map(|offset| ENTRY + offset).to_vec();

    let (mut cpu, mut bus) = fixture_in_mode(&code, GswMode::Gsw486);
    cpu.enable_direct_barrier_census(true);
    // EXPLICIT since the default flipped. The arm still exists and is still reachable; what
    // changed is that reaching it is now a policy choice rather than a property of the persona,
    // and a fixture that leaned on the default would silently stop testing the arm.
    cpu.set_word_operands_at_486(false);
    warm(&mut cpu, &mut bus, &addresses);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "the Word instruction must stop the walk while the policy refuses it"
    );
    let row = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot")
        .rows
        .iter()
        .find(|row| row.opcode == 0x89)
        .cloned()
        .expect("the Word-persona refusal must produce a census row");
    assert_eq!(row.stop_reason, "word_persona");
    assert_eq!(row.operand_size, "word");
    assert_eq!(row.prefix_mask, 1, "the operand-size override, and only it");

    // The control that makes the persona the cause: the same bytes on 586 are lowered, so the
    // walk runs past them and no row exists at all.
    let (mut cpu586, mut bus586) = fixture(&code);
    cpu586.enable_direct_barrier_census(true);
    warm(&mut cpu586, &mut bus586, &addresses);
    let wide = compiled(jit::direct::compile(&mut cpu586, ENTRY, true));
    assert!(
        wide.span.instructions > 4,
        "control: 586 must admit the Word form, got {} slots",
        wide.span.instructions
    );
    assert!(
        cpu586
            .direct_barrier_census_snapshot()
            .expect("enabled census snapshot")
            .rows
            .iter()
            .all(|row| row.stop_reason != "word_persona"),
        "control: the Word-persona arm must not fire on 586"
    );
}

/// The `HardBoundary` arm keeps its own label, so the three arms are distinguishable in the
/// report rather than merged into one undifferentiated row set.
#[test]
fn barrier_census_labels_the_opcode_coverage_arm_apart_from_the_others() {
    let code = [
        0x40,
        0x41,
        0x42,
        0x43,
        DIRECT_BARRIER,
        0x44,
        0x45,
        0x46,
        0x47,
    ];
    let addresses: Vec<_> = (0..code.len() as u32)
        .map(|offset| ENTRY + offset)
        .collect();
    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    warm(&mut cpu, &mut bus, &addresses);
    let _ = compiled(jit::direct::compile(&mut cpu, ENTRY, true));

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot.rows.first().expect("recorded structural stop");
    assert_eq!(row.opcode, u16::from(DIRECT_BARRIER));
    assert_eq!(row.stop_reason, "hard_boundary");
}

#[test]
fn supported_terminals_compile_even_when_the_block_is_short() {
    let (mut single_cpu, mut single_bus) = fixture(&[0xc3]);
    warm(&mut single_cpu, &mut single_bus, &[ENTRY]);
    let single = compiled(jit::direct::compile(&mut single_cpu, ENTRY, true));
    assert_eq!(single.span.instructions, 1);
    assert_eq!(single.span.guest_len, 1);

    let (mut pair_cpu, mut pair_bus) = fixture(&[0x40, 0xc3]);
    warm(&mut pair_cpu, &mut pair_bus, &[ENTRY, ENTRY + 1]);
    let pair = compiled(jit::direct::compile(&mut pair_cpu, ENTRY, true));
    assert_eq!(pair.span.instructions, 2);
    assert_eq!(pair.span.guest_len, 2);
}

#[test]
fn missing_third_decode_retries_then_compiles_without_a_guest_write() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, true),
        jit::direct::CompileOutcome::Retry(_)
    ));

    warm(&mut cpu, &mut bus, &[ENTRY + 2]);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}

#[test]
fn retry_state_stays_dormant_after_decode_recovery_until_cache_clear() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("missing decode retry");
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));

    warm(&mut cpu, &mut bus, &[ENTRY + 2]);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    for _ in 0..1_000 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("dormant probe");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installed);

    cpu.jit_direct.clear();
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation after clear");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("compile after clear");
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 2
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed + 1
    );
}

/// Every gate ahead of the JIT answers `Skipped`, never `Declined`: the decline seam counter
/// (`jit_direct_dispatch_declines`) counts boundaries the JIT was actually ASKED about. Warming
/// only `ENTRY` leaves the third instruction undecoded, so the compile walk cannot finish and the
/// consulted case lands on `Declined` without installing anything.
#[test]
fn dispatch_reports_skipped_for_every_gate_ahead_of_the_jit() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    cpu.set_jit_auto_admit(true);

    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, false)
            .expect("inactive continuations"),
        ContinuationDispatch::Skipped
    );

    cpu.set_native_backend_enabled(false);
    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("backend disabled"),
        ContinuationDispatch::Skipped
    );

    let (mut cpu, mut bus) = fixture_in_mode(&[0x40, 0x41, 0x42], GswMode::Gsw386Slow);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    cpu.set_jit_auto_admit(true);
    assert!(!cpu.mode().uses_approximate_timing());
    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("exact-timing persona"),
        ContinuationDispatch::Skipped
    );

    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    cpu.set_jit_auto_admit(true);
    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("consulted"),
        ContinuationDispatch::Declined
    );
}

/// The latch is consumed by a short-circuited `||`, so a pending skip is NOT spent while the
/// backend is disabled; it survives to the first boundary the JIT would really have taken.
#[test]
fn a_pending_direct_skip_survives_a_disabled_backend() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    cpu.set_jit_auto_admit(true);

    cpu.set_native_backend_enabled(false);
    cpu.set_skip_direct_once_for_test(true);
    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("backend disabled"),
        ContinuationDispatch::Skipped
    );
    assert!(cpu.skip_direct_once_for_test(), "the latch must survive");

    cpu.set_native_backend_enabled(true);
    assert_eq!(
        cpu.dispatch_continuation_for_test(&mut bus, ENTRY, true, true)
            .expect("latch spent"),
        ContinuationDispatch::Skipped
    );
    assert!(!cpu.skip_direct_once_for_test(), "the latch must be spent");
}

#[test]
fn direct_admission_waits_for_the_configured_heat() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(8);
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;

    for _ in 0..8 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("heat observation");
    }
    assert_eq!(cpu.perf_counters().jit_direct_compile_attempts, attempts);

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("compile after heat threshold");
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
}

#[test]
fn disabled_native_continuations_leave_direct_counters_cold() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    cpu.set_fast_map_enabled_for_test(false);
    cpu.set_jit_auto_admit(false);
    cpu.registers.eip = ENTRY;

    for _ in 0..4 {
        let outcome = cpu.run_budgeted(&mut bus, u64::MAX).expect("JIT-off run");
        if outcome.halted {
            break;
        }
    }

    assert!(cpu.halted);
    let perf = cpu.perf_counters();
    assert_eq!(perf.jit_direct_entries, 0);
    assert_eq!(perf.jit_direct_compile_attempts, 0);
    assert_eq!(perf.jit_direct_hot_hits, 0);
    assert_eq!(perf.jit_direct_hash_hits, 0);
    assert_eq!(perf.jit_direct_lookup_misses, 0);
}

#[test]
fn page_straddling_barrier_retries_instead_of_becoming_persistent() {
    let mut memory = vec![0; 0x2000];
    memory[0xfff..0x1001].copy_from_slice(&[0xcd, 0x80]);
    let mut bus = TestBus::with_memory(memory);
    let mut cpu = fresh();
    cpu.registers.eip = 0xfff;
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, 0xfff)
        .expect("straddling INT decode");

    assert!(matches!(
        jit::direct::compile(&mut cpu, 0xfff, true),
        jit::direct::CompileOutcome::Retry(_)
    ));
    assert!(cpu.decode_cache.get(0xfff, true).is_none());
}

#[test]
fn live_cs_limit_failure_retries_and_recovers_without_a_write() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let mut cs = cpu.registers.cs();
    let old_limit = cs.limit;
    cs.limit = ENTRY;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);

    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, true),
        jit::direct::CompileOutcome::Retry(_)
    ));

    cs.limit = old_limit;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
}

#[test]
fn live_segment_state_failure_retries_and_recovers_without_a_write() {
    let (mut cpu, mut bus) = fixture(&[0x8b, 0x00, 0xc3]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2]);
    cpu.control.cr0 |= CR0_PE;
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    let old_access = ds.access;
    ds.access = 0;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);

    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, true),
        jit::direct::CompileOutcome::Retry(_)
    ));

    ds.access = old_access;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 2);
}

#[test]
fn emitted_size_exhaustion_is_retryable_for_short_and_long_blocks() {
    let (mut terminal_cpu, mut terminal_bus) = fixture(&[0xc3]);
    warm(&mut terminal_cpu, &mut terminal_bus, &[ENTRY]);
    assert!(matches!(
        jit::direct::compile_with_page_len_for_test(&mut terminal_cpu, ENTRY, true, 1),
        jit::direct::CompileOutcome::Retry(_)
    ));

    let (mut prefix_cpu, mut prefix_bus) = fixture(&[0x40, 0x41, 0x42, 0x43]);
    warm(
        &mut prefix_cpu,
        &mut prefix_bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    assert!(matches!(
        jit::direct::compile_with_page_len_for_test(&mut prefix_cpu, ENTRY, true, 1),
        jit::direct::CompileOutcome::Retry(_)
    ));
}

fn reject(cache: &mut jit::JitState, key: jit::direct::BlockKey, len: usize) {
    assert!(matches!(
        cache.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(cache.probe(key), jit::direct::BlockProbe::Compile));
    cache.reject(jit::direct::RejectedSpan::new(key, len).expect("rejected fixture span"));
    assert!(matches!(
        cache.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

#[test]
fn rejected_span_invalidates_on_every_owned_byte_but_not_adjacent_bytes() {
    for offset in 0..4 {
        let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
        let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
        reject(&mut cache, key, 4);
        assert_eq!(cache.retire_physical_range_for_test(0x320 + offset, 1), 1);
        assert!(!cache.range_hits_compiled_code(0x320, 4));
    }

    let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
    let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
    reject(&mut cache, key, 4);
    assert_eq!(cache.retire_physical_range_for_test(0x31f, 1), 0);
    assert_eq!(cache.retire_physical_range_for_test(0x324, 1), 0);
    assert!(matches!(
        cache.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

#[test]
fn repeated_reject_does_not_acquire_a_second_watch_owner() {
    let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
    let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
    reject(&mut cache, key, 4);
    cache.reject(jit::direct::RejectedSpan::new(key, 4).expect("rejected fixture span"));

    assert_eq!(cache.retire_physical_range_for_test(0x322, 1), 1);
    assert!(!cache.range_hits_compiled_code(0x320, 4));
}

fn mixed_compiled_and_rejected_cache() -> (CpuGsw, jit::direct::BlockKey, jit::direct::BlockKey) {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    let compiled_key = compilation.span.key;
    assert!(matches!(
        cpu.jit_direct.probe(compiled_key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(compiled_key),
        jit::direct::BlockProbe::Compile
    ));
    cpu.jit_direct
        .install(&compilation)
        .expect("compiled fixture install");

    let rejected_key = jit::direct::BlockKey::new(ENTRY + 8, ENTRY + 8, cpu.jit_mode_key());
    reject(&mut cpu.jit_direct, rejected_key, 1);
    (cpu, compiled_key, rejected_key)
}

#[test]
fn compiled_and_rejected_owners_share_a_chunk_until_both_retire() {
    for reject_first in [false, true] {
        let (mut cpu, compiled_key, rejected_key) = mixed_compiled_and_rejected_cache();
        assert!(
            cpu.jit_direct
                .range_hits_compiled_code(compiled_key.physical, 16)
        );

        let first = if reject_first {
            rejected_key.physical
        } else {
            compiled_key.physical
        };
        let second = if reject_first {
            compiled_key.physical
        } else {
            rejected_key.physical
        };
        assert_eq!(cpu.jit_direct.retire_physical_range_for_test(first, 1), 1);
        assert!(
            cpu.jit_direct
                .range_hits_compiled_code(compiled_key.physical, 16)
        );
        assert_eq!(cpu.jit_direct.retire_physical_range_for_test(second, 1), 1);
        assert!(
            !cpu.jit_direct
                .range_hits_compiled_code(compiled_key.physical, 16)
        );
    }
}

#[test]
fn cache_clear_unpublishes_rejected_watch_and_keeps_the_table_base() {
    let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
    let key = jit::direct::BlockKey::new(0x200, 0x12_320, 7);
    let base = cache.native_code_watch_table();
    let entry = unsafe { (base as *const usize).add((key.physical >> 12) as usize) };
    reject(&mut cache, key, 4);
    assert_ne!(unsafe { *entry }, 0);

    cache.clear();
    assert_eq!(unsafe { *entry }, 0);
    assert_eq!(cache.native_code_watch_table(), base);
    assert!(!cache.range_hits_compiled_code(key.physical, 4));

    reject(&mut cache, key, 4);
    assert_ne!(unsafe { *entry }, 0);
    assert_eq!(cache.retire_physical_range_for_test(key.physical, 1), 1);
    assert_eq!(unsafe { *entry }, 0);
}

#[test]
fn failed_install_consumes_seen_without_a_code_watch() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let mut compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    let key = compilation.span.key;
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Compile
    ));
    compilation.code.clear();

    assert!(cpu.jit_direct.install(&compilation).is_none());
    cpu.jit_direct.dormant(
        key,
        jit::direct::DormantReason::CompileRetry,
        Some(jit::direct::RetryCause::TooShort),
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
    assert!(
        !cpu.jit_direct
            .range_hits_compiled_code(key.physical, u32::from(compilation.span.guest_len))
    );
}

#[test]
fn direct_page_coverage_failure_consumes_seen_without_a_watch() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.set_jit_auto_admit(true);
    bus.direct_pages_enabled = false;
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("coverage failure");
    for _ in 0..100_000 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("dormant probe");
    }

    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
    assert!(!cpu.jit_direct.range_hits_compiled_code(key.physical, 3));
    cpu.jit_direct.clear();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
}

fn rejected_after_decode_eviction() -> (CpuGsw, TestBus, jit::direct::BlockKey) {
    let (mut cpu, mut bus) = fixture(&[DIRECT_BARRIER]);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    let span = structural(jit::direct::compile(&mut cpu, ENTRY, true));
    let key = span.key();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Compile
    ));
    cpu.jit_direct.reject(span);

    let insn = cpu
        .decode_cache
        .get(ENTRY, true)
        .expect("rejected instruction decode");
    let collision = ENTRY + cpu.decode_cache.lines.len() as u32;
    assert!(cpu.decode_cache.put(collision, insn, true, 0x800).inserted);
    assert!(cpu.decode_cache.get(ENTRY, true).is_none());
    assert!(cpu.jit_direct.range_hits_compiled_code(ENTRY, 1));
    (cpu, bus, key)
}

#[test]
fn interpreter_write_recovers_rejection_after_decode_eviction() {
    let (mut cpu, mut bus, key) = rejected_after_decode_eviction();
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ENTRY,
        0x91,
        BusAccessKind::DataWrite,
    )
    .expect("interpreter write");

    assert_eq!(bus.memory[ENTRY as usize], 0x91);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
}

#[test]
fn device_write_recovers_rejection_after_decode_eviction() {
    let (mut cpu, _bus, key) = rejected_after_decode_eviction();
    cpu.note_device_memory_write_range(ENTRY, 1);

    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
}

// ---- G1 SMC heat demotion gate ----

#[test]
fn smc_heat_pre_compile_gate_demotes_a_hot_entry_chunk() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    // Pin the trial-off arm: the lane trial defaults ON since the disp lanes landed, and with
    // it on this key would legitimately spend one compile through the hot gate.
    cpu.jit_direct.direct.set_lane_trial_for_test(false);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    // Heat the entry 16-byte chunk past the churn threshold within epoch 0 (sync first so a
    // reset pending from setup is observed before the seed, not after).
    cpu.sync_smc_heat();
    for _ in 0..jit::direct::SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(ENTRY, 1, 0);
    }
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    let demotions = cpu.perf_counters().smc_heat_demotions;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    // The cheap entry-chunk gate refuses admission before a compile is even attempted. This
    // is the TRIAL-OFF arm (pinned above — the trial defaults ON since the disp lanes made
    // its installs survive); the trial's own semantics are pinned by
    // `lane_trial_compiles_once_and_installs_through_a_hot_span` (cpu_jit_imm_lane_test.rs,
    // whose fixtures are the proven lane-compiling environment).
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts,
        "no compile should be paid"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed,
        "the block must not install"
    );
    assert!(cpu.perf_counters().smc_heat_demotions > demotions);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

#[test]
fn smc_heat_pre_install_gate_demotes_a_hot_span_after_compiling() {
    let code: Vec<u8> = std::iter::repeat_n(0x40u8, 20).chain([0xf4]).collect();
    let addresses: Vec<u32> = (ENTRY..ENTRY + code.len() as u32).collect();
    // Learn the compiled span so the assertion below is self-checking.
    let (mut probe_cpu, mut probe_bus) = fixture(&code);
    warm(&mut probe_cpu, &mut probe_bus, &addresses);
    let span_len = compiled(jit::direct::compile(&mut probe_cpu, ENTRY, true))
        .span
        .guest_len;
    assert!(
        span_len > 16,
        "block must cross into the second 16-byte chunk (len={span_len})"
    );

    let (mut cpu, mut bus) = fixture(&code);
    warm(&mut cpu, &mut bus, &addresses);
    cpu.set_jit_auto_admit(true);
    // Trial-off arm, pinned for the same reason as the pre-compile-gate test above (the
    // lane-free block here could not install through a trial anyway, but the arm should be
    // explicit, not incidental).
    cpu.jit_direct.direct.set_lane_trial_for_test(false);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    // Heat a chunk inside the span but NOT the entry chunk: the cheap pre-compile gate passes and
    // the full-span gate after compilation must catch it before install.
    let far = ENTRY + 16;
    cpu.sync_smc_heat();
    for _ in 0..jit::direct::SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(far, 1, 0);
    }
    assert!(
        !cpu.jit_direct.smc_heat.chunk_hot(ENTRY, 0),
        "entry chunk stays cold"
    );
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    let demotions = cpu.perf_counters().smc_heat_demotions;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert!(
        cpu.perf_counters().jit_direct_compile_attempts > attempts,
        "the block compiles (this is the post-compile gate)"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed,
        "the block must not install"
    );
    assert!(cpu.perf_counters().smc_heat_demotions > demotions);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

// ---- G4 non-RAM code admission ----

#[test]
fn g4_admission_refuses_a_page_without_instruction_prefetch_cover() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    // Model a non-RAM code page: the bus serves Data kinds (decode still works) but yields no
    // direct page under InstructionPrefetch. The install-time cover check MUST use
    // InstructionPrefetch, so admission is refused and the block parks Dormant with no install.
    // Switching that check to a Data kind would let this install and fail the assertion below.
    bus.deny_instruction_prefetch_direct_page = true;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed,
        "no install without InstructionPrefetch cover"
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));

    // A cover-failure Dormant carries no heat stamp, so G1's epoch-aging recovery never lifts it:
    // still parked after a full epoch advance.
    cpu.perf.instructions = 1u64 << jit::direct::SMC_HEAT_EPOCH_SHIFT;
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("gate");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installed);

    // Positive control: restoring the cover installs the identical block, proving the missing
    // InstructionPrefetch page was the only thing that refused admission.
    bus.deny_instruction_prefetch_direct_page = false;
    cpu.jit_direct.clear();
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed + 1,
        "installs once RAM covers the page under InstructionPrefetch"
    );
}

#[test]
fn smc_heat_demoted_key_recovers_when_its_chunk_cools() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.sync_smc_heat();
    for _ in 0..jit::direct::SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(ENTRY, 1, 0);
    }
    // Demote through the gate: parked Dormant, no install, and probing within the same epoch does
    // NOT lift it (the stamp still reads current).
    let demotions = cpu.perf_counters().smc_heat_demotions;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert!(cpu.perf_counters().smc_heat_demotions > demotions);
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installed);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));

    // Advance one heat epoch: the next probe path lifts the Dormant (stale stamp), and the normal
    // admission path re-compiles and installs the block.
    cpu.perf.instructions = 1u64 << jit::direct::SMC_HEAT_EPOCH_SHIFT;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed + 1,
        "a cooled chunk re-admits and compiles"
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Ready(_)
    ));
}

// G2+G1 composition: a same-value store churn loop into watched code accrues NO heat (elision
// keeps it out of the invalidation choke entirely), so it can never demote anything.
#[test]
fn same_value_store_churn_accrues_no_heat() {
    const TARGET: u32 = 0x1800;
    const VALUE: u32 = 0xdead_beef;
    let (mut cpu, mut bus) = fixture(&[0x40, 0xf4]);
    bus.memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&VALUE.to_le_bytes());
    cpu.mark_decode_code_for_test(TARGET, 4);
    // Baseline after fixture setup (set_mode counts one translation-cache invalidation).
    let invalidations = cpu.perf_counters().code_invalidations;
    for _ in 0..16 {
        cpu.write_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            TARGET,
            OperandSize::Dword,
            VALUE,
            BusAccessKind::DataWrite,
        )
        .expect("watched same-value store");
    }
    assert_eq!(cpu.perf_counters().smc_heat_chunks_hot, 0);
    assert_eq!(cpu.perf_counters().smc_heat_demotions, 0);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations);
}

/// A fixture at an arbitrary entry with a FLAT code-segment limit.
///
/// `fresh()` loads CS in real mode, so its limit is 0xFFFF, and the per-slot fetch-limit check
/// in the compile loop refuses any slot whose last byte is above `cs.limit` BEFORE `classify`
/// runs. A high-entry case built on the ordinary `fixture()` would therefore be refused for a
/// reason that has nothing to do with the branch, and every assertion about the branch would
/// pass vacuously. That is the shape a design review caught before these were written.
fn flat_fixture(entry: u32, code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x1_1000];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    for segment in [SegmentIndex::Cs, SegmentIndex::Ss] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.limit = u32::MAX;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.registers.eip = entry;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

/// `66 0F 8x` in 32-bit code decodes at Word operand size, and the Word allowlist admits it.
///
/// Nothing else in the tree has a 66-prefixed Jcc, and nothing pins the Word allowlist as a
/// closed set, so this test and the short-form one below carry the whole allowlist argument: if
/// either range is dropped the entry instruction stops classifying, the block has no slots, and
/// `compiled()` panics on a `Retry`.
#[test]
fn a_word_operand_size_near_jcc_is_lowered() {
    // 66 0f 85 10 00: jnz +0x10 at Word operand size, five bytes.
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &[0x66, 0x0f, 0x85, 0x10, 0x00]);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 1);
    assert_eq!(compilation.span.guest_len, 5);
}

/// The short form, `66 7x`, is the other half of the allowlist edit. The two-byte range and the
/// one-byte range are separate `matches!` arms and separate classifier arms, so dropping either
/// leaves the other passing.
#[test]
fn a_word_operand_size_short_jcc_is_lowered() {
    // 66 75 10: jnz +0x10 at Word operand size, three bytes.
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &[0x66, 0x75, 0x10]);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 1);
    assert_eq!(compilation.span.guest_len, 3);
}

/// A Word-size branch whose target crosses the 16-bit wrap must NOT be lowered, because
/// `relative_jump` masks it to 16 bits while the emitted form bakes an unmasked delta.
///
/// The two halves run the SAME instruction stream and differ only in the entry address, which is
/// what makes the negative attributable: without the low-entry control the high-entry assertion
/// would also hold if the block simply failed to form for an unrelated reason.
///
/// The three `inc` fillers are load-bearing. The compile loop returns early on
/// `slots.len() < 3 && !last.is_terminal()`, so with fewer of them the refused case would come
/// back as `Retry` rather than as a shorter block and there would be nothing to assert.
#[test]
fn a_word_jcc_above_the_wrap_is_refused_while_the_same_block_below_it_compiles() {
    // inc eax; inc ecx; inc edx; 66 0f 85 10 00 (jnz +0x10 at Word operand size).
    const CODE: [u8; 8] = [0x40, 0x41, 0x42, 0x66, 0x0f, 0x85, 0x10, 0x00];
    const HIGH: u32 = 0x1_0100;

    // Below the wrap: the branch is lowered and the block is all four instructions.
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &CODE);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let low = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(low.span.instructions, 4, "control: the branch must lower");
    assert_eq!(low.span.guest_len, 8);
    // The two mechanism counters are the slice's PRIMARY gate on the pinned corpus, so pin them
    // here rather than shipping an instrument nothing checks.
    let low_perf = cpu.perf_counters();
    assert_eq!(low_perf.jit_direct_word_control_admitted, 1);
    assert_eq!(low_perf.jit_direct_word_control_refused, 0);

    // Above the wrap: the target is 0x1_0118, the architectural target is 0x0118, and the block
    // stops at the three fillers.
    let (mut cpu, mut bus) = flat_fixture(HIGH, &CODE);
    warm(&mut cpu, &mut bus, &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3]);
    let high = compiled(jit::direct::compile(&mut cpu, HIGH, true));
    assert_eq!(
        high.span.instructions, 3,
        "the Word branch must not be lowered above the 16-bit wrap"
    );
    assert_eq!(high.span.guest_len, 3);
    let high_perf = cpu.perf_counters();
    assert_eq!(high_perf.jit_direct_word_control_admitted, 0);
    assert_eq!(high_perf.jit_direct_word_control_refused, 1);
}

/// The same block at Dword operand size MUST still compile above 0xFFFF. Every other Jcc
/// fixture in this crate entries at 0x100, 0x101 or 0x500, so a clamp wrongly applied at Dword
/// would pass all of them and reach the pinned corpus undetected.
///
/// This is an end-to-end confirmation, not the primary catcher: the predicate itself is already
/// pinned by `the_word_control_clamp_is_a_no_op_at_a_real_mode_limit`.
#[test]
fn a_dword_jcc_above_the_wrap_still_compiles() {
    // inc eax; inc ecx; inc edx; 0f 85 10 00 00 00 (jnz +0x10 at Dword operand size).
    const CODE: [u8; 9] = [0x40, 0x41, 0x42, 0x0f, 0x85, 0x10, 0x00, 0x00, 0x00];
    const HIGH: u32 = 0x1_0100;

    let (mut cpu, mut bus) = flat_fixture(HIGH, &CODE);
    warm(&mut cpu, &mut bus, &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3]);
    let compilation = compiled(jit::direct::compile(&mut cpu, HIGH, true));
    assert_eq!(compilation.span.instructions, 4);
    assert_eq!(compilation.span.guest_len, 9);
}

/// A 16-bit stack under a 32-bit code segment, which is a reachable configuration TODAY and is
/// what makes this slice testable before 16-bit admission is flipped.
///
/// `fresh()` already leaves SS at `default_size_32 == false`, so the only thing owed is a stack
/// pointer inside the fixture's memory. The high half of ESP is deliberately non-zero: a
/// 16-bit stack must preserve ESP[31:16], and it must also mask the effective address, so both
/// mechanisms are load-bearing in every test built on this.
fn sixteen_bit_stack_fixture(entry: u32, code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x2000];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    cpu.registers.set_esp(0x1234_0800);
    cpu.registers.eip = entry;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

/// THE ANTI-VACUITY GATE for the 16-bit stack slice.
///
/// A design review found that leaving the old `uses_stack() && !stack_is_32bit()` stop in place
/// alongside the new mapping would refuse every `Push16`, so the slice would do nothing while
/// every counter stayed identical and the pre-registered gate passed. Counter identity cannot
/// tell a correct inert slice from a broken one. This test can: it fails if the push is not
/// admitted, for any reason.
#[test]
fn a_word_push_on_a_sixteen_bit_stack_enters_the_block() {
    // inc eax; inc ecx; 66 50 (push ax at Word operand size).
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0x50]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "the Word push must be admitted on a 16-bit stack"
    );
    assert_eq!(compilation.span.guest_len, 4);
    // It is a WORD store, not a dword one. A width mix-up here would be invisible to the
    // instruction count and would misreport the bus split.
    assert_eq!(compilation.word_stores, 1);
    assert_eq!(compilation.dword_stores, 0);
}

/// Matrix row 2: a Word push on a THIRTY-TWO bit stack must not be admitted, because the
/// shipped `Push` kind would write four bytes and decrement four where the guest moves two.
/// Reachable today through a 66-prefixed push in 32-bit code.
///
/// The 16-bit-stack control beside it is what makes the negative attributable: without it the
/// assertion would also hold if the push simply failed to classify.
#[test]
fn a_word_push_on_a_thirty_two_bit_stack_is_refused_but_admitted_on_a_sixteen_bit_one() {
    // inc eax; inc ecx; inc edx; 66 50 (push ax at Word operand size).
    const CODE: [u8; 5] = [0x40, 0x41, 0x42, 0x66, 0x50];
    const WARM: [u32; 4] = [ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3];

    // 32-bit stack: the three fillers compile, the push does not.
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    warm(&mut cpu, &mut bus, &WARM);
    let wide = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        wide.span.instructions, 3,
        "a two-byte push must not be lowered by the four-byte kind"
    );
    assert_eq!(wide.span.guest_len, 3);

    // 16-bit stack, same bytes: the push IS admitted.
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    warm(&mut cpu, &mut bus, &WARM);
    let narrow = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(narrow.span.instructions, 4, "control: the push must lower");
    assert_eq!(narrow.span.guest_len, 5);
}

/// Matrix row 4: a Dword push on a 16-bit stack stays refused. Four bytes on a 16-bit stack
/// pointer is a form this slice does not build, and it must not fall through to either kind.
#[test]
fn a_dword_push_on_a_sixteen_bit_stack_is_still_refused() {
    // inc eax; inc ecx; inc edx; 50 (push eax at Dword operand size).
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x42, 0x50]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}

/// The anti-vacuity gate for the 16-bit POP, the same shape as the push one above.
///
/// Without the compile loop's mapping arm `Pop16` is unconstructible: the tuple falls to the
/// matrix's catch-all, growth stops, every other registration site is dead, and the emit
/// dispatch is satisfied by an arm nothing reaches. Counter identity on the pinned corpus
/// cannot see any of that, so this test is the gate.
///
/// The clock and width assertions are the other half. A missing `raw_clocks` arm undercharges
/// by 2 per pop and is invisible to architectural state; a read declared Dword instead of Word
/// miscounts the bus split and flips `has_wide_accesses`.
#[test]
fn a_word_pop_on_a_sixteen_bit_stack_enters_the_block() {
    // inc eax; inc ecx; 66 58 (pop ax at Word operand size).
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0x58]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "the Word pop must be admitted on a 16-bit stack"
    );
    assert_eq!(compilation.span.guest_len, 4);
    // A WORD read, not a dword one.
    assert_eq!(compilation.word_reads, 1);
    assert_eq!(compilation.dword_reads, 0);
    // Two INC at 2 each plus the pop at 4. A missing `raw_clocks` arm shows up here as 6,
    // because the pop would fall to the `_ => 2` default.
    assert_eq!(compilation.raw_clocks, 8);
}

/// Matrix row 2 for the pop: a Word pop on a THIRTY-TWO bit stack is not admitted, because the
/// shipped kind would read four bytes, advance four, and replace the whole destination.
#[test]
fn a_word_pop_on_a_thirty_two_bit_stack_is_refused_but_admitted_on_a_sixteen_bit_one() {
    // inc eax; inc ecx; inc edx; 66 58 (pop ax at Word operand size).
    const CODE: [u8; 5] = [0x40, 0x41, 0x42, 0x66, 0x58];
    const WARM: [u32; 4] = [ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3];

    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    warm(&mut cpu, &mut bus, &WARM);
    let wide = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(wide.span.instructions, 3);
    assert_eq!(wide.span.guest_len, 3);

    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    warm(&mut cpu, &mut bus, &WARM);
    let narrow = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(narrow.span.instructions, 4, "control: the pop must lower");
    assert_eq!(narrow.span.guest_len, 5);
}

/// The two immediate pushes ride the already-merged `Push16` arm, so this slice adds them to
/// the Word allowlist and nothing else. The test exists because that claim is the entire
/// argument for their being free.
#[test]
fn word_immediate_pushes_lower_on_a_sixteen_bit_stack() {
    // inc eax; inc ecx; 66 6a 7f (push imm8, sign extended, at Word operand size). The 0x66 is
    // load-bearing: this fixture's code segment is 32-bit, so an unprefixed 0x6a is Dword and
    // would take the shipped four-byte arm instead.
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0x6a, 0x7f]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let byte_form = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(byte_form.span.instructions, 3);
    assert_eq!(byte_form.word_stores, 1);

    // inc eax; inc ecx; 66 68 34 12 (push imm16 at Word operand size).
    let (mut cpu, mut bus) =
        sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0x68, 0x34, 0x12]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let word_form = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(word_form.span.instructions, 3);
    assert_eq!(
        word_form.span.guest_len, 6,
        "prefix, opcode and a two-byte immediate"
    );
    assert_eq!(word_form.word_stores, 1);
}

/// A 16-bit stack under a FLAT code segment, which `sixteen_bit_stack_fixture` cannot provide.
///
/// `fresh()` loads CS in real mode, so its limit is 0xFFFF and `control_target_limit` is the
/// identity at Word size. Every 16-bit-stack fixture built on it therefore admits a Word control
/// transfer whether or not the guard is applied at all. This helper is the only thing in the
/// tree that can tell those two apart.
fn flat_code_sixteen_bit_stack_fixture(entry: u32, code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x1_2000];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    let mut cs = cpu.registers.segment(SegmentIndex::Cs);
    cs.limit = u32::MAX;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = false;
    ss.limit = u32::MAX;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    cpu.registers.set_esp(0x1234_0800);
    cpu.registers.eip = entry;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

/// The anti-vacuity gate for the 16-bit CALL, plus its clock and store-width pins.
#[test]
fn a_word_call_on_a_sixteen_bit_stack_enters_the_block() {
    // inc eax; inc ecx; 66 e8 10 00 (call +0x10 at Word operand size).
    let (mut cpu, mut bus) =
        sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0xe8, 0x10, 0x00]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 6);
    // A WORD store of the return address, not a dword one.
    assert_eq!(compilation.word_stores, 1);
    assert_eq!(compilation.dword_stores, 0);
    // Two INC at 2 each plus the call at 7. A missing `raw_clocks` arm shows up here as 6.
    assert_eq!(compilation.raw_clocks, 11);
    // The static link edge is the CALL TARGET, not the fall-through. A kind missing from the
    // successor match lands on the fall-through arm instead, which is a wrong edge rather than
    // a missing one: the emitted terminal sets EIP to the target and then jumps through this
    // cell, so a mislinked block transfers into the wrong body. Nothing in guest state or in the
    // block's shape shows that.
    let target = compilation.successors[0].expect("a call links its target");
    assert_eq!(
        target.linear,
        ENTRY + 6 + 0x10,
        "entry + return_delta + rel16"
    );
    assert!(
        compilation.successors[1].is_none(),
        "a call has no second edge"
    );
}

/// THE `is_terminal` CATCHER, and it is the only one.
///
/// If `Call16` is missing from `is_terminal` the compile loop does not stop at it, so the block
/// keeps growing. The emitter still breaks at its own arm, so the trailing slots are never
/// emitted while the span, the clock total and the successor records are all computed over the
/// LONGER slot list. The block then ends up with a fall-through link where it should have a call
/// edge, and control transfers into it after the call while EIP says the target.
///
/// The two slots AFTER the call are load-bearing. With the terminal last, a block whose last
/// slot is terminal is exempt from the three-slot floor, so a mutated build simply produces a
/// LONGER valid block and any "at least N" assertion still passes. Both counts are exact.
#[test]
fn a_word_call_ends_its_block() {
    // inc eax; inc ecx; 66 e8 10 00 (call); inc edx; inc ebx.
    let (mut cpu, mut bus) =
        sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0xe8, 0x10, 0x00, 0x42, 0x43]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 6, ENTRY + 7],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must end AT the call, not grow past it"
    );
    assert_eq!(compilation.span.guest_len, 6);
}

/// THE `static_control_target` CATCHER. Nothing else in the tree can see this.
///
/// A Word-size call whose target crosses the 16-bit wrap must not be lowered, because the
/// interpreter masks the target and the emitted form bakes an unmasked delta. The guard is the
/// clamp merged as #629, and it only applies to a kind that `static_control_target` matches.
///
/// It needs a FLAT code segment to be observable at all: under the real-mode limit of 0xFFFF the
/// clamp is the identity and an unguarded kind is admitted identically. That is why this uses
/// its own fixture helper.
#[test]
fn a_word_call_above_the_wrap_is_refused_while_the_same_block_below_it_compiles() {
    // inc eax; inc ecx; inc edx; 66 e8 10 00 (call +0x10 at Word operand size).
    const CODE: [u8; 7] = [0x40, 0x41, 0x42, 0x66, 0xe8, 0x10, 0x00];
    const HIGH: u32 = 0x1_0100;

    let (mut cpu, mut bus) = flat_code_sixteen_bit_stack_fixture(ENTRY, &CODE);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let low = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(low.span.instructions, 4, "control: the call must lower");
    assert_eq!(low.span.guest_len, 7);

    let (mut cpu, mut bus) = flat_code_sixteen_bit_stack_fixture(HIGH, &CODE);
    warm(&mut cpu, &mut bus, &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3]);
    let high = compiled(jit::direct::compile(&mut cpu, HIGH, true));
    assert_eq!(
        high.span.instructions, 3,
        "a Word call must not be lowered above the 16-bit wrap"
    );
    assert_eq!(high.span.guest_len, 3);

    // The Dword form at the same high entry MUST still compile, or the clamp has leaked into
    // the 32-bit path.
    const DWORD_CODE: [u8; 8] = [0x40, 0x41, 0x42, 0xe8, 0x10, 0x00, 0x00, 0x00];
    let (mut cpu, mut bus) = flat_code_sixteen_bit_stack_fixture(HIGH, &DWORD_CODE);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    warm(&mut cpu, &mut bus, &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3]);
    let dword = compiled(jit::direct::compile(&mut cpu, HIGH, true));
    assert_eq!(dword.span.instructions, 4);
}

/// Matrix row 2 for the call: a Word call on a THIRTY-TWO bit stack is not admitted, because the
/// shipped kind would push four bytes and decrement four.
#[test]
fn a_word_call_on_a_thirty_two_bit_stack_is_refused_but_admitted_on_a_sixteen_bit_one() {
    const CODE: [u8; 7] = [0x40, 0x41, 0x42, 0x66, 0xe8, 0x10, 0x00];
    const WARM: [u32; 4] = [ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3];

    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    warm(&mut cpu, &mut bus, &WARM);
    let wide = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(wide.span.instructions, 3);

    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    warm(&mut cpu, &mut bus, &WARM);
    let narrow = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(narrow.span.instructions, 4, "control: the call must lower");
}

/// The anti-vacuity gate for the 16-bit RET, plus the two link rows nothing else can see.
///
/// `Ret16` is unconstructible unless BOTH the Word allowlist gains `0xc2`/`0xc3` AND the inner
/// width gate inside that classifier arm is gone. Either one missing and the kind never exists,
/// every other registration site is dead, and a counter-identity gate on the pinned corpus
/// passes regardless.
///
/// The successor assertions are the other half. A terminal missing from `dynamic_successor`
/// stays correct in guest state and in block shape while never linking at all; one missing from
/// the successor match consumes link cell 0 for a static edge the return path then cannot use,
/// halving the return PIC. Neither shows up anywhere else.
#[test]
fn a_word_ret_on_a_sixteen_bit_stack_enters_the_block() {
    // inc eax; inc ecx; 66 c3 (ret at Word operand size).
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0xc3]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 4);
    assert_eq!(compilation.word_reads, 1);
    assert_eq!(compilation.dword_reads, 0);
    // Two INC at 2 each plus the ret at 10. A missing `raw_clocks` arm shows up here as 6.
    assert_eq!(compilation.raw_clocks, 14);
    assert!(
        compilation.dynamic_successor,
        "a RET links dynamically, and nothing else observes this"
    );
    assert!(compilation.successors[0].is_none());
    assert!(compilation.successors[1].is_none());
}

/// The `is_terminal` catcher. Two slots AFTER the ret, because a block whose last slot is
/// terminal is exempt from the minimum-length rule, so a build that failed to stop would produce
/// a longer VALID block and an "at least N" assertion would still pass.
#[test]
fn a_word_ret_ends_its_block() {
    let (mut cpu, mut bus) =
        sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0xc3, 0x42, 0x43]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 4, ENTRY + 5],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must end AT the ret"
    );
    assert_eq!(compilation.span.guest_len, 4);
}

/// `0xc2` releases its immediate on top of the popped word, and at a 16-bit stack that release
/// moves SP alone.
#[test]
fn a_word_ret_immediate_lowers_and_keeps_its_release() {
    // inc eax; inc ecx; 66 c2 08 00 (ret 8 at Word operand size).
    let (mut cpu, mut bus) =
        sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0xc2, 0x08, 0x00]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 6);
    assert_eq!(compilation.word_reads, 1);
    assert!(compilation.dynamic_successor);
}

/// Matrix row 2: a Word ret on a THIRTY-TWO bit stack is not admitted, because the shipped kind
/// would read four bytes and release four.
#[test]
fn a_word_ret_on_a_thirty_two_bit_stack_is_refused_but_admitted_on_a_sixteen_bit_one() {
    const CODE: [u8; 5] = [0x40, 0x41, 0x42, 0x66, 0xc3];
    const WARM: [u32; 4] = [ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3];

    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    warm(&mut cpu, &mut bus, &WARM);
    let wide = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(wide.span.instructions, 3);

    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    warm(&mut cpu, &mut bus, &WARM);
    let narrow = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(narrow.span.instructions, 4, "control: the ret must lower");
}

#[path = "cpu_jit_s5_allowlist_test.rs"]
mod s5_allowlist;

#[path = "cpu_jit_coverage_matrix_test.rs"]
mod coverage_matrix;

#[path = "cpu_jit_watch_bit_test.rs"]
mod watch_bit;

/// The guard finding that blocked LEAVE from merging for a week, discharged.
///
/// A 16-bit stack LEAVE is `SP <- BP` and a two-byte pop, preserving ESP[31:16]. The emitted
/// `Leave` arm does a full-width `mov ESP, EBP`, which would destroy that high half. There is no
/// `Leave16`, so the only thing keeping the 16-bit-stack form out of a compiled block is the
/// stack-width admission matrix, which sends every `uses_stack()` kind that is not an admitted
/// (SS.B, operand size) pair to `Retry`.
///
/// The original review recorded that this guard had no test for LEAVE or for any kind. The 16-bit
/// stack slices have since covered the other kinds; this covers LEAVE, and it is the last thing
/// that slice was waiting on. Growth must stop at the two INCs before the LEAVE.
#[test]
fn a_leave_on_a_sixteen_bit_stack_is_refused() {
    // inc eax; inc ecx; inc edx; c9 (leave). Operand size follows CS.D and is Dword here, so the
    // tuple is (Leave, stack_is_32bit = false, Dword), which no arm admits.
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x42, 0xc9]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}

/// The positive control for the test above: on a 32-bit stack the SAME instruction IS admitted.
/// Without this, a `Leave` arm that was never constructible at all would satisfy the refusal test
/// while doing nothing, which is the registration-site failure this campaign keeps hitting.
#[test]
fn a_leave_on_a_thirty_two_bit_stack_enters_the_block() {
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 4].copy_from_slice(&[0x40, 0x41, 0x42, 0xc9]);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    cpu.registers.set_esp(0x0800);
    cpu.registers.eip = ENTRY;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 4,
        "LEAVE must be admitted on a 32-bit stack, or the refusal test above proves nothing"
    );
    assert_eq!(compilation.span.guest_len, 4);
    // The clock pin. LEAVE is `ESP <- EBP` then a POP, and the interpreter charges clocks(4) for
    // the 0xc9 arm, the same as a bare POP. Without its own `raw_clocks` arm the kind rides the
    // `_ => 2` default and undercharges by 2 per LEAVE, which moves core clocks on a real guest
    // and is invisible to every architectural assertion. A mutation battery found that nothing
    // else in the suite catches it.
    assert_eq!(
        compilation.raw_clocks, 10,
        "three 2-clock INCs plus a 4-clock LEAVE"
    );
    // The read is one dword off SS, not a byte or a word: a wrong width miscounts the bus split
    // and flips `has_wide_accesses`.
    assert_eq!(compilation.dword_reads, 1);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
}

#[test]
fn a_nop_is_lowered_and_block_growth_continues_through_it() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x90, 0x41, DIRECT_BARRIER]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    // The whole point of the kind. A NOP that were still unclassifiable would stop the block at
    // one instruction, and the clock pin below would then be asserting about a different block.
    assert_eq!(
        compilation.span.instructions, 3,
        "growth must continue past the NOP"
    );
    assert_eq!(compilation.span.guest_len, 3);
    // The clock pin. Both interpreter arms charge clocks(3) for 0x90. Without its own
    // `raw_clocks` arm the kind rides the `_ => 2` default and undercharges by one core clock per
    // NOP, which moves core clocks on a real guest and is invisible to every architectural
    // assertion in this suite. The same gap shipped twice before a mutation battery found it.
    assert_eq!(
        compilation.raw_clocks, 7,
        "two 2-clock INCs plus a 3-clock NOP"
    );
    // A NOP touches no memory and no segment. A stray registration here would arm an alignment
    // guard and a code watch for an instruction that emits nothing.
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.dword_reads, 0);
}

/// `DIRECT_BARRIER` must actually stop a block. Every fixture that uses it to pin a block
/// boundary silently stops testing anything the day that opcode is lowered, and a vacuous PASS
/// is worse than a failure because nobody looks at it. This is the tripwire.
#[test]
fn direct_barrier_opcode_is_still_unclassifiable() {
    let (mut cpu, mut bus) = fixture(&[DIRECT_BARRIER]);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    let span = structural(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        span.guest_len(),
        1,
        "the barrier must be one byte and must reject structurally"
    );

    // The positive control, and it is not decoration: without it the assertion above also passes
    // when the harness itself is broken and refuses everything.
    let (mut cpu, mut bus) = fixture(&[0x40, 0xc3]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    assert_eq!(
        compiled(jit::direct::compile(&mut cpu, ENTRY, true))
            .span
            .instructions,
        2,
        "the same harness must still compile a lowerable pair"
    );
}

/// The first fixture to compile a `PushMem` slot at all.
///
/// The push sits BETWEEN two `inc` slots, so this also pins that block growth continues through
/// it. A push that stopped the block would leave only the first `inc` as a slot, which is below
/// the compiler's three-instruction minimum for a non-terminal span: `compile` would return
/// `StructuralReject` instead of `Compiled`, and `compiled()` would panic before any assertion
/// below runs.
#[test]
fn a_push_through_memory_is_lowered_with_both_accesses_registered() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0xff,
        0x35,
        0x00,
        0x08,
        0x00,
        0x00,           // push dword [0x800]
        0x40,           // inc eax
        DIRECT_BARRIER, // stop the block here
    ]);
    cpu.registers.set_esp(0x1000);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 7, ENTRY + 8],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "growth must continue past the push"
    );
    assert_eq!(compilation.span.guest_len, 8);
    // The clock pin. The interpreter's group-5 arm 6 returns clocks(2), which is already the
    // `_ => 2` default, so this kind carries NO raw_clocks arm. That is the ImulMemAcc situation
    // and not the LEAVE one. Pinned anyway, because "correctly rides the default" and "nobody
    // checked" are indistinguishable in a diff, and a wrong charge moves core clocks on a real
    // guest without failing any architectural assertion.
    assert_eq!(
        compilation.raw_clocks, 6,
        "two 2-clock INCs plus a 2-clock PUSH"
    );
    // The static access counts PushMem feeds into the bus charge: one dword read (the source) and
    // one dword store (the stack write), with every byte and word count at zero since this kind
    // is dword-only. This does NOT pin the segment-mask scenario -- `SegmentLayout`'s mask is
    // built from `read_segment()`/`write_segment()`, an independent accessor, not from these
    // counts -- that is covered instead by the mid-block execution fixture
    // (`a_mid_block_push_through_memory_matches_the_interpreter` in `cpu_jit_direct_test.rs`),
    // whose DS-based source is what actually exercises a `read_segment` that could leave DS out
    // of the mask.
    assert_eq!(compilation.dword_reads, 1);
    assert_eq!(compilation.dword_stores, 1);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
}

/// Slice 39, B2's fixture: a block with one FLD m64 has `dword_reads == 2`, not 1, because an
/// x87 Qword access is two independent dword bus transactions (`read_qword`,
/// `fpu_exec.rs:720-740`), and `byte_reads`/`word_reads` stay at zero since m64 is dword-only
/// traffic. Mirrors `a_push_through_memory_is_lowered_with_both_accesses_registered`'s shape:
/// two 2-clock INCs bracket the x87 slot so growth continues past it and the barrier stops the
/// block exactly there.
#[test]
fn an_fld_m64_is_lowered_with_two_dword_reads_registered() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0xdd,
        0x05,
        0x00,
        0x08,
        0x00,
        0x00,           // fld qword [0x800]
        0x40,           // inc eax
        DIRECT_BARRIER, // stop the block here
    ]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 7, ENTRY + 8],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "growth must continue past the FLD"
    );
    assert_eq!(compilation.span.guest_len, 8);
    assert_eq!(compilation.dword_reads, 2, "one m64 read counts as two");
    assert_eq!(compilation.dword_stores, 0);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
}

/// Slice 40's fixture 4: a block with one FILD m64 (0xDF /5) has `dword_reads == 2`, not 1, for
/// the same reason the FLD m64 fixture above does: an x87 Qword access is two independent dword
/// bus transactions regardless of whether the memory operand is interpreted as a float or an
/// integer. Same bracketing shape: two 2-clock INCs around the x87 slot so growth continues past
/// it and the barrier stops the block exactly there. `StoreI64` (FISTP m64) is deferred, so
/// there is no store-side counterpart to this fixture in this slice.
#[test]
fn an_fild_m64_is_lowered_with_two_dword_reads_registered() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0xdf,
        0x2d,
        0x00,
        0x08,
        0x00,
        0x00,           // fild qword [0x800]
        0x40,           // inc eax
        DIRECT_BARRIER, // stop the block here
    ]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 7, ENTRY + 8],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "growth must continue past the FILD"
    );
    assert_eq!(compilation.span.guest_len, 8);
    assert_eq!(compilation.dword_reads, 2, "one m64 read counts as two");
    assert_eq!(compilation.dword_stores, 0);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
}

/// Slice 39, B2's fixture, the store side: a block with one FSTP m64 has `dword_stores == 2`.
/// Unlike the read side, this does NOT price the bus (B2: store bus cost is dynamic-only,
/// `exit.ram_dword_writes`), but it still feeds the quota bound, the map/code-watch gates and
/// the self-consistency debug assert in `run.rs`, so the static count must be right regardless.
#[test]
fn an_fstp_m64_is_lowered_with_two_dword_stores_registered() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0xdd,
        0x1d,
        0x00,
        0x08,
        0x00,
        0x00,           // fstp qword [0x800]
        0x40,           // inc eax
        DIRECT_BARRIER, // stop the block here
    ]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 7, ENTRY + 8],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "growth must continue past the FSTP"
    );
    assert_eq!(compilation.span.guest_len, 8);
    assert_eq!(compilation.dword_stores, 2, "one m64 store counts as two");
    assert_eq!(compilation.dword_reads, 0);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
}

/// The REGISTER form of `FF /6`, `push eax`, must stay refused.
///
/// `classify` restricts `/6` to the memory operand only: the register form is architecturally
/// `PUSH r32`, already covered by `0x50..0x57`, and admitting `FF /6` register-form onto that
/// path without checking its own clock charge against those opcodes would be a timing bug rather
/// than a missed lowering.
///
/// Three `inc` fillers land the count on the refusal alone: a block that grew past the register
/// form would show 4 instructions instead of 3.
#[test]
fn ff_slash_6_register_form_stays_refused() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0x41, // inc ecx
        0x42, // inc edx
        0xff,
        0xf0, // push eax, REGISTER form
        DIRECT_BARRIER,
    ]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "the register form of FF /6 must be refused, so the block is the three fillers"
    );
}

/// The first fixture to compile a `JmpMem` slot at all.
///
/// Two `inc` fillers put the jump on the THIRD slot, never the entry: a terminal at the block's
/// own entry point is never proven to run natively by anything that follows it, because nothing
/// does. The compiled span must END at the jump: unlike `PushMem`, which lets growth continue
/// past it, `JmpMem` is a terminal and stops the block right there.
#[test]
fn a_jmp_through_memory_is_lowered_as_a_terminal_with_a_dynamic_successor() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x25, 0x00, 0x08, 0x00, 0x00, // jmp dword [0x800]
    ]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "two fillers plus the jump, and nothing past it: JmpMem is a terminal"
    );
    assert_eq!(
        compilation.span.guest_len, 8,
        "the span must end AT the jump, not run past it: two one-byte fillers plus the six-byte \
         FF /4 form"
    );
    // The clock pin. Two 2-clock INCs plus the interpreter's explicit clocks(7) for group-5 arm 4
    // (`execute_extended.rs:920-924`). Without its own `raw_clocks` arm JmpMem rides the `_ => 2`
    // default and undercharges every jump by 5.
    assert_eq!(
        compilation.raw_clocks, 11,
        "two 2-clock INCs plus a 7-clock JmpMem"
    );
    assert_eq!(compilation.dword_reads, 1);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
    assert_eq!(compilation.dword_stores, 0);
    // The whole value of the slice rides on these two. Dropping either registration compiles a
    // working-looking block whose jump either never links (`dynamic_successor`) or statically
    // binds the wrong edge (`successors`), the bytes after the jump that are never a successor of
    // an unconditional one.
    assert!(
        compilation.dynamic_successor,
        "without this, link_sources never learns the cell and every jump exits to the dispatcher \
         forever"
    );
    assert_eq!(
        compilation.successors,
        [None, None],
        "a dynamic target has no static successor; the fall-through arm would record the bytes \
         after the jump as one, a phantom edge a stale dynamically-bound cell can transfer into"
    );
}

/// The first fixture to compile a `CallReg` slot at all: `call ebx`, the REGISTER form of
/// `0xFF /2`.
///
/// Modelled on `a_jmp_through_memory_is_lowered_as_a_terminal_with_a_dynamic_successor` above.
/// Two `inc` fillers put the call on the THIRD slot, never the entry, and the compiled span must
/// END at the call: it is a terminal, so growth stops there exactly as it does for `JmpMem`.
#[test]
fn a_call_through_a_register_is_lowered_as_a_terminal_with_a_dynamic_successor() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0xd3, // call ebx
    ]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "two fillers plus the call, and nothing past it: CallReg is a terminal"
    );
    assert_eq!(
        compilation.span.guest_len, 4,
        "the span must end AT the call, not run past it: two one-byte fillers plus the two-byte \
         FF /2 register form"
    );
    // The clock pin. Two 2-clock INCs plus the interpreter's explicit clocks(7) for group-5 arm 2
    // (`execute_extended.rs:914-918`). Without its own `raw_clocks` arm CallReg rides the `_ => 2`
    // default and undercharges every call by 5.
    assert_eq!(
        compilation.raw_clocks, 11,
        "two 2-clock INCs plus a 7-clock CallReg"
    );
    // One dword store (the return push) and no reads at all: the target comes from a register,
    // not memory.
    assert_eq!(compilation.dword_stores, 1);
    assert_eq!(compilation.dword_reads, 0);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
    // As with JmpMem, the whole value of the slice rides on these two. Dropping either
    // registration compiles a working-looking block whose call either never links
    // (`dynamic_successor`) or statically binds the wrong edge (`successors`).
    assert!(
        compilation.dynamic_successor,
        "without this, link_sources never learns the cell and every call exits to the dispatcher \
         forever"
    );
    assert_eq!(
        compilation.successors,
        [None, None],
        "a dynamic target has no static successor; the fall-through arm would record the bytes \
         after the call as one, a phantom edge a stale dynamically-bound cell can transfer into"
    );
}

/// The REGISTER form of `0xFF /4`: `jmp ebx`, duke3d-486's fifth-largest rejected row
/// (11,718,562 static exits, 11,736,700 interpreted executions; 32.8M/32.8M at 586).
///
/// `CallReg` minus the push and `JmpMem` minus the read, so what has to be pinned is exactly what
/// neither sibling can pin for it: this is the first kind that is a terminal with a dynamic
/// successor and touches NO MEMORY AT ALL. Three of the four assertions below are silent when
/// wrong.
///
/// * `raw_clocks` 7. `execute_extended.rs` group-5 arm 4 reads its target through
///   `read_operand_sized` -- which serves both operand forms -- and returns `Ok(clocks(7))`
///   without branching on the shape, so the register form charges what the memory form charges.
///   A missing arm rides the `_ => 2` default and undercharges every indirect jump by exactly 5,
///   which is mutation M3 and which nothing else in the tree can see.
/// * Every memory counter zero. `CallReg` has a store, `JmpMem` has a read, `CallMem` has both.
///   Only this kind has neither, and a spurious count here would make `run.rs` subtract dynamic
///   mode-13 counts from a static total that was never issued.
/// * `dynamic_successor` with `successors == [None, None]`. This is mutation M2: recording a
///   static successor alongside the dynamic binding arms the `LinkCell` retarget trap documented
///   on `JmpMem`'s doc comment -- `LinkCell::clear` does not reset `target_eip`, so a cell that
///   was dynamically bound and is later statically rebound transfers a later jump natively into
///   the wrong block. The fall-through arm would record the bytes AFTER the jump as a successor,
///   which is precisely that phantom edge.
#[test]
fn a_jmp_through_a_register_is_lowered_as_a_terminal_with_a_dynamic_successor() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0xe3, // jmp ebx
    ]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "two fillers plus the jump, and nothing past it: JmpReg is a terminal"
    );
    assert_eq!(
        compilation.span.guest_len, 4,
        "the span must end AT the jump: two one-byte fillers plus the two-byte FF /4 register form"
    );
    assert_eq!(
        compilation.raw_clocks, 11,
        "two 2-clock INCs plus a 7-clock JmpReg; the `_ => 2` default would read 6"
    );
    assert_eq!(compilation.dword_reads, 0);
    assert_eq!(compilation.dword_stores, 0);
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
    assert!(
        compilation.dynamic_successor,
        "without this, link_sources never learns the cell and every jump exits to the dispatcher \
         forever"
    );
    assert_eq!(
        compilation.successors,
        [None, None],
        "a dynamic target has no static successor; the fall-through arm would record the bytes \
         after the jump as one, the phantom edge a stale dynamically-bound cell can transfer into"
    );
}

/// The MEMORY form of `0xFF /2`: `call dword [0x800]`, doom's largest rejected census row.
///
/// The accounting pin the emitter cannot state for itself. `CallMem` is the first kind that is a
/// terminal with a dynamic successor AND a memory read AND a memory store at once, so it has to
/// register in four accessors that no single sibling exercises together, and three of the four are
/// silent when wrong:
///
/// * `raw_clocks` 7. Group-5 arm 2 charges `clocks(7)` for both operand forms
///   (`execute_extended.rs`), and a missing arm rides the `_ => 2` default and undercharges every
///   indirect call by 5 with nothing else in the tree able to see it.
/// * `dword_reads` 1 AND `dword_stores` 1. `CallReg` has the store and no read; `JmpMem` has the
///   read and no store. Only this kind has both, and the static counts are what `run.rs` subtracts
///   the dynamic mode-13 counts from.
/// * `dynamic_successor` with `successors == [None, None]`. Dropping either registration compiles
///   a working-looking block whose call never links, or one that statically binds a phantom edge
///   into the bytes after the call that a stale dynamically-bound cell can transfer into.
#[test]
fn a_call_through_memory_is_lowered_as_a_terminal_with_a_dynamic_successor() {
    let (mut cpu, mut bus) = fixture(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800]
    ]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "two fillers plus the call, and nothing past it: CallMem is a terminal"
    );
    assert_eq!(
        compilation.span.guest_len, 8,
        "the span must end AT the call: two one-byte fillers plus the six-byte FF /2 disp32 form"
    );
    assert_eq!(
        compilation.raw_clocks, 11,
        "two 2-clock INCs plus a 7-clock CallMem"
    );
    assert_eq!(compilation.dword_reads, 1, "the target dword");
    assert_eq!(compilation.dword_stores, 1, "the return-address push");
    assert_eq!(compilation.byte_reads, 0);
    assert_eq!(compilation.word_reads, 0);
    assert_eq!(compilation.byte_stores, 0);
    assert_eq!(compilation.word_stores, 0);
    assert!(
        compilation.dynamic_successor,
        "without this, link_sources never learns the cell and every call exits to the dispatcher \
         forever"
    );
    assert_eq!(compilation.successors, [None, None]);
}

/// A `CallMem` at the block ENTRY compiles as a ONE-instruction block, and that is the whole
/// mechanism of this slice rather than an edge case.
///
/// The census attributes 1,847,385 doom exits to entry points whose compile walk stopped at this
/// opcode, and their mean native prefix is 1.86 instructions -- under the three-slot minimum. What
/// admits them is `slots.len() < 3 && !terminal`: a terminal excuses the minimum. Before the
/// lowering the walk produced a non-terminal prefix of one or two slots and the whole entry became
/// a `StructuralReject`, so every static link into it stayed unbound forever. After it, the same
/// entry compiles and its links bind.
#[test]
fn a_call_through_memory_at_the_block_entry_compiles_as_a_one_instruction_block() {
    let (mut cpu, mut bus) = fixture(&[
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800], AT THE ENTRY
    ]);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 1,
        "a terminal excuses the three-slot minimum, which is what turns these census entries from \
         permanent structural rejections into compiled blocks"
    );
    assert_eq!(compilation.span.guest_len, 6);
    assert_eq!(compilation.raw_clocks, 7);
    assert!(compilation.dynamic_successor);
}

/// The stack-access cap interacting with a terminal `CallReg`: `MAX_BLOCK_STACK_ACCESSES` is 4,
/// and the compile loop's cap check runs BEFORE the slot that would cross it is admitted.
///
/// Three pushes plus `call ebx` is four stack accesses total, admitted as ONE block: the check
/// at the call sees `stack_accesses == 3`, not yet at the cap, so the call joins the block as its
/// terminal fourth stack use.
///
/// Four pushes plus `call ebx` is five stack accesses, and the fifth is refused: the check at the
/// call sees `stack_accesses == 4`, the cap, and stops. The block compiled at that entry is just
/// the four pushes (non-terminal, but at four instructions well past the three-slot minimum). A
/// second compile at the call's own address, right after, forms its OWN one-instruction block: a
/// terminal excuses the three-slot minimum (`slots.len() < 3 && !terminal`), and `defer_short` is
/// dead outside tests, so nothing stops that one-slot block from being installed.
#[test]
fn call_through_a_register_respects_the_stack_access_cap() {
    // Three pushes plus the call: one block, four instructions, none of them refused.
    {
        let (mut cpu, mut bus) = fixture(&[
            0x50, // push eax
            0x51, // push ecx
            0x52, // push edx
            0xff, 0xd3, // call ebx
        ]);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );

        let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            compilation.span.instructions, 4,
            "three pushes plus the call must compile as one block: the cap is checked BEFORE the \
             call is admitted, and three stack uses have not reached it yet"
        );
    }

    // Four pushes plus the call: the pushes alone hit the cap, so the call SPLITS into its own
    // block at the next entry.
    {
        let (mut cpu, mut bus) = fixture(&[
            0x50, // push eax
            0x51, // push ecx
            0x52, // push edx
            0x53, // push ebx
            0xff, 0xd0, // call eax
        ]);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3, ENTRY + 4],
        );

        let pushes = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(
            pushes.span.instructions, 4,
            "the fourth push hits the cap; the call itself is refused and never joins this block"
        );
        assert_eq!(pushes.span.guest_len, 4);

        let call_entry = ENTRY + 4;
        let call_block = compiled(jit::direct::compile(&mut cpu, call_entry, true));
        assert_eq!(
            call_block.span.instructions, 1,
            "the call re-forms as its own one-slot block: a terminal excuses the three-slot \
             minimum"
        );
        assert!(call_block.dynamic_successor);
    }
}

/// The same cap for the MEMORY form, and the only fixture that can see `CallMem` in `uses_stack()`
/// at all.
///
/// `uses_stack` has two effects for this kind and BOTH are masked by something else on the default
/// path, which is why this fixture is built the way it is rather than copied from its `CallReg`
/// sibling:
///
/// * the stack-width admission matrix, masked by `classify`'s own Dword gate. Either check alone
///   refuses the Word form, so `word_size_call_through_memory_stays_refused` cannot see this one
///   go missing.
/// * `MAX_BLOCK_STACK_ACCESSES`, masked by the HOST-PAGE length search. A mutation established
///   that directly: with `CallMem` dropped from `uses_stack` the call does join as a fifth stack
///   access, the block's emitted code then overflows a host page, and `compile_with_page_len`'s
///   binary search cuts it back to the same four instructions the cap would have produced. The
///   two bounds are indistinguishable through `compile`.
///
/// So this compiles through `compile_with_page_len_for_test` with a page of 1 MiB, which cannot
/// bind, leaving the stack-access cap as the only thing that can stop the block. That also records
/// a real property of the shipped emitter worth knowing on its own: a five-slot block whose last
/// slot is a `CallMem` does not fit one host page.
#[test]
fn call_through_memory_respects_the_stack_access_cap() {
    // Four pushes plus the call is five stack accesses. The fifth is refused, so the call splits
    // into its own block.
    let (mut cpu, mut bus) = fixture(&[
        0x50, // push eax
        0x51, // push ecx
        0x52, // push edx
        0x53, // push ebx
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800]
    ]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3, ENTRY + 4],
    );

    let pushes = compiled(jit::direct::compile_with_page_len_for_test(
        &mut cpu,
        ENTRY,
        true,
        1 << 20,
    ));
    assert_eq!(
        pushes.span.instructions, 4,
        "the fourth push hits the cap; the call itself is refused and never joins this block, and \
         with a 1 MiB page nothing but the cap can be what refused it"
    );
    assert_eq!(pushes.span.guest_len, 4);

    let call_block = compiled(jit::direct::compile(&mut cpu, ENTRY + 4, true));
    assert_eq!(
        call_block.span.instructions, 1,
        "the call re-forms as its own one-slot block"
    );
    assert!(call_block.dynamic_successor);
}

/// The `Rejected` row attribution closes: every rejected-target exit is either charged to a row
/// or counted in the residual.
///
/// `note_unbound_rejected_at` used to drop an exit whose entry linear was not in
/// `rejected_barrier` with no trace at all, which is the barrier census's analogue of the link
/// census's `missing_id`. Without the residual the C3 identity below cannot be evaluated on a
/// fixture run at all, and a shortfall reads as a smaller `Rejected` population rather than as an
/// instrument gap.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn barrier_census_closure_counts_rejected_exits_the_map_cannot_attribute() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, DIRECT_BARRIER]);
    cpu.enable_direct_barrier_census(true);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let _ = compiled(jit::direct::compile(&mut cpu, ENTRY, true));

    // ENTRY is in the rejected-span map; a linear the compiler never refused is not.
    const UNMAPPED: u32 = ENTRY + 0x4000;
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::Rejected, ENTRY);
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::Rejected, UNMAPPED);
    cpu.jit_direct
        .note_dynamic_miss_target(jit::direct::UnboundTarget::Rejected, ENTRY);
    cpu.jit_direct
        .note_dynamic_miss_target(jit::direct::UnboundTarget::Rejected, UNMAPPED);

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    assert_eq!(
        snapshot.rejected_unattributed, 1,
        "the exit at an unmapped linear must be counted, not dropped"
    );
    assert_eq!(snapshot.dynamic_rejected_unattributed, 1);

    let class = |targets: &[(&'static str, u64)]| {
        targets
            .iter()
            .find(|(label, _)| *label == "rejected")
            .expect("rejected class")
            .1
    };
    let static_rejected = class(&snapshot.unbound_targets);
    let dynamic_rejected = class(&snapshot.dynamic_miss_targets);
    assert_eq!(static_rejected, 2);
    assert_eq!(dynamic_rejected, 2);

    // C3, both lanes: row sum + residual == the class total.
    let row_static: u64 = snapshot.rows.iter().map(|row| row.unbound_exits).sum();
    let row_dynamic: u64 = snapshot
        .rows
        .iter()
        .map(|row| row.dynamic_unbound_exits)
        .sum();
    assert_eq!(row_static + snapshot.rejected_unattributed, static_rejected);
    assert_eq!(
        row_dynamic + snapshot.dynamic_rejected_unattributed,
        dynamic_rejected
    );
}

/// A second rejection at a linear the map already holds OVERWRITES the first, and that is the
/// only honest signal the census has for stale-hit mis-attribution.
///
/// The residual above cannot see this hazard: an overwritten key still resolves, so the exit is
/// charged to a row (the wrong one, or a stale one) and the residual stays zero. The map is never
/// pruned and is keyed on linear alone, so a recompiled-then-accepted linear keeps answering with
/// the barrier that refused an earlier block there.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn barrier_census_closure_counts_a_rejected_map_overwrite() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, DIRECT_BARRIER]);
    cpu.enable_direct_barrier_census(true);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let _ = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        cpu.direct_barrier_census_snapshot()
            .expect("enabled census snapshot")
            .rejected_barrier_overwrites,
        0,
        "the first rejection at a linear claims a free slot"
    );

    let _ = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        cpu.direct_barrier_census_snapshot()
            .expect("enabled census snapshot")
            .rejected_barrier_overwrites,
        1,
        "a second rejection at the same linear displaces the first row's claim"
    );
}

/// The carried totals are the arrays they claim to summarize, and the perf-counter join actually
/// reads `PerfCounters` rather than re-deriving the census's own numbers.
///
/// The perf counters are set by hand here: `note_unbound_target` is the census hook and does not
/// touch them, so a join that returned `classified_static` would be indistinguishable from a real
/// one without a fixture that drives the two apart.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn barrier_census_closure_totals_match_the_class_arrays_and_the_perf_counters() {
    let mut cpu = CpuGsw::default();
    cpu.enable_direct_barrier_census(true);
    for kind in [
        jit::direct::UnboundTarget::Absent,
        jit::direct::UnboundTarget::Seen,
        jit::direct::UnboundTarget::Seen,
    ] {
        cpu.jit_direct.note_unbound_target(kind, ENTRY);
    }
    cpu.jit_direct
        .note_dynamic_miss_target(jit::direct::UnboundTarget::Compiled, ENTRY);
    cpu.perf.jit_direct_unresolved_static_unbound = 11;
    cpu.perf.jit_direct_unresolved_dynamic_miss_or_unbound = 5;

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    assert_eq!(
        snapshot.classified_static,
        snapshot
            .unbound_targets
            .iter()
            .map(|(_, count)| count)
            .sum::<u64>()
    );
    assert_eq!(snapshot.classified_static, 3);
    assert_eq!(
        snapshot.classified_dynamic,
        snapshot
            .dynamic_miss_targets
            .iter()
            .map(|(_, count)| count)
            .sum::<u64>()
    );
    assert_eq!(snapshot.classified_dynamic, 1);
    assert_eq!(snapshot.static_unbound_exits, 11);
    assert_eq!(snapshot.dynamic_miss_exits, 5);
}

/// B.3's closure identity: the `dormant_heat` histogram accounts for every exit of its class, on
/// both lanes, with the truncated tail carried rather than dropped.
///
/// This is the C3 standard the `Rejected` histogram already meets, and it is the reason the
/// instrument comes before the knob: a head-limited histogram that silently under-reported its own
/// class would answer "concentrated" for any workload, because the tail is where diffuse mass
/// lives by definition.
///
/// Note what is NOT here: a residual. `note_dormant_heat_at` has no lookup that can miss, so
/// unlike `rejected_unattributed` there is no honest way for an exit to go unrecorded, and a
/// residual field would be a slot that could only ever read zero.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn barrier_census_closure_dormant_heat_histogram_closes_on_its_class() {
    let mut cpu = CpuGsw::default();
    cpu.enable_direct_barrier_census(true);

    // Three sites, uneven counts, both lanes, plus one exit of a DIFFERENT dormant class at a
    // fourth linear: `DormantOther` must not enter the histogram, or the join is by address
    // rather than by class.
    for _ in 0..5 {
        cpu.jit_direct
            .note_unbound_target(jit::direct::UnboundTarget::DormantHeat, ENTRY);
    }
    for _ in 0..2 {
        cpu.jit_direct
            .note_unbound_target(jit::direct::UnboundTarget::DormantHeat, ENTRY + 0x40);
    }
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::DormantHeat, ENTRY + 0x80);
    for _ in 0..3 {
        cpu.jit_direct
            .note_dynamic_miss_target(jit::direct::UnboundTarget::DormantHeat, ENTRY);
    }
    cpu.jit_direct
        .note_dynamic_miss_target(jit::direct::UnboundTarget::DormantHeat, ENTRY + 0x80);
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::DormantOther, ENTRY + 0xc0);

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let class = |targets: &[(&'static str, u64)]| {
        targets
            .iter()
            .find(|(label, _)| *label == "dormant_heat")
            .expect("dormant_heat class")
            .1
    };
    let class_static = class(&snapshot.unbound_targets);
    let class_dynamic = class(&snapshot.dynamic_miss_targets);
    assert_eq!(class_static, 8);
    assert_eq!(class_dynamic, 4);

    let head_static: u64 = snapshot
        .dormant_heat_sites
        .iter()
        .map(|site| site.static_exits)
        .sum();
    let head_dynamic: u64 = snapshot
        .dormant_heat_sites
        .iter()
        .map(|site| site.dynamic_exits)
        .sum();
    assert_eq!(
        head_static + snapshot.dormant_heat_truncated_static,
        class_static,
        "the static histogram must account for every dormant_heat exit of its class"
    );
    assert_eq!(
        head_dynamic + snapshot.dormant_heat_truncated_dynamic,
        class_dynamic
    );
    assert_eq!(
        snapshot.dormant_heat_distinct_sites, 3,
        "the DormantOther exit at a fourth linear must not enter the histogram"
    );
    assert!(
        snapshot
            .dormant_heat_sites
            .iter()
            .all(|site| site.linear != ENTRY + 0xc0)
    );

    // Descending by TOTAL exits: ENTRY (5+3), then ENTRY+0x40 (2+0), then ENTRY+0x80 (1+1).
    let order: Vec<u32> = snapshot
        .dormant_heat_sites
        .iter()
        .map(|site| site.linear)
        .collect();
    assert_eq!(order, vec![ENTRY, ENTRY + 0x40, ENTRY + 0x80]);
    assert_eq!(snapshot.dormant_heat_sites[0].static_exits, 5);
    assert_eq!(snapshot.dormant_heat_sites[0].dynamic_exits, 3);
}

/// The head is limited and the tail is SUMMED, so the identity above survives truncation.
///
/// Written as its own fixture because the closure test cannot see this: with three sites nothing
/// is truncated, and a build that dropped the tail outright would pass it. This is the fixture
/// that proves the head limit is real and that the residual it produces is carried.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn barrier_census_closure_dormant_heat_histogram_carries_its_truncated_tail() {
    const SITES: usize = jit::direct::census::DORMANT_HEAT_SITES + 20;
    let mut cpu = CpuGsw::default();
    cpu.enable_direct_barrier_census(true);

    // Site `index` takes `SITES - index` exits, so the ordering is total and the tail is the 20
    // smallest.
    for index in 0..SITES {
        for _ in 0..(SITES - index) {
            cpu.jit_direct.note_unbound_target(
                jit::direct::UnboundTarget::DormantHeat,
                ENTRY + (index as u32) * 0x10,
            );
        }
    }

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    assert_eq!(
        snapshot.dormant_heat_sites.len(),
        jit::direct::census::DORMANT_HEAT_SITES,
        "the published head is limited"
    );
    assert_eq!(snapshot.dormant_heat_distinct_sites, SITES as u64);
    assert_ne!(
        snapshot.dormant_heat_truncated_static, 0,
        "a fixture with an empty tail cannot prove the tail is carried"
    );

    let class_static = snapshot
        .unbound_targets
        .iter()
        .find(|(label, _)| *label == "dormant_heat")
        .expect("dormant_heat class")
        .1;
    let head_static: u64 = snapshot
        .dormant_heat_sites
        .iter()
        .map(|site| site.static_exits)
        .sum();
    assert_eq!(
        head_static + snapshot.dormant_heat_truncated_static,
        class_static
    );
    // The head really is the LARGEST sites, not the first ones the map happened to yield.
    assert_eq!(snapshot.dormant_heat_sites[0].static_exits, SITES as u64);
    assert_eq!(
        snapshot.dormant_heat_sites[jit::direct::census::DORMANT_HEAT_SITES - 1].static_exits,
        (SITES - jit::direct::census::DORMANT_HEAT_SITES + 1) as u64
    );
}

/// A dormant-heat site whose entry a compile walk actually reached reads `compile_walked`, and one
/// no walk ever started from does not.
///
/// This is the distinction §B.4 turns on. "A walk ran over these bytes and no matcher fired"
/// argues for widening the lane class; "no walk was ever seen here" says nothing about the shape
/// at all. A single boolean conflating them would answer neither question.
///
/// WALK, not trial: the compile below is an ordinary `jit::direct::compile`, not the heat gate's
/// one-per-key-per-epoch lane trial. That is the point — the map records walks of either kind, and
/// this fixture pins that an ordinary one sets the bit.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn barrier_census_closure_dormant_heat_site_reports_whether_a_walk_ever_reached_it() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0x43, DIRECT_BARRIER]);
    cpu.enable_direct_barrier_census(true);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3, ENTRY + 4],
    );
    let _ = jit::direct::compile(&mut cpu, ENTRY, true);
    // A SECOND entry, walked but never dormant. It exists to make `walked_entries_run_wide`
    // provably larger than the walked dormant-heat set below, which is the whole reason that
    // field carries "run_wide" in its name: it is a superset over every entry the backend touched
    // and shares no denominator with `dormant_heat_distinct_sites`.
    let _ = jit::direct::compile(&mut cpu, ENTRY + 1, true);

    const NEVER_WALKED: u32 = ENTRY + 0x4000;
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::DormantHeat, ENTRY);
    cpu.jit_direct
        .note_unbound_target(jit::direct::UnboundTarget::DormantHeat, NEVER_WALKED);

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let site = |linear: u32| {
        *snapshot
            .dormant_heat_sites
            .iter()
            .find(|site| site.linear == linear)
            .expect("histogram site")
    };
    assert!(
        site(ENTRY).compile_walked,
        "an ordinary compile walk ran from ENTRY, so its bytes were offered to both lane matchers"
    );
    assert!(
        !site(NEVER_WALKED).compile_walked,
        "no walk was ever seen here, so the absence of a lane says nothing about the shape"
    );
    // Neither lane matcher can fire on `INC r32`, so this fixture also pins that `compile_walked`
    // is carried apart from the two lane bits rather than implied by them.
    assert!(!site(ENTRY).imm_lane_matched);
    assert!(!site(ENTRY).disp_lane_matched);

    // `walked_entries_run_wide` is RUN-WIDE and is not a per-class figure. Exactly one of the two
    // dormant-heat sites was walked, and the run walked at least two entries, so the field is
    // strictly larger than the quantity a reader might mistake it for. Pinned so a future rename
    // back to something class-shaped has to break a test rather than a reader.
    let walked_dormant = snapshot
        .dormant_heat_sites
        .iter()
        .filter(|site| site.compile_walked)
        .count() as u64;
    assert_eq!(walked_dormant, 1);
    assert_eq!(snapshot.dormant_heat_distinct_sites, 2);
    assert!(
        snapshot.walked_entries_run_wide > walked_dormant,
        "the walked set spans every entry the backend compiled, not only the dormant ones: \
         {} run-wide against {walked_dormant} walked dormant sites",
        snapshot.walked_entries_run_wide
    );
}

/// The suffix must stop where the compile walk's PAGE BUDGET stops, and the claim is an equality
/// against the block the walk actually installs.
///
/// The eighth divergence in `census_native_suffix`'s ledger. The budget ends the walk when the
/// running emitted-size estimate reaches one host page, which on a memory-heavy path binds long
/// before `MAX_BLOCK_INSTRUCTIONS`; a scan without the mirror walks on to the instruction cap and
/// reports a block nothing will ever build. It is not a bounded over-report either -- it grows
/// with how memory-heavy the path is, which is the axis this column ranks on.
///
/// Both legs are the same instruction stream. The census leg puts the barrier in front of it, and
/// the suffix then counts every slot of the counterfactual block except the barrier itself, so the
/// two must differ by exactly one.
#[test]
fn census_suffix_stops_where_the_page_budget_does() {
    const COUNT: usize = 31;
    // `mov eax,[0x3000]`: a memory load, so the block hits the page budget well inside
    // `MAX_BLOCK_INSTRUCTIONS`. A register-only stream could not -- at forty bytes a slot the
    // instruction cap binds first, and the fixture would pass with the mirror deleted.
    const LOAD: [u8; 6] = [0x8b, 0x05, 0x00, 0x30, 0x00, 0x00];

    let mut plain = Vec::new();
    let mut plain_starts = Vec::new();
    for _ in 0..COUNT {
        plain_starts.push(plain.len() as u32);
        plain.extend_from_slice(&LOAD);
    }
    let (mut cpu, mut bus) = fixture(&plain);
    let addresses: Vec<_> = plain_starts.iter().map(|offset| ENTRY + offset).collect();
    warm(&mut cpu, &mut bus, &addresses);
    let installed = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert!(
        usize::from(installed.span.instructions) < COUNT,
        "the fixture must be a stream the budget actually cuts: {} of {COUNT} slots",
        installed.span.instructions
    );

    let mut barred = vec![DIRECT_BARRIER];
    let mut barred_starts = vec![0u32];
    for _ in 0..COUNT {
        barred_starts.push(barred.len() as u32);
        barred.extend_from_slice(&LOAD);
    }
    assert_eq!(
        barrier_suffix(&barred, &barred_starts),
        u64::from(installed.span.instructions) - 1,
        "the counterfactual block the suffix describes must be the block the walk installs: the \
         barrier is one of its slots, and the rest are the suffix"
    );
}

/// The suffix must stop at a DEMOTED call-out site, for the same reason it stops at any other
/// boundary: no later compile walk will put that slot in a block, so a suffix that counts it is
/// describing a block that cannot exist.
///
/// The ninth divergence in `census_native_suffix`'s ledger, and the one whose over-report is
/// self-reinforcing: the rows that demote are the rows a widening slice is being asked to keep, so
/// a column that ignores demotions makes a losing row look like a winning one.
///
/// The site is marked directly rather than earned by three resyncs. `note_demoted_callout_site` is
/// the same door the governor uses, and driving a real demotion needs a protected-mode machine
/// this file does not have; what is under test here is the SCAN, not the governor.
#[test]
fn census_suffix_stops_at_a_demoted_call_out_site() {
    // barrier, two `inc eax`, `pop dword [0x3000]`, two more `inc eax`.
    const SLOT: u32 = ENTRY + 3;
    let mut code = vec![DIRECT_BARRIER, 0x40, 0x40];
    code.extend_from_slice(&[0x8f, 0x05, 0x00, 0x30, 0x00, 0x00]);
    code.extend_from_slice(&[0x40, 0x40]);
    let starts: &[u32] = &[0, 1, 2, 3, 9, 10];

    assert_eq!(
        barrier_suffix(&code, starts),
        5,
        "control: with the site clean the call-out joins and everything behind it follows"
    );

    let (mut cpu, mut bus) = fixture(&code);
    cpu.enable_direct_barrier_census(true);
    let addresses: Vec<_> = starts.iter().map(|offset| ENTRY + offset).collect();
    warm(&mut cpu, &mut bus, &addresses);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a key for the fixture block");
    // Physical is linear in this fixture, so the slot's site is its address.
    assert!(cpu.jit_direct.note_demoted_callout_site(SLOT, key.mode_key));
    let _ = jit::direct::compile(&mut cpu, ENTRY, true);
    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot");
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == u16::from(DIRECT_BARRIER))
        .expect("the barrier row");
    assert_eq!(
        row.native_suffix_instructions, 2,
        "the two slots in front of the demoted site, and nothing past it"
    );
}

// The retry-cause instrument (S4a). `DormantReason::CompileRetry` used to be the whole answer
// for every non-structural compile failure, and on the tombraid loader that one label held 464
// dormant keys absorbing 3.9 M static_unbound exits with no way to tell which of them a retry
// could ever help. These fixtures pin one cause per family and pin the two counter columns.

fn retry_cause(outcome: jit::direct::CompileOutcome) -> jit::direct::RetryCause {
    match outcome {
        jit::direct::CompileOutcome::Retry(cause) => cause,
        jit::direct::CompileOutcome::Compiled(_) => panic!("fixture unexpectedly compiled"),
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("fixture unexpectedly became a structural rejection")
        }
    }
}

#[test]
fn retry_cause_is_decode_miss_when_a_slot_has_no_line() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    assert_eq!(
        retry_cause(jit::direct::compile(&mut cpu, ENTRY, true)),
        jit::direct::RetryCause::DecodeMiss
    );
}

#[test]
fn retry_cause_is_segment_limit_when_a_slot_runs_past_cs_limit() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let mut cs = cpu.registers.cs();
    cs.limit = ENTRY;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert_eq!(
        retry_cause(jit::direct::compile(&mut cpu, ENTRY, true)),
        jit::direct::RetryCause::SegmentLimit
    );
}

/// The block-page rule, which is the walk's own 4 KiB boundary and not the guest's paging.
/// Two slots fit below `0x1000` and the third does not, so the walk stops there with a prefix
/// the min-length rule cannot install. The cause reported is the INNER one: `PageCross` and not
/// `TooShort`, because the walk had already given up before the length rule looked at it. That
/// ordering is the point of the assertion.
#[test]
fn retry_cause_is_page_cross_when_the_block_leaves_its_page() {
    let mut memory = vec![0; 0x2000];
    memory[0xffe..0x1001].copy_from_slice(&[0x40, 0x41, 0x42]);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    warm(&mut cpu, &mut bus, &[0xffe, 0xfff, 0x1000]);
    assert_eq!(
        retry_cause(jit::direct::compile(&mut cpu, 0xffe, true)),
        jit::direct::RetryCause::PageCross
    );
}

/// The min-length rule owns the answer only when the walk itself ended on a BOUNDARY. The
/// dirty-segment rule is the one boundary a two-slot block can reach: `mov ds,ax` overwrites the
/// segment whose base the following load would bake, so the walk ends cleanly at one slot and
/// the block is too short to install.
#[test]
fn retry_cause_is_too_short_when_a_boundary_leaves_a_one_slot_block() {
    let (mut cpu, mut bus) = fixture(&[0x8e, 0xd8, 0x8b, 0x00]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2]);
    assert_eq!(
        retry_cause(jit::direct::compile(&mut cpu, ENTRY, true)),
        jit::direct::RetryCause::TooShort
    );
}

/// The four block caps (x87 slots, call-out slots, memory-ALU slots, stack accesses) can only
/// fire once the walk already holds three or more slots, so each of them SHORTENS a block and
/// none of them can park a key. That is a structural property of the thresholds and not an
/// accident of this fixture, and it is why `RetryCause::CalloutCap` reads zero on every
/// workload today. Five `pop dword [eax]` slots against `MAX_BLOCK_CALLOUT_SLOTS` of four:
/// the block installs with four and the cap counter moves.
#[test]
fn the_call_out_cap_shortens_the_block_instead_of_parking_the_key() {
    let code = [0x8f, 0x00, 0x8f, 0x00, 0x8f, 0x00, 0x8f, 0x00, 0x8f, 0x00];
    let (mut cpu, mut bus) = fixture(&code);
    let addresses: Vec<_> = (0..5).map(|slot| ENTRY + slot * 2).collect();
    warm(&mut cpu, &mut bus, &addresses);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 4);
    assert_eq!(cpu.direct_stall_snapshot().callout_slot_cap_hits, 1);
    assert_eq!(
        retry_cause_counts(&cpu, jit::direct::RetryCause::CalloutCap),
        (0, 0)
    );
}

/// The one member of the cap family that CAN park a key, because it is not a budget: x87 and
/// call-out slots never share a block, in either order, and that rule fires at the second slot.
/// `pop dword [eax]` then `fld1` leaves a one-slot prefix the min-length rule cannot install,
/// and the inner cause is what gets reported.
#[test]
fn retry_cause_is_x87_cap_when_a_float_slot_follows_a_call_out() {
    let (mut cpu, mut bus) = fixture(&[0x8f, 0x00, 0xd9, 0xe8, 0xd9, 0xe8]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2, ENTRY + 4]);
    assert_eq!(
        retry_cause(jit::direct::compile(&mut cpu, ENTRY, true)),
        jit::direct::RetryCause::X87Cap
    );
}

/// A one-byte arena page: the full walk compiles, overflows, and the recovery search finds no
/// prefix that fits. That failure belongs to the search itself, so the cause is `HostPageLen`.
#[test]
fn retry_cause_is_host_page_len_when_no_prefix_fits_the_arena_page() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0x43]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    assert_eq!(
        retry_cause(jit::direct::compile_with_page_len_for_test(
            &mut cpu, ENTRY, true, 1
        )),
        jit::direct::RetryCause::HostPageLen
    );
}

/// The fold in `compile_with_page_len` must not overwrite a cause the walk already named. Same
/// one-byte page as above, but the full-length walk never reaches the size question because a
/// slot has no decode line: the reported cause stays `DecodeMiss`.
#[test]
fn the_host_page_fold_reports_the_inner_cause() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    assert_eq!(
        retry_cause(jit::direct::compile_with_page_len_for_test(
            &mut cpu, ENTRY, true, 1
        )),
        jit::direct::RetryCause::DecodeMiss
    );
}

fn retry_cause_counts(cpu: &CpuGsw, cause: jit::direct::RetryCause) -> (u64, u64) {
    let snapshot = cpu.direct_stall_snapshot();
    let compile_retry = snapshot
        .dormant
        .iter()
        .find(|(label, _)| *label == "compile_retry")
        .expect("the compile_retry dormant row")
        .1;
    assert_eq!(
        snapshot
            .retry_causes
            .iter()
            .map(|counts| counts.count)
            .sum::<u64>(),
        compile_retry,
        "the cause split must sum to the dormant row it splits"
    );
    let row = snapshot
        .retry_causes
        .iter()
        .find(|counts| counts.cause == cause.label())
        .expect("every cause has a row");
    (row.count, row.keys)
}

/// The two columns are different questions. `count` is attempts and rises on every park;
/// `keys` counts the distinct keys the park moved out of `Seen`, so a second attempt on a key
/// that is already Dormant raises the first and not the second.
#[test]
fn retry_causes_are_counted_once_per_attempt_and_once_per_key() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    cpu.set_jit_auto_admit(true);
    assert_eq!(
        retry_cause_counts(&cpu, jit::direct::RetryCause::DecodeMiss),
        (0, 0)
    );

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation parks the key Seen");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("the compile attempt retries");
    assert_eq!(
        retry_cause_counts(&cpu, jit::direct::RetryCause::DecodeMiss),
        (1, 1)
    );

    // A second park of the same key. The entry is no longer `Seen`, so only the attempt column
    // moves; that is what makes `keys` a count of the dormant population rather than of work.
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.jit_direct.dormant(
        key,
        jit::direct::DormantReason::CompileRetry,
        Some(jit::direct::RetryCause::DecodeMiss),
    );
    assert_eq!(
        retry_cause_counts(&cpu, jit::direct::RetryCause::DecodeMiss),
        (2, 1)
    );
}
