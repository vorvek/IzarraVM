// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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

    // Both writes ride the raster queue now (slice 1 of the texture-queue
    // lever, `dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section
    // 6): the STORE into texture memory is deferred to `drain_raster_queue`,
    // so a direct read of `distira.texture` must drain first, the same as
    // any real consumer (an LFB access, a scanout, a statistics read) would.
    distira.drain_raster_queue(DrainCause::Config);

    // Read the low texel back both ways: raw storage, and through the real
    // fetch path at the coordinates the low write targeted.
    distira.tex_base_addr = 0;
    let raw_low = distira.raster_owned.as_ref().unwrap().texture[0][0];
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

/// THE WRAP-VS-DROP DIVERGENCE
/// (`dev_docs/2026-09-01-tex4mb-review.md` section 8): before this fix, the
/// FBI pixel-offset path (`RasterView::framebuffer_pixel_offset`) dropped
/// any pixel whose computed address landed at or past `DISTIRA_FB_SIZE`
/// instead of wrapping, unlike the texture-memory path (which has always
/// masked with `& (DISTIRA_TEX_SIZE - 1)`) and unlike 86Box's own FBI write
/// path, which indexes every store as `fb_mem[write_addr & fb_mask]` with
/// no bounds check first (`vid_voodoo_fb.c`, `voodoo_fb_writew`/`writel`).
/// The FBI address decode has no more address lines than it has RAM to
/// back them, so a bit above that boundary is simply not routed -- a
/// modulo-2^n wrap, not an aborted write.
///
/// This test writes one pixel through the real LFB write path
/// (`write_lfb_u16`, the same call a guest's pixel-pipeline write goes
/// through) at raw byte offset 0, then a second, different pixel exactly
/// `DISTIRA_FB_SIZE` bytes above it -- a real row address the FBI's own
/// row/tile math can reach at a large enough configured pitch (the
/// machine-level `glide_destructive_framebuffer_probe_reports_four_megabytes`
/// fixture reaches an address in this shape through `SST_FBI_INIT2`'s
/// buffer-offset field). RED against the pre-fix drop behaviour: the high
/// write would fall outside `DISTIRA_FB_SIZE` and vanish, leaving the low
/// pixel unclobbered. GREEN under wrap: the high write lands back on
/// offset 0 and clobbers it, exactly as address-line truncation would on
/// real hardware.
#[test]
fn lfb_write_above_the_fbi_limit_wraps_instead_of_dropping() {
    let mut distira = Distira::new();
    distira.display.front_base = 0;
    // A pitch equal to the whole FBI size makes one aperture row exactly
    // `DISTIRA_FB_SIZE` bytes, so aperture y=1 is the write under test.
    distira.display.pitch = DISTIRA_FB_SIZE as u32;

    // aperture_offset 0 decodes to position (x=0, y=0): raw byte offset 0.
    distira.write_lfb_u16(0, 0x1111);
    assert_eq!(distira.read_lfb_u16(0), 0x1111);

    // aperture_offset with bit 11 set decodes to position (x=0, y=1): raw
    // byte offset `front_base + 1 * pitch` = `DISTIRA_FB_SIZE` exactly --
    // one whole framebuffer above the low write.
    distira.write_lfb_u16(1 << 11, 0x2222);

    assert_eq!(
        distira.read_lfb_u16(0),
        0x2222,
        "a write DISTIRA_FB_SIZE bytes above offset 0 must wrap and land on offset 0, \
         not vanish"
    );
}

/// The graduated lane split (`dev_docs/2026-09-05-tombraid-glide-foyer-profile.md`
/// section 5, lever B): raising the cap must not force a small batch out to
/// every lane the cap allows. `lanes_for_rows` grants a batch only as many
/// lanes as its own row span is worth (`rows / MIN_ROWS_PER_LANE`, capped at
/// `cap`), so a ten-row batch gets the same 2 lanes whether the cap is 4, 8,
/// or 16, while a 400- or 4000-row batch (typical post-#840, where triangles
/// per drain went 6.6 -> 395) always fills the whole cap.
#[test]
fn lane_split_is_graduated_by_row_span_not_forced_to_the_cap() {
    let cases: &[(usize, usize, usize)] = &[
        // (rows, cap, expected lanes)
        (1, 4, 1),
        (1, 8, 1),
        (1, 16, 1),
        (10, 4, 2),
        (10, 8, 2),
        (10, 16, 2),
        (400, 4, 4),
        (400, 8, 8),
        (400, 16, 16),
        (4000, 4, 4),
        (4000, 8, 8),
        (4000, 16, 16),
    ];
    for &(rows, cap, expected) in cases {
        assert_eq!(
            lanes_for_rows(rows, cap),
            expected,
            "rows={rows} cap={cap}: a batch's own row span decides its lane \
             count, not the cap alone"
        );
    }
}

/// Slice 0b of `dev_docs/2026-09-05-distira-async-overlap-review.md`
/// section 3: `raster_owned` is `None` only inside `drain_raster_queue`'s own
/// batch, and nothing outside that method ever re-enters while the box is
/// out -- there is no async yet to make that observable directly, so this
/// pins the invariant at every accessor's boundary instead: after ANY call
/// that could plausibly take the box (a queued texture write, a drain, the
/// `&mut self` census accessors this slice added the door to), the box is
/// back. If a future edit ever left it out (forgot to hand it back, or
/// panicked mid-batch and unwound past the restore), every one of these
/// would start panicking on the very next call via `raster_owned_mut`'s
/// `expect`, which is the point: the invariant is enforced at the door, not
/// asserted once and hoped for.
#[test]
fn raster_owned_is_present_after_every_call_that_could_take_it() {
    let mut distira = Distira::new();
    assert!(
        distira.raster_owned.is_some(),
        "a fresh Distira starts with the box in place"
    );

    // A queued texture write: `write_texture_u32` pushes a `TextureWrite`
    // command and may itself force a drain if the queue is full -- both
    // paths touch `raster_owned` (the queue-off store goes through
    // `raster_owned_mut` directly; a forced drain takes the box out for the
    // batch and hands it back before returning).
    distira.write_texture_u32(0, 0x1234_5678);
    assert!(
        distira.raster_owned.is_some(),
        "a queued texture write leaves the box in place"
    );

    // An explicit drain with no triangles queued: the empty-queue early
    // return (slice 0) never touches `raster_owned` at all.
    distira.drain_raster_queue(DrainCause::Config);
    assert!(
        distira.raster_owned.is_some(),
        "an empty-queue drain leaves the box in place"
    );

    // The queue-off synchronous store path in `write_texture_u32`, which
    // writes through `raster_owned_mut` directly with no queue involved.
    distira.set_raster_queue_enabled(false);
    distira.write_texture_u32(0, 0x9abc_def0);
    assert!(
        distira.raster_owned.is_some(),
        "the queue-off texture store leaves the box in place"
    );
    distira.set_raster_queue_enabled(true);

    // The four `&mut self` census accessors slice 0b routed through the
    // joining door (`Self::join_raster`).
    let _ = distira.census();
    let _ = distira.register_write_histogram();
    let _ = distira.register_read_histogram();
    let _ = distira.aperture_traffic();
    assert!(
        distira.raster_owned.is_some(),
        "the census accessors leave the box in place"
    );

    // A register write that goes through the NCC table, which now lives
    // inside `RasterOwned` too.
    distira.write_mmio_u8(SST_TEXTURE_MODE, 0);
    assert!(
        distira.raster_owned.is_some(),
        "an NCC-routed register write leaves the box in place"
    );
}
