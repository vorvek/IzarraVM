// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The prologue's vector zero-fill (`frame::STACK_ZERO_FILL_LEN`). Thirteen 8-byte stores that
//! cleared the accumulator slots one at a time became four 32-byte stores over the whole
//! window, with `STACK_EXIT` and `STACK_QUOTA` rewritten afterwards.
//!
//! What needs pinning here is the SHAPE, since the behaviour is pinned everywhere else: every
//! block's accounting is compared against the interpreter's, lane by lane, by the direct timing
//! battery, and an accumulator that started dirty would fail those immediately. So this file
//! proves the fill is really emitted, that it really covers the window, and that a block still
//! reports clean counters when it is entered twice over a frame the previous entry dirtied.

use super::jit_direct::{
    arm_store_fixture, drive, fresh, prime_direct_store_block, store_exit_program,
};
use super::*;

/// One `vmovupd [rsp + offset], ymm0` as the emitter encodes it.
fn fill_store_bytes(offset: i32) -> Vec<u8> {
    let mut probe = jit::encoder::Encoder::new();
    probe.vmovupd_disp32_ymm(jit::encoder::Reg::RSP, offset, jit::encoder::Ymm::YMM0);
    probe.finish()
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The window is covered exactly once, end to end, and nothing beyond it is touched.
///
/// The offsets are spelled out rather than derived so that a change to `STACK_ZERO_FILL_LEN`
/// has to come here and be argued about: 0, 32, 64 and 96 tile the 128-byte window, and 128 is
/// the first byte of the ALU scratch cluster, which must NOT be cleared.
#[test]
fn the_prologue_vector_fill_covers_the_accumulator_window_exactly_once() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(store_exit_program(0x4100));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);
    let compilation = jit::direct::compile(&mut cpu, super::jit_direct::STORE_ENTRY, true)
        .expect("the primed store block recompiles");

    for offset in [0, 32, 64, 96] {
        assert_eq!(
            occurrences(&compilation.code, &fill_store_bytes(offset)),
            1,
            "the prologue must clear frame bytes {offset}..{} exactly once",
            offset + 32
        );
    }
    assert_eq!(
        occurrences(&compilation.code, &fill_store_bytes(128)),
        0,
        "the fill must stop at the ALU scratch cluster"
    );
}

/// The behavioural half: a second entry must not inherit the first entry's accumulator values.
///
/// The primed store block is re-entered over and over on one CPU, and every round's charged BUS
/// clocks, retired instructions and native entries are compared against the round before it. Bus
/// clocks are the load-bearing member: the seven dynamic lanes and the read accumulators are
/// exactly what feeds them, so a slot the prologue stopped clearing would carry the previous
/// entry's count forward and the per-round delta would GROW instead of repeating. Round 0 is
/// excluded because it is the round that still pays compilation.
///
/// Charged CORE clocks are deliberately NOT compared: they run through the per-mode fractional
/// scaler, whose carry (`timing_rem`) makes an identical round land on 1 or 2 depending on where
/// the remainder stood. That jitter is bounded by one clock and has nothing to do with the frame.
#[test]
fn repeated_entries_do_not_inherit_the_previous_frames_accumulators() {
    let target = 0x4100u32;

    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(store_exit_program(target));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);

    let mut rounds = Vec::new();
    for round in 0..5u32 {
        let before = (
            cpu.perf_counters().instructions,
            cpu.elapsed_clocks,
            bus.trace.elapsed_clocks(),
            cpu.perf_counters().jit_direct_entries,
            cpu.timing_rem,
        );
        arm_store_fixture(&mut cpu);
        drive(&mut cpu, &mut bus);
        assert_eq!(
            &bus.memory[target as usize..target as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "round {round}: the store must land"
        );
        rounds.push((
            cpu.perf_counters().instructions - before.0,
            bus.trace.elapsed_clocks() - before.2,
            cpu.perf_counters().jit_direct_entries - before.3,
            (cpu.elapsed_clocks - before.1) * 12 + cpu.timing_rem - before.4,
        ));
        assert_eq!(
            rounds.last().unwrap().3,
            5 * 12 + 60,
            "one NOP, four MOVs and the five-clock HLT entry"
        );
    }
    assert!(
        rounds.iter().all(|round| round.2 > 0),
        "every round must have entered compiled code: {rounds:?}"
    );
    for (index, round) in rounds.iter().enumerate().skip(2) {
        assert_eq!(
            round, &rounds[1],
            "round {index} must charge exactly what round 1 charged; a growing lane means the \
             prologue stopped clearing it: {rounds:?}"
        );
    }
}
