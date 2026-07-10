// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::fs;
use std::path::PathBuf;

use izarravm_native_synth::{Error, FluidSynth, MuntSynth, NATIVE_SYNTH_AVAILABLE, SAMPLE_RATE_HZ};

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

    let invalid = directory.path().join("invalid.rom");
    fs::write(&invalid, b"not a Roland ROM").unwrap();
    assert_eq!(
        MuntSynth::new(&invalid, &invalid).unwrap_err(),
        Error::InvalidRom(invalid)
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
