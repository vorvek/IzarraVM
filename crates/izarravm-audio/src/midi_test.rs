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
    munt.render(&mut output, tick_for_frame(64));
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
fn backend_switch_keeps_the_guest_cursor_and_discards_old_pcm() {
    let mut midi = MidiEngine::open_receiver(&config(MidiBackend::Off));
    midi.render(&mut [(0, 0); 37], tick_for_frame(37));
    midi.staged.push_back((1, 1));
    midi.reconfigure(&config(MidiBackend::Munt));
    assert!(midi.staged.is_empty());
    midi.render(&mut [(0, 0); 5], tick_for_frame(42));
    assert_eq!(midi.guest_frame_cursor, 42);
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
    midi.render(&mut output, tick_for_frame(2_048));
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
fn frozen_guest_time_does_not_advance_synthesis_past_later_events() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }

    let mut midi = MidiEngine::open_wavetable(&config(MidiBackend::Off));
    midi.send(&message(0, &[0xc0, 0]));
    midi.render(&mut [(0, 0); 512], 0);
    midi.render(&mut [(0, 0); 512], 0);
    assert_eq!(midi.guest_frame_cursor, 0);

    midi.send(&message(128, &[0x90, 60, 110]));
    let mut output = vec![(0, 0); 2_048];
    midi.render(&mut output, tick_for_frame(2_048));
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
fn half_speed_guest_progress_keeps_the_event_offset_in_guest_pcm() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }

    let mut midi = MidiEngine::open_wavetable(&config(MidiBackend::Off));
    midi.send(&message(0, &[0xc0, 0]));
    midi.render(&mut [(0, 0); 512], tick_for_frame(256));
    assert_eq!(midi.guest_frame_cursor, 256);

    midi.send(&message(320, &[0x90, 60, 110]));
    let mut output = vec![(0, 0); 512];
    midi.render(&mut output, tick_for_frame(512));
    let early_peak = output[..64]
        .iter()
        .flat_map(|frame| [frame.0, frame.1])
        .map(i16::unsigned_abs)
        .max()
        .unwrap();
    let note_peak = output[64..256]
        .iter()
        .flat_map(|frame| [frame.0, frame.1])
        .map(i16::unsigned_abs)
        .max()
        .unwrap();
    assert!(early_peak <= 2, "pre-note dither was {early_peak}");
    assert!(note_peak > 16, "note peak was only {note_peak}");
    assert!(output[256..].iter().all(|frame| *frame == (0, 0)));
}

#[test]
fn full_staging_keeps_old_audio_and_resumes_without_dropping_events() {
    let mut midi = MidiEngine::open_receiver(&config(MidiBackend::Off));
    let target = (MAX_STAGED_FRAMES + 256) as u64;
    midi.queue(message((MAX_STAGED_FRAMES + 64) as u64, &[0x90, 60, 100]));

    midi.stage_until(target);
    assert_eq!(midi.staged.len(), MAX_STAGED_FRAMES);
    assert_eq!(midi.guest_frame_cursor, MAX_STAGED_FRAMES as u64);
    assert_eq!(midi.pending.len(), 1);

    for _ in 0..128 {
        midi.staged.pop_front();
    }
    midi.stage_until(target);
    assert_eq!(midi.staged.len(), MAX_STAGED_FRAMES);
    assert_eq!(midi.guest_frame_cursor, (MAX_STAGED_FRAMES + 128) as u64);
    assert!(midi.pending.is_empty());
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
        midi.render(&mut output, tick_for_frame(2_048));
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

/// The MIDI leg's volume control, which is the card's wavetable register pair
/// (`0x50`/`0x51`) arriving here as a linear gain.
///
/// This leg is the one source in the machine that joins the mix outside
/// `render_audio` -- native synthesis runs on the host clock and is added by
/// the frontend afterwards -- so the card's summing node cannot attenuate it
/// and the scalar has to be applied at the point it adds itself. Unity is an
/// exact identity, so an engine nobody has set a gain on is byte-for-byte what
/// it was before the control existed.
#[test]
fn the_wavetable_gain_attenuates_what_the_engine_adds_to_the_mix() {
    let staged = [(1000i16, -2000i16), (30000, 30000), (-32768, 12), (0, 0)];

    let render_with = |gain: Option<(f32, f32)>| {
        let mut midi = MidiEngine::open_wavetable(&config(MidiBackend::Off));
        if let Some(gain) = gain {
            midi.set_gain(gain);
        }
        for frame in staged {
            midi.staged.push_back(frame);
        }
        let mut mix = [(0i16, 0i16); 4];
        midi.render(&mut mix, tick_for_frame(4));
        mix
    };

    assert_eq!(
        render_with(None),
        staged,
        "an engine with no gain set adds exactly what it synthesised"
    );
    assert_eq!(
        render_with(Some((1.0, 1.0))),
        staged,
        "unity is an exact identity, not a rounding of one"
    );
    assert_eq!(
        render_with(Some((0.0, 0.0))),
        [(0, 0); 4],
        "a muted wavetable register silences the leg"
    );

    // -20 dB, which is level 21 on the card's 5-bit ladder: each channel scaled
    // on its own, so the pair is a real stereo control and not one scalar.
    let quiet = render_with(Some((0.1, 0.5)));
    for (index, (left, right)) in quiet.iter().enumerate() {
        let (want_l, want_r) = (
            (f32::from(staged[index].0) * 0.1).round() as i16,
            (f32::from(staged[index].1) * 0.5).round() as i16,
        );
        assert_eq!((*left, *right), (want_l, want_r), "frame {index}");
    }

    // The leg still SUMS into what is already in the buffer; the gain scales
    // the contribution, not the mix it lands in.
    let mut midi = MidiEngine::open_wavetable(&config(MidiBackend::Off));
    midi.set_gain((0.5, 0.5));
    midi.staged.push_back((1000, 1000));
    let mut mix = [(4000i16, -4000i16)];
    midi.render(&mut mix, tick_for_frame(1));
    assert_eq!(mix, [(4500, -3500)]);
}

/// A byte the synth will not take must cost that MESSAGE, not the synthesiser.
///
/// The MPU-401 parser forwards `0xF4`, `0xF5`, `0xF7`, `0xF9` and `0xFD` to us
/// as one-byte messages, verbatim, because that is what the guest wrote to the
/// port -- a lone `0xF7` is what a driver emits when it abandons a SysEx, and
/// undefined real-time bytes come from any program that pokes the port. None of
/// them is a message a synthesiser accepts, so `validate` refuses them.
///
/// It used to be that ONE such byte called `fail_native`, which drops the
/// adapter to Silent and latches the status at InitializationFailed for good:
/// the P300 went silent mid-game, the panel said "The MIDI output could not be
/// initialized.", and nothing short of restarting the machine brought it back.
/// The engine now drops the message and keeps playing.
#[test]
fn a_message_the_synth_refuses_does_not_silence_the_engine() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }
    let mut midi = MidiEngine::open_wavetable(&config(MidiBackend::Off));
    assert!(matches!(midi.adapter, MidiAdapter::Fluid(_)));

    // A note on, then every byte the MPU can hand us that no synth will take,
    // then a second note on -- all inside one staging window.
    midi.send(&message(0, &[0x90, 60, 110]));
    for (index, refused) in [0xF4_u8, 0xF5, 0xF7, 0xF9, 0xFD].into_iter().enumerate() {
        midi.send(&message(index as u64 + 1, &[refused]));
    }
    midi.send(&message(16, &[0x90, 67, 110]));

    let mut output = vec![(0, 0); 4_096];
    midi.render(&mut output, tick_for_frame(4_096));

    assert_eq!(midi.status(), MidiStatus::Ready, "the engine is still open");
    assert!(
        matches!(midi.adapter, MidiAdapter::Fluid(_)),
        "the adapter must not have been dropped to Silent"
    );
    assert_eq!(
        midi.rejected_messages(),
        5,
        "each refused byte is counted, and only the refused ones"
    );
    assert!(
        output.iter().any(|frame| *frame != (0, 0)),
        "the notes on either side of the refused bytes still sound"
    );
}

/// An engine that failed must be re-openable, and Accept is what re-opens it.
///
/// `fail_native` is a latch by design -- a broken synth cannot be played
/// through -- but the only thing that used to clear it was a settings change
/// the engine's own role cares about, and the wavetable's role cares about
/// exactly one setting: the SoundFont path. So a P300 that died stayed dead and
/// the config panel kept showing its error no matter what was changed there,
/// including switching the P330 receiver off. The stale red message the owner
/// reported and the dead wavetable were one bug.
#[test]
fn reconfiguring_reopens_an_engine_that_failed_even_with_identical_settings() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }
    let settings = config(MidiBackend::Off);
    let mut midi = MidiEngine::open_wavetable(&settings);
    midi.fail_native();
    assert_eq!(midi.status(), MidiStatus::InitializationFailed);
    assert!(matches!(midi.adapter, MidiAdapter::Silent));

    // The SAME settings. Nothing about the config has changed; the engine's
    // state has.
    midi.reconfigure(&settings);
    assert_eq!(midi.status(), MidiStatus::Ready);
    assert!(matches!(midi.adapter, MidiAdapter::Fluid(_)));
    assert_eq!(midi.rejected_messages(), 0, "the count restarts with it");

    // A healthy engine with unchanged settings is still left alone: reconfigure
    // must not tear down and rebuild a working synth on every Accept, which
    // would cut whatever it was playing.
    let mut healthy = MidiEngine::open_wavetable(&settings);
    healthy.queue(message(30, &[0x90, 60, 100]));
    healthy.reconfigure(&settings);
    assert_eq!(
        healthy.pending.len(),
        1,
        "a working engine keeps its queued messages"
    );
}

/// A synth whose queue is momentarily full is not a broken synth.
///
/// `mt32emu_play_msg` has exactly two failure answers -- the queue is full, or
/// there is no open synth behind the context -- and until now both arrived here
/// as an opaque `NativeCall`, which `send_native` treats as terminal. A P330
/// that got ahead of its own event queue for one audio window would therefore
/// latch itself Silent and stay there, the same permanent death a stray `0xF7`
/// used to cause. Only `NOT_OPENED` is worth the engine.
#[test]
fn a_full_synth_queue_costs_the_message_and_a_closed_synth_costs_the_engine() {
    assert!(matches!(
        triage_send_error(&SynthError::SynthQueueFull),
        NativeSend::Rejected
    ));
    assert!(matches!(
        triage_send_error(&SynthError::InvalidMidiMessage),
        NativeSend::Rejected
    ));
    assert!(matches!(
        triage_send_error(&SynthError::SynthNotOpened),
        NativeSend::Failed
    ));
    // Anything the synthesiser reports that is not one of those two is still
    // treated as a failure: an unknown answer from a native library is not a
    // thing to keep playing through.
    assert!(matches!(
        triage_send_error(&SynthError::NativeCall {
            operation: "Munt MIDI",
            code: -100,
        }),
        NativeSend::Failed
    ));
}
