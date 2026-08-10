// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::fs;
use std::path::PathBuf;

use izarravm_native_synth::{
    Error, FluidSynth, MuntSynth, NATIVE_SYNTH_AVAILABLE, RomKind, SAMPLE_RATE_HZ,
};

fn embedded_soundfont() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("third_party/fluidr3mono/FluidR3Mono_GM.sf3")
}

#[test]
fn fluidr3mono_renders_audible_program_zero() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }
    let mut synth = FluidSynth::new(embedded_soundfont()).unwrap();
    synth.send(&[0xC0, 0]).unwrap();
    synth.send(&[0x90, 60, 100]).unwrap();
    let mut output = vec![0_i16; (SAMPLE_RATE_HZ / 2 * 2) as usize];
    synth.render_interleaved_i16(&mut output).unwrap();
    synth.send(&[0x80, 60, 0]).unwrap();
    synth.all_notes_off().unwrap();
    synth.reset().unwrap();
    assert!(output.iter().any(|sample| sample.unsigned_abs() > 16));
}

#[test]
fn wrappers_reject_incomplete_midi_and_partial_frames() {
    if !NATIVE_SYNTH_AVAILABLE {
        assert_eq!(
            FluidSynth::new(embedded_soundfont()).unwrap_err(),
            Error::Unavailable
        );
        assert_eq!(
            MuntSynth::new("control.rom", "pcm.rom").unwrap_err(),
            Error::Unavailable
        );
        return;
    }
    let mut synth = FluidSynth::new(embedded_soundfont()).unwrap();
    synth.send(&[0xF8]).unwrap();
    synth.send(&[0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7]).unwrap();
    assert_eq!(synth.send(&[0x90, 60]), Err(Error::InvalidMidiMessage));
    assert_eq!(
        synth.render_interleaved_i16(&mut [0_i16]),
        Err(Error::OutputMustBeStereo)
    );
}

/// The ROM loader must say WHICH requirement failed, and take a set however it
/// is laid out.
///
/// Every distribution of these ROMs names its files differently --
/// `MT32_CONTROL.ROM`, `CM32L_CONTROL.ROM`, `MT32_1.0.7_control.rom`,
/// `ctrl_mt32_1_07.rom`, and half-image pairs that have to be merged before
/// they can be identified at all -- so the loader identifies by CONTENT and
/// accepts a folder as readily as a file. What it must never do is answer a
/// user with a complete, valid set and a message that says only "The MIDI
/// output could not be initialized."
#[test]
fn munt_reports_missing_and_unrecognized_roms() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.rom");
    assert_eq!(
        MuntSynth::new(&missing, &missing).unwrap_err(),
        Error::MissingRom(missing.clone())
    );

    // One file, and it is not a ROM this library knows: name it.
    let invalid = directory.path().join("invalid.rom");
    fs::write(&invalid, b"not a Roland ROM").unwrap();
    assert_eq!(
        MuntSynth::new(&invalid, &invalid).unwrap_err(),
        Error::InvalidRom(invalid.clone())
    );

    // A FOLDER is a legal hint, and a folder of files that are not ROMs is
    // reported as "no control ROM among these", listing what was tried. The
    // list is the whole point: it is what tells a user whose set is in another
    // directory, or whose download is short a file, which it was.
    let folder = tempfile::tempdir().unwrap();
    let names = ["MT32_CONTROL.ROM", "MT32_PCM.ROM", "notes.txt"];
    for name in names {
        fs::write(folder.path().join(name), b"still not a Roland ROM").unwrap();
    }
    let error = MuntSynth::new(folder.path(), folder.path()).unwrap_err();
    let Error::RomNotFound { kind, searched } = &error else {
        panic!("a folder of non-ROMs must report a missing control image, got {error:?}");
    };
    assert_eq!(*kind, RomKind::Control);
    assert_eq!(
        searched.len(),
        names.len(),
        "every file in the folder was offered to the library: {searched:?}"
    );
    let message = error.to_string();
    for name in names {
        assert!(
            message.contains(name),
            "the message must name the files it tried; {name} missing from {message:?}"
        );
    }

    // Two file hints in DIFFERENT folders both get their folder scanned, so a
    // set whose halves sit beside the file the user picked is still found. Here
    // nothing is a real ROM, but the search has to have covered both sides.
    let second = tempfile::tempdir().unwrap();
    fs::write(second.path().join("PCM_A.ROM"), b"nope").unwrap();
    let error = MuntSynth::new(folder.path().join("MT32_CONTROL.ROM"), second.path()).unwrap_err();
    let Error::RomNotFound { searched, .. } = &error else {
        panic!("expected a missing-image report, got {error:?}");
    };
    assert!(
        searched.iter().any(|path| path.ends_with("PCM_A.ROM"))
            && searched.iter().any(|path| path.ends_with("notes.txt")),
        "both hints' folders must be searched before giving up: {searched:?}"
    );
}

#[test]
#[ignore = "requires user-supplied Roland ROM paths"]
fn local_munt_roms_render_audio() {
    if !NATIVE_SYNTH_AVAILABLE {
        return;
    }
    let control = env::var_os("IZARRAVM_MT32_CONTROL_ROM")
        .expect("set IZARRAVM_MT32_CONTROL_ROM to run this ignored test");
    let pcm = env::var_os("IZARRAVM_MT32_PCM_ROM")
        .expect("set IZARRAVM_MT32_PCM_ROM to run this ignored test");
    let mut synth = MuntSynth::new(control, pcm).unwrap();
    synth.send(&[0xC1, 0]).unwrap();
    synth.send(&[0x91, 60, 100]).unwrap();
    let mut output = vec![0_i16; SAMPLE_RATE_HZ as usize * 2];
    synth.render_interleaved_i16(&mut output).unwrap();
    synth.all_notes_off().unwrap();
    synth.reset().unwrap();
    assert!(output.iter().any(|sample| sample.unsigned_abs() > 16));
}
