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
//! ABSOLUTE screen row: lane `i` owns the rows where `y % lanes == i`, for
//! every triangle in the batch. Rows partition the frame store and every
//! per-pixel test is a pure function of the pixel, so a lane can walk the
//! whole queue in submission order and the result is the serial result, byte
//! for byte. Splitting per triangle instead would need a barrier between
//! triangles, which is the cost this exists to remove.

use super::*;

/// How many triangles wait before the queue rasterises itself. A batch that
/// grows without bound would hold the frame's whole geometry in memory and
/// spend the win on cache misses; a few hundred triangles is already more
/// than enough to amortise one fork.
pub(super) const RASTER_QUEUE_CAPACITY: usize = 512;

/// A triangle waiting to be drawn, with the register state it was submitted
/// against. Both halves are `Copy`: the queue holds no borrow of the device,
/// which is what lets the guest keep writing registers.
#[derive(Debug, Clone, Copy)]
pub(super) struct QueuedTriangle {
    pub(super) params: RasterParams,
    pub(super) context: TriangleContext,
}

/// The pending triangles.
#[derive(Default)]
pub(super) struct RasterQueue {
    pending: Vec<QueuedTriangle>,
}

impl RasterQueue {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Add a triangle. Returns false when the queue is full and the caller
    /// must drain before it can add this one.
    pub(super) fn push(&mut self, triangle: QueuedTriangle) -> bool {
        if self.pending.len() >= RASTER_QUEUE_CAPACITY {
            return false;
        }
        self.pending.push(triangle);
        true
    }

    /// Hand the batch to the caller, leaving the queue empty.
    pub(super) fn take(&mut self) -> Vec<QueuedTriangle> {
        std::mem::take(&mut self.pending)
    }

    /// Take the drained batch's allocation back, so a steady render loop
    /// stops allocating after its first frame.
    pub(super) fn recycle(&mut self, mut batch: Vec<QueuedTriangle>) {
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
/// `lane` owns the absolute rows where `y % lanes == lane`. The stipple
/// pattern is re-seeded per triangle from that triangle's snapshot; a
/// triangle that uses the ROTATING stipple never reaches a batch with a
/// neighbour, because rotating stipple chains from one triangle to the next
/// and that chain cannot be split across lanes.
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
        let TriangleContext { min_y, max_y, .. } = job.context;
        let mut y = min_y + (lanes + lane - min_y % lanes) % lanes;
        while y < max_y {
            view.raster_row(&job.context, y, &mut local);
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
