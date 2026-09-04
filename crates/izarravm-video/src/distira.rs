// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Distira, VEGA's Glide-capable 3D unit. This first slice models the Voodoo
//! Graphics style scanout path: a 16-bit RGB565 front/back frame store, buffer
//! swaps, ordered dither, triangle setup, texture sampling, and host-color decode.

use std::collections::VecDeque;
use std::sync::Arc;

mod ncc;
mod raster_kernel;
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
#[cfg(test)]
use raster_queue::RASTER_QUEUE_CAPACITY;
use raster_queue::{
    QueuedCommand, QueuedTextureWrite, QueuedTriangle, RasterQueue, ViewMemory, render_band,
};
use raster_view::{RasterParams, RasterView};
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

/// Framebuffer rows a lane needs before it is worth forking out on its
/// own. `lanes_for_rows` divides a batch's row span into shares this
/// size, so raising the lane cap (`IZARRAVM_DISTIRA_LANES`) never forces
/// a batch that only has a handful of rows to share out across every
/// lane the cap allows -- it gets as many lanes as its own row span is
/// worth, same as before the cap could go past 4.
const MIN_ROWS_PER_LANE: usize = 4;

/// The graduated lane split: a batch's row span, cut into
/// `MIN_ROWS_PER_LANE`-row shares, capped at `cap`. A ten-row batch gets
/// two lanes whether `cap` is 4, 8, or 16 -- it is the row span that
/// decides, not the cap. This is the review's small-batch mitigation
/// (`dev_docs/2026-09-05-distira-texture-queue-review.md` finding 5)
/// generalised past a binary all-or-nothing choice: today, with texture
/// writes ordered instead of draining, batches are usually large (395
/// triangles/drain measured on `tombraid3d-586`'s Lara's Home walk,
/// up from 6.6 before #840), so this mostly returns `cap`; it stays
/// correct on the rare small batch too.
fn lanes_for_rows(rows: usize, cap: usize) -> usize {
    (rows / MIN_ROWS_PER_LANE).clamp(1, cap.max(1))
}

/// `IZARRAVM_DISTIRA_ASYNC`, read once and cached for the process --
/// B2 of `dev_docs/2026-09-05-distira-async-slice1-review.md`: slice 1
/// landed default-on with no in-binary OFF arm, so the design's own ladder
/// recipe (`dev_docs/2026-09-05-distira-async-overlap-design.md` section 8:
/// "`IZARRAVM_DISTIRA_ASYNC` off/on ... interleaved A/B/B/A") could not be
/// run without comparing two separate builds -- exactly the cross-build
/// variance the campaign's own measurement discipline rejects.
///
/// Same env-null convention as `IZARRAVM_JCC_SHADOW`/`IZARRAVM_PIT_BULK_ADVANCE`
/// (unset and `""` both mean the default): unset, unparsable, or any value
/// other than exactly `"0"` keeps slice 1's behaviour (the guest's
/// `swapbufferCMD` write flushes the queue to the raster pool and returns
/// without joining, see `write_mmio_u8`'s `SST_SWAPBUFFER_CMD` arm); `"0"`
/// makes that write fall back to `drain_raster_queue` like every other join
/// point, i.e. fully synchronous -- flush and its own join are always
/// adjacent, so `overlap_ns` reads ~0 for the whole run (see
/// `a_flush_immediately_joined_reports_approximately_zero_overlap`) and
/// `IZARRAVM_DISTIRA_ASYNC=0` is a true in-binary OFF arm for the ladder.
fn async_raster_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("IZARRAVM_DISTIRA_ASYNC")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}

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
#[derive(Debug, Clone, PartialEq)]
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
    /// parallel lanes (see `batch_lanes`).
    pub queue_drains_parallel: u64,
    /// Slice 0 of the texture-queue lever
    /// (`dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 6):
    /// which caller forced each of the `queue_drains` above. Named at the
    /// call site (`DrainCause`, passed to `Distira::drain_raster_queue`), not
    /// inferred after the fact, so it never drifts from the real trigger.
    ///
    /// **Widened in slice 0 of the async-overlap review**
    /// (`dev_docs/2026-09-05-distira-async-overlap-review.md` section 2):
    /// this now counts every CALL to `drain_raster_queue`, including one that
    /// finds the queue already empty and returns without drawing anything.
    /// Before this it undercounted by exactly the empty-queue calls, which
    /// hid the fastfill-after-swap question the review asks: a swap that
    /// flushes and a fastfill that immediately follows it, finding nothing
    /// left to draw, used to be invisible here. `queue_drains` above is
    /// unchanged -- it still counts only the calls that found a non-empty
    /// queue and actually rasterised.
    pub queue_drain_causes: DistiraDrainCauses,

    /// Slice 1 of the async-overlap review
    /// (`dev_docs/2026-09-05-distira-async-overlap-design.md` section 8):
    /// how many of `queue_drains` above were handed to the raster pool
    /// through `Distira::flush_raster_queue` instead of drawn on the
    /// calling thread. Under this slice that is every one of them --
    /// `flush_raster_queue` is the only path that ever takes a non-empty
    /// queue -- so this tracks `queue_drains` exactly; it exists as its
    /// own field because a later slice (depth > 1, or a synchronous
    /// fallback) could make the two diverge, and the whole point of a
    /// named counter is that it does not have to be re-derived from
    /// `queue_drains` when that happens.
    pub async_batches: u64,
    /// Per-cause breakdown of how many `Distira::join_raster` calls
    /// actually waited on an in-flight batch (a join that found nothing in
    /// flight is free and is not counted -- see `Distira::join_raster`'s
    /// doc comment). Keyed on the cause of the CALL that performed the
    /// join, not the cause that flushed the batch it waited for: a
    /// `fastfillCMD` write (`RegisterWriteUncovered`) that joins the
    /// previous frame's swap-flushed batch is what answers finding 2 of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` -- "does the
    /// call that follows a swap land microseconds later, or does it find
    /// the batch already gone?" -- and that answer lives under
    /// `register_write_uncovered` here, not `swap_or_scanout`.
    pub joins_by_cause: DistiraDrainCauses,
    /// **B1 of `dev_docs/2026-09-05-distira-async-slice1-review.md`.** The
    /// FIRST cut of this field (slice 1 as reviewed) stored
    /// `flushed_at.elapsed()` measured AFTER `Receiver::recv` returned --
    /// i.e. the whole flush-to-join-COMPLETE window, which on every join
    /// point but the swap is the batch's own raster time, not overlap (flush
    /// and join are adjacent there: see `Distira::drain_raster_queue`). A
    /// run with zero overlap and a run with perfect overlap both reported a
    /// large, indistinguishable number.
    ///
    /// Fixed: `Distira::join_raster` now takes two independent timings --
    /// `window_ns` (flush to join-complete, same as before) and `blocked_ns`
    /// (an `Instant` taken immediately before `recv`, so just the time THIS
    /// call spent actually waiting). `overlap_ns` is `window_ns - blocked_ns`,
    /// summed across every join: for a flush immediately followed by its own
    /// join (every join point but the swap, and the WHOLE run under
    /// `IZARRAVM_DISTIRA_ASYNC=0`) the two are nearly equal and this reads
    /// ~0, which is what
    /// `a_flush_immediately_joined_reports_approximately_zero_overlap` pins.
    /// Only time the guest spent doing something ELSE while the batch ran on
    /// the pool -- the actual lever -- accumulates here.
    pub overlap_ns: u64,
    /// The `blocked_ns` half of the same split: total wall-clock nanoseconds
    /// every join spent genuinely inside `Receiver::recv`, whether or not it
    /// overlapped anything. `overlap_ns + blocked_ns` recovers the OLD
    /// (wrong) single-number `overlap_ns` this field's sibling replaces.
    pub blocked_ns: u64,
    /// Per-cause breakdown of `window_ns` (flush to join-complete) -- see
    /// `overlap_ns`'s doc comment for what that measures and why B1 asked
    /// for it broken out per [`DrainCause`], not just as one accumulator:
    /// "one accumulator cannot distinguish 'the guest gave us a frame' from
    /// 'the fastfill joined immediately'"
    /// (`dev_docs/2026-09-05-distira-async-overlap-review.md` section 7).
    /// Keyed the same way as `joins_by_cause`: the cause of the CALL that
    /// performed the join, not the cause that flushed the batch it waited
    /// for.
    pub window_ns_by_cause: DistiraDrainNanos,
    /// Per-cause breakdown of `blocked_ns`. Together with
    /// `window_ns_by_cause`, `window_ns_by_cause[c] - blocked_ns_by_cause[c]`
    /// is the overlap THAT CAUSE'S joins achieved -- the number that answers
    /// finding 2 of the prior review directly: a `register_write_uncovered`
    /// entry (the fastfill) with a window near its blocked time joined
    /// almost immediately; a large gap between them means real guest work
    /// ran first.
    pub blocked_ns_by_cause: DistiraDrainNanos,
    /// Per-cause breakdown of `overlap_ns` itself
    /// (`window_ns_by_cause[c] - blocked_ns_by_cause[c]`, precomputed at the
    /// join so a reader does not have to subtract two histograms by hand).
    pub overlap_ns_by_cause: DistiraDrainNanos,
}

/// Per-reason breakdown of [`DistiraTriangleCensus::queue_drains`]. Answers
/// "a frame with hundreds of triangles takes dozens of drains -- which
/// caller is doing that?" without a sampling profiler. See
/// `dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 6.
///
/// Slice 0 (measured on `tombraid3d-586`'s Lara's Home walk, before the
/// fix): `texture_write` dominated, at roughly one drain per accepted
/// texture aperture write -- 364,713 of 370,680 drains, 98.4%. Slice 1 made
/// `Distira::write_texture_u32` stop draining outright (it queues a
/// `QueuedCommand::TextureWrite` instead), which is why that variant no
/// longer exists on [`DrainCause`] and this field is now a regression
/// sentinel: it stays zero by construction, and the day it is not, the
/// direct-drain path came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DistiraDrainCauses {
    /// ALWAYS ZERO after the texture-queue fix (slice 1). See this struct's
    /// doc comment.
    pub texture_write: u64,
    /// `read_lfb_u8/u16/u32`.
    pub lfb_read: u64,
    /// `write_lfb_u8/u16/u32`.
    pub lfb_write: u64,
    /// A statistics-register read (`register_read_needs_raster`).
    pub register_read_stats: u64,
    /// A register write `raster_snapshot_covers_register` does not cover --
    /// this is where `swapbufferCMD`, `fastfillCMD` and the framebuffer
    /// layout registers live (see that function's doc comment).
    pub register_write_uncovered: u64,
    /// `nopCMD` with the reset-statistics bit set.
    pub nop_cmd_reset_stats: u64,
    /// `Distira::swap_buffers` / `scanout_argb` / `scanout_state`: the
    /// present and diagnostic-snapshot paths.
    pub swap_or_scanout: u64,
    /// The queue was already at `RASTER_QUEUE_CAPACITY` and had to be
    /// drawn before the new triangle could be pushed.
    pub queue_full: u64,
    /// A triangle that cannot defer (rotating stipple) or a queue disabled
    /// by `set_raster_queue_enabled(false)`: the immediate-draw path.
    pub immediate_triangle: u64,
    /// Every other caller: the rare setup/config setters
    /// (`set_dither_enabled`, `set_force_point_sampling`,
    /// `set_raster_lanes`, `set_frame_size`, `clear_back_rgb`).
    pub config: u64,
}

/// Which caller is asking `Distira::drain_raster_queue` to draw the queue.
/// Purely a census tag -- it changes no behaviour -- so a call site that adds
/// a new drain point but forgets to name it correctly under-counts silently.
/// See [`DistiraDrainCauses`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DrainCause {
    LfbRead,
    LfbWrite,
    RegisterReadStats,
    RegisterWriteUncovered,
    NopCmdResetStats,
    SwapOrScanout,
    QueueFull,
    ImmediateTriangle,
    Config,
}

impl DistiraDrainCauses {
    fn record(&mut self, cause: DrainCause) {
        match cause {
            DrainCause::LfbRead => self.lfb_read += 1,
            DrainCause::LfbWrite => self.lfb_write += 1,
            DrainCause::RegisterReadStats => self.register_read_stats += 1,
            DrainCause::RegisterWriteUncovered => self.register_write_uncovered += 1,
            DrainCause::NopCmdResetStats => self.nop_cmd_reset_stats += 1,
            DrainCause::SwapOrScanout => self.swap_or_scanout += 1,
            DrainCause::QueueFull => self.queue_full += 1,
            DrainCause::ImmediateTriangle => self.immediate_triangle += 1,
            DrainCause::Config => self.config += 1,
        }
    }
}

/// Same field-per-[`DrainCause`] shape as [`DistiraDrainCauses`], but a
/// nanosecond SUM per cause instead of a call count -- `window_ns_by_cause`,
/// `blocked_ns_by_cause` and `overlap_ns_by_cause` on
/// [`DistiraTriangleCensus`] all use this. A separate type rather than
/// reusing `DistiraDrainCauses` for both: both are `u64` fields, but a count
/// and a nanosecond sum answer different questions, and giving them the same
/// type would let a future edit add them together without the compiler
/// noticing anything wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DistiraDrainNanos {
    pub texture_write: u64,
    pub lfb_read: u64,
    pub lfb_write: u64,
    pub register_read_stats: u64,
    pub register_write_uncovered: u64,
    pub nop_cmd_reset_stats: u64,
    pub swap_or_scanout: u64,
    pub queue_full: u64,
    pub immediate_triangle: u64,
    pub config: u64,
}

impl DistiraDrainNanos {
    fn add(&mut self, cause: DrainCause, nanos: u64) {
        let field = match cause {
            DrainCause::LfbRead => &mut self.lfb_read,
            DrainCause::LfbWrite => &mut self.lfb_write,
            DrainCause::RegisterReadStats => &mut self.register_read_stats,
            DrainCause::RegisterWriteUncovered => &mut self.register_write_uncovered,
            DrainCause::NopCmdResetStats => &mut self.nop_cmd_reset_stats,
            DrainCause::SwapOrScanout => &mut self.swap_or_scanout,
            DrainCause::QueueFull => &mut self.queue_full,
            DrainCause::ImmediateTriangle => &mut self.immediate_triangle,
            DrainCause::Config => &mut self.config,
        };
        *field = field.saturating_add(nanos);
    }
}

/// The swap -> next-drain-call wall-clock window, summarised. Answers
/// the fastfill-after-swap question from
/// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 2: does the
/// call that follows a `swapbufferCMD` land microseconds later (the window
/// an async lever would have to overlap the guest's frame with) or is it
/// effectively simultaneous (the fastfill finding an already-drained queue,
/// eating the whole window before any lever can act)? Wall-clock, not guest
/// cycles -- on the synchronous model the two are the same elapsed interval
/// on the one thread that runs both, so this is measured directly rather
/// than derived from a cycle counter Distira does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SwapToNextDrainStats {
    /// How many swap -> next-call windows were closed (i.e. a swap was
    /// followed by at least one more `drain_raster_queue` call before the
    /// run ended).
    pub count: usize,
    pub min_ns: u64,
    pub median_ns: u64,
    pub max_ns: u64,
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
    /// This instance's configured raster lane cap (`self.raster_lanes`;
    /// see [`Distira::set_raster_lanes`] and the `IZARRAVM_DISTIRA_LANES`
    /// default). Not necessarily how many lanes any given triangle
    /// actually forked across -- `lanes_for_rows` can return fewer for a
    /// small batch -- this is the ceiling, not a per-draw count.
    pub raster_lane_count: usize,
    /// The dedicated raster pool's REALIZED OS thread count
    /// (`raster_pool::pool_size`). Exposed so `--mode-census` can answer
    /// "did the knob actually grow the pool?" directly instead of only by
    /// inference from wall-clock deltas
    /// (`dev_docs/2026-09-05-lane-cap-ladder.md`'s "Pool sizing" section
    /// found no field for this).
    pub raster_pool_size: usize,
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
    ///
    /// Only `RasterView::raster_row` calls this now; `raster_row_specialized`
    /// extracts the Z/W terms itself so the WBUF flag can skip the enum
    /// match. Kept, and only `allow(dead_code)` outside tests, because
    /// `raster_row` is the differential oracle in `raster_kernel_test.rs`.
    #[cfg_attr(not(test), allow(dead_code))]
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
    /// `Arc`, not a plain `FrameStore`: slice 0b of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 3.
    /// `FrameStore` is ALREADY `Vec<AtomicU8>` (`raster_pool.rs`) -- every
    /// method takes `&self` and stores through relaxed atomics -- so the
    /// interior mutability this needs is already there, and `Arc` adds one
    /// indirection to an access that is already a load, never a `Mutex` or
    /// `RefCell`. Nothing here is unsafe: `#![forbid(unsafe_code)]` holds.
    fb: Arc<FrameStore>,
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
    /// Host override: sample the nearest texel regardless of the guest's own
    /// `texture_mode` bilinear bit. Off leaves every triangle exactly as the
    /// guest programmed it -- this is the "Glide texture filtering: Disabled"
    /// GUI setting, and it is the only thing that ever sets this true.
    force_point_sampling: bool,
    /// How many threads rasterise a batch of triangles, caller included.
    /// Chosen from the host core count at construction (or from
    /// `IZARRAVM_DISTIRA_LANES` when it is set -- read once, at process
    /// start, never per drain; see `raster_pool::host_lanes`); see also
    /// [`Distira::raster_lanes_for_cores`]. `set_raster_lanes` overrides it
    /// after construction, which is what the A/B lane-cap tests use.
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
    /// The texture stores and NCC/CLUT tables a raster batch touches, moved
    /// out for the duration of a batch rather than shared. Slice 0b of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 3: NO
    /// atomics here, unlike `fb` -- `self.texture` has exactly two `&mut
    /// self` writers (the drain's serial application of a queued texture
    /// write, and the queue-off store in `write_texture_u32`) and no `&self`
    /// reader outside the lane-side view (#840 established that a
    /// texture-aperture READ never reaches the device), so ownership can
    /// just move to the batch and back instead of sharing through atomics
    /// the way `fb` does.
    ///
    /// `None` while a batch has the box: from `flush_raster_queue`'s
    /// `take()` until `join_raster` gets it back over the completion
    /// channel. Slice 1 of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md`: this used to
    /// be `None` only for the duration of `drain_raster_queue`'s own call,
    /// on the calling thread; now the "duration" can span guest
    /// instructions between the flush and whichever call joins it, which
    /// is the whole point of the lever. Every access outside a batch goes
    /// through `raster_owned_mut`, which joins first, so nothing ever
    /// observes `None` here except the raster worker itself, which owns a
    /// local `Box<RasterOwned>` moved out of this field, not a borrow of
    /// it.
    raster_owned: Option<Box<RasterOwned>>,
    /// Slice 1 of the async-overlap review: the one batch (depth one) the
    /// raster pool may be working on right now. `None` whenever nothing is
    /// outstanding -- which is most of the time a caller looks, since
    /// `Distira::join_raster` takes it the moment anything needs to
    /// observe batch-produced state. See `Distira::flush_raster_queue` and
    /// `Distira::join_raster`.
    in_flight: Option<InFlight>,
    /// The instant of the most recent guest `swapbufferCMD` write
    /// (`SST_SWAPBUFFER_CMD`, byte 3, see `write_mmio_u8`) that has not yet
    /// been closed off by a following `drain_raster_queue` call. `None` once
    /// that following call lands -- see `swap_to_next_drain_ns` and slice 0
    /// of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 2: this
    /// is the fastfill-after-swap question, measured on the SYNCHRONOUS
    /// model as a lower bound (the async lever cannot widen a window this
    /// narrow, only fail to shrink it further).
    last_swap_instant: Option<std::time::Instant>,
    /// Every measured swap -> next-call window, in nanoseconds. Read out
    /// through `swap_to_next_drain_stats`, never compared field-by-field --
    /// it is wall-clock, so it is in the census "may move" whitelist, not
    /// graded for identity.
    swap_to_next_drain_ns: Vec<u64>,
}

/// The two per-batch memories a raster batch reads and (for `texture`)
/// writes: moved out of `Distira` for a batch's duration instead of shared.
/// See `Distira::raster_owned`'s doc comment and slice 0b of
/// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 3.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RasterOwned {
    texture: [Vec<u8>; 2],
    ncc: NccState,
}

/// One run of triangles a batch rasterised (a contiguous slice of the
/// batch's commands with no `TextureWrite` in it) and the per-lane counters
/// it produced. Sent back from the raster pool so `Distira::join_raster`
/// can fold them through `merge_pixel_stats` on the JOINING thread --
/// `dev_docs/2026-09-05-distira-async-overlap-design.md` section 3: "Keep
/// `merge_pixel_stats` exactly where it is -- on the joining thread, with
/// the batch's `jobs` slice in hand." `range` indexes into
/// `RasterBatchResult::commands`, which travels back in the same message.
struct RasterRunResult {
    range: std::ops::Range<usize>,
    lane_stats: Vec<PixelStats>,
    lanes: usize,
}

/// What a raster batch hands back through the join channel: the commands
/// (so the joining thread can recycle the allocation via
/// `RasterQueue::recycle` and re-derive each run's `jobs` slice for
/// `merge_pixel_stats`), the `RasterOwned` box (texture stores + NCC,
/// handed back exactly as the old synchronous `drain_raster_queue` did
/// before returning), and one `RasterRunResult` per triangle run.
struct RasterBatchResult {
    commands: Vec<QueuedCommand>,
    owned: Box<RasterOwned>,
    runs: Vec<RasterRunResult>,
    any_parallel: bool,
}

/// The one batch `Distira` ever lets be in flight at a time (depth one,
/// deliberately -- see
/// `dev_docs/2026-09-05-distira-async-overlap-design.md` section 2). Built
/// by `Distira::flush_raster_queue`, consumed by `Distira::join_raster`.
struct InFlight {
    /// `std::thread::Result` (`Result<RasterBatchResult, Box<dyn Any + Send>>`),
    /// not a bare `RasterBatchResult`: S1 of
    /// `dev_docs/2026-09-05-distira-async-slice1-review.md`. `raster_pool()`
    /// installs no `panic_handler`, and rayon's default aborts the whole
    /// process on a worker panic with no unwind and no destructors -- before
    /// slice 1 a raster bug surfaced as an ordinary panic on the emulation
    /// thread (`raster_pool().install(...)` propagates one out normally);
    /// moving the raster off that thread silently turned every raster panic
    /// into a process abort. `run_raster_batch` now runs inside
    /// `std::panic::catch_unwind` and the payload rides the channel; `Self::join_raster`
    /// calls `std::panic::resume_unwind` on an `Err`, which restores the old
    /// behaviour: the panic still surfaces on the emulation thread, at the
    /// join, instead of killing the process.
    rx: std::sync::mpsc::Receiver<std::thread::Result<RasterBatchResult>>,
    /// When this batch was handed to the pool, for `overlap_ns`/`blocked_ns`.
    flushed_at: std::time::Instant,
}

/// What one [`Distira::join_raster`] call learned about the batch it just
/// folded in. See [`DistiraTriangleCensus::overlap_ns`]'s doc comment for
/// what `window_ns` and `blocked_ns` measure and why B1 of
/// `dev_docs/2026-09-05-distira-async-slice1-review.md` asked for both.
struct JoinResult {
    written: u64,
    window_ns: u64,
    blocked_ns: u64,
}

/// Hand-rolled, in the same spirit as `FrameStore`'s and `RasterQueue`'s
/// impls just above: `Distira` derives `Debug`/`Clone`/`PartialEq`/`Eq`, but
/// `mpsc::Receiver` has none of those, and a receiver cannot be duplicated
/// meaningfully anyway. `in_flight` is ephemeral scheduling state, not
/// device state -- nothing a queued batch will produce is observable until
/// `Distira::join_raster` folds it in, and every accessor joins first -- so
/// two `Distira` values compare and print by PRESENCE only, exactly the way
/// `RasterQueue::eq` compares by pending count rather than content. Cloning
/// is different: there is no ephemeral placeholder a clone could hold that
/// would let it eventually complete a join of its own, so a clone taken
/// while a batch is genuinely in flight is a bug in the caller (it should
/// have joined first, the same invariant every other accessor keeps) and
/// this panics rather than fabricate a receiver that can never resolve.
impl std::fmt::Debug for InFlight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "InFlight(..)")
    }
}

impl Clone for InFlight {
    fn clone(&self) -> Self {
        panic!(
            "a Distira must not be cloned while a raster batch is in flight; \
             join_raster first"
        )
    }
}

impl PartialEq for InFlight {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for InFlight {}

/// How many lanes a batch (or one run within it) is worth. Below the pixel
/// threshold the wake-up cost of the pool beats the win outright and the
/// calling thread draws the batch (`lanes == 1`); above it, [`lanes_for_rows`]
/// grants only as many lanes as the batch's row span can fill, up to
/// `raster_lanes`.
///
/// The two measures answer different questions. Pixels are the work, so
/// they sum over the batch. Rows are the parallelism, and lanes divide
/// DISTINCT rows, so they take the union span rather than the sum: a stack
/// of small triangles sitting on top of each other has one triangle's
/// worth of rows to share out however many triangles it holds.
///
/// A free function, not `Distira::batch_lanes`, since slice 1 of
/// `dev_docs/2026-09-05-distira-async-overlap-review.md` moves the caller
/// (`raster_run`) onto the raster pool, where there is no `&self` -- only
/// `raster_lanes`, captured by value in `Distira::flush_raster_queue`.
fn batch_lanes(jobs: &[QueuedCommand], raster_lanes: usize) -> usize {
    if raster_lanes < 2 {
        return 1;
    }
    let mut lowest = u32::MAX;
    let mut highest = 0;
    let mut pixels = 0usize;
    for job in jobs {
        // See `render_band`: every entry in a run is a `Triangle` by
        // construction.
        let QueuedCommand::Triangle(job) = job else {
            continue;
        };
        let job_rows = job.context.max_y.saturating_sub(job.context.min_y) as usize;
        let columns = job.context.max_x.saturating_sub(job.context.min_x) as usize;
        if job_rows == 0 {
            continue;
        }
        lowest = lowest.min(job.context.min_y);
        highest = highest.max(job.context.max_y);
        pixels += job_rows.saturating_mul(columns);
    }
    if pixels < PARALLEL_PIXEL_THRESHOLD {
        return 1;
    }
    let rows = highest.saturating_sub(lowest) as usize;
    lanes_for_rows(rows, raster_lanes)
}

/// Rasterise one run of triangles with no queued texture write between
/// them: one fork and one join for the run, entirely on the raster pool.
/// Lane `i` owns the FRAMEBUFFER rows where `draw_y(y) % lanes == i` and
/// walks the run in submission order, so overlapping triangles land in the
/// order the guest sent them. `distira/raster_queue.rs` has why the
/// framebuffer row is the one that has to partition and not the triangle's
/// own row. Returns each lane's counters and the lane count, for the
/// JOINING thread to fold through `Distira::merge_pixel_stats` -- see
/// `Distira::join_raster`'s doc comment.
///
/// A free function (the old `Distira::render_triangle_segment`, minus
/// `&mut self`): it runs on the worker thread the batch was moved to,
/// which has no borrow of `Distira` at all, only `fb` and `owned` by
/// reference and `raster_lanes` by value.
fn raster_run(
    jobs: &[QueuedCommand],
    owned: &RasterOwned,
    fb: &FrameStore,
    raster_lanes: usize,
) -> (Vec<PixelStats>, usize) {
    let lanes = batch_lanes(jobs, raster_lanes);
    let mut lane_stats: Vec<PixelStats> = (0..lanes).map(|_| PixelStats::new(0)).collect();
    let memory = ViewMemory {
        fb,
        texture: &owned.texture,
        ncc: &owned.ncc,
    };
    if lanes == 1 {
        render_band(jobs, memory, 0, 1, &mut lane_stats[0]);
    } else {
        let lane_count = lanes as u32;
        // A lane accumulates into a stack-local `PixelStats` and stores
        // it once at the end: the `lane_stats` elements share cache
        // lines, and a per-pixel counter write there makes the lanes
        // false-share their way back to serial speed.
        let (worker_stats, install_stats) = lane_stats.split_at_mut(lanes - 1);
        // `install` moves the whole fork onto the pool: the scope then
        // spawns into a worker-local queue, which the other workers
        // steal far faster than an external injection wakes them. The
        // installed worker rasterises the last lane. This closure already
        // runs ON a pool thread (the batch driver
        // `Distira::flush_raster_queue` spawned onto), so `install` here
        // runs the fork inline on the current worker rather than adding a
        // thread -- the pool still does exactly `raster_lanes` threads of
        // work, not `raster_lanes + 1`
        // (`dev_docs/2026-09-05-distira-async-overlap-design.md` section 7).
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
    (lane_stats, lanes)
}

/// The whole batch, run on the raster pool: split into runs at every
/// `TextureWrite`, each run forked/joined through [`raster_run`], and
/// every write applied serially between the two runs it separates -- byte
/// for byte the body of the old (slice 0b and earlier) synchronous
/// `Distira::drain_raster_queue`
/// (`dev_docs/2026-09-05-distira-async-overlap-design.md` section 2, item
/// 3: "The worker task is byte-for-byte the body of today's
/// `drain_raster_queue`"), just no longer borrowing `self`.
///
/// `merge_pixel_stats` is deliberately NOT called here -- see
/// `Distira::join_raster`'s doc comment: it runs on the JOINING thread,
/// with `self` and the batch's `jobs` slice both in hand, never on the
/// worker.
fn run_raster_batch(
    commands: Vec<QueuedCommand>,
    mut owned: Box<RasterOwned>,
    fb: &FrameStore,
    raster_lanes: usize,
) -> RasterBatchResult {
    let mut runs = Vec::new();
    let mut any_parallel = false;
    // A run of triangles between two `TextureWrite`s (or the batch's
    // edges) is a CONTIGUOUS subslice of `commands` -- `run_start..index`
    // tracks it by index instead of copying triangles into a side `Vec`,
    // which used to memcpy every queued triangle (~600 B each) on every
    // drain, even the common case with no write in the batch at all.
    let mut run_start = 0usize;
    for (index, command) in commands.iter().enumerate() {
        let QueuedCommand::TextureWrite(write) = *command else {
            continue;
        };
        if index > run_start {
            let (lane_stats, lanes) =
                raster_run(&commands[run_start..index], &owned, fb, raster_lanes);
            any_parallel |= lanes > 1;
            runs.push(RasterRunResult {
                range: run_start..index,
                lane_stats,
                lanes,
            });
        }
        let mask = DISTIRA_TEX_SIZE - 1;
        for (byte_index, byte) in write.bytes.into_iter().enumerate() {
            owned.texture[write.tmu][(write.offset + byte_index) & mask] = byte;
        }
        run_start = index + 1;
    }
    if run_start < commands.len() {
        let (lane_stats, lanes) = raster_run(&commands[run_start..], &owned, fb, raster_lanes);
        any_parallel |= lanes > 1;
        runs.push(RasterRunResult {
            range: run_start..commands.len(),
            lane_stats,
            lanes,
        });
    }
    RasterBatchResult {
        commands,
        owned,
        runs,
        any_parallel,
    }
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
            fb: Arc::new(FrameStore::new(DISTIRA_FB_SIZE)),
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
            force_point_sampling: false,
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
            raster_owned: Some(Box::new(RasterOwned {
                texture: std::array::from_fn(|_| vec![0; DISTIRA_TEX_SIZE]),
                ncc: NccState::default(),
            })),
            in_flight: None,
            last_swap_instant: None,
            swap_to_next_drain_ns: Vec::new(),
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
    ///
    /// `&mut self` and an owned return, not `&self` and `&DistiraCensus`:
    /// slice 0b of `dev_docs/2026-09-05-distira-async-overlap-review.md`
    /// section 1 closes the `&self`-accessor hole by routing this through
    /// `join_raster` first. An owned `DistiraCensus` (it derives `Clone`)
    /// keeps the borrow from `self` from outliving the call, which matters
    /// once a caller (see `main.rs`'s `--mode-census` dump) reads several of
    /// these accessors back to back on the same `&mut Machine` -- a borrowed
    /// return would force them to interleave in a fixed order again, which
    /// is exactly the "ordering is luck" hazard this slice removes.
    pub fn census(&mut self) -> DistiraCensus {
        self.join_raster();
        self.census.clone()
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
    ///
    /// `&mut self`: see [`Self::census`]'s doc comment -- same hole, same
    /// fix.
    pub fn register_write_histogram(&mut self) -> Vec<(usize, u64)> {
        self.join_raster();
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
    ///
    /// `&mut self`: see [`Self::census`]'s doc comment -- same hole, same
    /// fix.
    pub fn register_read_histogram(&mut self) -> Vec<(usize, u64)> {
        self.join_raster();
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

    /// See [`SwapToNextDrainStats`]. Sorts a clone of the sample vector, so
    /// it is O(n log n) per call -- fine for an end-of-run census read, not
    /// meant for a hot loop.
    pub fn swap_to_next_drain_stats(&self) -> SwapToNextDrainStats {
        if self.swap_to_next_drain_ns.is_empty() {
            return SwapToNextDrainStats::default();
        }
        let mut samples = self.swap_to_next_drain_ns.clone();
        samples.sort_unstable();
        SwapToNextDrainStats {
            count: samples.len(),
            min_ns: samples[0],
            median_ns: samples[samples.len() / 2],
            max_ns: samples[samples.len() - 1],
        }
    }

    pub fn scanout_state(&mut self) -> DistiraScanoutState {
        self.drain_raster_queue(DrainCause::SwapOrScanout);
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
            raster_lane_count: self.raster_lanes,
            raster_pool_size: raster_pool::pool_size(),
        }
    }

    pub fn set_dither_enabled(&mut self, enabled: bool) {
        self.drain_raster_queue(DrainCause::Config);
        self.dither_enabled = enabled;
    }

    /// Host override for the Glide texture filtering setting: forces nearest
    /// (point) sampling for every TMU regardless of the guest's own
    /// `texture_mode` bilinear bit. Queued triangles were snapshotted with the
    /// old value in their `RasterParams`, so this drains them first, same as
    /// [`Self::set_dither_enabled`].
    pub fn set_force_point_sampling(&mut self, enabled: bool) {
        self.drain_raster_queue(DrainCause::Config);
        self.force_point_sampling = enabled;
    }

    /// See [`Self::set_force_point_sampling`].
    pub fn force_point_sampling(&self) -> bool {
        self.force_point_sampling
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
        self.drain_raster_queue(DrainCause::Config);
        self.raster_queue_enabled = enabled;
    }

    /// How many triangles are submitted but not yet drawn.
    pub fn raster_queue_depth(&self) -> usize {
        self.raster_queue.len()
    }

    /// Override the raster thread count (clamped to
    /// `1..=raster_pool::MAX_LANES`). One lane disables the worker pool
    /// entirely.
    pub fn set_raster_lanes(&mut self, lanes: usize) {
        self.drain_raster_queue(DrainCause::Config);
        self.raster_lanes = lanes.clamp(1, raster_pool::MAX_LANES);
    }

    pub fn set_frame_size(&mut self, width: u32, height: u32) {
        self.drain_raster_queue(DrainCause::Config);
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
        self.drain_raster_queue(DrainCause::Config);
        let pixel = pack_rgb565(r, g, b);
        let start = self.display.back_base as usize;
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = start.saturating_add(len).min(self.fb.len());
        for offset in (start..end.saturating_sub(1)).step_by(2) {
            self.fb.write_u16_le(offset, pixel);
        }
    }

    pub fn swap_buffers(&mut self) {
        self.drain_raster_queue(DrainCause::SwapOrScanout);
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
            force_point_sampling: self.force_point_sampling,
        }
    }

    /// The pipeline's view of the device THIS INSTANT. The LFB write path and
    /// the texture-aperture decode use it; a triangle uses the params it was
    /// submitted with instead.
    ///
    /// `&mut self`: joins first (see [`Self::join_raster`]), then borrows
    /// `self.fb`/`self.raster_owned` directly rather than through
    /// [`Self::raster_owned_mut`] -- that method's `&mut self` receiver would
    /// keep the whole of `self` borrowed for as long as the returned
    /// [`RasterView`] lives, which is longer than `self.raster_params()`
    /// (also `&self`) can tolerate. Reading the fields straight keeps the two
    /// borrows disjoint.
    fn raster_view(&mut self) -> RasterView<'_> {
        self.join_raster();
        let params = self.raster_params();
        let owned = self.raster_owned.as_ref().expect(
            "raster_owned is only None inside drain_raster_queue's own batch, \
             which never calls back into this accessor",
        );
        ViewMemory {
            fb: self.fb.as_ref(),
            texture: &owned.texture,
            ncc: &owned.ncc,
        }
        .view(params)
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
        let command = QueuedCommand::Triangle(triangle);
        if defer && self.raster_queue_enabled && triangle_defers(&triangle) {
            if !self.raster_queue.push(command) {
                // Full. Draw what is waiting, then this one joins an empty
                // queue, so a triangle is never dropped. The second push
                // cannot refuse: the drain above leaves the queue at zero and
                // the capacity is not zero.
                self.drain_raster_queue(DrainCause::QueueFull);
                // Not inside the assertion: a release build compiles the
                // assertion's expression away, and the push has to happen.
                let queued = self.raster_queue.push(command);
                debug_assert!(queued, "a drained queue accepts");
            }
            return 0;
        }

        // The immediate path. Draining first leaves this triangle alone in
        // the batch, so the pixel count belongs to it and to nothing else,
        // and the push cannot refuse for the same reason.
        self.drain_raster_queue(DrainCause::ImmediateTriangle);
        let queued = self.raster_queue.push(command);
        debug_assert!(queued, "a drained queue accepts");
        self.drain_raster_queue(DrainCause::ImmediateTriangle)
    }

    /// Wait for an in-flight raster batch, if there is one, before any
    /// caller observes state a batch could have produced, and fold its
    /// counters in. Slice 1 of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 2:
    /// this used to be a no-op (slice 0b); now it is the JOIN half of
    /// `Distira::flush_raster_queue`'s flush -- it blocks on the batch's
    /// completion channel, folds the returned `PixelStats` through
    /// `merge_pixel_stats` **on this thread** (`RasterBatchResult::runs`
    /// carries each run's `range`, so `merge_pixel_stats` gets the exact
    /// `jobs` slice it always has -- see
    /// `dev_docs/2026-09-05-distira-async-overlap-design.md` section 3),
    /// and hands `raster_owned` back.
    ///
    /// Returns what the join learned (see [`JoinResult`]), or `None` if
    /// nothing was in flight -- "a join on no in-flight batch is free and
    /// must not be counted" (design section 8). Callers that pass a
    /// [`DrainCause`] (`Self::flush_raster_queue`, `Self::drain_raster_queue`,
    /// via `Self::record_join`) use the `Some`/`None` split to decide
    /// whether to bump `joins_by_cause`; the plain accessor doors
    /// (`Self::raster_owned_mut`, `Self::census` and friends) just discard
    /// it -- they only need the join to have happened, not to account for
    /// it under a cause, because they never flush anything themselves. The
    /// AGGREGATE `overlap_ns`/`blocked_ns` are updated here unconditionally,
    /// regardless of caller, so they stay correct even for an accessor-door
    /// join; only the per-cause breakdowns depend on the caller passing one
    /// through `Self::record_join`.
    fn join_raster(&mut self) -> Option<JoinResult> {
        let in_flight = self.in_flight.take()?;
        // B1 of `dev_docs/2026-09-05-distira-async-slice1-review.md`: an
        // `Instant` taken immediately before `recv`, so `blocked_ns` is ONLY
        // the time this call itself spent waiting -- not the batch's whole
        // flush-to-join-complete window, which `window_ns` below still
        // measures for comparison. The old single `overlap_ns` stored
        // `window_ns` alone, so a join adjacent to its own flush (every join
        // point but the swap) reported the batch's full raster time as if it
        // were overlap.
        let recv_start = std::time::Instant::now();
        let outcome = in_flight.rx.recv().expect(
            "the Distira raster worker dropped its sender without a reply -- \
             it must have panicked, and a worker panic should have arrived \
             as an Err through the channel instead (see run_raster_batch's \
             catch_unwind, S1 of the slice 1 review)",
        );
        let blocked_ns = recv_start.elapsed().as_nanos() as u64;
        let window_ns = in_flight.flushed_at.elapsed().as_nanos() as u64;
        // S1: propagate a worker panic instead of losing it. `run_raster_batch`
        // ran inside `catch_unwind`, so an `Err` here means the worker
        // itself panicked; resuming it on THIS (the emulation) thread is
        // what `raster_pool().install(...)` used to do for free before the
        // batch moved off this thread.
        let result = match outcome {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        self.triangle_census.blocked_ns =
            self.triangle_census.blocked_ns.saturating_add(blocked_ns);
        self.triangle_census.overlap_ns = self
            .triangle_census
            .overlap_ns
            .saturating_add(window_ns.saturating_sub(blocked_ns));
        let mut written = 0u64;
        for run in &result.runs {
            written += self.merge_pixel_stats(
                &run.lane_stats,
                &result.commands[run.range.clone()],
                run.lanes,
            );
        }
        if result.any_parallel {
            self.triangle_census.queue_drains_parallel += 1;
        }
        self.raster_queue.recycle(result.commands);
        self.raster_owned = Some(result.owned);
        Some(JoinResult {
            written,
            window_ns,
            blocked_ns,
        })
    }

    /// Attribute one join's `window_ns`/`blocked_ns`/`overlap_ns` to `cause`
    /// -- the per-[`DrainCause`] half of `Self::join_raster`'s bookkeeping,
    /// factored out because both `Self::flush_raster_queue`'s depth-one
    /// pre-join and `Self::drain_raster_queue`'s own post-flush join need it
    /// (N3 of the slice 1 review: one `drain_raster_queue` call can record
    /// the same cause twice this way, and that is correct -- both are
    /// genuine waits).
    fn record_join(&mut self, cause: DrainCause, outcome: &JoinResult) {
        self.triangle_census.joins_by_cause.record(cause);
        self.triangle_census
            .window_ns_by_cause
            .add(cause, outcome.window_ns);
        self.triangle_census
            .blocked_ns_by_cause
            .add(cause, outcome.blocked_ns);
        self.triangle_census
            .overlap_ns_by_cause
            .add(cause, outcome.window_ns.saturating_sub(outcome.blocked_ns));
    }

    /// The door onto [`RasterOwned`]: every reader of `self.texture`/
    /// `self.ncc` outside a batch goes through this, never the fields
    /// directly. Joins first (see [`Self::join_raster`]), then unwraps --
    /// the `None` case is a batch in flight on the raster pool, and joining
    /// clears it, so the `expect` cannot fire: nothing else ever takes
    /// `raster_owned` and every caller of this method routes through the
    /// join above first.
    fn raster_owned_mut(&mut self) -> &mut RasterOwned {
        self.join_raster();
        self.raster_owned.as_mut().expect(
            "raster_owned is only None while a batch is in flight, and \
             join_raster above always clears that before this unwraps",
        )
    }

    /// Take the queue's commands and the [`RasterOwned`] box and move them
    /// to the raster pool (`raster_pool().spawn`); record one in-flight
    /// batch. This is the CALL half of the old (slice 0b and earlier)
    /// `drain_raster_queue` -- it never blocks on the batch it just
    /// started. [`Self::join_raster`] is the other half.
    ///
    /// **Depth one, deliberately**
    /// (`dev_docs/2026-09-05-distira-async-overlap-design.md` section 2):
    /// at most one batch is ever in flight. If this call has a NEW,
    /// non-empty batch to hand to the pool and one is already in flight,
    /// it joins the old one first -- a flush never has to choose which of
    /// two outstanding batches to wait for, and the join that does the
    /// waiting is attributed to `cause`, i.e. to whichever call forced it.
    /// An empty-queue call -- most calls, since a flush happens on every
    /// register write's pre-write check -- leaves `in_flight` completely
    /// alone: it must, or the swap's "flush and return" would only last
    /// until the very next call to this method, not the whole overlap
    /// window it exists to open.
    ///
    /// Every call site in
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 1
    /// pairs this with an immediate [`Self::join_raster`] (see
    /// [`Self::drain_raster_queue`]) **except** the guest's `swapbufferCMD`
    /// register write (`write_mmio_u8`'s `SST_SWAPBUFFER_CMD` arm), which
    /// calls this alone and returns -- the guest walks on into the next
    /// frame's geometry while the batch rasterises, and whichever call
    /// joins it next (almost always the following register write's own
    /// pre-write flush, which is where `fastfillCMD` lives) pays for it.
    ///
    /// Internally the batch still has to respect a queued texture write's
    /// ordering: a triangle queued before a `TextureWrite` must sample the
    /// OLD texel, one queued after must sample the NEW one, and a lane's
    /// borrow of the batch's texture store can never overlap the write's
    /// `&mut` (see `distira/raster_queue.rs`'s module doc). So the worker
    /// (`run_raster_batch`) splits the batch into runs of triangles at
    /// every `TextureWrite`, each run gets its own fork/join through
    /// [`raster_run`], and the write is applied serially, on the worker
    /// thread, between the two runs it separates -- byte for byte the body
    /// of the old synchronous drain, just moved off this thread.
    fn flush_raster_queue(&mut self, cause: DrainCause) {
        // Slice 0 of the async-overlap review
        // (`dev_docs/2026-09-05-distira-async-overlap-review.md` section 2):
        // record the CALL, not just a non-empty flush. Before this, an
        // empty-queue call returned above without ever reaching
        // `queue_drain_causes.record`, so a fastfill that landed after the
        // queue was already empty (the exact hazard the review names) was
        // invisible to the census. Every call is now counted, and the
        // swap -> next-call wall-clock window is measured here too: a
        // guest `swapbufferCMD` write (`issue_swapbuffer_command`) sets
        // `last_swap_instant`, and the very next `flush_raster_queue` call
        // of ANY cause closes the window. That next call is almost always
        // the following register write's own pre-write flush (see
        // `write_mmio_u8`), which is where `fastfillCMD` lives -- so this is
        // measuring exactly the interval the review's finding 2 asks about,
        // not the rare direct `DrainCause::SwapOrScanout` accessor call
        // (`scanout_state`/`scanout_argb`/`swap_buffers`), which on these two
        // rows fires only a handful of times a whole run (diagnostic reads,
        // not the guest's own per-frame swap).
        self.triangle_census.queue_drain_causes.record(cause);
        if let Some(start) = self.last_swap_instant.take() {
            self.swap_to_next_drain_ns
                .push(start.elapsed().as_nanos() as u64);
        }
        if self.raster_queue.is_empty() {
            // Nothing new to hand to the pool. Critically, this must NOT
            // join whatever is already in flight -- an empty-queue flush
            // (the common case: three of the swap's four byte writes,
            // every register write between one flush and the next) has to
            // be a complete no-op on `in_flight`, or the swap's "flush and
            // return" would only last until the very next call to this
            // method, which is nothing like a whole frame's overlap
            // window.
            return;
        }
        // Depth one (`dev_docs/2026-09-05-distira-async-overlap-design.md`
        // section 2): THIS call has a new batch to hand to the pool, so if
        // one is already in flight, join it first -- there is only ever
        // one in-flight slot, and the join that does the waiting is
        // attributed to `cause`, whichever call forced it.
        if let Some(outcome) = self.join_raster() {
            self.record_join(cause, &outcome);
        }
        let commands = self.raster_queue.take();
        self.triangle_census.queue_drains += 1;
        self.triangle_census.async_batches += 1;
        // The batch's memories move to the worker: `join_raster` above
        // just guaranteed `self.in_flight` is `None`, so this `expect`
        // cannot fire -- nothing else ever takes `raster_owned` while a
        // batch is in flight.
        let owned = self.raster_owned.take().expect(
            "raster_owned is only None while a batch is in flight, and the \
             join above just cleared any in-flight batch",
        );
        let fb = Arc::clone(&self.fb);
        let raster_lanes = self.raster_lanes;
        let (tx, rx) = std::sync::mpsc::channel();
        raster_pool().spawn(move || {
            // S1 of `dev_docs/2026-09-05-distira-async-slice1-review.md`:
            // `raster_pool()` has no `panic_handler`, so an unwound panic
            // that escaped this closure would abort the whole process
            // (rayon's documented default) -- no unwind, no destructors, no
            // error path, a straight-up regression from the old
            // `raster_pool().install(...)` behaviour on the emulation
            // thread, which propagated a panic to its caller normally.
            // `catch_unwind` turns that back into an ordinary `Result` that
            // rides the channel; `Distira::join_raster` resumes it on the
            // emulation thread. `AssertUnwindSafe`: `fb` is `&FrameStore`,
            // backed by `AtomicU8` (unconditionally `RefUnwindSafe`); `owned`
            // and `commands` are owned, moved-in data with no borrows to
            // invalidate.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_raster_batch(commands, owned, fb.as_ref(), raster_lanes)
            }));
            // The receiver may already be gone (the `Distira` was dropped
            // mid-batch); the batch still ran to completion (or panicked)
            // against its own owned memories, so there is nothing to unwind
            // here regardless -- N5 of the review: this does leave the
            // thread and the batch's memories alive until the batch (or the
            // catch_unwind) finishes, which is harmless in practice.
            let _ = tx.send(outcome);
        });
        self.in_flight = Some(InFlight {
            rx,
            flushed_at: std::time::Instant::now(),
        });
    }

    /// Flush then join: the old (slice 0b and earlier) `drain_raster_queue`'s
    /// exact behaviour, and every call site in
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md` section 1
    /// still uses it -- **except** the swap, which calls
    /// [`Self::flush_raster_queue`] alone (see that method's doc comment).
    fn drain_raster_queue(&mut self, cause: DrainCause) -> u64 {
        self.flush_raster_queue(cause);
        match self.join_raster() {
            Some(outcome) => {
                let written = outcome.written;
                self.record_join(cause, &outcome);
                written
            }
            None => 0,
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
        jobs: &[QueuedCommand],
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
        if let Some(QueuedCommand::Triangle(job)) = jobs.last()
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
        self.drain_raster_queue(DrainCause::SwapOrScanout);
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
        self.drain_raster_queue(DrainCause::LfbRead);
        self.lfb_reads.increment();
        self.lfb_byte_offset(self.lfb_read_base(), offset)
            .and_then(|offset| self.fb.get(offset))
            .unwrap_or(0xff)
    }

    pub fn read_lfb_u16(&mut self, offset: usize) -> u16 {
        self.drain_raster_queue(DrainCause::LfbRead);
        u16::from_le_bytes(self.read_lfb_bytes::<2>(offset & !1))
    }

    pub fn read_lfb_u32(&mut self, offset: usize) -> u32 {
        self.drain_raster_queue(DrainCause::LfbRead);
        u32::from_le_bytes(self.read_lfb_bytes::<4>(offset & !1))
    }

    /// Diagnostic: the aperture counters. See [`DistiraApertureTraffic`].
    ///
    /// `&mut self`: see [`Self::census`]'s doc comment -- same hole, same
    /// fix.
    pub fn aperture_traffic(&mut self) -> DistiraApertureTraffic {
        self.join_raster();
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
        self.drain_raster_queue(DrainCause::LfbWrite);
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
        self.drain_raster_queue(DrainCause::LfbWrite);
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
        self.drain_raster_queue(DrainCause::LfbWrite);
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
            self.drain_raster_queue(DrainCause::RegisterReadStats);
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
                self.drain_raster_queue(DrainCause::NopCmdResetStats);
            }
        } else if !raster_snapshot_covers_register(register)
            || !raster_snapshot_covers_register(voodoo_reg)
        {
            if register == SST_SWAPBUFFER_CMD && async_raster_enabled() {
                // Slice 1 of the async-overlap review
                // (`dev_docs/2026-09-05-distira-async-overlap-review.md`
                // section 2, `dev_docs/2026-09-05-distira-async-overlap-design.md`
                // section 2 item 2): the swap is the one uncovered write
                // that does NOT join. It flushes whatever is queued to the
                // raster pool and returns, so the guest walks on into the
                // next frame's geometry while the batch rasterises.
                // `fastfillCMD`, the other load-bearing uncovered write,
                // reads and clears the shared depth buffer a still-running
                // batch may be drawing into, so it stays on the `else`
                // branch below and joins like every other uncovered write.
                //
                // Gated on `async_raster_enabled()` (B2 of
                // `dev_docs/2026-09-05-distira-async-slice1-review.md`):
                // `IZARRAVM_DISTIRA_ASYNC=0` falls back to
                // `drain_raster_queue` here too, so the whole run behaves
                // exactly as if every join point joined -- the in-binary OFF
                // arm the ladder needs.
                self.flush_raster_queue(DrainCause::RegisterWriteUncovered);
            } else {
                self.drain_raster_queue(DrainCause::RegisterWriteUncovered);
            }
        }
        let chip = tmu_chip_mask(offset);
        // Slice 1 of the async-overlap review: only join for this if
        // `register` could actually be an NCC entry -- see
        // `NccState::touches_register`'s doc comment. Every NCC register is
        // already uncovered by `raster_snapshot_covers_register`, so a real
        // NCC write has already joined above; this second check exists so a
        // write to an UNRELATED register (a vertex, a colour, `triangleCMD`
        // -- the whole hot path a Glide driver spends most of its time on)
        // does not join here too.
        if NccState::touches_register(register)
            && self
                .raster_owned_mut()
                .ncc
                .write_register(chip, register, byte, value)
        {
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
                    // Opens the swap -> next-drain-call window `drain_raster_queue`
                    // closes. See its doc comment and
                    // `dev_docs/2026-09-05-distira-async-overlap-review.md`
                    // section 2.
                    self.last_swap_instant = Some(std::time::Instant::now());
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

    /// Texture memory is NOT part of the [`RasterParams`] snapshot: it is
    /// megabytes, and a lane's borrow of it can never overlap a write's
    /// `&mut` (`#![forbid(unsafe_code)]`, so there is no atomic fallback the
    /// way `FrameStore` has one). A write therefore no longer drains the
    /// queue outright -- see `distira/raster_queue.rs`'s module doc and
    /// `dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 6. It
    /// queues as a [`QueuedCommand::TextureWrite`] instead, which orders it
    /// against the triangles around it without forcing the guest to wait for
    /// a raster join every time a title uploads a texture.
    ///
    /// The DECODE still happens now, against the CURRENT register state,
    /// exactly as it always did -- only the STORE into texture memory is
    /// deferred to `drain_raster_queue`.
    pub fn write_texture_u32(&mut self, aperture_offset: usize, value: u32) {
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
        if !self.raster_queue_enabled {
            // `set_raster_queue_enabled` drains before it flips the flag, so
            // the queue starts empty when it goes false. The
            // immediate-triangle path in `draw_triangle_inner` still pushes
            // while the flag is off -- it does that regardless of
            // `raster_queue_enabled` -- but that push is always immediately
            // followed by its own drain in the same call, with no
            // reentrancy in between, so the queue is back to empty by the
            // time control returns here. The real invariant is that weaker
            // "push is always paired with its drain", not "nothing pushes":
            // a future edit that separated the two would let this
            // synchronous store apply out of order against a still-queued
            // triangle, silently. `queued_triangles_answer_
            // interleaved_reads_like_synchronous_ones` (queue OFF arm) is
            // what keeps the OBSERVABLE behaviour honest -- with the queue
            // off there is no ordering to build, only the same store there
            // always was.
            let mask = DISTIRA_TEX_SIZE - 1;
            let owned = self.raster_owned_mut();
            for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
                owned.texture[tmu][(offset + index) & mask] = byte;
            }
            return;
        }
        let command = QueuedCommand::TextureWrite(QueuedTextureWrite {
            tmu,
            offset,
            bytes: value.to_le_bytes(),
        });
        if !self.raster_queue.push(command) {
            // Full. Draw what is waiting, then this write joins an empty
            // queue, so it is never dropped. The second push cannot refuse:
            // the drain above leaves the queue at zero and the capacity is
            // not zero.
            self.drain_raster_queue(DrainCause::QueueFull);
            let queued = self.raster_queue.push(command);
            debug_assert!(queued, "a drained queue accepts");
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

    // `lfb_byte_offset` deliberately does NOT wrap. It backs the LFB
    // *read* aperture (`read_lfb_u8`, `read_lfb_bytes`) and the guest's own
    // `write_lfb_u8` (dead today -- `vega.rs::write_wide_memory` drops
    // `BusWidth::Byte` before it reaches Distira). Reads past the FBI
    // aperture return OPEN BUS on both references that model an SST-1:
    // 86Box's `voodoo_fb_readw`/`readl` guard with
    // `if (read_addr > fb_mask) return 0xffff` / `0xffffffff`
    // (`vid_voodoo_fb.c:91-95,132-136`) -- note the `& fb_mask` that follows
    // the guard is therefore redundant for memory safety, so the guard is a
    // deliberate behavioural choice, not a bounds check standing in for one
    // -- and DOSBox-X's `lfb_r` returns `0xffffffff` the same way
    // (`voodoo_emu.cpp:2860-2862`). Only the FBI *write* decode wraps
    // (`RasterView::framebuffer_pixel_offset`'s doc comment carries that
    // citation); this function stays unbounded and lets its callers apply
    // whichever contract is theirs.
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
        // Open bus past the aperture, not a wrap: see `lfb_byte_offset`'s
        // doc comment for the read-vs-write citation.
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

    fn lfb_pipeline_depth_test_passes(&mut self, position: (u32, u32), depth: u16) -> bool {
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
        // Wraps modulo FBI RAM size, the same write-side rule
        // `RasterView::framebuffer_pixel_offset` uses -- see that
        // function's doc comment for the hardware citation. Every
        // `color_offset`/`depth_offset` below stays even (both bases are
        // multiples of 8192 and `pixel_offset` is `draw_y * pitch + x * 2`
        // with `pitch` a multiple of 128), so a masked offset can never
        // land on `DISTIRA_FB_SIZE - 1` and hit the one-byte hole noted on
        // `FrameStore::write_u16_le`.
        let mask = (DISTIRA_FB_SIZE - 1) as u64;
        let params = self.raster_params();

        for y in low_y..high_y {
            let draw_y = u64::from(params.draw_y(y as u32));
            for x in left..right {
                let pixel_offset = draw_y
                    .saturating_mul(pitch)
                    .saturating_add(x.saturating_mul(2));
                let color_offset = (u64::from(color_start).saturating_add(pixel_offset)) & mask;
                if write_color {
                    self.fb.write_u16_le(color_offset as usize, color);
                    self.fastfill_pixels += 1;
                }
                let depth_offset = (u64::from(self.aux_base).saturating_add(pixel_offset)) & mask;
                if write_depth {
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
/// Reporting busy instead would be worse in both directions. **Updated after
/// slice 1 of `dev_docs/2026-09-05-distira-async-slice1-review.md` (S4):**
/// under async a busy bit genuinely COULD clear on its own now that a batch
/// runs on another thread -- the old "the drain is synchronous, so nothing
/// clears a busy bit while the guest spins on it" no longer holds as stated,
/// and reads like the reason this is safe when it is not the real one. The
/// actual reason is the join set above: `status` is deliberately not gated
/// by `register_read_needs_raster`, so a guest polling it never forces a
/// join and never observes a half-drawn batch either way -- it only ever
/// sees "idle", true under the synchronous model and unchanged under async,
/// and the pixels it eventually reads are still gated by whichever real
/// observer path it takes next (an LFB read, a scanout, ...), which always
/// joins. A guest waiting for idle before it submits more work therefore
/// still proceeds immediately, same as before -- it just no longer proves
/// anything about whether raster work is actually done, which slice 3 is the
/// one that has to make honest (`dev_docs/2026-09-05-distira-async-overlap-design.md`
/// section 5). And draining on a status poll would put the drain back on
/// the per-triangle path this queue exists to get off, since polling status
/// between triangles is exactly what a Glide driver does.
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
