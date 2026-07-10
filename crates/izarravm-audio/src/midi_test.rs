// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_native_synth::NATIVE_SYNTH_AVAILABLE;
use std::fs;

fn config(backend: MidiBackend) -> MidiConfig {
    MidiConfig {
        backend,
        external_port: None,
        soundfont: None,
        mt32_control_rom: None,
        mt32_pcm_rom: None,
    }
}

fn tick_for_frame(frame: u64) -> u64 {
    u64::try_from(u128::from(frame) * u128::from(MASTER_CLOCK_HZ) / u128::from(SAMPLE_RATE_HZ))
        .unwrap()
}

fn message(frame: u64, bytes: &[u8]) -> TimedMidiMessage {
    TimedMidiMessage {
        guest_tick: tick_for_frame(frame),
        bytes: bytes.to_vec(),
    }
}

#[test]
fn exact_port_name_and_ordinal_select_only_the_requested_duplicate() {
    let names = [Some("LoopMIDI"), None, Some("Other"), Some("LoopMIDI")];
    let requested = MidiPortId {
        name: "LoopMIDI".into(),
        ordinal: 1,
    };
    assert_eq!(matching_port_index(names, &requested), Some(3));

    let absent = MidiPortId {
        name: "loopmidi".into(),
        ordinal: 0,
    };
    assert_eq!(matching_port_index(names, &absent), None);

    let ids = port_ids(names);
    assert_eq!(
        ids,
        [
            MidiPortId {
                name: "LoopMIDI".into(),
                ordinal: 0,
            },
            MidiPortId {
                name: "Other".into(),
                ordinal: 0,
            },
            MidiPortId {
                name: "LoopMIDI".into(),
                ordinal: 1,
            },
        ]
    );
}

#[test]
fn off_external_without_a_port_and_munt_without_roms_stay_actionable() {
    assert_eq!(
        MidiEngine::open_receiver(&config(MidiBackend::Off)).status(),
        MidiStatus::Ready
    );
    assert_eq!(
        MidiEngine::open_receiver(&config(MidiBackend::External)).status(),
        MidiStatus::MissingPort
    );

    let mut munt = MidiEngine::open_receiver(&config(MidiBackend::Munt));
    assert_eq!(munt.status(), MidiStatus::MissingRoms);
    let mut output = vec![(0, 0); 64];
    munt.send(&message(0, &[0x90, 60, 100]));
    munt.render(&mut output);
    assert!(output.iter().all(|frame| *frame == (0, 0)));
}

#[test]
fn master_ticks_map_to_the_first_mixer_frame_at_or_after_the_deadline() {
    assert_eq!(sample_frame_for_tick(0), 0);
    for frame in [1, 2, 127, 128, 44_100] {
        assert_eq!(sample_frame_for_tick(tick_for_frame(frame)), frame);
    }
    assert_eq!(sample_frame_for_tick(MASTER_CLOCK_HZ), 44_100);
}

#[test]
fn pending_messages_are_ordered_by_guest_timestamp() {
    let mut midi = MidiEngine::open_receiver(&config(MidiBackend::Off));
    midi.queue(message(30, &[0x90, 62, 100]));
    midi.queue(message(10, &[0x90, 60, 100]));
    midi.queue(message(20, &[0x90, 61, 100]));

    let frames: Vec<_> = midi
        .pending
        .iter()
        .map(|message| sample_frame_for_tick(message.guest_tick))
        .collect();
    assert_eq!(frames, [10, 20, 30]);
}

#[test]
fn backend_switch_keeps_the_absolute_render_cursor() {
    let mut midi = MidiEngine::open_receiver(&config(MidiBackend::Off));
    midi.render(&mut [(0, 0); 37]);
    midi.reconfigure(&config(MidiBackend::Munt));
    midi.render(&mut [(0, 0); 5]);
    assert_eq!(midi.rendered_frames, 42);
}

#[test]
fn embedded_fluidsynth_starts_a_note_at_its_guest_timed_offset() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }

    let mut midi = MidiEngine::open_wavetable(&config(MidiBackend::Off));
    assert_eq!(midi.status(), MidiStatus::Ready);
    midi.send(&message(0, &[0xc0, 0]));
    midi.send(&message(128, &[0x90, 60, 110]));

    let mut output = vec![(0, 0); 2_048];
    midi.render(&mut output);
    let early_peak = output[..128]
        .iter()
        .flat_map(|frame| [frame.0, frame.1])
        .map(i16::unsigned_abs)
        .max()
        .unwrap();
    let note_peak = output[128..]
        .iter()
        .flat_map(|frame| [frame.0, frame.1])
        .map(i16::unsigned_abs)
        .max()
        .unwrap();
    assert!(early_peak <= 2, "pre-note dither was {early_peak}");
    assert!(note_peak > 16, "note peak was only {note_peak}");
}

#[test]
fn missing_and_bad_custom_soundfonts_warn_status_and_use_the_embedded_bank() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    for path in [temp.path().join("missing.sf3"), temp.path().join("bad.sf3")] {
        if path.file_name().unwrap() == "bad.sf3" {
            fs::write(&path, b"not a SoundFont").unwrap();
        }
        let mut custom = config(MidiBackend::Off);
        custom.soundfont = Some(path);
        let mut midi = MidiEngine::open_wavetable(&custom);
        assert_eq!(midi.status(), MidiStatus::MissingSoundFont);

        midi.send(&message(0, &[0x90, 60, 110]));
        let mut output = vec![(0, 0); 2_048];
        midi.render(&mut output);
        assert!(output.iter().any(|frame| *frame != (0, 0)));
    }
}

#[test]
fn external_close_covers_every_channel() {
    let messages: Vec<_> = all_notes_off_messages().collect();
    assert_eq!(messages.len(), 16);
    assert_eq!(messages[0], [0xb0, 123, 0]);
    assert_eq!(messages[15], [0xbf, 123, 0]);
}

#[test]
fn failed_external_send_enters_missing_port_state_without_hardware() {
    let mut midi = MidiEngine::open_receiver(&config(MidiBackend::Off));
    assert_eq!(midi.record_external_send_result(Ok::<(), &str>(())), Ok(()));
    assert_eq!(midi.status(), MidiStatus::Ready);

    assert_eq!(
        midi.record_external_send_result(Err("disconnected")),
        Err("disconnected")
    );
    assert!(matches!(midi.adapter, MidiAdapter::Silent));
    assert_eq!(midi.status(), MidiStatus::MissingPort);
}

#[test]
fn missing_p330_receiver_does_not_disable_p300_fluidsynth() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }
    let wavetable = MidiEngine::open_wavetable(&config(MidiBackend::External));
    let receiver = MidiEngine::open_receiver(&config(MidiBackend::External));

    assert!(matches!(wavetable.adapter, MidiAdapter::Fluid(_)));
    assert_eq!(wavetable.status(), MidiStatus::Ready);
    assert_eq!(receiver.status(), MidiStatus::MissingPort);
}

#[test]
fn p330_receiver_changes_do_not_reset_p300_messages() {
    let mut wavetable = MidiEngine::open_wavetable(&config(MidiBackend::Off));
    wavetable.queue(message(30, &[0x90, 60, 100]));

    wavetable.reconfigure(&config(MidiBackend::Munt));
    assert_eq!(wavetable.pending.len(), 1);
}

#[test]
fn p300_soundfont_changes_do_not_reset_p330_messages() {
    let mut receiver = MidiEngine::open_receiver(&config(MidiBackend::Off));
    receiver.queue(message(30, &[0x90, 60, 100]));
    let mut changed = config(MidiBackend::Off);
    changed.soundfont = Some("custom.sf3".into());

    receiver.reconfigure(&changed);
    assert_eq!(receiver.pending.len(), 1);
}
