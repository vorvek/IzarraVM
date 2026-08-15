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
        assert!(machine.set_vga_mode(0x13));
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

            for kind in [BusAccessKind::DataRead, BusAccessKind::DataWrite] {
                assert!(
                    bus.direct_page(0xA_1000, kind).unwrap().is_some(),
                    "{mode:?}: canonical Mode 13h must expose a direct page"
                );
                let before = bus.trace.elapsed_clocks();
                bus.charge_direct_memory(0xA_1234, BusWidth::Byte, kind)
                    .unwrap();
                let charged = bus.trace.elapsed_clocks() - before;
                assert!(
                    charged <= data_bound,
                    "{mode:?} {kind:?}: VGA charged {charged}, bound {data_bound}"
                );
            }
        });
    }
}

#[test]
fn accurate_direct_memory_preview_includes_custom_video_wait_states() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.wait_states.video = 123;
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    assert!(machine.set_vga_mode(0x13));

    with_bus(&mut machine, |bus| {
        let bound = bus
            .jit_direct_memory_max_clocks(BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        let before = bus.trace.elapsed_clocks();
        bus.charge_direct_memory(0xA_1200, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        assert_eq!(bus.trace.elapsed_clocks() - before, bound);
    });
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

/// The Margo arm of `predicted_beam_after_n_clocks_matches_a_real_advance_devices_of_the_same_n`.
///
/// While a VBE mode owns the display the beam is no longer the VGA raster's; it
/// is the timeline's Margo frame phase scaled into that mode's dots. That is a
/// SECOND formula, and it carries the same obligation as the first: a mid-batch
/// peek must equal a real `advance_devices` of the same clock total, because the
/// lazy 0x3DA arm answers a guest from the peek and never from device state.
///
/// The sweep deliberately reaches past one 60 Hz frame (`420_000` dots at
/// 640x480, and a 386 at 16 MHz covers that in ~1.1M clocks), so the
/// frame-crossing case -- the one where a wrong formula drifts instead of merely
/// being offset -- is really exercised; `any_wrap` fails the test if it is not.
#[test]
fn margo_predicted_beam_after_n_clocks_matches_a_real_advance_devices_of_the_same_n() {
    let mut any_wrap = false;
    for prior_runs_core_clocks in [0u64, 61, 33_000] {
        for core_clocks_so_far in [0u64, 100, 12_345, 450_000, 1_500_000] {
            let mut predicted_machine = margo_test_machine();
            let mut real_machine = margo_test_machine();
            assert_eq!(predicted_machine.timeline, real_machine.timeline);

            let frame_dots = real_machine
                .vega
                .margo_scanout()
                .expect("a VBE mode must be active")
                .frame_dots();

            let predicted = with_bus(&mut predicted_machine, |bus| {
                bus.prior_runs_core_clocks = prior_runs_core_clocks;
                bus.core_clocks_so_far = core_clocks_so_far;
                bus.predicted_beam()
            });

            let step = prior_runs_core_clocks + core_clocks_so_far;
            // Whole frames this step will cross, computed from the same pure
            // formula BEFORE the mutating advance: `dots` is
            // `frames * frame_dots + beam` with `beam < frame_dots`, so the
            // quotient IS the frame count.
            let frames_crossed = real_machine
                .timeline
                .preview_margo_scanout(step, frame_dots)
                .dots
                / frame_dots;
            real_machine.advance_devices(step);
            if frames_crossed > 0 {
                any_wrap = true;
            }

            assert_eq!(
                predicted,
                real_machine.scanout_beam_dots(),
                "predicted_beam(prior={prior_runs_core_clocks}, core={core_clocks_so_far}) must \
                 match a real advance_devices of the same clock total while Margo \
                 owns the display"
            );
        }
    }
    assert!(
        any_wrap,
        "the sweep must cross a Margo frame boundary, or the wrap coverage is hollow"
    );
}

/// The peek's OTHER half: `dots_until_status1_bit_change_from` must name the
/// exact edge, not an approximate one. One dot short of the answer the bit still
/// reads its old value; at the answer it reads the target. That is the property
/// the JIT poll-skip search leans on -- it skips iterations up to but not across
/// this distance -- so an off-by-one here is a guest that oversleeps a retrace.
#[test]
fn margo_status1_edge_peek_names_the_exact_dot() {
    let machine = margo_test_machine();
    let scan = machine
        .vega
        .margo_scanout()
        .expect("a VBE mode must be active");
    let frame_dots = scan.frame_dots();

    let mut edges_checked = 0;
    for start in [0u64, 1, 12_345, 400_000, 419_999] {
        for bit in [0u8, 3] {
            for target in [false, true] {
                let beam = start % frame_dots;
                let mask = 1u8 << bit;
                let current = scan.status1_bits(beam) & mask != 0;
                let Some(dots) = scan.dots_until_status1_bit_change_from(beam, bit, target) else {
                    // `None` is only legitimate when the bit is already there.
                    assert_eq!(
                        current, target,
                        "no edge was offered for bit {bit} -> {target} from beam \
                         {beam}, but the bit is not already {target}"
                    );
                    continue;
                };
                assert!(dots >= 1, "an edge distance must make progress");
                assert_eq!(
                    scan.status1_bits((beam + dots - 1) % frame_dots) & mask != 0,
                    current,
                    "one dot before the edge, bit {bit} must still read its old value (beam {beam}, dots {dots})"
                );
                assert_eq!(
                    scan.status1_bits((beam + dots) % frame_dots) & mask != 0,
                    target,
                    "at the edge, bit {bit} must read {target} (beam {beam}, dots {dots})"
                );
                edges_checked += 1;
            }
        }
    }
    assert!(
        edges_checked >= 8,
        "the sweep must exercise real edges in both directions on both bits, got \
         {edges_checked}"
    );
}

/// The display owner is NOT pinned for the length of a batch, and this branch is
/// what made that matter. `beam_at_batch_start` is captured in the dot unit of
/// whichever engine owns the display at batch entry, and a Margo dot and a VGA
/// dot are both a bare `u64`. `Distira::display_enabled` moves on a plain MMIO
/// write to FBIINIT0, which is a bus-side write in the MIDDLE of a batch -- so a
/// guest can hand the display from Margo to Distira with no batch boundary in
/// between, and tomb-raider-3dfx does precisely that after probing 101h.
///
/// Without the owner snapshot, `predicted_beam` would spend the rest of that
/// batch folding in-batch VGA dots onto a MARGO anchor (up to 1,083,263 at mode
/// 0x105) and reducing it modulo the LEGACY frame -- a wrong beam, and therefore
/// wrong 0x3DA / 0x3C2 bits, for every read until the batch ended.
///
/// Differential form, the same discipline as the peek twin above: the predicted
/// beam after the handover must equal a real `advance_devices` of the same clock
/// total read off the live VGA raster. The setup deliberately leaves the two
/// engines' beams far apart, so an anchor taken from the wrong one cannot
/// coincide with the right answer.
#[test]
fn predicted_beam_survives_a_mid_batch_handover_of_the_display() {
    const STEP: u64 = 12_345;

    let mut predicted_machine = distira_handover_machine();
    let mut real_machine = distira_handover_machine();
    assert_eq!(predicted_machine.timeline, real_machine.timeline);

    let margo_beam = predicted_machine.scanout_beam_dots();
    let vga_beam = predicted_machine.vega.beam_dots();
    assert!(
        margo_beam.abs_diff(vga_beam) > 1_000,
        "the two engines' beams must be far apart for this to be able to fail \
         (margo {margo_beam}, vga {vga_beam})"
    );

    let (predicted, raw_bus_clocks) = with_bus(&mut predicted_machine, |bus| {
        assert!(
            bus.vega.margo_scanout().is_some(),
            "the batch must START with Margo owning the display"
        );
        let before = bus.trace.elapsed_clocks();
        enable_distira_display(bus);
        // The handover write charges bus clocks of its own, and the prediction
        // folds them in. Read them back rather than assuming a cost, so the
        // real leg below advances by the SAME total.
        let raw_bus_clocks = bus.trace.elapsed_clocks() - before;
        assert!(raw_bus_clocks > 0, "the MMIO write must charge bus time");
        assert!(
            bus.vega.margo_scanout().is_none(),
            "the FBIINIT0 write must have handed the display over MID-BATCH"
        );
        bus.core_clocks_so_far = STEP;
        (bus.predicted_beam(), raw_bus_clocks)
    });

    // The real machine takes the same handover and then really advances.
    with_bus(&mut real_machine, enable_distira_display);
    let step = STEP + real_machine.scale_bus(raw_bus_clocks);
    real_machine.advance_devices(step);

    assert_eq!(
        predicted,
        real_machine.vega.beam_dots(),
        "after the display changes hands mid-batch the prediction must follow \
         the VGA raster, not a stale Margo anchor"
    );

    // WHY THE OPPOSITE MUTATION IS UNDETECTABLE, stated rather than left as a
    // hole in the ledger. Hardwiring the owner flag TRUE changes nothing on a
    // batch that started VGA-owned, because the fallback does not compute a
    // DIFFERENT anchor there -- it re-derives the same one. Devices advance only
    // at batch end, so the VGA raster's beam is constant for the life of a bus
    // and equals what was captured. The flag chooses between two values that are
    // equal whenever it is false, which is exactly why it is safe.
    let mut legacy = test_machine();
    legacy.run_cycles(5_000).unwrap();
    assert!(legacy.vega.margo_scanout().is_none());
    with_bus(&mut legacy, |bus| {
        assert!(!bus.margo_scanout_at_batch_start);
        assert_eq!(bus.beam_at_batch_start, bus.vega.beam_dots());
    });
}

/// A 386 machine in VBE mode 0x105 (1,083,264 dots per frame, comfortably wider
/// than the legacy 640x400 raster) with Distira's init writes unlocked, run far
/// enough that the Margo and VGA beams have drifted apart.
fn distira_handover_machine() -> Machine {
    let mut machine = test_machine();
    machine.run_cycles(5_000).unwrap();
    with_bus(&mut machine, |bus| {
        // PCI config 0x40 = initEnable, the write-protect on the FBIINIT
        // registers. Without it the FBIINIT0 write below is silently dropped.
        let address = 0x8000_0000 | (u32::from(DISTIRA_PCI_SLOT) << 11) | 0x40;
        bus.write_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, address, false)
            .unwrap();
        bus.write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, 1, false)
            .unwrap();
    });
    assert!(machine.vega.set_vbe_mode(0x0105));
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    machine.advance_devices_clocks(9_999);
    machine
}

/// Clear FBIINIT0's VGA_PASS bit through the bus, which is what hands the
/// display from whatever owns it to Distira.
fn enable_distira_display(bus: &mut MachineBus<'_>) {
    bus.write_memory(
        DISTIRA_MMIO_BASE + izarravm_video::SST_FBI_INIT0 as u32,
        BusWidth::Dword,
        0,
        BusAccessKind::DataWrite,
    )
    .unwrap();
}

/// A 386 test machine sitting in VBE mode 0x101 (640x480x8, BANKED -- the window
/// GP2 was measured asking for). The mode is set through `Vega::set_vbe_mode`,
/// the same host-side entry point `INT 10h 4F02h` funnels into, rather than by
/// running guest code: these tests are about the beam formula, and driving a ROM
/// stub would put an arbitrary number of cycles between the mode set and the
/// measurement. Tests that need the guest path itself are in the Margo file.
fn margo_test_machine() -> Machine {
    let mut machine = test_machine();
    machine.run_cycles(5_000).unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    machine
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

/// Program channel 0 as a mode-2 rate generator with an 18.2 Hz-ish divisor, the
/// shape a BIOS leaves behind and a calibration loop latches.
fn program_channel0_mode2(machine: &mut Machine, reload: u16) {
    with_bus(machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap(); // ch0, lo/hi, mode 2, binary
        bus.write_io(0x40, BusWidth::Byte, u32::from(reload & 0xff), false)
            .unwrap();
        bus.write_io(0x40, BusWidth::Byte, u32::from(reload >> 8), false)
            .unwrap();
    });
}

/// Latch channel 0 and read both halves back at the current in-batch offset.
fn latch_and_read_channel0(machine: &mut Machine, prior: u64, core: u64) -> (u16, u64) {
    with_bus(machine, |bus| {
        bus.prior_runs_core_clocks = prior;
        bus.core_clocks_so_far = core;
        let before = bus.trace.elapsed_clocks();
        bus.write_io(0x43, BusWidth::Byte, 0x00, false).unwrap(); // counter-latch, ch0
        // The latch peek is taken AFTER read_io/write_io records this access's own
        // bus time, so the raw total the peek converted is everything recorded
        // since the bus was built.
        let raw_bus_clocks = bus.trace.elapsed_clocks() - before;
        let lo = bus.read_io(0x40, BusWidth::Byte, core, false).unwrap() as u8;
        let hi = bus.read_io(0x40, BusWidth::Byte, core, false).unwrap() as u8;
        (u16::from_le_bytes([lo, hi]), raw_bus_clocks)
    })
}

#[test]
fn a_mid_batch_counter_latch_matches_a_real_advance_devices_of_the_same_clocks() {
    // The counter-value counterpart of
    // predicted_pit_out_after_n_clocks_matches_a_real_advance_devices_of_the_same_n,
    // and the test that retires the "counter reads are batch-start stale" caveat:
    // a latch taken partway into a batch must equal the value a real
    // advance_devices of the same clock total, followed by a latch at zero offset,
    // produces. Both timing classes, since this peek is not gated on
    // lazy_port_reads: the Accurate 386 class is the one whose batch grain the
    // deadline work coarsened, so it is the one that most needs the peek.
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        for prior in [0u64, 61, 33_000] {
            for core in [0u64, 100, 12_345, 450_000] {
                let mut predicted_machine = test_machine();
                predicted_machine.set_mode(mode);
                program_channel0_mode2(&mut predicted_machine, 0x4000);
                predicted_machine.run_cycles(5_000).unwrap();

                let mut real_machine = test_machine();
                real_machine.set_mode(mode);
                program_channel0_mode2(&mut real_machine, 0x4000);
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.timeline, real_machine.timeline);

                let (predicted, raw_bus_clocks) =
                    latch_and_read_channel0(&mut predicted_machine, prior, core);

                let step = prior + core + real_machine.scale_bus(raw_bus_clocks);
                real_machine.advance_devices(step);
                let (real, _) = latch_and_read_channel0(&mut real_machine, 0, 0);

                assert_eq!(
                    predicted, real,
                    "mode {mode:?} prior {prior} core {core}: a mid-batch latch must \
                     equal a real advance_devices of the same total"
                );
            }
        }
    }
}

#[test]
fn a_mid_batch_counter_latch_moves_with_the_in_batch_offset() {
    // Non-vacuity for the test above: without the peek every latch in a batch
    // returns the same batch-start value, so the two offsets below would be equal.
    // 450_000 clocks is ~20 ms at the 386 tier -- far more than one coarse batch --
    // and the mode-2 counter must have visibly walked in between.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    program_channel0_mode2(&mut machine, 0x4000);
    machine.run_cycles(5_000).unwrap();
    let (at_batch_start, _) = latch_and_read_channel0(&mut machine, 0, 0);
    let (mid_batch, _) = latch_and_read_channel0(&mut machine, 0, 450_000);
    assert_ne!(
        at_batch_start, mid_batch,
        "a latch 450k clocks into the batch must not report the batch-start count"
    );
}

/// Program channel 2 as a mode-3 square wave and leave port 0x61 in the classic
/// "PIT timing, no sound" configuration: GATE2 HIGH (bit 0 set), data enable LOW
/// (bit 1 clear). `speaker.data_enabled()` is false there, so
/// `fine_batch_grain_required` does not hold the batch fine for it, and a guest
/// that never touches 0x40-0x43 again also falls out of the PIT-observer window.
fn program_silent_channel2(machine: &mut Machine, reload: u16) {
    with_bus(machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, u32::from(reload & 0xff), false)
            .unwrap();
        bus.write_io(0x42, BusWidth::Byte, u32::from(reload >> 8), false)
            .unwrap();
        bus.write_io(0x61, BusWidth::Byte, 0x01, false).unwrap(); // GATE2, no data enable
    });
    assert!(
        !machine.speaker.data_enabled(),
        "sanity: this configuration must NOT arm the speaker term of \
         fine_batch_grain_required"
    );
}

/// Read port 0x61 at a given in-batch offset, returning the byte and the raw bus
/// clocks the access itself recorded (the same accounting `latch_and_read_channel0`
/// uses, so a differential can advance the twin machine by the identical total).
fn read_port_61_at(machine: &mut Machine, prior: u64, core: u64) -> (u8, u64) {
    with_bus(machine, |bus| {
        bus.prior_runs_core_clocks = prior;
        bus.core_clocks_so_far = core;
        let before = bus.trace.elapsed_clocks();
        let value = bus.read_io(0x61, BusWidth::Byte, core, false).unwrap() as u8;
        (value, bus.trace.elapsed_clocks() - before)
    })
}

#[test]
fn a_mid_batch_61_read_matches_a_real_advance_devices_of_the_same_clocks() {
    // The 0x61 counterpart of
    // a_mid_batch_counter_latch_matches_a_real_advance_devices_of_the_same_clocks.
    // Both timing classes: the `out_after` peek on bits 4/5 is taken in each, so
    // the Accurate class's channel-2 OUT is no longer a batch-start read even in
    // the silent-timing configuration the fine-grain gate does not cover.
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        for prior in [0u64, 61, 33_000] {
            // 100_000 is included deliberately: it is an offset where CHANNEL 2's
            // OUT has flipped relative to batch start (the other offsets move only
            // channel 1's ~15 us refresh bit), so the sweep exercises both bits.
            for core in [0u64, 100, 12_345, 100_000, 450_000] {
                let mut predicted_machine = test_machine();
                predicted_machine.set_mode(mode);
                program_silent_channel2(&mut predicted_machine, 0x2000);
                predicted_machine.run_cycles(5_000).unwrap();

                let mut real_machine = test_machine();
                real_machine.set_mode(mode);
                program_silent_channel2(&mut real_machine, 0x2000);
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.timeline, real_machine.timeline);

                let (predicted, raw_bus_clocks) =
                    read_port_61_at(&mut predicted_machine, prior, core);
                let step = prior + core + real_machine.scale_bus(raw_bus_clocks);
                real_machine.advance_devices(step);
                let (real, _) = read_port_61_at(&mut real_machine, 0, 0);
                assert_eq!(
                    predicted, real,
                    "mode {mode:?} prior {prior} core {core}: a mid-batch 0x61 read must \
                     equal a real advance_devices of the same total"
                );
            }
        }
    }
}

#[test]
fn a_mid_batch_61_read_moves_with_the_in_batch_offset_on_the_accurate_class() {
    // Non-vacuity for the test above, on the class that previously read the LIVE
    // level: without the peek every 0x61 read in a batch reports the same
    // batch-start bit 5, so the sweep below would be constant. The offsets span
    // ~20 ms at the 386 tier, far more than the 1 ms coarse cap the silent-timing
    // configuration falls back to.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    program_silent_channel2(&mut machine, 0x2000);
    machine.run_cycles(5_000).unwrap();
    let mut levels = std::collections::BTreeSet::new();
    for core in (0u64..=450_000).step_by(50_000) {
        let (value, _) = read_port_61_at(&mut machine, 0, core);
        levels.insert((value >> 5) & 1);
    }
    assert_eq!(
        levels.len(),
        2,
        "bit 5 (channel-2 OUT) must take both levels across a sweep of in-batch \
         offsets; a constant column means the peek is not being taken"
    );
}

/// Read a VGA status port at a given in-batch offset, returning the byte and the
/// raw bus clocks the access itself recorded (same accounting as
/// `read_port_61_at`, so a differential can advance the twin by the identical
/// total).
fn read_status_port_at(machine: &mut Machine, port: u16, prior: u64, core: u64) -> (u8, u64) {
    with_bus(machine, |bus| {
        bus.prior_runs_core_clocks = prior;
        bus.core_clocks_so_far = core;
        let before = bus.trace.elapsed_clocks();
        let value = bus.read_io(port, BusWidth::Byte, core, false).unwrap() as u8;
        (value, bus.trace.elapsed_clocks() - before)
    })
}

#[test]
fn a_mid_batch_3da_read_matches_a_real_advance_devices_of_the_same_clocks() {
    // The VGA-beam counterpart of
    // a_mid_batch_counter_latch_matches_a_real_advance_devices_of_the_same_clocks
    // and a_mid_batch_61_read_matches_a_real_advance_devices_of_the_same_clocks,
    // and the test that retires the "3DA reports the batch-start beam" caveat.
    //
    // The 386 (Accurate) tier is the case that needed it: `lazy_ports_386` is
    // DEFAULT OFF, so this arm sets io_touched and ends the batch exactly as it
    // always has, and before the beam peek it read the LIVE beam -- the beam as
    // of BATCH START. Nothing bounds that staleness once
    // `fine_batch_grain_required` gates the fine fallback off (no term in that
    // gate is armed by a display poll, and `vega_edge_ticks` carries the Margo
    // blit and DISPLAY_START terms, not a retrace edge), so the read could be a
    // full 1 ms coarse batch old. Gsw486 is swept alongside to pin that the
    // Approximate class, which reaches the value by the lazy arm instead, agrees
    // with the same oracle.
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        for prior in [0u64, 61, 33_000] {
            for core in [0u64, 100, 12_345, 100_000, 450_000] {
                let mut predicted_machine = test_machine();
                predicted_machine.set_mode(mode);
                assert!(predicted_machine.set_vga_mode(0x13));
                predicted_machine.run_cycles(5_000).unwrap();

                let mut real_machine = test_machine();
                real_machine.set_mode(mode);
                assert!(real_machine.set_vga_mode(0x13));
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.timeline, real_machine.timeline);

                let (predicted, predicted_raw) =
                    read_status_port_at(&mut predicted_machine, 0x3da, prior, core);
                // The step is exactly the in-batch offset, with NO term for the
                // access's own bus time. Unlike the PIT ports, a VGA status read
                // is charged video wait states, and that charge is recorded
                // before the peek on BOTH machines -- so it is already inside
                // each read's own `in_batch_clocks` and cancels. Adding it to the
                // step would advance the oracle by it twice, which bit 0
                // (display enable, one toggle per ~700 clocks at this tier) is
                // sharp enough to catch. The equality below is asserted, not
                // assumed.
                real_machine.advance_devices(prior + core);
                let (real, real_raw) = read_status_port_at(&mut real_machine, 0x3da, 0, 0);
                assert_eq!(
                    predicted_raw, real_raw,
                    "mode {mode:?}: the two reads must charge the same bus time for \
                     the own-charge term to cancel"
                );

                assert_eq!(
                    predicted, real,
                    "mode {mode:?} prior {prior} core {core}: a mid-batch 3DA read must \
                     equal a real advance_devices of the same total"
                );
            }
        }
    }
}

#[test]
fn a_mid_batch_3da_read_moves_with_the_in_batch_offset_on_the_accurate_class() {
    // Non-vacuity for the test above, on the class that previously read the LIVE
    // beam: without the peek every 3DA read in a batch reports the same
    // batch-start bits, so both columns below would be constant. Bit 3 is
    // vertical retrace and bit 0 is display-enable-inverted; the offsets span
    // more than a whole 70 Hz frame at the 386 tier, so each must take both
    // levels.
    //
    // The STEP matters and is why this is not a 10k-clock sweep: vertical
    // retrace is only a couple of scanlines (~64 us, ~1400 clocks at this
    // tier), so a coarse sweep can stride over the retrace window entirely and
    // report a constant bit 3 whether or not the peek is being taken -- a
    // fixture that cannot fail. 250 clocks is well inside both the retrace
    // window and one ~700-clock scanline.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    assert!(machine.set_vga_mode(0x13));
    machine.run_cycles(5_000).unwrap();
    let mut vretrace = std::collections::BTreeSet::new();
    let mut display_enable = std::collections::BTreeSet::new();
    for core in (0u64..=450_000).step_by(250) {
        let (value, _) = read_status_port_at(&mut machine, 0x3da, 0, core);
        vretrace.insert((value >> 3) & 1);
        display_enable.insert(value & 1);
    }
    assert_eq!(
        vretrace.len(),
        2,
        "bit 3 (vertical retrace) must take both levels across a sweep of in-batch \
         offsets; a constant column means the beam peek is not being taken"
    );
    assert_eq!(
        display_enable.len(),
        2,
        "bit 0 (display enable) must take both levels across a sweep of in-batch offsets"
    );
}

#[test]
fn the_accurate_3da_arm_still_ends_the_batch() {
    // The beam peek changes the VALUE only. `lazy_ports_386` is default OFF, so
    // the Accurate class must still set io_touched on a 3DA read exactly as it
    // did before the peek -- otherwise this would silently become the lazy
    // behavior the `IZARRAVM_LAZY_PORT_386` switch exists to keep opt-in, and
    // every 386 fixture's batch shape would move.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    assert!(machine.set_vga_mode(0x13));
    machine.run_cycles(5_000).unwrap();
    let touched = with_bus(&mut machine, |bus| {
        assert!(
            !bus.lazy_ports_386,
            "sanity: this test covers the DEFAULT (non-lazy) Accurate arm"
        );
        *bus.io_touched = false;
        bus.read_io(0x3da, BusWidth::Byte, 0, false).unwrap();
        *bus.io_touched
    });
    assert!(touched, "a 386-tier 3DA read must still end the batch");
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
fn opl_status_read_matches_the_live_byte_when_no_device_time_is_pending() {
    // The Approximate class predicts the status byte from un-applied device
    // time (see `predicted_opl_status`). With NOTHING pending -- which is the
    // case here, because run_cycles below commits the advance before the read
    // -- the prediction must reduce exactly to the live byte: `expired_after(0)`
    // is the live `expired` flag, since `advance` leaves `accumulated_us` below
    // one step. Pin that equality, and the batch-ending behaviour with it.
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
        assert!(*bus.io_touched);
        assert!(bus.requires_step_break());
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
            bus.direct_memory_bytes(0x2ff0, 16, BusWidth::Byte, BusAccessKind::DataRead),
            16,
            "same-page RAM span is direct"
        );

        assert_eq!(
            bus.direct_memory_bytes(0x2fff, 2, BusWidth::Byte, BusAccessKind::DataRead),
            0,
            "cross-page spans fall back"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2001, 2, BusWidth::Word, BusAccessKind::DataRead),
            0,
            "split word spans fall back"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2000, 3, BusWidth::Word, BusAccessKind::DataRead),
            0,
            "partial-width RAM spans fall back"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2000, 4, BusWidth::Dword, BusAccessKind::PageWalkRead),
            0,
            "non-data RAM spans fall back"
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
            bus.direct_memory_bytes(0x0E_0000, 4, BusWidth::Dword, BusAccessKind::DataRead,),
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
fn instruction_prefetch_direct_pages_pin_admission_to_ram_g4() {
    // G4 guarantee: the JIT admits a block only where bus.direct_page(_, InstructionPrefetch)
    // covers true RAM. The VGA mode-13 aperture answers Data kinds only, ROM never yields a direct
    // page, and above-RAM space is unmapped, so InstructionPrefetch returns None for all three and
    // compiled code can never be hosted from video/MMIO/ROM. Above-RAM = 32 MiB, past the 16 MiB
    // test machine's RAM top.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        for &addr in &[
            izarravm_video::VGA_MODE13H_BASE,
            VGA_TEXT_BASE,
            LOW_BIOS_BASE,
            0x0200_0000,
        ] {
            assert!(
                bus.direct_page(addr, BusAccessKind::InstructionPrefetch)
                    .unwrap()
                    .is_none(),
                "InstructionPrefetch must not yield a direct page at {addr:#x}"
            );
        }
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
        assert_eq!(
            bus.direct_memory_bytes(0xA_1200, 4, BusWidth::Dword, BusAccessKind::DataRead,),
            4
        );
        assert_eq!(
            bus.direct_memory_bytes(0xA_1200, 4, BusWidth::Dword, BusAccessKind::DataWrite,),
            4
        );
        assert_eq!(
            bus.direct_memory_bytes(0xA_1200, 3, BusWidth::Word, BusAccessKind::DataRead,),
            0,
            "partial-width VGA spans fall back"
        );
        let mut bytes = [0; 4];
        assert_eq!(
            bus.read_memory_bytes_direct(
                0xA_1200,
                &mut bytes,
                BusWidth::Dword,
                BusAccessKind::DataWrite,
            )
            .unwrap(),
            0,
            "the read helper rejects write access kinds"
        );
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

fn assert_exact_vga_read_cycles(machine: &Machine, start: u32, count: u32) {
    let cycles: Vec<_> = machine
        .trace
        .cycles()
        .iter()
        .filter(|cycle| {
            cycle.kind == BusAccessKind::DataRead && cycle.address.wrapping_sub(start) < count
        })
        .collect();
    assert_eq!(cycles.len(), count as usize);
    let expected = BusCycle::clocks_for(BusWidth::Byte, machine.profile.wait_states.video);
    for (offset, cycle) in cycles.into_iter().enumerate() {
        assert_eq!(cycle.address, start + offset as u32);
        assert_eq!(cycle.width, BusWidth::Byte);
        assert_eq!(cycle.clocks, expected);
    }
}

#[test]
fn rep_movsb_reads_canonical_mode13_once_per_iteration() {
    const PROGRAM: &[u8] = &[0xF3, 0xA4, 0xCD, 0x20];
    const VALUES: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    for (offset, value) in VALUES.into_iter().enumerate() {
        machine.video_mut().cpu_write_chain4(0x1200 + offset, value);
    }
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0xA000));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(DOS_LOAD_SEGMENT));
    machine.cpu.registers.set_esi(0x1200);
    machine.cpu.registers.set_edi(0x0180);
    machine.cpu.registers.set_ecx(VALUES.len() as u32);
    machine.trace.set_tracing_mode(TracingMode::Full);

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );

    let destination = (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x0180;
    for (offset, expected) in VALUES.into_iter().enumerate() {
        assert_eq!(
            machine.read_physical_u8(destination + offset as u32),
            expected
        );
    }
    assert_exact_vga_read_cycles(&machine, 0xA_1200, VALUES.len() as u32);
}

#[test]
fn rep_movsb_mode_x_reads_once_and_leaves_the_last_latches() {
    const PROGRAM: &[u8] = &[0xF3, 0xA4, 0xCD, 0x20];
    const PLANE_ZERO: [u8; 4] = [0x31, 0x32, 0x33, 0x34];
    const LAST_LATCHES: [u8; 4] = [0x34, 0x52, 0x73, 0x94];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    {
        let vga = machine.video_mut();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        for (plane, last_latch) in LAST_LATCHES.into_iter().enumerate() {
            vga.write_port(0x3C4, 0x02);
            vga.write_port(0x3C5, 1 << plane);
            for (offset, plane_zero) in PLANE_ZERO.into_iter().enumerate() {
                let value = if offset == 3 {
                    last_latch
                } else if plane == 0 {
                    plane_zero
                } else {
                    0xA0 | plane as u8
                };
                vga.cpu_write(0x1200 + offset, value);
            }
        }
    }
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0xA000));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(DOS_LOAD_SEGMENT));
    machine.cpu.registers.set_esi(0x1200);
    machine.cpu.registers.set_edi(0x0180);
    machine.cpu.registers.set_ecx(PLANE_ZERO.len() as u32);
    machine.trace.set_tracing_mode(TracingMode::Full);

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );

    let destination = (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x0180;
    for (offset, expected) in PLANE_ZERO.into_iter().enumerate() {
        assert_eq!(
            machine.read_physical_u8(destination + offset as u32),
            expected
        );
    }
    assert_exact_vga_read_cycles(&machine, 0xA_1200, PLANE_ZERO.len() as u32);
    let vga = machine.video_mut();
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x0F);
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x41);
    vga.cpu_write(0x1300, 0);
    for (plane, expected) in LAST_LATCHES.into_iter().enumerate() {
        assert_eq!(vga.plane_byte(plane, 0x1300), expected);
    }
}

#[test]
fn repe_cmpsb_mode_x_reads_the_destination_once() {
    const PROGRAM: &[u8] = &[0xF3, 0xA6, 0xCD, 0x20];
    const VALUES: [u8; 4] = [0x61, 0x62, 0x63, 0x64];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    {
        let vga = machine.video_mut();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        vga.write_port(0x3C4, 0x02);
        vga.write_port(0x3C5, 0x01);
        for (offset, value) in VALUES.into_iter().enumerate() {
            vga.cpu_write(0x1200 + offset, value);
        }
    }
    let source = (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x0180;
    for (offset, value) in VALUES.into_iter().enumerate() {
        machine.write_physical_u8(source + offset as u32, value);
    }
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(DOS_LOAD_SEGMENT));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xA000));
    machine.cpu.registers.set_esi(0x0180);
    machine.cpu.registers.set_edi(0x1200);
    machine.cpu.registers.set_ecx(VALUES.len() as u32);
    machine.trace.set_tracing_mode(TracingMode::Full);

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.cpu.registers.ecx(), 0);
    assert_exact_vga_read_cycles(&machine, 0xA_1200, VALUES.len() as u32);
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
fn int10_pixel_pan_preserves_direct_mode13_pixels_and_invalidates_the_mapping_once() {
    const PROGRAM: &[u8] = &[
        0xB8, 0x00, 0xA0, // mov ax,A000h
        0x8E, 0xC0, // mov es,ax
        0x31, 0xFF, // xor di,di
        0xB0, 0x11, 0x26, 0x88, 0x05, // mov al,11h; mov es:[di],al
        0x47, 0xB0, 0x22, 0x26, 0x88, 0x05, // inc di; mov al,22h; mov es:[di],al
        0x47, 0xB0, 0x33, 0x26, 0x88, 0x05, // inc di; mov al,33h; mov es:[di],al
        0x47, 0xB0, 0x44, 0x26, 0x88, 0x05, // inc di; mov al,44h; mov es:[di],al
        0xB8, 0x00, 0x10, // mov ax,1000h (set one attribute register)
        0xBB, 0x13, 0x01, // mov bx,0113h (AC13 pixel pan = 1)
        0xCD, 0x10, // int 10h
        0xCD, 0x20, // int 20h
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    assert_eq!(machine.video().direct_write_token(), 1);
    machine.direct_map_changed = false;
    machine.direct_data_map_changed = false;
    machine.cpu.reset_perf_counters();

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );

    assert_eq!(machine.video().direct_write_token(), 0);
    assert_eq!(machine.video().attr_register(0x13), 1);
    assert_eq!(
        &machine.video().render_256color_row(0)[..3],
        &[0x22, 0x33, 0x44]
    );
    assert_eq!(machine.video().plane_byte(0, 0), 0x11);
    assert_eq!(machine.video().plane_byte(1, 0), 0x22);
    assert_eq!(machine.video().plane_byte(2, 0), 0x33);
    assert_eq!(machine.video().plane_byte(3, 0), 0x44);
    assert!(machine.cpu.perf_counters().direct_data_pointer_writes >= 4);
    assert_eq!(machine.cpu.perf_counters().direct_map_invalidations, 1);
}

#[test]
fn int10_char_height_preserves_direct_mode13_pixels_and_invalidates_the_mapping_once() {
    const PROGRAM: &[u8] = &[
        0xB8, 0x00, 0xA0, // mov ax,A000h
        0x8E, 0xC0, // mov es,ax
        0x31, 0xFF, // xor di,di
        0xB0, 0x11, 0x26, 0x88, 0x05, // mov al,11h; mov es:[di],al
        0x47, 0xB0, 0x22, 0x26, 0x88, 0x05, // inc di; mov al,22h; mov es:[di],al
        0x47, 0xB0, 0x33, 0x26, 0x88, 0x05, // inc di; mov al,33h; mov es:[di],al
        0x47, 0xB0, 0x44, 0x26, 0x88, 0x05, // inc di; mov al,44h; mov es:[di],al
        0xB8, 0x12, 0x11, // mov ax,1112h (load 8x8 ROM font and set height)
        0x31, 0xDB, // xor bx,bx (font block zero)
        0xCD, 0x10, // int 10h
        0xCD, 0x20, // int 20h
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROGRAM).unwrap();
    assert!(machine.set_vga_mode(0x13));
    assert_eq!(machine.video().direct_write_token(), 1);
    machine.direct_map_changed = false;
    machine.direct_data_map_changed = false;
    machine.cpu.reset_perf_counters();

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );

    assert_eq!(machine.video().direct_write_token(), 0);
    assert_eq!(machine.video().char_height(), 8);
    assert_eq!(
        &machine.video().render_256color_row(0)[..4],
        &[0x11, 0x22, 0x33, 0x44]
    );
    assert_eq!(machine.video().plane_byte(0, 0), 0x11);
    assert_eq!(machine.video().plane_byte(1, 0), 0x22);
    assert_eq!(machine.video().plane_byte(2, 0), 0x33);
    assert_eq!(machine.video().plane_byte(3, 0), 0x44);
    assert!(machine.cpu.perf_counters().direct_data_pointer_writes >= 4);
    assert_eq!(machine.cpu.perf_counters().direct_map_invalidations, 1);
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
        assert_eq!(
            bus.direct_memory_bytes(0xA_1240, 4, BusWidth::Dword, BusAccessKind::DataRead,),
            0
        );
        assert_eq!(
            bus.direct_memory_bytes(0xA_1240, 4, BusWidth::Dword, BusAccessKind::DataWrite,),
            4
        );
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
fn mode_x_plane_switch_invalidates_data_mappings_without_flushing_code() {
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

/// The JIT serves a MISALIGNED page-local wide access natively and charges it `bytes()` RAM byte
/// cycles, where `compute_iteration_upper` prices the same access as one WIDE cycle. That the
/// per-access budget bound still dominates is a real invariant held today by a margin nobody had
/// written down: every `*_data_upper` term in that bound is maxed against the Mode 13h dial
/// (`video_wait_states_approx`, 45 on I486 and 147 on I586), which swamps the worst split charge.
///
/// Assert the relation against a real `MachineBus` so a DIAL change fails a test rather than only
/// a debug build.
///
/// Three things this test is and is not:
///
/// * It is NEW. Nothing in the tree compares an actual charge against `iteration_upper`; the
///   `per_hop_estimate <= global_block_upper` debug assert compares two BOUNDS, and batch-cap
///   overshoot is separately declared accepted and bounded in `run.rs`.
/// * The multiplicand is `split_byte`, the plain RAM dial, NOT the `*_data_upper` form. The latter
///   carries the same Mode 13h `max` as the bounds it would be compared against, so the relation
///   would reduce to `2X <= X` and fail on every persona today.
/// * It does not subsume `compute_iteration_upper`'s `debug_assert`, and that assert does not
///   subsume it. This covers dial changes on ALREADY-ADMITTED personas. Only the assert covers the
///   Accurate class being admitted to direct blocks, because a test that iterates "the admitted
///   personas" cannot notice that the admitted set grew.
#[test]
fn the_misaligned_split_charge_stays_inside_the_per_access_budget_bound_on_every_admitted_persona()
{
    // The Approximate class, which is exactly the set that runs a direct block at all: `run.rs`
    // returns `Skipped` unless `uses_approximate_timing()`, i.e. I486 or I586.
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        assert!(
            mode.uses_approximate_timing(),
            "{mode:?} must be in the class that runs direct blocks"
        );
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            let split_byte = bus.jit_data_cost_clocks(BusWidth::Byte);
            let word_upper = bus
                .jit_data_cost_clocks(BusWidth::Word)
                .max(bus.jit_mode13_data_cost_clocks(BusWidth::Word));
            let dword_upper = bus
                .jit_data_cost_clocks(BusWidth::Dword)
                .max(bus.jit_mode13_data_cost_clocks(BusWidth::Dword));
            assert!(
                split_byte * 2 <= word_upper,
                "{mode:?}: a misaligned word charges {} RAM byte cycles against a word bound of \
                 {word_upper}",
                split_byte * 2
            );
            assert!(
                split_byte * 4 <= dword_upper,
                "{mode:?}: a misaligned dword charges {} RAM byte cycles against a dword bound of \
                 {dword_upper}",
                split_byte * 4
            );
        });
    }
}

/// **The charge equality guard 3 rests on**: a page-local misaligned N-byte RAM access costs the
/// same whether the JIT serves it inside a block or the interpreter splits it.
///
/// * Natively, after the slice: one WIDE cycle from the block's static count, plus `N - 1` byte
///   cycles from the split deposit, both priced at `jit_data_cost_clocks`.
/// * Interpreted: `lookup_access` refuses a misaligned width, `should_split` fires, and the access
///   becomes N byte reads each charged the RAM wait states.
///
/// The two are equal for exactly one reason -- `BusCycle::clocks_for` ignores width -- and that is
/// what this asserts. Make `clocks_for` width-dependent and the equality breaks silently: the JIT
/// would over- or under-charge every misaligned access by `wide - byte` clocks, with no fault, no
/// counter, and no differential fixture able to see it, because the CPU crate's `TestBus` models a
/// width-DEPENDENT dial and cannot state this property at all.
#[test]
fn a_misaligned_access_costs_the_same_split_natively_and_interpreted() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            let byte = bus.jit_data_cost_clocks(BusWidth::Byte);
            for (width, bytes) in [(BusWidth::Word, 2u64), (BusWidth::Dword, 4)] {
                let native = bus.jit_data_cost_clocks(width) + (bytes - 1) * byte;
                let interpreted = bytes * byte;
                assert_eq!(
                    native,
                    interpreted,
                    "{mode:?} {width:?}: the JIT charges one wide cycle plus {} byte cycles \
                     ({native}) where the interpreter's split charges {bytes} byte cycles \
                     ({interpreted}); they agree only while `clocks_for` ignores width",
                    bytes - 1
                );
            }
        });
    }
}

#[test]
fn approximate_video_wait_states_keep_the_doom_calibration() {
    assert_eq!(video_wait_states_approx(CpuPersona::I486), 45);
    // 586: jointly solved with `bus_timing` 16/105 for the 166 MHz / PC100
    // spec so doom-586 holds ~1001 realtics; see video_wait_states_approx.
    assert_eq!(video_wait_states_approx(CpuPersona::I586), 147);
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
    let sum = machine.cmos_bytes()[0x10..=0x2d]
        .iter()
        .fold(0u16, |sum, byte| sum.wrapping_add(u16::from(*byte)));
    with_bus(&mut machine, |bus| {
        bus.write_io(0x70, BusWidth::Byte, 0x2e, false).unwrap();
        bus.write_io(0x71, BusWidth::Byte, u32::from((sum >> 8) as u8), false)
            .unwrap();
        bus.write_io(0x70, BusWidth::Byte, 0x2f, false).unwrap();
        bus.write_io(0x71, BusWidth::Byte, u32::from(sum as u8), false)
            .unwrap();
    });
    assert!(
        machine.take_cmos_dirty(),
        "an NVRAM write should mark dirty"
    );
    let saved = machine.cmos_bytes();

    // A fresh machine loads the saved image and reads the same bytes back.
    let mut other = test_machine();
    assert!(other.load_cmos(&saved));
    assert_eq!(other.cmos_bytes()[0x10], 3);
    assert_eq!(other.cmos_bytes()[0x11], 1);
}

#[test]
fn pc_speaker_renders_a_square_wave() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
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

/// Peak of the beeper at a PIT2 divisor, through the whole mix.
fn speaker_peak_at_divisor(low: u32, high: u32) -> i32 {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, low, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, high, false).unwrap();
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data enable
    });
    let clock_hz = machine.active_mode.clock_rate().floor_hz();
    let chunk = clock_hz / 100_000; // ~10 us, mimicking per-instruction advance
    for _ in 0..2_000 {
        machine.advance_devices_clocks(chunk); // ~20 ms total
    }
    machine
        .render_audio(OPL_NATIVE_HZ as usize / 50)
        .iter()
        .map(|&(l, r)| i32::from(l).abs().max(i32::from(r).abs()))
        .max()
        .unwrap_or(0)
}

/// An ultrasonic PIT2 square wave has to average down rather than alias at the
/// leg's full swing.
///
/// The bound is taken FROM the leg rather than written as a constant, because
/// the leg's ceiling moves whenever its staging does -- it just moved 13 dB
/// when the beeper started passing through the card's PC-SPK level and the
/// summing node's reserve, at which point a fixed `peak < 1200` had stopped
/// being a two-thirds bound on anything and become a bound the aliasing case
/// would also have passed. An audible tone measured through the same path is
/// what the ceiling actually is.
#[test]
fn pc_speaker_ultrasonic_square_wave_averages_quietly() {
    let audible = speaker_peak_at_divisor(0x97, 0x04); // ~1 kHz, a full swing
    let ultrasonic = speaker_peak_at_divisor(0x02, 0x00); // ~600 kHz
    assert!(audible > 0, "the audible reference tone must be audible");
    assert!(
        ultrasonic * 3 < audible * 2,
        "an ultrasonic PIT2 square wave should average down instead of aliasing at the leg's full scale: {ultrasonic} against a {audible} swing"
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

#[test]
fn opl_status_read_predicts_the_timer_from_un_applied_batch_time() {
    // In the Approximate class devices advance only at batch end, so a status
    // read taken mid-batch used to report the state the chip had when the batch
    // STARTED. AdLib detection starts timer 1 (one 80us step), runs a fixed
    // delay loop that never ends the batch because it is pure computation, then
    // reads status ONCE -- and always saw the pre-delay flags.
    //
    // The third read_io argument is the CPU's in-batch core clocks, which is
    // what the prediction converts to elapsed microseconds.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // latch reg 0x04
        bus.write_io(0x389, BusWidth::Byte, 0x80, false).unwrap(); // reset flags
        bus.write_io(0x388, BusWidth::Byte, 0x02, false).unwrap(); // latch reg 0x02
        bus.write_io(0x389, BusWidth::Byte, 0xff, false).unwrap(); // preset: one step
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // latch reg 0x04
        bus.write_io(0x389, BusWidth::Byte, 0x21, false).unwrap(); // unmask + start
    });

    // Immediately: the 80us step cannot have elapsed, so no overflow yet.
    let immediate = with_bus(&mut machine, |bus| {
        bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(
        immediate & 0x40,
        0,
        "timer 1 cannot have expired at zero time"
    );

    // A whole millisecond of in-batch CPU clocks later, still without any batch
    // end, the read must see the overflow. 66_000 clocks is ~1 ms at 66 MHz,
    // comfortably past the single 80 us step.
    let predicted = with_bus(&mut machine, |bus| {
        bus.read_io(0x388, BusWidth::Byte, 66_000, false).unwrap()
    });
    assert_ne!(
        predicted & 0x40,
        0,
        "timer 1 overflow must be visible from un-applied batch time; without \
         the prediction this read returns the stale batch-start flags and AdLib \
         detection fails on every fast persona"
    );
    assert_ne!(
        predicted & 0x80,
        0,
        "the IRQ bit accompanies an unmasked overflow"
    );
}

#[test]
fn opl_status_read_stays_live_in_the_accurate_class() {
    // The 386 class advances devices per instruction, so there is never
    // un-applied time to predict from and the byte must stay exactly live --
    // the prediction is an Approximate-class path only. A control for the test
    // above: same sequence, same in-batch clocks, no overflow conjured.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x388, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0x389, BusWidth::Byte, 0xff, false).unwrap();
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x389, BusWidth::Byte, 0x21, false).unwrap();
    });
    let value = with_bus(&mut machine, |bus| {
        bus.read_io(0x388, BusWidth::Byte, 66_000, false).unwrap()
    });
    let live = u32::from(machine.opl.status());
    assert_eq!(value, live, "the Accurate class must read the live byte");
}

// ---------------------------------------------------------------------------
// The Accurate-class (386) lazy poll ports: IZARRAVM_LAZY_PORT_386.
// ---------------------------------------------------------------------------

#[test]
fn the_386_lazy_port_switch_can_never_arm_the_approximate_class() {
    // The structural half of the design, and the reason the switch is a
    // separate bool from `lazy_port_reads`: 486/586 already take the 3DA and
    // 0x61 arms and have NEVER taken the gameport arm, so a switch that could
    // reach them would silently move a pinned 486/586 fixture. Pinned over the
    // whole mode set in both environment states rather than trusting the
    // default, because the default is the only thing an env-based test could
    // observe without racing the process environment.
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for env_enabled in [false, true] {
            let armed = crate::bus::lazy_ports_386_composed(mode, env_enabled);
            assert_eq!(
                armed,
                env_enabled && !mode.uses_approximate_timing(),
                "mode {mode:?} env {env_enabled}"
            );
            if mode.uses_approximate_timing() {
                assert!(
                    !armed,
                    "{mode:?} is Approximate; the switch must not reach it"
                );
            }
        }
    }
    // The loop above is the whole test. `lazy_ports_386_for` -- the env-composed
    // wrapper the bus actually calls -- is deliberately NOT asserted on here:
    // its only added term is `lazy_port_reads_386_enabled()`, and with the
    // switch off by default every assertion about it passes for the wrong reason
    // (a mutation deleting the mode test still returns false), while forcing the
    // switch on means writing the process environment under a threaded test
    // runner. Pinning `lazy_ports_386_composed` over both environment states,
    // which is what the loop does, covers the mode half without that race.
}

/// A 386 bus with the lazy poll ports armed as if `IZARRAVM_LAZY_PORT_386` were
/// set, without touching the process environment.
fn with_lazy_386_bus<R>(machine: &mut Machine, f: impl FnOnce(&mut MachineBus) -> R) -> R {
    with_bus(machine, |bus| {
        assert!(
            !bus.lazy_port_reads,
            "this helper is for the Accurate class only"
        );
        bus.lazy_ports_386 = true;
        f(bus)
    })
}

fn accurate_machine_with_joystick() -> Machine {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.set_joystick_state(Some(JoystickState {
        x: 0x80,
        y: 0x40,
        buttons: 0,
    }));
    machine.run_cycles(5_000).unwrap();
    machine
}

#[test]
fn the_386_lazy_switch_stops_the_poll_ports_ending_the_batch() {
    // What the whole slice buys: on the Accurate class a poll loop used to end
    // its batch on EVERY read, which is why a PoP-386 run spent 1.16M batch
    // entries to answer 934k 3DA polls. Each arm is checked for both states of
    // the switch in the same test, so neither direction can rot into vacuity.
    // 0x201 is deliberately NOT in this list. The gameport arm was moved off
    // the persona switch and onto the RC one-shots' own state; both of its
    // directions are pinned by the `a_gameport_read_*` tests below.
    for port in [0x3DA_u16, 0x61] {
        for lazy in [false, true] {
            let mut machine = accurate_machine_with_joystick();
            let touched = with_bus(&mut machine, |bus| {
                bus.lazy_ports_386 = lazy;
                *bus.io_touched = false;
                bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
                *bus.io_touched
            });
            assert_eq!(
                touched, !lazy,
                "port {port:#06X} with the 386 switch {lazy}: batch-ending must \
                 follow the switch exactly"
            );
        }
    }
}

/// Q2b, both directions of the one gate that decides them. A charged one-shot
/// is genuinely time-dependent, so its read must keep ending the batch; an idle
/// one is a constant function of state, so its read must not. The gate is
/// device state, so the assertion is made on BOTH personas -- the Approximate
/// class is where wolf3d runs and the Accurate class must not diverge.
#[test]
fn a_gameport_read_ends_the_batch_only_while_a_one_shot_is_charged() {
    for mode in [GswMode::Gsw386, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        machine.set_joystick_state(Some(JoystickState {
            x: 0x80,
            y: 0x40,
            buttons: 0,
        }));
        machine.run_cycles(5_000).unwrap();
        let (mid_pulse, mid_touched, late, late_touched) = with_bus(&mut machine, |bus| {
            // The guest arms the one-shots, then samples. Clear the flag the
            // WRITE set so the reads below are the only thing under test.
            bus.write_io(0x201, BusWidth::Byte, 0, false).unwrap();
            *bus.io_touched = false;
            let mid_pulse = bus.read_io(0x201, BusWidth::Byte, 0, false).unwrap();
            let mid_touched = *bus.io_touched;
            *bus.io_touched = false;
            // Far past both deadlines: 0x80 on X is ~0.58 ms, and 4M clocks at
            // either tier is well beyond that.
            let late = bus
                .read_io(0x201, BusWidth::Byte, 4_000_000, false)
                .unwrap();
            (mid_pulse, mid_touched, late, *bus.io_touched)
        });
        assert_eq!(
            mid_pulse & 0x03,
            0x03,
            "{mode:?}: the sampled value must still be time-dependent mid-pulse"
        );
        assert!(
            mid_touched,
            "{mode:?}: a mid-pulse gameport read must end the batch"
        );
        assert_eq!(
            late & 0x03,
            0x00,
            "{mode:?}: both one-shots have discharged by 4M clocks"
        );
        assert!(
            !late_touched,
            "{mode:?}: an idle gameport read must not end the batch"
        );
    }
}

/// The `!io_touched_before_read` half of the guard, which the lazy arm shares
/// with every other lazy port: going lazy may only take back a flag THIS read
/// set, never one an earlier access in the same batch set.
#[test]
fn an_idle_gameport_read_never_clears_an_earlier_accesss_batch_end() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    let touched = with_bus(&mut machine, |bus| {
        // An earlier device access in this batch. No stick is attached, so the
        // gameport read that follows is idle and would otherwise go lazy.
        bus.write_io(0x43, BusWidth::Byte, 0x36, false).unwrap();
        assert!(*bus.io_touched, "the PIT write must end the batch");
        bus.read_io(0x201, BusWidth::Byte, 0, false).unwrap();
        *bus.io_touched
    });
    assert!(
        touched,
        "an idle gameport read must not take back the PIT write's batch end"
    );
}

/// The absent-stick case, which is what every headless fixture actually runs:
/// no stick means no one-shot can ever charge, so every read is idle.
#[test]
fn a_gameport_read_with_no_stick_attached_is_always_idle() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    let (value, touched) = with_bus(&mut machine, |bus| {
        // Even after an arming write, which cannot charge what is not there.
        bus.write_io(0x201, BusWidth::Byte, 0, false).unwrap();
        *bus.io_touched = false;
        let value = bus.read_io(0x201, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });
    assert_eq!(value, 0xff, "an open connector floats every line high");
    assert!(!touched, "an absent stick is idle, so the read goes lazy");
}

#[test]
fn the_386_lazy_switch_leaves_the_opl_charging_rules_alone() {
    // The hard constraint of the slice. An OPL status poll is deliberately
    // batch-ending in BOTH classes (it is how the timer advances between
    // polls), and the Approximate class's ISA-I/O charge is Approximate-class
    // policy. Neither may move with this switch, so both are pinned here with
    // the switch ON.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    let (touched, isa_clocks) = with_lazy_386_bus(&mut machine, |bus| {
        *bus.io_touched = false;
        *bus.isa_io_clocks = 0;
        bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
        (*bus.io_touched, *bus.isa_io_clocks)
    });
    assert!(touched, "an OPL status poll stays batch-ending on 386");
    assert_eq!(
        isa_clocks, 0,
        "the Approximate-class ISA-I/O charge must not appear on the 386 class"
    );
}

#[test]
fn a_lazy_gameport_read_matches_a_real_advance_devices_of_the_same_clocks() {
    // The exactness proof for the one port whose VALUE provably cannot move:
    // `GamePort::read` is a pure function of the two RC discharge deadlines and
    // `guest_tick_now()`, and nothing in advance_devices touches the deadlines.
    // Differential form, same as predicted_beam/predicted_pit_out: read the
    // predicted machine mid-batch at total T, advance the other machine for
    // real by the same T and read it at zero offset, require equality.
    for core_clocks_so_far in [0u64, 1_000, 40_000, 150_000, 400_000] {
        let mut predicted_machine = accurate_machine_with_joystick();
        let mut real_machine = accurate_machine_with_joystick();
        assert_eq!(predicted_machine.timeline, real_machine.timeline);

        // The RC one-shots are armed by the 0x201 WRITE, in the batch before
        // the reads -- exactly as a guest arms them, and the reason the read
        // never observes a mid-batch mutation of its own inputs.
        for machine in [&mut predicted_machine, &mut real_machine] {
            with_bus(machine, |bus| {
                bus.write_io(0x201, BusWidth::Byte, 0, false).unwrap();
            });
        }

        let (predicted, raw_bus_clocks) = with_lazy_386_bus(&mut predicted_machine, |bus| {
            let before = bus.trace.elapsed_clocks();
            let value = bus
                .read_io(0x201, BusWidth::Byte, core_clocks_so_far, false)
                .unwrap();
            (value, bus.trace.elapsed_clocks() - before)
        });

        let step = core_clocks_so_far + real_machine.scale_bus(raw_bus_clocks);
        real_machine.advance_devices(step);
        let real = with_bus(&mut real_machine, |bus| {
            bus.read_io(0x201, BusWidth::Byte, 0, false).unwrap()
        });

        assert_eq!(
            predicted, real,
            "core {core_clocks_so_far}: a lazy gameport read must equal a real \
             advance_devices of the same clock total followed by a read"
        );
    }
}

#[test]
fn a_lazy_gameport_read_moves_with_the_in_batch_offset() {
    // Non-vacuity for the test above: the RC one-shots must actually discharge
    // across the swept range, or the differential test would pass on a
    // constant. 0x80 on X is ~0.58 ms, far more than one 386 batch, so the bit
    // is still SET early in the batch and CLEAR once the batch is long enough.
    let mut machine = accurate_machine_with_joystick();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x201, BusWidth::Byte, 0, false).unwrap();
    });
    let (early, late) = with_lazy_386_bus(&mut machine, |bus| {
        let early = bus.read_io(0x201, BusWidth::Byte, 0, false).unwrap();
        let late = bus
            .read_io(0x201, BusWidth::Byte, 4_000_000, false)
            .unwrap();
        (early, late)
    });
    assert_eq!(
        early & 0x03,
        0x03,
        "both one-shots are still charged at t=0"
    );
    assert_eq!(late & 0x03, 0x00, "both have discharged 4M clocks later");
}

// ---------------------------------------------------------------------------
// Margo STATUS.BUSY: the lazy peek, and the arm-time drain credit it is
// measured from. The counterparts of the PIT-latch and 3DA suites above; see
// `Machine::vega_edge_ticks` for why the arming write no longer ends its batch.
// ---------------------------------------------------------------------------

/// Program a Margo FILL whose modeled busy time spans a coarse batch, WITHOUT
/// issuing the COMMAND that arms it. 640x480 at 5 ns/pixel is 1,536,100 ns --
/// ~33,794 clocks at the 386 tier, so an in-batch offset sweep can straddle the
/// instant BUSY drops.
fn program_large_margo_fill(machine: &mut Machine) {
    write_mmio_reg(machine, 0x100, 0); // DST_BASE
    write_mmio_reg(machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(machine, 0x110, 1); // DEPTH
    write_mmio_reg(machine, 0x114, 0); // DST_XY: (0,0)
    write_mmio_reg(machine, 0x11c, (480 << 16) | 640); // DIM: 640x480
    write_mmio_reg(machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(machine, 0x128, 0xf0); // ROP: PATCOPY
    write_mmio_reg(machine, 0x130, 0); // FLAGS: none
}

/// The same, sized like a glyph blit: 20 pixels is 100 + 20*5 = 200 ns, about
/// 4.4 clocks at the 386 tier.
fn program_small_margo_fill(machine: &mut Machine) {
    write_mmio_reg(machine, 0x100, 0); // DST_BASE
    write_mmio_reg(machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(machine, 0x110, 1); // DEPTH
    write_mmio_reg(machine, 0x114, (2 << 16) | 3); // DST_XY: (3,2)
    write_mmio_reg(machine, 0x11c, (4 << 16) | 5); // DIM: 5x4
    write_mmio_reg(machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(machine, 0x128, 0xf0); // ROP: PATCOPY
    write_mmio_reg(machine, 0x130, 0); // FLAGS: none
}

/// Read Margo STATUS through the MMIO aperture at a given in-batch offset,
/// returning the byte and the raw bus clocks the access itself recorded (the
/// same accounting `read_status_port_at` uses, so a differential can advance the
/// twin machine by the identical total).
fn read_margo_status_at(machine: &mut Machine, prior: u64, core: u64) -> (u8, u64) {
    with_bus(machine, |bus| {
        bus.prior_runs_core_clocks = prior;
        bus.core_clocks_so_far = core;
        let before = bus.trace.elapsed_clocks();
        let value = bus
            .read_memory(
                MARGO_MMIO_BASE + 0x008,
                BusWidth::Byte,
                BusAccessKind::DataRead,
            )
            .unwrap() as u8;
        (value, bus.trace.elapsed_clocks() - before)
    })
}

/// Issue the four COMMAND bytes at a given in-batch offset, the way a guest that
/// has been computing for most of a batch does. Returns the elapsed Margo
/// nanoseconds at that instant -- the credit the arm just stamped.
fn arm_margo_fill_at(machine: &mut Machine, prior: u64, command: u32) -> u64 {
    with_bus(machine, |bus| {
        bus.prior_runs_core_clocks = prior;
        bus.core_clocks_so_far = 0;
        for (i, byte) in command.to_le_bytes().into_iter().enumerate() {
            bus.write_memory(
                MARGO_MMIO_BASE + 0x150 + i as u32,
                BusWidth::Byte,
                u32::from(byte),
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }
        assert!(
            !*bus.io_touched,
            "the Margo arming write must NOT end its own batch any more: that \
             break is what the analytic peek replaced"
        );
        bus.elapsed_margo_ns()
    })
}

#[test]
fn a_mid_batch_margo_status_read_matches_a_real_advance_devices_of_the_same_clocks() {
    // The Margo counterpart of
    // a_mid_batch_3da_read_matches_a_real_advance_devices_of_the_same_clocks,
    // and the test that retires the "STATUS.BUSY reports the batch-start engine"
    // caveat. It is the load-bearing one for this slice: the guest's blit wait
    // is an MMIO spin, which cannot end its own batch, so before the peek the
    // ONLY thing making that spin see BUSY drop on time was ending the batch on
    // the arming write. Removing that break is only sound if a mid-batch STATUS
    // read equals what a real advance_devices of the same clock total, followed
    // by a read at zero offset, produces.
    //
    // Both timing classes: the tier is a CPU-speed policy, and no GswMode
    // reaches margo.rs, so section 9 applies to both.
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        for prior in [0u64, 61, 33_000] {
            for core in [0u64, 100, 12_345, 40_000] {
                let mut predicted_machine = test_machine();
                predicted_machine.set_mode(mode);
                predicted_machine.run_cycles(5_000).unwrap();
                program_large_margo_fill(&mut predicted_machine);
                write_mmio_reg(&mut predicted_machine, 0x150, 0x01); // COMMAND: FILL

                let mut real_machine = test_machine();
                real_machine.set_mode(mode);
                real_machine.run_cycles(5_000).unwrap();
                program_large_margo_fill(&mut real_machine);
                write_mmio_reg(&mut real_machine, 0x150, 0x01);
                assert_eq!(predicted_machine.timeline, real_machine.timeline);
                assert_eq!(
                    predicted_machine.vega.blitter_busy_ns(),
                    real_machine.vega.blitter_busy_ns()
                );

                let (predicted, predicted_raw) =
                    read_margo_status_at(&mut predicted_machine, prior, core);
                // The step carries NO term for the access's own bus time: a
                // Margo MMIO read is charged video wait states, and that charge
                // is recorded before the peek on BOTH machines, so it is already
                // inside each read's own in_batch_clocks and cancels. The
                // equality below is asserted, not assumed.
                real_machine.advance_devices(prior + core);
                let (real, real_raw) = read_margo_status_at(&mut real_machine, 0, 0);
                assert_eq!(
                    predicted_raw, real_raw,
                    "mode {mode:?}: the two reads must charge the same bus time for \
                     the own-charge term to cancel"
                );
                assert_eq!(
                    predicted & 1,
                    real & 1,
                    "mode {mode:?} prior {prior} core {core}: a mid-batch STATUS read \
                     must equal a real advance_devices of the same total"
                );
            }
        }
    }
}

#[test]
fn a_mid_batch_margo_status_read_moves_with_the_in_batch_offset() {
    // Non-vacuity for the test above: without the peek every STATUS read in a
    // batch reports the same batch-start BUSY, so the column below would be
    // constant. The 640x480 fill models 1,536,100 ns, ~33,794 clocks at this
    // tier, so a 0..60,000 sweep straddles the instant the engine goes idle.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.run_cycles(5_000).unwrap();
    program_large_margo_fill(&mut machine);
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

    let mut levels = std::collections::BTreeSet::new();
    for core in (0u64..=60_000).step_by(1_000) {
        let (value, _) = read_margo_status_at(&mut machine, 0, core);
        levels.insert(value & 1);
    }
    assert_eq!(
        levels.len(),
        2,
        "STATUS.BUSY must take both levels across a sweep of in-batch offsets; a \
         constant column means the peek is not being taken"
    );
    // ... and in the right direction, at the modeled instant rather than merely
    // somewhere in the sweep.
    assert_eq!(read_margo_status_at(&mut machine, 0, 33_000).0 & 1, 1);
    assert_eq!(read_margo_status_at(&mut machine, 0, 34_500).0 & 1, 0);
}

#[test]
fn a_blit_armed_mid_batch_is_not_idle_early_at_the_next_batch_entry() {
    // The property a peek-only patch would have shipped broken, and the reason
    // `Margo::busy_credit_ns` exists. Margo drains ONCE, at batch end, with the
    // WHOLE batch's nanoseconds. While the arming write ended its own batch a
    // blit always started at in-batch offset ~0 and that was exactly right.
    // Without the break, a blit armed 20,000 clocks (~909 us) into a batch would
    // be billed all 909 us against its 200 ns of work and read IDLE at the next
    // batch entry -- an idle-early violation of section 9's first clause,
    // observable through the deadline term as well, since `vega_edge_ticks`
    // reads the drained `busy_ns` at batch entry.
    const ARM_OFFSET: u64 = 20_000;
    const BATCH_END: u64 = ARM_OFFSET + 4; // ~181 ns past the arm: less than 200

    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.run_cycles(5_000).unwrap();
    program_small_margo_fill(&mut machine);
    let credit_ns = arm_margo_fill_at(&mut machine, ARM_OFFSET, 0x01);
    assert_eq!(machine.vega.blitter_busy_ns(), 200); // 20 px: 100 + 20*5
    assert!(
        credit_ns > 800_000,
        "sanity: the arm must land deep into the batch, got {credit_ns} ns"
    );

    machine.advance_devices(BATCH_END);
    let remaining = machine.vega.blitter_busy_ns();
    assert!(
        remaining > 0 && remaining <= 200,
        "a blit armed mid-batch must still be busy at the next batch entry, with \
         only the time SINCE the arm drained; got {remaining} ns"
    );
    // What the guest's spin sees at the next batch entry, and what the deadline
    // term will size the next batch from.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // Control, so the assertion above is about the credit and not about the
    // operation simply outliving the advance: the IDENTICAL blit armed at offset
    // 0 of the batch really is drained to idle by the same advance.
    let mut control = test_machine();
    control.set_mode(GswMode::Gsw386);
    control.run_cycles(5_000).unwrap();
    program_small_margo_fill(&mut control);
    write_mmio_reg(&mut control, 0x150, 0x01);
    assert_eq!(control.vega.blitter_busy_ns(), 200);
    control.advance_devices(BATCH_END);
    assert_eq!(
        control.vega.blitter_busy_ns(),
        0,
        "control: a blit armed at offset 0 is drained by the whole batch"
    );
    assert_eq!(read_mmio_reg(&mut control, 0x008) & 1, 0);
}

#[test]
fn an_overlapping_write_does_not_restamp_the_blit_origin() {
    // The other half of the EDGE argument, which used to be pinned through
    // `io_touched` in `no_margo_write_ends_the_batch_while_a_long_blit_drains`.
    // `VideoWrite::ArmedBlit` now stamps the instant the busy time is measured
    // FROM, so a level-triggered test would re-stamp that origin on every
    // framebuffer store a guest overlaps with a running blit -- and the
    // operation would never finish. The 200 ns fill armed at offset 0 must be
    // drained to idle by a batch that also contains writes at offset 10,000.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.run_cycles(5_000).unwrap();
    program_small_margo_fill(&mut machine);
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL, at offset ~0
    assert_eq!(machine.vega.blitter_busy_ns(), 200);

    with_bus(&mut machine, |bus| {
        bus.prior_runs_core_clocks = 10_000;
        bus.core_clocks_so_far = 0;
        // An LFB store and a non-arming MMIO store, both while the engine runs.
        bus.write_memory(
            MARGO_LFB_BASE + 0x1234,
            BusWidth::Byte,
            0x5a,
            BusAccessKind::DataWrite,
        )
        .unwrap();
        bus.write_memory(
            MARGO_MMIO_BASE + 0x120, // FG_COLOR
            BusWidth::Byte,
            0x17,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    });

    machine.advance_devices(10_010);
    assert_eq!(
        machine.vega.blitter_busy_ns(),
        0,
        "an overlapping write must not move the instant the blit is timed from"
    );
}

#[test]
fn a_batch_is_not_cut_at_blit_completion_but_the_read_is_still_exact_across_it() {
    // The phase-2 property: with no pusher work the blit completion instant is
    // no longer a batch boundary, so a whole blit begins AND ends inside one
    // batch -- and STATUS.BUSY must still flip at exactly the modeled instant,
    // read from inside that same batch. This is what licenses dropping the
    // deadline term: section 9 constrains what software can OBSERVE, and the
    // peek keeps that exact without help from batch geometry.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.run_cycles(5_000).unwrap();
    program_small_margo_fill(&mut machine);
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL, at offset ~0
    assert_eq!(machine.vega.blitter_busy_ns(), 200); // 20 px: 100 + 20*5

    // The batch is NOT capped at the 200 ns completion edge any more.
    let blit_edge = machine
        .timeline
        .cpu_clocks_until(timeline::DeviceClock::MargoNs, 200, 1_000_000_000)
        .unwrap();
    let cap = machine.event_batch_cap(u64::MAX);
    assert!(
        cap > blit_edge,
        "the batch must no longer be cut at the blit edge: cap {cap}, edge {blit_edge}"
    );

    // Sweep in-batch offsets ACROSS the completion instant, all inside that one
    // uncapped batch. BUSY must be set strictly before the modeled instant and
    // clear from it onward -- one transition, at the right clock.
    let mut transitions = Vec::new();
    let mut previous = None;
    for core in 0..=(blit_edge + 8) {
        let busy = read_margo_status_at(&mut machine, 0, core).0 & 1;
        if previous.is_some_and(|p| p != busy) {
            transitions.push((core, busy));
        }
        previous = Some(busy);
    }
    assert_eq!(
        transitions.len(),
        1,
        "STATUS.BUSY must flip exactly once across the completion instant, got {transitions:?}"
    );
    let (flip_at, level) = transitions[0];
    assert_eq!(level, 0, "the single transition must be busy -> idle");
    // The read charges its own bus time before the peek, so the observed flip
    // lands at or just before the bare-offset edge; it must never be LATE (that
    // would be idle-late) and never at zero (that would be idle-early).
    assert!(
        flip_at > 0 && flip_at <= blit_edge,
        "the flip must land at the modeled edge {blit_edge}, got {flip_at}"
    );

    // And the engine is genuinely still busy in the machine's own state at the
    // batch boundary: nothing drained it early to make the sweep above pass.
    assert_eq!(machine.vega.blitter_busy_ns(), 200);
}

/// A 200x200 FILL: 40,000 pixels at 5 ns each plus 100 ns setup = 200,100 ns of
/// modeled busy time, ~4,402 clocks at the 386 tier. Big enough that a whole
/// operation-length error is unmissable, unlike the 200 ns glyph-sized fill.
fn program_square_margo_fill(machine: &mut Machine) {
    write_mmio_reg(machine, 0x100, 0); // DST_BASE
    write_mmio_reg(machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(machine, 0x110, 1); // DEPTH
    write_mmio_reg(machine, 0x114, 0); // DST_XY: (0,0)
    write_mmio_reg(machine, 0x11c, (200 << 16) | 200); // DIM: 200x200
    write_mmio_reg(machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(machine, 0x128, 0xf0); // ROP: PATCOPY
    write_mmio_reg(machine, 0x130, 0); // FLAGS: none
}

#[test]
fn a_second_identical_blit_in_one_batch_restamps_the_origin() {
    // THE SAME-DURATION RE-ARM. Two operations of IDENTICAL modeled duration,
    // armed in the same batch, are the case a busy_ns VALUE comparison cannot
    // see: every setter in margo.rs is an assign and nothing drains mid-batch,
    // so `busy_ns` before and after the second COMMAND are EQUAL, the write
    // reports `Accepted` instead of `ArmedBlit`, and the credit still names the
    // FIRST arm's offset. The second operation then reads idle for its entire
    // length -- a section 9 idle-early violation that scales with operation
    // size, not a rounding error.
    //
    // This is not a contrived shape: izbios' lfb_text draws every glyph with a
    // fixed MG_DIM of 0x00080008, so consecutive glyph blits all model exactly
    // the same busy time. The 200x200 fill below just makes the window wide
    // enough to assert on comfortably.
    const ARM2_OFFSET: u64 = 10_000;

    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.run_cycles(5_000).unwrap();
    program_square_margo_fill(&mut machine);

    // Both arms go through the REAL bus write path, which is the seam that
    // decides whether the origin is re-stamped. A test that called
    // `credit_busy_ns` by hand would pass while the bus edge stayed broken.
    arm_margo_fill_at(&mut machine, 0, 0x01);
    let busy_ns = machine.vega.blitter_busy_ns();
    assert_eq!(busy_ns, 200_100, "200x200 at 5 ns/px + 100 ns setup");
    arm_margo_fill_at(&mut machine, ARM2_OFFSET, 0x01);
    assert_eq!(
        machine.vega.blitter_busy_ns(),
        busy_ns,
        "the second FILL is identical, so busy_ns is unchanged -- which is \
         exactly why a value comparison cannot detect it"
    );

    // The second operation must run its full modeled length FROM ITS OWN ARM.
    let duration = machine
        .timeline
        .cpu_clocks_until(timeline::DeviceClock::MargoNs, busy_ns, 1_000_000_000)
        .unwrap();
    assert!(
        duration > 4_000,
        "sanity: a wide window, got {duration} clocks"
    );

    for probe in [0u64, 1, duration / 2, duration - 64] {
        let (status, _) = read_margo_status_at(&mut machine, 0, ARM2_OFFSET + probe);
        assert_eq!(
            status & 1,
            1,
            "BUSY must still be set {probe} clocks after the SECOND arm; \
             reading idle here bills the second operation against the first \
             one's start instant"
        );
    }
    // ... and it does finish: the credit moves the origin, it does not disable
    // the drain.
    let (late, _) = read_margo_status_at(&mut machine, 0, ARM2_OFFSET + duration + 64);
    assert_eq!(
        late & 1,
        0,
        "BUSY must clear once the second operation is done"
    );
}

/// A3: `MachineBus::icache_fetch_clocks` is the per-persona I-cache fetch cost, snapshotted at
/// bus construction so the conventional-RAM arm of `charge_physical_instruction_fetch_run` is
/// one add instead of a chase through the cache model. Two things have to hold for every
/// persona, and this checks both against the LIVE model rather than a table: the charge equals
/// `clocks_for(Byte, code_fetch_wait_states())`, and it is the same on the tracing arm (which
/// still reads the wait-state, so a stale snapshot would make the two arms disagree).
///
/// Personas are looped because the value is the only thing that distinguishes them here: a
/// snapshot taken from the wrong model, or not refreshed on a mode change, shows up as one
/// persona charging another's constant. (`Machine::set_mode` rewrites `cache_model`, and a bus
/// cannot outlive it -- `make_bus` borrows the machine mutably -- which is what makes the
/// snapshot sound; the `debug_assert` in the fast arm is the standing check.)
#[test]
fn cached_icache_fetch_cost_matches_the_live_model_in_every_persona() {
    use izarravm_bus::BusCycle;
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        let expected = u64::from(BusCycle::clocks_for(
            BusWidth::Byte,
            machine.cache_model.code_fetch_wait_states(),
        ));

        for tracing in [TracingMode::Off, TracingMode::Counts, TracingMode::Full] {
            machine.trace.set_tracing_mode(tracing);
            with_bus(&mut machine, |bus| {
                let before = bus.trace.elapsed_clocks();
                bus.charge_physical_instruction_fetch_run(0x0002_0000, 4)
                    .unwrap();
                assert_eq!(
                    bus.trace.elapsed_clocks() - before,
                    expected,
                    "{mode:?}/{tracing:?}: one collapsed I-cache access at the persona's constant"
                );
            });
        }
    }
}

/// `charge_direct_ram_split` must be BIT-IDENTICAL to the byte-splitting loop it replaces, in all
/// three accounting fields, for every width, every misalignment, both timing classes and every
/// tracing mode.
///
/// This is the single most load-bearing assertion in the interpreter data-path slice. The slice's
/// whole premise is that admitting misaligned accesses to the interpreter's fast path changes
/// WHERE the work happens and not WHAT the guest clock sees; the reference below is the actual
/// production loop (`CpuBus::read_memory` takes `should_split` for exactly these addresses), so a
/// charge that drifts fails here rather than as an unexplained frame-hash move six fixtures later.
///
/// The `Full`-mode cycle vector is compared element by element on purpose. `elapsed_clocks` alone
/// would pass a charge that folded N byte cycles into one wide cycle of the same total clocks --
/// which is precisely the mutation this test has to catch.
#[test]
fn charge_direct_ram_split_is_bit_identical_to_the_byte_splitting_loop() {
    // RAM well below the 0xA0000 aperture, page-local for every case below.
    const BASE: u32 = 0x0002_1000;
    for mode in [
        // Approximate: `flat_data_cost` true, so the split takes the `record_memory_run` arm.
        GswMode::Gsw486,
        GswMode::Gsw586,
        // Accurate: `flat_data_cost` false, so it takes the per-byte transcription whose
        // wait-state lookups mutate the modeled cache tags.
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        for (width, offset) in [
            (BusWidth::Word, 1u32),
            (BusWidth::Dword, 1),
            (BusWidth::Dword, 2),
            (BusWidth::Dword, 3),
            // Page-edge, still page-local: a Word at 0xFFD and a Dword at 0xFF9.
            (BusWidth::Word, 0xffd),
            (BusWidth::Dword, 0xff9),
        ] {
            for tracing in [TracingMode::Off, TracingMode::Counts, TracingMode::Full] {
                // BOTH DIRECTIONS. `write_memory_recorded` has its own `should_split` byte loop
                // (a separate site from `read_memory`'s), and the write path is where a charge
                // error would be worst, so the write twin is a reference here rather than an
                // assumed mirror of the read.
                for kind in [BusAccessKind::DataRead, BusAccessKind::DataWrite] {
                    let address = BASE + offset;
                    assert!(
                        width.misaligned_at(address),
                        "case ({width:?}, {offset:#x}) is not actually misaligned"
                    );

                    // Reference: the production byte loop, reached through `read_memory` /
                    // `write_memory` according to the direction under test.
                    let mut machine = test_machine();
                    machine.set_mode(mode);
                    machine.trace.set_tracing_mode(tracing);
                    let (ref_clocks, ref_accesses, ref_cycles) = with_bus(&mut machine, |bus| {
                        bus.trace.clear();
                        if kind == BusAccessKind::DataRead {
                            bus.read_memory(address, width, kind).unwrap();
                        } else {
                            bus.write_memory(address, width, 0x5a5a_5a5a, kind).unwrap();
                        }
                        (
                            bus.trace.elapsed_clocks(),
                            bus.trace.access_count(),
                            bus.trace.cycles().iter().cloned().collect::<Vec<_>>(),
                        )
                    });

                    // Candidate: the new charge, on a freshly built machine so the Accurate class's
                    // modeled cache tags start from the same state the reference did.
                    let mut machine = test_machine();
                    machine.set_mode(mode);
                    machine.trace.set_tracing_mode(tracing);
                    let (got_clocks, got_accesses, got_cycles) = with_bus(&mut machine, |bus| {
                        bus.trace.clear();
                        bus.charge_direct_ram_split(address, width, kind).unwrap();
                        (
                            bus.trace.elapsed_clocks(),
                            bus.trace.access_count(),
                            bus.trace.cycles().iter().cloned().collect::<Vec<_>>(),
                        )
                    });

                    let case = format!("{mode:?}/{tracing:?}/{kind:?}/{width:?}@{offset:#x}");
                    assert_eq!(got_clocks, ref_clocks, "{case}: elapsed_clocks");
                    assert_eq!(got_accesses, ref_accesses, "{case}: access_count");
                    assert_eq!(
                        got_cycles.len(),
                        got_accesses as usize * usize::from(tracing == TracingMode::Full),
                        "{case}: the Full-mode vector must hold one entry per access"
                    );
                    for (i, (a, b)) in got_cycles.iter().zip(ref_cycles.iter()).enumerate() {
                        assert_eq!(a.kind, b.kind, "{case}: cycle {i} kind");
                        assert_eq!(a.address, b.address, "{case}: cycle {i} address");
                        assert_eq!(a.width, b.width, "{case}: cycle {i} width");
                        assert_eq!(
                            a.wait_states, b.wait_states,
                            "{case}: cycle {i} wait states"
                        );
                        assert_eq!(a.clocks, b.clocks, "{case}: cycle {i} clocks");
                    }
                    assert_eq!(got_cycles.len(), ref_cycles.len(), "{case}: cycle count");
                    // A split really did happen -- otherwise every equality above is vacuous.
                    if tracing == TracingMode::Full {
                        assert_eq!(
                            ref_cycles.len(),
                            width.bytes() as usize,
                            "{case}: the reference did not split"
                        );
                    }
                }
            }
        }
    }
}

/// SF-5 / L-RAM: the `claims_no_byte_in` guard behind the unaligned direct admission WOULD catch
/// a Distira-BAR page, so "no BAR page is `direct_ram_bytes`-able" is a CHECKED claim and not an
/// assumed one.
///
/// Why this matters more than a timing detail: `write_wide_memory`'s LFB arm SWALLOWS byte writes
/// (`BusWidth::Byte => {}` while still returning `true`), so a byte-split write into the BAR is
/// DROPPED where the wide write would have stored -- silent data loss, not a re-timing. The
/// unaligned admission is only safe because `ram_lookup_page_is_direct` refuses every page the
/// BAR overlaps, page-granularly.
///
/// The test has two halves and needs both:
///
/// 1. **Unmutated (this test).** With the BAR decoding over a RAM page, the admission must
///    DECLINE that page -- unreachable by construction -- while an ordinary RAM page is still
///    admitted. Proving the decline alone would be the fixtures-that-cannot-fail shape, hence the
///    positive control.
/// 2. **Mutated (recorded in the branch's mutation ledger).** Forcing `ram_lookup_page_is_direct`
///    to return `true` for a `memory_bar_overlaps` page makes the admission accept it, and
///    `claims_no_byte_in`'s `debug_assert` then fires. That is what gives half 1 its teeth.
#[test]
fn a_distira_bar_page_is_refused_the_unaligned_direct_admission() {
    // 64 MiB so a 16 MiB-aligned BAR can be parked at 32 MiB, INSIDE guest RAM -- which is the
    // only way to build the overlap this guard exists for.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(64, VideoCard::Vega),
        I386DX25_TEST_ROM,
    )
    .unwrap();
    const BAR: u32 = 0x0200_0000;
    const PLAIN: u32 = 0x0100_0000;

    // BAR base lives in config byte 0x13 (bits 31..24), and bit 1 of the command word enables
    // memory decode. Both are needed before `memory_bar_overlaps` answers true.
    machine.vega.pci_write_config_byte(0x13, (BAR >> 24) as u8);
    machine.vega.pci_write_config_byte(0x04, 0x02);
    let memory_len = machine.memory.len();
    machine.ram_lookup.rebuild(memory_len, &machine.vega);

    assert!(
        machine
            .vega
            .memory_bar_overlaps(BAR as usize, BAR as usize + 0x1000),
        "the BAR is not actually decoding over that page; the refusal below would be vacuous"
    );
    assert!(
        machine.vega.claims_no_byte_in(PLAIN, 4),
        "an ordinary RAM page must be claimed by nothing"
    );
    assert!(
        !machine.vega.claims_no_byte_in(BAR, 4),
        "the guard cannot see a page the BAR decodes; it would never fire on a real regression"
    );

    with_bus(&mut machine, |bus| {
        // A MISALIGNED read of ordinary RAM: admitted, `direct: true`. The positive control -- it
        // is what makes the refusal below a statement about the BAR and not about the width.
        let plain = bus
            .read_memory_direct(PLAIN + 1, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        assert!(
            plain.direct,
            "a misaligned read of plain RAM was not admitted; the BAR case proves nothing"
        );

        // The same shape over the BAR page: must NOT take the direct admission. If it did, the
        // per-byte `debug_assert` would fire; that it does not reach the assert at all is the
        // by-construction half of the argument.
        let bar = bus
            .read_memory_direct(BAR + 1, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        assert!(
            !bar.direct,
            "a Distira-BAR page was admitted to the unaligned direct path"
        );
    });
}

/// A word `IN` from 0x3DA must still decompose into two byte cycles at 0x3DA and 0x3DB.
///
/// This pins the reason the 0x3DA fast path sits BELOW the `width != BusWidth::Byte` block
/// rather than at the top of `read_io`. Hoisting it above that block would answer the whole
/// word from the status port: the returned value would lose the 0x3DB byte, and the run would
/// record one bus access where the decomposition records three (the outer word plus one per
/// byte), moving recorded bus clocks and therefore every clock derived from them.
#[test]
fn a_word_read_of_the_status_port_still_decomposes_into_two_byte_cycles() {
    let mut machine = test_machine();
    let (word, byte_3da, byte_3db, word_clocks, byte_clocks) = with_bus(&mut machine, |bus| {
        let before = bus.trace.elapsed_clocks();
        let word = bus.read_io(0x3DA, BusWidth::Word, 0, false).unwrap();
        let word_clocks = bus.trace.elapsed_clocks() - before;

        let before = bus.trace.elapsed_clocks();
        let byte_3da = bus.read_io(0x3DA, BusWidth::Byte, 0, false).unwrap();
        let byte_3db = bus.read_io(0x3DB, BusWidth::Byte, 0, false).unwrap();
        let byte_clocks = bus.trace.elapsed_clocks() - before;
        (word, byte_3da, byte_3db, word_clocks, byte_clocks)
    });

    assert_eq!(
        word & 0xff00,
        (byte_3db & 0xff) << 8,
        "the high byte of a word read must come from 0x3DB, not from the status port"
    );
    assert_eq!(
        byte_3da & 0xfe,
        word & 0xfe,
        "the low byte must still be the status port (bit 0 is beam-dependent and excluded)"
    );
    assert!(
        word_clocks > byte_clocks,
        "a word read records its own outer access on top of the two byte cycles \
         (word {word_clocks}, two bytes {byte_clocks})"
    );
}

/// The IDE bus-master block keeps precedence at 0x3DA when a guest points BAR4 at it.
///
/// This pins the reason the 0x3DA fast path sits BELOW the bus-master check. `piix_ide_bm_base`
/// is guest-programmable through PCI config BAR4 and is masked only with `!0x0f`, so 0x03D0 is
/// reachable and `BusMasterIde::owns_io` then claims the whole 0x03D0..=0x03DF block including
/// 0x3DA. Hoisting the fast path above that check would silently reassign the decode to the VGA.
///
/// Observed through the attribute flip-flop rather than through the returned byte, because both
/// arms can legitimately return the same value: the bus-master registers read back 0 here and the
/// VGA status bits are 0 at some beam positions, so a value comparison is vacuous. The flip-flop
/// is not: only a VGA status read resets it, so if it survives the read, the VGA did not answer.
/// The indirect observation technique is the one used by
/// `lazy_3da_read_still_resets_the_attribute_flip_flop_and_calls_catch_up` above.
#[test]
fn a_bus_master_base_over_the_status_port_keeps_its_decode() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);

    // PIIX4 IDE function (bus 0, device 7, function 1), BAR4 = 0x03D0. Driven through PCI
    // config space the way a guest does, rather than by reaching into the device.
    const IDE_DEVFN: u32 = (7 << 3) | 1;
    with_bus(&mut machine, |bus| {
        for (offset, byte) in [(0x20u32, 0xD0u32), (0x21, 0x03), (0x22, 0x00), (0x23, 0x00)] {
            let address = 0x8000_0000 | (IDE_DEVFN << 8) | (offset & 0xfc);
            bus.write_io(0x0cf8, BusWidth::Dword, address, false)
                .unwrap();
            bus.write_io(0x0cfc + (offset & 0x03) as u16, BusWidth::Byte, byte, false)
                .unwrap();
        }
    });
    assert_eq!(
        machine.pci.ide_bus_master_io_base(),
        Some(0x03D0),
        "the base must land where the test needs it, or the assertion below is vacuous"
    );

    with_bus(&mut machine, |bus| {
        // Arm the flip-flop in its DATA phase with exactly one 0x3C0 write, per the technique
        // documented on the lazy-3DA test above.
        bus.write_io(0x3C0, BusWidth::Byte, 0x05, false).unwrap();
        assert_eq!(
            bus.read_io(0x3C0, BusWidth::Byte, 0, false).unwrap(),
            0x05,
            "sanity: the index write took effect"
        );

        // The read under test. The bus-master block owns this port, so the VGA must not see it
        // and the flip-flop must survive.
        let _ = bus.read_io(0x3DA, BusWidth::Byte, 0, false).unwrap();

        // Still in the DATA phase means no VGA status read happened: this write is consumed as
        // data for index 0x05, so the index read-back is unchanged.
        bus.write_io(0x3C0, BusWidth::Byte, 0x0A, false).unwrap();
        assert_eq!(
            bus.read_io(0x3C0, BusWidth::Byte, 0, false).unwrap(),
            0x05,
            "the VGA answered 0x3DA and reset the attribute flip-flop, so the bus-master              block lost a decode it owns"
        );
    });
}

/// `Cpu::read_system_linear` reads GDT/LDT/IDT descriptors, TSS fields and the TSS I/O
/// permission bitmap through `read_memory_direct` rather than `read_memory`, to skip the
/// device-window probing `read_memory` does before it reaches the same RAM slice. That is
/// only legitimate if the two agree on BOTH the value and the recorded charge, because every
/// derived clock in the machine comes off `trace`.
///
/// This pins that equality at the widths the system-read family uses (Byte and Word for
/// selectors, access bytes and the I/O bitmap; Dword for descriptor halves), aligned and
/// misaligned -- misaligned matters because a TSS base is arbitrary, so the Word read of
/// TSS+0x66 lands on an odd address whenever the base is odd.
///
/// The `direct` assertion is what stops this from being vacuous: without it a fallthrough to
/// `read_memory` would make the test compare `read_memory` against itself and pass forever.
///
/// `data_access_wait_states` consults the data-cache tag array, which is STATEFUL: the first
/// touch of a line is a miss and costs more than every later one. A plain A-then-B pair reads
/// that miss as a difference between the two paths (it charged 5 against 2 while this test was
/// being written). So each address is warmed first and then measured A/B/A/B -- both repeats
/// must agree, which also catches a path that only looks equal on its first call.
///
/// BOTH timing classes, because they do not share a misaligned arm: the Approximate class
/// (`flat_data_cost`) folds a split into one `record_memory_run`, while the Accurate class
/// records N byte cycles. `access_count` is asserted alongside `elapsed_clocks` for the same
/// reason -- a future fold that collapsed N cycles into one run of equal total cost would keep
/// the clocks equal and still change what the trace reports.
#[test]
fn a_direct_system_read_charges_exactly_what_the_general_read_charges() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        const BASE: u32 = 0x2000;
        for i in 0..8u32 {
            machine.write_physical_u8(BASE + i, 0x10 + i as u8);
        }

        with_bus(&mut machine, |bus| {
            for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
                for offset in [0, 1] {
                    let address = BASE + offset;

                    // Warm the line so the tag-array miss is not attributed to whichever path
                    // runs first.
                    bus.read_memory(address, width, BusAccessKind::DataRead)
                        .unwrap();

                    let mut direct_reads = Vec::new();
                    let mut general_reads = Vec::new();
                    for _ in 0..2 {
                        let clocks = bus.trace.elapsed_clocks();
                        let accesses = bus.trace.access_count();
                        let direct = bus
                            .read_memory_direct(address, width, BusAccessKind::DataRead)
                            .unwrap();
                        direct_reads.push((
                            direct,
                            bus.trace.elapsed_clocks() - clocks,
                            bus.trace.access_count() - accesses,
                        ));

                        let clocks = bus.trace.elapsed_clocks();
                        let accesses = bus.trace.access_count();
                        let general = bus
                            .read_memory(address, width, BusAccessKind::DataRead)
                            .unwrap();
                        general_reads.push((
                            general,
                            bus.trace.elapsed_clocks() - clocks,
                            bus.trace.access_count() - accesses,
                        ));
                    }

                    for (direct, direct_clocks, direct_accesses) in &direct_reads {
                        assert!(
                            direct.direct,
                            "{mode:?} {width:?} at {address:#x} did not take the direct arm; the \
                             equalities below would compare read_memory against itself"
                        );
                        for (general, general_clocks, general_accesses) in &general_reads {
                            assert_eq!(
                                direct.value, *general,
                                "{mode:?} {width:?} at {address:#x} read a different value \
                                 through the direct arm"
                            );
                            assert_eq!(
                                direct_clocks, general_clocks,
                                "{mode:?} {width:?} at {address:#x} charged a different number of \
                                 clocks through the direct arm; every system read would move the \
                                 machine's timing"
                            );
                            assert_eq!(
                                direct_accesses, general_accesses,
                                "{mode:?} {width:?} at {address:#x} recorded a different number \
                                 of bus accesses through the direct arm"
                            );
                        }
                    }
                }
            }
        });
    }
}
