// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Distira, VEGA's Glide-capable 3D unit. This first slice models the Voodoo
//! Graphics style scanout path: a 16-bit RGB565 front/back frame store, buffer
//! swaps, ordered dither, triangle setup, texture sampling, and host-color decode.

use std::collections::VecDeque;

mod lod_diag;
mod ncc;
mod raster_math;
mod raster_pool;
mod raster_queue;
mod raster_view;
mod registers;
mod texture_combine;
mod texture_raster;

use crate::{DistiraCensus, DistiraCensusKey};
use ncc::NccState;
use raster_math::*;
use raster_pool::{DiagCounter, FrameStore, raster_pool};
use raster_queue::{QueuedTriangle, RasterQueue, ViewMemory, render_band};
use raster_view::{RasterParams, RasterView};
pub use lod_diag::dump as lod_diag_dump;
pub use registers::*;
use texture_combine::TextureCombineTarget;
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
/// Bounding-box pixel count above which a triangle forks across the
/// raster lanes. Below it, the wake-up cost of the pool beats the win.
const PARALLEL_PIXEL_THRESHOLD: usize = 2048;

/// The triangle-constant inputs of `raster_row`, so a lane needs only
/// this, the snapshot, and the frame-store view.
#[derive(Debug, Clone, Copy)]
struct TriangleContext {
    vertices: [DistiraVertex; 3],
    area: f32,
    depths: Option<TriangleDepth>,
    texture: Option<TextureRaster>,
    coverage: Option<SstTriangleCoverage>,
    count_fbi_pixels: bool,
    affine_lods: [u32; 2],
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

/// One lane's per-pixel counters, merged into the device after the join.
struct PixelStats {
    written: u64,
    fbi_pixels_in: u32,
    fbi_zfunc_fail: u32,
    fbi_chroma_fail: u32,
    fbi_afunc_fail: u32,
    fbi_pixels_out: u32,
    pixels_in: u64,
    reject_stipple: u64,
    reject_depth: u64,
    reject_alpha_mask: u64,
    reject_chroma: u64,
    reject_alpha_test: u64,
    pixels_out: u64,
    color_written: u64,
    color_written_nonblack: u64,
    reject_rgb_wmask: u64,
    reject_offscreen: u64,
    depth_written: u64,
    color_offset_min: u32,
    color_offset_max: u32,
    /// The lane's rotating-stipple state, seeded from the register.
    stipple: u32,
}

impl PixelStats {
    fn new(stipple: u32) -> Self {
        Self {
            written: 0,
            fbi_pixels_in: 0,
            fbi_zfunc_fail: 0,
            fbi_chroma_fail: 0,
            fbi_afunc_fail: 0,
            fbi_pixels_out: 0,
            pixels_in: 0,
            reject_stipple: 0,
            reject_depth: 0,
            reject_alpha_mask: 0,
            reject_chroma: 0,
            reject_alpha_test: 0,
            pixels_out: 0,
            color_written: 0,
            color_written_nonblack: 0,
            reject_rgb_wmask: 0,
            reject_offscreen: 0,
            depth_written: 0,
            color_offset_min: u32::MAX,
            color_offset_max: 0,
            stipple,
        }
    }
}

/// Why the triangle rasteriser did not store a colour pixel. One counter per
/// exit, so a run that submits geometry and paints nothing names the predicate
/// that ate it instead of leaving a choice of five.
///
/// These are CUMULATIVE and the guest cannot reset them. The SST-1 statistics
/// registers (`fbi_pixels_in` and friends) share the same event sites, but the
/// guest clears those with `nopCMD` bit 0, so they cannot answer a
/// whole-run question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DistiraTriangleCensus {
    /// Submitted, before any test.
    pub submitted: u64,
    /// Refused for zero signed area.
    pub reject_zero_area: u64,
    /// Of those, how many had all three vertices at the SAME point. That is
    /// the signature of vertex registers the guest never wrote, wrote
    /// somewhere this device does not decode, or converted wrongly.
    pub zero_area_degenerate: u64,
    /// Of those, how many had three DISTINCT collinear points. That is a
    /// scaling or rounding fault, not a missing write.
    pub zero_area_collinear: u64,
    /// Refused because the clipped bounding box held no pixel.
    pub reject_empty_box: u64,

    /// Pixels that reached the per-pixel tests.
    pub pixels_in: u64,
    pub reject_stipple: u64,
    pub reject_depth: u64,
    pub reject_alpha_mask: u64,
    pub reject_chroma: u64,
    pub reject_alpha_test: u64,
    /// Passed every test, so the colour store was attempted.
    pub pixels_out: u64,

    /// Colour actually stored.
    pub color_written: u64,
    /// Of those, how many were NOT black. `painted_bytes` counts non-zero
    /// bytes, so a rasteriser that stores eight million BLACK pixels is
    /// indistinguishable from one that stores none. This separates them.
    pub color_written_nonblack: u64,
    /// Colour dropped because `fbzMode` has the RGB write mask clear.
    pub reject_rgb_wmask: u64,
    /// Colour dropped because the address fell outside the frame store.
    pub reject_offscreen: u64,
    pub depth_written: u64,
    /// The frame-store byte range the triangle path wrote colour into. Says
    /// whether the paint landed in a buffer the scanout reads.
    pub color_offset_min: u32,
    pub color_offset_max: u32,

    /// Byte writes that reached the fixed and the float vertex registers.
    /// Says which of the two vertex protocols the guest drives, and a guest
    /// may drive BOTH: Tomb Raider's splash uses the fixed path and its
    /// engine the float one.
    pub fixed_vertex_writes: u64,
    pub float_vertex_writes: u64,
    /// The first few REJECTED triangles, as the 12.4 fixed point the device
    /// derived, and as the raw bits of the float registers they came from.
    /// The pair is what separates a bad conversion from a bad write.
    pub zero_area_samples: [[(i16, i16); 3]; 4],
    pub zero_area_float_samples: [[(u32, u32); 3]; 4],
    /// The first few ACCEPTED triangles, for comparison.
    pub drawn_samples: [[(i16, i16); 3]; 4],

    /// How many times `drain_raster_queue` actually rasterised a non-empty
    /// batch. This is the async-queue win metric: a guest that fences with
    /// `nopCMD` between triangles used to force one drain per fence (see
    /// `nop_cmd_needs_drain`), so this number tracked the fence count rather
    /// than a real synchronisation need. It should now track only the real
    /// consumers: LFB reads and writes, texture-aperture writes, statistics
    /// reads, `swapbufferCMD`, `fastfillCMD`, and the queue filling up.
    pub queue_drains: u64,
    /// Of those drains, how many batch was large enough to reach the
    /// parallel lanes (see `Distira::batch_lanes`).
    pub queue_drains_parallel: u64,
}

/// Traffic through the three non-register apertures. A guest can look idle in
/// the register histogram and still be hammering one of these, so a "the device
/// is untouched" reading is not safe without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DistiraApertureTraffic {
    pub texture_writes: u64,
    /// Texture writes the aperture decode REFUSED, split by which test refused
    /// them. A refused write reaches no texture memory at all, so the texel it
    /// carried is simply lost and the guest is never told.
    pub texture_writes_refused: u64,
    /// Refused because the aperture address selects a TMU this board does not
    /// have (bit 22, so TMU 2 or 3).
    pub texture_refused_tmu_select: u64,
    /// Refused because the address names a level of detail above 8.
    pub texture_refused_lod: u64,
    /// The OR of every refused aperture offset. Names the bits in play.
    pub texture_refused_bits_or: usize,
    pub lfb_writes: u64,
    pub lfb_reads: u64,
    pub command_fifo_writes: u64,
    /// Filled in by the bus, not the device: a texture-aperture READ is
    /// answered with all-ones before it reaches Distira, and a texture write
    /// NARROWER than a dword is dropped there. Neither is visible to any
    /// counter inside the device, which is exactly why they are here.
    pub texture_reads: u64,
    pub texture_narrow_writes: u64,
}

/// See [`Distira::scanout_state`]. A diagnostic, not part of the device model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistiraScanoutState {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub front_base: u32,
    pub back_base: u32,
    pub scanout_base: u32,
    pub buffer_stride: u32,
    pub display_enabled: bool,
    pub pending_swaps: usize,
    pub swaps_issued: u64,
    pub triangles_drawn: u64,
    pub color_pixels_stored: u64,
    /// `color_pixels_stored` counts the LFB write path ONLY. The triangle
    /// rasteriser stores through a different function and is counted here.
    pub triangles: DistiraTriangleCensus,
    pub fastfill_pixels: u64,
    pub retrace_count: u64,
    pub painted_bytes: usize,
    pub painted_by_buffer: [usize; 3],
    pub fbz_mode: u32,
    pub lfb_mode: u32,
    pub aux_base: u32,
    pub frame_store_bytes: usize,
}

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

/// Per-vertex depth terms for the triangle rasteriser. The Z variant is
/// linear in screen space, so the interpolated value IS the depth. The W
/// variant carries raw 1/w: the wfloat encode is not linear, so each pixel
/// must interpolate 1/w first and encode second, the way 86Box's
/// `vid_voodoo_render.c` iterates `state->w` per pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TriangleDepth {
    Z([f32; 3]),
    W([f32; 3]),
}

impl TriangleDepth {
    /// The depth at one pixel, in the raw "4096 units per depth code" scale
    /// `depth_to_u16` divides back out.
    fn at(self, l0: f32, l1: f32, l2: f32) -> f32 {
        match self {
            Self::Z([za, zb, zc]) => lerp_f32(za, zb, zc, l0, l1, l2),
            Self::W([wa, wb, wc]) => {
                f32::from(wfloat_depth(lerp_f32(wa, wb, wc, l0, l1, l2))) * 4096.0
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSwap {
    target_base: u32,
    interval: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distira {
    /// Every frame size this guest gave Distira, against its count. Distira
    /// had register-level unit tests only until 2026-08-29 and no game had
    /// ever driven it, so this is the first instrument that says whether a
    /// real title reached the 3D unit at all.
    census: DistiraCensus,
    fb: FrameStore,
    texture: [Vec<u8>; 2],
    fifo: VecDeque<DistiraFifoEntry>,
    command_fifo: VecDeque<u32>,
    display: DistiraDisplay,
    scanout_base: u32,
    aux_base: u32,
    buffer_stride: u32,
    /// The display mux, derived purely from `FBIINIT0` bit 0 (`vgaPassthru`).
    /// PRIOR ART: 86Box `src/video/vid_voodoo.c:744-761` calls
    /// `svga_set_override(voodoo->svga, val & 1)` on every FBIINIT0 write and
    /// gates the Voodoo's own scanline draw on the same bit
    /// (`vid_voodoo_display.c:515,635`); DOSBox-X `voodoo_emu.cpp:1764-1775`
    /// calls `Voodoo_Output_Enable(FBIINIT0_VGA_PASSTHRU(data))` and that is
    /// the only switch that flips the render override. Both agree the bit is
    /// bare register state, sampled continuously — not a latch, not gated on
    /// SWAPBUFFER activity, and not touched by FBIINIT1 VIDEO_RESET (that bit
    /// is video *timing* reset; DOSBox-X even names it
    /// `FBIINIT1_VIDEO_TIMING_RESET`). This field is that same continuous
    /// read, cached at the byte-0 write instead of recomputed per query.
    display_enabled: bool,
    /// VIDEO_RESET falling edges in `write_fbi_init1`. Splash is one. A count
    /// that keeps climbing after a VBE yield is a Distira restick that SWAPBUFFER
    /// did not cause.
    video_reset_falling_edges: u64,
    /// FBIINIT0 byte-0 writes that set `display_enabled` because VGA_PASS was
    /// clear and VIDEO_RESET was already clear. Level-triggered, not an edge.
    fbi_init0_byte0_enables: u64,
    dither_enabled: bool,
    /// How many threads rasterise a batch of triangles, caller included.
    /// Chosen from the host core count at construction; see
    /// [`Distira::raster_lanes_for_cores`].
    raster_lanes: usize,
    /// Triangles submitted but not yet drawn. See `distira/raster_queue.rs`.
    raster_queue: RasterQueue,
    /// Whether a guest triangle may wait on the queue at all. Off draws
    /// every triangle at submission, which is what the queue is graded
    /// against.
    raster_queue_enabled: bool,
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
    /// Diagnostic: how many swapbufferCMD writes the guest has issued.
    /// See `scanout_state`; it answers whether a black front buffer is a
    /// swap that did not land or a swap that was never asked for.
    swaps_issued: u64,
    /// Diagnostic: triangles rasterised, and colour pixels actually stored.
    /// `painted_bytes` cannot tell a buffer CLEARED TO BLACK from one never
    /// written, because both are zero -- so it cannot answer whether geometry
    /// reached the rasteriser. These can.
    triangles_drawn: u64,
    /// LFB writes only; the triangle path is counted in `triangle_census`.
    color_pixels_stored: u64,
    triangle_census: DistiraTriangleCensus,
    fastfill_pixels: u64,
    /// Diagnostic: byte writes per SST-1 register index, and the OR of every
    /// MMIO offset seen. Together they say which register protocol a guest
    /// drives and which aperture bits it sets.
    register_writes: Box<[u64; 256]>,
    /// Reads are taken through `&self`, so the counters need a `Cell`. A guest
    /// that stops rendering and never writes again may still be POLLING, and
    /// only the read side shows it.
    register_reads: Box<[DiagCounter; 256]>,
    offset_bits_or: usize,
    /// The three apertures a guest reaches that are NOT register writes. The
    /// register histogram misses all of them, so a guest that looks idle there
    /// can still be hammering texture memory or the LFB.
    aperture_traffic: DistiraApertureTraffic,
    lfb_reads: DiagCounter,
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
            census: DistiraCensus::default(),
            fb: FrameStore::new(DISTIRA_FB_SIZE),
            texture: std::array::from_fn(|_| vec![0; DISTIRA_TEX_SIZE]),
            fifo: VecDeque::new(),
            command_fifo: VecDeque::new(),
            display,
            scanout_base: display.front_base,
            aux_base: buffer_stride * 2,
            buffer_stride,
            display_enabled: false,
            video_reset_falling_edges: 0,
            fbi_init0_byte0_enables: 0,
            dither_enabled: false,
            raster_lanes: raster_pool::host_lanes(),
            raster_queue: RasterQueue::default(),
            raster_queue_enabled: true,
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
            swaps_issued: 0,
            triangles_drawn: 0,
            color_pixels_stored: 0,
            triangle_census: DistiraTriangleCensus {
                color_offset_min: u32::MAX,
                ..DistiraTriangleCensus::default()
            },
            fastfill_pixels: 0,
            register_writes: Box::new([0; 256]),
            register_reads: Box::new(std::array::from_fn(|_| DiagCounter::default())),
            offset_bits_or: 0,
            aperture_traffic: DistiraApertureTraffic::default(),
            lfb_reads: DiagCounter::default(),
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

    /// The init-enable mirror. The Vega outer-routing latch is the canonical
    /// owner of this value; a future Distira-internals canonical section must
    /// exclude it, and capture verifies the mirror has not drifted.
    pub fn init_enable(&self) -> u32 {
        self.init_enable
    }

    pub fn fbi_init0(&self) -> u32 {
        self.fbi_init[0]
    }

    pub fn vga_pass(&self) -> bool {
        self.fbi_init[0] & FBIINIT0_VGA_PASS != 0
    }

    pub fn video_reset(&self) -> bool {
        self.fbi_init[1] & FBIINIT1_VIDEO_RESET != 0
    }

    pub fn video_reset_falling_edges(&self) -> u64 {
        self.video_reset_falling_edges
    }

    pub fn fbi_init0_byte0_enables(&self) -> u64 {
        self.fbi_init0_byte0_enables
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
        // Reference polarity: bit 0 SET means the Voodoo drives the monitor
        // (see the `display_enabled` field doc for the 86Box/DOSBox-X cites).
        let enabled = self.fbi_init[0] & FBIINIT0_VGA_PASS != 0;
        if enabled && !self.display_enabled {
            self.fbi_init0_byte0_enables = self.fbi_init0_byte0_enables.saturating_add(1);
        }
        self.display_enabled = enabled;
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
        // VIDEO_RESET is video *timing* reset (DOSBox-X names the bit
        // `FBIINIT1_VIDEO_TIMING_RESET`; 86Box's falling edge only resets
        // `line`, `swap_count`, `retrace_count`) -- it is not a mux input.
        // The display mux comes from FBIINIT0 bit 0 alone, so this arm
        // touches only the timing/swap state, never `display_enabled`.
        if old & FBIINIT1_VIDEO_RESET != 0 && self.fbi_init[1] & FBIINIT1_VIDEO_RESET == 0 {
            self.frame_phase_line = 0;
            self.reset_swap_state();
            self.video_reset_falling_edges = self.video_reset_falling_edges.saturating_add(1);
        } else if self.fbi_init[1] & FBIINIT1_VIDEO_RESET != 0 {
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

    /// Move the scanout base to a retired swap's target. On real hardware
    /// SWAPBUFFER is pure index rotation (86Box `vid_voodoo_reg.c:139-159`
    /// and its retrace consumer touch only `front_offset`/counters; DOSBox-X
    /// `swapbuffer()` rotates buffer indices) -- it never touches the
    /// FBIINIT0-derived mux.
    fn present_swap(&mut self, target_base: u32) {
        self.scanout_base = target_base;
    }

    fn issue_swapbuffer_command(&mut self, value: u32) {
        self.swaps_issued = self.swaps_issued.saturating_add(1);
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

    /// Every frame size this guest programmed, against its count.
    pub fn census(&self) -> &DistiraCensus {
        &self.census
    }

    pub fn display(&self) -> DistiraDisplay {
        self.display
    }

    pub fn display_enabled(&self) -> bool {
        self.display_enabled
    }

    /// A scanout snapshot, for answering ONE question: when a game drives this
    /// unit and the screen is black, did it render and we fail to show it, or
    /// did it never render?
    ///
    /// `painted_bytes` is what splits those. It counts non-zero bytes in the
    /// whole frame store, not in the scanned-out window, deliberately: pixels
    /// written at a base or pitch the scanout does not read are exactly the case
    /// this exists to catch, and counting only the window would hide them.
    /// A run that reports painted_bytes 0 never rendered; one that reports a
    /// large count with a black picture rendered somewhere nobody is looking.
    /// Diagnostic: `(register index, byte writes)`, busiest first.
    pub fn register_write_histogram(&self) -> Vec<(usize, u64)> {
        let mut rows: Vec<(usize, u64)> = self
            .register_writes
            .iter()
            .enumerate()
            .filter(|(_, count)| **count != 0)
            .map(|(index, count)| (index * 4, *count))
            .collect();
        rows.sort_by_key(|&(register, count)| (std::cmp::Reverse(count), register));
        rows
    }

    /// Diagnostic: `(register index, byte reads)`, busiest first.
    pub fn register_read_histogram(&self) -> Vec<(usize, u64)> {
        let mut rows: Vec<(usize, u64)> = self
            .register_reads
            .iter()
            .enumerate()
            .filter(|(_, count)| count.get() != 0)
            .map(|(index, count)| (index * 4, count.get()))
            .collect();
        rows.sort_by_key(|&(register, count)| (std::cmp::Reverse(count), register));
        rows
    }

    pub fn mmio_offset_bits_or(&self) -> usize {
        self.offset_bits_or
    }

    pub fn scanout_state(&mut self) -> DistiraScanoutState {
        self.drain_raster_queue();
        DistiraScanoutState {
            width: self.display.width,
            height: self.display.height,
            pitch: self.display.pitch,
            front_base: self.display.front_base,
            back_base: self.display.back_base,
            scanout_base: self.scanout_base,
            buffer_stride: self.buffer_stride,
            display_enabled: self.display_enabled,
            pending_swaps: usize::from(self.pending_swap.is_some()) + self.swap_commands.len(),
            swaps_issued: self.swaps_issued,
            triangles_drawn: self.triangles_drawn,
            color_pixels_stored: self.color_pixels_stored,
            triangles: self.triangle_census,
            fastfill_pixels: self.fastfill_pixels,
            retrace_count: self.retrace_count,
            painted_bytes: self.fb.count_nonzero(0, self.fb.len()),
            // Per BUFFER, so a black picture says WHICH buffer holds the paint.
            // A swap alternates the scanout between buffer 0 and buffer 1, so
            // paint in only one of them plus an unchanging picture is the
            // signature of a scanout that is not following the swap.
            fbz_mode: self.fbz_mode,
            lfb_mode: self.lfb_mode,
            aux_base: self.aux_base,
            painted_by_buffer: {
                let stride = self.buffer_stride.max(1) as usize;
                let mut counts = [0usize; 3];
                for (index, slot) in counts.iter_mut().enumerate() {
                    let start = index * stride;
                    let end = (start + stride).min(self.fb.len());
                    if start < end {
                        *slot = self.fb.count_nonzero(start, end);
                    }
                }
                counts
            },
            frame_store_bytes: self.fb.len(),
        }
    }

    pub fn set_dither_enabled(&mut self, enabled: bool) {
        self.drain_raster_queue();
        self.dither_enabled = enabled;
    }

    /// The raster thread count for a host with `cores` logical CPUs: two,
    /// or four when six or more cores leave room for the main emulation
    /// thread.
    pub fn raster_lanes_for_cores(cores: usize) -> usize {
        raster_pool::lanes_for_cores(cores)
    }

    /// Whether a guest triangle may wait on the queue. Off draws every
    /// triangle at submission, which is the behavior the queue is graded
    /// against.
    pub fn set_raster_queue_enabled(&mut self, enabled: bool) {
        self.drain_raster_queue();
        self.raster_queue_enabled = enabled;
    }

    /// How many triangles are submitted but not yet drawn.
    pub fn raster_queue_depth(&self) -> usize {
        self.raster_queue.len()
    }

    /// Override the raster thread count (clamped to 1..=4). One lane
    /// disables the worker pool entirely.
    pub fn set_raster_lanes(&mut self, lanes: usize) {
        self.drain_raster_queue();
        self.raster_lanes = lanes.clamp(1, 4);
    }

    pub fn set_frame_size(&mut self, width: u32, height: u32) {
        self.drain_raster_queue();
        let width = width.clamp(1, DISTIRA_MAX_WIDTH);
        let height = height.clamp(1, DISTIRA_MAX_HEIGHT);
        // Recorded AFTER the clamp: the census reports the geometry the device
        // ended up in, which is the same contract the VGA census keeps.
        self.census.record(DistiraCensusKey { width, height });
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
        self.drain_raster_queue();
        let pixel = pack_rgb565(r, g, b);
        let start = self.display.back_base as usize;
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = start.saturating_add(len).min(self.fb.len());
        for offset in (start..end.saturating_sub(1)).step_by(2) {
            self.fb.write_u16_le(offset, pixel);
        }
    }

    pub fn swap_buffers(&mut self) {
        self.drain_raster_queue();
        self.rotate_buffers();
        self.present_swap(self.display.front_base);
    }

    pub fn draw_triangle(&mut self, vertices: [DistiraVertex; 3]) -> u64 {
        self.draw_triangle_inner(vertices, None, None, None, false)
    }

    fn draw_triangle_with_depth(
        &mut self,
        vertices: [DistiraVertex; 3],
        depths: TriangleDepth,
        texture: TextureRaster,
        coverage: SstTriangleCoverage,
    ) -> u64 {
        self.draw_triangle_inner(vertices, Some(depths), Some(texture), Some(coverage), true)
    }

    /// Copy the register state the pixel pipeline reads. Taken once when a
    /// triangle is submitted, so a later register write cannot reach a
    /// triangle that has not been rasterised yet.
    fn raster_params(&self) -> RasterParams {
        RasterParams {
            display: self.display,
            aux_base: self.aux_base,
            dither_enabled: self.dither_enabled,
            fbz_mode: self.fbz_mode,
            fbz_color_path: self.fbz_color_path,
            alpha_mode: self.alpha_mode,
            fog_mode: self.fog_mode,
            fog_color: self.fog_color,
            za_color: self.za_color,
            chroma_key: self.chroma_key,
            color0: self.color0,
            color1: self.color1,
            stipple: self.stipple,
            texture_mode: self.texture_mode,
            texture_mode_tmu1: self.texture_mode_tmu1,
            texture_lod: self.texture_lod,
            texture_lod_tmu1: self.texture_lod_tmu1,
            texture_detail: self.texture_detail,
            texture_detail_tmu1: self.texture_detail_tmu1,
            tex_base_addr: self.tex_base_addr,
            tex_base_addr_tmu1: self.tex_base_addr_tmu1,
            tex_base_addr1: self.tex_base_addr1,
            tex_base_addr2: self.tex_base_addr2,
            tex_base_addr38: self.tex_base_addr38,
            trex_init1: self.trex_init1,
        }
    }

    /// The pipeline's view of the device THIS INSTANT. The LFB write path and
    /// the texture-aperture decode use it; a triangle uses the params it was
    /// submitted with instead.
    fn raster_view(&self) -> RasterView<'_> {
        self.view_memory().view(self.raster_params())
    }

    fn view_memory(&self) -> ViewMemory<'_> {
        ViewMemory {
            fb: &self.fb,
            texture: &self.texture,
            ncc: &self.ncc,
        }
    }

    /// Set a triangle up and either queue it or draw it.
    ///
    /// `defer` is what separates the guest's path from the direct one: a
    /// guest triangle may wait on the queue, but `draw_triangle` returns the
    /// pixel count for THIS triangle and so has to draw it now.
    fn draw_triangle_inner(
        &mut self,
        vertices: [DistiraVertex; 3],
        depths: Option<TriangleDepth>,
        texture: Option<TextureRaster>,
        coverage: Option<SstTriangleCoverage>,
        defer: bool,
    ) -> u64 {
        let count_fbi_pixels = coverage.is_some();
        if count_fbi_pixels {
            self.triangle_census.submitted += 1;
        }
        let [a, b, c] = vertices;
        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area == 0.0 {
            if count_fbi_pixels {
                let census = &mut self.triangle_census;
                census.reject_zero_area += 1;
                let points = [(a.x, a.y), (b.x, b.y), (c.x, c.y)];
                if points[0] == points[1] && points[1] == points[2] {
                    census.zero_area_degenerate += 1;
                } else if points[0] != points[1] && points[1] != points[2] && points[0] != points[2]
                {
                    census.zero_area_collinear += 1;
                }
                let slot = (census.reject_zero_area - 1) as usize;
                if slot < census.zero_area_samples.len() {
                    census.zero_area_samples[slot] = raw_vertex_sample(self.triangle_vertices);
                    census.zero_area_float_samples[slot] = self.ftriangle_vertices;
                }
            }
            return 0;
        }
        if count_fbi_pixels {
            let slot = (self.triangle_census.submitted - self.triangle_census.reject_zero_area - 1)
                as usize;
            if slot < self.triangle_census.drawn_samples.len() {
                self.triangle_census.drawn_samples[slot] =
                    raw_vertex_sample(self.triangle_vertices);
            }
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

        if count_fbi_pixels && (min_x >= max_x || min_y >= max_y) {
            self.triangle_census.reject_empty_box += 1;
        }

        let context = TriangleContext {
            vertices: [a, b, c],
            area,
            depths,
            texture,
            coverage,
            count_fbi_pixels,
            affine_lods: [self.texture_lod, self.texture_lod_tmu1],
            min_x,
            max_x,
            min_y,
            max_y,
        };
        let triangle = QueuedTriangle {
            params: self.raster_params(),
            context,
        };
        if defer && self.raster_queue_enabled && triangle_defers(&triangle) {
            if !self.raster_queue.push(triangle) {
                // Full. Draw what is waiting, then this one joins an empty
                // queue, so a triangle is never dropped. The second push
                // cannot refuse: the drain above leaves the queue at zero and
                // the capacity is not zero.
                self.drain_raster_queue();
                // Not inside the assertion: a release build compiles the
                // assertion's expression away, and the push has to happen.
                let queued = self.raster_queue.push(triangle);
                debug_assert!(queued, "a drained queue accepts");
            }
            return 0;
        }

        // The immediate path. Draining first leaves this triangle alone in
        // the batch, so the pixel count belongs to it and to nothing else,
        // and the push cannot refuse for the same reason.
        self.drain_raster_queue();
        let queued = self.raster_queue.push(triangle);
        debug_assert!(queued, "a drained queue accepts");
        self.drain_raster_queue()
    }

    /// Draw every triangle waiting on the queue and return how many pixels
    /// they stored.
    ///
    /// One fork and one join for the whole batch. Lane `i` owns the
    /// FRAMEBUFFER rows where `draw_y(y) % lanes == i` and walks the batch in
    /// submission order, so overlapping triangles land in the order the guest
    /// sent them. `distira/raster_queue.rs` has why the framebuffer row is the
    /// one that has to partition and not the triangle's own row.
    fn drain_raster_queue(&mut self) -> u64 {
        if self.raster_queue.is_empty() {
            return 0;
        }
        let jobs = self.raster_queue.take();
        let lanes = self.batch_lanes(&jobs);
        self.triangle_census.queue_drains += 1;
        if lanes > 1 {
            self.triangle_census.queue_drains_parallel += 1;
        }
        let mut lane_stats: Vec<PixelStats> = (0..lanes).map(|_| PixelStats::new(0)).collect();
        if lanes == 1 {
            render_band(&jobs, self.view_memory(), 0, 1, &mut lane_stats[0]);
        } else {
            let jobs = &jobs;
            let memory = self.view_memory();
            let lane_count = lanes as u32;
            // A lane accumulates into a stack-local `PixelStats` and stores
            // it once at the end: the `lane_stats` elements share cache
            // lines, and a per-pixel counter write there makes the lanes
            // false-share their way back to serial speed.
            let (worker_stats, install_stats) = lane_stats.split_at_mut(lanes - 1);
            // `install` moves the whole fork onto the pool: the scope then
            // spawns into a worker-local queue, which the other workers
            // steal far faster than an external injection wakes them. The
            // installed worker rasterises the last lane.
            raster_pool().install(|| {
                rayon::scope(|scope| {
                    for (lane, stats) in worker_stats.iter_mut().enumerate() {
                        scope.spawn(move |_| {
                            render_band(jobs, memory, lane as u32, lane_count, stats);
                        });
                    }
                    render_band(
                        jobs,
                        memory,
                        lane_count - 1,
                        lane_count,
                        &mut install_stats[0],
                    );
                });
            });
        }
        let written = self.merge_pixel_stats(&lane_stats, &jobs, lanes);
        self.raster_queue.recycle(jobs);
        written
    }

    /// How many lanes a batch is worth. Below the threshold the wake-up cost
    /// of the pool beats the win and the calling thread draws the batch.
    ///
    /// The two measures answer different questions. Pixels are the work, so
    /// they sum over the batch. Rows are the parallelism, and lanes divide
    /// DISTINCT rows, so they take the union span rather than the sum: a stack
    /// of small triangles sitting on top of each other has one triangle's
    /// worth of rows to share out however many triangles it holds.
    fn batch_lanes(&self, jobs: &[QueuedTriangle]) -> usize {
        if self.raster_lanes < 2 {
            return 1;
        }
        let mut lowest = u32::MAX;
        let mut highest = 0;
        let mut pixels = 0usize;
        for job in jobs {
            let job_rows = job.context.max_y.saturating_sub(job.context.min_y) as usize;
            let columns = job.context.max_x.saturating_sub(job.context.min_x) as usize;
            if job_rows == 0 {
                continue;
            }
            lowest = lowest.min(job.context.min_y);
            highest = highest.max(job.context.max_y);
            pixels += job_rows.saturating_mul(columns);
        }
        let rows = highest.saturating_sub(lowest) as usize;
        if rows >= self.raster_lanes * 2 && pixels >= PARALLEL_PIXEL_THRESHOLD {
            self.raster_lanes
        } else {
            1
        }
    }

    /// Fold a batch's lane counters into the device.
    ///
    /// The census fields are added unconditionally: `raster_row` only ever
    /// bumps them for a triangle whose context asked for them, so a lane's
    /// counters are already zero for the triangles that did not.
    fn merge_pixel_stats(
        &mut self,
        lane_stats: &[PixelStats],
        jobs: &[QueuedTriangle],
        lanes: usize,
    ) -> u64 {
        let mut written = 0;
        for stats in lane_stats {
            written += stats.written;
            self.fbi_pixels_in = self.fbi_pixels_in.wrapping_add(stats.fbi_pixels_in);
            self.fbi_zfunc_fail = self.fbi_zfunc_fail.wrapping_add(stats.fbi_zfunc_fail);
            self.fbi_chroma_fail = self.fbi_chroma_fail.wrapping_add(stats.fbi_chroma_fail);
            self.fbi_afunc_fail = self.fbi_afunc_fail.wrapping_add(stats.fbi_afunc_fail);
            self.fbi_pixels_out = self.fbi_pixels_out.wrapping_add(stats.fbi_pixels_out);
            {
                let census = &mut self.triangle_census;
                census.pixels_in += stats.pixels_in;
                census.reject_stipple += stats.reject_stipple;
                census.reject_depth += stats.reject_depth;
                census.reject_alpha_mask += stats.reject_alpha_mask;
                census.reject_chroma += stats.reject_chroma;
                census.reject_alpha_test += stats.reject_alpha_test;
                census.pixels_out += stats.pixels_out;
                census.color_written += stats.color_written;
                census.color_written_nonblack += stats.color_written_nonblack;
                census.reject_rgb_wmask += stats.reject_rgb_wmask;
                census.reject_offscreen += stats.reject_offscreen;
                census.depth_written += stats.depth_written;
                census.color_offset_min = census.color_offset_min.min(stats.color_offset_min);
                if stats.color_offset_max != 0 || stats.color_written != 0 {
                    census.color_offset_max = census.color_offset_max.max(stats.color_offset_max);
                }
            }
        }
        // The rotating stipple register keeps the value of the lane that
        // rasterised the LAST row of the last triangle. With one lane that is
        // the exact serial behavior; with more lanes it is the same
        // approximation 86Box makes with per-render-thread stipple state.
        //
        // Only the ROTATING stipple writes back. A patterned one is a pure
        // function of the pixel, so a lane's copy never moved, and storing it
        // would undo a stipple the guest wrote while the batch was waiting.
        // A rotating triangle is never batched with another (see
        // `triangle_defers`), so the batch that reaches this is one triangle
        // long whenever the value matters.
        if let Some(job) = jobs.last()
            && job.params.fbz_mode & FBZ_STIPPLE != 0
            && job.params.fbz_mode & FBZ_STIPPLE_PATT == 0
            && job.context.max_y > job.context.min_y
            && let Some(stats) = lane_stats.get((job.context.max_y - 1) as usize % lanes)
        {
            self.stipple = stats.stipple;
        }
        written
    }

    pub fn scanout_argb(&mut self) -> Vec<u32> {
        self.drain_raster_queue();
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
                    self.fb.read_u16_le(off as usize).unwrap_or(0)
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

    pub fn read_lfb_u8(&mut self, offset: usize) -> u8 {
        self.drain_raster_queue();
        self.lfb_reads.increment();
        self.lfb_byte_offset(self.lfb_read_base(), offset)
            .and_then(|offset| self.fb.get(offset))
            .unwrap_or(0xff)
    }

    pub fn read_lfb_u16(&mut self, offset: usize) -> u16 {
        self.drain_raster_queue();
        u16::from_le_bytes(self.read_lfb_bytes::<2>(offset & !1))
    }

    pub fn read_lfb_u32(&mut self, offset: usize) -> u32 {
        self.drain_raster_queue();
        u32::from_le_bytes(self.read_lfb_bytes::<4>(offset & !1))
    }

    /// Diagnostic: the aperture counters. See [`DistiraApertureTraffic`].
    pub fn aperture_traffic(&self) -> DistiraApertureTraffic {
        DistiraApertureTraffic {
            lfb_reads: self.lfb_reads.get(),
            ..self.aperture_traffic
        }
    }

    // `vega.rs::write_wide_memory` silently drops `BusWidth::Byte` before it
    // reaches the Distira LFB, so this method has no caller in the workspace
    // today. That drop is a separate, pre-existing bug (a guest that wrote
    // the LFB a byte at a time would lose every pixel) and is not this PR's
    // to fix.
    pub fn write_lfb_u8(&mut self, offset: usize, value: u8) {
        self.drain_raster_queue();
        self.aperture_traffic.lfb_writes += 1;
        let base = if self.lfb_mode & LFB_FORMAT_MASK == LFB_FORMAT_DEPTH {
            self.aux_base
        } else {
            self.lfb_write_base()
        };
        if let Some(offset) = self.lfb_byte_offset(base, offset) {
            self.fb.set(offset, value);
        }
    }

    pub fn write_lfb_u16(&mut self, offset: usize, value: u16) {
        self.drain_raster_queue();
        self.aperture_traffic.lfb_writes += 1;
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
        self.drain_raster_queue();
        self.aperture_traffic.lfb_writes += 1;
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
        self.aperture_traffic.command_fifo_writes += 1;
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
        // The entries below replay through the register, LFB and texture
        // paths, each of which draws the raster queue where it must.
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

    pub fn read_mmio_u8(&mut self, offset: usize) -> u8 {
        self.register_reads[(offset >> 2) & 0xff].increment();
        let reg = offset & !0x3;
        let voodoo_reg = offset & 0x3fc;
        if register_read_needs_raster(if reg < DISTIRA_REG_ID {
            voodoo_reg
        } else {
            reg
        }) {
            self.drain_raster_queue();
        }
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
        self.register_writes[(offset >> 2) & 0xff] += 1;
        self.offset_bits_or |= offset;
        let voodoo_reg = offset & 0x3fc;
        let register = canonical_write_register(offset, self.fbi_init[3]);
        let byte = offset & 0x3;
        // Classified BEFORE the NCC and texture-iterator arms below, which
        // return early: an NCC or palette write never reaches the `match`,
        // and it is one of the writes the queue has to be drawn for.
        //
        // `nopCMD` is carved out from the generic rule. On real Glide it is
        // Voodoo's fence: it travels the FIFO in order behind whatever
        // triangles precede it, so it needs ORDERING against the queue, not
        // a synchronous join. `nop_cmd_needs_drain` proves the common case
        // (byte != 0, or byte 0 with the reset bit clear) touches no device
        // state at all -- not even order matters, because nothing is read or
        // written. Only the reset-statistics case
        // (byte == 0 && value & 1 != 0) still drains: it zeroes
        // `fbi_pixels_in` and friends directly, and those counters are only
        // correct if every triangle submitted before the reset has already
        // folded its pixels in (see `merge_pixel_stats`). A reset that ran
        // ahead of still-queued triangles would let their pixels land in the
        // wrong epoch, so that path is not provably ordering-only and it
        // keeps the pre-existing drain.
        //
        // Gating on `voodoo_reg` here (the `match` below keys on `register`
        // instead) is safe only because the two never disagree about
        // `SST_NOP_CMD`: `0x120 >= 0x100`, so `canonical_write_register`'s
        // remap table never touches it, and no remap TARGET is `0x120`
        // either (they are all parameter and triangle registers). So
        // `voodoo_reg == SST_NOP_CMD` iff `register == SST_NOP_CMD`, and
        // gating on one is equivalent to gating on the other. Extending the
        // remap table has to preserve that, or this carve-out silently stops
        // firing.
        if voodoo_reg == SST_NOP_CMD {
            if nop_cmd_needs_drain(byte, value) {
                self.drain_raster_queue();
            }
        } else if !raster_snapshot_covers_register(register)
            || !raster_snapshot_covers_register(voodoo_reg)
        {
            self.drain_raster_queue();
        }
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
            SST_VERTEX_AX => {
                self.triangle_census.fixed_vertex_writes += 1;
                merge_vertex_component(&mut self.triangle_vertices[0].0, byte, value);
            }
            SST_VERTEX_AY => {
                self.triangle_census.fixed_vertex_writes += 1;
                merge_vertex_component(&mut self.triangle_vertices[0].1, byte, value);
            }
            SST_VERTEX_BX => {
                self.triangle_census.fixed_vertex_writes += 1;
                merge_vertex_component(&mut self.triangle_vertices[1].0, byte, value);
            }
            SST_VERTEX_BY => {
                self.triangle_census.fixed_vertex_writes += 1;
                merge_vertex_component(&mut self.triangle_vertices[1].1, byte, value);
            }
            SST_VERTEX_CX => {
                self.triangle_census.fixed_vertex_writes += 1;
                merge_vertex_component(&mut self.triangle_vertices[2].0, byte, value);
            }
            SST_VERTEX_CY => {
                self.triangle_census.fixed_vertex_writes += 1;
                merge_vertex_component(&mut self.triangle_vertices[2].1, byte, value);
            }
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
                self.triangle_census.float_vertex_writes += 1;
                merge_byte(&mut self.ftriangle_vertices[0].0, byte, value);
                self.triangle_vertices[0].0 = float_vertex_to_fixed(self.ftriangle_vertices[0].0);
            }
            SST_FVERTEX_AY => {
                self.triangle_census.float_vertex_writes += 1;
                merge_byte(&mut self.ftriangle_vertices[0].1, byte, value);
                self.triangle_vertices[0].1 = float_vertex_to_fixed(self.ftriangle_vertices[0].1);
            }
            SST_FVERTEX_BX => {
                self.triangle_census.float_vertex_writes += 1;
                merge_byte(&mut self.ftriangle_vertices[1].0, byte, value);
                self.triangle_vertices[1].0 = float_vertex_to_fixed(self.ftriangle_vertices[1].0);
            }
            SST_FVERTEX_BY => {
                self.triangle_census.float_vertex_writes += 1;
                merge_byte(&mut self.ftriangle_vertices[1].1, byte, value);
                self.triangle_vertices[1].1 = float_vertex_to_fixed(self.ftriangle_vertices[1].1);
            }
            SST_FVERTEX_CX => {
                self.triangle_census.float_vertex_writes += 1;
                merge_byte(&mut self.ftriangle_vertices[2].0, byte, value);
                self.triangle_vertices[2].0 = float_vertex_to_fixed(self.ftriangle_vertices[2].0);
            }
            SST_FVERTEX_CY => {
                self.triangle_census.float_vertex_writes += 1;
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
            SST_NOP_CMD if nop_cmd_needs_drain(byte, value) => {
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
                // Census on the COMPLETING byte only, the same convention
                // SST_DAC_DATA uses below. A dword register arrives as four byte
                // writes, and counting each one would record three intermediate
                // geometries the guest never asked for.
                //
                // Recorded here as well as in set_frame_size because THIS is the
                // path a real Glide driver takes: videoDimensions is an SST-1
                // register, while DISTIRA_REG_FB_WIDTH/HEIGHT are this chip's
                // private interface that no period driver writes. Hooking only
                // the private path made the census read EMPTY for Tomb Raider
                // Gold's 3dfx build while its presented frame was plainly
                // 640x480 -- an instrument that answers the same way whether the
                // guest reached Distira or not is not evidence.
                if byte == 3 {
                    self.census.record(DistiraCensusKey {
                        width: self.display.width,
                        height: self.display.height,
                    });
                }
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
        // Texture memory is NOT part of the snapshot: it is megabytes, and a
        // game uploads it at level load rather than between triangles. So the
        // queue is drawn before an upload lands instead of copied.
        self.drain_raster_queue();
        self.aperture_traffic.texture_writes += 1;
        let Some((tmu, offset)) = self.texture_write_offset(aperture_offset) else {
            let traffic = &mut self.aperture_traffic;
            traffic.texture_writes_refused += 1;
            traffic.texture_refused_bits_or |= aperture_offset;
            if aperture_offset & (1 << 22) != 0 {
                traffic.texture_refused_tmu_select += 1;
            } else {
                traffic.texture_refused_lod += 1;
            }
            return;
        };
        let mask = DISTIRA_TEX_SIZE - 1;
        lod_diag::note_upload_bytes(tmu, offset, 4, self.texture_write_offset_unmasked(aperture_offset));
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
        lod_diag::note_upload_lod(tmu, lod);

        let params = self.raster_params();
        let mode = params.texture_mode_for_tmu(tmu);
        let bytes_per_texel = if ((mode >> 8) & 0xf) & 8 != 0 { 2 } else { 1 };
        let s = if bytes_per_texel == 2 {
            (aperture_offset >> 1) & 0xfe
        } else if mode & TEXTUREMODE_SEQ_8_DOWNLD != 0 {
            aperture_offset & 0xfc
        } else {
            (aperture_offset >> 1) & 0xfc
        };
        let t = (aperture_offset >> 9) & 0xff;
        let lod_reg = params.texture_lod_for_tmu(tmu);
        let (width, _) = texture_dimensions(lod_reg, lod);
        let row_offset = t
            .saturating_mul(width)
            .saturating_add(s)
            .saturating_mul(bytes_per_texel);
        let offset = (params.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(texture_mip_offset(lod_reg, lod, bytes_per_texel))
            .saturating_add(row_offset);
        Some((tmu, offset & (DISTIRA_TEX_SIZE - 1)))
    }

    /// DIAGNOSTIC: the same address `texture_write_offset` computes, before
    /// the 2 MB wrap, so the diag can see an upload that ran past the TMU.
    fn texture_write_offset_unmasked(&self, aperture_offset: usize) -> usize {
        let aperture_offset = aperture_offset & !3;
        let tmu = usize::from(aperture_offset & (1 << 21) != 0);
        let lod = ((aperture_offset >> 17) & 0xf) as u32;
        let params = self.raster_params();
        let mode = params.texture_mode_for_tmu(tmu);
        let bytes_per_texel = if ((mode >> 8) & 0xf) & 8 != 0 { 2 } else { 1 };
        let s = if bytes_per_texel == 2 {
            (aperture_offset >> 1) & 0xfe
        } else if mode & TEXTUREMODE_SEQ_8_DOWNLD != 0 {
            aperture_offset & 0xfc
        } else {
            (aperture_offset >> 1) & 0xfc
        };
        let t = (aperture_offset >> 9) & 0xff;
        let lod_reg = params.texture_lod_for_tmu(tmu);
        let (width, _) = texture_dimensions(lod_reg, lod);
        let row_offset = t
            .saturating_mul(width)
            .saturating_add(s)
            .saturating_mul(bytes_per_texel);
        (params.tex_base_addr_for_tmu_lod(tmu, lod) as usize)
            .saturating_add(texture_mip_offset(lod_reg, lod, bytes_per_texel))
            .saturating_add(row_offset)
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
        if end <= self.fb.len() {
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = self.fb.get(start + index).unwrap_or(0xff);
            }
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
        let Some(old_depth) = self.raster_view().read_depth_pixel(position.0, position.1) else {
            return false;
        };
        depth_compare_passes(self.fbz_mode, old_depth, depth)
    }

    fn lfb_pipeline_color_passes(&mut self, color: (u8, u8, u8)) -> bool {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0
            || self
                .raster_params()
                .chroma_key_passes(color.0, color.1, color.2)
        {
            return true;
        }
        self.fbi_chroma_fail = self.fbi_chroma_fail.wrapping_add(1);
        false
    }

    fn lfb_pipeline_alpha_passes(&mut self, alpha: u8) -> bool {
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0
            || self.raster_params().alpha_test_passes(alpha)
        {
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
        self.raster_params()
            .framebuffer_pixel_offset(self.lfb_write_base(), x, y)?;
        if self.lfb_mode & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.stipple_test_passes(x, y) {
            return Some((x, y));
        }
        None
    }

    fn stipple_test_passes(&mut self, x: u32, y: u32) -> bool {
        let mut stipple = self.stipple;
        let passes = self.raster_params().stipple_test(&mut stipple, x, y);
        self.stipple = stipple;
        passes
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
        let view = self.raster_view();
        let (r, g, b) = view.apply_fog_color(color.0, color.1, color.2);
        let (r, g, b) = view.alpha_blend_color_at_base(base, (x, y), (r, g, b), alpha);
        Some(pack_rgb565_for_pixel(r, g, b, x, y, self.dither_enabled))
    }

    fn write_depth_pixel_at(&mut self, position: (u32, u32), value: u16) {
        let Some(offset) =
            self.raster_params()
                .framebuffer_pixel_offset(self.aux_base, position.0, position.1)
        else {
            return;
        };
        self.fb.write_u16_le(offset, value);
    }

    fn write_color_pixel(&mut self, base: u32, position: (u32, u32), value: u16) {
        self.color_pixels_stored = self.color_pixels_stored.saturating_add(1);
        let Some(offset) = self
            .raster_params()
            .framebuffer_pixel_offset(base, position.0, position.1)
        else {
            return;
        };
        self.fb.write_u16_le(offset, value);
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
        );
        let depth = self.za_color as u16;
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
        let params = self.raster_params();

        for y in low_y..high_y {
            let draw_y = u64::from(params.draw_y(y as u32));
            for x in left..right {
                let pixel_offset = draw_y
                    .saturating_mul(pitch)
                    .saturating_add(x.saturating_mul(2));
                let color_offset = u64::from(color_start).saturating_add(pixel_offset);
                if write_color && color_offset + 1 < len {
                    self.fb.write_u16_le(color_offset as usize, color);
                    self.fastfill_pixels += 1;
                }
                let depth_offset = u64::from(self.aux_base).saturating_add(pixel_offset);
                if write_depth && depth_offset + 1 < len {
                    self.fb.write_u16_le(depth_offset as usize, depth);
                }
            }
        }
    }

    fn run_triangle_command(&mut self) {
        self.triangles_drawn = self.triangles_drawn.saturating_add(1);
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
            // Raw per-vertex 1/w. The rasteriser interpolates 1/w per pixel
            // and runs the wfloat encode there; the encode is not linear, so
            // encoding here and interpolating the code would misplace every
            // interior pixel of a large triangle.
            TriangleDepth::W(
                coords.map(|(x, y)| self.texture_iterators.fbi_w_at(x, y, origin_x, origin_y)),
            )
        } else {
            TriangleDepth::Z(coords.map(|(x, y)| {
                fixed_depth_at(
                    self.triangle_depth,
                    self.triangle_depth_dx,
                    self.triangle_depth_dy,
                    x,
                    y,
                    origin_x,
                    origin_y,
                )
            }))
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

    fn clear_aux_depth(&mut self) {
        let Some(start) = usize::try_from(self.aux_base).ok() else {
            return;
        };
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = start.saturating_add(len).min(self.fb.len());
        self.fb.fill(start, end, 0xff);
    }
}

/// Whether a triangle may wait on the queue.
///
/// The ROTATING stipple is the one piece of per-pixel state that chains from
/// one triangle to the next, and the chain cannot be split across lanes. Such
/// a triangle is drawn at submission, exactly as it was before the queue
/// existed. The patterned stipple is a pure function of the pixel and queues
/// like anything else.
fn triangle_defers(triangle: &QueuedTriangle) -> bool {
    let mode = triangle.params.fbz_mode;
    mode & FBZ_STIPPLE == 0 || mode & FBZ_STIPPLE_PATT != 0
}

/// Whether [`RasterParams`] carries everything a write to this register
/// changes, so a triangle already on the queue does not have to be drawn
/// before the write lands.
///
/// The covered set is the triangle setup block (both the fixed and the float
/// protocol, the texture iterators, and the two triangle commands), the clip
/// window, and the mode, colour and TMU registers the snapshot copies.
/// Everything else draws the queue first: the framebuffer layout, the DAC and
/// the CLUT, the command registers, `lfbMode`, and the NCC tables and palette,
/// which the snapshot deliberately does not carry.
fn raster_snapshot_covers_register(register: usize) -> bool {
    matches!(
        register,
        // Vertices, colour, depth, alpha and texture iterators, fixed and
        // float, plus triangleCMD and ftriangleCMD themselves.
        SST_VERTEX_AX..=SST_FTRIANGLE_CMD
        // fbzColorPath, fogMode, alphaMode, fbzMode.
        | SST_FBZ_COLOR_PATH..=SST_FBZ_MODE
        // The clip window, consumed when the triangle is set up.
        | SST_CLIP_LEFT_RIGHT
        | SST_CLIP_LOW_Y_HIGH_Y
        | SST_FOG_COLOR..=SST_CHROMA_KEY
        | SST_STIPPLE..=SST_COLOR1
        | SST_TEXTURE_MODE..=SST_TEX_BASE_ADDR38
        | SST_TREX_INIT1
    )
}

/// Whether a `nopCMD` write actually mutates device state that a queued
/// triangle's later drain could still affect. See the call site in
/// `write_mmio_u8` for the ordering argument.
///
/// Real Voodoo hardware also gates a SECOND effect on bit 1: it resets
/// `fbiTrianglesOut` (DOSBox-X/MAME `voodoo_emu.cpp`, the `nopCMD` case).
/// This predicate ignores it, and that is correct ONLY because Distira does
/// not implement `fbiTrianglesOut` today -- there is no such register or
/// counter anywhere in this device. The day that counter is added, this
/// predicate must widen to `value & 3 != 0`, or a bit-1-only `nopCMD` will
/// reset it without draining first, which is exactly the epoch bug the
/// bit-0 case exists to avoid. Nothing enforces that widening happens; it is
/// a trap for whoever adds the counter, not a currently-live gap.
fn nop_cmd_needs_drain(byte: usize, value: u8) -> bool {
    byte == 0 && value & 1 != 0
}

/// Whether a READ of this register reports something a triangle still on the
/// queue would change. Only the SST-1 statistics registers do; `stipple` is
/// listed because the rotating stipple writes back through it.
///
/// Everything else is answered from state the rasteriser never writes, so
/// those reads cost nothing. `SST_STATUS` is the one worth arguing, because
/// it is deliberately NOT here and it therefore reports the FIFO idle while
/// the queue still holds triangles.
///
/// That is safe, and it is safe for a reason rather than by luck: the status
/// bits are a promise about what a guest will SEE, and everything that can
/// see a queued triangle's pixels draws the queue first. Those paths are the
/// whole list -- an LFB read or write, a texture-aperture write, the
/// statistics registers above, `scanout_argb` and `scanout_state`, and every
/// register write the snapshot does not cover, which is where `fastfillCMD`,
/// `swapbufferCMD` and the framebuffer layout live. A guest that polls status
/// until idle and then looks will find the work done by the act of looking.
///
/// Reporting busy instead would be worse in both directions. The drain is
/// synchronous, so nothing clears a busy bit while the guest spins on it: a
/// guest waiting for idle before it submits more work would wait forever.
/// And draining on a status poll would put the drain back on the per-triangle
/// path that this queue exists to get off, since polling status between
/// triangles is exactly what a Glide driver does.
fn register_read_needs_raster(register: usize) -> bool {
    matches!(
        register,
        SST_FBI_PIXELS_IN
            | SST_FBI_CHROMA_FAIL
            | SST_FBI_ZFUNC_FAIL
            | SST_FBI_AFUNC_FAIL
            | SST_FBI_PIXELS_OUT
            | SST_STIPPLE
    )
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

/// Diagnostic only: the raw 12.4 fixed-point vertex registers, as signed.
fn raw_vertex_sample(vertices: [(u32, u32); 3]) -> [(i16, i16); 3] {
    vertices.map(|(x, y)| (x as i16, y as i16))
}
