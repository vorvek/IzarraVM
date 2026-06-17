//! Margo, the VEGA 2D engine: the display register block, the linear frame
//! buffer, and the blit engine. The engine implements FILL, COPY, color expand,
//! and LINE, all with full ROP3 and rectangle clipping.

pub const MARGO_VRAM_SIZE: usize = 4 * 1024 * 1024;
pub const MARGO_MMIO_SIZE: usize = 0x0001_0000; // 64 KB register block
pub const MARGO_ID_VALUE: u32 = 0x4D47_0100; // 'M' 'G', version 1.00
pub const MARGO_CAPS_VALUE: u32 = 0x0000_007f; // bits 0 FILL, 1 COPY, 2 COLOR_EXPAND, 3 LINE, 4 ROP3, 5 CLIP, 6 COLORKEY

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbeMode {
    pub number: u16,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

/// The modes Margo lists, reports, and sets. Includes 8-bit indexed modes
/// (slice 2b) and hi-color/true-color modes (slice 8): 15bpp, 16bpp, and 32bpp.
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
    let v = value & ((1 << size) - 1);
    (v << (8 - size)) | (v >> (2 * size - 8))
}

/// Decode one scanout pixel to host ARGB `0x00RRGGBB`. `bpp` selects the format,
/// `raw` is the little-endian pixel value already assembled from 1/2/4 bytes,
/// and `palette` resolves 8-bit indices. Unknown depths decode to black.
fn decode_argb(bpp: u32, raw: u32, palette: &[u32; 256]) -> u32 {
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

pub const REG_ID: usize = 0x0000;
pub const REG_CAPS: usize = 0x0004;
pub const REG_STATUS: usize = 0x0008;
pub const REG_CONTROL: usize = 0x000c;
pub const REG_DISP_MODE: usize = 0x0010;
pub const REG_DISP_WIDTH: usize = 0x0014;
pub const REG_DISP_HEIGHT: usize = 0x0018;
pub const REG_DISP_BPP: usize = 0x001c;
pub const REG_DISP_PITCH: usize = 0x0020;
pub const REG_DISP_START: usize = 0x0024;

// Blit engine registers (section 7.3). All R/W; the engine reads the ones it
// needs when COMMAND fires. The block 0x100..0x150 is a flat R/W store.
pub const REG_DST_BASE: usize = 0x0100;
pub const REG_DST_PITCH: usize = 0x0104;
pub const REG_SRC_BASE: usize = 0x0108;
pub const REG_SRC_PITCH: usize = 0x010c;
pub const REG_DEPTH: usize = 0x0110;
pub const REG_DST_XY: usize = 0x0114;
pub const REG_SRC_XY: usize = 0x0118;
pub const REG_DIM: usize = 0x011c;
pub const REG_FG_COLOR: usize = 0x0120;
pub const REG_BG_COLOR: usize = 0x0124;
pub const REG_ROP: usize = 0x0128;
pub const REG_COLORKEY: usize = 0x012c;
pub const REG_FLAGS: usize = 0x0130;
pub const REG_CLIP_TL: usize = 0x0134;
pub const REG_CLIP_BR: usize = 0x0138;
pub const REG_LINE_START: usize = 0x013c;
pub const REG_LINE_END: usize = 0x0140;
pub const REG_COMMAND: usize = 0x0150;
pub const REG_MONO_DATA: usize = 0x0160;

const BLIT_BASE: usize = 0x0100;
const BLIT_REGS: usize = 20; // 0x100..0x150, twenty 32-bit slots; COMMAND at 0x150 is handled separately
const FILL_NS_PER_PIXEL: u64 = 5; // 200 Mpixels/s solid fill (section 1.1)
const COPY_NS_PER_PIXEL: u64 = 10; // 100 Mpixels/s screen-to-screen blit (section 1.1)
const EXPAND_NS_PER_PIXEL: u64 = 5; // 200 Mpixels/s color expand (section 1.1, fill class)
const LINE_NS_PER_PIXEL: u64 = 10; // 100 Mpixels/s, one pixel per clock (section 1.1)
const BLIT_SETUP_NS: u64 = 100; // fixed per-operation setup, shared by all blits

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MargoDisplay {
    pub mode: u16,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub pitch: u32,
    pub start: u32,
}

/// Evaluate an 8-bit ROP3 code: the boolean function of pattern P, source S, and
/// destination D, applied bitwise across the pixel value. Bit `4*P + 2*S + D` of
/// `rop` is the result for that input combination.
fn rop3(rop: u8, p: u32, s: u32, d: u32) -> u32 {
    let mut out = 0u32;
    if rop & 0x01 != 0 {
        out |= !p & !s & !d;
    }
    if rop & 0x02 != 0 {
        out |= !p & !s & d;
    }
    if rop & 0x04 != 0 {
        out |= !p & s & !d;
    }
    if rop & 0x08 != 0 {
        out |= !p & s & d;
    }
    if rop & 0x10 != 0 {
        out |= p & !s & !d;
    }
    if rop & 0x20 != 0 {
        out |= p & !s & d;
    }
    if rop & 0x40 != 0 {
        out |= p & s & !d;
    }
    if rop & 0x80 != 0 {
        out |= p & s & d;
    }
    out
}

/// Combine pattern P and source S with the destination pixel at `off` through the
/// ROP3 code `rop`, writing the low `depth` bytes (little-endian). The caller has
/// bounds-checked `[off, off + depth)`.
fn write_rop(vram: &mut [u8], off: usize, depth: usize, rop: u8, p: u32, s: u32) {
    let mut db = [0u8; 4];
    db[..depth].copy_from_slice(&vram[off..off + depth]);
    let d = u32::from_le_bytes(db);
    let result = rop3(rop, p, s, d).to_le_bytes();
    vram[off..off + depth].copy_from_slice(&result[..depth]);
}

/// The clip rectangle. `[x0, x1) x [y0, y1)`: top-left inclusive, bottom-right
/// exclusive (section 7.3). When disabled, `allows` is always true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Clip {
    enabled: bool,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Clip {
    fn allows(&self, x: u64, y: u64) -> bool {
        !self.enabled
            || (x >= self.x0 as u64
                && x < self.x1 as u64
                && y >= self.y0 as u64
                && y < self.y1 as u64)
    }
}

struct FillParams {
    dst_base: u32,
    dst_pitch: u32,
    depth: u32, // bytes per pixel: 1, 2, or 4
    dst_x: u32,
    dst_y: u32,
    width: u32,
    height: u32,
    fg_color: u32,
    rop: u8, // ROP3 code; P = FG_COLOR, no source (S = 0)
    clip: Clip,
}

/// Fill a rectangle in `vram` from the latched parameters, applying the ROP3 code
/// with P = `FG_COLOR` and S = 0 (FILL has no source, section 7.6). Returns the
/// number of pixels actually written (in bounds and inside the clip rectangle).
/// Off-store and clipped pixels are skipped, not wrapped (section 8). `depth`
/// outside {1, 2, 4} is a no-op. The loop is bounded to `vram.len()` considered
/// pixels and the offset math is u64-saturating, so a pathological DIM cannot
/// spin or overflow.
fn fill(vram: &mut [u8], p: &FillParams) -> u64 {
    if !matches!(p.depth, 1 | 2 | 4) {
        return 0;
    }
    let depth = p.depth as usize;
    let len = vram.len() as u64;
    let mut considered: u64 = 0;
    let mut written: u64 = 0;
    'rows: for row in 0..p.height {
        let y = p.dst_y as u64 + row as u64;
        for col in 0..p.width {
            if considered >= len {
                break 'rows;
            }
            considered += 1;
            let x = p.dst_x as u64 + col as u64;
            if !p.clip.allows(x, y) {
                continue;
            }
            let offset = (p.dst_base as u64)
                .saturating_add(y.saturating_mul(p.dst_pitch as u64))
                .saturating_add(x.saturating_mul(depth as u64));
            if offset.saturating_add(depth as u64) > len {
                continue;
            }
            written += 1;
            write_rop(vram, offset as usize, depth, p.rop, p.fg_color, 0);
        }
    }
    written
}

struct CopyParams {
    dst_base: u32,
    dst_pitch: u32,
    src_base: u32,
    src_pitch: u32,
    depth: u32, // bytes per pixel: 1, 2, or 4
    dst_x: u32,
    dst_y: u32,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    fg_color: u32, // pattern P for ROP3
    rop: u8,       // ROP3 code; S = source pixel
    colorkey: u32,
    colorkey_en: bool,
    clip: Clip,
}

/// Copy a source rectangle to a destination rectangle in `vram`, combining source
/// S, pattern P = `FG_COLOR`, and destination D through the ROP3 code. Returns the
/// number of pixels written (in bounds on both sides, inside the clip rectangle,
/// and not keyed out). Off-store, clipped, and keyed pixels are skipped, not
/// wrapped (section 8). `depth` outside {1, 2, 4} is a no-op. The loop is bounded
/// to `vram.len()` considered pixels and the offset math is u64-saturating.
/// Traversal direction is chosen from the coordinates so overlapping copies stay
/// correct (section 7.4).
fn copy(vram: &mut [u8], p: &CopyParams) -> u64 {
    if !matches!(p.depth, 1 | 2 | 4) {
        return 0;
    }
    let depth = p.depth as usize;
    let len = vram.len() as u64;
    let key = p.colorkey.to_le_bytes();
    let mut considered: u64 = 0;
    let mut written: u64 = 0;
    let row_rev = p.dst_y > p.src_y; // dest below source: copy bottom-to-top
    let col_rev = p.dst_x > p.src_x; // dest right of source: copy right-to-left
    'rows: for r in 0..p.height {
        let row = if row_rev { p.height - 1 - r } else { r };
        for c in 0..p.width {
            let col = if col_rev { p.width - 1 - c } else { c };
            if considered >= len {
                break 'rows;
            }
            considered += 1;
            let dest_x = p.dst_x as u64 + col as u64;
            let dest_y = p.dst_y as u64 + row as u64;
            if !p.clip.allows(dest_x, dest_y) {
                continue;
            }
            let src_off = (p.src_base as u64)
                .saturating_add((p.src_y as u64 + row as u64).saturating_mul(p.src_pitch as u64))
                .saturating_add((p.src_x as u64 + col as u64).saturating_mul(depth as u64));
            let dst_off = (p.dst_base as u64)
                .saturating_add(dest_y.saturating_mul(p.dst_pitch as u64))
                .saturating_add(dest_x.saturating_mul(depth as u64));
            if src_off.saturating_add(depth as u64) > len
                || dst_off.saturating_add(depth as u64) > len
            {
                continue;
            }
            let (src_off, dst_off) = (src_off as usize, dst_off as usize);
            let mut sb = [0u8; 4];
            sb[..depth].copy_from_slice(&vram[src_off..src_off + depth]);
            if p.colorkey_en && sb[..depth] == key[..depth] {
                continue;
            }
            let s = u32::from_le_bytes(sb);
            written += 1;
            write_rop(vram, dst_off, depth, p.rop, p.fg_color, s);
        }
    }
    written
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpandParams {
    dst_base: u32,
    dst_pitch: u32,
    depth: u32, // bytes per pixel: 1, 2, or 4
    dst_x: u32,
    dst_y: u32,
    width: u32,
    height: u32,
    fg_color: u32,
    bg_color: u32,
    transparent: bool, // EXPAND_TRANSPARENT: clear bits are skipped
    rop: u8,           // ROP3 code; S = expanded pixel (FG/BG), P = FG_COLOR
    clip: Clip,
}

/// Write one expanded destination pixel. `set` chooses the source S (FG for a set
/// bit, BG for a clear bit); a clear bit under EXPAND_TRANSPARENT is skipped. The
/// pixel is combined with pattern P = `FG_COLOR` and destination D through the
/// ROP3 code. A pixel outside the clip rectangle or the frame store is skipped,
/// not wrapped (section 8). Returns true if a pixel was written.
/// `p.depth` must be 1, 2, or 4; callers guard this before calling.
fn put_expand_pixel(vram: &mut [u8], p: &ExpandParams, x: u64, y: u64, set: bool) -> bool {
    if !set && p.transparent {
        return false;
    }
    if !p.clip.allows(x, y) {
        return false;
    }
    let depth = p.depth as usize;
    let s = if set { p.fg_color } else { p.bg_color };
    let off = (p.dst_base as u64)
        .saturating_add(y.saturating_mul(p.dst_pitch as u64))
        .saturating_add(x.saturating_mul(depth as u64));
    if off.saturating_add(depth as u64) > vram.len() as u64 {
        return false;
    }
    write_rop(vram, off as usize, depth, p.rop, p.fg_color, s);
    true
}

struct ExpandMemParams {
    common: ExpandParams,
    src_base: u32,
    src_pitch: u32,
    src_x: u32,
    src_y: u32,
}

/// Expand a 1-bpp source rectangle read from `vram` into a two-color destination
/// rectangle, also in `vram`. The source is most-significant-bit first within
/// each byte. A source byte or destination pixel outside the frame store is
/// skipped, not wrapped (section 8). `depth` outside {1, 2, 4} is a no-op. The
/// loop is bounded to `vram.len()` considered pixels and the offset math is
/// u64-saturating, so an adversarial rectangle cannot spin or overflow. Returns
/// the number of pixels written.
fn color_expand_mem(vram: &mut [u8], p: &ExpandMemParams) -> u64 {
    if !matches!(p.common.depth, 1 | 2 | 4) {
        return 0;
    }
    let len = vram.len() as u64;
    let mut considered: u64 = 0;
    let mut written: u64 = 0;
    'rows: for row in 0..p.common.height {
        for col in 0..p.common.width {
            if considered >= len {
                break 'rows;
            }
            considered += 1;
            let bit = p.src_x as u64 + col as u64;
            let src_off = (p.src_base as u64)
                .saturating_add((p.src_y as u64 + row as u64).saturating_mul(p.src_pitch as u64))
                .saturating_add(bit / 8);
            if src_off >= len {
                continue;
            }
            let set = vram[src_off as usize] & (0x80u8 >> ((bit % 8) as u32)) != 0;
            if put_expand_pixel(
                vram,
                &p.common,
                p.common.dst_x as u64 + col as u64,
                p.common.dst_y as u64 + row as u64,
                set,
            ) {
                written += 1;
            }
        }
    }
    written
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpandState {
    params: ExpandParams,
    words_per_row: u32,
    total_words: u64,
    words_received: u32,
    written: u64, // running count, charged to busy_ns when the stream completes
}

/// Expand one 32-bit MONO_DATA word at the position implied by `received` (the
/// count of words already consumed) and `words_per_row`. Bit 31 is the leftmost
/// pixel; columns at or past `width` are padding and are skipped. Returns the
/// number of pixels written by this word.
fn expand_word(
    vram: &mut [u8],
    p: &ExpandParams,
    words_per_row: u32,
    received: u32,
    word: u32,
) -> u64 {
    let row = received / words_per_row;
    let col_base = (received % words_per_row) * 32;
    let mut written: u64 = 0;
    for i in 0..32u32 {
        let col = col_base + i;
        if col >= p.width {
            break;
        }
        let set = word & (0x8000_0000u32 >> i) != 0;
        if put_expand_pixel(
            vram,
            p,
            p.dst_x as u64 + col as u64,
            p.dst_y as u64 + row as u64,
            set,
        ) {
            written += 1;
        }
    }
    written
}

struct LineParams {
    dst_base: u32,
    dst_pitch: u32,
    depth: u32, // bytes per pixel: 1, 2, or 4
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    fg_color: u32,
    rop: u8, // ROP3 code; P = FG_COLOR, no source (S = 0)
    clip: Clip,
}

/// Draw a line from `(x0, y0)` to `(x1, y1)` in `vram` with integer Bresenham.
/// Both endpoints are inclusive; a zero-length line plots one pixel. The ROP3
/// code is applied with P = `FG_COLOR` and S = 0 (LINE has no source). A pixel
/// outside the clip rectangle or the frame store is skipped, not wrapped
/// (section 8). `depth` outside {1, 2, 4} is a no-op. Coordinates must be
/// 16-bit (`run_line` supplies them as such), so the loop runs at most
/// `max(|dx|, |dy|) + 1 <= 65536` steps and cannot spin; the offset math is
/// u64-saturating so extreme `dst_base` / `dst_pitch` skip rather than overflow.
/// Returns the number of pixels written.
fn line(vram: &mut [u8], p: &LineParams) -> u64 {
    if !matches!(p.depth, 1 | 2 | 4) {
        return 0;
    }
    let depth = p.depth as usize;
    let len = vram.len() as u64;
    let (mut x, mut y) = (p.x0 as i64, p.y0 as i64);
    let (x1, y1) = (p.x1 as i64, p.y1 as i64);
    let dx = (x1 - x).abs();
    let dy = -(y1 - y).abs();
    let sx = if x < x1 { 1 } else { -1 };
    let sy = if y < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut written: u64 = 0;
    loop {
        if p.clip.allows(x as u64, y as u64) {
            let off = (p.dst_base as u64)
                .saturating_add((y as u64).saturating_mul(p.dst_pitch as u64))
                .saturating_add((x as u64).saturating_mul(depth as u64));
            if off.saturating_add(depth as u64) <= len {
                write_rop(vram, off as usize, depth, p.rop, p.fg_color, 0);
                written += 1;
            }
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    written
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Margo {
    vram: Vec<u8>,
    display: MargoDisplay,
    control: u32,
    blit: [u32; BLIT_REGS],
    command: u32,
    busy_ns: u64,
    expand: Option<ExpandState>,
    mono_data: u32,
}

impl Default for Margo {
    fn default() -> Self {
        Self {
            vram: vec![0; MARGO_VRAM_SIZE],
            display: MargoDisplay::default(),
            control: 0,
            blit: [0; BLIT_REGS],
            command: 0,
            busy_ns: 0,
            expand: None,
            mono_data: 0,
        }
    }
}

impl Margo {
    pub fn display(&self) -> MargoDisplay {
        self.display
    }

    /// Set the display to a VBE mode. Returns false for modes outside the table.
    pub fn set_mode(&mut self, number: u16) -> bool {
        let Some(mode) = vbe_mode(number) else {
            return false;
        };
        self.display = MargoDisplay {
            mode: mode.number,
            width: mode.width,
            height: mode.height,
            bpp: mode.bpp,
            pitch: mode.width * bytes_per_pixel(mode.bpp),
            start: 0,
        };
        true
    }

    pub fn set_mode_640x480x8(&mut self) {
        self.set_mode(0x101);
    }

    pub fn read_vram_u8(&self, offset: usize) -> u8 {
        self.vram.get(offset).copied().unwrap_or(0)
    }

    pub fn write_vram_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.vram.get_mut(offset) {
            *slot = value;
        }
    }

    pub fn vram(&self) -> &[u8] {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut [u8] {
        &mut self.vram
    }

    /// The visible scanout surface: `pitch * height` bytes starting at `start`.
    /// Returns an empty slice when no mode has been set (pitch or height is 0);
    /// callers reach this only when Margo is the active display, after a mode-set.
    pub fn visible_surface(&self) -> &[u8] {
        let start = (self.display.start as usize).min(self.vram.len());
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = (start + len).min(self.vram.len());
        &self.vram[start..end]
    }

    /// The visible surface decoded to host ARGB `0x00RRGGBB`, one entry per
    /// source pixel, `width * height` long. Empty when no mode is set. Reads are
    /// bounds-checked and default to 0, matching `visible_surface`.
    pub fn scanout_argb(&self, palette: &[u32; 256]) -> Vec<u32> {
        let width = self.display.width as usize;
        let height = self.display.height as usize;
        let pitch = self.display.pitch as usize;
        let bpp = self.display.bpp;
        let depth = bytes_per_pixel(bpp) as usize;
        let start = self.display.start as usize;
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let off = start + y * pitch + x * depth;
                let mut bytes = [0u8; 4];
                for (i, slot) in bytes.iter_mut().enumerate().take(depth) {
                    *slot = self.vram.get(off + i).copied().unwrap_or(0);
                }
                out.push(decode_argb(bpp, u32::from_le_bytes(bytes), palette));
            }
        }
        out
    }

    fn register_u32(&self, reg: usize) -> u32 {
        match reg {
            REG_ID => MARGO_ID_VALUE,
            REG_CAPS => MARGO_CAPS_VALUE,
            REG_STATUS => u32::from(self.busy_ns > 0 || self.expand.is_some()), // bit 0: BUSY
            REG_CONTROL => self.control,
            REG_DISP_MODE => u32::from(self.display.mode),
            REG_DISP_WIDTH => self.display.width,
            REG_DISP_HEIGHT => self.display.height,
            REG_DISP_BPP => self.display.bpp,
            REG_DISP_PITCH => self.display.pitch,
            REG_DISP_START => self.display.start,
            reg if (BLIT_BASE..BLIT_BASE + BLIT_REGS * 4).contains(&reg) => {
                self.blit[(reg - BLIT_BASE) / 4]
            }
            _ => 0,
        }
    }

    pub fn read_mmio_u8(&self, offset: usize) -> u8 {
        let reg = offset & !0x3;
        let byte = offset & 0x3;
        (self.register_u32(reg) >> (8 * byte)) as u8
    }

    pub fn write_mmio_u8(&mut self, offset: usize, value: u8) {
        let reg = offset & !0x3;
        let byte = offset & 0x3;
        let shift = 8 * byte;

        if reg == REG_COMMAND {
            self.command = (self.command & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            if byte == 3 {
                self.run_command();
            }
            return;
        }
        if reg == REG_MONO_DATA {
            self.mono_data = (self.mono_data & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            if byte == 3 {
                self.feed_mono_word(self.mono_data);
            }
            return;
        }
        if reg == REG_CONTROL {
            self.control = (self.control & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            if self.control & 0x1 != 0 {
                // RESET aborts the operation. It already completed, so this only
                // drops BUSY and any in-flight color-expand stream. Self-clearing.
                self.busy_ns = 0;
                self.expand = None;
                self.control &= !0x1;
            }
            return;
        }
        if (BLIT_BASE..BLIT_BASE + BLIT_REGS * 4).contains(&reg) {
            let slot = &mut self.blit[(reg - BLIT_BASE) / 4];
            *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            return;
        }
        if reg == REG_DISP_START {
            let slot = &mut self.display.start;
            *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
        }
    }

    fn blit_reg(&self, offset: usize) -> u32 {
        self.blit[(offset - BLIT_BASE) / 4]
    }

    fn build_clip(&self) -> Clip {
        let tl = self.blit_reg(REG_CLIP_TL);
        let br = self.blit_reg(REG_CLIP_BR);
        Clip {
            enabled: self.blit_reg(REG_FLAGS) & 0x2 != 0,
            x0: tl & 0xffff,
            y0: tl >> 16,
            x1: br & 0xffff,
            y1: br >> 16,
        }
    }

    fn run_command(&mut self) {
        // Any COMMAND write ends an in-flight COLOR_EXPAND_DATA stream; the
        // 0x03 arm below starts a fresh one.
        self.expand = None;
        match self.command & 0xff {
            0x01 => self.run_fill(),
            0x02 => self.run_copy(),
            0x03 => self.arm_expand_data(),
            0x04 => self.run_expand_mem(),
            0x05 => self.run_line(),
            _ => {}
        }
        self.command = 0;
    }

    fn run_fill(&mut self) {
        let dst_xy = self.blit_reg(REG_DST_XY);
        let dim = self.blit_reg(REG_DIM);
        let params = FillParams {
            dst_base: self.blit_reg(REG_DST_BASE),
            dst_pitch: self.blit_reg(REG_DST_PITCH),
            depth: self.blit_reg(REG_DEPTH),
            dst_x: dst_xy & 0xffff,
            dst_y: dst_xy >> 16,
            width: dim & 0xffff,
            height: dim >> 16,
            fg_color: self.blit_reg(REG_FG_COLOR),
            rop: self.blit_reg(REG_ROP) as u8,
            clip: self.build_clip(),
        };
        let pixels = fill(&mut self.vram, &params);
        self.busy_ns = BLIT_SETUP_NS + pixels * FILL_NS_PER_PIXEL;
    }

    fn run_copy(&mut self) {
        let dst_xy = self.blit_reg(REG_DST_XY);
        let src_xy = self.blit_reg(REG_SRC_XY);
        let dim = self.blit_reg(REG_DIM);
        let params = CopyParams {
            dst_base: self.blit_reg(REG_DST_BASE),
            dst_pitch: self.blit_reg(REG_DST_PITCH),
            src_base: self.blit_reg(REG_SRC_BASE),
            src_pitch: self.blit_reg(REG_SRC_PITCH),
            depth: self.blit_reg(REG_DEPTH),
            dst_x: dst_xy & 0xffff,
            dst_y: dst_xy >> 16,
            src_x: src_xy & 0xffff,
            src_y: src_xy >> 16,
            width: dim & 0xffff,
            height: dim >> 16,
            fg_color: self.blit_reg(REG_FG_COLOR),
            rop: self.blit_reg(REG_ROP) as u8,
            colorkey: self.blit_reg(REG_COLORKEY),
            colorkey_en: self.blit_reg(REG_FLAGS) & 0x1 != 0,
            clip: self.build_clip(),
        };
        let pixels = copy(&mut self.vram, &params);
        self.busy_ns = BLIT_SETUP_NS + pixels * COPY_NS_PER_PIXEL;
    }

    fn run_expand_mem(&mut self) {
        let dst_xy = self.blit_reg(REG_DST_XY);
        let src_xy = self.blit_reg(REG_SRC_XY);
        let dim = self.blit_reg(REG_DIM);
        let params = ExpandMemParams {
            common: ExpandParams {
                dst_base: self.blit_reg(REG_DST_BASE),
                dst_pitch: self.blit_reg(REG_DST_PITCH),
                depth: self.blit_reg(REG_DEPTH),
                dst_x: dst_xy & 0xffff,
                dst_y: dst_xy >> 16,
                width: dim & 0xffff,
                height: dim >> 16,
                fg_color: self.blit_reg(REG_FG_COLOR),
                bg_color: self.blit_reg(REG_BG_COLOR),
                transparent: self.blit_reg(REG_FLAGS) & 0x4 != 0,
                rop: self.blit_reg(REG_ROP) as u8,
                clip: self.build_clip(),
            },
            src_base: self.blit_reg(REG_SRC_BASE),
            src_pitch: self.blit_reg(REG_SRC_PITCH),
            src_x: src_xy & 0xffff,
            src_y: src_xy >> 16,
        };
        let pixels = color_expand_mem(&mut self.vram, &params);
        self.busy_ns = BLIT_SETUP_NS + pixels * EXPAND_NS_PER_PIXEL;
    }

    fn run_line(&mut self) {
        let start = self.blit_reg(REG_LINE_START);
        let end = self.blit_reg(REG_LINE_END);
        let params = LineParams {
            dst_base: self.blit_reg(REG_DST_BASE),
            dst_pitch: self.blit_reg(REG_DST_PITCH),
            depth: self.blit_reg(REG_DEPTH),
            x0: start & 0xffff,
            y0: start >> 16,
            x1: end & 0xffff,
            y1: end >> 16,
            fg_color: self.blit_reg(REG_FG_COLOR),
            rop: self.blit_reg(REG_ROP) as u8,
            clip: self.build_clip(),
        };
        let pixels = line(&mut self.vram, &params);
        self.busy_ns = BLIT_SETUP_NS + pixels * LINE_NS_PER_PIXEL;
    }

    fn arm_expand_data(&mut self) {
        let depth = self.blit_reg(REG_DEPTH);
        if !matches!(depth, 1 | 2 | 4) {
            return; // invalid pixel size: do not arm
        }
        let dst_xy = self.blit_reg(REG_DST_XY);
        let dim = self.blit_reg(REG_DIM);
        let width = dim & 0xffff;
        let height = dim >> 16;
        let words_per_row = width.div_ceil(32);
        let total_words = u64::from(words_per_row) * u64::from(height);
        if total_words == 0 {
            return; // zero-area: nothing to stream
        }
        let params = ExpandParams {
            dst_base: self.blit_reg(REG_DST_BASE),
            dst_pitch: self.blit_reg(REG_DST_PITCH),
            depth,
            dst_x: dst_xy & 0xffff,
            dst_y: dst_xy >> 16,
            width,
            height,
            fg_color: self.blit_reg(REG_FG_COLOR),
            bg_color: self.blit_reg(REG_BG_COLOR),
            transparent: self.blit_reg(REG_FLAGS) & 0x4 != 0,
            rop: self.blit_reg(REG_ROP) as u8,
            clip: self.build_clip(),
        };
        self.expand = Some(ExpandState {
            params,
            words_per_row,
            total_words,
            words_received: 0,
            written: 0,
        });
    }

    fn feed_mono_word(&mut self, word: u32) {
        let Some(mut state) = self.expand else {
            return; // nothing armed: a stray or overrun write
        };
        let written = expand_word(
            &mut self.vram,
            &state.params,
            state.words_per_row,
            state.words_received,
            word,
        );
        state.words_received += 1;
        state.written += written;
        if u64::from(state.words_received) >= state.total_words {
            self.busy_ns = BLIT_SETUP_NS + state.written * EXPAND_NS_PER_PIXEL;
            self.expand = None;
        } else {
            self.expand = Some(state);
        }
    }

    /// Drain `ns` nanoseconds of modeled busy time. The machine calls this each
    /// CPU cycle, converting machine clocks to nanoseconds.
    pub fn advance_busy(&mut self, ns: u64) {
        self.busy_ns = self.busy_ns.saturating_sub(ns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_reg_u32(margo: &Margo, offset: usize) -> u32 {
        (0..4)
            .map(|i| u32::from(margo.read_mmio_u8(offset + i)) << (8 * i))
            .fold(0, |a, b| a | b)
    }

    #[test]
    fn reports_identity_caps_and_display() {
        let mut margo = Margo::default();
        assert_eq!(read_reg_u32(&margo, REG_ID), MARGO_ID_VALUE);
        assert_eq!(read_reg_u32(&margo, REG_CAPS), MARGO_CAPS_VALUE);

        margo.set_mode_640x480x8();
        assert_eq!(read_reg_u32(&margo, REG_DISP_WIDTH), 640);
        assert_eq!(read_reg_u32(&margo, REG_DISP_HEIGHT), 480);
        assert_eq!(read_reg_u32(&margo, REG_DISP_BPP), 8);
        assert_eq!(read_reg_u32(&margo, REG_DISP_PITCH), 640);
    }

    #[test]
    fn disp_start_is_writable_byte_by_byte() {
        let mut margo = Margo::default();
        // Distinct values in every lane prove the byte recombination, not just
        // a single shift.
        margo.write_mmio_u8(REG_DISP_START, 0x01);
        margo.write_mmio_u8(REG_DISP_START + 1, 0x02);
        margo.write_mmio_u8(REG_DISP_START + 2, 0x03);
        margo.write_mmio_u8(REG_DISP_START + 3, 0x04);
        assert_eq!(read_reg_u32(&margo, REG_DISP_START), 0x0403_0201);
    }

    #[test]
    fn disp_dimensions_are_read_only_to_the_bus() {
        let mut margo = Margo::default();
        margo.set_mode_640x480x8();
        margo.write_mmio_u8(REG_DISP_WIDTH, 0); // ignored
        assert_eq!(read_reg_u32(&margo, REG_DISP_WIDTH), 640);
    }

    #[test]
    fn vram_reads_and_writes() {
        let mut margo = Margo::default();
        margo.write_vram_u8(100, 0xab);
        assert_eq!(margo.read_vram_u8(100), 0xab);
        assert_eq!(margo.vram().len(), MARGO_VRAM_SIZE);
    }

    #[test]
    fn visible_surface_tracks_the_mode() {
        let mut margo = Margo::default();
        assert!(margo.visible_surface().is_empty()); // no mode set yet

        margo.set_mode_640x480x8();
        margo.write_vram_u8(0, 0x11);
        let last = 640 * 480 - 1;
        margo.write_vram_u8(last, 0x22);
        // A byte just past the visible surface must not appear in it.
        margo.write_vram_u8(640 * 480, 0x33);

        let surface = margo.visible_surface();
        assert_eq!(surface.len(), 640 * 480);
        assert_eq!(surface[0], 0x11);
        assert_eq!(surface[last], 0x22);
    }

    #[test]
    fn set_mode_looks_up_the_table() {
        let mut margo = Margo::default();
        assert!(margo.set_mode(0x103));
        assert_eq!(margo.display().mode, 0x103);
        assert_eq!(margo.display().width, 800);
        assert_eq!(margo.display().height, 600);
        assert_eq!(margo.display().bpp, 8);
        assert_eq!(margo.display().pitch, 800);
    }

    #[test]
    fn set_mode_rejects_modes_outside_the_table() {
        let mut margo = Margo::default();
        assert!(!margo.set_mode(0x112)); // 640x480x24 packed, not in the table
        assert_eq!(margo.display(), MargoDisplay::default());
    }

    #[test]
    fn set_mode_640x480x8_wrapper_still_sets_0x101() {
        let mut margo = Margo::default();
        margo.set_mode_640x480x8();
        assert_eq!(margo.display().mode, 0x101);
        assert_eq!(margo.display().width, 640);
        assert_eq!(margo.display().height, 480);
        assert_eq!(margo.display().pitch, 640);
    }

    #[test]
    fn vbe_mode_lookup_finds_table_entries() {
        assert_eq!(
            vbe_mode(0x105).map(|m| (m.width, m.height)),
            Some((1024, 768))
        );
        assert!(vbe_mode(0x999).is_none());
    }

    #[test]
    fn blit_registers_round_trip() {
        let mut margo = Margo::default();
        // Distinct values in each lane prove byte recombination.
        margo.write_mmio_u8(REG_DST_BASE, 0x11);
        margo.write_mmio_u8(REG_DST_BASE + 1, 0x22);
        margo.write_mmio_u8(REG_DST_BASE + 2, 0x33);
        margo.write_mmio_u8(REG_DST_BASE + 3, 0x44);
        assert_eq!(read_reg_u32(&margo, REG_DST_BASE), 0x4433_2211);

        // A different blit register is independent.
        margo.write_mmio_u8(REG_FG_COLOR, 0xab);
        assert_eq!(read_reg_u32(&margo, REG_FG_COLOR), 0x0000_00ab);
        assert_eq!(read_reg_u32(&margo, REG_DST_BASE), 0x4433_2211);
    }

    #[test]
    fn fill_writes_a_solid_rectangle_depth_1() {
        let mut vram = vec![0u8; 64];
        // pitch 8, 2x2 rectangle at (x=1, y=1), color 0xAB, solid (ROP 0xF0).
        let p = FillParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            dst_x: 1,
            dst_y: 1,
            width: 2,
            height: 2,
            fg_color: 0x0000_00ab,
            rop: 0xf0,
            clip: Clip::default(),
        };
        let pixels = fill(&mut vram, &p);
        assert_eq!(pixels, 4);
        // Rows y=1 and y=2, columns x=1,2 -> offsets 9,10 and 17,18.
        assert_eq!(vram[9], 0xab);
        assert_eq!(vram[10], 0xab);
        assert_eq!(vram[17], 0xab);
        assert_eq!(vram[18], 0xab);
        // Neighbours stay zero.
        assert_eq!(vram[8], 0x00);
        assert_eq!(vram[11], 0x00);
    }

    #[test]
    fn fill_writes_depth_2_and_4_pixels() {
        let mut vram = vec![0u8; 64];
        // depth 2: one pixel at (0,0), color 0x1234 -> low 2 bytes little-endian.
        let p2 = FillParams {
            dst_base: 0,
            dst_pitch: 16,
            depth: 2,
            dst_x: 0,
            dst_y: 0,
            width: 1,
            height: 1,
            fg_color: 0x0000_1234,
            rop: 0xf0,
            clip: Clip::default(),
        };
        fill(&mut vram, &p2);
        assert_eq!(vram[0], 0x34);
        assert_eq!(vram[1], 0x12);
        assert_eq!(vram[2], 0x00);

        // depth 4: one pixel at offset 16, color 0xDEADBEEF.
        let p4 = FillParams {
            dst_base: 16,
            dst_pitch: 16,
            depth: 4,
            dst_x: 0,
            dst_y: 0,
            width: 1,
            height: 1,
            fg_color: 0xdead_beef,
            rop: 0xf0,
            clip: Clip::default(),
        };
        fill(&mut vram, &p4);
        assert_eq!(&vram[16..20], &[0xef, 0xbe, 0xad, 0xde]);
    }

    #[test]
    fn fill_xor_rop_inverts_the_destination() {
        let mut vram = vec![0xffu8; 16];
        let p = FillParams {
            dst_base: 0,
            dst_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 2,
            height: 1,
            fg_color: 0x0000_000f,
            rop: 0x5a, // PATINVERT: dst ^= fg
            clip: Clip::default(),
        };
        fill(&mut vram, &p);
        assert_eq!(vram[0], 0xf0); // 0xff ^ 0x0f
        assert_eq!(vram[1], 0xf0);
        assert_eq!(vram[2], 0xff); // outside the 2-wide rect
    }

    #[test]
    fn fill_skips_out_of_bounds_without_wrapping() {
        let mut vram = vec![0u8; 16];
        // A rectangle that runs off the end of the store. base 14, pitch 4,
        // depth 1, 4 wide x 1 high -> offsets 14,15,16,17. 16 and 17 are out.
        let p = FillParams {
            dst_base: 14,
            dst_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4,
            height: 1,
            fg_color: 0x0000_0077,
            rop: 0xf0,
            clip: Clip::default(),
        };
        fill(&mut vram, &p);
        assert_eq!(vram[14], 0x77);
        assert_eq!(vram[15], 0x77);
        assert_eq!(vram[0], 0x00); // not wrapped to the start
    }

    #[test]
    fn fill_rejects_invalid_depth() {
        let mut vram = vec![0u8; 16];
        let p = FillParams {
            dst_base: 0,
            dst_pitch: 4,
            depth: 3, // not 1, 2, or 4
            dst_x: 0,
            dst_y: 0,
            width: 2,
            height: 2,
            fg_color: 0x0000_00ff,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(fill(&mut vram, &p), 0);
        assert!(vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn fill_caps_iterations_at_the_store_size() {
        let mut vram = vec![0u8; 16];
        // A pathological DIM must not spin: capped at vram.len() iterations.
        let p = FillParams {
            dst_base: 0,
            dst_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4000,
            height: 4000,
            fg_color: 0x0000_0001,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(fill(&mut vram, &p), 16);
    }

    #[test]
    fn fill_skips_extreme_coordinates_without_overflow() {
        let mut vram = vec![0u8; 64];
        // Adversarial guest registers: every pixel is far out of the store.
        // Must not panic; nothing is written.
        let p = FillParams {
            dst_base: u32::MAX,
            dst_pitch: u32::MAX,
            depth: 4,
            dst_x: u32::MAX,
            dst_y: u32::MAX,
            width: 8,
            height: 8,
            fg_color: 0xdead_beef,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(fill(&mut vram, &p), 0);
        assert!(vram.iter().all(|&b| b == 0));
    }

    // Write a 32-bit register through the byte-granular MMIO path.
    fn write_reg(margo: &mut Margo, offset: usize, value: u32) {
        for (i, b) in value.to_le_bytes().into_iter().enumerate() {
            margo.write_mmio_u8(offset + i, b);
        }
    }

    fn setup_fill(margo: &mut Margo) {
        write_reg(margo, REG_DST_BASE, 0);
        write_reg(margo, REG_DST_PITCH, 8);
        write_reg(margo, REG_DEPTH, 1);
        write_reg(margo, REG_DST_XY, (1 << 16) | 1); // y=1, x=1
        write_reg(margo, REG_DIM, (2 << 16) | 2); // h=2, w=2
        write_reg(margo, REG_FG_COLOR, 0x0000_00ab);
        write_reg(margo, REG_ROP, 0xf0);
    }

    #[test]
    fn caps_reports_all_implemented_features() {
        let margo = Margo::default();
        // bits 0 FILL, 1 COPY, 2 COLOR_EXPAND, 3 LINE, 4 full ROP3, 5 CLIP, 6 COLORKEY.
        assert_eq!(read_reg_u32(&margo, REG_CAPS), 0x0000_007f);
    }

    #[test]
    fn command_line_draws_and_sets_busy() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_LINE_START, 0); // (0,0)
        write_reg(&mut margo, REG_LINE_END, 3); // (3,0): horizontal 4-pixel line
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_ROP, 0xf0);
        write_reg(&mut margo, REG_COMMAND, 0x05); // LINE

        for off in 0..4 {
            assert_eq!(margo.read_vram_u8(off), 0xab);
        }
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
    }

    #[test]
    fn command_line_busy_drains_at_the_line_rate() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_LINE_START, 0);
        write_reg(&mut margo, REG_LINE_END, 3); // 4-pixel line
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_ROP, 0xf0);
        write_reg(&mut margo, REG_COMMAND, 0x05);

        // 4 pixels -> busy_ns = 100 + 4*10 = 140. One ns short still reads busy.
        margo.advance_busy(139);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        margo.advance_busy(1);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn command_fill_writes_vram_and_sets_busy() {
        let mut margo = Margo::default();
        setup_fill(&mut margo);
        write_reg(&mut margo, REG_COMMAND, 0x01); // FILL

        // VRAM is filled immediately.
        assert_eq!(margo.read_vram_u8(9), 0xab); // y=1, x=1: pitch*y+x = 8+1
        assert_eq!(margo.read_vram_u8(18), 0xab); // y=2, x=2: 8*2+2
        // STATUS.BUSY is set.
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    }

    #[test]
    fn advance_busy_drains_to_idle() {
        let mut margo = Margo::default();
        setup_fill(&mut margo);
        write_reg(&mut margo, REG_COMMAND, 0x01);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);

        // 4 pixels: busy_ns = 100 + 4*5 = 120. One ns short still reads busy.
        margo.advance_busy(119);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        margo.advance_busy(1);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn unknown_command_is_a_no_op() {
        let mut margo = Margo::default();
        setup_fill(&mut margo);
        write_reg(&mut margo, REG_COMMAND, 0x07); // unused command code
        // No VRAM change and no busy time: offset 9 is the first pixel FILL would write.
        assert_eq!(margo.read_vram_u8(9), 0x00);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn control_reset_clears_busy() {
        let mut margo = Margo::default();
        setup_fill(&mut margo);
        write_reg(&mut margo, REG_COMMAND, 0x01);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);

        write_reg(&mut margo, REG_CONTROL, 0x01); // RESET
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
        // RESET is self-clearing.
        assert_eq!(read_reg_u32(&margo, REG_CONTROL) & 1, 0);
    }

    #[test]
    fn command_copy_moves_vram_and_sets_busy() {
        let mut margo = Margo::default();
        margo.write_vram_u8(0, 0x55); // source pixel (0,0), pitch 8
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_SRC_BASE, 0);
        write_reg(&mut margo, REG_SRC_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, (1 << 16) | 4); // y=1, x=4
        write_reg(&mut margo, REG_SRC_XY, 0); // (0,0)
        write_reg(&mut margo, REG_DIM, (1 << 16) | 1); // 1x1
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x02); // COPY

        assert_eq!(margo.read_vram_u8(8 + 4), 0x55); // (4,1) got the source byte
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
    }

    #[test]
    fn command_copy_busy_drains_at_the_copy_rate() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_SRC_BASE, 0);
        write_reg(&mut margo, REG_SRC_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 2 << 16); // y=2, x=0 (no overlap with src)
        write_reg(&mut margo, REG_SRC_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 2); // 2x1 = 2 pixels
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x02);

        // 2 pixels -> busy_ns = 100 + 2*10 = 120. One ns short still reads busy.
        margo.advance_busy(119);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        margo.advance_busy(1);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn copy_moves_a_non_overlapping_rectangle_depth_1() {
        // pitch 8. Source 2x2 at (0,0) holds distinct bytes; copy it to (4,2).
        let mut vram = vec![0u8; 64];
        vram[0] = 0xa1; // (0,0)
        vram[1] = 0xa2; // (1,0)
        vram[8] = 0xa3; // (0,1)
        vram[9] = 0xa4; // (1,1)
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 8,
            src_base: 0,
            src_pitch: 8,
            depth: 1,
            dst_x: 4,
            dst_y: 2,
            src_x: 0,
            src_y: 0,
            width: 2,
            height: 2,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        let pixels = copy(&mut vram, &p);
        assert_eq!(pixels, 4);
        // Destination (4,2)=20, (5,2)=21, (4,3)=28, (5,3)=29.
        assert_eq!(vram[20], 0xa1);
        assert_eq!(vram[21], 0xa2);
        assert_eq!(vram[28], 0xa3);
        assert_eq!(vram[29], 0xa4);
        // Source untouched.
        assert_eq!(vram[0], 0xa1);
    }

    #[test]
    fn copy_moves_depth_2_and_4_pixels() {
        let mut vram = vec![0u8; 64];
        // depth 2: source pixel at (0,0) = 0x1234, copy to (4,0).
        vram[0] = 0x34;
        vram[1] = 0x12;
        let p2 = CopyParams {
            dst_base: 0,
            dst_pitch: 32,
            src_base: 0,
            src_pitch: 32,
            depth: 2,
            dst_x: 4,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 1,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p2), 1);
        assert_eq!(&vram[8..10], &[0x34, 0x12]); // (4,0) at depth 2 = offset 8

        // depth 4: source pixel at (0,0) = 0xDEADBEEF, copy to (2,0).
        let mut vram = vec![0u8; 64];
        vram[0..4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        let p4 = CopyParams {
            dst_base: 0,
            dst_pitch: 32,
            src_base: 0,
            src_pitch: 32,
            depth: 4,
            dst_x: 2,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 1,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p4), 1);
        assert_eq!(&vram[8..12], &[0xef, 0xbe, 0xad, 0xde]); // (2,0) at depth 4 = offset 8
    }

    #[test]
    fn copy_color_key_skips_matching_source_pixels() {
        // Source row [0x05, 0x07] at (0,0); key 0x05 is transparent.
        let mut vram = vec![0u8; 32];
        vram[0] = 0x05;
        vram[1] = 0x07;
        // Pre-fill the destination so a skipped pixel is visibly left alone.
        vram[8] = 0xee;
        vram[9] = 0xee;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 8,
            src_base: 0,
            src_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 1,
            src_x: 0,
            src_y: 0,
            width: 2,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0x05,
            colorkey_en: true,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p), 1); // only the non-keyed pixel written
        assert_eq!(vram[8], 0xee); // keyed source 0x05 -> destination untouched
        assert_eq!(vram[9], 0x07); // non-keyed source copied
    }

    #[test]
    fn copy_color_key_matches_full_pixel_at_depth_2() {
        // depth 2, key 0x1234. A source pixel equal to the key is skipped; a pixel
        // sharing only the high byte is copied (proves the compare uses both bytes).
        let mut vram = vec![0u8; 32];
        // src (0,0) = 0x1234 (keyed), src (1,0) = 0x1299 (not keyed), pitch 16.
        vram[0] = 0x34;
        vram[1] = 0x12;
        vram[2] = 0x99;
        vram[3] = 0x12;
        // Destination row at y=1 pre-filled so a skip is visible.
        vram[16] = 0xee;
        vram[17] = 0xee;
        vram[18] = 0xee;
        vram[19] = 0xee;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 16,
            src_base: 0,
            src_pitch: 16,
            depth: 2,
            dst_x: 0,
            dst_y: 1,
            src_x: 0,
            src_y: 0,
            width: 2,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0x1234,
            colorkey_en: true,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p), 1); // only the non-keyed pixel written
        // Keyed pixel (0x1234) skipped -> destination (0,1) bytes untouched.
        assert_eq!(&vram[16..18], &[0xee, 0xee]);
        // Non-keyed pixel (0x1299) copied -> destination (1,1) bytes = 0x1299.
        assert_eq!(&vram[18..20], &[0x99, 0x12]);
    }

    #[test]
    fn copy_skips_out_of_bounds_source_and_destination() {
        // Source partly off the store: src base 14, 4 wide at depth 1 -> offsets
        // 14,15,16,17; 16 and 17 are out, so only two pixels are readable.
        let mut vram = vec![0u8; 16];
        vram[14] = 0x71;
        vram[15] = 0x72;
        let p_src = CopyParams {
            dst_base: 0,
            dst_pitch: 16,
            src_base: 14,
            src_pitch: 16,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 4,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p_src), 2);
        assert_eq!(vram[0], 0x71);
        assert_eq!(vram[1], 0x72);

        // Destination partly off the store: same idea, dst base 14.
        let mut vram = vec![0u8; 16];
        vram[0] = 0x81;
        vram[1] = 0x82;
        vram[2] = 0x83;
        vram[3] = 0x84;
        let p_dst = CopyParams {
            dst_base: 14,
            dst_pitch: 16,
            src_base: 0,
            src_pitch: 16,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 4,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p_dst), 2); // offsets 14,15 in; 16,17 out
        assert_eq!(vram[14], 0x81);
        assert_eq!(vram[15], 0x82);
    }

    #[test]
    fn copy_rejects_invalid_depth() {
        let mut vram = vec![0u8; 16];
        vram[0] = 0xaa;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 4,
            src_base: 0,
            src_pitch: 4,
            depth: 3, // not 1, 2, or 4
            dst_x: 1,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 1,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p), 0);
        assert_eq!(vram[1], 0x00); // nothing written
    }

    #[test]
    fn copy_caps_iterations_at_the_store_size() {
        let mut vram = vec![0u8; 16];
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 4,
            src_base: 0,
            src_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 4000,
            height: 4000,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        // A pathological DIM must not spin; the loop is capped at vram.len().
        // With src==dst every considered pixel is a no-op move, so written tracks
        // the in-bounds count up to the cap.
        assert_eq!(copy(&mut vram, &p), 16);
    }

    #[test]
    fn copy_skips_extreme_coordinates_without_overflow() {
        let mut vram = vec![0u8; 64];
        let p = CopyParams {
            dst_base: u32::MAX,
            dst_pitch: u32::MAX,
            src_base: u32::MAX,
            src_pitch: u32::MAX,
            depth: 4,
            dst_x: u32::MAX,
            dst_y: u32::MAX,
            src_x: u32::MAX,
            src_y: u32::MAX,
            width: 8,
            height: 8,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p), 0); // must not panic; nothing written
        assert!(vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn copy_overlap_down_does_not_corrupt() {
        // pitch 4. Rows 0,1,2 hold distinct bytes. Copy the 4x2 rect at (0,0) down
        // one row to (0,1): row 1 must become row 0's old bytes, row 2 row 1's.
        let mut vram = vec![0u8; 32];
        for i in 0..4 {
            vram[i] = 1; // row 0
            vram[4 + i] = 2; // row 1
            vram[8 + i] = 3; // row 2
        }
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 4,
            src_base: 0,
            src_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 1,
            src_x: 0,
            src_y: 0,
            width: 4,
            height: 2,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        copy(&mut vram, &p);
        assert_eq!(&vram[4..8], &[1, 1, 1, 1]); // row 1 = old row 0
        assert_eq!(&vram[8..12], &[2, 2, 2, 2]); // row 2 = old row 1, not corrupted
        assert_eq!(&vram[0..4], &[1, 1, 1, 1]); // row 0 untouched
    }

    #[test]
    fn copy_overlap_right_does_not_corrupt() {
        // One row [1,2,3,4,5,6,7,8]. Copy the 4-wide rect at x=0 to x=1.
        let mut vram = vec![0u8; 8];
        for (i, slot) in vram.iter_mut().enumerate() {
            *slot = (i + 1) as u8;
        }
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 8,
            src_base: 0,
            src_pitch: 8,
            depth: 1,
            dst_x: 1,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 4,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        copy(&mut vram, &p);
        // Destination x=1..4 takes source x=0..3 = [1,2,3,4]; x=0 and x>=5 untouched.
        assert_eq!(&vram[0..8], &[1, 1, 2, 3, 4, 6, 7, 8]);
    }

    #[test]
    fn copy_overlap_diagonal_does_not_corrupt() {
        // pitch 4. Copy the 3x2 rect at (0,0) to (1,1): both axes shift positive,
        // so both must traverse in reverse.
        let mut vram = vec![0u8; 16];
        // Row 0: [1,2,3,_], row 1: [4,5,6,_].
        vram[0] = 1;
        vram[1] = 2;
        vram[2] = 3;
        vram[4] = 4;
        vram[5] = 5;
        vram[6] = 6;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 4,
            src_base: 0,
            src_pitch: 4,
            depth: 1,
            dst_x: 1,
            dst_y: 1,
            src_x: 0,
            src_y: 0,
            width: 3,
            height: 2,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        copy(&mut vram, &p);
        // dst row 1 (offset 4) cols 1..4 = src row 0 [1,2,3]; dst row 2 (offset 8)
        // cols 1..4 = src row 1 [4,5,6].
        assert_eq!(&vram[5..8], &[1, 2, 3]);
        assert_eq!(&vram[9..12], &[4, 5, 6]);
        // Source row 0 is unchanged where the destination did not overwrite it.
        assert_eq!(vram[0], 1);
    }

    #[test]
    fn color_expand_mem_expands_to_fg_and_bg_depth_1() {
        // Source byte 0xA0 = 1010_0000: cols 0 and 2 set, cols 1 and 3 clear
        // (MSB first). Expand a 4x1 rect; set bits take FG 0xAB, clear bits BG 0xCD.
        let mut vram = vec![0u8; 64];
        vram[0] = 0xa0; // monochrome source row, src_base 0
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 8,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 4,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 4);
        assert_eq!(&vram[16..20], &[0xab, 0xcd, 0xab, 0xcd]);
    }

    #[test]
    fn color_expand_mem_transparent_skips_clear_bits() {
        // Same 0xA0 source; transparent leaves clear-bit destinations untouched.
        let mut vram = vec![0u8; 64];
        vram[0] = 0xa0;
        for slot in &mut vram[16..20] {
            *slot = 0xee; // pre-fill so a skip is visible
        }
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 8,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 4,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: true,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 2); // only the two set bits
        assert_eq!(&vram[16..20], &[0xab, 0xee, 0xab, 0xee]);
    }

    #[test]
    fn color_expand_mem_handles_depth_2_and_4() {
        // depth 2: source 0x80 (col 0 set, col 1 clear) over a 2x1 rect.
        let mut vram = vec![0u8; 64];
        vram[0] = 0x80;
        let p2 = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 32,
                depth: 2,
                dst_x: 0,
                dst_y: 0,
                width: 2,
                height: 1,
                fg_color: 0x1234,
                bg_color: 0x5678,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p2), 2);
        assert_eq!(&vram[16..18], &[0x34, 0x12]); // col 0 -> FG, little-endian
        assert_eq!(&vram[18..20], &[0x78, 0x56]); // col 1 -> BG

        // depth 4: source 0x80 (col 0 set) over a 1x1 rect -> FG 0xDEADBEEF.
        let mut vram = vec![0u8; 64];
        vram[0] = 0x80;
        let p4 = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 32,
                depth: 4,
                dst_x: 0,
                dst_y: 0,
                width: 1,
                height: 1,
                fg_color: 0xdead_beef,
                bg_color: 0,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p4), 1);
        assert_eq!(&vram[16..20], &[0xef, 0xbe, 0xad, 0xde]);
    }

    #[test]
    fn color_expand_mem_crosses_a_source_byte_boundary() {
        // src_x = 7 puts col 0 at bit 7 (byte 0, LSB) and col 1 at bit 8 (byte 1,
        // MSB). Byte 0 = 0x01 (col 0 set), byte 1 = 0x00 (col 1 clear).
        let mut vram = vec![0u8; 64];
        vram[0] = 0x01;
        vram[1] = 0x00;
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 8,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 2,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 2,
            src_x: 7,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 2);
        assert_eq!(&vram[16..18], &[0xab, 0xcd]); // col 0 set -> FG, col 1 clear -> BG
    }

    #[test]
    fn color_expand_mem_skips_off_store_source_and_dest() {
        // Destination runs off the store: base 14, 4 wide at depth 1 -> dst offsets
        // 14,15,16,17; 16 and 17 are out. Source byte 0xF0 sets all four cols.
        let mut vram = vec![0u8; 16];
        vram[0] = 0xf0;
        let p_dst = ExpandMemParams {
            common: ExpandParams {
                dst_base: 14,
                dst_pitch: 16,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 4,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p_dst), 2); // offsets 14,15 in
        assert_eq!(vram[14], 0xab);
        assert_eq!(vram[15], 0xab);

        // Source off the store: src_base beyond the end -> every pixel skipped.
        let mut vram = vec![0u8; 16];
        let p_src = ExpandMemParams {
            common: ExpandParams {
                dst_base: 0,
                dst_pitch: 16,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 4,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 100,
            src_pitch: 16,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p_src), 0);
        assert!(vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn color_expand_mem_rejects_invalid_depth() {
        let mut vram = vec![0u8; 16];
        vram[0] = 0xff;
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 0,
                dst_pitch: 4,
                depth: 3, // not 1, 2, or 4
                dst_x: 1,
                dst_y: 0,
                width: 1,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 8,
            src_pitch: 4,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 0);
        assert_eq!(vram[1], 0x00); // nothing written
    }

    #[test]
    fn color_expand_mem_caps_iterations_at_the_store_size() {
        // Pathological DIM: an all-clear source with BG 0 and dst_base 0 writes 0
        // over 0 for every in-bounds pixel, so the count equals the cap (vram.len()).
        let mut vram = vec![0u8; 64];
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 0,
                dst_pitch: 4000,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 4000,
                height: 4000,
                fg_color: 0xab,
                bg_color: 0,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 0,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 64);
    }

    #[test]
    fn color_expand_mem_skips_extreme_coordinates_without_overflow() {
        let mut vram = vec![0u8; 64];
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: u32::MAX,
                dst_pitch: u32::MAX,
                depth: 4,
                dst_x: u32::MAX,
                dst_y: u32::MAX,
                width: 8,
                height: 8,
                fg_color: 0xdead_beef,
                bg_color: 0,
                transparent: false,
                rop: 0xcc,
                clip: Clip::default(),
            },
            src_base: u32::MAX,
            src_pitch: u32::MAX,
            src_x: u32::MAX,
            src_y: u32::MAX,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 0); // must not panic
        assert!(vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn bg_color_round_trips() {
        let mut margo = Margo::default();
        margo.write_mmio_u8(REG_BG_COLOR, 0x11);
        margo.write_mmio_u8(REG_BG_COLOR + 1, 0x22);
        margo.write_mmio_u8(REG_BG_COLOR + 2, 0x33);
        margo.write_mmio_u8(REG_BG_COLOR + 3, 0x44);
        assert_eq!(read_reg_u32(&margo, REG_BG_COLOR), 0x4433_2211);
    }

    #[test]
    fn command_expand_mem_writes_vram_and_sets_busy() {
        let mut margo = Margo::default();
        margo.write_vram_u8(0, 0x80); // source row: col 0 set, col 1 clear
        write_reg(&mut margo, REG_DST_BASE, 16);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_SRC_BASE, 0);
        write_reg(&mut margo, REG_SRC_PITCH, 1);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0); // (0,0)
        write_reg(&mut margo, REG_SRC_XY, 0); // (0,0)
        write_reg(&mut margo, REG_DIM, (1 << 16) | 2); // h=1, w=2
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x04); // COLOR_EXPAND_MEM

        assert_eq!(margo.read_vram_u8(16), 0xab); // col 0 set -> FG
        assert_eq!(margo.read_vram_u8(17), 0xcd); // col 1 clear -> BG
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
    }

    #[test]
    fn command_expand_mem_busy_drains_at_the_expand_rate() {
        let mut margo = Margo::default();
        // All-clear source, opaque: a 4x1 rect writes 4 BG pixels.
        write_reg(&mut margo, REG_DST_BASE, 16);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_SRC_BASE, 0);
        write_reg(&mut margo, REG_SRC_PITCH, 1);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_SRC_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 4); // 4x1 = 4 pixels
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x04);

        // 4 pixels -> busy_ns = 100 + 4*5 = 120. One ns short still reads busy.
        margo.advance_busy(119);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        margo.advance_busy(1);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn command_expand_data_arms_and_reports_busy() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 8); // h=1, w=8 -> 1 word
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03); // COLOR_EXPAND_DATA

        // Armed: BUSY set before any data word, nothing drawn yet.
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        assert_eq!(margo.read_vram_u8(0), 0x00);
    }

    #[test]
    fn expand_data_word_paints_fg_and_bg_msb_first() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 4); // h=1, w=4 -> 1 word/row
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03);

        // 0xA0000000: bits 31 and 29 set -> cols 0 and 2 set, cols 1 and 3 clear.
        write_reg(&mut margo, REG_MONO_DATA, 0xa000_0000);

        assert_eq!(margo.read_vram_u8(0), 0xab); // col 0 set
        assert_eq!(margo.read_vram_u8(1), 0xcd); // col 1 clear
        assert_eq!(margo.read_vram_u8(2), 0xab); // col 2 set
        assert_eq!(margo.read_vram_u8(3), 0xcd); // col 3 clear
        // Stream complete (one word) -> BUSY now reflects the cost tail.
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    }

    #[test]
    fn expand_data_continues_a_wide_row_across_words() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 64);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 40); // h=1, w=40 -> 2 words/row
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03);

        write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // word 0: col 0 set
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // armed, one word left
        write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // word 1: col 32 set

        assert_eq!(margo.read_vram_u8(0), 0xab); // col 0 set (word 0, bit 31)
        assert_eq!(margo.read_vram_u8(1), 0xcd); // col 1 clear
        assert_eq!(margo.read_vram_u8(32), 0xab); // col 32 set (word 1, bit 31)
        assert_eq!(margo.read_vram_u8(33), 0xcd); // col 33 clear
    }

    #[test]
    fn expand_data_holds_busy_through_the_stream_then_drains() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (2 << 16) | 8); // h=2, w=8 -> 1 word/row, 2 words
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // armed

        write_reg(&mut margo, REG_MONO_DATA, 0); // row 0 (all clear -> all BG)
        // Mid-stream BUSY is the armed flag, not a timer: a huge clock advance
        // cannot clear it before the last word.
        margo.advance_busy(1_000_000);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);

        write_reg(&mut margo, REG_MONO_DATA, 0); // row 1: last word completes the stream
        // 16 pixels written (8x2, opaque) -> tail busy_ns = 100 + 16*5 = 180.
        margo.advance_busy(179);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        margo.advance_busy(1);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn expand_data_transparent_skips_clear_bits() {
        let mut margo = Margo::default();
        margo.write_vram_u8(1, 0xee); // col 1 destination pre-filled
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 2); // h=1, w=2
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0x04); // EXPAND_TRANSPARENT
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03);

        write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // col 0 set, col 1 clear

        assert_eq!(margo.read_vram_u8(0), 0xab); // col 0 set -> FG
        assert_eq!(margo.read_vram_u8(1), 0xee); // col 1 clear -> left untouched
    }

    #[test]
    fn expand_data_reset_aborts_the_stream() {
        let mut margo = Margo::default();
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (2 << 16) | 8); // 2 words
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03);
        write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000); // row 0: cols 0..7 set
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // armed, one word left

        write_reg(&mut margo, REG_CONTROL, 0x01); // RESET aborts the stream
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);

        // Row 1 was never fed; a further MONO_DATA write is now ignored.
        write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000);
        assert_eq!(margo.read_vram_u8(8), 0x00); // row 1 (offset 8) stays clear
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    }

    #[test]
    fn expand_data_ignores_mono_data_when_idle() {
        let mut margo = Margo::default();
        margo.write_vram_u8(0, 0x11);
        write_reg(&mut margo, REG_MONO_DATA, 0xffff_ffff); // nothing armed
        assert_eq!(margo.read_vram_u8(0), 0x11); // untouched
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0); // not busy
    }

    #[test]
    fn a_new_command_abandons_an_in_flight_expand_stream() {
        let mut margo = Margo::default();
        // Arm a 2-word DATA stream targeting row 0 and row 1 at base 0, pitch 8.
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (2 << 16) | 8); // h=2, w=8 -> 2 words
        write_reg(&mut margo, REG_FG_COLOR, 0xab);
        write_reg(&mut margo, REG_BG_COLOR, 0xcd);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
        write_reg(&mut margo, REG_COMMAND, 0x03);
        write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000); // feed only row 0; under-run

        // A synchronous FILL now starts a new operation and abandons the stream.
        setup_fill(&mut margo);
        write_reg(&mut margo, REG_COMMAND, 0x01);

        // BUSY is driven only by the FILL's modeled time now, not a pinned stream.
        margo.advance_busy(1_000_000);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);

        // The abandoned stream must not resume: a later MONO_DATA write is ignored.
        let row1 = margo.read_vram_u8(8); // row 1, col 0 of the original stream target
        write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000);
        assert_eq!(margo.read_vram_u8(8), row1);
    }

    #[test]
    fn line_draws_a_shallow_line_endpoints_inclusive() {
        // (0,0) -> (4,2), pitch 8, depth 1. Bresenham plots (0,0),(1,1),(2,1),(3,2),
        // (4,2): offsets 0, 9, 10, 19, 20. Both endpoints are drawn.
        let mut vram = vec![0u8; 64];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 2,
            fg_color: 0xab,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 5);
        for off in [0usize, 9, 10, 19, 20] {
            assert_eq!(vram[off], 0xab, "expected line pixel at offset {off}");
        }
        assert_eq!(vram[1], 0x00); // (1,0) is not on the line
    }

    #[test]
    fn line_draws_a_steep_line() {
        // (0,0) -> (2,4), pitch 8: (0,0),(1,1),(1,2),(2,3),(2,4) -> offsets
        // 0, 9, 17, 26, 34 (covers the y-major Bresenham branch).
        let mut vram = vec![0u8; 64];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 4,
            fg_color: 0xcd,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 5);
        for off in [0usize, 9, 17, 26, 34] {
            assert_eq!(vram[off], 0xcd, "expected line pixel at offset {off}");
        }
    }

    #[test]
    fn line_draws_horizontal_and_vertical_runs() {
        // Horizontal (0,1) -> (3,1), pitch 8: offsets 8, 9, 10, 11.
        let mut vram = vec![0u8; 64];
        let h = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 1,
            x1: 3,
            y1: 1,
            fg_color: 0x11,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &h), 4);
        assert_eq!(&vram[8..12], &[0x11, 0x11, 0x11, 0x11]);

        // Vertical (1,0) -> (1,3), pitch 8: offsets 1, 9, 17, 25.
        let mut vram = vec![0u8; 64];
        let v = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 1,
            y0: 0,
            x1: 1,
            y1: 3,
            fg_color: 0x22,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &v), 4);
        for off in [1usize, 9, 17, 25] {
            assert_eq!(vram[off], 0x22);
        }
    }

    #[test]
    fn line_draws_a_45_degree_diagonal() {
        // (0,0) -> (3,3), pitch 8: one pixel per step at offsets 0, 9, 18, 27.
        let mut vram = vec![0u8; 64];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 3,
            fg_color: 0x33,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 4);
        for off in [0usize, 9, 18, 27] {
            assert_eq!(vram[off], 0x33);
        }
    }

    #[test]
    fn line_degenerate_plots_one_pixel() {
        // LINE_START == LINE_END plots exactly the one pixel (5,5), offset 45.
        let mut vram = vec![0u8; 64];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 5,
            y0: 5,
            x1: 5,
            y1: 5,
            fg_color: 0x44,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 1);
        assert_eq!(vram[45], 0x44);
    }

    #[test]
    fn line_reversed_direction_covers_negative_steps() {
        // (3,3) -> (0,0): both sx and sy are negative. A diagonal is symmetric, so it
        // plots the same pixels as (0,0) -> (3,3): offsets 0, 9, 18, 27.
        let mut vram = vec![0u8; 64];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 3,
            y0: 3,
            x1: 0,
            y1: 0,
            fg_color: 0xab,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 4);
        for off in [0usize, 9, 18, 27] {
            assert_eq!(vram[off], 0xab);
        }
    }

    #[test]
    fn line_writes_depth_2_and_4_pixels() {
        // depth 2 horizontal 2-pixel line, FG 0x1234 little-endian, pitch 16.
        let mut vram = vec![0u8; 64];
        let p2 = LineParams {
            dst_base: 0,
            dst_pitch: 16,
            depth: 2,
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 0,
            fg_color: 0x1234,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p2), 2);
        assert_eq!(&vram[0..2], &[0x34, 0x12]); // (0,0)
        assert_eq!(&vram[2..4], &[0x34, 0x12]); // (1,0) at depth 2 = offset 2

        // depth 4 single point at (1,0) = offset 4, FG 0xDEADBEEF.
        let mut vram = vec![0u8; 64];
        let p4 = LineParams {
            dst_base: 0,
            dst_pitch: 16,
            depth: 4,
            x0: 1,
            y0: 0,
            x1: 1,
            y1: 0,
            fg_color: 0xdead_beef,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p4), 1);
        assert_eq!(&vram[4..8], &[0xef, 0xbe, 0xad, 0xde]);
    }

    #[test]
    fn line_xor_rop_draws_and_erases() {
        // Horizontal (0,0) -> (3,0) at offsets 0..4 over a 0xFF background; ROP 0x5A
        // XORs 0x0F in, and a second identical draw restores the background.
        let mut vram = vec![0xffu8; 32];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 0,
            fg_color: 0x0f,
            rop: 0x5a,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 4);
        assert_eq!(&vram[0..4], &[0xf0, 0xf0, 0xf0, 0xf0]); // 0xff ^ 0x0f
        assert_eq!(line(&mut vram, &p), 4);
        assert_eq!(&vram[0..4], &[0xff, 0xff, 0xff, 0xff]); // restored
    }

    #[test]
    fn line_skips_out_of_store_pixels() {
        // Vertical (0,0) -> (0,3), pitch 8, store 16: offsets 0, 8 are in; 16, 24 out.
        let mut vram = vec![0u8; 16];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 3,
            fg_color: 0xab,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 2);
        assert_eq!(vram[0], 0xab);
        assert_eq!(vram[8], 0xab);
    }

    #[test]
    fn line_rejects_invalid_depth() {
        let mut vram = vec![0u8; 16];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 4,
            depth: 3, // not 1, 2, or 4
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 0,
            fg_color: 0xff,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 0);
        assert!(vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn line_skips_extreme_offsets_without_overflow() {
        // 16-bit coordinates (as run_line supplies) with an extreme base and pitch:
        // every offset saturates past the store, nothing is written, no panic.
        let mut vram = vec![0u8; 64];
        let p = LineParams {
            dst_base: u32::MAX,
            dst_pitch: u32::MAX,
            depth: 4,
            x0: 0xffff,
            y0: 0xffff,
            x1: 0,
            y1: 0,
            fg_color: 0xdead_beef,
            rop: 0xf0,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 0);
        assert!(vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn fill_applies_rop3_pattern_and_dest_codes() {
        // Single pixel (0,0), pitch 4, depth 1, over dest 0x3C with FG 0x0F.
        // FILL has no source (S = 0), so only P/D codes are meaningful.
        let cases: [(u8, u8); 5] = [
            (0xf0, 0x0f),        // PATCOPY -> P (FG)
            (0x55, !0x3cu8),     // DSTINVERT -> ~D
            (0x5a, 0x3c ^ 0x0f), // PATINVERT -> D ^ P
            (0x00, 0x00),        // BLACKNESS
            (0xff, 0xff),        // WHITENESS
        ];
        for (rop, expected) in cases {
            let mut vram = vec![0u8; 16];
            vram[0] = 0x3c;
            let p = FillParams {
                dst_base: 0,
                dst_pitch: 4,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 1,
                height: 1,
                fg_color: 0x0f,
                rop,
                clip: Clip::default(),
            };
            assert_eq!(fill(&mut vram, &p), 1);
            assert_eq!(vram[0], expected, "rop {rop:#x}");
        }
    }

    #[test]
    fn fill_clips_to_the_rectangle() {
        // 4x1 fill at y=0, cols 0..3, pitch 8; clip to x in [1, 3), y in [0, 1).
        let mut vram = vec![0u8; 16];
        let p = FillParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4,
            height: 1,
            fg_color: 0xab,
            rop: 0xf0,
            clip: Clip {
                enabled: true,
                x0: 1,
                y0: 0,
                x1: 3,
                y1: 1,
            },
        };
        assert_eq!(fill(&mut vram, &p), 2); // only x = 1, 2
        assert_eq!(vram[0], 0x00); // x = 0 clipped
        assert_eq!(vram[1], 0xab);
        assert_eq!(vram[2], 0xab);
        assert_eq!(vram[3], 0x00); // x = 3 clipped (BR exclusive)
    }

    #[test]
    fn line_applies_rop3_against_the_destination() {
        // Horizontal 3-pixel line over a 0xFF background; DSTINVERT (0x55) -> 0x00.
        let mut vram = vec![0xffu8; 8];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 0,
            fg_color: 0,
            rop: 0x55,
            clip: Clip::default(),
        };
        assert_eq!(line(&mut vram, &p), 3);
        assert_eq!(&vram[0..3], &[0x00, 0x00, 0x00]);
        assert_eq!(vram[3], 0xff); // outside the line
    }

    #[test]
    fn line_clips_to_the_rectangle() {
        // Horizontal line cols 0..4 at y=0; clip to x in [1, 3).
        let mut vram = vec![0u8; 8];
        let p = LineParams {
            dst_base: 0,
            dst_pitch: 8,
            depth: 1,
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 0,
            fg_color: 0xab,
            rop: 0xf0,
            clip: Clip {
                enabled: true,
                x0: 1,
                y0: 0,
                x1: 3,
                y1: 1,
            },
        };
        assert_eq!(line(&mut vram, &p), 2); // only x = 1, 2
        assert_eq!(&vram[0..5], &[0x00, 0xab, 0xab, 0x00, 0x00]);
    }

    #[test]
    fn rop3_evaluates_the_named_codes() {
        // Distinct multi-bit operands so the test exercises the bitwise evaluation.
        let (p, s, d) = (0xf0u32, 0xccu32, 0xaau32);
        assert_eq!(rop3(0x00, p, s, d), 0); // BLACKNESS
        assert_eq!(rop3(0xff, p, s, d), u32::MAX); // WHITENESS
        assert_eq!(rop3(0xcc, p, s, d), s); // SRCCOPY
        assert_eq!(rop3(0xf0, p, s, d), p); // PATCOPY
        assert_eq!(rop3(0x55, p, s, d), !d); // DSTINVERT
        assert_eq!(rop3(0x5a, p, s, d), d ^ p); // PATINVERT
        assert_eq!(rop3(0x66, p, s, d), d ^ s); // SRCINVERT
        assert_eq!(rop3(0x88, p, s, d), d & s); // SRCAND
        assert_eq!(rop3(0xee, p, s, d), d | s); // SRCPAINT
    }

    #[test]
    fn copy_overlap_down_left_does_not_corrupt() {
        // pitch 4. Source 3x2 rect at (1,0); copy it down-left to (0,1). Rows must
        // reverse (dst below src) while columns stay forward (dst left of src).
        let mut vram = vec![0u8; 16];
        // src row 0 at offsets 1,2,3; src row 1 at offsets 5,6,7.
        vram[1] = 1;
        vram[2] = 2;
        vram[3] = 3;
        vram[5] = 4;
        vram[6] = 5;
        vram[7] = 6;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 4,
            src_base: 0,
            src_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 1,
            src_x: 1,
            src_y: 0,
            width: 3,
            height: 2,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        copy(&mut vram, &p);
        // dst row 1 (offsets 4,5,6) = src row 0 [1,2,3]; dst row 2 (offsets 8,9,10)
        // = src row 1 [4,5,6], uncorrupted by the row overlap.
        assert_eq!(&vram[4..7], &[1, 2, 3]);
        assert_eq!(&vram[8..11], &[4, 5, 6]);
    }

    #[test]
    fn copy_applies_rop3_source_and_dest() {
        // Source 0xCC at (0,0), dest 0xAA at (4,0), pitch 8. SRCINVERT (0x66) -> D^S.
        let mut vram = vec![0u8; 16];
        vram[0] = 0xcc;
        vram[4] = 0xaa;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 8,
            src_base: 0,
            src_pitch: 8,
            depth: 1,
            dst_x: 4,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 1,
            height: 1,
            fg_color: 0,
            rop: 0x66,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p), 1);
        assert_eq!(vram[4], 0xaa ^ 0xcc); // D ^ S
    }

    #[test]
    fn copy_clips_to_the_rectangle() {
        // Source row [1,2,3,4] at (0,0); copy to (0,1) cols 0..4; clip to x in [1,3).
        let mut vram = vec![0u8; 16];
        vram[0] = 1;
        vram[1] = 2;
        vram[2] = 3;
        vram[3] = 4;
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 8,
            src_base: 0,
            src_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 1,
            src_x: 0,
            src_y: 0,
            width: 4,
            height: 1,
            fg_color: 0,
            rop: 0xcc,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip {
                enabled: true,
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 2,
            },
        };
        assert_eq!(copy(&mut vram, &p), 2); // dest x = 1, 2 only
        assert_eq!(vram[8], 0); // dest (0,1) clipped
        assert_eq!(vram[9], 2); // dest (1,1) = src x=1
        assert_eq!(vram[10], 3); // dest (2,1) = src x=2
        assert_eq!(vram[11], 0); // dest (3,1) clipped
    }

    #[test]
    fn copy_applies_rop3_at_depth_2() {
        // Source pixel 0x1234, dest pixel 0xABCD, depth 2, SRCINVERT (0x66): D ^ S.
        // 0x1234 ^ 0xABCD = 0xB9F9, stored little-endian.
        let mut vram = vec![0u8; 32];
        vram[0] = 0x34; // src (0,0) low byte
        vram[1] = 0x12; // src (0,0) high byte
        vram[4] = 0xcd; // dst (2,0) low byte  (pitch 16 -> offset 2*depth = 4)
        vram[5] = 0xab; // dst (2,0) high byte
        let p = CopyParams {
            dst_base: 0,
            dst_pitch: 16,
            src_base: 0,
            src_pitch: 16,
            depth: 2,
            dst_x: 2,
            dst_y: 0,
            src_x: 0,
            src_y: 0,
            width: 1,
            height: 1,
            fg_color: 0,
            rop: 0x66,
            colorkey: 0,
            colorkey_en: false,
            clip: Clip::default(),
        };
        assert_eq!(copy(&mut vram, &p), 1);
        let result = u16::from_le_bytes([vram[4], vram[5]]);
        assert_eq!(result, 0x1234u16 ^ 0xabcdu16); // 0xB9F9
    }

    #[test]
    fn color_expand_mem_applies_rop3() {
        // Source bit set -> S = FG 0x0F; dest 0xAA; SRCINVERT (0x66) -> D ^ S.
        let mut vram = vec![0u8; 64];
        vram[0] = 0x80; // mono source, col 0 set
        vram[16] = 0xaa; // dest pixel
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 8,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 1,
                height: 1,
                fg_color: 0x0f,
                bg_color: 0,
                transparent: false,
                rop: 0x66,
                clip: Clip::default(),
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 1);
        assert_eq!(vram[16], 0xaa ^ 0x0f);
    }

    #[test]
    fn color_expand_mem_clips() {
        // Source 0xC0 (cols 0,1 set) expanded to dest cols 0,1; clip to x in [1,2).
        let mut vram = vec![0u8; 64];
        vram[0] = 0xc0;
        let p = ExpandMemParams {
            common: ExpandParams {
                dst_base: 16,
                dst_pitch: 8,
                depth: 1,
                dst_x: 0,
                dst_y: 0,
                width: 2,
                height: 1,
                fg_color: 0xab,
                bg_color: 0xcd,
                transparent: false,
                rop: 0xcc,
                clip: Clip {
                    enabled: true,
                    x0: 1,
                    y0: 0,
                    x1: 2,
                    y1: 1,
                },
            },
            src_base: 0,
            src_pitch: 1,
            src_x: 0,
            src_y: 0,
        };
        assert_eq!(color_expand_mem(&mut vram, &p), 1); // only col 1
        assert_eq!(vram[16], 0x00); // col 0 clipped
        assert_eq!(vram[17], 0xab); // col 1 set -> S = FG, 0xcc writes S
    }

    #[test]
    fn expand_data_applies_rop3() {
        // Streamed DATA must honor ROP too: arm with SRCINVERT, dest 0xAA, set bit.
        let mut margo = Margo::default();
        margo.write_vram_u8(0, 0xaa);
        write_reg(&mut margo, REG_DST_BASE, 0);
        write_reg(&mut margo, REG_DST_PITCH, 8);
        write_reg(&mut margo, REG_DEPTH, 1);
        write_reg(&mut margo, REG_DST_XY, 0);
        write_reg(&mut margo, REG_DIM, (1 << 16) | 1); // 1x1
        write_reg(&mut margo, REG_FG_COLOR, 0x0f);
        write_reg(&mut margo, REG_FLAGS, 0);
        write_reg(&mut margo, REG_ROP, 0x66); // SRCINVERT: D ^ S
        write_reg(&mut margo, REG_COMMAND, 0x03);
        write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // one word, col 0 set
        assert_eq!(margo.read_vram_u8(0), 0xaa ^ 0x0f);
    }

    #[test]
    fn bytes_per_pixel_rounds_up_to_whole_bytes() {
        assert_eq!(bytes_per_pixel(8), 1);
        assert_eq!(bytes_per_pixel(15), 2);
        assert_eq!(bytes_per_pixel(16), 2);
        assert_eq!(bytes_per_pixel(32), 4);
    }

    #[test]
    fn vbe_mode_lookup_finds_hicolor_modes() {
        assert_eq!(vbe_mode(0x110).unwrap().bpp, 15);
        assert_eq!(vbe_mode(0x111).unwrap().bpp, 16);
        assert_eq!(vbe_mode(0x14a).unwrap().bpp, 32);
        assert_eq!(vbe_mode(0x14e).unwrap().width, 1024);
    }

    #[test]
    fn set_mode_pitch_uses_whole_byte_pixels() {
        let mut margo = Margo::default();
        margo.set_mode(0x110); // 640x480x15
        assert_eq!(margo.display().bpp, 15);
        assert_eq!(margo.display().pitch, 1280); // 640 * 2, not 640 * 15 / 8
        margo.set_mode(0x111); // 640x480x16
        assert_eq!(margo.display().pitch, 1280);
        margo.set_mode(0x14a); // 640x480x32
        assert_eq!(margo.display().pitch, 2560);
    }

    #[test]
    fn pixel_format_describes_direct_color_layouts() {
        assert!(pixel_format(8).is_none()); // indexed, not direct color
        let f16 = pixel_format(16).unwrap();
        assert_eq!((f16.r.pos, f16.r.size), (11, 5));
        assert_eq!((f16.g.pos, f16.g.size), (5, 6));
        assert_eq!((f16.b.pos, f16.b.size), (0, 5));
        let f15 = pixel_format(15).unwrap();
        assert_eq!((f15.r.pos, f15.r.size), (10, 5));
        assert_eq!((f15.x.pos, f15.x.size), (15, 1));
        let f32 = pixel_format(32).unwrap();
        assert_eq!((f32.r.pos, f32.r.size), (16, 8));
        assert_eq!((f32.x.pos, f32.x.size), (24, 8));
    }

    #[test]
    fn decode_argb_handles_each_format() {
        let palette = {
            let mut p = [0u32; 256];
            p[7] = 0x0012_3456;
            p
        };
        // 8bpp indexed: straight palette lookup.
        assert_eq!(decode_argb(8, 7, &palette), 0x0012_3456);
        // 16bpp R5G6B5: red, green, blue, white, black.
        assert_eq!(decode_argb(16, 0xf800, &palette), 0x00ff_0000);
        assert_eq!(decode_argb(16, 0x07e0, &palette), 0x0000_ff00);
        assert_eq!(decode_argb(16, 0x001f, &palette), 0x0000_00ff);
        assert_eq!(decode_argb(16, 0xffff, &palette), 0x00ff_ffff);
        assert_eq!(decode_argb(16, 0x0000, &palette), 0x0000_0000);
        // 15bpp X1R5G5B5: red, green, blue; the X bit is ignored.
        assert_eq!(decode_argb(15, 0x7c00, &palette), 0x00ff_0000);
        assert_eq!(decode_argb(15, 0x03e0, &palette), 0x0000_ff00);
        assert_eq!(decode_argb(15, 0x001f, &palette), 0x0000_00ff);
        assert_eq!(decode_argb(15, 0x8000 | 0x7c00, &palette), 0x00ff_0000);
        // 32bpp X8R8G8B8: the X byte is ignored.
        assert_eq!(decode_argb(32, 0x0034_5678, &palette), 0x0034_5678);
        assert_eq!(decode_argb(32, 0xff34_5678, &palette), 0x0034_5678);
    }

    #[test]
    fn scanout_argb_decodes_the_visible_surface() {
        let palette = [0u32; 256];
        let mut margo = Margo::default();
        // No mode set yet: empty scanout.
        assert!(margo.scanout_argb(&palette).is_empty());

        margo.set_mode(0x111); // 640x480x16, pitch 1280
        // Red pixel at (3, 2): offset 2*1280 + 3*2 = 2566; R5G6B5 red = 0xf800 LE.
        margo.write_vram_u8(2566, 0x00);
        margo.write_vram_u8(2567, 0xf8);
        // Green pixel at (0, 0): 0x07e0 LE.
        margo.write_vram_u8(0, 0xe0);
        margo.write_vram_u8(1, 0x07);

        let argb = margo.scanout_argb(&palette);
        assert_eq!(argb.len(), 640 * 480);
        assert_eq!(argb[2 * 640 + 3], 0x00ff_0000);
        assert_eq!(argb[0], 0x0000_ff00);
        assert_eq!(argb[1], 0x0000_0000); // untouched pixel
    }
}
