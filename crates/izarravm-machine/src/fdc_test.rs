// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Drive the chip's data register the way a guest does: write the opcode then
/// the parameter bytes.
fn issue(fdc: &mut Fdc, bytes: &[u8]) {
    for &b in bytes {
        fdc.write_port(0x3F5, b);
    }
}

/// Read the whole pending result phase as a vector.
fn drain_result(fdc: &mut Fdc) -> Vec<u8> {
    let mut out = Vec::new();
    while fdc.main_status() & msr::DIO != 0 {
        out.push(fdc.read_port(0x3F5).unwrap());
    }
    out
}

fn ready_chip() -> Fdc {
    let mut fdc = Fdc::default();
    // Leave reset and select drive 0 with the DMA/IRQ gate on.
    fdc.write_port(0x3F2, 0x0C);
    // Drop the power-on reset interrupt so tests start from a clean line.
    issue(&mut fdc, &[0x08]); // SENSE INTERRUPT STATUS
    let _ = drain_result(&mut fdc);
    fdc
}

#[test]
fn version_returns_enhanced_controller() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x10]); // VERSION
    assert_eq!(drain_result(&mut fdc), vec![0x90]);
}

#[test]
fn specify_consumes_two_params_with_no_result() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x03, 0xDF, 0x02]); // SPECIFY: SRT/HUT, HLT/ND
    // No result phase: DIO is clear and the chip is back to the command phase.
    assert_eq!(fdc.main_status() & msr::DIO, 0, "no result bytes to read");
    assert_eq!(fdc.main_status() & msr::CB, 0, "command no longer busy");
    // The next opcode is accepted straight away.
    issue(&mut fdc, &[0x10]);
    assert_eq!(drain_result(&mut fdc), vec![0x90]);
}

#[test]
fn recalibrate_then_sense_interrupt_reports_seek_end_at_cyl_zero() {
    let mut fdc = ready_chip();
    // Move the head off track 0 first so RECALIBRATE has somewhere to come from.
    issue(&mut fdc, &[0x0F, 0x00, 10]); // SEEK drive 0 to cyl 10
    issue(&mut fdc, &[0x08]); // clear that seek interrupt
    let _ = drain_result(&mut fdc);

    issue(&mut fdc, &[0x07, 0x00]); // RECALIBRATE drive 0
    let res = {
        issue(&mut fdc, &[0x08]); // SENSE INTERRUPT STATUS
        drain_result(&mut fdc)
    };
    assert_eq!(res.len(), 2, "ST0 + present cylinder");
    assert_eq!(res[0] & st0::SE, st0::SE, "seek-end set in ST0");
    assert_eq!(res[0] & 0xC0, st0::IC_NORMAL, "normal termination");
    assert_eq!(res[1], 0, "present cylinder is 0 after recalibrate");
}

#[test]
fn sense_interrupt_with_none_pending_is_invalid() {
    let mut fdc = ready_chip();
    // ready_chip already cleared the power-on interrupt, so none is pending.
    issue(&mut fdc, &[0x08]);
    assert_eq!(drain_result(&mut fdc), vec![0x80], "invalid, no PCN");
}

#[test]
fn sense_drive_status_reports_track0_and_ready() {
    let mut fdc = ready_chip();
    fdc.set_media_present(true);
    issue(&mut fdc, &[0x04, 0x00]); // SENSE DRIVE STATUS, drive 0 head 0
    let st3v = drain_result(&mut fdc);
    assert_eq!(st3v.len(), 1);
    assert_eq!(st3v[0] & st3::TRACK0, st3::TRACK0, "head at cyl 0");
    assert_eq!(st3v[0] & st3::READY, st3::READY, "media present");
    assert_eq!(st3v[0] & st3::TWO_SIDED, st3::TWO_SIDED, "double-sided");
}

#[test]
fn read_data_stages_a_transfer_request() {
    let mut fdc = ready_chip();
    // READ DATA: HDS+DS=0, C=2, H=0, R=3, N=2(512), EOT=9, GPL, DTL.
    issue(
        &mut fdc,
        &[0xE6, 0x00, 0x02, 0x00, 0x03, 0x02, 0x09, 0x1B, 0xFF],
    );
    // While the transfer is staged the chip is busy with RQM low: it is not
    // asking the CPU for a byte, it is waiting for the execution phase to run.
    assert_eq!(fdc.main_status() & msr::CB, msr::CB);
    assert_eq!(fdc.main_status() & msr::RQM, 0);
    let req = fdc.take_transfer().expect("a staged transfer");
    assert!(req.read);
    assert_eq!(req.cylinder, 2);
    assert_eq!(req.sector, 3);
    assert_eq!(req.bytes_per_sec, 512);
    assert_eq!(req.end_sector, 9);
}

#[test]
fn completed_read_produces_a_seven_byte_result_and_irq() {
    let mut fdc = ready_chip();
    issue(
        &mut fdc,
        &[0xE6, 0x00, 0x02, 0x00, 0x03, 0x02, 0x09, 0x1B, 0xFF],
    );
    let req = fdc.take_transfer().unwrap();
    fdc.complete_transfer(req, 2, 0, 9, true);
    // The completion edge fires (DMA/IRQ gate is on).
    assert!(fdc.take_irq(), "IRQ6 raised on completion");
    let res = drain_result(&mut fdc);
    assert_eq!(res.len(), 7, "ST0,ST1,ST2,C,H,R,N");
    assert_eq!(res[0] & 0xC0, st0::IC_NORMAL, "normal termination");
    assert_eq!(res[3], 2, "ending cylinder");
    assert_eq!(res[5], 9, "ending sector");
    assert_eq!(res[6], 2, "N=2 (512-byte sectors)");
}

#[test]
fn invalid_opcode_returns_single_invalid_status() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x1E]); // not a modeled command
    assert_eq!(drain_result(&mut fdc), vec![0x80]);
}

#[test]
fn reset_clears_an_in_flight_command_and_raises_an_interrupt() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x03, 0xDF]); // SPECIFY, one parameter still owed
    assert_eq!(fdc.main_status() & msr::CB, msr::CB, "mid-command");
    // Pulse reset (clear bit2) then release it.
    fdc.write_port(0x3F2, 0x00);
    assert_eq!(fdc.main_status() & msr::CB, 0, "command dropped by reset");
    fdc.write_port(0x3F2, 0x0C);
    // Leaving reset raises the power-up interrupt with ST0 = 0xC0.
    issue(&mut fdc, &[0x08]);
    let res = drain_result(&mut fdc);
    assert_eq!(res[0], 0xC0, "ready-changed / abnormal after reset");
}

#[test]
fn irq_is_masked_when_the_dor_gate_is_off() {
    let mut fdc = Fdc::default();
    fdc.write_port(0x3F2, 0x04); // out of reset, drive 0, but DMA/IRQ gate off
    issue(&mut fdc, &[0x07, 0x00]); // RECALIBRATE raises a seek interrupt
    assert!(!fdc.take_irq(), "gate off masks the IRQ line");
    // The interrupt is still latched internally and clears via SENSE INTERRUPT.
    issue(&mut fdc, &[0x08]);
    let res = drain_result(&mut fdc);
    assert_eq!(res[0] & st0::SE, st0::SE);
}
