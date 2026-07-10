// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SstTriangleCoverage {
    vertices: [(i64, i64); 3],
    edge_slopes: [i64; 3],
    x_direction: i64,
}

impl SstTriangleCoverage {
    pub(super) fn new(vertices: [(u32, u32); 3], negative_direction: bool) -> Self {
        let vertices =
            vertices.map(|(x, y)| (i64::from(x as u16 as i16), i64::from(y as u16 as i16)));
        let edge_slope = |(x0, y0): (i64, i64), (x1, y1): (i64, i64)| {
            if y1 == y0 {
                0
            } else {
                (((x1 << 12) - (x0 << 12)) << 4) / (y1 - y0)
            }
        };
        Self {
            vertices,
            edge_slopes: [
                edge_slope(vertices[0], vertices[1]),
                edge_slope(vertices[0], vertices[2]),
                edge_slope(vertices[1], vertices[2]),
            ],
            x_direction: if negative_direction { -1 } else { 1 },
        }
    }

    pub(super) fn scanline_span(self, pixel_y: u32) -> Option<(i64, i64)> {
        let [(ax, ay), (bx, by), (_, cy)] = self.vertices;
        let y = i64::from(pixel_y);
        if y < (ay + 8) >> 4 || y >= (cy + 7) >> 4 {
            return None;
        }

        let [ab, ac, bc] = self.edge_slopes;
        let real_y = (y << 4) + 8;
        let mut long_x = (ax << 12) + ((ac * (real_y - ay)) >> 4);
        let mut other_x = if real_y < by {
            (ax << 12) + ((ab * (real_y - ay)) >> 4)
        } else {
            (bx << 12) + ((bc * (real_y - by)) >> 4)
        };
        if self.x_direction > 0 {
            other_x -= 1 << 16;
        } else {
            long_x -= 1 << 16;
        }
        let long_x = (long_x + 0x7000) >> 16;
        let other_x = (other_x + 0x7000) >> 16;
        if self.x_direction > 0 {
            Some((long_x, other_x))
        } else {
            Some((other_x, long_x))
        }
    }
}

pub(super) fn merge_byte(slot: &mut u32, byte: usize, value: u8) {
    let shift = byte * 8;
    *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
}

pub(super) fn depth_compare_passes(fbz_mode: u32, old_depth: u16, depth: u16) -> bool {
    match (fbz_mode >> FBZ_DEPTH_OP_SHIFT) & 7 {
        DEPTHOP_NEVER => false,
        DEPTHOP_LESSTHAN => depth < old_depth,
        DEPTHOP_EQUAL => depth == old_depth,
        DEPTHOP_LESSTHANEQUAL => depth <= old_depth,
        DEPTHOP_GREATERTHAN => depth > old_depth,
        DEPTHOP_NOTEQUAL => depth != old_depth,
        DEPTHOP_GREATERTHANEQUAL => depth >= old_depth,
        DEPTHOP_ALWAYS => true,
        _ => true,
    }
}

pub(super) fn tmu_chip_mask(offset: usize) -> usize {
    match (offset >> 10) & 0xf {
        0 => 0xf,
        chip => chip,
    }
}

pub(super) fn merge_vertex_component(slot: &mut u32, byte: usize, value: u8) {
    merge_byte(slot, byte, value);
    *slot &= 0xffff;
}

pub(super) fn merge_color_component(slot: &mut u32, byte: usize, value: u8) {
    merge_byte(slot, byte, value);
    *slot &= 0x00ff_ffff;
}

pub(super) fn fixed_vertex_to_f32(raw: u32) -> f32 {
    (raw as i16) as f32 / 16.0
}

pub(super) fn fixed_color_at(
    start: u32,
    dx: u32,
    dy: u32,
    x: f32,
    y: f32,
    origin_x: f32,
    origin_y: f32,
) -> u8 {
    (fixed_color_value(start)
        + fixed_color_value(dx) * (x - origin_x)
        + fixed_color_value(dy) * (y - origin_y))
        .round()
        .clamp(0.0, 255.0) as u8
}

fn fixed_color_value(raw: u32) -> f32 {
    sign_extend_24(raw) as f32 / 4096.0
}

/// Convert an iterated 1/w value to the SST-1 W-buffer's 16-bit floating
/// point depth code. This ports the *behavior* of 86Box's `vid_voodoo_render.c`
/// wfloat encode (itself the real SST-1 hardware algorithm: a
/// leading-zero-count exponent plus a 12-bit inverted mantissa, producing a
/// code where a larger 1/w — i.e. a nearer vertex — yields a SMALLER code,
/// the same "smaller code wins under DEPTHOP_LESSTHAN" convention the
/// fixed-point Z path already uses). 86Box represents the iterated W as a
/// 48-bit `.32` fixed-point accumulator (`state->w`, built from
/// `startW = w_float * 2^32`) and looks at bits 16-47; this port takes the
/// interpolated `f32` W value this codebase already produces (the same wire
/// value `SST_START_W`/`SST_DW_DX`/`SST_DW_DY` carry for texture
/// perspective) and reconstructs the equivalent 48-bit fixed value before
/// running the identical exponent/mantissa extraction, so the two
/// implementations agree bit-for-bit on any representable input.
pub(super) fn wfloat_depth(w: f32) -> u16 {
    if !w.is_finite() || w <= 0.0 {
        return 0; // Non-positive/non-finite 1/w: treat as "at infinity", code 0.
    }
    // Reconstruct 86Box's `state->w`: a 48-bit-significant `.32` fixed-point
    // value of the float 1/w, clamped into range rather than wrapping.
    let fixed = (f64::from(w) * 4294967296.0).clamp(0.0, u64::MAX as f64) as u64;
    if fixed & 0xffff_0000_0000 != 0 {
        // Bits 32-47 set: 1/w overflowed the representable range (too far).
        return 0;
    }
    let upper16 = ((fixed >> 16) & 0xffff) as u16;
    if upper16 == 0 {
        // Bits 16-31 all clear: 1/w is too large (too near) to represent.
        return 0xf001;
    }
    // voodoo_fls: count of leading zero bits in the 16-bit value (0..=15
    // here, since upper16 != 0).
    let exp = upper16.leading_zeros();
    let mant = ((!fixed as u32) >> (19 - exp)) & 0xfff;
    let code = (exp << 12) + mant + 1;
    code.min(0xffff) as u16
}

pub(super) fn fixed_depth_at(
    start: u32,
    dx: u32,
    dy: u32,
    x: f32,
    y: f32,
    origin_x: f32,
    origin_y: f32,
) -> f32 {
    start as f32 + dx as i32 as f32 * (x - origin_x) + dy as i32 as f32 * (y - origin_y)
}

pub(super) fn depth_to_u16(raw: f32) -> u16 {
    (raw / 4096.0).round().clamp(0.0, 65535.0) as u16
}

pub(super) fn fixed_depth_to_local_alpha(raw: f32) -> u8 {
    ((raw as i64) >> 20).clamp(0, 255) as u8
}

fn sign_extend_24(raw: u32) -> i32 {
    let raw = raw & 0x00ff_ffff;
    if raw & 0x0080_0000 != 0 {
        (raw | 0xff00_0000) as i32
    } else {
        raw as i32
    }
}

pub(super) fn float_vertex_to_fixed(raw: u32) -> u32 {
    ((f32::from_bits(raw) * 16.0) as i16 as u16).into()
}

pub(super) fn float_color_to_fixed(raw: u32) -> u32 {
    ((f32::from_bits(raw) * 4096.0) as i32 as u32) & 0x00ff_ffff
}

pub(super) fn float_depth_to_fixed(raw: u32) -> u32 {
    (f32::from_bits(raw) * 4096.0) as i32 as u32
}

pub(super) fn signed_ncc_component(raw: u32, shift: u32) -> i32 {
    let value = ((raw >> shift) & 0x1ff) as i32;
    if value & 0x100 != 0 {
        value | !0x1ff
    } else {
        value
    }
}

pub(super) fn clamp_ncc(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

pub(super) fn color_path_blend_component(channel: i32, factor: u8, reverse: bool) -> i32 {
    let factor = if reverse {
        i32::from(factor) + 1
    } else {
        i32::from((factor ^ 0xff).wrapping_add(1))
    };
    (channel * factor) >> 8
}

pub(super) fn texture_coord_index(coord: f32, size: usize, clamp: bool, mirror: bool) -> usize {
    texture_coord_index_i32(coord.floor() as i32, size, clamp, mirror)
}

pub(super) fn texture_coord_index_i32(coord: i32, size: usize, clamp: bool, mirror: bool) -> usize {
    if clamp {
        coord.clamp(0, size.saturating_sub(1) as i32) as usize
    } else if mirror {
        let period = (size * 2) as i32;
        let coord = coord.rem_euclid(period);
        if coord >= size as i32 {
            (period - 1 - coord) as usize
        } else {
            coord as usize
        }
    } else {
        coord as usize & (size - 1)
    }
}

fn expand3(v: u8) -> u8 {
    let v = u32::from(v & 0x07);
    ((v << 5) | (v << 2) | (v >> 1)) as u8
}

fn expand2(v: u8) -> u8 {
    let v = u32::from(v & 0x03);
    ((v << 6) | (v << 4) | (v << 2) | v) as u8
}

pub(super) fn expand4(v: u8) -> u8 {
    let v = u32::from(v & 0x0f);
    ((v << 4) | v) as u8
}

pub(super) fn expand_rgb332(raw: u8) -> (u8, u8, u8) {
    (expand3(raw >> 5), expand3(raw >> 2), expand2(raw))
}

pub(super) fn expand_apal8(raw: u32) -> (u8, u8, u8) {
    let r = (raw >> 16) as u8;
    let g = (raw >> 8) as u8;
    let b = raw as u8;
    (
        ((r & 3) << 6) | ((g & 0xf0) >> 2) | (r & 3),
        ((g & 0x0f) << 4) | ((b & 0xc0) >> 4) | ((g & 0x0f) >> 2),
        ((b & 0x3f) << 2) | ((b & 0x30) >> 4),
    )
}

pub(super) fn expand_rgb555(raw: u16) -> (u8, u8, u8) {
    (
        expand5(raw >> 10) as u8,
        expand5(raw >> 5) as u8,
        expand5(raw) as u8,
    )
}

pub(super) fn expand_rgb444(raw: u16) -> (u8, u8, u8) {
    (
        expand4((raw >> 8) as u8),
        expand4((raw >> 4) as u8),
        expand4(raw as u8),
    )
}

pub(super) fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

pub(super) fn lerp_u8(a: u8, b: u8, c: u8, w0: f32, w1: f32, w2: f32) -> u8 {
    (a as f32 * w0 + b as f32 * w1 + c as f32 * w2)
        .round()
        .clamp(0.0, 255.0) as u8
}

pub(super) fn lerp_f32(a: f32, b: f32, c: f32, w0: f32, w1: f32, w2: f32) -> f32 {
    a * w0 + b * w1 + c * w2
}

pub(super) fn alpha_blend_component(
    source_func: u32,
    dest_func: u32,
    source: u8,
    dest: u8,
    source_alpha: u8,
) -> u8 {
    (alpha_blend_source_channel(source_func, source, dest, source_alpha)
        + alpha_blend_dest_channel(dest_func, dest, source, source_alpha))
    .min(255) as u8
}

fn alpha_blend_source_channel(func: u32, source: u8, dest: u8, source_alpha: u8) -> u32 {
    let source = u32::from(source);
    let dest = u32::from(dest);
    let source_alpha = u32::from(source_alpha);
    match func {
        BLEND_AZERO => 0,
        BLEND_ASRC_ALPHA => source * source_alpha / 255,
        BLEND_A_COLOR => source * dest / 255,
        BLEND_ADST_ALPHA | BLEND_AONE => source,
        BLEND_AOMSRC_ALPHA => source * (255 - source_alpha) / 255,
        BLEND_AOM_COLOR => source * (255 - dest) / 255,
        BLEND_AOMDST_ALPHA | BLEND_ASATURATE => 0,
        _ => source,
    }
}

fn alpha_blend_dest_channel(func: u32, dest: u8, source: u8, source_alpha: u8) -> u32 {
    let dest = u32::from(dest);
    let source = u32::from(source);
    let source_alpha = u32::from(source_alpha);
    match func {
        BLEND_AZERO => 0,
        BLEND_ASRC_ALPHA => dest * source_alpha / 255,
        BLEND_A_COLOR => dest * source / 255,
        BLEND_ADST_ALPHA | BLEND_AONE => dest,
        BLEND_AOMSRC_ALPHA => dest * (255 - source_alpha) / 255,
        BLEND_AOM_COLOR => dest * (255 - source) / 255,
        BLEND_AOMDST_ALPHA | BLEND_ASATURATE => 0,
        _ => dest,
    }
}

fn quantize_channel(v: u8, bits: u32, cell: u32, dither: bool) -> u16 {
    let v = u32::from(v);
    if !dither {
        return u16::try_from(v >> (8 - bits)).unwrap();
    }
    let quantized = match bits {
        5 => ((v << 1) - (v >> 4) + (v >> 7) + cell) >> 4,
        6 => ((v << 2) - (v >> 4) + (v >> 6) + cell) >> 4,
        _ => unreachable!("RGB565 channels have five or six bits"),
    };
    u16::try_from(quantized).unwrap()
}

pub(super) fn pack_rgb565_for_pixel(r: u8, g: u8, b: u8, x: u32, y: u32, dither: bool) -> u16 {
    let cell = BAYER_4X4[(y & 3) as usize][(x & 3) as usize];
    let r = quantize_channel(r, 5, cell, dither);
    let g = quantize_channel(g, 6, cell, dither);
    let b = quantize_channel(b, 5, cell, dither);
    (r << 11) | (g << 5) | b
}

pub(super) fn pack_rgb565(r: u8, g: u8, b: u8) -> u16 {
    pack_rgb565_for_pixel(r, g, b, 0, 0, false)
}

pub(super) fn rgb555_to_rgb565(raw: u16) -> u16 {
    let r = ((raw >> 10) & 0x1f) << 11;
    let g5 = (raw >> 5) & 0x1f;
    let g = ((g5 << 1) | (g5 >> 4)) << 5;
    let b = raw & 0x1f;
    r | g | b
}

pub(super) fn argb1555_alpha(raw: u16) -> u8 {
    if raw & 0x8000 != 0 { 0xff } else { 0 }
}

pub(super) fn rgb565_components(raw: u16) -> (u8, u8, u8) {
    (
        expand5(raw >> 11) as u8,
        expand6(raw >> 5) as u8,
        expand5(raw) as u8,
    )
}

pub(super) fn expand5(v: u16) -> u32 {
    let v = u32::from(v & 0x1f);
    (v << 3) | (v >> 2)
}

pub(super) fn expand6(v: u16) -> u32 {
    let v = u32::from(v & 0x3f);
    (v << 2) | (v >> 4)
}

pub(super) fn rgb565_to_argb(raw: u16) -> u32 {
    let r = expand5(raw >> 11);
    let g = expand6(raw >> 5);
    let b = expand5(raw);
    (r << 16) | (g << 8) | b
}
