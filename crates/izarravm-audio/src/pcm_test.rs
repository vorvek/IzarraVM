// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn sample_u8_centers_unsigned_bytes() {
    assert_eq!(sample_u8(0x00), -32_768, "0x00 -> full negative");
    assert_eq!(sample_u8(0x80), 0, "0x80 -> silence");
    assert_eq!(sample_u8(0xFF), 32_512, "0xFF -> near full positive");
}

#[test]
fn sample_i8_expands_signed_bytes() {
    assert_eq!(sample_i8(0x80), -32_768, "0x80 -> full negative");
    assert_eq!(sample_i8(0x00), 0, "0x00 -> silence");
    assert_eq!(sample_i8(0x7F), 32_512, "0x7F -> near full positive");
}

#[test]
fn sample_u16_recenters_around_0x8000() {
    assert_eq!(sample_u16(0x0000), -32_768, "0x0000 -> full negative");
    assert_eq!(sample_u16(0x8000), 0, "0x8000 -> silence");
    assert_eq!(sample_u16(0xFFFF), 32_767, "0xFFFF -> full positive");
    assert_eq!(sample_u16(0x8001), 1, "0x8001 -> +1");
}

#[test]
fn ulaw_silence_code_decodes_to_zero() {
    // 0xFF is mu-law digital silence (smallest magnitude): the inverted
    // byte is 0x00 (sign 0, exponent 0, mantissa 0), giving a biased
    // magnitude of 0x21 that the bias removal cancels to 0.
    assert_eq!(sample_ulaw(0xFF), 0, "mu-law 0xFF -> 0");
}

#[test]
fn ulaw_sign_bit_selects_polarity() {
    // The wire sign bit is complemented before use, so a stored byte with
    // its high bit SET decodes positive and one with its high bit CLEAR
    // decodes negative. 0xF0 (high bit set) and 0x70 (high bit clear) are a
    // same-magnitude +/- pair (exponent 0, mantissa 0 after inversion).
    assert!(sample_ulaw(0xF0) > 0, "0xF0 high bit set -> positive");
    assert!(sample_ulaw(0x70) < 0, "0x70 high bit clear -> negative");
    assert_eq!(
        sample_ulaw(0xF0),
        -sample_ulaw(0x70),
        "mu-law is sign-symmetric"
    );
}

#[test]
fn ulaw_magnitude_is_monotonic_across_codes() {
    // Walking the positive half from silence (0xFF) toward full scale
    // (0x80) the magnitude must increase monotonically. Decreasing the
    // stored byte from 0xFF to 0x80 traverses rising exponents/mantissas.
    let mut last = -1i32;
    for code in (0x80u16..=0xFF).rev() {
        let mag = i32::from(sample_ulaw(code as u8)).abs();
        assert!(
            mag >= last,
            "magnitude must not decrease at code {code:#04x}: {mag} < {last}"
        );
        last = mag;
    }
}

#[test]
fn ulaw_full_scale_reaches_near_full_magnitude() {
    // 0x80 / 0x00 are the largest-magnitude codes. After inversion both
    // have exponent 7, mantissa 0x0F: the 14-bit magnitude maxes at
    // ((0x0F<<1)|0x21)<<7 - 0x21 = 8031, scaled <<2 = 32124, near full
    // scale. 0x80 inverts to a clear sign (positive); 0x00 to a set sign
    // (negative).
    let positive = sample_ulaw(0x80);
    let negative = sample_ulaw(0x00);
    assert_eq!(positive, 32_124, "mu-law full-scale positive");
    assert_eq!(negative, -32_124, "mu-law full-scale negative");
    assert!(positive > 30_000 && negative < -30_000, "near +/- full");
}

#[test]
fn alaw_silence_code_decodes_near_zero() {
    // 0xD5 is A-law digital silence. After the 0x55 toggle it is 0x80
    // (sign 1, exponent 0, mantissa 0): the exponent-0 segment yields
    // magnitude 1, scaled to 8, the smallest representable step. With the
    // sign bit set the value is the smallest positive step: magnitude
    // ((0<<1)|1) = 1, scaled <<3 = 8. 0x55 (toggled sign clear) is its
    // negative twin at -8.
    assert_eq!(sample_alaw(0xD5), 8, "A-law 0xD5 = +8 (smallest step)");
    assert_eq!(sample_alaw(0x55), -8, "A-law 0x55 = -8 (smallest step)");
}

#[test]
fn alaw_sign_bit_selects_polarity() {
    // After the 0x55 toggle bit7 is the sign; set -> positive. 0xD5 and
    // 0x55 are the +/- silence pair, and they are sign-symmetric.
    assert!(sample_alaw(0xD5) > 0, "0xD5 -> positive (toggled sign set)");
    assert!(
        sample_alaw(0x55) < 0,
        "0x55 -> negative (toggled sign clear)"
    );
    assert_eq!(
        sample_alaw(0xD5),
        -sample_alaw(0x55),
        "A-law is sign-symmetric"
    );
}

#[test]
fn alaw_magnitude_is_monotonic_across_codes() {
    // Walk the positive half (toggled sign set) by exponent/mantissa order
    // and confirm the magnitude never decreases. The toggled value's low 7
    // bits are exponent:mantissa, so iterating that ordering is monotonic.
    let mut last = -1i32;
    for em in 0u8..=0x7F {
        // Build the stored byte: toggled = 0x80 | em, then untoggle.
        let stored = (0x80u8 | em) ^ 0x55;
        let mag = i32::from(sample_alaw(stored)).abs();
        assert!(
            mag >= last,
            "magnitude must not decrease at em {em:#04x}: {mag} < {last}"
        );
        last = mag;
    }
}

#[test]
fn ulaw_mid_segment_matches_itu_reference_magnitude() {
    // Cross-check mid-segment codes against the ITU-T G.711 reference mu-law
    // decode (Rec. G.711 sec. 6 / Table 2): a code expands to the 14-bit
    // magnitude (((mantissa<<1)|0x21) << exponent) - 0x21. This independently
    // pins the bias/segment formula, not just this impl's <<2 scale shift.
    //   stored 0x55 -> wire 0xAA: exponent 2, mantissa 10 ->
    //     ((10<<1)|0x21)<<2 - 0x21 = (53<<2) - 33 = 179.
    let mag_55 = i32::from(sample_ulaw(0x55)).abs() >> 2; // undo the <<2 scale
    assert_eq!(mag_55, 179, "mu-law 0x55 ITU 14-bit magnitude = 179");
    //   stored 0x00 -> wire 0xFF: exponent 7, mantissa 15 (segment endpoint)
    //     ((15<<1)|0x21)<<7 - 0x21 = (63<<7) - 33 = 8031.
    let mag_00 = i32::from(sample_ulaw(0x00)).abs() >> 2;
    assert_eq!(mag_00, 8031, "mu-law 0x00 ITU 14-bit magnitude = 8031");
    // Anchor a full mid-range decoded sample to the ITU reference, not just
    // the magnitude: the wire byte's sign bit is complemented before use, so
    // stored 0x55 (high bit clear) decodes negative; the 14-bit magnitude 179
    // scaled <<2 = -716.
    assert_eq!(
        sample_ulaw(0x55),
        -716,
        "mu-law 0x55 ITU reference decode = -716 (-(179 << 2))"
    );
}

#[test]
fn alaw_mid_segment_matches_itu_reference_magnitude() {
    // Cross-check mid-segment codes against the ITU-T G.711 reference A-law
    // decode (Rec. G.711 sec. 5 / Table 1): exponent 0 -> (mantissa<<1)|1;
    // exponents 1..7 -> ((mantissa<<1)|0x21) << (exponent-1), a 13-bit
    // magnitude. Independent of this impl's <<3 scale shift.
    //   stored 0xAB -> untoggled 0xFE: exponent 7, mantissa 14 ->
    //     ((14<<1)|0x21)<<6 = 61<<6 = 3904.
    let mag_ab = i32::from(sample_alaw(0xAB)).abs() >> 3; // undo the <<3 scale
    assert_eq!(mag_ab, 3904, "A-law 0xAB ITU 13-bit magnitude = 3904");
    //   stored 0xC5 -> untoggled 0x90: exponent 1, mantissa 0 ->
    //     ((0<<1)|0x21)<<0 = 33.
    let mag_c5 = i32::from(sample_alaw(0xC5)).abs() >> 3;
    assert_eq!(mag_c5, 33, "A-law 0xC5 ITU 13-bit magnitude = 33");
    // Anchor a full mid-range decoded sample to the ITU reference, not just
    // the magnitude: untoggled 0xFE has its sign bit set, so 0xAB decodes
    // positive; the 13-bit magnitude 3904 scaled <<3 = +31232.
    assert_eq!(
        sample_alaw(0xAB),
        31_232,
        "A-law 0xAB ITU reference decode = +31232 (3904 << 3)"
    );
}

#[test]
fn alaw_full_scale_reaches_near_full_magnitude() {
    // 0xAA / 0x2A are the largest-magnitude positive / negative codes.
    // 0xAA toggles to 0xFF (sign set, exp 7, mantissa 0x0F):
    // ((0x0F<<1)|0x21)<<6 = 4032 (13-bit magnitude), scaled <<3 = 32256,
    // near full-scale 16-bit. 0x2A toggles to 0x7F (sign clear) -> negative.
    let positive = sample_alaw(0xAA);
    let negative = sample_alaw(0x2A);
    assert_eq!(positive, 32_256, "A-law full-scale positive");
    assert_eq!(negative, -32_256, "A-law full-scale negative");
    assert!(positive > 30_000 && negative < -30_000, "near +/- full");
}

#[test]
fn push_frame_capped_reports_the_drop() {
    let mut ring = std::collections::VecDeque::new();
    for i in 0..RENDER_RING_CAP {
        assert!(
            !push_frame_capped(&mut ring, (i as i16, 0)),
            "no drop below the cap"
        );
    }
    // At the cap the oldest frame is evicted, and the caller is told.
    assert!(push_frame_capped(&mut ring, (0, 0)));
    assert_eq!(ring.len(), RENDER_RING_CAP);
}
