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

/// F7: the io poll skip's certificate must refuse structurally once `timing_epoch >= 2`,
/// independent of `IZARRAVM_ISA_IO_WAIT`/`IZARRAVM_DIRECT_POLL_SKIP` defaults.
#[test]
#[cfg(feature = "jit")]
fn epoch_2_refuses_the_poll_skip_certificate_structurally() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.timing_epoch = 2;
    // `test_machine()` arms detailed bus tracing, which is its own certificate refusal
    // (`trace.tracing_mode() != TracingMode::Off`) -- turn it off so this test exercises the
    // epoch refusal specifically, not that unrelated one.
    machine.trace.set_tracing_mode(TracingMode::Off);
    let before = machine.poll_skip_epoch_refusals();
    let refused = with_bus(&mut machine, |bus| {
        bus.poll_bus_certificate_from_for_test(0x03da).is_none()
    });
    assert!(
        refused,
        "epoch 2 must refuse the poll-skip certificate on 0x3DA"
    );
    assert_eq!(
        machine.poll_skip_epoch_refusals(),
        before + 1,
        "the structural refusal must be counted"
    );
}
