// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A full queue and a closed synth are DIFFERENT answers, and the caller acts
/// on the difference: one costs a message, the other costs the engine.
///
/// `mt32emu_play_msg` and `mt32emu_play_sysex` return only these two failures
/// (libmt32emu 2.8.2, `c_interface.cpp`: `if (!isOpen()) return NOT_OPENED;
/// return playMsg(...) ? OK : QUEUE_FULL`). Collapsing both onto one opaque
/// `NativeCall` -- which is what this used to do -- made a momentarily busy
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
