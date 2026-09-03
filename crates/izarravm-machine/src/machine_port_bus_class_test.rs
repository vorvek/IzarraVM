// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! T1 (`dev_docs/2026-09-05-port-io-repricing-design.md` §5): a table-driven class matrix over
//! every named range plus both sides of each edge. Pure `const fn`, no machine.

use super::*;
use crate::bus::{PortBusClass, port_bus_class};

#[test]
fn class_matrix_named_ranges_and_edges() {
    // (port, expected class, label) -- one row per named range in the design's §1.2 table, plus
    // the boundary pairs the design calls out explicitly.
    let rows: &[(u16, PortBusClass, &str)] = &[
        // DMA1 / page regs / DMA2.
        (0x0000, PortBusClass::IsaXBus, "dma1 low"),
        (0x000F, PortBusClass::IsaXBus, "dma1 high"),
        (0x0080, PortBusClass::IsaXBus, "page regs low"),
        (0x008F, PortBusClass::IsaXBus, "page regs high"),
        // F12 (`dev_docs/2026-09-05-port-io-repricing-review.md`): the page-register aliases
        // at 0x90-0x9F (`dma_page_register_port`), except 0x92 which is its own device.
        (0x0090, PortBusClass::IsaXBus, "page reg alias low"),
        (
            0x0092,
            PortBusClass::IsaXBus,
            "system control port a (within the alias block)",
        ),
        (
            0x0093,
            PortBusClass::IsaXBus,
            "page reg alias, just past 0x92",
        ),
        (0x009F, PortBusClass::IsaXBus, "page reg alias high"),
        (0x00C0, PortBusClass::IsaXBus, "dma2 low"),
        (0x00DF, PortBusClass::IsaXBus, "dma2 high"),
        (
            0x00E0,
            PortBusClass::ChipsetInternal,
            "lotura low (0xDF/0xE0 edge)",
        ),
        (0x00E7, PortBusClass::ChipsetInternal, "lotura high"),
        (
            0x00E8,
            PortBusClass::Unclaimed,
            "past lotura (0xE7/0xE8 edge)",
        ),
        // 8259A PICs.
        (0x0020, PortBusClass::IsaXBus, "pic1 low"),
        (0x0021, PortBusClass::IsaXBus, "pic1 high"),
        (0x00A0, PortBusClass::IsaXBus, "pic2 low"),
        (0x00A1, PortBusClass::IsaXBus, "pic2 high"),
        // 8042 keyboard controller.
        (0x0060, PortBusClass::IsaXBus, "8042 data"),
        (0x0064, PortBusClass::IsaXBus, "8042 status/command"),
        // x87 error-ack latches.
        (0x00F0, PortBusClass::IsaXBus, "x87 ack low"),
        (0x00FF, PortBusClass::IsaXBus, "x87 ack high"),
        // 8254 PIT / PPI port B + mirror / RTC.
        (0x0040, PortBusClass::IsaXBus, "pit low"),
        (0x0043, PortBusClass::IsaXBus, "pit high"),
        (0x0061, PortBusClass::IsaXBus, "ppi port b"),
        (0x0063, PortBusClass::IsaXBus, "ppi port b mirror"),
        (0x0070, PortBusClass::IsaXBus, "rtc index"),
        (0x0071, PortBusClass::IsaXBus, "rtc data"),
        // 0x92, the one port TOKAEMM traps.
        (0x0092, PortBusClass::IsaXBus, "system control port a"),
        // IDE command-block / control-block.
        (
            0x01EF,
            PortBusClass::Unclaimed,
            "before ide (0x1EF/0x1F0 edge)",
        ),
        (0x01F0, PortBusClass::PciTarget, "ide data"),
        (0x01F7, PortBusClass::PciTarget, "ide status"),
        (
            0x01F8,
            PortBusClass::Unclaimed,
            "past ide (0x1F7/0x1F8 edge)",
        ),
        (0x03F6, PortBusClass::PciTarget, "ide control block"),
        (0x0170, PortBusClass::PciTarget, "ide secondary data"),
        (0x0177, PortBusClass::PciTarget, "ide secondary status"),
        (0x0376, PortBusClass::PciTarget, "ide secondary control"),
        // Gameport, legacy audio, LPT/COM, FDC.
        (0x0200, PortBusClass::IsaXBus, "gameport low"),
        (
            0x0201,
            PortBusClass::IsaXBus,
            "gameport (wolf3d's second site)",
        ),
        (0x0207, PortBusClass::IsaXBus, "gameport high"),
        (0x0220, PortBusClass::IsaXBus, "sb low"),
        (0x022F, PortBusClass::IsaXBus, "sb high"),
        // F12 edge list around the OPL block.
        (
            0x0387,
            PortBusClass::Unclaimed,
            "before opl (0x0387/0x0388 edge)",
        ),
        (0x0388, PortBusClass::IsaXBus, "opl low"),
        (0x038B, PortBusClass::IsaXBus, "opl high"),
        (
            0x038C,
            PortBusClass::Unclaimed,
            "past opl (0x038B/0x038C edge)",
        ),
        // F12 edge list around the SB block (0x0225/0x0226/0x0230): 0x220-0x22F is SB, so
        // 0x225 is inside it and 0x230 is just past it.
        (0x0225, PortBusClass::IsaXBus, "inside sb block"),
        (0x0226, PortBusClass::IsaXBus, "sb dsp write/data port"),
        (
            0x0230,
            PortBusClass::Unclaimed,
            "past sb block (0x022F/0x0230 edge)",
        ),
        (0x0300, PortBusClass::IsaXBus, "mpu low block"),
        (0x0330, PortBusClass::IsaXBus, "mpu high block"),
        (0x0530, PortBusClass::IsaXBus, "wss low"),
        (0x0537, PortBusClass::IsaXBus, "wss high"),
        (0x0278, PortBusClass::IsaXBus, "lpt2"),
        (0x0378, PortBusClass::IsaXBus, "lpt1"),
        (0x02F8, PortBusClass::IsaXBus, "com2"),
        (0x03F8, PortBusClass::IsaXBus, "com1"),
        (
            0x03FD,
            PortBusClass::IsaXBus,
            "com1 line status (nascar's site)",
        ),
        (0x03F0, PortBusClass::IsaXBus, "fdc low"),
        (0x03F7, PortBusClass::IsaXBus, "fdc digital input"),
        // Legacy VGA, including 0x3DA.
        (
            0x03AF,
            PortBusClass::Unclaimed,
            "before vga (0x3AF/0x3B0 edge)",
        ),
        (0x03B0, PortBusClass::PciLegacyVga, "vga low"),
        (
            0x03DA,
            PortBusClass::PciLegacyVga,
            "vga input status 1 (wolf3d's first site)",
        ),
        (0x03DF, PortBusClass::PciLegacyVga, "vga high"),
        (
            0x03E0,
            PortBusClass::Unclaimed,
            "past vga (0x3DF/0x3E0 edge)",
        ),
        // PCI configuration mechanism.
        (
            0x0CF7,
            PortBusClass::Unclaimed,
            "before pci config (0x0CF7/0x0CF8 edge)",
        ),
        (0x0CF8, PortBusClass::PciTarget, "pci config address"),
        (0x0CFF, PortBusClass::PciTarget, "pci config data high"),
        (
            0x0D00,
            PortBusClass::Unclaimed,
            "past pci config (0x0CFF/0x0D00 edge)",
        ),
        // Unclaimed, elsewhere.
        (0xFFFF, PortBusClass::Unclaimed, "top of the port space"),
    ];

    for &(port, expected, label) in rows {
        assert_eq!(
            port_bus_class(port),
            expected,
            "port {port:#06x} ({label}) classified {:?}, expected {:?}",
            port_bus_class(port),
            expected
        );
    }
}

#[test]
fn class_clocks_match_the_design_table() {
    assert_eq!(PortBusClass::IsaXBus.clocks(), 160);
    assert_eq!(PortBusClass::PciLegacyVga.clocks(), 56);
    assert_eq!(PortBusClass::PciTarget.clocks(), 56);
    assert_eq!(PortBusClass::Unclaimed.clocks(), 56);
    assert_eq!(PortBusClass::ChipsetInternal.clocks(), 0);
}

#[test]
fn legacy_isa_predicate_is_unchanged_by_the_new_classifier() {
    // `port_is_legacy_isa_io` keeps gating epoch 1's `IZARRAVM_ISA_IO_WAIT` charge exactly as
    // it did before this slice; it is a narrower, independent set from `IsaXBus`.
    use crate::bus::port_is_legacy_isa_io_for_test;
    for port in 0u32..=0xFFFF {
        let port = port as u16;
        let legacy = port_is_legacy_isa_io_for_test(port);
        if legacy {
            // Every port the epoch-1 predicate covers is a subset of the epoch-2 `IsaXBus`
            // class -- the seven-port table did not move.
            assert_eq!(
                port_bus_class(port),
                PortBusClass::IsaXBus,
                "port {port:#06x} is legacy-ISA under epoch 1 but not IsaXBus under epoch 2"
            );
        }
    }
    // And the predicate is still exactly the seven-port set the design cites, not the wider
    // epoch-2 IsaXBus class.
    assert!(port_is_legacy_isa_io_for_test(0x0040));
    assert!(!port_is_legacy_isa_io_for_test(0x0020)); // PIC1: IsaXBus under epoch 2, not legacy today
    assert!(!port_is_legacy_isa_io_for_test(0x0092)); // 0x92: IsaXBus under epoch 2, not legacy today
}

#[test]
fn timing_epoch_spelling_table() {
    use crate::bus::parse_timing_epoch_for_test;
    assert_eq!(
        parse_timing_epoch_for_test(Err(std::env::VarError::NotPresent)),
        1
    );
    assert_eq!(parse_timing_epoch_for_test(Ok(String::new())), 1);
    assert_eq!(parse_timing_epoch_for_test(Ok("2".to_string())), 2);
}

#[test]
#[should_panic(expected = "IZARRAVM_TIMING_EPOCH")]
fn timing_epoch_rejects_zero_as_an_off_spelling() {
    // memory `parameter-knobs-have-no-off-spelling`: an epoch names a version, not an on/off
    // state, so `0` must panic like any other unrecognized spelling, never silently mean
    // epoch 1.
    use crate::bus::parse_timing_epoch_for_test;
    parse_timing_epoch_for_test(Ok("0".to_string()));
}

/// T2-lite: the epoch-2 per-class charge for one byte cycle, on an I586 machine (so
/// `bus_num_at_batch_start`/`bus_den_at_batch_start` are `(16, 105)` and the generic 4-raw-clock
/// cycle scales to 0, making the charge exactly the class figure -- design §1.3's headline
/// numbers). Epoch 1 (the knob left unset) must charge nothing extra on a non-legacy port.
#[test]
fn epoch_2_charges_the_class_figure_per_byte_cycle() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);

    // Epoch 1 (unset): a gameport read charges through the pre-slice predicate only, which
    // does not cover 0x201 -- no port-bus accrual at all.
    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        bus.read_io(0x0201, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 0,
        "epoch 1 must not charge a gameport read (not in the seven-port legacy set)"
    );

    // Epoch 2: every port charges its class figure. IsaXBus (gameport 0x201) -> 160.
    machine.timing_epoch = 2;
    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        bus.read_io(0x0201, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 160,
        "epoch 2 IsaXBus charge must be exactly 160 (the generic 4-raw cycle scales to 0 \
         under I586's (16,105) bus ratio)"
    );

    // PciLegacyVga (0x3DA) -> 56. Reset between accesses since 0x3DA has its own fast-path
    // read arm but still routes through the same `charge_port_bus` call.
    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        bus.read_io(0x03DA, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 56,
        "epoch 2 PciLegacyVga charge must be 56"
    );

    // ChipsetInternal (Lotura 0xE0) -> 0.
    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        bus.read_io(0x00E0, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 0,
        "epoch 2 ChipsetInternal charge must be 0 (Lotura, a host-native device)"
    );
}

/// F3 (`dev_docs/2026-09-05-port-io-repricing-review.md`): under epoch 2, `IN 0x388` (OPL
/// status) must pay exactly the `IsaXBus` class charge once, not twice -- the OPL arm's own
/// hand-accrual must be epoch-1-only.
#[test]
fn epoch_2_does_not_double_charge_the_opl_status_read() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.timing_epoch = 2;
    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        bus.read_io(0x0388, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 160,
        "IN 0x388 under epoch 2 must charge IsaXBus (160) exactly once, not 160 + 166 from the \
         OPL arm's own pre-slice hand accrual"
    );
}

/// P2 replaces F7's structural epoch refusal (and the older ISA refusal) with an explicit
/// admission: under epoch 2 the certificate ADMITS 0x3DA and carries the port's own class
/// charge on its third, unscaled lane. The counter that says so must move.
#[test]
#[cfg(feature = "jit")]
fn epoch_2_admits_the_poll_skip_certificate_with_the_port_lane() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.timing_epoch = 2;
    // `test_machine()` arms detailed bus tracing, which is its own certificate refusal
    // (`trace.tracing_mode() != TracingMode::Off`) -- turn it off so this test exercises the
    // admission specifically.
    machine.trace.set_tracing_mode(TracingMode::Off);
    let (admitted_before, _, _) = machine.poll_skip_certificate_counters();
    let lane = with_bus(&mut machine, |bus| {
        bus.poll_bus_certificate_from_for_test(0x03da)
            .map(|certificate| certificate.port_bus_clocks_per_iteration())
    });
    // `PciLegacyVga` 56, minus the generic byte cycle `read_io` records and scales (4 raw
    // through the I586 bus dial 16/105 = 0), so lane + generic == the class figure exactly.
    assert_eq!(
        lane,
        Some(56),
        "epoch 2 must ADMIT 0x3DA and price the iteration at the PciLegacyVga class charge"
    );
    let (admitted_after, _, _) = machine.poll_skip_certificate_counters();
    assert_eq!(
        admitted_after,
        admitted_before + 1,
        "the epoch-2 admission must be counted"
    );
}

/// F9: the refusal is REPLACED, not deleted. Any port other than `POLL_SKIP_IO_PORT` is
/// declined by name and counted -- the lane prices a port, it cannot express a port whose read
/// is not idempotent (the 8254 read-back latch, the 0x61 refresh toggle).
#[test]
#[cfg(feature = "jit")]
fn a_port_other_than_3da_is_refused_by_name_in_both_epochs() {
    for epoch in [1, 2] {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        machine.timing_epoch = epoch;
        machine.trace.set_tracing_mode(TracingMode::Off);
        let (_, refused_before, _) = machine.poll_skip_certificate_counters();
        let refused = with_bus(&mut machine, |bus| {
            bus.poll_bus_certificate_from_for_test(0x0040).is_none()
        });
        assert!(
            refused,
            "epoch {epoch}: the certificate must refuse a port it is not admitted for"
        );
        let (_, refused_after, _) = machine.poll_skip_certificate_counters();
        assert_eq!(
            refused_after,
            refused_before + 1,
            "epoch {epoch}: the named refusal must be counted"
        );
    }
}

/// Epoch 1 keeps the pre-P2 certificate byte for byte: 0x3DA is admitted, and its third lane is
/// structurally zero, so nothing about the elided iteration's price moves on a knob-unset build.
#[test]
#[cfg(feature = "jit")]
fn epoch_1_admits_3da_with_a_zero_port_lane() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.timing_epoch = 1;
    machine.trace.set_tracing_mode(TracingMode::Off);
    let lane = with_bus(&mut machine, |bus| {
        bus.poll_bus_certificate_from_for_test(0x03da)
            .map(|certificate| certificate.port_bus_clocks_per_iteration())
    });
    assert_eq!(lane, Some(0));
    let (admitted, _, _) = machine.poll_skip_certificate_counters();
    assert_eq!(
        admitted, 0,
        "the epoch-2 admission counter must stay zero under epoch 1"
    );
}

// ---------------------------------------------------------------------------------------------
// F2: the port lane counts against the batch cap.
//
// `run.rs`'s own overshoot note stated the hole outright -- "`port_bus_batch_clocks` joins the
// batch-end `step` without ever having been counted against the cap". With seven charged ports
// that was hundreds of clocks. Under epoch 2 every port is charged, so a mode-set burst of 768
// `OUT 0x3C9` would accrue ~40,000 clocks (a quarter of a PIT tick) that no cap test could see,
// and the batch would run past a device deadline by that much with interrupt edge placement
// downstream.
//
// The fix has two halves and BOTH are load-bearing, which is why they are pinned separately:
// `in_batch_scaled_bus_clocks()` (what `spent` is built from, run.rs) now carries the lane, and
// `in_batch_scaled_bus_clocks_screen_scale()` returns 0 so the CPU's per-instruction screen stops
// answering "certainly below the cap" from a bound that no longer holds.
// ---------------------------------------------------------------------------------------------

/// A hundred `OUT 0x3C9, AL` -- a VGA palette load, the mode-set burst the review names -- driven
/// through the bus in the RING-0 PROTECTED-MODE regime, where `skip_io_touched` is live and the
/// batch loop's `io_touched` break does NOT end the batch after each access. That regime is the
/// only one in which the overshoot is interesting: a real-mode guest's `OUT` sets `io_touched`,
/// which breaks the batch on its own, so a fixture built there would pass whatever this
/// accounting did.
fn burst_of_palette_writes(machine: &mut Machine, count: u32) {
    with_bus(machine, |bus| {
        for i in 0..count {
            bus.write_io(0x03C9, BusWidth::Byte, i & 0x3f, true)
                .expect("the palette port must accept a write");
        }
    });
}

#[test]
fn epoch_2_counts_the_port_lane_against_the_batch_cap() {
    // `run.rs:1666` is literally `let spent = u64::from(batch_core) + bus.in_batch_scaled_bus_clocks();`
    // so this accessor IS the cap accounting. Under epoch 1 the lane is invisible to it (the
    // pre-slice behaviour, byte-identical); under epoch 2 it must be there in full.
    const BURST: u32 = 100;
    // PciLegacyVga (0x3C9 is in the 0x3C0-0x3CF VGA register block) = 56 clocks a byte cycle,
    // less the generic 4-raw cycle scaled by I586's (16, 105), which floors to 0.
    const PER_ACCESS: u64 = 56;

    for epoch in [1u32, 2] {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        machine.timing_epoch = epoch;
        machine.port_bus_batch_clocks = 0;
        let before = with_bus(&mut machine, |bus| bus.in_batch_scaled_bus_clocks());
        burst_of_palette_writes(&mut machine, BURST);
        let lane = machine.port_bus_batch_clocks;
        let after = with_bus(&mut machine, |bus| bus.in_batch_scaled_bus_clocks());

        if epoch == 1 {
            assert_eq!(
                lane, 0,
                "epoch 1 must not charge the VGA palette port at all (86Box parity)"
            );
            assert_eq!(
                after, before,
                "epoch 1's cap accounting must be byte-identical to the pre-slice figure"
            );
        } else {
            assert_eq!(
                lane,
                u64::from(BURST) * PER_ACCESS,
                "epoch 2 must charge every write in the burst"
            );
            assert_eq!(
                after - before,
                lane,
                "the whole port lane must be visible to the cap accounting; the burst is the \
                 review's own example and the difference here is exactly the overshoot F2 names"
            );
        }
    }
}

#[test]
fn the_division_free_cap_test_agrees_with_the_value_form_under_both_epochs() {
    // The run loop asks `in_batch_scaled_bus_clocks_at_least(target)` once per retired
    // instruction and takes its answer as EXACTLY `in_batch_scaled_bus_clocks() >= target`. Epoch
    // 2 adds an unscaled additive term to the value form that does not survive the u128
    // multiply-through, so the two forms had to stop sharing an implementation -- and a
    // divergence between them would move run boundaries with nothing to notice.
    for epoch in [1u32, 2] {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        machine.timing_epoch = epoch;
        machine.port_bus_batch_clocks = 0;
        burst_of_palette_writes(&mut machine, 7);
        with_bus(&mut machine, |bus| {
            let value = bus.in_batch_scaled_bus_clocks();
            assert!(
                epoch == 1 || value > 0,
                "epoch 2's burst must have moved the figure, or the sweep below proves nothing"
            );
            for target in 0..=value + 8 {
                assert_eq!(
                    bus.in_batch_scaled_bus_clocks_at_least(target),
                    value >= target,
                    "epoch {epoch}: the two cap-test forms disagreed at target {target} \
                     (value {value})"
                );
            }
            assert!(
                !bus.in_batch_scaled_bus_clocks_at_least(u64::MAX),
                "epoch {epoch}: an unreachable target must answer 'not at the cap'"
            );
        });
    }
}

#[test]
fn epoch_2_turns_the_per_instruction_cap_screen_off() {
    // The screen's contract (`CpuBus::in_batch_scaled_bus_clocks_screen_scale`) is a per-batch
    // constant F with `S(raw2) - S(raw1) <= (raw2 - raw1) * F`. Epoch 2's figure carries a term
    // that is not a function of raw bus clocks, so the bus ratio is NOT such an F -- and the
    // screen only ever skips the exact test, so an invalid F silently removes cap breaks. The
    // trait spells `0` as "no bound offered", which sends every ask to the exact test.
    //
    // Non-vacuity: the burst below is measured against the epoch-1 F on the same machine, and the
    // arithmetic showing why that F fails is asserted rather than described.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);

    machine.timing_epoch = 1;
    let epoch_1_screen = with_bus(&mut machine, |bus| {
        bus.in_batch_scaled_bus_clocks_screen_scale()
    });
    assert_eq!(
        epoch_1_screen, 1,
        "I586's (16, 105) bus ratio gives ceil(num/den).max(1) = 1; if this moved, the \
         arithmetic below is stale"
    );

    machine.timing_epoch = 2;
    assert_eq!(
        with_bus(&mut machine, |bus| bus
            .in_batch_scaled_bus_clocks_screen_scale()),
        0,
        "epoch 2 must offer no screen bound at all"
    );

    // WHY 1 would have been wrong, in the same units the screen uses. One palette write records
    // one 4-raw-clock generic I/O cycle and accrues 56 unscaled clocks on the port lane, so the
    // scaled figure grows by 56 for 4 raw clocks -- fourteen times the bound the epoch-1 F claims.
    machine.port_bus_batch_clocks = 0;
    let (raw_growth, scaled_growth) = with_bus(&mut machine, |bus| {
        let raw_before = bus.in_batch_raw_bus_clocks();
        let scaled_before = bus.in_batch_scaled_bus_clocks();
        bus.write_io(0x03C9, BusWidth::Byte, 0, true).unwrap();
        (
            bus.in_batch_raw_bus_clocks() - raw_before,
            bus.in_batch_scaled_bus_clocks() - scaled_before,
        )
    });
    assert!(
        scaled_growth > raw_growth * epoch_1_screen,
        "one epoch-2 port access grew the scaled figure by {scaled_growth} over {raw_growth} raw \
         clocks, which the epoch-1 screen bound of {epoch_1_screen} per raw clock does NOT cover \
         -- this is the arithmetic that makes the screen unsound under epoch 2, and if it ever \
         stops holding the screen could be re-armed with a widened bound instead of turned off"
    );
}

/// T7's machine half: the charge follows the EXECUTED access. The CPU-side fixture
/// (`izarravm-cpu`'s `a_bitmap_denied_port_charges_no_column_and_the_epoch_cannot_change_that`)
/// pins that the FAULTING V86 instruction pays no column; this one pins that the monitor's own
/// 0x92 access -- the ring-0 instruction the `#GP` handler eventually runs -- pays the `IsaXBus`
/// class charge exactly once, on both the read and the write side.
#[test]
fn the_monitors_own_0x92_access_pays_the_isa_class_charge_once() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.timing_epoch = 2;

    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        // `cpu_is_ring0_pm = true`: the monitor, not the trapped V86 task.
        bus.read_io(0x0092, BusWidth::Byte, 0, true).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 160,
        "the monitor's 0x92 read must pay IsaXBus (160) exactly once"
    );

    machine.port_bus_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0092, BusWidth::Byte, 0x02, true).unwrap();
    });
    assert_eq!(
        machine.port_bus_batch_clocks, 160,
        "the monitor's 0x92 write must pay IsaXBus (160) exactly once -- 0x92 is one of the four \
         ports the ring-0 io_touched exemption does NOT cover, so this arm has side effects (A20) \
         the charge must not be folded into or duplicated by"
    );
}
