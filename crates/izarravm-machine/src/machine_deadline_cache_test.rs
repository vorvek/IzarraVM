// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The device-edge deadline cache behind `Machine::event_batch_cap_cached`.
//!
//! `event_batch_cap` (the fresh pull-scan) is the oracle throughout: the cache
//! is correct exactly when it agrees with it, and its correctness contract is
//! one-sided -- an earlier answer only shortens a batch, a LATER one lets a
//! device edge land mid-batch. Every assertion below is therefore an equality
//! against a fresh scan taken at the same instant.
//!
//! These tests are deliberately built so nothing invalidates the cache for them:
//! `advance_devices_ticks` and `with_bus` do NOT touch it (only the run loop and
//! the host-side mutators do), so an assertion that survives a long walk over
//! real device edges is testing the cache's own due-check and not the run
//! loop's blanket invalidation. Removing the `at <= now` rescan in
//! `event_batch_cap_cached` fails `the_cache_tracks_every_device_edge_it_armed`
//! on its first PIT period.

use super::*;

const COM1_THR: u16 = 0x03f8;
const COM1_IER: u16 = 0x03f9;
const COM1_LCR: u16 = 0x03fb;
const LPT1_DATA: u16 = 0x0378;
const LPT1_CONTROL: u16 = 0x037a;

const ALL_MODES: [GswMode; 4] = [
    GswMode::Gsw386Slow,
    GswMode::Gsw386,
    GswMode::Gsw486,
    GswMode::Gsw586,
];

fn out(machine: &mut Machine, port: u16, value: u8) {
    with_bus(machine, |bus| {
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    });
}

/// Channel 0, mode 2, `reload` PIT input clocks per period.
fn program_pit_channel0(machine: &mut Machine, reload: u16) {
    out(machine, 0x43, 0x34);
    out(machine, 0x40, reload as u8);
    out(machine, 0x40, (reload >> 8) as u8);
}

fn program_rtc_periodic(machine: &mut Machine) {
    out(machine, 0x70, 0x0b);
    out(machine, 0x71, 0x40); // PIE
}

/// 9600 baud, then one character queued for transmission.
fn transmit_one_serial_byte(machine: &mut Machine, byte: u8) {
    out(machine, COM1_LCR, 0x80);
    out(machine, COM1_THR, 12);
    out(machine, COM1_IER, 0);
    out(machine, COM1_LCR, 0x03);
    out(machine, COM1_THR, byte);
}

/// One printer byte plus a strobe pulse: an LPT busy deadline and an ACK
/// deadline after it.
fn print_one_lpt_byte(machine: &mut Machine, byte: u8) {
    out(machine, LPT1_DATA, byte);
    out(machine, LPT1_CONTROL, 0x11);
    out(machine, LPT1_CONTROL, 0x10);
}

/// A one-data-track, one-audio-track disc: enough for `mount_cd` / `eject_cd`
/// and for the CD front-panel transport, which declines on a data-only disc.
fn audio_disc() -> crate::cdimage::CdImage {
    use crate::cdimage::{DATA_SECTOR, RAW_SECTOR};
    let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
               TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
    let mut bin = vec![0u8; DATA_SECTOR + 100 * RAW_SECTOR];
    for byte in bin[DATA_SECTOR..].iter_mut() {
        *byte = 0x20;
    }
    crate::cdimage::CdImage::from_cue(cue, bin).unwrap()
}

/// SEEK to `cylinder`, which arms an FDC step deadline and an IRQ6 after it.
fn issue_seek(machine: &mut Machine, cylinder: u8) {
    with_bus(machine, |bus| {
        bus.write_io(0x3f2, BusWidth::Byte, 0x1c, false).unwrap(); // motor on, reset released
        for byte in [0x0fu8, 0x00, cylinder] {
            bus.write_io(0x3f5, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
}

/// 8-bit mono DSP DMA output at 11025 Hz over a 4096-frame block: one block IRQ
/// roughly 370 ms out, far past either batch fallback.
fn arm_sb16_block(machine: &mut Machine) {
    for index in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + index, 0x80);
    }
    with_bus(machine, |bus| {
        bus.write_io(0x0b, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xff, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0f, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0a, BusWidth::Byte, 0x01, false).unwrap();
        for byte in [0x41u8, 0x2b, 0x11, 0xc0, 0x00, 0xff, 0x0f] {
            bus.write_io(0x22c, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
}

/// Every device family that contributes a CACHED term, armed at once. The Margo
/// terms are deliberately not cached (see `Machine::vega_edge_ticks`) and so are
/// not part of this fixture.
fn arm_every_cached_device(machine: &mut Machine) {
    program_pit_channel0(machine, 1000);
    program_rtc_periodic(machine);
    arm_sb16_block(machine);
    transmit_one_serial_byte(machine, b'A');
    print_one_lpt_byte(machine, b'P');
    issue_seek(machine, 40);
    machine.inject_key_scancodes(&[0x1e, 0x9e]);
}

/// The cache's answer and a fresh pull-scan at this instant, in that order.
fn cached_and_fresh(machine: &mut Machine) -> (u64, u64) {
    let cached = machine.event_batch_cap_cached(u64::MAX);
    (cached, machine.event_batch_cap(u64::MAX))
}

#[test]
fn the_cache_tracks_every_device_edge_it_armed() {
    // The core property, with no help from any invalidation site: arm the whole
    // cached term list, drop the cache ONCE the way `run_until_tick` entry does,
    // then walk 30 ms of guest time in small steps. Nothing in the loop
    // invalidates, so every PIT period, the RTC periodic edge, the serial and
    // LPT completions, the FDC step chain and the SB16 block boundary have to be
    // caught by the cache's own "the edge I cached is due" rescan.
    for mode in ALL_MODES {
        let mut machine = test_machine();
        machine.set_mode(mode);
        arm_every_cached_device(&mut machine);
        machine.invalidate_device_edge_cache();

        // ~50 us per step, so the 1000-clock PIT period (~838 us) is crossed
        // repeatedly and is never landed on exactly.
        let step = izarravm_core::MASTER_CLOCK_HZ / 20_000;
        for iteration in 0..600 {
            let (cached, fresh) = cached_and_fresh(&mut machine);
            assert_eq!(cached, fresh, "{mode:?}: step {iteration}");
            machine.advance_devices_ticks(step);
        }
        // Non-vacuity: the walk must actually have crossed device edges, or the
        // loop above proves only that an unarmed machine stays unarmed.
        assert!(
            machine.pic.irr_bit(0),
            "{mode:?}: the walk must have crossed PIT channel-0 edges"
        );
        assert_eq!(
            machine.serial_output(),
            b"A",
            "{mode:?}: the serial transmit deadline must have fired"
        );
        assert_eq!(
            machine.lpt_output(),
            b"P",
            "{mode:?}: the LPT busy deadline must have fired"
        );
    }
}

#[test]
fn the_cache_follows_a_pit_reprogramming_down_to_the_shorter_period() {
    // Reprogramming a counter is a guest port write, which the run loop turns
    // into an invalidation via `io_touched`. Mimic exactly that one step (and
    // nothing else) so the assertion is about the cache picking up the NEW,
    // much earlier edge rather than about the walk above.
    for mode in ALL_MODES {
        let mut machine = test_machine();
        machine.set_mode(mode);
        program_pit_channel0(&mut machine, 60_000);
        machine.invalidate_device_edge_cache();
        let long = machine.event_batch_cap_cached(u64::MAX);

        program_pit_channel0(&mut machine, 20);
        machine.invalidate_device_edge_cache(); // the run loop's io_touched rule
        let (cached, fresh) = cached_and_fresh(&mut machine);
        assert_eq!(cached, fresh, "{mode:?}");
        assert!(
            cached < long,
            "{mode:?}: a 20-clock reload must bind tighter than a 60000-clock one \
             (long {long}, short {cached})"
        );
    }
}

#[test]
fn host_side_injection_and_media_changes_drop_the_cache_at_their_own_site() {
    // These run between run calls, so `run_until_tick` entry would cover them
    // anyway. They invalidate at their own site as well, and this is what proves
    // it: no run happens here at all.
    //
    // Asserted on the cache STATE, not on the cap: the fallback grain is 1 ms of
    // guest time, so a device edge further out than that (the 8042's own delivery
    // timer is exactly 1 ms, an SB16 block ~370 ms) never shows up in the cap at
    // all, and a cap comparison would pass with the invalidation deleted.
    //
    // `setup` runs BEFORE the fixture arms and caches an edge, so a mutator that
    // needs media present (the CD front-panel transport) can mount it without
    // the mount's own invalidation being what the assertion sees.
    type HostMutator = (&'static str, fn(&mut Machine), fn(&mut Machine));
    fn no_setup(_: &mut Machine) {}
    let mutators: [HostMutator; 14] = [
        ("inject_key_scancodes", no_setup, |machine| {
            machine.inject_key_scancodes(&[0x1e])
        }),
        ("inject_mouse", no_setup, |machine| {
            machine.inject_mouse(4, -4, 0)
        }),
        ("inject_mouse_wheel", no_setup, |machine| {
            machine.inject_mouse_wheel(1)
        }),
        ("set_mode", no_setup, |machine| {
            machine.set_mode(GswMode::Gsw386)
        }),
        ("seed_rtc", no_setup, |machine| {
            machine.seed_rtc(2026, 8, 9, 1, 3, 4, 5)
        }),
        ("set_cmos_byte", no_setup, |machine| {
            machine.set_cmos_byte(0x0a, 0x26)
        }),
        ("mount_floppy", no_setup, |machine| {
            machine.mount_floppy(vec![0u8; 1_474_560]).unwrap();
        }),
        ("eject_floppy", no_setup, |machine| {
            machine.eject_floppy();
        }),
        ("mount_hdd", no_setup, |machine| {
            machine.mount_hdd(vec![0u8; 64 * 512]);
        }),
        ("mount_cd", no_setup, |machine| {
            machine.mount_cd(audio_disc());
        }),
        (
            "eject_cd",
            |machine| machine.mount_cd(audio_disc()),
            |machine| machine.eject_cd(),
        ),
        (
            "eject_hdd",
            |machine| machine.mount_hdd(vec![0u8; 64 * 512]),
            |machine| {
                machine.eject_hdd();
            },
        ),
        (
            "cd_front_panel_play",
            |machine| machine.mount_cd(audio_disc()),
            |machine| machine.cd_front_panel_play(),
        ),
        (
            "cd_front_panel_stop",
            |machine| {
                machine.mount_cd(audio_disc());
                machine.cd_front_panel_play();
            },
            |machine| machine.cd_front_panel_stop(),
        ),
    ];

    for (name, setup, mutate) in mutators {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        setup(&mut machine);
        program_pit_channel0(&mut machine, 60_000);
        issue_seek(&mut machine, 12);
        machine.invalidate_device_edge_cache();
        machine.event_batch_cap_cached(u64::MAX);
        assert!(
            matches!(
                machine.device_edge_cache_state(),
                timing::DeviceEdgeCache::Due(_)
            ),
            "{name}: the fixture must leave a live cached edge to invalidate"
        );

        mutate(&mut machine);
        assert_eq!(
            machine.device_edge_cache_state(),
            timing::DeviceEdgeCache::Stale,
            "{name} must drop the device-edge cache at its own site"
        );
        let (cached, fresh) = cached_and_fresh(&mut machine);
        assert_eq!(cached, fresh, "{name}");
    }
}

#[test]
fn a_live_mode_switch_reconverts_the_cached_edge() {
    // The cache stores master ticks, which are mode-invariant, but the cap is
    // CPU clocks and the fallback grain is per-mode. Both directions.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    program_pit_channel0(&mut machine, 3_000);
    machine.invalidate_device_edge_cache();
    let fast = machine.event_batch_cap_cached(u64::MAX);

    machine.set_mode(GswMode::Gsw386Slow);
    let (slow_cached, slow_fresh) = cached_and_fresh(&mut machine);
    assert_eq!(slow_cached, slow_fresh);
    assert!(
        slow_cached < fast,
        "the same guest-time edge is fewer clocks on a slower CPU \
         (586 {fast}, 386-slow {slow_cached})"
    );

    machine.set_mode(GswMode::Gsw586);
    let (back_cached, back_fresh) = cached_and_fresh(&mut machine);
    assert_eq!(back_cached, back_fresh);
}

#[test]
fn the_run_loop_serves_most_batch_entries_from_the_cache() {
    // The point of the whole slice, stated as a measurement: a guest running
    // straight-line code with only a periodic timer armed must not re-scan the
    // device list on every batch. Without the cache this ratio is 1.0 by
    // construction.
    //
    // `cli` then a `nop`-padded self-loop, so the run takes no interrupt and
    // makes no port access: nothing in the loop invalidates except the PIT edges
    // themselves.
    let code = [
        0xfau8, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, // cli; nop x8
        0xeb, 0xf6, // jmp back over the nops
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);
    // The hit-rate counters are gated at the call site on the machine-phase
    // profiling flag, so that a normal run pays nothing for an instrument only
    // `--machine-phase-timing` ever reads. A test that asserts on the instrument
    // has to turn it on; without this the counters stay (0, 0) and the
    // assertions below fail, which is the gate's own non-vacuity proof.
    machine.enable_machine_profiling();
    program_pit_channel0(&mut machine, 60_000); // ~50 ms, the slowest useful period

    machine
        .run_master_ticks(izarravm_core::MASTER_CLOCK_HZ / 20)
        .unwrap(); // 50 ms of guest time
    let (batches, scans) = machine.device_edge_cache_counts();
    assert!(
        batches > 20,
        "the run must have entered many batches ({batches})"
    );
    assert!(
        scans * 4 < batches,
        "the cache must serve most batch entries: {scans} scans over {batches} batches"
    );
}
