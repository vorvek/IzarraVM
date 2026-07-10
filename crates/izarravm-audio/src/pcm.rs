// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Shared PCM format converters for the audio crate: the linear 8/16-bit DMA
//! sample mappings used by the Sound Blaster DSP, plus the ITU-T G.711 mu-law
//! and A-law companding decoders the AD1848 (WSS) codec expands internally.
//! Every converter returns a centered signed 16-bit value ready for the mixer.

use std::collections::VecDeque;

/// Rendered-frame ring capacity shared by the streaming DACs (SB DSP, WSS,
/// ADPCM); the host audio path drains the ring, so an unread backlog is
/// bounded by dropping the oldest frame (see [`push_frame_capped`]).
pub(crate) const RENDER_RING_CAP: usize = 8192;

/// Push one rendered stereo frame, dropping the oldest frame once the ring
/// holds [`RENDER_RING_CAP`] entries. Single-sourced so the per-device DACs
/// cannot drift on the cap/drop policy.
pub(crate) fn push_frame_capped(ring: &mut VecDeque<(i16, i16)>, frame: (i16, i16)) {
    if ring.len() >= RENDER_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(frame);
}

/// Convert one 8-bit Sound Blaster PCM sample (unsigned) to a centered signed
/// 16-bit value for the mixer: (byte - 128) * 256.
pub(crate) fn sample_u8(byte: u8) -> i16 {
    (i32::from(byte) - 128).clamp(-128, 127) as i16 * 256
}

/// Convert one signed 8-bit Sound Blaster PCM sample to 16-bit mixer range.
pub(crate) fn sample_i8(byte: u8) -> i16 {
    i16::from(byte as i8) * 256
}

/// Convert one signed 16-bit DMA sample directly (no centering): the SB16 16-bit
/// path is already signed PCM, so the bit pattern maps straight to i16.
pub(crate) fn sample_i16(word: u16) -> i16 {
    word as i16
}

/// Convert one unsigned 16-bit DMA sample (rare, mode-byte-selected) by
/// re-centering around 0x8000: the upper half (>= 0x8000) maps to 0..=32767 and
/// the lower half wraps to -32768..=-1.
pub(crate) fn sample_u16(word: u16) -> i16 {
    word.wrapping_sub(0x8000) as i16
}

/// G.711 mu-law bias added to the magnitude before encoding and subtracted on
/// decode. The standard fixes it at 33 (0x21).
#[allow(
    dead_code,
    reason = "consumed by the WSS/AD1848 codec path (later phase)"
)]
const ULAW_BIAS: i32 = 0x21;

/// Decode one ITU-T G.711 mu-law byte to signed 16-bit linear PCM.
///
/// The AD1848 (WSS) codec expands companded data internally; this decoder is
/// the expansion the machine-side codec path will pull. It is not yet wired to
/// a caller, hence the dead-code allow until the WSS integration lands.
///
/// Mu-law stores a sign bit (bit 7), a 3-bit exponent (bits 6..4), and a 4-bit
/// mantissa (bits 3..0), all stored complemented on the wire. The decode
/// inverts the byte, reconstructs the biased magnitude
/// `((mantissa << 1) | 0x21) << exponent`, removes the 0x21 bias, and applies
/// the sign. The standard decode yields a 14-bit magnitude (the AD1848 notes
/// mu-law expands to 14 bits); shifting left by 2 scales that into the full
/// signed 16-bit range the mixer expects.
///
/// Reference anchors:
/// - 0xFF (mu-law digital silence) -> 0, the smallest magnitude.
/// - 0x80 / 0x00 are the largest-magnitude positive / negative codes
///   (the inverted sign bit makes a stored high bit set decode positive).
#[allow(
    dead_code,
    reason = "consumed by the WSS/AD1848 codec path (later phase)"
)]
pub(crate) fn sample_ulaw(byte: u8) -> i16 {
    let inverted = !byte;
    let sign = inverted & 0x80;
    let exponent = (inverted >> 4) & 0x07;
    let mantissa = inverted & 0x0F;
    // Reconstruct the biased 14-bit magnitude, then drop the encode bias.
    let magnitude = (((i32::from(mantissa) << 1) | ULAW_BIAS) << exponent) - ULAW_BIAS;
    // 14-bit magnitude -> full-scale 16-bit PCM.
    let linear = magnitude << 2;
    if sign != 0 {
        -linear as i16
    } else {
        linear as i16
    }
}

/// Decode one ITU-T G.711 A-law byte to signed 16-bit linear PCM.
///
/// A-law stores a sign bit (bit 7), a 3-bit exponent (bits 6..4), and a 4-bit
/// mantissa (bits 3..0). On the wire every other bit is inverted with the 0x55
/// toggle mask. After undoing the toggle, the magnitude is reconstructed from
/// the exponent: exponent 0 is the linear segment `(mantissa << 1) | 1`, and
/// exponents 1..7 add the implicit leading one as `((mantissa << 1) | 0x21)`
/// shifted left by `exponent - 1`. The standard decode yields a 13-bit
/// magnitude (the AD1848 notes A-law expands to 13 bits); shifting left by 3
/// scales that into the full signed 16-bit range the mixer expects.
///
/// Reference anchors:
/// - 0xD5 (A-law digital silence) -> the smallest positive magnitude (~0).
/// - 0x55 (silence with the toggled sign cleared) -> the smallest negative.
/// - 0xAA / 0x2A are the largest-magnitude positive / negative codes.
#[allow(
    dead_code,
    reason = "consumed by the WSS/AD1848 codec path (later phase)"
)]
pub(crate) fn sample_alaw(byte: u8) -> i16 {
    let toggled = byte ^ 0x55;
    let sign = toggled & 0x80;
    let exponent = (toggled >> 4) & 0x07;
    let mantissa = i32::from(toggled & 0x0F);
    // Reconstruct the 13-bit magnitude per A-law's piecewise segments.
    let magnitude = if exponent == 0 {
        (mantissa << 1) | 1
    } else {
        ((mantissa << 1) | 0x21) << (exponent - 1)
    };
    // 13-bit magnitude -> full-scale 16-bit PCM.
    let linear = magnitude << 3;
    if sign != 0 {
        linear as i16
    } else {
        -linear as i16
    }
}

#[cfg(test)]
#[path = "pcm_test.rs"]
mod tests;
