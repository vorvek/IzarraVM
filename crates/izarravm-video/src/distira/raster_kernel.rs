// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Mode-keyed raster kernels: the compile-time equivalent of 86Box's
//! per-mode pixel-pipeline codegen (`vid_voodoo_codegen_x86-64.h`).
//!
//! Distira's pixel loop re-tests a handful of triangle-constant flags at
//! every pixel: whether the triangle carries a depth value at all, whether
//! that value is Z or W encoded, whether the colour combine reads a texture,
//! and whether the blend unit reads the destination pixel back. None of
//! those four flags can change mid-triangle, so paying their branch on
//! every pixel is pure waste once the triangle is on the queue.
//!
//! [`ModeKey`] packs the four flags a triangle's [`RasterParams`] and
//! [`TriangleContext`] decide before the first pixel is touched.
//! [`select_kernel`] turns a key into a monomorphized instantiation of
//! [`RasterView::raster_row_specialized`] — a `fn` item, coerced to a plain
//! function pointer, so `render_band` picks it ONCE per triangle and the row
//! loop underneath never branches on the key again.
//!
//! Per the L2 handoff's binary-size guard, only single-bit flags that gate a
//! WHOLE per-pixel code path are keyed here. Multi-bit fields — the depth
//! comparison function, the blend source/dest functions, the colour combine
//! unit's mode bits — stay data, read out of `RasterParams` exactly as
//! `raster_row` already reads them, so the kernel table cannot drift out of
//! sync with them.
//!
//! `ModeKey`'s four booleans are a total description of a (depth, wbuffer,
//! textured, blend) tuple, and `select_kernel`'s match is exhaustive over
//! that tuple — every key this module can construct maps to a real kernel.
//! There is no "unlisted key" case to fall back from: correctness does not
//! depend on which combinations Tomb Raider or Descent II happen to hit.
//! `RasterView::raster_row`, the unspecialized function every kernel body
//! was copied from, stays in `raster_view.rs` untouched, as the oracle the
//! differential test in `raster_kernel_test.rs` checks every kernel against.

use super::*;

/// The four single-bit flags that select a monomorphized raster kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ModeKey {
    /// The triangle carries a depth value (`TriangleContext::depths` is
    /// `Some`). When false, `wbuffer` is meaningless and ignored.
    pub(super) depth: bool,
    /// The depth value is W-encoded (`TriangleDepth::W`) rather than linear
    /// Z (`TriangleDepth::Z`). Only meaningful when `depth` is set.
    pub(super) wbuffer: bool,
    /// The colour combine reads a texture (`FBZCP_TEXTURE_ENABLED`).
    pub(super) textured: bool,
    /// The blend unit reads the destination pixel back
    /// (`ALPHA_BLEND_ENABLE`).
    pub(super) blend: bool,
}

impl ModeKey {
    /// Derive the key from exactly the fields `raster_row_specialized`'s
    /// `debug_assert!`s check, so the two can never silently drift apart:
    /// a mismatch fails loudly in any debug or test build.
    pub(super) fn for_triangle(params: &RasterParams, context: &TriangleContext) -> Self {
        Self {
            depth: context.depths.is_some(),
            wbuffer: matches!(context.depths, Some(TriangleDepth::W(_))),
            textured: params.fbz_color_path & FBZCP_TEXTURE_ENABLED != 0,
            blend: params.alpha_mode & ALPHA_BLEND_ENABLE != 0,
        }
    }
}

/// A monomorphized row rasteriser, coerced from a `raster_row_specialized`
/// instantiation to a plain function pointer.
pub(super) type RasterRowKernel = fn(&RasterView<'_>, &TriangleContext, u32, &mut PixelStats);

/// One named, non-generic wrapper per `raster_row_specialized` instantiation.
/// `RasterView::raster_row_specialized::<D, W, T, B>` cannot be named
/// directly as a `RasterRowKernel` value: its inferred type carries the
/// higher-ranked lifetime bound from the generic method one rank narrower
/// than the `for<'a, 'b, 'c, 'd>` `RasterRowKernel` needs, so rustc rejects
/// the coercion at the match arms in `select_kernel` (E0308, "one type is
/// more general than the other"). A free function with explicit,
/// non-generic argument types has no such inference to narrow, so wrapping
/// each instantiation in one is the fix, not a workaround.
macro_rules! kernel_fn {
    ($name:ident, $depth:literal, $wbuf:literal, $tex:literal, $blend:literal) => {
        fn $name(view: &RasterView<'_>, context: &TriangleContext, y: u32, stats: &mut PixelStats) {
            view.raster_row_specialized::<$depth, $wbuf, $tex, $blend>(context, y, stats);
        }
    };
}

kernel_fn!(kernel_ff_f_f, false, false, false, false);
kernel_fn!(kernel_ff_f_t, false, false, false, true);
kernel_fn!(kernel_ff_t_f, false, false, true, false);
kernel_fn!(kernel_ff_t_t, false, false, true, true);
kernel_fn!(kernel_tf_f_f, true, false, false, false);
kernel_fn!(kernel_tf_f_t, true, false, false, true);
kernel_fn!(kernel_tf_t_f, true, false, true, false);
kernel_fn!(kernel_tf_t_t, true, false, true, true);
kernel_fn!(kernel_tt_f_f, true, true, false, false);
kernel_fn!(kernel_tt_f_t, true, true, false, true);
kernel_fn!(kernel_tt_t_f, true, true, true, false);
kernel_fn!(kernel_tt_t_t, true, true, true, true);

/// Pick the kernel for a mode key. Called once per triangle
/// (`render_band`), never per row or per pixel.
pub(super) fn select_kernel(key: ModeKey) -> RasterRowKernel {
    match (key.depth, key.wbuffer, key.textured, key.blend) {
        (false, _, false, false) => kernel_ff_f_f,
        (false, _, false, true) => kernel_ff_f_t,
        (false, _, true, false) => kernel_ff_t_f,
        (false, _, true, true) => kernel_ff_t_t,
        (true, false, false, false) => kernel_tf_f_f,
        (true, false, false, true) => kernel_tf_f_t,
        (true, false, true, false) => kernel_tf_t_f,
        (true, false, true, true) => kernel_tf_t_t,
        (true, true, false, false) => kernel_tt_f_f,
        (true, true, false, true) => kernel_tt_f_t,
        (true, true, true, false) => kernel_tt_t_f,
        (true, true, true, true) => kernel_tt_t_t,
    }
}

#[cfg(test)]
#[path = "raster_kernel_test.rs"]
mod tests;
