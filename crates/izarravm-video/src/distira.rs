//! Distira, VEGA's Glide-capable 3D unit. This first slice models the Voodoo
//! Graphics style scanout path: a 16-bit RGB565 front/back frame store, buffer
//! swaps, ordered dither, simple triangle setup, and exact host-color decode.
//! Texture sampling and the PCI/Glide command FIFO will hang off the same state.

use std::collections::VecDeque;

pub const DISTIRA_FB_SIZE: usize = 2 * 1024 * 1024;
pub const DISTIRA_MMIO_SIZE: usize = 0x0001_0000;
pub const DISTIRA_TEX_SIZE: usize = 2 * 1024 * 1024;
pub const DISTIRA_FIFO_CAPACITY: usize = 65_536;
pub const DISTIRA_ID_VALUE: u32 = 0x4454_0100; // 'D''T', version 1.00
pub const DISTIRA_MODEL_VALUE: u32 = 1;
pub const DISTIRA_TMU_COUNT: u32 = 2;
pub const BIG_DISTIRA_CHIP_NAME: &str = "BigDistira";
pub const SMALL_DISTIRA_CHIP_NAME: &str = "SmallDistira";
pub const DISTIRA_DEFAULT_RENDER_THREADS: u8 = 2;
pub const DISTIRA_RENDER_THREAD_CHOICES: [u8; 3] = [1, 2, 4];
pub const DISTIRA_MAX_WIDTH: u32 = 640;
pub const DISTIRA_MAX_HEIGHT: u32 = 480;

pub const DISTIRA_CAPS_TRIANGLE: u32 = 1 << 0;
pub const DISTIRA_CAPS_DITHER: u32 = 1 << 1;
pub const DISTIRA_CAPS_TMU1: u32 = 1 << 2;
pub const DISTIRA_CAPS_TMU2: u32 = 1 << 3;
pub const DISTIRA_CAPS_LFB: u32 = 1 << 4;

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
pub const SST_NCC_TABLE0_Y0: usize = 0x324;
pub const SST_NCC_TABLE0_Y1: usize = 0x328;
pub const SST_NCC_TABLE0_Y2: usize = 0x32c;
pub const SST_NCC_TABLE0_Y3: usize = 0x330;
pub const SST_NCC_TABLE0_I0: usize = 0x334;
pub const SST_NCC_TABLE0_I1: usize = 0x338;
pub const SST_NCC_TABLE0_I2: usize = 0x33c;
pub const SST_NCC_TABLE0_I3: usize = 0x340;
pub const SST_NCC_TABLE0_Q2: usize = 0x34c;
pub const SST_NCC_TABLE0_Q3: usize = 0x350;

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

pub const FBZ_CHROMAKEY: u32 = 1 << 1;
pub const FBZ_STIPPLE: u32 = 1 << 2;
pub const FBZ_W_BUFFER: u32 = 1 << 3;
pub const FBZ_DEPTH_ENABLE: u32 = 1 << 4;
pub const FBZ_DEPTH_OP_SHIFT: u32 = 5;
pub const FBZ_DITHER: u32 = 1 << 8;
pub const FBZ_RGB_WMASK: u32 = 1 << 9;
pub const FBZ_DEPTH_WMASK: u32 = 1 << 10;
pub const FBZ_DITHER_2X2: u32 = 1 << 11;
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
pub const FBZCP_CC_LOCALSELECT_COLOR0: u32 = 1 << 4;
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
pub const FBZCP_CC_ADD_CLOCAL: u32 = 1 << 14;
pub const FBZCP_CC_ADD_ALOCAL: u32 = 2 << 14;
pub const FBZCP_CC_INVERT_OUTPUT: u32 = 1 << 16;
pub const FBZCP_CCA_ZERO_OTHER: u32 = 1 << 17;
pub const FBZCP_CCA_SUB_CLOCAL: u32 = 1 << 18;
pub const FBZCP_CCA_MSELECT_SHIFT: u32 = 19;
pub const FBZCP_CCA_MSELECT_MASK: u32 = 0x7;
pub const CCA_MSELECT_ALOCAL: u32 = 1;
pub const CCA_MSELECT_AOTHER: u32 = 2;
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
pub const TEXTUREMODE_TCLAMPS: u32 = 1 << 6;
pub const TEXTUREMODE_TCLAMPT: u32 = 1 << 7;
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
const CHIP_TREX0: usize = 0x2;
const CHIP_TREX1: usize = 0x4;

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
pub const FBIINIT1_MULTI_SST: u32 = 1 << 2;
pub const FBIINIT1_VIDEO_RESET: u32 = 1 << 8;
pub const FBIINIT1_SLI_ENABLE: u32 = 1 << 23;
pub const FBIINIT2_SWAP_ALGORITHM_MASK: u32 = 3 << 9;
pub const FBIINIT3_REMAP: u32 = 1;
pub const FBIINIT5_MULTI_CVG: u32 = 1 << 14;
pub const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

pub fn normalize_distira_render_threads(threads: u8) -> u8 {
    if DISTIRA_RENDER_THREAD_CHOICES.contains(&threads) {
        threads
    } else {
        DISTIRA_DEFAULT_RENDER_THREADS
    }
}

const CONTROL_DITHER: u32 = 1 << 1;
const STATUS_DISPLAY_ENABLED: u32 = 1 << 1;
const BAYER_4X4: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistiraDisplay {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub front_base: u32,
    pub back_base: u32,
}

impl Default for DistiraDisplay {
    fn default() -> Self {
        let width = DISTIRA_MAX_WIDTH;
        let height = DISTIRA_MAX_HEIGHT;
        let pitch = width * 2;
        let frame = pitch * height;
        Self {
            width,
            height,
            pitch,
            front_base: 0,
            back_base: frame,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistiraVertex {
    pub x: f32,
    pub y: f32,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
    pub s: f32,
    pub t: f32,
}

impl DistiraVertex {
    pub fn rgb(x: f32, y: f32, r: u8, g: u8, b: u8) -> Self {
        Self {
            x,
            y,
            r,
            g,
            b,
            a: 255,
            s: 0.0,
            t: 0.0,
        }
    }
}

#[derive(Clone, Copy)]
struct TmuTextureSample {
    width: usize,
    height: usize,
    base_addr: u32,
    mip_offset: usize,
    mode: u32,
    lod_reg: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistiraFifoEntry {
    Register { offset: usize, value: u32 },
    LfbU32 { offset: usize, value: u32 },
    TextureU32 { offset: usize, value: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distira {
    fb: Vec<u8>,
    depth: Vec<u16>,
    texture: Vec<u8>,
    fifo: VecDeque<DistiraFifoEntry>,
    command_fifo: VecDeque<u32>,
    display: DistiraDisplay,
    display_enabled: bool,
    dither_enabled: bool,
    clear_color: u32,
    command: u32,
    render_threads: u8,
    intr_ctrl: u32,
    fbz_color_path: u32,
    fog_mode: u32,
    alpha_mode: u32,
    fbz_mode: u32,
    lfb_mode: u32,
    clip_left: u32,
    clip_right: u32,
    clip_low_y: u32,
    clip_high_y: u32,
    fog_color: u32,
    za_color: u32,
    chroma_key: u32,
    stipple: u32,
    color0: u32,
    color1: u32,
    triangle_vertices: [(u32, u32); 3],
    triangle_color: [u32; 3],
    triangle_color_dx: [u32; 3],
    triangle_color_dy: [u32; 3],
    triangle_depth: u32,
    triangle_depth_dx: u32,
    triangle_depth_dy: u32,
    triangle_alpha: u32,
    triangle_alpha_dx: u32,
    triangle_alpha_dy: u32,
    triangle_tex_coord: [u32; 3],
    triangle_tex_coord_dx: [u32; 3],
    triangle_tex_coord_dy: [u32; 3],
    ftriangle_vertices: [(u32, u32); 3],
    ftriangle_color: [u32; 3],
    ftriangle_color_dx: [u32; 3],
    ftriangle_color_dy: [u32; 3],
    ftriangle_depth: u32,
    ftriangle_depth_dx: u32,
    ftriangle_depth_dy: u32,
    ftriangle_alpha: u32,
    ftriangle_alpha_dx: u32,
    ftriangle_alpha_dy: u32,
    ftriangle_tex_coord: [u32; 3],
    ftriangle_tex_coord_dx: [u32; 3],
    ftriangle_tex_coord_dy: [u32; 3],
    fbi_pixels_in: u32,
    fbi_chroma_fail: u32,
    fbi_zfunc_fail: u32,
    fbi_afunc_fail: u32,
    fbi_pixels_out: u32,
    cmd_fifo_base: u32,
    cmd_fifo_end: u32,
    cmd_fifo_read_ptr: u32,
    cmd_fifo_amin: u32,
    cmd_fifo_amax: u32,
    cmd_fifo_holes: u32,
    fbi_init: [u32; 8],
    back_porch: u32,
    video_dimensions: u32,
    h_sync: u32,
    v_sync: u32,
    texture_mode: u32,
    texture_mode_tmu1: u32,
    texture_lod: u32,
    texture_lod_tmu1: u32,
    texture_detail: u32,
    texture_detail_tmu1: u32,
    tex_base_addr: u32,
    tex_base_addr_tmu1: u32,
    tex_base_addr1: [u32; 2],
    tex_base_addr2: [u32; 2],
    tex_base_addr38: [u32; 2],
    ncc_table0_q2: [u32; 2],
    ncc_table0_q3: [u32; 2],
    ncc_table0_y: [[u32; 4]; 2],
    ncc_table0_i: [[u32; 4]; 2],
    ncc_table0_q: [[u32; 4]; 2],
    texture_palette: [[u32; 256]; 2],
}

impl Default for Distira {
    fn default() -> Self {
        Self::new()
    }
}

impl Distira {
    pub fn new() -> Self {
        let display = DistiraDisplay::default();
        Self {
            fb: vec![0; DISTIRA_FB_SIZE],
            depth: vec![0xffff; (DISTIRA_MAX_WIDTH * DISTIRA_MAX_HEIGHT) as usize],
            texture: vec![0; DISTIRA_TEX_SIZE],
            fifo: VecDeque::new(),
            command_fifo: VecDeque::new(),
            display,
            display_enabled: false,
            dither_enabled: false,
            clear_color: 0,
            command: 0,
            render_threads: DISTIRA_DEFAULT_RENDER_THREADS,
            intr_ctrl: 0,
            fbz_color_path: 0,
            fog_mode: 0,
            alpha_mode: 0,
            fbz_mode: 0,
            lfb_mode: LFB_FORMAT_RGB565 | LFB_WRITE_FRONT | LFB_READ_FRONT,
            clip_left: 0,
            clip_right: display.width,
            clip_low_y: 0,
            clip_high_y: display.height,
            fog_color: 0,
            za_color: 0,
            chroma_key: 0,
            stipple: 0,
            color0: 0,
            color1: 0,
            triangle_vertices: [(0, 0); 3],
            triangle_color: [0; 3],
            triangle_color_dx: [0; 3],
            triangle_color_dy: [0; 3],
            triangle_depth: 0,
            triangle_depth_dx: 0,
            triangle_depth_dy: 0,
            triangle_alpha: 0x00ff_0000,
            triangle_alpha_dx: 0,
            triangle_alpha_dy: 0,
            triangle_tex_coord: [0; 3],
            triangle_tex_coord_dx: [0; 3],
            triangle_tex_coord_dy: [0; 3],
            ftriangle_vertices: [(0, 0); 3],
            ftriangle_color: [0; 3],
            ftriangle_color_dx: [0; 3],
            ftriangle_color_dy: [0; 3],
            ftriangle_depth: 0,
            ftriangle_depth_dx: 0,
            ftriangle_depth_dy: 0,
            ftriangle_alpha: f32::to_bits(255.0),
            ftriangle_alpha_dx: 0,
            ftriangle_alpha_dy: 0,
            ftriangle_tex_coord: [0; 3],
            ftriangle_tex_coord_dx: [0; 3],
            ftriangle_tex_coord_dy: [0; 3],
            fbi_pixels_in: 0,
            fbi_chroma_fail: 0,
            fbi_zfunc_fail: 0,
            fbi_afunc_fail: 0,
            fbi_pixels_out: 0,
            cmd_fifo_base: 0,
            cmd_fifo_end: 0,
            cmd_fifo_read_ptr: 0,
            cmd_fifo_amin: 0,
            cmd_fifo_amax: 0,
            cmd_fifo_holes: 0,
            fbi_init: [0; 8],
            back_porch: 0,
            video_dimensions: 0,
            h_sync: 0,
            v_sync: 0,
            texture_mode: 0,
            texture_mode_tmu1: 0,
            texture_lod: 0,
            texture_lod_tmu1: 0,
            texture_detail: 0,
            texture_detail_tmu1: 0,
            tex_base_addr: 0,
            tex_base_addr_tmu1: 0,
            tex_base_addr1: [0; 2],
            tex_base_addr2: [0; 2],
            tex_base_addr38: [0; 2],
            ncc_table0_q2: [0; 2],
            ncc_table0_q3: [0; 2],
            ncc_table0_y: [[0; 4]; 2],
            ncc_table0_i: [[0; 4]; 2],
            ncc_table0_q: [[0; 4]; 2],
            texture_palette: [[0; 256]; 2],
        }
    }

    pub const fn tmu_count(&self) -> u32 {
        DISTIRA_TMU_COUNT
    }

    pub const fn chip_names(&self) -> [&'static str; 2] {
        [BIG_DISTIRA_CHIP_NAME, SMALL_DISTIRA_CHIP_NAME]
    }

    pub const fn render_threads(&self) -> u8 {
        self.render_threads
    }

    pub fn set_render_threads(&mut self, threads: u8) {
        self.render_threads = normalize_distira_render_threads(threads);
    }

    pub fn display(&self) -> DistiraDisplay {
        self.display
    }

    pub fn display_enabled(&self) -> bool {
        self.display_enabled
    }

    pub fn set_dither_enabled(&mut self, enabled: bool) {
        self.dither_enabled = enabled;
    }

    pub fn disable_display(&mut self) {
        self.display_enabled = false;
    }

    pub fn set_frame_size(&mut self, width: u32, height: u32) {
        let width = width.clamp(1, DISTIRA_MAX_WIDTH);
        let height = height.clamp(1, DISTIRA_MAX_HEIGHT);
        let pitch = width * 2;
        let frame = pitch.saturating_mul(height);
        self.display = DistiraDisplay {
            width,
            height,
            pitch,
            front_base: 0,
            back_base: frame,
        };
        self.clip_right = self.clip_right.min(width);
        self.clip_high_y = self.clip_high_y.min(height);
        self.depth.fill(0xffff);
    }

    pub fn clear_back_rgb(&mut self, r: u8, g: u8, b: u8) {
        let pixel = pack_rgb565(r, g, b).to_le_bytes();
        let start = self.display.back_base as usize;
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = start.saturating_add(len).min(self.fb.len());
        for chunk in self.fb[start..end].chunks_exact_mut(2) {
            chunk.copy_from_slice(&pixel);
        }
    }

    pub fn swap_buffers(&mut self) {
        std::mem::swap(&mut self.display.front_base, &mut self.display.back_base);
        self.display_enabled = true;
    }

    pub fn draw_triangle(&mut self, vertices: [DistiraVertex; 3]) -> u64 {
        self.draw_triangle_inner(vertices, None)
    }

    fn draw_triangle_with_depth(&mut self, vertices: [DistiraVertex; 3], depths: [f32; 3]) -> u64 {
        self.draw_triangle_inner(vertices, Some(depths))
    }

    fn draw_triangle_inner(
        &mut self,
        vertices: [DistiraVertex; 3],
        depths: Option<[f32; 3]>,
    ) -> u64 {
        let [a, b, c] = vertices;
        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area == 0.0 {
            return 0;
        }

        let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as u32;
        let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as u32;
        let max_x = a.x.max(b.x).max(c.x).ceil().min(self.display.width as f32) as u32;
        let max_y = a.y.max(b.y).max(c.y).ceil().min(self.display.height as f32) as u32;

        let mut written = 0;
        for y in min_y..max_y {
            for x in min_x..max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let w0 = edge(b.x, b.y, c.x, c.y, px, py);
                let w1 = edge(c.x, c.y, a.x, a.y, px, py);
                let w2 = edge(a.x, a.y, b.x, b.y, px, py);
                let inside = if area < 0.0 {
                    w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
                } else {
                    w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
                };
                if !inside {
                    continue;
                }

                let inv_area = 1.0 / area;
                let l0 = w0 * inv_area;
                let l1 = w1 * inv_area;
                let l2 = w2 * inv_area;
                let depth =
                    depths.map(|[za, zb, zc]| depth_to_u16(lerp_f32(za, zb, zc, l0, l1, l2)));
                if let Some(depth) = depth {
                    if !self.depth_test_passes(x, y, depth) {
                        self.fbi_zfunc_fail = self.fbi_zfunc_fail.wrapping_add(1);
                        continue;
                    }
                }

                let r = lerp_u8(a.r, b.r, c.r, l0, l1, l2);
                let g = lerp_u8(a.g, b.g, c.g, l0, l1, l2);
                let blue = lerp_u8(a.b, b.b, c.b, l0, l1, l2);
                let s = lerp_f32(a.s, b.s, c.s, l0, l1, l2);
                let t = lerp_f32(a.t, b.t, c.t, l0, l1, l2);
                let alpha = lerp_u8(a.a, b.a, c.a, l0, l1, l2);
                let texture_alpha = self.texture_alpha_factor(s, t);
                let aother = self.texture_alpha_or_source(alpha, s, t);
                let (r, g, blue) =
                    self.texture_color_or_source((x, y), (r, g, blue), alpha, aother, (s, t));
                let alpha = self.apply_alpha_path(alpha, aother, texture_alpha);
                if !self.alpha_test_passes(alpha) {
                    self.fbi_afunc_fail = self.fbi_afunc_fail.wrapping_add(1);
                    continue;
                }
                if !self.chroma_key_passes(r, g, blue) {
                    self.fbi_chroma_fail = self.fbi_chroma_fail.wrapping_add(1);
                    continue;
                }
                let (r, g, blue) = self.apply_fog_color(r, g, blue);
                let (r, g, blue) = self.alpha_blend_color(x, y, r, g, blue, alpha);
                let pixel = pack_rgb565_for_pixel(r, g, blue, x, y, self.dither_enabled);
                if self.write_back_pixel(x, y, pixel) {
                    if let Some(depth) = depth {
                        self.write_depth_pixel(x, y, depth);
                    }
                    written += 1;
                }
            }
        }
        written
    }

    pub fn scanout_argb(&self) -> Vec<u32> {
        let width = self.display.width as usize;
        let height = self.display.height as usize;
        let pitch = self.display.pitch as u64;
        let start = self.display.front_base as u64;
        let len = self.fb.len() as u64;
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height as u64 {
            for x in 0..width as u64 {
                let off = start
                    .saturating_add(y.saturating_mul(pitch))
                    .saturating_add(x.saturating_mul(2));
                let raw = if off + 1 < len {
                    u16::from_le_bytes([self.fb[off as usize], self.fb[off as usize + 1]])
                } else {
                    0
                };
                out.push(rgb565_to_argb(raw));
            }
        }
        out
    }

    pub fn read_lfb_u8(&self, offset: usize) -> u8 {
        self.fb.get(offset).copied().unwrap_or(0)
    }

    pub fn write_lfb_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.fb.get_mut(offset) {
            *slot = value;
        }
    }

    pub fn write_lfb_u32(&mut self, offset: usize, value: u32) {
        let base = self.lfb_write_base();
        match self.lfb_mode & LFB_FORMAT_MASK {
            LFB_FORMAT_RGB565 => {
                let pixel = offset / 2;
                self.write_color_pixel(base, pixel, value as u16);
                self.write_color_pixel(base, pixel + 1, (value >> 16) as u16);
            }
            LFB_FORMAT_RGB555 | LFB_FORMAT_ARGB1555 => {
                let pixel = offset / 2;
                self.write_color_pixel(base, pixel, rgb555_to_rgb565(value as u16));
                self.write_color_pixel(base, pixel + 1, rgb555_to_rgb565((value >> 16) as u16));
            }
            LFB_FORMAT_XRGB8888 | LFB_FORMAT_ARGB8888 => {
                let r = (value >> 16) as u8;
                let g = (value >> 8) as u8;
                let b = value as u8;
                self.write_color_pixel(base, offset / 4, pack_rgb565(r, g, b));
            }
            _ => {}
        }
    }

    pub fn queue_register_write(&mut self, offset: usize, value: u32) -> bool {
        self.push_fifo(DistiraFifoEntry::Register { offset, value })
    }

    pub fn queue_lfb_write_u32(&mut self, offset: usize, value: u32) -> bool {
        self.push_fifo(DistiraFifoEntry::LfbU32 { offset, value })
    }

    pub fn queue_texture_write_u32(&mut self, offset: usize, value: u32) -> bool {
        self.push_fifo(DistiraFifoEntry::TextureU32 { offset, value })
    }

    pub fn write_command_fifo_u32(&mut self, aperture_offset: usize, value: u32) -> bool {
        if self.fbi_init[7] & FBIINIT7_CMDFIFO_ENABLE == 0 || self.fifo_is_full() {
            return false;
        }
        let _write_offset = self
            .cmd_fifo_base
            .wrapping_add((aperture_offset as u32) & 0x3fffc);
        self.command_fifo.push_back(value);
        true
    }

    pub fn fifo_depth(&self) -> usize {
        self.command_fifo.len() + self.fifo.len()
    }

    pub fn fifo_is_empty(&self) -> bool {
        self.command_fifo.is_empty() && self.fifo.is_empty()
    }

    pub fn fifo_is_full(&self) -> bool {
        self.fifo_depth() >= DISTIRA_FIFO_CAPACITY
    }

    pub fn drain_fifo(&mut self) {
        self.drain_command_fifo();
        while let Some(entry) = self.fifo.pop_front() {
            match entry {
                DistiraFifoEntry::Register { offset, value } => self.write_mmio_u32(offset, value),
                DistiraFifoEntry::LfbU32 { offset, value } => self.write_lfb_u32(offset, value),
                DistiraFifoEntry::TextureU32 { offset, value } => {
                    self.write_texture_u32(offset, value);
                }
            }
        }
    }

    pub fn read_texture_u32(&self, offset: usize) -> u32 {
        let Some(end) = offset.checked_add(4) else {
            return 0;
        };
        let Some(bytes) = self.texture.get(offset..end) else {
            return 0;
        };
        u32::from_le_bytes(bytes.try_into().unwrap())
    }

    pub fn read_mmio_u8(&self, offset: usize) -> u8 {
        let reg = offset & !0x3;
        let byte = offset & 0x3;
        (self.register_u32(reg) >> (byte * 8)) as u8
    }

    pub fn write_mmio_u8(&mut self, offset: usize, value: u8) {
        let reg = offset & !0x3;
        let voodoo_reg = offset & 0x3fc;
        let byte = offset & 0x3;
        let chip = tmu_chip_mask(offset);
        match reg {
            SST_INTR_CTRL => merge_byte(&mut self.intr_ctrl, byte, value),
            SST_VERTEX_AX => merge_vertex_component(&mut self.triangle_vertices[0].0, byte, value),
            SST_VERTEX_AY => merge_vertex_component(&mut self.triangle_vertices[0].1, byte, value),
            SST_VERTEX_BX => merge_vertex_component(&mut self.triangle_vertices[1].0, byte, value),
            SST_VERTEX_BY => merge_vertex_component(&mut self.triangle_vertices[1].1, byte, value),
            SST_VERTEX_CX => merge_vertex_component(&mut self.triangle_vertices[2].0, byte, value),
            SST_VERTEX_CY => merge_vertex_component(&mut self.triangle_vertices[2].1, byte, value),
            SST_START_R => merge_color_component(&mut self.triangle_color[0], byte, value),
            SST_START_G => merge_color_component(&mut self.triangle_color[1], byte, value),
            SST_START_B => merge_color_component(&mut self.triangle_color[2], byte, value),
            SST_START_Z => merge_byte(&mut self.triangle_depth, byte, value),
            SST_START_A => merge_color_component(&mut self.triangle_alpha, byte, value),
            SST_START_S => merge_byte(&mut self.triangle_tex_coord[0], byte, value),
            SST_START_T => merge_byte(&mut self.triangle_tex_coord[1], byte, value),
            SST_START_W => merge_byte(&mut self.triangle_tex_coord[2], byte, value),
            SST_DR_DX => merge_color_component(&mut self.triangle_color_dx[0], byte, value),
            SST_DG_DX => merge_color_component(&mut self.triangle_color_dx[1], byte, value),
            SST_DB_DX => merge_color_component(&mut self.triangle_color_dx[2], byte, value),
            SST_DZ_DX => merge_byte(&mut self.triangle_depth_dx, byte, value),
            SST_DA_DX => merge_color_component(&mut self.triangle_alpha_dx, byte, value),
            SST_DS_DX => merge_byte(&mut self.triangle_tex_coord_dx[0], byte, value),
            SST_DT_DX => merge_byte(&mut self.triangle_tex_coord_dx[1], byte, value),
            SST_DW_DX => merge_byte(&mut self.triangle_tex_coord_dx[2], byte, value),
            SST_DR_DY => merge_color_component(&mut self.triangle_color_dy[0], byte, value),
            SST_DG_DY => merge_color_component(&mut self.triangle_color_dy[1], byte, value),
            SST_DB_DY => merge_color_component(&mut self.triangle_color_dy[2], byte, value),
            SST_DZ_DY => merge_byte(&mut self.triangle_depth_dy, byte, value),
            SST_DA_DY => merge_color_component(&mut self.triangle_alpha_dy, byte, value),
            SST_DS_DY => merge_byte(&mut self.triangle_tex_coord_dy[0], byte, value),
            SST_DT_DY => merge_byte(&mut self.triangle_tex_coord_dy[1], byte, value),
            SST_DW_DY => merge_byte(&mut self.triangle_tex_coord_dy[2], byte, value),
            SST_TRIANGLE_CMD => {
                if byte == 0 && value != 0 {
                    self.run_triangle_command();
                }
            }
            SST_FVERTEX_AX => {
                merge_byte(&mut self.ftriangle_vertices[0].0, byte, value);
                self.triangle_vertices[0].0 = float_vertex_to_fixed(self.ftriangle_vertices[0].0);
            }
            SST_FVERTEX_AY => {
                merge_byte(&mut self.ftriangle_vertices[0].1, byte, value);
                self.triangle_vertices[0].1 = float_vertex_to_fixed(self.ftriangle_vertices[0].1);
            }
            SST_FVERTEX_BX => {
                merge_byte(&mut self.ftriangle_vertices[1].0, byte, value);
                self.triangle_vertices[1].0 = float_vertex_to_fixed(self.ftriangle_vertices[1].0);
            }
            SST_FVERTEX_BY => {
                merge_byte(&mut self.ftriangle_vertices[1].1, byte, value);
                self.triangle_vertices[1].1 = float_vertex_to_fixed(self.ftriangle_vertices[1].1);
            }
            SST_FVERTEX_CX => {
                merge_byte(&mut self.ftriangle_vertices[2].0, byte, value);
                self.triangle_vertices[2].0 = float_vertex_to_fixed(self.ftriangle_vertices[2].0);
            }
            SST_FVERTEX_CY => {
                merge_byte(&mut self.ftriangle_vertices[2].1, byte, value);
                self.triangle_vertices[2].1 = float_vertex_to_fixed(self.ftriangle_vertices[2].1);
            }
            SST_FSTART_R => {
                merge_byte(&mut self.ftriangle_color[0], byte, value);
                self.triangle_color[0] = float_color_to_fixed(self.ftriangle_color[0]);
            }
            SST_FSTART_G => {
                merge_byte(&mut self.ftriangle_color[1], byte, value);
                self.triangle_color[1] = float_color_to_fixed(self.ftriangle_color[1]);
            }
            SST_FSTART_B => {
                merge_byte(&mut self.ftriangle_color[2], byte, value);
                self.triangle_color[2] = float_color_to_fixed(self.ftriangle_color[2]);
            }
            SST_FSTART_Z => {
                merge_byte(&mut self.ftriangle_depth, byte, value);
                self.triangle_depth = float_depth_to_fixed(self.ftriangle_depth);
            }
            SST_FSTART_A => {
                merge_byte(&mut self.ftriangle_alpha, byte, value);
                self.triangle_alpha = float_color_to_fixed(self.ftriangle_alpha);
            }
            SST_FSTART_S => {
                merge_byte(&mut self.ftriangle_tex_coord[0], byte, value);
                self.triangle_tex_coord[0] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord[0]);
            }
            SST_FSTART_T => {
                merge_byte(&mut self.ftriangle_tex_coord[1], byte, value);
                self.triangle_tex_coord[1] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord[1]);
            }
            SST_FSTART_W => {
                merge_byte(&mut self.ftriangle_tex_coord[2], byte, value);
                self.triangle_tex_coord[2] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord[2]);
            }
            SST_FDR_DX => {
                merge_byte(&mut self.ftriangle_color_dx[0], byte, value);
                self.triangle_color_dx[0] = float_color_to_fixed(self.ftriangle_color_dx[0]);
            }
            SST_FDG_DX => {
                merge_byte(&mut self.ftriangle_color_dx[1], byte, value);
                self.triangle_color_dx[1] = float_color_to_fixed(self.ftriangle_color_dx[1]);
            }
            SST_FDB_DX => {
                merge_byte(&mut self.ftriangle_color_dx[2], byte, value);
                self.triangle_color_dx[2] = float_color_to_fixed(self.ftriangle_color_dx[2]);
            }
            SST_FDZ_DX => {
                merge_byte(&mut self.ftriangle_depth_dx, byte, value);
                self.triangle_depth_dx = float_depth_to_fixed(self.ftriangle_depth_dx);
            }
            SST_FDA_DX => {
                merge_byte(&mut self.ftriangle_alpha_dx, byte, value);
                self.triangle_alpha_dx = float_color_to_fixed(self.ftriangle_alpha_dx);
            }
            SST_FDS_DX => {
                merge_byte(&mut self.ftriangle_tex_coord_dx[0], byte, value);
                self.triangle_tex_coord_dx[0] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord_dx[0]);
            }
            SST_FDT_DX => {
                merge_byte(&mut self.ftriangle_tex_coord_dx[1], byte, value);
                self.triangle_tex_coord_dx[1] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord_dx[1]);
            }
            SST_FDW_DX => {
                merge_byte(&mut self.ftriangle_tex_coord_dx[2], byte, value);
                self.triangle_tex_coord_dx[2] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord_dx[2]);
            }
            SST_FDR_DY => {
                merge_byte(&mut self.ftriangle_color_dy[0], byte, value);
                self.triangle_color_dy[0] = float_color_to_fixed(self.ftriangle_color_dy[0]);
            }
            SST_FDG_DY => {
                merge_byte(&mut self.ftriangle_color_dy[1], byte, value);
                self.triangle_color_dy[1] = float_color_to_fixed(self.ftriangle_color_dy[1]);
            }
            SST_FDB_DY => {
                merge_byte(&mut self.ftriangle_color_dy[2], byte, value);
                self.triangle_color_dy[2] = float_color_to_fixed(self.ftriangle_color_dy[2]);
            }
            SST_FDZ_DY => {
                merge_byte(&mut self.ftriangle_depth_dy, byte, value);
                self.triangle_depth_dy = float_depth_to_fixed(self.ftriangle_depth_dy);
            }
            SST_FDA_DY => {
                merge_byte(&mut self.ftriangle_alpha_dy, byte, value);
                self.triangle_alpha_dy = float_color_to_fixed(self.ftriangle_alpha_dy);
            }
            SST_FDS_DY => {
                merge_byte(&mut self.ftriangle_tex_coord_dy[0], byte, value);
                self.triangle_tex_coord_dy[0] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord_dy[0]);
            }
            SST_FDT_DY => {
                merge_byte(&mut self.ftriangle_tex_coord_dy[1], byte, value);
                self.triangle_tex_coord_dy[1] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord_dy[1]);
            }
            SST_FDW_DY => {
                merge_byte(&mut self.ftriangle_tex_coord_dy[2], byte, value);
                self.triangle_tex_coord_dy[2] =
                    float_texture_coord_to_fixed(self.ftriangle_tex_coord_dy[2]);
            }
            SST_FTRIANGLE_CMD => {
                if byte == 0 && value != 0 {
                    self.run_triangle_command();
                }
            }
            SST_FBZ_COLOR_PATH => merge_byte(&mut self.fbz_color_path, byte, value),
            SST_FOG_MODE => merge_byte(&mut self.fog_mode, byte, value),
            SST_ALPHA_MODE => merge_byte(&mut self.alpha_mode, byte, value),
            SST_FBZ_MODE => {
                merge_byte(&mut self.fbz_mode, byte, value);
                self.dither_enabled = self.fbz_mode & FBZ_DITHER != 0;
            }
            SST_LFB_MODE => merge_byte(&mut self.lfb_mode, byte, value),
            SST_CLIP_LEFT_RIGHT => {
                let mut clip = self.clip_right | (self.clip_left << 16);
                merge_byte(&mut clip, byte, value);
                self.clip_right = clip & 0xffff;
                self.clip_left = (clip >> 16) & 0xffff;
            }
            SST_CLIP_LOW_Y_HIGH_Y => {
                let mut clip = self.clip_high_y | (self.clip_low_y << 16);
                merge_byte(&mut clip, byte, value);
                self.clip_high_y = clip & 0xffff;
                self.clip_low_y = (clip >> 16) & 0xffff;
            }
            SST_NOP_CMD => {}
            SST_FASTFILL_CMD => {
                if byte == 0 && value != 0 {
                    self.run_fastfill();
                }
            }
            SST_SWAPBUFFER_CMD => {
                if byte == 0 && value != 0 {
                    self.swap_buffers();
                }
            }
            SST_FOG_COLOR => merge_byte(&mut self.fog_color, byte, value),
            SST_ZA_COLOR => merge_byte(&mut self.za_color, byte, value),
            SST_CHROMA_KEY => merge_byte(&mut self.chroma_key, byte, value),
            SST_STIPPLE => merge_byte(&mut self.stipple, byte, value),
            SST_COLOR0 => merge_byte(&mut self.color0, byte, value),
            SST_COLOR1 => merge_byte(&mut self.color1, byte, value),
            SST_CMD_FIFO_BASE_ADDR => {
                let mut base = self.cmd_fifo_base_addr_value();
                merge_byte(&mut base, byte, value);
                self.cmd_fifo_base = (base & 0x3ff) << 12;
                self.cmd_fifo_end = ((base >> 16) & 0x3ff) << 12;
            }
            SST_CMD_FIFO_BUMP => {}
            SST_CMD_FIFO_RD_PTR => merge_byte(&mut self.cmd_fifo_read_ptr, byte, value),
            SST_CMD_FIFO_AMIN => merge_byte(&mut self.cmd_fifo_amin, byte, value),
            SST_CMD_FIFO_AMAX => merge_byte(&mut self.cmd_fifo_amax, byte, value),
            SST_CMD_FIFO_DEPTH => {
                if byte == 0 && value == 0 {
                    self.command_fifo.clear();
                }
            }
            SST_CMD_FIFO_HOLES => merge_byte(&mut self.cmd_fifo_holes, byte, value),
            SST_FBI_INIT4 => merge_byte(&mut self.fbi_init[4], byte, value),
            SST_BACK_PORCH => merge_byte(&mut self.back_porch, byte, value),
            SST_VIDEO_DIMENSIONS => merge_byte(&mut self.video_dimensions, byte, value),
            SST_FBI_INIT0 => merge_byte(&mut self.fbi_init[0], byte, value),
            SST_FBI_INIT1 => merge_byte(&mut self.fbi_init[1], byte, value),
            SST_FBI_INIT2 => merge_byte(&mut self.fbi_init[2], byte, value),
            SST_FBI_INIT3 => merge_byte(&mut self.fbi_init[3], byte, value),
            SST_H_SYNC => merge_byte(&mut self.h_sync, byte, value),
            SST_V_SYNC => merge_byte(&mut self.v_sync, byte, value),
            SST_FBI_INIT5 => merge_byte(&mut self.fbi_init[5], byte, value),
            SST_FBI_INIT6 => merge_byte(&mut self.fbi_init[6], byte, value),
            SST_FBI_INIT7 => merge_byte(&mut self.fbi_init[7], byte, value),
            SST_TEXTURE_MODE => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.texture_mode, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.texture_mode_tmu1, byte, value);
                }
            }
            SST_TLOD => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.texture_lod, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.texture_lod_tmu1, byte, value);
                }
            }
            SST_TDETAIL => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.texture_detail, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.texture_detail_tmu1, byte, value);
                }
            }
            SST_TEX_BASE_ADDR => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.tex_base_addr, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.tex_base_addr_tmu1, byte, value);
                }
            }
            SST_TEX_BASE_ADDR1 => self.write_tex_base_addr_registers(chip, 1, byte, value),
            SST_TEX_BASE_ADDR2 => self.write_tex_base_addr_registers(chip, 2, byte, value),
            SST_TEX_BASE_ADDR38 => self.write_tex_base_addr_registers(chip, 38, byte, value),
            SST_NCC_TABLE0_Y0 => self.write_ncc_y_registers(chip, 0, byte, value),
            SST_NCC_TABLE0_Y1 => self.write_ncc_y_registers(chip, 1, byte, value),
            SST_NCC_TABLE0_Y2 => self.write_ncc_y_registers(chip, 2, byte, value),
            SST_NCC_TABLE0_Y3 => self.write_ncc_y_registers(chip, 3, byte, value),
            SST_NCC_TABLE0_I0 => self.write_ncc_i_registers(chip, 0, byte, value),
            SST_NCC_TABLE0_I1 => self.write_ncc_i_registers(chip, 1, byte, value),
            SST_NCC_TABLE0_I2 => self.write_ncc_i_registers(chip, 2, byte, value),
            SST_NCC_TABLE0_I3 => self.write_ncc_i_registers(chip, 3, byte, value),
            SST_NCC_TABLE0_Q2 => self.write_palette_registers(chip, false, byte, value),
            SST_NCC_TABLE0_Q3 => self.write_palette_registers(chip, true, byte, value),
            _ if voodoo_reg == SST_TEXTURE_MODE => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.texture_mode, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.texture_mode_tmu1, byte, value);
                }
            }
            _ if voodoo_reg == SST_TLOD => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.texture_lod, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.texture_lod_tmu1, byte, value);
                }
            }
            _ if voodoo_reg == SST_TDETAIL => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.texture_detail, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.texture_detail_tmu1, byte, value);
                }
            }
            _ if voodoo_reg == SST_TEX_BASE_ADDR => {
                if chip & CHIP_TREX0 != 0 {
                    merge_byte(&mut self.tex_base_addr, byte, value);
                }
                if chip & CHIP_TREX1 != 0 {
                    merge_byte(&mut self.tex_base_addr_tmu1, byte, value);
                }
            }
            _ if voodoo_reg == SST_TEX_BASE_ADDR1 => {
                self.write_tex_base_addr_registers(chip, 1, byte, value);
            }
            _ if voodoo_reg == SST_TEX_BASE_ADDR2 => {
                self.write_tex_base_addr_registers(chip, 2, byte, value);
            }
            _ if voodoo_reg == SST_TEX_BASE_ADDR38 => {
                self.write_tex_base_addr_registers(chip, 38, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_Y0 => {
                self.write_ncc_y_registers(chip, 0, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_Y1 => {
                self.write_ncc_y_registers(chip, 1, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_Y2 => {
                self.write_ncc_y_registers(chip, 2, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_Y3 => {
                self.write_ncc_y_registers(chip, 3, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_I0 => {
                self.write_ncc_i_registers(chip, 0, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_I1 => {
                self.write_ncc_i_registers(chip, 1, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_I2 => {
                self.write_ncc_i_registers(chip, 2, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_I3 => {
                self.write_ncc_i_registers(chip, 3, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_Q2 => {
                self.write_palette_registers(chip, false, byte, value);
            }
            _ if voodoo_reg == SST_NCC_TABLE0_Q3 => {
                self.write_palette_registers(chip, true, byte, value);
            }
            DISTIRA_REG_CONTROL => {
                let mut control = self.control_value();
                merge_byte(&mut control, byte, value);
                self.dither_enabled = control & CONTROL_DITHER != 0;
            }
            DISTIRA_REG_FB_WIDTH => {
                let mut width = self.display.width;
                merge_byte(&mut width, byte, value);
                self.set_frame_size(width, self.display.height);
            }
            DISTIRA_REG_FB_HEIGHT => {
                let mut height = self.display.height;
                merge_byte(&mut height, byte, value);
                self.set_frame_size(self.display.width, height);
            }
            DISTIRA_REG_FRONT_BASE => merge_byte(&mut self.display.front_base, byte, value),
            DISTIRA_REG_BACK_BASE => merge_byte(&mut self.display.back_base, byte, value),
            DISTIRA_REG_CLEAR_COLOR => merge_byte(&mut self.clear_color, byte, value),
            DISTIRA_REG_COMMAND => {
                merge_byte(&mut self.command, byte, value);
                if self.command != 0 {
                    self.run_command();
                }
            }
            _ => {}
        }
    }

    fn push_fifo(&mut self, entry: DistiraFifoEntry) -> bool {
        if self.fifo_is_full() {
            return false;
        }
        self.fifo.push_back(entry);
        true
    }

    fn write_mmio_u32(&mut self, offset: usize, value: u32) {
        for (byte, value) in value.to_le_bytes().into_iter().enumerate() {
            self.write_mmio_u8(offset + byte, value);
        }
    }

    fn write_texture_u32(&mut self, offset: usize, value: u32) {
        let Some(end) = offset.checked_add(4) else {
            return;
        };
        let Some(bytes) = self.texture.get_mut(offset..end) else {
            return;
        };
        bytes.copy_from_slice(&value.to_le_bytes());
    }

    fn drain_command_fifo(&mut self) {
        while let Some(header) = self.command_fifo.pop_front() {
            self.cmd_fifo_read_ptr = self.cmd_fifo_read_ptr.wrapping_add(4);
            match header & 7 {
                1 => {
                    let mut offset = ((header & 0x7ff8) >> 1) as usize;
                    let increment = header & (1 << 15) != 0;
                    for _ in 0..(header >> 16) {
                        let Some(value) = self.command_fifo.pop_front() else {
                            return;
                        };
                        self.cmd_fifo_read_ptr = self.cmd_fifo_read_ptr.wrapping_add(4);
                        self.push_fifo(DistiraFifoEntry::Register { offset, value });
                        if increment {
                            offset += 4;
                        }
                    }
                }
                5 => {
                    let Some(address) = self.command_fifo.pop_front() else {
                        return;
                    };
                    self.cmd_fifo_read_ptr = self.cmd_fifo_read_ptr.wrapping_add(4);
                    let mut offset = (address & 0x00ff_ffff) as usize;
                    let count = ((header >> 3) & 0x7ffff).max(1);
                    let space = header >> 30;
                    for _ in 0..count {
                        let Some(value) = self.command_fifo.pop_front() else {
                            return;
                        };
                        self.cmd_fifo_read_ptr = self.cmd_fifo_read_ptr.wrapping_add(4);
                        match space {
                            2 => self.push_fifo(DistiraFifoEntry::LfbU32 { offset, value }),
                            3 => self.push_fifo(DistiraFifoEntry::TextureU32 { offset, value }),
                            _ => false,
                        };
                        offset = offset.wrapping_add(4);
                    }
                }
                _ => {}
            }
        }
    }

    fn cmd_fifo_base_addr_value(&self) -> u32 {
        (self.cmd_fifo_base >> 12) | ((self.cmd_fifo_end >> 12) << 16)
    }

    fn write_palette_registers(&mut self, chip: usize, odd: bool, byte: usize, value: u8) {
        if chip & CHIP_TREX0 != 0 {
            self.write_palette_register(0, odd, byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            self.write_palette_register(1, odd, byte, value);
        }
    }

    fn write_palette_register(&mut self, tmu: usize, odd: bool, byte: usize, value: u8) {
        let slot = if odd {
            &mut self.ncc_table0_q3[tmu]
        } else {
            &mut self.ncc_table0_q2[tmu]
        };
        merge_byte(slot, byte, value);
        let raw = *slot;
        if raw & (1 << 31) != 0 {
            let index = ((raw >> 23) & 0xfe) as usize | usize::from(odd);
            self.texture_palette[tmu][index] = raw | 0xff00_0000;
        }
    }

    fn write_tex_base_addr_registers(&mut self, chip: usize, lod: u32, byte: usize, value: u8) {
        let slots = match lod {
            1 => &mut self.tex_base_addr1,
            2 => &mut self.tex_base_addr2,
            _ => &mut self.tex_base_addr38,
        };
        if chip & CHIP_TREX0 != 0 {
            merge_byte(&mut slots[0], byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            merge_byte(&mut slots[1], byte, value);
        }
    }

    fn write_ncc_y_registers(&mut self, chip: usize, index: usize, byte: usize, value: u8) {
        if chip & CHIP_TREX0 != 0 {
            merge_byte(&mut self.ncc_table0_y[0][index], byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            merge_byte(&mut self.ncc_table0_y[1][index], byte, value);
        }
    }

    fn write_ncc_i_registers(&mut self, chip: usize, index: usize, byte: usize, value: u8) {
        if chip & CHIP_TREX0 != 0 {
            merge_byte(&mut self.ncc_table0_i[0][index], byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            merge_byte(&mut self.ncc_table0_i[1][index], byte, value);
        }
    }

    fn control_value(&self) -> u32 {
        u32::from(self.dither_enabled) << 1
    }

    fn register_u32(&self, reg: usize) -> u32 {
        match reg {
            SST_STATUS => self.status_value(),
            SST_INTR_CTRL => self.intr_ctrl,
            SST_VERTEX_AX => self.triangle_vertices[0].0,
            SST_VERTEX_AY => self.triangle_vertices[0].1,
            SST_VERTEX_BX => self.triangle_vertices[1].0,
            SST_VERTEX_BY => self.triangle_vertices[1].1,
            SST_VERTEX_CX => self.triangle_vertices[2].0,
            SST_VERTEX_CY => self.triangle_vertices[2].1,
            SST_START_R => self.triangle_color[0],
            SST_START_G => self.triangle_color[1],
            SST_START_B => self.triangle_color[2],
            SST_START_Z => self.triangle_depth,
            SST_START_A => self.triangle_alpha,
            SST_DR_DX => self.triangle_color_dx[0],
            SST_DG_DX => self.triangle_color_dx[1],
            SST_DB_DX => self.triangle_color_dx[2],
            SST_DZ_DX => self.triangle_depth_dx,
            SST_DA_DX => self.triangle_alpha_dx,
            SST_DR_DY => self.triangle_color_dy[0],
            SST_DG_DY => self.triangle_color_dy[1],
            SST_DB_DY => self.triangle_color_dy[2],
            SST_DZ_DY => self.triangle_depth_dy,
            SST_DA_DY => self.triangle_alpha_dy,
            SST_TRIANGLE_CMD => 0,
            SST_FVERTEX_AX => self.ftriangle_vertices[0].0,
            SST_FVERTEX_AY => self.ftriangle_vertices[0].1,
            SST_FVERTEX_BX => self.ftriangle_vertices[1].0,
            SST_FVERTEX_BY => self.ftriangle_vertices[1].1,
            SST_FVERTEX_CX => self.ftriangle_vertices[2].0,
            SST_FVERTEX_CY => self.ftriangle_vertices[2].1,
            SST_FSTART_R => self.ftriangle_color[0],
            SST_FSTART_G => self.ftriangle_color[1],
            SST_FSTART_B => self.ftriangle_color[2],
            SST_FSTART_Z => self.ftriangle_depth,
            SST_FSTART_A => self.ftriangle_alpha,
            SST_FDR_DX => self.ftriangle_color_dx[0],
            SST_FDG_DX => self.ftriangle_color_dx[1],
            SST_FDB_DX => self.ftriangle_color_dx[2],
            SST_FDZ_DX => self.ftriangle_depth_dx,
            SST_FDA_DX => self.ftriangle_alpha_dx,
            SST_FDR_DY => self.ftriangle_color_dy[0],
            SST_FDG_DY => self.ftriangle_color_dy[1],
            SST_FDB_DY => self.ftriangle_color_dy[2],
            SST_FDZ_DY => self.ftriangle_depth_dy,
            SST_FDA_DY => self.ftriangle_alpha_dy,
            SST_FTRIANGLE_CMD => 0,
            SST_FBZ_COLOR_PATH => self.fbz_color_path,
            SST_FOG_MODE => self.fog_mode,
            SST_ALPHA_MODE => self.alpha_mode,
            SST_FBZ_MODE => self.fbz_mode,
            SST_LFB_MODE => self.lfb_mode,
            SST_CLIP_LEFT_RIGHT => self.clip_right | (self.clip_left << 16),
            SST_CLIP_LOW_Y_HIGH_Y => self.clip_high_y | (self.clip_low_y << 16),
            SST_FOG_COLOR => self.fog_color,
            SST_ZA_COLOR => self.za_color,
            SST_CHROMA_KEY => self.chroma_key,
            SST_STIPPLE => self.stipple,
            SST_COLOR0 => self.color0,
            SST_COLOR1 => self.color1,
            SST_FBI_PIXELS_IN => self.fbi_pixels_in & 0x00ff_ffff,
            SST_FBI_CHROMA_FAIL => self.fbi_chroma_fail & 0x00ff_ffff,
            SST_FBI_ZFUNC_FAIL => self.fbi_zfunc_fail & 0x00ff_ffff,
            SST_FBI_AFUNC_FAIL => self.fbi_afunc_fail & 0x00ff_ffff,
            SST_FBI_PIXELS_OUT => self.fbi_pixels_out & 0x00ff_ffff,
            SST_CMD_FIFO_BASE_ADDR => self.cmd_fifo_base_addr_value(),
            SST_CMD_FIFO_BUMP => 0,
            SST_CMD_FIFO_RD_PTR => self.cmd_fifo_read_ptr,
            SST_CMD_FIFO_AMIN => self.cmd_fifo_amin,
            SST_CMD_FIFO_AMAX => self.cmd_fifo_amax,
            SST_CMD_FIFO_DEPTH => self.command_fifo.len() as u32,
            SST_CMD_FIFO_HOLES => self.cmd_fifo_holes,
            SST_FBI_INIT4 => self.fbi_init[4],
            SST_V_RETRACE => 0,
            SST_BACK_PORCH => self.back_porch,
            SST_VIDEO_DIMENSIONS => self.video_dimensions,
            SST_FBI_INIT0 => self.fbi_init[0],
            SST_FBI_INIT1 => self.fbi_init[1],
            SST_FBI_INIT2 => self.fbi_init[2],
            SST_FBI_INIT3 => self.fbi_init[3] | (1 << 10) | (2 << 8),
            SST_H_SYNC => self.h_sync,
            SST_V_SYNC => self.v_sync,
            SST_HV_RETRACE => 0,
            SST_FBI_INIT5 => self.fbi_init[5] & !0x1ff,
            SST_FBI_INIT6 => self.fbi_init[6],
            SST_FBI_INIT7 => self.fbi_init[7] & !0xff,
            SST_TEXTURE_MODE => self.texture_mode,
            SST_TLOD => self.texture_lod,
            SST_TDETAIL => self.texture_detail,
            SST_TEX_BASE_ADDR => self.tex_base_addr,
            DISTIRA_REG_STATUS => {
                if self.display_enabled {
                    STATUS_DISPLAY_ENABLED
                } else {
                    0
                }
            }
            DISTIRA_REG_CONTROL => self.control_value(),
            DISTIRA_REG_MODEL => DISTIRA_MODEL_VALUE,
            DISTIRA_REG_FB_WIDTH => self.display.width,
            DISTIRA_REG_FB_HEIGHT => self.display.height,
            DISTIRA_REG_FB_PITCH => self.display.pitch,
            DISTIRA_REG_FRONT_BASE => self.display.front_base,
            DISTIRA_REG_BACK_BASE => self.display.back_base,
            DISTIRA_REG_CLEAR_COLOR => self.clear_color,
            DISTIRA_REG_COMMAND => self.command,
            _ => 0,
        }
    }

    fn status_value(&self) -> u32 {
        // 86Box reports a large free FIFO count plus low empty bits when idle.
        // This synchronous first slice keeps work in a host-drained queue, so
        // expose the same busy bit shape while any FIFO entry is pending.
        let mut status = 0x0fff_f07f;
        if !self.fifo_is_empty() {
            status |= 0x380;
        }
        status
    }

    fn lfb_write_base(&self) -> u32 {
        match self.lfb_mode & LFB_WRITE_MASK {
            LFB_WRITE_FRONT => self.display.front_base,
            LFB_WRITE_BACK => self.display.back_base,
            _ => self.display.back_base,
        }
    }

    fn write_color_pixel(&mut self, base: u32, pixel: usize, value: u16) {
        let width = self.display.width as usize;
        if width == 0 {
            return;
        }
        let x = pixel % width;
        let y = pixel / width;
        if y >= self.display.height as usize {
            return;
        }
        let off = u64::from(base)
            .saturating_add((y as u64).saturating_mul(u64::from(self.display.pitch)))
            .saturating_add((x as u64).saturating_mul(2));
        if off + 1 >= self.fb.len() as u64 {
            return;
        }
        let bytes = value.to_le_bytes();
        self.fb[off as usize] = bytes[0];
        self.fb[off as usize + 1] = bytes[1];
    }

    fn run_fastfill(&mut self) {
        if self.fbz_mode & FBZ_RGB_WMASK == 0 {
            return;
        }

        let pixel = pack_rgb565(
            (self.color1 >> 16) as u8,
            (self.color1 >> 8) as u8,
            self.color1 as u8,
        )
        .to_le_bytes();
        let start = match self.fbz_mode & FBZ_DRAW_MASK {
            FBZ_DRAW_FRONT => self.display.front_base,
            _ => self.display.back_base,
        };
        let left = self.clip_left.min(self.display.width) as u64;
        let right = self.clip_right.min(self.display.width) as u64;
        let low_y = self.clip_low_y.min(self.display.height) as u64;
        let high_y = self.clip_high_y.min(self.display.height) as u64;
        let pitch = u64::from(self.display.pitch);
        let len = self.fb.len() as u64;

        for y in low_y..high_y {
            for x in left..right {
                let off = u64::from(start)
                    .saturating_add(y.saturating_mul(pitch))
                    .saturating_add(x.saturating_mul(2));
                if off + 1 < len {
                    self.fb[off as usize] = pixel[0];
                    self.fb[off as usize + 1] = pixel[1];
                }
            }
        }
    }

    fn run_triangle_command(&mut self) {
        if self.fbz_mode & FBZ_RGB_WMASK == 0 {
            return;
        }

        let coords = self
            .triangle_vertices
            .map(|(x, y)| (fixed_vertex_to_f32(x), fixed_vertex_to_f32(y)));
        let (origin_x, origin_y) = coords[0];
        let depths = coords.map(|(x, y)| {
            fixed_depth_at(
                self.triangle_depth,
                self.triangle_depth_dx,
                self.triangle_depth_dy,
                x,
                y,
                origin_x,
                origin_y,
            )
        });
        let vertices = coords.map(|(x, y)| DistiraVertex {
            x,
            y,
            r: fixed_color_at(
                self.triangle_color[0],
                self.triangle_color_dx[0],
                self.triangle_color_dy[0],
                x,
                y,
                origin_x,
                origin_y,
            ),
            g: fixed_color_at(
                self.triangle_color[1],
                self.triangle_color_dx[1],
                self.triangle_color_dy[1],
                x,
                y,
                origin_x,
                origin_y,
            ),
            b: fixed_color_at(
                self.triangle_color[2],
                self.triangle_color_dx[2],
                self.triangle_color_dy[2],
                x,
                y,
                origin_x,
                origin_y,
            ),
            a: fixed_color_at(
                self.triangle_alpha,
                self.triangle_alpha_dx,
                self.triangle_alpha_dy,
                x,
                y,
                origin_x,
                origin_y,
            ),
            s: fixed_texture_coord_at(
                self.triangle_tex_coord[0],
                self.triangle_tex_coord_dx[0],
                self.triangle_tex_coord_dy[0],
                x,
                y,
                origin_x,
                origin_y,
            ),
            t: fixed_texture_coord_at(
                self.triangle_tex_coord[1],
                self.triangle_tex_coord_dx[1],
                self.triangle_tex_coord_dy[1],
                x,
                y,
                origin_x,
                origin_y,
            ),
        });
        let written = self.draw_triangle_with_depth(vertices, depths) as u32;
        self.fbi_pixels_in = self.fbi_pixels_in.wrapping_add(written);
        self.fbi_pixels_out = self.fbi_pixels_out.wrapping_add(written);
    }

    fn run_command(&mut self) {
        match self.command & 0xff {
            DISTIRA_CMD_CLEAR => {
                let r = (self.clear_color >> 16) as u8;
                let g = (self.clear_color >> 8) as u8;
                let b = self.clear_color as u8;
                self.clear_back_rgb(r, g, b);
            }
            DISTIRA_CMD_SWAP => self.swap_buffers(),
            _ => {}
        }
        self.command = 0;
    }

    fn write_back_pixel(&mut self, x: u32, y: u32, pixel: u16) -> bool {
        let off = u64::from(self.display.back_base)
            .saturating_add(u64::from(y).saturating_mul(u64::from(self.display.pitch)))
            .saturating_add(u64::from(x).saturating_mul(2));
        if off + 1 >= self.fb.len() as u64 {
            return false;
        }
        let bytes = pixel.to_le_bytes();
        self.fb[off as usize] = bytes[0];
        self.fb[off as usize + 1] = bytes[1];
        true
    }

    fn depth_test_passes(&self, x: u32, y: u32, depth: u16) -> bool {
        if self.fbz_mode & FBZ_DEPTH_ENABLE == 0 {
            return true;
        }
        let Some(old_depth) = self.read_depth_pixel(x, y) else {
            return false;
        };
        match (self.fbz_mode >> FBZ_DEPTH_OP_SHIFT) & 7 {
            DEPTHOP_NEVER => false,
            DEPTHOP_LESSTHAN => depth < old_depth,
            DEPTHOP_EQUAL => depth == old_depth,
            DEPTHOP_LESSTHANEQUAL => depth <= old_depth,
            DEPTHOP_GREATERTHAN => depth > old_depth,
            DEPTHOP_NOTEQUAL => depth != old_depth,
            DEPTHOP_GREATERTHANEQUAL => depth >= old_depth,
            DEPTHOP_ALWAYS => true,
            _ => true,
        }
    }

    fn alpha_test_passes(&self, alpha: u8) -> bool {
        if self.alpha_mode & ALPHA_TEST_ENABLE == 0 {
            return true;
        }
        let reference = (self.alpha_mode >> ALPHA_REF_SHIFT) as u8;
        match (self.alpha_mode >> ALPHA_FUNC_SHIFT) & 7 {
            AFUNC_NEVER => false,
            AFUNC_LESSTHAN => alpha < reference,
            AFUNC_EQUAL => alpha == reference,
            AFUNC_LESSTHANEQUAL => alpha <= reference,
            AFUNC_GREATERTHAN => alpha > reference,
            AFUNC_NOTEQUAL => alpha != reference,
            AFUNC_GREATERTHANEQUAL => alpha >= reference,
            AFUNC_ALWAYS => true,
            _ => true,
        }
    }

    fn chroma_key_passes(&self, r: u8, g: u8, b: u8) -> bool {
        self.fbz_mode & FBZ_CHROMAKEY == 0
            || r != (self.chroma_key >> 16) as u8
            || g != (self.chroma_key >> 8) as u8
            || b != self.chroma_key as u8
    }

    fn texture_color_or_source(
        &self,
        position: (u32, u32),
        source: (u8, u8, u8),
        alocal: u8,
        aother: u8,
        texture_coords: (f32, f32),
    ) -> (u8, u8, u8) {
        let (r, g, b) = source;
        let (s, t) = texture_coords;
        if self.fbz_color_path & FBZCP_TEXTURE_ENABLED == 0 {
            return source;
        }

        let selected = match self.fbz_color_path & FBZCP_RGB_SELECT_MASK {
            RGB_SELECT_COLOR1 => (
                (self.color1 >> 16) as u8,
                (self.color1 >> 8) as u8,
                self.color1 as u8,
            ),
            RGB_SELECT_LFB => self.read_back_pixel_rgb(position.0, position.1),
            _ => {
                let format = (self.texture_mode >> 8) & 0xf;
                if !matches!(
                    format,
                    TEX_RGB332
                        | TEX_Y4I2Q2
                        | TEX_A8
                        | TEX_I8
                        | TEX_AI8
                        | TEX_PAL8
                        | TEX_APAL8
                        | TEX_ARGB8332
                        | TEX_A8Y4I2Q2
                        | TEX_R5G6B5
                        | TEX_ARGB1555
                        | TEX_ARGB4444
                        | TEX_A8I8
                        | TEX_APAL88
                ) {
                    return (r, g, b);
                }
                let tmu0 = self.apply_texture_detail_blend(0, self.sample_tmu_texture(0, s, t));
                if self.texture_mode & (1 << 18) != 0
                    && ((self.texture_mode_tmu1 >> 8) & 0xf) == TEX_R5G6B5
                {
                    let tmu1 = self.apply_texture_detail_blend(1, self.sample_tmu_texture(1, s, t));
                    (
                        tmu0.0.saturating_add(tmu1.0),
                        tmu0.1.saturating_add(tmu1.1),
                        tmu0.2.saturating_add(tmu1.2),
                    )
                } else {
                    tmu0
                }
            }
        };
        let texture_alpha = self.sample_tmu_alpha(0, s, t);
        let texture_rgb = self.sample_tmu_texture(0, s, t);
        let color = self.apply_color_path_local_combine(
            selected,
            source,
            alocal,
            aother,
            texture_alpha,
            texture_rgb,
        );
        self.apply_color_path_output_invert(color)
    }

    fn apply_color_path_output_invert(&self, color: (u8, u8, u8)) -> (u8, u8, u8) {
        if self.fbz_color_path & FBZCP_CC_INVERT_OUTPUT == 0 {
            return color;
        }
        (color.0 ^ 0xff, color.1 ^ 0xff, color.2 ^ 0xff)
    }

    fn apply_color_path_local_combine(
        &self,
        color: (u8, u8, u8),
        source: (u8, u8, u8),
        alocal: u8,
        aother: u8,
        texture_alpha: u8,
        texture_rgb: (u8, u8, u8),
    ) -> (u8, u8, u8) {
        let mselect = (self.fbz_color_path >> FBZCP_CC_MSELECT_SHIFT) & FBZCP_CC_MSELECT_MASK;
        if self.fbz_color_path
            & (FBZCP_CC_ZERO_OTHER
                | FBZCP_CC_SUB_CLOCAL
                | FBZCP_CC_ADD_CLOCAL
                | FBZCP_CC_ADD_ALOCAL)
            == 0
            && mselect != CC_MSELECT_CLOCAL
            && mselect != CC_MSELECT_AOTHER
            && mselect != CC_MSELECT_ALOCAL
            && mselect != CC_MSELECT_TEX_ALPHA
            && mselect != CC_MSELECT_TEX_RGB
        {
            return color;
        }
        let color = if self.fbz_color_path & FBZCP_CC_ZERO_OTHER != 0 {
            (0, 0, 0)
        } else {
            color
        };
        let local = if self.fbz_color_path & FBZCP_CC_LOCALSELECT_COLOR0 != 0 {
            (
                (self.color0 >> 16) as u8,
                (self.color0 >> 8) as u8,
                self.color0 as u8,
            )
        } else {
            source
        };
        let color = if mselect == CC_MSELECT_CLOCAL {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_channel(color.0, local.0, reverse),
                color_path_blend_channel(color.1, local.1, reverse),
                color_path_blend_channel(color.2, local.2, reverse),
            )
        } else if mselect == CC_MSELECT_AOTHER {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_channel(color.0, aother, reverse),
                color_path_blend_channel(color.1, aother, reverse),
                color_path_blend_channel(color.2, aother, reverse),
            )
        } else if mselect == CC_MSELECT_ALOCAL {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_channel(color.0, alocal, reverse),
                color_path_blend_channel(color.1, alocal, reverse),
                color_path_blend_channel(color.2, alocal, reverse),
            )
        } else if mselect == CC_MSELECT_TEX_ALPHA {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_channel(color.0, texture_alpha, reverse),
                color_path_blend_channel(color.1, texture_alpha, reverse),
                color_path_blend_channel(color.2, texture_alpha, reverse),
            )
        } else if mselect == CC_MSELECT_TEX_RGB {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_channel(color.0, texture_rgb.0, reverse),
                color_path_blend_channel(color.1, texture_rgb.1, reverse),
                color_path_blend_channel(color.2, texture_rgb.2, reverse),
            )
        } else {
            color
        };
        let color = if self.fbz_color_path & FBZCP_CC_SUB_CLOCAL != 0 {
            (
                color.0.saturating_sub(local.0),
                color.1.saturating_sub(local.1),
                color.2.saturating_sub(local.2),
            )
        } else {
            color
        };
        if self.fbz_color_path & FBZCP_CC_ADD_CLOCAL != 0 {
            (
                color.0.saturating_add(local.0),
                color.1.saturating_add(local.1),
                color.2.saturating_add(local.2),
            )
        } else if self.fbz_color_path & FBZCP_CC_ADD_ALOCAL != 0 {
            (
                color.0.saturating_add(alocal),
                color.1.saturating_add(alocal),
                color.2.saturating_add(alocal),
            )
        } else {
            color
        }
    }

    fn apply_texture_detail_blend(&self, tmu: usize, color: (u8, u8, u8)) -> (u8, u8, u8) {
        let mode = self.texture_mode_for_tmu(tmu);
        let mselect = (mode >> TC_MSELECT_SHIFT) & TC_MSELECT_MASK;
        if mode & TC_ADD_CLOCAL == 0 || mselect != TC_MSELECT_DETAIL {
            return color;
        }

        let lod = self.texture_lod_level(tmu);
        let factor = self.texture_detail_factor(tmu, lod);
        (
            detail_blend_channel(color.0, factor),
            detail_blend_channel(color.1, factor),
            detail_blend_channel(color.2, factor),
        )
    }

    fn texture_detail_factor(&self, tmu: usize, lod: u32) -> u8 {
        let detail = self.texture_detail_for_tmu(tmu);
        let max = (detail & 0xff).min(0xff) as i32;
        let bias = ((detail >> 8) & 0x3f) as i32;
        let scale = (detail >> 14) & 0x7;
        ((bias - lod as i32) << scale).clamp(0, max).min(255) as u8
    }

    fn texture_alpha_or_source(&self, alpha: u8, s: f32, t: f32) -> u8 {
        if self.fbz_color_path & FBZCP_TEXTURE_ENABLED == 0
            || ((self.fbz_color_path >> FBZCP_A_SELECT_SHIFT) & FBZCP_A_SELECT_MASK) != A_SELECT_TEX
        {
            return alpha;
        }

        self.sample_tmu_alpha(0, s, t)
    }

    fn texture_alpha_factor(&self, s: f32, t: f32) -> u8 {
        if self.fbz_color_path & FBZCP_TEXTURE_ENABLED == 0 {
            0xff
        } else {
            self.sample_tmu_alpha(0, s, t)
        }
    }

    fn apply_alpha_path(&self, alocal: u8, aother: u8, texture_alpha: u8) -> u8 {
        let mut alpha = if self.fbz_color_path & FBZCP_CCA_ZERO_OTHER != 0 {
            0
        } else {
            aother
        };
        if self.fbz_color_path & FBZCP_CCA_SUB_CLOCAL != 0 {
            alpha = alpha.saturating_sub(alocal);
        }
        let mselect = (self.fbz_color_path >> FBZCP_CCA_MSELECT_SHIFT) & FBZCP_CCA_MSELECT_MASK;
        if mselect == CCA_MSELECT_ALOCAL
            || mselect == CCA_MSELECT_AOTHER
            || mselect == CCA_MSELECT_TEX_ALPHA
        {
            let factor = if mselect == CCA_MSELECT_AOTHER {
                aother
            } else if mselect == CCA_MSELECT_TEX_ALPHA {
                texture_alpha
            } else {
                alocal
            };
            let reverse = self.fbz_color_path & FBZCP_CCA_REVERSE_BLEND != 0;
            alpha = color_path_blend_channel(alpha, factor, reverse);
        }
        if ((self.fbz_color_path >> FBZCP_CCA_ADD_SHIFT) & FBZCP_CCA_ADD_MASK) != 0 {
            alpha = alpha.saturating_add(alocal);
        }
        if self.fbz_color_path & FBZCP_CCA_INVERT_OUTPUT != 0 {
            alpha ^= 0xff;
        }
        alpha
    }

    fn sample_tmu_alpha(&self, tmu: usize, s: f32, t: f32) -> u8 {
        match (self.texture_mode_for_tmu(tmu) >> 8) & 0xf {
            TEX_A8 => self.sample_tmu_u8(tmu, s, t),
            TEX_AI8 => expand4(self.sample_tmu_u8(tmu, s, t) >> 4),
            TEX_ARGB8332 | TEX_A8Y4I2Q2 | TEX_A8I8 | TEX_APAL88 => {
                (self.sample_tmu_u16(tmu, s, t) >> 8) as u8
            }
            TEX_ARGB1555 => {
                if self.sample_tmu_u16(tmu, s, t) & 0x8000 != 0 {
                    0xff
                } else {
                    0
                }
            }
            TEX_ARGB4444 => expand4((self.sample_tmu_u16(tmu, s, t) >> 12) as u8),
            _ => 0xff,
        }
    }

    fn sample_tmu_texture(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        match (self.texture_mode_for_tmu(tmu) >> 8) & 0xf {
            TEX_RGB332 => self.sample_tmu_rgb332(tmu, s, t),
            TEX_Y4I2Q2 => self.sample_tmu_yiq_ncc(tmu, s, t),
            TEX_A8 => self.sample_tmu_a8(tmu, s, t),
            TEX_I8 => self.sample_tmu_i8(tmu, s, t),
            TEX_AI8 => self.sample_tmu_ai44(tmu, s, t),
            TEX_PAL8 => self.sample_tmu_pal8(tmu, s, t),
            TEX_APAL8 => self.sample_tmu_apal8(tmu, s, t),
            TEX_ARGB8332 => self.sample_tmu_argb8332(tmu, s, t),
            TEX_A8Y4I2Q2 => self.sample_tmu_a8_yiq_ncc(tmu, s, t),
            TEX_R5G6B5 => self.sample_tmu_rgb565(tmu, s, t),
            TEX_ARGB1555 => self.sample_tmu_argb1555(tmu, s, t),
            TEX_ARGB4444 => self.sample_tmu_argb4444(tmu, s, t),
            TEX_A8I8 => self.sample_tmu_ai88(tmu, s, t),
            TEX_APAL88 => self.sample_tmu_apal88(tmu, s, t),
            _ => (0, 0, 0),
        }
    }

    fn sample_tmu_rgb332(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let lod = self.texture_lod_level(tmu);
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let (width, height) = self.texture_dimensions_for_lod(tmu, lod);
        let s = texture_coord_index(
            s / scale,
            width,
            mode & TEXTUREMODE_TCLAMPS != 0,
            lod_reg & LOD_TMIRROR_S != 0,
        );
        let t = texture_coord_index(
            t / scale,
            height,
            mode & TEXTUREMODE_TCLAMPT != 0,
            lod_reg & LOD_TMIRROR_T != 0,
        );
        let texel = t * width + s;
        let offset = (self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(self.texture_mip_offset_for_lod(tmu, lod, 1))
            .saturating_add(texel);
        let Some(&raw) = self.texture.get(offset) else {
            return (0, 0, 0);
        };
        expand_rgb332(raw)
    }

    fn sample_tmu_yiq_ncc(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, s, t);
        self.ncc_color(tmu, raw)
    }

    fn sample_tmu_a8_yiq_ncc(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u16(tmu, s, t) as u8;
        self.ncc_color(tmu, raw)
    }

    fn ncc_color(&self, tmu: usize, raw: u8) -> (u8, u8, u8) {
        let y_index = usize::from(raw >> 4);
        let i_index = usize::from((raw >> 2) & 0x03);
        let q_index = usize::from(raw & 0x03);
        let y = ((self.ncc_table0_y[tmu][y_index >> 2] >> ((y_index & 3) * 8)) & 0xff) as i32;
        let i = self.ncc_table0_i[tmu][i_index];
        let q = self.ncc_table0_q[tmu][q_index];
        (
            clamp_ncc(y + signed_ncc_component(i, 18) + signed_ncc_component(q, 18)),
            clamp_ncc(y + signed_ncc_component(i, 9) + signed_ncc_component(q, 9)),
            clamp_ncc(y + signed_ncc_component(i, 0) + signed_ncc_component(q, 0)),
        )
    }

    fn sample_tmu_a8(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, s, t);
        (raw, raw, raw)
    }

    fn sample_tmu_ai44(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let intensity = expand4(self.sample_tmu_u8(tmu, s, t));
        (intensity, intensity, intensity)
    }

    fn sample_tmu_ai88(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let intensity = self.sample_tmu_u16(tmu, s, t) as u8;
        (intensity, intensity, intensity)
    }

    fn sample_tmu_pal8(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let raw = self.texture_palette[tmu][usize::from(self.sample_tmu_u8(tmu, s, t))];
        ((raw >> 16) as u8, (raw >> 8) as u8, raw as u8)
    }

    fn sample_tmu_apal8(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        expand_apal8(self.texture_palette[tmu][usize::from(self.sample_tmu_u8(tmu, s, t))])
    }

    fn sample_tmu_apal88(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let index = (self.sample_tmu_u16(tmu, s, t) & 0xff) as usize;
        let raw = self.texture_palette[tmu][index];
        ((raw >> 16) as u8, (raw >> 8) as u8, raw as u8)
    }

    fn sample_tmu_argb8332(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        expand_rgb332(self.sample_tmu_u16(tmu, s, t) as u8)
    }

    fn sample_tmu_argb1555(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        expand_rgb555(self.sample_tmu_u16(tmu, s, t))
    }

    fn sample_tmu_argb4444(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        expand_rgb444(self.sample_tmu_u16(tmu, s, t))
    }

    fn sample_tmu_i8(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, s, t);
        (raw, raw, raw)
    }

    fn sample_tmu_u8(&self, tmu: usize, s: f32, t: f32) -> u8 {
        let lod = self.texture_lod_level(tmu);
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let (width, height) = self.texture_dimensions_for_lod(tmu, lod);
        let s = texture_coord_index(
            s / scale,
            width,
            mode & TEXTUREMODE_TCLAMPS != 0,
            lod_reg & LOD_TMIRROR_S != 0,
        );
        let t = texture_coord_index(
            t / scale,
            height,
            mode & TEXTUREMODE_TCLAMPT != 0,
            lod_reg & LOD_TMIRROR_T != 0,
        );
        let texel = t * width + s;
        let offset = (self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(self.texture_mip_offset_for_lod(tmu, lod, 1))
            .saturating_add(texel);
        self.texture.get(offset).copied().unwrap_or(0)
    }

    fn sample_tmu_u16(&self, tmu: usize, s: f32, t: f32) -> u16 {
        let lod = self.texture_lod_level(tmu);
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let (width, height) = self.texture_dimensions_for_lod(tmu, lod);
        let s = texture_coord_index(
            s / scale,
            width,
            mode & TEXTUREMODE_TCLAMPS != 0,
            lod_reg & LOD_TMIRROR_S != 0,
        );
        let t = texture_coord_index(
            t / scale,
            height,
            mode & TEXTUREMODE_TCLAMPT != 0,
            lod_reg & LOD_TMIRROR_T != 0,
        );
        let texel = (t * width + s).saturating_mul(2);
        let offset = (self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(self.texture_mip_offset_for_lod(tmu, lod, 2))
            .saturating_add(texel);
        let Some(bytes) = self.texture.get(offset..offset.saturating_add(2)) else {
            return 0;
        };
        u16::from_le_bytes(bytes.try_into().unwrap())
    }

    fn sample_tmu_rgb565(&self, tmu: usize, s: f32, t: f32) -> (u8, u8, u8) {
        let lod = self.texture_lod_level(tmu);
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let s = s / scale;
        let t = t / scale;
        let base_addr = self.tex_base_addr_for_tmu_lod(tmu, lod);
        let mip_offset = self.texture_mip_offset_for_lod(tmu, lod, 2);
        let (width, height) = self.texture_dimensions_for_lod(tmu, lod);
        let sample = TmuTextureSample {
            width,
            height,
            base_addr,
            mip_offset,
            mode,
            lod_reg,
        };
        if mode & 0x6 != 0 {
            return self.bilinear_rgb565(s, t, sample);
        }
        let s = texture_coord_index(
            s,
            width,
            mode & TEXTUREMODE_TCLAMPS != 0,
            lod_reg & LOD_TMIRROR_S != 0,
        ) as i32;
        let t = texture_coord_index(
            t,
            height,
            mode & TEXTUREMODE_TCLAMPT != 0,
            lod_reg & LOD_TMIRROR_T != 0,
        ) as i32;
        self.sample_rgb565_texel(s, t, sample)
    }

    fn bilinear_rgb565(&self, s: f32, t: f32, sample: TmuTextureSample) -> (u8, u8, u8) {
        let base_s = s.floor();
        let base_t = t.floor();
        let frac_s = ((s - base_s) * 16.0).floor().clamp(0.0, 15.0) as u32;
        let frac_t = ((t - base_t) * 16.0).floor().clamp(0.0, 15.0) as u32;
        let s0 = base_s as i32;
        let t0 = base_t as i32;
        let samples = [
            self.sample_rgb565_texel(s0, t0, sample),
            self.sample_rgb565_texel(s0 + 1, t0, sample),
            self.sample_rgb565_texel(s0, t0 + 1, sample),
            self.sample_rgb565_texel(s0 + 1, t0 + 1, sample),
        ];
        let weights = [
            (16 - frac_s) * (16 - frac_t),
            frac_s * (16 - frac_t),
            (16 - frac_s) * frac_t,
            frac_s * frac_t,
        ];
        let blend = |component: fn((u8, u8, u8)) -> u8| -> u8 {
            samples
                .iter()
                .zip(weights)
                .map(|(&sample, weight)| u32::from(component(sample)) * weight)
                .sum::<u32>()
                .checked_shr(8)
                .unwrap_or(0)
                .min(255) as u8
        };
        (
            blend(|(r, _, _)| r),
            blend(|(_, g, _)| g),
            blend(|(_, _, b)| b),
        )
    }

    fn texture_mode_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.texture_mode
        } else {
            self.texture_mode_tmu1
        }
    }

    fn texture_lod_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.texture_lod
        } else {
            self.texture_lod_tmu1
        }
    }

    fn texture_detail_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.texture_detail
        } else {
            self.texture_detail_tmu1
        }
    }

    fn texture_lod_level(&self, tmu: usize) -> u32 {
        let lod = self.texture_lod_for_tmu(tmu);
        let min_lod = ((lod >> 2) & 0xf).min(8);
        let max_lod = ((lod >> 8) & 0xf).min(8);
        if max_lod == 0 {
            min_lod
        } else {
            min_lod.min(max_lod)
        }
    }

    fn texture_dimensions_for_lod(&self, tmu: usize, lod: u32) -> (usize, usize) {
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let aspect = ((lod_reg >> 21) & 0x3) as usize;
        let mut width = (256_usize >> lod).max(1);
        let mut height = (256_usize >> lod).max(1);
        if lod_reg & LOD_S_IS_WIDER != 0 {
            height = (height >> aspect).max(1);
        } else {
            width = (width >> aspect).max(1);
        }
        (width, height)
    }

    fn tex_base_addr_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.tex_base_addr
        } else {
            self.tex_base_addr_tmu1
        }
    }

    fn tex_base_addr_for_tmu_lod(&self, tmu: usize, lod: u32) -> u32 {
        let lod_reg = self.texture_lod_for_tmu(tmu);
        if lod_reg & LOD_TMULTIBASEADDR == 0 {
            return self.tex_base_addr_for_tmu(tmu);
        }
        let base_lod = if lod_reg & (LOD_SPLIT | LOD_ODD) == (LOD_SPLIT | LOD_ODD) && lod == 0 {
            1
        } else {
            lod
        };
        match base_lod {
            0 => self.tex_base_addr_for_tmu(tmu),
            1 => self.tex_base_addr1[tmu],
            2 => self.tex_base_addr2[tmu],
            _ => self.tex_base_addr38[tmu],
        }
    }

    fn texture_mip_offset_for_lod(&self, tmu: usize, lod: u32, bytes_per_texel: usize) -> usize {
        if self.texture_lod_for_tmu(tmu) & LOD_TMULTIBASEADDR != 0 {
            0
        } else {
            (0..lod)
                .map(|level| {
                    let (width, height) = self.texture_dimensions_for_lod(tmu, level);
                    width * height * bytes_per_texel
                })
                .sum()
        }
    }

    fn sample_rgb565_texel(&self, s: i32, t: i32, sample: TmuTextureSample) -> (u8, u8, u8) {
        let s = texture_coord_index_i32(
            s,
            sample.width,
            sample.mode & TEXTUREMODE_TCLAMPS != 0,
            sample.lod_reg & LOD_TMIRROR_S != 0,
        );
        let t = texture_coord_index_i32(
            t,
            sample.height,
            sample.mode & TEXTUREMODE_TCLAMPT != 0,
            sample.lod_reg & LOD_TMIRROR_T != 0,
        );
        let texel = (t * sample.width + s).saturating_mul(2);
        let offset = (sample.base_addr as usize)
            .saturating_add(sample.mip_offset)
            .saturating_add(texel);
        let Some(bytes) = self.texture.get(offset..offset.saturating_add(2)) else {
            return (0, 0, 0);
        };
        let raw = u16::from_le_bytes([bytes[0], bytes[1]]);
        (
            expand5(raw >> 11) as u8,
            expand6(raw >> 5) as u8,
            expand5(raw) as u8,
        )
    }

    fn apply_fog_color(&self, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        if self.fog_mode & (FOG_ENABLE | FOG_CONSTANT) != (FOG_ENABLE | FOG_CONSTANT) {
            return (r, g, b);
        }
        (
            r.saturating_add((self.fog_color >> 16) as u8),
            g.saturating_add((self.fog_color >> 8) as u8),
            b.saturating_add(self.fog_color as u8),
        )
    }

    fn alpha_blend_color(&self, x: u32, y: u32, r: u8, g: u8, b: u8, alpha: u8) -> (u8, u8, u8) {
        if self.alpha_mode & ALPHA_BLEND_ENABLE == 0 {
            return (r, g, b);
        }
        let (dest_r, dest_g, dest_b) = self.read_back_pixel_rgb(x, y);
        let source_func = (self.alpha_mode >> ALPHA_SRC_FUNC_SHIFT) & 0xf;
        let dest_func = (self.alpha_mode >> ALPHA_DST_FUNC_SHIFT) & 0xf;
        (
            blend_channel(source_func, r, r, alpha)
                .saturating_add(blend_channel(dest_func, dest_r, r, alpha)),
            blend_channel(source_func, g, g, alpha)
                .saturating_add(blend_channel(dest_func, dest_g, g, alpha)),
            blend_channel(source_func, b, b, alpha)
                .saturating_add(blend_channel(dest_func, dest_b, b, alpha)),
        )
    }

    fn read_back_pixel_rgb(&self, x: u32, y: u32) -> (u8, u8, u8) {
        let off = u64::from(self.display.back_base)
            .saturating_add(u64::from(y).saturating_mul(u64::from(self.display.pitch)))
            .saturating_add(u64::from(x).saturating_mul(2));
        let raw = if off + 1 < self.fb.len() as u64 {
            u16::from_le_bytes([self.fb[off as usize], self.fb[off as usize + 1]])
        } else {
            0
        };
        (
            expand5(raw >> 11) as u8,
            expand6(raw >> 5) as u8,
            expand5(raw) as u8,
        )
    }

    fn read_depth_pixel(&self, x: u32, y: u32) -> Option<u16> {
        self.depth_pixel_index(x, y)
            .and_then(|index| self.depth.get(index).copied())
    }

    fn write_depth_pixel(&mut self, x: u32, y: u32, depth: u16) {
        if self.fbz_mode & (FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK)
            != (FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK)
        {
            return;
        }
        let Some(index) = self.depth_pixel_index(x, y) else {
            return;
        };
        if let Some(slot) = self.depth.get_mut(index) {
            *slot = depth;
        }
    }

    fn depth_pixel_index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.display.width || y >= self.display.height {
            return None;
        }
        Some((y as usize * self.display.width as usize) + x as usize)
    }
}

fn merge_byte(slot: &mut u32, byte: usize, value: u8) {
    let shift = byte * 8;
    *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
}

fn tmu_chip_mask(offset: usize) -> usize {
    match (offset >> 10) & 0xf {
        0 => 0xf,
        chip => chip,
    }
}

fn merge_vertex_component(slot: &mut u32, byte: usize, value: u8) {
    merge_byte(slot, byte, value);
    *slot &= 0xffff;
}

fn merge_color_component(slot: &mut u32, byte: usize, value: u8) {
    merge_byte(slot, byte, value);
    *slot &= 0x00ff_ffff;
}

fn fixed_vertex_to_f32(raw: u32) -> f32 {
    (raw as i16) as f32 / 16.0
}

fn fixed_color_at(
    start: u32,
    dx: u32,
    dy: u32,
    x: f32,
    y: f32,
    origin_x: f32,
    origin_y: f32,
) -> u8 {
    (fixed_color_value(start)
        + fixed_color_value(dx) * (x - origin_x)
        + fixed_color_value(dy) * (y - origin_y))
        .round()
        .clamp(0.0, 255.0) as u8
}

fn fixed_color_value(raw: u32) -> f32 {
    sign_extend_24(raw) as f32 / 4096.0
}

fn fixed_texture_coord_at(
    start: u32,
    dx: u32,
    dy: u32,
    x: f32,
    y: f32,
    origin_x: f32,
    origin_y: f32,
) -> f32 {
    start as i32 as f32 / 16384.0
        + dx as i32 as f32 / 16384.0 * (x - origin_x)
        + dy as i32 as f32 / 16384.0 * (y - origin_y)
}

fn fixed_depth_at(
    start: u32,
    dx: u32,
    dy: u32,
    x: f32,
    y: f32,
    origin_x: f32,
    origin_y: f32,
) -> f32 {
    start as f32 + dx as i32 as f32 * (x - origin_x) + dy as i32 as f32 * (y - origin_y)
}

fn depth_to_u16(raw: f32) -> u16 {
    (raw / 4096.0).round().clamp(0.0, 65535.0) as u16
}

fn sign_extend_24(raw: u32) -> i32 {
    let raw = raw & 0x00ff_ffff;
    if raw & 0x0080_0000 != 0 {
        (raw | 0xff00_0000) as i32
    } else {
        raw as i32
    }
}

fn float_vertex_to_fixed(raw: u32) -> u32 {
    ((f32::from_bits(raw) * 16.0) as i16 as u16).into()
}

fn float_color_to_fixed(raw: u32) -> u32 {
    ((f32::from_bits(raw) * 4096.0) as i32 as u32) & 0x00ff_ffff
}

fn float_depth_to_fixed(raw: u32) -> u32 {
    (f32::from_bits(raw) * 4096.0) as i32 as u32
}

fn float_texture_coord_to_fixed(raw: u32) -> u32 {
    (f32::from_bits(raw) * 16384.0) as i32 as u32
}

fn signed_ncc_component(raw: u32, shift: u32) -> i32 {
    let value = ((raw >> shift) & 0x1ff) as i32;
    if value & 0x100 != 0 {
        value | !0x1ff
    } else {
        value
    }
}

fn clamp_ncc(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn detail_blend_channel(channel: u8, factor: u8) -> u8 {
    let inverse_factor = u32::from((factor ^ 0xff).wrapping_add(1));
    let subtract = (u32::from(channel) * inverse_factor) >> 8;
    channel.saturating_sub(subtract.min(u32::from(channel)) as u8)
}

fn color_path_blend_channel(channel: u8, factor: u8, reverse: bool) -> u8 {
    let factor = if reverse {
        u32::from(factor) + 1
    } else {
        u32::from((factor ^ 0xff).wrapping_add(1))
    };
    ((u32::from(channel) * factor) >> 8).min(255) as u8
}

fn texture_coord_index(coord: f32, size: usize, clamp: bool, mirror: bool) -> usize {
    texture_coord_index_i32(coord.floor() as i32, size, clamp, mirror)
}

fn texture_coord_index_i32(coord: i32, size: usize, clamp: bool, mirror: bool) -> usize {
    if clamp {
        coord.clamp(0, size.saturating_sub(1) as i32) as usize
    } else if mirror {
        let period = (size * 2) as i32;
        let coord = coord.rem_euclid(period);
        if coord >= size as i32 {
            (period - 1 - coord) as usize
        } else {
            coord as usize
        }
    } else {
        coord as usize & (size - 1)
    }
}

fn expand3(v: u8) -> u8 {
    let v = u32::from(v & 0x07);
    ((v << 5) | (v << 2) | (v >> 1)) as u8
}

fn expand2(v: u8) -> u8 {
    let v = u32::from(v & 0x03);
    ((v << 6) | (v << 4) | (v << 2) | v) as u8
}

fn expand4(v: u8) -> u8 {
    let v = u32::from(v & 0x0f);
    ((v << 4) | v) as u8
}

fn expand_rgb332(raw: u8) -> (u8, u8, u8) {
    (expand3(raw >> 5), expand3(raw >> 2), expand2(raw))
}

fn expand_apal8(raw: u32) -> (u8, u8, u8) {
    let r = (raw >> 16) as u8;
    let g = (raw >> 8) as u8;
    let b = raw as u8;
    (
        ((r & 3) << 6) | ((g & 0xf0) >> 2) | (r & 3),
        ((g & 0x0f) << 4) | ((b & 0xc0) >> 4) | ((g & 0x0f) >> 2),
        ((b & 0x3f) << 2) | ((b & 0x30) >> 4),
    )
}

fn expand_rgb555(raw: u16) -> (u8, u8, u8) {
    (
        expand5(raw >> 10) as u8,
        expand5(raw >> 5) as u8,
        expand5(raw) as u8,
    )
}

fn expand_rgb444(raw: u16) -> (u8, u8, u8) {
    (
        expand4((raw >> 8) as u8),
        expand4((raw >> 4) as u8),
        expand4(raw as u8),
    )
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

fn lerp_u8(a: u8, b: u8, c: u8, w0: f32, w1: f32, w2: f32) -> u8 {
    (a as f32 * w0 + b as f32 * w1 + c as f32 * w2)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn lerp_f32(a: f32, b: f32, c: f32, w0: f32, w1: f32, w2: f32) -> f32 {
    a * w0 + b * w1 + c * w2
}

fn blend_channel(func: u32, component: u8, source: u8, source_alpha: u8) -> u8 {
    let component = u32::from(component);
    let source = u32::from(source);
    let source_alpha = u32::from(source_alpha);
    let destination_alpha = 255;
    let value = match func {
        BLEND_AZERO => 0,
        BLEND_ASRC_ALPHA => component * source_alpha / 255,
        BLEND_A_COLOR => component * source / 255,
        BLEND_ADST_ALPHA => component * destination_alpha / 255,
        BLEND_AONE => component,
        BLEND_AOMSRC_ALPHA => component * (255 - source_alpha) / 255,
        BLEND_AOM_COLOR => component * (255 - source) / 255,
        BLEND_AOMDST_ALPHA => component * (255 - destination_alpha) / 255,
        BLEND_ASATURATE => component * source_alpha.min(255 - destination_alpha) / 255,
        _ => component,
    };
    value.min(255) as u8
}

fn quantize_channel(v: u8, bits: u32, cell: u32, dither: bool) -> u16 {
    let shift = 8 - bits;
    let offset = if dither { (cell << shift) / 16 } else { 0 };
    u16::try_from((u32::from(v) + offset).min(255) >> shift).unwrap()
}

fn pack_rgb565_for_pixel(r: u8, g: u8, b: u8, x: u32, y: u32, dither: bool) -> u16 {
    let cell = BAYER_4X4[(y & 3) as usize][(x & 3) as usize];
    let r = quantize_channel(r, 5, cell, dither);
    let g = quantize_channel(g, 6, cell, dither);
    let b = quantize_channel(b, 5, cell, dither);
    (r << 11) | (g << 5) | b
}

fn pack_rgb565(r: u8, g: u8, b: u8) -> u16 {
    pack_rgb565_for_pixel(r, g, b, 0, 0, false)
}

fn rgb555_to_rgb565(raw: u16) -> u16 {
    let r = ((raw >> 10) & 0x1f) << 11;
    let g5 = (raw >> 5) & 0x1f;
    let g = (g5 << 1) | (g5 >> 4);
    let b = raw & 0x1f;
    r | (g << 5) | b
}

fn expand5(v: u16) -> u32 {
    let v = u32::from(v & 0x1f);
    (v << 3) | (v >> 2)
}

fn expand6(v: u16) -> u32 {
    let v = u32::from(v & 0x3f);
    (v << 2) | (v >> 4)
}

fn rgb565_to_argb(raw: u16) -> u32 {
    let r = expand5(raw >> 11);
    let g = expand6(raw >> 5);
    let b = expand5(raw);
    (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_always_reports_two_tmus() {
        assert_eq!(Distira::new().tmu_count(), 2);
    }

    #[test]
    fn render_threads_default_to_86box_choices() {
        let mut distira = Distira::new();

        assert_eq!(distira.render_threads(), 2);
        distira.set_render_threads(4);
        assert_eq!(distira.render_threads(), 4);
        distira.set_render_threads(3);
        assert_eq!(distira.render_threads(), 2);
    }
}
