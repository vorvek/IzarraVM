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
///
/// 512 was sized for a triangle-only queue. `tombraid3d-586`'s Lara's Home
/// walk pushes ~387 triangles and ~65 texture writes a frame (
/// `dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 4) -- ~452
/// entries against a 512 cap, one entry-fill from an extra `queue_full`
/// drain most frames. 1024 buys headroom back without a redesign: each
/// `QueuedCommand` slot costs ~600 B (`QueuedTriangle`'s size dominates,
/// see the `large_enum_variant` allow below), so the doubled capacity is
/// ~300 KB more on the one allocation `RasterQueue::recycle` keeps alive,
/// not a per-pixel cost.
pub(super) const RASTER_QUEUE_CAPACITY: usize = 1024;

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

/// A COALESCED RUN of guest LFB writes waiting to be applied, all against
/// one snapshot (`distira/lfb_write.rs`) and one store width. The words
/// themselves live in the batch's own `lfb_words` buffer, not in the entry:
/// see [`RasterQueue::push_lfb_write`] for why a run is one entry however
/// many words it holds.
#[derive(Debug, Clone, Copy)]
pub(super) struct QueuedLfbWrite {
    pub(super) params: LfbWriteParams,
    pub(super) width: LfbWriteWidth,
    /// First word of this run in the batch's `lfb_words`.
    pub(super) start: u32,
    /// How many words the run holds. Always at least one.
    pub(super) count: u32,
}

/// What [`RasterQueue::push_lfb_write`] did with a word, so the caller can
/// tell a coalesced word (free) from a new run (one more queue entry) from a
/// refusal (drain and retry) without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LfbPush {
    /// Folded into the run already at the tail of the queue.
    Extended,
    /// Started a new run, which cost one `RASTER_QUEUE_CAPACITY` entry.
    Started,
    /// The queue (or the word buffer) is full; the caller must drain.
    Full,
}

/// One guest LFB store, as `(aperture offset, value)`. A `u16` store keeps
/// its value in the low half.
#[derive(Debug, Clone, Copy)]
pub(super) struct LfbWord {
    pub(super) offset: u32,
    pub(super) value: u32,
}

/// One entry on the raster queue, in submission order.
///
/// `TextureWrite` and `LfbWrite` pay `QueuedTriangle`'s full size (~600
/// bytes, mostly the `f32`/`f64` interpolation planes in `TriangleContext`)
/// as padding. Boxing the triangle would shrink that, but it would also make
/// the queue allocate per triangle and lose `Copy` -- exactly the cost this
/// queue exists to avoid paying per entry (see this module's header). The
/// queue is capacity-bounded (`RASTER_QUEUE_CAPACITY`), so the wasted bytes
/// are a few hundred KB at most, once, not a per-pixel cost.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy)]
pub(super) enum QueuedCommand {
    Triangle(QueuedTriangle),
    TextureWrite(QueuedTextureWrite),
    LfbWrite(QueuedLfbWrite),
}

/// How many LFB words one batch may coalesce before the queue forces a
/// drain. Descent II's per-frame blit is ~40,830 words
/// (`dev_docs/2026-09-05-distira-async-slice1-review.md` section 7), so this
/// holds several frames' worth and the cap is a runaway guard, not a working
/// limit: at 8 bytes a word it bounds the buffer at 2 MB, on the one
/// allocation `RasterQueue::recycle` keeps alive.
pub(super) const LFB_WORD_CAPACITY: usize = 1 << 18;

/// A batch handed to the raster worker: the ordered commands, plus the word
/// payload every `QueuedCommand::LfbWrite` in them indexes into.
#[derive(Default)]
pub(super) struct RasterBatch {
    pub(super) commands: Vec<QueuedCommand>,
    pub(super) lfb_words: Vec<LfbWord>,
}

/// The pending commands.
#[derive(Default)]
pub(super) struct RasterQueue {
    pending: Vec<QueuedCommand>,
    /// The word payload for the `QueuedCommand::LfbWrite` entries in
    /// `pending`, in submission order. Kept beside the commands rather than
    /// inside them so a run of thousands of guest stores is ONE entry
    /// against `RASTER_QUEUE_CAPACITY` -- see `push_lfb_write`.
    lfb_words: Vec<LfbWord>,
    /// S2 of `dev_docs/2026-09-05-distira-async-slice1-review.md`: a
    /// recycled batch allocation parked here when `recycle` ran but
    /// `pending` was not itself available to adopt it. See `recycle`'s doc
    /// comment for why that happens under async and did not before.
    /// `push` claims it the next time `pending` needs to grow from empty.
    spare: Vec<QueuedCommand>,
    /// The same idea for the word payload.
    spare_words: Vec<LfbWord>,
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
        // S2: claim the spare allocation the moment `pending` needs to grow
        // from empty -- this is the other half of `recycle`'s fix: a batch
        // parked in `spare` because `pending` was non-empty at the join is
        // otherwise never seen again.
        if self.pending.is_empty() && self.spare.capacity() > self.pending.capacity() {
            self.pending = std::mem::take(&mut self.spare);
        }
        if self.lfb_words.is_empty() && self.spare_words.capacity() > self.lfb_words.capacity() {
            self.lfb_words = std::mem::take(&mut self.spare_words);
        }
        self.pending.push(command);
        true
    }

    /// Add a guest LFB store, coalescing it into the run at the tail of the
    /// queue when it can. Returns false when the caller must drain first.
    ///
    /// **Coalescing is what makes slice 2 pay at all.** Descent II writes
    /// ~40,830 LFB words a frame; as one entry each they would overrun
    /// `RASTER_QUEUE_CAPACITY` forty times a frame, and every overrun is a
    /// `queue_full` drain -- a flush AND a full join -- so the burst would
    /// force MORE joins than the synchronous J2 it replaced, not fewer
    /// (`dev_docs/2026-09-05-distira-async-slice1-review.md` section 7,
    /// caveat 1). A run therefore extends for as long as the next write
    /// carries the SAME snapshot and the same store width, and the words
    /// pile into `lfb_words` behind one entry.
    ///
    /// **The run key is the snapshot and the width, not address
    /// contiguity.** The design and the review both say "`{base, bytes}`
    /// runs", and a contiguity-keyed run is the natural reading of that --
    /// but an LFB aperture offset packs `x` into bits 0..10 and `y` above
    /// them, so a full-width blit is contiguous only WITHIN a scanline and
    /// jumps at every row boundary. On `descent2-3dfx-586` (640x480, two
    /// bytes a pixel) that is ~480 entries a frame beside ~891 triangles --
    /// back over the 1024 cap, and back to `queue_full` drains, which is the
    /// exact failure coalescing exists to prevent. Keying on the snapshot
    /// instead costs 4 bytes a word (the offset rides along) and makes the
    /// whole burst ONE entry. Ordering is unaffected either way: the words
    /// replay in submission order inside the run, and the run itself sits in
    /// submission order among the triangles.
    pub(super) fn push_lfb_write(
        &mut self,
        params: LfbWriteParams,
        width: LfbWriteWidth,
        offset: u32,
        value: u32,
    ) -> LfbPush {
        if self.lfb_words.len() >= LFB_WORD_CAPACITY {
            return LfbPush::Full;
        }
        let word = LfbWord { offset, value };
        if let Some(QueuedCommand::LfbWrite(run)) = self.pending.last_mut()
            && run.width == width
            && run.params == params
        {
            self.lfb_words.push(word);
            run.count += 1;
            return LfbPush::Extended;
        }
        if self.pending.len() >= RASTER_QUEUE_CAPACITY {
            return LfbPush::Full;
        }
        let start = self.lfb_words.len() as u32;
        self.lfb_words.push(word);
        // Not `self.push`: the spare-allocation adoption there would swap
        // `pending` out from under the run this call may have just extended,
        // and a new run is exactly the case where `pending` is NOT empty (it
        // holds the triangles this run is ordered against) often enough that
        // the adoption would not fire anyway.
        self.pending.push(QueuedCommand::LfbWrite(QueuedLfbWrite {
            params,
            width,
            start,
            count: 1,
        }));
        LfbPush::Started
    }

    /// Hand the batch to the caller, leaving the queue empty.
    pub(super) fn take(&mut self) -> RasterBatch {
        RasterBatch {
            commands: std::mem::take(&mut self.pending),
            lfb_words: std::mem::take(&mut self.lfb_words),
        }
    }

    /// Take the drained batch's allocation back, so a steady render loop
    /// stops allocating after its first frame.
    ///
    /// **Widened for S2 of `dev_docs/2026-09-05-distira-async-slice1-review.md`.**
    /// The OLD guard (`self.pending.is_empty()` alone) silently dropped the
    /// allocation whenever it did not hold: harmless under the fully
    /// synchronous model (slice 0b and earlier), where a drain always ran
    /// with nothing else queued yet. Under async the swap-flushed batch is
    /// joined at a call where the guest has ALREADY queued the next frame's
    /// triangles into `pending` -- exactly the case the lever exists to
    /// create -- so that guard failed on precisely the batch this fix cares
    /// about, reintroducing the per-frame allocation and memcpy #840's nit D
    /// removed, on the hot path a change chasing milliseconds should not be
    /// paying. `spare` is the fallback: when `pending` cannot adopt the
    /// allocation directly, it waits in `spare` instead of being dropped,
    /// and `push` above claims it the next time `pending` empties.
    pub(super) fn recycle(&mut self, batch: RasterBatch) {
        let RasterBatch {
            mut commands,
            mut lfb_words,
        } = batch;
        commands.clear();
        if self.pending.is_empty() && commands.capacity() > self.pending.capacity() {
            self.pending = commands;
        } else if commands.capacity() > self.spare.capacity() {
            self.spare = commands;
        }
        lfb_words.clear();
        if self.lfb_words.is_empty() && lfb_words.capacity() > self.lfb_words.capacity() {
            self.lfb_words = lfb_words;
        } else if lfb_words.capacity() > self.spare_words.capacity() {
            self.spare_words = lfb_words;
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
        // `spare` is a scratch allocation, not device state -- like
        // `pending`'s own capacity, a clone need not carry it, and starting
        // empty is simpler than deciding whether to clone dead memory too.
        Self {
            pending: self.pending.clone(),
            lfb_words: self.lfb_words.clone(),
            spare: Vec::new(),
            spare_words: Vec::new(),
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
    jobs: &[QueuedCommand],
    view_memory: ViewMemory<'_>,
    lane: u32,
    lanes: u32,
    stats: &mut PixelStats,
) {
    let mut local = PixelStats::new(0);
    for job in jobs {
        // Every entry in `jobs` is a `Triangle` by construction: the caller
        // (`Distira::drain_raster_queue`) splits the batch into runs at
        // every `TextureWrite`, so a run never holds one. Matching instead
        // of asserting keeps this function total even if that invariant is
        // ever loosened.
        let QueuedCommand::Triangle(job) = job else {
            continue;
        };
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

#[cfg(test)]
impl RasterQueue {
    /// Test-only: `pending`'s allocation size, so a recycle test can prove
    /// an allocation survived rather than being silently dropped and
    /// regrown. Not used by any non-test code.
    fn pending_capacity(&self) -> usize {
        self.pending.capacity()
    }
}

#[cfg(test)]
#[path = "raster_queue_test.rs"]
mod tests;
