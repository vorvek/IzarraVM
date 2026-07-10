// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(Clone, Copy)]
struct BilinearFormat {
    name: &'static str,
    format: u32,
    bytes_per_texel: usize,
    black: u16,
    white: u16,
    has_alpha: bool,
}

const BILINEAR_FORMATS: [BilinearFormat; 14] = [
    BilinearFormat {
        name: "RGB332",
        format: 0x00,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0xff,
        has_alpha: false,
    },
    BilinearFormat {
        name: "YIQ",
        format: 0x01,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0xf0,
        has_alpha: false,
    },
    BilinearFormat {
        name: "A8",
        format: 0x02,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0xff,
        has_alpha: true,
    },
    BilinearFormat {
        name: "I8",
        format: 0x03,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0xff,
        has_alpha: false,
    },
    BilinearFormat {
        name: "AI44",
        format: 0x04,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0xff,
        has_alpha: true,
    },
    BilinearFormat {
        name: "PAL8",
        format: 0x05,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0x01,
        has_alpha: false,
    },
    BilinearFormat {
        name: "APAL8",
        format: 0x06,
        bytes_per_texel: 1,
        black: 0x00,
        white: 0x01,
        has_alpha: true,
    },
    BilinearFormat {
        name: "ARGB8332",
        format: 0x08,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xffff,
        has_alpha: true,
    },
    BilinearFormat {
        name: "A8YIQ",
        format: 0x09,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xfff0,
        has_alpha: true,
    },
    BilinearFormat {
        name: "RGB565",
        format: 0x0a,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xffff,
        has_alpha: false,
    },
    BilinearFormat {
        name: "ARGB1555",
        format: 0x0b,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xffff,
        has_alpha: true,
    },
    BilinearFormat {
        name: "ARGB4444",
        format: 0x0c,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xffff,
        has_alpha: true,
    },
    BilinearFormat {
        name: "A8I8",
        format: 0x0d,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xffff,
        has_alpha: true,
    },
    BilinearFormat {
        name: "APAL88",
        format: 0x0e,
        bytes_per_texel: 2,
        black: 0x0000,
        white: 0xff01,
        has_alpha: true,
    },
];

fn render_bilinear_format(case: BilinearFormat, alpha_probe: bool) -> u32 {
    const BILINEAR: u32 = 1 << 1;
    const A_SELECT_TEXTURE: u32 = 1 << 2;
    const ALPHA_GREATER_THAN_200: u32 = 1 | (4 << 1) | (200 << 24);
    const SST_NCC_TABLE0_Y3: usize = 0x330;
    const SST_NCC_TABLE0_Q2: usize = 0x34c;
    const SST_NCC_TABLE0_Q3: usize = 0x350;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (case.format << 8) | TEXTUREMODE_LOCAL | BILINEAR,
    );
    if matches!(case.format, 0x01 | 0x09) {
        write_reg(&mut distira, SST_NCC_TABLE0_Y3, 0xff00_0000);
    }
    if matches!(case.format, 0x05 | 0x06 | 0x0e) {
        write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x8000_0000);
        write_reg(&mut distira, SST_NCC_TABLE0_Q3, 0x80ff_ffff);
    }
    let pair = |left: u16, right: u16| {
        if case.bytes_per_texel == 1 {
            u32::from(left as u8) | (u32::from(right as u8) << 8)
        } else {
            u32::from(left) | (u32::from(right) << 16)
        }
    };
    distira.write_texture_u32(0, pair(case.black, case.white));
    distira.write_texture_u32(
        1 << 9,
        pair(
            case.white,
            if alpha_probe { case.white } else { case.black },
        ),
    );

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED
            | if alpha_probe {
                RGB_SELECT_COLOR1 | A_SELECT_TEXTURE
            } else {
                RGB_SELECT_TEXTURE
            },
    );
    if alpha_probe {
        write_reg(&mut distira, SST_COLOR1, 0xffff_0000);
        write_reg(&mut distira, SST_ALPHA_MODE, ALPHA_GREATER_THAN_200);
    }
    write_reg(&mut distira, SST_START_S, 1 << 18);
    write_reg(&mut distira, SST_START_T, 1 << 18);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);
    distira.scanout_argb()[0]
}

#[test]
fn triangle_cmd_samples_rgb565_texture_when_texture_path_is_enabled() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_ONE: u32 = 1 << 18;

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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;

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
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
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
fn triangle_cmd_bilinear_centers_texels_at_half_coordinates() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEXTUREMODE_BILINEAR_FILTER: u32 = 0x2;
    const TEX_COORD_HALF: u32 = 1 << 17;

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
        (TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL) | TEXTUREMODE_BILINEAR_FILTER,
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
    write_reg(&mut distira, SST_START_S, TEX_COORD_HALF);
    write_reg(&mut distira, SST_START_T, TEX_COORD_HALF);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_bilinear_blends_four_rgb565_neighbors_at_integer_coordinates() {
    const TEXTUREMODE_BILINEAR_FILTER: u32 = 0x2;
    const TEX_COORD_ONE: u32 = 1 << 18;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_LOCAL | TEXTUREMODE_BILINEAR_FILTER,
    );
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    assert!(distira.queue_texture_write_u32(256 * 2, 0xffff_001f));
    distira.drain_fifo();
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        (1 << 27) | RGB_SELECT_TEXTURE,
    );
    write_reg(&mut distira, SST_START_S, TEX_COORD_ONE);
    write_reg(&mut distira, SST_START_T, TEX_COORD_ONE);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    assert_eq!(distira.scanout_argb()[0], 0x007b_7d7b);
}

#[test]
fn bilinear_filter_blends_decoded_rgb_for_every_sst_texture_format() {
    for case in BILINEAR_FORMATS {
        assert_eq!(
            render_bilinear_format(case, false),
            0x007b_7d7b,
            "{}",
            case.name
        );
    }
}

#[test]
fn bilinear_filter_blends_alpha_for_every_alpha_texture_format() {
    for case in BILINEAR_FORMATS.into_iter().filter(|case| case.has_alpha) {
        assert_eq!(render_bilinear_format(case, true), 0, "{}", case.name);
    }
}

#[test]
fn triangle_cmd_selects_rgb565_mip_level_from_tlod_min() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;
    const LOD1_MIN: u32 = 1 << 2;
    const LOD1_MAX: u32 = 1 << 8;
    const RGB565_LOD1_APERTURE: usize = 1 << 17;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD1_MIN | LOD1_MAX);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_APERTURE, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD1_MIN | LOD1_MAX);
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_R5G6B5: u32 = 0x0a;
    const LOD2_MIN: u32 = 2 << 2;
    const LOD1_MAX: u32 = 1 << 8;
    const RGB565_LOD1_APERTURE: usize = 1 << 17;
    const RGB565_LOD2_APERTURE: usize = 2 << 17;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD2_MIN | LOD1_MAX);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_APERTURE, 0x07e0_07e0));
    assert!(distira.queue_texture_write_u32(RGB565_LOD2_APERTURE, 0x001f_001f));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD2_MIN | LOD1_MAX);
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const LOD1_MIN: u32 = 1 << 2;
    const LOD1_MAX: u32 = 1 << 8;
    const LOD_TMULTIBASEADDR: u32 = 1 << 24;
    const TEX_R5G6B5: u32 = 0x0a;
    const RGB565_LOD1_APERTURE: usize = 1 << 17;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD1_MIN | LOD1_MAX | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    assert!(distira.queue_texture_write_u32(0, 0x001f_001f));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_APERTURE, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD1_MIN | LOD1_MAX | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn multibase_lod_keeps_cumulative_offset_from_lod_zero() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_TEX_BASE_ADDR1: usize = 0x310;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const LOD1_MIN: u32 = 1 << 2;
    const LOD1_MAX: u32 = 1 << 8;
    const LOD_TMULTIBASEADDR: u32 = 1 << 24;
    const TEX_R5G6B5: u32 = 0x0a;
    const LOD0_BYTES: u32 = 256 * 256 * 2;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, LOD0_BYTES >> 3);
    distira.write_texture_u32(0, 0x07e0_07e0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD1_MIN | LOD1_MAX | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    assert_eq!(distira.scanout_argb()[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_selects_split_odd_multibase_lod_address() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const SST_TEX_BASE_ADDR1: usize = 0x310;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const LOD1_MAX: u32 = 1 << 8;
    const LOD_ODD: u32 = 1 << 18;
    const LOD_SPLIT: u32 = 1 << 19;
    const LOD_TMULTIBASEADDR: u32 = 1 << 24;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD1_MAX | LOD_SPLIT | LOD_ODD | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    assert!(distira.queue_texture_write_u32(1 << 17, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(
        &mut distira,
        SST_TLOD,
        LOD1_MAX | LOD_SPLIT | LOD_ODD | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_TEX_BASE_ADDR1, 1);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const LOD_S_IS_WIDER: u32 = 1 << 20;
    const ASPECT_2_TO_1: u32 = 1 << 21;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_130: u32 = 130 << 18;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD_S_IS_WIDER | ASPECT_2_TO_1);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32((2 * 256) * 2, 0x07e0_07e0));
    assert!(distira.queue_texture_write_u32((130 * 256) * 2, 0xf800_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD_S_IS_WIDER | ASPECT_2_TO_1);
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
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
        (TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL)
            | TC_ZERO_OTHER
            | TC_SUB_CLOCAL
            | TC_MSELECT_DETAIL
            | TC_ADD_CLOCAL,
    );
    write_reg(
        &mut distira,
        TREX0 | SST_TDETAIL,
        DETAIL_MAX_128 | DETAIL_BIAS_32 | DETAIL_SCALE_2,
    );
    assert_eq!(
        read_reg(&distira, SST_TEXTURE_MODE),
        (TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL)
            | TC_ZERO_OTHER
            | TC_SUB_CLOCAL
            | TC_MSELECT_DETAIL
            | TC_ADD_CLOCAL,
    );
    assert_eq!(
        read_reg(&distira, SST_TDETAIL),
        DETAIL_MAX_128 | DETAIL_BIAS_32 | DETAIL_SCALE_2,
    );
    write_reg(&mut distira, TREX0 | SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_7d00);
}

#[test]
fn triangle_cmd_clamps_rgb565_s_texture_coordinate() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEXTUREMODE_TCLAMPS: u32 = 1 << 6;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_300: u32 = 300 << 18;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL) | TEXTUREMODE_TCLAMPS,
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
        (TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL) | TEXTUREMODE_TCLAMPS,
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const LOD_TMIRROR_S: u32 = 1 << 28;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_300: u32 = 300 << 18;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD_TMIRROR_S);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    assert!(distira.queue_texture_write_u32(44 * 2, 0x001f_001f));
    assert!(distira.queue_texture_write_u32(211 * 2, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_R5G6B5 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(&mut distira, SST_TLOD, LOD_TMIRROR_S);
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
    write_reg(&mut distira, SST_START_S, TEX_COORD_300);
    write_reg(&mut distira, SST_START_T, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn bilinear_mirror_reflects_coordinate_before_half_texel_offset() {
    const BILINEAR: u32 = 1 << 1;
    const LOD_TMIRROR_S: u32 = 1 << 28;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_LOCAL | BILINEAR,
    );
    write_reg(&mut distira, SST_TLOD, LOD_TMIRROR_S);
    distira.write_texture_u32(0, 0x0000_07e0);
    distira.write_texture_u32(0x1fc, 0xf800_0000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );
    write_reg(&mut distira, SST_START_S, 256 << 18);
    write_reg(&mut distira, SST_START_T, 1 << 17);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    assert_eq!(distira.scanout_argb()[0], 0x008c_6d00);
}

#[test]
fn triangle_cmd_samples_rgb332_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_RGB332: u32 = 0x00;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_00e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_RGB332 << 8 | TEXTUREMODE_LOCAL,
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_i8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_I8: u32 = 0x03;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0080));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_I8 << 8 | TEXTUREMODE_LOCAL,
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_a8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_A8: u32 = 0x02;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0080));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_ai44_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_AI8: u32 = 0x04;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0008));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_AI8 << 8 | TEXTUREMODE_LOCAL,
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x008c_8a8c);
}

#[test]
fn triangle_cmd_samples_ai88_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_A8I8: u32 = 0x0d;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff80));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_A8I8 << 8 | TEXTUREMODE_LOCAL,
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_argb8332_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ffe0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_argb1555_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB1555: u32 = 0x0b;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_fc00));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB1555 << 8 | TEXTUREMODE_LOCAL,
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_argb4444_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_ARGB4444: u32 = 0x0c;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff00));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_ARGB4444 << 8 | TEXTUREMODE_LOCAL,
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_PAL8: u32 = 0x05;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x80ff_0000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_PAL8 << 8 | TEXTUREMODE_LOCAL,
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_APAL8: u32 = 0x06;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x8003_f000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_APAL8 << 8 | TEXTUREMODE_LOCAL,
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_APAL88: u32 = 0x0e;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff00));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_Q2, 0x80ff_0000);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_APAL88 << 8 | TEXTUREMODE_LOCAL,
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_Y4I2Q2: u32 = 0x01;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 4));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_I1, 255 << 18);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_Y4I2Q2 << 8 | TEXTUREMODE_LOCAL,
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
    const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
    const TEX_A8Y4I2Q2: u32 = 0x09;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff04));
    distira.drain_fifo();

    write_reg(&mut distira, SST_NCC_TABLE0_I1, 255 << 18);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        TEX_A8Y4I2Q2 << 8 | TEXTUREMODE_LOCAL,
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

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

fn render_two_tmu_pair(mode0: u32, mode1: u32, texels0: u32, texels1: u32) -> [u32; 2] {
    const TREX0: usize = 2 << 10;
    const TREX1: usize = 4 << 10;
    const TMU1_APERTURE: usize = 1 << 21;
    const TEX_COORD_ONE: u32 = 1 << 18;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    assert!(distira.queue_texture_write_u32(0, texels0));
    assert!(distira.queue_texture_write_u32(TMU1_APERTURE, texels1));
    distira.drain_fifo();
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE,
    );
    write_reg(&mut distira, TREX0 | SST_TEXTURE_MODE, mode0);
    write_reg(&mut distira, TREX1 | SST_TEXTURE_MODE, mode1);
    for chip in [TREX0, TREX1] {
        write_reg(&mut distira, chip | SST_START_S, 0);
        write_reg(&mut distira, chip | SST_START_T, 0);
        write_reg(&mut distira, chip | SST_DS_DX, TEX_COORD_ONE);
    }
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 4 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 4 << 4);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    [frame[0], frame[1]]
}

#[test]
fn tmu0_mode_selects_local_passthrough_and_modulated_texture_paths() {
    const TEX_ARGB4444: u32 = 0x0c;
    const FORMAT: u32 = TEX_ARGB4444 << 8;
    const TC_MSELECT_CLOCAL: u32 = 1 << 14;
    const TC_REVERSE_BLEND: u32 = 1 << 17;
    const RED: u32 = 0xff00_ff00;
    const GREEN: u32 = 0xf0f0_f0f0;
    const WHITE: u32 = 0xffff_ffff;

    assert_eq!(
        render_two_tmu_pair(FORMAT | TEXTUREMODE_LOCAL, FORMAT, RED, GREEN),
        [0x00ff_0000; 2]
    );
    assert_eq!(
        render_two_tmu_pair(FORMAT, FORMAT | TEXTUREMODE_LOCAL, RED, GREEN),
        [0x0000_ff00; 2]
    );
    assert_eq!(
        render_two_tmu_pair(
            FORMAT | TC_MSELECT_CLOCAL | TC_REVERSE_BLEND,
            FORMAT | TEXTUREMODE_LOCAL,
            RED,
            WHITE,
        ),
        [0x00ff_0000; 2]
    );
}

#[test]
fn tmu_color_factors_use_the_current_other_and_local_texel_alpha() {
    const TEX_ARGB4444: u32 = 0x0c;
    const FORMAT: u32 = TEX_ARGB4444 << 8;
    const TC_MSELECT_AOTHER: u32 = 2 << 14;
    const TC_MSELECT_ALOCAL: u32 = 3 << 14;
    const TC_REVERSE_BLEND: u32 = 1 << 17;
    const OPAQUE_WHITE: u32 = 0xffff_ffff;
    const ALPHA_RAMP_WHITE: u32 = 0xffff_0fff;

    assert_eq!(
        render_two_tmu_pair(
            FORMAT | TC_MSELECT_AOTHER | TC_REVERSE_BLEND,
            FORMAT | TEXTUREMODE_LOCAL,
            OPAQUE_WHITE,
            ALPHA_RAMP_WHITE,
        ),
        [0, 0x00ff_ffff]
    );
    assert_eq!(
        render_two_tmu_pair(
            FORMAT | TC_MSELECT_ALOCAL | TC_REVERSE_BLEND,
            FORMAT | TEXTUREMODE_LOCAL,
            ALPHA_RAMP_WHITE,
            OPAQUE_WHITE,
        ),
        [0, 0x00ff_ffff]
    );
}

#[test]
fn tmu_add_alocal_uses_the_current_local_texel_alpha() {
    const TEX_ARGB4444: u32 = 0x0c;
    const FORMAT: u32 = TEX_ARGB4444 << 8;
    const TC_ZERO_OTHER: u32 = 1 << 12;
    const TC_ADD_ALOCAL: u32 = 1 << 19;
    const ALPHA_RAMP_WHITE: u32 = 0xffff_0fff;

    assert_eq!(
        render_two_tmu_pair(
            FORMAT | TC_ZERO_OTHER | TC_ADD_ALOCAL,
            FORMAT | TEXTUREMODE_LOCAL,
            ALPHA_RAMP_WHITE,
            0,
        ),
        [0, 0x00ff_ffff]
    );
}
