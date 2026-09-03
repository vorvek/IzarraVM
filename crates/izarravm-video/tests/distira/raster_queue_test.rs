// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Everything the guest could observe while the scene below runs. The
/// asynchronous triangle queue is a scheduling change and nothing else, so
/// every one of these answers has to come out the same whether triangles
/// rasterise at submission or at the next drain point.
#[derive(Debug, PartialEq, Eq)]
struct QueueTrace {
    /// Register and LFB reads taken BETWEEN triangles, in order.
    readings: Vec<(&'static str, u32)>,
    frame: Vec<u32>,
    depth: Vec<u16>,
    census: Vec<(&'static str, u64)>,
}

const SCENE_WIDTH: usize = 256;
const SCENE_HEIGHT: usize = 128;

fn texture_upload(distira: &mut Distira, seed: u32) {
    // A small RGB565 texture at texture base 0. Uploaded through the aperture
    // the way a Glide driver does, which is one of the drain points.
    for texel in 0..64u32 {
        distira.write_texture_u32(
            (texel as usize) * 4,
            texel
                .wrapping_mul(0x0041_0041)
                .wrapping_add(seed.wrapping_mul(0x1234_5678)),
        );
    }
}

fn submit_triangle(
    distira: &mut Distira,
    vertices: [(u32, u32); 3],
    color: (u32, u32, u32),
    start_w: u32,
) {
    write_reg(distira, SST_VERTEX_AX, vertices[0].0 << 4);
    write_reg(distira, SST_VERTEX_AY, vertices[0].1 << 4);
    write_reg(distira, SST_VERTEX_BX, vertices[1].0 << 4);
    write_reg(distira, SST_VERTEX_BY, vertices[1].1 << 4);
    write_reg(distira, SST_VERTEX_CX, vertices[2].0 << 4);
    write_reg(distira, SST_VERTEX_CY, vertices[2].1 << 4);
    write_reg(distira, SST_START_R, color.0 << 12);
    write_reg(distira, SST_START_G, color.1 << 12);
    write_reg(distira, SST_START_B, color.2 << 12);
    write_reg(distira, SST_START_A, 0xc0 << 12);
    write_reg(distira, SST_DR_DX, 1 << 10);
    write_reg(distira, SST_DR_DY, 1 << 9);
    write_reg(distira, SST_START_W, start_w);
    write_reg(distira, SST_DW_DX, 3_942_646);
    write_reg(distira, SST_DW_DY, 1 << 18);
    write_reg(distira, SST_START_S, 0);
    write_reg(distira, SST_START_T, 0);
    write_reg(distira, SST_DS_DX, 1 << 16);
    write_reg(distira, SST_DT_DY, 1 << 16);
    write_reg(distira, SST_DS_DY, 0);
    write_reg(distira, SST_DT_DX, 0);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
}

/// A triangle with FLAT depth and no colour gradient, so its depth at every
/// pixel is exactly `start_w` and two of them can be ordered by depth alone.
fn submit_flat_triangle(
    distira: &mut Distira,
    vertices: [(u32, u32); 3],
    color: (u32, u32, u32),
    start_w: u32,
) {
    write_reg(distira, SST_VERTEX_AX, vertices[0].0 << 4);
    write_reg(distira, SST_VERTEX_AY, vertices[0].1 << 4);
    write_reg(distira, SST_VERTEX_BX, vertices[1].0 << 4);
    write_reg(distira, SST_VERTEX_BY, vertices[1].1 << 4);
    write_reg(distira, SST_VERTEX_CX, vertices[2].0 << 4);
    write_reg(distira, SST_VERTEX_CY, vertices[2].1 << 4);
    write_reg(distira, SST_START_R, color.0 << 12);
    write_reg(distira, SST_START_G, color.1 << 12);
    write_reg(distira, SST_START_B, color.2 << 12);
    write_reg(distira, SST_START_A, 0xc0 << 12);
    write_reg(distira, SST_DR_DX, 0);
    write_reg(distira, SST_DR_DY, 0);
    write_reg(distira, SST_START_W, start_w);
    write_reg(distira, SST_DW_DX, 0);
    write_reg(distira, SST_DW_DY, 0);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
}

fn read_counters(
    distira: &mut Distira,
    label: &'static str,
    readings: &mut Vec<(&'static str, u32)>,
) {
    let _ = label;
    readings.push(("fbi_pixels_in", read_reg(distira, SST_FBI_PIXELS_IN)));
    readings.push(("fbi_pixels_out", read_reg(distira, SST_FBI_PIXELS_OUT)));
    readings.push(("fbi_zfunc_fail", read_reg(distira, SST_FBI_ZFUNC_FAIL)));
    readings.push(("fbi_chroma_fail", read_reg(distira, SST_FBI_CHROMA_FAIL)));
    readings.push(("fbi_afunc_fail", read_reg(distira, SST_FBI_AFUNC_FAIL)));
    readings.push(("status", read_reg(distira, SST_STATUS)));
}

fn read_lfb_probe(distira: &mut Distira, readings: &mut Vec<(&'static str, u32)>) {
    // Sixteen pixels spread over the buffer, read through the LFB aperture the
    // way a game reads back what it just drew.
    for index in 0..16usize {
        let y = index * 7;
        let x = index * 13;
        readings.push(("lfb", distira.read_lfb_u32(y * 2048 + x * 2)));
    }
}

/// One scene, driven twice: once with the triangle queue on and once with it
/// forced off. Interleaves the reads that must drain it.
fn queue_stress_scene(queue_enabled: bool) -> QueueTrace {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(queue_enabled);
    distira.set_raster_lanes(4);
    distira.set_frame_size(SCENE_WIDTH as u32, SCENE_HEIGHT as u32);
    let mut readings = Vec::new();

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | FBZ_DITHER
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_BACK,
    );

    // Clear the back buffer, then paint over it. fastfillCMD is a drain point.
    write_reg(&mut distira, SST_COLOR1, 0x0020_4060);
    write_reg(&mut distira, SST_ZA_COLOR, 0xffff);
    write_reg(&mut distira, SST_FASTFILL_CMD, 1);

    texture_upload(&mut distira, 1);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEXTUREMODE_LOCAL | (TEX_R5G6B5 << 8),
    );
    write_reg(&mut distira, SST_TLOD, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );

    submit_triangle(
        &mut distira,
        [(0, 0), (256, 0), (0, 128)],
        (0x40, 0x80, 0xc0),
        10_737_418,
    );
    // A counter read right after a triangle must see that triangle's pixels.
    read_counters(&mut distira, "after-first", &mut readings);

    submit_triangle(
        &mut distira,
        [(32, 16), (224, 24), (64, 120)],
        (0xff, 0x20, 0x10),
        536_870_912,
    );
    // An LFB read right after a triangle must see that triangle's pixels.
    read_lfb_probe(&mut distira, &mut readings);

    // Two triangles with no read between them: the pair the queue actually
    // batches. Then a texture upload -- which now ORDERS against them
    // instead of draining them (the texture-queue lever,
    // `dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 6): the
    // upload's 64 writes join the same queue as the triangles ahead of it,
    // so all 66 entries wait together with the queue on.
    submit_triangle(
        &mut distira,
        [(0, 64), (256, 96), (16, 127)],
        (0x10, 0xf0, 0x30),
        200_000_000,
    );
    submit_triangle(
        &mut distira,
        [(200, 0), (255, 0), (128, 127)],
        (0x90, 0x90, 0x10),
        300_000_000,
    );
    // Without this the scene could pass every assertion below while the queue
    // silently drew each triangle at submission, which would test nothing.
    assert_eq!(
        distira.raster_queue_depth(),
        usize::from(queue_enabled) * 2,
        "two triangles with no read between them must be waiting together"
    );
    texture_upload(&mut distira, 2);
    assert_eq!(
        distira.raster_queue_depth(),
        usize::from(queue_enabled) * 66,
        "a texture upload must queue alongside triangles waiting ahead of it, \
         not drain them"
    );
    submit_triangle(
        &mut distira,
        [(0, 0), (255, 100), (0, 127)],
        (0x33, 0x66, 0x99),
        420_000_000,
    );
    read_counters(&mut distira, "after-texture", &mut readings);

    // An LFB WRITE between triangles: it touches the same buffer the queued
    // triangles paint, so it has to drain them first.
    for index in 0..32usize {
        distira.write_lfb_u32(index * 2048 + index * 4, 0xf81f_07e0);
    }
    submit_triangle(
        &mut distira,
        [(8, 8), (248, 40), (8, 120)],
        (0x11, 0x22, 0x33),
        480_000_000,
    );
    read_lfb_probe(&mut distira, &mut readings);

    // The rotating stipple chains from triangle to triangle. Its register is a
    // drain point, and the value the guest reads back afterwards is the state
    // the last queued triangle left.
    write_reg(&mut distira, SST_STIPPLE, 0xa5a5_1234);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | FBZ_STIPPLE
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    submit_triangle(
        &mut distira,
        [(0, 0), (200, 20), (20, 110)],
        (0x70, 0x10, 0x50),
        90_000_000,
    );
    submit_triangle(
        &mut distira,
        [(40, 4), (255, 30), (60, 100)],
        (0x20, 0xa0, 0x60),
        95_000_000,
    );
    readings.push(("stipple", read_reg(&mut distira, SST_STIPPLE)));

    // The PATTERNED stipple is a pure function of the pixel, so a triangle
    // that uses it waits on the queue like any other. The pattern the guest
    // writes while one is waiting is the pattern it must read back: the
    // batch carries its own copy and must not store it over this one.
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | FBZ_STIPPLE
            | FBZ_STIPPLE_PATT
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    submit_triangle(
        &mut distira,
        [(4, 2), (250, 18), (30, 126)],
        (0x80, 0x40, 0x20),
        97_000_000,
    );
    write_reg(&mut distira, SST_STIPPLE, 0x0f0f_5555);
    readings.push(("stipple_pattern", read_reg(&mut distira, SST_STIPPLE)));

    // nopCMD bit 0 clears the statistics registers. It must not race a
    // triangle that has not been rasterised yet.
    write_reg(&mut distira, SST_NOP_CMD, 1);
    submit_triangle(
        &mut distira,
        [(0, 0), (255, 0), (128, 127)],
        (0x55, 0x55, 0x55),
        99_000_000,
    );
    read_counters(&mut distira, "after-nop", &mut readings);

    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);
    let frame = distira.scanout_argb();

    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    let depth: Vec<u16> = (0..SCENE_HEIGHT)
        .flat_map(|y| (0..SCENE_WIDTH).map(move |x| (y, x)))
        .map(|(y, x)| distira.read_lfb_u16(y * 2048 + x * 2))
        .collect();

    let state = distira.scanout_state();
    let triangles = state.triangles;
    let census = vec![
        ("submitted", triangles.submitted),
        ("pixels_in", triangles.pixels_in),
        ("pixels_out", triangles.pixels_out),
        ("reject_depth", triangles.reject_depth),
        ("reject_stipple", triangles.reject_stipple),
        ("reject_chroma", triangles.reject_chroma),
        ("reject_alpha_test", triangles.reject_alpha_test),
        ("reject_rgb_wmask", triangles.reject_rgb_wmask),
        ("reject_offscreen", triangles.reject_offscreen),
        ("color_written", triangles.color_written),
        ("color_written_nonblack", triangles.color_written_nonblack),
        ("depth_written", triangles.depth_written),
        ("color_offset_min", u64::from(triangles.color_offset_min)),
        ("color_offset_max", u64::from(triangles.color_offset_max)),
        ("fastfill_pixels", state.fastfill_pixels),
        ("painted_bytes", state.painted_bytes as u64),
    ];

    QueueTrace {
        readings,
        frame,
        depth,
        census,
    }
}

#[test]
fn queued_triangles_answer_interleaved_reads_like_synchronous_ones() {
    let synchronous = queue_stress_scene(false);
    let queued = queue_stress_scene(true);

    assert_eq!(
        synchronous.readings, queued.readings,
        "an interleaved register or LFB read must see the same device state"
    );
    assert_eq!(synchronous.frame, queued.frame, "colour output must match");
    assert_eq!(synchronous.depth, queued.depth, "depth output must match");
    assert_eq!(synchronous.census, queued.census, "the census must match");

    // A scene that painted nothing, or that rejected nothing, would pass
    // every assertion above without proving anything.
    let count = |name: &str| {
        queued
            .census
            .iter()
            .find(|(field, _)| *field == name)
            .unwrap_or_else(|| panic!("the census carries {name}"))
            .1
    };
    assert!(
        count("color_written") > 50_000,
        "the scene must actually paint: {}",
        count("color_written")
    );
    assert!(
        count("color_written_nonblack") > 0,
        "the scene must paint something that is not black"
    );
    assert!(
        count("reject_depth") > 0 && count("reject_stipple") > 0,
        "the scene must exercise the per-pixel tests, not just fill"
    );
    assert!(
        queued.frame.iter().any(|&pixel| pixel != 0),
        "the scanned-out frame must not be blank"
    );
}

/// One batch, two triangles, opposite Y origins, overlapping framebuffer
/// rows.
///
/// `fbzMode` is a register the snapshot covers, so the guest can flip the Y
/// origin between two triangles without the queue being drawn in between and
/// both land in the same batch. Lanes own FRAMEBUFFER rows, and the flip
/// means the two triangles reach a given framebuffer row from opposite
/// triangle rows. Split the batch on the triangle's row instead and at two
/// lanes the flip always swaps parity, so two lanes meet on one framebuffer
/// row and race each other's read-modify-write in the depth test and the
/// blend.
fn mixed_y_origin_scene(queue_enabled: bool, lanes: usize) -> (Vec<u32>, Vec<u16>, u64) {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(queue_enabled);
    distira.set_raster_lanes(lanes);
    distira.set_frame_size(SCENE_WIDTH as u32, SCENE_HEIGHT as u32);

    let depth_mode = FBZ_RGB_WMASK
        | FBZ_DRAW_BACK
        | FBZ_DEPTH_ENABLE
        | FBZ_DEPTH_WMASK
        | FBZ_W_BUFFER
        | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT);
    // Blending makes every pixel a read of the destination followed by a
    // write of it, which is what a row shared by two lanes corrupts.
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (1 << 4) | (0x1 << 8) | (0x5 << 12) | (0x80 << 24),
    );
    write_reg(&mut distira, SST_COLOR1, 0x0010_2030);
    write_reg(&mut distira, SST_ZA_COLOR, 0xffff);
    write_reg(&mut distira, SST_FBZ_MODE, depth_mode);
    write_reg(&mut distira, SST_FASTFILL_CMD, 1);

    // Both triangles run off the edges of the frame, so each one covers
    // nearly all of it and they overlap nearly everywhere. The second is
    // nearer at every pixel and its depth is flat, so the depth test decides
    // the result purely by ORDER: drawn second it blends over the first and
    // takes the depth, drawn first it is overwritten and the first is
    // rejected. A row two lanes share comes out differently every run.
    write_reg(&mut distira, SST_FBZ_MODE, depth_mode);
    submit_flat_triangle(
        &mut distira,
        [(0, 0), (512, 0), (0, 256)],
        (0xc0, 0x30, 0x30),
        20_000_000,
    );
    write_reg(&mut distira, SST_FBZ_MODE, depth_mode | FBZ_Y_ORIGIN);
    submit_flat_triangle(
        &mut distira,
        [(0, 0), (512, 0), (0, 256)],
        (0x20, 0xb0, 0x40),
        500_000_000,
    );
    assert_eq!(
        distira.raster_queue_depth(),
        usize::from(queue_enabled) * 2,
        "the two triangles must share one batch for this to test anything"
    );

    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);
    let frame = distira.scanout_argb();
    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    let depth: Vec<u16> = (0..SCENE_HEIGHT)
        .flat_map(|y| (0..SCENE_WIDTH).map(move |x| (y, x)))
        .map(|(y, x)| distira.read_lfb_u16(y * 2048 + x * 2))
        .collect();
    let written = distira.scanout_state().triangles.color_written;
    (frame, depth, written)
}

#[test]
fn a_batch_may_mix_y_origins_without_two_lanes_meeting_on_one_row() {
    let serial = mixed_y_origin_scene(false, 1);
    assert!(
        serial.2 > 20_000,
        "the scene must actually paint: {}",
        serial.2
    );

    // Two lanes is the sharp case: flipping always swaps row parity there.
    for lanes in [1, 2, 4] {
        let queued = mixed_y_origin_scene(true, lanes);
        assert_eq!(
            serial.0, queued.0,
            "colour must match the unqueued render at {lanes} lanes"
        );
        assert_eq!(
            serial.1, queued.1,
            "depth must match the unqueued render at {lanes} lanes"
        );
        assert_eq!(
            serial.2, queued.2,
            "color_written must match the unqueued render at {lanes} lanes"
        );
    }
}

/// `nopCMD` with the reset bit clear is Glide's fence: real hardware queues
/// it in FIFO order behind pending triangles and moves on. It carries no
/// state this device reads or writes, so it must not force a drain. Before
/// the L1 fix every `nopCMD` write drained synchronously regardless of the
/// value, which is exactly the per-fence drain the 2026-09-01 diagnosis
/// measured (248 drains for 960 triangles, 3.9 triangles per drain). This
/// test fails on that old behavior: it would see the queue empty right after
/// The other half of the nopCMD story: the reset-statistics case (byte 0,
/// bit 0 set) must still drain before it zeroes the FBI pixel counters. A
/// queued triangle's pixels belong to the epoch BEFORE the reset, so they
/// have to be folded in first. Deleting that drain (making `nopCMD` never
/// drain at all) passes every other test in this suite -- the pre-existing
/// `nop_command_bit_zero_resets_all_fbi_pixel_counters` test never enables
/// the queue, so it never exercises this ordering. This is the test that
/// does.
#[test]
fn a_reset_statistics_nop_cmd_drains_the_queue_first() {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(true);
    distira.set_frame_size(SCENE_WIDTH as u32, SCENE_HEIGHT as u32);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    submit_flat_triangle(
        &mut distira,
        [(0, 0), (256, 0), (0, 128)],
        (0xff, 0x00, 0x00),
        10_000_000,
    );
    assert_eq!(distira.raster_queue_depth(), 1, "triangle must be queued");
    // Reset statistics. The queued triangle's pixels belong to the epoch
    // BEFORE the reset, so they must be folded in and then zeroed.
    write_reg(&mut distira, SST_NOP_CMD, 1);
    assert_eq!(
        read_reg(&mut distira, SST_FBI_PIXELS_IN),
        0,
        "a reset-statistics nopCMD must drain first: pixels from triangles \
         submitted before the reset must not survive into the new epoch"
    );
}

/// the fence instead of still holding both triangles.
#[test]
fn a_plain_nop_cmd_fence_does_not_drain_pending_triangles() {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(true);
    distira.set_frame_size(SCENE_WIDTH as u32, SCENE_HEIGHT as u32);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );

    submit_flat_triangle(
        &mut distira,
        [(0, 0), (256, 0), (0, 128)],
        (0x40, 0x80, 0xc0),
        10_737_418,
    );
    submit_flat_triangle(
        &mut distira,
        [(32, 16), (224, 24), (64, 120)],
        (0xff, 0x20, 0x10),
        20_000_000,
    );
    assert_eq!(distira.raster_queue_depth(), 2, "both triangles must batch");

    // Glide's ordinary fence: no bits set. Real Glide code sends this between
    // essentially every triangle to mark FIFO order, which is why it dominates
    // the drain-trigger census by two orders of magnitude.
    write_reg(&mut distira, SST_NOP_CMD, 0);

    assert_eq!(
        distira.raster_queue_depth(),
        2,
        "an ordering-only nopCMD must not join the queue: it touches no state \
         a queued triangle could still change"
    );
}

/// Many triangles fenced by plain `nopCMD` writes, the shape the diagnosis's
/// demo-frame trace actually saw. Before the fix each fence forced its own
/// drain (one triangle per drain, in this scene); after it the whole run is
/// one batch, because nothing between the triangles needs the framebuffer.
#[test]
fn nop_cmd_fences_between_triangles_batch_into_one_drain() {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(true);
    distira.set_frame_size(SCENE_WIDTH as u32, SCENE_HEIGHT as u32);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );

    const TRIANGLES: u32 = 20;
    for index in 0..TRIANGLES {
        let offset = (index % 4) * 8;
        submit_flat_triangle(
            &mut distira,
            [(offset, 0), (256, offset), (0, 128)],
            (0x10 + index, 0x20, 0x30),
            10_000_000 + index * 1000,
        );
        write_reg(&mut distira, SST_NOP_CMD, 0);
    }
    assert_eq!(
        distira.raster_queue_depth(),
        TRIANGLES as usize,
        "the fences must not have drawn anything yet"
    );

    // A real consumer: scanning out drains whatever is left.
    let frame = distira.scanout_argb();
    assert!(
        frame.iter().any(|&pixel| pixel != 0),
        "the batched triangles must actually have painted"
    );
    assert_eq!(
        distira.scanout_state().triangles.queue_drains,
        1,
        "twenty triangles fenced only by ordering-only nopCMD writes must \
         collapse into a single drain, not twenty"
    );
}

/// The correctness half of the same story: a framebuffer read that comes
/// right after an ordering-only barrier must still see every triangle
/// submitted before that barrier. This is what pins the consumer path --
/// `scanout_argb` is a true consumer and must drain, even though the
/// `nopCMD` immediately before it must not.
#[test]
fn framebuffer_read_after_a_barrier_sees_every_prior_triangle() {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(true);
    distira.set_frame_size(64, 64);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    submit_flat_triangle(
        &mut distira,
        [(0, 0), (64, 0), (0, 64)],
        (0xff, 0x00, 0x00),
        10_000_000,
    );
    // A pure ordering barrier: no reset bit, nothing to drain for.
    write_reg(&mut distira, SST_NOP_CMD, 0);

    let frame = distira.scanout_argb();
    assert!(
        frame.iter().any(|&pixel| red_channel(pixel) > 0),
        "scanout after a barrier must still show the triangle submitted before it"
    );
}

#[test]
fn a_pending_triangle_is_drained_before_the_frame_is_scanned_out() {
    // The narrow version of the stress test: submit and scan out with no
    // other drain point in between. scanout_argb has to drain the queue, or
    // the presented frame is missing the triangle that was just submitted.
    let mut distira = Distira::new();
    distira.set_frame_size(64, 64);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 64 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 64 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);

    let frame = distira.scanout_argb();
    assert!(
        frame.iter().any(|&pixel| red_channel(pixel) > 0),
        "scanout must show the triangle submitted just before it"
    );
}

/// Submit a triangle whose texture iterators are FLAT: `s` and `t` are 0 at
/// every pixel, so it samples exactly one texel (TMU0, base 0, LOD 0, s=0,
/// t=0) no matter where on screen it is drawn. That is what lets the test
/// below tell "which texture epoch did this triangle see" apart from "which
/// texel did this pixel happen to land on".
fn submit_flat_textured_triangle(distira: &mut Distira, vertices: [(u32, u32); 3], start_w: u32) {
    write_reg(distira, SST_VERTEX_AX, vertices[0].0 << 4);
    write_reg(distira, SST_VERTEX_AY, vertices[0].1 << 4);
    write_reg(distira, SST_VERTEX_BX, vertices[1].0 << 4);
    write_reg(distira, SST_VERTEX_BY, vertices[1].1 << 4);
    write_reg(distira, SST_VERTEX_CX, vertices[2].0 << 4);
    write_reg(distira, SST_VERTEX_CY, vertices[2].1 << 4);
    write_reg(distira, SST_START_R, 0);
    write_reg(distira, SST_START_G, 0);
    write_reg(distira, SST_START_B, 0);
    write_reg(distira, SST_START_A, 0xff << 12);
    write_reg(distira, SST_DR_DX, 0);
    write_reg(distira, SST_DR_DY, 0);
    write_reg(distira, SST_START_W, start_w);
    write_reg(distira, SST_DW_DX, 0);
    write_reg(distira, SST_DW_DY, 0);
    write_reg(distira, SST_START_S, 0);
    write_reg(distira, SST_START_T, 0);
    write_reg(distira, SST_DS_DX, 0);
    write_reg(distira, SST_DS_DY, 0);
    write_reg(distira, SST_DT_DX, 0);
    write_reg(distira, SST_DT_DY, 0);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
}

/// THE ORDERING RULE ITSELF
/// (`dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 6, slice
/// 1): a texture-aperture write must order against the triangles around it
/// instead of draining them. Triangle A is enqueued, then a write changes
/// the texel it samples, then triangle B is enqueued sampling the same
/// texel. Both are queued (not drawn) the whole time -- proved by
/// `raster_queue_depth` after each step, which is RED against the old
/// unconditional-drain `write_texture_u32`: that design left the queue
/// holding only triangle B by the time this function returns, because the
/// write drained triangle A out from under it. The pixel-level assertions
/// (A shows the OLD texel, B shows the NEW one) hold under EITHER design --
/// this test's whole point is that they must ALSO hold once the write stops
/// forcing a drain, which the depth assertions are what actually pin. The
/// sample point is `(8, y)`, not the triangles' own midpoints: `(32, y)`
/// sits past B's hypotenuse (`[(0, 32), (64, 32), (0, 64)]` covers `x < 28`
/// at `y=50`) and would silently compare A's colour against unpainted
/// background instead of B's real sample.
#[test]
fn a_texture_write_orders_against_triangles_instead_of_draining_them() {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(true);
    distira.set_frame_size(64, 64);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEXTUREMODE_LOCAL | (TEX_R5G6B5 << 8),
    );
    write_reg(&mut distira, SST_TLOD, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );

    // The OLD texel: an RGB565 value with all three channels non-zero and
    // distinct from the NEW one below. This write itself queues too -- there
    // is no drain point yet -- so it counts as entry 1.
    distira.write_texture_u32(0, 0x1111_1111);
    assert_eq!(
        distira.raster_queue_depth(),
        1,
        "the first write must queue, not apply synchronously"
    );

    // Triangle A: top half of the frame, must see the OLD texel.
    submit_flat_textured_triangle(&mut distira, [(0, 0), (64, 0), (0, 32)], 10_000_000);
    assert_eq!(
        distira.raster_queue_depth(),
        2,
        "triangle A must be queued, not drawn yet"
    );

    // The write that separates A from B. It must join the queue, not drain
    // it -- that is the whole lever.
    distira.write_texture_u32(0, 0x2222_2222);
    assert_eq!(
        distira.raster_queue_depth(),
        3,
        "the texture write must queue alongside triangle A, not drain it \
         (this is RED against the old unconditional-drain write_texture_u32)"
    );

    // Triangle B: bottom half, must see the NEW texel.
    submit_flat_textured_triangle(&mut distira, [(0, 32), (64, 32), (0, 64)], 20_000_000);
    assert_eq!(
        distira.raster_queue_depth(),
        4,
        "triangle B must also just queue: both writes and both triangles wait together"
    );

    let frame = distira.scanout_argb();
    assert_eq!(
        distira.scanout_state().triangles.queue_drains,
        1,
        "everything queued -- two triangles and a texture write -- must \
         collapse into ONE drain at the real synchronisation point (scanout)"
    );

    let width = 64usize;
    let old_pixel = frame[10 * width + 8];
    let new_pixel = frame[50 * width + 8];
    assert_ne!(
        old_pixel, new_pixel,
        "triangle A (queued before the write) and triangle B (queued after \
         it) must have sampled different texels: old={old_pixel:#010x} \
         new={new_pixel:#010x}"
    );
    assert_ne!(
        red_channel(old_pixel),
        red_channel(new_pixel),
        "the two texels differ in every channel, including red"
    );
}

/// The segment-splitting half of the same lever: a batch that mixes
/// triangles and texture writes must still reach the parallel lanes on the
/// TRIANGLE runs either side of a write, not fall back to serial because a
/// write happens to sit in the middle. Two large triangle batches (well
/// past `PARALLEL_PIXEL_THRESHOLD`) separated by one texture write must
/// produce the identical picture whether the run has 1 lane or 4 -- the
/// segment split is a scheduling seam, not a pixel seam.
#[test]
fn a_texture_write_mid_batch_splits_lanes_without_moving_a_pixel() {
    fn render(lanes: usize) -> (Vec<u32>, u64, u64) {
        let mut distira = Distira::new();
        distira.set_raster_queue_enabled(true);
        distira.set_raster_lanes(lanes);
        distira.set_frame_size(256, 256);
        write_reg(
            &mut distira,
            SST_FBZ_COLOR_PATH,
            FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
        );
        write_reg(
            &mut distira,
            SST_TEXTURE_MODE,
            TEXTUREMODE_LOCAL | (TEX_R5G6B5 << 8),
        );
        write_reg(&mut distira, SST_TLOD, 0);
        write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
        write_reg(
            &mut distira,
            SST_FBZ_MODE,
            FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
        );
        distira.write_texture_u32(0, 0x1111_1111);

        // A large triangle -- comfortably over the parallel threshold --
        // queued BEFORE the write.
        submit_flat_textured_triangle(&mut distira, [(0, 0), (256, 0), (0, 256)], 10_000_000);

        distira.write_texture_u32(0, 0x2222_2222);

        // A second large triangle, queued AFTER the write, drawn on top
        // (both cover nearly the whole frame; the second one paints last
        // in submission order, so its colour is what survives without a
        // depth test).
        submit_flat_textured_triangle(&mut distira, [(0, 0), (256, 0), (0, 256)], 20_000_000);

        let frame = distira.scanout_argb();
        let state = distira.scanout_state();
        (
            frame,
            state.triangles.queue_drains,
            state.triangles.queue_drains_parallel,
        )
    }

    let (serial_frame, serial_drains, serial_parallel) = render(1);
    let (parallel_frame, parallel_drains, parallel_parallel) = render(4);

    assert_eq!(
        serial_frame, parallel_frame,
        "the write's segment split must not change a single pixel between 1 and 4 lanes"
    );
    assert_eq!(
        serial_drains, 1,
        "one drain at scanout, regardless of lane count"
    );
    assert_eq!(
        parallel_drains, 1,
        "one drain at scanout, regardless of lane count"
    );
    assert_eq!(
        serial_parallel, 0,
        "raster_lanes(1) never reaches the parallel lanes"
    );
    assert_eq!(
        parallel_parallel, 1,
        "raster_lanes(4) with triangles this large must reach the parallel \
         lanes on at least one of the two segments the write splits"
    );
}

/// THE SAFETY CLAIM THE WHOLE LEVER RESTS ON: a queued triangle's texture
/// coordinates resolve against the register snapshot it was submitted
/// with, not against live device state at raster time
/// (`dev_docs/2026-09-05-distira-texture-queue-review.md` section 1/finding
/// F). Triangle A is enqueued, `SST_TEX_BASE_ADDR` changes (a
/// snapshot-covered register -- it does NOT drain), a texture write lands
/// at the NEW base, then triangle B is enqueued. A must resolve through its
/// OWN (old) base and see the OLD texel; B must resolve through the NEW
/// base and see the NEW one.
///
/// This is a materially different case from
/// `a_texture_write_orders_against_triangles_instead_of_draining_them`:
/// that test holds the registers fixed and only changes texture CONTENT: A
/// and B could share the exact same `RasterParams` and it would still pass.
/// Here A and B decode to two DIFFERENT physical texture addresses, so a
/// broken snapshot (say, `QueuedTriangle` resolving its texel through
/// `self.raster_params()` at DRAIN time instead of carrying its own copy
/// from ENQUEUE time) would make A read the NEW base too and see whatever
/// this test's control run below can distinguish. Manually confirmed RED:
/// forcing `triangle.params.tex_base_addr` to the OLD value at enqueue time
/// for every queued triangle (simulating a snapshot that a later live
/// register write could still reach) makes both A and B resolve through the
/// OLD base -- both come back `0x0010208c`, the OLD texel's exact RGB565
/// expansion, and the pixel comparison below panics with `left == right`.
/// (The sample point matters here: an earlier draft of this test sampled
/// `(32, 50)`, which sits past triangle B's hypotenuse -- `[(0, 32), (64,
/// 32), (0, 64)]` only covers `x < 28` at `y=50` -- so it silently compared
/// A's colour against unpainted background instead of B's real sample. `(8,
/// 50)` stays interior to every triangle shape both tests here use.)
#[test]
fn a_queued_triangle_resolves_its_own_texture_register_snapshot_not_live_state() {
    let mut distira = Distira::new();
    distira.set_raster_queue_enabled(true);
    distira.set_frame_size(64, 64);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEXTUREMODE_LOCAL | (TEX_R5G6B5 << 8),
    );
    write_reg(&mut distira, SST_TLOD, 0);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );

    // The OLD base, and the OLD texel sitting there.
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    distira.write_texture_u32(0, 0x1111_1111);
    assert_eq!(
        distira.raster_queue_depth(),
        1,
        "the first write must queue"
    );

    // Triangle A: queued against the OLD base. Must NOT see anything past
    // this point.
    submit_flat_textured_triangle(&mut distira, [(0, 0), (64, 0), (0, 32)], 10_000_000);
    assert_eq!(distira.raster_queue_depth(), 2, "triangle A must be queued");

    // A base-address change. `raster_snapshot_covers_register` lists
    // SST_TEX_BASE_ADDR, so this must NOT drain -- triangle A stays queued,
    // still holding its OLD base in its own snapshot.
    const NEW_BASE: u32 = 2 * 1024 * 1024;
    assert_eq!(
        NEW_BASE & 0x7,
        0,
        "must round-trip the <<3 base-address shift"
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, NEW_BASE >> 3);
    assert_eq!(
        distira.raster_queue_depth(),
        2,
        "a snapshot-covered register write must not drain triangle A"
    );

    // The NEW texel, written at the NEW base (decode is always live, at
    // write time -- this lands at NEW_BASE + 0, a different physical
    // address than A's write above).
    distira.write_texture_u32(0, 0x2222_2222);
    assert_eq!(
        distira.raster_queue_depth(),
        3,
        "the write must queue alongside A, not drain it"
    );

    // Triangle B: queued against the NEW base. Must see the NEW texel.
    submit_flat_textured_triangle(&mut distira, [(0, 32), (64, 32), (0, 64)], 20_000_000);
    assert_eq!(
        distira.raster_queue_depth(),
        4,
        "triangle B must also just queue"
    );

    let frame = distira.scanout_argb();
    assert_eq!(
        distira.scanout_state().triangles.queue_drains,
        1,
        "everything queued -- two triangles, a register write and a texture \
         write -- must collapse into ONE drain at scanout"
    );

    let width = 64usize;
    let old_pixel = frame[10 * width + 8];
    let new_pixel = frame[50 * width + 8];
    assert_ne!(
        old_pixel, new_pixel,
        "A (old base, old snapshot) and B (new base, new snapshot) must \
         resolve to different texels: old={old_pixel:#010x} new={new_pixel:#010x}"
    );
    assert_ne!(
        red_channel(old_pixel),
        red_channel(new_pixel),
        "the two texels differ in every channel, including red"
    );
}
