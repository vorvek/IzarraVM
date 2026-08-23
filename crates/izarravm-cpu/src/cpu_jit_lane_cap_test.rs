// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The per-family lane-BUDGET counters: `imm_lane_cap_refusals`, `imm8_lane_cap_refusals`,
//! `count_lane_cap_refusals` and `disp_lane_cap_refusals`. The two Option D arms
//! (`disp_store_lane_cap_refusals`, `disp_load_widen_lane_cap_refusals`) draw on the SAME budget
//! and are pinned OFF here; their own fixtures live in `cpu_jit_disp_store_lane_test.rs`.
//!
//! Six lane matchers draw on one `MAX_BLOCK_IMM_LANES` budget, and until these counters existed a
//! block that ran out of lanes and a family that had no lane-shaped slots produced the same
//! reading: registrations, and nothing else. That is the blindness this file pins. A widening
//! ladder that reads its registrations flat needs to know whether the family was unlaneable or
//! whether the budget turned it away, because the two point at completely different next steps.
//!
//! # The four things the fixtures here pin
//!
//! **The cap arm is charged to ONE family.** Each matcher tests the cap UNDER its own kind,
//! opcode, prefix and length bars, so the slot that trips the budget has already been narrowed to
//! one family. Every fixture therefore asserts all six counters, not just its own: a cap test
//! hoisted to the head of the matchers would move all six on the same slot and the five zeros are
//! what catches it.
//!
//! **The cap arm is NOT the knob arm.** Three of the four matchers used to read
//! `lanes_used >= MAX_BLOCK_IMM_LANES || !<knob>_enabled()` as one disjunction, and a counter on
//! that fused refusal cannot tell budget pressure from an off arm, which makes it worthless on
//! exactly the A/B leg it would be read on. Each lane CLASS knob is now tested first and returns
//! before the cap is consulted, so its off arm reads zero. `IZARRAVM_LANE_FAMILY` is a different
//! kind of knob and its fixtures say a different thing: it narrows `imm_lane_for`'s admission set
//! rather than switching a class off, so on its narrow arm the `/0` fillers still charge when
//! capped and only the widened `0x81 /5` tail goes uncounted.
//!
//! **The cap sits on a chosen SIDE of the page guard, and the side differs by family.**
//! `imm_lane_for`, `imm8_lane_for` and `count_lane_for` test the budget above `direct_host_bytes`,
//! so a capped block stops paying the fetch-cache scan per slot and their counters include slots
//! the page guard would also have refused. `disp_lane_for` keeps the budget below both its heat
//! gate (required) and that scan, so its counter is the tighter number. Both directions are
//! pinned, by `a_capped_slot_whose_lane_bytes_are_not_direct_mapped_still_charges_its_family` and
//! `a_disp_slot_whose_lane_bytes_are_not_direct_mapped_charges_no_cap_refusal`, which run the same
//! clipped fetch entry against the two orderings and assert opposite answers.
//!
//! **The tally counts INSTALLED blocks, and nothing else.** The refusals are recorded on the walk
//! (`LaneCapRefusals`, carried on the `Compilation`) and folded into the census tally by
//! `JitState::install` on the success arm of its inner install. That is what gives them the same
//! denominator as the lane REGISTRATIONS they are read against, and the reason it matters is that
//! the compile path walks a block more than once: `compile_with_page_len`'s recovery search
//! re-walks prefixes after an emission overruns the arena page, and a walk can end in a `Retry`
//! the caller throws away. `a_walk_that_does_not_install_charges_nothing` (which also forges a
//! FAILED install, the arm that says which side of the `?` the fold sits on) and
//! `re_walking_the_same_bytes_charges_only_the_walk_that_installs` are what hold that.
//!
//! # Why the filler is the `0x81` family and the subject is one trailing slot
//!
//! Three of the four families cannot supply twelve laneable slots in one block on their own:
//! `disp_lane_for`'s `0x8A` loads are memory slots priced at `EMITTED_MEMORY_SLOT_BYTES`, and
//! thirteen of them overrun the walk's 4 KB page budget before the lane budget is anywhere near
//! spent. So the budget is spent by twelve register-only `ADD EBP, imm32` fillers and the
//! thirteenth slot is the family under test. That is not a workaround, it is the stronger
//! fixture: `lanes_used` reaches twelve whatever the subject family's knob says, so an
//! implementation that fused the arms back together is caught by the off-arm fixtures instead of
//! quietly reading zero on both sides.
//!
//! The filler is admitted on BOTH arms of `IZARRAVM_LANE_FAMILY` (`op: 0`, `MemoryWidth::Dword`
//! is the narrow arm's whole admission set), so no fixture here depends on a knob it cannot
//! force. All four overridable knobs are stated on every fixture, per `cpu_test.rs`'s
//! `DIRECT_BARRIER` rule: a fixture that read an ambient arm would pass for the wrong reason the
//! next time a default flips.
//!
//! # Mutation record
//!
//! Twenty-one mutants, each applied to the source named and the whole file re-run. ALL TWENTY-ONE
//! DIED. The fixture named first is the one whose claim the mutant contradicts most directly, and
//! the count in brackets is how many of the fifteen fixtures failed in total.
//!
//! ## The counter exists, per family
//!
//! 1. Drop the `LANE_CAP_IMM` add in `imm_lane_for` ->
//!    `imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot` [5].
//! 2. Drop the `LANE_CAP_IMM8` add in `imm8_lane_for` ->
//!    `imm8_lane_cap_refusal_is_charged_to_the_imm8_family` [2].
//! 3. Drop the `LANE_CAP_COUNT` add in `count_lane_for` ->
//!    `count_lane_cap_refusal_is_charged_to_the_count_family` [2].
//! 4. Drop the `LANE_CAP_DISP` add in `disp_lane_for` ->
//!    `disp_lane_cap_refusal_is_charged_to_the_disp_family` [1].
//!
//! ## The cap arm is not the knob arm, per family
//!
//! 5. Fuse the arms back in `imm8_lane_for` (`lanes_used >= MAX_BLOCK_IMM_LANES ||
//!    !imm8_lanes_enabled()` at the head, charging there, the split test removed) ->
//!    `imm8_lane_cap_counter_stays_zero_on_the_off_arm` [13: every fixture whose block reaches the
//!    cap now charges imm8 as well, which is the conflation the split exists to prevent]. The same
//!    mutant in `count_lane_for` -> `count_lane_cap_counter_stays_zero_on_the_off_arm` [13], and in
//!    `disp_lane_for` -> `disp_lane_cap_counter_stays_zero_on_the_off_arm` [13].
//! 6. Charge on the KNOB arm instead of the cap arm in `imm8_lane_for` (charge where
//!    `!imm8_lanes_enabled()` refuses) -> `imm8_lane_cap_counter_stays_zero_on_the_off_arm` [3].
//! 7. Charge the imm family where `!lane_family_enabled()` refuses the widened shape ->
//!    `imm_lane_cap_counter_stays_zero_on_the_lane_family_off_arm` [1].
//!
//! ## The cap sits under its family's shape bars, per family
//!
//! 8. Hoist the cap test above the shape bars, directly below the knob (below the family arm, for
//!    `imm_lane_for`). In `imm_lane_for` -> `imm8_lane_cap_refusal_is_charged_to_the_imm8_family`
//!    [10]; in `imm8_lane_for` -> `imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot` [12]; in
//!    `count_lane_for` -> `imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot` [12]; in
//!    `disp_lane_for` -> `imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot` [12]. In every
//!    case the "every other counter is zero" half of the assertions is what dies, which is the
//!    per-family split.
//!
//! ## The cap sits on the RIGHT SIDE of the page guard, per family
//!
//! The two directions are pinned separately because they disagree by family, and the pair is what
//! makes the ordering a decision rather than an accident.
//!
//! 9. Sink the cap test in `disp_lane_for` ABOVE the `has_record_range` heat gate ->
//!    `an_ungated_disp_slot_charges_no_cap_refusal` [2].
//! 10. Hoist the cap test in `disp_lane_for` above its `direct_host_bytes` call, so it sits where
//!     the other three sit ->
//!     `a_disp_slot_whose_lane_bytes_are_not_direct_mapped_charges_no_cap_refusal` [1].
//! 11. Sink the cap test BELOW `direct_host_bytes` in `imm_lane_for` (the tightening the disp
//!     family takes, at the price of the fetch-cache scan on every slot of a capped block) ->
//!     `a_capped_slot_whose_lane_bytes_are_not_direct_mapped_still_charges_its_family` [1]. The
//!     same mutant in `imm8_lane_for` [1] and in `count_lane_for` [1], each killed by that one
//!     fixture and nothing else.
//!
//! ## The denominator is installed blocks
//!
//! 12. Fold at the END OF THE WALK instead of at install (charge `cpu.jit_direct` where
//!     `CompileOutcome::Compiled` is built, and drop the fold in `JitState::install`), which is
//!     what the first revision of this slice did through the matchers ->
//!     `a_walk_that_does_not_install_charges_nothing` [7].
//! 13. Fold in `JitState::install` BEFORE the inner install rather than after its `?` ->
//!     `a_walk_that_does_not_install_charges_nothing` [1], through the forged install failure at
//!     the end of that fixture. This mutant SURVIVED the first round of this file and the forged
//!     failure was added for it: every other block here installs successfully, so without it the
//!     failed-install arm was uncovered and the fold could have moved to the wrong side of the
//!     `?` unnoticed.
//! 14. Sum the four cells into `imm_lane_cap_refusals` in `note_lane_cap_refusals`, dropping the
//!     per-family split at the fold instead of at the matcher ->
//!     `imm8_lane_cap_refusal_is_charged_to_the_imm8_family` [4].

use super::*;

/// Block entry.
const ENTRY: u32 = 0x500;

/// `add ebp, imm32`, the `imm_lane_for` shape and the block's lane-budget filler. Register-only,
/// six bytes, immediate at offset 2. `/0 ADD` is the narrow `IZARRAVM_LANE_FAMILY` arm's whole
/// admission set, so the filler lanes on both arms of that knob.
const IMM_SLOT: [u8; 6] = [0x81, 0xc5, 0x11, 0x22, 0x33, 0x44];

/// `sub ebp, imm32`, the `0x81 /5` shape the 2026-08-08 widening added. Same kind and same lane
/// address as `IMM_SLOT`, and refused outright on the narrow `IZARRAVM_LANE_FAMILY` arm, which is
/// what makes it the imm family's off-arm subject.
const IMM_WIDE_SLOT: [u8; 6] = [0x81, 0xed, 0x11, 0x22, 0x33, 0x44];

/// `add al, imm8`, the `imm8_lane_for` shape. Three bytes, immediate at offset 2.
const IMM8_SLOT: [u8; 3] = [0x80, 0xc0, 0x7f];

/// `shl eax, imm8`, the `count_lane_for` shape. The `0xC1 /4` dword shift rather than a `/0` ROL,
/// so the fixture carries no dependency on `IZARRAVM_ROTATE_ROWS`, which has no test override.
const COUNT_SLOT: [u8; 3] = [0xc1, 0xe0, 0x03];

/// `mov bl, [0x2000]`, the `disp_lane_for` shape. Six bytes, disp32 at offset 2, which is where
/// the fixture seeds the patch history the heat gate demands.
const DISP_SLOT: [u8; 6] = [0x8a, 0x1d, 0x00, 0x20, 0x00, 0x00];

/// The shared budget, spelled here so a fixture that stops matching the constant fails loudly
/// rather than silently testing a block that never reaches the cap.
const LANES: usize = jit::direct::MAX_BLOCK_IMM_LANES;

/// The SIX counters in the order this file always reports them: imm, imm8, count, disp, and the
/// two DEFAULT-OFF Option D arms (`disp_store`, `disp_load_widen`). The last two are asserted
/// zero on every fixture here for the same reason the other four are asserted on every fixture
/// that is not their own: a cap test hoisted above a family's shape bars would move a counter
/// that has no business moving, and the zeros are what catches it. Their own fixtures live in
/// `cpu_jit_disp_store_lane_test.rs`.
type CapRefusals = [u64; jit::direct::LANE_CAP_FAMILIES];

/// All six overridable lane arms, restored on drop. `Drop` rather than a trailing statement for
/// `cpu_jit_count_lane_test`'s reason: an assertion failure is the normal way a fixture here ends
/// when something is wrong, and a panic skips trailing statements.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_lane_family_for_test(None);
        jit::direct::set_imm8_lanes_for_test(None);
        jit::direct::set_count_lanes_for_test(None);
        jit::direct::set_disp_lanes_for_test(None);
        jit::direct::set_disp_store_lanes_for_test(None);
        jit::direct::set_disp_load_widen_for_test(None);
    }
}

/// State every arm this file can force. Bind the result for the fixture's lifetime.
///
/// The two Option D arms are pinned OFF here rather than exposed as parameters: no fixture in
/// this file has a `0x89` / `0x88` / `0x8B` slot, so their only job is to make the two trailing
/// zeros in every `CapRefusals` assertion mean "the arm was off", per the STATE-THE-ARM rule,
/// instead of "the ambient default happened to be off today".
///
/// SINCE THE 2026-08-23 FLIP the store pin runs AGAINST the shipped default rather than with it,
/// which makes it load-bearing rather than belt-and-braces: without it these fixtures would read
/// the ON arm, and a future `0x89`-shaped filler or tail would start charging
/// `disp_store_lane_cap_refusals` while every assertion here still expected a zero.
#[must_use]
fn force_arms(family: bool, imm8: bool, count: bool, disp: bool) -> ArmOverride {
    jit::direct::set_lane_family_for_test(Some(family));
    jit::direct::set_imm8_lanes_for_test(Some(imm8));
    jit::direct::set_count_lanes_for_test(Some(count));
    jit::direct::set_disp_lanes_for_test(Some(disp));
    jit::direct::set_disp_store_lanes_for_test(Some(false));
    jit::direct::set_disp_load_widen_for_test(Some(false));
    ArmOverride
}

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_mode(GswMode::Gsw486);
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

fn test_bus(memory: Vec<u8>) -> TestBus {
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus
}

/// Identity-map the image into the fast map, which a memory-bearing block needs before the
/// compile walk will answer anything but `Retry`. Harmless on the register-only fixtures.
fn map_flat_pages(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for page in (0..0x5000u32).step_by(0x1000) {
        let read = bus
            .direct_page(page, BusAccessKind::DataRead)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_read(
            page,
            page,
            read,
            jit::fast_map::PagePermissions::UNPAGED,
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
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(page)
        ));
    }
}

/// A block of `fillers` copies of `IMM_SLOT` followed by `tail`. Returns the image and the linear
/// start of every instruction the walk will slot.
///
/// The trailing `0xF4` is a STOPPER, not a slot. `decode_at` primes exactly the starts returned
/// here, so the walk meets a decode miss at that byte and ends there with
/// `CompileStop::Retry(DecodeMiss)`. That is a clean end rather than a refusal: the walk's
/// minimum-length rule only turns a `Retry` stop into a rejected block when fewer than three slots
/// were formed, and every image here carries twelve or thirteen. The byte is there so the miss
/// lands on something deliberate rather than on whatever `HLT` would have decoded to if a later
/// change ever primed it.
fn image(fillers: usize, tail: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut code = Vec::new();
    let mut starts = Vec::new();
    for _ in 0..fillers {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(&IMM_SLOT);
    }
    if !tail.is_empty() {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(tail);
    }
    code.push(0xf4);
    let mut memory = vec![0u8; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    (memory, starts)
}

/// Prime the decode cache for every slot the walk will visit. The walk answers `Retry` on a
/// decode miss, so a fixture that skipped this would never reach a lane matcher at all.
fn decode_at(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).unwrap();
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// Seed a heat RECORD for the four bytes at `lane`, the way one real patch of a decoded
/// instruction leaves one. `disp_lane_for` admits nothing without it, which is the doom cut.
fn seed_patch_history(cpu: &mut CpuGsw, lane: u32) {
    cpu.sync_smc_heat();
    cpu.jit_direct.smc_heat.bump(lane, 4, 0);
}

/// The census tally: what a run has charged across every block it INSTALLED.
fn cap_refusals(cpu: &CpuGsw) -> CapRefusals {
    let stalls = cpu.direct_stall_snapshot();
    [
        stalls.imm_lane_cap_refusals,
        stalls.imm8_lane_cap_refusals,
        stalls.count_lane_cap_refusals,
        stalls.disp_lane_cap_refusals,
        stalls.disp_store_lane_cap_refusals,
        stalls.disp_load_widen_lane_cap_refusals,
    ]
}

/// What ONE walk recorded, before anything decides whether its block installs.
fn walk_refusals(compilation: &jit::direct::Compilation) -> CapRefusals {
    compilation.lane_cap_refusals().map(u64::from)
}

/// Everything a fixture needs to compile the `fillers` + `tail` image once. The bus is returned
/// so it outlives the compilation: the lanes hold host pointers into its memory.
fn fixture(fillers: usize, tail: &[u8], seed_disp: bool) -> (CpuGsw, TestBus, Vec<u32>) {
    let (memory, starts) = image(fillers, tail);
    let mut cpu = flat_cpu();
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &starts);
    if seed_disp {
        let tail_start = *starts.last().expect("a seeded fixture has a tail slot");
        seed_patch_history(&mut cpu, tail_start + 2);
    }
    assert_eq!(
        cap_refusals(&cpu),
        [0, 0, 0, 0, 0, 0],
        "the fixture charged a cap refusal before it compiled anything"
    );
    // `install` only accepts a key the cache has already SEEN, so every fixture here has to walk
    // the production order: probe, compile, install. Without the probe the install returns `None`
    // and the tally would read zero for a reason that has nothing to do with the counters.
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("the fixture entry has a block key");
    assert!(
        matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ),
        "the first probe of a fresh cache must be the one that marks the key Seen"
    );
    (cpu, bus, starts)
}

/// Compile the block at `ENTRY` and check it has the shape the fixture built.
fn compile_checked(cpu: &mut CpuGsw, starts: &[u32]) -> jit::direct::Compilation {
    let compilation = jit::direct::compile(cpu, ENTRY, true).expect("fixture block compiles");
    assert_eq!(
        compilation.span.instructions as usize,
        starts.len(),
        "fixture block shape changed; the walk did not slot every instruction the fixture built"
    );
    compilation
}

/// Compile one block of `fillers` imm slots plus `tail`, INSTALL it, and report the four cap
/// counters and the lanes the block actually took. `seed_disp` names the tail slot as a `0x8A`
/// load whose patch history has to exist before the heat gate will look at it.
///
/// The install is not decoration. The counters live on the walk and reach the tally only through
/// `JitState::install`, so a helper that stopped at `compile` would read four zeros for every
/// fixture in this file. It asserts the pre-install tally on the way through, which is the cheap
/// half of the denominator claim; `a_walk_that_does_not_install_charges_nothing` is the half that
/// states it on its own.
fn compile_block(fillers: usize, tail: &[u8], seed_disp: bool) -> (CapRefusals, usize) {
    let (mut cpu, _bus, starts) = fixture(fillers, tail, seed_disp);
    let compilation = compile_checked(&mut cpu, &starts);
    let lanes = compilation.imm_lane_count();
    let walked = walk_refusals(&compilation);
    assert_eq!(
        cap_refusals(&cpu),
        [0, 0, 0, 0, 0, 0],
        "the walk charged the tally before its block installed"
    );
    cpu.jit_direct
        .install(&compilation)
        .expect("the fixture block installs");
    let tally = cap_refusals(&cpu);
    assert_eq!(
        tally, walked,
        "one installed block must charge the tally exactly what its own walk recorded"
    );
    (tally, lanes)
}

/// The `0x81` family on its own: thirteen laneable slots, twelve lanes, one refusal, and it is
/// charged to the imm family and to nothing else.
#[test]
fn imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES + 1, &[], false);
    assert_eq!(
        lanes, LANES,
        "the budget did not bind on the thirteenth slot"
    );
    assert_eq!(
        refusals,
        [1, 0, 0, 0, 0, 0],
        "the thirteenth imm slot must charge the imm family exactly once"
    );
}

/// Twelve laneable slots spend the budget exactly and charge nothing. The control for every
/// fixture above and below: without it, a counter that fired on every slot would still pass them.
#[test]
fn a_block_that_fits_the_budget_charges_no_refusal() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &[], false);
    assert_eq!(lanes, LANES, "the fixture did not fill the budget");
    assert_eq!(
        refusals,
        [0, 0, 0, 0, 0, 0],
        "a block that fits the budget must charge no family"
    );
}

/// The split arms, imm side. `IZARRAVM_LANE_FAMILY` off refuses the widened `0x81 /5` tail before
/// the cap is consulted, and the twelve `/0` fillers still spend the budget because they are the
/// narrow arm's own admission set. A fused `cap || !knob` refusal would charge the imm counter
/// here; so would counting on the knob arm.
#[test]
fn imm_lane_cap_counter_stays_zero_on_the_lane_family_off_arm() {
    let _arms = force_arms(false, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &IMM_WIDE_SLOT, false);
    assert_eq!(
        lanes, LANES,
        "the `/0` fillers must still spend the budget with the family arm narrow"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The same image on the ON arm, which is what makes the fixture above a statement about the knob
/// rather than about `0x81 /5` being unlaneable.
#[test]
fn the_widened_imm_tail_charges_the_imm_family_on_the_lane_family_on_arm() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &IMM_WIDE_SLOT, false);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [1, 0, 0, 0, 0, 0],
        "a capped `0x81 /5` slot belongs to the imm family alone"
    );
}

/// The budget is spent by imm slots and the thirteenth slot is a `0x80 /r`: the refusal is the
/// imm8 family's, not the imm family's.
#[test]
fn imm8_lane_cap_refusal_is_charged_to_the_imm8_family() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &IMM8_SLOT, false);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [0, 1, 0, 0, 0, 0],
        "a capped `0x80` slot belongs to the imm8 family alone"
    );
}

/// The split arms, imm8 side. `lanes_used` still reaches the cap on this image because the twelve
/// fillers are imm lanes, so a fused `cap || !knob` refusal would charge the imm8 counter here.
#[test]
fn imm8_lane_cap_counter_stays_zero_on_the_off_arm() {
    let _arms = force_arms(true, false, true, true);
    let (refusals, lanes) = compile_block(LANES, &IMM8_SLOT, false);
    assert_eq!(
        lanes, LANES,
        "the fillers must still spend the budget with the imm8 arm off"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The group-2 count family, same shape of claim.
#[test]
fn count_lane_cap_refusal_is_charged_to_the_count_family() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &COUNT_SLOT, false);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [0, 0, 1, 0, 0, 0],
        "a capped `0xC1` slot belongs to the count family alone"
    );
}

/// The split arms, count side.
#[test]
fn count_lane_cap_counter_stays_zero_on_the_off_arm() {
    let _arms = force_arms(true, true, false, true);
    let (refusals, lanes) = compile_block(LANES, &COUNT_SLOT, false);
    assert_eq!(
        lanes, LANES,
        "the fillers must still spend the budget with the count arm off"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The displacement family. The tail slot carries seeded patch history, so it clears the heat gate
/// and the cap is genuinely the only bar left.
#[test]
fn disp_lane_cap_refusal_is_charged_to_the_disp_family() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &DISP_SLOT, true);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [0, 0, 0, 1, 0, 0],
        "a capped `0x8A` slot belongs to the disp family alone"
    );
}

/// The split arms, disp side.
#[test]
fn disp_lane_cap_counter_stays_zero_on_the_off_arm() {
    let _arms = force_arms(true, true, true, false);
    let (refusals, lanes) = compile_block(LANES, &DISP_SLOT, true);
    assert_eq!(
        lanes, LANES,
        "the fillers must still spend the budget with the disp arm off"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The disp heat gate sits ABOVE the cap test, and this is what says so. Same capped block, same
/// arm, but no seeded patch history: `disp_lane_for` refuses on the gate and the budget is never
/// consulted, so a never-patched load population cannot be reported as budget pressure. That
/// distinction is the whole reason the cap is the last bar in this one family.
#[test]
fn an_ungated_disp_slot_charges_no_cap_refusal() {
    let _arms = force_arms(true, true, true, true);
    let (refusals, lanes) = compile_block(LANES, &DISP_SLOT, false);
    assert_eq!(lanes, LANES, "the fillers must still spend the budget");
    assert_eq!(
        refusals,
        [0, 0, 0, 0, 0, 0],
        "a load with no patch history was never a lane the budget cost"
    );
}

/// Cut the fetch-cache entry covering `page` short at `len` bytes, so `direct_host_bytes` answers
/// for every lane wholly below that offset and `None` for one that reaches past it.
///
/// The clip rather than `fetch_page.invalidate()`, which the other lane files use: dropping every
/// entry would refuse the twelve FILLERS' lanes too, the budget would never be spent, and the
/// fixture would read four zeros without the cap having been reached at all, which is a
/// fixture that cannot fail.
fn clip_fetch_entry(cpu: &mut CpuGsw, page: u32, len: usize) {
    let mut clipped = 0usize;
    for entry in &mut cpu.fetch_page.entries {
        if entry.valid && entry.physical_page == page {
            entry.len = len;
            clipped += 1;
        }
    }
    assert_eq!(
        clipped, 1,
        "the fixture must clip exactly the one fetch entry covering the code page"
    );
}

/// The disp family's PAGE GUARD sits above its cap test, and this is what says so. It is the one
/// family where that ordering holds: `imm_lane_for`, `imm8_lane_for` and `count_lane_for` all test
/// the cap before `direct_host_bytes` so a capped block stops paying the fetch-cache scan, while
/// `disp_lane_for` keeps the cap under both the heat gate (required) and the scan (because the
/// gate above it is the expensive bar, so reordering the cheap one buys nothing).
///
/// The block is the capped disp image with its patch history seeded, so the heat gate passes and
/// the ONLY thing standing between the tail slot and the cap is the page guard. The fetch entry is
/// clipped exactly at the end of the last filler's lane: twelve lanes still register, the tail's
/// four disp bytes reach one byte past the clip, and the family charges nothing.
#[test]
fn a_disp_slot_whose_lane_bytes_are_not_direct_mapped_charges_no_cap_refusal() {
    let _arms = force_arms(true, true, true, true);
    let (mut cpu, _bus, starts) = fixture(LANES, &DISP_SLOT, true);
    let tail_start = *starts.last().expect("the image has a tail slot");
    let filler_lane_end = tail_start as usize;
    let tail_lane_end = tail_start as usize + 2 + 4;
    assert!(
        filler_lane_end < tail_lane_end,
        "the clip has to separate the fillers' lanes from the tail's"
    );
    clip_fetch_entry(&mut cpu, 0, filler_lane_end);

    let compilation = compile_checked(&mut cpu, &starts);
    assert_eq!(
        compilation.imm_lane_count(),
        LANES,
        "the twelve fillers must still lane, or the block never reaches the cap"
    );
    cpu.jit_direct
        .install(&compilation)
        .expect("the fixture block installs");
    assert_eq!(
        cap_refusals(&cpu),
        [0, 0, 0, 0, 0, 0],
        "a disp slot whose lane bytes no direct page covers was never a lane the budget cost"
    );
}

/// A walk that never installs charges nothing, and the walk itself still knows what it refused.
///
/// This is the denominator claim stated on its own. The four counters are read beside the lane
/// REGISTRATIONS, which `run.rs` charges on the success arm of `install`; a refusal counter
/// charged from inside the matcher would count walks that end in a `Retry`, walks whose caller
/// discards the result, and every prefix the page-overflow recovery search tries, and the ratio
/// between the two would then be a ratio between different populations.
#[test]
fn a_walk_that_does_not_install_charges_nothing() {
    let _arms = force_arms(true, true, true, true);
    let (mut cpu, _bus, starts) = fixture(LANES + 1, &[], false);
    let compilation = compile_checked(&mut cpu, &starts);
    assert_eq!(
        walk_refusals(&compilation),
        [1, 0, 0, 0, 0, 0],
        "the walk must record the thirteenth slot's refusal on the compilation"
    );
    assert_eq!(
        cap_refusals(&cpu),
        [0, 0, 0, 0, 0, 0],
        "a compilation nobody installed must leave the tally alone"
    );
    // AND THE FAILED INSTALL, forged the way `failed_install_consumes_seen_without_a_code_watch`
    // forges it: an empty code buffer is one of the shapes `BlockCache::install` refuses outright.
    // This is the arm that separates "folded after the install succeeded" from "folded on the way
    // in"; without it, moving the fold above the inner install's `?` survives every fixture here,
    // because every other block in this file installs.
    let mut refused = compilation;
    refused.code.clear();
    assert!(
        cpu.jit_direct.install(&refused).is_none(),
        "an empty code buffer must be refused, or the fixture is not testing a failed install"
    );
    assert_eq!(
        cap_refusals(&cpu),
        [0, 0, 0, 0, 0, 0],
        "an install that failed is not an installed block and must charge nothing"
    );
}

/// The cap sits ABOVE the page guard in the imm, imm8 and count families, and this is what says
/// so. It is the mirror of the disp fixture above: same clip, same capped block, and the OPPOSITE
/// answer, because those three matchers test the budget before they scan the fetch-page cache and
/// `disp_lane_for` does not.
///
/// The counter therefore means "lane-shaped slots the budget turned away" for these three, not
/// "lanes a larger budget would certainly have taken": the thirteenth slot charges even though no
/// direct page could have supplied its bytes. That is the reading `DirectStallTally` states, and
/// sinking any of the three caps back under `direct_host_bytes` to tighten it (at the price of the
/// scan on every slot of a capped block) fails here.
#[test]
fn a_capped_slot_whose_lane_bytes_are_not_direct_mapped_still_charges_its_family() {
    let _arms = force_arms(true, true, true, true);
    for (family, tail, expected) in [
        ("imm", IMM_SLOT.as_slice(), [1, 0, 0, 0, 0, 0]),
        ("imm8", IMM8_SLOT.as_slice(), [0, 1, 0, 0, 0, 0]),
        ("count", COUNT_SLOT.as_slice(), [0, 0, 1, 0, 0, 0]),
    ] {
        let (mut cpu, _bus, starts) = fixture(LANES, tail, false);
        let tail_start = *starts.last().expect("the image has a tail slot");
        // Exactly at the end of the last filler's lane: twelve lanes still resolve, and the tail's
        // lane bytes reach past the clip whatever its width.
        clip_fetch_entry(&mut cpu, 0, tail_start as usize);

        let compilation = compile_checked(&mut cpu, &starts);
        assert_eq!(
            compilation.imm_lane_count(),
            LANES,
            "{family}: the twelve fillers must still lane, or the block never reaches the cap"
        );
        cpu.jit_direct
            .install(&compilation)
            .expect("the fixture block installs");
        assert_eq!(
            cap_refusals(&cpu),
            expected,
            "{family}: the cap is above the page guard, so a capped slot charges whether or not \
             its lane bytes are fetch-cached"
        );
    }
}

/// RE-WALKING the same bytes charges once, not once per walk.
///
/// The production path that does this is `compile_with_page_len`'s recovery search: a full-length
/// walk whose emission overran the arena page is followed by a binary search that re-walks
/// PREFIXES of the same entry, and every one of those walks meets the same capped slots. Only the
/// prefix that finally installs may charge.
///
/// The fixture re-walks directly rather than forcing a real page overflow, and the reason is
/// recorded rather than hidden: the walk carries its own byte budget (`EMITTED_BLOCK_FIXED_BYTES`
/// plus a per-slot estimate, checked from the fourth slot on), that estimate is deliberately
/// conservative, and `the_page_budget_ends_the_walk_before_the_recovery_search_runs` pins that it
/// keeps full-length walks inside the page. There is no `page_len` for these register-only shapes
/// that makes the estimate under-predict, so a fixture claiming to reach the overflow arm would be
/// claiming something the size model prevents. What the search does to the counters is exactly
/// what this does: N walks over one entry, one install.
#[test]
fn re_walking_the_same_bytes_charges_only_the_walk_that_installs() {
    let _arms = force_arms(true, true, true, true);
    let (mut cpu, _bus, starts) = fixture(LANES + 1, &[], false);
    // The discarded walks: two at full length, and the SHORTER prefix a recovery search would
    // also try, which refuses nothing because it never reaches the cap. Each walk's compilation
    // carries its own answer, which is what makes "the prefix that installs is the one that
    // counts" a statement with content.
    for (limit, expected) in [
        (starts.len(), [1, 0, 0, 0, 0, 0]),
        (LANES, [0, 0, 0, 0, 0, 0]),
        (starts.len(), [1, 0, 0, 0, 0, 0]),
    ] {
        let candidate =
            jit::direct::compile_with_instruction_limit_for_test(&mut cpu, ENTRY, true, limit)
                .expect("every prefix of this fixture compiles");
        assert_eq!(
            walk_refusals(&candidate),
            expected,
            "the {limit}-slot walk must record its OWN refusals on its OWN compilation"
        );
    }
    assert_eq!(
        cap_refusals(&cpu),
        [0, 0, 0, 0, 0, 0],
        "three discarded walks must have charged nothing"
    );
    let compilation = compile_checked(&mut cpu, &starts);
    cpu.jit_direct
        .install(&compilation)
        .expect("the fixture block installs");
    assert_eq!(
        cap_refusals(&cpu),
        [1, 0, 0, 0, 0, 0],
        "the refused slot must be counted once, by the walk whose block installed"
    );
}
