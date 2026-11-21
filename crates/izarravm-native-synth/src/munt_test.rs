// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A full queue and a closed synth are DIFFERENT answers, and the caller acts
/// on the difference: one costs a message, the other costs the engine.
///
/// `mt32emu_play_msg` and `mt32emu_play_sysex` return only these two failures
/// (libmt32emu 2.8.2, `c_interface.cpp`: `if (!isOpen()) return NOT_OPENED;
/// return playMsg(...) ? OK : QUEUE_FULL`). Collapsing both onto one opaque
/// `NativeCall` (which is what this used to do) made a momentarily busy
/// synth indistinguishable from a dead one, and `MidiEngine` latches itself
/// permanently silent on the latter.
#[test]
fn a_full_queue_and_a_closed_synth_do_not_report_the_same_error() {
    assert_eq!(munt_result("play", 0), Ok(()));
    assert_eq!(munt_result("play", QUEUE_FULL), Err(Error::SynthQueueFull));
    assert_eq!(munt_result("play", NOT_OPENED), Err(Error::SynthNotOpened));
    assert_ne!(
        munt_result("play", QUEUE_FULL),
        munt_result("play", NOT_OPENED)
    );
    // Anything else still surfaces as itself, with the library's code intact:
    // an answer this wrapper does not know is not one to keep playing through.
    assert_eq!(
        munt_result("play", -100),
        Err(Error::NativeCall {
            operation: "play",
            code: -100,
        })
    );
}

/// Bytes that are not a ROM must come back as "not identified" from both the
/// single and the merge call, and must not crash the library.
#[test]
fn garbage_bytes_are_not_identified_by_either_rom_call() {
    let garbage = vec![0xA5_u8; 4096];
    let other = vec![0x5A_u8; 4096];
    let empty: Vec<u8> = Vec::new();
    let handler = ReportHandler { v0: ptr::null() };
    // SAFETY: A null report handler and instance pointer select library defaults.
    let context =
        NonNull::new(unsafe { mt32emu_create_context(handler, ptr::null_mut()) }).unwrap();
    let mut synth = MuntSynth {
        context,
        roms: Vec::new(),
    };

    // SAFETY: The library refuses these buffers, so it keeps no pointer to them,
    // and they outlive `synth` in any case.
    let single = unsafe { synth.add_image(&garbage) };
    let merged = unsafe { synth.merge_images(&garbage, &other) };
    let empty_single = unsafe { synth.add_image(&empty) };
    let empty_merged = unsafe { synth.merge_images(&empty, &empty) };
    for code in [single, merged, empty_single, empty_merged] {
        assert!(code < 0, "garbage must not be accepted, got {code}");
    }
}
