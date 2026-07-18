// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Distira, VEGA's Glide-capable 3D unit. This first slice models the Voodoo
//! Graphics style scanout path: a 16-bit RGB565 front/back frame store, buffer
//! swaps, ordered dither, triangle setup, texture sampling, and host-color decode.

use std::collections::VecDeque;

mod ncc;
mod raster_math;
mod registers;
mod texture_combine;
mod texture_raster;

use ncc::NccState;
use raster_math::*;
pub use registers::*;
use texture_raster::{
    TextureIteratorState, TextureRaster, TextureSample, texture_base_slot, texture_dimensions,
    texture_mip_offset,
};

const CONTROL_DITHER: u32 = 1 << 1;
const STATUS_DISPLAY_ENABLED: u32 = 1 << 1;
const SWAPBUFFER_SYNC_TO_RETRACE: u32 = 1;
const SWAPBUFFER_INTERVAL_MASK: u32 = 0xff;
const DISTIRA_TMU_CONFIG: u8 = 1 | (1 << 6) | (1 << 7);
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
    tmu: usize,
    width: usize,
    height: usize,
    base_addr: u32,
    mip_offset: usize,
    mode: u32,
    lod_reg: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextureRgba {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

impl TextureRgba {
    const TRANSPARENT_BLACK: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 0,
    };

    fn rgb(self) -> (u8, u8, u8) {
        (self.red, self.green, self.blue)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistiraFifoEntry {
    Register { offset: usize, value: u32 },
    LfbU32 { offset: usize, value: u32 },
    TextureU32 { offset: usize, value: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSwap {
    target_base: u32,
    interval: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distira {
    fb: Vec<u8>,
    texture: [Vec<u8>; 2],
    fifo: VecDeque<DistiraFifoEntry>,
    command_fifo: VecDeque<u32>,
    display: DistiraDisplay,
    scanout_base: u32,
    aux_base: u32,
    buffer_stride: u32,
    display_enabled: bool,
    dither_enabled: bool,
    clear_color: u32,
    command: u32,
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
    triangle_command: u32,
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
    texture_iterators: TextureIteratorState,
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
    init_enable: u32,
    back_porch: u32,
    video_dimensions: u32,
    h_sync: u32,
    v_sync: u32,
    /// The DAC's 8 indexed registers (`dac_data[0..=7]`), matching the ICS
    /// GENDAC layout 86Box models: `dac_data[4]` is the PLL sub-register
    /// address latch and `dac_data[5]` is the PLL sub-register write port
    /// (odd/even byte selected by `dac_reg_ff`); `dac_data[7]` doubles as
    /// the ICS-detect probe's own addressed-register storage.
    dac_data: [u8; 8],
    /// The DAC register index most recently addressed by a `dacData` write
    /// (bits 8-10 of the write value).
    dac_reg: u32,
    /// The value `SST_DAC_DATA` was armed with on the last read-cycle write,
    /// latched for readback through `fbiInit2` while `initEnable`'s remap
    /// bit is set (mirrors 86Box's `dac_readdata`).
    dac_readdata: u8,
    /// The ICS PLL sub-registers (16 clock synthesizer registers), indexed
    /// by `dac_data[4] & 0xf` and written a byte at a time via the
    /// high/low toggle `dac_reg_ff`.
    dac_pll_regs: [u16; 16],
    dac_reg_ff: bool,
    /// Byte-merge target for an in-progress `SST_DAC_DATA` dword write; the
    /// write's side effect (address decode, PLL register update, or ICS
    /// probe response) runs once the whole dword has been assembled.
    dac_data_write: u32,
    clut_data_write: u32,
    clut_anchors: [[u8; 3]; 64],
    clut: [[u8; 3]; 256],
    clut_programmed: bool,
    /// Current line in the fixed 525-line SST-1 scanout phase.
    frame_phase_line: u32,
    retrace_count: u64,
    swapbuffer_command: u32,
    pending_swap: Option<PendingSwap>,
    swap_commands: VecDeque<u32>,
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
    trex_init0: [u32; 2],
    trex_init1: [u32; 2],
    ncc: NccState,
}

impl Default for Distira {
    fn default() -> Self {
        Self::new()
    }
}

impl Distira {
    pub fn new() -> Self {
        let display = DistiraDisplay::default();
        let buffer_stride = display.pitch * display.height;
        let mut distira = Self {
            fb: vec![0; DISTIRA_FB_SIZE],
            texture: std::array::from_fn(|_| vec![0; DISTIRA_TEX_SIZE]),
            fifo: VecDeque::new(),
            command_fifo: VecDeque::new(),
            display,
            scanout_base: display.front_base,
            aux_base: buffer_stride * 2,
            buffer_stride,
            display_enabled: false,
            dither_enabled: false,
            clear_color: 0,
            command: 0,
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
            triangle_command: 0,
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
            texture_iterators: TextureIteratorState::default(),
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
            init_enable: 0,
            back_porch: 0,
            video_dimensions: 0,
            h_sync: 0,
            v_sync: 0,
            dac_data: [0; 8],
            dac_reg: 0,
            dac_readdata: 0,
            dac_pll_regs: [0; 16],
            dac_reg_ff: false,
            dac_data_write: 0,
            clut_data_write: 0,
            clut_anchors: std::array::from_fn(|index| {
                let value = (index.saturating_mul(8)).min(255) as u8;
                [value; 3]
            }),
            clut: std::array::from_fn(|value| [value as u8; 3]),
            clut_programmed: false,
            frame_phase_line: 0,
            retrace_count: 0,
            swapbuffer_command: 0,
            pending_swap: None,
            swap_commands: VecDeque::new(),
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
            trex_init0: [0; 2],
            trex_init1: [0; 2],
            ncc: NccState::default(),
        };
        distira.clear_aux_depth();
        distira
    }

    pub const fn tmu_count(&self) -> u32 {
        DISTIRA_TMU_COUNT
    }

    /// Set the SST `initEnable` value. On real hardware and in this
    /// codebase's PCI function, `initEnable` lives in PCI config space
    /// (offset 0x40) rather than the MMIO register window; the machine
    /// crate calls this whenever the guest writes that config dword so the
    /// init-register write gate and `SST_FBI_INIT2` DAC remap match SST-1.
    pub fn set_init_enable(&mut self, value: u32) {
        self.init_enable = value;
    }

    fn init_writes_enabled(&self) -> bool {
        self.init_enable & INIT_ENABLE_WRITE != 0
    }

    fn write_fbi_init0(&mut self, byte: usize, value: u8) {
        if !self.init_writes_enabled() {
            return;
        }
        merge_byte(&mut self.fbi_init[0], byte, value);
        if byte != 0 {
            return;
        }
        if self.fbi_init[0] & FBIINIT0_VGA_PASS != 0 {
            self.display_enabled = false;
        } else if self.fbi_init[1] & FBIINIT1_VIDEO_RESET == 0 {
            self.display_enabled = true;
        }
        if self.fbi_init[0] & FBIINIT0_GRAPHICS_RESET != 0 {
            self.display.front_base = 0;
            self.display.back_base = self.buffer_stride;
            self.scanout_base = 0;
            self.reset_swap_state();
        }
    }

    fn write_fbi_init1(&mut self, byte: usize, value: u8) {
        if !self.init_writes_enabled() {
            return;
        }
        let old = self.fbi_init[1];
        let mut new = old;
        merge_byte(&mut new, byte, value);
        self.fbi_init[1] = (new & !5) | (old & 5);
        if old & FBIINIT1_VIDEO_RESET != 0 && self.fbi_init[1] & FBIINIT1_VIDEO_RESET == 0 {
            self.frame_phase_line = 0;
            self.reset_swap_state();
            self.display_enabled = self.fbi_init[0] & FBIINIT0_VGA_PASS == 0;
        } else if self.fbi_init[1] & FBIINIT1_VIDEO_RESET != 0 {
            self.display_enabled = false;
            self.reset_swap_state();
        }
        self.recalculate_fbi_layout();
    }

    fn write_fbi_init2(&mut self, byte: usize, value: u8) {
        if self.init_writes_enabled() {
            merge_byte(&mut self.fbi_init[2], byte, value);
            self.recalculate_fbi_layout();
        }
    }

    fn write_fbi_init(&mut self, index: usize, byte: usize, value: u8) {
        if self.init_writes_enabled() {
            merge_byte(&mut self.fbi_init[index], byte, value);
        }
    }

    fn recalculate_fbi_layout(&mut self) {
        let old_stride = self.buffer_stride;
        let front_buffer = self.display.front_base.checked_div(old_stride).unwrap_or(0);
        let back_buffer = self.display.back_base.checked_div(old_stride).unwrap_or(1);
        let scanout_buffer = self.scanout_base.checked_div(old_stride).unwrap_or(0);
        let pending_buffer = self
            .pending_swap
            .map(|swap| swap.target_base.checked_div(old_stride).unwrap_or(0));
        let stride = ((self.fbi_init[2] & FBIINIT2_BUFFER_OFFSET_MASK)
            >> FBIINIT2_BUFFER_OFFSET_SHIFT)
            * 4096;
        let tiles = (self.fbi_init[1] & FBIINIT1_TILES_IN_X_MASK) >> FBIINIT1_TILES_IN_X_SHIFT;
        self.buffer_stride = stride;
        self.display.pitch = tiles * 128;
        self.display.front_base = front_buffer.min(2).saturating_mul(stride);
        self.display.back_base = back_buffer.min(2).saturating_mul(stride);
        self.scanout_base = scanout_buffer.min(2).saturating_mul(stride);
        if let (Some(buffer), Some(pending)) = (pending_buffer, self.pending_swap.as_mut()) {
            pending.target_base = buffer.min(2).saturating_mul(stride);
        }
        self.aux_base = stride.saturating_mul(if self.fbi_init[2] & FBIINIT2_TRIPLE_BUFFER != 0 {
            3
        } else {
            2
        });
    }

    fn update_video_dimensions(&mut self) {
        let width = (self.video_dimensions & 0xfff) + 1;
        let mut height = (self.video_dimensions >> 16) & 0xfff;
        if matches!(height, 386 | 402 | 482 | 602) {
            height -= 2;
        }
        self.display.width = width.clamp(1, 800);
        self.display.height = height.clamp(1, 600);
    }

    /// Advance a 525-line, 60 Hz scanout by whole lines. The machine timeline
    /// generates these independently of CPU mode.
    pub fn advance_frame_phase(&mut self, lines: u64) {
        let total = u128::from(FRAME_PHASE_TOTAL_LINES);
        let retrace_start = u128::from(FRAME_PHASE_TOTAL_LINES - FRAME_PHASE_VRETRACE_LINES);
        let old = u128::from(self.frame_phase_line);
        let advanced = old + u128::from(lines);
        let edges_through = |position: u128| {
            if position < retrace_start {
                0
            } else {
                (position - retrace_start) / total + 1
            }
        };
        let retrace_edges = edges_through(advanced) - edges_through(old);
        self.frame_phase_line = (advanced % total) as u32;
        self.advance_retrace_edges(retrace_edges as u64);
    }

    fn in_vretrace(&self) -> bool {
        self.frame_phase_line >= FRAME_PHASE_TOTAL_LINES - FRAME_PHASE_VRETRACE_LINES
    }

    fn advance_retrace_edges(&mut self, mut edges: u64) {
        while edges > 0 {
            let Some(pending) = self.pending_swap else {
                self.retrace_count = self.retrace_count.saturating_add(edges);
                return;
            };
            let interval = u64::from(pending.interval);
            let until_swap = if self.retrace_count > interval {
                1
            } else {
                interval + 1 - self.retrace_count
            };
            if edges < until_swap {
                self.retrace_count = self.retrace_count.saturating_add(edges);
                return;
            }

            edges -= until_swap;
            self.pending_swap = None;
            self.retrace_count = 0;
            self.present_swap(pending.target_base);
            self.start_next_swap();
        }
    }

    fn reset_swap_state(&mut self) {
        self.retrace_count = 0;
        self.swapbuffer_command = 0;
        self.pending_swap = None;
        self.swap_commands.clear();
    }

    fn rotate_buffers(&mut self) {
        if self.fbi_init[2] & FBIINIT2_TRIPLE_BUFFER != 0 && self.buffer_stride != 0 {
            let stride = self.buffer_stride;
            self.display.front_base = ((self.display.front_base / stride + 1) % 3) * stride;
            self.display.back_base = ((self.display.back_base / stride + 1) % 3) * stride;
        } else {
            std::mem::swap(&mut self.display.front_base, &mut self.display.back_base);
        }
    }

    fn present_swap(&mut self, target_base: u32) {
        self.scanout_base = target_base;
        self.display_enabled = true;
    }

    fn issue_swapbuffer_command(&mut self, value: u32) {
        self.swap_commands.push_back(value);
        self.start_next_swap();
    }

    fn start_next_swap(&mut self) {
        while self.pending_swap.is_none() {
            let Some(value) = self.swap_commands.pop_front() else {
                return;
            };
            self.rotate_buffers();
            let target_base = self.display.front_base;
            if value & SWAPBUFFER_SYNC_TO_RETRACE == 0 {
                self.present_swap(target_base);
            } else {
                self.pending_swap = Some(PendingSwap {
                    target_base,
                    interval: ((value >> 1) & SWAPBUFFER_INTERVAL_MASK) as u8,
                });
            }
        }
    }

    pub const fn chip_names(&self) -> [&'static str; 2] {
        [BIG_DISTIRA_CHIP_NAME, SMALL_DISTIRA_CHIP_NAME]
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
        self.scanout_base = self.display.front_base;
        self.reset_swap_state();
        self.buffer_stride = frame;
        self.aux_base = frame.saturating_mul(2);
        self.clip_right = self.clip_right.min(width);
        self.clip_high_y = self.clip_high_y.min(height);
        self.clear_aux_depth();
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
        self.rotate_buffers();
        self.present_swap(self.display.front_base);
    }

    pub fn draw_triangle(&mut self, vertices: [DistiraVertex; 3]) -> u64 {
        self.draw_triangle_inner(vertices, None, None, None)
    }

    fn draw_triangle_with_depth(
        &mut self,
        vertices: [DistiraVertex; 3],
        depths: [f32; 3],
        texture: TextureRaster,
        coverage: SstTriangleCoverage,
    ) -> u64 {
        self.draw_triangle_inner(vertices, Some(depths), Some(texture), Some(coverage))
    }

    fn draw_triangle_inner(
        &mut self,
        vertices: [DistiraVertex; 3],
        depths: Option<[f32; 3]>,
        texture: Option<TextureRaster>,
        coverage: Option<SstTriangleCoverage>,
    ) -> u64 {
        let count_fbi_pixels = coverage.is_some();
        let [a, b, c] = vertices;
        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area == 0.0 {
            return 0;
        }

        let mut min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as u32;
        let mut min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as u32;
        let mut max_x = a.x.max(b.x).max(c.x).ceil().min(self.display.width as f32) as u32;
        let mut max_y = a.y.max(b.y).max(c.y).ceil().min(self.display.height as f32) as u32;
        if coverage.is_some() {
            min_x = min_x.saturating_sub(1);
            max_x = max_x.saturating_add(1).min(self.display.width);
        }
        if self.fbz_mode & FBZ_CLIP_ENABLE != 0 {
            min_x = min_x.max(self.clip_left);
            max_x = max_x.min(self.clip_right);
            min_y = min_y.max(self.clip_low_y);
            max_y = max_y.min(self.clip_high_y);
        }

        let affine_lods = [self.texture_lod, self.texture_lod_tmu1];
        let mut written = 0;
        for y in min_y..max_y {
            let (row_min_x, row_max_x) = if let Some(coverage) = coverage {
                let Some((span_min, span_max)) = coverage.scanline_span(y) else {
                    continue;
                };
                let span_min = span_min.max(0) as u32;
                let span_max = span_max.max(-1).saturating_add(1) as u32;
                (min_x.max(span_min), max_x.min(span_max))
            } else {
                (min_x, max_x)
            };
            for x in row_min_x..row_max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let w0 = edge(b.x, b.y, c.x, c.y, px, py);
                let w1 = edge(c.x, c.y, a.x, a.y, px, py);
                let w2 = edge(a.x, a.y, b.x, b.y, px, py);
                let inside = if coverage.is_some() {
                    true
                } else if area < 0.0 {
                    w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
                } else {
                    w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
                };
                if !inside {
                    continue;
                }
                if count_fbi_pixels {
                    self.fbi_pixels_in = self.fbi_pixels_in.wrapping_add(1);
                }
                let draw_y = self.draw_y(y);
                if !self.stipple_test_passes(x, draw_y) {
                    continue;
                }

                let inv_area = 1.0 / area;
                let l0 = w0 * inv_area;
                let l1 = w1 * inv_area;
                let l2 = w2 * inv_area;
                let depth_raw = depths.map(|[za, zb, zc]| lerp_f32(za, zb, zc, l0, l1, l2));
                let depth = depth_raw.map(|raw| self.biased_triangle_depth(raw));
                if let Some(depth) = depth
                    && !self.depth_test_passes(x, draw_y, depth)
                {
                    self.fbi_zfunc_fail = self.fbi_zfunc_fail.wrapping_add(1);
                    continue;
                }

                let r = lerp_u8(a.r, b.r, c.r, l0, l1, l2);
                let g = lerp_u8(a.g, b.g, c.g, l0, l1, l2);
                let blue = lerp_u8(a.b, b.b, c.b, l0, l1, l2);
                let texture_samples = if let Some(texture) = texture {
                    texture.samples(px, py)
                } else {
                    let s = lerp_f32(a.s, b.s, c.s, l0, l1, l2);
                    let t = lerp_f32(a.t, b.t, c.t, l0, l1, l2);
                    std::array::from_fn(|tmu| TextureSample::affine(s, t, affine_lods[tmu]))
                };
                let alpha = lerp_u8(a.a, b.a, c.a, l0, l1, l2);
                let alocal = self.alpha_local_source(alpha, depth_raw);
                let texture = if self.fbz_color_path & FBZCP_TEXTURE_ENABLED != 0 {
                    self.combined_texture(texture_samples)
                } else {
                    TextureRgba::TRANSPARENT_BLACK
                };
                let texture_alpha = self.texture_alpha_factor(texture.alpha);
                let aother = self.texture_alpha_or_source(alpha, texture.alpha);
                if self.fbz_mode & FBZ_ALPHA_MASK != 0 && aother & 1 == 0 {
                    continue;
                }

                let selected = self.selected_color_or_source((x, draw_y), (r, g, blue), texture);
                if !self.chroma_key_passes(selected.0, selected.1, selected.2) {
                    self.fbi_chroma_fail = self.fbi_chroma_fail.wrapping_add(1);
                    continue;
                }
                let (r, g, blue) =
                    self.texture_color_or_source(selected, (r, g, blue), alocal, aother, texture);
                let alpha = self.apply_alpha_path(alocal, aother, texture_alpha);
                if !self.alpha_test_passes(alpha) {
                    self.fbi_afunc_fail = self.fbi_afunc_fail.wrapping_add(1);
                    continue;
                }
                if count_fbi_pixels {
                    self.fbi_pixels_out = self.fbi_pixels_out.wrapping_add(1);
                }
                let (r, g, blue) = self.apply_fog_color(r, g, blue);
                let (r, g, blue) = self.alpha_blend_color(x, draw_y, r, g, blue, alpha);
                let pixel = pack_rgb565_for_pixel(r, g, blue, x, draw_y, self.dither_enabled);
                let wrote_color = if depths.is_none() {
                    self.write_pixel_at_base(self.display.back_base, x, draw_y, pixel)
                } else {
                    self.fbz_mode & FBZ_RGB_WMASK != 0 && self.write_draw_pixel(x, draw_y, pixel)
                };
                let wrote_depth =
                    depth.is_some_and(|depth| self.write_depth_pixel(x, draw_y, depth));
                if wrote_color || wrote_depth {
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
        let start = self.scanout_base as u64;
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
                out.push(self.scanout_rgb565(raw));
            }
        }
        out
    }

    fn scanout_rgb565(&self, raw: u16) -> u32 {
        if !self.clut_programmed {
            return rgb565_to_argb(raw);
        }
        let red = usize::from((raw >> 8) & 0xf8);
        let green = usize::from((raw >> 3) & 0xfc);
        let blue = usize::from((raw << 3) & 0xf8);
        (u32::from(self.clut[red][0]) << 16)
            | (u32::from(self.clut[green][1]) << 8)
            | u32::from(self.clut[blue][2])
    }

    fn write_clut_data(&mut self, byte: usize, value: u8) {
        merge_byte(&mut self.clut_data_write, byte, value);
        if byte != 3 {
            return;
        }

        let index = ((self.clut_data_write >> 24) & 0x3f) as usize;
        self.clut_anchors[index] = if self.clut_data_write & (1 << 29) != 0 {
            [255; 3]
        } else {
            [
                (self.clut_data_write >> 16) as u8,
                (self.clut_data_write >> 8) as u8,
                self.clut_data_write as u8,
            ]
        };
        self.clut_programmed = true;
        for value in 0..256 {
            let base = value >> 3;
            let fraction = value & 7;
            for channel in 0..3 {
                self.clut[value][channel] = ((usize::from(self.clut_anchors[base][channel])
                    * (8 - fraction)
                    + usize::from(self.clut_anchors[base + 1][channel]) * fraction)
                    >> 3) as u8;
            }
        }
    }

    pub fn read_lfb_u8(&self, offset: usize) -> u8 {
        self.lfb_byte_offset(self.lfb_read_base(), offset)
            .and_then(|offset| self.fb.get(offset).copied())
            .unwrap_or(0xff)
    }

    pub fn read_lfb_u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes(self.read_lfb_bytes::<2>(offset & !1))
    }

    pub fn read_lfb_u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.read_lfb_bytes::<4>(offset & !1))
    }

    pub fn write_lfb_u8(&mut self, offset: usize, value: u8) {
        let base = if self.lfb_mode & LFB_FORMAT_MASK == LFB_FORMAT_DEPTH {
            self.aux_base
        } else {
            self.lfb_write_base()
        };
        if let Some(slot) = self
            .lfb_byte_offset(base, offset)
            .and_then(|offset| self.fb.get_mut(offset))
        {
            *slot = value;
        }
    }

    pub fn write_lfb_u16(&mut self, offset: usize, value: u16) {
        let base = self.lfb_write_base();
        let write_color = self.lfb_pipeline_writes_color();
        let write_depth = self.lfb_pipeline_writes_depth();
        let pipeline = self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE != 0;
        let position = lfb_position(offset, false);
        match self.lfb_mode & LFB_FORMAT_MASK {
            LFB_FORMAT_RGB565 => {
                self.write_lfb_color_pipeline_pixel(
                    base,
                    position,
                    value,
                    rgb565_components(value),
                    0xff,
                );
            }
            LFB_FORMAT_RGB555 => {
                let raw = rgb555_to_rgb565(value);
                self.write_lfb_color_pipeline_pixel(
                    base,
                    position,
                    raw,
                    rgb565_components(raw),
                    0xff,
                );
            }
            LFB_FORMAT_ARGB1555 => {
                let raw = rgb555_to_rgb565(value);
                self.write_lfb_color_pipeline_pixel(
                    base,
                    position,
                    raw,
                    rgb565_components(raw),
                    argb1555_alpha(value),
                );
            }
            LFB_FORMAT_DEPTH if write_depth || write_color || pipeline => {
                if let Some(color) = self.lfb_pipeline_depth_only_color(base, position, value) {
                    if let Some(color) = color {
                        self.write_color_pixel(base, position, color);
                    }
                    if write_depth {
                        self.write_depth_pixel_at(position, value);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn write_lfb_u32(&mut self, offset: usize, value: u32) {
        let base = self.lfb_write_base();
        let write_color = self.lfb_pipeline_writes_color();
        let write_depth = self.lfb_pipeline_writes_depth();
        let format = self.lfb_mode & LFB_FORMAT_MASK;
        let position = lfb_position(
            offset,
            matches!(
                format,
                LFB_FORMAT_XRGB8888
                    | LFB_FORMAT_ARGB8888
                    | LFB_FORMAT_DEPTH_RGB565
                    | LFB_FORMAT_DEPTH_RGB555
                    | LFB_FORMAT_DEPTH_ARGB1555
            ),
        );
        let next = (position.0 + 1, position.1);
        match format {
            LFB_FORMAT_RGB565 => {
                let raw0 = value as u16;
                let raw1 = (value >> 16) as u16;
                self.write_lfb_color_pipeline_pixel(
                    base,
                    position,
                    raw0,
                    rgb565_components(raw0),
                    0xff,
                );
                self.write_lfb_color_pipeline_pixel(
                    base,
                    next,
                    raw1,
                    rgb565_components(raw1),
                    0xff,
                );
            }
            LFB_FORMAT_RGB555 => {
                let raw0 = value as u16;
                let raw1 = (value >> 16) as u16;
                let raw0_rgb565 = rgb555_to_rgb565(raw0);
                let raw1_rgb565 = rgb555_to_rgb565(raw1);
                self.write_lfb_color_pipeline_pixel(
                    base,
                    position,
                    raw0_rgb565,
                    rgb565_components(raw0_rgb565),
                    0xff,
                );
                self.write_lfb_color_pipeline_pixel(
                    base,
                    next,
                    raw1_rgb565,
                    rgb565_components(raw1_rgb565),
                    0xff,
                );
            }
            LFB_FORMAT_ARGB1555 => {
                let raw0 = value as u16;
                let raw1 = (value >> 16) as u16;
                let raw0_rgb565 = rgb555_to_rgb565(raw0);
                let raw1_rgb565 = rgb555_to_rgb565(raw1);
                self.write_lfb_color_pipeline_pixel(
                    base,
                    position,
                    raw0_rgb565,
                    rgb565_components(raw0_rgb565),
                    argb1555_alpha(raw0),
                );
                self.write_lfb_color_pipeline_pixel(
                    base,
                    next,
                    raw1_rgb565,
                    rgb565_components(raw1_rgb565),
                    argb1555_alpha(raw1),
                );
            }
            LFB_FORMAT_XRGB8888 | LFB_FORMAT_ARGB8888 => {
                let r = (value >> 16) as u8;
                let g = (value >> 8) as u8;
                let b = value as u8;
                let alpha = if (self.lfb_mode & LFB_FORMAT_MASK) == LFB_FORMAT_ARGB8888 {
                    (value >> 24) as u8
                } else {
                    0xff
                };
                let raw = pack_rgb565(r, g, b);
                self.write_lfb_color_pipeline_pixel(base, position, raw, (r, g, b), alpha);
            }
            LFB_FORMAT_DEPTH_RGB565 => {
                let raw = value as u16;
                let depth = (value >> 16) as u16;
                let color = self.lfb_pipeline_depth_color_pixel(
                    base,
                    position,
                    raw,
                    rgb565_components(raw),
                    0xff,
                    depth,
                );
                if let Some(color) = color {
                    if write_color {
                        self.write_color_pixel(base, position, color);
                    }
                    if write_depth {
                        self.write_depth_pixel_at(position, depth);
                    }
                }
            }
            LFB_FORMAT_DEPTH_RGB555 => {
                let raw = value as u16;
                let raw_rgb565 = rgb555_to_rgb565(raw);
                let depth = (value >> 16) as u16;
                let color = self.lfb_pipeline_depth_color_pixel(
                    base,
                    position,
                    raw_rgb565,
                    rgb565_components(raw_rgb565),
                    0xff,
                    depth,
                );
                if let Some(color) = color {
                    if write_color {
                        self.write_color_pixel(base, position, color);
                    }
                    if write_depth {
                        self.write_depth_pixel_at(position, depth);
                    }
                }
            }
            LFB_FORMAT_DEPTH_ARGB1555 => {
                let raw = value as u16;
                let raw_rgb565 = rgb555_to_rgb565(raw);
                let depth = (value >> 16) as u16;
                let color = self.lfb_pipeline_depth_color_pixel(
                    base,
                    position,
                    raw_rgb565,
                    rgb565_components(raw_rgb565),
                    argb1555_alpha(raw),
                    depth,
                );
                if let Some(color) = color {
                    if write_color {
                        self.write_color_pixel(base, position, color);
                    }
                    if write_depth {
                        self.write_depth_pixel_at(position, depth);
                    }
                }
            }
            LFB_FORMAT_DEPTH if write_depth || write_color => {
                let depth0 = value as u16;
                let depth1 = (value >> 16) as u16;
                if let Some(color) = self.lfb_pipeline_depth_only_color(base, position, depth0) {
                    if let Some(color) = color {
                        self.write_color_pixel(base, position, color);
                    }
                    if write_depth {
                        self.write_depth_pixel_at(position, depth0);
                    }
                }
                if let Some(color) = self.lfb_pipeline_depth_only_color(base, next, depth1) {
                    if let Some(color) = color {
                        self.write_color_pixel(base, next, color);
                    }
                    if write_depth {
                        self.write_depth_pixel_at(next, depth1);
                    }
                }
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

    pub fn read_mmio_u8(&self, offset: usize) -> u8 {
        let reg = offset & !0x3;
        let voodoo_reg = offset & 0x3fc;
        let byte = offset & 0x3;
        let chip = tmu_chip_mask(offset);
        let value = match voodoo_reg {
            SST_TREX_INIT0 => self.tmu_register(chip, &self.trex_init0),
            SST_TREX_INIT1 => self.tmu_register(chip, &self.trex_init1),
            _ => self.register_u32(if reg < DISTIRA_REG_ID {
                voodoo_reg
            } else {
                reg
            }),
        };
        (value >> (byte * 8)) as u8
    }

    pub fn write_mmio_u8(&mut self, offset: usize, value: u8) {
        let voodoo_reg = offset & 0x3fc;
        let register = canonical_write_register(offset, self.fbi_init[3]);
        let byte = offset & 0x3;
        let chip = tmu_chip_mask(offset);
        if self.ncc.write_register(chip, register, byte, value) {
            return;
        }
        if register < DISTIRA_REG_ID
            && self
                .texture_iterators
                .write_register(chip, register, byte, value)
        {
            return;
        }
        match register {
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
            SST_DR_DX => merge_color_component(&mut self.triangle_color_dx[0], byte, value),
            SST_DG_DX => merge_color_component(&mut self.triangle_color_dx[1], byte, value),
            SST_DB_DX => merge_color_component(&mut self.triangle_color_dx[2], byte, value),
            SST_DZ_DX => merge_byte(&mut self.triangle_depth_dx, byte, value),
            SST_DA_DX => merge_color_component(&mut self.triangle_alpha_dx, byte, value),
            SST_DR_DY => merge_color_component(&mut self.triangle_color_dy[0], byte, value),
            SST_DG_DY => merge_color_component(&mut self.triangle_color_dy[1], byte, value),
            SST_DB_DY => merge_color_component(&mut self.triangle_color_dy[2], byte, value),
            SST_DZ_DY => merge_byte(&mut self.triangle_depth_dy, byte, value),
            SST_DA_DY => merge_color_component(&mut self.triangle_alpha_dy, byte, value),
            SST_TRIANGLE_CMD => {
                merge_byte(&mut self.triangle_command, byte, value);
                if byte == 3 {
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
            SST_FTRIANGLE_CMD => {
                merge_byte(&mut self.triangle_command, byte, value);
                if byte == 3 {
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
            SST_LFB_MODE => {
                merge_byte(&mut self.lfb_mode, byte, value);
            }
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
            SST_NOP_CMD if byte == 0 && value & 1 != 0 => {
                self.fbi_pixels_in = 0;
                self.fbi_chroma_fail = 0;
                self.fbi_zfunc_fail = 0;
                self.fbi_afunc_fail = 0;
                self.fbi_pixels_out = 0;
            }
            SST_NOP_CMD => {}
            SST_FASTFILL_CMD if byte == 0 => {
                self.run_fastfill();
            }
            SST_FASTFILL_CMD => {}
            SST_SWAPBUFFER_CMD => {
                merge_byte(&mut self.swapbuffer_command, byte, value);
                if byte == 3 {
                    self.issue_swapbuffer_command(self.swapbuffer_command);
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
            SST_CMD_FIFO_DEPTH if byte == 0 && value == 0 => {
                self.command_fifo.clear();
            }
            SST_CMD_FIFO_DEPTH => {}
            SST_CMD_FIFO_HOLES => merge_byte(&mut self.cmd_fifo_holes, byte, value),
            SST_FBI_INIT4 => self.write_fbi_init(4, byte, value),
            SST_BACK_PORCH => merge_byte(&mut self.back_porch, byte, value),
            SST_VIDEO_DIMENSIONS => {
                merge_byte(&mut self.video_dimensions, byte, value);
                self.update_video_dimensions();
            }
            SST_FBI_INIT0 => self.write_fbi_init0(byte, value),
            SST_FBI_INIT1 => self.write_fbi_init1(byte, value),
            SST_FBI_INIT2 => self.write_fbi_init2(byte, value),
            SST_FBI_INIT3 => self.write_fbi_init(3, byte, value),
            SST_H_SYNC => merge_byte(&mut self.h_sync, byte, value),
            SST_V_SYNC => merge_byte(&mut self.v_sync, byte, value),
            SST_CLUT_DATA => self.write_clut_data(byte, value),
            SST_DAC_DATA => {
                merge_byte(&mut self.dac_data_write, byte, value);
                if byte == 3 {
                    self.run_dac_data_write(self.dac_data_write);
                }
            }
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
            SST_TREX_INIT0 => self.write_tmu_registers(chip, 0, byte, value),
            SST_TREX_INIT1 => self.write_tmu_registers(chip, 1, byte, value),
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
            _ if voodoo_reg == SST_TREX_INIT0 => {
                self.write_tmu_registers(chip, 0, byte, value);
            }
            _ if voodoo_reg == SST_TREX_INIT1 => {
                self.write_tmu_registers(chip, 1, byte, value);
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
            DISTIRA_REG_FRONT_BASE => {
                merge_byte(&mut self.display.front_base, byte, value);
                merge_byte(&mut self.scanout_base, byte, value);
            }
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

    pub fn write_texture_u32(&mut self, aperture_offset: usize, value: u32) {
        let Some((tmu, offset)) = self.texture_write_offset(aperture_offset) else {
            return;
        };
        let mask = DISTIRA_TEX_SIZE - 1;
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.texture[tmu][(offset + index) & mask] = byte;
        }
    }

    fn texture_write_offset(&self, aperture_offset: usize) -> Option<(usize, usize)> {
        let aperture_offset = aperture_offset & !3;
        if aperture_offset & (1 << 22) != 0 {
            return None;
        }
        let tmu = usize::from(aperture_offset & (1 << 21) != 0);
        let lod = ((aperture_offset >> 17) & 0xf) as u32;
        if lod > 8 {
            return None;
        }

        let mode = self.texture_mode_for_tmu(tmu);
        let bytes_per_texel = if ((mode >> 8) & 0xf) & 8 != 0 { 2 } else { 1 };
        let s = if bytes_per_texel == 2 {
            (aperture_offset >> 1) & 0xfe
        } else if mode & TEXTUREMODE_SEQ_8_DOWNLD != 0 {
            aperture_offset & 0xfc
        } else {
            (aperture_offset >> 1) & 0xfc
        };
        let t = (aperture_offset >> 9) & 0xff;
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let (width, _) = texture_dimensions(lod_reg, lod);
        let row_offset = t
            .saturating_mul(width)
            .saturating_add(s)
            .saturating_mul(bytes_per_texel);
        let offset = (self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(texture_mip_offset(lod_reg, lod, bytes_per_texel))
            .saturating_add(row_offset);
        Some((tmu, offset & (DISTIRA_TEX_SIZE - 1)))
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

    /// Run the `SST_DAC_DATA` write side effect. This ports the real
    /// hardware protocol 86Box's `vid_voodoo.c` `SST_dacData` case models,
    /// which is itself the register-mapped addr/data bridge
    /// `sst1InitDacRd`/`sst1InitDacWr` poke (dac.c) rather than raw I2C
    /// bit-banging: bits 8-10 select one of 8 indexed DAC registers, bit 11
    /// requests a read cycle (latching a result byte for later `fbiInit2`
    /// readback), and register 5 is special-cased as the ICS5342 GENDAC's
    /// PLL sub-register port. `sst1InitDacDetectICS` (dac.c) probes PLL
    /// sub-registers `VCLK1`/`VCLK7`/`GCLK1` and checks the values below,
    /// which are that chip's power-on defaults.
    fn run_dac_data_write(&mut self, value: u32) {
        self.dac_reg = (value >> DACDATA_ADDR_SHIFT) & 7;
        self.dac_readdata = 0xff;
        if value & DACDATA_RD != 0 {
            if self.dac_reg == DAC_REG_PLL {
                self.dac_readdata = match self.dac_data[7] {
                    ICS_PLL_VCLK1 => ICS_DEFAULT_VCLK1,
                    ICS_PLL_VCLK7 => ICS_DEFAULT_VCLK7,
                    ICS_PLL_GCLK1 => ICS_DEFAULT_GCLK1,
                    _ => 0xff,
                };
            } else {
                self.dac_readdata = self.dac_data[self.dac_reg as usize];
            }
            return;
        }
        if self.dac_reg == DAC_REG_PLL {
            let pll_index = (self.dac_data[4] & 0xf) as usize;
            let byte = (value & 0xff) as u16;
            if !self.dac_reg_ff {
                self.dac_pll_regs[pll_index] = (self.dac_pll_regs[pll_index] & 0xff00) | byte;
            } else {
                self.dac_pll_regs[pll_index] = (self.dac_pll_regs[pll_index] & 0xff) | (byte << 8);
            }
            self.dac_reg_ff = !self.dac_reg_ff;
            if !self.dac_reg_ff {
                self.dac_data[4] = self.dac_data[4].wrapping_add(1);
            }
        } else {
            self.dac_data[self.dac_reg as usize] = (value & 0xff) as u8;
            self.dac_reg_ff = false;
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

    fn write_tmu_registers(&mut self, chip: usize, register: usize, byte: usize, value: u8) {
        let slots = if register == 0 {
            &mut self.trex_init0
        } else {
            &mut self.trex_init1
        };
        if chip & CHIP_TREX0 != 0 {
            merge_byte(&mut slots[0], byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            merge_byte(&mut slots[1], byte, value);
        }
    }

    fn tmu_register(&self, chip: usize, slots: &[u32; 2]) -> u32 {
        if chip & CHIP_TREX0 != 0 {
            slots[0]
        } else if chip & CHIP_TREX1 != 0 {
            slots[1]
        } else {
            0
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
            SST_V_RETRACE => self.frame_phase_line & 0x1fff,
            SST_BACK_PORCH => self.back_porch,
            SST_VIDEO_DIMENSIONS => self.video_dimensions,
            SST_FBI_INIT0 => self.fbi_init[0],
            SST_FBI_INIT1 => self.fbi_init[1],
            SST_FBI_INIT2 => {
                if self.init_enable & INIT_ENABLE_REMAP != 0 {
                    u32::from(self.dac_readdata)
                } else {
                    self.fbi_init[2]
                }
            }
            SST_FBI_INIT3 => self.fbi_init[3] | (1 << 10) | (2 << 8),
            SST_H_SYNC => self.h_sync,
            SST_V_SYNC => self.v_sync,
            SST_CLUT_DATA => self.clut_data_write,
            // The low field is the scanline. Distira has no horizontal dot
            // phase yet, so the high horizontal-position field remains zero.
            SST_HV_RETRACE => self.frame_phase_line & 0x1fff,
            SST_FBI_INIT5 => self.fbi_init[5] & !0x1ff,
            SST_FBI_INIT6 => self.fbi_init[6],
            SST_FBI_INIT7 => self.fbi_init[7] & !0xff,
            SST_TEXTURE_MODE => self.texture_mode,
            SST_TLOD => self.texture_lod,
            SST_TDETAIL => self.texture_detail,
            SST_TEX_BASE_ADDR => self.tex_base_addr,
            DISTIRA_REG_ID => DISTIRA_ID_VALUE,
            DISTIRA_REG_CAPS => DISTIRA_CAPS_VALUE,
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
            _ => u32::MAX,
        }
    }

    fn status_value(&self) -> u32 {
        // 86Box reports a large free FIFO count plus low empty bits when idle.
        // Bit 6 is set outside vertical retrace. Bits 28 through 30 hold the
        // number of submitted swaps, capped at seven.
        let mut status = 0x0fff_f07f;
        if self.in_vretrace() {
            status &= !0x40;
        }
        let swap_count = usize::from(self.pending_swap.is_some()) + self.swap_commands.len();
        status |= (swap_count.min(7) as u32) << 28;
        if !self.fifo_is_empty() || swap_count != 0 {
            status |= 0x380;
        }
        status
    }

    fn lfb_write_base(&self) -> u32 {
        match self.lfb_mode & LFB_WRITE_MASK {
            LFB_WRITE_FRONT => self.display.front_base,
            LFB_WRITE_BACK => self.display.back_base,
            _ => self.display.front_base,
        }
    }

    fn lfb_read_base(&self) -> u32 {
        match self.lfb_mode & LFB_READ_MASK {
            LFB_READ_BACK => self.display.back_base,
            LFB_READ_AUX => self.aux_base,
            _ => self.display.front_base,
        }
    }

    fn lfb_byte_offset(&self, base: u32, aperture_offset: usize) -> Option<usize> {
        let x = (aperture_offset & 0x7fe) | (aperture_offset & 1);
        let y = (aperture_offset >> 11) & 0x3ff;
        usize::try_from(
            u64::from(base)
                .checked_add((y as u64).checked_mul(u64::from(self.display.pitch))?)?
                .checked_add(x as u64)?,
        )
        .ok()
    }

    fn read_lfb_bytes<const N: usize>(&self, aperture_offset: usize) -> [u8; N] {
        let mut bytes = [0xff; N];
        let Some(start) = self.lfb_byte_offset(self.lfb_read_base(), aperture_offset) else {
            return bytes;
        };
        let Some(end) = start.checked_add(N) else {
            return bytes;
        };
        if let Some(source) = self.fb.get(start..end) {
            bytes.copy_from_slice(source);
        }
        bytes
    }

    fn lfb_pipeline_writes_color(&self) -> bool {
        self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.fbz_mode & FBZ_RGB_WMASK != 0
    }

    fn lfb_pipeline_writes_depth(&self) -> bool {
        self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.fbz_mode & FBZ_DEPTH_WMASK != 0
    }

    fn write_lfb_color_pipeline_pixel(
        &mut self,
        base: u32,
        position: (u32, u32),
        raw: u16,
        color: (u8, u8, u8),
        alpha: u8,
    ) {
        let pipeline = self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE != 0;
        let write_color = self.lfb_pipeline_writes_color();
        let write_depth = pipeline && self.fbz_mode & FBZ_DEPTH_WMASK != 0;
        let depth = self.za_color as u16;
        let color = if pipeline {
            self.lfb_pipeline_depth_color_pixel(base, position, raw, color, alpha, depth)
        } else {
            self.lfb_pipeline_color_pixel(base, position, raw, color, alpha)
        };
        if let Some(color) = color {
            if write_color {
                self.write_color_pixel(base, position, color);
            }
            if write_depth {
                self.write_depth_pixel_at(position, depth);
            }
        }
    }

    fn lfb_pipeline_depth_test_passes(&self, position: (u32, u32), depth: u16) -> bool {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.fbz_mode & FBZ_DEPTH_ENABLE == 0 {
            return true;
        }
        let Some(old_depth) = self.read_depth_pixel(position.0, position.1) else {
            return false;
        };
        depth_compare_passes(self.fbz_mode, old_depth, depth)
    }

    fn lfb_pipeline_color_passes(&mut self, color: (u8, u8, u8)) -> bool {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0
            || self.chroma_key_passes(color.0, color.1, color.2)
        {
            return true;
        }
        self.fbi_chroma_fail = self.fbi_chroma_fail.wrapping_add(1);
        false
    }

    fn lfb_pipeline_alpha_passes(&mut self, alpha: u8) -> bool {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.alpha_test_passes(alpha) {
            return true;
        }
        self.fbi_afunc_fail = self.fbi_afunc_fail.wrapping_add(1);
        false
    }

    fn lfb_pipeline_color_pixel(
        &mut self,
        base: u32,
        position: (u32, u32),
        raw: u16,
        color: (u8, u8, u8),
        alpha: u8,
    ) -> Option<u16> {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 {
            return Some(raw);
        }
        let position = self.lfb_pipeline_stipple_position(position)?;
        self.lfb_pipeline_shade_color_at(base, position, raw, color, alpha)
    }

    fn lfb_pipeline_depth_color_pixel(
        &mut self,
        base: u32,
        position: (u32, u32),
        raw: u16,
        color: (u8, u8, u8),
        alpha: u8,
        depth: u16,
    ) -> Option<u16> {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 {
            return Some(raw);
        }
        let position = self.lfb_pipeline_stipple_position(position)?;
        if !self.lfb_pipeline_depth_test_passes(position, depth) {
            return None;
        }
        self.lfb_pipeline_shade_color_at(base, position, raw, color, alpha)
    }

    fn lfb_pipeline_depth_only_color(
        &mut self,
        base: u32,
        position: (u32, u32),
        depth: u16,
    ) -> Option<Option<u16>> {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 {
            return Some(None);
        }
        let position = self.lfb_pipeline_stipple_position(position)?;
        if !self.lfb_pipeline_depth_test_passes(position, depth) {
            return None;
        }
        let alpha = (self.za_color >> 24) as u8;
        self.lfb_pipeline_shade_color_at(base, position, pack_rgb565(0, 0, 0), (0, 0, 0), alpha)
            .map(Some)
    }

    fn lfb_pipeline_stipple_position(&mut self, position: (u32, u32)) -> Option<(u32, u32)> {
        let (x, y) = position;
        self.framebuffer_pixel_offset(self.lfb_write_base(), x, y)?;
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.stipple_test_passes(x, y) {
            return Some((x, y));
        }
        None
    }

    fn stipple_test_passes(&mut self, x: u32, y: u32) -> bool {
        if self.fbz_mode & FBZ_STIPPLE == 0 {
            return true;
        }
        if self.fbz_mode & FBZ_STIPPLE_PATT != 0 {
            let index = ((y & 3) << 3) | ((!x) & 7);
            self.stipple & (1 << index) != 0
        } else {
            self.stipple = self.stipple.rotate_left(1);
            self.stipple & 0x8000_0000 != 0
        }
    }

    fn lfb_pipeline_shade_color_at(
        &mut self,
        base: u32,
        position: (u32, u32),
        _raw: u16,
        color: (u8, u8, u8),
        alpha: u8,
    ) -> Option<u16> {
        if !self.lfb_pipeline_color_passes(color) || !self.lfb_pipeline_alpha_passes(alpha) {
            return None;
        }
        let (x, y) = position;
        let (r, g, b) = self.apply_fog_color(color.0, color.1, color.2);
        let (r, g, b) = self.alpha_blend_color_at_base(base, (x, y), (r, g, b), alpha);
        Some(pack_rgb565_for_pixel(r, g, b, x, y, self.dither_enabled))
    }

    fn write_depth_pixel_at(&mut self, position: (u32, u32), value: u16) {
        let Some(offset) = self.framebuffer_pixel_offset(self.aux_base, position.0, position.1)
        else {
            return;
        };
        self.fb[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_color_pixel(&mut self, base: u32, position: (u32, u32), value: u16) {
        let Some(offset) = self.framebuffer_pixel_offset(base, position.0, position.1) else {
            return;
        };
        self.fb[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn run_fastfill(&mut self) {
        let write_color = self.fbz_mode & FBZ_RGB_WMASK != 0;
        let write_depth = self.fbz_mode & FBZ_DEPTH_WMASK != 0;
        if !write_color && !write_depth {
            return;
        }

        let color = pack_rgb565(
            (self.color1 >> 16) as u8,
            (self.color1 >> 8) as u8,
            self.color1 as u8,
        )
        .to_le_bytes();
        let depth = (self.za_color as u16).to_le_bytes();
        let color_start = match self.fbz_mode & FBZ_DRAW_MASK {
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
            let draw_y = u64::from(self.draw_y(y as u32));
            for x in left..right {
                let pixel_offset = draw_y
                    .saturating_mul(pitch)
                    .saturating_add(x.saturating_mul(2));
                let color_offset = u64::from(color_start).saturating_add(pixel_offset);
                if write_color && color_offset + 1 < len {
                    self.fb[color_offset as usize] = color[0];
                    self.fb[color_offset as usize + 1] = color[1];
                }
                let depth_offset = u64::from(self.aux_base).saturating_add(pixel_offset);
                if write_depth && depth_offset + 1 < len {
                    self.fb[depth_offset as usize] = depth[0];
                    self.fb[depth_offset as usize + 1] = depth[1];
                }
            }
        }
    }

    fn run_triangle_command(&mut self) {
        let coords = self
            .triangle_vertices
            .map(|(x, y)| (fixed_vertex_to_f32(x), fixed_vertex_to_f32(y)));
        let (origin_x, origin_y) = coords[0];
        let texture = self.texture_iterators.raster(
            [self.texture_mode, self.texture_mode_tmu1],
            [self.texture_lod, self.texture_lod_tmu1],
            (origin_x, origin_y),
        );
        let depths = if self.fbz_mode & FBZ_W_BUFFER != 0 {
            coords.map(|(x, y)| {
                let w = self.texture_iterators.fbi_w_at(x, y, origin_x, origin_y);
                // wfloat_depth already returns the same "raw, pre depth_to_u16
                // divide-by-4096" units fixed_depth_at produces for Z, so both
                // paths feed the shared depth_to_u16 conversion unchanged.
                f32::from(wfloat_depth(w)) * 4096.0
            })
        } else {
            coords.map(|(x, y)| {
                fixed_depth_at(
                    self.triangle_depth,
                    self.triangle_depth_dx,
                    self.triangle_depth_dy,
                    x,
                    y,
                    origin_x,
                    origin_y,
                )
            })
        };
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
            s: 0.0,
            t: 0.0,
        });
        let coverage = SstTriangleCoverage::new(
            self.triangle_vertices,
            self.triangle_command & (1 << 31) != 0,
        );
        self.draw_triangle_with_depth(vertices, depths, texture, coverage);
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

    fn write_pixel_at_base(&mut self, base: u32, x: u32, y: u32, pixel: u16) -> bool {
        let Some(offset) = self.framebuffer_pixel_offset(base, x, y) else {
            return false;
        };
        self.fb[offset..offset + 2].copy_from_slice(&pixel.to_le_bytes());
        true
    }

    fn write_draw_pixel(&mut self, x: u32, y: u32, pixel: u16) -> bool {
        let base = match self.fbz_mode & FBZ_DRAW_MASK {
            FBZ_DRAW_FRONT => self.display.front_base,
            _ => self.display.back_base,
        };
        self.write_pixel_at_base(base, x, y, pixel)
    }

    fn depth_test_passes(&self, x: u32, y: u32, depth: u16) -> bool {
        if self.fbz_mode & FBZ_DEPTH_ENABLE == 0 {
            return true;
        }
        let Some(old_depth) = self.read_depth_pixel(x, y) else {
            return false;
        };
        let test_depth = if self.fbz_mode & FBZ_DEPTH_SOURCE != 0 {
            self.za_color as u16
        } else {
            depth
        };
        depth_compare_passes(self.fbz_mode, old_depth, test_depth)
    }

    fn biased_triangle_depth(&self, raw: f32) -> u16 {
        let depth = i32::from(depth_to_u16(raw));
        if self.fbz_mode & FBZ_DEPTH_BIAS == 0 {
            return depth as u16;
        }
        let bias = i32::from(self.za_color as u16 as i16);
        (depth + bias).clamp(0, i32::from(u16::MAX)) as u16
    }

    fn draw_y(&self, logical_y: u32) -> u32 {
        if self.fbz_mode & FBZ_Y_ORIGIN == 0 {
            logical_y
        } else {
            self.display
                .height
                .saturating_sub(1)
                .saturating_sub(logical_y)
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
                | FBZCP_CC_LOCALSELECT_COLOR0
                | FBZCP_CC_LOCALSELECT_OVERRIDE
                | FBZCP_CC_INVERT_OUTPUT)
            == 0
            && ((self.fbz_color_path >> FBZCP_CC_ADD_SHIFT) & 0x3) == 0
            && mselect != CC_MSELECT_CLOCAL
            && mselect != CC_MSELECT_AOTHER
            && mselect != CC_MSELECT_ALOCAL
            && mselect != CC_MSELECT_TEX_ALPHA
            && mselect != CC_MSELECT_TEX_RGB
        {
            return color;
        }
        let mut color = if self.fbz_color_path & FBZCP_CC_ZERO_OTHER != 0 {
            (0_i32, 0_i32, 0_i32)
        } else {
            (i32::from(color.0), i32::from(color.1), i32::from(color.2))
        };
        let local_select_color0 = if self.fbz_color_path & FBZCP_CC_LOCALSELECT_OVERRIDE != 0 {
            texture_alpha & 0x80 != 0
        } else {
            self.fbz_color_path & FBZCP_CC_LOCALSELECT_COLOR0 != 0
        };
        let local = if local_select_color0 {
            (
                i32::from((self.color0 >> 16) as u8),
                i32::from((self.color0 >> 8) as u8),
                i32::from(self.color0 as u8),
            )
        } else {
            (
                i32::from(source.0),
                i32::from(source.1),
                i32::from(source.2),
            )
        };
        if self.fbz_color_path & FBZCP_CC_SUB_CLOCAL != 0 {
            color.0 -= local.0;
            color.1 -= local.1;
            color.2 -= local.2;
        }

        color = if mselect == CC_MSELECT_CLOCAL {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_component(color.0, local.0 as u8, reverse),
                color_path_blend_component(color.1, local.1 as u8, reverse),
                color_path_blend_component(color.2, local.2 as u8, reverse),
            )
        } else if mselect == CC_MSELECT_AOTHER {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_component(color.0, aother, reverse),
                color_path_blend_component(color.1, aother, reverse),
                color_path_blend_component(color.2, aother, reverse),
            )
        } else if mselect == CC_MSELECT_ALOCAL {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_component(color.0, alocal, reverse),
                color_path_blend_component(color.1, alocal, reverse),
                color_path_blend_component(color.2, alocal, reverse),
            )
        } else if mselect == CC_MSELECT_TEX_ALPHA {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_component(color.0, texture_alpha, reverse),
                color_path_blend_component(color.1, texture_alpha, reverse),
                color_path_blend_component(color.2, texture_alpha, reverse),
            )
        } else if mselect == CC_MSELECT_TEX_RGB {
            let reverse = self.fbz_color_path & FBZCP_CC_REVERSE_BLEND != 0;
            (
                color_path_blend_component(color.0, texture_rgb.0, reverse),
                color_path_blend_component(color.1, texture_rgb.1, reverse),
                color_path_blend_component(color.2, texture_rgb.2, reverse),
            )
        } else {
            color
        };

        match (self.fbz_color_path >> FBZCP_CC_ADD_SHIFT) & 0x3 {
            1 => {
                color.0 += local.0;
                color.1 += local.1;
                color.2 += local.2;
            }
            2 => {
                let alocal = i32::from(alocal);
                color.0 += alocal;
                color.1 += alocal;
                color.2 += alocal;
            }
            _ => {}
        }

        (
            color.0.clamp(0, 255) as u8,
            color.1.clamp(0, 255) as u8,
            color.2.clamp(0, 255) as u8,
        )
    }

    fn texture_detail_factor(&self, tmu: usize, lod: u32) -> u8 {
        let detail = self.texture_detail_for_tmu(tmu);
        let max = (detail & 0xff).min(0xff) as i32;
        let bias = ((detail >> 8) & 0x3f) as i32;
        let scale = (detail >> 14) & 0x7;
        ((bias - lod as i32) << scale).clamp(0, max).min(255) as u8
    }

    fn texture_alpha_or_source(&self, alpha: u8, texture_alpha: u8) -> u8 {
        match (self.fbz_color_path >> FBZCP_A_SELECT_SHIFT) & FBZCP_A_SELECT_MASK {
            A_SELECT_TEX => texture_alpha,
            A_SELECT_COLOR1 => (self.color1 >> 24) as u8,
            _ => alpha,
        }
    }

    fn alpha_local_source(&self, alpha: u8, depth_raw: Option<f32>) -> u8 {
        match (self.fbz_color_path >> FBZCP_CCA_LOCALSELECT_SHIFT) & FBZCP_CCA_LOCALSELECT_MASK {
            CCA_LOCALSELECT_COLOR0 => (self.color0 >> 24) as u8,
            CCA_LOCALSELECT_ITER_Z => depth_raw.map_or(0, fixed_depth_to_local_alpha),
            _ => alpha,
        }
    }

    fn texture_alpha_factor(&self, texture_alpha: u8) -> u8 {
        texture_alpha
    }

    fn apply_alpha_path(&self, alocal: u8, aother: u8, texture_alpha: u8) -> u8 {
        let mut alpha = if self.fbz_color_path & FBZCP_CCA_ZERO_OTHER != 0 {
            0
        } else {
            i32::from(aother)
        };
        if self.fbz_color_path & FBZCP_CCA_SUB_CLOCAL != 0 {
            alpha -= i32::from(alocal);
        }
        let mselect = (self.fbz_color_path >> FBZCP_CCA_MSELECT_SHIFT) & FBZCP_CCA_MSELECT_MASK;
        if mselect == CCA_MSELECT_ALOCAL
            || mselect == CCA_MSELECT_AOTHER
            || mselect == CCA_MSELECT_ALOCAL2
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
            alpha = color_path_blend_component(alpha, factor, reverse);
        }
        if ((self.fbz_color_path >> FBZCP_CCA_ADD_SHIFT) & FBZCP_CCA_ADD_MASK) != 0 {
            alpha += i32::from(alocal);
        }
        let mut alpha = alpha.clamp(0, 255) as u8;
        if self.fbz_color_path & FBZCP_CCA_INVERT_OUTPUT != 0 {
            alpha ^= 0xff;
        }
        alpha
    }

    fn sample_tmu_alpha(&self, tmu: usize, sample: TextureSample) -> u8 {
        match (self.texture_mode_for_tmu(tmu) >> 8) & 0xf {
            TEX_A8 => self.sample_tmu_u8(tmu, sample),
            TEX_AI8 => expand4(self.sample_tmu_u8(tmu, sample) >> 4),
            TEX_APAL8 => {
                let index = usize::from(self.sample_tmu_u8(tmu, sample));
                let red = (self.ncc.palette(tmu, index) >> 16) as u8;
                (red & 0xfc) | ((red & 0xc0) >> 6)
            }
            TEX_ARGB8332 | TEX_A8Y4I2Q2 | TEX_A8I8 | TEX_APAL88 => {
                (self.sample_tmu_u16(tmu, sample) >> 8) as u8
            }
            TEX_ARGB1555 => {
                if self.sample_tmu_u16(tmu, sample) & 0x8000 != 0 {
                    0xff
                } else {
                    0
                }
            }
            TEX_ARGB4444 => expand4((self.sample_tmu_u16(tmu, sample) >> 12) as u8),
            _ => 0xff,
        }
    }

    fn sample_tmu_texture(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        match (self.texture_mode_for_tmu(tmu) >> 8) & 0xf {
            TEX_RGB332 => self.sample_tmu_rgb332(tmu, sample),
            TEX_Y4I2Q2 => self.sample_tmu_yiq_ncc(tmu, sample),
            TEX_A8 => self.sample_tmu_a8(tmu, sample),
            TEX_I8 => self.sample_tmu_i8(tmu, sample),
            TEX_AI8 => self.sample_tmu_ai44(tmu, sample),
            TEX_PAL8 => self.sample_tmu_pal8(tmu, sample),
            TEX_APAL8 => self.sample_tmu_apal8(tmu, sample),
            TEX_ARGB8332 => self.sample_tmu_argb8332(tmu, sample),
            TEX_A8Y4I2Q2 => self.sample_tmu_a8_yiq_ncc(tmu, sample),
            TEX_R5G6B5 => self.sample_tmu_rgb565(tmu, sample),
            TEX_ARGB1555 => self.sample_tmu_argb1555(tmu, sample),
            TEX_ARGB4444 => self.sample_tmu_argb4444(tmu, sample),
            TEX_A8I8 => self.sample_tmu_ai88(tmu, sample),
            TEX_APAL88 => self.sample_tmu_apal88(tmu, sample),
            _ => (0, 0, 0),
        }
    }

    fn sample_tmu_rgb332(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let TextureSample { s, t, lod, .. } = sample;
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let (width, height) = texture_dimensions(lod_reg, lod);
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
        let offset = ((self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(texture_mip_offset(lod_reg, lod, 1))
            .saturating_add(texel))
            & (DISTIRA_TEX_SIZE - 1);
        let Some(&raw) = self.texture[tmu].get(offset) else {
            return (0, 0, 0);
        };
        expand_rgb332(raw)
    }

    fn sample_tmu_yiq_ncc(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, sample);
        self.ncc_color(tmu, raw)
    }

    fn sample_tmu_a8_yiq_ncc(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u16(tmu, sample) as u8;
        self.ncc_color(tmu, raw)
    }

    fn ncc_color(&self, tmu: usize, raw: u8) -> (u8, u8, u8) {
        let table = usize::from(self.texture_mode_for_tmu(tmu) & TEXTUREMODE_TNCCSELECT != 0);
        self.ncc.color(tmu, table, raw)
    }

    fn sample_tmu_a8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, sample);
        (raw, raw, raw)
    }

    fn sample_tmu_ai44(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let intensity = expand4(self.sample_tmu_u8(tmu, sample));
        (intensity, intensity, intensity)
    }

    fn sample_tmu_ai88(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let intensity = self.sample_tmu_u16(tmu, sample) as u8;
        (intensity, intensity, intensity)
    }

    fn sample_tmu_pal8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self
            .ncc
            .palette(tmu, usize::from(self.sample_tmu_u8(tmu, sample)));
        ((raw >> 16) as u8, (raw >> 8) as u8, raw as u8)
    }

    fn sample_tmu_apal8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_apal8(
            self.ncc
                .palette(tmu, usize::from(self.sample_tmu_u8(tmu, sample))),
        )
    }

    fn sample_tmu_apal88(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let index = (self.sample_tmu_u16(tmu, sample) & 0xff) as usize;
        let raw = self.ncc.palette(tmu, index);
        ((raw >> 16) as u8, (raw >> 8) as u8, raw as u8)
    }

    fn sample_tmu_argb8332(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_rgb332(self.sample_tmu_u16(tmu, sample) as u8)
    }

    fn sample_tmu_argb1555(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_rgb555(self.sample_tmu_u16(tmu, sample))
    }

    fn sample_tmu_argb4444(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_rgb444(self.sample_tmu_u16(tmu, sample))
    }

    fn sample_tmu_i8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, sample);
        (raw, raw, raw)
    }

    fn sample_tmu_u8(&self, tmu: usize, sample: TextureSample) -> u8 {
        self.texture[tmu][self.tmu_u8_offset(tmu, sample)]
    }

    fn tmu_u8_offset(&self, tmu: usize, sample: TextureSample) -> usize {
        let TextureSample { s, t, lod, .. } = sample;
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let (width, height) = texture_dimensions(lod_reg, lod);
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
        ((self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(texture_mip_offset(lod_reg, lod, 1))
            .saturating_add(texel))
            & (DISTIRA_TEX_SIZE - 1)
    }

    fn sample_tmu_u16(&self, tmu: usize, sample: TextureSample) -> u16 {
        let TextureSample { s, t, lod, .. } = sample;
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let (width, height) = texture_dimensions(lod_reg, lod);
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
        let offset = ((self.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(texture_mip_offset(lod_reg, lod, 2))
            .saturating_add(texel))
            & (DISTIRA_TEX_SIZE - 1);
        u16::from_le_bytes([
            self.texture[tmu][offset],
            self.texture[tmu][(offset + 1) & (DISTIRA_TEX_SIZE - 1)],
        ])
    }

    fn sample_tmu_rgb565(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let TextureSample { s, t, lod, .. } = sample;
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let scale = (1_u32 << lod).max(1) as f32;
        let s = s / scale;
        let t = t / scale;
        let base_addr = self.tex_base_addr_for_tmu_lod(tmu, lod);
        let mip_offset = texture_mip_offset(lod_reg, lod, 2);
        let (width, height) = texture_dimensions(lod_reg, lod);
        let texture_sample = TmuTextureSample {
            tmu,
            width,
            height,
            base_addr,
            mip_offset,
            mode,
            lod_reg,
        };
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
        self.sample_rgb565_texel(s, t, texture_sample)
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

    fn tex_base_addr_for_tmu(&self, tmu: usize) -> u32 {
        let value = if tmu == 0 {
            self.tex_base_addr
        } else {
            self.tex_base_addr_tmu1
        };
        (value & 0x0007_ffff) << 3
    }

    fn tex_base_addr_for_tmu_lod(&self, tmu: usize, lod: u32) -> u32 {
        let lod_reg = self.texture_lod_for_tmu(tmu);
        match texture_base_slot(lod_reg, lod) {
            0 => self.tex_base_addr_for_tmu(tmu),
            1 => (self.tex_base_addr1[tmu] & 0x0007_ffff) << 3,
            2 => (self.tex_base_addr2[tmu] & 0x0007_ffff) << 3,
            _ => (self.tex_base_addr38[tmu] & 0x0007_ffff) << 3,
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
        let offset = ((sample.base_addr as usize)
            .saturating_add(sample.mip_offset)
            .saturating_add(texel))
            & (DISTIRA_TEX_SIZE - 1);
        let raw = u16::from_le_bytes([
            self.texture[sample.tmu][offset],
            self.texture[sample.tmu][(offset + 1) & (DISTIRA_TEX_SIZE - 1)],
        ]);
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
        self.alpha_blend_color_at_base(self.display.back_base, (x, y), (r, g, b), alpha)
    }

    fn alpha_blend_color_at_base(
        &self,
        base: u32,
        position: (u32, u32),
        color: (u8, u8, u8),
        alpha: u8,
    ) -> (u8, u8, u8) {
        let (r, g, b) = color;
        if self.alpha_mode & ALPHA_BLEND_ENABLE == 0 {
            return (r, g, b);
        }
        let (x, y) = position;
        let (dest_r, dest_g, dest_b) = self.read_pixel_rgb_at_base(base, x, y);
        let source_func = (self.alpha_mode >> ALPHA_SRC_FUNC_SHIFT) & 0xf;
        let dest_func = (self.alpha_mode >> ALPHA_DST_FUNC_SHIFT) & 0xf;
        (
            alpha_blend_component(source_func, dest_func, r, dest_r, alpha),
            alpha_blend_component(source_func, dest_func, g, dest_g, alpha),
            alpha_blend_component(source_func, dest_func, b, dest_b, alpha),
        )
    }

    fn read_back_pixel_rgb(&self, x: u32, y: u32) -> (u8, u8, u8) {
        self.read_pixel_rgb_at_base(self.display.back_base, x, y)
    }

    fn read_pixel_rgb_at_base(&self, base: u32, x: u32, y: u32) -> (u8, u8, u8) {
        let raw = self
            .framebuffer_pixel_offset(base, x, y)
            .map(|offset| u16::from_le_bytes([self.fb[offset], self.fb[offset + 1]]))
            .unwrap_or(0);
        (
            expand5(raw >> 11) as u8,
            expand6(raw >> 5) as u8,
            expand5(raw) as u8,
        )
    }

    fn read_depth_pixel(&self, x: u32, y: u32) -> Option<u16> {
        self.framebuffer_pixel_offset(self.aux_base, x, y)
            .map(|offset| u16::from_le_bytes([self.fb[offset], self.fb[offset + 1]]))
    }

    fn write_depth_pixel(&mut self, x: u32, y: u32, depth: u16) -> bool {
        if self.fbz_mode & (FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK)
            != (FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK)
        {
            return false;
        }
        let Some(offset) = self.framebuffer_pixel_offset(self.aux_base, x, y) else {
            return false;
        };
        self.fb[offset..offset + 2].copy_from_slice(&depth.to_le_bytes());
        true
    }

    fn framebuffer_pixel_offset(&self, base: u32, x: u32, y: u32) -> Option<usize> {
        let offset = u64::from(base)
            .checked_add(u64::from(y).checked_mul(u64::from(self.display.pitch))?)?
            .checked_add(u64::from(x).checked_mul(2)?)?;
        let offset = usize::try_from(offset).ok()?;
        (offset.checked_add(2)? <= self.fb.len()).then_some(offset)
    }

    fn clear_aux_depth(&mut self) {
        let Some(start) = usize::try_from(self.aux_base).ok() else {
            return;
        };
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = start.saturating_add(len).min(self.fb.len());
        if let Some(bytes) = self.fb.get_mut(start..end) {
            bytes.fill(0xff);
        }
    }
}

fn lfb_position(aperture_offset: usize, packed_32_bit: bool) -> (u32, u32) {
    let address = if packed_32_bit {
        aperture_offset >> 1
    } else {
        aperture_offset
    };
    (
        ((address & 0x7fe) >> 1) as u32,
        ((address >> 11) & 0x3ff) as u32,
    )
}

#[cfg(test)]
#[path = "distira_test.rs"]
mod tests;
