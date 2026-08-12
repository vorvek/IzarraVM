// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

/// CRTC vertical/horizontal timing, in scan-counter units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrtcTiming {
    pub htotal_chars: u32,
    pub char_width: u32, // 8 or 9, from Sequencer Clocking Mode
    pub hdisp_end: u32,  // dots
    pub vtotal: u32,
    pub vdisp_end: u32,
    pub vblank_start: u32,
    pub vblank_end: u32,
    pub vretrace_start: u32,
    pub vretrace_end: u32,
    pub max_scan: u32,
    pub double_scan: bool,
    pub start_address: u32,
    pub offset: u32,
    pub mode_control: u8,    // CRTC index 17h
    pub underline_loc: u8,   // CRTC index 14h
    pub line_compare: u32,   // assembled 10-bit value: CRTC 18h + 07h.4 + 09h.6
    pub preset_row_scan: u8, // CRTC index 08h: bits 4-0 first font scanline, bits 6-5 byte pan
}

impl CrtcTiming {
    /// Standard 80x25 text (mode 03h): 70 Hz, 9-dot chars. Boot default so the
    /// beam math is valid before any graphics mode-set.
    pub fn text_03h() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 9,
            hdisp_end: 720,
            vtotal: 449,
            vdisp_end: 400,
            vblank_start: 407,
            vblank_end: 442,
            vretrace_start: 412,
            vretrace_end: 414,
            max_scan: 15,
            double_scan: false,
            start_address: 0,
            offset: 80,
            mode_control: 0xA3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// Monochrome text (BIOS mode 07h): 80x25, 9x14 cells, 720x350 active.
    pub fn text_07h() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 9,
            hdisp_end: 720,
            vtotal: 449,
            vdisp_end: 350,
            vblank_start: 355,
            vblank_end: 442,
            vretrace_start: 387,
            vretrace_end: 389,
            max_scan: 13,
            double_scan: false,
            start_address: 0,
            offset: 80,
            mode_control: 0xA3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// CGA-style 40x25 text (BIOS modes 00h/01h): 8x8 cells, 320x200 active.
    pub fn text_40x25() -> Self {
        Self {
            htotal_chars: 57,
            char_width: 8,
            hdisp_end: 320,
            vtotal: 262,
            vdisp_end: 200,
            vblank_start: 200,
            vblank_end: 255,
            vretrace_start: 224,
            vretrace_end: 226,
            max_scan: 7,
            double_scan: false,
            start_address: 0,
            offset: 40,
            mode_control: 0xA3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// CGA-style 80x25 text (BIOS modes 02h/03h): 8x8 cells, 640x200 active.
    pub fn text_80x25_cga() -> Self {
        Self {
            htotal_chars: 114,
            hdisp_end: 640,
            offset: 80,
            ..Self::text_40x25()
        }
    }

    /// Mode 0Dh: 320x200x16 planar, 70 Hz, double-scanned, 8-dot chars.
    pub fn mode_0dh() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 320,
            vtotal: 449,
            vdisp_end: 400,
            vblank_start: 407,
            vblank_end: 442,
            vretrace_start: 412,
            vretrace_end: 414,
            max_scan: 1,
            double_scan: true,
            start_address: 0,
            offset: 20,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// Mode 0Eh: 640x200x16 planar, 70 Hz, double-scanned, 8-dot chars. Same
    /// vertical timing as 0Dh, wider active, 80-byte line (offset 40).
    pub fn mode_0eh() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 640,
            vtotal: 449,
            vdisp_end: 400,
            vblank_start: 407,
            vblank_end: 442,
            vretrace_start: 412,
            vretrace_end: 414,
            max_scan: 1,
            double_scan: true,
            start_address: 0,
            offset: 40,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// Mode 10h: 640x350x16 planar, 70 Hz, not double-scanned, 8-dot chars.
    pub fn mode_10h() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 640,
            vtotal: 449,
            vdisp_end: 350,
            vblank_start: 355,
            vblank_end: 442,
            vretrace_start: 387,
            vretrace_end: 389,
            max_scan: 0,
            double_scan: false,
            start_address: 0,
            offset: 40,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// Mode 0Fh: 640x350 monochrome (2-colour) planar. Shares mode 10h's
    /// 640x350 timing; only the colour count differs, and the scanout handles
    /// that through the attribute palette (the BIOS programs a 2-colour set).
    pub fn mode_0fh() -> Self {
        Self::mode_10h()
    }

    /// Mode 11h: 640x480 monochrome (2-colour) planar. Shares mode 12h's
    /// 640x480 timing; 2-colour, like 0Fh against 10h.
    pub fn mode_11h() -> Self {
        Self::mode_12h()
    }

    /// Mode 12h: 640x480x16 planar, 60 Hz, not double-scanned, 8-dot chars.
    pub fn mode_12h() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 640,
            vtotal: 525,
            vdisp_end: 480,
            vblank_start: 490,
            vblank_end: 520,
            vretrace_start: 490,
            vretrace_end: 492,
            max_scan: 0,
            double_scan: false,
            start_address: 0,
            offset: 40,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// Mode X / mode Y base: 320x200 unchained 256-color. Offset 40 gives 80 bytes
    /// per scanline per plane (320 pixels / 4 planes). 320x240 is reached when the
    /// guest reprograms the vertical timing while unchained (see
    /// `recompute_vertical_timing`).
    pub fn mode_x() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 320,
            vtotal: 449,
            vdisp_end: 400,
            vblank_start: 407,
            vblank_end: 442,
            vretrace_start: 412,
            vretrace_end: 414,
            max_scan: 1,
            double_scan: true,
            start_address: 0,
            offset: 40,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// Standard chained mode 13h: 320x200 256-color, 70 Hz, double-scanned to
    /// 400 scanlines (200 source rows), 8-dot chars. The display scanout is
    /// identical to mode X (chain-4 changes only the CPU write decode), so the
    /// timing matches `mode_x()`; offset 40 gives 80 bytes per source row per
    /// plane, the 256-color byte pitch. Installed by `set_mode13h`.
    pub fn mode13h() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 320,
            vtotal: 449,
            vdisp_end: 400,
            vblank_start: 407,
            vblank_end: 442,
            vretrace_start: 412,
            vretrace_end: 414,
            max_scan: 1,
            double_scan: true,
            start_address: 0,
            offset: 40,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// CGA 320x200 graphics (modes 04h/05h): 200 active scanlines, ~60 Hz. The
    /// CGA framebuffer carries its own interleave and decode (see `render_cga_row`),
    /// so this timing only drives the beam and the active-area extent. Not
    /// double-scanned in the raster model: 200 source rows map to 200 raster lines.
    pub fn cga_320x200() -> Self {
        Self {
            htotal_chars: 57,
            char_width: 8,
            hdisp_end: 320,
            vtotal: 262,
            vdisp_end: 200,
            vblank_start: 200,
            vblank_end: 255,
            vretrace_start: 224,
            vretrace_end: 226,
            max_scan: 0,
            double_scan: false,
            start_address: 0,
            offset: 40,
            mode_control: 0xE3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }

    /// CGA 640x200 graphics (mode 06h): the 6845 uses the same 40-column
    /// horizontal timing registers as 320x200 graphics, but the high-res dot
    /// clock makes each displayed character time cover 16 active pixels.
    pub fn cga_640x200() -> Self {
        Self {
            char_width: 16,
            hdisp_end: 640,
            ..Self::cga_320x200()
        }
    }

    /// Total dots per frame = htotal_dots * vtotal (scan-counter lines).
    pub fn frame_dots(&self) -> u64 {
        (self.htotal_chars * self.char_width) as u64 * self.vtotal as u64
    }

    /// Hercules Graphics Card 720x348 graphics mode, derived from the HGC's stock
    /// 6845 register set (R0=35h R1=2Dh R2=2Eh R3=07h R4=5Bh R5=02h R6=57h R7=57h
    /// R8=02h R9=03h; seasip.info "Hercules Graphics Card Plus Notes"). 16-dot
    /// characters (R1=2Dh -> 45 char columns * 16 = 720 active dots); 4 scanlines
    /// per character row (R9=03h) with R6=57h (87 rows) giving 348 active
    /// scanlines, and R4/R5 giving 370 scanlines total -- matching the card's
    /// well-documented ~50 Hz refresh at its 16.257 MHz dot clock (864 dots/line
    /// * 370 lines). 90 bytes/scanline (720 pixels / 8, 1bpp).
    pub fn hgc_720x348() -> Self {
        Self {
            htotal_chars: 54,
            char_width: 16,
            hdisp_end: 720,
            vtotal: 370,
            vdisp_end: 348,
            vblank_start: 348,
            vblank_end: 366,
            vretrace_start: 348,
            vretrace_end: 350,
            max_scan: 3,
            double_scan: false,
            start_address: 0,
            offset: 90,
            mode_control: 0xA3,
            underline_loc: 0x00,
            line_compare: 0x3FF,
            preset_row_scan: 0,
        }
    }
}

/// Raw CRTC register bytes. VGA graphics modes derive vertical timing from these
/// bytes; CGA personalities also derive Motorola 6845 horizontal R0/R1 and
/// vertical R4/R5/R6/R7/R9 into `CrtcTiming`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrtcRegs {
    pub r00: u8, // horizontal total
    pub r01: u8, // horizontal display end
    pub r02: u8, // start horizontal blanking
    pub r03: u8, // end horizontal blanking (bits 4-0 end, 6-5 display skew, 7 reserved)
    pub r04: u8, // start horizontal retrace
    pub r05: u8, // end horizontal retrace (bits 4-0 end, 6-5 delay, 7 EHB bit 5)
    pub r06: u8, // vertical total (low 8)
    pub r07: u8, // overflow (high bits of several fields)
    pub r08: u8, // preset row scan / interlace mode (CGA: interlace/skew)
    pub r09: u8, // maximum scan line (double-scan, max_scan, line-compare bit 9)
    pub r10: u8, // vertical retrace start (low 8)
    pub r11: u8, // vertical retrace end (low-nibble compare)
    pub r12: u8, // vertical display end (low 8)
    pub r13: u8, // offset
    pub r14: u8, // underline location / dword addressing bit
    pub r15: u8, // vertical blank start (low 8)
    pub r16: u8, // vertical blank end (8-bit compare)
    pub r17: u8, // CRTC mode control
    pub r18: u8, // line compare low 8
}

impl CrtcRegs {
    pub fn from_timing(t: CrtcTiming) -> Self {
        let vtotal = t.vtotal.saturating_sub(2);
        let vdisp = t.vdisp_end.saturating_sub(1);
        let vretrace = t.vretrace_start;
        let vblank = t.vblank_start;
        let hdisplay = (t.hdisp_end / t.char_width).saturating_sub(1);

        Self {
            r00: (t.htotal_chars.saturating_sub(5) & 0xFF) as u8,
            r01: (hdisplay & 0xFF) as u8,
            r02: ((hdisplay + 1) & 0xFF) as u8,
            r03: 0x82,
            r04: ((hdisplay + 5) & 0xFF) as u8,
            r05: 0x80,
            r06: (vtotal & 0xFF) as u8,
            r07: (((vtotal >> 8) & 1)
                | (((vdisp >> 8) & 1) << 1)
                | (((vretrace >> 8) & 1) << 2)
                | (((vblank >> 8) & 1) << 3)
                | (((t.line_compare >> 8) & 1) << 4)
                | (((vtotal >> 9) & 1) << 5)
                | (((vdisp >> 9) & 1) << 6)
                | (((vretrace >> 9) & 1) << 7)) as u8,
            r08: t.preset_row_scan,
            r09: (t.max_scan as u8 & 0x1F)
                | (((vblank >> 9) as u8 & 1) << 5)
                | (((t.line_compare >> 9) as u8 & 1) << 6),
            r10: (vretrace & 0xFF) as u8,
            r11: (t.vretrace_end & 0x0F) as u8,
            r12: (vdisp & 0xFF) as u8,
            r13: t.offset as u8,
            r14: t.underline_loc,
            r15: (vblank & 0xFF) as u8,
            r16: (t.vblank_end & 0xFF) as u8,
            r17: t.mode_control,
            r18: t.line_compare as u8,
        }
    }

    pub(super) fn from_vgabios_crtc(regs: [u8; 25]) -> Self {
        Self {
            r00: regs[0x00],
            r01: regs[0x01],
            r02: regs[0x02],
            r03: regs[0x03],
            r04: regs[0x04],
            r05: regs[0x05],
            r06: regs[0x06],
            r07: regs[0x07],
            r08: regs[0x08],
            r09: regs[0x09],
            r10: regs[0x10],
            r11: regs[0x11],
            r12: regs[0x12],
            r13: regs[0x13],
            r14: regs[0x14],
            r15: regs[0x15],
            r16: regs[0x16],
            r17: regs[0x17],
            r18: regs[0x18],
        }
    }

    /// The 320x200 unchained register set, matching `CrtcTiming::mode_x()`. The
    /// horizontal group (r00-r05) carries the stock 320-pixel CRTC values so a
    /// guest that reads them back before reprogramming sees the mode default.
    pub fn mode_x_320x200() -> Self {
        Self {
            r00: 0x5F,
            r01: 0x4F,
            r02: 0x50,
            r03: 0x82,
            r04: 0x54,
            r05: 0x80,
            r06: 0xBF,
            r07: 0x1F,
            r08: 0x00,
            r09: 0x41,
            r10: 0x9C,
            r11: 0x0E,
            r12: 0x8F,
            r13: 0x28,
            r14: 0x40,
            r15: 0x97,
            r16: 0xBA,
            r17: 0xA3,
            r18: 0xFF,
        }
    }

    pub fn cga_text_40x25() -> Self {
        Self {
            r00: 0x38,
            r01: 0x28,
            r02: 0x2D,
            r03: 0x0A,
            r04: 0x1F,
            r05: 0x06,
            r06: 0x19,
            r07: 0x1C,
            r08: 0x02,
            r09: 0x07,
            r10: 0x06,
            r11: 0x07,
            r12: 0x00,
            r13: 0x00,
            r14: 0x00,
            r15: 0x00,
            r16: 0x00,
            r17: 0x00,
            r18: 0x00,
        }
    }

    pub fn cga_text_80x25() -> Self {
        Self {
            r00: 0x71,
            r01: 0x50,
            r02: 0x5A,
            r03: 0x0A,
            ..Self::cga_text_40x25()
        }
    }

    pub fn cga_graphics_320x200() -> Self {
        Self {
            r00: 0x38,
            r01: 0x28,
            r02: 0x2D,
            r03: 0x0A,
            r04: 0x7F,
            r05: 0x06,
            r06: 0x64,
            r07: 0x70,
            r08: 0x02,
            r09: 0x01,
            r10: 0x06,
            r11: 0x07,
            r12: 0x00,
            r13: 0x00,
            r14: 0x00,
            r15: 0x00,
            r16: 0x00,
            r17: 0x00,
            r18: 0x00,
        }
    }

    pub fn cga_graphics_640x200() -> Self {
        Self::cga_graphics_320x200()
    }
}

pub(super) fn vgabios_crtc_regs(mode: u8) -> Option<[u8; 25]> {
    match mode {
        0x03 => Some([
            0x5f, 0x4f, 0x50, 0x82, 0x55, 0x81, 0xbf, 0x1f, 0x00, 0x4f, 0x0d, 0x0e, 0x00, 0x00,
            0x00, 0x00, 0x9c, 0x8e, 0x8f, 0x28, 0x1f, 0x96, 0xb9, 0xa3, 0xff,
        ]),
        0x0d => Some([
            0x2d, 0x27, 0x28, 0x90, 0x2b, 0x80, 0xbf, 0x1f, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x9c, 0x8e, 0x8f, 0x14, 0x00, 0x96, 0xb9, 0xe3, 0xff,
        ]),
        0x0e => Some([
            0x5f, 0x4f, 0x50, 0x82, 0x54, 0x80, 0xbf, 0x1f, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x9c, 0x8e, 0x8f, 0x28, 0x00, 0x96, 0xb9, 0xe3, 0xff,
        ]),
        0x0f | 0x10 => Some([
            0x5f, 0x4f, 0x50, 0x82, 0x54, 0x80, 0xbf, 0x1f, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x83, 0x85, 0x5d, 0x28, 0x0f, 0x63, 0xba, 0xe3, 0xff,
        ]),
        0x11 | 0x12 => Some([
            0x5f, 0x4f, 0x50, 0x82, 0x54, 0x80, 0x0b, 0x3e, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xea, 0x8c, 0xdf, 0x28, 0x00, 0xe7, 0x04, 0xe3, 0xff,
        ]),
        0x13 => Some([
            0x5f, 0x4f, 0x50, 0x82, 0x54, 0x80, 0xbf, 0x1f, 0x00, 0x41, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x9c, 0x8e, 0x8f, 0x28, 0x40, 0x96, 0xb9, 0xa3, 0xff,
        ]),
        _ => None,
    }
}

pub(super) fn vgabios_seq_regs(mode: u8) -> Option<[u8; 4]> {
    match mode {
        0x03 => Some([0x00, 0x03, 0x00, 0x02]),
        0x0d => Some([0x09, 0x0f, 0x00, 0x06]),
        0x0e..=0x12 => Some([0x01, 0x0f, 0x00, 0x06]),
        0x13 => Some([0x01, 0x0f, 0x00, 0x0e]),
        _ => None,
    }
}

pub(super) fn vgabios_gc_regs(mode: u8) -> Option<[u8; 9]> {
    match mode {
        0x03 => Some([0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0e, 0x0f, 0xff]),
        0x0d..=0x12 => Some([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x0f, 0xff]),
        0x13 => Some([0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x05, 0x0f, 0xff]),
        _ => None,
    }
}

pub(super) fn vgabios_attr_regs(mode: u8) -> Option<[u8; 20]> {
    match mode {
        0x03 => Some([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d,
            0x3e, 0x3f, 0x0c, 0x00, 0x0f, 0x08,
        ]),
        0x0d | 0x0e => Some([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15,
            0x16, 0x17, 0x01, 0x00, 0x0f, 0x00,
        ]),
        0x0f => Some([
            0x00, 0x08, 0x00, 0x00, 0x18, 0x18, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x18,
            0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
        ]),
        0x10 | 0x12 => Some([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d,
            0x3e, 0x3f, 0x01, 0x00, 0x0f, 0x00,
        ]),
        0x11 => Some([
            0x00, 0x3f, 0x00, 0x3f, 0x00, 0x3f, 0x00, 0x3f, 0x00, 0x3f, 0x00, 0x3f, 0x00, 0x3f,
            0x00, 0x3f, 0x01, 0x00, 0x0f, 0x00,
        ]),
        0x13 => Some([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x41, 0x00, 0x0f, 0x00,
        ]),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Sequencer {
    pub reset: u8,           // idx 0 (bits 0 and 1 must both be 1 for output)
    pub clocking_mode: u8,   // idx 1 (bit 0 set = 8-dot chars; clear = 9-dot)
    pub map_mask: u8,        // idx 2, low 4 bits
    pub char_map_select: u8, // idx 3 (map A bits 0,1,4 select the active font table)
    pub memory_mode: u8,     // idx 4
}

impl Default for Sequencer {
    fn default() -> Self {
        Self {
            reset: 0x03,
            clocking_mode: 0,
            map_mask: 0,
            char_map_select: 0,
            memory_mode: 0,
        }
    }
}

/// Attribute Controller register block (3C0/3C1).
#[derive(Debug, Clone, Copy)]
pub struct Attribute {
    pub palette: [u8; 16],    // idx 0..15
    pub mode_control: u8,     // idx 0x10
    pub overscan: u8,         // idx 0x11
    pub plane_enable: u8,     // idx 0x12
    pub pixel_pan: u8,        // idx 0x13, low 4 bits
    pub color_select: u8,     // idx 0x14
    pub flip_flop_data: bool, // false = next 3C0 write is an index
    pub index: u8,
    // Palette Address Source (3C0 index bit 5): set = normal display, clear =
    // screen blanked while the palette is being programmed.
    pub pas: bool,
}

impl Default for Attribute {
    fn default() -> Self {
        // Real VGA powers up with ATC palette register N = N and all four colour
        // planes enabled, so a 4-bit plane index maps straight to DAC N (vgabios
        // video_param_table actl_regs). The BIOS mode-set programs the rest.
        Self {
            palette: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            mode_control: 0,
            overscan: 0,
            plane_enable: 0x0F,
            pixel_pan: 0,
            color_select: 0,
            flip_flop_data: false,
            index: 0,
            // Powers up display-enabled so the boot screen shows before any 3C0
            // program; the BIOS sets PAS to 1 at the end of every mode-set.
            pas: true,
        }
    }
}

pub(super) const VGA_DOT_CLOCK_25_HZ: u64 = 25_175_000;
pub(super) const VGA_DOT_CLOCK_28_HZ: u64 = 28_322_000;
// Wired VGA colour-display switch sense, selected by Misc Output bits 3-2.
pub(super) const VGA_COLOR_SWITCH_SENSE: u8 = 0b0110;

/// Total horizontal dots per scan line.
pub fn htotal_dots(t: &CrtcTiming) -> u64 {
    (t.htotal_chars * t.char_width) as u64
}

/// Current scan line (0-based, scan-counter units) for a dot position.
pub fn beam_line(t: &CrtcTiming, dots: u64) -> u32 {
    ((dots / htotal_dots(t)) % t.vtotal as u64) as u32
}

/// Dot position within the current scan line.
pub fn beam_dot(t: &CrtcTiming, dots: u64) -> u32 {
    (dots % htotal_dots(t)) as u32
}

/// True when the beam is in the active display area (both H and V).
pub fn beam_display_enable(t: &CrtcTiming, dots: u64) -> bool {
    beam_line(t, dots) < t.vdisp_end && beam_dot(t, dots) < t.hdisp_end
}

/// True when the beam is inside the vertical retrace interval.
pub fn beam_vretrace(t: &CrtcTiming, dots: u64) -> bool {
    let line = beam_line(t, dots);
    line >= t.vretrace_start && line < t.vretrace_end
}

/// The two GEOMETRY bits of Input Status Register 1, off a caller-supplied beam:
/// bit 0 (display inactive / safe VRAM window) and bit 3 (vertical retrace).
///
/// `display_forced_inactive` is the caller's own "the display is not refreshing
/// at all" verdict, which is where the two callers differ: the VGA core folds in
/// its sequencer / attribute-controller / CGA gates, while Margo's VBE modes have
/// none of those registers in their path and pass `false`. Everything else --
/// which is to say all of the timing -- comes from `t`.
///
/// SHARED ON PURPOSE. `Vga::status1_bits` and `MargoScanTiming::status1_bits`
/// both call this, so a guest that polls 0x3DA cannot see two different notions
/// of "in retrace" depending on which engine owns the display.
pub fn status1_geometry_bits(t: &CrtcTiming, beam: u64, display_forced_inactive: bool) -> u8 {
    let mut status = 0u8;
    if display_forced_inactive || !beam_display_enable(t, beam) {
        status |= 0x01;
    }
    if beam_vretrace(t, beam) {
        status |= 0x08;
    }
    status
}

/// Dots from `beam` to the first transition of Input Status 1 bit 3 (vertical
/// retrace) or bit 0 (display inactive) to `target`. Pure geometry from `t`; the
/// live beam is never moved.
///
/// THE ANALYTIC PEEK. A mid-batch 0x3DA read is answered from a PREDICTED beam
/// rather than from device state, and the JIT's poll-skip binary search uses
/// this distance as the deadline it may not cross. Both only stay honest if the
/// answer here equals what a real advance of the same clocks would produce, so
/// this is the ONE implementation, shared by the VGA core and by Margo
/// (`status1_geometry_bits` is its companion, and the pair has to agree: this
/// returns `None` exactly when the bit already has the target value, so a caller
/// that sees `None` knows the state is settled, not that the geometry is unusable
/// -- the unusable cases return `None` too, and both mean "do not schedule").
///
/// `display_forced_inactive` carries the same meaning as in
/// `status1_geometry_bits`; when it is set, bit 0 never transitions and this
/// returns `None`.
///
/// IT IS A CLOSURE, and that is not decoration. Only the bit-0 arm consults it,
/// while the DOMINANT caller is the bit-3 vertical-retrace poll (doom and
/// wolf3d spin on exactly that). The VGA's verdict costs a sequencer read and a
/// personality test; taking it eagerly at the call site would have added that
/// work to every retrace poll, which is not what "the extraction is a pure move"
/// is supposed to mean. Deferring it keeps this function's cost identical to the
/// method it was lifted out of, arm for arm.
pub fn dots_until_status1_bit_change(
    t: &CrtcTiming,
    beam: u64,
    bit: u8,
    target: bool,
    display_forced_inactive: impl FnOnce() -> bool,
) -> Option<u64> {
    let htotal = htotal_dots(t);
    let frame = t.frame_dots();
    if htotal == 0 || frame == 0 || t.vtotal == 0 {
        return None;
    }
    let beam = beam % frame;
    match bit {
        3 => {
            if t.vretrace_start >= t.vretrace_end || t.vretrace_end > t.vtotal {
                return None;
            }
            let current = beam_vretrace(t, beam);
            if current == target {
                return None;
            }
            let line = if target {
                t.vretrace_start
            } else {
                t.vretrace_end
            };
            let edge = u64::from(line).checked_mul(htotal)?;
            Some(if beam < edge {
                edge - beam
            } else {
                frame - beam + edge
            })
        }
        0 => {
            // Called here and only here, in the position the method evaluated
            // it in, so the work this arm does is unchanged term for term.
            if display_forced_inactive()
                || t.hdisp_end == 0
                || u64::from(t.hdisp_end) > htotal
                || t.vdisp_end == 0
                || t.vdisp_end > t.vtotal
            {
                return None;
            }
            let current = !beam_display_enable(t, beam);
            if current == target {
                return None;
            }
            let line = beam_line(t, beam);
            let dot = u64::from(beam_dot(t, beam));
            if target {
                Some(u64::from(t.hdisp_end) - dot)
            } else if line + 1 < t.vdisp_end {
                Some(htotal - dot)
            } else {
                Some(frame - beam)
            }
        }
        _ => None,
    }
}

/// Dots from `beam` to the next vertical-retrace START edge (the first dot of
/// scan line `vretrace_start`).
///
/// If the beam sits at or past the edge on this frame (on the edge, inside the
/// retrace window, or in the bottom border below it), the result is the NEXT
/// frame's edge, up to a full frame ahead, so the returned distance is never
/// zero -- which is what makes a caller that loops "advance to the edge, run a
/// little, repeat" terminate. `None` when the geometry is unusable (zero-dot
/// frame, or a retrace edge outside the frame), so callers skip edge-aware
/// scheduling. Shared by the VGA core and Margo for the same reason as
/// `dots_until_status1_bit_change`.
pub fn dots_until_vretrace_start(t: &CrtcTiming, beam: u64) -> Option<u64> {
    let frame = t.frame_dots();
    if frame == 0 {
        return None;
    }
    let edge = u64::from(t.vretrace_start) * htotal_dots(t);
    if edge >= frame {
        return None;
    }
    // A mode switch to a smaller frame can leave `beam` beyond the new frame
    // until the next advance() wraps it; normalize first.
    let beam = beam % frame;
    Some(if beam < edge {
        edge - beam
    } else {
        frame - beam + edge
    })
}

/// True when the beam is inside the horizontal blanking/retrace interval:
/// past the active dots on the current scan line. `CrtcTiming` does not carry
/// a separate horizontal retrace start/width (only the CGA/MDA/VGA personalities
/// this core hosts need it, and none of their timing tables model it finer than
/// "past hdisp_end"), so this is the whole non-active portion of the line --
/// good enough for a status bit a detection loop polls for a toggle, which is
/// its only consumer (`read_hgc_status`).
pub fn beam_hsync(t: &CrtcTiming, dots: u64) -> bool {
    beam_dot(t, dots) >= t.hdisp_end
}
