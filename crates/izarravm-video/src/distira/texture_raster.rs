// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CHIP_FBI, CHIP_TREX0, CHIP_TREX1, LOD_ODD, LOD_S_IS_WIDER, LOD_SPLIT, LOD_TMULTIBASEADDR,
    SST_DS_DX, SST_DS_DY, SST_DT_DX, SST_DT_DY, SST_DW_DX, SST_DW_DY, SST_FDS_DX, SST_FDS_DY,
    SST_FDT_DX, SST_FDT_DY, SST_FDW_DX, SST_FDW_DY, SST_FSTART_S, SST_FSTART_T, SST_FSTART_W,
    SST_START_S, SST_START_T, SST_START_W, TEXTUREMODE_TCLAMPW, TEXTUREMODE_TPERSP_ST, merge_byte,
};

const S: usize = 0;
const T: usize = 1;
const W: usize = 2;
const START: usize = 0;
const DX: usize = 1;
const DY: usize = 2;
const INTERNAL_SCALE: f64 = (1_u64 << 32) as f64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RegisterPlane {
    values: [u32; 3],
}

impl RegisterPlane {
    fn slot_mut(&mut self, term: usize) -> &mut u32 {
        &mut self.values[term]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RegisterSet {
    components: [RegisterPlane; 3],
}

impl RegisterSet {
    fn slot_mut(&mut self, component: usize, term: usize) -> &mut u32 {
        self.components[component].slot_mut(term)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct IteratorPlane {
    values: [i64; 3],
}

impl IteratorPlane {
    fn slot_mut(&mut self, term: usize) -> &mut i64 {
        &mut self.values[term]
    }

    fn as_f64(self) -> RasterPlane {
        RasterPlane {
            start: self.values[START] as f64 / INTERNAL_SCALE,
            dx: self.values[DX] as f64 / INTERNAL_SCALE,
            dy: self.values[DY] as f64 / INTERNAL_SCALE,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TmuIterators {
    components: [IteratorPlane; 3],
}

impl TmuIterators {
    fn slot_mut(&mut self, component: usize, term: usize) -> &mut i64 {
        self.components[component].slot_mut(term)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TextureIteratorState {
    tmu: [TmuIterators; 2],
    fbi_w: IteratorPlane,
    fixed_tmu: [RegisterSet; 2],
    fixed_fbi_w: RegisterPlane,
    float_tmu: [RegisterSet; 2],
    float_fbi_w: RegisterPlane,
}

impl TextureIteratorState {
    pub(super) fn write_register(
        &mut self,
        chip: usize,
        register: usize,
        byte: usize,
        value: u8,
    ) -> bool {
        let Some((floating, term, component)) = decode_register(register) else {
            return false;
        };
        if component == W && chip & CHIP_FBI != 0 {
            self.write_fbi_w(floating, term, byte, value);
        }
        if chip & CHIP_TREX0 != 0 {
            self.write_tmu(0, floating, term, component, byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            self.write_tmu(1, floating, term, component, byte, value);
        }
        true
    }

    pub(super) fn fbi_w_at(&self, x: f32, y: f32, origin_x: f32, origin_y: f32) -> f32 {
        self.fbi_w
            .as_f64()
            .at(x as f64, y as f64, origin_x as f64, origin_y as f64) as f32
    }

    pub(super) fn raster(
        &self,
        texture_modes: [u32; 2],
        texture_lods: [u32; 2],
        origin: (f32, f32),
    ) -> TextureRaster {
        TextureRaster {
            tmu: std::array::from_fn(|tmu| {
                TmuRaster::new(self.tmu[tmu], texture_modes[tmu], texture_lods[tmu], origin)
            }),
        }
    }

    fn write_tmu(
        &mut self,
        tmu: usize,
        floating: bool,
        term: usize,
        component: usize,
        byte: usize,
        value: u8,
    ) {
        let raw = if floating {
            let slot = self.float_tmu[tmu].slot_mut(component, term);
            merge_byte(slot, byte, value);
            *slot
        } else {
            let slot = self.fixed_tmu[tmu].slot_mut(component, term);
            merge_byte(slot, byte, value);
            *slot
        };
        *self.tmu[tmu].slot_mut(component, term) = if floating {
            float_to_internal(raw)
        } else {
            fixed_to_internal(raw, component)
        };
    }

    fn write_fbi_w(&mut self, floating: bool, term: usize, byte: usize, value: u8) {
        let raw = if floating {
            let slot = self.float_fbi_w.slot_mut(term);
            merge_byte(slot, byte, value);
            *slot
        } else {
            let slot = self.fixed_fbi_w.slot_mut(term);
            merge_byte(slot, byte, value);
            *slot
        };
        *self.fbi_w.slot_mut(term) = if floating {
            float_to_internal(raw)
        } else {
            fixed_to_internal(raw, W)
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TextureSample {
    pub(super) s: f32,
    pub(super) t: f32,
    pub(super) lod: u32,
    pub(super) lod_floor: u32,
    pub(super) lod_fraction: u8,
}

impl TextureSample {
    /// The placeholder for a TMU the texture combine never reads. Its
    /// `lod` fields do not feed any visible output; only its presence in
    /// the `[TextureSample; 2]` pair matters, and `combined_texture` never
    /// indexes the slot `tmu_need` marked unused.
    pub(super) const UNUSED: Self = Self {
        s: 0.0,
        t: 0.0,
        lod: 0,
        lod_floor: 0,
        lod_fraction: 0,
    };

    pub(super) fn affine(s: f32, t: f32, texture_lod: u32) -> Self {
        let lod = select_lod(f64::NEG_INFINITY, 1.0, 0, texture_lod);
        Self {
            s,
            t,
            lod: lod.physical,
            lod_floor: lod.floor,
            lod_fraction: lod.fraction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextureLod {
    physical: u32,
    floor: u32,
    fraction: u8,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TextureRaster {
    tmu: [TmuRaster; 2],
}

impl TextureRaster {
    /// Sample only the TMUs the texture combine will actually read.
    /// `need` comes from `RasterView::tmu_need`, which mirrors
    /// `combined_texture`'s own branching, so the unread slot is a
    /// placeholder that `combined_texture` never looks at. `need = [true,
    /// true]` samples both unconditionally, which is what every triangle
    /// did before this hoist and is what the equivalence tests in
    /// `texture_raster_test.rs` use as the oracle.
    pub(super) fn samples_masked(self, x: f32, y: f32, need: [bool; 2]) -> [TextureSample; 2] {
        [
            if need[0] {
                self.tmu[0].sample(x, y)
            } else {
                TextureSample::UNUSED
            },
            if need[1] {
                self.tmu[1].sample(x, y)
            } else {
                TextureSample::UNUSED
            },
        ]
    }
}

#[derive(Debug, Clone, Copy)]
struct TmuRaster {
    s_over_w: RasterPlane,
    t_over_w: RasterPlane,
    reciprocal_w: RasterPlane,
    origin: (f64, f64),
    texture_mode: u32,
    texture_lod: u32,
    base_lod: f64,
    /// Triangle-constant halves of `select_lod`, derived once from
    /// `texture_lod` here instead of at every pixel.
    lod_min: f64,
    lod_max: f64,
    lod_bias: f64,
    lod_perspective: bool,
}

impl TmuRaster {
    fn new(
        iterators: TmuIterators,
        texture_mode: u32,
        texture_lod: u32,
        origin: (f32, f32),
    ) -> Self {
        let s_over_w = iterators.components[S].as_f64();
        let t_over_w = iterators.components[T].as_f64();
        let rho_x = s_over_w.dx.hypot(t_over_w.dx);
        let rho_y = s_over_w.dy.hypot(t_over_w.dy);
        let rho = rho_x.max(rho_y);
        Self {
            s_over_w,
            t_over_w,
            reciprocal_w: iterators.components[W].as_f64(),
            origin: (origin.0 as f64, origin.1 as f64),
            texture_mode,
            texture_lod,
            base_lod: if rho > 0.0 {
                rho.log2()
            } else {
                f64::NEG_INFINITY
            },
            lod_min: lod_min(texture_lod),
            lod_max: lod_max(texture_lod),
            lod_bias: lod_bias(texture_lod),
            lod_perspective: texture_mode & TEXTUREMODE_TPERSP_ST != 0,
        }
    }

    fn sample(self, x: f32, y: f32) -> TextureSample {
        let x = x as f64;
        let y = y as f64;
        let s_over_w = self.s_over_w.at(x, y, self.origin.0, self.origin.1);
        let t_over_w = self.t_over_w.at(x, y, self.origin.0, self.origin.1);
        let reciprocal_w = self.reciprocal_w.at(x, y, self.origin.0, self.origin.1);
        let (s, t) = if self.lod_perspective {
            if reciprocal_w == 0.0
                || self.texture_mode & TEXTUREMODE_TCLAMPW != 0 && reciprocal_w < 0.0
            {
                (0.0, 0.0)
            } else {
                (s_over_w / reciprocal_w, t_over_w / reciprocal_w)
            }
        } else {
            (s_over_w, t_over_w)
        };
        let lod = select_lod_hoisted(
            self.base_lod,
            reciprocal_w,
            self.lod_perspective,
            self.texture_lod,
            self.lod_min,
            self.lod_max,
            self.lod_bias,
        );
        TextureSample {
            s: s as f32,
            t: t as f32,
            lod: lod.physical,
            lod_floor: lod.floor,
            lod_fraction: lod.fraction,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RasterPlane {
    start: f64,
    dx: f64,
    dy: f64,
}

impl RasterPlane {
    fn at(self, x: f64, y: f64, origin_x: f64, origin_y: f64) -> f64 {
        self.start + self.dx * (x - origin_x) + self.dy * (y - origin_y)
    }
}

pub(super) fn texture_dimensions(texture_lod: u32, lod: u32) -> (usize, usize) {
    let aspect = ((texture_lod >> 21) & 0x3) as usize;
    let mut width = (256_usize >> lod).max(1);
    let mut height = (256_usize >> lod).max(1);
    if texture_lod & LOD_S_IS_WIDER != 0 {
        height = (height >> aspect).max(1);
    } else {
        width = (width >> aspect).max(1);
    }
    (width, height)
}

pub(super) fn texture_base_slot(texture_lod: u32, lod: u32) -> usize {
    if texture_lod & LOD_TMULTIBASEADDR == 0 {
        0
    } else {
        lod.min(3) as usize
    }
}

pub(super) fn texture_mip_offset(texture_lod: u32, lod: u32, bytes_per_texel: usize) -> usize {
    (0..lod)
        .filter(|&level| owns_lod(texture_lod, level))
        .map(|level| {
            let (width, height) = texture_dimensions(texture_lod, level);
            width
                .saturating_mul(height)
                .max(4)
                .saturating_mul(bytes_per_texel)
        })
        .sum()
}

fn lod_min(texture_lod: u32) -> f64 {
    (f64::from(texture_lod & 0x3f) / 4.0).min(8.0)
}

fn lod_max(texture_lod: u32) -> f64 {
    (f64::from((texture_lod >> 6) & 0x3f) / 4.0).min(8.0)
}

fn lod_bias(texture_lod: u32) -> f64 {
    let bias_raw = ((texture_lod >> 12) & 0x3f) as i32;
    f64::from(if bias_raw & 0x20 != 0 {
        bias_raw | !0x3f
    } else {
        bias_raw
    }) / 4.0
}

/// Selects a mip level for one sample. `texture_mode` and `texture_lod`
/// only ever contribute the four triangle-constant values computed below
/// (perspective flag, min, max, bias); `select_lod_hoisted` takes them
/// pre-derived so a triangle's raster loop computes them once instead of
/// per pixel.
fn select_lod(base_lod: f64, reciprocal_w: f64, texture_mode: u32, texture_lod: u32) -> TextureLod {
    select_lod_hoisted(
        base_lod,
        reciprocal_w,
        texture_mode & TEXTUREMODE_TPERSP_ST != 0,
        texture_lod,
        lod_min(texture_lod),
        lod_max(texture_lod),
        lod_bias(texture_lod),
    )
}

#[allow(clippy::too_many_arguments)]
fn select_lod_hoisted(
    base_lod: f64,
    reciprocal_w: f64,
    perspective: bool,
    texture_lod: u32,
    min: f64,
    max: f64,
    bias: f64,
) -> TextureLod {
    let perspective_adjust = if perspective && reciprocal_w > 0.0 {
        reciprocal_w.log2()
    } else {
        0.0
    };
    let lod = (base_lod - perspective_adjust + bias).max(min).min(max);
    let fixed = ((lod * 256.0).floor() as u32).min(8 << 8);
    let floor = fixed >> 8;
    let physical = if owns_lod(texture_lod, floor) {
        floor
    } else {
        floor.saturating_add(1).min(8)
    };
    TextureLod {
        physical,
        floor,
        fraction: fixed as u8,
    }
}

fn owns_lod(texture_lod: u32, lod: u32) -> bool {
    texture_lod & LOD_SPLIT == 0 || (lod & 1 != 0) == (texture_lod & LOD_ODD != 0)
}

fn fixed_to_internal(raw: u32, component: usize) -> i64 {
    let shift = if component == W { 2 } else { 14 };
    i64::from(raw as i32) << shift
}

fn float_to_internal(raw: u32) -> i64 {
    (f64::from(f32::from_bits(raw)) * INTERNAL_SCALE) as i64
}

fn decode_register(register: usize) -> Option<(bool, usize, usize)> {
    Some(match register {
        SST_START_S => (false, START, S),
        SST_START_T => (false, START, T),
        SST_START_W => (false, START, W),
        SST_DS_DX => (false, DX, S),
        SST_DT_DX => (false, DX, T),
        SST_DW_DX => (false, DX, W),
        SST_DS_DY => (false, DY, S),
        SST_DT_DY => (false, DY, T),
        SST_DW_DY => (false, DY, W),
        SST_FSTART_S => (true, START, S),
        SST_FSTART_T => (true, START, T),
        SST_FSTART_W => (true, START, W),
        SST_FDS_DX => (true, DX, S),
        SST_FDT_DX => (true, DX, T),
        SST_FDW_DX => (true, DX, W),
        SST_FDS_DY => (true, DY, S),
        SST_FDT_DY => (true, DY, T),
        SST_FDW_DY => (true, DY, W),
        _ => return None,
    })
}

#[cfg(test)]
#[path = "texture_raster_test.rs"]
mod tests;
