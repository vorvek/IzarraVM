// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn take_bytes(mpu: &mut Mpu401) -> Vec<u8> {
    mpu.take_message().expect("complete MIDI message").bytes
}

#[test]
fn reset_and_uart_commands_acknowledge() {
    let mut mpu = Mpu401::default();
    assert_eq!(mpu.status(), RX_EMPTY);

    mpu.write_command(RESET);
    assert_eq!(mpu.status(), 0);
    assert_eq!(mpu.read_data(), ACK);
    assert_eq!(mpu.status(), RX_EMPTY);

    mpu.write_command(ENTER_UART);
    assert!(mpu.is_uart());
    assert_eq!(mpu.read_data(), ACK);
}

#[test]
fn version_and_revision_follow_the_ack() {
    let mut mpu = Mpu401::default();
    mpu.write_command(REQUEST_VERSION);
    assert_eq!(mpu.read_data(), ACK);
    assert_eq!(mpu.read_data(), 0x15);
    mpu.write_command(REQUEST_REVISION);
    assert_eq!(mpu.read_data(), ACK);
    assert_eq!(mpu.read_data(), 0x01);
}

#[test]
fn intelligent_timebase_and_tempo_commands_store_their_parameters() {
    let mut mpu = Mpu401::default();
    mpu.write_command(0xc5);
    assert_eq!(mpu.read_data(), ACK);
    assert_eq!(mpu.timebase(), 120);

    mpu.write_command(0xe0);
    assert_eq!(mpu.read_data(), ACK);
    mpu.write_data(90, 12);
    assert_eq!(mpu.tempo(), 90);
    assert!(mpu.take_message().is_none());
}

#[test]
fn channel_messages_keep_running_status_and_timestamp_completion() {
    let mut mpu = Mpu401::default();
    for (byte, clock) in [(0x90, 1), (60, 2), (100, 3), (61, 4), (0, 5)] {
        mpu.write_data(byte, clock);
    }

    let first = mpu.take_message().expect("first note");
    assert_eq!(first.bytes, [0x90, 60, 100]);
    assert_eq!(first.guest_tick, 3);
    let second = mpu.take_message().expect("running-status note");
    assert_eq!(second.bytes, [0x90, 61, 0]);
    assert_eq!(second.guest_tick, 5);
}

#[test]
fn program_change_and_system_common_have_their_own_lengths() {
    let mut mpu = Mpu401::default();
    for byte in [0xc1, 9, 10, 0xf2, 1, 2] {
        mpu.write_data(byte, 7);
    }

    assert_eq!(take_bytes(&mut mpu), [0xc1, 9]);
    assert_eq!(take_bytes(&mut mpu), [0xc1, 10]);
    assert_eq!(take_bytes(&mut mpu), [0xf2, 1, 2]);
}

#[test]
fn real_time_bytes_do_not_interrupt_sysex() {
    let mut mpu = Mpu401::default();
    for byte in [0xf0, 0x41, 0xf8, 0x10, 0xf7] {
        mpu.write_data(byte, 11);
    }

    assert_eq!(take_bytes(&mut mpu), [0xf8]);
    assert_eq!(take_bytes(&mut mpu), [0xf0, 0x41, 0x10, 0xf7]);
}

#[test]
fn host_input_is_bounded_and_read_in_order() {
    let mut mpu = Mpu401::default();
    assert_eq!(mpu.inject_input(&[0x90, 60, 100]), 3);
    assert_eq!(mpu.status(), 0);
    assert_eq!(mpu.read_data(), 0x90);
    assert_eq!(mpu.read_data(), 60);
    assert_eq!(mpu.read_data(), 100);
    assert_eq!(mpu.read_data(), 0xff);

    let oversized = vec![0; INPUT_CAPACITY + 1];
    assert_eq!(mpu.inject_input(&oversized), INPUT_CAPACITY);
    assert_eq!(mpu.inject_input(&[1]), 0);
}
