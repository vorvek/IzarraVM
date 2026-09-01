// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The one function this whole feature turns on. See
//! `dev_docs/2026-09-01-display-gamma-design.md` for the derivation.
//!
//! Every video path in IzarraVM hands the presenter a byte that means DAC
//! output level -- linear in analog video voltage. The host panel instead
//! treats that byte as an sRGB code, which lifts near-blacks (the sRGB toe).
//! `display_transform` corrects it: decode with the period CRT's assumed EOTF
//! to get the light the tube actually emitted, then re-encode with the exact
//! sRGB OETF so the host panel emits that same light.
//!
//! `gamma == None` ("Raw") is the identity, byte for byte -- today's
//! behaviour before this existed.

/// Exact sRGB OETF (IEC 61966-2-1), the inverse of the sRGB EOTF in the
/// design's `L_srgb`. `l` is normalised light in [0, 1] (values outside that
/// range are not expected here; the caller always feeds a `pow` result of a
/// normalised byte).
fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// CRT EOTF -> sRGB OETF, at present time. `p_out = 255 * srgb_oetf((p/255)^gamma)`.
///
/// `gamma == None` is the identity: the guest byte reaches the panel as an
/// sRGB code unchanged, which is what every path did before this function
/// existed. This makes "Raw" a first-class mode rather than a special case
/// threaded through by hand at every call site.
///
/// Not yet called from anywhere in this slice; the pref and shader plumbing
/// that call it land in later slices of the same design. `allow(dead_code)`
/// comes off once a caller exists.
#[allow(dead_code)]
pub fn display_transform(code: u8, gamma: Option<f32>) -> u8 {
    let Some(gamma) = gamma else {
        return code;
    };
    let p = f32::from(code) / 255.0;
    let light = p.powf(gamma);
    let out = 255.0 * srgb_oetf(light);
    out.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
#[path = "display_transform_test.rs"]
mod tests;
