//! Integration coverage for Stream C1: the four simple port probes (Lotura
//! identity and mode, the 8042 keyboard controller, the PIT, and COM1). The test
//! boots the real Izarra BIOS image, runs POST to completion, parses the VDTS
//! result block out of guest memory, and asserts each probe's record by name.
//! Records are order independent and other streams add their own, so every check
//! matches by name rather than by position.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::{SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile};

/// Boot the BIOS, run POST, and return the parsed result block.
fn run_post() -> izarravm_firmware::SuiteResults {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .expect("the Izarra BIOS image builds a machine");

    // POST runs at reset and writes every record well within this budget. The
    // run may end on the cycle limit or a halt depending on what the later
    // streams leave the boot tail doing; either way the result block is complete,
    // so we only require that the call returns without surfacing a machine error.
    machine
        .run_until_halt_or_cycles(20_000_000)
        .expect("the BIOS runs without a machine error");

    parse_result_block(machine.memory().as_slice()).expect("POST writes a valid VDTS result block")
}

/// Find a record by name regardless of where the streams placed it.
fn find<'a>(
    results: &'a izarravm_firmware::SuiteResults,
    name: &str,
) -> &'a izarravm_firmware::SuiteRecord {
    results
        .records
        .iter()
        .find(|record| record.name == name)
        .unwrap_or_else(|| panic!("expected a record named {name}"))
}

#[test]
fn lotura_identity_probe_passes() {
    let results = run_post();
    let record = find(&results, "component.cpu_lotura");
    assert_eq!(record.status, SuiteRecordStatus::Pass);
}

#[test]
fn lotura_mode_probe_measures_the_boot_mode() {
    let results = run_post();
    let record = find(&results, "cpu.gsw_mode");
    assert_eq!(record.status, SuiteRecordStatus::Measure);
    // The machine boots in GSW-386 compatibility mode, which the Lotura mode
    // register reports as code 0.
    assert_eq!(record.value.as_deref(), Some("0"));
}

#[test]
fn keyboard_8042_self_test_probe_passes() {
    let results = run_post();
    let record = find(&results, "component.kbd_8042");
    assert_eq!(record.status, SuiteRecordStatus::Pass);
}

#[test]
fn pit_probe_passes() {
    let results = run_post();
    let record = find(&results, "component.timer_pit");
    assert_eq!(record.status, SuiteRecordStatus::Pass);
}

#[test]
fn serial_com1_probe_passes() {
    let results = run_post();
    let record = find(&results, "component.serial_com1");
    assert_eq!(record.status, SuiteRecordStatus::Pass);
}

#[test]
fn machine_boots_in_gsw386_mode() {
    // The mode probe formats whatever the Lotura register reports for the active
    // mode. Pin the boot default here so a future change to it surfaces alongside
    // the MEASURE value above rather than silently.
    let machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .expect("machine builds");
    assert_eq!(machine.active_mode(), GswMode::Gsw386);
}
