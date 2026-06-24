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

pub const DISTIRA_REG_ID: usize = 0x0000;
pub const DISTIRA_REG_CAPS: usize = 0x0004;
pub const DISTIRA_REG_STATUS: usize = 0x0008;
pub const DISTIRA_REG_CONTROL: usize = 0x000c;
pub const DISTIRA_REG_MODEL: usize = 0x0010;
pub const DISTIRA_REG_FB_WIDTH: usize = 0x0020;
pub const DISTIRA_REG_FB_HEIGHT: usize = 0x0024;
pub const DISTIRA_REG_FB_PITCH: usize = 0x0028;
pub const DISTIRA_REG_FRONT_BASE: usize = 0x002c;
pub const DISTIRA_REG_BACK_BASE: usize = 0x0030;
pub const DISTIRA_REG_CLEAR_COLOR: usize = 0x0040;
pub const DISTIRA_REG_COMMAND: usize = 0x00fc;

pub const DISTIRA_CMD_CLEAR: u32 = 1;
pub const DISTIRA_CMD_SWAP: u32 = 2;

pub const SST_STATUS: usize = 0x000;
pub const SST_INTR_CTRL: usize = 0x004;
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
        }
    }
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
    texture: Vec<u8>,
    fifo: VecDeque<DistiraFifoEntry>,
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
    fbi_pixels_in: u32,
    fbi_chroma_fail: u32,
    fbi_zfunc_fail: u32,
    fbi_afunc_fail: u32,
    fbi_pixels_out: u32,
    fbi_init: [u32; 8],
    back_porch: u32,
    video_dimensions: u32,
    h_sync: u32,
    v_sync: u32,
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
            texture: vec![0; DISTIRA_TEX_SIZE],
            fifo: VecDeque::new(),
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
            fbi_pixels_in: 0,
            fbi_chroma_fail: 0,
            fbi_zfunc_fail: 0,
            fbi_afunc_fail: 0,
            fbi_pixels_out: 0,
            fbi_init: [0; 8],
            back_porch: 0,
            video_dimensions: 0,
            h_sync: 0,
            v_sync: 0,
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
                let r = lerp_u8(a.r, b.r, c.r, l0, l1, l2);
                let g = lerp_u8(a.g, b.g, c.g, l0, l1, l2);
                let blue = lerp_u8(a.b, b.b, c.b, l0, l1, l2);
                let pixel = pack_rgb565_for_pixel(r, g, blue, x, y, self.dither_enabled);
                if self.write_back_pixel(x, y, pixel) {
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

    pub fn fifo_depth(&self) -> usize {
        self.fifo.len()
    }

    pub fn fifo_is_empty(&self) -> bool {
        self.fifo.is_empty()
    }

    pub fn fifo_is_full(&self) -> bool {
        self.fifo.len() >= DISTIRA_FIFO_CAPACITY
    }

    pub fn drain_fifo(&mut self) {
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
        let byte = offset & 0x3;
        match reg {
            SST_INTR_CTRL => merge_byte(&mut self.intr_ctrl, byte, value),
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

    fn control_value(&self) -> u32 {
        u32::from(self.dither_enabled) << 1
    }

    fn register_u32(&self, reg: usize) -> u32 {
        match reg {
            SST_STATUS => self.status_value(),
            SST_INTR_CTRL => self.intr_ctrl,
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
}

fn merge_byte(slot: &mut u32, byte: usize, value: u8) {
    let shift = byte * 8;
    *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

fn lerp_u8(a: u8, b: u8, c: u8, w0: f32, w1: f32, w2: f32) -> u8 {
    (a as f32 * w0 + b as f32 * w1 + c as f32 * w2)
        .round()
        .clamp(0.0, 255.0) as u8
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
