//! Integration coverage for Stream C3: the four Phase-3 component probes that
//! complete the graphical POST's icon sweep (GSW-586 CPU, floppy controller, ATA
//! hard disk, ATAPI optical). They do real port reads, so an absent device is
//! reported as FAIL (its icon stays grey) -- this guards the faithful behaviour:
//! the HDD probe FAILs on a bare machine because C: is HLE-backed rather than a
//! real ATA disk, while CPU/floppy/optical PASS (those controllers are present).

use izarravm_core::VideoCard;
use izarravm_firmware::{SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile};

/// Boot a bare machine (no ATA disk mounted), run POST, return the result block.
fn run_post_bare() -> izarravm_firmware::SuiteResults {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .expect("the Izarra BIOS image builds a machine");
    // POST runs at reset; the full-screen RLE background delays the step loop to
    // ~10M cycles, so give it room to append every record.
    machine
        .run_until_halt_or_cycles(20_000_000)
        .expect("the BIOS runs without a machine error");
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

#[test]
fn cpu_gsw_probe_passes_via_cpuid() {
    // The GSW-586 always answers CPUID with its vendor identity, regardless of the
    // live throttle mode, so the CPU icon lights on every machine.
    assert_eq!(
        status(&run_post_bare(), "component.cpu_gsw"),
        SuiteRecordStatus::Pass
    );
}

#[test]
fn floppy_fdc_probe_passes_on_the_present_controller() {
    // The floppy controller is always present (RQM ready at idle), so its icon lights.
    assert_eq!(
        status(&run_post_bare(), "component.floppy_fdc"),
        SuiteRecordStatus::Pass
    );
}

#[test]
fn optical_atapi_probe_passes_on_the_signature() {
    // The secondary channel presents an ATAPI CD-ROM drive (signature 0x14/0xEB)
    // from power-on, media optional, so the optical icon lights.
    assert_eq!(
        status(&run_post_bare(), "component.optical_atapi"),
        SuiteRecordStatus::Pass
    );
}

#[test]
fn hdd_probe_fails_when_no_real_ata_disk_is_present() {
    // Faithful behaviour: with no ATA image mounted (C: is HLE-backed), the primary
    // channel reads open-bus, so the HDD probe FAILs and the icon stays grey. It
    // lights only when a real ATA disk is attached (see the aggregate test).
    assert_eq!(
        status(&run_post_bare(), "component.disk_hdd"),
        SuiteRecordStatus::Fail
    );
}

#[test]
fn hdd_probe_passes_with_a_mounted_ata_disk() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .unwrap();
    machine.mount_hdd(vec![0u8; 64 * 512]);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    let results = parse_result_block(machine.memory().as_slice()).expect("valid VDTS result block");
    assert_eq!(
        status(&results, "component.disk_hdd"),
        SuiteRecordStatus::Pass
    );
}
