//! Integration coverage for STREAM C3: the Izarra 3000 BIOS Margo (VEGA 2D)
//! probe. Boots the real izarra-bios image on a gsw_386 machine, runs POST, and
//! parses the live VDTS result block. Asserts the probe's two records by name:
//! component.video_margo PASSes and video.margo_caps reports the low 16 bits of
//! Margo's capability register (0x0fff) as four hex digits.
//!
//! result_append rewrites the VDTS header and checksum on every record, so the
//! block parses while POST is still running. The probe appends both records well
//! inside the cycle budget, before the long RAM scan reaches its idle halt, so
//! this asserts on the records rather than on POST terminating.

use izarravm_core::VideoCard;
use izarravm_firmware::{SuiteRecord, SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile};

/// Boot the BIOS, run POST for the standard budget, and return its records.
fn post_records() -> Vec<SuiteRecord> {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .expect("the izarra-bios image builds a gsw_386 machine");

    machine
        .run_until_halt_or_cycles(5_000_000)
        .expect("POST runs without a CPU fault");

    parse_result_block(machine.memory().as_slice())
        .expect("the VDTS result block parses")
        .records
}

#[test]
fn margo_probe_passes_on_a_present_card() {
    let records = post_records();
    let record = records
        .iter()
        .find(|record| record.name == "component.video_margo")
        .expect("the Margo probe emits a component.video_margo record");
    assert_eq!(
        record.status,
        SuiteRecordStatus::Pass,
        "Margo's identity register matches, so the probe PASSes"
    );
}

#[test]
fn margo_caps_measure_reports_the_capability_bits() {
    let records = post_records();
    let record = records
        .iter()
        .find(|record| record.name == "video.margo_caps")
        .expect("the Margo probe emits a video.margo_caps record");
    assert_eq!(record.status, SuiteRecordStatus::Measure);
    // Margo reports all twelve feature bits set; the probe formats the low 16
    // bits of the capability register as four lowercase hex digits.
    assert_eq!(record.value.as_deref(), Some("0fff"));
}
