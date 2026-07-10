// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn triangle_cmd_selects_color1_over_texture_color_path() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_LOCALSELECT_COLOR1: u32 = 2;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_LOCALSELECT_COLOR1,
    );
    write_reg(&mut distira, SST_COLOR1, 0x00ff_0000);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_selects_lfb_over_texture_color_path() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_LOCALSELECT_LFB: u32 = 3;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_LOCALSELECT_LFB,
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
}

#[test]
fn triangle_cmd_adds_color0_local_to_texture_color_path() {
    const SST_COLOR0: usize = 0x144;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_LOCALSELECT_COLOR0: u32 = 1 << 4;
    const CC_ADD_CLOCAL: u32 = 1 << 14;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_LOCALSELECT_COLOR0 | CC_ADD_CLOCAL,
    );
    write_reg(&mut distira, SST_COLOR0, 0x00ff_0000);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_ff00);
}

#[test]
fn triangle_cmd_subtracts_color0_local_from_texture_color_path() {
    const SST_COLOR0: usize = 0x144;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_LOCALSELECT_COLOR0: u32 = 1 << 4;
    const CC_SUB_CLOCAL: u32 = 1 << 9;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0xffe0_ffe0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_LOCALSELECT_COLOR0 | CC_SUB_CLOCAL,
    );
    write_reg(&mut distira, SST_COLOR0, 0x00ff_0000);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_reverse_blends_color0_local_with_texture_color_path() {
    const SST_COLOR0: usize = 0x144;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_LOCALSELECT_COLOR0: u32 = 1 << 4;
    const CC_MSELECT_CLOCAL: u32 = 1 << 10;
    const CC_REVERSE_BLEND: u32 = 1 << 13;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_LOCALSELECT_COLOR0 | CC_MSELECT_CLOCAL | CC_REVERSE_BLEND,
    );
    write_reg(&mut distira, SST_COLOR0, 0x0000_8000);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_8200);
}

#[test]
fn triangle_cmd_nonreverse_blends_color0_local_with_texture_color_path() {
    const SST_COLOR0: usize = 0x144;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_LOCALSELECT_COLOR0: u32 = 1 << 4;
    const CC_MSELECT_CLOCAL: u32 = 1 << 10;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_LOCALSELECT_COLOR0 | CC_MSELECT_CLOCAL,
    );
    write_reg(&mut distira, SST_COLOR0, 0x0000_8000);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_7d00);
}

#[test]
fn triangle_cmd_inverts_texture_color_path_output() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_INVERT_OUTPUT: u32 = 1 << 16;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_INVERT_OUTPUT,
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_00ff);
}

#[test]
fn triangle_cmd_adds_alocal_to_texture_color_path() {
    const SST_START_A: usize = 0x030;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_ADD_ALOCAL: u32 = 2 << 14;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_ADD_ALOCAL,
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0042_ff42);
}

#[test]
fn triangle_cmd_modulates_texture_color_path_by_aother() {
    const SST_START_A: usize = 0x030;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_MSELECT_AOTHER: u32 = 2 << 10;
    const CC_REVERSE_BLEND: u32 = 1 << 13;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_MSELECT_AOTHER | CC_REVERSE_BLEND,
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_4100);
}

#[test]
fn triangle_cmd_modulates_texture_color_path_by_alocal() {
    const SST_START_A: usize = 0x030;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_MSELECT_ALOCAL: u32 = 3 << 10;
    const CC_REVERSE_BLEND: u32 = 1 << 13;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_MSELECT_ALOCAL | CC_REVERSE_BLEND,
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_A, 0x40 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_4100);
}

#[test]
fn triangle_cmd_modulates_texture_color_path_by_texture_alpha() {
    const SST_START_A: usize = 0x030;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_MSELECT_TEX_ALPHA: u32 = 4 << 10;
    const CC_REVERSE_BLEND: u32 = 1 << 13;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_ARGB4444: u32 = 0x0c;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x40f0_40f0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_MSELECT_TEX_ALPHA | CC_REVERSE_BLEND,
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB4444 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_A, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_4500);
}

#[test]
fn triangle_cmd_modulates_color1_path_by_texture_rgb() {
    const SST_COLOR1_LOCAL: usize = 0x148;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const RGB_SELECT_COLOR1: u32 = 2;
    const CC_MSELECT_TEX_RGB: u32 = 5 << 10;
    const CC_REVERSE_BLEND: u32 = 1 << 13;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | RGB_SELECT_COLOR1 | CC_MSELECT_TEX_RGB | CC_REVERSE_BLEND,
    );
    write_reg(&mut distira, SST_COLOR1_LOCAL, 0x00ff_ffff);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 0);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_zero_other_zeros_texture_color_path() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const CC_ZERO_OTHER: u32 = 1 << 8;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | CC_ZERO_OTHER,
    );
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
    assert_eq!(frame[0], 0x0000_0000);
}
