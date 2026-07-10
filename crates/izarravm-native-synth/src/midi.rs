// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::Error;

pub(crate) enum MidiMessage<'a> {
    Short { bytes: [u8; 3], len: usize },
    SysEx(&'a [u8]),
}

pub(crate) fn validate(message: &[u8]) -> Result<MidiMessage<'_>, Error> {
    let Some(&status) = message.first() else {
        return Err(Error::InvalidMidiMessage);
    };
    if status == 0xF0 {
        if message.len() < 2
            || message.last() != Some(&0xF7)
            || message[1..message.len() - 1]
                .iter()
                .any(|byte| byte & 0x80 != 0)
        {
            return Err(Error::InvalidMidiMessage);
        }
        return Ok(MidiMessage::SysEx(message));
    }

    let expected = match status {
        0x80..=0xBF | 0xE0..=0xEF | 0xF2 => 3,
        0xC0..=0xDF | 0xF1 | 0xF3 => 2,
        0xF6 | 0xF8 | 0xFA..=0xFC | 0xFE | 0xFF => 1,
        _ => return Err(Error::InvalidMidiMessage),
    };
    if message.len() != expected || message[1..].iter().any(|byte| byte & 0x80 != 0) {
        return Err(Error::InvalidMidiMessage);
    }
    let mut bytes = [0; 3];
    bytes[..expected].copy_from_slice(message);
    Ok(MidiMessage::Short {
        bytes,
        len: expected,
    })
}
