// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn custom_identity_and_capabilities_are_readable() {
    let distira = Distira::new();

    assert_eq!(read_reg(&distira, DISTIRA_REG_ID), DISTIRA_ID_VALUE);
    assert_eq!(read_reg(&distira, DISTIRA_REG_CAPS), DISTIRA_CAPS_VALUE);
}

#[test]
fn unknown_sst_register_reads_return_open_bus() {
    assert_eq!(read_reg(&Distira::new(), 0x1d0), u32::MAX);
}

#[test]
fn glide_init_sum_table_distinguishes_all_color1_values() {
    let mut distira = Distira::new();
    distira.set_frame_size(64, 64);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DRAW_FRONT | FBZ_RGB_WMASK | FBZ_DITHER,
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 36 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 36 << 4);
    write_reg(&mut distira, SST_START_S, 0);
    write_reg(&mut distira, SST_START_T, 0);
    write_reg(&mut distira, SST_START_W, 0);
    write_reg(&mut distira, SST_DS_DX, 1 << 18);
    write_reg(&mut distira, SST_DT_DX, 0);
    write_reg(&mut distira, SST_DW_DX, 0);
    write_reg(&mut distira, SST_DS_DY, 0);
    write_reg(&mut distira, SST_DT_DY, 1 << 18);
    write_reg(&mut distira, SST_DW_DY, 0);

    let mut red_blue_sums = [false; 4096];
    let mut green_sums = [false; 4096];
    for color in 0..=255u32 {
        write_reg(
            &mut distira,
            SST_COLOR1,
            (color << 16) | (color << 8) | color,
        );
        write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
        write_reg(&mut distira, SST_LFB_MODE, LFB_READ_FRONT);

        let mut red_sum = 0usize;
        let mut green_sum = 0usize;
        let mut blue_sum = 0usize;
        for y in 0..4 {
            for x in (0..4).step_by(2) {
                let pair = distira.read_lfb_u32(y * 2048 + x * 2);
                for pixel in [pair as u16, (pair >> 16) as u16] {
                    red_sum += usize::from((pixel >> 11) & 0x1f) << 3;
                    green_sum += usize::from((pixel >> 5) & 0x3f) << 2;
                    blue_sum += usize::from(pixel & 0x1f) << 3;
                }
            }
        }

        assert_eq!(red_sum, blue_sum, "grayscale probe {color:#04x}");
        assert!(
            !red_blue_sums[red_sum],
            "color {color:#04x} repeats red/blue sum {red_sum:#05x}"
        );
        assert!(
            !green_sums[green_sum],
            "color {color:#04x} repeats green sum {green_sum:#05x}"
        );
        red_blue_sums[red_sum] = true;
        green_sums[green_sum] = true;
    }
}

#[test]
fn glide_tmu_memory_probe_reads_the_selected_texture_base() {
    const SENSE2: u32 = 0x92f5_6eb0;
    const SENSE1: u32 = 0xf2a9_16b5;
    const SENSE0: u32 = 0xbadb_eef1;
    const TREX0: usize = 2 << 10;
    const TMU1_APERTURE: usize = 1 << 21;
    const TC_REPLACE: u32 = (1 << 12) | (1 << 18);
    const TCA_REPLACE: u32 = (1 << 21) | (1 << 27);

    fn sense(distira: &mut Distira, tmu: usize, memory_offset: u32) -> u32 {
        for (offset, value) in [(0x20_0000, SENSE2), (0x10_0000, SENSE1), (0, SENSE0)] {
            write_reg(distira, SST_TEX_BASE_ADDR, offset >> 3);
            distira.write_texture_u32(tmu * TMU1_APERTURE, value);
        }
        write_reg(distira, SST_TEX_BASE_ADDR, memory_offset >> 3);
        write_reg(distira, SST_TRIANGLE_CMD, 0);
        distira.read_lfb_u32(0)
    }

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_FRONT);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_FRONT | FBZ_RGB_WMASK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL) | TC_REPLACE | TCA_REPLACE,
    );
    write_reg(&mut distira, SST_TLOD, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_S, 0);
    write_reg(&mut distira, SST_START_T, 0);
    write_reg(&mut distira, SST_START_W, 0);
    write_reg(&mut distira, SST_DS_DX, 1 << 18);
    write_reg(&mut distira, SST_DT_DX, 0);
    write_reg(&mut distira, SST_DW_DX, 0);
    write_reg(&mut distira, SST_DS_DY, 0);
    write_reg(&mut distira, SST_DT_DY, 1 << 18);
    write_reg(&mut distira, SST_DW_DY, 0);

    assert_eq!(sense(&mut distira, 0, 0x20_0000), SENSE0);
    assert_eq!(sense(&mut distira, 0, 0x10_0000), SENSE1);
    assert_eq!(sense(&mut distira, 0, 0), SENSE0);

    write_reg(&mut distira, TREX0 | SST_TEXTURE_MODE, 0);
    assert_eq!(sense(&mut distira, 1, 0x20_0000), SENSE0);
    assert_eq!(sense(&mut distira, 1, 0x10_0000), SENSE1);
    assert_eq!(sense(&mut distira, 1, 0), SENSE0);
}

#[test]
fn fbi_chip_alias_updates_triangle_registers() {
    const FBI: usize = 1 << 10;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(
        &mut distira,
        FBI | SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_FRONT,
    );
    write_reg(&mut distira, FBI | SST_VERTEX_AX, 0);
    write_reg(&mut distira, FBI | SST_VERTEX_AY, 0);
    write_reg(&mut distira, FBI | SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, FBI | SST_VERTEX_BY, 0);
    write_reg(&mut distira, FBI | SST_VERTEX_CX, 0);
    write_reg(&mut distira, FBI | SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, FBI | SST_START_R, 0xff << 12);
    write_reg(&mut distira, FBI | SST_TRIANGLE_CMD, 0);

    assert_eq!(distira.scanout_argb()[0], 0x00ff_0000);
    assert_eq!(read_reg(&distira, FBI | SST_VERTEX_BX), 4 << 4);
}

#[test]
fn adjacent_sst_quads_share_their_edge_without_a_seam() {
    fn draw_triangle(
        distira: &mut Distira,
        vertices: [(u32, u32); 3],
        color: u32,
        negative_direction: bool,
    ) {
        let [(ax, ay), (bx, by), (cx, cy)] = vertices;
        write_reg(distira, SST_VERTEX_AX, ax);
        write_reg(distira, SST_VERTEX_AY, ay);
        write_reg(distira, SST_VERTEX_BX, bx);
        write_reg(distira, SST_VERTEX_BY, by);
        write_reg(distira, SST_VERTEX_CX, cx);
        write_reg(distira, SST_VERTEX_CY, cy);
        write_reg(distira, SST_COLOR1, color);
        write_reg(
            distira,
            SST_TRIANGLE_CMD,
            u32::from(negative_direction) << 31,
        );
    }

    fn draw_quad(distira: &mut Distira, left: u32, right: u32, color: u32) {
        let bottom = 16 << 4;
        draw_triangle(
            distira,
            [(left, 0), (right, 0), (left, bottom)],
            color,
            false,
        );
        draw_triangle(
            distira,
            [(right, 0), (left, bottom), (right, bottom)],
            color,
            true,
        );
    }

    let mut distira = Distira::new();
    distira.set_frame_size(32, 16);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_FRONT | FBZ_RGB_WMASK);

    draw_quad(&mut distira, 0, 16 << 4, 0x00ff_0000);
    draw_quad(&mut distira, 16 << 4, 32 << 4, 0x0000_ff00);

    let frame = distira.scanout_argb();
    assert_eq!(read_reg(&distira, SST_FBI_PIXELS_IN), 32 * 16);
    assert_eq!(read_reg(&distira, SST_FBI_PIXELS_OUT), 32 * 16);
    for y in 0..16 {
        assert_eq!(frame[y * 32 + 15], 0x00ff_0000, "left tile row {y}");
        assert_eq!(frame[y * 32 + 16], 0x0000_ff00, "right tile row {y}");
        assert!(
            frame[y * 32..(y + 1) * 32].iter().all(|&pixel| pixel != 0),
            "seam in row {y}"
        );
    }
}

#[test]
fn fbi_pixel_counters_do_not_depend_on_write_masks() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_FRONT);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    assert_eq!(read_reg(&distira, SST_FBI_PIXELS_IN), 6);
    assert_eq!(read_reg(&distira, SST_FBI_PIXELS_OUT), 6);
    assert!(distira.scanout_argb().iter().all(|&pixel| pixel == 0));
}

#[test]
fn nop_command_bit_zero_resets_all_fbi_pixel_counters() {
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
    write_reg(&mut distira, SST_COLOR1, 0x00ff_0000);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DRAW_FRONT | FBZ_RGB_WMASK | FBZ_CHROMAKEY,
    );
    write_reg(&mut distira, SST_CHROMA_KEY, 0x00ff_0000);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_FRONT | FBZ_RGB_WMASK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (0x80 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(&mut distira, SST_START_A, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(&mut distira, SST_ALPHA_MODE, 0);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DRAW_FRONT | FBZ_DEPTH_ENABLE | (DEPTHOP_GREATERTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_Z, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_FRONT | FBZ_RGB_WMASK);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    let counters = [
        SST_FBI_PIXELS_IN,
        SST_FBI_CHROMA_FAIL,
        SST_FBI_ZFUNC_FAIL,
        SST_FBI_AFUNC_FAIL,
        SST_FBI_PIXELS_OUT,
    ];
    assert!(
        counters
            .into_iter()
            .all(|counter| read_reg(&distira, counter) != 0)
    );

    write_reg(&mut distira, SST_NOP_CMD, 0);
    write_reg(&mut distira, SST_NOP_CMD, 2);
    assert!(
        counters
            .into_iter()
            .all(|counter| read_reg(&distira, counter) != 0)
    );

    write_reg(&mut distira, SST_NOP_CMD, 1);
    assert!(
        counters
            .into_iter()
            .all(|counter| read_reg(&distira, counter) == 0)
    );
}

#[test]
fn triangle_stipple_pattern_rejects_masked_pixels() {
    fn draw_triangle(distira: &mut Distira, vertices: [(u32, u32); 3], negative: bool) {
        let [(ax, ay), (bx, by), (cx, cy)] = vertices;
        write_reg(distira, SST_VERTEX_AX, ax);
        write_reg(distira, SST_VERTEX_AY, ay);
        write_reg(distira, SST_VERTEX_BX, bx);
        write_reg(distira, SST_VERTEX_BY, by);
        write_reg(distira, SST_VERTEX_CX, cx);
        write_reg(distira, SST_VERTEX_CY, cy);
        write_reg(distira, SST_TRIANGLE_CMD, u32::from(negative) << 31);
    }

    let mut distira = Distira::new();
    distira.set_frame_size(8, 4);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DRAW_FRONT | FBZ_RGB_WMASK | FBZ_STIPPLE | FBZ_STIPPLE_PATT,
    );
    write_reg(&mut distira, SST_COLOR1, 0x00ff_0000);
    write_reg(&mut distira, SST_STIPPLE, 1 << 7);
    draw_triangle(&mut distira, [(0, 0), (8 << 4, 0), (0, 4 << 4)], false);
    draw_triangle(
        &mut distira,
        [(8 << 4, 0), (0, 4 << 4), (8 << 4, 4 << 4)],
        true,
    );

    let frame = distira.scanout_argb();
    assert_eq!(read_reg(&distira, SST_FBI_PIXELS_IN), 8 * 4);
    assert_eq!(read_reg(&distira, SST_FBI_PIXELS_OUT), 1);
    assert_eq!(frame[0], 0x00ff_0000);
    assert!(frame[1..].iter().all(|&pixel| pixel == 0));
}

#[test]
fn triangle_y_origin_flips_color_and_depth_destinations() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DRAW_FRONT
            | FBZ_RGB_WMASK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_Y_ORIGIN
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_COLOR1, 0x00ff_0000);
    write_reg(&mut distira, SST_START_Z, 0x1234 << 12);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0);
    assert_eq!(frame[3 * 4], 0x00ff_0000);
    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    assert_eq!(distira.read_lfb_u16(0), u16::MAX);
    assert_eq!(distira.read_lfb_u16(3 * 2048), 0x1234);
}

#[test]
fn y_origin_uses_physical_rows_for_stipple_and_dither() {
    fn render(y_origin: bool) -> Vec<u32> {
        fn draw_triangle(distira: &mut Distira, vertices: [(u32, u32); 3], negative: bool) {
            let [(ax, ay), (bx, by), (cx, cy)] = vertices;
            write_reg(distira, SST_VERTEX_AX, ax);
            write_reg(distira, SST_VERTEX_AY, ay);
            write_reg(distira, SST_VERTEX_BX, bx);
            write_reg(distira, SST_VERTEX_BY, by);
            write_reg(distira, SST_VERTEX_CX, cx);
            write_reg(distira, SST_VERTEX_CY, cy);
            write_reg(distira, SST_TRIANGLE_CMD, u32::from(negative) << 31);
        }

        let mut distira = Distira::new();
        distira.set_frame_size(8, 4);
        write_reg(&mut distira, SST_FBZ_COLOR_PATH, RGB_SELECT_COLOR1);
        write_reg(
            &mut distira,
            SST_FBZ_MODE,
            FBZ_DRAW_FRONT
                | FBZ_RGB_WMASK
                | FBZ_DITHER
                | FBZ_STIPPLE
                | FBZ_STIPPLE_PATT
                | if y_origin { FBZ_Y_ORIGIN } else { 0 },
        );
        write_reg(&mut distira, SST_COLOR1, 0x0011_1111);
        write_reg(&mut distira, SST_STIPPLE, 0x8142_2814);
        draw_triangle(&mut distira, [(0, 0), (8 << 4, 0), (0, 4 << 4)], false);
        draw_triangle(
            &mut distira,
            [(8 << 4, 0), (0, 4 << 4), (8 << 4, 4 << 4)],
            true,
        );
        distira.scanout_argb()
    }

    assert_eq!(render(false), render(true));
}

#[test]
fn fastfill_honors_y_origin_but_lfb_addresses_remain_direct() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_CLIP_LEFT_RIGHT, 4);
    write_reg(&mut distira, SST_CLIP_LOW_Y_HIGH_Y, 1);
    write_reg(&mut distira, SST_COLOR1, 0x00ff_0000);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DRAW_FRONT | FBZ_RGB_WMASK | FBZ_Y_ORIGIN,
    );
    write_reg(&mut distira, SST_FASTFILL_CMD, 0);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_FRONT,
    );
    distira.write_lfb_u16(0, 0x07e0);
    let frame = distira.scanout_argb();
    assert_eq!(&frame[..4], &[0x0000_ff00, 0, 0, 0]);
    assert_eq!(&frame[12..], &[0x00ff_0000; 4]);
}

#[test]
fn alternate_register_map_routes_glide_setup_columns() {
    const ALT: usize = 1 << 21;

    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);

    write_reg(&mut distira, ALT | 0x024, 0x0011_1111);
    assert_eq!(read_reg(&distira, SST_START_G), 0x0011_1111);
    assert_eq!(read_reg(&distira, SST_DR_DX), 0);

    write_reg(&mut distira, SST_FBI_INIT3, FBIINIT3_REMAP);
    write_reg(&mut distira, ALT | 0x020, 0x0022_2222);
    write_reg(&mut distira, ALT | 0x024, 0x0033_3333);
    write_reg(&mut distira, ALT | 0x028, 0x0044_4444);
    write_reg(&mut distira, ALT | 0x02c, 0x0055_5555);
    assert_eq!(read_reg(&distira, SST_START_R), 0x0022_2222);
    assert_eq!(read_reg(&distira, SST_DR_DX), 0x0033_3333);
    assert_eq!(read_reg(&distira, SST_DR_DY), 0x0044_4444);
    assert_eq!(read_reg(&distira, SST_START_G), 0x0055_5555);

    distira.set_frame_size(4, 4);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);

    for (register, value) in [
        (0x088, 0.0f32),
        (0x08c, 0.0),
        (0x090, 3.0),
        (0x094, 0.0),
        (0x098, 0.0),
        (0x09c, 3.0),
        (0x0a0, 255.0),
        (0x0a4, 2.0),
        (0x0a8, 3.0),
        (0x0ac, 255.0),
        (0x0b8, 255.0),
        (0x0dc, 0.0),
        (0x0e0, 1.0),
        (0x0e4, 0.0),
        (0x0e8, 0.0),
        (0x0ec, 0.0),
        (0x0f0, 0.0),
        (0x0f4, 1.0),
        (0x0f8, 0.0),
        (0x0fc, 0.0),
    ] {
        write_reg(&mut distira, ALT | register, value.to_bits());
    }
    assert_eq!(read_reg(&distira, SST_FSTART_R), 255.0f32.to_bits());
    assert_eq!(read_reg(&distira, SST_FDR_DX), 2.0f32.to_bits());
    assert_eq!(read_reg(&distira, SST_FDR_DY), 3.0f32.to_bits());

    write_reg(&mut distira, ALT | SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x0000_ff00);
}

#[test]
fn clut_data_interpolates_rgb565_scanout() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    for index in 0..=32u32 {
        let value = (index * 8).min(255);
        let (red, green, blue) = match index {
            5 => (50, 10, 70),
            6 => (60, 30, 90),
            _ => (value, value, value),
        };
        write_reg(
            &mut distira,
            SST_CLUT_DATA,
            (index << 24) | (red << 16) | (green << 8) | blue,
        );
    }

    let raw = (5 << 11) | (11 << 5) | 6;
    distira.write_lfb_u16(0, raw);

    assert_eq!(distira.scanout_argb(), vec![0x0032_145a]);
}

#[test]
fn voodoo_registers_store_init_and_render_state() {
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);

    write_reg(&mut distira, SST_FBI_INIT0, 0x0000_0003);
    write_reg(&mut distira, SST_FBI_INIT1, 0x0000_0100);
    write_reg(&mut distira, SST_FBI_INIT2, 0x0000_0200);
    write_reg(&mut distira, SST_FBI_INIT3, 0x0000_0001);
    write_reg(&mut distira, SST_LFB_MODE, 0x0000_0005);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK);
    write_reg(&mut distira, SST_ALPHA_MODE, 0x0001_0001);
    write_reg(&mut distira, SST_CLIP_LEFT_RIGHT, (2 << 16) | 7);
    write_reg(&mut distira, SST_CLIP_LOW_Y_HIGH_Y, (3 << 16) | 9);

    assert_eq!(read_reg(&distira, SST_STATUS) & 0x380, 0);
    assert_eq!(read_reg(&distira, SST_FBI_INIT0), 0x0000_0003);
    assert_eq!(read_reg(&distira, SST_FBI_INIT1), 0x0000_0100);
    assert_eq!(read_reg(&distira, SST_FBI_INIT2), 0x0000_0200);
    assert_eq!(read_reg(&distira, SST_FBI_INIT3), 0x0000_0601);
    assert_eq!(read_reg(&distira, SST_LFB_MODE), 0x0000_0005);
    assert_eq!(read_reg(&distira, SST_FBZ_MODE), FBZ_RGB_WMASK);
    assert_eq!(read_reg(&distira, SST_ALPHA_MODE), 0x0001_0001);
    assert_eq!(read_reg(&distira, SST_CLIP_LEFT_RIGHT), (2 << 16) | 7);
    assert_eq!(read_reg(&distira, SST_CLIP_LOW_Y_HIGH_Y), (3 << 16) | 9);
}

#[test]
fn fbi_init_register_writes_require_pci_init_enable() {
    let mut distira = Distira::new();
    let initial_init2 = read_reg(&distira, SST_FBI_INIT2);

    write_reg(&mut distira, SST_FBI_INIT0, FBIINIT0_GRAPHICS_RESET);
    write_reg(
        &mut distira,
        SST_FBI_INIT2,
        247 << FBIINIT2_BUFFER_OFFSET_SHIFT,
    );
    write_reg(&mut distira, SST_FBI_INIT4, 0x1234_5678);

    assert_eq!(read_reg(&distira, SST_FBI_INIT0), 0);
    assert_eq!(read_reg(&distira, SST_FBI_INIT2), initial_init2);
    assert_eq!(read_reg(&distira, SST_FBI_INIT4), 0);

    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(&mut distira, SST_FBI_INIT4, 0x1234_5678);
    assert_eq!(read_reg(&distira, SST_FBI_INIT4), 0x1234_5678);
}

#[test]
fn fbi_init_layout_and_reset_select_physical_buffers() {
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(
        &mut distira,
        SST_FBI_INIT1,
        FBIINIT1_VIDEO_RESET | (10 << FBIINIT1_TILES_IN_X_SHIFT),
    );
    write_reg(
        &mut distira,
        SST_FBI_INIT2,
        150 << FBIINIT2_BUFFER_OFFSET_SHIFT,
    );
    write_reg(&mut distira, SST_VIDEO_DIMENSIONS, (480 << 16) | 639);

    assert!(!distira.display_enabled());
    assert_eq!(distira.display().width, 640);
    assert_eq!(distira.display().height, 480);
    assert_eq!(distira.display().pitch, 1280);
    assert_eq!(distira.display().front_base, 0);
    assert_eq!(distira.display().back_base, 150 * 4096);

    write_reg(&mut distira, SST_FBI_INIT1, 10 << FBIINIT1_TILES_IN_X_SHIFT);
    assert!(distira.display_enabled());
    distira.swap_buffers();
    assert_eq!(distira.display().front_base, 150 * 4096);

    write_reg(&mut distira, SST_FBI_INIT0, FBIINIT0_GRAPHICS_RESET);
    assert_eq!(distira.display().front_base, 0);
    assert_eq!(distira.display().back_base, 150 * 4096);
}

#[test]
fn voodoo_texture_detail_register_round_trips() {
    const SST_TDETAIL: usize = 0x308;

    let mut distira = Distira::new();

    write_reg(&mut distira, SST_TDETAIL, 0x0001_c23f);

    assert_eq!(read_reg(&distira, SST_TDETAIL), 0x0001_c23f);
}

#[test]
fn clear_back_buffer_and_swap_presents_rgb565_words() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 2);
    distira.clear_back_rgb(0x34, 0x56, 0x78);

    assert!(!distira.display_enabled());
    distira.swap_buffers();

    assert!(distira.display_enabled());
    let frame = distira.scanout_argb();
    assert_eq!(frame.len(), 8);
    assert!(frame.iter().all(|&pixel| pixel == 0x0031_557b));
}

#[test]
fn voodoo_fastfill_and_swap_present_the_back_buffer() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 2);

    write_reg(&mut distira, SST_CLIP_LEFT_RIGHT, 2);
    write_reg(&mut distira, SST_CLIP_LOW_Y_HIGH_Y, 2);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DEPTH_WMASK | FBZ_DRAW_BACK,
    );
    write_reg(&mut distira, SST_COLOR1, 0x0034_5678);
    write_reg(&mut distira, SST_ZA_COLOR, 0x1234);
    write_reg(&mut distira, SST_FASTFILL_CMD, 0);
    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    assert_eq!(distira.read_lfb_u16(0), 0x1234);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame, vec![0x0031_557b; 4]);
}

#[test]
fn swapbuffer_command_distinguishes_immediate_and_retrace_swaps() {
    const STATUS_BUSY: u32 = 0x380;
    const STATUS_SWAP_COUNT: u32 = 0x7000_0000;

    let mut immediate = Distira::new();
    immediate.set_frame_size(1, 1);
    immediate.clear_back_rgb(0xff, 0, 0);
    write_reg(&mut immediate, SST_SWAPBUFFER_CMD, 0);
    assert_eq!(immediate.scanout_argb(), vec![0x00ff_0000]);
    assert_eq!(read_reg(&immediate, SST_STATUS) & STATUS_BUSY, 0);
    assert_eq!(read_reg(&immediate, SST_STATUS) & STATUS_SWAP_COUNT, 0);

    let mut queued = Distira::new();
    queued.set_frame_size(1, 1);
    queued.clear_back_rgb(0xff, 0, 0);
    write_reg(&mut queued, SST_SWAPBUFFER_CMD, 1);
    assert_eq!(queued.scanout_argb(), vec![0]);
    assert_eq!(read_reg(&queued, SST_STATUS) & STATUS_BUSY, STATUS_BUSY);
    assert_eq!(
        read_reg(&queued, SST_STATUS) & STATUS_SWAP_COUNT,
        0x1000_0000
    );

    queued.advance_frame_phase(479);
    assert_eq!(queued.scanout_argb(), vec![0]);
    queued.advance_frame_phase(1);
    assert_eq!(queued.scanout_argb(), vec![0x00ff_0000]);
    assert_eq!(read_reg(&queued, SST_STATUS) & STATUS_BUSY, 0);
    assert_eq!(read_reg(&queued, SST_STATUS) & STATUS_SWAP_COUNT, 0);
    assert_eq!(read_reg(&queued, SST_STATUS) & 0x40, 0);
}

#[test]
fn retrace_swap_honors_the_requested_interval() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);
    distira.clear_back_rgb(0, 0xff, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1 | (1 << 1));

    distira.advance_frame_phase(480);
    assert_eq!(distira.scanout_argb(), vec![0]);
    assert_ne!(read_reg(&distira, SST_STATUS) & 0x380, 0);

    distira.advance_frame_phase(524);
    assert_eq!(distira.scanout_argb(), vec![0]);
    distira.advance_frame_phase(1);
    assert_eq!(distira.scanout_argb(), vec![0x0000_ff00]);
    assert_eq!(read_reg(&distira, SST_STATUS) & 0x380, 0);
}

#[test]
fn queued_retrace_swaps_commit_in_submission_order() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    distira.clear_back_rgb(0xff, 0, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);
    distira.clear_back_rgb(0, 0, 0xff);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(read_reg(&distira, SST_STATUS) & 0x7000_0000, 0x2000_0000);
    distira.advance_frame_phase(480);
    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000]);
    assert_eq!(read_reg(&distira, SST_STATUS) & 0x7000_0000, 0x1000_0000);
    assert_ne!(read_reg(&distira, SST_STATUS) & 0x380, 0);

    distira.advance_frame_phase(525);
    assert_eq!(distira.scanout_argb(), vec![0x0000_00ff]);
    assert_eq!(read_reg(&distira, SST_STATUS) & 0x7000_0000, 0);
    assert_eq!(read_reg(&distira, SST_STATUS) & 0x380, 0);
}

#[test]
fn retrace_swap_is_invariant_to_split_frame_advances() {
    let mut whole = Distira::new();
    whole.set_frame_size(1, 1);
    whole.clear_back_rgb(0, 0, 0xff);
    write_reg(&mut whole, SST_SWAPBUFFER_CMD, 1 | (2 << 1));
    let mut split = whole.clone();

    whole.advance_frame_phase(1_530);
    for lines in [479, 1, 200, 325, 524, 1] {
        split.advance_frame_phase(lines);
    }

    assert_eq!(split, whole);
    assert_eq!(whole.scanout_argb(), vec![0x0000_00ff]);
    assert_eq!(read_reg(&whole, SST_STATUS) & 0x380, 0);
}

#[test]
fn voodoo_lfb_writes_convert_argb8888_to_the_selected_back_buffer() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    distira.write_lfb_u32(0, 0x0034_5678);
    distira.swap_buffers();

    let frame = distira.scanout_argb();
    assert_eq!(frame, vec![0x0031_557b, 0x0000_0000]);
}

#[test]
fn voodoo_fifo_drains_queued_register_and_lfb_writes_in_order() {
    let mut direct = Distira::new();
    direct.set_frame_size(2, 1);
    write_reg(&mut direct, SST_CLIP_LEFT_RIGHT, 2);
    write_reg(&mut direct, SST_CLIP_LOW_Y_HIGH_Y, 1);
    write_reg(&mut direct, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut direct, SST_COLOR1, 0x0011_2233);
    write_reg(&mut direct, SST_FASTFILL_CMD, 1);
    write_reg(
        &mut direct,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    direct.write_lfb_u32(0, 0x0034_5678);
    write_reg(&mut direct, SST_SWAPBUFFER_CMD, 0);

    let mut queued = Distira::new();
    queued.set_frame_size(2, 1);
    queued.queue_register_write(SST_CLIP_LEFT_RIGHT, 2);
    queued.queue_register_write(SST_CLIP_LOW_Y_HIGH_Y, 1);
    queued.queue_register_write(SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    queued.queue_register_write(SST_COLOR1, 0x0011_2233);
    queued.queue_register_write(SST_FASTFILL_CMD, 1);
    queued.queue_register_write(SST_LFB_MODE, LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK);
    queued.queue_lfb_write_u32(0, 0x0034_5678);
    queued.queue_register_write(SST_SWAPBUFFER_CMD, 0);

    assert_eq!(queued.fifo_depth(), 8);
    assert!(!queued.fifo_is_empty());
    assert!(!queued.fifo_is_full());
    assert_ne!(read_reg(&queued, SST_STATUS) & 0x380, 0);

    queued.drain_fifo();

    assert!(queued.fifo_is_empty());
    assert_eq!(read_reg(&queued, SST_STATUS) & 0x380, 0);
    assert_eq!(queued.scanout_argb(), direct.scanout_argb());
}

#[test]
fn motherboard_chip_names_are_big_distira_and_small_distira() {
    let distira = Distira::new();

    assert_eq!(
        distira.chip_names(),
        [BIG_DISTIRA_CHIP_NAME, SMALL_DISTIRA_CHIP_NAME]
    );
}

#[test]
fn triangle_rasterizes_to_the_back_buffer_with_rgb565_scanout() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    let written = distira.draw_triangle([
        DistiraVertex::rgb(0.0, 0.0, 255, 0, 0),
        DistiraVertex::rgb(3.0, 0.0, 255, 0, 0),
        DistiraVertex::rgb(0.0, 3.0, 255, 0, 0),
    ]);
    assert_eq!(written, 6);

    distira.swap_buffers();
    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[5], 0x00ff_0000);
    assert_eq!(frame[6], 0x0000_0000);
    assert_eq!(frame[8], 0x00ff_0000);
    assert_eq!(frame[9], 0x0000_0000);
}

#[test]
fn ordered_dither_changes_low_colors_by_pixel_position() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    distira.set_dither_enabled(true);

    distira.draw_triangle([
        DistiraVertex::rgb(0.0, 0.0, 7, 3, 7),
        DistiraVertex::rgb(4.0, 0.0, 7, 3, 7),
        DistiraVertex::rgb(0.0, 4.0, 7, 3, 7),
    ]);
    distira.swap_buffers();

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(frame[1], 0x0008_0408);
}

#[test]
fn dac_data_ics_probe_answers_gclk1_vclk1_vclk7_through_fbi_init2() {
    // Mirrors sst1InitDacDetectICS (dac.c): the guest addresses DAC register
    // 7 with the ICS PLL sub-register index to probe (VCLK1=0x01, VCLK7=0x07,
    // GCLK1=0x0b), then issues a read cycle against DAC register 5 (the PLL
    // port) and expects fbiInit2's readback (gated by initEnable's remap
    // bit) to answer with that sub-register's ICS5342 power-on default.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_REMAP);

    let probe = |distira: &mut Distira, pll_index: u32| -> u32 {
        // Address DAC register 7 (write cycle, no SST_DACDATA_RD) and load
        // the PLL sub-register index into it.
        write_reg(distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | pll_index);
        // Now issue a read cycle against DAC register 5 (the PLL port).
        write_reg(
            distira,
            SST_DAC_DATA,
            (5 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
        );
        read_reg(distira, SST_FBI_INIT2) & 0xff
    };

    assert_eq!(
        probe(&mut distira, 0x01),
        0x55,
        "VCLK1 should read back 0x55"
    );
    assert_eq!(
        probe(&mut distira, 0x07),
        0x71,
        "VCLK7 should read back 0x71"
    );
    assert_eq!(
        probe(&mut distira, 0x0b),
        0x79,
        "GCLK1 should read back 0x79"
    );
}

#[test]
fn dac_data_write_side_effects_are_accepted_without_special_casing() {
    // Writing an arbitrary DAC register (not the PLL port, not a read cycle)
    // stores the byte and does not panic or corrupt other DAC state; a read
    // cycle against the PLL port with an unprobed index falls through to
    // the default 0xff, matching 86Box's dac_readdata reset-then-maybe-
    // overwritten shape. This is the "accepted/ignored gracefully" contract
    // the plan calls for beyond the three known ICS registers.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_REMAP);

    write_reg(&mut distira, SST_DAC_DATA, (2 << DACDATA_ADDR_SHIFT) | 0x42);
    write_reg(&mut distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | 0x99);
    write_reg(
        &mut distira,
        SST_DAC_DATA,
        (5 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
    );
    assert_eq!(read_reg(&distira, SST_FBI_INIT2) & 0xff, 0xff);
}

#[test]
fn dac_read_cycle_returns_the_addressed_register() {
    // A read cycle against a non-PLL DAC register must answer with THAT
    // register's byte, not whatever dac_data[7] (the PLL index latch) holds.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_REMAP);

    write_reg(&mut distira, SST_DAC_DATA, (2 << DACDATA_ADDR_SHIFT) | 0x42);
    write_reg(&mut distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | 0x99);
    write_reg(
        &mut distira,
        SST_DAC_DATA,
        (2 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
    );
    assert_eq!(
        read_reg(&distira, SST_FBI_INIT2) & 0xff,
        0x42,
        "register 2 reads back its own byte"
    );
}

#[test]
fn fbi_init2_reads_raw_storage_when_remap_bit_is_clear() {
    // Without initEnable's remap bit, fbiInit2 behaves like every other
    // fbiInit register: plain byte-mergeable storage, and a DAC read cycle
    // does not leak into it.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(&mut distira, SST_FBI_INIT2, 0x0000_0200);

    write_reg(&mut distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | 0x0b);
    write_reg(
        &mut distira,
        SST_DAC_DATA,
        (5 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
    );

    assert_eq!(read_reg(&distira, SST_FBI_INIT2), 0x0000_0200);
}

#[test]
fn w_buffer_mode_orders_depth_by_nearer_reciprocal_w() {
    // FBZ_W_BUFFER (SST_WBUFFER, bit 3 of fbzMode): when selected, the depth
    // test/write path uses the iterated 1/w value instead of the
    // fixed-point Z path. Drives the same shape as the existing
    // triangle_cmd_depth_test_rejects_farther_pixels test through the W
    // registers (SST_START_W/SST_DW_DX/DY) instead of SST_START_Z, and
    // checks the nearer (larger 1/w) triangle wins under the LESSTHAN op.
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    // A small 1/w (0.01 in signed 2.30 fixed point): far away.
    write_reg(&mut distira, SST_START_W, 10_737_418);
    write_reg(&mut distira, SST_DW_DX, 0);
    write_reg(&mut distira, SST_DW_DY, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    // A larger 1/w (0.5): nearer. Must win and overwrite the far red triangle.
    write_reg(&mut distira, SST_START_W, 536_870_912);
    write_reg(&mut distira, SST_DW_DX, 0);
    write_reg(&mut distira, SST_DW_DY, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(
        frame[0], 0x0000_00ff,
        "the nearer (larger 1/w) triangle must win"
    );
    assert_eq!(frame[1], 0x0000_00ff);
}

#[test]
fn w_buffer_depth_codes_are_encoded_per_pixel_not_interpolated() {
    // The SST-1 W-buffer code is a floating-point-style encode (exponent
    // from a leading-zero count plus an inverted 12-bit mantissa). 86Box's
    // vid_voodoo_render.c iterates 1/w linearly PER PIXEL and encodes each
    // pixel's value. Encoding only the three vertices and interpolating the
    // CODE linearly is wrong: the encode is not linear, so interior pixels
    // of a large triangle get a depth that is off by thousands of codes.
    // That inverts occlusion between big polygons (Tomb Raider's room
    // geometry flickers). This test draws one wide triangle with a 1/w
    // gradient and checks the depth stored at an interior pixel against
    // the per-pixel encode.
    let mut distira = Distira::new();
    distira.set_frame_size(256, 8);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 256 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 8 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    // 1/w rises from 0.01 at x=0 to 0.95 at x=256 (signed 2.30 fixed point).
    let start_w = 10_737_418u32; // 0.01 * 2^30
    let dw_dx = 3_942_646u32; // (0.95 - 0.01) / 256 * 2^30
    write_reg(&mut distira, SST_START_W, start_w);
    write_reg(&mut distira, SST_DW_DX, dw_dx);
    write_reg(&mut distira, SST_DW_DY, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);

    // 86Box's per-pixel wfloat encode, over a .32 fixed-point 1/w.
    let wfloat_code = |w: f64| -> u16 {
        let fixed = (w * 4294967296.0) as u64;
        if fixed & 0xffff_0000_0000 != 0 {
            return 0;
        }
        if fixed & 0xffff_0000 == 0 {
            return 0xf001;
        }
        let exp = ((fixed >> 16) as u16).leading_zeros();
        let mant = ((!fixed as u32) >> (19 - exp)) & 0xfff;
        ((exp << 12) + mant + 1).min(0xffff) as u16
    };

    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    for x in [32u32, 64, 128, 192] {
        let stored = distira.read_lfb_u16((x as usize) * 2);
        // The rasteriser samples at the pixel centre (x + 0.5, y = 0.5).
        let w = f64::from(start_w) / 1_073_741_824.0
            + f64::from(dw_dx) / 1_073_741_824.0 * (f64::from(x) + 0.5);
        let expected = wfloat_code(w);
        let error = (i32::from(stored) - i32::from(expected)).abs();
        assert!(
            error <= 2,
            "x={x}: stored depth code {stored} but the per-pixel encode of \
             1/w={w:.5} is {expected} (error {error} codes)"
        );
    }
}

#[test]
fn z_buffer_mode_is_unaffected_by_the_w_buffer_wiring() {
    // Regression guard: adding W-buffer support must not change Z-buffer
    // behavior when FBZ_W_BUFFER is clear. Same shape as the existing
    // triangle_cmd_depth_test_rejects_farther_pixels_and_counts_failures
    // test, kept here as a direct before/after comparison point.
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_Z, 0x0100 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_Z, 0x0200 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_ne!(read_reg(&distira, SST_FBI_ZFUNC_FAIL), 0);
}

#[test]
fn v_retrace_and_status_bit_toggle_so_a_poll_loop_terminates() {
    // SST_V_RETRACE/SST_HV_RETRACE/SST_STATUS's vsync bit (bit 6, per
    // 86Box's vid_voodoo.c SST_status handler: "temp |= 0x40" when NOT in
    // retrace) were previously hardcoded, which would hang a real
    // grSstVRetrace()-style poll loop forever on whichever edge it waits
    // for. Advancing the device's frame-phase clock must move the beam
    // through both a "not retracing" and a "retracing" phase, so a guest
    // polling loop waiting on either edge observes it and terminates.
    let mut distira = Distira::new();
    distira.set_frame_size(64, 48);

    let mut saw_not_retracing = (read_reg(&distira, SST_STATUS) & 0x40) != 0;
    let mut saw_retracing = (read_reg(&distira, SST_STATUS) & 0x40) == 0;
    let initial_v_retrace = read_reg(&distira, SST_V_RETRACE);
    let mut v_retrace_changed = false;
    let mut hv_retrace_nonzero = false;

    for _ in 0..2000 {
        distira.advance_frame_phase(10_000);
        let status = read_reg(&distira, SST_STATUS);
        if status & 0x40 != 0 {
            saw_not_retracing = true;
        } else {
            saw_retracing = true;
        }
        let v_retrace = read_reg(&distira, SST_V_RETRACE);
        let hv_retrace = read_reg(&distira, SST_HV_RETRACE);
        assert_eq!(hv_retrace & 0x1fff, v_retrace);
        assert_eq!(hv_retrace >> 16, 0);
        if v_retrace != initial_v_retrace {
            v_retrace_changed = true;
        }
        if hv_retrace != 0 {
            hv_retrace_nonzero = true;
        }
    }

    assert!(
        saw_not_retracing,
        "the beam must spend time outside retrace"
    );
    assert!(saw_retracing, "the beam must spend time inside retrace");
    assert!(
        v_retrace_changed,
        "SST_V_RETRACE must advance, not stay fixed"
    );
    assert!(
        hv_retrace_nonzero,
        "SST_HV_RETRACE must report a nonzero line/time value at some point"
    );
}

#[test]
fn lfb_aperture_wraps_its_unused_high_address_bit() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);
    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_WRITE_FRONT | LFB_FORMAT_RGB565,
    );
    distira.write_lfb_u16(1 << 21, 0xf800);
    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000]);
}

#[test]
fn lfb_physical_addresses_past_two_megabytes_are_open_bus() {
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(&mut distira, SST_FBI_INIT1, 13 << FBIINIT1_TILES_IN_X_SHIFT);
    write_reg(
        &mut distira,
        SST_FBI_INIT2,
        247 << FBIINIT2_BUFFER_OFFSET_SHIFT,
    );
    write_reg(&mut distira, SST_VIDEO_DIMENSIONS, (600 << 16) | 799);
    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH | LFB_WRITE_FRONT | LFB_READ_AUX,
    );

    let aperture_offset = (100 << 11) | (128 << 1);
    distira.write_lfb_u16(aperture_offset, 0xdead);

    assert_eq!(
        distira.read_lfb_u16(aperture_offset),
        0xffff,
        "the 800x600 auxiliary buffer starts near the end of installed memory"
    );
}

#[test]
fn the_census_counts_every_frame_size_distira_is_given() {
    // Distira had register-level unit tests only until 2026-08-29 and no game
    // had ever driven it. This census is the first instrument that answers
    // whether a real title reached it at all.
    use izarravm_video::DistiraCensusKey;

    let mut distira = Distira::new();
    distira.set_frame_size(640, 480);
    distira.set_frame_size(640, 480);
    distira.set_frame_size(512, 384);

    let rows: Vec<_> = distira
        .census()
        .entries()
        .map(|(key, count)| (*key, *count))
        .collect();
    assert_eq!(rows.len(), 2, "two distinct sizes");
    assert_eq!(
        rows[0].0,
        DistiraCensusKey {
            width: 512,
            height: 384
        }
    );
    assert_eq!(rows[1].1, 2, "640x480 was set twice");
}

#[test]
fn the_census_records_the_clamped_size_distira_actually_used() {
    // set_frame_size clamps to the device maximum. The census records what the
    // device ENDED UP in, not what the guest asked for, which is the same
    // contract the VGA census keeps: it reports effective geometry.
    let mut distira = Distira::new();
    distira.set_frame_size(u32::MAX, u32::MAX);

    let (key, _) = distira
        .census()
        .entries()
        .map(|(key, count)| (*key, *count))
        .next()
        .expect("a frame size was recorded");
    assert_eq!(key.width, distira.display().width);
    assert_eq!(key.height, distira.display().height);
}

#[test]
fn the_census_records_the_video_dimensions_register_a_real_driver_writes() {
    // THE PATH THAT MATTERS. DISTIRA_REG_FB_WIDTH/HEIGHT are this chip's private
    // interface and no period Glide driver writes them; videoDimensions is the
    // SST-1 register a real one uses. Hooking only the private path made the
    // census read EMPTY for Tomb Raider Gold's 3dfx build while its presented
    // frame was 640x480 -- an instrument that answers the same way whether the
    // guest reached Distira or not is not evidence.
    let mut distira = Distira::new();
    write_reg(&mut distira, SST_VIDEO_DIMENSIONS, (480 << 16) | 639);

    let rows: Vec<_> = distira
        .census()
        .entries()
        .map(|(key, count)| (*key, *count))
        .collect();
    assert_eq!(rows.len(), 1, "one geometry, one row");
    assert_eq!(rows[0].0.width, 640, "639 in the register means 640 pixels");
    assert_eq!(rows[0].0.height, 480);
}
