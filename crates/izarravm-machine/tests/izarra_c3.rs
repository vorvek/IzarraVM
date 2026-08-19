// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Integration coverage for the four component probes that
//! complete the graphical POST's icon sweep (GSW CPU, floppy controller, ATA
//! hard disk, ATAPI optical). They do real port reads, so an absent device is
//! reported as FAIL (its icon stays grey). This guards the faithful behaviour:
//! the HDD probe FAILs on a bare machine because C: is HLE-backed rather than a
//! real ATA disk, while CPU/floppy/optical PASS (those controllers are present).

use izarravm_core::{GswMode, MASTER_CLOCK_HZ, VideoCard};
use izarravm_firmware::{SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile, StopReason};

/// Boot a bare machine (no ATA disk mounted), run POST, return the result block.
fn run_post_bare(mode: GswMode) -> izarravm_firmware::SuiteResults {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine =
        Machine::new(profile, izarra_bios()).expect("the Izarra BIOS image builds a machine");
    // Keep the original 386 instruction room while scaling the POST pacing wait
    // to the selected clock.
    let cycles = 20_000_000u64
        .saturating_mul(mode.clock_hz())
        .div_ceil(GswMode::Gsw386.clock_hz())
        .max(20_000_000);
    let stop = machine
        .run_until_halt_or_cycles(cycles)
        .expect("the BIOS runs without a machine error");
    assert!(
        !matches!(stop, StopReason::CpuError(_)),
        "BIOS faulted in {mode} mode: {stop:?}"
    );
    parse_result_block(machine.memory().as_slice()).expect("POST writes a valid VDTS result block")
}

fn status(results: &izarravm_firmware::SuiteResults, name: &str) -> SuiteRecordStatus {
    results
        .records
        .iter()
        .find(|record| record.name == name)
        .unwrap_or_else(|| panic!("expected a record named {name}"))
        .status
}

fn full_post_timing(mode: GswMode) -> (std::time::Duration, u64, u64) {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine = Machine::new(profile, izarra_bios()).unwrap();
    machine.set_fast_post(false);
    machine.enable_phase_marks();

    let started = std::time::Instant::now();
    for _ in 0..15_000 {
        machine.run_master_ticks(MASTER_CLOCK_HZ / 1_000).unwrap();
        if let Some(mark) = machine
            .phase_marks()
            .iter()
            .find(|mark| mark.id == izarravm_machine::phase_mark::POST_END)
        {
            return (
                mark.wall.duration_since(started),
                mark.master_ticks,
                mark.perf.instructions,
            );
        }
    }
    panic!("full POST did not finish in {mode} mode");
}

#[test]
fn full_post_work_does_not_scale_with_cpu_frequency() {
    let samples = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ]
    .map(|mode| (mode, full_post_timing(mode)));
    for (mode, (wall, ticks, instructions)) in samples {
        eprintln!(
            "{mode}: wall={:.3}s guest={:.3}s instructions={instructions}",
            wall.as_secs_f64(),
            ticks as f64 / MASTER_CLOCK_HZ as f64
        );
        assert!(
            ticks >= MASTER_CLOCK_HZ,
            "full POST must retain its visible guest-time cadence in {mode} mode"
        );
    }

    for pair in samples.windows(2) {
        let (slower_mode, (_, slower_ticks, slower_instructions)) = pair[0];
        let (faster_mode, (_, faster_ticks, faster_instructions)) = pair[1];
        assert!(
            faster_ticks <= slower_ticks,
            "full POST got longer from {slower_mode} ({:.3}s) to {faster_mode} ({:.3}s)",
            slower_ticks as f64 / MASTER_CLOCK_HZ as f64,
            faster_ticks as f64 / MASTER_CLOCK_HZ as f64
        );
        // The band, not a pinned count: POST work must not track the CPU clock.
        // Measured spread across the four modes is 4.021M/4.021M/4.021M/4.059M,
        // so the worst adjacent ratio is 1.010; 1.25 leaves room for a probe or a
        // POST step to grow without a re-pin. The regression this catches is the
        // PIT busy poll, whose 386 -> 586 ratio was about 22x.
        assert!(
            faster_instructions <= slower_instructions.saturating_mul(5) / 4,
            "full POST work grew from {slower_mode} ({slower_instructions} instructions) to \
             {faster_mode} ({faster_instructions} instructions)"
        );
    }
}

#[test]
fn cpu_gsw_probe_passes_on_all_four_modes() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let results = run_post_bare(mode);
        let actual = results
            .records
            .iter()
            .find(|record| record.name == "component.cpu_gsw")
            .map(|record| record.status);
        assert_eq!(
            actual,
            Some(SuiteRecordStatus::Pass),
            "CPU probe failed in {mode} mode; records: {:?}",
            results.records
        );
    }
}

#[test]
fn floppy_fdc_probe_passes_on_the_present_controller() {
    // The floppy controller is always present (RQM ready at idle), so its icon lights.
    assert_eq!(
        status(&run_post_bare(GswMode::Gsw386), "component.floppy_fdc"),
        SuiteRecordStatus::Pass
    );
}

#[test]
fn optical_atapi_probe_passes_on_the_signature() {
    // The secondary channel presents an ATAPI CD-ROM drive (signature 0x14/0xEB)
    // from power-on, media optional, so the optical icon lights.
    assert_eq!(
        status(&run_post_bare(GswMode::Gsw386), "component.optical_atapi"),
        SuiteRecordStatus::Pass
    );
}

#[test]
fn hdd_probe_fails_when_no_real_ata_disk_is_present() {
    // Faithful behaviour: with no ATA image mounted (C: is HLE-backed), the primary
    // channel reads open-bus, so the HDD probe FAILs and the icon stays grey. It
    // lights only when a real ATA disk is attached (see the aggregate test).
    assert_eq!(
        status(&run_post_bare(GswMode::Gsw386), "component.disk_hdd"),
        SuiteRecordStatus::Fail
    );
}

#[test]
fn hdd_probe_passes_with_a_mounted_ata_disk() {
    let mut machine =
        Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), izarra_bios()).unwrap();
    machine.mount_hdd(vec![0u8; 64 * 512]);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    let results = parse_result_block(machine.memory().as_slice()).expect("valid VDTS result block");
    assert_eq!(
        status(&results, "component.disk_hdd"),
        SuiteRecordStatus::Pass
    );
}
