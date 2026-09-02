// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Gate N of `dev_docs/sticky-decline-memo-design.md`: the sticky-decline memo's non-vacuity
//! battery.
//!
//! The memo short-circuits the admission chain for a decline it can prove repeatable, so the only
//! thing standing between it and a wrong answer is its guard: the pack must still be live for the
//! same `(linear, d)`, the era stamp must still read what the memo was written under, and the two
//! uncovered `jit_mode_key` bits must still match. Every fixture below therefore has to be able to
//! FAIL when one of those terms is removed — the recorded lesson is six fixtures that passed while
//! proving nothing, and the design's own Gate N test 1 was one of them (review N5: it asserted
//! `BlockCacheStats` deltas that a Dormant probe never moves on `main` either).
//!
//! What makes them able to fail, term by term:
//!
//! | fixture | guard under test | what it does if the guard is deleted |
//! |---|---|---|
//! | `a_block_installs_at_a_memoized_address_once_the_era_advances` | the whole era term | the stale memo answers every ask, `key_for_phys` never runs, and no block is ever installed |
//! | `an_epoch_change_kills_a_live_memo` | the epoch half (review M3, both directions) | the memo keeps answering after the lift becomes possible |
//! | `a_block_cache_clear_kills_a_live_memo` | the `heat_resets` half (review B1) | the memo answers for a key `entries` no longer holds |
//! | `flipping_ss_d_or_v86_misses_the_memo` | the two mode bits | a V86 or SS.D flip reads another mode's verdict |
//! | `the_era_stamp_wrap_sweeps_every_memo` | the 63-wrap sweep | a pack still carrying stamp 1 aliases back into life |
//! | `a_guest_write_to_the_entry_byte_retires_the_slot` | the decode-line lifecycle (§2.3) | the memo outlives the Dormant entry a guest write dropped |
//! | `the_census_arm_is_byte_identical_with_and_without_the_memo` | counter identity (§3) | the census closure breaks, in either direction |

use super::*;

use crate::jit::direct::SMC_HEAT_EPOCH_SHIFT;

/// The loop entry, and the address every fixture memoises.
const ENTRY: u32 = 0x105;

/// Sixty-four iterations of a three-instruction loop. The count matters: the admission heat gate
/// has to be crossed, the block has to be parked Dormant, the memo has to be written on the
/// decline after that, and only then can a hit happen. Five iterations (the shape the sibling
/// direct battery uses) leaves no room for the last two.
fn loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x110].copy_from_slice(&[
        0xb9, 0x40, 0x00, 0x00, 0x00, // 0x100 mov ecx,0x40
        0x83, 0xc0, 0x03, // 0x105 add eax,3      <- ENTRY, the jnz target
        0x89, 0xc2, // 0x108 mov edx,eax
        0x83, 0xe9, 0x01, // 0x10a sub ecx,1
        0x75, 0xf6, // 0x10d jnz 0x105
        0xf4, // 0x10f hlt
    ]);
    memory
}

fn fresh() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.registers.eip = 0x100;
    cpu.set_jit_auto_admit(true);
    cpu
}

fn drive(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..256 {
        if cpu.run_straight_line(bus, u64::MAX).unwrap().halted {
            cpu.halted = false;
            return;
        }
    }
    panic!("guest did not halt");
}

/// A CPU that has run the loop to completion with `direct_pages_enabled` LEFT OFF, so
/// `code_page_covers_block` parked the entry Dormant and the declines behind it wrote a memo.
///
/// Returns the CPU, its bus, and the decode slot the memo landed in.
fn cpu_with_a_live_memo() -> (CpuGsw, TestBus, u32) {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(loop_program());
    drive(&mut cpu, &mut bus);

    let stalls = cpu.direct_stall_snapshot();
    assert!(
        stalls.decline_memo_hits > 0,
        "the memo never answered, so every fixture built on this helper is vacuous"
    );
    // The memo must not be answering the FIRST ask at this slot: if it were, the write site would
    // be running before the full chain had ever produced the verdict it replays.
    assert!(
        cpu.perf_counters().jit_direct_dispatch_declines > stalls.decline_memo_hits,
        "full-chain declines must outnumber memo hits, or the memo is answering its own premise"
    );
    assert_eq!(
        cpu.jit_direct.len(),
        0,
        "no block may install while direct pages are off, or the entry is not Dormant"
    );

    let slot = live_slot(&cpu, ENTRY);
    assert_ne!(
        cpu.decode_cache.decline_memo_at(slot),
        0,
        "the entry slot must carry the memo the fixtures are about to attack"
    );
    assert!(
        cpu.decline_memo_hit(slot),
        "the memo must answer at rest, or every 'it stops answering' assertion below is vacuous"
    );
    (cpu, bus, slot)
}

fn live_slot(cpu: &CpuGsw, lin: u32) -> u32 {
    cpu.decode_cache
        .get_packed(lin, true)
        .unwrap_or_else(|| panic!("no live decode line at {lin:#x}"))
        .slot
}

/// Push the retired-instruction clock into the next heat epoch. The epoch is
/// `perf.instructions >> SMC_HEAT_EPOCH_SHIFT`, and it is the term that says "the dormant lift
/// may now succeed".
fn cross_an_epoch(cpu: &mut CpuGsw) {
    cpu.perf.instructions += 1 << SMC_HEAT_EPOCH_SHIFT;
}

/// Write a memo at `slot` exactly the way the production write site does — advance the era FIRST,
/// because an advance that wraps sweeps the whole array and a sweep after the store would erase
/// what the store just earned — and prove it answers before the fixture attacks it.
fn arm(cpu: &mut CpuGsw, slot: u32) {
    let _ = cpu.advance_decline_memo_era();
    let live = cpu.decline_memo_comparand();
    cpu.decode_cache.set_decline_memo_at(slot, live);
    assert!(
        cpu.decline_memo_hit(slot),
        "a freshly armed memo must answer, or the assertion that follows is vacuous"
    );
}

// ---------------------------------------------------------------------------------------
// Gate N test 1, rewritten per review N5.
// ---------------------------------------------------------------------------------------

/// The era guard, end to end, on the transition that makes it BLOCKING rather than tidy.
///
/// `BlockCache::clear` empties `entries` without touching the decode cache, so every Dormant key
/// vanishes while its pack — and its memo — survives. A memo that outlived that clear would keep
/// replaying `DormantProbe` for a key whose true next verdict is `BlockProbe::Interpret` with a
/// `Seen` insert, and the address would never be admitted again until the next epoch rolled.
///
/// This is what review B1 named as the design's blocker and what folding `heat_resets` into the
/// era term fixes. The assertion is the strongest one available: with direct pages now enabled the
/// address is COMPILABLE, so if the memo still answers, `key_for_phys` is never reached, no block
/// is installed, and no native entry is ever taken. Delete the `heat_resets` term from
/// `advance_decline_memo_era` and this fixture fails on `jit_direct.len()`.
#[test]
fn a_block_installs_at_a_memoized_address_once_the_era_advances() {
    let (mut cpu, mut bus, slot) = cpu_with_a_live_memo();
    let advances_before = cpu.direct_stall_snapshot().decline_memo_advances;

    // The same address is lowerable now; only the memo stands between it and admission.
    bus.direct_pages_enabled = true;
    cpu.jit_direct.clear();
    assert!(
        cpu.decode_cache.get_packed(ENTRY, true).is_some(),
        "the clear must leave the decode line — and therefore the memo — alive, \
         or this fixture is not testing the transition it claims to"
    );
    assert_ne!(
        cpu.decode_cache.decline_memo_at(slot),
        0,
        "the memo byte must survive the block-cache clear; that is the whole hazard"
    );

    cpu.registers.eip = 0x100;
    cpu.registers.set_eax(0);
    drive(&mut cpu, &mut bus);

    assert!(
        cpu.jit_direct.len() > 0,
        "a compilable address behind a stale memo must still be admitted"
    );
    assert!(
        cpu.perf_counters().jit_direct_entries > 0,
        "the installed block must actually be entered, or 'installed' proves nothing"
    );
    assert!(
        cpu.direct_stall_snapshot().decline_memo_advances > advances_before,
        "the era must have advanced; if it did not, the block installed for some other reason"
    );
}

// ---------------------------------------------------------------------------------------
// The era term, half by half.
// ---------------------------------------------------------------------------------------

/// The epoch half, tested in BOTH directions (review M3). `reset_perf_counters` restarts the
/// instruction count, so epoch numbers repeat; the guard is an inequality precisely so a backward
/// jump kills a memo exactly as a forward one does. A `>` comparison would pass the first half of
/// this fixture and fail the second.
#[test]
fn an_epoch_change_kills_a_live_memo() {
    let (mut cpu, _bus, slot) = cpu_with_a_live_memo();
    let advances = cpu.direct_stall_snapshot().decline_memo_advances;

    cross_an_epoch(&mut cpu);
    assert!(
        !cpu.decline_memo_hit(slot),
        "a new heat epoch must kill the memo: this is the epoch in which the dormant lift can \
         start succeeding"
    );
    assert_eq!(
        cpu.direct_stall_snapshot().decline_memo_advances,
        advances + 1,
        "exactly one era advance"
    );
    // The stamp moved, so the pack's byte is stale for good — not merely suppressed for one ask.
    assert!(
        !cpu.decline_memo_hit(slot),
        "the memo must stay dead after the advance, not just miss the ask that advanced"
    );

    // CONTROL: a still era must neither advance nor kill. Without this the fixture would pass on
    // an implementation that advanced on every single ask.
    arm(&mut cpu, slot);
    let advances = cpu.direct_stall_snapshot().decline_memo_advances;
    assert!(cpu.decline_memo_hit(slot));
    assert_eq!(
        cpu.direct_stall_snapshot().decline_memo_advances,
        advances,
        "an unchanged era term must produce no advance"
    );

    // Backwards, the way `reset_perf_counters` moves it (review M3). A `>` comparison would pass
    // everything above and fail here.
    cpu.perf.instructions = 40 << SMC_HEAT_EPOCH_SHIFT;
    arm(&mut cpu, slot);
    cpu.perf.instructions = 3 << SMC_HEAT_EPOCH_SHIFT;
    assert!(
        !cpu.decline_memo_hit(slot),
        "a BACKWARD epoch move must kill the memo too; the guard is an inequality, not a >"
    );
}

/// The `heat_resets` half (review B1), at the level of the guard itself. `BlockCache::clear` and
/// `reset_storage` have no site a `JitState` field could be advanced at, which is why the era term
/// reads a counter they both bump rather than trying to hook them.
#[test]
fn a_block_cache_clear_kills_a_live_memo() {
    let (mut cpu, _bus, slot) = cpu_with_a_live_memo();
    let advances = cpu.direct_stall_snapshot().decline_memo_advances;
    let epoch_before = cpu.perf.instructions >> SMC_HEAT_EPOCH_SHIFT;

    cpu.jit_direct.clear();

    assert_eq!(
        cpu.perf.instructions >> SMC_HEAT_EPOCH_SHIFT,
        epoch_before,
        "the epoch must NOT have moved, or this fixture is retesting the epoch half"
    );
    assert!(
        !cpu.decline_memo_hit(slot),
        "a block-cache clear empties `entries`, so every Dormant verdict the memo replays is gone"
    );
    assert_eq!(
        cpu.direct_stall_snapshot().decline_memo_advances,
        advances + 1
    );
}

// ---------------------------------------------------------------------------------------
// The two mode bits.
// ---------------------------------------------------------------------------------------

/// SS.D and V86 are the only two `jit_mode_key` inputs no existing invalidation covers, which is
/// why they are spent as two of the memo byte's eight bits. V86 is the load-bearing one: V86
/// forces CS.D = 0, so a V86 key and a non-V86 16-bit key at one linear address share one decode
/// line and one pack, and `PACK_FLAG_D` cannot separate them.
#[test]
fn flipping_ss_d_or_v86_misses_the_memo() {
    let (mut cpu, _bus, slot) = cpu_with_a_live_memo();

    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    let original_ss_d = ss.default_size_32;
    ss.default_size_32 = !original_ss_d;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    assert!(
        !cpu.decline_memo_hit(slot),
        "an SS.D flip changes `jit_mode_key` and nothing else notices; the memo must"
    );
    ss.default_size_32 = original_ss_d;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    assert!(
        cpu.decline_memo_hit(slot),
        "restoring SS.D must restore the hit, or the miss above was some other term"
    );

    let cr0 = cpu.control.cr0;
    let eflags = cpu.registers.eflags;
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags |= FLAG_VM;
    assert!(cpu.is_v86_mode(), "the fixture must actually reach V86");
    assert!(
        !cpu.decline_memo_hit(slot),
        "entering V86 changes `jit_mode_key` bit 2 without touching the decode cache"
    );
    cpu.control.cr0 = cr0;
    cpu.registers.eflags = eflags;
    assert!(
        cpu.decline_memo_hit(slot),
        "leaving V86 must restore the hit, or the miss above was some other term"
    );
}

// ---------------------------------------------------------------------------------------
// The 63-wrap sweep.
// ---------------------------------------------------------------------------------------

/// The stamp is six bits with 0 reserved, so it wraps 63 -> 1. Without the sweep, a pack that has
/// carried stamp 1 through sixty-three advances would answer again the moment the counter came
/// back round. Delete `sweep_decline_memos` and the final assertion fails.
#[test]
fn the_era_stamp_wrap_sweeps_every_memo() {
    let (mut cpu, _bus, slot) = cpu_with_a_live_memo();
    // Walk to stamp 1 first, so the arithmetic below does not depend on how many advances the
    // drive above happened to take.
    let mut epoch = (cpu.perf.instructions >> SMC_HEAT_EPOCH_SHIFT) + 1;
    while cpu.jit_direct.decline_memo_stamp != 1 {
        cpu.perf.instructions = epoch << SMC_HEAT_EPOCH_SHIFT;
        assert!(cpu.advance_decline_memo_era());
        epoch += 1;
    }
    arm(&mut cpu, slot);
    let memo = cpu.decode_cache.decline_memo_at(slot);
    let sweeps = cpu.direct_stall_snapshot().decline_memo_sweeps;

    // Sixty-two advances take the stamp 1 -> 63 without wrapping.
    for _ in 1..=62u64 {
        cpu.perf.instructions = epoch << SMC_HEAT_EPOCH_SHIFT;
        epoch += 1;
        assert!(!cpu.decline_memo_hit(slot));
    }
    assert_eq!(cpu.jit_direct.decline_memo_stamp, 63);
    assert_eq!(
        cpu.direct_stall_snapshot().decline_memo_sweeps,
        sweeps,
        "no sweep before the wrap"
    );
    assert_eq!(
        cpu.decode_cache.decline_memo_at(slot),
        memo,
        "the byte must still carry stamp 1, or the alias this test exists for cannot arise"
    );

    // The sixty-third wraps back to 1 — the stamp the surviving byte carries.
    cpu.perf.instructions = epoch << SMC_HEAT_EPOCH_SHIFT;
    assert!(!cpu.decline_memo_hit(slot));
    assert_eq!(cpu.jit_direct.decline_memo_stamp, 1);
    assert_eq!(
        cpu.direct_stall_snapshot().decline_memo_sweeps,
        sweeps + 1,
        "the wrap must sweep exactly once"
    );
    assert_eq!(
        cpu.decode_cache.decline_memo_at(slot),
        0,
        "the wrap must have swept the array; otherwise this byte now aliases a live stamp"
    );
    assert!(
        !cpu.decline_memo_hit(slot),
        "a memo written 63 eras ago must not answer after the wrap"
    );
}

// ---------------------------------------------------------------------------------------
// The decode-line lifecycle (design §2.1 / §2.3).
// ---------------------------------------------------------------------------------------

/// §2.3's chain, which is the reason a Dormant memo needs no invalidation choke of its own: a
/// guest write that drops a Dormant key from `entries` must CONTAIN the key's own first byte, that
/// byte is a marked code byte because the line was published, so the write reaches the SMC choke
/// and retires the line. A retired line is never reached with a slot in hand, so the memo behind
/// it is unreachable — and the republish that brings the line back writes `decline_memo: 0`.
#[test]
fn a_guest_write_to_the_entry_byte_retires_the_slot() {
    let (mut cpu, mut bus, slot) = cpu_with_a_live_memo();
    let kills = cpu.perf.smc_narrow_kills;

    // Real mode with a zero base: the entry's physical address is its linear address.
    assert!(
        cpu.decode_cache.is_code_byte(ENTRY),
        "the entry byte must be MARKED, which is §2.3 step 2 and the step a reviewer should push \
         hardest on"
    );
    assert!(cpu.note_code_write_hit(ENTRY, 1));

    assert!(
        cpu.perf.smc_narrow_kills > kills || cpu.decode_cache.get_packed(ENTRY, true).is_none(),
        "the write must have retired the line, narrowly or wholesale"
    );
    assert!(
        cpu.decode_cache.get_packed(ENTRY, true).is_none(),
        "a retired slot must not answer the first-touch screen, so the memo behind it is \
         unreachable by construction"
    );

    // And the republish that brings it back carries no memo.
    cpu.registers.eip = 0x100;
    drive(&mut cpu, &mut bus);
    let fresh_slot = live_slot(&cpu, ENTRY);
    assert_eq!(fresh_slot, slot, "the same slot, republished");
}

// ---------------------------------------------------------------------------------------
// Counter identity (design §3).
// ---------------------------------------------------------------------------------------

/// The whole claim of the slice is that a memo hit is indistinguishable from the chain it
/// replaces. The census arm is the counter that would say otherwise, so run the identical program
/// twice — once with the memo answering, once with it switched off — and require the arm to match
/// byte for byte, with both terms of the closure non-zero.
///
/// Both directions can fail: a memo that forgets to increment `DormantProbe` under-counts, and one
/// that increments it on a path the full chain would have classified differently over-counts.
#[cfg(feature = "direct-admission-census")]
#[test]
fn the_census_arm_is_byte_identical_with_and_without_the_memo() {
    use crate::jit::direct::AdmissionDecline;

    fn run(memo: bool) -> (u64, u64, u64) {
        let mut cpu = fresh();
        cpu.jit_direct.decline_memo_disabled_for_test = !memo;
        cpu.enable_direct_barrier_census(true);
        let mut bus = TestBus::with_memory(loop_program());
        drive(&mut cpu, &mut bus);
        let census = cpu
            .jit_direct
            .barrier_census_snapshot()
            .expect("the census must be armed");
        (
            {
                let (label, count) =
                    census.admission_declines[AdmissionDecline::DormantProbe as usize];
                assert_eq!(label, AdmissionDecline::DormantProbe.label());
                count
            },
            cpu.perf_counters().jit_direct_dispatch_declines,
            cpu.direct_stall_snapshot().decline_memo_hits,
        )
    }

    let (memo_dormant, memo_declines, memo_hits) = run(true);
    let (plain_dormant, plain_declines, plain_hits) = run(false);

    assert_eq!(plain_hits, 0, "the kill switch must actually switch it off");
    assert!(
        memo_hits > 0,
        "the memo must have answered, or this proves nothing"
    );
    assert!(
        memo_dormant > memo_hits,
        "both terms of the closure `dormant_probe == memo_hits + full_chain` must be non-zero"
    );
    assert_eq!(
        memo_dormant, plain_dormant,
        "admission_declines[dormant_probe] must be byte-identical with and without the memo"
    );
    assert_eq!(
        memo_declines, plain_declines,
        "jit_direct_dispatch_declines must be byte-identical: the memo removes cost, not declines"
    );
}
