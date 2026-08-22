// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The per-family lane-BUDGET counters: `imm_lane_cap_refusals`, `imm8_lane_cap_refusals`,
//! `count_lane_cap_refusals` and `disp_lane_cap_refusals`.
//!
//! Four lane matchers draw on one `MAX_BLOCK_IMM_LANES` budget, and until these counters existed a
//! block that ran out of lanes and a family that had no lane-shaped slots produced the same
//! reading: registrations, and nothing else. That is the blindness this file pins. A widening
//! ladder that reads its registrations flat needs to know whether the family was unlaneable or
//! whether the budget turned it away, because the two point at completely different next steps.
//!
//! # The two things every fixture here pins
//!
//! **The cap arm is charged to ONE family.** Each matcher tests the cap LAST, after its shape,
//! prefix, page and (for `disp_lane_for`) patch-history bars, so the slot that trips the budget
//! has already been narrowed to one family. Every fixture therefore asserts all four counters,
//! not just its own: a cap test hoisted back to the head of the matchers would move all four on
//! the same slot and the three zeros are what catches it.
//!
//! **The cap arm is NOT the knob arm.** Three of the four matchers used to read
//! `lanes_used >= MAX_BLOCK_IMM_LANES || !<knob>_enabled()` as one disjunction, and a counter on
//! that fused refusal cannot tell budget pressure from an off arm, which makes it worthless on
//! exactly the A/B leg it would be read on. The knob is tested first and returns before the cap
//! is consulted, so an off arm reads zero. The off-arm fixtures below are what hold that.
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
//! force. The three knobs that DO have overrides are stated on every fixture, per `cpu_test.rs`'s
//! `DIRECT_BARRIER` rule: a fixture that read an ambient arm would pass for the wrong reason the
//! next time a default flips.
//!
//! # Mutation record
//!
//! Each of these was applied to `direct.rs` and the whole file re-run. Every one died; the
//! fixture named first is the one whose claim the mutant contradicts most directly, and the count
//! in brackets is how many of the nine fixtures failed in total.
//!
//! 1. Delete `note_imm_lane_cap_refusal` ->
//!    `imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot` [1].
//! 2. Delete `note_imm8_lane_cap_refusal` ->
//!    `imm8_lane_cap_refusal_is_charged_to_the_imm8_family` [1].
//! 3. Delete `note_count_lane_cap_refusal` ->
//!    `count_lane_cap_refusal_is_charged_to_the_count_family` [1].
//! 4. Delete `note_disp_lane_cap_refusal` ->
//!    `disp_lane_cap_refusal_is_charged_to_the_disp_family` [1].
//! 5. Fuse the arms back in `imm8_lane_for` (`lanes_used >= MAX_BLOCK_IMM_LANES ||
//!    !imm8_lanes_enabled()` at the head, counting there, tail check removed) ->
//!    `imm8_lane_cap_counter_stays_zero_on_the_off_arm` [7: every fixture whose block reaches the
//!    cap now charges imm8 as well, which is the conflation the split exists to prevent]. The
//!    same mutant in `count_lane_for` [7] and in `disp_lane_for` [7].
//! 6. Count on the KNOB arm instead of the cap arm in `imm8_lane_for` (increment where
//!    `!imm8_lanes_enabled()` refuses) -> `imm8_lane_cap_counter_stays_zero_on_the_off_arm` [1].
//! 7. Hoist the cap test back above the shape bars, below the knob. In `imm8_lane_for` ->
//!    `imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot` [6]; in `disp_lane_for` ->
//!    `imm8_lane_cap_refusal_is_charged_to_the_imm8_family` [6]. In both cases the "every other
//!    counter is zero" half of the assertions is what dies, which is the per-family split.

use super::*;

/// Block entry.
const ENTRY: u32 = 0x500;

/// `add ebp, imm32`, the `imm_lane_for` shape and the block's lane-budget filler. Register-only,
/// six bytes, immediate at offset 2.
const IMM_SLOT: [u8; 6] = [0x81, 0xc5, 0x11, 0x22, 0x33, 0x44];

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

/// The four counters in the order this file always reports them: imm, imm8, count, disp.
type CapRefusals = [u64; 4];

/// All three overridable lane arms, restored on drop. `Drop` rather than a trailing statement for
/// `cpu_jit_count_lane_test`'s reason: an assertion failure is the normal way a fixture here ends
/// when something is wrong, and a panic skips trailing statements.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_imm8_lanes_for_test(None);
        jit::direct::set_count_lanes_for_test(None);
        jit::direct::set_disp_lanes_for_test(None);
    }
}

/// State every arm this file can force. Bind the result for the fixture's lifetime.
#[must_use]
fn force_arms(imm8: bool, count: bool, disp: bool) -> ArmOverride {
    jit::direct::set_imm8_lanes_for_test(Some(imm8));
    jit::direct::set_count_lanes_for_test(Some(count));
    jit::direct::set_disp_lanes_for_test(Some(disp));
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

/// A block of `fillers` copies of `IMM_SLOT` followed by `tail`, terminated by a HLT boundary.
/// Returns the image and the linear start of every instruction the walk will slot.
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

fn cap_refusals(cpu: &CpuGsw) -> CapRefusals {
    let stalls = cpu.direct_stall_snapshot();
    [
        stalls.imm_lane_cap_refusals,
        stalls.imm8_lane_cap_refusals,
        stalls.count_lane_cap_refusals,
        stalls.disp_lane_cap_refusals,
    ]
}

/// Compile one block of `fillers` imm slots plus `tail`, and report the four cap counters and the
/// lanes the block actually took. `seed_disp` names the tail slot as a `0x8A` load whose patch
/// history has to exist before the heat gate will look at it.
fn compile_block(fillers: usize, tail: &[u8], seed_disp: bool) -> (CapRefusals, usize) {
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
        [0, 0, 0, 0],
        "the fixture charged a cap refusal before it compiled anything"
    );
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("fixture block compiles");
    assert_eq!(
        compilation.span.instructions as usize,
        starts.len(),
        "fixture block shape changed; the walk did not slot every instruction the fixture built"
    );
    let lanes = compilation.imm_lane_count();
    (cap_refusals(&cpu), lanes)
}

/// The `0x81` family on its own: thirteen laneable slots, twelve lanes, one refusal, and it is
/// charged to the imm family and to nothing else.
#[test]
fn imm_lane_cap_refusal_is_charged_at_the_thirteenth_slot() {
    let _arms = force_arms(true, true, true);
    let (refusals, lanes) = compile_block(LANES + 1, &[], false);
    assert_eq!(
        lanes, LANES,
        "the budget did not bind on the thirteenth slot"
    );
    assert_eq!(
        refusals,
        [1, 0, 0, 0],
        "the thirteenth imm slot must charge the imm family exactly once"
    );
}

/// Twelve laneable slots spend the budget exactly and charge nothing. The control for every
/// fixture above and below: without it, a counter that fired on every slot would still pass them.
#[test]
fn a_block_that_fits_the_budget_charges_no_refusal() {
    let _arms = force_arms(true, true, true);
    let (refusals, lanes) = compile_block(LANES, &[], false);
    assert_eq!(lanes, LANES, "the fixture did not fill the budget");
    assert_eq!(
        refusals,
        [0, 0, 0, 0],
        "a block that fits the budget must charge no family"
    );
}

/// The budget is spent by imm slots and the thirteenth slot is a `0x80 /r`: the refusal is the
/// imm8 family's, not the imm family's.
#[test]
fn imm8_lane_cap_refusal_is_charged_to_the_imm8_family() {
    let _arms = force_arms(true, true, true);
    let (refusals, lanes) = compile_block(LANES, &IMM8_SLOT, false);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [0, 1, 0, 0],
        "a capped `0x80` slot belongs to the imm8 family alone"
    );
}

/// The split arms, imm8 side. `lanes_used` still reaches the cap on this image because the twelve
/// fillers are imm lanes, so a fused `cap || !knob` refusal would charge the imm8 counter here.
#[test]
fn imm8_lane_cap_counter_stays_zero_on_the_off_arm() {
    let _arms = force_arms(false, true, true);
    let (refusals, lanes) = compile_block(LANES, &IMM8_SLOT, false);
    assert_eq!(
        lanes, LANES,
        "the fillers must still spend the budget with the imm8 arm off"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The group-2 count family, same shape of claim.
#[test]
fn count_lane_cap_refusal_is_charged_to_the_count_family() {
    let _arms = force_arms(true, true, true);
    let (refusals, lanes) = compile_block(LANES, &COUNT_SLOT, false);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [0, 0, 1, 0],
        "a capped `0xC1` slot belongs to the count family alone"
    );
}

/// The split arms, count side.
#[test]
fn count_lane_cap_counter_stays_zero_on_the_off_arm() {
    let _arms = force_arms(true, false, true);
    let (refusals, lanes) = compile_block(LANES, &COUNT_SLOT, false);
    assert_eq!(
        lanes, LANES,
        "the fillers must still spend the budget with the count arm off"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The displacement family. The tail slot carries seeded patch history, so it clears the heat gate
/// and the cap is genuinely the only bar left.
#[test]
fn disp_lane_cap_refusal_is_charged_to_the_disp_family() {
    let _arms = force_arms(true, true, true);
    let (refusals, lanes) = compile_block(LANES, &DISP_SLOT, true);
    assert_eq!(lanes, LANES, "the budget did not bind on the tail slot");
    assert_eq!(
        refusals,
        [0, 0, 0, 1],
        "a capped `0x8A` slot belongs to the disp family alone"
    );
}

/// The split arms, disp side.
#[test]
fn disp_lane_cap_counter_stays_zero_on_the_off_arm() {
    let _arms = force_arms(true, true, false);
    let (refusals, lanes) = compile_block(LANES, &DISP_SLOT, true);
    assert_eq!(
        lanes, LANES,
        "the fillers must still spend the budget with the disp arm off"
    );
    assert_eq!(
        refusals,
        [0, 0, 0, 0],
        "an off knob is not budget pressure and must charge nothing"
    );
}

/// The disp heat gate sits ABOVE the cap test, and this is what says so. Same capped block, same
/// arm, but no seeded patch history: `disp_lane_for` refuses on the gate and the budget is never
/// consulted, so a never-patched load population cannot be reported as budget pressure. That
/// distinction is the whole reason the cap is the last bar rather than the first.
#[test]
fn an_ungated_disp_slot_charges_no_cap_refusal() {
    let _arms = force_arms(true, true, true);
    let (refusals, lanes) = compile_block(LANES, &DISP_SLOT, false);
    assert_eq!(lanes, LANES, "the fillers must still spend the budget");
    assert_eq!(
        refusals,
        [0, 0, 0, 0],
        "a load with no patch history was never a lane the budget cost"
    );
}
