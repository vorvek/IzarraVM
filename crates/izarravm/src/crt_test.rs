// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{SHADER, pack_argb_rows, uniform_bytes, upload_is_full};

/// Parse and validate the WGSL through naga so a shader error fails the test
/// suite instead of panicking at pipeline creation when the GUI launches.
/// Catches the easy-to-trip cases: textureSample outside uniform control flow,
/// type mismatches, and uniform-buffer layout errors.
#[test]
fn shader_compiles_under_naga() {
    let module = wgpu::naga::front::wgsl::parse_str(SHADER)
        .unwrap_or_else(|e| panic!("WGSL parse error: {e}"));
    let mut validator = wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("WGSL validation error: {e}"));
}

/// The WGSL `srgb_oetf` helper (the display-gamma correction's re-encode
/// half) must use `display_transform.rs`'s exact constants IN THE OPERAND
/// POSITIONS its own `srgb_oetf` uses them, not merely contain the same five
/// numbers somewhere in the function body.
///
/// A bare "does this text contain the number 2.4" check cannot catch the
/// single most damaging drift this test exists to prevent: swapping the
/// OETF's reciprocal exponent `pow(l, 1.0 / gamma)` for the EOTF's bare
/// `pow(l, gamma)`, which silently turns the re-encode step into a second
/// decode. `2.4` remains present in the text either way; the reciprocal
/// `1.0 / 2.4` does not survive that mutation, so this test pins that exact
/// substring rather than the number alone.
#[test]
fn shader_srgb_oetf_constants_mirror_display_transform() {
    use crate::display_transform::{
        SRGB_A, SRGB_B, SRGB_GAMMA, SRGB_LOW_SLOPE, SRGB_LOW_THRESHOLD,
    };

    let start = SHADER
        .find("fn srgb_oetf")
        .expect("the srgb_oetf WGSL helper must exist");
    let end = SHADER[start..]
        .find("@fragment")
        .map(|offset| start + offset)
        .unwrap_or(SHADER.len());
    let body = &SHADER[start..end];

    let checks = [
        format!("l * {SRGB_LOW_SLOPE}"),
        format!("1.0 / {SRGB_GAMMA}"),
        format!("{SRGB_A} * pow"),
        format!("vec3<f32>({SRGB_B})"),
        format!("vec3<f32>({SRGB_LOW_THRESHOLD})"),
    ];
    for expected in checks {
        assert!(
            body.contains(&expected),
            "srgb_oetf must contain `{expected}`, matching display_transform.rs's operand \
             positions exactly; got:\n{body}"
        );
    }
}

/// The uniform block must stay 32 bytes (std140-safe as 8 f32s) with
/// `monitor_gamma` at the offset the WGSL struct `U` gives it: after
/// `src_size.xy, style, srgb, time` (16 bytes), so byte offset 20.
#[test]
fn uniform_block_is_32_bytes_with_monitor_gamma_at_offset_20() {
    let bytes = uniform_bytes(320.0, 200.0, 1.0, true, 2.5, 2.4, 1.5);
    assert_eq!(bytes.len(), 32);
    let gamma_bytes: [u8; 4] = bytes[20..24].try_into().unwrap();
    assert_eq!(f32::from_le_bytes(gamma_bytes), 2.4);
}

/// The Glide compensation takes the block's first pad slot, so the uniform
/// stays 32 bytes: after `src_size.xy, style, srgb, time, monitor_gamma`
/// (24 bytes), so byte offset 24.
#[test]
fn glide_gamma_lands_at_uniform_offset_24_without_growing_the_block() {
    let bytes = uniform_bytes(320.0, 200.0, 0.0, false, 0.0, 2.4, 1.5);
    assert_eq!(bytes.len(), 32);
    let glide_bytes: [u8; 4] = bytes[24..28].try_into().unwrap();
    assert_eq!(f32::from_le_bytes(glide_bytes), 1.5);
    assert_eq!(&bytes[28..32], &[0u8; 4], "the last pad slot stays zero");
}

/// The shader's compensation exponent must be the one `display_transform.rs`
/// defines, and it must be applied through an explicit inequality branch:
/// `pow(c, 1.0)` is not guaranteed bit-exact, and "Original" has to be
/// provably today's picture.
#[test]
fn shader_applies_the_glide_compensation_before_the_style_branch() {
    let compensation = SHADER
        .find("u.glide_gamma != 1.0")
        .expect("the shader must branch on the Original sentinel rather than pow(c, 1.0)");
    let style_branch = SHADER
        .find("if (u.style > 0.5)")
        .expect("the CRT effect block must still be there");
    assert!(
        compensation < style_branch,
        "the compensation is a signal-domain edit and must run before the display model"
    );
    assert!(
        SHADER.contains("pow(col, vec3<f32>(u.glide_gamma))"),
        "the shader must raise the sampled code to the compensation exponent"
    );
}

/// Packing one run produces exactly that run's bytes, in upload order: the
/// scratch buffer holds the rows being uploaded and nothing else, because the
/// texture write places it at the run's own origin.
#[test]
fn row_packing_emits_only_the_named_run() {
    let words = [0x00ff_0000, 0x0000_ff00, 0x00ab_cdef, 0x0012_3456, 1, 2];
    let mut rgba = vec![0x5au8; 32];

    pack_argb_rows(&words, 2, 1..2, &mut rgba);

    assert_eq!(rgba, [0xab, 0xcd, 0xef, 0xff, 0x12, 0x34, 0x56, 0xff]);
}

/// A frame whose damage does not continue from the last one the texture took
/// must be uploaded whole.
///
/// Nothing acknowledges a paint, and several ordinary things drop a polled
/// frame before it reaches the GPU: egui discards a sizing pass, the monitor
/// stops painting while the machine is off. The runs in the frame after a drop
/// describe only ITS changes, so applying them would leave the dropped frame's
/// rows stale with no later frame to repair them.
#[test]
fn a_frame_published_after_a_dropped_one_is_uploaded_whole() {
    // Publications 2 and 3 published, only 3 reaches the texture.
    assert!(
        upload_is_full(3, 1, false),
        "a gap in the publication chain must force a full upload"
    );
    // Publication 1 applied; 2 follows it directly.
    assert!(!upload_is_full(2, 1, false));
    // Nothing applied yet, and publications start at 1.
    assert!(
        upload_is_full(1, u64::MAX, false),
        "a texture that has applied nothing cannot take a delta"
    );
    // A recreated texture holds no rows at all, however the chain reads.
    assert!(upload_is_full(2, 1, true));
}
