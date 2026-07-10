// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const SST_TEXTURE_MODE: usize = 0x300;
const SST_TEX_BASE_ADDR: usize = 0x30c;
const SST_NCC_TABLE0_I0: usize = 0x334;
const SST_NCC_TABLE1_Y1: usize = 0x358;
const SST_NCC_TABLE1_I0: usize = 0x364;
const SST_NCC_TABLE1_I2: usize = 0x36c;
const SST_NCC_TABLE1_Q0: usize = 0x374;
const SST_NCC_TABLE1_Q3: usize = 0x380;
const TREX0: usize = 0x2 << 10;
const TREX1: usize = 0x4 << 10;
const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
const TEX_Y4I2Q2: u32 = 0x01;
const TNCCSELECT: u32 = 1 << 5;

fn ncc_device(texel: u8) -> Distira {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, u32::from(texel)));
    distira.drain_fifo();
    distira
}

fn render_ncc(distira: &mut Distira, table1: bool) -> u32 {
    let table = if table1 { TNCCSELECT } else { 0 };
    write_reg(distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(distira, SST_TEXTURE_MODE, (TEX_Y4I2Q2 << 8) | table);
    write_reg(distira, SST_TEX_BASE_ADDR, 0);
    write_reg(distira, SST_VERTEX_AX, 0 << 4);
    write_reg(distira, SST_VERTEX_AY, 0 << 4);
    write_reg(distira, SST_VERTEX_BX, 3 << 4);
    write_reg(distira, SST_VERTEX_BY, 0 << 4);
    write_reg(distira, SST_VERTEX_CX, 0 << 4);
    write_reg(distira, SST_VERTEX_CY, 3 << 4);
    write_reg(distira, SST_START_R, 0xff << 12);
    write_reg(distira, SST_START_G, 0xff << 12);
    write_reg(distira, SST_START_B, 0xff << 12);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
    write_reg(distira, SST_SWAPBUFFER_CMD, 0);
    distira.scanout_argb()[0]
}

#[test]
fn texture_mode_selects_between_independent_ncc_tables() {
    let mut programmed = ncc_device(0);
    write_reg(&mut programmed, SST_NCC_TABLE0_I0, 255 << 18);
    write_reg(&mut programmed, SST_NCC_TABLE1_Q0, 255);

    let mut table0 = programmed.clone();
    assert_eq!(render_ncc(&mut table0, false), 0x00ff_0000);
    assert_eq!(render_ncc(&mut programmed, true), 0x0000_00ff);
}

#[test]
fn table1_y_i_and_q_register_groups_feed_the_decoder() {
    let mut distira = ncc_device(0x5b);
    write_reg(&mut distira, SST_NCC_TABLE1_Y1, 32 << 8);
    write_reg(&mut distira, SST_NCC_TABLE1_I2, 223 << 18);
    write_reg(&mut distira, SST_NCC_TABLE1_Q3, 223 << 9);

    assert_eq!(render_ncc(&mut distira, true), 0x00ff_ff21);
}

#[test]
fn new_tables_are_clear_and_tmu_targeted_writes_stay_local() {
    let mut fresh = ncc_device(0);
    assert_eq!(render_ncc(&mut fresh, true), 0);

    let mut tmu1_only = ncc_device(0);
    write_reg(&mut tmu1_only, TREX1 | SST_NCC_TABLE1_I0, 255 << 18);
    assert_eq!(render_ncc(&mut tmu1_only, true), 0);

    let mut tmu0 = ncc_device(0);
    write_reg(&mut tmu0, TREX0 | SST_NCC_TABLE1_I0, 255 << 18);
    assert_eq!(render_ncc(&mut tmu0, true), 0x00ff_0000);
}
