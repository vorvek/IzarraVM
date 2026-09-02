// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Integration coverage for the four simple port probes (Lotura
//! identity and mode, the 8042 keyboard controller, the PIT, and COM1). One POST
//! of the real Izarra BIOS image, then every probe's record by name. Records are
//! order independent and other probes add their own, so every check matches by
//! name rather than by position.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::{SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile};

/// Boot the BIOS, run POST, and return the parsed result block.
fn run_post() -> izarravm_firmware::SuiteResults {
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), izarra_bios())
        .expect("the Izarra BIOS image builds a machine");
    assert_eq!(machine.active_mode(), GswMode::Gsw386);

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
fn given_post_completes_when_the_result_block_is_parsed_then_the_port_probes_pass() {
    // Given: a bare Izarra BIOS machine (one POST, not one POST per record).
    // When: POST writes the VDTS result block.
    // Then: the four simple port probes PASS and the Lotura mode MEASURE is the
    // boot default (code 0). Host-side active_mode is pinned inside run_post.
    let results = run_post();
    assert_eq!(
        find(&results, "component.cpu_lotura").status,
        SuiteRecordStatus::Pass
    );
    let mode = find(&results, "cpu.gsw_mode");
    assert_eq!(mode.status, SuiteRecordStatus::Measure);
    assert_eq!(mode.value.as_deref(), Some("0"));
    assert_eq!(
        find(&results, "component.kbd_8042").status,
        SuiteRecordStatus::Pass
    );
    assert_eq!(
        find(&results, "component.timer_pit").status,
        SuiteRecordStatus::Pass
    );
    assert_eq!(
        find(&results, "component.serial_com1").status,
        SuiteRecordStatus::Pass
    );
}
