// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_video::{
    BIG_DISTIRA_CHIP_NAME, DACDATA_ADDR_SHIFT, DACDATA_RD, DEPTHOP_ALWAYS, DEPTHOP_LESSTHAN,
    Distira, DistiraVertex, FBZ_CHROMAKEY, FBZ_CLIP_ENABLE, FBZ_DEPTH_ENABLE, FBZ_DEPTH_OP_SHIFT,
    FBZ_DEPTH_WMASK, FBZ_DRAW_BACK, FBZ_RGB_WMASK, FBZ_W_BUFFER, INIT_ENABLE_REMAP,
    LFB_FORMAT_ARGB8888, LFB_WRITE_BACK, SMALL_DISTIRA_CHIP_NAME, SST_ALPHA_MODE, SST_CHROMA_KEY,
    SST_CLIP_LEFT_RIGHT, SST_CLIP_LOW_Y_HIGH_Y, SST_COLOR1, SST_DAC_DATA, SST_DR_DX, SST_DR_DY,
    SST_DW_DX, SST_DW_DY, SST_FASTFILL_CMD, SST_FBI_INIT0, SST_FBI_INIT1, SST_FBI_INIT2,
    SST_FBI_INIT3, SST_FBI_INIT7, SST_FBI_ZFUNC_FAIL, SST_FBZ_COLOR_PATH, SST_FBZ_MODE, SST_FDR_DX,
    SST_FDR_DY, SST_FDZ_DX, SST_FOG_COLOR, SST_FOG_MODE, SST_FSTART_B, SST_FSTART_G, SST_FSTART_R,
    SST_FSTART_Z, SST_FTRIANGLE_CMD, SST_FVERTEX_AX, SST_FVERTEX_AY, SST_FVERTEX_BX,
    SST_FVERTEX_BY, SST_FVERTEX_CX, SST_FVERTEX_CY, SST_HV_RETRACE, SST_LFB_MODE, SST_START_B,
    SST_START_G, SST_START_R, SST_START_W, SST_START_Z, SST_STATUS, SST_SWAPBUFFER_CMD,
    SST_TRIANGLE_CMD, SST_V_RETRACE, SST_VERTEX_AX, SST_VERTEX_AY, SST_VERTEX_BX, SST_VERTEX_BY,
    SST_VERTEX_CX, SST_VERTEX_CY,
};

fn read_reg(distira: &Distira, reg: usize) -> u32 {
    (0..4)
        .map(|i| u32::from(distira.read_mmio_u8(reg + i)) << (i * 8))
        .fold(0, |a, b| a | b)
}

fn write_reg(distira: &mut Distira, reg: usize, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        distira.write_mmio_u8(reg + i, byte);
    }
}

fn cmdfifo_type5_header(space: u32, count: u32) -> u32 {
    (space << 30) | (count << 3) | 5
}

fn red_channel(pixel: u32) -> u32 {
    (pixel >> 16) & 0xff
}

#[path = "distira/device_test.rs"]
mod device;

#[path = "distira/raster_alpha_test.rs"]
mod raster_alpha;

#[path = "distira/color_combine_test.rs"]
mod color_combine;

#[path = "distira/texture_test.rs"]
mod texture;
