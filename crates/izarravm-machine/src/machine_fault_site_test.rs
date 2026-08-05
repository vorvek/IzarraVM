// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// An access to an undecoded port USED TO BE fatal by default. It is not any
/// more -- real hardware floats an unclaimed read and swallows an unclaimed
/// write, and stopping on the first one hid every later probe (see
/// `bus::OpenBusPorts`). The fatal path survives as an opt-in, which is exactly
/// what these tests need and what they arm with `set_fatal_ports`: it is the
/// only path that records a `fault_site`, and chasing which instruction probes
/// one specific port is still worth doing.
///
/// When a port IS fatal, the stop reports the whole diagnosis, and until this
/// test it named the wrong
/// instruction: EIP advances at fetch, and the fatal path did not rewind it, so
/// the report pointed one instruction PAST the IN or OUT. Prince of Persia was
/// investigated for hours off a CS:IP that was a return address.
///
/// The address and the port are both load-bearing. Checking the address alone
/// would also pass if the IN never ran at all (a decode refusal, a segment
/// fault, any earlier stop), so the port is checked too: the fixture must not
/// be able to pass by never reaching the instruction it is about. The third
/// assertion, on cs_moved, is documentation rather than a guard: this fixture
/// runs in real mode, where nothing here can move CS.
#[test]
fn a_fatal_port_fault_names_the_faulting_instruction_not_the_next_one() {
    // 0x100: BA 10 20  mov dx, 0x2010    <- 0x2010 is decoded by nothing
    // 0x103: EC        in  al, dx        <- the faulting instruction
    // 0x104: CD 20     int 20h
    const PROG: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xCD, 0x20];
    const IN_AL_DX: u32 = 0x103;

    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    // Opt 0x2010 back onto the fatal path; open bus does not stop, and a stop is
    // what carries the fault site this test is about.
    machine.set_fatal_ports(&[0x2010]);
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

/// The byte dump used to hand a LINEAR address to `read_physical_u8`, which
/// does no page walk, so under paging it printed whatever happened to sit at
/// that physical address. It never said so; it printed plausible hex either
/// way, which is the failure mode that makes a diagnostic worse than useless.
///
/// The fixture puts the faulting code in a page whose linear address is NOT its
/// physical one, and plants a decoy at the physical address. The decoy is what
/// makes this test able to fail: without it, an unfixed dump reading the
/// physical address would find zeros, and "not the instruction" and "zeros"
/// would be indistinguishable from a correct read of an unmapped page. The
/// precondition assertion pins that the decoy really is where the broken path
/// would look.
#[test]
fn the_fault_dump_reads_code_through_the_guest_page_tables() {
    const PD: u32 = 0x1000;
    const PT: u32 = 0x2000;
    // Linear page 5 is mapped to a frame at 0x9000, so linear != physical for
    // the code the dump has to find.
    const CODE_LINEAR: u32 = 0x5000;
    const CODE_FRAME: u32 = 0x9000;
    // mov edx,0x2010; in al,dx; hlt. CS here is a 32-bit descriptor, so the
    // immediate is four bytes: the 16-bit encoding would swallow the IN as part
    // of it and the fixture would run off into unmapped memory.
    const PROG: [u8; 7] = [0xBA, 0x10, 0x20, 0x00, 0x00, 0xEC, 0xF4];
    const DECOY: u8 = 0xA5;

    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xf4]).unwrap();
    machine.set_fatal_ports(&[0x2010]);
    machine.write_physical_u32(PD, PT | 7);
    // Identity-map the whole first 4 MB, so nothing in the fixture can take a
    // page fault for an unrelated reason, and override exactly one page.
    for page in 0u32..1024 {
        let pte = if page == CODE_LINEAR >> 12 {
            CODE_FRAME | 7
        } else {
            (page << 12) | 7
        };
        machine.write_physical_u32(PT + page * 4, pte);
    }
    for (offset, byte) in PROG.iter().enumerate() {
        machine.write_physical_u8(CODE_FRAME + offset as u32, *byte);
    }
    // The decoy sits where the unfixed, identity-assuming dump would read.
    for offset in 0..PROG.len() as u32 {
        machine.write_physical_u8(CODE_LINEAR + offset, DECOY);
    }
    assert_eq!(
        machine.read_physical_u8(CODE_LINEAR),
        DECOY,
        "precondition: the physical address must hold the decoy, or this test \
         cannot tell a paging-aware read from an identity-assuming one"
    );

    machine.cpu.control.cr3 = PD;
    machine.cpu.control.cr0 |= 0x8000_0001;
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        machine
            .cpu
            .registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    machine.cpu.registers.eip = CODE_LINEAR;
    // No IDT is set up here, so a timer IRQ arriving mid-fixture would stop the
    // run on a nested delivery fault before the IN is ever reached. Mask it;
    // this fixture is about the dump, not about interrupt delivery.
    machine.cpu.registers.eflags &= !0x200;

    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("0x2010")),
        "expected the undecoded-port stop, got {stop:?}"
    );

    let error = CpuError::Bus(izarravm_bus::BusError::UnsupportedPort { port: 0x2010 });
    let report = machine.fault_trace_report(&error);
    let at_eip = report
        .lines()
        .find(|line| line.contains("bytes at/after EIP"))
        .expect("the report must carry the bytes at EIP");
    let before_eip = report
        .lines()
        .find(|line| line.contains("bytes before EIP"))
        .expect("the report must carry the bytes before EIP");

    // The window opens ON the IN, so at/after starts with the IN and the HLT
    // behind it, and the MOV that set up DX is in the window before. All of it
    // lives in the mapped frame, so none of it is reachable without the walk.
    assert!(
        at_eip.contains("ec f4"),
        "the dump must walk the page tables to the real instruction, got: {at_eip}"
    );
    assert!(
        before_eip.contains("ba 10 20 00 00"),
        "the preceding window must walk too, got: {before_eip}"
    );
    assert!(
        !at_eip.contains("a5") && !before_eip.contains("a5"),
        "the dump read the physical address instead of translating:\n{before_eip}\n{at_eip}"
    );
}

/// The diagnosis has to arrive without anyone having set anything. That is the
/// whole of T3, and it is a claim about the CALL SITE being unconditional, not
/// about a formatter, so this drives a real machine to a real stop rather than
/// calling the formatter and watching it format.
///
/// No test here touches IZARRAVM_FAULT_TRACE. The CPU-side gate latches in a
/// OnceLock, so the first reader in a test binary fixes it process-wide, and
/// mutating process env from a threaded harness is racy anyway.
#[test]
fn a_fatal_port_fault_reports_itself_without_any_env_var() {
    const PROG: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    // Opt 0x2010 back onto the fatal path; open bus does not stop, and a stop is
    // what carries the fault site this test is about.
    machine.set_fatal_ports(&[0x2010]);
    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    let line = machine
        .last_fault_line()
        .expect("a fatal stop must report itself with no env var set");
    assert!(line.contains("0x2010"), "must name the port: {line}");
    assert!(
        line.contains("0x00000103"),
        "must name the faulting instruction, not the one after it: {line}"
    );
    // The window opens ON the faulting instruction, so the first byte is its
    // opcode: 0xEC is IN AL,DX. That byte is the datum the old report lacked,
    // and having it is the difference between "port 0x2010 came from
    // somewhere" and "an IN AL,DX did this".
    assert!(
        line.contains("bytes=[ec cd 20"),
        "must carry the faulting instruction's own bytes: {line}"
    );
}

/// The report is latched on the SITE. Without a latch it floods, because a
/// fatal error leaves the machine resumable and callers do resume it; with a
/// plain "print once" it would hide every later fault, and the interesting one
/// is often not the first. Neither failure mode is visible by running the
/// emulator once, so it is pinned here.
#[test]
fn the_fault_report_latches_per_site_not_per_run() {
    // Both halves are driven for real, using the property that motivates the
    // latch in the first place: a fatal error leaves the machine resumable, so
    // calling run again continues the guest from where it stopped.

    // Spinning on ONE bad port: mov dx,0x2010; in al,dx; jmp back to the IN.
    // The second run re-enters at the JMP and faults at the same address.
    const SPIN: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xEB, 0xFD];
    let mut spinner =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), SPIN).unwrap();
    spinner.set_fatal_ports(&[0x2010]);
    spinner.run_until_halt_or_cycles(1_000_000).unwrap();
    let first = spinner
        .last_fault_line()
        .expect("first fault reports")
        .to_string();
    assert!(first.contains("0x00000103"));
    // Clear the record before the second run. Asserting the line is UNCHANGED
    // would not test the latch at all: a re-report at the same site rebuilds a
    // byte-identical string, so the assertion passes whether the latch runs or
    // not (verified: disabling the latch left that version green). Clearing
    // first makes a re-report visible, and also catches a fixture whose second
    // run silently never faulted.
    spinner.last_fault_line = None;
    spinner.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(
        spinner.last_fault_line(),
        None,
        "a repeat at the same site must not re-report, or a spinning guest \
         floods stderr for as long as it runs"
    );

    // TWO bad ports back to back. The second run faults one byte further on,
    // which is a different site and must get through: hiding it would bury the
    // fault that matters behind whichever one happened to come first.
    const TWO: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xEC, 0xCD, 0x20];
    let mut two =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), TWO).unwrap();
    two.set_fatal_ports(&[0x2010]);
    two.run_until_halt_or_cycles(1_000_000).unwrap();
    let one = two
        .last_fault_line()
        .expect("first fault reports")
        .to_string();
    assert!(one.contains("0x00000103"));
    two.run_until_halt_or_cycles(1_000_000).unwrap();
    let other = two.last_fault_line().expect("second site reports");
    assert!(
        other.contains("0x00000104"),
        "a fault at a different site must be reported, not swallowed: {other}"
    );
}

/// A clean run must leave nothing behind for a reporter to pick up. Nothing
/// clears the field, because a fatal CpuError leaves the machine resumable and
/// callers that ignore the stop reason go on running it, so the real guarantee
/// is on the READ side: only the fatal arm consults it.
///
/// Honest scope: this is a WEAK test, and the reason is worth keeping. Deleting
/// all three record calls leaves it green, because nothing in the fixture ever
/// writes the field and the assertion then just reads `FaultSite::default()`.
/// What it does catch is a `record_fault_site` call that wandered onto a
/// non-fault path. The stop-reason assertion is load-bearing for a different
/// reason: without it the test also passes when the program never ran at all
/// and the machine merely hit the cycle limit.
#[test]
fn a_run_that_did_not_fault_records_no_fault_site() {
    // mov ax,0x4c00; int 21h -- exits cleanly, touches no port.
    const PROG: &[u8] = &[0xB8, 0x00, 0x4C, 0xCD, 0x21];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    // Opt 0x2010 back onto the fatal path; open bus does not stop, and a stop is
    // what carries the fault site this test is about.
    machine.set_fatal_ports(&[0x2010]);
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(stop, StopReason::DosExit { .. }),
        "the program must actually have run and exited, got {stop:?}"
    );
    assert!(machine.cpu().fault_site().is_none());
    assert!(machine.last_fault_line().is_none());
}
