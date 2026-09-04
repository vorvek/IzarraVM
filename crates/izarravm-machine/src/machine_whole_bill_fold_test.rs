// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 2, the whole-bill fold (`dev_docs/2026-09-05-recalibration-slice2-brief.md` §1):
//! under `IZARRAVM_TIMING_EPOCH=2` a CACHED-RAM instruction fetch and an L1-hit
//! data access charge nothing, because Intel's per-instruction class counts --
//! which the epoch-2 table charges from -- already contain both. Everything that
//! is not a cached-RAM route keeps its cost, which is the half the design calls
//! out as "too broad" in the first draft: ROM, UMB, shadow and device-window
//! fetches, and every aperture data access.
//!
//! Every test states BOTH arms. Epoch 1 is the merge bar, so a fold that leaked
//! into it would show up here as a changed epoch-1 number rather than only in the
//! fixture identity run.

use super::*;

/// Conventional RAM, well below the 0xA0000 aperture: the cached-RAM route.
const RAM_ADDRESS: u32 = 0x0002_0000;
/// The mode-13h aperture: a device window, never folded.
const APERTURE_ADDRESS: u32 = 0x000A_1000;
/// Inside the low BIOS ROM window: uncached code, never folded.
const ROM_ADDRESS: u32 = 0x000F_0000;

fn fold_machine(mode: GswMode, epoch: u32) -> Machine {
    let mut machine = test_machine();
    machine.set_mode(mode);
    machine.set_timing_epoch_for_test(epoch);
    machine
}

/// Raw bus clocks charged by one instruction-fetch run of `len` bytes at `address`.
fn fetch_run_clocks(machine: &mut Machine, address: u32, len: u32) -> u64 {
    let before = machine.trace.elapsed_clocks();
    with_bus(machine, |bus| {
        bus.charge_instruction_fetch_run(address, len)
            .expect("instruction fetch run");
    });
    machine.trace.elapsed_clocks() - before
}

/// Raw bus clocks charged by one dword data read at `address`.
fn data_read_clocks(machine: &mut Machine, address: u32) -> u64 {
    let before = machine.trace.elapsed_clocks();
    with_bus(machine, |bus| {
        let _ = bus.read_memory(address, BusWidth::Dword, BusAccessKind::DataRead);
    });
    machine.trace.elapsed_clocks() - before
}

#[test]
fn epoch_2_folds_the_cached_ram_instruction_fetch_to_zero() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut epoch1 = fold_machine(mode, 1);
        let charged = fetch_run_clocks(&mut epoch1, RAM_ADDRESS, 3);
        assert!(
            charged > 0,
            "{mode:?}: epoch 1 must still charge a cached-RAM fetch (it charged {charged})"
        );

        let mut epoch2 = fold_machine(mode, 2);
        assert_eq!(
            fetch_run_clocks(&mut epoch2, RAM_ADDRESS, 3),
            0,
            "{mode:?}: epoch 2 must fold the cached-RAM fetch into the class count"
        );
        // Every fetch length takes the same route, including the per-byte cold
        // fallback's own length.
        assert_eq!(fetch_run_clocks(&mut epoch2, RAM_ADDRESS, 1), 0, "{mode:?}");
        assert_eq!(
            fetch_run_clocks(&mut epoch2, RAM_ADDRESS, 15),
            0,
            "{mode:?}"
        );
    }
}

#[test]
fn epoch_2_leaves_rom_and_device_window_fetches_charged() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut epoch1 = fold_machine(mode, 1);
        let mut epoch2 = fold_machine(mode, 2);
        for address in [ROM_ADDRESS, APERTURE_ADDRESS] {
            let one = fetch_run_clocks(&mut epoch1, address, 3);
            let two = fetch_run_clocks(&mut epoch2, address, 3);
            assert!(
                two > 0,
                "{mode:?} {address:#x}: an uncached fetch must never be folded -- BIOS and \
                 option ROMs would run free"
            );
            assert_eq!(
                one, two,
                "{mode:?} {address:#x}: the uncached fetch route must not move across the epoch"
            );
        }
    }
}

#[test]
fn epoch_2_folds_the_l1_hit_data_access_to_zero() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut epoch1 = fold_machine(mode, 1);
        // Warm, then measure: the first touch of a line is the same route as the
        // second on these personas (`flat_data_cost`), but warming keeps the test
        // honest if that ever stops being true.
        let _ = data_read_clocks(&mut epoch1, RAM_ADDRESS);
        let charged = data_read_clocks(&mut epoch1, RAM_ADDRESS);
        assert!(
            charged > 0,
            "{mode:?}: epoch 1 must still charge an L1-hit data access (it charged {charged})"
        );

        let mut epoch2 = fold_machine(mode, 2);
        let _ = data_read_clocks(&mut epoch2, RAM_ADDRESS);
        assert_eq!(
            data_read_clocks(&mut epoch2, RAM_ADDRESS),
            0,
            "{mode:?}: epoch 2 must fold the L1-hit data access into the class count"
        );
    }
}

#[test]
fn epoch_2_leaves_the_aperture_data_access_charged() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut epoch1 = fold_machine(mode, 1);
        let mut epoch2 = fold_machine(mode, 2);
        let one = data_read_clocks(&mut epoch1, APERTURE_ADDRESS);
        let two = data_read_clocks(&mut epoch2, APERTURE_ADDRESS);
        assert!(
            two > 0,
            "{mode:?}: an aperture access is not inside anyone's instruction count"
        );
        assert_eq!(
            one, two,
            "{mode:?}: the aperture route's RAW charge must not move across the epoch (the \
             mode-13h wait state is slice 4's, not this one's)"
        );
    }
}

#[test]
fn the_386_is_out_of_scope_and_folds_nothing() {
    // No epoch-2 class table exists for the 386, so nothing has absorbed its
    // fetch and data terms; folding them would simply delete them.
    let mut epoch1 = fold_machine(GswMode::Gsw386, 1);
    let mut epoch2 = fold_machine(GswMode::Gsw386, 2);
    assert_eq!(
        fetch_run_clocks(&mut epoch1, RAM_ADDRESS, 3),
        fetch_run_clocks(&mut epoch2, RAM_ADDRESS, 3),
    );
    let _ = data_read_clocks(&mut epoch1, RAM_ADDRESS);
    let _ = data_read_clocks(&mut epoch2, RAM_ADDRESS);
    assert_eq!(
        data_read_clocks(&mut epoch1, RAM_ADDRESS),
        data_read_clocks(&mut epoch2, RAM_ADDRESS),
    );
}

#[test]
fn epoch_2_takes_the_fast_personas_bus_ratio_to_one() {
    // The fold's third leg: with fetch and L1 data out of the bus, every clock
    // that still rides it is a real clock. Stated here so a change to either
    // half without the other fails a test rather than a fixture.
    for persona in [CpuPersona::I486, CpuPersona::I586] {
        assert_eq!(bus_timing(persona, 2), (1, 1), "{persona:?}");
        assert_ne!(bus_timing(persona, 1), (1, 1), "{persona:?}");
    }
    assert_eq!(
        bus_timing(CpuPersona::I386, 2),
        bus_timing(CpuPersona::I386, 1),
        "the 386 is out of the recalibration's scope"
    );
}

#[test]
fn epoch_2_tier_costs_stay_inside_their_pre_registered_ranges() {
    // The slice's own no-fitting rule: a knob that has to leave its physical
    // range is a finding, not a fit. The ranges are `cache_config::tier_cost`'s
    // documented ones, written before the slice was measured.
    let l586 = crate::cache_config::tier_cost(GswMode::Gsw586, 2);
    assert_eq!(l586.l1, 0);
    assert!((8..=16).contains(&l586.l2), "586 L2 ws {}", l586.l2);
    assert!((23..=38).contains(&l586.ram), "586 RAM ws {}", l586.ram);

    let l486 = crate::cache_config::tier_cost(GswMode::Gsw486, 2);
    assert_eq!(l486.l1, 0);
    assert!((2..=8).contains(&l486.l2), "486 L2 ws {}", l486.l2);
    assert!((8..=18).contains(&l486.ram), "486 RAM ws {}", l486.ram);

    // The 386 keeps its epoch-1 tiers under both epochs.
    assert_eq!(
        [
            crate::cache_config::tier_cost(GswMode::Gsw386, 2).l1,
            crate::cache_config::tier_cost(GswMode::Gsw386, 2).l2,
            crate::cache_config::tier_cost(GswMode::Gsw386, 2).ram,
        ],
        [
            crate::cache_config::tier_cost(GswMode::Gsw386, 1).l1,
            crate::cache_config::tier_cost(GswMode::Gsw386, 1).l2,
            crate::cache_config::tier_cost(GswMode::Gsw386, 1).ram,
        ],
    );
}

#[test]
fn epoch_2_gives_each_fast_persona_its_own_cache_line() {
    use crate::cache_config::{CACHE_LINE_BYTES, cache_line_bytes};
    // Census row 4: the P55C's line is 32 bytes and the 486DX2's is 16; 64 was
    // nobody's.
    assert_eq!(cache_line_bytes(GswMode::Gsw586, 2), 32);
    assert_eq!(cache_line_bytes(GswMode::Gsw486, 2), 16);
    assert_eq!(cache_line_bytes(GswMode::Gsw386, 2), CACHE_LINE_BYTES);
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        assert_eq!(cache_line_bytes(mode, 1), CACHE_LINE_BYTES, "{mode:?}");
    }
}
