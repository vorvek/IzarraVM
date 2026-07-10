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

#[test]
fn tiny_mips_reserve_at_least_four_texels() {
    const ASPECT_8_TO_1: u32 = 3 << 21;
    let lod = LOD_S_IS_WIDER | ASPECT_8_TO_1;

    let lod7 = texture_mip_offset(lod, 7, 2);
    let lod8 = texture_mip_offset(lod, 8, 2);

    assert_eq!(lod8 - lod7, 4 * 2);
}
