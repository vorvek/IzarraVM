// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The LFB write pipeline, driven by a SNAPSHOT of the device registers
//! rather than by the live device.
//!
//! Slice 2 of `dev_docs/2026-09-05-distira-async-overlap-design.md` (section
//! 8). Before it, a guest store into the linear frame buffer joined the
//! raster batch first (`DrainCause::LfbWrite`, the design's J2) and then ran
//! the whole per-pixel pipeline on the emulation thread. Descent II writes
//! ~40,830 LFB words a frame against 891 triangles, so under slice 1's async
//! overlap the FIRST word of that burst would join the frame's in-flight
//! batch and collapse the overlap window to whatever the guest managed
//! between its last triangle and the blit -- which is why section 7 of
//! `dev_docs/2026-09-05-distira-async-slice1-review.md` calls this slice "not
//! optional for the Descent row".
//!
//! The fix is the same shape `QueuedCommand::TextureWrite` already has: the
//! write becomes an ordered queue entry, and the pipeline that applies it
//! runs on the raster worker, between the two triangle runs it separates. To
//! survive that deferral the pipeline may not read one field of the live
//! device, so everything it used to reach through `&mut Distira` is either in
//! [`LfbWriteParams`] (the snapshot) or in [`LfbWriteStats`] (the counters it
//! produces, folded into the device at the join, exactly the way
//! `Distira::merge_pixel_stats` folds a lane's `PixelStats`).
//!
//! **The snapshot is `RasterParams` plus `lfbMode`, and that is the whole
//! list.** Every other input the pipeline reads --  `fbzMode`, the write
//! masks inside it, `zaColor`, the chroma key, the alpha mode, the fog
//! registers, the stipple pattern, the framebuffer layout, `aux_base`, the
//! dither flag -- is already a `RasterParams` field, because the triangle
//! path needs the same values snapshotted for the same reason. `lfbMode` is
//! the one addition, and it deliberately stays OUTSIDE
//! `raster_snapshot_covers_register`: a guest that changes the LFB format or
//! the write buffer mid-frame still drains the queue first
//! (`DrainCause::RegisterWriteUncovered`), so this snapshot never has to
//! model a mode change inside a coalesced run. That is the review's section 7
//! caveat 2, and the design's own rule -- "any register added to the snapshot
//! must be added to `RasterParams` in the same commit" -- is satisfied
//! vacuously here: nothing was added to `RasterParams`.
//!
//! **The rotating stipple never defers.** `RasterParams::stipple_test`
//! rotates its caller's copy, so a rotating-stipple write is a serial
//! dependency from one pixel to the next AND from one write to the next --
//! its snapshot would differ at every word, so a coalesced run could not
//! exist at all, and `Distira::stipple` would need a write-back ordered
//! against the guest's own stipple writes. `Distira::lfb_write_defers` routes
//! that case onto the synchronous path, exactly as `triangle_defers` does for
//! a rotating-stipple triangle.

use super::*;

/// The register state one LFB write applies against, copied out of the
/// device when the guest issues it. See this module's header for why the
/// list is `RasterParams` plus `lfbMode` and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LfbWriteParams {
    pub(super) params: RasterParams,
    pub(super) lfb_mode: u32,
}

/// Which of the guest's two live LFB store widths a queued word replays as.
/// `write_lfb_u8` is not here: `vega.rs::write_wide_memory` drops
/// `BusWidth::Byte` before it reaches Distira, so that path has no caller in
/// the workspace and is left fully synchronous rather than given a deferred
/// twin no guest can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LfbWriteWidth {
    U16,
    U32,
}

/// What applying one run of LFB writes did to the device counters. The
/// pipeline used to bump these on `self` as it went; a deferred run produces
/// them on the raster worker and `Distira::join_raster` folds them in, so the
/// counters a statistics read observes are still whole-batch epochs.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LfbWriteStats {
    pub(super) color_pixels_stored: u64,
    pub(super) fbi_chroma_fail: u32,
    pub(super) fbi_afunc_fail: u32,
    /// The rotating-stipple state. Only ever moved by a write that took the
    /// SYNCHRONOUS path (see this module's header); a deferred run carries
    /// its snapshot's pattern through unchanged, so the join has nothing to
    /// write back.
    pub(super) stipple: u32,
}

impl LfbWriteStats {
    pub(super) fn add(&mut self, other: &Self) {
        self.color_pixels_stored = self
            .color_pixels_stored
            .saturating_add(other.color_pixels_stored);
        self.fbi_chroma_fail = self.fbi_chroma_fail.wrapping_add(other.fbi_chroma_fail);
        self.fbi_afunc_fail = self.fbi_afunc_fail.wrapping_add(other.fbi_afunc_fail);
    }
}

/// One LFB write's pipeline, bound to its snapshot, the memories it reads
/// and writes, and the counters it produces.
///
/// The bodies below are the pre-slice-2 `Distira` methods verbatim, with
/// `self.lfb_mode` becoming `self.params.lfb_mode`, every other register
/// becoming a `self.params.params` field, `self.fb` becoming
/// `self.memory.fb`, and `self.raster_view()` becoming
/// `self.memory.view(self.params.params)`. Nothing about the pixel maths
/// changed; only where the inputs come from.
pub(super) struct LfbWriter<'memory, 'stats> {
    params: LfbWriteParams,
    memory: ViewMemory<'memory>,
    stats: &'stats mut LfbWriteStats,
}

impl<'memory, 'stats> LfbWriter<'memory, 'stats> {
    pub(super) fn new(
        params: LfbWriteParams,
        memory: ViewMemory<'memory>,
        stats: &'stats mut LfbWriteStats,
    ) -> Self {
        Self {
            params,
            memory,
            stats,
        }
    }

    /// Replay one queued (or immediate) word at `aperture_offset`.
    pub(super) fn write(&mut self, width: LfbWriteWidth, aperture_offset: usize, value: u32) {
        match width {
            LfbWriteWidth::U16 => self.write_u16(aperture_offset, value as u16),
            LfbWriteWidth::U32 => self.write_u32(aperture_offset, value),
        }
    }

    fn lfb_mode(&self) -> u32 {
        self.params.lfb_mode
    }

    fn fbz_mode(&self) -> u32 {
        self.params.params.fbz_mode
    }

    fn za_color(&self) -> u32 {
        self.params.params.za_color
    }

    fn view(&self) -> RasterView<'memory> {
        self.memory.view(self.params.params)
    }

    pub(super) fn lfb_write_base(&self) -> u32 {
        match self.lfb_mode() & LFB_WRITE_MASK {
            LFB_WRITE_FRONT => self.params.params.display.front_base,
            LFB_WRITE_BACK => self.params.params.display.back_base,
            _ => self.params.params.display.front_base,
        }
    }

    fn lfb_pipeline_writes_color(&self) -> bool {
        self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.fbz_mode() & FBZ_RGB_WMASK != 0
    }

    fn lfb_pipeline_writes_depth(&self) -> bool {
        self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.fbz_mode() & FBZ_DEPTH_WMASK != 0
    }

    fn write_lfb_color_pipeline_pixel(
        &mut self,
        base: u32,
        position: (u32, u32),
        raw: u16,
        color: (u8, u8, u8),
        alpha: u8,
    ) {
        let pipeline = self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE != 0;
        let write_color = self.lfb_pipeline_writes_color();
        let write_depth = pipeline && self.fbz_mode() & FBZ_DEPTH_WMASK != 0;
        let depth = self.za_color() as u16;
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
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0
            || self.fbz_mode() & FBZ_DEPTH_ENABLE == 0
        {
            return true;
        }
        let Some(old_depth) = self.view().read_depth_pixel(position.0, position.1) else {
            return false;
        };
        depth_compare_passes(self.fbz_mode(), old_depth, depth)
    }

    fn lfb_pipeline_color_passes(&mut self, color: (u8, u8, u8)) -> bool {
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0
            || self
                .params
                .params
                .chroma_key_passes(color.0, color.1, color.2)
        {
            return true;
        }
        self.stats.fbi_chroma_fail = self.stats.fbi_chroma_fail.wrapping_add(1);
        false
    }

    fn lfb_pipeline_alpha_passes(&mut self, alpha: u8) -> bool {
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0
            || self.params.params.alpha_test_passes(alpha)
        {
            return true;
        }
        self.stats.fbi_afunc_fail = self.stats.fbi_afunc_fail.wrapping_add(1);
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
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0 {
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
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0 {
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
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0 {
            return Some(None);
        }
        let position = self.lfb_pipeline_stipple_position(position)?;
        if !self.lfb_pipeline_depth_test_passes(position, depth) {
            return None;
        }
        let alpha = (self.za_color() >> 24) as u8;
        self.lfb_pipeline_shade_color_at(base, position, pack_rgb565(0, 0, 0), (0, 0, 0), alpha)
            .map(Some)
    }

    fn lfb_pipeline_stipple_position(&mut self, position: (u32, u32)) -> Option<(u32, u32)> {
        let (x, y) = position;
        let base = self.lfb_write_base();
        self.params.params.framebuffer_pixel_offset(base, x, y)?;
        if self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE == 0 || self.stipple_test_passes(x, y) {
            return Some((x, y));
        }
        None
    }

    fn stipple_test_passes(&mut self, x: u32, y: u32) -> bool {
        let mut stipple = self.stats.stipple;
        let passes = self.params.params.stipple_test(&mut stipple, x, y);
        self.stats.stipple = stipple;
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
        let view = self.view();
        let (r, g, b) = view.apply_fog_color(color.0, color.1, color.2);
        let (r, g, b) = view.alpha_blend_color_at_base(base, (x, y), (r, g, b), alpha);
        Some(pack_rgb565_for_pixel(
            r,
            g,
            b,
            x,
            y,
            self.params.params.dither_enabled,
        ))
    }

    fn write_depth_pixel_at(&mut self, position: (u32, u32), value: u16) {
        let Some(offset) = self.params.params.framebuffer_pixel_offset(
            self.params.params.aux_base,
            position.0,
            position.1,
        ) else {
            return;
        };
        self.memory.fb.write_u16_le(offset, value);
    }

    fn write_color_pixel(&mut self, base: u32, position: (u32, u32), value: u16) {
        self.stats.color_pixels_stored = self.stats.color_pixels_stored.saturating_add(1);
        let Some(offset) = self
            .params
            .params
            .framebuffer_pixel_offset(base, position.0, position.1)
        else {
            return;
        };
        self.memory.fb.write_u16_le(offset, value);
    }

    fn write_u16(&mut self, offset: usize, value: u16) {
        let base = self.lfb_write_base();
        let write_color = self.lfb_pipeline_writes_color();
        let write_depth = self.lfb_pipeline_writes_depth();
        let pipeline = self.lfb_mode() & LFB_ENABLE_PIXEL_PIPELINE != 0;
        let position = lfb_position(offset, false);
        match self.lfb_mode() & LFB_FORMAT_MASK {
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

    fn write_u32(&mut self, offset: usize, value: u32) {
        let base = self.lfb_write_base();
        let write_color = self.lfb_pipeline_writes_color();
        let write_depth = self.lfb_pipeline_writes_depth();
        let format = self.lfb_mode() & LFB_FORMAT_MASK;
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
                let alpha = if format == LFB_FORMAT_ARGB8888 {
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
}
