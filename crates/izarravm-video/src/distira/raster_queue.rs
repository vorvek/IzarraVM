// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The deferred triangle queue.
//!
//! A guest triangle costs a fork and a join, and the join is what the CPU
//! thread waits on. 86Box does not pay that per triangle: `voodoo_queue_triangle`
//! copies the register state and returns, and the render threads walk the
//! queue on their own. Distira does the same thing without owning threads of
//! its own: `run_triangle_command` copies the registers into a
//! [`QueuedTriangle`] and returns, and the queue is rasterised at the next
//! point where the guest could tell the difference (see
//! `Distira::drain_raster_queue` and its callers).
//!
//! The whole batch takes ONE fork and ONE join. Lanes split the batch by
//! FRAMEBUFFER row: lane `i` owns the rows where `draw_y(y) % lanes == i`,
//! for every triangle in the batch.
//!
//! The framebuffer row is the one that matters, and it is not the triangle's
//! row. `fbzMode`'s Y-origin bit flips a triangle vertically, so a lane's
//! rows are `y % lanes == i` for an unflipped triangle and
//! `(height - 1 - y) % lanes == i` for a flipped one. Every store in
//! `RasterView::raster_row` goes through `draw_y`, so deriving the lane from
//! it is what makes the rows a true partition of the frame store. Splitting
//! on the triangle's own row instead would look right and would be wrong the
//! moment one batch held triangles with different Y-origin bits: at two lanes
//! the flip always swaps parity, so two lanes would land on one framebuffer
//! row and race each other's read-modify-write in the depth test and the
//! blend.
//!
//! Given that partition, every per-pixel test is a pure function of the
//! pixel, so a lane can walk the whole queue in submission order and the
//! result is the serial result, byte for byte. Splitting per triangle instead
//! would need a barrier between triangles, which is the cost this exists to
//! remove.
//!
//! **Texture-aperture writes ride the same queue.** `write_texture_u32` used
//! to drain unconditionally -- 65 times a frame on `tombraid3d-586`'s Lara's
//! Home walk, once per accepted write, which is what actually forced the 60
//! drains a frame the module doc above is written against (see
//! `dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 6). A
//! [`QueuedCommand::TextureWrite`] entry orders a write against the
//! triangles around it instead: `Distira::drain_raster_queue` walks the
//! batch in submission order and splits it at every `TextureWrite`, so a
//! triangle queued before the write still samples the OLD texel (its
//! segment rasterises before the write is applied) and a triangle queued
//! after samples the NEW one (its segment rasterises after). The write
//! itself always applies serially, between segments, on the thread that
//! calls `drain_raster_queue` -- never inside a lane. `texture: &'a [Vec<u8>;
//! 2]` in [`ViewMemory`] is a plain shared borrow, not `FrameStore`'s atomic
//! one, and the crate forbids unsafe, so a lane touching it while another
//! lane (or the write) touches it too would be undefined behaviour, not
//! merely a race the tests might catch. Splitting at the write is what keeps
//! every lane's borrow and the write's `&mut` from ever overlapping in time.

use super::raster_kernel::{ModeKey, select_kernel};
use super::*;

/// How many entries wait before the queue rasterises itself. A batch that
/// grows without bound would hold the frame's whole geometry in memory and
/// spend the win on cache misses; a few hundred entries is already more
/// than enough to amortise one fork. Texture writes share the capacity with
/// triangles -- both are queued commands now, and both need the guest to
/// keep moving without a synchronous drain.
pub(super) const RASTER_QUEUE_CAPACITY: usize = 512;

/// A triangle waiting to be drawn, with the register state it was submitted
/// against. Both halves are `Copy`: the queue holds no borrow of the device,
/// which is what lets the guest keep writing registers.
#[derive(Debug, Clone, Copy)]
pub(super) struct QueuedTriangle {
    pub(super) params: RasterParams,
    pub(super) context: TriangleContext,
}

/// A texture-aperture write waiting to be applied, already decoded to a TMU
/// and a byte offset (masked into `0..DISTIRA_TEX_SIZE`) -- the decode reads
/// live register state (`Distira::texture_write_offset`), so it happens at
/// enqueue time, exactly as it always did. Only the STORE into texture
/// memory is deferred.
#[derive(Debug, Clone, Copy)]
pub(super) struct QueuedTextureWrite {
    pub(super) tmu: usize,
    pub(super) offset: usize,
    pub(super) bytes: [u8; 4],
}

/// One entry on the raster queue, in submission order.
///
/// `TextureWrite` pays `QueuedTriangle`'s full size (~600 bytes, mostly the
/// `f32`/`f64` interpolation planes in `TriangleContext`) as padding.
/// Boxing the triangle would shrink that, but it would also make the queue
/// allocate per triangle and lose `Copy` -- exactly the cost this queue
/// exists to avoid paying per entry (see this module's header). The queue
/// is capacity-bounded (`RASTER_QUEUE_CAPACITY`), so the wasted bytes are a
/// few hundred KB at most, once, not a per-pixel cost.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy)]
pub(super) enum QueuedCommand {
    Triangle(QueuedTriangle),
    TextureWrite(QueuedTextureWrite),
}

/// The pending commands.
#[derive(Default)]
pub(super) struct RasterQueue {
    pending: Vec<QueuedCommand>,
}

impl RasterQueue {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Add a command. Returns false when the queue is full and the caller
    /// must drain before it can add this one.
    pub(super) fn push(&mut self, command: QueuedCommand) -> bool {
        if self.pending.len() >= RASTER_QUEUE_CAPACITY {
            return false;
        }
        self.pending.push(command);
        true
    }

    /// Hand the batch to the caller, leaving the queue empty.
    pub(super) fn take(&mut self) -> Vec<QueuedCommand> {
        std::mem::take(&mut self.pending)
    }

    /// Take the drained batch's allocation back, so a steady render loop
    /// stops allocating after its first frame.
    pub(super) fn recycle(&mut self, mut batch: Vec<QueuedCommand>) {
        if self.pending.is_empty() && batch.capacity() > self.pending.capacity() {
            batch.clear();
            self.pending = batch;
        }
    }
}

/// A pending triangle is scheduling state, not device state: the pixels it
/// will paint are decided already, they have simply not been stored yet. Two
/// devices therefore compare on how much work is outstanding, not on the
/// geometry of it. Same reasoning as [`FrameStore`]'s hand-written impls.
///
/// The blind spot is real and worth naming: two devices holding the SAME
/// number of pending triangles compare equal however different that geometry
/// is. Nothing can currently observe it, because the geometry is only ever
/// observed as pixels and every path to a pixel draws the queue first, so a
/// comparison that ran after either device was read would be comparing two
/// empty queues. Comparing properly is not an option anyway: `TriangleContext`
/// carries `f32` and `f64` planes, so it has no `Eq`, and `Distira` derives
/// one.
impl PartialEq for RasterQueue {
    fn eq(&self, other: &Self) -> bool {
        self.pending.len() == other.pending.len()
    }
}

impl Eq for RasterQueue {}

impl Clone for RasterQueue {
    fn clone(&self) -> Self {
        Self {
            pending: self.pending.clone(),
        }
    }
}

impl std::fmt::Debug for RasterQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "RasterQueue({} pending)", self.pending.len())
    }
}

/// Rasterise one lane's rows of a whole batch, in submission order.
///
/// `lane` owns the FRAMEBUFFER rows where `draw_y(y) % lanes == lane`, which
/// is the triangle's own row only while the Y-origin bit is clear. See this
/// module's header for why the framebuffer row is the one that has to
/// partition.
///
/// The stipple pattern is re-seeded per triangle from that triangle's
/// snapshot; a triangle that uses the ROTATING stipple never reaches a batch
/// with a neighbour, because rotating stipple chains from one triangle to the
/// next and that chain cannot be split across lanes.
pub(super) fn render_band(
    jobs: &[QueuedTriangle],
    view_memory: ViewMemory<'_>,
    lane: u32,
    lanes: u32,
    stats: &mut PixelStats,
) {
    let mut local = PixelStats::new(0);
    for job in jobs {
        local.stipple = job.params.stipple;
        let view = view_memory.view(job.params);
        // Picked once per triangle, never per row or per pixel: the mode
        // key is triangle-constant, so re-deriving it inside the row loop
        // below would just reintroduce the branch the kernel exists to
        // remove.
        let kernel = select_kernel(ModeKey::for_triangle(&job.params, &job.context));
        let TriangleContext { min_y, max_y, .. } = job.context;
        // The triangle row this lane wants. `draw_y` is `y` or
        // `height - 1 - y`, and the bounding box is clamped to the display
        // height when the triangle is set up, so over `min_y..max_y` it is a
        // bijection and inverting it on the residue is exact.
        let residue = if job.params.fbz_mode & FBZ_Y_ORIGIN == 0 {
            lane
        } else {
            (job.params.display.height.saturating_sub(1) % lanes + lanes - lane) % lanes
        };
        let mut y = min_y + (lanes + residue - min_y % lanes) % lanes;
        while y < max_y {
            debug_assert_eq!(view.draw_y(y) % lanes, lane, "lanes must partition rows");
            kernel(&view, &job.context, y, &mut local);
            y = y.saturating_add(lanes);
        }
    }
    *stats = local;
}

/// The memories a [`RasterView`] reads live, borrowed once for a whole batch.
#[derive(Clone, Copy)]
pub(super) struct ViewMemory<'a> {
    pub(super) fb: &'a FrameStore,
    pub(super) texture: &'a [Vec<u8>; 2],
    pub(super) ncc: &'a NccState,
}

impl<'a> ViewMemory<'a> {
    pub(super) fn view(self, params: RasterParams) -> RasterView<'a> {
        RasterView {
            params,
            fb: self.fb,
            texture: self.texture,
            ncc: self.ncc,
        }
    }
}
