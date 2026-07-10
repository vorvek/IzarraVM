// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn take_bytes(mpu: &mut Mpu401) -> Vec<u8> {
    mpu.take_message().expect("complete MIDI message").bytes
}

fn acknowledged(mpu: &mut Mpu401, command: u8) {
    mpu.write_command(command);
    assert_eq!(mpu.read_data(), ACK, "command {command:#04x}");
}

fn arm_tracks(mpu: &mut Mpu401, mask: u8) -> u64 {
    acknowledged(mpu, 0xec);
    mpu.write_data(mask, 0);
    acknowledged(mpu, 0xb8);
    acknowledged(mpu, 0x08);
    let pulse = mpu.ticks_until_event().expect("first sequencer pulse");
    mpu.advance_to(pulse);
    pulse
}

fn submit_note(mpu: &mut Mpu401, timing: u8, note: u8, velocity: u8) {
    assert_eq!(mpu.read_data(), 0xf0);
    for value in [timing, 0x90, note, velocity] {
        mpu.write_data(value, mpu.now_tick);
    }
}

#[test]
fn reset_and_uart_commands_acknowledge() {
    let mut mpu = Mpu401::default();
    assert_eq!(mpu.status(), RX_EMPTY);

    mpu.write_command(RESET);
    assert_eq!(mpu.status(), 0);
    assert!(mpu.irq_level());
    assert_eq!(mpu.read_data(), ACK);
    assert_eq!(mpu.status(), RX_EMPTY);
    assert!(!mpu.irq_level());

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
fn intelligent_track_requests_count_timing_bytes_on_the_master_clock() {
    let mut mpu = Mpu401::default();
    let pulse = arm_tracks(&mut mpu, 1);
    assert!(mpu.irq_level());
    submit_note(&mut mpu, 2, 60, 100);
    assert!(!mpu.irq_level());

    mpu.advance_to(pulse * 2);
    assert!(mpu.take_message().is_none());
    mpu.advance_to(pulse * 3 - 1);
    assert!(mpu.take_message().is_none());
    mpu.advance_to(pulse * 3);

    let note = mpu.take_message().expect("timed track note");
    assert_eq!(note.bytes, [0x90, 60, 100]);
    assert_eq!(note.guest_tick, pulse * 3);
    assert_eq!(mpu.read_data(), 0xf0, "the track asks for its next event");
}

#[test]
fn intelligent_track_request_accepts_running_status() {
    let mut mpu = Mpu401::default();
    let pulse = arm_tracks(&mut mpu, 1);
    submit_note(&mut mpu, 1, 60, 100);
    mpu.advance_to(pulse * 2);
    assert_eq!(take_bytes(&mut mpu), [0x90, 60, 100]);

    assert_eq!(mpu.read_data(), 0xf0);
    for value in [1, 61, 0] {
        mpu.write_data(value, mpu.now_tick);
    }
    mpu.advance_to(pulse * 3);
    assert_eq!(take_bytes(&mut mpu), [0x90, 61, 0]);
}

#[test]
fn zero_timing_uses_the_short_end_of_input_deadline() {
    let mut mpu = Mpu401::default();
    let pulse = arm_tracks(&mut mpu, 1);
    submit_note(&mut mpu, 0, 64, 127);
    let due = pulse + IMMEDIATE_DELAY_TICKS;

    mpu.advance_to(due - 1);
    assert!(mpu.take_message().is_none());
    assert_eq!(mpu.status(), RX_EMPTY);
    mpu.advance_to(due);

    let note = mpu.take_message().expect("zero-timing note");
    assert_eq!(note.bytes, [0x90, 64, 127]);
    assert_eq!(note.guest_tick, due);
    assert_eq!(mpu.read_data(), 0xf0);
}

#[test]
fn conductor_requests_apply_tempo_on_the_requested_pulse() {
    let mut mpu = Mpu401::default();
    acknowledged(&mut mpu, 0x8f);
    acknowledged(&mut mpu, 0xb8);
    acknowledged(&mut mpu, 0x08);
    let first_pulse = mpu.ticks_until_event().unwrap();
    mpu.advance_to(first_pulse);
    assert_eq!(mpu.read_data(), 0xf9);

    for value in [1, 0xe0, 200] {
        mpu.write_data(value, mpu.now_tick);
    }
    mpu.advance_to(first_pulse * 2);

    assert_eq!(mpu.tempo(), 200);
    assert_eq!(
        mpu.read_data(),
        0xf9,
        "the conductor asks for its next command"
    );
    for value in [1, 0x8f] {
        mpu.write_data(value, mpu.now_tick);
    }
    assert_eq!(
        mpu.ticks_until_event(),
        Some(CLOCK_NUMERATOR.div_ceil(120 * 200) as u64),
        "the tempo change restarts the rational clock"
    );
}

#[test]
fn sequencer_advance_is_batch_invariant() {
    let mut whole = Mpu401::default();
    let mut split = Mpu401::default();
    let pulse = arm_tracks(&mut whole, 1);
    assert_eq!(arm_tracks(&mut split, 1), pulse);
    submit_note(&mut whole, 3, 67, 90);
    submit_note(&mut split, 3, 67, 90);
    let end = pulse * 4;

    whole.advance_to(end);
    for point in [pulse + 7, pulse * 2 - 3, pulse * 3 + 11, end] {
        split.advance_to(point);
    }

    assert_eq!(whole.take_message(), split.take_message());
    assert_eq!(whole.read_data(), split.read_data());
    assert_eq!(whole.ticks_until_event(), split.ticks_until_event());
}

#[test]
fn stop_playback_acknowledges_and_silences_every_channel() {
    let mut mpu = Mpu401::default();
    let _ = arm_tracks(&mut mpu, 1);
    assert_eq!(mpu.read_data(), 0xf0);
    acknowledged(&mut mpu, 0x04);
    assert!(!mpu.is_playing());
    for channel in 0..16 {
        assert_eq!(take_bytes(&mut mpu), [0xb0 | channel, 123, 0]);
    }
}

#[test]
fn uart_mode_gates_pending_intelligent_playback() {
    let mut mpu = Mpu401::default();
    let pulse = arm_tracks(&mut mpu, 1);
    submit_note(&mut mpu, 1, 60, 100);

    acknowledged(&mut mpu, ENTER_UART);
    assert!(mpu.is_uart());
    assert!(!mpu.is_playing());
    assert_eq!(mpu.ticks_until_event(), None);
    mpu.advance_to(pulse * 8);
    assert!(mpu.take_message().is_none());
    assert_eq!(mpu.status(), RX_EMPTY);

    for byte in [0x90, 61, 90] {
        mpu.write_data(byte, pulse * 8);
    }
    let message = mpu.take_message().expect("UART message");
    assert_eq!(message.bytes, [0x90, 61, 90]);
    assert_eq!(message.guest_tick, pulse * 8);
}

#[test]
fn clear_play_map_does_not_apply_configured_tracks_or_conductor() {
    let mut mpu = Mpu401::default();
    acknowledged(&mut mpu, 0xec);
    mpu.write_data(1, 0);
    acknowledged(&mut mpu, 0x8f);
    acknowledged(&mut mpu, 0xb9);
    acknowledged(&mut mpu, 0x08);

    assert_eq!(mpu.ticks_until_event(), None);
    let dormant_pulse = CLOCK_NUMERATOR.div_ceil(120 * 100) as u64;
    mpu.advance_to(dormant_pulse * 4);
    assert_eq!(mpu.status(), RX_EMPTY);

    acknowledged(&mut mpu, 0xb8);
    let pulse = mpu.ticks_until_event().expect("B8 applies the play map");
    mpu.advance_to(mpu.now_tick + pulse);
    assert_eq!(mpu.read_data(), 0xf0);
    mpu.write_data(0xf0, mpu.now_tick);
    assert_eq!(mpu.read_data(), 0xf9);
}

#[test]
fn reset_preserves_completed_output_and_appends_a_timed_silence() {
    let mut mpu = Mpu401::default();
    for byte in [0x90, 60, 100] {
        mpu.write_data(byte, 7);
    }

    mpu.write_command_at(RESET, 11);
    assert_eq!(mpu.read_data_at(11), ACK);
    let note = mpu.take_message().expect("pre-reset note");
    assert_eq!(note.bytes, [0x90, 60, 100]);
    assert_eq!(note.guest_tick, 7);
    let reset = mpu.take_message().expect("MIDI reset");
    assert_eq!(reset.bytes, [RESET]);
    assert_eq!(reset.guest_tick, 11);
    for channel in 0..16 {
        let silence = mpu.take_message().expect("all-notes-off");
        assert_eq!(silence.bytes, [0xb0 | channel, 123, 0]);
        assert_eq!(silence.guest_tick, 11);
    }
    assert!(mpu.take_message().is_none());
}

#[test]
fn track_counters_pause_until_the_active_request_is_complete() {
    let mut mpu = Mpu401::default();
    let pulse = arm_tracks(&mut mpu, 0b11);
    submit_note(&mut mpu, 2, 60, 100);

    assert_eq!(mpu.read_data(), 0xf1);
    for value in [10, 0x90, 65] {
        mpu.write_data(value, mpu.now_tick);
    }
    let held_at = mpu.now_tick;
    mpu.advance_to(held_at + pulse * 5);
    assert!(mpu.take_message().is_none());

    mpu.write_data(100, mpu.now_tick);
    let released_at = mpu.now_tick;
    mpu.advance_to(released_at + pulse * 2 - 1);
    assert!(mpu.take_message().is_none());
    mpu.advance_to(released_at + pulse * 2);
    let note = mpu.take_message().expect("track 0 note after request EOI");
    assert_eq!(note.bytes, [0x90, 60, 100]);
    assert_eq!(note.guest_tick, released_at + pulse * 2);
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
