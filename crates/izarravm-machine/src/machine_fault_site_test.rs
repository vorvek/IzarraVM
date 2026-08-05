// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// An access to an undecoded port is fatal on purpose, so that a hardware gap
/// stays visible instead of open-bussing into a silent divergence. That makes
/// the stop report the whole diagnosis, and until this test it named the wrong
/// instruction: EIP advances at fetch, and the fatal path did not rewind it, so
/// the report pointed one instruction PAST the IN or OUT. Prince of Persia was
/// investigated for hours off a CS:IP that was a return address.
///
/// Both assertions are load-bearing. Checking the address alone would also pass
/// if the IN never ran at all (a decode refusal, a segment fault, any earlier
/// stop), so the port is checked too: the fixture must not be able to pass by
/// never reaching the instruction it is about.
#[test]
fn a_fatal_port_fault_names_the_faulting_instruction_not_the_next_one() {
    // 0x100: BA 10 20  mov dx, 0x2010    <- 0x2010 is decoded by nothing
    // 0x103: EC        in  al, dx        <- the faulting instruction
    // 0x104: CD 20     int 20h
    const PROG: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xCD, 0x20];
    const IN_AL_DX: u32 = 0x103;

    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("0x2010")),
        "expected the run to stop on the undecoded port, got {stop:?}"
    );
    let site = machine
        .cpu()
        .fault_site()
        .expect("a fatal CpuError must record where it was raised");
    assert_eq!(
        site.eip, IN_AL_DX,
        "the recorded site must be the IN itself, not the instruction after it"
    );
    assert!(
        !site.cs_moved,
        "IN cannot change CS, so the recorded segment must be trustworthy"
    );
}

/// The record must not survive into a run that did not fault. Nothing clears
/// it, because a fatal CpuError leaves the machine resumable and callers that
/// ignore the stop reason go on running it, so the guarantee is on the READ
/// side: only the fatal arm consults the field. This pins the machine-visible
/// half of that, namely that a clean run reports nothing.
#[test]
fn a_run_that_did_not_fault_records_no_fault_site() {
    // mov ax,0x4c00; int 21h -- exits cleanly, touches no port.
    const PROG: &[u8] = &[0xB8, 0x00, 0x4C, 0xCD, 0x21];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(machine.cpu().fault_site().is_none());
}
