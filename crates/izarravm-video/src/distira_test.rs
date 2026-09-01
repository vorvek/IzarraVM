// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn model_always_reports_two_tmus() {
    assert_eq!(Distira::new().tmu_count(), 2);
}

/// Reproduces the latent aliasing defect recorded in
/// `dev_docs/2026-09-01-tr-mipmap-diag.md` section 5: Distira advertises a
/// dual-TMU board (`DISTIRA_TMU_CONFIG` sets both TMU-count bits, matching
/// DOSBox-X's `VOODOO_1_DTMU`), and a real dual-TMU Voodoo 1 board carries
/// 4 MB of texture RAM per TMU. `write_texture_u32` and every texture fetch
/// mask the aperture/fetch offset with `DISTIRA_TEX_SIZE - 1`. While that
/// constant stays at 2 MB, an upload whose address lands one 2 MB stride
/// above a live texel silently wraps and clobbers it -- Tomb Raider's own
/// uploads reach 0x40255c (4.01 MB) and wrap 2,396 times in the diagnosed
/// session.
///
/// This test writes a distinctive byte at TMU0 offset 0 (a "low" texel a
/// triangle has already sampled), then writes a different byte at the
/// address exactly the single-TMU board's 2 MB budget above it (a "high"
/// upload -- the stride at which Tomb Raider's own uploads, which reach
/// 0x40255c on the real fixture, wrap against a 2 MB-per-TMU allocation).
/// It reads the low texel back both as raw storage and through the real
/// sampling path (`sample_tmu_u8`, s=0 t=0 lod=0 -- the same coordinates
/// the low write targeted). On a 2 MB-per-TMU board the high write aliases
/// onto the low address and clobbers it (RED here, on `main`). Once
/// `DISTIRA_TEX_SIZE` matches the advertised 4 MB per TMU, 2 MB above the
/// low texel is memory the low texel never occupies, so both survive
/// (GREEN).
#[test]
fn high_texture_upload_does_not_alias_a_low_texel_within_tmu_budget() {
    let mut distira = Distira::new();

    // aperture_offset = 0 decodes (see `texture_write_offset`) to tmu=0,
    // lod=0, s=0, t=0 with the default (all-zero) texture_mode/lod
    // registers, and tex_base_addr_for_tmu_lod(0, 0) reads back
    // `self.tex_base_addr` directly (shifted left 3), so a base-address
    // register of 0 puts this write at raw offset 0.
    distira.tex_base_addr = 0;
    distira.write_texture_u32(0, 0x11_11_11_11);

    // Point the same TMU's base address exactly 2 MB (a single-TMU board's
    // whole texture budget) higher and repeat the same all-zero aperture
    // write. This is a fixed literal, not `DISTIRA_TEX_SIZE`: the point is
    // that a specific, real upload distance aliases under the old 2 MB
    // mask and does not under the fixed 4 MB one, not that the stride
    // tracks whatever the constant currently says.
    const SINGLE_TMU_BUDGET: u32 = 2 * 1024 * 1024;
    assert_eq!(
        SINGLE_TMU_BUDGET & 0x7,
        0,
        "stride must be a multiple of 8 to round-trip through the base-address register's <<3 shift"
    );
    distira.tex_base_addr = SINGLE_TMU_BUDGET >> 3;
    distira.write_texture_u32(0, 0x22_22_22_22);

    // Read the low texel back both ways: raw storage, and through the real
    // fetch path at the coordinates the low write targeted.
    distira.tex_base_addr = 0;
    let raw_low = distira.texture[0][0];
    let sampled_low = distira.raster_view().sample_tmu_u8(
        0,
        TextureSample {
            s: 0.0,
            t: 0.0,
            lod: 0,
            lod_floor: 0,
            lod_fraction: 0,
        },
    );

    assert_eq!(
        raw_low, 0x11,
        "a texture upload {SINGLE_TMU_BUDGET:#x} bytes above a live texel must not alias it \
         within a single TMU's advertised {DISTIRA_TEX_SIZE:#x}-byte budget"
    );
    assert_eq!(
        sampled_low, 0x11,
        "the real sampling path must read back what was uploaded, not a high-address \
         upload that wrapped onto this texel's storage"
    );
}
