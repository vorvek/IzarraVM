// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn config(backend: MidiBackend) -> MidiConfig {
    MidiConfig {
        backend,
        external_port: None,
        soundfont: None,
        mt32_control_rom: None,
        mt32_pcm_rom: None,
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

    let failed_name = MidiPortId {
        name: String::new(),
        ordinal: 0,
    };
    assert_eq!(matching_port_index([None], &failed_name), None);
}

#[test]
fn silent_and_deferred_backends_report_actionable_status() {
    assert_eq!(
        MidiEngine::open(&config(MidiBackend::Off)).status(),
        MidiStatus::Ready
    );
    assert_eq!(
        MidiEngine::open(&config(MidiBackend::External)).status(),
        MidiStatus::MissingPort
    );
    assert_eq!(
        MidiEngine::open(&config(MidiBackend::FluidSynth)).status(),
        MidiStatus::InitializationFailed
    );
    assert_eq!(
        MidiEngine::open(&config(MidiBackend::Munt)).status(),
        MidiStatus::MissingRoms
    );

    let temp = tempfile::tempdir().unwrap();
    let mut missing_soundfont = config(MidiBackend::FluidSynth);
    missing_soundfont.soundfont = Some(temp.path().join("missing.sf3"));
    assert_eq!(
        MidiEngine::open(&missing_soundfont).status(),
        MidiStatus::MissingSoundFont
    );
}

#[test]
fn external_close_covers_every_channel() {
    let messages: Vec<_> = all_notes_off_messages().collect();
    assert_eq!(messages.len(), 16);
    assert_eq!(messages[0], [0xb0, 123, 0]);
    assert_eq!(messages[15], [0xbf, 123, 0]);
}
