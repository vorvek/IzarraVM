// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn fractional_lod_keeps_logical_fraction_before_split_selection() {
    const LOD8_MAX: u32 = (8 * 4) << 6;

    let lod = select_lod(1.25, 1.0, 0, LOD8_MAX | LOD_SPLIT);

    assert_eq!(lod.floor, 1);
    assert_eq!(lod.fraction, 64);
    assert_eq!(lod.physical, 2);
}

/// The pre-hoist formula, kept only here as the reference for the
/// bit-identity check below. Computes `min`, `max` and `bias` from
/// `texture_lod` inline, at call time, the way `select_lod` did before
/// `TmuRaster::new` started deriving them once per triangle.
fn select_lod_unhoisted(
    base_lod: f64,
    reciprocal_w: f64,
    texture_mode: u32,
    texture_lod: u32,
) -> TextureLod {
    let min = f64::from(texture_lod & 0x3f) / 4.0;
    let max = (f64::from((texture_lod >> 6) & 0x3f) / 4.0).min(8.0);
    let min = min.min(8.0);
    let bias_raw = ((texture_lod >> 12) & 0x3f) as i32;
    let bias = f64::from(if bias_raw & 0x20 != 0 {
        bias_raw | !0x3f
    } else {
        bias_raw
    }) / 4.0;
    let perspective_adjust = if texture_mode & TEXTUREMODE_TPERSP_ST != 0 && reciprocal_w > 0.0 {
        reciprocal_w.log2()
    } else {
        0.0
    };
    let lod = (base_lod - perspective_adjust + bias).max(min).min(max);
    let fixed = ((lod * 256.0).floor() as u32).min(8 << 8);
    let floor = fixed >> 8;
    let physical = if owns_lod(texture_lod, floor) {
        floor
    } else {
        floor.saturating_add(1).min(8)
    };
    TextureLod {
        physical,
        floor,
        fraction: fixed as u8,
    }
}

#[test]
fn select_lod_hoist_is_bit_identical_to_the_unhoisted_formula() {
    // The hoist moves min/max/bias/perspective-flag out of the per-pixel
    // call and into `TmuRaster::new`, computed once per triangle instead
    // of once per pixel. It must not change a single bit of the result:
    // sweep register patterns and reciprocal_w/base_lod combinations and
    // compare against the formula reconstructed inline every call.
    let texture_lods: [u32; 6] = [
        0,
        0xffff_ffff,
        LOD_SPLIT | LOD_ODD,
        (8 * 4) | ((8 * 4) << 6) | (0x20 << 12),
        (3 * 4) | ((5 * 4) << 6) | (10 << 12),
        LOD_TMULTIBASEADDR | (1 << 6),
    ];
    let base_lods = [f64::NEG_INFINITY, -3.5, 0.0, 1.25, 8.0, 12.0];
    let reciprocal_ws = [-1.0, 0.0, 0.5, 1.0, 4.0];
    let texture_modes = [0u32, TEXTUREMODE_TPERSP_ST, TEXTUREMODE_TPERSP_ST | 0x40];

    for &texture_lod in &texture_lods {
        for &base_lod in &base_lods {
            for &reciprocal_w in &reciprocal_ws {
                for &texture_mode in &texture_modes {
                    let want =
                        select_lod_unhoisted(base_lod, reciprocal_w, texture_mode, texture_lod);
                    let got = select_lod(base_lod, reciprocal_w, texture_mode, texture_lod);
                    assert_eq!(
                        got, want,
                        "texture_lod={texture_lod:#010x} base_lod={base_lod} \
                         reciprocal_w={reciprocal_w} texture_mode={texture_mode:#x}"
                    );
                }
            }
        }
    }
}

#[test]
fn samples_masked_matches_the_unmasked_sample_on_the_needed_slot() {
    // The needed slot must read exactly what the pre-hoist `samples()`
    // (recreated here as need = [true, true]) read; the unneeded slot
    // must be the placeholder, never a real sample.
    //
    // TMU0 and TMU1 get distinct, non-zero S/T planes on purpose. A
    // `TextureIteratorState::default()` oracle makes every plane zero,
    // which makes every sample equal `TextureSample::UNUSED` -- the
    // assertions below would then compare the placeholder to itself and
    // pass no matter what `samples_masked` actually indexed. The
    // `assert_ne!` guards make that failure mode impossible to reintroduce
    // silently: if a future edit collapses `full` back to the placeholder,
    // those guards fail before the rest of the test gets a chance to be
    // vacuous again.
    fn write_start(state: &mut TextureIteratorState, chip: usize, register: usize, value: u32) {
        for (byte, b) in value.to_le_bytes().into_iter().enumerate() {
            state.write_register(chip, register, byte, b);
        }
    }

    let mut state = TextureIteratorState::default();
    write_start(&mut state, CHIP_TREX0, SST_START_S, 1);
    write_start(&mut state, CHIP_TREX0, SST_START_T, 2);
    write_start(&mut state, CHIP_TREX1, SST_START_S, 3);
    write_start(&mut state, CHIP_TREX1, SST_START_T, 4);
    let raster = state.raster([0, 0], [0, 0], (0.0, 0.0));

    let full = raster.samples_masked(4.5, 4.5, [true, true]);
    assert_ne!(
        full[0],
        TextureSample::UNUSED,
        "TMU0's real sample must not coincide with the placeholder, or this test cannot fail"
    );
    assert_ne!(
        full[1],
        TextureSample::UNUSED,
        "TMU1's real sample must not coincide with the placeholder, or this test cannot fail"
    );
    assert_ne!(
        full[0], full[1],
        "TMU0 and TMU1 must sample differently, or a swapped index would be invisible"
    );

    let tmu0_only = raster.samples_masked(4.5, 4.5, [true, false]);
    assert_eq!(tmu0_only[0], full[0], "needed TMU0 slot must match");
    assert_eq!(
        tmu0_only[1],
        TextureSample::UNUSED,
        "unneeded TMU1 slot must be the placeholder"
    );

    let tmu1_only = raster.samples_masked(4.5, 4.5, [false, true]);
    assert_eq!(tmu1_only[1], full[1], "needed TMU1 slot must match");
    assert_eq!(
        tmu1_only[0],
        TextureSample::UNUSED,
        "unneeded TMU0 slot must be the placeholder"
    );

    let neither = raster.samples_masked(4.5, 4.5, [false, false]);
    assert_eq!(neither, [TextureSample::UNUSED; 2]);
}

#[test]
fn tiny_mips_reserve_at_least_four_texels() {
    const ASPECT_8_TO_1: u32 = 3 << 21;
    let lod = LOD_S_IS_WIDER | ASPECT_8_TO_1;

    let lod7 = texture_mip_offset(lod, 7, 2);
    let lod8 = texture_mip_offset(lod, 8, 2);

    assert_eq!(lod8 - lod7, 4 * 2);
}
