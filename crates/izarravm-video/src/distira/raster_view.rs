// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The pixel pipeline, driven by a SNAPSHOT of the device registers rather
//! than by the live device.
//!
//! Distira defers a guest triangle onto a queue and rasterises it later, so
//! the pipeline cannot read the registers through the device: by the time a
//! triangle is drawn the guest has usually moved the mode registers on to the
//! next one. [`RasterParams`] is the copy taken when the triangle is
//! submitted, the way 86Box copies `voodoo_params_t` in
//! `voodoo_queue_triangle`. [`RasterView`] pairs that copy with the memories
//! the pipeline reads and writes live: the frame store, texture memory, and
//! the NCC tables and palette.
//!
//! Texture memory, the NCC tables and the palette are deliberately NOT part
//! of the snapshot. They are large, and a game uploads textures at level load
//! rather than between triangles, so Distira drains the queue before every
//! texture-aperture write and every NCC or palette register write instead of
//! copying them. See `Distira::register_write_defers_raster`.

use super::*;

/// The register state one triangle rasterises against. Copied out of the
/// device when the triangle is submitted, so later register writes cannot
/// reach a triangle that has not been drawn yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RasterParams {
    pub(super) display: DistiraDisplay,
    pub(super) aux_base: u32,
    pub(super) dither_enabled: bool,
    pub(super) fbz_mode: u32,
    pub(super) fbz_color_path: u32,
    pub(super) alpha_mode: u32,
    pub(super) fog_mode: u32,
    pub(super) fog_color: u32,
    pub(super) za_color: u32,
    pub(super) chroma_key: u32,
    pub(super) color0: u32,
    pub(super) color1: u32,
    /// The stipple PATTERN. The rotating stipple is a serial dependency
    /// between triangles, so a triangle that uses it never reaches the queue;
    /// see `Distira::triangle_defers`.
    pub(super) stipple: u32,
    pub(super) texture_mode: u32,
    pub(super) texture_mode_tmu1: u32,
    pub(super) texture_lod: u32,
    pub(super) texture_lod_tmu1: u32,
    pub(super) texture_detail: u32,
    pub(super) texture_detail_tmu1: u32,
    pub(super) tex_base_addr: u32,
    pub(super) tex_base_addr_tmu1: u32,
    pub(super) tex_base_addr1: [u32; 2],
    pub(super) tex_base_addr2: [u32; 2],
    pub(super) tex_base_addr38: [u32; 2],
    pub(super) trex_init1: [u32; 2],
}

/// One triangle's snapshot, plus the memories the pipeline reads live.
///
/// The `Deref` is what keeps the pipeline bodies unchanged: `self.fbz_mode`
/// resolves through it to the snapshot, and `self.fb`, `self.texture` and
/// `self.ncc` are the live memories.
pub(super) struct RasterView<'a> {
    pub(super) params: RasterParams,
    pub(super) fb: &'a FrameStore,
    pub(super) texture: &'a [Vec<u8>; 2],
    pub(super) ncc: &'a NccState,
}

impl std::ops::Deref for RasterView<'_> {
    type Target = RasterParams;

    fn deref(&self) -> &Self::Target {
        &self.params
    }
}

impl RasterParams {
    /// The stipple test against a caller-owned rotating state, so each
    /// raster lane rotates its own copy.
    pub(super) fn stipple_test(&self, stipple: &mut u32, x: u32, y: u32) -> bool {
        if self.fbz_mode & FBZ_STIPPLE == 0 {
            return true;
        }
        if self.fbz_mode & FBZ_STIPPLE_PATT != 0 {
            *stipple & (1 << (((y & 3) << 3) | ((!x) & 7))) != 0
        } else {
            *stipple = stipple.rotate_left(1);
            *stipple & 0x8000_0000 != 0
        }
    }

    pub(super) fn biased_triangle_depth(&self, raw: f32) -> u16 {
        let depth = i32::from(depth_to_u16(raw));
        if self.fbz_mode & FBZ_DEPTH_BIAS == 0 {
            return depth as u16;
        }
        let bias = i32::from(self.za_color as u16 as i16);
        (depth + bias).clamp(0, i32::from(u16::MAX)) as u16
    }

    pub(super) fn draw_y(&self, logical_y: u32) -> u32 {
        if self.fbz_mode & FBZ_Y_ORIGIN == 0 {
            logical_y
        } else {
            self.display
                .height
                .saturating_sub(1)
                .saturating_sub(logical_y)
        }
    }

    pub(super) fn alpha_test_passes(&self, alpha: u8) -> bool {
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

    pub(super) fn chroma_key_passes(&self, r: u8, g: u8, b: u8) -> bool {
        self.fbz_mode & FBZ_CHROMAKEY == 0
            || r != (self.chroma_key >> 16) as u8
            || g != (self.chroma_key >> 8) as u8
            || b != self.chroma_key as u8
    }

    pub(super) fn apply_color_path_local_combine(
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

    pub(super) fn texture_detail_factor(&self, tmu: usize, lod: u32) -> u8 {
        let detail = self.texture_detail_for_tmu(tmu);
        let max = (detail & 0xff).min(0xff) as i32;
        let bias = ((detail >> 8) & 0x3f) as i32;
        let scale = (detail >> 14) & 0x7;
        ((bias - lod as i32) << scale).clamp(0, max).min(255) as u8
    }

    pub(super) fn texture_alpha_or_source(&self, alpha: u8, texture_alpha: u8) -> u8 {
        match (self.fbz_color_path >> FBZCP_A_SELECT_SHIFT) & FBZCP_A_SELECT_MASK {
            A_SELECT_TEX => texture_alpha,
            A_SELECT_COLOR1 => (self.color1 >> 24) as u8,
            _ => alpha,
        }
    }

    pub(super) fn alpha_local_source(&self, alpha: u8, depth_raw: Option<f32>) -> u8 {
        match (self.fbz_color_path >> FBZCP_CCA_LOCALSELECT_SHIFT) & FBZCP_CCA_LOCALSELECT_MASK {
            CCA_LOCALSELECT_COLOR0 => (self.color0 >> 24) as u8,
            CCA_LOCALSELECT_ITER_Z => depth_raw.map_or(0, fixed_depth_to_local_alpha),
            _ => alpha,
        }
    }

    pub(super) fn texture_alpha_factor(&self, texture_alpha: u8) -> u8 {
        texture_alpha
    }

    pub(super) fn apply_alpha_path(&self, alocal: u8, aother: u8, texture_alpha: u8) -> u8 {
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

    pub(super) fn texture_mode_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.texture_mode
        } else {
            self.texture_mode_tmu1
        }
    }

    pub(super) fn texture_lod_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.texture_lod
        } else {
            self.texture_lod_tmu1
        }
    }

    pub(super) fn texture_detail_for_tmu(&self, tmu: usize) -> u32 {
        if tmu == 0 {
            self.texture_detail
        } else {
            self.texture_detail_tmu1
        }
    }

    pub(super) fn tex_base_addr_for_tmu(&self, tmu: usize) -> u32 {
        let value = if tmu == 0 {
            self.tex_base_addr
        } else {
            self.tex_base_addr_tmu1
        };
        (value & 0x0007_ffff) << 3
    }

    pub(super) fn tex_base_addr_for_tmu_lod(&self, tmu: usize, lod: u32) -> u32 {
        let lod_reg = self.texture_lod_for_tmu(tmu);
        match texture_base_slot(lod_reg, lod) {
            0 => self.tex_base_addr_for_tmu(tmu),
            1 => (self.tex_base_addr1[tmu] & 0x0007_ffff) << 3,
            2 => (self.tex_base_addr2[tmu] & 0x0007_ffff) << 3,
            _ => (self.tex_base_addr38[tmu] & 0x0007_ffff) << 3,
        }
    }

    pub(super) fn apply_fog_color(&self, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        if self.fog_mode & (FOG_ENABLE | FOG_CONSTANT) != (FOG_ENABLE | FOG_CONSTANT) {
            return (r, g, b);
        }
        (
            r.saturating_add((self.fog_color >> 16) as u8),
            g.saturating_add((self.fog_color >> 8) as u8),
            b.saturating_add(self.fog_color as u8),
        )
    }

    pub(super) fn framebuffer_pixel_offset(&self, base: u32, x: u32, y: u32) -> Option<usize> {
        let offset = u64::from(base)
            .checked_add(u64::from(y).checked_mul(u64::from(self.display.pitch))?)?
            .checked_add(u64::from(x).checked_mul(2)?)?;
        let offset = usize::try_from(offset).ok()?;
        // The frame store never changes size, so the constant keeps this
        // valid while the raster path holds the store outside `self`.
        (offset.checked_add(2)? <= DISTIRA_FB_SIZE).then_some(offset)
    }
}

impl RasterView<'_> {
    /// Rasterise one row of a triangle. Every frame-store access goes
    /// through the atomic store and every counter goes into the lane's
    /// stats, so any number of lanes can run disjoint rows at once.
    pub(super) fn raster_row(&self, context: &TriangleContext, y: u32, stats: &mut PixelStats) {
        let TriangleContext {
            vertices: [a, b, c],
            area,
            depths,
            texture,
            coverage,
            count_fbi_pixels,
            affine_lods,
            min_x,
            max_x,
            min_y: _,
            max_y: _,
        } = *context;
        {
            let (row_min_x, row_max_x) = if let Some(coverage) = coverage {
                let Some((span_min, span_max)) = coverage.scanline_span(y) else {
                    return;
                };
                let span_min = span_min.max(0) as u32;
                let span_max = span_max.max(-1).saturating_add(1) as u32;
                (min_x.max(span_min), max_x.min(span_max))
            } else {
                (min_x, max_x)
            };
            // Triangle-constant; it was a reciprocal per PIXEL.
            let inv_area = 1.0 / area;
            // Which TMUs the texture combine will read this triangle.
            // Constant per triangle; sampling the other one is thrown away.
            let tmu_need = self.tmu_need();
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
                    stats.fbi_pixels_in += 1;
                    stats.pixels_in += 1;
                }
                let draw_y = self.draw_y(y);
                if !self.stipple_test(&mut stats.stipple, x, draw_y) {
                    if count_fbi_pixels {
                        stats.reject_stipple += 1;
                    }
                    continue;
                }

                let l0 = w0 * inv_area;
                let l1 = w1 * inv_area;
                let l2 = w2 * inv_area;
                let depth_raw = depths.map(|depths| depths.at(l0, l1, l2));
                let depth = depth_raw.map(|raw| self.biased_triangle_depth(raw));
                if let Some(depth) = depth
                    && !self.depth_test_passes(x, draw_y, depth)
                {
                    stats.fbi_zfunc_fail += 1;
                    if count_fbi_pixels {
                        stats.reject_depth += 1;
                    }
                    continue;
                }

                let r = lerp_u8(a.r, b.r, c.r, l0, l1, l2);
                let g = lerp_u8(a.g, b.g, c.g, l0, l1, l2);
                let blue = lerp_u8(a.b, b.b, c.b, l0, l1, l2);
                let texture_samples = if let Some(texture) = texture {
                    texture.samples_masked(px, py, tmu_need)
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
                    if count_fbi_pixels {
                        stats.reject_alpha_mask += 1;
                    }
                    continue;
                }

                let selected = self.selected_color_or_source((x, draw_y), (r, g, blue), texture);
                if !self.chroma_key_passes(selected.0, selected.1, selected.2) {
                    stats.fbi_chroma_fail += 1;
                    if count_fbi_pixels {
                        stats.reject_chroma += 1;
                    }
                    continue;
                }
                let (r, g, blue) =
                    self.texture_color_or_source(selected, (r, g, blue), alocal, aother, texture);
                let alpha = self.apply_alpha_path(alocal, aother, texture_alpha);
                if !self.alpha_test_passes(alpha) {
                    stats.fbi_afunc_fail += 1;
                    if count_fbi_pixels {
                        stats.reject_alpha_test += 1;
                    }
                    continue;
                }
                if count_fbi_pixels {
                    stats.fbi_pixels_out += 1;
                    stats.pixels_out += 1;
                }
                let (r, g, blue) = self.apply_fog_color(r, g, blue);
                let (r, g, blue) = self.alpha_blend_color(x, draw_y, r, g, blue, alpha);
                let pixel = pack_rgb565_for_pixel(r, g, blue, x, draw_y, self.dither_enabled);
                let wrote_color = if depths.is_none() {
                    self.write_pixel_at_base(self.display.back_base, x, draw_y, pixel)
                } else if self.fbz_mode & FBZ_RGB_WMASK == 0 {
                    if count_fbi_pixels {
                        stats.reject_rgb_wmask += 1;
                    }
                    false
                } else {
                    let stored = self.write_draw_pixel(x, draw_y, pixel);
                    if count_fbi_pixels && !stored {
                        stats.reject_offscreen += 1;
                    }
                    stored
                };
                if count_fbi_pixels && wrote_color {
                    stats.color_written += 1;
                    if pixel != 0 {
                        stats.color_written_nonblack += 1;
                    }
                    let base = match self.fbz_mode & FBZ_DRAW_MASK {
                        FBZ_DRAW_FRONT => self.display.front_base,
                        _ => self.display.back_base,
                    };
                    if let Some(offset) = self.framebuffer_pixel_offset(base, x, draw_y) {
                        let offset = offset as u32;
                        stats.color_offset_min = stats.color_offset_min.min(offset);
                        stats.color_offset_max = stats.color_offset_max.max(offset);
                    }
                }
                let wrote_depth =
                    depth.is_some_and(|depth| self.write_depth_pixel(x, draw_y, depth));
                if count_fbi_pixels && wrote_depth {
                    stats.depth_written += 1;
                }
                if wrote_color || wrote_depth {
                    stats.written += 1;
                }
            }
        }
    }

    /// Which of the two TMUs `combined_texture` reads under the current
    /// texture mode, folding in whether texturing is enabled at all.
    /// Delegates the mode decode to `texture_combine_target`, the same
    /// function `combined_texture` matches on, so the two decisions cannot
    /// drift out of sync as the combine unit's mode space grows: any mode
    /// `combined_texture` would read from a given TMU is, by construction,
    /// a mode this returns `true` for.
    pub(super) fn tmu_need(&self) -> [bool; 2] {
        if self.fbz_color_path & FBZCP_TEXTURE_ENABLED == 0 {
            return [false, false];
        }
        match self.texture_combine_target() {
            TextureCombineTarget::Tmu0Only => [true, false],
            TextureCombineTarget::Tmu1Only => [false, true],
            TextureCombineTarget::Both => [true, true],
        }
    }

    pub(super) fn write_pixel_at_base(&self, base: u32, x: u32, y: u32, pixel: u16) -> bool {
        let Some(offset) = self.framebuffer_pixel_offset(base, x, y) else {
            return false;
        };
        self.fb.write_u16_le(offset, pixel)
    }

    pub(super) fn write_draw_pixel(&self, x: u32, y: u32, pixel: u16) -> bool {
        let base = match self.fbz_mode & FBZ_DRAW_MASK {
            FBZ_DRAW_FRONT => self.display.front_base,
            _ => self.display.back_base,
        };
        self.write_pixel_at_base(base, x, y, pixel)
    }

    pub(super) fn depth_test_passes(&self, x: u32, y: u32, depth: u16) -> bool {
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

    pub(super) fn sample_tmu_alpha(&self, tmu: usize, sample: TextureSample) -> u8 {
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

    pub(super) fn sample_tmu_texture(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
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

    pub(super) fn sample_tmu_rgb332(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
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

    pub(super) fn sample_tmu_yiq_ncc(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, sample);
        self.ncc_color(tmu, raw)
    }

    pub(super) fn sample_tmu_a8_yiq_ncc(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u16(tmu, sample) as u8;
        self.ncc_color(tmu, raw)
    }

    pub(super) fn ncc_color(&self, tmu: usize, raw: u8) -> (u8, u8, u8) {
        let table = usize::from(self.texture_mode_for_tmu(tmu) & TEXTUREMODE_TNCCSELECT != 0);
        self.ncc.color(tmu, table, raw)
    }

    pub(super) fn sample_tmu_a8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, sample);
        (raw, raw, raw)
    }

    pub(super) fn sample_tmu_ai44(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let intensity = expand4(self.sample_tmu_u8(tmu, sample));
        (intensity, intensity, intensity)
    }

    pub(super) fn sample_tmu_ai88(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let intensity = self.sample_tmu_u16(tmu, sample) as u8;
        (intensity, intensity, intensity)
    }

    pub(super) fn sample_tmu_pal8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self
            .ncc
            .palette(tmu, usize::from(self.sample_tmu_u8(tmu, sample)));
        ((raw >> 16) as u8, (raw >> 8) as u8, raw as u8)
    }

    pub(super) fn sample_tmu_apal8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_apal8(
            self.ncc
                .palette(tmu, usize::from(self.sample_tmu_u8(tmu, sample))),
        )
    }

    pub(super) fn sample_tmu_apal88(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let index = (self.sample_tmu_u16(tmu, sample) & 0xff) as usize;
        let raw = self.ncc.palette(tmu, index);
        ((raw >> 16) as u8, (raw >> 8) as u8, raw as u8)
    }

    pub(super) fn sample_tmu_argb8332(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_rgb332(self.sample_tmu_u16(tmu, sample) as u8)
    }

    pub(super) fn sample_tmu_argb1555(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_rgb555(self.sample_tmu_u16(tmu, sample))
    }

    pub(super) fn sample_tmu_argb4444(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        expand_rgb444(self.sample_tmu_u16(tmu, sample))
    }

    pub(super) fn sample_tmu_i8(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
        let raw = self.sample_tmu_u8(tmu, sample);
        (raw, raw, raw)
    }

    pub(super) fn sample_tmu_u8(&self, tmu: usize, sample: TextureSample) -> u8 {
        self.texture[tmu][self.tmu_u8_offset(tmu, sample)]
    }

    pub(super) fn tmu_u8_offset(&self, tmu: usize, sample: TextureSample) -> usize {
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

    pub(super) fn sample_tmu_u16(&self, tmu: usize, sample: TextureSample) -> u16 {
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

    pub(super) fn sample_tmu_rgb565(&self, tmu: usize, sample: TextureSample) -> (u8, u8, u8) {
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

    pub(super) fn sample_rgb565_texel(
        &self,
        s: i32,
        t: i32,
        sample: TmuTextureSample,
    ) -> (u8, u8, u8) {
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

    pub(super) fn alpha_blend_color(
        &self,
        x: u32,
        y: u32,
        r: u8,
        g: u8,
        b: u8,
        alpha: u8,
    ) -> (u8, u8, u8) {
        self.alpha_blend_color_at_base(self.display.back_base, (x, y), (r, g, b), alpha)
    }

    pub(super) fn alpha_blend_color_at_base(
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

    pub(super) fn read_back_pixel_rgb(&self, x: u32, y: u32) -> (u8, u8, u8) {
        self.read_pixel_rgb_at_base(self.display.back_base, x, y)
    }

    pub(super) fn read_pixel_rgb_at_base(&self, base: u32, x: u32, y: u32) -> (u8, u8, u8) {
        let raw = self
            .framebuffer_pixel_offset(base, x, y)
            .and_then(|offset| self.fb.read_u16_le(offset))
            .unwrap_or(0);
        (
            expand5(raw >> 11) as u8,
            expand6(raw >> 5) as u8,
            expand5(raw) as u8,
        )
    }

    pub(super) fn read_depth_pixel(&self, x: u32, y: u32) -> Option<u16> {
        self.framebuffer_pixel_offset(self.aux_base, x, y)
            .and_then(|offset| self.fb.read_u16_le(offset))
    }

    pub(super) fn write_depth_pixel(&self, x: u32, y: u32, depth: u16) -> bool {
        if self.fbz_mode & (FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK)
            != (FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK)
        {
            return false;
        }
        let Some(offset) = self.framebuffer_pixel_offset(self.aux_base, x, y) else {
            return false;
        };
        self.fb.write_u16_le(offset, depth)
    }
}
