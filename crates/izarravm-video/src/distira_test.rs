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

fn write_reg(distira: &mut Distira, reg: usize, value: u32) {
    for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
        distira.write_mmio_u8(reg + index, byte);
    }
}

/// Queue one small, non-degenerate, non-textured triangle through the guest
/// register path (`SST_TRIANGLE_CMD`) -- the same path `run_triangle_command`
/// drives -- so it defers onto `raster_queue` instead of drawing at
/// submission (`draw_triangle`, by contrast, always draws immediately: see
/// its `defer: false` argument to `draw_triangle_inner`).
fn queue_triangle(distira: &mut Distira, red: u32) {
    write_reg(
        distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_DRAW_FRONT,
    );
    write_reg(distira, SST_VERTEX_AX, 0 << 4);
    write_reg(distira, SST_VERTEX_AY, 0 << 4);
    write_reg(distira, SST_VERTEX_BX, 8 << 4);
    write_reg(distira, SST_VERTEX_BY, 0 << 4);
    write_reg(distira, SST_VERTEX_CX, 0 << 4);
    write_reg(distira, SST_VERTEX_CY, 8 << 4);
    write_reg(distira, SST_START_R, red << 12);
    write_reg(distira, SST_START_G, 0);
    write_reg(distira, SST_START_B, 0);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
}

/// Slice 1 of `dev_docs/2026-09-05-distira-async-overlap-review.md`: a
/// batch flushed at the guest's `swapbufferCMD` write (`write_mmio_u8`'s
/// `SST_SWAPBUFFER_CMD` arm calls `flush_raster_queue` alone, never
/// `drain_raster_queue` -- see `dev_docs/2026-09-05-distira-async-overlap-design.md`
/// section 2, item 2) is left running on the raster pool, and the next call
/// that could observe its pixels -- here, an LFB read -- joins it first.
/// The result must be byte-identical to the fully synchronous
/// (queue-disabled) path: async changes WHEN the batch runs, never WHAT it
/// computes.
#[test]
fn a_batch_flushed_at_swap_is_joined_by_the_next_lfb_read_with_identical_pixels_to_the_synchronous_path()
 {
    // The synchronous reference: the queue off, so every triangle draws at
    // submission -- the exact pre-async (and pre-#840) behaviour.
    let mut sync = Distira::new();
    sync.set_frame_size(8, 8);
    sync.set_raster_queue_enabled(false);
    queue_triangle(&mut sync, 0xff);
    write_reg(&mut sync, SST_SWAPBUFFER_CMD, 0);
    write_reg(&mut sync, SST_LFB_MODE, LFB_READ_FRONT);
    let sync_pixels: Vec<u16> = (0..8 * 8).map(|i| sync.read_lfb_u16(i * 2)).collect();

    // The async path: the queue on (the default), so the triangle defers.
    let mut asynchronous = Distira::new();
    asynchronous.set_frame_size(8, 8);
    queue_triangle(&mut asynchronous, 0xff);
    write_reg(&mut asynchronous, SST_SWAPBUFFER_CMD, 0);
    assert!(
        asynchronous.in_flight.is_some(),
        "the swap must flush the queued triangle to the raster pool and \
         return without joining"
    );
    write_reg(&mut asynchronous, SST_LFB_MODE, LFB_READ_FRONT);
    let async_pixels: Vec<u16> = (0..8 * 8)
        .map(|i| asynchronous.read_lfb_u16(i * 2))
        .collect();
    assert!(
        asynchronous.in_flight.is_none(),
        "the LFB read must have joined the batch the swap left in flight"
    );

    assert_eq!(
        sync_pixels, async_pixels,
        "a batch flushed at the swap and joined at the next LFB read must \
         produce the same pixels as the fully synchronous path"
    );
    assert!(
        sync_pixels.iter().any(|&pixel| pixel != 0),
        "the scene must actually paint something, or this test proves nothing"
    );
}

/// Slice 1: `raster_owned` moves to the raster pool for the whole time a
/// batch is in flight, not just for the duration of one method call the
/// way the fully synchronous model (slice 0b) held it. It comes back the
/// instant `join_raster` folds the batch in, whichever call triggered
/// that join.
#[test]
fn raster_owned_is_absent_exactly_while_a_batch_is_in_flight_and_present_after_every_join() {
    let mut distira = Distira::new();
    distira.set_frame_size(8, 8);
    queue_triangle(&mut distira, 0xff);
    assert_eq!(distira.raster_queue_depth(), 1, "the triangle must defer");
    assert!(
        distira.raster_owned.is_some(),
        "the box is untouched before anything flushes the queue"
    );

    distira.flush_raster_queue(DrainCause::Config);
    assert!(
        distira.in_flight.is_some(),
        "a non-empty queue must produce an in-flight batch"
    );
    assert!(
        distira.raster_owned.is_none(),
        "the box belongs to the worker for the whole time the batch is in \
         flight, not just for the duration of the flush call"
    );

    let written = distira.join_raster();
    assert!(
        written.is_some(),
        "the join must actually wait on the batch it just flushed"
    );
    assert!(distira.in_flight.is_none(), "the join clears in_flight");
    assert!(
        distira.raster_owned.is_some(),
        "the box comes back the moment the batch is joined"
    );

    // A join with nothing in flight is free and reports no wait.
    assert!(
        distira.join_raster().is_none(),
        "a join with nothing outstanding must not fabricate a wait"
    );
}

/// Slice 1, depth one, deliberately
/// (`dev_docs/2026-09-05-distira-async-overlap-design.md` section 2): at
/// most one batch is ever in flight. A second `flush_raster_queue` call
/// while one is still outstanding must join it FIRST, and that forced join
/// is attributed to whichever cause forced it -- the caller that pays for
/// the wait, not the flush that originally started the batch it waited on.
#[test]
fn a_second_flush_while_one_is_in_flight_joins_it_first() {
    let mut distira = Distira::new();
    distira.set_frame_size(8, 8);

    queue_triangle(&mut distira, 0xff);
    distira.flush_raster_queue(DrainCause::Config);
    assert!(
        distira.in_flight.is_some(),
        "the first flush must leave a batch in flight"
    );
    assert_eq!(
        distira.triangle_census.joins_by_cause.swap_or_scanout, 0,
        "nothing has forced a join yet"
    );

    queue_triangle(&mut distira, 0x80);
    distira.flush_raster_queue(DrainCause::SwapOrScanout);
    assert_eq!(
        distira.triangle_census.joins_by_cause.swap_or_scanout, 1,
        "the depth-one pre-join inside the second flush must be attributed \
         to the cause that forced it, not to the first flush's own cause"
    );
    assert!(
        distira.in_flight.is_some(),
        "the second flush's own batch is now the one in flight"
    );

    // Never more than one batch outstanding: joining once must be enough
    // to observe both triangles' pixels.
    let written = distira.join_raster();
    assert!(
        written.is_some(),
        "the second batch must still be waitable after the pre-join \
         consumed the first"
    );
    assert!(distira.in_flight.is_none());
}
