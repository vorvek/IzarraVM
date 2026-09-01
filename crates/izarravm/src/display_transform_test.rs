// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// `gamma == None` must be the identity for every possible byte.
#[test]
fn identity_when_gamma_is_none() {
    for code in 0u8..=255 {
        assert_eq!(display_transform(code, None), code);
    }
}

/// Exact bytes from the design doc's anchor table
/// (`dev_docs/2026-09-01-display-gamma-design.md` section 2.3), gamma 2.4 --
/// the default. Independently recomputed and confirmed to match before being
/// pinned here.
#[test]
fn golden_ramp_gamma_2_4() {
    let anchors: [(u8, u8); 12] = [
        (0, 0),
        (8, 1),
        (16, 4),
        (32, 20),
        (48, 37),
        (64, 53),
        (96, 87),
        (128, 121),
        (160, 155),
        (192, 189),
        (224, 222),
        (255, 255),
    ];
    for (code, expected) in anchors {
        assert_eq!(
            display_transform(code, Some(2.4)),
            expected,
            "code {code} at gamma 2.4"
        );
    }
}

/// Same anchor table, gamma 2.2.
#[test]
fn golden_ramp_gamma_2_2() {
    let anchors: [(u8, u8); 8] = [
        (8, 2),
        (16, 7),
        (32, 26),
        (64, 62),
        (96, 96),
        (128, 129),
        (192, 193),
        (255, 255),
    ];
    for (code, expected) in anchors {
        assert_eq!(
            display_transform(code, Some(2.2)),
            expected,
            "code {code} at gamma 2.2"
        );
    }
}

/// Same anchor table, gamma 2.5.
#[test]
fn golden_ramp_gamma_2_5() {
    let anchors: [(u8, u8); 9] = [
        (8, 1),
        (16, 3),
        (32, 17),
        (48, 33),
        (64, 50),
        (96, 83),
        (128, 117),
        (192, 186),
        (224, 221),
    ];
    for (code, expected) in anchors {
        assert_eq!(
            display_transform(code, Some(2.5)),
            expected,
            "code {code} at gamma 2.5"
        );
    }
}

/// 0 and 255 are fixed points for every gamma in the supported band.
#[test]
fn endpoints_are_fixed() {
    for gamma in [1.8, 2.2, 2.4, 2.5, 3.0] {
        assert_eq!(display_transform(0, Some(gamma)), 0, "gamma {gamma}");
        assert_eq!(display_transform(255, Some(gamma)), 255, "gamma {gamma}");
    }
}

/// The transform must never darken a brighter input code below a dimmer one's
/// output, for every gamma in the supported band.
#[test]
fn monotonic_over_all_codes() {
    for gamma in [1.8, 2.2, 2.4, 2.5, 3.0] {
        let mut previous = 0u8;
        for code in 0u8..=255 {
            let out = display_transform(code, Some(gamma));
            assert!(
                out >= previous,
                "gamma {gamma}: code {code} produced {out}, which is less than the previous code's {previous}"
            );
            previous = out;
        }
    }
}

/// `srgb_oetf` inverts the sRGB EOTF (`L_srgb` in the design) to within
/// floating-point tolerance, checked on both sides of both piecewise knees
/// (0.0031308 in the OETF's own domain, 0.04045 in the EOTF's).
#[test]
fn srgb_oetf_inverts_the_eotf() {
    fn srgb_eotf(v: f32) -> f32 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    let mut v = 0.0f32;
    while v <= 1.0 {
        let light = srgb_eotf(v);
        let back = srgb_oetf(light);
        assert!(
            (back - v).abs() < 1e-4,
            "round trip failed at v={v}: eotf={light}, oetf(eotf)={back}"
        );
        v += 1.0 / 4096.0;
    }
    // Explicit knee-straddling samples.
    for v in [0.0031307_f32, 0.0031309, 0.04044, 0.04046, 1.0] {
        let back = srgb_oetf(srgb_eotf(v));
        assert!((back - v).abs() < 1e-4, "round trip failed at v={v}");
    }
}
