//! Margo, the VEGA 2D engine: the display register block, the linear frame
//! buffer, and the blit engine. The engine implements FILL and COPY.

pub const MARGO_VRAM_SIZE: usize = 4 * 1024 * 1024;
pub const MARGO_MMIO_SIZE: usize = 0x0001_0000; // 64 KB register block
pub const MARGO_ID_VALUE: u32 = 0x4D47_0100; // 'M' 'G', version 1.00
pub const MARGO_CAPS_VALUE: u32 = 0x0000_0043; // bits 0 FILL, 1 COPY, 6 COLORKEY

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbeMode {
    pub number: u16,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

/// The modes Margo lists, reports, and sets. Slice 2b implements the 8-bit
/// indexed modes only; hi-color modes arrive in a later slice.
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
];

pub fn vbe_mode(number: u16) -> Option<VbeMode> {
    MARGO_VBE_MODES
        .iter()
        .copied()
        .find(|mode| mode.number == number)
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
pub const REG_COMMAND: usize = 0x0150;

const BLIT_BASE: usize = 0x0100;
const BLIT_REGS: usize = 20; // 0x100..0x150, twenty 32-bit slots; COMMAND at 0x150 is handled separately
const FILL_NS_PER_PIXEL: u64 = 5; // 200 Mpixels/s solid fill (section 1.1)
const COPY_NS_PER_PIXEL: u64 = 10; // 100 Mpixels/s screen-to-screen blit (section 1.1)
const EXPAND_NS_PER_PIXEL: u64 = 5; // 200 Mpixels/s color expand (section 1.1, fill class)
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

struct FillParams {
    dst_base: u32,
    dst_pitch: u32,
    depth: u32, // bytes per pixel: 1, 2, or 4
    dst_x: u32,
    dst_y: u32,
    width: u32,
    height: u32,
    fg_color: u32,
    rop: u8, // 0xF0 PATCOPY (solid), 0x5A PATINVERT (XOR), others treated as solid
}

/// Fill a rectangle in `vram` from the latched parameters. Returns the number of
/// pixels actually written inside the frame store; for a fill that fits, that is
/// the rectangle area. Off-store pixels are skipped, not wrapped (section 8).
/// `depth` outside {1, 2, 4} is a no-op. The loop is bounded to `vram.len()`
/// considered pixels, so a pathological DIM cannot spin, and the offset math is
/// done in u64 with saturating arithmetic so extreme coordinates skip rather
/// than overflow.
fn fill(vram: &mut [u8], p: &FillParams) -> u64 {
    if !matches!(p.depth, 1 | 2 | 4) {
        return 0;
    }
    let depth = p.depth as usize;
    let fg = p.fg_color.to_le_bytes();
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
            let offset = (p.dst_base as u64)
                .saturating_add(y.saturating_mul(p.dst_pitch as u64))
                .saturating_add(x.saturating_mul(depth as u64));
            if offset.saturating_add(depth as u64) > len {
                continue;
            }
            written += 1;
            let offset = offset as usize;
            if p.rop == 0x5a {
                for b in 0..depth {
                    vram[offset + b] ^= fg[b];
                }
            } else {
                vram[offset..offset + depth].copy_from_slice(&fg[..depth]);
            }
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
    colorkey: u32,
    colorkey_en: bool,
}

/// Copy a source rectangle to a destination rectangle in `vram`, SRCCOPY.
/// Returns the number of pixels actually written (in-bounds on both sides and
/// not keyed out). A source or destination pixel whose byte range falls outside
/// the frame store is skipped, not wrapped (section 8). `depth` outside {1, 2, 4}
/// is a no-op. The loop is bounded to `vram.len()` considered pixels, and the
/// offset math is u64-saturating, so a pathological or adversarial rectangle
/// cannot spin or overflow. Traversal direction is chosen from the coordinates so
/// overlapping copies stay correct (section 7.4).
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
            let src_off = (p.src_base as u64)
                .saturating_add((p.src_y as u64 + row as u64).saturating_mul(p.src_pitch as u64))
                .saturating_add((p.src_x as u64 + col as u64).saturating_mul(depth as u64));
            let dst_off = (p.dst_base as u64)
                .saturating_add((p.dst_y as u64 + row as u64).saturating_mul(p.dst_pitch as u64))
                .saturating_add((p.dst_x as u64 + col as u64).saturating_mul(depth as u64));
            if src_off.saturating_add(depth as u64) > len
                || dst_off.saturating_add(depth as u64) > len
            {
                continue;
            }
            let (src_off, dst_off) = (src_off as usize, dst_off as usize);
            if p.colorkey_en && vram[src_off..src_off + depth] == key[..depth] {
                continue;
            }
            written += 1;
            vram.copy_within(src_off..src_off + depth, dst_off);
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
}

/// Write one expanded destination pixel. `set` chooses FG vs BG; a clear bit
/// under EXPAND_TRANSPARENT is skipped. A pixel whose byte range falls outside
/// the frame store is skipped, not wrapped (section 8). Returns true if a pixel
/// was written.
fn put_expand_pixel(vram: &mut [u8], p: &ExpandParams, x: u64, y: u64, set: bool) -> bool {
    if !set && p.transparent {
        return false;
    }
    let depth = p.depth as usize;
    let color = if set { p.fg_color } else { p.bg_color };
    let bytes = color.to_le_bytes();
    let off = (p.dst_base as u64)
        .saturating_add(y.saturating_mul(p.dst_pitch as u64))
        .saturating_add(x.saturating_mul(depth as u64));
    if off.saturating_add(depth as u64) > vram.len() as u64 {
        return false;
    }
    let off = off as usize;
    vram[off..off + depth].copy_from_slice(&bytes[..depth]);
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Margo {
    vram: Vec<u8>,
    display: MargoDisplay,
    control: u32,
    blit: [u32; BLIT_REGS],
    command: u32,
    busy_ns: u64,
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
            pitch: mode.width * mode.bpp / 8,
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

    fn register_u32(&self, reg: usize) -> u32 {
        match reg {
            REG_ID => MARGO_ID_VALUE,
            REG_CAPS => MARGO_CAPS_VALUE,
            REG_STATUS => u32::from(self.busy_ns > 0), // bit 0: BUSY
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
        if reg == REG_CONTROL {
            self.control = (self.control & !(0xff_u32 << shift)) | (u32::from(value) << shift);
            if self.control & 0x1 != 0 {
                // RESET aborts the operation. It already completed, so this only
                // drops BUSY. The bit is self-clearing.
                self.busy_ns = 0;
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

    fn run_command(&mut self) {
        match self.command & 0xff {
            0x01 => self.run_fill(),
            0x02 => self.run_copy(),
            0x04 => self.run_expand_mem(),
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
            colorkey: self.blit_reg(REG_COLORKEY),
            colorkey_en: self.blit_reg(REG_FLAGS) & 0x1 != 0,
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
            },
            src_base: self.blit_reg(REG_SRC_BASE),
            src_pitch: self.blit_reg(REG_SRC_PITCH),
            src_x: src_xy & 0xffff,
            src_y: src_xy >> 16,
        };
        let pixels = color_expand_mem(&mut self.vram, &params);
        self.busy_ns = BLIT_SETUP_NS + pixels * EXPAND_NS_PER_PIXEL;
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
        assert!(!margo.set_mode(0x111)); // 640x480x16, not implemented in this slice
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
    fn caps_reports_fill_copy_and_colorkey() {
        let margo = Margo::default();
        // bit 0 FILL, bit 1 COPY, bit 6 COLORKEY.
        assert_eq!(read_reg_u32(&margo, REG_CAPS), 0x0000_0043);
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0x05,
            colorkey_en: true,
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
            colorkey: 0x1234,
            colorkey_en: true,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
            colorkey: 0,
            colorkey_en: false,
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
        write_reg(&mut margo, REG_COMMAND, 0x04);

        // 4 pixels -> busy_ns = 100 + 4*5 = 120. One ns short still reads busy.
        margo.advance_busy(119);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
        margo.advance_busy(1);
        assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
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
            colorkey: 0,
            colorkey_en: false,
        };
        copy(&mut vram, &p);
        // dst row 1 (offsets 4,5,6) = src row 0 [1,2,3]; dst row 2 (offsets 8,9,10)
        // = src row 1 [4,5,6], uncorrupted by the row overlap.
        assert_eq!(&vram[4..7], &[1, 2, 3]);
        assert_eq!(&vram[8..11], &[4, 5, 6]);
    }
}
