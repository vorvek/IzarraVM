// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The fetch-trace emission A/B (`JitState::native_fetch_trace`), split from
//! `cpu_jit_direct_test.rs` for the source-line ceiling and borrowing that battery's
//! store-driving helpers, exactly as the R15 table-bases A/B does.
//!
//! `emit_fetch_trace` appends a `NativeBlockTrace` entry per completed path, per self-loop
//! return and per side-exit return, and it opens by loading `NativeExit::trace_ptr` and
//! comparing it against zero. A bus whose fetches are uniform hands out `trace_ptr == 0`, so
//! that preamble can only ever fall through its own `jz`. These tests pin BOTH halves: the
//! preamble really disappears when the bus cannot arm it, and it is really still there — and
//! still populates the trace — when the bus can.

use super::jit_direct::{
    STORE_ENTRY, arm_store_fixture, drive, fresh, prime_direct_store_block, store_exit_program,
};
use super::*;

/// The `mov rcx, [rax + NativeExit::trace_ptr]` that opens every `emit_fetch_trace` append,
/// built by the same encoder the emitter uses rather than transcribed as literal bytes.
///
/// Nothing else in an emitted block loads that displacement into RCX, so a hit is a preamble
/// and a miss is its absence.
fn trace_ptr_probe_bytes() -> Vec<u8> {
    let mut probe = jit::encoder::Encoder::new();
    probe.load_r64_disp32(
        jit::encoder::Reg::RCX,
        jit::encoder::Reg::RAX,
        core::mem::offset_of!(jit::direct::NativeExit, trace_ptr) as i32,
    );
    probe.finish()
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The emission A/B. Both arms compile the SAME store program and land the SAME guest result;
/// the uniform-fetch arm emits strictly less code and contains not one trace-append preamble,
/// while the observing arm contains at least one. The occurrence count is what proves the arm
/// actually flipped emission rather than merely shrinking for some other reason.
#[test]
fn uniform_fetch_bus_drops_the_trace_append_preamble_and_lands_identically() {
    let target = 0x4100u32;
    let needle = trace_ptr_probe_bytes();
    let mut emitted = [0u64; 2];
    let mut preambles = [0usize; 2];
    for (slot, fetch_trace) in [(0usize, true), (1, false)] {
        let mut cpu = fresh();
        cpu.jit_direct.native_fetch_trace = fetch_trace;
        let mut bus = TestBus::with_memory(store_exit_program(target));
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = !fetch_trace;
        prime_direct_store_block(&mut cpu, &mut bus);
        emitted[slot] = cpu.jit_direct.total_live_code_len_for_test();
        assert_ne!(
            emitted[slot], 0,
            "fetch_trace={fetch_trace}: a block must have installed"
        );

        let compilation = jit::direct::compile(&mut cpu, STORE_ENTRY, true)
            .expect("the primed store block recompiles");
        preambles[slot] = occurrences(&compilation.code, &needle);

        arm_store_fixture(&mut cpu);
        drive(&mut cpu, &mut bus);
        assert_eq!(
            &bus.memory[target as usize..target as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "fetch_trace={fetch_trace}: the store must land"
        );
    }
    assert!(
        preambles[0] > 0,
        "the observing arm must emit the trace-append preamble"
    );
    assert_eq!(
        preambles[1], 0,
        "the uniform-fetch arm must emit no trace-append preamble at all (found {})",
        preambles[1]
    );
    assert!(
        emitted[1] < emitted[0],
        "the uniform-fetch arm must emit strictly less: observing arm {} bytes, uniform arm {} \
         bytes",
        emitted[0],
        emitted[1],
    );
}

/// The synchronisation contract, from the dangerous direction. A cache full of trace-elided
/// blocks must not survive a bus that wants fetch observations: `run_direct_block`'s backstop
/// clears it, and the recompiled blocks carry the append again.
#[test]
fn a_trace_observing_bus_reinstates_the_preamble_over_an_elided_cache() {
    let target = 0x4100u32;
    let needle = trace_ptr_probe_bytes();

    let mut cpu = fresh();
    cpu.jit_direct.native_fetch_trace = false;
    // ONE bus throughout: a direct page bakes host pointers into the emitted block, so moving a
    // primed CPU to a second `TestBus` would leave it storing into the first bus's buffer.
    let mut bus = TestBus::with_memory(store_exit_program(target));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.uniform_native_fetches = true;
    prime_direct_store_block(&mut cpu, &mut bus);
    assert!(!cpu.jit_direct.native_fetch_trace);
    assert!(cpu.jit_direct.len() > 0);
    bus.memory[target as usize..target as usize + 4].fill(0);

    // The bus starts observing fetches. ONE continuation is driven first, in isolation, because
    // that is the only window in which the clear is observable: `try_direct_continuation`
    // synchronises ahead of its own probe, so any fuller drive would have refilled the cache by
    // the time the assertion ran. Asserting only the field flip and the recompiled shape below
    // would be VACUOUS for the clear — deleting `jit_direct.clear()` from
    // `sync_native_fetch_trace` passes both (verified by mutation, review finding).
    //
    // MUTATION-VERIFIED, and worth recording which guard wins the race. With the clear deleted,
    // the resident elided block is still Ready, so this continuation ENTERS it and
    // `run_direct_block`'s `debug_assert_eq!(exit.trace_len == 0, uniform_fetches)` fires first
    // — a stronger signal than the assertion below, since it catches the dropped observation
    // itself rather than the bookkeeping. That guard is debug-only; the `len()` assertion is
    // what kills the same mutation in a release-profile test run.
    bus.uniform_native_fetches = false;
    let elided_blocks = cpu.jit_direct.len();
    assert!(elided_blocks > 0, "the elided cache must be non-empty here");
    cpu.try_direct_continuation_for_test(&mut bus, STORE_ENTRY, true)
        .expect("the synchronising continuation must not fault");
    assert!(
        cpu.jit_direct.native_fetch_trace,
        "an observing bus must flip the emission arm back"
    );
    assert_eq!(
        cpu.jit_direct.len(),
        0,
        "the {elided_blocks} trace-elided blocks must be CLEARED, not left resident for an \
         observing bus to enter"
    );

    prime_direct_store_block(&mut cpu, &mut bus);
    assert!(cpu.jit_direct.native_fetch_trace);

    let compilation = jit::direct::compile(&mut cpu, STORE_ENTRY, true)
        .expect("the primed store block recompiles");
    assert!(
        occurrences(&compilation.code, &needle) > 0,
        "blocks compiled for an observing bus must carry the trace-append preamble"
    );

    arm_store_fixture(&mut cpu);
    drive(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[target as usize..target as usize + 4],
        &0x1234_5678u32.to_le_bytes(),
        "the store must still land under the observing bus"
    );
}
