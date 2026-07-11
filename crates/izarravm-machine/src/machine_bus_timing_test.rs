// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_video::{
    FBIINIT3_REMAP, FBZ_DRAW_BACK, FBZ_RGB_WMASK, SST_FBI_INIT3, SST_FBZ_MODE, SST_SWAPBUFFER_CMD,
};

#[test]
fn dma_software_request_drives_a_mem_to_mem_block_copy() {
    // Program the 8237A through the ports for a memory-to-memory copy, then
    // arm it with a software DREQ on channel 0 (a write to the request
    // register) and confirm the destination block in guest memory matches the
    // source. The machine fires the burst on that request-register write.
    let mut machine = test_machine();
    const SRC: u32 = 0x1000;
    const DST: u32 = 0x1100;
    let src = [0xDE, 0xAD, 0xBE, 0xEFu8];
    for (i, &b) in src.iter().enumerate() {
        machine.write_physical_u8(SRC + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // Channel 0 source address 0x1000, channel 1 dest address 0x1100.
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap(); // ch0 addr LSB
        bus.write_io(0x00, BusWidth::Byte, 0x10, false).unwrap(); // ch0 addr MSB -> 0x1000
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap(); // ch1 addr LSB
        bus.write_io(0x02, BusWidth::Byte, 0x11, false).unwrap(); // ch1 addr MSB -> 0x1100
        bus.write_io(0x03, BusWidth::Byte, 0x03, false).unwrap(); // ch1 count LSB
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap(); // ch1 count MSB -> 3 (4 bytes)
        bus.write_io(0x87, BusWidth::Byte, 0x00, false).unwrap(); // ch0 page 0
        bus.write_io(0x83, BusWidth::Byte, 0x00, false).unwrap(); // ch1 page 0
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0 (the requester)
        bus.write_io(0x08, BusWidth::Byte, 0x01, false).unwrap(); // command: mem-to-mem enable
        // Arm the software DREQ on channel 0: bit2 set, channel bits 0-1 = 0.
        // This write triggers the block copy.
        bus.write_io(0x09, BusWidth::Byte, 0x04, false).unwrap();
    });
    for (i, &b) in src.iter().enumerate() {
        assert_eq!(
            machine.read_physical_u8(DST + i as u32),
            b,
            "dest byte {i} copied from the source block"
        );
    }
}

#[test]
fn dma_software_request_without_mem_to_mem_enable_does_nothing() {
    // The same request-register write, but with mem-to-mem disabled (command
    // bit0 clear), must not move any memory: the destination stays zero.
    let mut machine = test_machine();
    const SRC: u32 = 0x1000;
    const DST: u32 = 0x1100;
    for i in 0..4 {
        machine.write_physical_u8(SRC + i, 0xAB);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x10, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x03, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0
        bus.write_io(0x09, BusWidth::Byte, 0x04, false).unwrap(); // arm, but command bit0 not set
    });
    for i in 0..4 {
        assert_eq!(
            machine.read_physical_u8(DST + i),
            0x00,
            "no copy when mem-to-mem is disabled"
        );
    }
}

#[test]
fn machine_bus_snapshots_batch_entry_state() {
    // Run the machine forward a bit first so timeline/beam/
    // bus_rem are not all trivially zero, then check that a freshly-built
    // MachineBus's five batch-entry snapshot fields equal the live machine
    // state at the moment the bus is constructed.
    let mut machine = test_machine();
    machine.run_cycles(5_000).unwrap();
    let expected_timeline = machine.timeline;
    let expected_ticks = machine.timeline.now_ticks();
    let expected_beam = machine.video().beam_dots();
    let expected_trace_elapsed = machine.trace.elapsed_clocks();
    let expected_bus_rem = machine.bus_rem;
    with_bus(&mut machine, |bus| {
        assert_eq!(
            bus.timeline_at_batch_start, expected_timeline,
            "the bus must snapshot the authoritative timeline"
        );
        assert_eq!(
            bus.master_ticks_at_batch_start, expected_ticks,
            "the MIDI timestamp snapshot must use master ticks"
        );
        assert_eq!(
            bus.beam_at_batch_start, expected_beam,
            "beam_at_batch_start must mirror the VGA beam dot counter at construction"
        );
        assert_eq!(
            bus.trace_elapsed_at_batch_start, expected_trace_elapsed,
            "trace_elapsed_at_batch_start must mirror BusTrace::elapsed_clocks at construction"
        );
        assert_eq!(
            bus.bus_rem_at_batch_start, expected_bus_rem,
            "bus_rem_at_batch_start must mirror Machine::bus_rem at construction"
        );
    });
}

#[test]
fn jit_fetch_preview_matches_the_live_bus_charge() {
    for (start, count) in [(0x1000, 5), (0x000f_8000, 5)] {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            let raw = bus
                .jit_cached_fetch_run_clocks(start, count)
                .expect("uniform RAM and ROM fetches have an exact preview");
            let projected = bus
                .jit_projected_batch_scaled_bus_clocks(raw)
                .expect("the machine bus can scale an exact raw preview");
            bus.charge_instruction_fetch_run(start, count).unwrap();
            assert_eq!(projected, bus.in_batch_scaled_bus_clocks());
        });
    }

    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        assert_eq!(bus.jit_cached_fetch_run_clocks(0x000f_ffff, 2), None);
    });
}

#[test]
fn jit_direct_memory_preview_bounds_the_live_bus_charge() {
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            let data_bound = bus
                .jit_direct_memory_max_clocks(BusWidth::Byte, BusAccessKind::DataRead)
                .expect("the machine bus has a direct-memory bound");
            let fetch = bus
                .jit_cached_fetch_run_clocks(0x3000, 2)
                .expect("ordinary RAM has a stable fetch cost");
            let additional = data_bound + fetch;
            let now = bus.in_batch_scaled_bus_clocks();
            let scaled_bound = bus
                .jit_projected_batch_scaled_bus_clocks(additional)
                .unwrap()
                - now
                + 1;
            for i in 0..32 {
                let now = bus.in_batch_scaled_bus_clocks();
                let projected = bus
                    .jit_projected_batch_scaled_bus_clocks(additional)
                    .unwrap();
                assert!(projected - now <= scaled_bound, "{mode:?}, phase {i}");
                bus.charge_instruction_fetch(0x4000 + i).unwrap();
            }

            let before = bus.trace.elapsed_clocks();
            bus.charge_direct_memory(0x3000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap();
            let charged = bus.trace.elapsed_clocks() - before;
            assert!(
                charged <= data_bound,
                "{mode:?}: charged {charged}, bound {data_bound}"
            );
        });
    }
}

#[test]
fn predicted_beam_at_batch_start_equals_the_unmutated_beam() {
    // At core_clocks_so_far = 0 with zero in-batch bus clocks (the very first
    // instruction of a batch, before any fetch/data access has been recorded
    // into the trace this batch), the lazy formula must degenerate to exactly
    // the batch-entry beam because no in-batch advance has happened yet.
    let mut machine = test_machine();
    machine.run_cycles(5_000).unwrap();
    let expected_beam = machine.video().beam_dots();
    with_bus(&mut machine, |bus| {
        // core_clocks_so_far and prior_runs_core_clocks default to 0 (no
        // read_io call has run yet on this bus, no prior run this batch) and
        // trace.elapsed_clocks() at this instant equals
        // trace_elapsed_at_batch_start (nothing has been recorded since
        // construction), so in-batch clocks are zero on all terms.
        assert_eq!(
            bus.predicted_beam(),
            expected_beam,
            "zero in-batch clocks must predict exactly the batch-entry beam"
        );
    });
}

#[test]
fn predicted_beam_after_n_clocks_matches_a_real_advance_devices_of_the_same_n() {
    // Differential no-time-travel test: build two identically-driven machines
    // (the established pattern, see
    // predict_vga_dots_matches_the_real_advance_devices_accumulator_step). Run
    // both forward an odd cycle count first so the dot phase and
    // bus_rem is nonzero at batch entry (vga_dots
    // ~0.4397, bus_rem 24 after 5000 cycles). Snapshot one into a MachineBus
    // and compute predicted_beam for a given in-batch clock total; call
    // advance_devices for real on the other with the same total (expressed in
    // the same core+scaled-bus units predicted_beam consumes) and assert the
    // beam positions agree exactly. The sweep covers: the trivial zero path,
    // small deltas inside one scanline, larger multi-scanline ones, a
    // 450_000-core case whose dot total exceeds the ~404k-dot frame so the
    // modulo wrap REALLY happens (asserted below via frames_completed), and
    // nonzero prior_runs_core_clocks values so the batch-scoped core term
    // (prior runs of the same batch) is exercised, not just the run-scoped
    // one. Other lazy-read tests drive the prior-runs seam end to end through
    // read_io; here the field is set directly, paired with
    // the batch-loop pin test below
    // (batch_loop_publishes_prior_runs_core_clocks_before_every_run).
    let mut any_wrap = false;
    for prior_runs_core_clocks in [0u64, 61, 33_000] {
        for core_clocks_so_far in [0u64, 100, 12_345, 450_000] {
            for fetch_count in [0u32, 1, 4_096] {
                let mut predicted_machine = test_machine();
                predicted_machine.run_cycles(5_000).unwrap();
                let mut real_machine = test_machine();
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.timeline, real_machine.timeline);
                assert_eq!(predicted_machine.bus_rem, real_machine.bus_rem);
                assert_eq!(
                    predicted_machine.video().beam_dots(),
                    real_machine.video().beam_dots()
                );

                let (predicted, raw_bus_clocks) = with_bus(&mut predicted_machine, |bus| {
                    // Simulate fetch_count bytes' worth of bus traffic having
                    // been recorded into the trace since batch entry (prior
                    // instructions of this straight-line run, or this
                    // instruction's own fetch), at zero wait-states, then read
                    // back the actual raw clocks the trace charged for it so
                    // the real-machine side below combines the exact same
                    // total (not an assumed one) --
                    // record_instruction_fetch_run's per-byte cost is an
                    // internal BusCycle detail this test must not hardcode.
                    let before = bus.trace.elapsed_clocks();
                    if fetch_count > 0 {
                        bus.trace.record_instruction_fetch_run(0, fetch_count, 0);
                    }
                    let raw_bus_clocks = bus.trace.elapsed_clocks() - before;
                    bus.prior_runs_core_clocks = prior_runs_core_clocks;
                    bus.core_clocks_so_far = core_clocks_so_far;
                    (bus.predicted_beam(), raw_bus_clocks)
                });

                // The real batch-end step (run_until_tick / advance_devices):
                // core is the batch total (prior runs + the current run's
                // clocks), bus_clocks is what the trace recorded since batch
                // entry (mirrored here by raw_bus_clocks), scaled through
                // scale_bus's exact carry arithmetic.
                let step = prior_runs_core_clocks
                    + core_clocks_so_far
                    + real_machine.scale_bus(raw_bus_clocks);
                // Compute whether this step wraps the frame, from the same
                // pure formula, BEFORE the mutating advance: the prediction
                // only claims position, but the wrap cases must be shown to
                // really wrap (frames_completed bumps) or the coverage claim
                // above is hollow.
                let frames_before = real_machine.video().frames_completed();
                real_machine.advance_devices(step);
                let wraps = real_machine.video().frames_completed() > frames_before;

                assert_eq!(
                    predicted,
                    real_machine.video().beam_dots(),
                    "predicted_beam(prior={prior_runs_core_clocks}, \
                         core={core_clocks_so_far}, fetch_count={fetch_count}) must match a \
                         real advance_devices of the same core+scaled-bus clock total"
                );
                if wraps {
                    any_wrap = true;
                    assert!(
                        real_machine.video().frames_completed() > frames_before,
                        "a wrapping step must bump the real machine's frame counter \
                             (prior={prior_runs_core_clocks}, core={core_clocks_so_far}, \
                             fetch_count={fetch_count})"
                    );
                }
            }
        }
    }
    assert!(
        any_wrap,
        "the sweep must include at least one case that crosses a frame boundary, \
             or the wrap coverage this test claims is not exercised"
    );
}

#[test]
fn batch_loop_publishes_prior_runs_core_clocks_before_every_run() {
    // Pins the run_until_tick batch loop's prior_runs_core_clocks updates
    // through the cfg(test) push logs: before every run_straight_line call the
    // loop must republish the batch-scoped core accumulator (interrupt-service
    // charge + prior runs) into the bus, so a mid-run lazy prediction sees a
    // clock total that is monotone across run boundaries and bounded by the
    // core total the batch-end step later consumes. This pins the loop-update
    // mechanics directly: per batch, pushes are non-decreasing
    // prefix sums of the final batch core total, they reset at batch entry,
    // and real ROM execution produces multi-run batches where a later run
    // observes a NONZERO prior-runs value (the case the run-scoped
    // core_clocks_so_far alone would get wrong).
    let mut machine = test_machine();
    machine.run_cycles(300_000).unwrap();
    assert_eq!(
        machine.test_prior_core_pushes.len(),
        machine.test_batch_core_totals.len(),
        "one push log and one core total per completed batch"
    );
    assert!(
        !machine.test_prior_core_pushes.is_empty(),
        "the run must have executed at least one batch"
    );
    let mut saw_multi_run_nonzero_prior = false;
    for (batch, (pushes, total)) in machine
        .test_prior_core_pushes
        .iter()
        .zip(&machine.test_batch_core_totals)
        .enumerate()
    {
        let mut prev = 0u64;
        for &push in pushes {
            assert!(
                push >= prev,
                "batch {batch}: prior_runs_core_clocks pushes must be non-decreasing \
                     (a later run saw a smaller prior-core total: {push} after {prev})"
            );
            assert!(
                push <= *total,
                "batch {batch}: a push ({push}) exceeded the final batch core total \
                     ({total}) that fed the batch-end step"
            );
            prev = push;
        }
        if pushes.len() >= 2 && *pushes.last().unwrap() > 0 {
            saw_multi_run_nonzero_prior = true;
        }
    }
    assert!(
        saw_multi_run_nonzero_prior,
        "the boot run must contain at least one multi-run batch whose later run \
             saw a nonzero prior-runs core total; if this stops holding, drive the \
             machine differently rather than weakening the assert"
    );
}

#[test]
fn lazy_3da_read_does_not_set_io_touched_in_approximate_class_but_does_in_accurate() {
    // In the Approximate class (486/586), a
    // 0x3DA/0x3BA/0x3C2 read must NOT end the batch (io_touched stays false),
    // while the 386 modes keep the exact prior behavior
    // (io_touched set on every status-port read). Covers all three ports.
    for port in [0x3DAu16, 0x3BA, 0x3C2] {
        // Accurate class: unchanged behavior, io_touched set.
        let mut accurate = test_machine(); // Gsw386 by construction
        with_bus(&mut accurate, |bus| {
            let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
            assert!(
                *bus.io_touched,
                "port {port:#06X}: the Accurate class must still set io_touched \
                     on a status-port read"
            );
        });

        // Approximate class: the new lazy behavior, io_touched stays false.
        let mut approximate = test_machine();
        approximate.set_mode(GswMode::Gsw486);
        with_bus(&mut approximate, |bus| {
            let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
            assert!(
                !*bus.io_touched,
                "port {port:#06X}: the Approximate class must NOT set io_touched \
                     on a status-port read (the lazy path)"
            );
        });
    }
}

#[test]
fn ring0_monitor_port_access_does_not_set_io_touched_in_approximate_class() {
    // V86 trap tax, Part 1: a port access made by the ring-0 monitor
    // (cpu_is_ring0_pm = true, the TOKAEMM vec13 discriminator's PIC OCW3
    // probe being the motivating case) must NOT end the batch in the
    // Approximate class (486/586) -- the io_touched flag stays false on
    // both the read AND the write half of the OCW3 select-then-read idiom.
    // A guest (non-monitor) access to the same port keeps the old
    // unconditional-set behavior, both timing classes.
    let mut approximate = test_machine();
    approximate.set_mode(GswMode::Gsw486);
    with_bus(&mut approximate, |bus| {
        // OCW3: select ISR readback (0x0B) on the master PIC. Monitor access.
        bus.write_io(0x20, BusWidth::Byte, 0x0B, true).unwrap();
        assert!(
            !*bus.io_touched,
            "a ring-0-monitor OCW3 select write must NOT set io_touched \
                 in the Approximate class"
        );
        let _ = bus.read_io(0x20, BusWidth::Byte, 0, true).unwrap();
        assert!(
            !*bus.io_touched,
            "a ring-0-monitor PIC read must NOT set io_touched in the \
                 Approximate class"
        );
    });
}

#[test]
fn ring0_monitor_port_access_still_sets_io_touched_in_accurate_class() {
    // The 386 modes keep byte-identical batch semantics:
    // the ring-0-monitor exemption is Approximate-only, matching every
    // other lazy gate in read_io/write_io.
    let mut accurate = test_machine(); // Gsw386 by construction
    with_bus(&mut accurate, |bus| {
        bus.write_io(0x20, BusWidth::Byte, 0x0B, true).unwrap();
        assert!(
            *bus.io_touched,
            "a ring-0-monitor OCW3 select write must still set io_touched \
                 in the Accurate class"
        );
        *bus.io_touched = false;
        let _ = bus.read_io(0x20, BusWidth::Byte, 0, true).unwrap();
        assert!(
            *bus.io_touched,
            "a ring-0-monitor PIC read must still set io_touched in the \
                 Accurate class"
        );
    });
}

#[test]
fn guest_port_access_still_sets_io_touched_regardless_of_ring0_pm_flag() {
    // A false cpu_is_ring0_pm (the ordinary guest/V86 case) must keep the
    // exact pre-Part-1 behavior in BOTH timing classes -- the exemption is
    // opt-in per access, never a global relaxation.
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            bus.write_io(0x20, BusWidth::Byte, 0x0B, false).unwrap();
            assert!(
                *bus.io_touched,
                "{mode:?}: a guest OCW3 select write must set io_touched"
            );
            *bus.io_touched = false;
            let _ = bus.read_io(0x20, BusWidth::Byte, 0, false).unwrap();
            assert!(
                *bus.io_touched,
                "{mode:?}: a guest PIC read must set io_touched"
            );
        });
    }
}

#[test]
fn ring0_monitor_wide_port_access_stays_lazy_across_byte_decomposition() {
    // The width != Byte decomposition path in both read_io and write_io
    // recurses per byte; cpu_is_ring0_pm must survive that recursion so a
    // (hypothetical) wide ring-0-monitor access stays exempt on every byte,
    // not just the first.
    let mut approximate = test_machine();
    approximate.set_mode(GswMode::Gsw486);
    with_bus(&mut approximate, |bus| {
        bus.write_io(0x20, BusWidth::Word, 0x0B0B, true).unwrap();
        assert!(
            !*bus.io_touched,
            "a wide ring-0-monitor write must NOT set io_touched in the \
                 Approximate class, on any decomposed byte"
        );
    });
}

#[test]
fn lazy_3da_read_still_resets_the_attribute_flip_flop_and_calls_catch_up() {
    // A lazy 0x3DA read must perform the exact same guest-visible side effects
    // as the non-lazy read (catch_up + the Attribute Controller address/data
    // flip-flop reset), even though io_touched stays false. `Attribute`'s
    // flip_flop_data field is pub(crate) to izarravm-video, not reachable
    // directly from this crate, so this observes the flip-flop indirectly
    // through 0x3C0's own read-back semantics: a first 0x3C0 write always sets
    // the index (armed as pending data); if the flip-flop is still "data"
    // after the 3DA read, a second 0x3C0 write would be consumed as a data
    // write to the FIRST index rather than a new index, and 0x3C0's own
    // read-back (`Some(attr.index | pas<<5)`) would show the stale value.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    with_bus(&mut machine, |bus| {
        // ONE 0x3C0 write: consumed in the index phase (sets index = 0x05)
        // and leaves the flip-flop armed in the DATA phase. Exactly one
        // write, deliberately: `write_attr` toggles the flip-flop on EVERY
        // write, so a second "re-arm" write would itself consume the data
        // phase and put the flip-flop back at "index" regardless of whether
        // the 3DA reset fires -- which would make this test pass even with
        // the reset deleted (a mutation the spec review actually ran).
        bus.write_io(0x3C0, BusWidth::Byte, 0x05, false).unwrap();
        // Reading 0x3C0 returns `attr.index | pas << 5` and does NOT touch
        // the flip-flop, so this sanity check leaves the data phase armed.
        assert_eq!(
            bus.read_io(0x3C0, BusWidth::Byte, 0, false).unwrap(),
            0x05,
            "sanity: the index write took effect"
        );
        // The setup write above is an ordinary (non-lazy) port write and
        // unconditionally sets io_touched; clear it so the sanity check below
        // observes only the upcoming 3DA read's own effect on the flag.
        *bus.io_touched = false;

        // The lazy 3DA read: must reset the flip-flop to "index" despite not
        // setting io_touched.
        let _ = bus.read_io(0x3DA, BusWidth::Byte, 0, false).unwrap();
        assert!(
            !*bus.io_touched,
            "sanity: this is the lazy path (Approximate class)"
        );

        // A second 0x3C0 write with a DIFFERENT value. If the 3DA read reset
        // the flip-flop to "index", this is an index write (index becomes
        // 0x0A) and the read-back shows 0x0A. If the reset did NOT fire, the
        // flip-flop is still in the data phase, so this write lands as DATA
        // for the stale index 0x05 (palette[5] = 0x0A) and the read-back
        // still shows 0x05, failing the assertion. Mutation-verified: with
        // `flip_flop_data = false` deleted from status1_side_effects this
        // assertion fails; restored, it passes.
        bus.write_io(0x3C0, BusWidth::Byte, 0x0A, false).unwrap();
        assert_eq!(
            bus.read_io(0x3C0, BusWidth::Byte, 0, false).unwrap(),
            0x0A,
            "the 3DA read must have reset the attribute flip-flop to \"index\", \
                 so the next 0x3C0 write is treated as a new index (0x0A), not a \
                 data write to the stale index 0x05"
        );
    });
}

#[test]
fn lazy_3da_read_returns_the_same_bits_a_non_lazy_read_would_at_batch_start() {
    // At batch start (zero in-batch clocks, predicted_beam degenerates to the
    // batch-entry beam exactly, per
    // predicted_beam_at_batch_start_equals_the_unmutated_beam), the lazy
    // status1 bits must be byte-identical to what a non-lazy read_status1
    // would return for the same live beam. Compared
    // within a SINGLE Approximate-class machine (not across two differently
    // clocked machines, whose beams would drift apart independently of this
    // task's change): clone the live Vga state before either read touches
    // it, compute the accurate read_status1() on the clone, then compute the
    // lazy value through the real bus, both starting from the identical
    // device state.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    machine.run_cycles(5_000).unwrap();

    let mut accurate_clone = machine.video().clone();
    let expected = accurate_clone.read_status1();

    let (lazy_value, io_touched) = with_bus(&mut machine, |bus| {
        let value = bus.read_io(0x3DA, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });

    assert!(
        !io_touched,
        "sanity: this is the lazy path (Approximate class)"
    );
    assert_eq!(
        lazy_value,
        u32::from(expected),
        "a lazy 3DA read at batch start must return byte-identical bits to a \
             non-lazy read of the same live beam"
    );
}

#[test]
fn lazy_reads_chain_into_far_fewer_batches_than_poll_iterations_with_monotone_observations() {
    // End-to-end no-time-travel test: a real mode-13h guest tightly polls
    // 0x3DA in a loop and
    // maintains, in guest memory, a running sample count and a toggle count
    // of the vretrace bit (0x08) across every sample it has ever taken --
    // not just a bounded ring, so the toggle observation cannot be an
    // artifact of a capture window that happens to miss an edge. Asserts (a)
    // the Approximate-class run collapses many poll iterations into far
    // fewer `run_straight_line` calls (each 0x3DA IN no longer ends the
    // batch), and (b) the vretrace bit toggled at least once across the
    // whole run -- proving the lazy per-read prediction actually tracked
    // beam motion across many samples rather than reading a frozen value.
    //
    // Guest memory layout: [0x7000] sample count, [0x7004] toggle count,
    // [0x7006] last-observed vretrace bit (byte).
    let code = [
        0xB8, 0x13, 0x00, // 0: mov ax, 0x0013 (mode 13h)
        0xCD, 0x10, // 3: int 0x10
        0x31, 0xC0, // 5: xor ax, ax
        0x8E, 0xD8, // 7: mov ds, ax
        0xC7, 0x06, 0x00, 0x70, 0x00, 0x00, // 9: mov word [0x7000], 0 (sample count)
        0xC7, 0x06, 0x04, 0x70, 0x00, 0x00, // 15: mov word [0x7004], 0 (toggle count)
        0xC6, 0x06, 0x06, 0x70, 0xFF, // 21: mov byte [0x7006], 0xFF (no prior sample)
        0xBA, 0xDA, 0x03, // 26: mov dx, 0x03DA
        // poll (29): read status, isolate the vretrace bit, compare against
        // the last-observed bit, bump the toggle count on a change, stash
        // the new last-observed bit, bump the sample count, loop forever.
        0xEC, // 29: in al, dx
        0x24, 0x08, // 30: and al, 0x08 (isolate the vretrace bit)
        0x3A, 0x06, 0x06, 0x70, // 32: cmp al, [0x7006]
        0x74, 0x04, // 36: jz same (+4: skip the toggle bump)
        0xFF, 0x06, 0x04, 0x70, // 38: inc word [0x7004] (toggle count)
        // same (42):
        0xA2, 0x06, 0x70, // 42: mov [0x7006], al
        0xFF, 0x06, 0x00, 0x70, // 45: inc word [0x7000] (sample count)
        0xEB, 0xEA, // 49: jmp poll (displacement -22: poll=29, jmp ends at 51)
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path

    // Warm up until mode 13h is set and the guest is inside the poll loop.
    machine.run_cycles(50_000).unwrap();
    machine.cpu.reset_perf_counters();
    let sample_count_before = machine.memory.read_u16(0x7000).unwrap();

    // Run enough guest clocks to complete several frames' worth of polling.
    let clock_hz = machine.active_mode().clock_hz();
    machine.run_cycles(clock_hz / 20).unwrap(); // 50ms of guest time

    // `straight_line_runs` counts every `run_straight_line` call (opening a
    // new run OR chaining continuations); each poll iteration is one IN, so
    // without continuation-chaining this would grow roughly 1:1 with the
    // sample count. With lazy reads admitted as continuations, many samples
    // land inside a single run.
    let runs = machine.cpu.perf_counters().straight_line_runs;
    let sample_count_after = machine.memory.read_u16(0x7000).unwrap();
    let samples_taken = sample_count_after.wrapping_sub(sample_count_before);
    let toggles = machine.memory.read_u16(0x7004).unwrap();

    assert!(
        samples_taken > 1000,
        "sanity: the poll loop must have run many iterations in 50ms of \
             guest time, saw {samples_taken}"
    );
    assert!(
        runs < u64::from(samples_taken) / 4,
        "lazy reads must chain many poll iterations per run_straight_line \
             call: saw {runs} runs for {samples_taken} samples (expected far \
             fewer runs than samples)"
    );
    assert!(
        toggles > 0,
        "the vretrace bit must have toggled at least once across the whole \
             run's samples in 50ms of guest time (multiple frames), or the lazy \
             prediction never actually tracked beam motion; saw {toggles} \
             toggles across {samples_taken} samples"
    );
    // Upper bound (spec-review hardening): a prediction jittering BACKWARD
    // across the vretrace edge would inflate the toggle count and still
    // satisfy toggles > 0, so bound it by the physically possible edge
    // count. Derivation: the measured window is 50ms of guest time; mode
    // 13h runs ~70 frames/s, so ~3.5 frames, and the vretrace bit toggles
    // exactly twice per frame (set at retrace start, clear at its end) =
    // ~7 toggles. Plus the 50_000-clock warm-up (< 1ms, at most one edge
    // pair -- the counter accumulates from boot) and +1 from the guest's
    // 0xFF last-bit sentinel mismatching the first real sample. Total
    // expected <= ~10; 20 leaves generous slack while still failing on any
    // per-read jitter (which would produce hundreds of spurious toggles
    // across >1000 samples).
    assert!(
        toggles < 20,
        "the vretrace bit toggled {toggles} times across {samples_taken} \
             samples in ~3.5 frames of guest time; more than ~2 per frame (+ \
             slack) means the lazy prediction is jittering back and forth \
             across the retrace edge instead of advancing monotonically"
    );
}

#[test]
fn lazy_read_after_an_interrupt_service_charge_sees_the_batch_scoped_total() {
    // Carried-forward review note: the first lazy read of a batch that opened
    // with an interrupt-service charge (the once-per-batch IRQ dispatch cost
    // added to batch_core before the first run_straight_line call) must see a
    // clock total that includes that charge -- prior_runs_core_clocks is
    // republished from batch_core before every run, and the very first
    // publish (before run 1) already carries the service charge. Observable
    // via the cfg(test) log seam: the FIRST prior-runs push of a batch that
    // serviced an interrupt must be nonzero.
    //
    // Reuses approximate_class_delivers_pit_irq0_during_long_compute_stretches'
    // exact setup (a pure `sti; jmp $` compute loop after arming the PIC/PIT
    // for ~3.43ms IRQ0 ticks) so an interrupt is serviced at a KNOWN, reliable
    // cadence rather than depending on incidental BIOS/POST timing.
    let code = [
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xB0, 0x11, 0xE6, 0x20, // ICW1
        0xB0, 0x08, 0xE6, 0x21, // ICW2: vector base 0x08
        0xB0, 0x04, 0xE6, 0x21, // ICW3
        0xB0, 0x01, 0xE6, 0x21, // ICW4
        0xB0, 0x00, 0xE6, 0x21, // unmask all lines
        0xB0, 0x34, 0xE6, 0x43, // PIT ch0 mode 2, LSB/MSB
        0xB0, 0x00, 0xE6, 0x40, // reload low 0x00
        0xB0, 0x10, 0xE6, 0x40, // reload high 0x10 -> 4096 ticks (~3.43 ms)
        0xFB, // sti
        0xEB, 0xFE, // jmp $ (pure compute, no port I/O)
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    // IRQ0 handler at 0x0700: mov al,0x20; out 0x20,al; iret (EOI only, no
    // guest-visible port I/O beyond that, which keeps the batch shape simple).
    let handler: [u8; 5] = [0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x08] (IRQ0 at PIC base 0x08) -> 0000:0700.
    machine.write_physical_u8(0x20, 0x00);
    machine.write_physical_u8(0x21, 0x07);
    machine.write_physical_u8(0x22, 0x00);
    machine.write_physical_u8(0x23, 0x00);
    // A few periods of 4096 PIT ticks at the Gsw486 clock rate, comfortably
    // enough for several IRQ0 edges (and thus several interrupt-opened
    // batches) to land.
    machine.run_cycles(5_000_000).unwrap();

    assert!(
        !machine.test_prior_core_pushes.is_empty(),
        "the run must have executed at least one batch"
    );
    let saw_batch_with_serviced_interrupt_charge = machine
        .test_prior_core_pushes
        .iter()
        .any(|pushes| pushes.first().is_some_and(|&first| first > 0));
    assert!(
        saw_batch_with_serviced_interrupt_charge,
        "at least one batch's FIRST prior-runs publish (before its first \
             run_straight_line call) must be nonzero, proving an interrupt- \
             service charge from batch entry is visible to the first lazy read \
             of that batch's first run, not just to later runs"
    );
}

#[test]
fn lazy_61_read_does_not_set_io_touched_in_approximate_class_but_does_in_accurate() {
    // Mirroring the 3DA/3BA/3C2 case, in
    // the Approximate class (486/586) a port 0x61 read must NOT end the
    // batch (io_touched stays false), while the 386 modes
    // keeps the exact prior behavior (io_touched set).
    let mut accurate = test_machine(); // Gsw386 by construction
    with_bus(&mut accurate, |bus| {
        let _ = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        assert!(
            *bus.io_touched,
            "the Accurate class must still set io_touched on a port 0x61 read"
        );
    });

    let mut approximate = test_machine();
    approximate.set_mode(GswMode::Gsw486);
    with_bus(&mut approximate, |bus| {
        let _ = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        assert!(
            !*bus.io_touched,
            "the Approximate class must NOT set io_touched on a port 0x61 \
                 read (the lazy path)"
        );
    });
}

#[test]
fn lazy_61_read_returns_the_same_bits_a_non_lazy_read_would_at_batch_start() {
    // At batch start (zero in-batch clocks, predicted_pit_out degenerates to
    // the batch-entry live channel_out exactly, the PIT counterpart of
    // predicted_beam_at_batch_start_equals_the_unmutated_beam), the lazy 0x61
    // byte must be byte-identical to what a non-lazy read would return for the
    // same live PIT/speaker state.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    machine.run_cycles(5_000).unwrap();

    let expected = (machine.speaker.control_bits() & 0x03)
        | (u8::from(machine.pit.channel_out(1)) << 4)
        | (u8::from(machine.pit.channel_out(2)) << 5);

    let (lazy_value, io_touched) = with_bus(&mut machine, |bus| {
        let value = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });

    assert!(
        !io_touched,
        "sanity: this is the lazy path (Approximate class)"
    );
    assert_eq!(
        lazy_value,
        u32::from(expected),
        "the lazy 0x61 byte must equal the non-lazy read at batch start \
             (zero in-batch clocks)"
    );
}

#[test]
fn predicted_pit_out_after_n_clocks_matches_a_real_advance_devices_of_the_same_n() {
    // Differential no-time-travel test, the PIT counterpart of
    // predicted_beam_after_n_clocks_matches_a_real_advance_devices_of_the_same_n:
    // build two identically-driven machines, snapshot one into a MachineBus,
    // compute predicted_pit_out for a given in-batch clock total, and call
    // advance_devices for real on the other with the same total (expressed
    // in the same core+scaled-bus units) -- the two must agree exactly.
    // Mode-2 (channel 1, the AT refresh timer, pre-seeded at power-on) and
    // mode-3 (channel 2, PC speaker) channels are both covered, including
    // totals crossing several OUT edges, so the sweep exercises both this
    // slice's channels at both the periods the real machine actually uses.
    for prior_runs_core_clocks in [0u64, 61, 33_000] {
        for core_clocks_so_far in [0u64, 100, 12_345, 450_000] {
            for channel in [1usize, 2] {
                let mut predicted_machine = test_machine();
                predicted_machine.set_mode(GswMode::Gsw486);
                if channel == 2 {
                    // Arm channel 2 in mode 3 (square wave) with a short
                    // divisor so several OUT edges land inside the swept
                    // clock range; GATE2 comes from port 0x61 bit 0.
                    with_bus(&mut predicted_machine, |bus| {
                        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
                        bus.write_io(0x42, BusWidth::Byte, 0x10, false).unwrap(); // divisor low
                        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap(); // divisor high (16)
                        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data
                    });
                }
                predicted_machine.run_cycles(5_000).unwrap();
                let mut real_machine = test_machine();
                real_machine.set_mode(GswMode::Gsw486);
                if channel == 2 {
                    with_bus(&mut real_machine, |bus| {
                        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
                        bus.write_io(0x42, BusWidth::Byte, 0x10, false).unwrap();
                        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
                        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
                    });
                }
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.timeline, real_machine.timeline);
                assert_eq!(
                    predicted_machine.pit.channel_out(channel),
                    real_machine.pit.channel_out(channel)
                );

                let (predicted, raw_bus_clocks) = with_bus(&mut predicted_machine, |bus| {
                    let before = bus.trace.elapsed_clocks();
                    if core_clocks_so_far > 0 {
                        // A cheap stand-in for real bus traffic: any nonzero
                        // fetch count exercises the scaled-bus term the same
                        // way predicted_beam's twin test does.
                        bus.trace.record_instruction_fetch_run(0, 1, 0);
                    }
                    let raw_bus_clocks = bus.trace.elapsed_clocks() - before;
                    bus.prior_runs_core_clocks = prior_runs_core_clocks;
                    bus.core_clocks_so_far = core_clocks_so_far;
                    (bus.predicted_pit_out(channel), raw_bus_clocks)
                });

                let step = prior_runs_core_clocks
                    + core_clocks_so_far
                    + real_machine.scale_bus(raw_bus_clocks);
                real_machine.advance_devices(step);

                assert_eq!(
                    predicted,
                    Some(real_machine.pit.channel_out(channel)),
                    "predicted_pit_out(channel={channel}, prior={prior_runs_core_clocks}, \
                         core={core_clocks_so_far}) must match a real advance_devices \
                         of the same core+scaled-bus clock total"
                );
            }
        }
    }
}

#[test]
fn lazy_61_read_falls_back_to_the_non_lazy_path_for_a_bcd_counter() {
    // `out_after` conservatively declines for a
    // BCD-programmed counter, so the lazy 0x61 arm must fall all the way
    // back to the exact non-lazy path -- io_touched set, today's live read
    // -- rather than a second implementation of the bit composition.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path would
    // otherwise apply.
    with_bus(&mut machine, |bus| {
        // Program channel 1 as BCD, mode 2: SC=01, RW=11, mode=010, BCD=1.
        bus.write_io(0x43, BusWidth::Byte, 0x75, false).unwrap();
        bus.write_io(0x41, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x41, BusWidth::Byte, 0x01, false).unwrap();
        *bus.io_touched = false; // clear the setup writes' own effect

        let expected = (bus.speaker.control_bits() & 0x03)
            | (u8::from(bus.pit.channel_out(1)) << 4)
            | (u8::from(bus.pit.channel_out(2)) << 5);
        let value = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        assert!(
            *bus.io_touched,
            "a BCD-programmed channel must fall back to the non-lazy path, \
                 which sets io_touched"
        );
        assert_eq!(
            value,
            u32::from(expected),
            "the BCD fallback must return exactly today's live read"
        );
    });
}

#[test]
fn opl_status_read_sets_io_touched_in_every_cpu_mode() {
    // AdLib detection is a timer probe, so every OPL status read must end
    // the current CPU batch even in approximate 486/586 modes. Covers every
    // alias `opl_port` maps to a status read: the native 0x388/0x38A and the
    // SB16 mirrors 0x220/0x222/0x228.
    let status_ports = [0x388u16, 0x38a, 0x220, 0x222, 0x228];

    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        for &port in &status_ports {
            with_bus(&mut machine, |bus| {
                *bus.io_touched = false;
                let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
                assert!(
                    *bus.io_touched,
                    "mode {mode:?}, port {port:#06X}: OPL status reads \
                         must set io_touched"
                );
            });
        }
    }
}

#[test]
fn opl_status_read_returns_the_live_status_byte_in_approximate_mode() {
    // 486/586 still use exact OPL status reads. Pin the byte value as well
    // as the batch-ending behavior on an active timer.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // latch reg 0x04
        bus.write_io(0x389, BusWidth::Byte, 0x80, false).unwrap(); // reset IRQ flags
        bus.write_io(0x388, BusWidth::Byte, 0x02, false).unwrap(); // latch reg 0x02
        bus.write_io(0x389, BusWidth::Byte, 0xff, false).unwrap(); // timer 1 preset
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // latch reg 0x04
        bus.write_io(0x389, BusWidth::Byte, 0x01, false).unwrap(); // start timer 1
    });
    machine.run_cycles(5_000).unwrap();

    let expected = machine.opl.status();

    let (lazy_value, io_touched) = with_bus(&mut machine, |bus| {
        let value = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });

    assert!(
        io_touched,
        "486-mode OPL status reads must stay batch-ending"
    );
    assert_eq!(
        lazy_value,
        u32::from(expected),
        "the OPL status byte must equal the live device status"
    );
}

#[test]
fn adlib_detection_idiom_ends_the_batch_on_status_reads() {
    // The AdLib detection idiom is one address-port write followed by
    // status-port polling. Both the write and the reads must end the CPU
    // batch so the OPL timers advance between polls in approximate modes.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);

    with_bus(&mut machine, |bus| {
        *bus.io_touched = false;
        let _ = bus.write_io(0x388, BusWidth::Byte, 0x04, false); // address write
        assert!(
            *bus.io_touched,
            "the address-port write must still set io_touched (writes \
                 stay batch-ending)"
        );

        for _ in 0..6 {
            *bus.io_touched = false;
            let _ = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
            assert!(
                *bus.io_touched,
                "status-port reads must set io_touched in the \
                     Approximate class"
            );
        }
    });
}

#[test]
fn opl_status_poll_charges_isa_bus_time_only_in_approximate_class() {
    // A fast CPU retires a tight IN loop so quickly that the 80 us OPL timer
    // AdLib detection waits on never overflows, so Doom disables FM music. The
    // fix charges each OPL status read one ISA bus period (~1 us), folded into
    // the batch's device advance, so the poll cannot outrun the timer. The
    // The 486/586 modes accrue it. The slower 386 modes already span the
    // window and do not need the extra charge.
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        machine.isa_io_batch_clocks = 0;
        with_bus(&mut machine, |bus| {
            let _ = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
        });
        let expected = (mode.clock_hz() / 1_000_000).max(1);
        assert_eq!(
            machine.isa_io_batch_clocks, expected,
            "{mode:?}: one OPL status poll charges one ISA bus period"
        );
    }

    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.isa_io_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        let _ = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.isa_io_batch_clocks, 0,
        "the Accurate class must not charge ISA I/O time (byte-identical cadence)"
    );
}

#[test]
fn instruction_fetch_run_fast_path_stops_at_the_video_aperture() {
    // Pins the `end < 0xA0000` guard in charge_physical_instruction_fetch_run: a run whose
    // last byte is 0x9FFFF takes the conventional-RAM fast path (one collapsed
    // I-cache access at the per-mode code-fetch constant), while a run straddling
    // 0xA0000 must fall through to the full classification, which sees the VGA
    // window's wait-states, goes non-uniform, and charges per byte.
    use izarravm_bus::BusCycle;
    let mut machine = test_machine();
    // Preconditions for the straddle case: the A0000 window decodes as a device
    // window, and its wait-states differ from the code-fetch constant (otherwise
    // the uniform arm legitimately collapses the run and the paths are
    // charge-identical by design).
    assert!(machine.video().video_memory_enabled());
    let code_ws = machine.cache_model.code_fetch_wait_states();
    let video_ws = machine.profile.wait_states.video;
    assert_ne!(
        code_ws, video_ws,
        "test needs distinct RAM/video wait-states"
    );

    with_bus(&mut machine, |bus| {
        // Fast path: 4 bytes ending exactly at 0x9FFFF -> one I-cache access.
        let before = bus.trace.elapsed_clocks();
        bus.charge_physical_instruction_fetch_run(0x0009_FFFC, 4)
            .unwrap();
        assert_eq!(
            bus.trace.elapsed_clocks() - before,
            u64::from(BusCycle::clocks_for(BusWidth::Byte, code_ws)),
            "run ending at 0x9FFFF charges a single I-cache access"
        );
        // Slow path: 4 bytes straddling 0xA0000 -> non-uniform (RAM then VGA
        // window), charged per byte: two at the code-fetch constant, two at
        // the video cost.
        let before = bus.trace.elapsed_clocks();
        bus.charge_physical_instruction_fetch_run(0x0009_FFFE, 4)
            .unwrap();
        assert_eq!(
            bus.trace.elapsed_clocks() - before,
            2 * u64::from(BusCycle::clocks_for(BusWidth::Byte, code_ws))
                + 2 * u64::from(BusCycle::clocks_for(BusWidth::Byte, video_ws)),
            "run straddling 0xA0000 keeps the per-byte classification"
        );
    });
}

#[test]
fn ram_lookup_rebuilds_on_each_effective_distira_decode_change() {
    fn write_config(bus: &mut MachineBus<'_>, register: u32, value: u32) {
        let address = 0x8000_0000 | (u32::from(DISTIRA_PCI_SLOT) << 11) | register;
        bus.write_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, address, false)
            .unwrap();
        bus.write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, value, false)
            .unwrap();
    }

    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    const RAM_ADDR: u32 = 0x0100_0000;
    const ALT_REG_RAM_ADDR: u32 = RAM_ADDR + 0x0020_0000;
    machine.memory.write_u8(RAM_ADDR as usize, 0x5a).unwrap();
    machine
        .memory
        .write_u8(ALT_REG_RAM_ADDR as usize, 0x6b)
        .unwrap();

    with_bus(&mut machine, |bus| {
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_some(),
            "extended RAM starts direct"
        );
        write_config(bus, 0x10, RAM_ADDR);
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "Distira BAR overlap removes the direct page"
        );
        assert!(
            *bus.direct_map_changed,
            "BAR relocation marks CPU direct caches stale"
        );
        bus.write_memory(RAM_ADDR, BusWidth::Byte, 0xa5, BusAccessKind::DataWrite)
            .unwrap();

        *bus.direct_map_changed = false;
        write_config(bus, 0x04, 0);
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_some(),
            "command disable restores the direct RAM page"
        );
        assert!(*bus.direct_map_changed);

        *bus.direct_map_changed = false;
        write_config(bus, 0x10, DISTIRA_MMIO_BASE);
        assert!(
            !*bus.direct_map_changed,
            "moving a disabled BAR does not change the effective decode"
        );

        write_config(bus, 0x04, 0x0000_0002);
        assert!(*bus.direct_map_changed);
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_some(),
            "enabling a BAR outside RAM leaves the direct page available"
        );

        *bus.direct_map_changed = false;
        write_config(bus, 0x10, RAM_ADDR);
        assert!(*bus.direct_map_changed);
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "relocating the enabled BAR over RAM removes the direct page again"
        );
    });
    machine.write_physical_u8(ALT_REG_RAM_ADDR, 0xb6);

    assert_eq!(
        machine.memory.read_u8(RAM_ADDR as usize).unwrap(),
        0x5a,
        "Distira BAR relocation must invalidate direct-RAM lookup entries"
    );
    assert_eq!(
        machine.memory.read_u8(ALT_REG_RAM_ADDR as usize).unwrap(),
        0x6b,
        "the alternate register aperture must not leak writes into backing RAM"
    );
}

#[test]
fn distira_alt_register_aperture_reaches_glide_setup() {
    const RAM_BAR: u32 = 0x0100_0000;
    const ALT: u32 = 1 << 21;
    const SST_VERTEX_AX: u32 = 0x008;
    const SST_VERTEX_AY: u32 = 0x00c;
    const SST_VERTEX_BX: u32 = 0x010;
    const SST_VERTEX_BY: u32 = 0x014;
    const SST_VERTEX_CX: u32 = 0x018;
    const SST_VERTEX_CY: u32 = 0x01c;
    const ALT_START_R: u32 = 0x020;
    const ALT_START_G: u32 = 0x02c;
    const ALT_START_B: u32 = 0x038;
    const ALT_TRIANGLE_CMD: u32 = 0x080;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    let backing_address = (RAM_BAR + ALT + ALT_START_R) as usize;
    machine.memory.write_u8(backing_address, 0x5a).unwrap();

    with_bus(&mut machine, |bus| {
        for (register, value) in [(0x10, RAM_BAR), (0x40, 1)] {
            let address = 0x8000_0000 | (u32::from(DISTIRA_PCI_SLOT) << 11) | register;
            bus.write_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, address, false)
                .unwrap();
            bus.write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, value, false)
                .unwrap();
        }
    });

    machine.write_physical_u32(RAM_BAR + SST_FBI_INIT3 as u32, FBIINIT3_REMAP);
    machine.write_physical_u32(RAM_BAR + SST_FBZ_MODE as u32, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    for (register, value) in [
        (SST_VERTEX_AX, 0),
        (SST_VERTEX_AY, 0),
        (SST_VERTEX_BX, 3 << 4),
        (SST_VERTEX_BY, 0),
        (SST_VERTEX_CX, 0),
        (SST_VERTEX_CY, 3 << 4),
        (ALT_START_R, 0xff << 12),
        (ALT_START_G, 0),
        (ALT_START_B, 0),
    ] {
        machine.write_physical_u32(RAM_BAR + ALT + register, value);
    }
    machine.write_physical_u32(RAM_BAR + ALT + ALT_TRIANGLE_CMD, 0);
    machine.write_physical_u32(RAM_BAR + SST_SWAPBUFFER_CMD as u32, 0);

    assert_eq!(machine.active_display(), ActiveDisplay::Distira);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (640, 480));
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(machine.memory.read_u8(backing_address).unwrap(), 0x5a);
}

#[test]
fn distira_decode_writes_end_ring0_approximate_batches() {
    let mut profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    profile.cpu = GswMode::Gsw486;
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    const RAM_ADDR: u32 = 0x0100_0000;

    with_bus(&mut machine, |bus| {
        assert!(bus.lazy_port_reads);
        *bus.io_touched = false;
        let address = 0x8000_0000 | (u32::from(DISTIRA_PCI_SLOT) << 11) | 0x10;
        bus.write_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, address, true)
            .unwrap();
        *bus.io_touched = false;

        bus.write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, RAM_ADDR, true)
            .unwrap();

        assert!(*bus.io_touched, "a decode change must end the CPU batch");
        assert!(*bus.direct_map_changed);
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn guest_distira_decode_transitions_invalidate_cpu_direct_maps_once_each() {
    fn push_out_dx_eax(code: &mut Vec<u8>, port: u16, value: u32) {
        code.push(0xba);
        code.extend_from_slice(&port.to_le_bytes());
        code.extend_from_slice(&[0x66, 0xb8]);
        code.extend_from_slice(&value.to_le_bytes());
        code.extend_from_slice(&[0x66, 0xef]);
    }

    const BAR_CONFIG: u32 = 0x8000_0000 | ((DISTIRA_PCI_SLOT as u32) << 11) | 0x10;
    const COMMAND_CONFIG: u32 = 0x8000_0000 | ((DISTIRA_PCI_SLOT as u32) << 11) | 0x04;
    const RAM_ADDR: u32 = 0x0100_0000;
    let mut code = Vec::new();
    push_out_dx_eax(&mut code, PCI_CONFIG_ADDRESS_PORT, BAR_CONFIG);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, RAM_ADDR);
    push_out_dx_eax(&mut code, PCI_CONFIG_ADDRESS_PORT, COMMAND_CONFIG);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, 0);
    push_out_dx_eax(&mut code, PCI_CONFIG_ADDRESS_PORT, BAR_CONFIG);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, DISTIRA_MMIO_BASE);
    push_out_dx_eax(&mut code, PCI_CONFIG_ADDRESS_PORT, COMMAND_CONFIG);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, 2);
    push_out_dx_eax(&mut code, PCI_CONFIG_ADDRESS_PORT, BAR_CONFIG);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, RAM_ADDR);
    push_out_dx_eax(&mut code, PCI_CONFIG_ADDRESS_PORT, COMMAND_CONFIG);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, 0);
    push_out_dx_eax(&mut code, PCI_CONFIG_DATA_PORT, 2);
    code.extend_from_slice(&[0xcd, 0x20]);

    let mut profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    profile.cpu = GswMode::Gsw486;
    let mut machine = Machine::new_raw_program(profile, &code).unwrap();
    machine.cpu.reset_perf_counters();

    assert_eq!(
        machine.run_until_halt_or_cycles(500_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.cpu.perf_counters().direct_map_invalidations, 6);
}

#[test]
fn direct_memory_helpers_accept_only_page_local_ram() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    machine.memory.write_u32(0x2000, 0x1234_5678).unwrap();

    with_bus(&mut machine, |bus| {
        let read_page = bus
            .direct_page(0x2000, BusAccessKind::DataRead)
            .unwrap()
            .expect("ordinary RAM page is direct");
        assert_eq!(read_page.physical_page, 0x2000);
        assert_eq!(read_page.len, RAM_LOOKUP_PAGE_SIZE);
        assert!(!read_page.ptr.is_null());
        assert!(!read_page.writable, "read lookup is not a write grant");
        assert!(
            bus.direct_page(0x2000, BusAccessKind::DataWrite)
                .unwrap()
                .expect("ordinary RAM write page is direct")
                .writable,
            "write lookup grants writes"
        );
        let ram = bus
            .read_memory_direct(0x2000, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        assert!(ram.direct, "ordinary RAM is direct");
        assert_eq!(ram.value, 0x1234_5678);
        assert!(
            bus.write_memory_direct(
                0x2004,
                BusWidth::Dword,
                0xDEAD_BEEF,
                BusAccessKind::DataWrite
            )
            .unwrap()
            .direct,
            "ordinary RAM writes are direct"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2ff0, 16, BusWidth::Byte),
            16,
            "same-page RAM span is direct"
        );

        assert_eq!(
            bus.direct_memory_bytes(0x2fff, 2, BusWidth::Byte),
            0,
            "cross-page spans fall back"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2001, 2, BusWidth::Word),
            0,
            "split word spans fall back"
        );
        assert!(
            !bus.read_memory_direct(LOW_BIOS_BASE, BusWidth::Dword, BusAccessKind::DataRead)
                .unwrap()
                .direct,
            "ROM falls back"
        );
        assert!(
            bus.direct_page(LOW_BIOS_BASE, BusAccessKind::InstructionPrefetch)
                .unwrap()
                .is_none(),
            "ROM has no direct page"
        );
        assert!(
            !bus.write_memory_direct(
                VGA_TEXT_BASE,
                BusWidth::Byte,
                b'X'.into(),
                BusAccessKind::DataWrite
            )
            .unwrap()
            .direct,
            "VGA memory falls back"
        );
        assert!(
            bus.direct_page(VGA_TEXT_BASE, BusAccessKind::DataWrite)
                .unwrap()
                .is_none(),
            "VGA memory has no direct page"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x0E_0000, 4, BusWidth::Dword),
            0,
            "upper-memory window falls back"
        );
        assert!(
            bus.direct_page(0x0E_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "the upper-memory window has no direct page"
        );
    });

    machine.keyboard.set_a20(false);
    with_bus(&mut machine, |bus| {
        assert!(
            !bus.read_memory_direct(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap()
                .direct,
            "A20-folded accesses fall back"
        );
        assert!(
            bus.direct_page(0x10_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "A20-folded pages are not direct"
        );
    });
}

#[test]
fn canonical_mode13_page_round_trips_through_the_cpu_cache() {
    const RESULT_OFFSET: u32 = 0x0130;
    const PROGRAM: &[u8] = &[
        0xB8, 0x00, 0xA0, // mov ax,A000h
        0x8E, 0xC0, // mov es,ax
        0xBF, 0x34, 0x12, // mov di,1234h
        0xB0, 0x5A, // mov al,5Ah
        0x26, 0x88, 0x05, // mov es:[di],al
        0x30, 0xC0, // xor al,al
        0x26, 0x8A, 0x05, // mov al,es:[di]
        0xA2, 0x30, 0x01, // mov [0130h],al
        0xCD, 0x20, // int 20h
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    with_bus(&mut machine, |bus| {
        let page = bus
            .direct_page(0xA_1234, BusAccessKind::DataWrite)
            .unwrap()
            .expect("stock chained Mode 13h page is direct");
        assert_eq!(page.physical_page, 0xA_1000);
        assert!(page.writable);
    });
    machine.cpu.reset_perf_counters();

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    let result = (u32::from(DOS_LOAD_SEGMENT) << 4) + RESULT_OFFSET;
    assert_eq!(machine.read_physical_u8(result), 0x5A);
    assert_eq!(machine.video().cpu_read_chain4(0x1234), 0x5A);
    let perf = machine.cpu.perf_counters();
    assert!(perf.direct_data_pointer_reads > 0);
    assert!(perf.direct_data_pointer_writes > 0);
}

#[test]
fn canonical_mode13_bulk_read_uses_the_linear_page() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    for (offset, value) in [0x11, 0x22, 0x33, 0x44].into_iter().enumerate() {
        machine.video_mut().cpu_write_chain4(0x1200 + offset, value);
    }

    with_bus(&mut machine, |bus| {
        assert_eq!(bus.direct_memory_bytes(0xA_1200, 4, BusWidth::Dword), 4);
        let mut bytes = [0; 4];
        assert_eq!(
            bus.read_memory_bytes_direct(
                0xA_1200,
                &mut bytes,
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap(),
            4
        );
        assert_eq!(bytes, [0x11, 0x22, 0x33, 0x44]);
    });
}

#[test]
fn int10_invalidates_direct_data_pages_only_when_availability_changes() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    machine.direct_map_changed = false;
    machine.direct_data_map_changed = false;

    machine.cpu.registers.set_eax(0x0E41);
    machine.cpu.registers.set_ebx(0x000F);
    machine.handle_int10();
    assert!(
        !machine.direct_map_changed,
        "teletype output keeps the canonical Mode 13h mapping"
    );
    assert!(!machine.direct_data_map_changed);

    machine.cpu.registers.set_eax(0x0003);
    machine.handle_int10();
    assert!(
        machine.direct_data_map_changed,
        "leaving Mode 13h invalidates the direct mapping"
    );
    assert!(!machine.direct_map_changed);
}

#[test]
fn mode13_direct_write_materializes_before_mode_x() {
    const PROGRAM: &[u8] = &[
        0xB8, 0x00, 0xA0, // mov ax,A000h
        0x8E, 0xC0, // mov es,ax
        0xBF, 0x34, 0x12, // mov di,1234h
        0xB0, 0x6B, // mov al,6Bh
        0x26, 0x88, 0x05, // mov es:[di],al
        0xBA, 0xC4, 0x03, // mov dx,3C4h
        0xB0, 0x04, // mov al,04h
        0xEE, // out dx,al
        0x42, // inc dx
        0xB0, 0x06, // mov al,06h (chain-4 off)
        0xEE, // out dx,al
        0xCD, 0x20, // int 20h
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    machine.cpu.reset_perf_counters();

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.video().active_mode(), VideoMode::ModeX);
    assert_eq!(machine.video().plane_byte(0, 0x1234 >> 2), 0x6B);
    with_bus(&mut machine, |bus| {
        assert!(
            bus.direct_page(0xA_1234, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "unchained layout falls back to the planar handler"
        );
    });
    assert!(
        machine.cpu.perf_counters().direct_map_invalidations >= 2,
        "mode set and chain-4 transition each invalidate cached pages"
    );
}

#[test]
fn mode_x_direct_page_writes_one_plane_and_keeps_reads_on_the_handler() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    {
        let vga = machine.video_mut();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        vga.write_port(0x3C4, 0x02);
        vga.write_port(0x3C5, 0x04);
    }
    let content_before = machine.video().content_gen();
    let frame_before = machine.frame_generation();

    with_bus(&mut machine, |bus| {
        assert!(
            bus.direct_page(0xA_1234, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "Mode X reads must retain latch and read-map handling"
        );
        let page = bus
            .direct_page(0xA_1234, BusAccessKind::DataWrite)
            .unwrap()
            .expect("transparent single-plane Mode X write page");
        assert_eq!(page.physical_page, 0xA_1000);
        unsafe { page.ptr.add(0x234).write(0x5A) };
        bus.charge_native_vga_writes(NativeVgaWrites {
            dirty_pages: 1 << 1,
            byte_writes: 1,
            word_writes: 0,
            dword_writes: 0,
        });
        assert_eq!(bus.direct_memory_bytes(0xA_1240, 4, BusWidth::Dword), 4);
        let mut readback = [0; 4];
        assert_eq!(
            bus.read_memory_bytes_direct(
                0xA_1240,
                &mut readback,
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap(),
            0,
            "Mode X bulk reads must retain VGA latch handling"
        );
        assert_eq!(
            bus.write_memory_bytes_direct(
                0xA_1240,
                &[1, 2, 3, 4],
                BusWidth::Dword,
                BusAccessKind::DataWrite,
            )
            .unwrap(),
            4
        );
    });

    assert_eq!(machine.video().plane_byte(2, 0x1234), 0x5A);
    assert_eq!(
        &[
            machine.video().plane_byte(2, 0x1240),
            machine.video().plane_byte(2, 0x1241),
            machine.video().plane_byte(2, 0x1242),
            machine.video().plane_byte(2, 0x1243),
        ],
        &[1, 2, 3, 4]
    );
    assert_eq!(machine.video().plane_byte(0, 0x1234), 0);
    assert_eq!(machine.video().content_gen(), content_before + 1);
    assert_ne!(machine.frame_generation(), frame_before);
}

#[test]
fn mode_x_plane_switch_invalidates_only_vga_data_mappings() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    {
        let vga = machine.video_mut();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        vga.write_port(0x3C4, 0x02);
        vga.write_port(0x3C5, 0x01);
    }
    let decode_generation = machine.cpu.decode_cache_generation();

    with_bus(&mut machine, |bus| {
        *bus.direct_map_changed = false;
        *bus.direct_data_map_changed = false;
        bus.write_io(0x3C4, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0x3C5, BusWidth::Byte, 0x08, false).unwrap();
        assert!(!*bus.direct_map_changed);
        assert!(*bus.direct_data_map_changed);
        assert!(
            bus.direct_page(0xA_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_none()
        );
        assert!(
            bus.direct_page(0xA_0000, BusAccessKind::DataWrite)
                .unwrap()
                .is_some()
        );
    });

    machine.cpu.note_direct_data_map_changed();
    machine.direct_data_map_changed = false;
    assert_eq!(machine.cpu.decode_cache_generation(), decode_generation);
}

#[test]
fn pending_crtc_start_ends_a_lazy_ring0_direct_page_run() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    assert!(machine.set_vga_mode(0x13));

    with_bus(&mut machine, |bus| {
        *bus.direct_map_changed = false;
        *bus.direct_data_map_changed = false;
        *bus.io_touched = false;
        assert!(
            bus.direct_page(0xA_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_some()
        );

        bus.write_io(0x3D4, BusWidth::Byte, 0x0C, true).unwrap();
        bus.write_io(0x3D5, BusWidth::Byte, 0x01, true).unwrap();

        assert!(
            *bus.io_touched,
            "a VGA decode change ends even a lazy ring-0 batch"
        );
        assert!(!*bus.direct_map_changed);
        assert!(*bus.direct_data_map_changed);
        assert!(bus.requires_step_break());
        assert!(
            bus.direct_page(0xA_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "a pending noncanonical start address removes the direct page"
        );
    });
}

#[test]
fn mode13_direct_write_moves_frame_generation_once() {
    const PROGRAM: &[u8] = &[
        0xB8, 0x00, 0xA0, // mov ax,A000h
        0x8E, 0xC0, // mov es,ax
        0xBF, 0x34, 0x12, // mov di,1234h
        0xB0, 0x7C, // mov al,7Ch
        0x26, 0x88, 0x05, // mov es:[di],al
        0x47, // inc di
        0xB0, 0x7D, // mov al,7Dh
        0x26, 0x88, 0x05, // mov es:[di],al
        0xCD, 0x20, // int 20h
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    let content_before = machine.video().content_gen();
    let frame_before = machine.frame_generation();

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.video().content_gen(), content_before + 1);
    assert_ne!(machine.frame_generation(), frame_before);
    assert_eq!(machine.video().cpu_read_chain4(0x1234), 0x7C);
    assert_eq!(machine.video().cpu_read_chain4(0x1235), 0x7D);
}

#[test]
fn native_mode13_page_batches_charge_video_timing_and_move_generation_once() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    assert!(machine.set_vga_mode(0x13));
    let content_before = machine.video().content_gen();
    let frame_before = machine.frame_generation();

    with_bus(&mut machine, |bus| {
        let page0 = bus
            .direct_page(0xA_0000, BusAccessKind::DataWrite)
            .unwrap()
            .expect("canonical Mode 13h page 0 must be direct");
        let page15 = bus
            .direct_page(0xA_F000, BusAccessKind::DataWrite)
            .unwrap()
            .expect("canonical Mode 13h page 15 must be direct");
        unsafe {
            *page0.ptr = 0x2A;
            *page15.ptr.add(0x123) = 0x6B;
        }
        let clocks_before = bus.trace.elapsed_clocks();
        let byte_cost = bus.jit_mode13_data_cost_clocks(BusWidth::Byte);
        let dword_cost = bus.jit_mode13_data_cost_clocks(BusWidth::Dword);
        bus.charge_native_vga_writes(NativeVgaWrites {
            dirty_pages: 1,
            byte_writes: 1,
            word_writes: 0,
            dword_writes: 0,
        });
        bus.charge_native_vga_writes(NativeVgaWrites {
            dirty_pages: 1 << 15,
            byte_writes: 0,
            word_writes: 0,
            dword_writes: 1,
        });
        assert_eq!(
            bus.trace.elapsed_clocks() - clocks_before,
            byte_cost + dword_cost
        );
    });

    assert_eq!(machine.video().content_gen(), content_before + 1);
    assert_ne!(machine.frame_generation(), frame_before);
    assert_eq!(machine.video().cpu_read_chain4(0), 0x2A);
    assert_eq!(machine.video().cpu_read_chain4(0xF123), 0x6B);
}

#[test]
fn native_cached_fetch_batch_charges_the_exact_warm_ram_cost() {
    const FETCHES: u64 = 25_000;
    const FETCH_LENS: &[u8] = &[1, 3, 2, 4];
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);

    with_bus(&mut machine, |bus| {
        let clocks_before = bus.trace.elapsed_clocks();
        let fetch_cost = bus.jit_fetch_cost_clocks();
        assert!(bus.charge_native_cached_fetches(0xF_4000, 0x100, FETCH_LENS, FETCHES));
        assert_eq!(
            bus.trace.elapsed_clocks() - clocks_before,
            fetch_cost * FETCHES * FETCH_LENS.len() as u64
        );
    });
}

#[test]
fn native_deadline_bound_uses_the_same_bus_scale_as_batch_accounting() {
    const RAW_CLOCKS: u64 = 301;
    let mut machine = test_machine();

    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        machine.set_mode(mode);
        let (num, den) = bus_timing(mode.persona());
        let expected = RAW_CLOCKS
            .saturating_mul(u64::from(num))
            .saturating_add(u64::from(den) - 1)
            / u64::from(den);
        with_bus(&mut machine, |bus| {
            assert_eq!(bus.jit_scale_bus_cost_upper(RAW_CLOCKS), expected);
        });
    }
}

#[test]
fn native_cached_fetch_batch_observes_the_linear_stub_address() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    machine.last_int_vector = Some(0x10);

    with_bus(&mut machine, |bus| {
        assert!(bus.charge_native_cached_fetches(BIOS_LEGACY_IRET_LINEAR, 0x5000, &[1], 4,));
    });

    assert_eq!(machine.pending_soft_int, Some(0x10));
    assert_eq!(machine.last_int_vector, None);
}

const NATIVE_FETCH_LINEAR: u32 = 0xF_4000;
const NATIVE_FETCH_PHYSICAL: u32 = 0x5000;

fn arm_native_fetch_loop(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = NATIVE_FETCH_LINEAR;
    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_esp(0);
    cpu.registers.set_ebp(0);
    cpu.registers.set_esi(0);
    cpu.registers.set_edi(0);
    cpu.registers.eflags = 0x203;
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    cs.access = 0x9b;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
}

fn drive_native_fetch_loop(cpu: &mut CpuGsw, machine: &mut Machine) -> Vec<CycleOutcome> {
    let mut outcomes = Vec::new();
    for _ in 0..64 {
        let outcome = with_bus(machine, |bus| cpu.run_straight_line(bus, u64::MAX).unwrap());
        outcomes.push(outcome);
        if outcome.halted {
            return outcomes;
        }
    }
    panic!("native fetch loop did not halt");
}

#[test]
fn direct_large_self_loop_bulk_fetch_uses_physical_paging_alias_timing() {
    const ITERATIONS: u32 = 1_000;
    const PROGRAM: [u8; 16] = [
        0xb9, 0xe8, 0x03, 0x00, 0x00, // mov ecx,1000
        0x83, 0xc0, 0x03, // add eax,3
        0x89, 0xc2, // mov edx,eax
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz to the loop body
        0xf4,
    ];
    let mut interp_machine = test_machine();
    let mut native_machine = test_machine();
    interp_machine.set_mode(GswMode::Gsw586);
    native_machine.set_mode(GswMode::Gsw586);
    for machine in [&mut interp_machine, &mut native_machine] {
        machine.write_physical_u32(0x1000, 0x2007);
        machine.write_physical_u32(
            0x2000 + ((NATIVE_FETCH_LINEAR >> 12) & 0x3FF) * 4,
            NATIVE_FETCH_PHYSICAL | 7,
        );
    }
    for (offset, byte) in PROGRAM.into_iter().enumerate() {
        interp_machine.write_physical_u8(NATIVE_FETCH_PHYSICAL + offset as u32, byte);
        native_machine.write_physical_u8(NATIVE_FETCH_PHYSICAL + offset as u32, byte);
    }
    let mut interp_cpu = interp_machine.cpu.clone();
    let mut native_cpu = native_machine.cpu.clone();
    for cpu in [&mut interp_cpu, &mut native_cpu] {
        cpu.control.cr0 |= 0x8000_0001;
        cpu.control.cr3 = 0x1000;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x08,
                base: 0,
                limit: u32::MAX,
                access: 0x9b,
                default_size_32: true,
            },
        );
    }
    interp_cpu.set_jit_auto_admit(false);
    native_cpu.set_jit_auto_admit(true);

    for _ in 0..4 {
        arm_native_fetch_loop(&mut interp_cpu);
        arm_native_fetch_loop(&mut native_cpu);
        drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
        drive_native_fetch_loop(&mut native_cpu, &mut native_machine);
    }
    interp_machine.trace = BusTrace::default();
    native_machine.trace = BusTrace::default();
    arm_native_fetch_loop(&mut interp_cpu);
    arm_native_fetch_loop(&mut native_cpu);
    let traced_direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let traced_direct_entries = native_cpu.perf_counters().jit_direct_entries;

    let interp_traced_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_traced_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_traced_outcomes, interp_traced_outcomes);
    assert_eq!(native_machine.trace, interp_machine.trace);
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns,
        traced_direct_insns
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_entries,
        traced_direct_entries
    );

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    arm_native_fetch_loop(&mut interp_cpu);
    arm_native_fetch_loop(&mut native_cpu);
    let direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let direct_entries = native_cpu.perf_counters().jit_direct_entries;

    let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native_cpu, interp_cpu);
    assert_eq!(
        native_machine.trace.elapsed_clocks(),
        interp_machine.trace.elapsed_clocks()
    );
    assert_eq!(native_cpu.registers.eax(), ITERATIONS * 3);
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns - direct_insns,
        u64::from(ITERATIONS) * 4
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_entries - direct_entries,
        1
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn paged_fast_map_tlb_collision_keeps_interpreter_and_native_timing_equal() {
    const PAGE_DIRECTORY: u32 = 0x1000;
    const PAGE_TABLE: u32 = 0x2000;
    const WARM_CODE_LINEAR: u32 = 0x000f_4000;
    const WARM_CODE_PHYSICAL: u32 = 0x5000;
    const MEASURE_CODE_LINEAR: u32 = 0x000f_5000;
    const MEASURE_CODE_PHYSICAL: u32 = 0x8000;
    const LINEAR_A: u32 = 0x3000;
    const LINEAR_B: u32 = LINEAR_A + 64 * 0x1000;
    const FRAME_A: u32 = 0x6000;
    const FRAME_B: u32 = 0x7000;
    const VALUE_A: u32 = 0x1020_3040;
    const VALUE_B: u32 = 0x5566_7788;
    const PTE_A: u32 = PAGE_TABLE + ((LINEAR_A >> 12) & 0x3ff) * 4;
    const PTE_B: u32 = PAGE_TABLE + ((LINEAR_B >> 12) & 0x3ff) * 4;

    assert_eq!((LINEAR_A >> 12) & 63, (LINEAR_B >> 12) & 63);

    let mut warm_program = vec![0xa1];
    warm_program.extend_from_slice(&LINEAR_A.to_le_bytes());
    warm_program.push(0xa1);
    warm_program.extend_from_slice(&LINEAR_B.to_le_bytes());
    warm_program.push(0xf4);

    let mut program = vec![0x90, 0xa1];
    program.extend_from_slice(&LINEAR_A.to_le_bytes());
    program.extend_from_slice(&[
        0x85, 0xc0, // test eax,eax
        0x74, 0xf7, // jz back to the entry, not taken for VALUE_A
        0xf4, // hlt
    ]);

    let make_fixture = || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw486);
        machine.write_physical_u32(PAGE_DIRECTORY, PAGE_TABLE | 7);
        machine.write_physical_u32(
            PAGE_TABLE + ((WARM_CODE_LINEAR >> 12) & 0x3ff) * 4,
            WARM_CODE_PHYSICAL | 7,
        );
        machine.write_physical_u32(
            PAGE_TABLE + ((MEASURE_CODE_LINEAR >> 12) & 0x3ff) * 4,
            MEASURE_CODE_PHYSICAL | 7,
        );
        machine.write_physical_u32(PTE_A, FRAME_A | 7);
        machine.write_physical_u32(PTE_B, FRAME_B | 7);
        machine.write_physical_u32(FRAME_A, VALUE_A);
        machine.write_physical_u32(FRAME_B, VALUE_B);
        for (offset, byte) in warm_program.iter().copied().enumerate() {
            machine.write_physical_u8(WARM_CODE_PHYSICAL + offset as u32, byte);
        }
        for (offset, byte) in program.iter().copied().enumerate() {
            machine.write_physical_u8(MEASURE_CODE_PHYSICAL + offset as u32, byte);
        }
        machine.trace = BusTrace::default();
        machine.trace.set_tracing_mode(TracingMode::Full);
        machine
    };
    let mut interp_machine = make_fixture();
    let mut native_machine = make_fixture();

    let configure_cpu = |machine: &Machine| {
        let mut cpu = machine.cpu.clone();
        cpu.control.cr0 |= 0x8000_0001;
        cpu.control.cr3 = PAGE_DIRECTORY;
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
        cpu.set_jit_auto_admit(false);
        cpu
    };
    let mut interp_cpu = configure_cpu(&interp_machine);
    let mut native_cpu = configure_cpu(&native_machine);
    let arm = |cpu: &mut CpuGsw, eip: u32| {
        cpu.halted = false;
        cpu.registers.eip = eip;
        cpu.registers.set_eax(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_esp(0);
        cpu.registers.set_ebp(0);
        cpu.registers.set_esi(0);
        cpu.registers.set_edi(0);
        cpu.registers.eflags = 0x202;
    };

    for (cpu, machine) in [
        (&mut interp_cpu, &mut interp_machine),
        (&mut native_cpu, &mut native_machine),
    ] {
        arm(cpu, WARM_CODE_LINEAR);
        let outcomes = drive_native_fetch_loop(cpu, machine);
        assert!(outcomes.last().is_some_and(|outcome| outcome.halted));
        for pte in [PTE_A, PTE_B] {
            assert!(
                machine.trace.cycles().iter().any(|cycle| {
                    cycle.kind == BusAccessKind::PageWalkRead && cycle.address == pte
                }),
                "the cold warmup must walk PTE {pte:#x}"
            );
        }
    }

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    native_cpu.set_jit_auto_admit(true);
    for _ in 0..3 {
        interp_machine.write_physical_u32(FRAME_A, VALUE_A);
        native_machine.write_physical_u32(FRAME_A, VALUE_A);
        arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
        arm(&mut native_cpu, MEASURE_CODE_LINEAR);
        let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
        let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);
        assert_eq!(native_outcomes, interp_outcomes);
    }
    assert!(
        native_cpu.perf_counters().jit_direct_insns >= 3,
        "{:?}",
        native_cpu.perf_counters()
    );

    interp_machine.write_physical_u32(FRAME_A, VALUE_A);
    native_machine.write_physical_u32(FRAME_A, VALUE_A);
    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
    arm(&mut native_cpu, MEASURE_CODE_LINEAR);
    interp_cpu.elapsed_clocks = 0;
    native_cpu.elapsed_clocks = 0;
    let direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let direct_loads = native_cpu.perf_counters().jit_native_load_hits;

    let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native_cpu, interp_cpu);
    assert_eq!(
        native_machine.trace.elapsed_clocks(),
        interp_machine.trace.elapsed_clocks(),
        "production aggregate accounting must preserve raw bus clocks"
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns - direct_insns,
        3
    );
    assert_eq!(
        native_cpu.perf_counters().jit_native_load_hits - direct_loads,
        1,
        "the evicted first alias must be read by native code"
    );
    assert_eq!(
        native_machine.memory.as_slice(),
        interp_machine.memory.as_slice()
    );
    assert_eq!(
        interp_machine.memory.read_u32(FRAME_A as usize).unwrap(),
        VALUE_A
    );
    assert_eq!(
        interp_machine.memory.read_u32(FRAME_B as usize).unwrap(),
        VALUE_B
    );

    interp_machine.write_physical_u32(FRAME_A, VALUE_A);
    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Full);
    arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
    drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    assert!(
        interp_machine.trace.cycles().iter().all(|cycle| !matches!(
            cycle.kind,
            BusAccessKind::PageWalkRead | BusAccessKind::PageWalkWrite
        )),
        "the shared FastMap must survive the old 64-entry TLB collision"
    );
}

#[test]
fn ram_lookup_does_not_expose_partial_final_pages_as_full_pages() {
    let vega = Vega::default();
    let lookup = RamPageLookup::new(RAM_LOOKUP_PAGE_SIZE + 17, &vega);
    assert!(lookup.direct_bytes(0, RAM_LOOKUP_PAGE_SIZE).is_some());
    assert!(
        lookup
            .direct_bytes(RAM_LOOKUP_PAGE_SIZE as u32, RAM_LOOKUP_PAGE_SIZE)
            .is_none(),
        "a final partial page cannot back a full direct-page pointer"
    );
}

#[test]
#[ignore]
fn ram_lookup_profile() {
    let iters = std::env::var("IZARRAVM_PROFILE_ITERS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(5_000_000);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();

    for i in 0..1024u32 {
        machine.write_physical_u8(0x2000 + i, i as u8);
        machine.write_physical_u8(0x10_0000 + i, i as u8);
    }

    fn report(label: &str, iters: u32, mut body: impl FnMut(u32) -> u32) -> u32 {
        let t = std::time::Instant::now();
        let mut checksum = 0u32;
        for i in 0..iters {
            checksum = checksum
                .wrapping_add(std::hint::black_box(body(i)).rotate_left(i & 31))
                .wrapping_add(i);
        }
        let secs = t.elapsed().as_secs_f64();
        let ns = secs * 1.0e9 / f64::from(iters);
        println!(
            "{label:<32} {ns:>8.2} ns/op  {:>8.1} Mops/s  checksum={checksum:#010x}",
            f64::from(iters) / secs / 1.0e6
        );
        checksum
    }

    with_bus(&mut machine, |bus| {
        println!("ram_lookup_profile: {iters} iterations");
        assert!(bus.direct_ram_bytes(0x10_0000, 4).is_some());
        assert!(bus.direct_ram_bytes(LOW_BIOS_BASE, 4).is_none());

        let low = report("lookup low RAM", iters, |i| {
            let (start, _) = bus.direct_ram_bytes(0x2000 + ((i & 0xff) << 2), 4).unwrap();
            start as u32
        });
        let high = report("lookup extended RAM", iters, |i| {
            let (start, _) = bus
                .direct_ram_bytes(0x10_0000 + ((i & 0xff) << 2), 4)
                .unwrap();
            start as u32
        });
        let slow = report("lookup ROM miss", iters, |i| {
            u32::from(
                bus.direct_ram_bytes(LOW_BIOS_BASE + ((i & 0xff) << 2), 4)
                    .is_some(),
            )
        });
        let read_low = report("bus read low RAM", iters, |i| {
            bus.read_memory(
                0x2000 + ((i & 0xff) << 2),
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        });
        let read_high = report("bus read extended RAM", iters, |i| {
            bus.read_memory(
                0x10_0000 + ((i & 0xff) << 2),
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        });
        let read_rom = report("bus read ROM", iters, |i| {
            bus.read_memory(
                LOW_BIOS_BASE + ((i & 0xff) << 2),
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        });

        std::hint::black_box((low, high, slow, read_low, read_high, read_rom));
    });
}

#[test]
fn rtc_ports_round_trip_through_the_bus() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x70, BusWidth::Byte, 0x00, false).unwrap(); // select seconds
        bus.write_io(0x71, BusWidth::Byte, 42, false).unwrap();
        bus.write_io(0x70, BusWidth::Byte, 0x00, false).unwrap();
        let secs = bus.read_io(0x70 + 1, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(secs, 42);
    });
}

#[test]
fn rtc_advances_seconds_on_the_machine_clock() {
    let mut machine = test_machine();
    machine.seed_rtc(2026, 6, 20, 6, 12, 0, 0);
    // Step roughly three seconds of emulated time, in ~10 ms chunks so the
    // sub-second accumulator carries the way it does during a real run.
    let clock_hz = machine.active_mode.clock_rate().floor_hz();
    let chunk = clock_hz / 100; // ~10 ms
    for _ in 0..300 {
        machine.advance_devices_clocks(chunk);
    }
    let bytes = machine.cmos_bytes();
    // Seconds register (0x00) should have advanced to about 3.
    assert!(
        (2..=4).contains(&bytes[0x00]),
        "expected the seconds register near 3, got {}",
        bytes[0x00]
    );
}

#[test]
fn cmos_persists_and_reloads_via_bytes() {
    let mut machine = test_machine();
    // Guest writes a layout byte and a boot-order byte, then refreshes the
    // checksum the way the setup page would.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x70, BusWidth::Byte, 0x10, false).unwrap();
        bus.write_io(0x71, BusWidth::Byte, 3, false).unwrap(); // FR layout
        bus.write_io(0x70, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x71, BusWidth::Byte, 1, false).unwrap(); // disk-first
    });
    assert!(
        machine.take_cmos_dirty(),
        "an NVRAM write should mark dirty"
    );
    let saved = machine.cmos_bytes();

    // A fresh machine loads the saved image and reads the same bytes back.
    let mut other = test_machine();
    other.load_cmos(&saved);
    assert_eq!(other.cmos_bytes()[0x10], 3);
    assert_eq!(other.cmos_bytes()[0x11], 1);
}

#[test]
fn pc_speaker_renders_a_square_wave() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap(); // divisor low
        bus.write_io(0x42, BusWidth::Byte, 0x04, false).unwrap(); // divisor high (0x0400)
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data enable
    });
    let clock_hz = machine.active_mode.clock_rate().floor_hz();
    let chunk = clock_hz / 100_000; // ~10 us, mimicking per-instruction advance
    for _ in 0..2_000 {
        machine.advance_devices_clocks(chunk); // ~20 ms total
    }
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
    assert!(
        pcm.iter().any(|&(l, _)| l > 0) && pcm.iter().any(|&(l, _)| l < 0),
        "a toggling speaker tone should produce both polarities"
    );
}

#[test]
fn pc_speaker_ultrasonic_square_wave_averages_quietly() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, 0x02, false).unwrap(); // divisor low
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap(); // divisor high
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data enable
    });
    let clock_hz = machine.active_mode.clock_rate().floor_hz();
    let chunk = clock_hz / 100_000; // ~10 us, mimicking per-instruction advance
    for _ in 0..2_000 {
        machine.advance_devices_clocks(chunk); // ~20 ms total
    }
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
    let peak = pcm
        .iter()
        .map(|&(l, r)| i32::from(l).abs().max(i32::from(r).abs()))
        .max()
        .unwrap_or(0);
    assert!(
        peak < 1_200,
        "an ultrasonic PIT2 square wave should average down instead of aliasing at full scale, peak {peak}"
    );
}

#[test]
fn port_61_reports_out_gate_enable_and_refresh() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
    });
    let clock_hz = machine.active_mode.clock_rate().floor_hz();
    machine.advance_devices_clocks(clock_hz / 100_000); // ~10 us
    let b = with_bus(&mut machine, |bus| {
        bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(
        (b >> 5) & 1,
        u8::from(machine.pit.channel_out(2)),
        "bit 5 = ch2 OUT"
    );
    assert_eq!(b & 0x03, 0x03, "bits 0,1 read back GATE2 + data enable");

    // Bit 4 is now PIT channel 1 OUT (the AT DRAM-refresh timer, mode 2),
    // pre-seeded at power-on. This guest never programmed channel 1, yet the
    // bit must still toggle. Mode 2 pulses OUT low for one input clock per
    // refresh period, so over a couple of periods sampled finely bit 4 reads
    // both high (the bulk) and low (the short pulse).
    let mut saw_high = false;
    let mut saw_low = false;
    // Advance one PIT input clock at a time; one CPU step worth of clocks is
    // clock_hz / PIT_INPUT_HZ, so step that to move roughly one PIT tick.
    let per_pit_clock = (clock_hz / u64::from(PIT_INPUT_HZ)).max(1);
    for _ in 0..40 {
        machine.advance_devices_clocks(per_pit_clock);
        let bit4 = with_bus(&mut machine, |bus| {
            (bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap() as u8 >> 4) & 1
        });
        if bit4 == 1 {
            saw_high = true;
        } else {
            saw_low = true;
        }
    }
    assert!(
        saw_high,
        "refresh bit (4) reads high for the bulk of a period"
    );
    assert!(
        saw_low,
        "refresh bit (4) pulses low once per refresh period"
    );
}
