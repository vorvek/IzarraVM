// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for RAM sizing through INT 15h AH=88h
//! and pattern-checks it, publishing memory.ramtest (PASS/FAIL) and
//! memory.detected_kib (MEASURE, total KiB) plus the INT 12h word.

use izarravm_core::VideoCard;
use izarravm_firmware::{SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile, StopReason};

mod support;
use support::mount_idle_boot_floppy;

/// Boot the BIOS with `memory_mib` of RAM, run POST to halt, and return the
/// memory.ramtest status and the parsed memory.detected_kib value.
fn ramtest_result(memory_mib: u16) -> (SuiteRecordStatus, u32) {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(memory_mib, VideoCard::Vega),
        izarra_bios(),
    )
    .unwrap();
    mount_idle_boot_floppy(&mut machine);
    let stop = machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert!(
        matches!(stop, StopReason::CycleLimit { .. }),
        "POST completes and the BIOS idles (it does not halt)"
    );
    let results = parse_result_block(machine.memory().as_slice()).unwrap();
    let ramtest = results
        .records
        .iter()
        .find(|record| record.name == "memory.ramtest")
        .expect("memory.ramtest record present");
    let detected = results
        .records
        .iter()
        .find(|record| record.name == "memory.detected_kib")
        .expect("memory.detected_kib record present");
    assert_eq!(detected.status, SuiteRecordStatus::Measure);
    let kib: u32 = detected
        .value
        .as_deref()
        .expect("detected_kib has a value")
        .parse()
        .expect("detected_kib is decimal");
    (ramtest.status, kib)
}

#[test]
fn ramtest_passes_and_sizes_16mib() {
    let (status, kib) = ramtest_result(16);
    assert_eq!(status, SuiteRecordStatus::Pass);
    assert_eq!(kib, 16384);
}

#[test]
fn ramtest_passes_and_sizes_4mib() {
    let (status, kib) = ramtest_result(4);
    assert_eq!(status, SuiteRecordStatus::Pass);
    assert_eq!(kib, 4096);
}

#[test]
fn ramtest_preserves_the_ebda_adjusted_int12h_word() {
    // The INT 12h slot 0040:0013 reports conventional memory below the EBDA.
    // The full detected total
    // (16384 KiB here) lives in the memory.detected_kib record instead. A
    // self-booting game sizes its heap off this word, so reporting the total
    // sent QuickDOS booters into ROM/unmapped space and a "Bad free parm" halt.
    let mut machine =
        Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), izarra_bios()).unwrap();
    mount_idle_boot_floppy(&mut machine);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    let memory = machine.memory().as_slice();
    let word = u16::from_le_bytes([memory[0x413], memory[0x414]]);
    assert_eq!(word, 639);
}
