// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::cache_config::cache_line_bytes;

#[test]
fn cache_level_config_matches_geometry() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        // Both epochs: slice 2 gives the 486 a 16-byte line and the 586 a 32-byte
        // one, so the mask must follow the epoch's line size, not a constant.
        for epoch in [1, 2] {
            let g = cache_geometry(mode);
            let config = cache_level_config(mode, epoch);
            let line = cache_line_bytes(mode, epoch);
            assert_eq!(1u32 << config.line_shift, line, "{mode:?} epoch {epoch}");
            let l1_lines = g.l1_bytes / line;
            let l2_lines = g.l2_bytes / line;

            assert_eq!(
                config.l1_mask,
                if l1_lines == 0 {
                    CACHE_TIER_DISABLED_MASK
                } else {
                    l1_lines - 1
                }
            );
            assert_eq!(
                config.l2_mask,
                if l2_lines == 0 {
                    CACHE_TIER_DISABLED_MASK
                } else {
                    l2_lines - 1
                }
            );
        }
    }
}

#[test]
fn cache_model_resolves_tiers_by_working_set() {
    let mut c = CacheModel::new(GswMode::Gsw486, 1);
    let warm = |c: &mut CacheModel, base: u32, len: u32| {
        for off in (0..len).step_by(64) {
            c.data_tier(GswMode::Gsw486, base + off);
        }
    };
    warm(&mut c, 0x10_0000, 4 * 1024); // 4K fits 486 L1 (8K)
    assert_eq!(c.data_tier(GswMode::Gsw486, 0x10_0000), Tier::L1);
    warm(&mut c, 0x20_0000, 64 * 1024); // 64K exceeds L1, fits L2 (256K)
    assert_eq!(c.data_tier(GswMode::Gsw486, 0x20_0000), Tier::L2);
    warm(&mut c, 0x40_0000, 512 * 1024); // 512K exceeds 486 L2 -> RAM
    assert_eq!(c.data_tier(GswMode::Gsw486, 0x40_0000), Tier::Ram);
}

#[test]
fn cache_model_reset_goes_cold() {
    let mut c = CacheModel::new(GswMode::Gsw586, 1);
    c.data_tier(GswMode::Gsw586, 0x30_0000); // installs the line
    assert_eq!(c.data_tier(GswMode::Gsw586, 0x30_0000), Tier::L1); // hot
    c.reset();
    assert_ne!(c.data_tier(GswMode::Gsw586, 0x30_0000), Tier::L1); // cold again
}

#[test]
fn measure_read_bandwidth_returns_a_finite_sample_in_every_mode() {
    // Small block (fits every mode's L1) and a large block (exceeds every L2).
    // Both must move bytes and take clocks; we do NOT assert tier ordering here
    // -- the tier costs are calibrated and a separate ordering test covers the
    // descending L1/L2/RAM curve.
    const TOTAL: u64 = 4 * 1024 * 1024;
    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];
    for mode in modes {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(24, VideoCard::Vega),
            izarravm_firmware::neurketa_image(),
        )
        .expect("boot image");
        machine.set_mode(mode);
        for block in [4 * 1024u32, 1024 * 1024] {
            let sample = machine.measure_read_bandwidth(0x10_0000, block, TOTAL);
            assert!(sample.bytes > 0, "{mode:?} block {block}: zero bytes");
            assert!(sample.clocks > 0, "{mode:?} block {block}: zero clocks");
            // bytes == block_bytes * passes, with passes = max(2, TOTAL/block).
            let passes = (TOTAL / u64::from(block)).max(2);
            assert_eq!(
                sample.bytes,
                u64::from(block) * passes,
                "{mode:?} block {block}: bytes != block * passes"
            );
        }
    }
}

#[test]
fn measure_read_bandwidth_curve_descends_per_tier() {
    // The end-to-end proof that "the three speed levels work": drive the bus over
    // block sizes chosen to land well inside each tier FOR THAT MODE (not at a
    // boundary, so the separation is clean), compute MB/s, and assert the curve
    // descends L1 > L2 > RAM. A fixed TOTAL budget amortizes the cold first pass.
    const TOTAL: u64 = 16 * 1024 * 1024;
    const BASE: u32 = 0x10_0000;

    // MB/s from a sample, mirroring the --headless-bandwidth derivation.
    fn mbps(machine: &mut Machine, mode: GswMode, block: u32) -> f64 {
        let sample = machine.measure_read_bandwidth(BASE, block, TOTAL);
        assert!(sample.clocks > 0, "{mode:?} block {block}: zero clocks");
        sample.bytes as f64 / (sample.clocks as f64 / mode.clock_hz() as f64) / 1.0e6
    }

    // A fresh machine per (mode, block) so each measurement starts cold and
    // nothing carries over, exactly like the bandwidth tool does.
    fn measure(mode: GswMode, block: u32) -> f64 {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(24, VideoCard::Vega),
            izarravm_firmware::neurketa_image(),
        )
        .expect("boot image");
        machine.set_mode(mode);
        mbps(&mut machine, mode, block)
    }

    // 586: L1 32K, L2 512K. 16K is deep in L1, 256K deep in L2, 2M is RAM.
    {
        let l1 = measure(GswMode::Gsw586, 16 * 1024);
        let l2 = measure(GswMode::Gsw586, 256 * 1024);
        let ram = measure(GswMode::Gsw586, 2 * 1024 * 1024);
        assert!(
            l1 > l2 * 1.05,
            "586: L1 {l1:.1} must exceed L2 {l2:.1} MB/s"
        );
        assert!(
            l2 > ram * 1.05,
            "586: L2 {l2:.1} must exceed RAM {ram:.1} MB/s"
        );
    }

    // 486: L1 8K, L2 256K. 4K is deep in L1, 64K deep in L2, 512K is RAM.
    {
        let l1 = measure(GswMode::Gsw486, 4 * 1024);
        let l2 = measure(GswMode::Gsw486, 64 * 1024);
        let ram = measure(GswMode::Gsw486, 512 * 1024);
        assert!(
            l1 > l2 * 1.05,
            "486: L1 {l1:.1} must exceed L2 {l2:.1} MB/s"
        );
        assert!(
            l2 > ram * 1.05,
            "486: L2 {l2:.1} must exceed RAM {ram:.1} MB/s"
        );
    }

    // 386: L2 64K, no L1. 32K is deep in L2, 1M is well into RAM. The 386 L2-vs-RAM
    // step is the narrowest, so pick a small L2 block and a large RAM block to
    // separate them cleanly and assert a >5% margin.
    for mode in [GswMode::Gsw386, GswMode::Gsw386Slow] {
        let l2 = measure(mode, 32 * 1024);
        let ram = measure(mode, 1024 * 1024);
        assert!(
            l2 > ram * 1.05,
            "{mode:?}: L2 {l2:.1} must exceed RAM {ram:.1} MB/s"
        );
    }
}

fn scaled_bus_loop_machine(mode: GswMode) -> Machine {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xeb, 0xfe])
            .expect("raw loop machine");
    machine.set_mode(mode);
    machine
}

fn expected_scaled_bus_delta(raw: u64, remainder: u64, mode: GswMode) -> (u64, u64) {
    let (num, den) = bus_timing(mode.persona(), 1);
    let numerator = u128::from(raw) * u128::from(num) + u128::from(remainder);
    (
        u64::try_from(numerator / u128::from(den)).unwrap(),
        u64::try_from(numerator % u128::from(den)).unwrap(),
    )
}

#[test]
fn committed_scaled_bus_total_is_split_invariant() {
    let mode = GswMode::Gsw586;
    let mut machine = scaled_bus_loop_machine(mode);
    let raw_before = machine.raw_bus_clocks();
    let scaled_before = machine.scaled_bus_clocks();
    let remainder_before = machine.bus_rem;

    let _ = machine.run_until_halt_or_cycles(20_000).unwrap();
    let _ = machine.run_until_halt_or_cycles(30_000).unwrap();

    let raw_delta = machine.raw_bus_clocks() - raw_before;
    let (expected_delta, expected_remainder) =
        expected_scaled_bus_delta(raw_delta, remainder_before, mode);
    assert!(raw_delta > 0);
    assert_eq!(machine.scaled_bus_clocks() - scaled_before, expected_delta);
    assert_eq!(machine.bus_rem, expected_remainder);
}

#[test]
fn scaled_bus_mode_switch_preserves_total_and_discards_only_old_carry() {
    let mut machine = scaled_bus_loop_machine(GswMode::Gsw586);
    let _ = machine.run_until_halt_or_cycles(20_000).unwrap();
    let total_before_switch = machine.scaled_bus_clocks();
    assert!(total_before_switch > 0);
    machine.bus_rem = 29;

    machine.set_mode(GswMode::Gsw386);
    assert_eq!(machine.scaled_bus_clocks(), total_before_switch);
    assert_eq!(machine.bus_rem, 0);

    let raw_before = machine.raw_bus_clocks();
    let _ = machine.run_until_halt_or_cycles(20_000).unwrap();
    let raw_delta = machine.raw_bus_clocks() - raw_before;
    let (expected_delta, expected_remainder) =
        expected_scaled_bus_delta(raw_delta, 0, GswMode::Gsw386);
    assert_eq!(
        machine.scaled_bus_clocks() - total_before_switch,
        expected_delta
    );
    assert_eq!(machine.bus_rem, expected_remainder);
}

#[test]
fn bandwidth_probe_does_not_commit_scaled_bus_clocks() {
    let mut machine = scaled_bus_loop_machine(GswMode::Gsw586);
    let _ = machine.run_until_halt_or_cycles(20_000).unwrap();
    let scaled_before = machine.scaled_bus_clocks();
    let raw_before = machine.raw_bus_clocks();
    machine.bus_rem = 29;

    let sample = machine.measure_read_bandwidth(0x10_0000, 4 * 1024, 64 * 1024);

    assert!(sample.clocks > 0);
    assert!(machine.raw_bus_clocks() > raw_before);
    assert_eq!(machine.scaled_bus_clocks(), scaled_before);
    let raw_delta = machine.raw_bus_clocks() - raw_before;
    let (_, expected_remainder) = expected_scaled_bus_delta(raw_delta, 0, GswMode::Gsw586);
    assert_eq!(machine.bus_rem, expected_remainder);
}

#[test]
fn device_and_halted_advances_do_not_commit_scaled_bus_clocks() {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xf4])
            .expect("raw hlt machine");
    machine.set_mode(GswMode::Gsw586);
    let raw_before = machine.raw_bus_clocks();
    let remainder_before = machine.bus_rem;
    assert_eq!(
        machine.run_until_halt_or_cycles(20_000).unwrap(),
        StopReason::Halted
    );
    let raw_delta = machine.raw_bus_clocks() - raw_before;
    let (expected_delta, expected_remainder) =
        expected_scaled_bus_delta(raw_delta, remainder_before, GswMode::Gsw586);
    let after_hlt = machine.scaled_bus_clocks();
    assert!(raw_delta > 0);
    assert_eq!(after_hlt, expected_delta);
    assert_eq!(machine.bus_rem, expected_remainder);

    machine.advance_devices_ticks(1_000);
    machine.advance_devices_clocks(1_000);
    machine.advance_halted_ticks(1_000);

    assert_eq!(machine.scaled_bus_clocks(), after_hlt);
}

#[test]
fn committed_scaled_bus_total_saturates() {
    let mut machine = scaled_bus_loop_machine(GswMode::Gsw586);
    machine.scaled_bus_clocks = u64::MAX;

    let _ = machine.run_until_halt_or_cycles(20_000).unwrap();

    assert_eq!(machine.scaled_bus_clocks(), u64::MAX);
}

#[test]
fn approximate_class_bypasses_cache_tiering_accurate_class_does_not() {
    use izarravm_core::GswMode;
    let mut machine = test_machine();

    // Accurate class (386): a conventional-RAM data access warms the tier model.
    // (read_physical_u16 routes through read_memory -> data_access_wait_states;
    // read_physical_u8 takes a raw read_phys_u8 path that never tiers.)
    machine.set_mode(GswMode::Gsw386);
    let before = machine.cache_tier_lookups();
    let _ = machine.read_physical_u16(0x2_0000);
    assert!(
        machine.cache_tier_lookups() > before,
        "386 (Accurate) must tier the access (lookups increment)"
    );

    // Approximate class (586): the same access charges the flat cost, no tiering.
    machine.set_mode(GswMode::Gsw586);
    let before = machine.cache_tier_lookups();
    let _ = machine.read_physical_u16(0x2_0000);
    assert_eq!(
        machine.cache_tier_lookups(),
        before,
        "586 (Approximate) must bypass tiering (lookups unchanged)"
    );
}

#[test]
fn cache_geometry_matches_cache_kb() {
    // The machine geometry must agree with the CPU's cache_kb readout (KB).
    for (mode, (l1_kib, external_kib)) in [
        (GswMode::Gsw386Slow, (0u16, 64u16)),
        (GswMode::Gsw386, (0, 64)),
        (GswMode::Gsw486, (8, 256)),
        (GswMode::Gsw586, (32, 512)),
    ] {
        let g = cache_geometry(mode);
        assert_eq!(g.l1_bytes / 1024, u32::from(l1_kib), "{mode:?} L1");
        assert_eq!(
            g.l2_bytes / 1024,
            u32::from(external_kib),
            "{mode:?} external cache"
        );
        assert_eq!(mode.cache_kb(), (l1_kib, external_kib));
    }
}

#[test]
fn slow_post_paces_without_null_vector_runaway() {
    // Under slow POST the BIOS drives PIT channel 0 to pace the chime and the
    // RAM count-up. Those OUT edges raise IRQ0 with IF set; before INT 08h was
    // installed the timer vectored through the zeroed IVT[08h] (CS=0000) and ran
    // away through low memory. Run a slice that covers the chime and the start of
    // the count-up, then confirm the CPU never left the BIOS region and the INT
    // 08h handler advanced the BDA tick count.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.set_fast_post(false);
    let mut max_ticks = 0u32;
    for _ in 0..400 {
        let _ = machine.run_until_halt_or_cycles(50_000).unwrap();
        let cs = machine.cpu().registers.cs().selector;
        assert_ne!(cs, 0, "CPU vectored to CS=0000 (null IVT runaway)");
        let lo = u32::from(machine.read_physical_u8(0x46c));
        let hi = u32::from(machine.read_physical_u8(0x46d));
        max_ticks = max_ticks.max(lo | (hi << 8));
    }
    assert!(
        max_ticks > 3,
        "INT 08h did not advance the BDA tick (got {max_ticks})"
    );
}

#[test]
fn bios_seeds_low_exception_and_irq_vectors() {
    let mut machine = int15_machine(16);

    for vector in 0x00u32..=0x07 {
        let base = vector * 4;
        assert_eq!(
            read_u16(&mut machine, base),
            bios_int_stub_off(vector as u8)
        );
        assert_eq!(read_u16(&mut machine, base + 2), BIOS_ROM_IRET_SEG);
    }
    assert_eq!(read_u16(&mut machine, 0x08 * 4), BIOS_TIMER_ISR_ROM_OFF);
    assert_eq!(read_u16(&mut machine, 0x08 * 4 + 2), BIOS_ROM_IRET_SEG);
    for vector in [0x0Au32, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F] {
        let base = vector * 4;
        assert_eq!(read_u16(&mut machine, base), BIOS_MASTER_IRQ_ISR_ROM_OFF);
        assert_eq!(read_u16(&mut machine, base + 2), BIOS_ROM_IRET_SEG);
    }
    for vector in [0x71u32, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77] {
        let base = vector * 4;
        assert_eq!(
            read_u16(&mut machine, base),
            BIOS_SLAVE_IRQ_ISR_ADDRESS as u16
        );
        assert_eq!(read_u16(&mut machine, base + 2), 0);
    }
}

#[test]
fn default_int08_ticks_and_returns_from_irq0() {
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, // ICW1 master
        0xb0, 0x08, 0xe6, 0x21, // ICW2 base 08h
        0xb0, 0x04, 0xe6, 0x21, // ICW3 slave on IR2
        0xb0, 0x01, 0xe6, 0x21, // ICW4 8086 mode
        0xb0, 0xfe, 0xe6, 0x21, // unmask IRQ0 only
        0xb0, 0x36, 0xe6, 0x43, // PIT channel 0 mode 3
        0xb0, 0xe8, 0xe6, 0x40, // count low 1000
        0xb0, 0x03, 0xe6, 0x40, // count high
        0xfb, 0xf4, // sti; hlt
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(read_u16(&mut machine, 0x46c), 1);
}

#[test]
fn sti_hlt_idle_loop_still_takes_irq0_per_batch() {
    // PIC + PIT init as in default_int08_ticks..., then `sti; hlt; jmp $-2`:
    // STI enables interrupts (one-instruction shadow), HLT parks until IRQ0,
    // the default INT 08h handler runs and bumps the BDA tick, IRET returns to
    // the JMP which loops back to STI. The tick at 0x46c must keep advancing.
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, // ICW1 master
        0xb0, 0x08, 0xe6, 0x21, // ICW2 base 08h
        0xb0, 0x04, 0xe6, 0x21, // ICW3 slave on IR2
        0xb0, 0x01, 0xe6, 0x21, // ICW4 8086 mode
        0xb0, 0xfe, 0xe6, 0x21, // unmask IRQ0 only
        0xb0, 0x36, 0xe6, 0x43, // PIT channel 0 mode 3
        0xb0, 0xe8, 0xe6, 0x40, // count low 1000
        0xb0, 0x03, 0xe6, 0x40, // count high
        0xfb, // sti
        0xf4, // hlt
        0xeb, 0xfc, // jmp $-2 (back to the sti)
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();

    // The loop never genuinely halts, so it exhausts the budget; the assertion
    // is that the timer ISR ran while it spun, proving the per-batch check still
    // takes the interrupt for an STI; HLT idle loop.
    let _ = machine.run_until_halt_or_cycles(2_000_000).unwrap();

    assert!(
        read_u16(&mut machine, 0x46c) >= 1,
        "the IRQ0 handler must run for an STI; HLT idle loop under per-batch \
             interrupt checking (it would spin forever if the STI shadow were \
             consumed at batch entry instead of per instruction)"
    );
}

#[test]
fn sti_busy_loop_takes_irq0_despite_intervening_instructions() {
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, // ICW1 master
        0xb0, 0x08, 0xe6, 0x21, // ICW2 base 08h
        0xb0, 0x04, 0xe6, 0x21, // ICW3 slave on IR2
        0xb0, 0x01, 0xe6, 0x21, // ICW4 8086 mode
        0xb0, 0xfe, 0xe6, 0x21, // unmask IRQ0 only
        0xb0, 0x36, 0xe6, 0x43, // PIT channel 0 mode 3
        0xb0, 0xe8, 0xe6, 0x40, // count low 1000
        0xb0, 0x03, 0xe6, 0x40, // count high
        0xfb, // sti
        0x90, 0x90, 0x90, 0x90, 0x90, // nop x5
        0xeb, 0xf9, // jmp $-7 (back to the sti)
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();

    let _ = machine.run_until_halt_or_cycles(2_000_000).unwrap();

    assert!(
        read_u16(&mut machine, 0x46c) >= 1,
        "the IRQ0 handler must run for a busy STI loop even with NOPs between \
             interrupt checks"
    );
}

#[test]
fn halt_fast_forward_survives_a_batch_that_overshot_the_run_deadline() {
    // A batch is granted a budget derived from the ticks LEFT to the caller's
    // deadline, but it can spend more than it was granted: `run_budgeted`
    // always retires at least one instruction, the batch-entry interrupt
    // service is charged before any cap test, and the ISA I/O charge is added
    // to the batch-end step without ever having been counted against the cap.
    // So `now_ticks()` can sit PAST `deadline_ticks` when the batch ends -- and
    // when that same batch ended on a HLT with IF clear, `next_timer_wake`
    // returns None and the halt fast-forward computes the ticks it may still
    // consume. That subtraction used to be plain and panicked in debug
    // ("attempt to subtract with overflow"); in release it wrapped to ~u64::MAX
    // and let the fast-forward run all the way to the next timed-I/O edge,
    // ignoring the caller's deadline entirely.
    //
    // `cli; hlt` in a raw program is the smallest guest that reaches that
    // branch. The burst sweep is what makes it deterministic: some burst size
    // in this range always ends its final batch on the HLT with the deadline
    // already a clock or two behind.
    //
    // "Did not panic" is all this test can assert directly, and under the fix
    // that would pass even if nothing overshot -- so count the clamps that
    // actually fired and require at least one. Without that, a future change to
    // the batch grant or cap arithmetic could stop reaching the branch and
    // hollow this test out silently. Bursts 1..=7 all overshoot today, so the
    // assert has margin.
    let mut clamps = 0u64;
    for burst in 1u64..=64 {
        let mut machine =
            Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xfa, 0xf4])
                .expect("raw cli; hlt machine");
        for _ in 0..8 {
            let _ = machine
                .run_until_halt_or_cycles(burst)
                .unwrap_or_else(|e| panic!("burst {burst}: {e}"));
        }
        clamps += machine.test_halt_deadline_clamps;
    }
    assert!(
        clamps > 0,
        "no burst ended a HLT batch past the deadline, so this test no longer \
         exercises the halt fast-forward's remaining-ticks clamp at all"
    );
}

#[test]
fn machine_accepts_256k_flash_and_shadows_top_64k() {
    // A 256 KiB flash whose top 64 KiB carries a recognizable reset far-jump
    // boots: the machine maps the top 64 KiB at 0xF0000, so the reset vector at
    // 0xFFFF0 reads the far jump.
    let mut flash = vec![0u8; 256 * 1024];
    let top = flash.len() - BIOS_ROM_SIZE;
    flash[top + 0xfff0..top + 0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), flash).unwrap();
    assert_eq!(machine.read_physical_u8(0xffff0), 0xea);
    assert_eq!(machine.read_physical_u8(0xffff4), 0xf0);
}

#[test]
fn izarra_bios_boots_into_margo_lfb_screen() {
    // POST sets the proprietary 320x240x8 Margo mode and draws its screen
    // there. Fast POST (default) skips delays so the screen is up within the
    // cycle budget.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    // The full-screen RLE background blit writes 76800 LFB bytes through the
    // unreal FS, so the screen comes up at ~10M cycles (still ~40 ms at the
    // machine clock); give the boot loop headroom past that.
    for _ in 0..40 {
        let _ = machine.run_until_halt_or_cycles(500_000).unwrap();
        if machine.active_display() == ActiveDisplay::MargoLfb {
            break;
        }
    }
    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "POST never set the Margo LFB mode"
    );
    let (words, w, h) = machine.frame_argb();
    assert_eq!((w, h), (320, 240), "proprietary mode is 320x240");
    assert!(
        words.iter().any(|&p| p != words[0]),
        "screen is a single flat color - nothing was drawn"
    );
}

#[test]
fn izarra_bios_lfb_carries_rle_background() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    for _ in 0..40 {
        let _ = machine.run_until_halt_or_cycles(500_000).unwrap();
        if machine.active_display() == ActiveDisplay::MargoLfb {
            break;
        }
    }
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    // A top-left pixel is the cream field; the icon-strip row (y 175) carries
    // the baked-in grey icons, so it is not a single flat colour.
    let field = machine.read_physical_u8(MARGO_LFB_BASE + 4 * 320 + 4);
    let mut varied = false;
    for x in (0..320u32).step_by(7) {
        if machine.read_physical_u8(MARGO_LFB_BASE + 175 * 320 + x) != field {
            varied = true;
            break;
        }
    }
    assert!(
        varied,
        "icon-strip row is flat — RLE background did not blit"
    );
}

#[test]
fn word_out_to_a_vga_register_pair_splits_into_two_byte_cycles() {
    // `OUT DX, AX` (16-bit) to a VGA index/data port pair is the canonical
    // mode-set idiom: the low byte (AL) selects the index at the port, the high
    // byte (AH) writes the data at port+1. The byte-only I/O bus used to reject
    // any non-byte width with WidthMismatch, halting the VM on real VGA setup
    // code (HOUSERS / TSUMERA SETUP.EXE both crash on exactly this).
    let mut m = int15_machine(1);
    {
        let mut bus = m.make_bus();
        // AX = 0x420F: CRTC index 0x0F (cursor location low), data 0x42.
        bus.write_io(0x3D4, BusWidth::Word, 0x420F, false).unwrap();
    }
    // The low byte set the index, the high byte wrote the data at 0x3D5.
    assert_eq!(color_crtc_reg(&mut m, 0x0F), 0x42);
}
