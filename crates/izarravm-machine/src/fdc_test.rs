// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn issue(fdc: &mut Fdc, bytes: &[u8]) {
    for &byte in bytes {
        fdc.write_port_at(0x3F5, byte, fdc.now_ticks);
    }
}

fn drain_result(fdc: &mut Fdc) -> Vec<u8> {
    let mut bytes = Vec::new();
    while fdc.main_status() & msr::DIO != 0 {
        bytes.push(fdc.read_port(0x3F5).unwrap());
    }
    bytes
}

fn advance_next(fdc: &mut Fdc) -> Option<DmaByteRequest> {
    let ticks = fdc
        .ticks_until_event(fdc.now_ticks)
        .expect("a pending FDC deadline");
    fdc.advance_to(fdc.now_ticks + ticks)
}

fn ready_chip() -> Fdc {
    let mut fdc = Fdc::default();
    fdc.write_port_at(0x3F2, 0x0C, fdc.now_ticks);
    issue(&mut fdc, &[0x08]);
    let _ = drain_result(&mut fdc);
    fdc
}

fn ready_disk() -> Fdc {
    let mut fdc = ready_chip();
    fdc.set_media_geometry(Some(Geometry {
        cylinders: 80,
        heads: 2,
        sectors: 18,
        drive_type: 0x04,
    }));
    fdc.write_port_at(0x3F2, 0x1C, fdc.now_ticks);
    assert_eq!(advance_next(&mut fdc), None, "motor spin-up deadline");
    assert!(fdc.drive_ready(0));
    fdc
}

#[test]
fn version_returns_enhanced_controller() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x10]);
    assert_eq!(drain_result(&mut fdc), vec![0x90]);
}

#[test]
fn specify_programs_seek_and_head_load_timing_without_a_result() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x03, 0xF0, 0x04]);
    assert_eq!(fdc.step_rate_ticks, MILLIS_TICKS);
    assert_eq!(fdc.head_load_ticks, 4 * MILLIS_TICKS);
    assert_eq!(fdc.main_status() & (msr::DIO | msr::CB), 0);

    issue(&mut fdc, &[0x0F, 0x00, 4]);
    assert_eq!(fdc.seek_busy & 1, 1);
    assert_eq!(fdc.ticks_until_event(fdc.now_ticks), Some(4 * MILLIS_TICKS));
}

#[test]
fn seek_is_busy_until_its_exact_deadline_then_sense_reports_the_cylinder() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x0F, 0x00, 10]);
    let delay = 30 * MILLIS_TICKS;
    assert_eq!(fdc.main_status() & 1, 1, "drive 0 busy");
    assert_eq!(fdc.main_status() & msr::CB, 0, "command phase reopened");
    assert_eq!(fdc.advance_to(delay - 1), None);
    assert!(!fdc.take_irq());
    assert_eq!(fdc.present_cyl[0], 0);

    assert_eq!(fdc.advance_to(delay), None);
    assert!(fdc.take_irq());
    assert_eq!(fdc.main_status() & 1, 0);
    issue(&mut fdc, &[0x08]);
    let result = drain_result(&mut fdc);
    assert_eq!(result[0] & st0::SE, st0::SE);
    assert_eq!(result[1], 10);
}

#[test]
fn recalibrate_uses_the_same_step_clock_and_finishes_at_track_zero() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x0F, 0x00, 10]);
    advance_next(&mut fdc);
    issue(&mut fdc, &[0x08]);
    let _ = drain_result(&mut fdc);

    issue(&mut fdc, &[0x07, 0x00]);
    assert_eq!(
        fdc.ticks_until_event(fdc.now_ticks),
        Some(30 * MILLIS_TICKS)
    );
    advance_next(&mut fdc);
    issue(&mut fdc, &[0x08]);
    let result = drain_result(&mut fdc);
    assert_eq!(result[0] & st0::SE, st0::SE);
    assert_eq!(result[1], 0);
}

#[test]
fn sense_interrupt_without_a_completed_seek_is_invalid() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x08]);
    assert_eq!(drain_result(&mut fdc), vec![st0::IC_INVALID]);
}

#[test]
fn motor_spin_up_controls_ready_and_spin_down_reaches_stopped() {
    let mut fdc = ready_chip();
    fdc.set_media_geometry(Some(Geometry {
        cylinders: 80,
        heads: 2,
        sectors: 18,
        drive_type: 0x04,
    }));
    fdc.write_port_at(0x3F2, 0x1C, fdc.now_ticks);
    issue(&mut fdc, &[0x04, 0x00]);
    assert_eq!(drain_result(&mut fdc)[0] & st3::READY, 0);
    assert_eq!(fdc.advance_to(MOTOR_SPIN_UP_TICKS - 1), None);
    assert_eq!(fdc.motors[0].phase, MotorPhase::Starting);
    assert_eq!(fdc.advance_to(MOTOR_SPIN_UP_TICKS), None);
    assert!(fdc.drive_ready(0));

    fdc.write_port_at(0x3F2, 0x0C, fdc.now_ticks);
    assert_eq!(fdc.motors[0].phase, MotorPhase::Stopping);
    let stopped_at = MOTOR_SPIN_UP_TICKS + MOTOR_SPIN_DOWN_TICKS;
    assert_eq!(fdc.advance_to(stopped_at - 1), None);
    assert_eq!(fdc.motors[0].phase, MotorPhase::Stopping);
    assert_eq!(fdc.advance_to(stopped_at), None);
    assert_eq!(fdc.motors[0].phase, MotorPhase::Stopped);
}

#[test]
fn read_id_waits_for_rotation_then_enters_the_result_phase() {
    let mut fdc = ready_disk();
    issue(&mut fdc, &[0x0A, 0x00]);
    assert_eq!(fdc.main_status() & msr::RQM, 0);
    let deadline = fdc.next_deadline().unwrap();
    assert_eq!(fdc.advance_to(deadline - 1), None);
    assert_eq!(fdc.main_status() & msr::DIO, 0);
    assert_eq!(fdc.advance_to(deadline), None);
    assert!(fdc.take_irq());
    let result = drain_result(&mut fdc);
    assert_eq!(result.len(), 7);
    assert_eq!(result[0] & 0xC0, st0::IC_NORMAL);
    assert!((1..=18).contains(&result[5]));
}

#[test]
fn read_data_requests_one_timed_dma_cycle_per_byte() {
    let mut fdc = ready_disk();
    issue(
        &mut fdc,
        &[0xE6, 0x00, 0x02, 0x00, 0x03, 0x02, 0x03, 0x1B, 0xFF],
    );
    assert_eq!(fdc.main_status() & msr::CB, msr::CB);
    assert_eq!(fdc.main_status() & msr::RQM, 0);

    for offset in 0..512u16 {
        let request = advance_next(&mut fdc).expect("one byte reaches channel 2");
        assert_eq!(request.sector, 3);
        assert_eq!(request.offset, offset);
        fdc.complete_dma_byte(DmaByteOutcome {
            transferred: true,
            terminal_count: offset == 511,
        });
    }

    assert!(fdc.take_irq());
    let result = drain_result(&mut fdc);
    assert_eq!(result.len(), 7);
    assert_eq!(result[0] & 0xC0, st0::IC_NORMAL);
    assert_eq!(result[3], 2);
    assert_eq!(result[5], 3);
    assert_eq!(result[6], 2);
}

#[test]
fn a_masked_dma_cycle_ends_read_data_abnormally_at_its_deadline() {
    let mut fdc = ready_disk();
    issue(
        &mut fdc,
        &[0xE6, 0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0x1B, 0xFF],
    );
    let _ = advance_next(&mut fdc).expect("first byte request");
    fdc.complete_dma_byte(DmaByteOutcome {
        transferred: false,
        terminal_count: false,
    });
    assert!(fdc.take_irq());
    let result = drain_result(&mut fdc);
    assert_eq!(result[0] & 0xC0, st0::IC_ABNORMAL);
    assert_eq!(result[1] & 0x04, 0x04, "no-data status");
}

#[test]
fn dor_dma_gate_prevents_a_channel_request() {
    let mut fdc = ready_disk();
    fdc.write_port_at(0x3F2, 0x14, fdc.now_ticks); // motor and reset on, DMA/IRQ gate off
    issue(
        &mut fdc,
        &[0xE6, 0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0x1B, 0xFF],
    );
    assert_eq!(advance_next(&mut fdc), None);
    assert!(!fdc.take_irq());
    assert_eq!(drain_result(&mut fdc)[0] & 0xC0, st0::IC_ABNORMAL);
}

#[test]
fn reset_drops_an_in_flight_data_command() {
    let mut fdc = ready_disk();
    issue(
        &mut fdc,
        &[0xE6, 0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0x1B, 0xFF],
    );
    assert_eq!(fdc.main_status() & msr::CB, msr::CB);
    fdc.write_port_at(0x3F2, 0x00, fdc.now_ticks);
    assert_eq!(fdc.main_status() & msr::CB, 0);
    assert!(fdc.operation.is_none());
    fdc.write_port_at(0x3F2, 0x0C, fdc.now_ticks);
    issue(&mut fdc, &[0x08]);
    assert_eq!(drain_result(&mut fdc)[0], 0xC0);
}

#[test]
fn dor_gate_masks_the_seek_edge_but_not_sense_interrupt_state() {
    let mut fdc = Fdc::default();
    fdc.write_port_at(0x3F2, 0x04, fdc.now_ticks);
    issue(&mut fdc, &[0x07, 0x00]);
    advance_next(&mut fdc);
    assert!(!fdc.take_irq());
    issue(&mut fdc, &[0x08]);
    assert_eq!(drain_result(&mut fdc)[0] & st0::SE, st0::SE);
}

#[test]
fn invalid_opcode_returns_one_invalid_status_byte() {
    let mut fdc = ready_chip();
    issue(&mut fdc, &[0x1E]);
    assert_eq!(drain_result(&mut fdc), vec![st0::IC_INVALID]);
}
