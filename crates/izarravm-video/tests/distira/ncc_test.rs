// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const SST_TEXTURE_MODE: usize = 0x300;
const SST_TEX_BASE_ADDR: usize = 0x30c;
const SST_NCC_TABLE0_I0: usize = 0x334;
const SST_NCC_TABLE0_I1: usize = 0x338;
const SST_NCC_TABLE0_I2: usize = 0x33c;
const SST_NCC_TABLE0_I3: usize = 0x340;
const SST_NCC_TABLE0_Q0: usize = 0x344;
const SST_NCC_TABLE0_Q1: usize = 0x348;
const SST_NCC_TABLE0_Q2: usize = 0x34c;
const SST_NCC_TABLE0_Q3: usize = 0x350;
const SST_NCC_TABLE1_Y1: usize = 0x358;
const SST_NCC_TABLE1_I0: usize = 0x364;
const SST_NCC_TABLE1_I2: usize = 0x36c;
const SST_NCC_TABLE1_Q0: usize = 0x374;
const SST_NCC_TABLE1_Q3: usize = 0x380;
const TREX0: usize = 0x2 << 10;
const TREX1: usize = 0x4 << 10;
const FBZCP_TEXTURE_ENABLED: u32 = (1 << 27) | RGB_SELECT_TEXTURE;
const TEX_Y4I2Q2: u32 = 0x01;
const TEX_PAL8: u32 = 0x05;
const TNCCSELECT: u32 = 1 << 5;
const TABLE0_IQ_ALIASES: [usize; 8] = [
    SST_NCC_TABLE0_I0,
    SST_NCC_TABLE0_I1,
    SST_NCC_TABLE0_I2,
    SST_NCC_TABLE0_I3,
    SST_NCC_TABLE0_Q0,
    SST_NCC_TABLE0_Q1,
    SST_NCC_TABLE0_Q2,
    SST_NCC_TABLE0_Q3,
];

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
    write_reg(
        distira,
        SST_TEXTURE_MODE,
        (TEX_Y4I2Q2 << 8 | TEXTUREMODE_LOCAL) | table,
    );
    write_reg(distira, SST_TEX_BASE_ADDR, 0);
    write_reg(distira, SST_VERTEX_AX, 0 << 4);
    write_reg(distira, SST_VERTEX_AY, 0 << 4);
    write_reg(distira, SST_VERTEX_BX, 4 << 4);
    write_reg(distira, SST_VERTEX_BY, 0 << 4);
    write_reg(distira, SST_VERTEX_CX, 0 << 4);
    write_reg(distira, SST_VERTEX_CY, 4 << 4);
    write_reg(distira, SST_START_R, 0xff << 12);
    write_reg(distira, SST_START_G, 0xff << 12);
    write_reg(distira, SST_START_B, 0xff << 12);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
    write_reg(distira, SST_SWAPBUFFER_CMD, 0);
    distira.scanout_argb()[0]
}

fn render_pal8(distira: &mut Distira, tmu: usize, index: u8) -> u32 {
    let chip = if tmu == 0 { TREX0 } else { TREX1 };
    let aperture = tmu << 21;
    let texel = u32::from(index) * 0x0101_0101;

    write_reg(distira, TREX0 | SST_TEXTURE_MODE, 0);
    write_reg(
        distira,
        chip | SST_TEXTURE_MODE,
        TEX_PAL8 << 8 | TEXTUREMODE_LOCAL,
    );
    write_reg(distira, chip | SST_TEX_BASE_ADDR, 0);
    distira.write_texture_u32(aperture, texel);
    write_reg(distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(distira, SST_VERTEX_AX, 0);
    write_reg(distira, SST_VERTEX_AY, 0);
    write_reg(distira, SST_VERTEX_BX, 4 << 4);
    write_reg(distira, SST_VERTEX_BY, 0);
    write_reg(distira, SST_VERTEX_CX, 0);
    write_reg(distira, SST_VERTEX_CY, 4 << 4);
    write_reg(distira, SST_START_R, 0xff << 12);
    write_reg(distira, SST_START_G, 0xff << 12);
    write_reg(distira, SST_START_B, 0xff << 12);
    write_reg(distira, SST_TRIANGLE_CMD, 0);
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

#[test]
fn every_table0_iq_alias_programs_pal8_on_both_tmus() {
    for tmu in 0..2 {
        let chip = if tmu == 0 { TREX0 } else { TREX1 };
        for (alias_index, register) in TABLE0_IQ_ALIASES.into_iter().enumerate() {
            let index = 0x20 + alias_index as u8 * 2 + (alias_index as u8 & 1);
            let palette_write = (1 << 31) | (u32::from(index & 0xfe) << 23) | 0x00ff_0000;
            let mut distira = Distira::new();
            distira.set_frame_size(4, 4);
            write_reg(&mut distira, chip | register, palette_write);

            assert_eq!(
                render_pal8(&mut distira, tmu, index),
                0x00ff_0000,
                "TMU {tmu}, table0 alias {register:#05x}"
            );
        }
    }
}

#[test]
fn table0_palette_aliases_keep_independent_partial_write_latches() {
    let red: u32 = (1 << 31) | 0x00ff_0000;
    let green: u32 = (1 << 31) | (2 << 23) | 0x0000_ff00;
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);

    for byte in 0..3 {
        distira.write_mmio_u8(TREX0 | SST_NCC_TABLE0_I0 | byte, red.to_le_bytes()[byte]);
        distira.write_mmio_u8(TREX0 | SST_NCC_TABLE0_I2 | byte, green.to_le_bytes()[byte]);
    }
    distira.write_mmio_u8(TREX0 | SST_NCC_TABLE0_I0 | 3, red.to_le_bytes()[3]);
    distira.write_mmio_u8(TREX0 | SST_NCC_TABLE0_I2 | 3, green.to_le_bytes()[3]);

    let mut red_device = distira.clone();
    assert_eq!(render_pal8(&mut red_device, 0, 0), 0x00ff_0000);
    assert_eq!(render_pal8(&mut distira, 0, 2), 0x0000_ff00);
}
