// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Margo, the VEGA 2D engine: the display register block, the linear frame
//! buffer, and the blit engine. The engine implements FILL, COPY, color expand,
//! LINE, and PATTERN_FILL, all with full ROP3 and rectangle clipping, plus a
//! display-path hardware cursor, a scaled YUV video overlay, a DMA command pusher, and
//! hardware dithering.

mod modes;
mod registers;
mod scan;

use modes::{BAYER_4X4, decode_argb, quantize_channel, yuv_to_argb};
pub use modes::{
    Channel, MARGO_VBE_MODES, MargoDisplay, PixelFormat, VbeMode, bytes_per_pixel, pixel_format,
    vbe_mode,
};
pub use registers::*;
pub use scan::{MARGO_FRAME_HZ, MargoScanTiming};

pub const MARGO_VRAM_SIZE: usize = 4 * 1024 * 1024;

const FILL_NS_PER_PIXEL: u64 = 5; // 200 Mpixels/s solid fill (section 1.1)
const COPY_NS_PER_PIXEL: u64 = 10; // 100 Mpixels/s screen-to-screen blit (section 1.1)
const EXPAND_NS_PER_PIXEL: u64 = 5; // 200 Mpixels/s color expand (section 1.1, fill class)
const LINE_NS_PER_PIXEL: u64 = 10; // 100 Mpixels/s, one pixel per clock (section 1.1)
const PATTERN_NS_PER_PIXEL: u64 = 5; // fill class, 200 Mpixels/s (section 1.1)
const BLIT_SETUP_NS: u64 = 100; // fixed per-operation setup, shared by all blits

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

struct PatternParams {
    dst_base: u32,
    dst_pitch: u32,
    pat_base: u32,
    depth: u32, // bytes per pixel: 1, 2, or 4
    dst_x: u32,
    dst_y: u32,
    width: u32,
    height: u32,
    rop: u8, // ROP3 code; P = pattern pixel, no source (S = 0)
    colorkey: u32,
    colorkey_en: bool,
    clip: Clip,
}

/// Fill the destination rectangle in `vram` by tiling the 8x8 pattern at
/// `pat_base`. The pattern is in the destination format with a row pitch of
/// `8 * depth` bytes; the pixel for destination `(x, y)` is `pattern[y % 8][x % 8]`
/// using absolute destination coordinates, so the phase is aligned to the surface
/// origin and adjacent fills tile seamlessly (section 7.4). The ROP3 code is
/// applied with P = the pattern pixel and S = 0 (PATTERN_FILL has no source,
/// section 7.6). With `colorkey_en`, a pattern pixel whose bytes equal `colorkey`
/// is skipped, so a hatch keys its background through. A pixel outside the clip
/// rectangle or the frame store, and a pattern byte range outside the store, are
/// skipped, not wrapped (section 8). `depth` outside {1, 2, 4} is a no-op. The
/// loop is bounded to `vram.len()` considered pixels and the offset math is
/// u64-saturating, so a pathological DIM cannot spin or overflow. Returns the
/// number of pixels written.
fn pattern(vram: &mut [u8], p: &PatternParams) -> u64 {
    if !matches!(p.depth, 1 | 2 | 4) {
        return 0;
    }
    let depth = p.depth as usize;
    let len = vram.len() as u64;
    let pat_pitch: u64 = 8 * depth as u64;
    let key = p.colorkey.to_le_bytes();
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
            // Pattern phase from absolute destination coordinates (& 7 == mod 8).
            let pat_off = (p.pat_base as u64)
                .saturating_add((y & 7).saturating_mul(pat_pitch))
                .saturating_add((x & 7).saturating_mul(depth as u64));
            if pat_off.saturating_add(depth as u64) > len {
                continue;
            }
            let pat_off = pat_off as usize;
            let mut pb = [0u8; 4];
            pb[..depth].copy_from_slice(&vram[pat_off..pat_off + depth]);
            if p.colorkey_en && pb[..depth] == key[..depth] {
                continue;
            }
            let ppix = u32::from_le_bytes(pb);
            let off = (p.dst_base as u64)
                .saturating_add(y.saturating_mul(p.dst_pitch as u64))
                .saturating_add(x.saturating_mul(depth as u64));
            if off.saturating_add(depth as u64) > len {
                continue;
            }
            written += 1;
            write_rop(vram, off as usize, depth, p.rop, ppix, 0);
        }
    }
    written
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Margo {
    vram: Vec<u8>,
    display: MargoDisplay,
    display_start_latch: u32,
    display_start_pending: bool,
    control: u32,
    blit: [u32; BLIT_REGS],
    command: u32,
    busy_ns: u64,
    /// Nanoseconds of the CURRENT batch that had already elapsed when the last
    /// write moved `busy_ns` -- the ORIGIN the busy time is measured from.
    ///
    /// It exists because the machine drains busy time once, at batch end, with
    /// the whole batch's nanoseconds. A blit armed partway into a batch was not
    /// running for the part before the arm, so that part must not be drained
    /// from it; `advance_busy` spends this credit first. Without it, dropping
    /// the arming write's batch break would over-drain by the in-batch offset
    /// and let software observe the engine idle before its modeled time had
    /// passed (`docs/vega/vega-technical-reference.md` section 9, first clause).
    ///
    /// BATCH-SCOPED SCRATCH, not device state: the host sets it only from a
    /// mid-batch offset and `advance_busy` consumes it as the batch's device
    /// time is delivered. It is zero once a batch's time has been delivered in
    /// FULL -- but NOT necessarily at every intermediate point, because
    /// `Machine::advance_master_time` can deliver one batch in several
    /// FDC-bounded steps and a blit armed late in the batch can outlast the
    /// first of them. Canonical capture runs between run calls, after full
    /// delivery, so excluding it stays sound the way `io_touched` is excluded.
    busy_credit_ns: u64,
    /// Bumped by every write to `busy_ns` that is an ARM (a COMMAND, the final
    /// MONO_DATA word, or a RESET) and never by the drain.
    ///
    /// It exists because the bus cannot infer an arm from the busy VALUE. Every
    /// setter below is an assign and nothing drains mid-batch, so two operations
    /// of identical modeled duration armed in the same batch leave `busy_ns`
    /// unchanged across the second one -- and a value comparison would report it
    /// as an ordinary write, leaving the drain credit naming the FIRST arm's
    /// instant. The second operation would then read idle for its whole length.
    /// That is not a corner case: izbios' `lfb_text` draws every glyph with a
    /// fixed `MG_DIM` of 0x00080008, so consecutive glyph blits model exactly
    /// the same busy time.
    busy_stamp: u64,
    expand: Option<ExpandState>,
    mono_data: u32,
    cursor: [u32; CURSOR_REGS],
    overlay: [u32; OVL_REGS],
    pusher: [u32; PUSH_REGS],
}

impl Default for Margo {
    fn default() -> Self {
        Self {
            vram: vec![0; MARGO_VRAM_SIZE],
            display: MargoDisplay::default(),
            display_start_latch: 0,
            display_start_pending: false,
            control: 0,
            blit: [0; BLIT_REGS],
            command: 0,
            busy_ns: 0,
            busy_credit_ns: 0,
            busy_stamp: 0,
            expand: None,
            mono_data: 0,
            cursor: [0; CURSOR_REGS],
            overlay: [0; OVL_REGS],
            pusher: [0; PUSH_REGS],
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
        self.display_start_latch = 0;
        self.display_start_pending = false;
        true
    }

    /// Queue a display origin for the next 60 Hz frame boundary. VBE callers use
    /// this checked path; direct MMIO writes retain the hardware's raw register
    /// behavior and may program an off-store address.
    pub fn program_display_start(&mut self, start: u32) -> bool {
        if !self.display_start_available(start) {
            return false;
        }
        self.display_start_latch = start;
        self.display_start_pending = true;
        true
    }

    pub fn display_start_available(&self, start: u32) -> bool {
        let visible_bytes = u64::from(self.display.pitch) * u64::from(self.display.height);
        visible_bytes > 0
            && u64::from(start).saturating_add(visible_bytes) <= self.vram.len() as u64
    }

    pub fn display_start_pending(&self) -> bool {
        self.display_start_pending
    }

    /// Apply a queued display origin when one or more frame boundaries elapsed.
    pub fn advance_frames(&mut self, frames: u64) {
        if frames > 0 && self.display_start_pending {
            self.display.start = self.display_start_latch;
            self.display_start_pending = false;
        }
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
        let pitch = self.display.pitch as u64;
        let bpp = self.display.bpp;
        let depth = bytes_per_pixel(bpp) as u64;
        let start = self.display.start as u64;
        let len = self.vram.len() as u64;
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height as u64 {
            for x in 0..width as u64 {
                // Saturating like the blit paths, so an extreme DISP_START or
                // pitch skips past the store rather than overflowing the offset.
                let off = start
                    .saturating_add(y.saturating_mul(pitch))
                    .saturating_add(x.saturating_mul(depth));
                let mut bytes = [0u8; 4];
                for (i, slot) in bytes.iter_mut().enumerate().take(depth as usize) {
                    let addr = off.saturating_add(i as u64);
                    if addr < len {
                        *slot = self.vram[addr as usize];
                    }
                }
                out.push(decode_argb(bpp, u32::from_le_bytes(bytes), palette));
            }
        }
        self.composite_overlay(&mut out);
        self.composite_cursor(&mut out, palette);
        out
    }

    /// Overlay the 64x64 two-plane hardware cursor onto the decoded scanout `out`
    /// (`width * height` ARGB pixels). No-op unless `CURSOR_CTRL` bit 0 is set. The
    /// bitmap at `CURSOR_ADDR` is the AND plane (512 bytes) then the XOR plane
    /// (`CURSOR_ADDR + 512`), each 64x64 at 1 bpp, 8 bytes per row, MSB first. The
    /// (AND, XOR) bits select per the section 7.7 table: (0,0) background color,
    /// (0,1) foreground color, (1,0) transparent, (1,1) the screen pixel inverted.
    /// `CURSOR_FG`/`CURSOR_BG` decode through the display format like a pixel.
    /// `CURSOR_POS` X/Y are signed 16-bit, so the cursor can run off the top/left;
    /// pixels outside the screen are clipped. An off-store plane byte skips that
    /// cursor pixel (transparent), never wraps.
    fn composite_cursor(&self, out: &mut [u32], palette: &[u32; 256]) {
        if self.cursor_reg(REG_CURSOR_CTRL) & 0x1 == 0 {
            return;
        }
        let width = self.display.width as i32;
        let height = self.display.height as i32;
        let bpp = self.display.bpp;
        let fg = decode_argb(bpp, self.cursor_reg(REG_CURSOR_FG), palette);
        let bg = decode_argb(bpp, self.cursor_reg(REG_CURSOR_BG), palette);
        let addr = self.cursor_reg(REG_CURSOR_ADDR) as u64;
        let pos = self.cursor_reg(REG_CURSOR_POS);
        let pos_x = (pos & 0xffff) as u16 as i16 as i32;
        let pos_y = (pos >> 16) as u16 as i16 as i32;
        let len = self.vram.len() as u64;
        for cy in 0..64i32 {
            for cx in 0..64i32 {
                let sx = pos_x + cx;
                let sy = pos_y + cy;
                if sx < 0 || sx >= width || sy < 0 || sy >= height {
                    continue;
                }
                let byte = (cy as u64) * 8 + (cx as u64) / 8;
                let mask = 0x80u8 >> (cx & 7);
                let and_off = addr.saturating_add(byte);
                let xor_off = addr.saturating_add(512).saturating_add(byte);
                if and_off >= len || xor_off >= len {
                    continue; // off-store plane byte: skip (transparent), no wrap
                }
                let and_bit = self.vram[and_off as usize] & mask != 0;
                let xor_bit = self.vram[xor_off as usize] & mask != 0;
                let idx = (sy as usize) * (width as usize) + sx as usize;
                out[idx] = match (and_bit, xor_bit) {
                    (false, false) => bg,
                    (false, true) => fg,
                    (true, false) => continue, // transparent: leave the screen pixel
                    (true, true) => !out[idx] & 0x00ff_ffff, // invert the screen pixel
                };
            }
        }
    }

    /// Composite the scaled YUV video overlay onto the decoded scanout `out`
    /// (`width * height` ARGB pixels), before the cursor so the pointer stays on
    /// top (section 7.8). No-op unless `OVL_CTRL` bit 0 (ENABLE) is set. Within the
    /// destination rectangle (`OVL_DST_XY` / `OVL_DST_DIM`) the source is sampled
    /// scaled from `OVL_SRC_DIM` by point sampling (section 9), converted by
    /// `yuv_to_argb`, and written. Destination pixels off the screen are skipped,
    /// not wrapped; every source read is bounds-checked against the frame store and
    /// an off-store byte skips that overlay pixel. Zero source or destination
    /// extent is a no-op (guards the scale divide).
    fn composite_overlay(&self, out: &mut [u32]) {
        let ctrl = self.overlay_reg(REG_OVL_CTRL);
        if ctrl & 0x1 == 0 {
            return;
        }
        let format = (ctrl >> 1) & 0x3;
        let key_en = ctrl & 0x8 != 0;
        let colorkey = self.overlay_reg(REG_OVL_COLORKEY);

        let width = self.display.width as u64;
        let height = self.display.height as u64;
        let dst_xy = self.overlay_reg(REG_OVL_DST_XY);
        let dst_dim = self.overlay_reg(REG_OVL_DST_DIM);
        let src_dim = self.overlay_reg(REG_OVL_SRC_DIM);
        let dst_x = (dst_xy & 0xffff) as u64;
        let dst_y = (dst_xy >> 16) as u64;
        let dst_w = (dst_dim & 0xffff) as u64;
        let dst_h = (dst_dim >> 16) as u64;
        let src_w = (src_dim & 0xffff) as u64;
        let src_h = (src_dim >> 16) as u64;
        if width == 0 || height == 0 || dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
            return;
        }
        let len = self.vram.len() as u64;

        for dy in 0..dst_h {
            let screen_y = dst_y + dy;
            if screen_y >= height {
                continue;
            }
            for dx in 0..dst_w {
                let screen_x = dst_x + dx;
                if screen_x >= width {
                    continue;
                }
                if key_en && !self.primary_keyed(screen_x, screen_y, colorkey) {
                    continue; // occluded: another surface painted over the key here
                }
                let sx = (dx * src_w) / dst_w;
                let sy = (dy * src_h) / dst_h;
                let Some((y, u, v)) = self.sample_overlay(format, sx, sy, len) else {
                    continue;
                };
                out[(screen_y * width + screen_x) as usize] =
                    self.present_overlay_pixel(yuv_to_argb(y, u, v), screen_x, screen_y);
            }
        }
    }

    /// Present one composited overlay ARGB pixel on the current display. On a 15 or
    /// 16-bit display each channel is reduced to its display width (ordered-dithered
    /// when `CONTROL.DITHER_EN` is set, truncated otherwise) and bit-expanded back to
    /// 8 bits, so a real 15/16-bit display's banding (and its absence under dither)
    /// is reproduced. On 8-bit (indexed) and 32-bit displays no precision is lost, so
    /// the pixel passes through unchanged.
    fn present_overlay_pixel(&self, argb: u32, x: u64, y: u64) -> u32 {
        let Some(fmt) = pixel_format(self.display.bpp) else {
            return argb; // 8-bit indexed: no direct-color reduction
        };
        if fmt.r.size >= 8 {
            return argb; // 32-bit: no precision lost
        }
        let dither = self.control & 0x2 != 0; // CONTROL.DITHER_EN (bit 1)
        let cell = BAYER_4X4[(y & 3) as usize][(x & 3) as usize];
        let r = quantize_channel((argb >> 16) & 0xff, fmt.r.size, cell, dither);
        let g = quantize_channel((argb >> 8) & 0xff, fmt.g.size, cell, dither);
        let b = quantize_channel(argb & 0xff, fmt.b.size, cell, dither);
        (r << 16) | (g << 8) | b
    }

    /// True when the raw primary pixel at screen `(x, y)` equals `colorkey` in its
    /// low `depth` bytes (display format, little-endian), computed with the same
    /// offset math as `scanout_argb` and bounds-checked. Mirrors COPY's color key
    /// (section 7.5): the overlay shows only where the application painted the key,
    /// and is hidden where another window drew over it.
    fn primary_keyed(&self, x: u64, y: u64, colorkey: u32) -> bool {
        let depth = bytes_per_pixel(self.display.bpp) as u64;
        let pitch = self.display.pitch as u64;
        let start = self.display.start as u64;
        let len = self.vram.len() as u64;
        let off = start
            .saturating_add(y.saturating_mul(pitch))
            .saturating_add(x.saturating_mul(depth));
        let mut bytes = [0u8; 4];
        for (i, slot) in bytes.iter_mut().enumerate().take(depth as usize) {
            let addr = off.saturating_add(i as u64);
            if addr < len {
                *slot = self.vram[addr as usize];
            }
        }
        colorkey.to_le_bytes()[..depth as usize] == bytes[..depth as usize]
    }

    /// Fetch the (Y, U, V) triple for source pixel `(sx, sy)` in the configured
    /// overlay `format` (0 YUY2, 1 YV12; others reserved). Returns None when a
    /// needed source byte falls outside the frame store (skip the pixel, no wrap)
    /// or the format is reserved. All offsets are u64-saturating.
    fn sample_overlay(&self, format: u32, sx: u64, sy: u64, len: u64) -> Option<(u8, u8, u8)> {
        let src_y = self.overlay_reg(REG_OVL_SRC_Y) as u64;
        let pitch = self.overlay_reg(REG_OVL_SRC_PITCH) as u64;
        match format {
            0 => {
                // YUY2 packed 4:2:2, byte order Y0, U, Y1, V; a 4-byte group is two
                // horizontally adjacent pixels sharing one U and one V.
                let base = src_y
                    .saturating_add(sy.saturating_mul(pitch))
                    .saturating_add((sx / 2).saturating_mul(4));
                let y = self.vram_byte(base.saturating_add((sx & 1) * 2), len)?;
                let u = self.vram_byte(base.saturating_add(1), len)?;
                let v = self.vram_byte(base.saturating_add(3), len)?;
                Some((y, u, v))
            }
            1 => {
                // YV12 planar 4:2:0: a full-resolution Y plane, then V and U planes
                // at half width and half height. The register set carries no chroma
                // pitch, so it is the Y pitch halved. Chroma is
                // upsampled by point sampling: (sx/2, sy/2) addresses both planes.
                let y = self.vram_byte(
                    src_y
                        .saturating_add(sy.saturating_mul(pitch))
                        .saturating_add(sx),
                    len,
                )?;
                let cpitch = pitch / 2;
                let cx = sx / 2;
                let cy = sy / 2;
                let u_base = self.overlay_reg(REG_OVL_SRC_U) as u64;
                let v_base = self.overlay_reg(REG_OVL_SRC_V) as u64;
                let u = self.vram_byte(
                    u_base
                        .saturating_add(cy.saturating_mul(cpitch))
                        .saturating_add(cx),
                    len,
                )?;
                let v = self.vram_byte(
                    v_base
                        .saturating_add(cy.saturating_mul(cpitch))
                        .saturating_add(cx),
                    len,
                )?;
                Some((y, u, v))
            }
            _ => None,
        }
    }

    /// Read one frame-store byte at `off`, or None when it falls at or past the end
    /// of the store (the overlay then skips that pixel rather than wrapping).
    fn vram_byte(&self, off: u64, len: u64) -> Option<u8> {
        if off < len {
            Some(self.vram[off as usize])
        } else {
            None
        }
    }

    fn register_u32(&self, reg: usize, elapsed_ns: u64) -> u32 {
        match reg {
            REG_ID => MARGO_ID_VALUE,
            REG_CAPS => MARGO_CAPS_VALUE,
            REG_STATUS => u32::from(self.status_busy_after(elapsed_ns)), // bit 0: BUSY
            REG_CONTROL => self.control,
            REG_DISP_MODE => u32::from(self.display.mode),
            REG_DISP_WIDTH => self.display.width,
            REG_DISP_HEIGHT => self.display.height,
            REG_DISP_BPP => self.display.bpp,
            REG_DISP_PITCH => self.display.pitch,
            REG_DISP_START => self.display_start_latch,
            reg if (CURSOR_BASE..CURSOR_BASE + CURSOR_REGS * 4).contains(&reg) => {
                self.cursor[(reg - CURSOR_BASE) / 4]
            }
            reg if (OVL_BASE..OVL_BASE + OVL_REGS * 4).contains(&reg) => {
                self.overlay[(reg - OVL_BASE) / 4]
            }
            reg if (PUSH_BASE_REG..PUSH_BASE_REG + PUSH_REGS * 4).contains(&reg) => {
                self.pusher[(reg - PUSH_BASE_REG) / 4]
            }
            reg if (BLIT_BASE..BLIT_BASE + BLIT_REGS * 4).contains(&reg) => {
                self.blit[(reg - BLIT_BASE) / 4]
            }
            _ => 0,
        }
    }

    pub fn read_mmio_u8(&self, offset: usize) -> u8 {
        self.read_mmio_u8_at(offset, 0)
    }

    /// Read a register as of `elapsed_ns` nanoseconds into the current batch.
    ///
    /// Only STATUS is time-dependent, and it is the reason this form exists: the
    /// machine advances Margo once, at batch end, so a `read_mmio_u8` taken
    /// partway through a batch would report the BUSY the engine had when the
    /// batch STARTED. The guest's blit wait is an MMIO spin, which cannot end
    /// its own batch, so that staleness is exactly what it would read. This is
    /// the same lazy peek `Counter::count_after` and `OplChip::status_after`
    /// give the PIT and the OPL; `elapsed_ns == 0` reduces to the live state.
    pub fn read_mmio_u8_at(&self, offset: usize, elapsed_ns: u64) -> u8 {
        let reg = offset & !0x3;
        let byte = offset & 0x3;
        (self.register_u32(reg, elapsed_ns) >> (8 * byte)) as u8
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
                self.arm_busy_ns(0);
                self.busy_credit_ns = 0;
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
        if (CURSOR_BASE..CURSOR_BASE + CURSOR_REGS * 4).contains(&reg) {
            let slot = &mut self.cursor[(reg - CURSOR_BASE) / 4];
            *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            return;
        }
        if (OVL_BASE..OVL_BASE + OVL_REGS * 4).contains(&reg) {
            let slot = &mut self.overlay[(reg - OVL_BASE) / 4];
            *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            return;
        }
        if (PUSH_BASE_REG..PUSH_BASE_REG + PUSH_REGS * 4).contains(&reg) {
            // PUSH_GET (0x0090) is read-only to the bus; the engine owns it.
            if reg != REG_PUSH_GET {
                let slot = &mut self.pusher[(reg - PUSH_BASE_REG) / 4];
                *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            }
            return;
        }
        if reg == REG_DISP_START {
            let slot = &mut self.display_start_latch;
            *slot = (*slot & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            self.display_start_pending = true;
        }
    }

    fn blit_reg(&self, offset: usize) -> u32 {
        self.blit[(offset - BLIT_BASE) / 4]
    }

    fn cursor_reg(&self, offset: usize) -> u32 {
        self.cursor[(offset - CURSOR_BASE) / 4]
    }

    fn overlay_reg(&self, offset: usize) -> u32 {
        self.overlay[(offset - OVL_BASE) / 4]
    }

    /// Charge `ns` of modeled busy time as a NEW operation, bumping the arm
    /// stamp with it.
    ///
    /// Every arming path goes through here rather than assigning `busy_ns`
    /// directly, so the stamp cannot fall out of step with the field: a new
    /// operation whose duration happens to equal the running one's is still a
    /// new arm, and `Vega::write_memory_u8` can only see that through the stamp.
    /// The drain (`advance_busy`) deliberately does NOT come through here.
    fn arm_busy_ns(&mut self, ns: u64) {
        self.busy_ns = ns;
        self.busy_stamp = self.busy_stamp.wrapping_add(1);
    }

    /// The arm stamp: changes exactly when an operation is armed (or reset).
    /// The bus compares this across an MMIO write to detect the arming edge.
    pub fn busy_stamp(&self) -> u64 {
        self.busy_stamp
    }

    /// The remaining modeled busy time in nanoseconds. The pusher gates on this so
    /// it stalls at each COMMAND until that operation completes; an armed but unfed
    /// color-expand stream reads 0 here (busy_ns is only set when an operation
    /// completes), so the pusher keeps feeding its MONO_DATA words.
    pub fn busy_ns(&self) -> u64 {
        self.busy_ns
    }

    /// The modeled busy time remaining `elapsed_ns` nanoseconds from now, without
    /// draining anything. `busy_ns_after(0)` IS `busy_ns()`.
    ///
    /// The credit is subtracted from the elapsed time, not added to the busy
    /// time: a blit armed at in-batch offset `a` has only been running for
    /// `elapsed - a`, and the arm-time drain credit (`busy_credit_ns`) is
    /// exactly `a`.
    pub fn busy_ns_after(&self, elapsed_ns: u64) -> u64 {
        self.busy_ns
            .saturating_sub(elapsed_ns.saturating_sub(self.busy_credit_ns))
    }

    /// STATUS.BUSY as of `elapsed_ns` into the batch: the time-draining term
    /// peeked, OR the armed-but-unfed color-expand term, which is not
    /// time-derived at all (it waits on guest MONO_DATA writes) and so passes
    /// through untransformed. `Vega::blitter_busy_ns` deliberately excludes that
    /// second term from the DEADLINE for the same reason -- it would never come
    /// due -- so the two must not be unified.
    pub fn status_busy_after(&self, elapsed_ns: u64) -> bool {
        self.busy_ns_after(elapsed_ns) > 0 || self.expand.is_some()
    }

    /// Record that the write which just moved `busy_ns` landed `elapsed_ns`
    /// nanoseconds into the current batch, so the batch-end drain does not bill
    /// the new operation for time that passed before it started.
    ///
    /// ASSIGN, never accumulate: every busy setter in this file is an `=`, so
    /// the LATEST write is the new origin, whether it raised busy time (a
    /// COMMAND, or the final MONO_DATA word of a color-expand stream) or lowered
    /// it (a RESET while an earlier blit was still draining).
    pub fn credit_busy_ns(&mut self, elapsed_ns: u64) {
        self.busy_credit_ns = elapsed_ns;
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
            0x06 => self.run_pattern(),
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
        self.arm_busy_ns(BLIT_SETUP_NS + pixels * FILL_NS_PER_PIXEL);
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
        self.arm_busy_ns(BLIT_SETUP_NS + pixels * COPY_NS_PER_PIXEL);
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
        self.arm_busy_ns(BLIT_SETUP_NS + pixels * EXPAND_NS_PER_PIXEL);
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
        self.arm_busy_ns(BLIT_SETUP_NS + pixels * LINE_NS_PER_PIXEL);
    }

    fn run_pattern(&mut self) {
        let dst_xy = self.blit_reg(REG_DST_XY);
        let dim = self.blit_reg(REG_DIM);
        let params = PatternParams {
            dst_base: self.blit_reg(REG_DST_BASE),
            dst_pitch: self.blit_reg(REG_DST_PITCH),
            pat_base: self.blit_reg(REG_PAT_BASE),
            depth: self.blit_reg(REG_DEPTH),
            dst_x: dst_xy & 0xffff,
            dst_y: dst_xy >> 16,
            width: dim & 0xffff,
            height: dim >> 16,
            rop: self.blit_reg(REG_ROP) as u8,
            colorkey: self.blit_reg(REG_COLORKEY),
            colorkey_en: self.blit_reg(REG_FLAGS) & 0x1 != 0,
            clip: self.build_clip(),
        };
        let pixels = pattern(&mut self.vram, &params);
        self.arm_busy_ns(BLIT_SETUP_NS + pixels * PATTERN_NS_PER_PIXEL);
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
            self.arm_busy_ns(BLIT_SETUP_NS + state.written * EXPAND_NS_PER_PIXEL);
            self.expand = None;
        } else {
            self.expand = Some(state);
        }
    }

    /// Drain `ns` nanoseconds of modeled busy time. The machine calls this at
    /// batch end, converting machine clocks to nanoseconds.
    ///
    /// Any arm-time credit is spent FIRST: those nanoseconds elapsed before the
    /// current operation was armed, so they are not time it was running. The
    /// `min` matters -- `advance_master_time` can deliver one batch here in
    /// several FDC-bounded steps, and the credit has to survive being consumed
    /// across them rather than being written off whole on the first call.
    pub fn advance_busy(&mut self, ns: u64) {
        let credit = self.busy_credit_ns.min(ns);
        self.busy_credit_ns -= credit;
        self.busy_ns = self.busy_ns.saturating_sub(ns - credit);
    }
}

#[cfg(test)]
#[path = "margo_test.rs"]
mod tests;
