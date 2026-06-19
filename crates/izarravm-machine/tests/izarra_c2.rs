// Stream C2 integration test: the Izarra 3000 BIOS audio probes.
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

fn run_post() -> Vec<SuiteRecord> {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarra_bios()).expect("build machine with izarra BIOS");
    let stop = machine
        .run_until_halt_or_cycles(5_000_000)
        .expect("run POST");
    // POST ends at the BIOS idle halt with no wake source armed, so a clean
    // Halted stop means the whole step table ran and the result block is final.
    assert_eq!(stop, StopReason::Halted, "POST should run to the idle halt");
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
fn dsp_reset_probe_passes() {
    let records = run_post();
    let dsp = record(&records, "component.audio_sbdsp");
    assert_eq!(
        dsp.status,
        SuiteRecordStatus::Pass,
        "DSP reset handshake should see the 0xAA acknowledge"
    );
}

#[test]
fn dsp_version_measure_reports_sb16_4_5() {
    let records = run_post();
    let version = record(&records, "sound.dsp_version");
    assert_eq!(version.status, SuiteRecordStatus::Measure);
    // The CT1747 model reports DSP version 4.5 (DSP_VERSION_HI/LO), the SB16
    // identity games auto-detect; the probe formats it major.minor.
    assert_eq!(
        version.value.as_deref(),
        Some("4.5"),
        "command 0xE1 returns the SB16 4.5 version"
    );
}

#[test]
fn opl_probe_passes() {
    let records = run_post();
    let opl = record(&records, "component.audio_opl");
    assert_eq!(
        opl.status,
        SuiteRecordStatus::Pass,
        "OPL status should read the rest-state signature after a flag reset"
    );
}

#[test]
fn audio_records_are_present_and_distinct() {
    // All three audio records exist exactly once, so neither probe clobbered the
    // other's name and the shared PROBE_SCRATCH version buffer did not corrupt a
    // sibling record.
    let records = run_post();
    for name in [
        "component.audio_sbdsp",
        "sound.dsp_version",
        "component.audio_opl",
    ] {
        let count = records.iter().filter(|r| r.name == name).count();
        assert_eq!(count, 1, "expected exactly one {name} record, got {count}");
    }
}
