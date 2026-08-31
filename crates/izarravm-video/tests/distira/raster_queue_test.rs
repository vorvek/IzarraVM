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
    // batches. Then a texture upload, which must drain before it lands.
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
        0,
        "a texture upload must draw what is waiting first"
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
