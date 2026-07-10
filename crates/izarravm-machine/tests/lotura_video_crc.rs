// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::VideoCard;
use izarravm_machine::{Machine, MachineProfile, StopReason};

const LOTURA_CRC_COM: &[u8] = include_bytes!("fixtures/lotura_crc.com");
const CHECK_NAMES: [&str; 24] = [
    "BIOS mode 00h",
    "BIOS mode 01h",
    "BIOS mode 02h",
    "BIOS mode 03h",
    "text split and preset row scan",
    "BIOS mode 04h",
    "BIOS mode 05h",
    "BIOS mode 06h",
    "BIOS mode 07h",
    "BIOS mode 0Dh",
    "mode 0Dh word addressing",
    "mode 0Dh doubleword addressing",
    "mode 0Dh byte-address round trip",
    "mode 0Dh no-clear transition",
    "BIOS mode 0Eh",
    "BIOS mode 0Fh",
    "BIOS mode 10h",
    "BIOS mode 11h",
    "BIOS mode 12h",
    "BIOS mode 13h",
    "mode 13h no-clear transition",
    "mode X",
    "mode X pixel panning",
    "Hercules graphics",
];

#[test]
fn guest_checks_fixed_legacy_video_crcs() {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), LOTURA_CRC_COM)
            .expect("the CRC fixture loads as a DOS COM program");

    let reason = machine
        .run_until_halt_or_cycles(60_000_000)
        .expect("the guest video matrix runs without a CPU fault");

    match reason {
        StopReason::TestExit { code: 0 } => {}
        StopReason::TestExit { code } => {
            let name = CHECK_NAMES.get(usize::from(code) - 1).unwrap_or(&"unknown");
            panic!("guest CRC mismatch at check {code}: {name}");
        }
        other => panic!("guest video matrix stopped before completion: {other:?}"),
    }
}
