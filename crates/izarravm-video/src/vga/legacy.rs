// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

/// CGA graphics framebuffer size: 16 KiB at B800:0000. Two 8000-byte banks
/// (100 scanlines x 80 bytes each) hold the even and odd scanlines.
pub const CGA_FB_SIZE: usize = 16 * 1024;
/// Byte offset of the odd-scanline bank inside the CGA framebuffer. Even
/// scanlines (0, 2, 4, ...) live at 0x0000; odd scanlines (1, 3, 5, ...) at
/// 0x2000. Each bank is 8000 bytes (100 lines x 80 bytes per line).
pub const CGA_ODD_BANK: usize = 0x2000;
/// Standard CGA graphics bytes per scanline. Register-banged horizontal modes
/// derive the live pitch from `hdisp_end` instead.
pub const CGA_BYTES_PER_LINE: usize = 80;

/// The CGA graphics submode the B800 framebuffer is decoded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgaMode {
    /// 320x200, 4 colors, 2 bits per pixel (INT 10h modes 04h and 05h).
    Graphics320x200,
    /// 640x200, 2 colors, 1 bit per pixel (INT 10h mode 06h).
    Graphics640x200,
}

/// CGA graphics state: the framebuffer plus the two control registers the CGA
/// exposes (mode control 0x3D8 and color select 0x3D9). The mode-control
/// register drives the CGA text/graphics personality and blanking; color decode
/// reads `color_select`.
#[derive(Debug, Clone)]
pub struct Cga {
    pub fb: Vec<u8>,
    pub submode: CgaMode,
    /// INT 10h mode number (04h, 05h, or 06h). Mode 05h forces the alternate
    /// red/cyan/white palette regardless of the color-select palette bit.
    pub bios_mode: u8,
    pub mode_control: u8, // port 0x3D8 output latch
    pub color_select: u8, // port 0x3D9
    pub light_pen_triggered: bool,
    pub light_pen_latch: u16,
    pub light_pen_pixel_col: u16,
    pub light_pen_pixel_row: u16,
}

impl Default for Cga {
    fn default() -> Self {
        Self {
            fb: vec![0; CGA_FB_SIZE],
            submode: CgaMode::Graphics320x200,
            bios_mode: 0x04,
            mode_control: 0x00,
            color_select: 0x00,
            light_pen_triggered: false,
            light_pen_latch: 0,
            light_pen_pixel_col: 0,
            light_pen_pixel_row: 0,
        }
    }
}

// Hercules Mode Control register (port 3B8h) bits (seasip.info "Hercules
// Graphics Card Plus Notes"): bit 1 selects graphics vs text, bit 3 gates
// video output, bit 5 picks blink vs high-intensity background in text mode,
// bit 7 picks which 32K page (B0000 or B8000) the CRTC scans out.
pub(super) const HGC_MODE_GRAPHICS: u8 = 0x02;
pub(super) const HGC_MODE_VIDEO_ENABLE: u8 = 0x08;
pub(super) const HGC_MODE_PAGE1: u8 = 0x80;

// Hercules Configuration Switch register (port 3BFh, write-only): bit 0
// allows the Mode Control register's graphics bit to take effect and unlocks
// the B1000h-B7FFFh half of the first page; bit 1 pages the second 32K bank
// in at B8000h. A real HGC's graphics mode is refused (stays text) unless the
// guest has first unlocked it here -- this is the two-step "configure then
// switch" sequence Hercules software issues before painting graphics.
const HGC_CONFIG_ALLOW_GRAPHICS: u8 = 0x01;
const HGC_CONFIG_ENABLE_PAGE1: u8 = 0x02;

/// Hercules graphics state: the two 32K pages (B0000 and B8000) plus the
/// Mode Control (3B8h) and Configuration Switch (3BFh) latches. Mirrors `Cga`
/// for the third legacy personality this raster core hosts.
#[derive(Debug, Clone)]
pub struct Hgc {
    /// Both 32K pages back to back: page 0 (B0000) at offset 0, page 1
    /// (B8000) at offset 0x8000. Real hardware only backs page 1 with RAM
    /// when a full (non-"Plus") HGC or an HGC+ with the RAM option is fitted;
    /// this core always backs it so a guest that pages it in before checking
    /// for it (a common detection shortcut) sees ordinary RAM, not open bus.
    pub fb: Vec<u8>,
    pub mode_control: u8,  // port 0x3B8 output latch
    pub config_switch: u8, // port 0x3BF output latch
}

pub const HGC_FB_SIZE: usize = 32 * 1024;
pub const HGC_PAGE1_OFFSET: usize = 0x8000;
/// Byte offset of interleave bank N (0..=3) inside one 32K Hercules page.
/// Scanline `y` maps to bank `y & 3` at `(y & 3) * HGC_BANK_SIZE`, the
/// four-way generalization of CGA's two-bank even/odd interleave.
pub const HGC_BANK_SIZE: usize = 0x2000;
/// Hercules graphics bytes per scanline: 720 pixels / 8 bits-per-byte.
pub const HGC_BYTES_PER_LINE: usize = 90;

impl Default for Hgc {
    fn default() -> Self {
        Self {
            fb: vec![0; HGC_FB_SIZE * 2],
            mode_control: 0x00,
            config_switch: 0x00,
        }
    }
}

impl Hgc {
    pub(super) fn graphics_allowed(&self) -> bool {
        self.config_switch & HGC_CONFIG_ALLOW_GRAPHICS != 0
    }

    pub(super) fn page1_enabled(&self) -> bool {
        self.config_switch & HGC_CONFIG_ENABLE_PAGE1 != 0
    }

    /// Which 32K page (0 or 1) the CRTC currently scans out: Mode Control bit
    /// 7, but a page-1 select only takes effect once 3BFh has paged it in
    /// (real hardware: the second bank is simply not there otherwise).
    pub(super) fn active_page(&self) -> usize {
        if self.mode_control & HGC_MODE_PAGE1 != 0 && self.page1_enabled() {
            1
        } else {
            0
        }
    }
}

/// The 16 EGA/CGA color numbers as DAC indices. On the stock VGA palette the
/// first 16 entries are the EGA colors, so a CGA color number is its own DAC
/// index. Named for the four-color and two-color palette tables below.
pub(super) const CGA_BLACK: u8 = 0;
pub(super) const CGA_GREEN: u8 = 2;
pub(super) const CGA_CYAN: u8 = 3;
pub(super) const CGA_RED: u8 = 4;
pub(super) const CGA_MAGENTA: u8 = 5;
pub(super) const CGA_BROWN: u8 = 6;
pub(super) const CGA_LIGHT_GRAY: u8 = 7;
pub(super) const CGA_LIGHT_GREEN: u8 = 10;
pub(super) const CGA_LIGHT_CYAN: u8 = 11;
pub(super) const CGA_LIGHT_RED: u8 = 12;
pub(super) const CGA_LIGHT_MAGENTA: u8 = 13;
pub(super) const CGA_YELLOW: u8 = 14;
pub(super) const CGA_WHITE: u8 = 15;

impl Cga {
    /// The three foreground colors (pixel values 1, 2, 3) for 320x200x4, decoded
    /// from the color-select register (port 0x3D9). Bit 5 selects palette 1
    /// (cyan/magenta/white) over palette 0 (green/red/brown); bit 4 brightens all
    /// three to their light variants. Mode 05h overrides the palette to the fixed
    /// cyan/red/white set (IBM CGA / DOSBox), still honoring the intensity bit.
    /// Pixel value 0 is the background/border from `background_index`.
    fn palette_320x200(&self) -> [u8; 3] {
        let intensity = self.color_select & 0x10 != 0;
        if self.bios_mode == 0x05 {
            // Alternate palette: cyan / red / white.
            return if intensity {
                [CGA_LIGHT_CYAN, CGA_LIGHT_RED, CGA_WHITE]
            } else {
                [CGA_CYAN, CGA_RED, CGA_LIGHT_GRAY]
            };
        }
        let palette1 = self.color_select & 0x20 != 0;
        match (palette1, intensity) {
            (false, false) => [CGA_GREEN, CGA_RED, CGA_BROWN],
            (false, true) => [CGA_LIGHT_GREEN, CGA_LIGHT_RED, CGA_YELLOW],
            (true, false) => [CGA_CYAN, CGA_MAGENTA, CGA_LIGHT_GRAY],
            (true, true) => [CGA_LIGHT_CYAN, CGA_LIGHT_MAGENTA, CGA_WHITE],
        }
    }

    /// The background/border color (pixel value 0 in 320x200x4, the 0 bit in
    /// 640x200x2): color-select bits 0-3 with bit 4 as the intensity bit, a full
    /// 4-bit CGA color number, which is its own DAC index on the stock palette.
    pub(super) fn background_index(&self) -> u8 {
        self.color_select & 0x0F
    }

    /// The foreground color for the 1 bits in 640x200x2: color-select bits 0-3,
    /// the same field as the background nibble. The background is always black.
    pub(super) fn foreground_640x200(&self) -> u8 {
        self.color_select & 0x0F
    }

    /// Decode the four DAC indices a 320x200x4 framebuffer byte holds, MSB-first:
    /// bits 7-6 are pixel 0, 5-4 pixel 1, 3-2 pixel 2, 1-0 pixel 3. Value 0 is the
    /// background; values 1-3 select from the active four-color palette.
    pub(super) fn decode_byte_320x200(&self, byte: u8) -> [u8; 4] {
        let bg = self.background_index();
        let fg = self.palette_320x200();
        let mut out = [0u8; 4];
        for (px, slot) in out.iter_mut().enumerate() {
            let shift = 6 - px * 2;
            let value = (byte >> shift) & 0x03;
            *slot = if value == 0 {
                bg
            } else {
                fg[(value - 1) as usize]
            };
        }
        out
    }
}
