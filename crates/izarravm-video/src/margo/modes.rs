// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

/// Ordered 4x4 Bayer matrix (cells 0..15) for hardware dithering (section 7.10).
pub(super) const BAYER_4X4: [[u32; 4]; 4] =
    [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbeMode {
    pub number: u16,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

/// The modes Margo lists, reports, and sets. Includes 8-bit indexed modes and
/// 15bpp, 16bpp, and 32bpp direct-color modes.
pub const MARGO_VBE_MODES: &[VbeMode] = &[
    VbeMode {
        number: 0x100,
        width: 640,
        height: 400,
        bpp: 8,
    },
    VbeMode {
        number: 0x101,
        width: 640,
        height: 480,
        bpp: 8,
    },
    // Proprietary VEGA/Margo OEM mode: 320x240x256, line-doubled to the display
    // by the monitor/scaler. Used by the Izarra-BIOS graphical POST screen.
    VbeMode {
        number: 0x150,
        width: 320,
        height: 240,
        bpp: 8,
    },
    VbeMode {
        number: 0x103,
        width: 800,
        height: 600,
        bpp: 8,
    },
    VbeMode {
        number: 0x105,
        width: 1024,
        height: 768,
        bpp: 8,
    },
    VbeMode {
        number: 0x110,
        width: 640,
        height: 480,
        bpp: 15,
    },
    VbeMode {
        number: 0x111,
        width: 640,
        height: 480,
        bpp: 16,
    },
    VbeMode {
        number: 0x113,
        width: 800,
        height: 600,
        bpp: 15,
    },
    VbeMode {
        number: 0x114,
        width: 800,
        height: 600,
        bpp: 16,
    },
    VbeMode {
        number: 0x116,
        width: 1024,
        height: 768,
        bpp: 15,
    },
    VbeMode {
        number: 0x117,
        width: 1024,
        height: 768,
        bpp: 16,
    },
    VbeMode {
        number: 0x14a,
        width: 640,
        height: 480,
        bpp: 32,
    },
    VbeMode {
        number: 0x14c,
        width: 800,
        height: 600,
        bpp: 32,
    },
    VbeMode {
        number: 0x14e,
        width: 1024,
        height: 768,
        bpp: 32,
    },
];

pub fn vbe_mode(number: u16) -> Option<VbeMode> {
    MARGO_VBE_MODES
        .iter()
        .copied()
        .find(|mode| mode.number == number)
}

/// Bytes a pixel of `bpp` occupies in the frame store: 8->1, 15->2, 16->2,
/// 32->4. The 15bpp case is why this is not `bpp / 8`.
pub fn bytes_per_pixel(bpp: u32) -> u32 {
    bpp.div_ceil(8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub pos: u32,  // bit position of the low bit
    pub size: u32, // bit width
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub r: Channel,
    pub g: Channel,
    pub b: Channel,
    pub x: Channel, // unused/reserved bits; size 0 when none
}

/// Direct-color layout for `bpp`. 8bpp is indexed (palette), not a direct-color
/// format, so it returns None, as do depths outside the mode table.
pub fn pixel_format(bpp: u32) -> Option<PixelFormat> {
    match bpp {
        15 => Some(PixelFormat {
            r: Channel { pos: 10, size: 5 },
            g: Channel { pos: 5, size: 5 },
            b: Channel { pos: 0, size: 5 },
            x: Channel { pos: 15, size: 1 },
        }),
        16 => Some(PixelFormat {
            r: Channel { pos: 11, size: 5 },
            g: Channel { pos: 5, size: 6 },
            b: Channel { pos: 0, size: 5 },
            x: Channel { pos: 0, size: 0 },
        }),
        32 => Some(PixelFormat {
            r: Channel { pos: 16, size: 8 },
            g: Channel { pos: 8, size: 8 },
            b: Channel { pos: 0, size: 8 },
            x: Channel { pos: 24, size: 8 },
        }),
        _ => None,
    }
}

/// Expand a `size`-bit color component to 8 bits by replicating the high bits
/// into the low ones. Only called with size 5, 6, or 8 (the R/G/B widths here);
/// the `2 * size - 8` shift assumes size >= 4.
fn expand_to_8(value: u32, size: u32) -> u32 {
    if size >= 8 {
        return value & 0xff;
    }
    debug_assert!(
        size >= 4,
        "expand_to_8: size {size} below 4 underflows the replicate shift"
    );
    let v = value & ((1 << size) - 1);
    (v << (8 - size)) | (v >> (2 * size - 8))
}

/// Decode one scanout pixel to host ARGB `0x00RRGGBB`. `bpp` selects the format,
/// `raw` is the little-endian pixel value already assembled from 1/2/4 bytes,
/// and `palette` resolves 8-bit indices. Unknown depths decode to black.
pub(super) fn decode_argb(bpp: u32, raw: u32, palette: &[u32; 256]) -> u32 {
    if bpp == 8 {
        return palette[(raw & 0xff) as usize];
    }
    let Some(fmt) = pixel_format(bpp) else {
        return 0;
    };
    // expand_to_8 masks to `size` bits, so the raw shift needs no extra mask.
    let r = expand_to_8(raw >> fmt.r.pos, fmt.r.size);
    let g = expand_to_8(raw >> fmt.g.pos, fmt.g.size);
    let b = expand_to_8(raw >> fmt.b.pos, fmt.b.size);
    (r << 16) | (g << 8) | b
}

/// Convert one YUV triple to host ARGB 0x00RRGGBB by studio-swing ITU-R BT.601
/// (Y 16..=235, chroma 16..=240), the canonical integer coefficients. Rounds with
/// a +128 bias then an arithmetic shift, and clamps each channel to 0..=255. This
/// is the overlay's conversion (section 7.8); it produces ARGB directly rather
/// than going through `decode_argb`.
pub(super) fn yuv_to_argb(y: u8, u: u8, v: u8) -> u32 {
    let c = y as i32 - 16;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clamp = |x: i32| x.clamp(0, 255) as u32;
    let r = clamp((298 * c + 409 * e + 128) >> 8);
    let g = clamp((298 * c - 100 * d - 208 * e + 128) >> 8);
    let b = clamp((298 * c + 516 * d + 128) >> 8);
    (r << 16) | (g << 8) | b
}

/// Reduce one 8-bit channel `v` to `bits` (5 or 6) for a 15/16-bit display, then
/// bit-replicate back to 8 bits (matching `expand_to_8`, so the host sees exactly
/// what an N-bit DAC would show). With `dither` off the channel is plainly
/// truncated (the value the primary surface stores). With it on, the ordered 4x4
/// Bayer cell offset (scaled to the dropped low bits) is added before truncating,
/// spreading the quantization error spatially. `bits` is 5 or 6 here.
pub(super) fn quantize_channel(v: u32, bits: u32, cell: u32, dither: bool) -> u32 {
    let shift = 8 - bits;
    let offset = if dither { (cell << shift) / 16 } else { 0 };
    let code = (v + offset).min(255) >> shift;
    (code << shift) | (code >> (2 * bits - 8))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MargoDisplay {
    pub mode: u16,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub pitch: u32,
    pub start: u32,
}
