// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const SST_START_S: usize = 0x034;
const SST_START_T: usize = 0x038;
const SST_START_W: usize = 0x03c;
const SST_DS_DX: usize = 0x054;
const SST_DT_DX: usize = 0x058;
const SST_DW_DX: usize = 0x05c;
const SST_DS_DY: usize = 0x074;
const SST_DT_DY: usize = 0x078;
const SST_DW_DY: usize = 0x07c;
const SST_FSTART_S: usize = 0x0b4;
const SST_FSTART_T: usize = 0x0b8;
const SST_FSTART_W: usize = 0x0bc;
const SST_TEXTURE_MODE: usize = 0x300;
const SST_TLOD: usize = 0x304;
const SST_TEX_BASE_ADDR38: usize = 0x318;
const TREX0: usize = 2 << 10;
const TREX1: usize = 4 << 10;
const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
const TEXTUREMODE_TPERSP_ST: u32 = 1;
const TMU0_ADD_TMU1: u32 = 1 << 18;
const TEX_R5G6B5: u32 = 0x0a;
const LOD_ODD: u32 = 1 << 18;
const LOD_SPLIT: u32 = 1 << 19;
const LOD_TMULTIBASEADDR: u32 = 1 << 24;
const LOD8_MAX: u32 = (8 * 4) << 6;
const ST_ONE: u32 = 1 << 18;
const W_ONE: u32 = 1 << 30;

fn textured_triangle() -> Distira {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_VERTEX_AX, 0);
    write_reg(&mut distira, SST_VERTEX_AY, 0);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0);
    write_reg(&mut distira, SST_VERTEX_CX, 0);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    distira
}

fn set_iterator(distira: &mut Distira, chip: usize, s: u32, ds_dx: u32, w: u32) {
    write_reg(distira, chip | SST_START_S, s);
    write_reg(distira, chip | SST_START_T, 0);
    write_reg(distira, chip | SST_START_W, w);
    write_reg(distira, chip | SST_DS_DX, ds_dx);
    write_reg(distira, chip | SST_DT_DX, 0);
    write_reg(distira, chip | SST_DW_DX, 0);
    write_reg(distira, chip | SST_DS_DY, 0);
    write_reg(distira, chip | SST_DT_DY, 0);
    write_reg(distira, chip | SST_DW_DY, 0);
}

fn draw_first_pixel(distira: &mut Distira) -> u32 {
    write_reg(distira, SST_TRIANGLE_CMD, 1);
    write_reg(distira, SST_SWAPBUFFER_CMD, 0);
    distira.scanout_argb()[0]
}

fn render_affine_contract(perspective: bool) -> u32 {
    let mut distira = textured_triangle();
    let mode = (TEX_R5G6B5 << 8) | (u32::from(perspective) * TEXTUREMODE_TPERSP_ST);
    write_reg(&mut distira, TREX0 | SST_TEXTURE_MODE, mode);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();
    set_iterator(&mut distira, TREX0, 0, ST_ONE, W_ONE / 2);
    draw_first_pixel(&mut distira)
}

#[test]
fn texture_mode_bit_zero_selects_affine_or_perspective_coordinates() {
    assert_eq!(render_affine_contract(false), 0x00ff_0000);
    assert_eq!(render_affine_contract(true), 0x0000_ff00);
}

#[test]
fn float_iterators_keep_values_beyond_the_fixed_w_range() {
    let mut distira = textured_triangle();
    write_reg(
        &mut distira,
        TREX0 | SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_TPERSP_ST,
    );
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    assert!(distira.queue_texture_write_u32(4, 0x001f_001f));
    distira.drain_fifo();
    write_reg(&mut distira, TREX0 | SST_FSTART_S, 4.0_f32.to_bits());
    write_reg(&mut distira, TREX0 | SST_FSTART_T, 0.0_f32.to_bits());
    write_reg(&mut distira, TREX0 | SST_FSTART_W, 4.0_f32.to_bits());

    assert_eq!(draw_first_pixel(&mut distira), 0x0000_ff00);
}

fn render_mip_for_reciprocal_w(reciprocal_w: u32) -> u32 {
    let mut distira = textured_triangle();
    write_reg(
        &mut distira,
        TREX0 | SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_TPERSP_ST,
    );
    write_reg(&mut distira, TREX0 | SST_TLOD, LOD8_MAX);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(2 << 17, 0x001f_001f));
    distira.drain_fifo();
    set_iterator(&mut distira, TREX0, 0, ST_ONE, reciprocal_w);
    draw_first_pixel(&mut distira)
}

#[test]
fn reciprocal_w_selects_near_and_far_mip_levels_from_the_gradient() {
    assert_eq!(render_mip_for_reciprocal_w(W_ONE), 0x00ff_0000);
    assert_eq!(render_mip_for_reciprocal_w(W_ONE / 4), 0x0000_00ff);
}

fn render_mip_with_bias(bias: u32) -> u32 {
    let mut distira = textured_triangle();
    write_reg(&mut distira, TREX0 | SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, TREX0 | SST_TLOD, LOD8_MAX | bias);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(1 << 17, 0x07e0_07e0));
    distira.drain_fifo();
    set_iterator(&mut distira, TREX0, 0, ST_ONE, W_ONE);
    draw_first_pixel(&mut distira)
}

#[test]
fn tlod_bias_offsets_the_gradient_selected_mip_level() {
    assert_eq!(render_mip_with_bias(0), 0x00ff_0000);
    assert_eq!(render_mip_with_bias(4 << 12), 0x0000_ff00);
}

#[test]
fn split_odd_multibase_packs_later_mips_after_owned_levels_only() {
    const LOD5_MIN: u32 = 5 * 4;
    const LOD5_MAX: u32 = (5 * 4) << 6;

    let mut distira = textured_triangle();
    write_reg(&mut distira, TREX0 | SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(
        &mut distira,
        TREX0 | SST_TLOD,
        LOD5_MIN | LOD5_MAX | LOD_SPLIT | LOD_ODD | LOD_TMULTIBASEADDR,
    );
    write_reg(&mut distira, TREX0 | SST_TEX_BASE_ADDR38, 1);
    assert!(distira.queue_texture_write_u32(3 << 17, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(5 << 17, 0x07e0_07e0));
    distira.drain_fifo();
    set_iterator(&mut distira, TREX0, 0, 0, W_ONE);

    assert_eq!(draw_first_pixel(&mut distira), 0x0000_ff00);
}

#[test]
fn two_tmus_use_independent_perspective_and_lod_iterators() {
    let mut distira = textured_triangle();
    write_reg(
        &mut distira,
        TREX0 | SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_TPERSP_ST | TMU0_ADD_TMU1,
    );
    write_reg(
        &mut distira,
        TREX1 | SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_TPERSP_ST,
    );
    write_reg(&mut distira, TREX0 | SST_TLOD, LOD8_MAX);
    write_reg(&mut distira, TREX1 | SST_TLOD, LOD8_MAX);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(1 << 21, 0x001f_001f));
    assert!(distira.queue_texture_write_u32((1 << 21) | (1 << 17), 0x07e0_07e0,));
    distira.drain_fifo();
    set_iterator(&mut distira, TREX0, 0, ST_ONE, W_ONE);
    set_iterator(&mut distira, TREX1, 0, ST_ONE, W_ONE / 2);

    assert_eq!(draw_first_pixel(&mut distira), 0x00ff_ff00);
}
