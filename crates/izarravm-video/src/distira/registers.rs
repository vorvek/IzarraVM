// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

pub const DISTIRA_FB_SIZE: usize = 2 * 1024 * 1024;
pub const DISTIRA_MMIO_SIZE: usize = 0x0001_0000;
pub const DISTIRA_TEX_SIZE: usize = 2 * 1024 * 1024;
pub const DISTIRA_FIFO_CAPACITY: usize = 65_536;
pub const DISTIRA_ID_VALUE: u32 = 0x4454_0100; // 'D''T', version 1.00
pub const DISTIRA_MODEL_VALUE: u32 = 1;
pub const DISTIRA_TMU_COUNT: u32 = 2;
pub const BIG_DISTIRA_CHIP_NAME: &str = "BigDistira";
pub const SMALL_DISTIRA_CHIP_NAME: &str = "SmallDistira";
pub const DISTIRA_MAX_WIDTH: u32 = 640;
pub const DISTIRA_MAX_HEIGHT: u32 = 480;

/// Total scanlines per frame for the lightweight `advance_frame_phase` beam
/// counter (a 640x480@60Hz-shaped total, not tied to the live display size).
pub(super) const FRAME_PHASE_TOTAL_LINES: u32 = 525;
/// Scanlines at the bottom of the frame treated as the vertical retrace
/// window (matches a typical VGA-shaped ~8% vblank fraction).
pub(super) const FRAME_PHASE_VRETRACE_LINES: u32 = 45;

pub const DISTIRA_CAPS_TRIANGLE: u32 = 1 << 0;
pub const DISTIRA_CAPS_DITHER: u32 = 1 << 1;
pub const DISTIRA_CAPS_TMU1: u32 = 1 << 2;
pub const DISTIRA_CAPS_TMU2: u32 = 1 << 3;
pub const DISTIRA_CAPS_LFB: u32 = 1 << 4;
pub const DISTIRA_CAPS_VALUE: u32 = DISTIRA_CAPS_TRIANGLE
    | DISTIRA_CAPS_DITHER
    | DISTIRA_CAPS_TMU1
    | DISTIRA_CAPS_TMU2
    | DISTIRA_CAPS_LFB;

pub const DISTIRA_REG_ID: usize = 0xf000;
pub const DISTIRA_REG_CAPS: usize = 0xf004;
pub const DISTIRA_REG_STATUS: usize = 0xf008;
pub const DISTIRA_REG_CONTROL: usize = 0xf00c;
pub const DISTIRA_REG_MODEL: usize = 0xf010;
pub const DISTIRA_REG_FB_WIDTH: usize = 0xf020;
pub const DISTIRA_REG_FB_HEIGHT: usize = 0xf024;
pub const DISTIRA_REG_FB_PITCH: usize = 0xf028;
pub const DISTIRA_REG_FRONT_BASE: usize = 0xf02c;
pub const DISTIRA_REG_BACK_BASE: usize = 0xf030;
pub const DISTIRA_REG_CLEAR_COLOR: usize = 0xf040;
pub const DISTIRA_REG_COMMAND: usize = 0xf0fc;

pub const DISTIRA_CMD_CLEAR: u32 = 1;
pub const DISTIRA_CMD_SWAP: u32 = 2;

pub const SST_STATUS: usize = 0x000;
pub const SST_INTR_CTRL: usize = 0x004;
pub const SST_VERTEX_AX: usize = 0x008;
pub const SST_VERTEX_AY: usize = 0x00c;
pub const SST_VERTEX_BX: usize = 0x010;
pub const SST_VERTEX_BY: usize = 0x014;
pub const SST_VERTEX_CX: usize = 0x018;
pub const SST_VERTEX_CY: usize = 0x01c;
pub const SST_START_R: usize = 0x020;
pub const SST_START_G: usize = 0x024;
pub const SST_START_B: usize = 0x028;
pub const SST_START_Z: usize = 0x02c;
pub const SST_START_A: usize = 0x030;
pub const SST_START_S: usize = 0x034;
pub const SST_START_T: usize = 0x038;
pub const SST_START_W: usize = 0x03c;
pub const SST_DR_DX: usize = 0x040;
pub const SST_DG_DX: usize = 0x044;
pub const SST_DB_DX: usize = 0x048;
pub const SST_DZ_DX: usize = 0x04c;
pub const SST_DA_DX: usize = 0x050;
pub const SST_DS_DX: usize = 0x054;
pub const SST_DT_DX: usize = 0x058;
pub const SST_DW_DX: usize = 0x05c;
pub const SST_DR_DY: usize = 0x060;
pub const SST_DG_DY: usize = 0x064;
pub const SST_DB_DY: usize = 0x068;
pub const SST_DZ_DY: usize = 0x06c;
pub const SST_DA_DY: usize = 0x070;
pub const SST_DS_DY: usize = 0x074;
pub const SST_DT_DY: usize = 0x078;
pub const SST_DW_DY: usize = 0x07c;
pub const SST_TRIANGLE_CMD: usize = 0x080;
pub const SST_FVERTEX_AX: usize = 0x088;
pub const SST_FVERTEX_AY: usize = 0x08c;
pub const SST_FVERTEX_BX: usize = 0x090;
pub const SST_FVERTEX_BY: usize = 0x094;
pub const SST_FVERTEX_CX: usize = 0x098;
pub const SST_FVERTEX_CY: usize = 0x09c;
pub const SST_FSTART_R: usize = 0x0a0;
pub const SST_FSTART_G: usize = 0x0a4;
pub const SST_FSTART_B: usize = 0x0a8;
pub const SST_FSTART_Z: usize = 0x0ac;
pub const SST_FSTART_A: usize = 0x0b0;
pub const SST_FSTART_S: usize = 0x0b4;
pub const SST_FSTART_T: usize = 0x0b8;
pub const SST_FSTART_W: usize = 0x0bc;
pub const SST_FDR_DX: usize = 0x0c0;
pub const SST_FDG_DX: usize = 0x0c4;
pub const SST_FDB_DX: usize = 0x0c8;
pub const SST_FDZ_DX: usize = 0x0cc;
pub const SST_FDA_DX: usize = 0x0d0;
pub const SST_FDS_DX: usize = 0x0d4;
pub const SST_FDT_DX: usize = 0x0d8;
pub const SST_FDW_DX: usize = 0x0dc;
pub const SST_FDR_DY: usize = 0x0e0;
pub const SST_FDG_DY: usize = 0x0e4;
pub const SST_FDB_DY: usize = 0x0e8;
pub const SST_FDZ_DY: usize = 0x0ec;
pub const SST_FDA_DY: usize = 0x0f0;
pub const SST_FDS_DY: usize = 0x0f4;
pub const SST_FDT_DY: usize = 0x0f8;
pub const SST_FDW_DY: usize = 0x0fc;
pub const SST_FTRIANGLE_CMD: usize = 0x100;
pub const SST_FBZ_COLOR_PATH: usize = 0x104;
pub const SST_FOG_MODE: usize = 0x108;
pub const SST_ALPHA_MODE: usize = 0x10c;
pub const SST_FBZ_MODE: usize = 0x110;
pub const SST_LFB_MODE: usize = 0x114;
pub const SST_CLIP_LEFT_RIGHT: usize = 0x118;
pub const SST_CLIP_LOW_Y_HIGH_Y: usize = 0x11c;
pub const SST_NOP_CMD: usize = 0x120;
pub const SST_FASTFILL_CMD: usize = 0x124;
pub const SST_SWAPBUFFER_CMD: usize = 0x128;
pub const SST_FOG_COLOR: usize = 0x12c;
pub const SST_ZA_COLOR: usize = 0x130;
pub const SST_CHROMA_KEY: usize = 0x134;
pub const SST_STIPPLE: usize = 0x140;
pub const SST_COLOR0: usize = 0x144;
pub const SST_COLOR1: usize = 0x148;
pub const SST_FBI_PIXELS_IN: usize = 0x14c;
pub const SST_FBI_CHROMA_FAIL: usize = 0x150;
pub const SST_FBI_ZFUNC_FAIL: usize = 0x154;
pub const SST_FBI_AFUNC_FAIL: usize = 0x158;
pub const SST_FBI_PIXELS_OUT: usize = 0x15c;
pub const SST_CMD_FIFO_BASE_ADDR: usize = 0x1e0;
pub const SST_CMD_FIFO_BUMP: usize = 0x1e4;
pub const SST_CMD_FIFO_RD_PTR: usize = 0x1e8;
pub const SST_CMD_FIFO_AMIN: usize = 0x1ec;
pub const SST_CMD_FIFO_AMAX: usize = 0x1f0;
pub const SST_CMD_FIFO_DEPTH: usize = 0x1f4;
pub const SST_CMD_FIFO_HOLES: usize = 0x1f8;
pub const SST_FBI_INIT4: usize = 0x200;
pub const SST_V_RETRACE: usize = 0x204;
pub const SST_BACK_PORCH: usize = 0x208;
pub const SST_VIDEO_DIMENSIONS: usize = 0x20c;
pub const SST_FBI_INIT0: usize = 0x210;
pub const SST_FBI_INIT1: usize = 0x214;
pub const SST_FBI_INIT2: usize = 0x218;
pub const SST_FBI_INIT3: usize = 0x21c;
pub const SST_H_SYNC: usize = 0x220;
pub const SST_V_SYNC: usize = 0x224;
pub const SST_CLUT_DATA: usize = 0x228;
pub const SST_DAC_DATA: usize = 0x22c;
pub const SST_HV_RETRACE: usize = 0x240;
pub const SST_FBI_INIT5: usize = 0x244;
pub const SST_FBI_INIT6: usize = 0x248;
pub const SST_FBI_INIT7: usize = 0x24c;
pub const SST_TEXTURE_MODE: usize = 0x300;
pub const SST_TLOD: usize = 0x304;
pub const SST_TDETAIL: usize = 0x308;
pub const SST_TEX_BASE_ADDR: usize = 0x30c;
pub const SST_TEX_BASE_ADDR1: usize = 0x310;
pub const SST_TEX_BASE_ADDR2: usize = 0x314;
pub const SST_TEX_BASE_ADDR38: usize = 0x318;
pub const SST_TREX_INIT0: usize = 0x31c;
pub const SST_TREX_INIT1: usize = 0x320;
pub const SST_NCC_TABLE0_Y0: usize = 0x324;
pub const SST_NCC_TABLE0_Y1: usize = 0x328;
pub const SST_NCC_TABLE0_Y2: usize = 0x32c;
pub const SST_NCC_TABLE0_Y3: usize = 0x330;
pub const SST_NCC_TABLE0_I0: usize = 0x334;
pub const SST_NCC_TABLE0_I1: usize = 0x338;
pub const SST_NCC_TABLE0_I2: usize = 0x33c;
pub const SST_NCC_TABLE0_I3: usize = 0x340;
pub const SST_NCC_TABLE0_Q0: usize = 0x344;
pub const SST_NCC_TABLE0_Q1: usize = 0x348;
pub const SST_NCC_TABLE0_Q2: usize = 0x34c;
pub const SST_NCC_TABLE0_Q3: usize = 0x350;
pub const SST_NCC_TABLE1_Y0: usize = 0x354;
pub const SST_NCC_TABLE1_Y1: usize = 0x358;
pub const SST_NCC_TABLE1_Y2: usize = 0x35c;
pub const SST_NCC_TABLE1_Y3: usize = 0x360;
pub const SST_NCC_TABLE1_I0: usize = 0x364;
pub const SST_NCC_TABLE1_I1: usize = 0x368;
pub const SST_NCC_TABLE1_I2: usize = 0x36c;
pub const SST_NCC_TABLE1_I3: usize = 0x370;
pub const SST_NCC_TABLE1_Q0: usize = 0x374;
pub const SST_NCC_TABLE1_Q1: usize = 0x378;
pub const SST_NCC_TABLE1_Q2: usize = 0x37c;
pub const SST_NCC_TABLE1_Q3: usize = 0x380;

pub const LFB_WRITE_FRONT: u32 = 0x0000;
pub const LFB_WRITE_BACK: u32 = 0x0010;
pub const LFB_WRITE_MASK: u32 = 0x0030;
pub const LFB_READ_FRONT: u32 = 0x0000;
pub const LFB_READ_BACK: u32 = 0x0040;
pub const LFB_READ_AUX: u32 = 0x0080;
pub const LFB_READ_MASK: u32 = 0x00c0;

pub const LFB_FORMAT_RGB565: u32 = 0;
pub const LFB_FORMAT_RGB555: u32 = 1;
pub const LFB_FORMAT_ARGB1555: u32 = 2;
pub const LFB_FORMAT_XRGB8888: u32 = 4;
pub const LFB_FORMAT_ARGB8888: u32 = 5;
pub const LFB_FORMAT_DEPTH_RGB565: u32 = 12;
pub const LFB_FORMAT_DEPTH_RGB555: u32 = 13;
pub const LFB_FORMAT_DEPTH_ARGB1555: u32 = 14;
pub const LFB_FORMAT_DEPTH: u32 = 15;
pub const LFB_FORMAT_MASK: u32 = 15;
pub const LFB_ENABLE_PIXEL_PIPELINE: u32 = 0x100;

/// fbzMode bit 0: enable the clip rectangle for rendering (SST-1). Fastfill
/// always uses the clip registers as its extent regardless of this bit.
pub const FBZ_CLIP_ENABLE: u32 = 1 << 0;
pub const FBZ_CHROMAKEY: u32 = 1 << 1;
pub const FBZ_STIPPLE: u32 = 1 << 2;
pub const FBZ_W_BUFFER: u32 = 1 << 3;
pub const FBZ_DEPTH_ENABLE: u32 = 1 << 4;
pub const FBZ_DEPTH_OP_SHIFT: u32 = 5;
pub const FBZ_DITHER: u32 = 1 << 8;
pub const FBZ_RGB_WMASK: u32 = 1 << 9;
pub const FBZ_DEPTH_WMASK: u32 = 1 << 10;
pub const FBZ_DITHER_2X2: u32 = 1 << 11;
pub const FBZ_STIPPLE_PATT: u32 = 1 << 12;
pub const FBZ_ALPHA_MASK: u32 = 1 << 13;
pub const FBZ_DRAW_FRONT: u32 = 0x0000;
pub const FBZ_DRAW_BACK: u32 = 0x4000;
pub const FBZ_DRAW_MASK: u32 = 0xc000;
pub const FBZ_ALPHA_ENABLE: u32 = 1 << 18;
pub const FBZ_DITHER_SUB: u32 = 1 << 19;
pub const FBZ_DEPTH_SOURCE: u32 = 1 << 20;
pub const FBZ_PARAM_ADJUST: u32 = 1 << 26;
pub const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
pub const FBZCP_RGB_SELECT_MASK: u32 = 0x3;
pub const RGB_SELECT_COLOR1: u32 = 2;
pub const RGB_SELECT_LFB: u32 = 3;
pub const FBZCP_A_SELECT_SHIFT: u32 = 2;
pub const FBZCP_A_SELECT_MASK: u32 = 0x3;
pub const A_SELECT_TEX: u32 = 1;
pub const A_SELECT_COLOR1: u32 = 2;
pub const FBZCP_CC_LOCALSELECT_COLOR0: u32 = 1 << 4;
pub const FBZCP_CC_LOCALSELECT_OVERRIDE: u32 = 1 << 7;
pub const FBZCP_CCA_LOCALSELECT_SHIFT: u32 = 5;
pub const FBZCP_CCA_LOCALSELECT_MASK: u32 = 0x3;
pub const CCA_LOCALSELECT_COLOR0: u32 = 1;
pub const CCA_LOCALSELECT_ITER_Z: u32 = 2;
pub const FBZCP_CC_ZERO_OTHER: u32 = 1 << 8;
pub const FBZCP_CC_SUB_CLOCAL: u32 = 1 << 9;
pub const FBZCP_CC_MSELECT_SHIFT: u32 = 10;
pub const FBZCP_CC_MSELECT_MASK: u32 = 0x7;
pub const CC_MSELECT_CLOCAL: u32 = 1;
pub const CC_MSELECT_AOTHER: u32 = 2;
pub const CC_MSELECT_ALOCAL: u32 = 3;
pub const CC_MSELECT_TEX_ALPHA: u32 = 4;
pub const CC_MSELECT_TEX_RGB: u32 = 5;
pub const FBZCP_CC_REVERSE_BLEND: u32 = 1 << 13;
pub const FBZCP_CC_ADD_SHIFT: u32 = 14;
pub const FBZCP_CC_ADD_MASK: u32 = 0x3;
pub const FBZCP_CC_ADD_CLOCAL: u32 = 1 << 14;
pub const FBZCP_CC_ADD_ALOCAL: u32 = 2 << 14;
pub const FBZCP_CC_INVERT_OUTPUT: u32 = 1 << 16;
pub const FBZCP_CCA_ZERO_OTHER: u32 = 1 << 17;
pub const FBZCP_CCA_SUB_CLOCAL: u32 = 1 << 18;
pub const FBZCP_CCA_MSELECT_SHIFT: u32 = 19;
pub const FBZCP_CCA_MSELECT_MASK: u32 = 0x7;
pub const CCA_MSELECT_ALOCAL: u32 = 1;
pub const CCA_MSELECT_AOTHER: u32 = 2;
pub const CCA_MSELECT_ALOCAL2: u32 = 3;
pub const CCA_MSELECT_TEX_ALPHA: u32 = 4;
pub const FBZCP_CCA_REVERSE_BLEND: u32 = 1 << 22;
pub const FBZCP_CCA_ADD_SHIFT: u32 = 23;
pub const FBZCP_CCA_ADD_MASK: u32 = 0x3;
pub const FBZCP_CCA_INVERT_OUTPUT: u32 = 1 << 25;
pub const TC_ZERO_OTHER: u32 = 1 << 12;
pub const TC_SUB_CLOCAL: u32 = 1 << 13;
pub const TC_MSELECT_SHIFT: u32 = 14;
pub const TC_MSELECT_MASK: u32 = 0x7;
pub const TC_MSELECT_DETAIL: u32 = 4;
pub const TC_ADD_CLOCAL: u32 = 1 << 18;
pub const TEXTUREMODE_TPERSP_ST: u32 = 1;
pub const TEXTUREMODE_TCLAMPW: u32 = 1 << 3;
pub const TEXTUREMODE_TNCCSELECT: u32 = 1 << 5;
pub const TEXTUREMODE_TCLAMPS: u32 = 1 << 6;
pub const TEXTUREMODE_TCLAMPT: u32 = 1 << 7;
pub const TEXTUREMODE_SEQ_8_DOWNLD: u32 = 1 << 31;
pub const LOD_ODD: u32 = 1 << 18;
pub const LOD_SPLIT: u32 = 1 << 19;
pub const LOD_S_IS_WIDER: u32 = 1 << 20;
pub const LOD_TMULTIBASEADDR: u32 = 1 << 24;
pub const LOD_TMIRROR_S: u32 = 1 << 28;
pub const LOD_TMIRROR_T: u32 = 1 << 29;

pub const TEX_RGB332: u32 = 0x00;
pub const TEX_Y4I2Q2: u32 = 0x01;
pub const TEX_A8: u32 = 0x02;
pub const TEX_I8: u32 = 0x03;
pub const TEX_AI8: u32 = 0x04;
pub const TEX_PAL8: u32 = 0x05;
pub const TEX_APAL8: u32 = 0x06;
pub const TEX_ARGB8332: u32 = 0x08;
pub const TEX_A8Y4I2Q2: u32 = 0x09;
pub const TEX_R5G6B5: u32 = 0x0a;
pub const TEX_ARGB1555: u32 = 0x0b;
pub const TEX_ARGB4444: u32 = 0x0c;
pub const TEX_A8I8: u32 = 0x0d;
pub const TEX_APAL88: u32 = 0x0e;
pub(super) const CHIP_FBI: usize = 0x1;
pub(super) const CHIP_TREX0: usize = 0x2;
pub(super) const CHIP_TREX1: usize = 0x4;
pub(super) const TREXINIT1_SEND_CONFIG: u32 = 1 << 18;

pub const DEPTHOP_NEVER: u32 = 0;
pub const DEPTHOP_LESSTHAN: u32 = 1;
pub const DEPTHOP_EQUAL: u32 = 2;
pub const DEPTHOP_LESSTHANEQUAL: u32 = 3;
pub const DEPTHOP_GREATERTHAN: u32 = 4;
pub const DEPTHOP_NOTEQUAL: u32 = 5;
pub const DEPTHOP_GREATERTHANEQUAL: u32 = 6;
pub const DEPTHOP_ALWAYS: u32 = 7;

pub const AFUNC_NEVER: u32 = 0;
pub const AFUNC_LESSTHAN: u32 = 1;
pub const AFUNC_EQUAL: u32 = 2;
pub const AFUNC_LESSTHANEQUAL: u32 = 3;
pub const AFUNC_GREATERTHAN: u32 = 4;
pub const AFUNC_NOTEQUAL: u32 = 5;
pub const AFUNC_GREATERTHANEQUAL: u32 = 6;
pub const AFUNC_ALWAYS: u32 = 7;
pub const ALPHA_TEST_ENABLE: u32 = 1;
pub const ALPHA_BLEND_ENABLE: u32 = 1 << 4;
pub const ALPHA_FUNC_SHIFT: u32 = 1;
pub const ALPHA_SRC_FUNC_SHIFT: u32 = 8;
pub const ALPHA_DST_FUNC_SHIFT: u32 = 12;
pub const ALPHA_REF_SHIFT: u32 = 24;

pub const BLEND_AZERO: u32 = 0;
pub const BLEND_ASRC_ALPHA: u32 = 1;
pub const BLEND_A_COLOR: u32 = 2;
pub const BLEND_ADST_ALPHA: u32 = 3;
pub const BLEND_AONE: u32 = 4;
pub const BLEND_AOMSRC_ALPHA: u32 = 5;
pub const BLEND_AOM_COLOR: u32 = 6;
pub const BLEND_AOMDST_ALPHA: u32 = 7;
pub const BLEND_ASATURATE: u32 = 0xf;

pub const FOG_ENABLE: u32 = 0x01;
pub const FOG_CONSTANT: u32 = 0x20;

pub const FBIINIT0_VGA_PASS: u32 = 1;
pub const FBIINIT0_GRAPHICS_RESET: u32 = 1 << 1;
pub const FBIINIT1_TILES_IN_X_SHIFT: u32 = 4;
pub const FBIINIT1_TILES_IN_X_MASK: u32 = 0xf << FBIINIT1_TILES_IN_X_SHIFT;
pub const FBIINIT1_MULTI_SST: u32 = 1 << 2;
pub const FBIINIT1_VIDEO_RESET: u32 = 1 << 8;
pub const FBIINIT1_SLI_ENABLE: u32 = 1 << 23;
pub const FBIINIT2_SWAP_ALGORITHM_MASK: u32 = 3 << 9;
pub const FBIINIT2_BUFFER_OFFSET_SHIFT: u32 = 11;
pub const FBIINIT2_BUFFER_OFFSET_MASK: u32 = 0x1ff << FBIINIT2_BUFFER_OFFSET_SHIFT;
pub const FBIINIT2_TRIPLE_BUFFER: u32 = 1 << 4;
pub const FBIINIT3_REMAP: u32 = 1;
pub const FBIINIT5_MULTI_CVG: u32 = 1 << 14;
pub const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

/// `initEnable` bit 0 allows writes to the SST-1 framebuffer init registers.
pub const INIT_ENABLE_WRITE: u32 = 1;
/// `initEnable` bit that remaps `fbiInit2` readback onto the DAC readback
/// latch instead of the stored `fbiInit2` value. This is `initEnable` bit 2
/// (`SST_FBIINIT23_REMAP` in the Glide init source), written through PCI
/// config space (offset 0x40 in this codebase's PCI function) rather than
/// through the MMIO register window, matching real SST-1 hardware where
/// `initEnable` is a PCI-config-only register.
pub const INIT_ENABLE_REMAP: u32 = 1 << 2;

/// `dacData` write bit 11 (`SST_DACDATA_RD` in the Glide init source):
/// requests a DAC read cycle instead of a write cycle.
pub const DACDATA_RD: u32 = 1 << 11;
/// `dacData` address field shift: bits 8-10 select the DAC's internal
/// register index (`SST_DACDATA_ADDR_SHIFT`).
pub const DACDATA_ADDR_SHIFT: u32 = 8;
/// The ICS5342 GENDAC PLL sub-register index (`dacData`'s addressed
/// register 5), used to reach the clock-synthesizer sub-registers
/// (`GCLK1`/`VCLK1`/`VCLK7`) that `sst1InitDacDetectICS` probes.
pub(super) const DAC_REG_PLL: u32 = 5;
/// ICS5342 GENDAC PLL sub-register indices for the three clocks the ICS
/// detection probe reads, and their known power-on-default byte values
/// (`sst1InitDacDetectICS`, matching the real chip and 86Box's
/// `vid_voodoo.c` `SST_dacData` handler).
pub(super) const ICS_PLL_VCLK1: u8 = 0x01;
pub(super) const ICS_PLL_VCLK7: u8 = 0x07;
pub(super) const ICS_PLL_GCLK1: u8 = 0x0b;
pub(super) const ICS_DEFAULT_VCLK1: u8 = 0x55;
pub(super) const ICS_DEFAULT_VCLK7: u8 = 0x71;
pub(super) const ICS_DEFAULT_GCLK1: u8 = 0x79;
