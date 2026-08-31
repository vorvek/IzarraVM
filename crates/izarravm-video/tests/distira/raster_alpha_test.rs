// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn triangle_cmd_rasterizes_flat_untextured_triangle_from_integer_registers() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[5], 0x00ff_0000);
    assert_eq!(frame[6], 0x0000_0000);
    assert_eq!(frame[8], 0x00ff_0000);
}

#[test]
fn triangle_cmd_honors_the_clip_rectangle_when_enabled() {
    // fbzMode bit 0 enables the clip rectangle for rendering; fastfill
    // already uses it as its extent, triangles must intersect with it.
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_CLIP_ENABLE,
    );
    write_reg(&mut distira, SST_CLIP_LEFT_RIGHT, (1 << 16) | 3);
    write_reg(&mut distira, SST_CLIP_LOW_Y_HIGH_Y, (1 << 16) | 3);
    // Triangle large enough to cover the whole 4x4 target.
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 8 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 8 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0, "outside clip (0,0) untouched");
    assert_eq!(frame[3], 0, "outside clip (3,0) untouched");
    assert_eq!(frame[4], 0, "outside clip (0,1) untouched");
    assert_eq!(frame[5], 0x00ff_0000, "inside clip (1,1) filled");
    assert_eq!(frame[10], 0x00ff_0000, "inside clip (2,2) filled");
    assert_eq!(frame[15], 0, "outside clip (3,3) untouched");
}

#[test]
fn triangle_cmd_applies_integer_gouraud_color_gradients() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_DR_DX, 85 << 12);
    write_reg(&mut distira, SST_DR_DY, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert!(red_channel(frame[0]) < red_channel(frame[1]));
    assert!(red_channel(frame[1]) < red_channel(frame[2]));
    assert!(red_channel(frame[8]) < red_channel(frame[2]));
    assert_eq!(frame[3], 0x0000_0000);
}

#[test]
fn ftriangle_cmd_rasterizes_flat_untextured_triangle_from_float_registers() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[5], 0x00ff_0000);
    assert_eq!(frame[6], 0x0000_0000);
    assert_eq!(frame[8], 0x00ff_0000);
}

#[test]
fn triangle_cmd_depth_test_rejects_farther_pixels_and_counts_failures() {
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
    assert_ne!(read_reg(&mut distira, SST_FBI_ZFUNC_FAIL), 0);
}

#[test]
fn triangle_depth_bias_is_signed_clamped_and_used_for_test_and_write() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_Z, 100 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_DEPTH_BIAS
            | (DEPTHOP_GREATERTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_ZA_COLOR, 20);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_Z, 90 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    assert_eq!(distira.read_lfb_u16(0), 110);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_DEPTH_BIAS
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_ZA_COLOR, u32::from((-20_i16) as u16));
    write_reg(&mut distira, SST_START_Z, 10 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    assert_eq!(distira.read_lfb_u16(0), 0);

    write_reg(&mut distira, SST_ZA_COLOR, 20);
    write_reg(&mut distira, SST_START_Z, 65_530 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    assert_eq!(distira.read_lfb_u16(0), u16::MAX);

    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);
    assert_eq!(distira.scanout_argb()[0], 0x0000_ff00);
}

#[test]
fn depth_source_compares_za_but_writes_biased_interpolated_depth() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_Z, 100 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_DEPTH_BIAS
            | FBZ_DEPTH_SOURCE
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_ZA_COLOR, 20);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_Z, 90 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);

    write_reg(&mut distira, SST_LFB_MODE, LFB_READ_AUX);
    assert_eq!(distira.read_lfb_u16(0), 110);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);
    assert_eq!(distira.scanout_argb()[0], 0x0000_00ff);
}

#[test]
fn ftriangle_cmd_applies_float_gouraud_color_gradients() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FDR_DX, 85.0f32.to_bits());
    write_reg(&mut distira, SST_FDR_DY, 0.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert!(red_channel(frame[0]) < red_channel(frame[1]));
    assert!(red_channel(frame[1]) < red_channel(frame[2]));
    assert!(red_channel(frame[8]) < red_channel(frame[2]));
    assert_eq!(frame[3], 0x0000_0000);
}

#[test]
fn ftriangle_cmd_depth_test_accepts_closer_float_z() {
    const DEPTH_LESS_THAN: u32 = DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT;
    const DEPTH_ALWAYS: u32 = DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK | DEPTH_ALWAYS,
    );
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_Z, 256.0f32.to_bits());
    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK | DEPTH_LESS_THAN,
    );
    write_reg(&mut distira, SST_FSTART_R, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_Z, 512.0f32.to_bits());
    write_reg(&mut distira, SST_FDZ_DX, (-170.0f32).to_bits());
    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[2], 0x0000_00ff);
}

#[test]
fn triangle_cmd_alpha_test_rejects_pixels_below_reference() {
    const SST_START_A: usize = 0x030;
    const SST_DA_DX: usize = 0x050;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (90 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
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
    write_reg(&mut distira, SST_START_A, 0);
    write_reg(&mut distira, SST_DA_DX, 100 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_ne!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_test_uses_texture_alpha_when_selected() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_A8: u32 = 0x02;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x4040_4040));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_A8 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_zero_other_rejects_texture_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_ZERO_OTHER: u32 = 1 << 17;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_ZERO_OTHER,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_subtracts_local_from_texture_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_SUB_CLOCAL: u32 = 1 << 18;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x801c_801c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_SUB_CLOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_modulates_texture_alpha_by_local_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_MSELECT_ALOCAL: u32 = 1 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_MSELECT_ALOCAL
            | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_modulates_texture_alpha_by_local_alpha_2() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_MSELECT_ALOCAL2: u32 = 3 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_MSELECT_ALOCAL2
            | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_modulates_texture_alpha_by_other_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_MSELECT_AOTHER: u32 = 2 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x801c_801c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_MSELECT_AOTHER
            | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_modulates_iterated_alpha_by_texture_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_CCA_MSELECT_TEX: u32 = 4 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x401c_401c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_CCA_MSELECT_TEX | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_adds_local_alpha_to_texture_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_ADD_ALOCAL: u32 = 2 << 23;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x401c_401c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_ADD_ALOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_adds_local_alpha_with_saturation() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_ADD_ALOCAL: u32 = 2 << 23;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xc01c_c01c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_ADD_ALOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (0xf0 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x80 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_subtracts_before_adding_local_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_SUB_CLOCAL: u32 = 1 << 18;
    const FBZCP_CCA_ADD_ALOCAL: u32 = 2 << 23;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x201c_201c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_SUB_CLOCAL | FBZCP_CCA_ADD_ALOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (0x30 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn untextured_alpha_path_selects_color_registers_and_combines_them() {
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        (A_SELECT_COLOR1 << FBZCP_A_SELECT_SHIFT)
            | (CCA_LOCALSELECT_COLOR0 << FBZCP_CCA_LOCALSELECT_SHIFT)
            | (2 << FBZCP_CCA_ADD_SHIFT),
    );
    write_reg(&mut distira, SST_COLOR0, 0x4000_0000);
    write_reg(&mut distira, SST_COLOR1, 0x2000_0000);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (0x50 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    assert_eq!(distira.scanout_argb()[0], 0x0000_ff00);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_subtracts_then_modulates_then_adds_local_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_SUB_CLOCAL: u32 = 1 << 18;
    const FBZCP_CCA_MSELECT_ALOCAL: u32 = 1 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_CCA_ADD_ALOCAL: u32 = 2 << 23;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x801c_801c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_SUB_CLOCAL
            | FBZCP_CCA_MSELECT_ALOCAL
            | FBZCP_CCA_REVERSE_BLEND
            | FBZCP_CCA_ADD_ALOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (0x48 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_adds_local_alpha_for_clocal_add_mode() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_ADD_CLOCAL: u32 = 1 << 23;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x401c_401c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_ADD_CLOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_inverts_texture_alpha_output() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_INVERT_OUTPUT: u32 = 1 << 25;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x001c_001c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_INVERT_OUTPUT,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_nonreverse_modulates_by_inverted_local_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_MSELECT_ALOCAL: u32 = 1 << 19;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX | FBZCP_CCA_MSELECT_ALOCAL,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xbf << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_selects_color1_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_COLOR1: u32 = 2 << 2;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_COLOR1,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(&mut distira, SST_COLOR1, 0x0012_3456);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_selects_color0_as_local_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_COLOR0: usize = 0x144;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_LOCALSELECT_COLOR0: u32 = 1 << 5;
    const FBZCP_CCA_MSELECT_ALOCAL: u32 = 1 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_LOCALSELECT_COLOR0
            | FBZCP_CCA_MSELECT_ALOCAL
            | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(&mut distira, SST_COLOR0, 0x0012_3456);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn triangle_cmd_alpha_selects_iter_z_as_local_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_LOCALSELECT_ITER_Z: u32 = 2 << 5;
    const FBZCP_CCA_MSELECT_ALOCAL: u32 = 1 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_LOCALSELECT_ITER_Z
            | FBZCP_CCA_MSELECT_ALOCAL
            | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_Z, 0);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn ftriangle_cmd_alpha_selects_float_iter_z_as_local_alpha() {
    const SST_FSTART_A: usize = 0x0b0;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_CCA_LOCALSELECT_ITER_Z: u32 = 2 << 5;
    const FBZCP_CCA_MSELECT_ALOCAL: u32 = 1 << 19;
    const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xff1c_ff1c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | FBZCP_A_SELECT_TEX
            | FBZCP_CCA_LOCALSELECT_ITER_Z
            | FBZCP_CCA_MSELECT_ALOCAL
            | FBZCP_CCA_REVERSE_BLEND,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_Z, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_A, 255.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 6);
}

#[test]
fn ftriangle_cmd_alpha_test_uses_float_alpha_derivatives() {
    const SST_FSTART_A: usize = 0x0b0;
    const SST_FDA_DX: usize = 0x0d0;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (90 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 4.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_A, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FDA_DX, 100.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_ne!(read_reg(&mut distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_blends_source_over_destination() {
    const SST_START_A: usize = 0x030;
    const AFUNC_ASRC_ALPHA: u32 = 1;
    const AFUNC_AOMSRC_ALPHA: u32 = 5;
    const ALPHA_BLEND_ENABLE: u32 = 1 << 4;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        ALPHA_BLEND_ENABLE | (AFUNC_ASRC_ALPHA << 8) | (AFUNC_AOMSRC_ALPHA << 12),
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
    write_reg(&mut distira, SST_START_A, 128 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_007b);
    assert_eq!(frame[3], 0x0000_00ff);
}

#[test]
fn triangle_cmd_alpha_blends_texture_alpha_over_destination() {
    const SST_START_A: usize = 0x030;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const AFUNC_ASRC_ALPHA: u32 = 1;
    const AFUNC_AOMSRC_ALPHA: u32 = 5;
    const ALPHA_BLEND_ENABLE: u32 = 1 << 4;
    const FBZCP_A_SELECT_TEX: u32 = 1 << 2;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x401c_401c));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | FBZCP_A_SELECT_TEX,
    );
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        ALPHA_BLEND_ENABLE | (AFUNC_ASRC_ALPHA << 8) | (AFUNC_AOMSRC_ALPHA << 12),
    );
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB8332 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_41bd);
}

#[test]
fn triangle_cmd_chroma_key_rejects_matching_source_color() {
    const SST_FBI_CHROMA_FAIL: usize = 0x150;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_CHROMAKEY,
    );
    write_reg(&mut distira, SST_CHROMA_KEY, 0x00ff_0000);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_CHROMA_FAIL), 6);
}

#[test]
fn triangle_cmd_chroma_key_rejects_matching_texture_color() {
    const SST_FBI_CHROMA_FAIL: usize = 0x150;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    distira.drain_fifo();

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_CHROMAKEY,
    );
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_CHROMA_KEY, 0x00ff_0000);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_CHROMA_FAIL), 6);
}

#[test]
fn triangle_cmd_chroma_key_checks_cother_before_color_combining() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    distira.drain_fifo();

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_CHROMAKEY,
    );
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | RGB_SELECT_TEXTURE
            | FBZCP_CC_LOCALSELECT_COLOR0
            | FBZCP_CC_ADD_CLOCAL,
    );
    write_reg(&mut distira, SST_CHROMA_KEY, 0x00ff_0000);
    write_reg(&mut distira, SST_COLOR0, 0x0000_00ff);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    assert_eq!(distira.scanout_argb()[0], 0x0000_00ff);
    assert_eq!(read_reg(&mut distira, SST_FBI_CHROMA_FAIL), 6);
}

#[test]
fn triangle_cmd_applies_constant_fog_color() {
    const FOG_ENABLE: u32 = 0x01;
    const FOG_CONSTANT: u32 = 0x20;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FOG_MODE, FOG_ENABLE | FOG_CONSTANT);
    write_reg(&mut distira, SST_FOG_COLOR, 0x0000_0033);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0031);
}

#[test]
fn triangle_cmd_applies_fog_after_texture_color() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const FOG_ENABLE: u32 = 0x01;
    const FOG_CONSTANT: u32 = 0x20;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_FOG_MODE, FOG_ENABLE | FOG_CONSTANT);
    write_reg(&mut distira, SST_FOG_COLOR, 0x0000_0033);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff31);
}

/// Tomb Raider's 3dfx build, and Glide itself, snap a screen coordinate to the
/// 4-bit subpixel grid by ADDING a magic constant of 2^19 and writing the whole
/// biased float to the vertex register. 2^19 gives the sum an ulp of 1/16, so
/// the low 16 bits of the mantissa hold the 12.4 fixed-point coordinate.
///
/// The SST-1 latches only those 16 bits: it converts the float to 28.4 and
/// TRUNCATES. A model that saturates instead turns every such vertex into
/// 32767, which collapses the triangle to a point and rejects it for zero area.
/// MEASURED before this test existed: 213,323 of Tomb Raider's 213,584
/// triangles died that way.
#[test]
fn fvertex_truncates_a_magic_snapped_coordinate_instead_of_saturating() {
    // 311.4375 + 524288.0. The low 16 bits are 0x1377 = 4983 = 311.4375 in 12.4.
    let snapped = |x: f32| (x + 524288.0).to_bits();

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FVERTEX_AX, snapped(0.0));
    write_reg(&mut distira, SST_FVERTEX_AY, snapped(0.0));
    write_reg(&mut distira, SST_FVERTEX_BX, snapped(4.0));
    write_reg(&mut distira, SST_FVERTEX_BY, snapped(0.0));
    write_reg(&mut distira, SST_FVERTEX_CX, snapped(0.0));
    write_reg(&mut distira, SST_FVERTEX_CY, snapped(4.0));
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000, "the snapped triangle must rasterise");
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000, "and must stay inside its own edges");
}
