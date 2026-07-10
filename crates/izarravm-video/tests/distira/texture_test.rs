// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn triangle_cmd_samples_rgb565_texture_when_texture_path_is_enabled() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_uses_s_texture_gradient_for_nearest_rgb565_sampling() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_DS_DX: usize = 0x054;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_ONE: u32 = 1 << 14;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, 0);
    write_reg(&mut distira, SST_START_T, 0);
    write_reg(&mut distira, SST_DS_DX, TEX_COORD_ONE);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x0000_ff00);
}

#[test]
fn ftriangle_cmd_uses_float_s_texture_gradient_for_nearest_rgb565_sampling() {
    const SST_FSTART_S: usize = 0x0b4;
    const SST_FSTART_T: usize = 0x0b8;
    const SST_FDS_DX: usize = 0x0d4;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_S, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_T, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FDS_DX, 1.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x0000_ff00);
}

#[test]
fn triangle_cmd_bilinear_filters_rgb565_texels() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEXTUREMODE_BILINEAR_FILTER: u32 = 0x2;
    const TEX_COORD_HALF: u32 = 1 << 13;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_BILINEAR_FILTER,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, TEX_COORD_HALF);
    write_reg(&mut distira, SST_START_T, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x007b_7d00);
}

#[test]
fn triangle_cmd_selects_rgb565_mip_level_from_tlod_min() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const LOD1_MIN: u32 = 1 << 2;
    const RGB565_LOD1_APERTURE: usize = 1 << 17;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD1_MIN);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_APERTURE, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD1_MIN);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_clamps_rgb565_mip_level_to_tlod_max() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const LOD2_MIN: u32 = 2 << 2;
    const LOD1_MAX: u32 = 1 << 8;
    const RGB565_LOD1_APERTURE: usize = 1 << 17;
    const RGB565_LOD2_APERTURE: usize = 2 << 17;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD2_MIN | LOD1_MAX);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_APERTURE, 0x07e0_07e0));
    assert!(distira.queue_texture_write_u32(RGB565_LOD2_APERTURE, 0x001f_001f));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD2_MIN | LOD1_MAX);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_selects_rgb565_multibase_lod_address() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_TEX_BASE_ADDR1: usize = 0x310;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const LOD1_MIN: u32 = 1 << 2;
    const LOD_TMULTIBASEADDR: u32 = 1 << 24;
    const TEX_R5G6B5: u32 = 0x0a;
    const RGB565_LOD1_APERTURE: usize = 1 << 17;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD1_MIN | LOD_TMULTIBASEADDR);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    assert!(distira.queue_texture_write_u32(0, 0x001f_001f));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_APERTURE, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD1_MIN | LOD_TMULTIBASEADDR);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_selects_split_odd_multibase_lod_address() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_TEX_BASE_ADDR1: usize = 0x310;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const LOD_ODD: u32 = 1 << 18;
    const LOD_SPLIT: u32 = 1 << 19;
    const LOD_TMULTIBASEADDR: u32 = 1 << 24;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD_SPLIT | LOD_ODD | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD_SPLIT | LOD_ODD | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_applies_rgb565_s_wider_aspect_ratio() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const LOD_S_IS_WIDER: u32 = 1 << 20;
    const ASPECT_2_TO_1: u32 = 1 << 21;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_130: u32 = 130 << 14;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD_S_IS_WIDER | ASPECT_2_TO_1);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32((2 * 256) * 2, 0x07e0_07e0));
    assert!(distira.queue_texture_write_u32((130 * 256) * 2, 0xf800_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD_S_IS_WIDER | ASPECT_2_TO_1);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, 0);
    write_reg(&mut distira, SST_START_T, TEX_COORD_130);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_applies_texture_detail_blend_factor() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TDETAIL: usize = 0x308;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const TREX0: usize = 0x2 << 10;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TC_ZERO_OTHER: u32 = 1 << 12;
    const TC_SUB_CLOCAL: u32 = 1 << 13;
    const TC_MSELECT_DETAIL: u32 = 4 << 14;
    const TC_ADD_CLOCAL: u32 = 1 << 18;
    const TEX_R5G6B5: u32 = 0x0a;
    const DETAIL_MAX_128: u32 = 0x80;
    const DETAIL_BIAS_32: u32 = 32 << 8;
    const DETAIL_SCALE_2: u32 = 2 << 14;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        TREX0 | SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TC_ZERO_OTHER | TC_SUB_CLOCAL | TC_MSELECT_DETAIL | TC_ADD_CLOCAL,
    );
    write_reg(
        &mut distira,
        TREX0 | SST_TDETAIL,
        DETAIL_MAX_128 | DETAIL_BIAS_32 | DETAIL_SCALE_2,
    );
    assert_eq!(
        read_reg(&distira, SST_TEXTURE_MODE),
        (TEX_R5G6B5 << 8) | TC_ZERO_OTHER | TC_SUB_CLOCAL | TC_MSELECT_DETAIL | TC_ADD_CLOCAL,
    );
    assert_eq!(
        read_reg(&distira, SST_TDETAIL),
        DETAIL_MAX_128 | DETAIL_BIAS_32 | DETAIL_SCALE_2,
    );
    write_reg(&mut distira, TREX0 | SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_8200);
}

#[test]
fn triangle_cmd_clamps_rgb565_s_texture_coordinate() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEXTUREMODE_TCLAMPS: u32 = 1 << 6;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_300: u32 = 300 << 14;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_TCLAMPS,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(44 * 2, 0x001f_001f));
    assert!(distira.queue_texture_write_u32(254 * 2, 0x07e0_001f));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_TCLAMPS,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, TEX_COORD_300);
    write_reg(&mut distira, SST_START_T, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_mirrors_rgb565_s_texture_coordinate() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const LOD_TMIRROR_S: u32 = 1 << 28;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_300: u32 = 300 << 14;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD_TMIRROR_S);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(44 * 2, 0x001f_001f));
    assert!(distira.queue_texture_write_u32(211 * 2, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD_TMIRROR_S);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, TEX_COORD_300);
    write_reg(&mut distira, SST_START_T, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_samples_rgb332_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_RGB332: u32 = 0x00;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_00e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_RGB332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_i8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_I8: u32 = 0x03;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0080));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_I8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_a8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_A8: u32 = 0x02;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0080));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_A8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_ai44_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_AI8: u32 = 0x04;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0008));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_AI8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x008c_8a8c);
}

#[test]
fn triangle_cmd_samples_ai88_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_A8I8: u32 = 0x0d;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff80));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_A8I8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_argb8332_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ffe0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB8332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_argb1555_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_ARGB1555: u32 = 0x0b;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_fc00));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB1555 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_argb4444_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_ARGB4444: u32 = 0x0c;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff00));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB4444 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_pal8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_NCC_TABLE0_Q2: usize = 0x34c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_PAL8: u32 = 0x05;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x80ff_0000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_PAL8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_apal8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_NCC_TABLE0_Q2: usize = 0x34c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_APAL8: u32 = 0x06;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x8003_f000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_APAL8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_apal88_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_NCC_TABLE0_Q2: usize = 0x34c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_APAL88: u32 = 0x0e;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff00));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x80ff_0000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_APAL88 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_yiq_ncc_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_NCC_TABLE0_I1: usize = 0x338;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_Y4I2Q2: u32 = 0x01;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 4));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_I1, 255 << 18);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_Y4I2Q2 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_a8_yiq_ncc_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_NCC_TABLE0_I1: usize = 0x338;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_A8Y4I2Q2: u32 = 0x09;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff04));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_I1, 255 << 18);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_A8Y4I2Q2 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}
