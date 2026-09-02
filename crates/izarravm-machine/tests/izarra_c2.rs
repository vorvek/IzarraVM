// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

// Integration test for the Izarra3000 BIOS audio probes.
//
// Boots the real izarra-bios ROM on a 386-class machine, runs POST to the idle
// halt, and parses the VDTS result block the BIOS built in low memory. The audio
// probes (probe-sbdsp.inc, probe-opl.inc) drive the emulator's Sound Blaster DSP
// and OPL models through plain port I/O, so a passing record proves the probe
// observed the device's real behavior: the DSP's 0xAA reset acknowledge and
// version, and the OPL's rest-state status. Records are matched by name and are
// order-independent; other streams add their own.

use izarravm_core::VideoCard;
use izarravm_firmware::{SuiteRecord, SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile, StopReason};

mod support;
use support::mount_idle_boot_floppy;

fn run_post() -> Vec<SuiteRecord> {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarra_bios()).expect("build machine with izarra BIOS");
    mount_idle_boot_floppy(&mut machine);
    let stop = machine
        .run_until_halt_or_cycles(20_000_000)
        .expect("run POST");
    // POST runs the whole step table and then the BIOS idles (it keeps running
    // rather than halting), so the budget is reached with the result block final.
    assert!(
        matches!(stop, StopReason::CycleLimit { .. }),
        "POST completes and the BIOS idles"
    );
    let results =
        parse_result_block(machine.memory().as_slice()).expect("parse the VDTS result block");
    results.records
}

fn record<'a>(records: &'a [SuiteRecord], name: &str) -> &'a SuiteRecord {
    records
        .iter()
        .find(|record| record.name == name)
        .unwrap_or_else(|| panic!("no record named {name}"))
}

#[test]
fn given_post_completes_when_the_result_block_is_parsed_then_the_audio_probes_pass() {
    // Given: Izarra BIOS with an idle boot floppy, one POST.
    // When: the audio probes (DSP reset / version, OPL rest-state) run.
    // Then: DSP and OPL PASS, the version MEASURE is SB16 4.5, and each name
    // appears once (neither probe clobbered the shared PROBE_SCRATCH buffer).
    let records = run_post();
    let dsp = record(&records, "component.audio_sbdsp");
    assert_eq!(
        dsp.status,
        SuiteRecordStatus::Pass,
        "DSP reset handshake should see the 0xAA acknowledge"
    );
    let version = record(&records, "sound.dsp_version");
    assert_eq!(version.status, SuiteRecordStatus::Measure);
    assert_eq!(
        version.value.as_deref(),
        Some("4.5"),
        "command 0xE1 returns the SB16 4.5 version"
    );
    let opl = record(&records, "component.audio_opl");
    assert_eq!(
        opl.status,
        SuiteRecordStatus::Pass,
        "OPL status should read the rest-state signature after a flag reset"
    );
    for name in [
        "component.audio_sbdsp",
        "sound.dsp_version",
        "component.audio_opl",
    ] {
        let count = records.iter().filter(|r| r.name == name).count();
        assert_eq!(count, 1, "expected exactly one {name} record, got {count}");
    }
}
