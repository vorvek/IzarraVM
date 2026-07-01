//! The legacy VGA core: 256 KB planar VRAM, the VGA register blocks, a
//! cycle-coupled beam clock, and a catch-up rasterizer. This is Margo's
//! VGA-compatibility personality (one chip, one frame store, one RAMDAC).
//!
//! It also carries the text personality: the 80x25 character buffer, the
//! RAMDAC, and the CRTC text cursor. Chained mode 13h routes through the same
//! raster engine as the planar and mode-X paths; chain-4 only rewrites the CPU
//! write/read decode.

use crate::{
    DAC_ENTRIES, Dac, TextCell, TextFrame, VGA_MONO_TEXT_BASE, VGA_TEXT_BASE, VGA_TEXT_COLUMNS,
    VGA_TEXT_MEMORY_SIZE, VGA_TEXT_ROWS, VideoError, VideoMode,
};

pub const VGA_PLANE_SIZE: usize = 64 * 1024;
pub const VGA_PLANES: usize = 4;
pub const VGA_PLANAR_SIZE: usize = VGA_PLANE_SIZE * VGA_PLANES; // 256 KB

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
}

const CGA_MODE_80_COLUMNS: u8 = 0x01;
const CGA_MODE_GRAPHICS: u8 = 0x02;
const CGA_MODE_BW: u8 = 0x04;
const CGA_MODE_VIDEO_ENABLE: u8 = 0x08;
const CGA_MODE_HIGH_RES: u8 = 0x10;
const CGA_MODE_BLINK: u8 = 0x20;

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

    fn from_vgabios_crtc(regs: [u8; 25]) -> Self {
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

fn vgabios_crtc_regs(mode: u8) -> Option<[u8; 25]> {
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

fn vgabios_seq_regs(mode: u8) -> Option<[u8; 4]> {
    match mode {
        0x03 => Some([0x00, 0x03, 0x00, 0x02]),
        0x0d => Some([0x09, 0x0f, 0x00, 0x06]),
        0x0e..=0x12 => Some([0x01, 0x0f, 0x00, 0x06]),
        0x13 => Some([0x01, 0x0f, 0x00, 0x0e]),
        _ => None,
    }
}

fn vgabios_gc_regs(mode: u8) -> Option<[u8; 9]> {
    match mode {
        0x03 => Some([0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0e, 0x0f, 0xff]),
        0x0d..=0x12 => Some([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x0f, 0xff]),
        0x13 => Some([0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x05, 0x0f, 0xff]),
        _ => None,
    }
}

fn vgabios_attr_regs(mode: u8) -> Option<[u8; 20]> {
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

/// The 16 EGA/CGA color numbers as DAC indices. On the stock VGA palette the
/// first 16 entries are the EGA colors, so a CGA color number is its own DAC
/// index. Named for the four-color and two-color palette tables below.
const CGA_BLACK: u8 = 0;
const CGA_GREEN: u8 = 2;
const CGA_CYAN: u8 = 3;
const CGA_RED: u8 = 4;
const CGA_MAGENTA: u8 = 5;
const CGA_BROWN: u8 = 6;
const CGA_LIGHT_GRAY: u8 = 7;
const CGA_LIGHT_GREEN: u8 = 10;
const CGA_LIGHT_CYAN: u8 = 11;
const CGA_LIGHT_RED: u8 = 12;
const CGA_LIGHT_MAGENTA: u8 = 13;
const CGA_YELLOW: u8 = 14;
const CGA_WHITE: u8 = 15;

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
    fn background_index(&self) -> u8 {
        self.color_select & 0x0F
    }

    /// The foreground color for the 1 bits in 640x200x2: color-select bits 0-3,
    /// the same field as the background nibble. The background is always black.
    fn foreground_640x200(&self) -> u8 {
        self.color_select & 0x0F
    }

    /// Decode the four DAC indices a 320x200x4 framebuffer byte holds, MSB-first:
    /// bits 7-6 are pixel 0, 5-4 pixel 1, 3-2 pixel 2, 1-0 pixel 3. Value 0 is the
    /// background; values 1-3 select from the active four-color palette.
    fn decode_byte_320x200(&self, byte: u8) -> [u8; 4] {
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

/// The pixel-perfect raster the host pulls. Square pixels, no aspect correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgaRaster {
    pub width: u32,
    pub height: u32,
    /// Visible active scanlines (`vdisp_end`). The top `display_height` rows of
    /// `pixels` are the displayed image; the rows below are vertical blanking and
    /// border, which a real monitor never shows. `height` stays the full beam
    /// frame (`vtotal`); the host crops to `display_height` before presenting so
    /// the active image — not the retrace region — is what fills the screen.
    pub display_height: u32,
    pub pixels: Vec<u8>, // DAC indices; renderer resolves through the Dac
}

const VGA_DOT_CLOCK_25_HZ: u64 = 25_175_000;
const VGA_DOT_CLOCK_28_HZ: u64 = 28_322_000;
// Wired VGA colour-display switch sense, selected by Misc Output bits 3-2.
const VGA_COLOR_SWITCH_SENSE: u8 = 0b0110;

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

#[derive(Debug, Clone)]
pub struct Vga {
    pub(crate) vram: Vec<u8>,
    pub(crate) crtc: CrtcTiming,
    pub(crate) crtc_regs: CrtcRegs,
    pub(crate) seq: Sequencer,
    pub(crate) gc: GfxController,
    pub(crate) attr: Attribute,
    pub(crate) latches: [u8; VGA_PLANES],
    pub(crate) beam: u64,
    pub(crate) last_line: u32,
    pub(crate) frames: u64,
    pub(crate) work: Vec<u8>,
    pub(crate) presented: Option<VgaRaster>,
    pub(crate) pending_start: Option<u32>,
    pub(crate) seq_index: u8,
    pub(crate) gc_index: u8,
    pub(crate) crtc_index: u8,
    // Legacy text/RAMDAC/cursor personality, folded in from VgaTextMode.
    pub(crate) text_memory: [u8; VGA_TEXT_MEMORY_SIZE],
    pub(crate) text_columns: usize,
    // The writable font store: eight tables of 256 glyphs x 32 bytes (the max
    // 8x32). Table 0 seeds from the ROM 8x16 font; the rest seed as copies, so a
    // title that selects an unloaded table still renders. The Sequencer
    // Character Map Select picks the active table; INT 10h AH=11h loads glyphs.
    pub(crate) font: [[u8; 256 * 32]; 8],
    pub(crate) dac: Dac,
    pub(crate) cursor_offset: u16,
    pub(crate) cursor_start: u8,
    pub(crate) cursor_end: u8,
    pub(crate) mode: VideoMode,
    pub(crate) planar_bios_mode: u8,
    pub(crate) misc_output: u8,
    pub(crate) pel_mask: u8,
    // Feature Control (read 3CA, write 3DA color / 3BA mono). Stored read-back
    // only; the FEAT0/FEAT1 lines drive nothing in this core.
    pub(crate) feature_control: u8,
    // Video Subsystem Enable (3C3, bit 0). The register stores the latch; it
    // gates VGA I/O and combines with Misc Output bit 1 for memory decode.
    pub(crate) video_subsystem_enable: u8,
    // BIOS video-refresh control (INT 10h AX=1200h/1201h, BL=36h). This blanks
    // display output/status but leaves guest-visible sequencer registers alone.
    pub(crate) display_refresh_enabled: bool,
    // DAC State (read 3C7, bits 1-0): 0b11 after a read-index (3C7) write,
    // 0b00 after a write-index (3C8) write. Tracks which DAC access mode was
    // armed last so a program polling 3C7 sees the documented state.
    pub(crate) dac_state: u8,
    pub(crate) default_palette_loading_enabled: bool,
    pub(crate) grayscale_summing_enabled: bool,
    pub(crate) cga: Cga,
    // Content-generation counter for the host-side dirty-framebuffer cache. Bumped
    // inside every display mutator on this Vga: the VRAM writers, the register/DAC
    // write port, and the start-address latch at vsync. Putting it here (not on the
    // machine bus) makes it caller-agnostic — a write lands the same whether it came
    // through the CPU bus or directly from an HLE BIOS INT 10h graphics service, so
    // neither path can change the output without bumping the gen. The machine folds
    // this into `Machine::frame_generation`. Over-bumping a no-op write is harmless
    // (a missed cache hit); missing a change would show a stale frame.
    pub(crate) content_gen: u64,
}

impl Default for Vga {
    fn default() -> Self {
        let mut text_memory = [0; VGA_TEXT_MEMORY_SIZE];
        for cell in text_memory.chunks_exact_mut(2) {
            cell[0] = b' ';
            cell[1] = 0x07;
        }

        let mut vga = Self {
            vram: vec![0; VGA_PLANAR_SIZE],
            crtc: CrtcTiming::text_03h(),
            crtc_regs: CrtcRegs::default(),
            seq: Sequencer::default(),
            gc: GfxController::default(),
            attr: Attribute::default(),
            latches: [0; VGA_PLANES],
            beam: 0,
            last_line: 0,
            frames: 0,
            work: Vec::new(),
            presented: None,
            pending_start: None,
            seq_index: 0,
            gc_index: 0,
            crtc_index: 0,
            text_memory,
            text_columns: VGA_TEXT_COLUMNS,
            font: Self::seed_fonts(),
            // Power-up is mode 03h, which seeds the EGA attribute remap below, so
            // the DAC must be palette2 to match (palette3 would mis-resolve the
            // remapped brown and bright colors).
            dac: Dac::for_mode(0x03),
            cursor_offset: 0,
            // Mode 03h uses an 8x16 font, so the bottom two scanlines form the
            // normal underscore cursor.
            cursor_start: 0x0E,
            cursor_end: 0x0F,
            mode: VideoMode::Text,
            planar_bios_mode: 0,
            // Misc Output powers up as mode 03h (text/CGA clock, CRTC at 3Dx); the
            // DAC pel mask defaults to all-pass.
            misc_output: 0x67,
            pel_mask: 0xFF,
            feature_control: 0x00,
            // Video subsystem powers up enabled so the framebuffer aperture is live.
            video_subsystem_enable: 0x01,
            display_refresh_enabled: true,
            // DAC powers up armed for writes (3C8 path), so the state reads 0b00.
            dac_state: 0x00,
            default_palette_loading_enabled: true,
            grayscale_summing_enabled: false,
            cga: Cga::default(),
            content_gen: 0,
        };
        // Size the work buffer for the boot text mode so the raster is published
        // from the first frame (finalize_frame only publishes a non-empty work).
        vga.seed_vgabios_crtc_readback(0x03);
        vga.seed_vgabios_seq_readback(0x03);
        vga.seed_vgabios_gc_readback(0x03);
        vga.seed_vgabios_attr_readback(0x03);
        vga.resize_work();
        vga
    }
}

impl Vga {
    pub fn frame_dots(&self) -> u64 {
        self.crtc.frame_dots()
    }

    pub fn dot_clock_hz(&self) -> u64 {
        match (self.misc_output >> 2) & 0x03 {
            0x01 => VGA_DOT_CLOCK_28_HZ,
            // ponytail: external/reserved VGA clocks fall back to the stock 25 MHz
            // clock until VEGA exposes a programmable clock generator.
            _ => VGA_DOT_CLOCK_25_HZ,
        }
    }

    fn set_misc_mode_bits(&mut self, clock_select: u8, color_emulation: bool, vertical_size: u8) {
        self.misc_output = (self.misc_output & !0xCF)
            | 0x02
            | ((vertical_size & 0x03) << 6)
            | ((clock_select & 0x03) << 2)
            | u8::from(color_emulation);
    }

    fn seed_vgabios_crtc_readback(&mut self, mode: u8) {
        if let Some(regs) = vgabios_crtc_regs(mode) {
            self.crtc_regs = CrtcRegs::from_vgabios_crtc(regs);
            self.crtc.preset_row_scan = regs[0x08];
        }
    }

    fn seed_vgabios_seq_readback(&mut self, mode: u8) {
        if let Some([clocking_mode, map_mask, char_map_select, memory_mode]) =
            vgabios_seq_regs(mode)
        {
            self.seq.reset = 0x03;
            self.seq.clocking_mode = clocking_mode;
            self.seq.map_mask = map_mask;
            self.seq.char_map_select = char_map_select;
            self.seq.memory_mode = memory_mode;
        }
    }

    fn seed_vgabios_gc_readback(&mut self, mode: u8) {
        if let Some(regs) = vgabios_gc_regs(mode) {
            for (index, value) in regs.into_iter().enumerate() {
                self.write_gc(index as u8, value);
            }
        }
    }

    fn seed_vgabios_attr_readback(&mut self, mode: u8) {
        if let Some(regs) = vgabios_attr_regs(mode) {
            if self.default_palette_loading_enabled {
                self.attr.palette.copy_from_slice(&regs[..16]);
                self.attr.overscan = regs[17];
            }
            self.attr.mode_control = regs[16];
            self.attr.plane_enable = regs[18];
            self.attr.pixel_pan = regs[19] & 0x0F;
            self.attr.color_select = 0;
            self.attr.pas = true;
        }
    }

    fn color_emulation(&self) -> bool {
        self.misc_output & 0x01 != 0
    }

    fn crtc_index_port_selected(&self, port: u16) -> bool {
        matches!(
            (self.color_emulation(), port),
            (true, 0x3D4) | (false, 0x3B4)
        )
    }

    fn crtc_data_port_selected(&self, port: u16) -> bool {
        matches!(
            (self.color_emulation(), port),
            (true, 0x3D5) | (false, 0x3B5)
        )
    }

    fn status1_port_selected(&self, port: u16) -> bool {
        matches!(
            (self.color_emulation(), port),
            (true, 0x3DA) | (false, 0x3BA)
        )
    }

    fn switch_sense_bit(&self) -> bool {
        let selected = (self.misc_output >> 2) & 0x03;
        VGA_COLOR_SWITCH_SENSE & (1u8 << selected) != 0
    }

    pub fn beam_dots(&self) -> u64 {
        self.beam
    }

    pub fn frames_completed(&self) -> u64 {
        self.frames
    }

    /// The active CRTC Start Address (0C/0Dh), the display-address counter value
    /// latched at the last frame boundary. In word mode (mode 03h) this is a
    /// cell/word address into the text buffer.
    pub fn crtc_start_address(&self) -> u32 {
        self.crtc.start_address
    }

    pub fn text_memory_base(&self) -> u32 {
        if self.mode == VideoMode::Text && !self.color_emulation() {
            VGA_MONO_TEXT_BASE
        } else {
            VGA_TEXT_BASE
        }
    }

    pub fn video_subsystem_enabled(&self) -> bool {
        self.video_subsystem_enable & 0x01 != 0
    }

    pub fn video_memory_enabled(&self) -> bool {
        self.video_subsystem_enabled() && self.misc_output & 0x02 != 0
    }

    pub fn display_refresh_enabled(&self) -> bool {
        self.display_refresh_enabled
    }

    pub fn set_display_refresh_enabled(&mut self, enabled: bool) {
        self.bump_content_gen(); // blanks/unblanks visible output
        self.display_refresh_enabled = enabled;
    }

    /// The start-address change buffered by the last `set_start_address`, applied
    /// at the next vretrace (finalize_frame). `None` when no change is pending.
    pub fn pending_start_address(&self) -> Option<u32> {
        self.pending_start
    }

    fn crtc_start_register(&self) -> u32 {
        self.pending_start.unwrap_or(self.crtc.start_address)
    }

    /// Seed the eight font tables from the ROM 8x16 font: table 0 holds the
    /// glyphs (rows 0..15 of each 32-byte slot, the rest zero), and tables 1..7
    /// are copies so a title that selects an unloaded table still renders.
    fn seed_fonts() -> [[u8; 256 * 32]; 8] {
        let mut tables = [[0u8; 256 * 32]; 8];
        for glyph in 0..256usize {
            for row in 0..16usize {
                tables[0][glyph * 32 + row] = crate::font::VGAFONT_8X16[glyph * 16 + row];
            }
        }
        for table in 1..8 {
            tables[table] = tables[0];
        }
        tables
    }

    /// The active font table index, decoded from the Sequencer Character Map
    /// Select (index 3) map-A field (bits 0, 1, 4), the font plane 2 displays in
    /// 256-glyph text. (Abrash, Graphics Programming Black Book ch.24.)
    pub fn active_font_table(&self) -> usize {
        char_map_a_decode(self.seq.char_map_select)
    }

    /// One glyph row for BIOS graphics-mode text. CGA has a fixed 8x8 character
    /// ROM; VGA-family modes use the active writable font table.
    pub fn active_font_glyph_row(&self, ch: u8, row: usize) -> u8 {
        if self.is_cga_personality() {
            crate::font::VGAFONT_8X8[ch as usize * 8 + row.min(7)]
        } else {
            self.font[self.active_font_table()][ch as usize * 32 + row.min(31)]
        }
    }

    /// The second font table index, decoded from the map-B field of the Sequencer
    /// Character Map Select (bits 2, 3, 5). In 512-glyph mode each cell's attribute
    /// bit 3 picks map A (clear) or map B (set); when both maps select the same
    /// table the cell is 256-glyph and bit 3 stays foreground intensity. See A4 in
    /// dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
    pub fn active_font_table_b(&self) -> usize {
        char_map_b_decode(self.seq.char_map_select)
    }

    /// Decode a block-specifier value (BL) to a font table index with the same
    /// map-A field as `active_font_table`, so a font loaded with a block and then
    /// selected with the same block specifier always displays.
    pub fn char_map_table(&self, block: u8) -> usize {
        char_map_a_decode(block)
    }

    /// The shared blink hide phase, driven by the vertical-retrace (frame)
    /// counter: 16 frames on, 16 frames off (period 32). At mode 03h's 70 Hz that
    /// is the documented ~2.19 Hz cursor/attribute blink rate. Both the attribute
    /// blink (foreground collapse) and the hardware-cursor blink read this single
    /// source so they stay in lockstep. See A6 in
    /// dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
    pub fn blink_hide_phase(&self) -> bool {
        (self.frames / 16) % 2 == 1
    }

    /// Write the Sequencer Character Map Select (index 3), picking the active
    /// font table for text. Used by INT 10h AH=11h AL=03.
    pub fn set_char_map_select(&mut self, value: u8) {
        self.seq.char_map_select = value;
    }

    /// Load user font glyphs into one table (INT 10h AH=11h AL=00/10). `data` is
    /// `count` consecutive glyphs of `bytes_per_char` bytes each (bit 7 = leftmost
    /// pixel), for the character codes starting at `first_char`. Each glyph fills
    /// the low rows of its 32-byte slot; the rows above are left as-is, matching
    /// the VGA BIOS byte-copy load.
    pub fn load_font_table(
        &mut self,
        table: usize,
        first_char: u16,
        bytes_per_char: u8,
        data: &[u8],
    ) {
        let table = table & 0x07;
        let bpc = bytes_per_char as usize;
        if bpc == 0 {
            return;
        }
        let count = data.len() / bpc;
        for i in 0..count {
            let code = (first_char as usize).wrapping_add(i) & 0xFF;
            let slot = code * 32;
            for row in 0..bpc.min(32) {
                self.font[table][slot + row] = data[i * bpc + row];
            }
        }
    }

    /// Copy one of the ROM fonts (8x8, 8x14, or 8x16) into all 256 glyph slots of
    /// a table (INT 10h AH=11h AL=01/02/04). `height` selects the source font.
    pub fn load_rom_font(&mut self, table: usize, height: u8) {
        let table = table & 0x07;
        let (src, h) = match height {
            8 => (&crate::font::VGAFONT_8X8[..], 8usize),
            14 => (&crate::font::VGAFONT_8X14[..], 14usize),
            _ => (&crate::font::VGAFONT_8X16[..], 16usize),
        };
        for code in 0..256usize {
            let slot = code * 32;
            for row in 0..h {
                self.font[table][slot + row] = src[code * h + row];
            }
        }
    }

    /// Set the text character height: CRTC Maximum Scan Line (index 09h) low five
    /// bits = height - 1, reprogramming the renderer's rows-per-character divide.
    /// Used by the INT 10h AH=11h 1x variants that reprogram the scan lines.
    pub fn set_char_height(&mut self, height: u8) {
        self.crtc.max_scan = u32::from(height.saturating_sub(1));
    }

    pub fn char_height(&self) -> u8 {
        self.crtc.max_scan.saturating_add(1).min(u32::from(u8::MAX)) as u8
    }

    pub fn font_table_image(&self, table: usize, bytes_per_char: u8) -> Vec<u8> {
        let bpc = usize::from(bytes_per_char.min(32));
        let table = table & 0x07;
        let mut bytes = Vec::with_capacity(256 * bpc);
        for code in 0..256usize {
            let slot = code * 32;
            bytes.extend_from_slice(&self.font[table][slot..slot + bpc]);
        }
        bytes
    }

    /// Reload the power-on default DAC palette, attribute palette, and pel mask
    /// for `mode`. Real hardware reprograms the RAMDAC to the mode's defaults on
    /// a mode set, so a prior program's custom palette (the BIOS, say) does not
    /// leak into the program that sets the next mode. The default DAC differs by
    /// mode: EGA graphics modes load palette0/1/2 (see [`Dac::for_mode`]), every
    /// other mode keeps the 256-color palette3.
    fn reset_palette_defaults(&mut self, mode: u8) {
        let dac = self.dac.clone();
        let attr_palette = self.attr.palette;
        let overscan = self.attr.overscan;
        let color_select = self.attr.color_select;
        let pel_mask = self.pel_mask;
        self.attr = Attribute::default();
        if self.default_palette_loading_enabled {
            self.dac = Dac::for_mode(mode);
            self.pel_mask = 0xFF;
        } else {
            self.dac = dac;
            self.attr.palette = attr_palette;
            self.attr.overscan = overscan;
            self.attr.color_select = color_select;
            self.pel_mask = pel_mask;
        }
    }

    /// Install a planar mode's timing and reset the beam to the top of frame.
    fn set_planar_mode(&mut self, mode: u8, timing: CrtcTiming, clear: bool) {
        // A mode change alters the scanout interpretation even between two graphics
        // modes of identical raster dims (e.g. 0Dh<->13h, both 320x449), which the
        // dimension fold in `Machine::frame_generation` cannot see. Bump so the host
        // frame cache re-renders the switch.
        self.bump_content_gen();
        self.crtc = timing;
        self.crtc_regs = CrtcRegs::from_timing(timing);
        self.seed_vgabios_crtc_readback(mode);
        self.seed_vgabios_seq_readback(mode);
        let vertical_size = match mode {
            0x0F | 0x10 => 0x02, // 350-line family
            0x11 | 0x12 => 0x03, // 480-line family
            _ => 0x01,           // 400-line / double-scanned 200-line family
        };
        self.set_misc_mode_bits(0, mode != 0x0F, vertical_size);
        self.gc = GfxController::default();
        self.seed_vgabios_gc_readback(mode);
        self.latches = [0; VGA_PLANES];
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Planar;
        self.planar_bios_mode = mode;
        if clear {
            self.vram.fill(0);
        }
        self.presented = None; // drop any stale frame from a prior mode
        self.reset_palette_defaults(mode);
        self.seed_vgabios_attr_readback(mode);
        self.resize_work();
    }

    /// Switch to mode 0Dh. Kept as an alias so existing callers do not churn;
    /// new call sites can use `set_mode(0x0D)`.
    pub fn set_mode_0dh(&mut self) {
        self.set_planar_mode(0x0D, CrtcTiming::mode_0dh(), false);
    }

    /// Select a VGA graphics mode by its INT 10h number. Returns false for a number this
    /// slice does not implement, leaving the current mode untouched.
    pub fn set_mode(&mut self, mode: u8) -> bool {
        self.set_mode_with_clear(mode, false)
    }

    /// Select a VGA graphics mode and optionally clear VGA graphics memory, matching
    /// INT 10h AH=00h's bit-7 clear/preserve flag.
    pub fn set_mode_with_clear(&mut self, mode: u8, clear: bool) -> bool {
        let timing = match mode {
            0x0D => CrtcTiming::mode_0dh(),
            0x0E => CrtcTiming::mode_0eh(),
            0x0F => CrtcTiming::mode_0fh(),
            0x10 => CrtcTiming::mode_10h(),
            0x11 => CrtcTiming::mode_11h(),
            0x12 => CrtcTiming::mode_12h(),
            0x13 => {
                self.set_mode13h_with_clear(clear);
                return true;
            }
            _ => return false,
        };
        self.set_planar_mode(mode, timing, clear);
        true
    }

    pub fn raster_width(&self) -> u32 {
        self.crtc.hdisp_end
    }

    /// Scanlines per source row (the double-scan factor). For every mode this
    /// slice supports this equals `max_scan + 1`, the form the spec and the
    /// conformance doc use for the source divide; a triple-scan mode would have
    /// to read `max_scan` directly.
    fn scan_factor(&self) -> u32 {
        if self.crtc.double_scan { 2 } else { 1 }
    }

    /// Full visible frame height in raster lines. One raster row per scanline, so
    /// this is `vtotal`; double-scan divides the source address (see
    /// `render_active_row`) rather than multiplying the output.
    pub fn raster_height(&self) -> u32 {
        self.crtc.vtotal
    }

    fn resize_work(&mut self) {
        self.work = vec![0; (self.raster_width() * self.raster_height()) as usize];
    }

    /// Render scanlines from last_line up to (not including) the current beam
    /// line, using current register state. Returns how many lines were drawn.
    pub fn catch_up(&mut self) -> u32 {
        let current = beam_line(&self.crtc, self.beam);
        let mut drawn = 0;
        while self.last_line < current {
            self.render_scanline(self.last_line);
            self.last_line += 1;
            drawn += 1;
        }
        drawn
    }

    fn text_aperture_size(&self) -> usize {
        if self.is_cga_text_mode() {
            CGA_FB_SIZE
        } else {
            VGA_TEXT_MEMORY_SIZE
        }
    }

    fn text_byte(&self, offset: usize) -> u8 {
        if self.is_cga_text_mode() {
            return self.cga_read(offset);
        }
        self.text_memory[offset % self.text_aperture_size()]
    }

    /// Byte offset of the char/attr pair for a displayed cell at `(char_row, col)`
    /// relative to the start-address origin `start_cells` (word/cell units),
    /// wrapped at the live text aperture. Mode 03h is word mode, so the cell
    /// index is `start_cells + char_row*offset + col` and the byte pair sits at
    /// that index times two. Shared by pixel scanout and the headless cell view.
    fn text_cell_base(&self, start_cells: usize, char_row: usize, col: usize) -> usize {
        ((start_cells + char_row * self.crtc.offset as usize + col) * 2) % self.text_aperture_size()
    }

    /// Display-address origin for one scanline, honoring the CRTC Line Compare
    /// split (Abrash, Graphics Programming Black Book ch.30). Returns
    /// `(start_base, first_line)`: above the split the start address scrolls the
    /// region; at and below the split threshold the address counter reloads to 0
    /// and row counting restarts there. VGA starts the split at `line_compare + 1`;
    /// EGA-era planar BIOS modes start it two scanlines lower. The comparison is
    /// in scan-counter units and is not divided by the double-scan factor.
    fn split_origin(&self, counter_line: u32) -> (u32, u32) {
        let first_line = self.split_first_line();
        if counter_line >= first_line {
            (0, first_line)
        } else {
            (self.crtc.start_address, 0)
        }
    }

    fn split_first_line(&self) -> u32 {
        self.crtc
            .line_compare
            .saturating_add(1)
            .saturating_add(self.ega_split_delay())
    }

    fn below_split(&self, counter_line: u32) -> bool {
        counter_line >= self.split_first_line()
    }

    fn ega_split_delay(&self) -> u32 {
        if self.mode == VideoMode::Planar && matches!(self.planar_bios_mode, 0x0D..=0x10) {
            2
        } else {
            0
        }
    }

    /// Effective horizontal pel-pan for one scanline, honoring the Attribute Mode
    /// Control (10h) bit 5 "enable pixel panning up to line compare" forcing
    /// (RBIL PORTS.B table P0664): below the CRTC Line Compare split the pan is
    /// forced to 0 when bit 5 is set. Returns the raw 13h value masked to 0-15;
    /// the mode-X caller masks further to 0-3.
    fn pel_pan(&self, below_split: bool) -> usize {
        if self.pan_resets_below_split(below_split) {
            0
        } else {
            (self.attr.pixel_pan & 0x0F) as usize
        }
    }

    fn text_pel_pan(&self, below_split: bool, char_width: usize) -> usize {
        let pan = self.pel_pan(below_split);
        if char_width == 9 && pan == 8 {
            0
        } else {
            pan.min(char_width - 1)
        }
    }

    /// Whether the horizontal pan (AC 13h pel-pan and CRTC 08h byte pan) is forced
    /// to 0 below the CRTC Line Compare split: only when AC Mode Control 10h bit 5
    /// is set (FreeVGA crtcreg.htm 18h). Shared by `pel_pan` and the byte-pan
    /// computation so the two horizontal pans obey the same rule. The CRTC 08h
    /// preset-row-scan reset below the split is unconditional and stays separate.
    fn pan_resets_below_split(&self, below_split: bool) -> bool {
        below_split && (self.attr.mode_control & 0x20 != 0)
    }

    fn preset_row_scan(&self, below_split: bool) -> u32 {
        if below_split {
            0
        } else {
            u32::from(self.crtc.preset_row_scan & 0x1F)
        }
    }

    fn byte_pan(&self, below_split: bool) -> u32 {
        if self.pan_resets_below_split(below_split) {
            0
        } else {
            u32::from((self.crtc.preset_row_scan >> 5) & 0x03)
        }
    }

    fn sequencer_outputs_enabled(&self) -> bool {
        self.seq.reset & 0x03 == 0x03 && self.seq.clocking_mode & 0x20 == 0
    }

    /// Fold the Attribute Color Select register (14h) into a 6-bit attribute
    /// palette value to form the 8-bit DAC index, then apply the pel mask. In the
    /// 16-color and text paths the attribute palette is 6 bits wide; the Color
    /// Select supplies the top DAC bits (FreeVGA attrreg.htm 10h/14h):
    ///
    /// DAC index bits 7-6 always come from Color Select (14h) bits 3-2. Bits 3-0 always
    /// come from the palette register. Bits 5-4 depend on AC Mode Control (10h) bit 7:
    /// - bit 7 clear: DAC bits 5-4 are the palette register's own bits 5-4 (the full 6-bit
    ///   palette value passes through), with Color Select 3-2 supplying bits 7-6.
    /// - bit 7 set: the palette value's bits 5-4 are replaced by Color Select bits 1-0
    ///   (the "P5/P4 from C0/C1" page-select mode), with Color Select 3-2 still bits 7-6.
    ///
    /// The pel mask (3C6) gates the final index in both cases.
    fn dac_index(&self, palette_6bit: u8) -> u8 {
        let cs = self.attr.color_select;
        let index = if self.attr.mode_control & 0x80 == 0 {
            (palette_6bit & 0x3F) | ((cs & 0x0C) << 4)
        } else {
            (palette_6bit & 0x0F) | ((cs & 0x03) << 4) | ((cs & 0x0C) << 4)
        };
        index & self.pel_mask
    }

    fn attr_lookup(&self, index: u8) -> u8 {
        self.attr.palette[(index & self.attr.plane_enable & 0x0F) as usize] & 0x3F
    }

    fn planar_logical_attr_index(&self, plane_bits: u8) -> u8 {
        match self.planar_bios_mode {
            // EGA/VGA mode 0Fh uses C0 as the video plane and C2 as the
            // intensity/blink plane: 00,01,10,11 -> attributes 0,3,C,F.
            0x0F => match plane_bits & 0x05 {
                0x00 => 0x00,
                0x01 => 0x03,
                0x04 => 0x0C,
                _ => 0x0F,
            },
            // VGA mode 11h is mode-6 style one-bit graphics in map 0.
            0x11 => {
                if plane_bits & 0x01 != 0 {
                    0x0F
                } else {
                    0x00
                }
            }
            _ => plane_bits & 0x0F,
        }
    }

    fn planar_scanout_attr_index(&self, plane_bits: u8) -> u8 {
        let index = self.planar_logical_attr_index(plane_bits);
        if self.planar_bios_mode == 0x0F
            && index == 0x0C
            && self.attr.mode_control & 0x08 != 0
            && self.blink_hide_phase()
        {
            0
        } else {
            index
        }
    }

    fn planar_storage_bits(&self, color: u8) -> u8 {
        match self.planar_bios_mode {
            0x0F => u8::from(color & 0x01 != 0) | (u8::from(color & 0x04 != 0) << 2),
            0x11 => u8::from(color & 0x01 != 0),
            _ => color & 0x0F,
        }
    }

    fn video_status_mux_bits(&self) -> u8 {
        if self.is_cga_personality() || !beam_display_enable(&self.crtc, self.beam) {
            return 0;
        }
        let line = beam_line(&self.crtc, self.beam);
        let dot = beam_dot(&self.crtc, self.beam) as usize;
        let color = match self.mode {
            VideoMode::Mode13h | VideoMode::ModeX => self.render_256color_row(line)[dot],
            VideoMode::Text => self.render_text_row(line)[dot],
            VideoMode::Planar => self.render_active_row(line)[dot],
            VideoMode::Cga => 0,
        };
        let pair = match (self.attr.plane_enable >> 4) & 0x03 {
            0x00 => (((color >> 2) & 1) << 1) | (color & 1),
            0x01 => (color >> 4) & 0x03,
            0x02 => (((color >> 3) & 1) << 1) | ((color >> 1) & 1),
            _ => (color >> 6) & 0x03,
        };
        pair << 4
    }

    /// Assemble one active scanline into `hdisp_end` DAC indices, applying pel-pan
    /// and the attribute palette. `counter_line` is the scanline in scan-counter
    /// units; double-scan maps it to source row `counter_line / scan_factor`, so a
    /// doubled mode holds each VRAM row for two scanlines.
    pub fn render_active_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        // Line Compare split (CRTC 18h + 07h.4 + 09h.6). The comparison is in
        // scan-counter units, so it is not divided by the double-scan factor.
        let below_split = self.below_split(counter_line);
        let (start, first_line) = self.split_origin(counter_line);
        let pan = self.pel_pan(below_split);
        let row_scan = counter_line - first_line + self.preset_row_scan(below_split);
        let source_row = row_scan / self.scan_factor();
        // The per-scanline counter increment is offset*2 in every addressing mode; the
        // byte/word/doubleword transform lives in display_offset, not the stride.
        let row_base = start + source_row * self.crtc.offset * 2 + self.byte_pan(below_split);
        let mut row = vec![0u8; width];
        for (x, slot) in row.iter_mut().enumerate() {
            let px = x + pan;
            let byte = px / 8;
            let bit = 7 - (px % 8);
            let ma = display_counter(
                self.crtc.mode_control,
                self.crtc.underline_loc,
                row_base,
                byte as u32,
            );
            let off = display_offset_row(
                self.crtc.mode_control,
                self.crtc.underline_loc,
                ma,
                row_scan,
            );
            let mut index = 0u8;
            for plane in 0..VGA_PLANES {
                let b = self.vram[plane * VGA_PLANE_SIZE + off];
                index |= ((b >> bit) & 1) << plane;
            }
            *slot = self.dac_index(self.attr_lookup(self.planar_scanout_attr_index(index)));
        }
        row
    }

    /// Assemble one 256-color scanline, shared by chained mode 13h and unchained
    /// mode X. Chain-4 (Sequencer Memory Mode 04h bit 3) changes only the CPU
    /// write/read decode, so the CRTC display scanout is identical in both modes:
    /// Abrash, Graphics Programming Black Book ch.47 gives `M = N/4` (plane
    /// offset), `P = N mod 4` (plane). Four planes are column-interleaved: pixel
    /// x is plane `x_eff & 3` at plane offset `row_base + (x_eff >> 2)`, where
    /// `x_eff = x + pan`, and the byte is the 8-bit DAC index directly (no
    /// attribute palette, no 6-bit mask). `counter_line` is in scan-counter
    /// units; double-scan maps it to the source row, exactly as the 16-color
    /// path.
    /// The CRTC Line Compare split is applied: at and below `line_compare + 1`
    /// the display-address counter reloads to 0 and row counting restarts there
    /// (Abrash, Graphics Programming Black Book ch.30). The AC Horizontal Pixel
    /// Panning register (13h) applies as a fine 0-3 column shift (one plane per
    /// pel, four pels per plane-offset address) through the shared `pel_pan`,
    /// which also forces it to 0 below the split when AC Mode Control (10h) bit 5
    /// is set.
    pub fn render_256color_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        let below_split = self.below_split(counter_line);
        let (start, first_line) = self.split_origin(counter_line);
        // The split branch returns first_line = line_compare + 1 and is taken only when
        // counter_line > line_compare, so counter_line >= first_line holds: the
        // subtraction never underflows.
        let row_scan = counter_line - first_line + self.preset_row_scan(below_split);
        let source_row = row_scan / self.scan_factor();
        let row_base = start + source_row * self.crtc.offset * 2 + self.byte_pan(below_split);
        // Mode-X pel-pan: one plane per pel, so the fine range is 0-3 (a pan of 4
        // equals a start-address bump). The below-split forcing is shared with the
        // 16-color path through pel_pan.
        let pan = (self.pel_pan(below_split) & 0x03) as u32;
        let mut row = vec![0u8; width];
        for (x, slot) in row.iter_mut().enumerate() {
            let x_eff = x as u32 + pan;
            let plane = (x_eff & 3) as usize;
            let ma = display_counter(
                self.crtc.mode_control,
                self.crtc.underline_loc,
                row_base,
                x_eff >> 2,
            );
            let off = display_offset_row(
                self.crtc.mode_control,
                self.crtc.underline_loc,
                ma,
                row_scan,
            );
            *slot = self.vram[plane * VGA_PLANE_SIZE + off] & self.pel_mask;
        }
        row
    }

    /// Assemble one text-mode scanline (counter line) into `hdisp_end` DAC
    /// indices, sharing the raster engine with the graphics paths. Text mode lays
    /// out the active column count in `max_scan + 1` scanlines per cell; the
    /// CRTC Line Compare split reuses `split_origin`, with the character-row
    /// count restarting below the split. VGA text maps each cell's foreground and
    /// background nibbles through the Attribute palette and pel mask; CGA text
    /// decodes those nibbles directly as RGBI color indexes. Blink (Attribute
    /// Mode Control 10h bit 3, or CGA mode-control 3D8h bit 5) collapses the
    /// foreground to the background on its hide phase; with blink clear, attribute
    /// bit 7 is background intensity instead. In 9-dot mode the 9th pixel column
    /// replicates the 8th for the box-drawing glyphs 0xC0-0xDF (a solid line join)
    /// and is the background otherwise (Abrash, Graphics Programming Black Book).
    pub fn render_text_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        let cga_text = self.is_cga_text_mode();
        if cga_text && self.cga.mode_control & CGA_MODE_VIDEO_ENABLE == 0 {
            return vec![CGA_BLACK; width];
        }
        let rows_per_char = self.crtc.max_scan + 1;
        // The display origin scrolls with the CRTC Start Address (0C/0Dh). Above
        // the line-compare split the origin is `start_address`; at and below the
        // split the counter reloads to 0 (split_origin). Mode 03h is word mode
        // (CR17 bit 6 clear), so `start_address` is a word/cell address, the same
        // units as the CRTC cursor location (0E/0Fh): a displayed cell at
        // (char_row, col) has the absolute cell index `start + char_row*offset +
        // col` and reads the char/attr byte pair at that cell index * 2. The byte
        // read wraps at the 32 KB text aperture (FreeVGA 0Dh wrap behavior). See
        // A1 in dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
        let below_split = self.below_split(counter_line);
        let (start, first_line) = self.split_origin(counter_line);
        // split_origin returns first_line <= counter_line in both branches, so the
        // subtraction never underflows.
        let rel = counter_line - first_line;
        // CRTC Preset Row Scan (08h, FreeVGA crtcreg.htm): bits 4-0 offset the
        // first displayed font scanline (vertical sub-row smooth scroll), bits 6-5
        // are the byte pan added to the start address. Below the line-compare split
        // the preset always resets to 0; the byte pan resets to 0 below the split
        // only when AC 10h bit 5 is set (FreeVGA 18h). See A3 in
        // dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
        let preset_row = self.preset_row_scan(below_split);
        let byte_pan = self.byte_pan(below_split) as usize;
        // Effective scanline = rel + preset_row scrolls the display up; char_row
        // advances when the addition wraps past rows_per_char.
        let eff = rel + preset_row;
        let char_row = (eff / rows_per_char) as usize;
        let font_line = (eff % rows_per_char) as usize;
        #[allow(clippy::if_same_then_else)]
        let char_width = if cga_text {
            8
        } else if self.seq.clocking_mode & 0x01 != 0 {
            8
        } else {
            9
        };
        // AC 13h Horizontal Pixel Panning shifts the display left by `pan` pels
        // (FreeVGA attrreg.htm 13h). A non-zero pan reveals the right portion of
        // each cell and pulls in the leading pixels of the cell after the last
        // visible column; the leftmost `pan` pels of cell 0 scroll off the left
        // edge. Range 0..char_width (0-8 for 9-dot, 0-7 for 8-dot); routed through
        // the shared pel_pan so AC 10h bit 5 forces it to 0 below the line-compare
        // split (FreeVGA crtcreg.htm 18h). See A2 in
        // dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
        let pan = if cga_text {
            0
        } else {
            self.text_pel_pan(below_split, char_width)
        };
        let blink_enabled = if cga_text {
            self.cga.mode_control & CGA_MODE_BLINK != 0
        } else {
            self.attr.mode_control & 0x08 != 0
        };
        // The shared blink hide phase: 16 frames on, 16 off, driven by the frame
        // (vertical-retrace) counter. Attribute blink and the cursor blink both
        // read this single source. See A6 in
        // dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
        let blink_hide_phase = self.blink_hide_phase();
        let start_cells = start as usize;
        // VGA text uses Sequencer font maps; CGA text uses the fixed 8x8
        // character ROM, so attribute bit 3 stays foreground intensity there.
        let table_a = self.active_font_table();
        let table_b = self.active_font_table_b();
        let dual_font = !cga_text && table_a != table_b;
        // VGA uses 0Ah bit 5 as cursor disable and 0Bh bits 5-6 as cursor skew.
        // CGA's 6845 instead uses 0Ah bits 5-6 as cursor mode; R11 is 5-bit.
        let (skew, cursor_disabled, cursor_hidden) = if cga_text {
            let mode = (self.cursor_start >> 5) & 0x03;
            let hidden = match mode {
                0x00 => false,
                0x01 => false,
                0x02 => blink_hide_phase,
                _ => (self.frames / 32) % 2 == 1,
            };
            (0, mode == 0x01, hidden)
        } else {
            (
                (self.cursor_end >> 5) & 0x03,
                self.cursor_start & 0x20 != 0,
                blink_hide_phase,
            )
        };
        let text_aperture_size = self.text_aperture_size();
        let cursor_byte = ((self.cursor_offset as usize + skew as usize) * 2) % text_aperture_size;
        let start_line = (self.cursor_start & 0x1F) as usize;
        let end_line = (self.cursor_end & 0x1F) as usize;
        let mut row = vec![0u8; width];
        // Render one extra cell column so a non-zero pan's right edge pulls in the
        // next cell's leading pixels; the left edge clips cell 0's scrolled-off
        // leading pixels.
        for dc in 0..=self.text_columns {
            // Absolute cell index (char/attr pair) scrolled by the start address;
            // the CRTC byte pan (08h bits 6-5) adds a byte offset to the origin,
            // so a pan of 2 shifts one whole cell and a pan of 1 lands on the
            // attribute byte (the real-hardware half-cell scramble).
            let base =
                (self.text_cell_base(start_cells, char_row, dc) + byte_pan) % text_aperture_size;
            let char_byte = self.text_byte(base);
            let attr = self.text_byte(base + 1);
            let blink_attr = attr & 0x80 != 0;
            // 512-glyph mode: when the Sequencer selects two distinct font tables
            // (map A != map B), attribute bit 3 becomes the per-cell font selector
            // and is no longer foreground intensity, so the foreground is masked to
            // 8 colors. See A4 in dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
            let font_select = (attr >> 3) & 1 != 0;
            let font_table = if dual_font && font_select {
                table_b
            } else {
                table_a
            };
            let fg_index = if dual_font {
                (attr & 0x07) as usize
            } else {
                (attr & 0x0F) as usize
            };
            let bg_index = if blink_enabled && blink_attr {
                ((attr >> 4) & 0x07) as usize
            } else {
                ((attr >> 4) & 0x0F) as usize
            };
            let mut fg = if cga_text {
                fg_index as u8
            } else {
                self.dac_index(self.attr_lookup(fg_index as u8))
            };
            let mut bg = if cga_text {
                bg_index as u8
            } else {
                self.dac_index(self.attr_lookup(bg_index as u8))
            };
            let hide_fg = blink_enabled && blink_attr && blink_hide_phase;
            // Hardware text cursor (CRTC 0A/0B): on the cursor cell, swap fg/bg
            // on the active scanlines for reverse video. 0A bit 5 disables the
            // cursor; bits 0-4 of 0A/0B bound the scanline range (start > end
            // wraps). The cursor blinks on the same hide phase as attribute
            // blink, but is not gated on the attribute-blink enable. The cursor
            // location register (0E/0Fh) is a cell index, so its byte address is
            // cursor_offset*2; it fires when the displayed cell's byte offset
            // matches, scrolling with the start address. The Cursor Skew (0Bh
            // bits 6-5) delays the onset by that many character clocks, so the
            // effective cursor cell is cursor_offset + skew (FreeVGA crtcreg.htm
            // 0Bh; IBM VGA, not the clone "skew 3 = off" variant). See A5 in
            // dev_docs/reference/vga/text-mode-gaps-confirm-notes.md. The skew,
            // cursor byte, disable bit, and scanline range are decoded once per
            // scanline above the loop.
            let cursor_here = base == cursor_byte;
            let in_range = if start_line <= end_line {
                font_line >= start_line && font_line <= end_line
            } else {
                font_line >= start_line || font_line <= end_line
            };
            if cursor_here && !cursor_disabled && in_range && !cursor_hidden {
                std::mem::swap(&mut fg, &mut bg);
            }
            // VGA reads the active writable font table. CGA has a fixed 8x8 ROM
            // character generator, not VGA plane-2 font RAM.
            let glyph_row = if cga_text {
                crate::font::VGAFONT_8X8[char_byte as usize * 8 + font_line.min(7)]
            } else {
                self.font[font_table][char_byte as usize * 32 + font_line.min(31)]
            };
            let extend_ninth = (0xC0..=0xDF).contains(&char_byte);
            // Place the cell shifted left by `pan` pels. Use signed math so cell 0's
            // leading `pan` pels (which scroll off the left edge) clip to negative
            // positions instead of underflowing usize.
            let cell_origin = dc as isize * char_width as isize;
            for px in 0..char_width {
                let x = cell_origin + px as isize - pan as isize;
                if x < 0 || x as usize >= width {
                    continue;
                }
                let lit = if px < 8 {
                    (glyph_row >> (7 - px)) & 1 != 0
                } else {
                    // 9th column: replicate the 8th (bit 0) for box glyphs.
                    extend_ninth && (glyph_row & 0x01 != 0)
                };
                row[x as usize] = if lit && !hide_fg { fg } else { bg };
            }
        }
        row
    }

    fn region_color(&self, scan_line: u32) -> u8 {
        // scan_line in counter units; caller guarantees scan_line >= vdisp_end.
        if self.is_cga_personality() && self.cga.mode_control & CGA_MODE_VIDEO_ENABLE == 0 {
            return CGA_BLACK;
        }
        if scan_line < self.crtc.vblank_start || scan_line >= self.crtc.vblank_end {
            if self.is_cga_text_mode() {
                return self.cga.background_index();
            }
            if self.mode == VideoMode::Cga {
                return match self.cga.submode {
                    CgaMode::Graphics320x200 => self.cga.background_index(),
                    CgaMode::Graphics640x200 => CGA_BLACK,
                };
            }
            self.attr.overscan & 0x3F // border = overscan color
        } else {
            0 // vertical blank = black
        }
    }

    /// Render one scanline (counter line) into a single raster row. Active lines
    /// come from the planes; below `vdisp_end` the row is the border or blank
    /// color. `catch_up` and `render_full_frame` both step in counter lines, the
    /// space the beam counts in.
    fn render_scanline(&mut self, counter_line: u32) {
        let width = self.raster_width() as usize;
        let pixels =
            if !self.display_refresh_enabled || !self.attr.pas || !self.sequencer_outputs_enabled()
            {
                vec![0u8; width]
            } else if counter_line < self.crtc.vdisp_end {
                match self.mode {
                    VideoMode::Mode13h | VideoMode::ModeX => self.render_256color_row(counter_line),
                    VideoMode::Text => self.render_text_row(counter_line),
                    VideoMode::Cga => self.render_cga_row(counter_line),
                    _ => self.render_active_row(counter_line),
                }
            } else {
                vec![self.region_color(counter_line); width]
            };
        let dst = counter_line as usize * width;
        if dst + width <= self.work.len() {
            self.work[dst..dst + width].copy_from_slice(&pixels);
        }
    }

    /// Render an entire frame to a fresh raster (used by tests/goldens).
    pub fn render_full_frame(&mut self) -> VgaRaster {
        let w = self.raster_width();
        let h = self.raster_height();
        self.work = vec![0u8; (w * h) as usize];
        for counter_line in 0..self.crtc.vtotal {
            self.render_scanline(counter_line);
        }
        VgaRaster {
            width: w,
            height: h,
            display_height: self.crtc.vdisp_end,
            pixels: self.work.clone(),
        }
    }

    fn finalize_frame(&mut self) {
        // Render the lines the beam has not yet crossed, with the current register
        // state, so a mid-frame change shows below the seam.
        while self.last_line < self.crtc.vtotal {
            self.render_scanline(self.last_line);
            self.last_line += 1;
        }
        // Every mode (planar, mode X, mode 13h, and text) sizes `work` at its
        // mode-set, so a frame built from it has the matching pixel count. The
        // empty-work guard only suppresses publication before any mode is set.
        if !self.work.is_empty() {
            self.presented = Some(VgaRaster {
                width: self.raster_width(),
                height: self.raster_height(),
                display_height: self.crtc.vdisp_end,
                pixels: self.work.clone(),
            });
        }
        if let Some(addr) = self.pending_start.take() {
            // A start-address latch changes the scanout origin with no VRAM/register
            // write of its own, so bump the content generation here (only when it
            // actually moves) so the host dirty-framebuffer cache re-renders.
            if addr != self.crtc.start_address {
                self.bump_content_gen();
            }
            self.crtc.start_address = addr; // latched for the next frame
        }
        self.last_line = 0;
    }

    pub fn presented_ready(&self) -> bool {
        self.presented.is_some()
    }

    pub fn take_presented(&mut self) -> Option<VgaRaster> {
        self.presented.take()
    }

    /// The most recent finalized frame, read without consuming it. A host polling
    /// faster than frames complete keeps seeing the last frame instead of black.
    pub fn last_presented(&self) -> Option<&VgaRaster> {
        self.presented.as_ref()
    }

    /// Advance the beam by whole dots, rolling over each completed frame
    /// arithmetically (O(1)).
    pub fn advance(&mut self, dots: u64) {
        let frame = self.frame_dots();
        if frame == 0 {
            return; // guard: un-programmed CRTC
        }
        let total = self.beam + dots;
        let crossed = total / frame;
        if crossed > 0 {
            if crossed > 1 {
                self.last_line = 0; // skipped frames: the final frame is a full render
            }
            self.finalize_frame(); // finalize only the final completed frame
            self.frames += crossed;
        }
        self.beam = total % frame;
    }

    pub fn plane_byte(&self, plane: usize, offset: usize) -> u8 {
        self.vram[plane * VGA_PLANE_SIZE + offset]
    }

    fn plane_slice_mut(&mut self, offset: usize) -> [[u8; 1]; VGA_PLANES] {
        let mut planes = [[0u8; 1]; VGA_PLANES];
        for (plane, slot) in planes.iter_mut().enumerate() {
            slot[0] = self.vram[plane * VGA_PLANE_SIZE + offset];
        }
        planes
    }

    fn store_planes(&mut self, offset: usize, planes: &[[u8; 1]; VGA_PLANES]) {
        for (plane, slot) in planes.iter().enumerate() {
            if (self.seq.map_mask >> plane) & 1 != 0 {
                self.vram[plane * VGA_PLANE_SIZE + offset] = slot[0];
            }
        }
    }

    fn odd_even_offset(offset: usize) -> (usize, usize) {
        (offset >> 1, offset & 1)
    }

    fn cpu_write_odd_even(&mut self, offset: usize, data: u8) {
        let (plane_offset, odd) = Self::odd_even_offset(offset);
        if plane_offset >= VGA_PLANE_SIZE {
            return;
        }
        let mut planes = self.plane_slice_mut(plane_offset);
        let old = planes;
        let gc = self.gc;
        let latches = self.latches;
        write_planes(&mut planes, data, &gc, &latches);
        for plane in 0..VGA_PLANES {
            if plane & 1 == odd && (self.seq.map_mask >> plane) & 1 != 0 {
                self.vram[plane * VGA_PLANE_SIZE + plane_offset] = planes[plane][0];
            } else {
                self.vram[plane * VGA_PLANE_SIZE + plane_offset] = old[plane][0];
            }
        }
    }

    fn cpu_read_odd_even(&mut self, offset: usize) -> u8 {
        let (plane_offset, odd) = Self::odd_even_offset(offset);
        if plane_offset >= VGA_PLANE_SIZE {
            return 0xFF;
        }
        let planes = self.plane_slice_mut(plane_offset);
        for (plane, slot) in planes.iter().enumerate() {
            self.latches[plane] = slot[0];
        }
        let plane = (usize::from(self.gc.read_map & 0x02)) | odd;
        planes[plane][0]
    }

    pub fn cpu_write(&mut self, offset: usize, data: u8) {
        if offset >= VGA_PLANE_SIZE {
            return;
        }
        self.bump_content_gen();
        if self.seq.memory_mode & 0x04 == 0 {
            self.cpu_write_odd_even(offset, data);
            return;
        }
        let mut planes = self.plane_slice_mut(offset);
        let gc = self.gc;
        let latches = self.latches;
        write_planes(&mut planes, data, &gc, &latches);
        self.store_planes(offset, &planes);
    }

    pub fn cpu_read(&mut self, offset: usize) -> u8 {
        if offset >= VGA_PLANE_SIZE {
            return 0xFF;
        }
        if self.gc.mode_odd_even() && self.gc.aperture().chain_odd_even {
            return self.cpu_read_odd_even(offset);
        }
        let planes = self.plane_slice_mut(offset);
        let gc = self.gc;
        read_planes(&planes, &gc, &mut self.latches)
    }

    fn planar_pixel_offset_at(&self, start: u32, x: u16, y: u16) -> Option<(usize, u8)> {
        if self.mode != VideoMode::Planar {
            return None;
        }
        let x = u32::from(x);
        let y = u32::from(y);
        let source_height = self.crtc.vdisp_end / self.scan_factor();
        if x >= self.crtc.hdisp_end || y >= source_height {
            return None;
        }
        let row_base = start + y * self.crtc.offset * 2;
        let ma = display_counter(
            self.crtc.mode_control,
            self.crtc.underline_loc,
            row_base,
            x / 8,
        );
        let offset = display_offset(self.crtc.mode_control, self.crtc.underline_loc, ma);
        if offset >= VGA_PLANE_SIZE {
            return None;
        }
        Some((offset, (7 - (x & 7)) as u8))
    }

    pub fn planar_write_pixel(&mut self, x: u16, y: u16, color: u8, xor: bool) -> bool {
        self.planar_write_pixel_at(0, x, y, color, xor)
    }

    pub fn planar_write_pixel_at(
        &mut self,
        start: u32,
        x: u16,
        y: u16,
        color: u8,
        xor: bool,
    ) -> bool {
        let Some((offset, bit)) = self.planar_pixel_offset_at(start, x, y) else {
            return false;
        };
        self.bump_content_gen();
        let old = self.planar_read_pixel_at(start, x, y);
        let color = self.planar_storage_bits(if xor { old ^ color } else { color });
        let mask = 1 << bit;
        for plane in 0..VGA_PLANES {
            let byte = &mut self.vram[plane * VGA_PLANE_SIZE + offset];
            if (color >> plane) & 1 != 0 {
                *byte |= mask;
            } else {
                *byte &= !mask;
            }
        }
        true
    }

    pub fn planar_read_pixel(&self, x: u16, y: u16) -> u8 {
        self.planar_read_pixel_at(0, x, y)
    }

    pub fn planar_read_pixel_at(&self, start: u32, x: u16, y: u16) -> u8 {
        let Some((offset, bit)) = self.planar_pixel_offset_at(start, x, y) else {
            return 0;
        };
        let mut color = 0u8;
        for plane in 0..VGA_PLANES {
            color |= ((self.vram[plane * VGA_PLANE_SIZE + offset] >> bit) & 1) << plane;
        }
        self.planar_logical_attr_index(color)
    }

    /// Chained mode-13h CPU write: chain-4 (Sequencer Memory Mode 04h bit 3)
    /// routes byte N straight to plane `N & 3` at plane-offset `N >> 2`, bypassing
    /// the planar datapath (map mask, write mode, bit mask, latches). This is the
    /// CPU write decode that mode X turns off; the CRTC display scanout reads the
    /// same four-plane VRAM either way (Abrash, Graphics Programming Black Book
    /// ch.47).
    pub fn cpu_write_chain4(&mut self, offset: usize, data: u8) {
        self.bump_content_gen();
        let plane = offset & 0x3;
        let plane_off = offset >> 2;
        if plane_off < VGA_PLANE_SIZE {
            self.vram[plane * VGA_PLANE_SIZE + plane_off] = data;
        }
    }

    /// Chained mode-13h CPU read: chain-4 selects plane `N & 3` at plane-offset
    /// `N >> 2` via the low two address bits, the symmetric counterpart to
    /// `cpu_write_chain4`.
    pub fn cpu_read_chain4(&self, offset: usize) -> u8 {
        let plane = offset & 0x3;
        let plane_off = offset >> 2;
        if plane_off < VGA_PLANE_SIZE {
            self.vram[plane * VGA_PLANE_SIZE + plane_off]
        } else {
            0xFF
        }
    }

    /// Buffer a CRTC start-address change. The value is latched into the active
    /// start address at the next frame boundary (finalize_frame), so mid-frame
    /// writes do not tear.
    pub fn set_start_address(&mut self, addr: u32) {
        self.pending_start = Some(addr); // snapshot at next vretrace (finalize)
    }

    /// Move the hardware text cursor (CRTC 0E/0Fh) to a cell offset. The HLE
    /// teletype uses this so the visible cursor tracks the BDA cursor without a
    /// round trip through CRTC port writes.
    pub fn set_cursor_offset(&mut self, offset: u16) {
        self.cursor_offset = offset;
    }

    /// Set the hardware text cursor shape (CRTC 0A/0Bh).
    pub fn set_cursor_shape(&mut self, start: u8, end: u8) {
        if self.is_cga_personality() {
            self.cursor_start = start & 0x7F;
            self.cursor_end = end & 0x1F;
        } else {
            self.cursor_start = start;
            self.cursor_end = end;
        }
    }

    /// Read Input Status Register 1 (port 3DAh).
    ///
    /// Bit 0: display inactive. Attribute PAS blanking and CGA 3D8h video-disable
    /// make VRAM access safe for the whole frame instead of only during beam
    /// blank/retrace.
    /// Bit 1: CGA light pen trigger latched.
    /// Bit 2: CGA light pen switch off (no attached switch pressed).
    /// Bit 3: vertical retrace active.
    ///
    /// Reading this register also resets the Attribute Controller address/data
    /// flip-flop so that the next write to 3C0 is treated as an index.
    pub fn read_status1(&mut self) -> u8 {
        self.catch_up(); // a 3DA read catches the raster up, like a register write
        self.attr.flip_flop_data = false; // reading 3DA resets the flip-flop
        let mut status = 0u8;
        let display_disabled = !self.display_refresh_enabled
            || !self.attr.pas
            || !self.sequencer_outputs_enabled()
            || (self.is_cga_personality() && self.cga.mode_control & CGA_MODE_VIDEO_ENABLE == 0);
        let display_inactive = display_disabled || !beam_display_enable(&self.crtc, self.beam);
        if display_inactive {
            status |= 0x01; // display inactive / safe VRAM window
        }
        if self.is_cga_personality() {
            if self.cga.light_pen_triggered {
                status |= 0x02;
            }
            status |= 0x04; // no light pen switch is pressed/attached
        }
        if beam_vretrace(&self.crtc, self.beam) {
            status |= 0x08; // vertical retrace
        }
        status |= self.video_status_mux_bits();
        status
    }

    /// Read Input Status Register 0 (port 3C2h).
    ///
    /// Bit 4: the display switch sense bit selected by Misc Output bits 3-2.
    /// Bit 7: vertical retrace active (the CRT interrupt status the BIOS polls).
    pub fn read_status0(&mut self) -> u8 {
        self.catch_up(); // a 3C2 read catches the raster up, like 3DA
        let mut status = 0u8;
        if self.switch_sense_bit() {
            status |= 0x10;
        }
        if beam_vretrace(&self.crtc, self.beam) {
            status |= 0x80; // vertical retrace -> CRT interrupt status
        }
        status
    }

    /// Write to a VGA I/O port. Calls `catch_up()` first so any lines already
    /// past the beam are rendered with the previous register state before the
    /// new value takes effect. Returns `true` if the port is handled.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.catch_up();
        // Any VGA register / DAC write can change the scanout (palette, CRTC origin,
        // sequencer/GC/attribute state), so bump the content generation. This also
        // covers HLE BIOS palette writes (e.g. INT 10h AH=10h driving 0x3D9 directly).
        // Index-only and unhandled-port writes over-bump harmlessly.
        self.bump_content_gen();
        match port {
            0x3C2 => {
                self.misc_output = value;
                true
            }
            0x3C4 => {
                self.seq_index = value;
                true
            }
            0x3C5 => {
                let idx = self.seq_index;
                self.write_seq(idx, value);
                true
            }
            0x3C6 => {
                self.pel_mask = value;
                true
            }
            0x3C3 => {
                self.video_subsystem_enable = value & 0x01;
                true
            }
            0x3C7 => {
                self.dac.set_read_index(value);
                self.dac_state = 0x03; // armed for a DAC read
                true
            }
            0x3C8 => {
                self.dac.set_write_index(value);
                self.dac_state = 0x00; // armed for a DAC write
                true
            }
            0x3C9 => {
                if let Some(index) = self.dac.write_data(value) {
                    self.sum_dac_entry_to_gray(index);
                }
                self.dac_state = 0x00;
                true
            }
            0x3CE => {
                self.gc_index = value;
                true
            }
            0x3CF => {
                let idx = self.gc_index;
                self.write_gc(idx, value);
                true
            }
            0x3D0 | 0x3D2 | 0x3D4 | 0x3D6 if self.is_cga_personality() => {
                self.crtc_index = value & 0x1F;
                true
            }
            port if self.crtc_index_port_selected(port) => {
                self.crtc_index = value;
                true
            }
            0x3D1 | 0x3D3 | 0x3D5 | 0x3D7 if self.is_cga_personality() => {
                let idx = self.crtc_index;
                self.write_crtc(idx, value);
                true
            }
            port if self.crtc_data_port_selected(port) => {
                let idx = self.crtc_index;
                self.write_crtc(idx, value);
                true
            }
            0x3C0 => {
                self.write_attr(value);
                true
            }
            0x3D8 => {
                self.write_cga_mode_control(value);
                true
            }
            // CGA Color Select register: background/border nibble (bits 0-3),
            // intensity (bit 4), and palette select (bit 5). Decoded per scanline
            // in render_cga_row.
            0x3D9 => {
                self.cga.color_select = value & 0x3F;
                true
            }
            // Feature Control: written at 3DA in colour setups, 3BA in mono.
            // Read back at 3CA. The two write addresses are the colour/mono
            // alias of the same register.
            port if self.status1_port_selected(port) => {
                self.feature_control = value;
                true
            }
            0x3DB => {
                self.clear_cga_light_pen();
                true
            }
            0x3DC => {
                self.latch_cga_light_pen();
                true
            }
            _ => false,
        }
    }

    /// Read from a VGA I/O port. Returns `Some(value)` for handled ports.
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x3C2 => Some(self.read_status0()),
            0x3C0 => Some(self.attr.index | (u8::from(self.attr.pas) << 5)),
            0x3C1 => Some(self.attr_indexed_read()),
            0x3C3 => Some(self.video_subsystem_enable),
            0x3C4 => Some(self.seq_index),
            0x3C5 => Some(self.read_seq(self.seq_index)),
            0x3C6 => Some(self.pel_mask),
            0x3C7 => Some(self.dac_state & 0x03),
            0x3CA => Some(self.feature_control),
            0x3C8 => Some(self.dac.write_index()),
            0x3C9 => {
                self.dac_state = 0x03;
                Some(self.dac.read_data())
            }
            0x3CC => Some(self.misc_output),
            0x3CE => Some(self.gc_index),
            0x3CF => Some(self.read_gc(self.gc_index)),
            0x3D0 | 0x3D2 | 0x3D4 | 0x3D6 if self.is_cga_personality() => None,
            port if self.crtc_index_port_selected(port) => Some(self.crtc_index),
            0x3D1 | 0x3D3 | 0x3D5 | 0x3D7 if self.is_cga_personality() => {
                self.read_cga_crtc_data_port()
            }
            port if self.crtc_data_port_selected(port) => {
                Some(self.crtc_register_latch(self.crtc_index))
            }
            port if self.status1_port_selected(port) => Some(self.read_status1()),
            0x3DB => {
                self.catch_up();
                self.clear_cga_light_pen();
                Some(0xFF)
            }
            0x3DC => {
                self.catch_up();
                self.latch_cga_light_pen();
                Some(0xFF)
            }
            _ => None,
        }
    }

    fn read_cga_crtc_data_port(&self) -> Option<u8> {
        match self.crtc_index {
            0x0E => Some((self.cursor_offset >> 8) as u8),
            0x0F => Some(self.cursor_offset as u8),
            0x10 => Some((self.cga.light_pen_latch >> 8) as u8),
            0x11 => Some(self.cga.light_pen_latch as u8),
            _ => None,
        }
    }

    pub fn crtc_index_latch(&self) -> u8 {
        self.crtc_index
    }

    pub fn crtc_register_latch(&self, index: u8) -> u8 {
        if self.is_cga_personality() {
            return match index {
                0x00 => self.crtc_regs.r00,
                0x01 => self.crtc_regs.r01,
                0x02 => self.crtc_regs.r02,
                0x03 => self.crtc_regs.r03,
                0x04 => self.crtc_regs.r04,
                0x05 => self.crtc_regs.r05,
                0x06 => self.crtc_regs.r06,
                0x07 => self.crtc_regs.r07,
                0x08 => self.crtc_regs.r08,
                0x09 => self.crtc_regs.r09,
                0x0A => self.cursor_start,
                0x0B => self.cursor_end,
                0x0C => (self.crtc_start_register() >> 8) as u8,
                0x0D => self.crtc_start_register() as u8,
                0x0E => (self.cursor_offset >> 8) as u8,
                0x0F => self.cursor_offset as u8,
                0x10 => (self.cga.light_pen_latch >> 8) as u8,
                0x11 => self.cga.light_pen_latch as u8,
                _ => 0,
            };
        }
        match index {
            // Horizontal timing group: read back the byte last written (00h-05h).
            0x00 => self.crtc_regs.r00,
            0x01 => self.crtc_regs.r01,
            0x02 => self.crtc_regs.r02,
            0x03 => self.crtc_regs.r03,
            0x04 => self.crtc_regs.r04,
            0x05 => self.crtc_regs.r05,
            0x06 => self.crtc_regs.r06,
            0x07 => self.crtc_regs.r07,
            0x08 => self.crtc.preset_row_scan,
            0x09 => self.crtc_regs.r09,
            0x0A => self.cursor_start,
            0x0B => self.cursor_end,
            0x0C => (self.crtc_start_register() >> 8) as u8,
            0x0D => self.crtc_start_register() as u8,
            0x0E => (self.cursor_offset >> 8) as u8,
            0x0F => self.cursor_offset as u8,
            0x10 => self.crtc_regs.r10,
            0x11 => self.crtc_regs.r11,
            0x12 => self.crtc_regs.r12,
            0x13 => self.crtc_regs.r13,
            0x14 => self.crtc_regs.r14,
            0x15 => self.crtc_regs.r15,
            0x16 => self.crtc_regs.r16,
            0x17 => self.crtc_regs.r17,
            0x18 => self.crtc_regs.r18,
            _ => 0,
        }
    }

    /// Read the Attribute register selected by the last 3C0 index write (the
    /// 3C1 readback port). Mirrors `write_attr`'s data phase.
    fn attr_indexed_read(&self) -> u8 {
        match self.attr.index {
            0x00..=0x0F => self.attr.palette[self.attr.index as usize],
            0x10 => self.attr.mode_control,
            0x11 => self.attr.overscan,
            0x12 => self.attr.plane_enable,
            0x13 => self.attr.pixel_pan,
            0x14 => self.attr.color_select,
            _ => 0,
        }
    }

    fn write_seq(&mut self, index: u8, value: u8) {
        match index {
            0x00 => self.seq.reset = value,
            0x01 => self.seq.clocking_mode = value,
            0x02 => self.seq.map_mask = value & 0x0F,
            0x03 => self.seq.char_map_select = value,
            0x04 => {
                self.seq.memory_mode = value;
                // Chain-4 (bit 3) cleared while in chained 256-color (mode 13h)
                // selects unchained 256-color (mode X / mode Y). Setting it again
                // returns to chained mode 13h. Acting on the write that toggles the
                // bit is the faithful register-bang entry; the default memory_mode of
                // 0 cannot trigger it because set_mode13h never writes index 04h.
                let chain4_off = value & 0x08 == 0;
                if chain4_off && self.mode == VideoMode::Mode13h {
                    self.enter_mode_x();
                } else if !chain4_off && self.mode == VideoMode::ModeX {
                    self.set_mode13h();
                }
            }
            _ => {}
        }
    }

    fn read_seq(&self, index: u8) -> u8 {
        match index {
            0x00 => self.seq.reset,
            0x01 => self.seq.clocking_mode,
            0x02 => self.seq.map_mask,
            0x03 => self.seq.char_map_select,
            0x04 => self.seq.memory_mode,
            _ => 0,
        }
    }

    fn write_gc(&mut self, index: u8, value: u8) {
        match index {
            0x00 => self.gc.set_reset = value & 0x0F,
            0x01 => self.gc.enable_set_reset = value & 0x0F,
            0x02 => self.gc.color_compare = value & 0x0F,
            0x03 => {
                self.gc.rotate = value & 7;
                self.gc.logic = (value >> 3) & 3;
            }
            0x04 => self.gc.read_map = value & 3,
            0x05 => {
                self.gc.write_mode = value & 3;
                self.gc.read_mode = (value >> 3) & 1;
                self.gc.mode_flags = value & 0x70;
            }
            0x06 => self.gc.misc = value & 0x0F,
            0x07 => self.gc.color_dont_care = value & 0x0F,
            0x08 => self.gc.bit_mask = value,
            _ => {}
        }
    }

    /// Read back a Graphics Controller register (3CF data port). Each index
    /// returns the value last written, reassembled where one port packs two
    /// fields (03h rotate+logic, 05h write+read mode). Unmodeled indices read 0.
    fn read_gc(&self, index: u8) -> u8 {
        match index {
            0x00 => self.gc.set_reset,
            0x01 => self.gc.enable_set_reset,
            0x02 => self.gc.color_compare,
            0x03 => self.gc.rotate | (self.gc.logic << 3),
            0x04 => self.gc.read_map,
            0x05 => self.gc.write_mode | (self.gc.read_mode << 3) | self.gc.mode_flags,
            0x06 => self.gc.misc,
            0x07 => self.gc.color_dont_care,
            0x08 => self.gc.bit_mask,
            _ => 0,
        }
    }

    fn write_cga_crtc(&mut self, index: u8, value: u8) {
        match index {
            0x00 => {
                self.crtc_regs.r00 = value;
                self.recompute_cga_horizontal_timing();
            }
            0x01 => {
                self.crtc_regs.r01 = value;
                self.recompute_cga_horizontal_timing();
            }
            0x02 => self.crtc_regs.r02 = value,
            0x03 => self.crtc_regs.r03 = value & 0x0F,
            0x04 => {
                self.crtc_regs.r04 = value & 0x7F;
                self.recompute_cga_vertical_timing();
            }
            0x05 => {
                self.crtc_regs.r05 = value & 0x1F;
                self.recompute_cga_vertical_timing();
            }
            0x06 => {
                self.crtc_regs.r06 = value & 0x7F;
                self.recompute_cga_vertical_timing();
            }
            0x07 => {
                self.crtc_regs.r07 = value & 0x7F;
                self.recompute_cga_vertical_timing();
            }
            0x08 => self.crtc_regs.r08 = value & 0x03,
            0x09 => {
                self.crtc_regs.r09 = value & 0x1F;
                self.recompute_cga_vertical_timing();
            }
            0x0A => self.cursor_start = value & 0x7F,
            0x0B => self.cursor_end = value & 0x1F,
            0x0C => {
                let cur = self.pending_start.unwrap_or(self.crtc.start_address);
                self.set_start_address((cur & 0x00FF) | (u32::from(value & 0x3F) << 8));
            }
            0x0D => {
                let cur = self.pending_start.unwrap_or(self.crtc.start_address);
                self.set_start_address((cur & 0xFF00) | u32::from(value));
            }
            0x0E => {
                self.cursor_offset = (self.cursor_offset & 0x00FF) | (u16::from(value & 0x3F) << 8)
            }
            0x0F => self.cursor_offset = (self.cursor_offset & 0xFF00) | u16::from(value),
            _ => {}
        }
    }

    fn recompute_cga_horizontal_timing(&mut self) {
        let displayed_chars = u32::from(self.crtc_regs.r01).max(1);
        self.crtc.char_width =
            if self.mode == VideoMode::Cga && self.cga.submode == CgaMode::Graphics640x200 {
                16
            } else {
                8
            };
        self.crtc.htotal_chars = (u32::from(self.crtc_regs.r00) + 1).max(displayed_chars);
        self.crtc.hdisp_end = displayed_chars * self.crtc.char_width;
        if self.mode == VideoMode::Text {
            self.crtc.offset = displayed_chars;
            self.text_columns = displayed_chars.min(VGA_TEXT_COLUMNS as u32) as usize;
        }
        self.resize_work();
    }

    fn recompute_cga_vertical_timing(&mut self) {
        let scanlines_per_row = u32::from(self.crtc_regs.r09 & 0x1F) + 1;
        let vtotal =
            (u32::from(self.crtc_regs.r04) + 1) * scanlines_per_row + u32::from(self.crtc_regs.r05);
        let vdisp = u32::from(self.crtc_regs.r06) * scanlines_per_row;
        let vretrace_start = u32::from(self.crtc_regs.r07) * scanlines_per_row;

        self.crtc.max_scan = scanlines_per_row - 1;
        self.crtc.double_scan = false;
        self.crtc.vtotal = vtotal.max(1);
        self.crtc.vdisp_end = vdisp.min(self.crtc.vtotal);
        self.crtc.vblank_start = self.crtc.vdisp_end;
        self.crtc.vblank_end = self.crtc.vtotal.saturating_sub(1);
        self.crtc.vretrace_start = vretrace_start.min(self.crtc.vtotal.saturating_sub(1));
        self.crtc.vretrace_end = (self.crtc.vretrace_start + 2).min(self.crtc.vtotal);
        self.resize_work();
    }

    fn write_crtc(&mut self, index: u8, value: u8) {
        if self.is_cga_personality() {
            self.write_cga_crtc(index, value);
            return;
        }
        let crtc_protected = self.crtc_regs.r11 & 0x80 != 0;
        if crtc_protected && index <= 0x07 {
            if index == 0x07 {
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x100) | (u32::from((value >> 4) & 1) << 8);
            }
            return;
        }
        match index {
            // Horizontal timing group (FreeVGA crtcreg.htm 00h-05h): horizontal
            // total, display end, start/end blanking, start/end retrace. Stored as
            // written for exact read-back; no geometry is derived from them yet, so
            // they do not retune the active mode. The End Horizontal Blanking field
            // splits across 03h bits 4-0 and 05h bit 7, and End Horizontal Retrace
            // across 05h bits 4-0; the whole register byte round-trips, the field
            // masks apply only when a future path decodes these into dot counts.
            0x00 => self.crtc_regs.r00 = value,
            0x01 => self.crtc_regs.r01 = value,
            0x02 => self.crtc_regs.r02 = value,
            0x03 => self.crtc_regs.r03 = value,
            0x04 => self.crtc_regs.r04 = value,
            0x05 => self.crtc_regs.r05 = value,
            // Preset Row Scan (FreeVGA crtcreg.htm 08h): bits 4-0 first font
            // scanline (vertical sub-row), bits 6-5 byte pan.
            0x08 => {
                self.crtc_regs.r08 = value;
                self.crtc.preset_row_scan = value;
            }
            // Cursor shape (start scanline + disable bit / end scanline + skew).
            0x0A => self.cursor_start = value,
            0x0B => self.cursor_end = value,
            // Both start-address bytes buffer through the vretrace latch (no mid-frame
            // tearing). Assemble against the pending value, or the active value if none.
            0x0C => {
                let cur = self.pending_start.unwrap_or(self.crtc.start_address);
                self.set_start_address((cur & 0x00FF) | (u32::from(value) << 8));
            }
            0x0D => {
                let cur = self.pending_start.unwrap_or(self.crtc.start_address);
                self.set_start_address((cur & 0xFF00) | u32::from(value));
            }
            // Text cursor location (high/low byte), shared CRTC index with timing.
            0x0E => self.cursor_offset = (self.cursor_offset & 0x00FF) | (u16::from(value) << 8),
            0x0F => self.cursor_offset = (self.cursor_offset & 0xFF00) | u16::from(value),
            0x13 => {
                self.crtc_regs.r13 = value;
                self.crtc.offset = u32::from(value);
            }
            0x14 => {
                self.crtc_regs.r14 = value;
                self.crtc.underline_loc = value;
            }
            0x17 => {
                self.crtc_regs.r17 = value;
                self.crtc.mode_control = value;
            }
            0x18 => {
                self.crtc_regs.r18 = value;
                self.crtc.line_compare = (self.crtc.line_compare & !0xFF) | u32::from(value);
            }
            0x07 => {
                self.crtc_regs.r07 = value;
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x100) | (u32::from((value >> 4) & 1) << 8);
            }
            0x09 => {
                self.crtc_regs.r09 = value;
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x200) | (u32::from((value >> 6) & 1) << 9);
            }
            0x11 => self.crtc_regs.r11 = value,
            _ => {} // full timing programmed via set_mode_0dh in slice 1
        }
        // Graphics modes honor guest vertical CRTC timing. The absolute fields are
        // derived in recompute_vertical_timing; line-compare bits are handled above.
        if matches!(
            self.mode,
            VideoMode::Planar | VideoMode::Mode13h | VideoMode::ModeX
        ) && matches!(index, 0x06 | 0x07 | 0x09 | 0x10 | 0x11 | 0x12 | 0x15 | 0x16)
        {
            match index {
                0x06 => self.crtc_regs.r06 = value,
                0x07 => self.crtc_regs.r07 = value,
                0x09 => self.crtc_regs.r09 = value,
                0x10 => self.crtc_regs.r10 = value,
                0x11 => self.crtc_regs.r11 = value,
                0x12 => self.crtc_regs.r12 = value,
                0x15 => self.crtc_regs.r15 = value,
                0x16 => self.crtc_regs.r16 = value,
                _ => unreachable!(),
            }
            self.recompute_vertical_timing();
        }
    }

    fn write_attr(&mut self, value: u8) {
        if !self.attr.flip_flop_data {
            self.attr.index = value & 0x1F;
            // Bit 5 is the Palette Address Source: set = normal display, clear =
            // screen blanked while the palette is programmed. It rides on the
            // index write and is dropped from the index itself (masked to 0x1F).
            self.attr.pas = value & 0x20 != 0;
            self.attr.flip_flop_data = true;
        } else {
            match self.attr.index {
                0x00..=0x0F => self.attr.palette[self.attr.index as usize] = value & 0x3F,
                0x10 => self.attr.mode_control = value,
                0x11 => self.attr.overscan = value,
                0x12 => self.attr.plane_enable = value,
                0x13 => self.attr.pixel_pan = value & 0x0F,
                0x14 => self.attr.color_select = value,
                _ => {}
            }
            self.attr.flip_flop_data = false;
        }
    }

    pub fn read_u8(&self, offset: usize) -> Result<u8, VideoError> {
        if self.is_cga_text_mode() {
            return Ok(self.cga_read(offset));
        }
        self.text_memory
            .get(offset)
            .copied()
            .ok_or(VideoError::TextMemoryOutOfBounds { offset })
    }

    pub fn write_u8(&mut self, offset: usize, value: u8) -> Result<(), VideoError> {
        if self.is_cga_text_mode() {
            self.cga_write(offset, value); // bumps content_gen itself
            return Ok(());
        }
        let slot = self
            .text_memory
            .get_mut(offset)
            .ok_or(VideoError::TextMemoryOutOfBounds { offset })?;
        *slot = value;
        self.bump_content_gen();
        Ok(())
    }

    /// Switch to chained mode 13h, installing the standard 320x200 70Hz timing
    /// and routing the scanout through the shared raster engine (the same path
    /// as the planar and mode-X modes). Chain-4 is the mode-13h-specific CPU
    /// write decode; the CRTC display scanout is shared with mode X.
    pub fn set_mode13h(&mut self) {
        self.set_mode13h_with_clear(false);
    }

    /// Switch to mode 13h and optionally clear VGA graphics memory, matching
    /// INT 10h AH=00h's bit-7 clear/preserve flag.
    pub fn set_mode13h_with_clear(&mut self, clear: bool) {
        // A mode change alters the scanout even at identical raster dims (0Dh<->13h are
        // both 320x449); the dimension fold can't see it, so bump the content gen.
        self.bump_content_gen();
        self.crtc = CrtcTiming::mode13h();
        self.crtc_regs = CrtcRegs::from_timing(self.crtc);
        self.seed_vgabios_crtc_readback(0x13);
        self.seed_vgabios_seq_readback(0x13);
        self.set_misc_mode_bits(0, true, 0x01);
        self.gc = GfxController::default();
        self.seed_vgabios_gc_readback(0x13);
        self.latches = [0; VGA_PLANES];
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Mode13h;
        if clear {
            self.vram.fill(0);
        }
        self.presented = None; // drop any stale frame from a prior mode
        self.reset_palette_defaults(0x13);
        self.seed_vgabios_attr_readback(0x13);
        self.resize_work();
    }

    /// Switch to a CGA graphics mode by its INT 10h number (04h, 05h, or 06h),
    /// clearing the framebuffer like a normal BIOS mode set.
    pub fn set_cga_mode(&mut self, mode: u8) -> bool {
        self.set_cga_mode_with_clear(mode, true)
    }

    /// Switch to a CGA graphics mode, optionally preserving the B800 framebuffer
    /// for INT 10h AH=00h mode numbers with bit 7 set.
    pub fn set_cga_mode_with_clear(&mut self, mode: u8, clear: bool) -> bool {
        let (timing, submode) = match mode {
            0x04 | 0x05 => (CrtcTiming::cga_320x200(), CgaMode::Graphics320x200),
            0x06 => (CrtcTiming::cga_640x200(), CgaMode::Graphics640x200),
            _ => return false,
        };
        // A mode change alters the scanout even at identical raster dims; bump the
        // content gen so the host frame cache re-renders the switch (after validation,
        // so an unsupported mode that returns false above does not bump).
        self.bump_content_gen();
        self.crtc = timing;
        self.set_misc_mode_bits(0, true, 0x01);
        self.seq.reset = 0x03;
        self.crtc_regs = match mode {
            0x06 => CrtcRegs::cga_graphics_640x200(),
            _ => CrtcRegs::cga_graphics_320x200(),
        };
        self.recompute_cga_vertical_timing();
        self.cga.submode = submode;
        self.cga.bios_mode = mode;
        // The BIOS mode-set programs the color-select default. Mode 06h is white
        // on black; 320x200 modes start with background black, palette 0, low intensity.
        self.cga.color_select = if mode == 0x06 { CGA_WHITE } else { 0x00 };
        self.cga.mode_control = match mode {
            0x05 => 0x0E,
            0x06 => 0x1A,
            _ => 0x0A,
        };
        self.seq.char_map_select = 0;
        self.load_rom_font(0, 8);
        if clear {
            for byte in self.cga.fb.iter_mut() {
                *byte = 0;
            }
        }
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Cga;
        self.presented = None;
        self.pending_start = None;
        self.reset_palette_defaults(mode);
        self.resize_work();
        true
    }

    fn write_cga_mode_control(&mut self, value: u8) {
        let value = value & 0x3F;
        let old_control = self.cga.mode_control;
        let was_cga = self.is_cga_personality();
        self.cga.mode_control = value;
        let decode_changed = !was_cga
            || ((old_control ^ value)
                & (CGA_MODE_80_COLUMNS | CGA_MODE_GRAPHICS | CGA_MODE_HIGH_RES)
                != 0);

        if value & CGA_MODE_GRAPHICS != 0 {
            if value & CGA_MODE_HIGH_RES != 0 {
                if !was_cga {
                    self.crtc = CrtcTiming::cga_640x200();
                    self.crtc_regs = CrtcRegs::cga_graphics_640x200();
                    self.recompute_cga_vertical_timing();
                }
                if decode_changed {
                    self.crtc.char_width = 16;
                    self.crtc.htotal_chars = 57;
                    self.crtc.hdisp_end = 640;
                }
                self.cga.submode = CgaMode::Graphics640x200;
                self.cga.bios_mode = 0x06;
            } else {
                if !was_cga {
                    self.crtc = CrtcTiming::cga_320x200();
                    self.crtc_regs = CrtcRegs::cga_graphics_320x200();
                    self.recompute_cga_vertical_timing();
                }
                if decode_changed {
                    self.crtc.char_width = 8;
                    self.crtc.htotal_chars = 57;
                    self.crtc.hdisp_end = 320;
                }
                self.cga.submode = CgaMode::Graphics320x200;
                self.cga.bios_mode = if value & CGA_MODE_BW != 0 { 0x05 } else { 0x04 };
            }
            self.mode = VideoMode::Cga;
            self.resize_work();
        } else {
            if value & CGA_MODE_80_COLUMNS != 0 {
                if !was_cga {
                    self.crtc = CrtcTiming::text_80x25_cga();
                    self.crtc_regs = CrtcRegs::cga_text_80x25();
                    self.recompute_cga_vertical_timing();
                }
                if decode_changed {
                    self.crtc.char_width = 8;
                    self.crtc.htotal_chars = 114;
                    self.crtc.hdisp_end = 640;
                    self.crtc.offset = 80;
                    self.text_columns = VGA_TEXT_COLUMNS;
                }
            } else {
                if !was_cga {
                    self.crtc = CrtcTiming::text_40x25();
                    self.crtc_regs = CrtcRegs::cga_text_40x25();
                    self.recompute_cga_vertical_timing();
                }
                if decode_changed {
                    self.crtc.char_width = 8;
                    self.crtc.htotal_chars = 57;
                    self.crtc.hdisp_end = 320;
                    self.crtc.offset = 40;
                    self.text_columns = 40;
                }
            }
            self.seq.clocking_mode |= 0x01;
            self.mode = VideoMode::Text;
            self.resize_work();
        }
    }

    fn is_cga_text_mode(&self) -> bool {
        self.mode == VideoMode::Text && self.crtc.char_width == 8
    }

    pub fn is_cga_personality(&self) -> bool {
        self.mode == VideoMode::Cga || self.is_cga_text_mode()
    }

    fn cga_light_pen_cell_position(&self) -> (u16, u16) {
        let line = beam_line(&self.crtc, self.beam).min(self.crtc.vdisp_end.saturating_sub(1));
        let dot = beam_dot(&self.crtc, self.beam).min(self.crtc.hdisp_end.saturating_sub(1));
        let (columns, row_divisor) = if self.mode == VideoMode::Cga {
            (40u32, 2u32)
        } else {
            (self.text_columns as u32, self.crtc.max_scan + 1)
        };
        let col = dot.saturating_mul(columns) / self.crtc.hdisp_end.max(1);
        let row = line / row_divisor.max(1);
        (col as u16, row as u16)
    }

    fn latch_cga_light_pen(&mut self) {
        if !self.is_cga_personality() || self.cga.light_pen_triggered {
            return;
        }
        let (col, row) = self.cga_light_pen_cell_position();
        let pitch = if self.mode == VideoMode::Cga {
            40
        } else {
            self.text_columns as u16
        };
        let start = self.crtc.start_address as u16;
        self.cga.light_pen_latch =
            start.wrapping_add(row.wrapping_mul(pitch).wrapping_add(col)) & 0x3FFF;
        let row_height = if self.mode == VideoMode::Cga {
            2
        } else {
            self.crtc.max_scan + 1
        };
        self.cga.light_pen_pixel_row = row.saturating_mul(row_height as u16).min(199);
        self.cga.light_pen_pixel_col = match self.mode {
            VideoMode::Cga if self.cga.submode == CgaMode::Graphics640x200 => {
                let dot =
                    beam_dot(&self.crtc, self.beam).min(self.crtc.hdisp_end.saturating_sub(1));
                (dot as u16 & !3).min(639)
            }
            VideoMode::Cga => {
                let dot =
                    beam_dot(&self.crtc, self.beam).min(self.crtc.hdisp_end.saturating_sub(1));
                (dot as u16 & !1).min(319)
            }
            _ => (col * 8).min(639),
        };
        self.cga.light_pen_triggered = true;
    }

    fn clear_cga_light_pen(&mut self) {
        self.cga.light_pen_triggered = false;
    }

    pub fn cga_light_pen_report(&self) -> Option<(u16, u8, u8, u8)> {
        if !self.is_cga_personality() || !self.cga.light_pen_triggered {
            return None;
        }
        let pixel_row = self.cga.light_pen_pixel_row.min(199);
        let pixel_col = self.cga.light_pen_pixel_col.min(639);
        let (char_row, char_col) = match self.mode {
            VideoMode::Cga if self.cga.submode == CgaMode::Graphics640x200 => {
                ((pixel_row / 8).min(24), (pixel_col / 16).min(39))
            }
            VideoMode::Cga => ((pixel_row / 8).min(24), (pixel_col / 8).min(39)),
            _ => (
                (pixel_row / (self.crtc.max_scan + 1) as u16).min(24),
                (pixel_col / 8).min(self.text_columns.saturating_sub(1) as u16),
            ),
        };
        Some((pixel_col, pixel_row as u8, char_row as u8, char_col as u8))
    }

    /// Reset to a text mode. `ega_attr_dac` selects the default DAC: VGA
    /// 16-color text (mode 03h) drives colors through the EGA attribute remap
    /// (6 -> 0x14, the bright eight -> 0x38..0x3F), so it needs palette2 in the
    /// first 64 DAC entries. CGA text (modes 00h-02h, direct RGBI color numbers)
    /// and MDA-style mono text (identity attribute palette) instead need the
    /// standard 16 colors at entries 0..15, which the 256-color palette3 holds.
    fn reset_text_mode(&mut self, clear: bool, ega_attr_dac: bool) {
        self.cursor_offset = 0;
        if clear {
            if self.crtc.char_width == 8 {
                for cell in self.cga.fb.chunks_exact_mut(2) {
                    cell[0] = b' ';
                    cell[1] = 0x07;
                }
            } else {
                for cell in self.text_memory.chunks_exact_mut(2) {
                    cell[0] = b' ';
                    cell[1] = 0x07;
                }
            }
        }
        self.beam = 0;
        self.last_line = 0;
        self.seq.reset = 0x03;
        self.mode = VideoMode::Text;
        self.presented = None;
        // A buffered start-address change from a prior graphics mode must not
        // carry across the mode switch: the text origin resets to page 0.
        self.pending_start = None;
        // Mode 03h installs the EGA attribute remap and so wants palette2; CGA
        // and mono text keep the 256-color palette3 (standard 16 at 0..15).
        self.reset_palette_defaults(if ega_attr_dac { 0x03 } else { 0x13 });
        self.resize_work();
    }

    pub fn set_cga_text_mode(&mut self, mode: u8) -> bool {
        self.set_cga_text_mode_with_clear(mode, true)
    }

    pub fn set_cga_text_mode_with_clear(&mut self, mode: u8, clear: bool) -> bool {
        let (timing, regs, columns, mode_control) = match mode {
            0x00 => (
                CrtcTiming::text_40x25(),
                CrtcRegs::cga_text_40x25(),
                40,
                CGA_MODE_BW | CGA_MODE_VIDEO_ENABLE | CGA_MODE_BLINK,
            ),
            0x01 => (
                CrtcTiming::text_40x25(),
                CrtcRegs::cga_text_40x25(),
                40,
                CGA_MODE_VIDEO_ENABLE | CGA_MODE_BLINK,
            ),
            0x02 => (
                CrtcTiming::text_80x25_cga(),
                CrtcRegs::cga_text_80x25(),
                VGA_TEXT_COLUMNS,
                CGA_MODE_80_COLUMNS | CGA_MODE_BW | CGA_MODE_VIDEO_ENABLE | CGA_MODE_BLINK,
            ),
            0x03 => (
                CrtcTiming::text_80x25_cga(),
                CrtcRegs::cga_text_80x25(),
                VGA_TEXT_COLUMNS,
                CGA_MODE_80_COLUMNS | CGA_MODE_VIDEO_ENABLE | CGA_MODE_BLINK,
            ),
            _ => return false,
        };

        self.crtc = timing;
        self.set_misc_mode_bits(0, true, 0x01);
        self.crtc_regs = regs;
        self.recompute_cga_vertical_timing();
        self.text_columns = columns;
        self.cga.mode_control = mode_control;
        self.seq.clocking_mode |= 0x01;
        self.seq.char_map_select = 0;
        self.load_rom_font(0, 8);
        self.cursor_start = 0x06;
        self.cursor_end = 0x07;
        self.reset_text_mode(clear, false); // CGA text: direct RGBI, keep palette3
        true
    }

    pub fn set_cga_80_text_mode(&mut self) {
        let _ = self.set_cga_text_mode(0x02);
    }

    fn set_vga_80_text_mode(&mut self) {
        self.crtc = CrtcTiming::text_03h();
        self.crtc_regs = CrtcRegs::from_timing(self.crtc);
        self.seed_vgabios_crtc_readback(0x03);
        self.seed_vgabios_seq_readback(0x03);
        self.seed_vgabios_gc_readback(0x03);
        self.set_misc_mode_bits(1, true, 0x01);
        self.text_columns = VGA_TEXT_COLUMNS;
        self.load_rom_font(0, 16);
        self.cursor_start = 0x0E;
        self.cursor_end = 0x0F;
        self.reset_text_mode(true, true); // VGA mode 03h: EGA attribute remap, palette2
        self.seed_vgabios_attr_readback(0x03);
    }

    pub fn set_color_text_mode_scanlines(&mut self, mode: u8, scanlines: u16, clear: bool) -> bool {
        let columns = match mode & 0x7F {
            0x00 | 0x01 => 40,
            0x02 | 0x03 => VGA_TEXT_COLUMNS,
            _ => return false,
        };
        if scanlines == 200 {
            return self.set_cga_text_mode_with_clear(mode, clear);
        }

        let mut timing = match scanlines {
            350 => CrtcTiming::text_07h(),
            400 => CrtcTiming::text_03h(),
            _ => return false,
        };
        if columns <= 40 {
            timing.hdisp_end /= 2;
            timing.offset = 40;
        }
        self.crtc = timing;
        self.crtc_regs = CrtcRegs::from_timing(timing);
        // Only mode 03h at 400 lines installs the EGA attribute remap; the DAC
        // palette has to match it (palette2), while the other text variants here
        // keep their identity attribute palette and so keep palette3.
        let install_ega_attr = mode & 0x7F == 0x03 && scanlines == 400;
        if install_ega_attr {
            self.seed_vgabios_crtc_readback(0x03);
            self.seed_vgabios_seq_readback(0x03);
            self.seed_vgabios_gc_readback(0x03);
        }
        self.set_misc_mode_bits(1, true, if scanlines == 350 { 0x02 } else { 0x01 });
        self.text_columns = columns;
        self.seq.clocking_mode &= !0x01;
        self.seq.char_map_select = 0;
        let height = if scanlines == 350 { 14 } else { 16 };
        self.load_rom_font(0, height);
        self.cursor_start = height - 2;
        self.cursor_end = height - 1;
        self.reset_text_mode(clear, install_ega_attr);
        if install_ega_attr {
            self.seed_vgabios_attr_readback(0x03);
        }
        true
    }

    /// Switch to the 80x25 VGA text mode (mode 03h).
    pub fn set_text_mode(&mut self) {
        self.set_text_mode_columns(VGA_TEXT_COLUMNS);
    }

    pub fn set_mono_text_mode(&mut self) {
        self.crtc = CrtcTiming::text_07h();
        self.crtc_regs = CrtcRegs::from_timing(self.crtc);
        self.set_misc_mode_bits(1, false, 0x02);
        self.text_columns = VGA_TEXT_COLUMNS;
        self.seq.clocking_mode &= !0x01;
        self.seq.char_map_select = 0;
        self.load_rom_font(0, 14);
        self.cursor_start = 0x0C;
        self.cursor_end = 0x0D;
        self.reset_text_mode(true, false); // mono text: identity attribute, keep palette3
    }

    /// Switch to a text mode with 40 or 80 visible columns, resetting the beam,
    /// clearing the text buffer, and dropping any stale graphics frame.
    pub fn set_text_mode_columns(&mut self, columns: usize) {
        if columns <= 40 {
            let _ = self.set_cga_text_mode(0x01);
        } else {
            self.set_vga_80_text_mode();
        }
    }

    /// Write one byte into the CGA framebuffer at a B800 aperture offset. The
    /// offset is the raw byte offset from B800:0000 (0..16383); the interleave
    /// lives in the layout the guest writes, so the store is a flat copy and the
    /// scanout (`render_cga_row`) reinterprets the banks.
    pub fn cga_write(&mut self, offset: usize, value: u8) {
        self.bump_content_gen();
        if let Some(slot) = self.cga.fb.get_mut(offset & (CGA_FB_SIZE - 1)) {
            *slot = value;
        }
    }

    /// Read one byte from the CGA framebuffer at a B800 aperture offset.
    pub fn cga_read(&self, offset: usize) -> u8 {
        self.cga
            .fb
            .get(offset & (CGA_FB_SIZE - 1))
            .copied()
            .unwrap_or(0)
    }

    fn cga_pixel_offset(&self, x: u16, y: u16) -> Option<(usize, u8, u8)> {
        if y >= 200 || u32::from(x) >= self.crtc.hdisp_end {
            return None;
        }
        let row = usize::from(y);
        let bank = (row & 1) * CGA_ODD_BANK;
        let row_base = bank + (row >> 1) * self.cga_bytes_per_scanline();
        match self.cga.submode {
            CgaMode::Graphics320x200 => {
                let pixel = usize::from(x);
                let shift = 6 - ((pixel & 3) * 2);
                Some((row_base + pixel / 4, shift as u8, 0x03))
            }
            CgaMode::Graphics640x200 => {
                let pixel = usize::from(x);
                Some((row_base + pixel / 8, (7 - (pixel & 7)) as u8, 0x01))
            }
        }
    }

    pub fn cga_write_pixel(&mut self, x: u16, y: u16, color: u8, xor: bool) -> bool {
        let Some((offset, shift, mask_bits)) = self.cga_pixel_offset(x, y) else {
            return false;
        };
        let old = self.cga_read(offset);
        let mask = mask_bits << shift;
        let old_bits = (old >> shift) & mask_bits;
        let color_bits = color & mask_bits;
        let new_bits = if xor {
            old_bits ^ color_bits
        } else {
            color_bits
        };
        self.cga_write(offset, (old & !mask) | (new_bits << shift));
        true
    }

    pub fn cga_read_pixel(&self, x: u16, y: u16) -> u8 {
        let Some((offset, shift, mask_bits)) = self.cga_pixel_offset(x, y) else {
            return 0;
        };
        (self.cga_read(offset) >> shift) & mask_bits
    }

    fn cga_bytes_per_scanline(&self) -> usize {
        match self.cga.submode {
            CgaMode::Graphics320x200 => (self.crtc.hdisp_end as usize / 4).max(1),
            CgaMode::Graphics640x200 => (self.crtc.hdisp_end as usize / 8).max(1),
        }
    }

    /// Assemble one CGA graphics scanline into `hdisp_end` DAC indices. The
    /// classic CGA interleave maps display scanline `y` to framebuffer bank
    /// `(y & 1) * 0x2000` plus `(y >> 1) * live_pitch`; even lines sit in the
    /// low bank, odd lines in the high bank. 320x200x4 unpacks 4 pixels per byte
    /// (2 bits each, MSB first) through the four-color palette; 640x200x2 unpacks
    /// 8 pixels per byte (1 bit each) through the background/foreground pair.
    pub fn render_cga_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        if self.cga.mode_control & 0x08 == 0 {
            return vec![CGA_BLACK; width];
        }
        let y = counter_line as usize;
        let bank = (y & 1) * CGA_ODD_BANK;
        let pitch = self.cga_bytes_per_scanline();
        let row_base =
            (self.crtc.start_address as usize + bank + (y >> 1) * pitch) & (CGA_FB_SIZE - 1);
        let mut row = vec![0u8; width];
        match self.cga.submode {
            CgaMode::Graphics320x200 => {
                for byte_col in 0..pitch {
                    let byte = self.cga_read(row_base + byte_col);
                    let pixels = self.cga.decode_byte_320x200(byte);
                    for (sub, &index) in pixels.iter().enumerate() {
                        let x = byte_col * 4 + sub;
                        if x < width {
                            row[x] = index;
                        }
                    }
                }
            }
            CgaMode::Graphics640x200 => {
                let bg = CGA_BLACK;
                let fg = self.cga.foreground_640x200();
                for byte_col in 0..pitch {
                    let byte = self.cga_read(row_base + byte_col);
                    for bit in 0..8 {
                        let x = byte_col * 8 + bit;
                        if x < width {
                            row[x] = if (byte >> (7 - bit)) & 1 != 0 { fg } else { bg };
                        }
                    }
                }
            }
        }
        row
    }

    /// Derive the absolute vertical timing in `crtc` from the raw register bytes in
    /// `crtc_regs`, applying the overflow-bit assembly and the VGA register
    /// conventions (vertical total + 2, vertical display end + 1, the retrace/blank
    /// ends as line-counter compares). Used only while unchained (mode X).
    fn recompute_vertical_timing(&mut self) {
        let r = self.crtc_regs;
        let vtotal =
            ((r.r06 as u32) | (((r.r07 & 1) as u32) << 8) | ((((r.r07 >> 5) & 1) as u32) << 9)) + 2;
        let vdisp = ((r.r12 as u32)
            | ((((r.r07 >> 1) & 1) as u32) << 8)
            | ((((r.r07 >> 6) & 1) as u32) << 9))
            + 1;
        let vretrace_start = (r.r10 as u32)
            | ((((r.r07 >> 2) & 1) as u32) << 8)
            | ((((r.r07 >> 7) & 1) as u32) << 9);
        let vblank_start = (r.r15 as u32)
            | ((((r.r07 >> 3) & 1) as u32) << 8)
            | ((((r.r09 >> 5) & 1) as u32) << 9);
        let vretrace_end = {
            let target = (r.r11 & 0x0F) as u32;
            let mut e = (vretrace_start & !0x0F) | target;
            if e <= vretrace_start {
                e += 0x10;
            }
            e
        };
        let vblank_end = {
            let target = r.r16 as u32;
            let mut e = (vblank_start & !0xFF) | target;
            if e <= vblank_start {
                e += 0x100;
            }
            e
        };
        let max_scan = (r.r09 & 0x1F) as u32;
        self.crtc.vtotal = vtotal;
        self.crtc.vdisp_end = vdisp;
        self.crtc.vretrace_start = vretrace_start;
        self.crtc.vretrace_end = vretrace_end;
        self.crtc.vblank_start = vblank_start;
        self.crtc.vblank_end = vblank_end;
        self.crtc.max_scan = max_scan;
        self.crtc.double_scan = (r.r09 & 0x80 != 0) || max_scan == 1;
        self.resize_work();
    }

    /// Enter unchained 256-color (mode X / mode Y) with the 320x200 base. The guest
    /// retunes the geometry by reprogramming the vertical CRTC timing while here.
    fn enter_mode_x(&mut self) {
        // seq.memory_mode already holds the chain-4-off value from the write_seq
        // call that triggered this entry, so it is not reseeded here.
        self.crtc = CrtcTiming::mode_x();
        self.crtc_regs = CrtcRegs::mode_x_320x200();
        self.recompute_vertical_timing(); // derives the vertical fields and sizes work
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::ModeX;
        self.presented = None;
    }

    pub fn active_mode(&self) -> VideoMode {
        self.mode
    }

    /// True only in a text mode. Text adds time-based cursor/attribute blink with
    /// no guest write, so the host dirty-framebuffer cache must keep re-rendering
    /// text screens (the content generation cannot capture blink in v1).
    pub fn is_text_mode(&self) -> bool {
        self.mode == VideoMode::Text
    }

    /// The content-generation counter, bumped by every display mutator (see
    /// `bump_content_gen`). The machine folds this into `Machine::frame_generation`
    /// so any output change — from the CPU bus or an HLE BIOS service — invalidates
    /// the host frame cache.
    pub fn content_gen(&self) -> u64 {
        self.content_gen
    }

    /// Bump the content generation. Called by every method that can change what the
    /// display scans out (VRAM writers, register/DAC port writes, the start-address
    /// latch), so the host dirty-framebuffer cache re-renders regardless of which
    /// caller (CPU bus or HLE BIOS) drove the write. Over-bumping is harmless.
    #[inline]
    fn bump_content_gen(&mut self) {
        self.content_gen = self.content_gen.wrapping_add(1);
    }

    /// The CPU aperture window the Graphics Controller Miscellaneous register
    /// (06h) selects, plus the graphics and chain-odd-even flags. The machine bus
    /// consults this to route the legacy A0000/B0000 mapping in graphics modes.
    pub fn gfx_aperture(&self) -> GfxAperture {
        self.gc.aperture()
    }

    pub fn cga_mode_control(&self) -> u8 {
        self.cga.mode_control
    }

    pub fn cga_color_select(&self) -> u8 {
        self.cga.color_select
    }

    /// Set the border/overscan color. VGA stores Attribute register 11h raw;
    /// CGA mirrors the low five bits into 3D9h's background/intensity field.
    pub fn set_overscan(&mut self, value: u8) {
        self.bump_content_gen();
        self.attr.overscan = value;
        if self.is_cga_personality() {
            self.cga.color_select = (self.cga.color_select & !0x1F) | (value & 0x1F);
        }
    }

    pub fn set_text_blink_enabled(&mut self, enabled: bool) {
        if self.is_cga_text_mode() {
            if enabled {
                self.cga.mode_control |= CGA_MODE_BLINK;
            } else {
                self.cga.mode_control &= !CGA_MODE_BLINK;
            }
        } else if enabled {
            self.attr.mode_control |= 0x08;
        } else {
            self.attr.mode_control &= !0x08;
        }
    }

    pub fn overscan(&self) -> u8 {
        if self.is_cga_personality() {
            self.cga.color_select & 0x1F
        } else {
            self.attr.overscan
        }
    }

    /// Set one Attribute palette register (0-15). The index is masked to 4 bits,
    /// the value to 6 bits, matching the 3C0 datapath. Used by INT 10h AH=10h.
    pub fn set_attr_palette_reg(&mut self, index: u8, value: u8) {
        self.bump_content_gen();
        self.attr.palette[(index & 0x0F) as usize] = value & 0x3F;
    }

    pub fn attr_palette_reg(&self, index: u8) -> u8 {
        self.attr.palette[(index & 0x0F) as usize]
    }

    pub fn set_attr_register(&mut self, index: u8, value: u8) {
        // Attribute palette / mode-control / overscan / panning / color-select all
        // change graphics output; the HLE INT 10h palette services write these
        // directly. The 0x00..=0x0F path double-bumps via set_attr_palette_reg —
        // harmless.
        self.bump_content_gen();
        match index & 0x1F {
            0x00..=0x0F => self.set_attr_palette_reg(index, value),
            0x10 => self.attr.mode_control = value,
            0x11 => self.set_overscan(value),
            0x12 => self.attr.plane_enable = value,
            0x13 => self.attr.pixel_pan = value & 0x0F,
            0x14 => self.attr.color_select = value,
            _ => {}
        }
    }

    pub fn attr_register(&self, index: u8) -> u8 {
        match index & 0x1F {
            0x00..=0x0F => self.attr_palette_reg(index),
            0x10 => self.attr.mode_control,
            0x11 => self.overscan(),
            0x12 => self.attr.plane_enable,
            0x13 => self.attr.pixel_pan,
            0x14 => self.attr.color_select,
            _ => 0,
        }
    }

    pub fn set_dac_entry(&mut self, index: u8, r: u8, g: u8, b: u8) {
        // A palette change is a graphics-mode output change with no VRAM write — the
        // HLE INT 10h AH=10h palette services call this directly, bypassing the bus.
        self.bump_content_gen();
        let [r, g, b] = self.dac_entry_for_write(r, g, b);
        self.dac.set_entry(index, r, g, b);
    }

    pub fn dac_entry(&self, index: u8) -> [u8; 3] {
        self.dac.entry(index)
    }

    pub fn set_dac_block(&mut self, start: u8, entries: &[[u8; 3]]) {
        for (offset, &[r, g, b]) in entries.iter().enumerate() {
            self.set_dac_entry(start.wrapping_add(offset as u8), r, g, b);
        }
    }

    pub fn dac_block_bytes(&self, start: u8, count: u16) -> Vec<u8> {
        self.dac.block_bytes(start, count)
    }

    pub fn default_palette_loading_enabled(&self) -> bool {
        self.default_palette_loading_enabled
    }

    pub fn set_default_palette_loading_enabled(&mut self, enabled: bool) {
        self.default_palette_loading_enabled = enabled;
    }

    pub fn grayscale_summing_enabled(&self) -> bool {
        self.grayscale_summing_enabled
    }

    pub fn set_grayscale_summing_enabled(&mut self, enabled: bool) {
        self.grayscale_summing_enabled = enabled;
    }

    pub fn sum_dac_entry_to_gray(&mut self, index: u8) {
        if self.grayscale_summing_enabled {
            self.bump_content_gen();
            let [r, g, b] = self.dac.entry(index);
            let gray = Self::gray6(r, g, b);
            self.dac.set_entry(index, gray, gray, gray);
        }
    }

    fn dac_entry_for_write(&self, r: u8, g: u8, b: u8) -> [u8; 3] {
        if self.grayscale_summing_enabled {
            let gray = Self::gray6(r, g, b);
            [gray, gray, gray]
        } else {
            [r & 0x3F, g & 0x3F, b & 0x3F]
        }
    }

    fn gray6(r: u8, g: u8, b: u8) -> u8 {
        ((u16::from(r & 0x3F) * 77 + u16::from(g & 0x3F) * 151 + u16::from(b & 0x3F) * 28) >> 8)
            as u8
    }

    pub fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        let mut out = [0u32; DAC_ENTRIES];
        for (index, slot) in out.iter_mut().enumerate() {
            let (r, g, b) = self.dac.rgb888(index as u8);
            *slot = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
        out
    }

    pub fn frame(&self) -> TextFrame {
        // The visible text page, read from the start-address origin so the
        // headless cell view matches the pixel scanout (render_text_row). Mode
        // 03h is word mode, so start_address is a word/cell address: the cell
        // index for (row, col) is start + row*offset + col, and the char/attr
        // byte pair sits at that cell index * 2, wrapped at the live text aperture.
        let start_cells = self.crtc.start_address as usize;
        let columns = self.text_columns;
        let mut cells = Vec::with_capacity(columns * VGA_TEXT_ROWS);
        for row in 0..VGA_TEXT_ROWS {
            for col in 0..columns {
                let base = self.text_cell_base(start_cells, row, col);
                cells.push(TextCell {
                    character: self.text_byte(base),
                    attribute: self.text_byte(base + 1),
                });
            }
        }

        TextFrame {
            columns,
            rows: VGA_TEXT_ROWS,
            cells,
            cursor_offset: self.cursor_offset,
        }
    }
}

/// Decode a three-bit Sequencer Character Map Select field out of `spec` at bit
/// positions `b0`, `b1`, `b2` to a font table index 0..7. Map A gathers bits
/// 0/1/4 and map B gathers bits 2/3/5; the two must stay exact shape-mirrors, so
/// the gather lives in one place. Shared by the active-table read and the
/// block-specifier load so a loaded font and its display selector always agree.
fn char_map_decode(spec: u8, b0: u32, b1: u32, b2: u32) -> usize {
    ((spec >> b0) & 0x01) as usize
        | (((spec >> b1) & 0x01) as usize) << 1
        | (((spec >> b2) & 0x01) as usize) << 2
}

/// Map-A font table (Sequencer Character Map Select bits 0, 1, 4).
fn char_map_a_decode(spec: u8) -> usize {
    char_map_decode(spec, 0, 1, 4)
}

/// Map-B font table (Sequencer Character Map Select bits 2, 3, 5), the mirror of
/// `char_map_a_decode` for the second character set. Per cell, attribute bit 3
/// selects map B (set) or map A (clear) in 512-glyph mode. See A4 in
/// dev_docs/reference/vga/text-mode-gaps-confirm-notes.md.
fn char_map_b_decode(spec: u8) -> usize {
    char_map_decode(spec, 2, 3, 5)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GfxController {
    pub set_reset: u8,        // idx 0, low 4 bits
    pub enable_set_reset: u8, // idx 1, low 4 bits
    pub color_compare: u8,    // idx 2
    pub rotate: u8,           // idx 3 bits 0..2
    pub logic: u8,            // idx 3 bits 3..4: 0 copy,1 AND,2 OR,3 XOR
    pub read_map: u8,         // idx 4
    pub write_mode: u8,       // idx 5 bits 0..1
    pub read_mode: u8,        // idx 5 bit 3
    pub mode_flags: u8,       // idx 5 bits 4..6: odd/even + shift modes
    pub color_dont_care: u8,  // idx 7
    pub bit_mask: u8,         // idx 8
    // idx 6 Miscellaneous Graphics: bit 0 graphics (vs alphanumeric), bit 1 chain
    // odd/even, bits 3-2 memory map select. Stored as written; the fields are
    // decoded by `aperture` (FreeVGA gfxreg.htm 06h).
    pub misc: u8,
}

impl GfxController {
    fn mode_odd_even(&self) -> bool {
        self.mode_flags & 0x10 != 0
    }
}

/// The decoded Graphics Controller Miscellaneous register (index 06h): the CPU
/// aperture window the legacy A0000/B0000 mapping points at, plus the two mode
/// flags the bus and the read/write decode consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GfxAperture {
    /// Aperture base linear address (A0000, A0000, B0000, or B8000).
    pub base: u32,
    /// Aperture length in bytes (0x20000, 0x10000, 0x8000, or 0x8000).
    pub length: u32,
    /// Misc bit 0: graphics mode (clear = alphanumeric/text).
    pub graphics: bool,
    /// Misc bit 1: chain odd/even enable.
    pub chain_odd_even: bool,
}

impl GfxController {
    /// Decode the Miscellaneous register (06h) into the selected aperture window
    /// and the graphics / chain-odd-even flags. Memory Map Select (bits 3-2):
    /// 00 = A0000-BFFFF (128K), 01 = A0000-AFFFF (64K), 10 = B0000-B7FFF (32K),
    /// 11 = B8000-BFFFF (32K). FreeVGA gfxreg.htm 06h.
    pub fn aperture(&self) -> GfxAperture {
        let (base, length) = match (self.misc >> 2) & 0x03 {
            0b00 => (0xA_0000, 0x2_0000),
            0b01 => (0xA_0000, 0x1_0000),
            0b10 => (0xB_0000, 0x0_8000),
            _ => (0xB_8000, 0x0_8000),
        };
        GfxAperture {
            base,
            length,
            graphics: self.misc & 0x01 != 0,
            chain_odd_even: self.misc & 0x02 != 0,
        }
    }
}

fn apply_logic(logic: u8, value: u8, latch: u8) -> u8 {
    match logic {
        1 => value & latch,
        2 => value | latch,
        3 => value ^ latch,
        _ => value,
    }
}

/// Read one byte through the VGA read datapath, loading the four latches.
/// Spec section 4.
pub fn read_planes(
    planes: &[[u8; 1]; VGA_PLANES],
    gc: &GfxController,
    latches: &mut [u8; VGA_PLANES],
) -> u8 {
    for plane in 0..VGA_PLANES {
        latches[plane] = planes[plane][0];
    }
    if gc.read_mode == 0 {
        return planes[(gc.read_map & 3) as usize][0];
    }
    // Read mode 1: per bit, set the result bit where every cared-about plane
    // matches the corresponding color_compare bit.
    let mut result = 0u8;
    for bit in 0..8 {
        let mut matches = true;
        for (plane, slot) in planes.iter().enumerate() {
            if (gc.color_dont_care >> plane) & 1 == 0 {
                continue;
            }
            let plane_bit = (slot[0] >> bit) & 1;
            let cmp_bit = (gc.color_compare >> plane) & 1;
            if plane_bit != cmp_bit {
                matches = false;
                break;
            }
        }
        if matches {
            result |= 1 << bit;
        }
    }
    result
}

/// Write one byte through the VGA write datapath into all four planes. `planes[i]`
/// is plane i's slice; `latches` are the four latch registers. Spec section 4.
pub fn write_planes(
    planes: &mut [[u8; 1]; VGA_PLANES],
    data: u8,
    gc: &GfxController,
    latches: &[u8; VGA_PLANES],
) {
    let rotated = data.rotate_right(u32::from(gc.rotate & 7));
    for plane in 0..VGA_PLANES {
        let latch = latches[plane];
        let value = match gc.write_mode {
            1 => {
                planes[plane][0] = latch; // WM1: latches straight to planes
                continue;
            }
            2 => {
                if (data >> plane) & 1 != 0 { 0xFF } else { 0x00 } // WM2
            }
            3 => {
                if (gc.set_reset >> plane) & 1 != 0 {
                    0xFF
                } else {
                    0x00
                } // WM3 color
            }
            _ => {
                // WM0: set/reset substitution where enabled, else rotated data.
                if (gc.enable_set_reset >> plane) & 1 != 0 {
                    if (gc.set_reset >> plane) & 1 != 0 {
                        0xFF
                    } else {
                        0x00
                    }
                } else {
                    rotated
                }
            }
        };
        let mask = if gc.write_mode == 3 {
            gc.bit_mask & rotated
        } else {
            gc.bit_mask
        };
        let alu = apply_logic(gc.logic, value, latch);
        planes[plane][0] = (alu & mask) | (latch & !mask);
    }
}

/// Map a display-address counter value `ma` to a per-plane byte offset, applying
/// the CRTC byte/word/doubleword addressing transform and the 16-bit (64 KB)
/// counter wrap. `mode_control` is CRTC index 17h, `underline_loc` is index 14h.
/// See `docs/vga-core/README.md` slice 3.
pub fn display_offset(mode_control: u8, underline_loc: u8, ma: u32) -> usize {
    display_offset_row(mode_control, underline_loc, ma, 0)
}

pub fn display_counter(mode_control: u8, underline_loc: u8, row_base: u32, column: u32) -> u32 {
    let divisor = if underline_loc & 0x20 != 0 {
        4
    } else if mode_control & 0x08 != 0 {
        2
    } else {
        1
    };
    row_base + column / divisor
}

pub fn display_offset_row(mode_control: u8, underline_loc: u8, ma: u32, row_scan: u32) -> usize {
    let mut addr = if mode_control & 0x40 != 0 {
        ma // byte mode (CR17 bit 6): identity
    } else if underline_loc & 0x40 != 0 {
        // Doubleword mode (CR14 bit 6): MA0/MA1 are forced low, MA2..MA15 receive
        // A0..A13; CR17 bits 0/1 may still replace MA13/MA14 with row-scan bits.
        ma << 2
    } else {
        // word mode: rotate left 1, MA15 (CR17 bit 5 = 1) or MA13 (= 0) -> bit 0
        let wrap_bit = if mode_control & 0x20 != 0 { 15 } else { 13 };
        (ma << 1) | ((ma >> wrap_bit) & 1)
    };
    if mode_control & 0x01 == 0 {
        addr = (addr & !(1 << 13)) | ((row_scan & 0x01) << 13);
    }
    if mode_control & 0x02 == 0 {
        addr = (addr & !(1 << 14)) | (((row_scan >> 1) & 0x01) << 14);
    }
    (addr as usize) % VGA_PLANE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIOS_TEXT_WHITE: u8 = 0x3F;

    #[test]
    fn cga_320x200_decodes_a_byte_msb_first() {
        // Mode 04h, default color select (palette 0, low intensity): foreground
        // colors are green(2)/red(4)/brown(6), background is 0.
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        // 0b00_01_10_11: px0 = 0 (bg), px1 = 1 (green), px2 = 2 (red), px3 = 3 (brown).
        let decoded = vga.cga.decode_byte_320x200(0b00_01_10_11);
        assert_eq!(decoded, [CGA_BLACK, CGA_GREEN, CGA_RED, CGA_BROWN]);
        // 0b11_10_01_00: the reverse order.
        let decoded = vga.cga.decode_byte_320x200(0b11_10_01_00);
        assert_eq!(decoded, [CGA_BROWN, CGA_RED, CGA_GREEN, CGA_BLACK]);
    }

    #[test]
    fn cga_color_select_picks_the_palette() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        // Palette 1 (bit 5), low intensity: cyan(3)/magenta(5)/light gray(7).
        vga.write_port(0x3D9, 0x20);
        assert_eq!(
            vga.cga.decode_byte_320x200(0b00_01_10_11),
            [CGA_BLACK, CGA_CYAN, CGA_MAGENTA, CGA_LIGHT_GRAY]
        );
        // Palette 1 with intensity (bit 4 + bit 5): light cyan/light magenta/white.
        vga.write_port(0x3D9, 0x30);
        assert_eq!(
            vga.cga.decode_byte_320x200(0b00_01_10_11),
            [CGA_BLACK, CGA_LIGHT_CYAN, CGA_LIGHT_MAGENTA, CGA_WHITE]
        );
        // Palette 0 with intensity (bit 4 only): light green/light red/yellow.
        vga.write_port(0x3D9, 0x10);
        assert_eq!(
            vga.cga.decode_byte_320x200(0b00_01_10_11),
            [CGA_BLACK, CGA_LIGHT_GREEN, CGA_LIGHT_RED, CGA_YELLOW]
        );
        // The background nibble (bits 0-3) sets pixel value 0.
        vga.write_port(0x3D9, 0x01); // background = blue(1)
        assert_eq!(vga.cga.decode_byte_320x200(0b00_00_00_00)[0], 1);
        let raster = vga.render_full_frame();
        let border = (raster.height as usize - 1) * raster.width as usize;
        assert_eq!(raster.pixels[border], 1);
    }

    #[test]
    fn cga_mode_05h_forces_the_alternate_palette() {
        // Mode 05h ignores the palette-select bit and uses cyan/red/white. With
        // intensity off the canonical IBM/DOSBox set is cyan(3)/red(4)/light
        // gray(7).
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x05));
        vga.write_port(0x3D9, 0x20); // palette-select bit is ignored in mode 05h
        assert_eq!(
            vga.cga.decode_byte_320x200(0b00_01_10_11),
            [CGA_BLACK, CGA_CYAN, CGA_RED, CGA_LIGHT_GRAY]
        );
        // With intensity (bit 4): light cyan/light red/white.
        vga.write_port(0x3D9, 0x10);
        assert_eq!(
            vga.cga.decode_byte_320x200(0b00_01_10_11),
            [CGA_BLACK, CGA_LIGHT_CYAN, CGA_LIGHT_RED, CGA_WHITE]
        );
    }

    #[test]
    fn cga_interleave_addresses_odd_lines_in_the_high_bank() {
        // The even/odd interleave: scanline 0 reads framebuffer offset 0x0000,
        // scanline 1 reads offset 0x2000, scanline 2 reads 0x0050 (80 bytes), and
        // scanline 3 reads 0x2050.
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        // Place a distinctive byte at the start of each bank's first two rows.
        vga.cga_write(0x0000, 0b01_01_01_01); // even bank, row 0: value 1 -> green
        vga.cga_write(0x2000, 0b10_10_10_10); // odd bank, row 0: value 2 -> red
        vga.cga_write(0x0050, 0b11_11_11_11); // even bank, row 1 (line 2)
        vga.cga_write(0x2050, 0b00_01_10_11); // odd bank, row 1 (line 3)
        // Scanline 1 (odd) must read from 0x2000: every pixel is value 2 -> red.
        let line1 = vga.render_cga_row(1);
        assert_eq!(&line1[0..4], &[CGA_RED; 4]);
        // Scanline 0 (even) reads 0x0000: value 1 -> green for every pixel,
        // confirming bank selection by scanline parity.
        let line0 = vga.render_cga_row(0);
        assert_eq!(&line0[0..4], &[CGA_GREEN; 4]);
        // Scanline 2 (even, second row) reads 0x0050: value 3 -> brown.
        let line2 = vga.render_cga_row(2);
        assert_eq!(&line2[0..4], &[CGA_BROWN; 4]);
        // Scanline 3 (odd, second row) reads 0x2050: bg/green/red/brown.
        let line3 = vga.render_cga_row(3);
        assert_eq!(&line3[0..4], &[CGA_BLACK, CGA_GREEN, CGA_RED, CGA_BROWN]);
    }

    #[test]
    fn cga_graphics_scanout_honors_start_address_and_wraps() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        vga.cga_write(0x0000, 0b01_01_01_01);
        vga.cga_write(0x0001, 0b10_10_10_10);
        assert_eq!(&vga.render_cga_row(0)[0..4], &[CGA_GREEN; 4]);

        vga.crtc.start_address = 1;
        assert_eq!(&vga.render_cga_row(0)[0..4], &[CGA_RED; 4]);

        vga.crtc.start_address = (CGA_FB_SIZE - 1) as u32;
        vga.cga_write(CGA_FB_SIZE - 1, 0b11_11_11_11);
        assert_eq!(&vga.render_cga_row(0)[0..4], &[CGA_BROWN; 4]);
    }

    #[test]
    fn cga_640x200_unpacks_one_bit_per_pixel() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x06));
        assert_eq!(vga.crtc_regs.r00, 0x38);
        assert_eq!(vga.crtc_regs.r01, 0x28);
        assert_eq!(vga.crtc.char_width, 16);
        assert_eq!(vga.crtc.hdisp_end, 640);
        assert_eq!(htotal_dots(&vga.crtc), 912);
        assert_eq!(vga.active_mode(), VideoMode::Cga);
        // BIOS mode 06h starts white-on-black; 0b10101010 lights every other pixel.
        vga.cga_write(0x0000, 0b1010_1010);
        let line0 = vga.render_cga_row(0);
        assert_eq!(&line0[0..8], &[15, 0, 15, 0, 15, 0, 15, 0]);

        vga.write_port(0x3D9, 0x00);
        assert_eq!(&vga.render_cga_row(0)[0..8], &[0; 8]);

        vga.write_port(0x3D9, 0x04);
        assert_eq!(&vga.render_cga_row(0)[0..8], &[4, 0, 4, 0, 4, 0, 4, 0]);
    }

    #[test]
    fn cga_pixel_helpers_pack_and_xor_raw_pixel_values() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        assert!(vga.cga_write_pixel(2, 1, 3, false));
        assert_eq!(vga.cga_read(0x2000), 0x0C);
        assert_eq!(vga.cga_read_pixel(2, 1), 3);
        assert_eq!(vga.render_cga_row(1)[2], CGA_BROWN);

        assert!(vga.cga_write_pixel(2, 1, 1, true));
        assert_eq!(vga.cga_read_pixel(2, 1), 2);
        assert_eq!(vga.render_cga_row(1)[2], CGA_RED);

        assert!(vga.set_cga_mode(0x06));
        assert!(vga.cga_write_pixel(9, 0, 1, false));
        assert_eq!(vga.cga_read(1), 0x40);
        assert_eq!(vga.cga_read_pixel(9, 0), 1);
        assert_eq!(vga.render_cga_row(0)[9], CGA_WHITE);
    }

    #[test]
    fn cga_mode_set_installs_geometry_and_mode() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        assert_eq!(vga.active_mode(), VideoMode::Cga);
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(htotal_dots(&vga.crtc), 456);
        assert_eq!(vga.crtc.vdisp_end, 200);
        assert_eq!(vga.cga_mode_control(), 0x0A);
        // An unimplemented number leaves the mode untouched.
        assert!(!vga.set_cga_mode(0x09));
    }

    #[test]
    fn cga_mode_control_switches_graphics_decode_without_clearing_fb() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        vga.cga_write(0, 0b10_00_00_00);
        assert_eq!(vga.render_cga_row(0)[0], CGA_RED);

        assert!(vga.write_port(0x3D8, 0x1A));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.crtc.char_width, 16);
        assert_eq!(vga.crtc_regs.r01, 0x28);
        assert_eq!(htotal_dots(&vga.crtc), 912);
        assert_eq!(vga.cga_read(0), 0b10_00_00_00);
        assert_eq!(vga.render_cga_row(0)[0], CGA_BLACK);

        vga.write_port(0x3D9, 0x0F);
        assert_eq!(vga.render_cga_row(0)[0], CGA_WHITE);

        assert!(vga.write_port(0x3D8, 0x02));
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(vga.crtc.char_width, 8);
        assert_eq!(htotal_dots(&vga.crtc), 456);
        assert_eq!(vga.render_cga_row(0)[0], CGA_BLACK);
    }

    #[test]
    fn cga_mode_control_switches_text_width_and_blanks_without_clearing() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));
        text_put(&mut vga, 0, 0, 0xDB, 0x0F);

        assert_eq!(vga.cga_mode_control(), 0x28);
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);

        assert!(vga.write_port(0x3D8, 0x20));
        assert_eq!(vga.render_text_row(0)[0], CGA_BLACK);
        assert_eq!(vga.read_u8(0).unwrap(), 0xDB);

        assert!(vga.write_port(0x3D8, 0x29));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.frame().columns, 80);
        assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);
    }

    #[test]
    fn cga_mode_control_switches_between_text_and_graphics_without_clearing() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));
        text_put(&mut vga, 0, 0, 0b10_00_00_00, 0x0F);

        assert!(vga.write_port(0x3D8, 0x0A));
        assert_eq!(vga.active_mode(), VideoMode::Cga);
        assert_eq!(vga.render_cga_row(0)[0], CGA_RED);
        vga.cga_write(0, b'T');

        assert!(vga.write_port(0x3D8, 0x28));
        assert_eq!(vga.active_mode(), VideoMode::Text);
        assert_eq!(vga.frame().columns, 40);
        assert_eq!(vga.frame().cells[0].character, b'T');
    }

    #[test]
    fn cga_light_pen_ports_set_clear_status_and_latch_position() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        assert_eq!(vga.read_port(0x3DA).unwrap() & 0x06, 0x04);

        vga.advance(htotal_dots(&vga.crtc) * 16 + 80);
        assert_eq!(vga.read_port(0x3DC), Some(0xFF));
        assert_eq!(vga.read_port(0x3DA).unwrap() & 0x06, 0x06);

        vga.write_port(0x3D4, 0x10);
        assert_eq!(vga.read_port(0x3D5), Some(0x01));
        vga.write_port(0x3D4, 0x11);
        assert_eq!(vga.read_port(0x3D5), Some(0x4A));

        assert_eq!(vga.read_port(0x3DB), Some(0xFF));
        assert_eq!(vga.read_port(0x3DA).unwrap() & 0x06, 0x04);
    }

    #[test]
    fn cga_light_pen_graphics_column_has_cga_precision() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        vga.advance(htotal_dots(&vga.crtc) * 11 + 95);
        assert_eq!(vga.read_port(0x3DC), Some(0xFF));
        assert_eq!(vga.cga_light_pen_report(), Some((94, 10, 1, 11)));
        assert_eq!(vga.read_port(0x3DB), Some(0xFF));

        assert!(vga.set_cga_mode(0x06));
        vga.advance(htotal_dots(&vga.crtc) * 11 + 95);
        assert_eq!(vga.read_port(0x3DC), Some(0xFF));
        assert_eq!(vga.cga_light_pen_report(), Some((92, 10, 1, 5)));
    }

    #[test]
    fn cga_crtc_ports_decode_3d0_through_3d7_aliases() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        for (index_port, data_port, value) in [
            (0x3D0, 0x3D1, 0x20),
            (0x3D2, 0x3D3, 0x21),
            (0x3D4, 0x3D5, 0x22),
            (0x3D6, 0x3D7, 0x23),
        ] {
            assert!(vga.write_port(index_port, 0x01));
            assert!(vga.write_port(data_port, value));
            assert_eq!(vga.crtc_index, 0x01);
            assert_eq!(vga.read_port(index_port), None);
            assert_eq!(vga.read_port(data_port), None);
        }

        assert_eq!(vga.raster_width(), 0x23 * 8);
    }

    #[test]
    fn cga_crtc_timing_and_cursor_shape_registers_are_write_only() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        for (index, value) in [(0x01, 0x20), (0x09, 0x01), (0x0A, 0x06), (0x0B, 0x07)] {
            assert!(vga.write_port(0x3D4, index));
            assert!(vga.write_port(0x3D5, value));
            assert_eq!(vga.read_port(0x3D5), None, "CGA CRTC register {index:02X}");
        }
    }

    #[test]
    fn cga_crtc_index_is_a_5_bit_pointer() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        assert!(vga.write_port(0x3D4, 0x8E));
        assert!(vga.write_port(0x3D5, 0x12));
        assert!(vga.write_port(0x3D4, 0x8F));
        assert!(vga.write_port(0x3D5, 0x34));

        assert_eq!(vga.crtc_index, 0x0F);
        assert_eq!(vga.read_port(0x3D4), None);
        assert!(vga.write_port(0x3D4, 0x0E));
        assert_eq!(vga.read_port(0x3D5), Some(0x12));
        assert!(vga.write_port(0x3D4, 0x0F));
        assert_eq!(vga.read_port(0x3D5), Some(0x34));
    }

    #[test]
    fn cga_control_registers_are_6_bit_latches() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        assert!(vga.write_port(0x3D8, 0xFF));
        assert_eq!(vga.cga_mode_control(), 0x3F);
        assert!(vga.write_port(0x3D9, 0xFF));
        assert_eq!(vga.cga_color_select(), 0x3F);
    }

    #[test]
    fn cga_crtc_address_high_registers_are_6_bit_fields() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        assert!(vga.write_port(0x3D4, 0x0C));
        assert!(vga.write_port(0x3D5, 0xFF));
        assert!(vga.write_port(0x3D4, 0x0D));
        assert!(vga.write_port(0x3D5, 0xEE));
        assert!(vga.write_port(0x3D4, 0x0C));
        assert_eq!(vga.crtc_start_register(), 0x3FEE);
        assert_eq!(vga.read_port(0x3D5), None);
        assert!(vga.write_port(0x3D4, 0x0D));
        assert_eq!(vga.read_port(0x3D5), None);

        assert!(vga.write_port(0x3D4, 0x0E));
        assert!(vga.write_port(0x3D5, 0xFF));
        assert!(vga.write_port(0x3D4, 0x0F));
        assert!(vga.write_port(0x3D5, 0xAA));
        assert!(vga.write_port(0x3D4, 0x0E));
        assert_eq!(vga.read_port(0x3D5), Some(0x3F));
        assert!(vga.write_port(0x3D4, 0x0F));
        assert_eq!(vga.read_port(0x3D5), Some(0xAA));
    }

    #[test]
    fn cga_crtc_timing_registers_mask_to_6845_field_widths() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        for index in 0x03..=0x09 {
            assert!(vga.write_port(0x3D4, index));
            assert!(vga.write_port(0x3D5, 0xF0));
        }

        assert_eq!(vga.crtc_regs.r03, 0x00);
        assert_eq!(vga.crtc_regs.r04, 0x70);
        assert_eq!(vga.crtc_regs.r05, 0x10);
        assert_eq!(vga.crtc_regs.r06, 0x70);
        assert_eq!(vga.crtc_regs.r07, 0x70);
        assert_eq!(vga.crtc_regs.r08, 0x00);
        assert_eq!(vga.crtc_regs.r09, 0x10);
        assert_eq!(vga.crtc.max_scan, 0x10);
    }

    #[test]
    fn cga_cursor_mode_zero_does_not_blink() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x02));
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x00;
        vga.cursor_end = 0x07;
        vga.frames = 16;

        assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);
    }

    #[test]
    fn cga_cursor_end_ignores_vga_skew_bits() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x02));
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.frames = 0;

        assert!(vga.write_port(0x3D4, 0x0A));
        assert!(vga.write_port(0x3D5, 0x00));
        assert!(vga.write_port(0x3D4, 0x0B));
        assert!(vga.write_port(0x3D5, 0x67));

        assert_eq!(vga.cursor_end, 0x07);
        assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);
    }

    #[test]
    fn cga_status_reports_display_inactive_when_video_is_disabled() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        assert_eq!(vga.read_port(0x3DA).unwrap() & 0x01, 0x00);

        assert!(vga.write_port(0x3D8, CGA_MODE_GRAPHICS));
        assert_eq!(vga.read_port(0x3DA).unwrap() & 0x01, 0x01);

        assert!(vga.write_port(0x3D8, CGA_MODE_GRAPHICS | CGA_MODE_VIDEO_ENABLE));
        assert_eq!(vga.read_port(0x3DA).unwrap() & 0x01, 0x00);
    }

    #[test]
    fn cga_crtc_registers_can_retune_80_column_text_to_160x100() {
        let mut vga = Vga::default();
        vga.set_text_mode();
        assert_eq!(vga.raster_width(), 720);

        assert!(vga.write_port(0x3D8, 0x01)); // 80-column text, video disabled
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.render_text_row(0)[0], CGA_BLACK);

        for (index, value) in [(0x04, 0x7F), (0x06, 0x64), (0x07, 0x70), (0x09, 0x01)] {
            vga.write_port(0x3D4, index);
            vga.write_port(0x3D5, value);
        }
        text_put(&mut vga, 99, 0, 0xDB, 0x0F);

        assert!(vga.write_port(0x3D8, 0x09)); // preserve CRTC retune, enable video
        assert_eq!(vga.crtc.max_scan, 1);
        assert_eq!(vga.crtc.vtotal, 262);
        assert_eq!(vga.crtc.vdisp_end, 200);
        assert_eq!(vga.crtc.vretrace_start, 224);
        assert_eq!(vga.frame().columns, 80);

        vga.write_port(0x3D4, 0x09);
        assert_eq!(vga.read_port(0x3D5), None);
        assert_eq!(vga.render_text_row(198)[0], CGA_WHITE);

        let raster = vga.render_full_frame();
        assert_eq!(raster.width, 640);
        assert_eq!(raster.height, 262);
        assert_eq!(raster.pixels[200 * raster.width as usize], CGA_BLACK);
    }

    #[test]
    fn cga_crtc_horizontal_registers_drive_width_and_survive_video_enable() {
        let mut vga = Vga::default();
        vga.set_text_mode();

        assert!(vga.write_port(0x3D8, 0x01)); // 80-column text, video disabled
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(htotal_dots(&vga.crtc), 912);

        for (index, value) in [(0x00, 0x63), (0x01, 0x28)] {
            vga.write_port(0x3D4, index);
            vga.write_port(0x3D5, value);
        }

        assert_eq!(vga.raster_width(), 320);
        assert_eq!(htotal_dots(&vga.crtc), 800);
        assert_eq!(vga.frame().columns, 40);

        assert!(vga.write_port(0x3D8, 0x09)); // enable only; keep manual R0/R1
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(htotal_dots(&vga.crtc), 800);
        vga.write_port(0x3D4, 0x01);
        assert_eq!(vga.read_port(0x3D5), None);
    }

    #[test]
    fn cga_crtc_horizontal_displayed_drives_graphics_row_stride() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        vga.write_port(0x3D4, 0x01);
        vga.write_port(0x3D5, 0x20); // 32 displayed chars: 256 pixels, 64 bytes/row
        assert_eq!(vga.raster_width(), 256);

        vga.cga_write(80, 0b10_10_10_10); // old fixed stride would read this
        vga.cga_write(64, 0b01_01_01_01); // live 64-byte stride reads this
        assert_eq!(vga.cga_read_pixel(0, 2), 1);
        assert_eq!(vga.render_cga_row(2)[0], CGA_GREEN);
    }

    #[test]
    fn cga_640x200_crtc_displayed_uses_16_dot_characters() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x06));

        vga.write_port(0x3D4, 0x01);
        vga.write_port(0x3D5, 0x20); // 32 displayed chars: 512 pixels in high-res CGA.
        assert_eq!(vga.crtc.char_width, 16);
        assert_eq!(vga.raster_width(), 512);

        vga.cga_write(80, 0x00); // old fixed 80-byte stride would read this
        vga.cga_write(64, 0x80); // live 64-byte stride reads this
        assert_eq!(vga.cga_read_pixel(0, 2), 1);
        assert_eq!(vga.render_cga_row(2)[0], CGA_WHITE);
    }

    #[test]
    fn display_offset_applies_byte_word_dword_transforms() {
        // Byte mode (CR17 bit 6 = 1): identity, wrapped at 64 KB.
        assert_eq!(display_offset(0xE3, 0x00, 0x1234), 0x1234);
        assert_eq!(display_offset(0xE3, 0x00, 0x1_0005), 0x0005); // 64 KB counter wrap
        assert_eq!(
            display_counter(0xE3, 0x00, 0x1000, 3),
            0x1003,
            "normal address clock increments every character"
        );
        assert_eq!(
            display_counter(0xEB, 0x00, 0x1000, 3),
            0x1001,
            "CR17 bit 3 divides the address clock by two"
        );
        assert_eq!(
            display_counter(0xE3, 0x20, 0x1000, 7),
            0x1001,
            "CR14 bit 5 divides the address clock by four"
        );
        // Word mode, 16-bit wrap (CR17 = 0xA3: bit 6 = 0, bit 5 = 1): rotate left 1,
        // MA15 into bit 0.
        assert_eq!(display_offset(0xA3, 0x00, 0x4001), 0x8002); // MA15 = 0
        assert_eq!(display_offset(0xA3, 0x00, 0x8000), 0x0001); // MA15 = 1 -> bit 0
        // Word mode, 14-bit wrap (CR17 = 0x83: bit 6 = 0, bit 5 = 0): MA13 into bit 0.
        assert_eq!(display_offset(0x83, 0x00, 0x2000), 0x4001); // MA13 = 1 -> bit 0
        // Doubleword mode (CR14 bit 6 = 1): shift left two, forcing MA0/MA1 low.
        assert_eq!(display_offset(0xA3, 0x40, 0x3000), 0xC000);
        assert_eq!(
            display_offset_row(0xA0, 0x40, 0, 3),
            0x6000,
            "CR17 bits 0/1 still substitute row-scan bits in doubleword mode"
        );
        // Byte mode wins over the doubleword bit.
        assert_eq!(display_offset(0xE3, 0x40, 0x1234), 0x1234);
        assert_eq!(
            display_offset_row(0xE0, 0x00, 0, 3),
            0x6000,
            "CR17 bits 0/1 clear substitute row-scan bits into address bits 13/14"
        );
    }

    #[test]
    fn crtc_addressing_registers_are_wired_and_default_per_mode() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // 16-color planar modes power up in byte mode (CR17 = 0xE3).
        assert_eq!(vga.crtc.mode_control, 0xE3);
        assert_eq!(vga.crtc.underline_loc, 0x00);
        // A guest write through the CRTC ports updates the live registers.
        vga.write_port(0x3D4, 0x17); // CRTC index 17h
        vga.write_port(0x3D5, 0xA3); // word mode
        assert_eq!(vga.crtc.mode_control, 0xA3);
        vga.write_port(0x3D4, 0x14); // CRTC index 14h
        vga.write_port(0x3D5, 0x40); // doubleword bit
        assert_eq!(vga.crtc.underline_loc, 0x40);
    }

    #[test]
    fn crtc_address_generation_bits_affect_scanout() {
        let mut vga = Vga::default();
        vga.set_mode_0dh(); // double-scanned, so row 0 and row 1 share source row 0.
        vga.crtc.mode_control &= !0x01; // substitute row-scan bit 0 for address bit 13.
        vga.vram[0] = 0x80;
        vga.vram[VGA_PLANE_SIZE + 0x2000] = 0x80;

        assert_eq!(vga.render_active_row(0)[0], 0x01);
        assert_eq!(vga.render_active_row(1)[0], 0x02);

        let mut divided = Vga::default();
        assert!(divided.set_mode(0x10));
        divided.crtc.mode_control |= 0x08; // divide address clock by two.
        divided.vram[0] = 0x80;
        divided.vram[VGA_PLANE_SIZE + 1] = 0x80;
        assert_eq!(divided.render_active_row(0)[0], 0x01);
        assert_eq!(
            divided.render_active_row(0)[8],
            0x01,
            "second byte column still reads offset 0 when the address clock is /2"
        );
        assert_eq!(divided.render_active_row(0)[16], 0x02);
    }

    #[test]
    fn line_compare_registers_assemble_ten_bits_and_default_per_mode() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // 16-color planar modes power up with the split disabled (line compare 0x3FF).
        assert_eq!(vga.crtc.line_compare, 0x3FF);
        // Assemble a split at scanline 0x150: low byte via 18h, bit 8 set via the
        // Overflow register 07h bit 4, bit 9 cleared via the Maximum Scan Line 09h bit 6.
        vga.write_port(0x3D4, 0x18);
        vga.write_port(0x3D5, 0x50);
        vga.write_port(0x3D4, 0x07);
        vga.write_port(0x3D5, 0x10); // bit 4 set -> line compare bit 8 = 1
        vga.write_port(0x3D4, 0x09);
        vga.write_port(0x3D5, 0x00); // bit 6 clear -> line compare bit 9 = 0
        assert_eq!(vga.crtc.line_compare, 0x150);
        // Clearing the overflow bit 4 drops line compare bit 8.
        vga.write_port(0x3D4, 0x07);
        vga.write_port(0x3D5, 0x00);
        assert_eq!(vga.crtc.line_compare, 0x050);
    }

    #[test]
    fn beam_position_tracks_dots_in_scan_counter_units() {
        let t = CrtcTiming::mode_0dh();
        let htotal = (t.htotal_chars * t.char_width) as u64; // 800
        let dots = htotal * 5 + 10; // 5 full lines + 10 dots
        assert_eq!(beam_line(&t, dots), 5);
        assert_eq!(beam_dot(&t, dots), 10);
        assert!(beam_display_enable(&t, dots)); // line 5 < 400, dot 10 < 320
        assert!(!beam_vretrace(&t, dots)); // 5 < vretrace_start 412
    }

    #[test]
    fn advance_rolls_over_one_frame_in_o1() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        let frame = vga.frame_dots();
        vga.advance(frame * 2 + 7); // just past two frames in one call
        assert_eq!(vga.beam_dots(), 7); // (2*frame+7) mod frame
        assert_eq!(vga.frames_completed(), 2);
    }

    #[test]
    fn boots_with_defined_frame_dots_and_zeroed_vram() {
        let vga = Vga::default();
        assert_eq!(vga.vram.len(), VGA_PLANAR_SIZE);
        assert!(vga.vram.iter().all(|&b| b == 0));
        // frame_dots must be non-zero at boot (default text timing) so the
        // per-instruction beam advance never divides by zero. (Spec §3/§6.)
        assert!(
            vga.frame_dots() > 0,
            "frame_dots must be defined before any mode-set"
        );
    }

    #[test]
    fn write_mode_0_applies_rotate_setreset_logic_and_bitmask() {
        // Latches preloaded to 0xFF on all planes; write 0x0F with bit mask 0xF0,
        // copy logic, no set/reset. Result per plane = (data & mask) | (latch & !mask)
        // = (0x0F & 0xF0) | (0xFF & 0x0F) = 0x00 | 0x0F = 0x0F.
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let gc = GfxController {
            bit_mask: 0xF0,
            ..Default::default()
        };
        let latches = [0xFFu8; VGA_PLANES];
        write_planes(&mut planes, 0x0F, &gc, &latches);
        for p in &planes {
            assert_eq!(p[0], 0x0F);
        }
    }

    #[test]
    fn write_mode_0_set_reset_substitutes_color_per_plane() {
        // Enable set/reset on all planes, set/reset value = 0b1010 (planes 1 and 3).
        // With full bit mask and copy, each enabled plane writes its set/reset bit
        // expanded to 0xFF or 0x00.
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let gc = GfxController {
            bit_mask: 0xFF,
            enable_set_reset: 0x0F,
            set_reset: 0b1010,
            ..Default::default()
        };
        let latches = [0u8; VGA_PLANES];
        write_planes(&mut planes, 0x00, &gc, &latches);
        assert_eq!(planes[0][0], 0x00);
        assert_eq!(planes[1][0], 0xFF);
        assert_eq!(planes[2][0], 0x00);
        assert_eq!(planes[3][0], 0xFF);
    }

    #[test]
    fn write_mode_1_copies_latches_to_planes() {
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let gc = GfxController {
            write_mode: 1,
            ..Default::default()
        };
        let latches = [0x12, 0x34, 0x56, 0x78];
        write_planes(&mut planes, 0x00, &gc, &latches); // data ignored in WM1
        for plane in 0..VGA_PLANES {
            assert_eq!(planes[plane][0], latches[plane]);
        }
    }

    #[test]
    fn write_mode_2_expands_color_nibble_per_plane() {
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let gc = GfxController {
            write_mode: 2,
            bit_mask: 0xFF,
            ..Default::default()
        };
        let latches = [0u8; VGA_PLANES];
        write_planes(&mut planes, 0b0101, &gc, &latches); // planes 0 and 2 set
        assert_eq!(planes[0][0], 0xFF);
        assert_eq!(planes[1][0], 0x00);
        assert_eq!(planes[2][0], 0xFF);
        assert_eq!(planes[3][0], 0x00);
    }

    #[test]
    fn write_mode_3_uses_set_reset_color_with_rotated_bitmask() {
        // Effective mask = bit_mask (0xFF) & rotated data (0xF0, rotate=0) = 0xF0.
        // Set/Reset 0b0011 -> planes 0,1 color 0xFF, planes 2,3 color 0x00.
        // Result = (color & 0xF0) | (latch 0 & 0x0F).
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let gc = GfxController {
            write_mode: 3,
            set_reset: 0b0011,
            bit_mask: 0xFF,
            rotate: 0,
            ..Default::default()
        };
        let latches = [0u8; VGA_PLANES];
        write_planes(&mut planes, 0xF0, &gc, &latches);
        assert_eq!(planes[0][0], 0xF0);
        assert_eq!(planes[1][0], 0xF0);
        assert_eq!(planes[2][0], 0x00);
        assert_eq!(planes[3][0], 0x00);
    }

    #[test]
    fn read_mode_0_returns_selected_plane_and_loads_latches() {
        let planes = [[0x11u8; 1], [0x22u8; 1], [0x33u8; 1], [0x44u8; 1]];
        let gc = GfxController {
            read_map: 2,
            ..Default::default()
        };
        let mut latches = [0u8; VGA_PLANES];
        let byte = read_planes(&planes, &gc, &mut latches);
        assert_eq!(byte, 0x33);
        assert_eq!(latches, [0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn read_mode_1_color_compares_each_bit() {
        let planes = [[0xFFu8; 1], [0x00u8; 1], [0xFFu8; 1], [0x00u8; 1]];
        let gc = GfxController {
            read_mode: 1,
            color_dont_care: 0x0F, // care about all four planes
            color_compare: 0b0101, // planes 0 and 2 set, 1 and 3 clear
            ..Default::default()
        };
        let mut latches = [0u8; VGA_PLANES];
        let byte = read_planes(&planes, &gc, &mut latches);
        assert_eq!(byte, 0xFF); // every bit position matches the pattern
    }

    #[test]
    fn cpu_write_then_read_round_trips_through_latches() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.cpu_write(0x10, 0xA5);
        vga.gc.read_map = 0;
        assert_eq!(vga.cpu_read(0x10), 0xA5);
        assert_eq!(vga.latches, [0xA5; VGA_PLANES]);
    }

    #[test]
    fn map_mask_gates_which_planes_are_written() {
        let mut vga = Vga::default();
        vga.seq.memory_mode = 0x04; // sequential planar addressing
        vga.seq.map_mask = 0b0001; // only plane 0
        vga.gc.bit_mask = 0xFF;
        vga.cpu_write(0, 0xFF);
        assert_eq!(vga.plane_byte(0, 0), 0xFF);
        assert_eq!(vga.plane_byte(1, 0), 0x00);
    }

    #[test]
    fn odd_even_write_routes_cpu_addresses_to_plane_pairs() {
        let mut vga = Vga::default();
        vga.seq.memory_mode = 0x02; // bit 2 clear: odd/even writes enabled
        vga.seq.map_mask = 0x0F;
        vga.gc.bit_mask = 0xFF;

        vga.cpu_write(0, 0xA5);
        vga.cpu_write(1, 0x5A);

        assert_eq!(vga.plane_byte(0, 0), 0xA5);
        assert_eq!(vga.plane_byte(2, 0), 0xA5);
        assert_eq!(vga.plane_byte(1, 0), 0x5A);
        assert_eq!(vga.plane_byte(3, 0), 0x5A);
        assert_eq!(
            vga.plane_byte(0, 1),
            0x00,
            "odd/even addressing advances the plane offset every two CPU bytes"
        );
    }

    #[test]
    fn odd_even_read_uses_address_parity_and_read_map_pair() {
        let mut vga = Vga::default();
        vga.write_port(0x3CE, 0x05);
        vga.write_port(0x3CF, 0x10); // GC05 bit 4: odd/even read mode
        assert_eq!(vga.read_port(0x3CF), Some(0x10));
        vga.write_port(0x3CE, 0x06);
        vga.write_port(0x3CF, 0x03); // graphics + chain odd/even

        vga.vram[0] = 0x10;
        vga.vram[VGA_PLANE_SIZE] = 0x11;
        vga.vram[2 * VGA_PLANE_SIZE] = 0x20;
        vga.vram[3 * VGA_PLANE_SIZE] = 0x21;
        vga.vram[1] = 0x30;
        vga.vram[VGA_PLANE_SIZE + 1] = 0x31;

        vga.gc.read_map = 0;
        assert_eq!(vga.cpu_read(0), 0x10);
        assert_eq!(vga.cpu_read(1), 0x11);
        assert_eq!(vga.cpu_read(3), 0x31);

        vga.gc.read_map = 2;
        assert_eq!(vga.cpu_read(0), 0x20);
        assert_eq!(vga.cpu_read(1), 0x21);
        assert_eq!(vga.latches, [0x10, 0x11, 0x20, 0x21]);
    }

    #[test]
    fn catch_up_is_incremental_and_zero_when_beam_has_not_moved_a_line() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.advance(htotal_dots(&vga.crtc) * 3 + 5); // beam at line 3
        let drawn = vga.catch_up();
        assert_eq!(drawn, 3); // lines 0,1,2 rendered
        let drawn_again = vga.catch_up();
        assert_eq!(drawn_again, 0); // no line crossed since
    }

    #[test]
    fn advance_past_a_frame_finalizes_a_presented_buffer() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        assert!(vga.take_presented().is_none());
        vga.advance(vga.frame_dots() + 10); // cross one frame
        assert!(vga.presented_ready());
    }

    #[test]
    fn presented_frame_carries_active_visible_height_below_the_beam_total() {
        // The host crops to the active region for display: `display_height`
        // (vdisp_end) is the visible image; `height` stays the full beam frame
        // (vtotal) including the retrace/border the monitor never shows. Cropping
        // to display_height is what drops the black bottom bar before aspect-fill.
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.advance(vga.frame_dots() + 10);
        let raster = vga.take_presented().unwrap();
        assert_eq!(
            raster.height, vga.crtc.vtotal,
            "height is the full beam frame"
        );
        assert_eq!(
            raster.display_height, vga.crtc.vdisp_end,
            "display_height is the visible active region"
        );
        assert!(
            raster.display_height < raster.height,
            "the vertical blanking/border is excluded from the visible region"
        );
        assert_eq!(raster.pixels.len(), (raster.width * raster.height) as usize);
    }

    #[test]
    fn short_display_end_top_justifies_with_shortfall_at_bottom() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.crtc.vdisp_end = 199;
        vga.crtc.vtotal = 525;
        vga.crtc.vblank_start = 245;
        vga.crtc.vblank_end = 520;
        vga.crtc.vretrace_start = 247;
        vga.crtc.vretrace_end = 249;
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
            *b = 0xFF;
        } // plane 0 set
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        let raster = vga.render_full_frame();
        let w = raster.width as usize;
        assert_ne!(
            raster.pixels[0], 0,
            "row 0 should be active (top-justified)"
        );
        let last = (raster.height as usize - 1) * w;
        assert_eq!(
            raster.pixels[last], 0,
            "bottom row is border/blank, not active"
        );
    }

    #[test]
    fn pixel_pan_shifts_the_active_row_left() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.vram[0] = 0x80; // pixel 0 set in plane 0
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.attr.pixel_pan = 0;
        let row0 = vga.render_active_row(0);
        vga.attr.pixel_pan = 1;
        let row1 = vga.render_active_row(0);
        assert_eq!(row1[0], row0[1], "pan=1 shifts the row one pixel left");
    }

    #[test]
    fn start_address_write_applies_next_frame_not_mid_frame() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.advance(htotal_dots(&vga.crtc) * 100); // beam mid-frame, line 100
        vga.set_start_address(0x2000); // buffered, not active yet
        assert_eq!(
            vga.crtc.start_address, 0,
            "start address unchanged this frame"
        );
        vga.advance(vga.frame_dots()); // cross the frame boundary
        assert_eq!(vga.crtc.start_address, 0x2000, "applied on the next frame");
    }

    #[test]
    fn start_address_write_during_retrace_still_applies_next_frame() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.advance(htotal_dots(&vga.crtc) * (vga.crtc.vretrace_start as u64 + 1));
        vga.set_start_address(0x4000);
        vga.advance(vga.frame_dots());
        assert_eq!(vga.crtc.start_address, 0x4000, "no two-frame lag");
    }

    #[test]
    fn cga_start_address_registers_are_write_only_but_latch_pending_value() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));

        vga.write_port(0x3D4, 0x0C);
        vga.write_port(0x3D5, 0x12);
        vga.write_port(0x3D4, 0x0D);
        vga.write_port(0x3D5, 0x34);

        assert_eq!(vga.crtc_start_address(), 0);
        assert_eq!(vga.pending_start_address(), Some(0x1234));
        assert_eq!(vga.crtc_start_register(), 0x1234);
        vga.write_port(0x3D4, 0x0C);
        assert_eq!(vga.read_port(0x3D5), None);
        vga.write_port(0x3D4, 0x0D);
        assert_eq!(vga.read_port(0x3D5), None);
    }

    #[test]
    fn gc_and_seq_ports_round_trip_and_catch_up_runs_first() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.advance(htotal_dots(&vga.crtc) * 4); // beam at line 4
        vga.write_port(0x3CE, 8); // GC index 8 = bit mask
        vga.write_port(0x3CF, 0x0F);
        assert_eq!(vga.gc.bit_mask, 0x0F);
        assert_eq!(vga.last_line, 4); // the write caught up through line 4
    }

    #[test]
    fn gc06_memory_map_select_decodes_four_apertures() {
        // Memory Map Select (bits 3-2) picks the CPU aperture window.
        let mut vga = Vga::default();
        for (sel, base, length) in [
            (0b00u8, 0xA_0000u32, 0x2_0000u32), // A0000-BFFFF, 128K
            (0b01, 0xA_0000, 0x1_0000),         // A0000-AFFFF, 64K
            (0b10, 0xB_0000, 0x0_8000),         // B0000-B7FFF, 32K
            (0b11, 0xB_8000, 0x0_8000),         // B8000-BFFFF, 32K
        ] {
            vga.write_port(0x3CE, 0x06); // GC index 06h
            vga.write_port(0x3CF, sel << 2);
            let ap = vga.gfx_aperture();
            assert_eq!(ap.base, base, "base for map select {sel:#04b}");
            assert_eq!(ap.length, length, "length for map select {sel:#04b}");
        }
    }

    #[test]
    fn gc06_graphics_and_chain_odd_even_flags_read_back() {
        let mut vga = Vga::default();
        vga.write_port(0x3CE, 0x06);
        // bit 0 graphics, bit 1 chain odd/even, both set.
        vga.write_port(0x3CF, 0x03);
        let ap = vga.gfx_aperture();
        assert!(ap.graphics, "bit 0 set selects graphics mode");
        assert!(ap.chain_odd_even, "bit 1 set enables chain odd/even");
        // The raw register reads back through 3CF (low 4 bits stored).
        assert_eq!(vga.read_port(0x3CF), Some(0x03));

        // Clearing both flags reads back as alphanumeric, no chaining.
        vga.write_port(0x3CF, 0x00);
        let ap = vga.gfx_aperture();
        assert!(!ap.graphics);
        assert!(!ap.chain_odd_even);
    }

    #[test]
    fn horizontal_crtc_timing_registers_round_trip() {
        // Indices 00h-05h are the horizontal timing group; each reads back the
        // exact byte written, including the split-field registers 03h and 05h.
        let mut vga = Vga::default();
        let writes = [
            (0x00u8, 0x5Fu8),
            (0x01, 0x4F),
            (0x02, 0x50),
            (0x03, 0x82),
            (0x04, 0x54),
            (0x05, 0x80),
        ];
        vga.write_port(0x3D4, 0x11);
        vga.write_port(0x3D5, 0x00);
        for (index, value) in writes {
            vga.write_port(0x3D4, index);
            vga.write_port(0x3D5, value);
        }
        for (index, value) in writes {
            vga.write_port(0x3D4, index);
            assert_eq!(
                vga.read_port(0x3D5),
                Some(value),
                "horizontal CRTC index {index:#04x} round-trips"
            );
        }
    }

    #[test]
    fn crtc_11h_write_protect_locks_registers_00h_through_07h() {
        let mut vga = Vga::default();
        vga.crtc_regs.r00 = 0x5F;
        vga.crtc_regs.r07 = 0x00;
        vga.crtc.line_compare = 0;

        vga.write_port(0x3D4, 0x11);
        vga.write_port(0x3D5, 0x80);
        vga.write_port(0x3D4, 0x00);
        vga.write_port(0x3D5, 0x77);
        assert_eq!(vga.crtc_regs.r00, 0x5F);

        vga.write_port(0x3D4, 0x07);
        vga.write_port(0x3D5, 0x10);
        assert_eq!(vga.crtc_regs.r07, 0x00);
        assert_eq!(vga.crtc.line_compare & 0x100, 0x100);

        vga.write_port(0x3D4, 0x11);
        vga.write_port(0x3D5, 0x00);
        vga.write_port(0x3D4, 0x00);
        vga.write_port(0x3D5, 0x77);
        assert_eq!(vga.crtc_regs.r00, 0x77);
    }

    #[test]
    fn attribute_flipflop_alternates_index_then_data() {
        let mut vga = Vga::default();
        vga.read_status1(); // reset flip-flop to "index"
        vga.write_port(0x3C0, 0x13); // pixel pan index
        vga.write_port(0x3C0, 0x02); // value
        assert_eq!(vga.attr.pixel_pan, 0x02);
    }

    #[test]
    fn default_attr_palette_is_identity() {
        // Real VGA powers up with ATC palette register N = N, so a 4-bit plane
        // index maps straight to DAC N.
        let attr = Attribute::default();
        assert_eq!(
            attr.palette,
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert_eq!(attr.plane_enable & 0x0F, 0x0F);
    }

    #[test]
    fn misc_output_round_trips_3c2_3cc() {
        let mut vga = Vga::default();
        assert!(vga.write_port(0x3C2, 0x42));
        assert_eq!(vga.read_port(0x3CC), Some(0x42));
    }

    #[test]
    fn misc_output_clock_select_drives_dot_clock() {
        let mut vga = Vga::default();
        assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_28_HZ);

        assert!(vga.write_port(0x3C2, 0x00));
        assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_25_HZ);
        assert!(vga.write_port(0x3C2, 0x04));
        assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_28_HZ);
        assert!(vga.write_port(0x3C2, 0x08));
        assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_25_HZ);

        vga.set_mode13h();
        assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_25_HZ);
        vga.set_text_mode();
        assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_28_HZ);
    }

    #[test]
    fn misc_output_ios_selects_crtc_status_and_feature_ports() {
        let mut vga = Vga::default();

        assert!(vga.write_port(0x3D4, 0x0C));
        assert_eq!(vga.read_port(0x3D4), Some(0x0C));
        assert_eq!(vga.read_port(0x3B4), None);
        assert!(vga.read_port(0x3DA).is_some());
        assert_eq!(vga.read_port(0x3BA), None);
        assert!(vga.write_port(0x3DA, 0x0A));
        assert_eq!(vga.read_port(0x3CA), Some(0x0A));

        assert!(vga.write_port(0x3C2, vga.misc_output & !0x01));
        assert!(!vga.write_port(0x3D4, 0x0A));
        assert!(vga.write_port(0x3B4, 0x0A));
        assert_eq!(vga.read_port(0x3B4), Some(0x0A));
        assert_eq!(vga.read_port(0x3D4), None);
        assert!(vga.write_port(0x3B5, 0x05));
        assert_eq!(vga.cursor_start, 0x05);
        assert!(vga.read_port(0x3BA).is_some());
        assert_eq!(vga.read_port(0x3DA), None);
        assert!(vga.write_port(0x3BA, 0x05));
        assert_eq!(vga.read_port(0x3CA), Some(0x05));
    }

    #[test]
    fn mono_text_mode_uses_b000_9x14_720x350() {
        let mut vga = Vga::default();
        vga.set_mono_text_mode();

        assert_eq!(vga.active_mode(), VideoMode::Text);
        assert_eq!(vga.text_memory_base(), VGA_MONO_TEXT_BASE);
        assert_eq!(vga.raster_width(), 720);
        assert_eq!(vga.raster_height(), 449);
        assert_eq!(vga.crtc.vdisp_end, 350);
        assert_eq!(vga.crtc.max_scan, 13);
        assert_eq!(vga.misc_output & 0xCD, 0x84);
        assert_eq!(vga.cursor_start, 0x0C);
        assert_eq!(vga.cursor_end, 0x0D);

        text_put(&mut vga, 0, 0, 0xDB, 0x0F);
        assert_eq!(vga.render_text_row(0)[0], 0x0F);
    }

    #[test]
    fn pel_mask_round_trips_3c6() {
        let mut vga = Vga::default();
        assert!(vga.write_port(0x3C6, 0x0F));
        assert_eq!(vga.read_port(0x3C6), Some(0x0F));
    }

    #[test]
    fn atc_readback_3c1_returns_indexed_register() {
        let mut vga = Vga::default();
        vga.read_status1(); // reset the 3C0 flip-flop to "address"
        vga.write_port(0x3C0, 0x13); // address: select the Pixel Pan register
        vga.write_port(0x3C0, 0x07); // data: pixel_pan = 7
        // 3C1 reads back the register selected by the last index write.
        assert_eq!(vga.read_port(0x3C1), Some(0x07));
    }

    #[test]
    fn pel_mask_masks_the_dac_index_in_render() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // Plane 0 set everywhere so every pixel is the 4-bit index 1.
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
            *b = 0xFF;
        }
        vga.attr.palette[1] = 0x2A; // ATC maps index 1 -> DAC 42
        vga.pel_mask = 0xFF;
        let full = vga.render_active_row(0);
        assert_eq!(full[0], 0x2A, "no mask: index 1 reaches DAC 42");
        vga.pel_mask = 0x0F;
        let masked = vga.render_active_row(0);
        assert_eq!(
            masked[0], 0x0A,
            "pel mask 0x0F folds DAC 42 to the low nibble"
        );
    }

    #[test]
    fn mid_frame_palette_change_splits_the_raster_at_the_beam_row() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // Active content = attribute index 1 everywhere (plane 0 set).
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
            *b = 0xFF;
        }
        vga.attr.palette = core::array::from_fn(|i| i as u8); // index 1 -> DAC 1
        // Run to counter line 50, then repaint palette[1] = 9 via the attribute port.
        vga.advance(htotal_dots(&vga.crtc) * 50);
        // Index 1 with bit 5 (Palette Address Source) set keeps the display on
        // while the palette register is rewritten, so the screen does not blank.
        vga.write_port(0x3C0, 0x20 | 0x01); // attr index 1, PAS on
        vga.write_port(0x3C0, 9); // palette[1] = 9
        // Finish the frame.
        vga.advance(vga.frame_dots());
        let raster = vga.take_presented().unwrap();
        let w = raster.width as usize;
        assert_eq!(raster.pixels[0], 1, "above the split uses the old palette");
        let below = 120 * w; // raster row 120 (counter line 120, > split at 50)
        assert_eq!(
            raster.pixels[below], 9,
            "below the split uses the new palette"
        );
    }

    #[test]
    fn status1_reports_beam_and_resets_attribute_flipflop() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // Park the beam in vertical retrace.
        let htotal = htotal_dots(&vga.crtc);
        vga.beam = htotal * (vga.crtc.vretrace_start as u64);
        let status = vga.read_status1();
        assert_eq!(status & 0x08, 0x08); // bit 3 vertical retrace
        assert_eq!(status & 0x01, 0x01); // bit 0 display disabled (in retrace)
        // Reading 3DA resets the attribute address/data flip-flop to "address".
        assert!(!vga.attr.flip_flop_data);
    }

    #[test]
    fn status1_reports_attribute_video_status_mux_bits() {
        let mut vga = Vga::default();
        vga.set_mode13h();

        for (color, mux, expected) in [
            (0x04, 0x00, 0x20), // mux 00: status bits 5/4 = colour bits 2/0
            (0x01, 0x00, 0x10),
            (0x20, 0x10, 0x20), // mux 01: colour bits 5/4
            (0x10, 0x10, 0x10),
            (0x08, 0x20, 0x20), // mux 10: colour bits 3/1
            (0x02, 0x20, 0x10),
            (0x80, 0x30, 0x20), // mux 11: colour bits 7/6
            (0x40, 0x30, 0x10),
        ] {
            vga.beam = 0;
            vga.vram[0] = color;
            vga.attr.plane_enable = 0x0F | mux;
            assert_eq!(vga.read_status1() & 0x30, expected);
        }

        let htotal = htotal_dots(&vga.crtc);
        vga.beam = htotal * u64::from(vga.crtc.vretrace_start);
        vga.vram[0] = 0xC0;
        vga.attr.plane_enable = 0x3F;
        assert_eq!(vga.read_status1() & 0x30, 0x00);
    }

    #[test]
    fn mode_set_resets_beam_and_reports_planar_geometry() {
        let mut vga = Vga::default();
        vga.advance(12345); // dirty the beam in text mode
        vga.set_mode_0dh();
        assert_eq!(vga.beam_dots(), 0);
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(vga.frame_dots(), CrtcTiming::mode_0dh().frame_dots());
    }

    #[test]
    fn text_mode_defaults_to_blank_80x25_screen() {
        let text = Vga::default();
        let frame = text.frame();

        assert_eq!(frame.columns, 80);
        assert_eq!(frame.rows, 25);
        assert_eq!(frame.cells.len(), 2000);
        assert!(frame.line_string(0).is_empty());
        assert_eq!((text.cursor_start, text.cursor_end), (0x0E, 0x0F));
    }

    #[test]
    fn text_memory_write_updates_frame_cell() {
        let mut text = Vga::default();
        text.write_u8(0, b'V').unwrap();
        text.write_u8(1, 0x0a).unwrap();

        let frame = text.frame();
        assert_eq!(frame.cells[0].character, b'V');
        assert_eq!(frame.cells[0].attribute, 0x0a);
        assert_eq!(frame.line_string(0), "V");
    }

    #[test]
    fn mode13h_chain4_write_routes_byte_n_to_plane_n_mod_4() {
        let mut video = Vga::default();
        video.set_mode13h();
        // Chain-4 writes byte 123 (0x7B) to plane 123 & 3 = 3 at plane offset
        // 123 >> 2 = 30, bypassing the planar datapath. The other planes at that
        // plane offset stay clear.
        video.cpu_write_chain4(123, 0x2a);
        assert_eq!(
            video.plane_byte(3, 30),
            0x2a,
            "byte 123 lands in plane 3 @ 30"
        );
        for plane in 0..VGA_PLANES {
            if plane == 3 {
                continue;
            }
            assert_eq!(
                video.plane_byte(plane, 30),
                0,
                "plane {plane} at offset 30 is untouched"
            );
        }
        // The chain-4 read selects the same plane/offset, so it round-trips.
        assert_eq!(video.cpu_read_chain4(123), 0x2a);
        // The shared 256-color scanout reads plane 123 & 3 = 3 at plane offset
        // 123 >> 2 = 30 as pixel 123, so the raster carries the written byte.
        assert_eq!(
            video.render_256color_row(0)[123],
            0x2a,
            "pixel 123 scans out the chain-4 written byte"
        );
    }

    #[test]
    fn crtc_cursor_ports_track_offset() {
        let mut text = Vga::default();
        assert!(text.write_port(0x03d4, 0x0e));
        assert!(text.write_port(0x03d5, 0x12));
        assert!(text.write_port(0x03d4, 0x0f));
        assert!(text.write_port(0x03d5, 0x34));

        assert_eq!(text.cursor_offset, 0x1234);
        assert_eq!(text.read_port(0x03d5), Some(0x34));
    }

    #[test]
    fn cursor_shape_registers_round_trip() {
        let mut vga = Vga::default();
        assert!(vga.write_port(0x3D4, 0x0A));
        assert!(vga.write_port(0x3D5, 0x0E)); // start scanline 14
        assert!(vga.write_port(0x3D4, 0x0B));
        assert!(vga.write_port(0x3D5, 0x0F)); // end scanline 15

        assert_eq!(vga.cursor_start, 0x0E);
        assert_eq!(vga.cursor_end, 0x0F);
        // Readback through the CRTC data port.
        assert!(vga.write_port(0x3D4, 0x0A));
        assert_eq!(vga.read_port(0x3D5), Some(0x0E));
        assert!(vga.write_port(0x3D4, 0x0B));
        assert_eq!(vga.read_port(0x3D5), Some(0x0F));
    }

    #[test]
    fn set_mode_selects_mode13h() {
        let mut video = Vga::default();
        assert_eq!(video.active_mode(), VideoMode::Text);
        assert!(video.set_mode(0x13));
        assert_eq!(video.active_mode(), VideoMode::Mode13h);
    }

    #[test]
    fn ega_modes_load_the_matching_bios_dac_palette() {
        // Mode 13h keeps the 256-color palette3: brown sits at index 6 directly
        // and the gray ramp at 0x10..0x1F. (This is the value an EGA mode used
        // to wrongly inherit, turning brown attributes gray.)
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x13));
        assert_eq!(vga.dac.entry(0x06), [0x2a, 0x15, 0x00]);
        assert_eq!(vga.dac.entry(0x14), [0x0e, 0x0e, 0x0e]); // gray ramp, not brown

        // Mode 10h loads palette2, the EGA 64-color decode. Its default
        // attribute map sends color 6 -> 0x14 and the bright eight -> 0x38..0x3F,
        // so those entries must hold real colors, not the gray ramp.
        vga.set_mode(0x10);
        assert_eq!(vga.dac.entry(0x14), [0x2a, 0x15, 0x00], "0x10 brown");
        assert_eq!(vga.dac.entry(0x38), [0x15, 0x15, 0x15], "0x10 dark gray");
        assert_eq!(vga.dac.entry(0x3f), [0x3f, 0x3f, 0x3f], "0x10 white");

        // Mode 0Dh (CGA 320x200, the Monkey Island mode) loads palette1: brown at
        // 6 and the bright eight at 0x10..0x17 (this mode's attribute targets).
        vga.set_mode(0x0D);
        assert_eq!(vga.dac.entry(0x06), [0x2a, 0x15, 0x00], "0x0D brown");
        assert_eq!(vga.dac.entry(0x10), [0x15, 0x15, 0x15], "0x0D bright black");
        assert_eq!(vga.dac.entry(0x17), [0x3f, 0x3f, 0x3f], "0x0D bright white");
    }

    #[test]
    fn vga_text_mode3_resolves_remapped_colors_through_palette2() {
        // Mode 03h drives text colors through the EGA attribute remap (color 6 ->
        // 0x14, bright eight -> 0x38..0x3F, white -> 0x3F), so the DAC must be
        // palette2 or those land on the 256-color ramps: white came out pink,
        // brown gray, dark gray blue. Resolve the final RGB the remap produces.
        let mut vga = Vga::default();
        vga.set_text_mode();
        assert_eq!(vga.dac.rgb888(0x3f), (0xff, 0xff, 0xff), "bright white");
        assert_eq!(vga.dac.rgb888(0x14), (0xaa, 0x55, 0x00), "brown");
        assert_eq!(vga.dac.rgb888(0x38), (0x55, 0x55, 0x55), "dark gray");

        // CGA text (modes 00h-02h) uses direct RGBI color numbers, which must stay
        // the standard 16 at entries 0..15 (palette3): white is index 15, not 0x3F.
        assert!(vga.set_cga_text_mode(0x01));
        assert_eq!(
            vga.dac.rgb888(0x0f),
            (0xff, 0xff, 0xff),
            "CGA white at index 15"
        );
        assert_eq!(
            vga.dac.rgb888(0x06),
            (0xaa, 0x55, 0x00),
            "CGA brown at index 6"
        );
    }

    #[test]
    fn mode13h_mode_set_installs_chain4_and_a000_graphics_defaults() {
        let mut video = Vga::default();
        assert!(video.set_mode(0x13));

        assert_eq!(video.seq.map_mask, 0x0F);
        assert_eq!(video.seq.memory_mode, 0x0E);
        assert_eq!(video.gc.bit_mask, 0xFF);
        assert_eq!(video.gc.color_dont_care, 0x0F);
        let ap = video.gfx_aperture();
        assert_eq!((ap.base, ap.length), (0x000A_0000, 0x0001_0000));
        assert!(ap.graphics);
    }

    #[test]
    fn dac_write_then_read_round_trips() {
        let mut video = Vga::default();
        video.write_port(0x03c8, 5); // write index = 5
        video.write_port(0x03c9, 63); // R
        video.write_port(0x03c9, 10); // G
        video.write_port(0x03c9, 31); // B
        video.write_port(0x03c7, 5); // read index = 5
        assert_eq!(video.read_port(0x03c9), Some(63));
        assert_eq!(video.read_port(0x03c9), Some(10));
        assert_eq!(video.read_port(0x03c9), Some(31));
    }

    #[test]
    fn palette_argb_expands_six_bit_components() {
        let mut video = Vga::default();
        video.write_port(0x03c8, 1);
        video.write_port(0x03c9, 63); // R
        video.write_port(0x03c9, 0); // G
        video.write_port(0x03c9, 0); // B
        assert_eq!(video.palette_argb()[1], 0x00FF_0000);
    }

    #[test]
    fn mode_0dh_raster_height_equals_vtotal_not_doubled() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // One raster row per scanline: vtotal (449), not vtotal * scan_factor.
        assert_eq!(vga.raster_height(), 449);
    }

    #[test]
    fn double_scan_holds_each_source_row_for_two_scanlines() {
        let mut vga = Vga::default();
        vga.set_mode_0dh(); // doubled mode
        // Source row 0 has pixel 0 set in plane 0; source row 1 (byte pitch
        // offset*2 = 40) is clear.
        vga.vram[0] = 0x80;
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        let r0 = vga.render_active_row(0);
        let r1 = vga.render_active_row(1);
        let r2 = vga.render_active_row(2);
        assert_eq!(r0, r1, "scanlines 0 and 1 read the same source row");
        assert_ne!(r0, r2, "scanline 2 reads the next source row");
        assert_eq!(r0[0], 1, "source row 0 pixel 0 is attribute index 1");
        assert_eq!(r2[0], 0, "source row 1 pixel 0 is attribute index 0");
    }

    #[test]
    fn preset_row_scan_offsets_graphics_source_rows() {
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x10));
        let pitch = (vga.crtc.offset * 2) as usize;
        vga.vram[0] = 0x80; // row 0, plane 0 -> index 1
        vga.vram[VGA_PLANE_SIZE + pitch] = 0x80; // row 1, plane 1 -> index 2

        assert_eq!(vga.render_active_row(0)[0], 0x01);
        vga.crtc.preset_row_scan = 0x01;
        assert_eq!(vga.render_active_row(0)[0], 0x02);
        vga.crtc.line_compare = 0;
        assert_eq!(
            vga.render_active_row(3)[0],
            0x01,
            "preset row resets below the line-compare split"
        );
        vga.crtc.line_compare = u32::MAX;
        vga.crtc.preset_row_scan = 0x20;
        vga.vram[0] = 0x80; // row 0, byte 0, plane 0 -> index 1
        vga.vram[VGA_PLANE_SIZE] = 0x00;
        vga.vram[VGA_PLANE_SIZE + 1] = 0x80; // row 0, byte 1, plane 1 -> index 2
        assert_eq!(vga.render_active_row(0)[0], 0x02);
        vga.crtc.line_compare = 0;
        vga.attr.mode_control = 0x20;
        assert_eq!(
            vga.render_active_row(3)[0],
            0x01,
            "byte pan resets below the split when AC 10h bit 5 requests it"
        );

        let mut mode13h = Vga::default();
        mode13h.set_mode13h();
        let pitch = (mode13h.crtc.offset * 2) as usize;
        mode13h.vram[0] = 0x11;
        mode13h.vram[pitch] = 0x22;
        mode13h.crtc.preset_row_scan = 0x02;
        assert_eq!(mode13h.render_256color_row(0)[0], 0x22);
        mode13h.crtc.preset_row_scan = 0x20;
        mode13h.vram[1] = 0x33;
        assert_eq!(mode13h.render_256color_row(0)[0], 0x33);
    }

    #[test]
    fn set_mode_selects_geometry_for_each_graphics_number() {
        let mut vga = Vga::default();

        assert!(vga.set_mode(0x0E));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.raster_height(), 449); // 0Eh vtotal 449; 200 rows double-scanned to 400 active
        assert_eq!(vga.active_mode(), VideoMode::Planar);

        assert!(vga.set_mode(0x10));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.raster_height(), 449); // 640x350, vtotal 449

        assert!(vga.set_mode(0x12));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.raster_height(), 525); // 640x480, vtotal 525

        assert!(vga.set_mode(0x0D));
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(vga.raster_height(), 449);

        assert!(vga.set_mode(0x13));
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(vga.raster_height(), 449);
        assert_eq!(vga.active_mode(), VideoMode::Mode13h);

        assert!(!vga.set_mode(0x99)); // unknown number leaves a false result
    }

    #[test]
    fn bios_mode_sets_seed_vgabios_crtc_readback() {
        fn assert_regs(vga: &Vga, mode: u8, expected: &[(u8, u8)]) {
            for &(index, value) in expected {
                assert_eq!(
                    vga.crtc_register_latch(index),
                    value,
                    "mode {mode:02X} CRTC {index:02X}"
                );
            }
        }

        let mut vga = Vga::default();
        assert_regs(
            &vga,
            0x03,
            &[
                (0x04, 0x55),
                (0x05, 0x81),
                (0x09, 0x4F),
                (0x13, 0x28),
                (0x14, 0x1F),
                (0x15, 0x96),
                (0x17, 0xA3),
            ],
        );

        for (mode, expected) in [
            (
                0x0D,
                &[
                    (0x00, 0x2D),
                    (0x01, 0x27),
                    (0x04, 0x2B),
                    (0x09, 0xC0),
                    (0x13, 0x14),
                    (0x15, 0x96),
                    (0x16, 0xB9),
                ][..],
            ),
            (0x0E, &[(0x00, 0x5F), (0x01, 0x4F), (0x13, 0x28)][..]),
            (0x0F, &[(0x09, 0x40), (0x14, 0x0F), (0x15, 0x63)][..]),
            (0x10, &[(0x09, 0x40), (0x14, 0x0F), (0x15, 0x63)][..]),
            (
                0x11,
                &[(0x06, 0x0B), (0x07, 0x3E), (0x10, 0xEA), (0x16, 0x04)][..],
            ),
            (
                0x12,
                &[(0x06, 0x0B), (0x07, 0x3E), (0x10, 0xEA), (0x16, 0x04)][..],
            ),
        ] {
            assert!(vga.set_mode(mode));
            assert_regs(&vga, mode, expected);
        }

        vga.set_mode13h();
        assert_regs(
            &vga,
            0x13,
            &[(0x09, 0x41), (0x13, 0x28), (0x14, 0x40), (0x17, 0xA3)],
        );
    }

    #[test]
    fn bios_mode_sets_seed_vgabios_sequencer_readback() {
        fn seq_reg(vga: &mut Vga, index: u8) -> u8 {
            assert!(vga.write_port(0x3C4, index));
            vga.read_port(0x3C5).unwrap()
        }

        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.set_text_mode();
        assert_eq!(seq_reg(&mut vga, 1), 0x00);
        assert_eq!(seq_reg(&mut vga, 2), 0x03);
        assert_eq!(seq_reg(&mut vga, 4), 0x02);

        for (mode, clocking_mode, memory_mode) in [
            (0x0D, 0x09, 0x06),
            (0x0E, 0x01, 0x06),
            (0x0F, 0x01, 0x06),
            (0x10, 0x01, 0x06),
            (0x11, 0x01, 0x06),
            (0x12, 0x01, 0x06),
        ] {
            assert!(vga.set_mode(mode));
            assert_eq!(seq_reg(&mut vga, 1), clocking_mode, "mode {mode:02X}");
            assert_eq!(seq_reg(&mut vga, 2), 0x0F, "mode {mode:02X}");
            assert_eq!(seq_reg(&mut vga, 3), 0x00, "mode {mode:02X}");
            assert_eq!(seq_reg(&mut vga, 4), memory_mode, "mode {mode:02X}");
        }

        vga.set_mode13h();
        assert_eq!(seq_reg(&mut vga, 1), 0x01);
        assert_eq!(seq_reg(&mut vga, 2), 0x0F);
        assert_eq!(seq_reg(&mut vga, 4), 0x0E);
    }

    #[test]
    fn bios_mode_sets_seed_vgabios_graphics_controller_readback() {
        fn gc_reg(vga: &mut Vga, index: u8) -> u8 {
            assert!(vga.write_port(0x3CE, index));
            vga.read_port(0x3CF).unwrap()
        }

        let mut vga = Vga::default();
        assert_eq!(gc_reg(&mut vga, 5), 0x10);
        assert_eq!(gc_reg(&mut vga, 6), 0x0E);
        assert_eq!(gc_reg(&mut vga, 7), 0x0F);
        assert_eq!(gc_reg(&mut vga, 8), 0xFF);

        for mode in 0x0D..=0x12 {
            assert!(vga.set_mode(mode));
            assert_eq!(gc_reg(&mut vga, 5), 0x00, "mode {mode:02X}");
            assert_eq!(gc_reg(&mut vga, 6), 0x05, "mode {mode:02X}");
            assert_eq!(gc_reg(&mut vga, 7), 0x0F, "mode {mode:02X}");
            assert_eq!(gc_reg(&mut vga, 8), 0xFF, "mode {mode:02X}");
        }

        vga.set_mode13h();
        assert_eq!(gc_reg(&mut vga, 5), 0x40);
        assert_eq!(gc_reg(&mut vga, 6), 0x05);
        assert_eq!(gc_reg(&mut vga, 7), 0x0F);
        assert_eq!(gc_reg(&mut vga, 8), 0xFF);

        vga.set_text_mode();
        assert_eq!(gc_reg(&mut vga, 5), 0x10);
        assert_eq!(gc_reg(&mut vga, 6), 0x0E);
    }

    #[test]
    fn bios_graphics_modes_seed_vgabios_attribute_controller_readback() {
        fn attr_reg(vga: &mut Vga, index: u8) -> u8 {
            vga.read_status1();
            assert!(vga.write_port(0x3C0, 0x20 | (index & 0x1F)));
            vga.read_port(0x3C1).unwrap()
        }

        let mut vga = Vga::default();
        assert_eq!(attr_reg(&mut vga, 0x06), 0x14, "mode 03H AC06");
        assert_eq!(attr_reg(&mut vga, 0x08), 0x38, "mode 03H AC08");
        assert_eq!(attr_reg(&mut vga, 0x0F), 0x3F, "mode 03H AC0F");
        assert_eq!(attr_reg(&mut vga, 0x10), 0x0C, "mode 03H AC10");
        assert_eq!(attr_reg(&mut vga, 0x12), 0x0F, "mode 03H AC12");
        assert_eq!(attr_reg(&mut vga, 0x13), 0x08, "mode 03H AC13");
        assert_eq!(attr_reg(&mut vga, 0x14), 0x00, "mode 03H AC14");

        for (mode, expected) in [
            (0x0D, &[(0x08, 0x10), (0x10, 0x01), (0x12, 0x0F)][..]),
            (0x0E, &[(0x08, 0x10), (0x10, 0x01), (0x12, 0x0F)][..]),
            (
                0x0F,
                &[(0x01, 0x08), (0x04, 0x18), (0x10, 0x01), (0x12, 0x01)][..],
            ),
            (
                0x10,
                &[(0x06, 0x14), (0x08, 0x38), (0x10, 0x01), (0x12, 0x0F)][..],
            ),
            (
                0x11,
                &[(0x01, 0x3F), (0x0F, 0x3F), (0x10, 0x01), (0x12, 0x0F)][..],
            ),
            (
                0x12,
                &[(0x06, 0x14), (0x08, 0x38), (0x10, 0x01), (0x12, 0x0F)][..],
            ),
        ] {
            assert!(vga.set_mode(mode));
            for &(index, value) in expected {
                assert_eq!(
                    attr_reg(&mut vga, index),
                    value,
                    "mode {mode:02X} AC{index:02X}"
                );
            }
            assert_eq!(attr_reg(&mut vga, 0x13), 0x00, "mode {mode:02X} AC13");
            assert_eq!(attr_reg(&mut vga, 0x14), 0x00, "mode {mode:02X} AC14");
        }

        vga.set_mode13h();
        assert_eq!(attr_reg(&mut vga, 0x0F), 0x0F);
        assert_eq!(attr_reg(&mut vga, 0x10), 0x41);
        assert_eq!(attr_reg(&mut vga, 0x12), 0x0F);
        assert_eq!(attr_reg(&mut vga, 0x14), 0x00);
    }

    #[test]
    fn planar_mode_set_installs_writeable_graphics_defaults() {
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x10));

        vga.cpu_write(0, 0xA5);
        for plane in 0..VGA_PLANES {
            vga.gc.read_map = plane as u8;
            assert_eq!(vga.cpu_read(0), 0xA5);
        }
    }

    #[test]
    fn word_mode_render_rotates_the_address() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.crtc.mode_control = 0xA3; // force word mode (bit 6 = 0), 16-bit wrap
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        // The second character (byte_col 1) has counter ma = 1. Word mode maps ma = 1
        // to plane offset 2 ((1 << 1) | 0); byte mode would read offset 1. Mark only
        // offset 2, so a correct word-mode read shows index 1 at pixel 8.
        vga.vram[2] = 0x80; // bit 7 -> the first pixel of that character
        let row = vga.render_active_row(0);
        assert_eq!(
            row[8], 1,
            "word mode reads plane offset 2 for the 2nd character"
        );
        assert_eq!(row[0], 0, "char 0 (offset 0) is clear");
    }

    #[test]
    fn byte_mode_wrap_scanout_equals_top_of_vram() {
        let mut vga = Vga::default();
        vga.set_mode_0dh(); // byte mode (CR17 = 0xE3)
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        // Distinct mark at the very top of VRAM (plane 0 offset 0): pixels 0..7 = index 1.
        vga.vram[0] = 0xFF;
        // Reference row from start_address 0: its first 8 pixels come from offset 0.
        vga.crtc.start_address = 0;
        let top = vga.render_active_row(0);
        assert_eq!(
            &top[0..8],
            &[1u8; 8],
            "top-of-VRAM byte renders 8 pixels of index 1"
        );
        // Start 8 bytes before the 64 KB wrap: byte_col 0..7 read 0xFFF8..0xFFFF (clear),
        // byte_col 8 wraps to offset 0 (the marked byte). So pixels 64..71 must equal
        // the top-of-VRAM pixels, not tear.
        vga.crtc.start_address = 0xFFF8;
        let wrapped = vga.render_active_row(0);
        assert_eq!(
            &wrapped[0..64],
            &[0u8; 64],
            "pre-wrap pixels read the cleared tail"
        );
        assert_eq!(
            &wrapped[64..72],
            &top[0..8],
            "wrapped scanout pixels equal the top-of-VRAM pixels at the seam"
        );
    }

    #[test]
    fn line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
        // A distinct byte per plane-0 offset so each source row is recognizable.
        fn pattern(off: usize) -> u8 {
            ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
        }
        // Reference renderer: no split (line compare stays 0x3FF), configurable scroll
        // and pel-pan, rendering one row.
        fn reference(s: u32, pan: u8, row: u32) -> Vec<u8> {
            let mut r = Vga::default();
            r.set_mode(0x12);
            r.attr.palette = core::array::from_fn(|i| i as u8);
            for off in 0..VGA_PLANE_SIZE {
                r.vram[off] = pattern(off);
            }
            r.crtc.start_address = s;
            r.attr.pixel_pan = pan;
            r.render_active_row(row)
        }

        let mut vga = Vga::default();
        vga.set_mode(0x12); // 640x480, not double-scanned, offset 40 (byte pitch 80)
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        for off in 0..VGA_PLANE_SIZE {
            vga.vram[off] = pattern(off);
        }
        let start = 0x1000u32;
        let split = 300u32;
        vga.crtc.start_address = start;
        vga.crtc.line_compare = split;
        vga.attr.pixel_pan = 3;
        vga.attr.mode_control = 0x20; // bit 5: pel-pan up to line compare only

        // Top row 200 (<= split): scrolled by `start`, panned by 3.
        assert_eq!(
            vga.render_active_row(200),
            reference(start, 3, 200),
            "top region renders scrolled and pel-panned"
        );
        // First split scanline (split+1): source row 0 from offset 0, pel-pan forced 0.
        assert_eq!(
            vga.render_active_row(split + 1),
            reference(0, 0, 0),
            "first split line renders source row 0 from offset 0 with pel-pan forced to 0"
        );
        // Split region row k: source row k from offset 0, pel-pan forced 0.
        assert_eq!(
            vga.render_active_row(split + 11),
            reference(0, 0, 10),
            "split region row k renders source row k from offset 0"
        );
    }

    #[test]
    fn line_compare_split_starts_on_the_line_after_the_match() {
        let split = 100u32;
        let mut vga = Vga::default();
        vga.set_mode(0x12);
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0xFF; // offset 0 marked: index 1 across pixels 0..7
        // Scroll the top region past the marked byte so the top reads cleared VRAM.
        vga.crtc.start_address = 0x4000;
        vga.crtc.line_compare = split;
        // The matching scanline is the last top line: reads start_address (clear) -> 0.
        assert_eq!(
            vga.render_active_row(split)[0],
            0,
            "scanline == line_compare is still the top region"
        );
        // The next scanline is the first split line: reads offset 0 (marked) -> 1.
        assert_eq!(
            vga.render_active_row(split + 1)[0],
            1,
            "scanline line_compare+1 is the first split line, from offset 0"
        );
    }

    #[test]
    fn ega_line_compare_split_starts_two_scanlines_later() {
        let split = 100u32;
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x10));
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0xFF; // offset 0 marked: index 1 across pixels 0..7
        vga.crtc.start_address = 0x4000;
        vga.crtc.line_compare = split;

        assert_eq!(vga.render_active_row(split + 1)[0], 0);
        assert_eq!(vga.render_active_row(split + 2)[0], 0);
        assert_eq!(vga.render_active_row(split + 3)[0], 1);
    }

    #[test]
    fn ega_line_compare_compares_against_the_scan_counter_line_in_a_doubled_mode() {
        let mut vga = Vga::default();
        vga.set_mode_0dh(); // double-scanned: 400 active scanlines, source rows 0..200
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0xFF; // offset 0 marked -> index 1
        // Split at scan-counter line 320. The source row counter only reaches ~200, so a
        // split here can only match if the comparison is in scan-counter units.
        let split = 320u32;
        vga.crtc.start_address = 0x4000; // top region reads cleared VRAM
        vga.crtc.line_compare = split;
        assert_eq!(
            vga.render_active_row(320)[0],
            0,
            "scanline 320 == line_compare is the last top line"
        );
        assert_eq!(
            vga.render_active_row(321)[0],
            0,
            "EGA split is delayed two scanlines after the VGA threshold"
        );
        assert_eq!(
            vga.render_active_row(322)[0],
            0,
            "EGA split is still delayed on the second scanline after the match"
        );
        // Scanlines 323 and 324 are the first two split scanlines: the same doubled
        // source row 0, read from offset 0.
        assert_eq!(
            vga.render_active_row(323)[0],
            1,
            "first split scanline, offset 0"
        );
        assert_eq!(
            vga.render_active_row(324)[0],
            1,
            "second scanline holds the same doubled source row 0"
        );
    }

    #[test]
    fn pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
        // Render the first split-region row (offset 0) with a non-uniform byte so a
        // pel-pan shift is visible. `mode_control` carries Attribute index 10h, `pan`
        // the pel-pan value.
        fn render(mode_control: u8, pan: u8) -> Vec<u8> {
            let mut vga = Vga::default();
            vga.set_mode(0x12);
            vga.attr.palette = core::array::from_fn(|i| i as u8);
            vga.vram[0] = 0b0101_0101; // alternating pixels in source row 0
            vga.crtc.line_compare = 100;
            vga.attr.pixel_pan = pan;
            vga.attr.mode_control = mode_control;
            vga.render_active_row(101) // first split line: source row 0, offset 0
        }
        // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
        assert_eq!(
            render(0x20, 1),
            render(0x20, 0),
            "Attribute 10h bit 5 set forces split-region pel-pan to 0"
        );
        // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
        assert_ne!(
            render(0x00, 1),
            render(0x00, 0),
            "Attribute 10h bit 5 clear pans the split region"
        );
    }

    #[test]
    fn wide_mode_assembles_four_bit_index_across_the_full_line() {
        let mut vga = Vga::default();
        vga.set_mode(0x12); // 640 wide, not doubled
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        // Column 639 is byte 79, bit 0. Set that bit in all four planes so the
        // assembled index is 0b1111 = 15.
        for plane in 0..VGA_PLANES {
            vga.vram[plane * VGA_PLANE_SIZE + 79] = 0x01;
        }
        let row = vga.render_active_row(0);
        assert_eq!(row.len(), 640);
        assert_eq!(row[639], 15, "column 639 reads bit 0 of all four planes");
        assert_eq!(row[0], 0, "column 0 is clear");
    }

    #[test]
    fn guest_crtc_bang_retunes_mode_x_to_320x240() {
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // enter mode X, 320x200 base
        assert_eq!(vga.raster_height(), 449);
        // Abrash's 320x240 vertical timing (Black Book Listing 47.1), index then data.
        for (idx, val) in [
            (0x06u8, 0x0Du8), // vertical total
            (0x07, 0x3E),     // overflow (high bits)
            (0x09, 0x41),     // max scan line: 2 scanlines per row
            (0x10, 0xEA),     // vretrace start
            (0x11, 0xAC),     // vretrace end + protect
            (0x12, 0xDF),     // vertical display end
            (0x15, 0xE7),     // vblank start
            (0x16, 0x06),     // vblank end
        ] {
            vga.write_port(0x3D4, idx);
            vga.write_port(0x3D5, val);
        }
        assert_eq!(vga.crtc.vtotal, 527, "527 total scanlines");
        assert_eq!(vga.crtc.vdisp_end, 480, "480 active scanlines");
        assert_eq!(vga.crtc.max_scan, 1);
        assert!(
            vga.crtc.double_scan,
            "double-scanned: 240 source rows over 480 lines"
        );
        assert_eq!(vga.raster_height(), 527);
    }

    #[test]
    fn planar_vertical_crtc_writes_recompute_ega_timing() {
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x10));
        assert_eq!(vga.crtc.vdisp_end, 350);
        assert_eq!(vga.raster_height(), 449);

        vga.write_port(0x3D4, 0x11);
        vga.write_port(0x3D5, 0x00);
        for (idx, val) in [
            (0x06u8, 0x0Du8),
            (0x07, 0x3E),
            (0x09, 0x41),
            (0x10, 0xEA),
            (0x11, 0xAC),
            (0x12, 0xDF),
            (0x15, 0xE7),
            (0x16, 0x06),
        ] {
            vga.write_port(0x3D4, idx);
            vga.write_port(0x3D5, val);
            vga.write_port(0x3D4, idx);
            assert_eq!(vga.read_port(0x3D5), Some(val));
        }

        assert_eq!(vga.crtc.vtotal, 527);
        assert_eq!(vga.crtc.vdisp_end, 480);
        assert_eq!(vga.crtc.vblank_start, 487);
        assert_eq!(vga.crtc.vretrace_start, 490);
        assert_eq!(vga.crtc.max_scan, 1);
        assert!(vga.crtc.double_scan);
        assert_eq!(vga.raster_height(), 527);
    }

    #[test]
    fn mode13h_vertical_crtc_writes_recompute_timing() {
        let mut vga = Vga::default();
        vga.set_mode13h();
        assert_eq!(vga.raster_height(), 449);

        vga.write_port(0x3D4, 0x11);
        vga.write_port(0x3D5, 0x00);
        for (idx, val) in [
            (0x06u8, 0x0Du8),
            (0x07, 0x3E),
            (0x09, 0x41),
            (0x10, 0xEA),
            (0x11, 0xAC),
            (0x12, 0xDF),
            (0x15, 0xE7),
            (0x16, 0x06),
        ] {
            vga.write_port(0x3D4, idx);
            vga.write_port(0x3D5, val);
        }

        assert_eq!(vga.crtc.vtotal, 527);
        assert_eq!(vga.crtc.vdisp_end, 480);
        assert_eq!(vga.crtc.max_scan, 1);
        assert!(vga.crtc.double_scan);
        assert_eq!(vga.raster_height(), 527);
    }

    #[test]
    fn clearing_chain4_in_mode13h_enters_and_leaves_mode_x() {
        let mut vga = Vga::default();
        vga.set_mode13h();
        assert_eq!(vga.active_mode(), VideoMode::Mode13h);
        // Sequencer Memory Mode (04h) written with chain-4 (bit 3) cleared enters
        // unchained 256-color (mode X) from chained mode 13h.
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        assert_eq!(vga.active_mode(), VideoMode::ModeX);
        // The unchained 320x200 base geometry: 320 wide, vtotal 449, offset 40.
        assert_eq!(vga.raster_width(), 320);
        assert_eq!(vga.raster_height(), 449);
        assert_eq!(vga.crtc.offset, 40);
        // Writing 04h with chain-4 set again reverts to chained mode 13h.
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x0E);
        assert_eq!(vga.active_mode(), VideoMode::Mode13h);
    }

    #[test]
    fn mode_x_scanout_is_column_interleaved_8bit_direct() {
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X, 320x200 base
        // Distinct full bytes in planes 0..3 at plane offset 0. 0x40 also proves the
        // byte is not masked to 6 bits (0x40 & 0x3F would be 0).
        vga.vram[0] = 0x10; // plane 0, offset 0
        vga.vram[VGA_PLANE_SIZE] = 0x20;
        vga.vram[2 * VGA_PLANE_SIZE] = 0x30;
        vga.vram[3 * VGA_PLANE_SIZE] = 0x40;
        vga.vram[1] = 0x11; // plane 0, offset 1: pixel 4 must read this
        let row = vga.render_256color_row(0);
        // Pixels 0..3 are planes 0..3 at offset 0, as full 8-bit DAC indices.
        assert_eq!(&row[0..4], &[0x10, 0x20, 0x30, 0x40]);
        assert_eq!(row[4], 0x11, "pixel 4 wraps to plane 0 at plane offset 1");
    }

    #[test]
    fn mode_x_pel_pan_shifts_the_column_origin_by_the_pan_value() {
        // A distinct byte per plane and plane offset so every column is recognizable;
        // values reach above 0x3F, re-proving the 8-bit-direct DAC read.
        fn byte(plane: usize, off: usize) -> u8 {
            ((plane as u32 * 0x11 + off as u32 * 7 + 0x40) & 0xFF) as u8
        }
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X, 320x200 double-scanned base
        for plane in 0..VGA_PLANES {
            for off in 0..VGA_PLANE_SIZE {
                vga.vram[plane * VGA_PLANE_SIZE + off] = byte(plane, off);
            }
        }
        vga.attr.pixel_pan = 0;
        let reference = vga.render_256color_row(0); // top line, no split forcing
        for pan in 1..=3u8 {
            vga.attr.pixel_pan = pan;
            let row = vga.render_256color_row(0);
            for x in 0..(reference.len() - pan as usize) {
                assert_eq!(
                    row[x],
                    reference[x + pan as usize],
                    "pan {pan} shifts the row so column x reads the pan-0 column x+pan"
                );
            }
        }
    }

    #[test]
    fn mode_x_pel_pan_rotates_the_plane_sequence() {
        // Distinct bytes per plane at plane offset 0 (values above 0x3F prove the
        // 8-bit-direct DAC read); other offsets stay cleared.
        let plane0_byte: [u8; VGA_PLANES] = [0x40, 0x50, 0x60, 0x70];
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X
        for (plane, &b) in plane0_byte.iter().enumerate() {
            vga.vram[plane * VGA_PLANE_SIZE] = b;
        }
        // With pan N (0..3), column 0 reads plane N at plane offset 0: the
        // (0,1,2,3) origin rotates to (N, N+1, ...).
        for pan in 0..VGA_PLANES as u8 {
            vga.attr.pixel_pan = pan;
            let row = vga.render_256color_row(0);
            assert_eq!(
                row[0], plane0_byte[pan as usize],
                "pan {pan} rotates column 0 to plane {pan} at plane offset 0"
            );
        }
    }

    #[test]
    fn mode_x_pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
        // Below the CRTC Line Compare split, render the first split row (source row 0
        // at plane offset 0) with distinct bytes per plane so a pel-pan shift is
        // visible. `mode_control` carries Attribute index 10h, `pan` the pel-pan value.
        fn render(mode_control: u8, pan: u8) -> Vec<u8> {
            let mut vga = Vga::default();
            vga.set_mode13h();
            vga.write_port(0x3C4, 0x04);
            vga.write_port(0x3C5, 0x06); // mode X
            let plane0_byte: [u8; VGA_PLANES] = [0x40, 0x50, 0x60, 0x70];
            for (plane, &b) in plane0_byte.iter().enumerate() {
                vga.vram[plane * VGA_PLANE_SIZE] = b;
            }
            vga.crtc.line_compare = 100;
            vga.attr.pixel_pan = pan;
            vga.attr.mode_control = mode_control;
            vga.render_256color_row(101) // first split line: below_split, source row 0, offset 0
        }
        // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
        assert_eq!(
            render(0x20, 1),
            render(0x20, 0),
            "Attribute 10h bit 5 set forces split-region pel-pan to 0"
        );
        // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
        assert_ne!(
            render(0x00, 1),
            render(0x00, 0),
            "Attribute 10h bit 5 clear pans the split region"
        );
    }

    #[test]
    fn mode_x_page_flip_reads_the_selected_page() {
        // Checks render_256color_row's row_base arithmetic directly; the start-address
        // vretrace latch is exercised end to end in the machine test (slice-5 task 5).
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        let page1 = 0x3E80usize; // 16000 plane-bytes: a 320x200 page
        vga.vram[0] = 0xAA; // page 0, plane 0, offset 0
        vga.vram[page1] = 0x55; // page 1, plane 0, offset 0
        assert_eq!(vga.render_256color_row(0)[0], 0xAA, "start 0 reads page 0");
        vga.crtc.start_address = page1 as u32;
        assert_eq!(
            vga.render_256color_row(0)[0],
            0x55,
            "start at page 1 reads page 1"
        );
    }

    #[test]
    fn mode_x_line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
        // A distinct byte per plane offset so each source row is recognizable. The
        // values reach above 0x3F, which also proves mode X reads the full 8-bit DAC
        // index directly (no attribute 6-bit mask).
        fn pattern(off: usize) -> u8 {
            ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
        }
        // Reference renderer with line compare left at the 0x3FF default (disabled):
        // produces a single scrolled row via the mode-X scanout.
        fn reference(start: u32, row: u32) -> Vec<u8> {
            let mut r = Vga::default();
            r.set_mode13h();
            r.write_port(0x3C4, 0x04);
            r.write_port(0x3C5, 0x06); // mode X, 320x200 base, double-scanned
            for plane in 0..VGA_PLANES {
                for off in 0..VGA_PLANE_SIZE {
                    r.vram[plane * VGA_PLANE_SIZE + off] = pattern(off);
                }
            }
            r.crtc.start_address = start;
            r.render_256color_row(row)
        }

        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X, double-scanned: source_row = counter_line / 2
        for plane in 0..VGA_PLANES {
            for off in 0..VGA_PLANE_SIZE {
                vga.vram[plane * VGA_PLANE_SIZE + off] = pattern(off);
            }
        }
        let start = 0x1000u32;
        let split = 300u32;
        vga.crtc.start_address = start;
        vga.crtc.line_compare = split;

        // Top row 200 (<= split): source row 100, scrolled by start.
        assert_eq!(
            vga.render_256color_row(200),
            reference(start, 200),
            "top region renders scrolled by start_address"
        );
        // First split scanline (split + 1): source row 0 from offset 0.
        assert_eq!(
            vga.render_256color_row(split + 1),
            reference(0, 0),
            "first split line renders source row 0 from offset 0"
        );
        // Deeper split scanline: (counter_line - (split + 1)) / 2 = 10, so source
        // row 10 from offset 0 matches the reference's source row 10 (row 20 / 2).
        assert_eq!(
            vga.render_256color_row(split + 21),
            reference(0, 20),
            "split region row 10 renders source row 10 from offset 0"
        );
    }

    #[test]
    fn mode_x_line_compare_split_starts_on_the_line_after_the_match() {
        let split = 100u32;
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X
        vga.vram[0] = 0xFF; // plane 0, offset 0 marked (pixel 0)
        // Scroll the top region past the marked byte so the top reads cleared VRAM.
        vga.crtc.start_address = 0x4000;
        vga.crtc.line_compare = split;
        // The matching scanline is the last top line: reads start_address (clear).
        assert_eq!(
            vga.render_256color_row(split)[0],
            0,
            "scanline == line_compare is still the top region"
        );
        // The next scanline is the first split line: reads offset 0 (marked).
        assert_eq!(
            vga.render_256color_row(split + 1)[0],
            0xFF,
            "scanline line_compare + 1 is the first split line, from offset 0"
        );
    }

    #[test]
    fn mode_x_line_compare_compares_in_scan_counter_units_double_scanned() {
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X
        // Abrash's 320x240 vertical timing (Black Book Listing 47.1): double-scanned,
        // 240 source rows over 480 scanlines. Same bang as the guest-CRTC retune test.
        for (idx, val) in [
            (0x06u8, 0x0Du8),
            (0x07, 0x3E),
            (0x09, 0x41),
            (0x10, 0xEA),
            (0x11, 0xAC),
            (0x12, 0xDF),
            (0x15, 0xE7),
            (0x16, 0x06),
        ] {
            vga.write_port(0x3D4, idx);
            vga.write_port(0x3D5, val);
        }
        vga.vram[0] = 0xFF; // plane 0, offset 0 marked (pixel 0)
        // Split at scan-counter line 400. The source row counter only reaches 240, so
        // a split here can only match if the comparison is in scan-counter units, not
        // divided by the double-scan factor.
        let split = 400u32;
        vga.crtc.start_address = 0x4000; // top region reads cleared VRAM
        vga.crtc.line_compare = split;
        assert_eq!(
            vga.render_256color_row(400)[0],
            0,
            "scanline 400 == line_compare is the last top line"
        );
        // Scanlines 401 and 402 are the first two split scanlines: the same doubled
        // source row 0, read from offset 0.
        assert_eq!(
            vga.render_256color_row(401)[0],
            0xFF,
            "first split scanline, offset 0"
        );
        assert_eq!(
            vga.render_256color_row(402)[0],
            0xFF,
            "second scanline holds the same doubled source row 0"
        );
    }

    #[test]
    fn render_scanline_dispatches_to_the_mode_x_scanout() {
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06);
        vga.vram[0] = 0x7E;
        let raster = vga.render_full_frame();
        assert_eq!(
            raster.pixels[0], 0x7E,
            "row 0 pixel 0 is plane 0 offset 0, 8-bit direct"
        );
    }

    #[test]
    fn mode13h_scanout_is_column_interleaved_8bit_direct() {
        // Chain-4 routes the A0000 byte at offset N to plane N & 3 at plane
        // offset N >> 2, so four writes at offsets 0..3 land one byte per plane
        // at plane offset 0, and the write at offset 4 lands in plane 0 at plane
        // offset 1. The shared scanout then reads pixel x as plane x & 3 at plane
        // offset x >> 2, so the raster carries each written byte as the full 8-bit
        // DAC index (0x40 has bits above 0x3F, proving no 6-bit mask).
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.cpu_write_chain4(0, 0x10); // plane 0, offset 0 -> pixel 0
        vga.cpu_write_chain4(1, 0x20); // plane 1, offset 0 -> pixel 1
        vga.cpu_write_chain4(2, 0x30); // plane 2, offset 0 -> pixel 2
        vga.cpu_write_chain4(3, 0x40); // plane 3, offset 0 -> pixel 3
        vga.cpu_write_chain4(4, 0x11); // plane 0, offset 1 -> pixel 4
        let row = vga.render_256color_row(0);
        assert_eq!(&row[0..4], &[0x10, 0x20, 0x30, 0x40]);
        assert_eq!(row[4], 0x11, "pixel 4 wraps to plane 0 at plane offset 1");
    }

    #[test]
    fn mode13h_pel_pan_shifts_the_column_origin_by_the_pan_value() {
        // A distinct byte per plane and plane offset so every column is
        // recognizable; values reach above 0x3F, re-proving the 8-bit-direct DAC
        // read. Pel-pan is masked to 0-3 (one plane per pel; a pan of 4 equals a
        // start-address bump), so pan 1..3 shifts the row by that many pixels and
        // pan 4 folds to 0.
        fn byte(plane: usize, off: usize) -> u8 {
            ((plane as u32 * 0x11 + off as u32 * 7 + 0x40) & 0xFF) as u8
        }
        let mut vga = Vga::default();
        vga.set_mode13h();
        for plane in 0..VGA_PLANES {
            for off in 0..VGA_PLANE_SIZE {
                vga.vram[plane * VGA_PLANE_SIZE + off] = byte(plane, off);
            }
        }
        vga.crtc.start_address = 0;
        vga.attr.pixel_pan = 0;
        let reference = vga.render_256color_row(0); // top line, no split forcing
        for pan in 1..=3u8 {
            vga.attr.pixel_pan = pan;
            let row = vga.render_256color_row(0);
            for x in 0..(reference.len() - pan as usize) {
                assert_eq!(
                    row[x],
                    reference[x + pan as usize],
                    "pan {pan} shifts the row so column x reads the pan-0 column x+pan"
                );
            }
        }
        // Pel-pan 4 is masked to 0 (& 0x03), so it reproduces the pan-0 row rather
        // than shifting by four pixels.
        vga.attr.pixel_pan = 4;
        assert_eq!(
            vga.render_256color_row(0),
            reference,
            "pan 4 folds to 0 under the 0-3 mask"
        );
        // The four-pixel shift a true pan 4 would perform is reached by bumping the
        // start address by one plane-offset unit instead: start + 1 at pan 0 equals
        // the pan-0 row shifted by four columns. This is the smooth-scroll loop
        // boundary (pan 0->3, then start + 1).
        vga.attr.pixel_pan = 0;
        vga.crtc.start_address = 1;
        let scrolled = vga.render_256color_row(0);
        for x in 0..(reference.len() - 4) {
            assert_eq!(
                scrolled[x],
                reference[x + 4],
                "start + 1 at pan 0 scans out the pan-0 row shifted by four columns"
            );
        }
    }

    #[test]
    fn mode13h_pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
        // Below the CRTC Line Compare split, render the first split row (source row
        // 0 at plane offset 0) with distinct bytes per plane so a pel-pan shift is
        // visible. `mode_control` carries Attribute index 10h, `pan` the pel-pan
        // value.
        fn render(mode_control: u8, pan: u8) -> Vec<u8> {
            let mut vga = Vga::default();
            vga.set_mode13h();
            let plane0_byte: [u8; VGA_PLANES] = [0x40, 0x50, 0x60, 0x70];
            for (plane, &b) in plane0_byte.iter().enumerate() {
                vga.vram[plane * VGA_PLANE_SIZE] = b;
            }
            vga.crtc.line_compare = 100;
            vga.attr.pixel_pan = pan;
            vga.attr.mode_control = mode_control;
            vga.render_256color_row(101) // first split line: below_split, source row 0, offset 0
        }
        // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
        assert_eq!(
            render(0x20, 1),
            render(0x20, 0),
            "Attribute 10h bit 5 set forces split-region pel-pan to 0"
        );
        // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
        assert_ne!(
            render(0x00, 1),
            render(0x00, 0),
            "Attribute 10h bit 5 clear pans the split region"
        );
    }

    #[test]
    fn mode13h_line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
        // A distinct byte per plane offset so each source row is recognizable. The
        // values reach above 0x3F, which also proves mode 13h reads the full 8-bit
        // DAC index directly (no attribute 6-bit mask).
        fn pattern(off: usize) -> u8 {
            ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
        }
        // Reference renderer with line compare left at the 0x3FF default (disabled):
        // produces a single scrolled row via the shared 256-color scanout.
        fn reference(start: u32, row: u32) -> Vec<u8> {
            let mut r = Vga::default();
            r.set_mode13h();
            for plane in 0..VGA_PLANES {
                for off in 0..VGA_PLANE_SIZE {
                    r.vram[plane * VGA_PLANE_SIZE + off] = pattern(off);
                }
            }
            r.crtc.start_address = start;
            r.render_256color_row(row)
        }

        let mut vga = Vga::default();
        vga.set_mode13h(); // 320x200, double-scanned: source_row = counter_line / 2
        for plane in 0..VGA_PLANES {
            for off in 0..VGA_PLANE_SIZE {
                vga.vram[plane * VGA_PLANE_SIZE + off] = pattern(off);
            }
        }
        let start = 0x1000u32;
        let split = 300u32;
        vga.crtc.start_address = start;
        vga.crtc.line_compare = split;

        // Top row 200 (<= split): source row 100, scrolled by start.
        assert_eq!(
            vga.render_256color_row(200),
            reference(start, 200),
            "top region renders scrolled by start_address"
        );
        // First split scanline (split + 1): source row 0 from offset 0.
        assert_eq!(
            vga.render_256color_row(split + 1),
            reference(0, 0),
            "first split line renders source row 0 from offset 0"
        );
        // Deeper split scanline: (counter_line - (split + 1)) / 2 = 10, so source
        // row 10 from offset 0 matches the reference's source row 10 (row 20 / 2).
        assert_eq!(
            vga.render_256color_row(split + 21),
            reference(0, 20),
            "split region row 10 renders source row 10 from offset 0"
        );
    }

    /// Write a character/attribute pair into a text cell (row, col).
    fn text_put(vga: &mut Vga, row: usize, col: usize, ch: u8, attr: u8) {
        let i = row * vga.text_columns + col;
        vga.write_u8(i * 2, ch).unwrap();
        vga.write_u8(i * 2 + 1, attr).unwrap();
    }

    #[test]
    fn text_scanout_renders_cp437_glyph_rows_at_9x16() {
        let mut vga = Vga {
            cursor_start: 0x20,
            ..Default::default()
        };
        // 0xDB is the solid full block (all-ones rows); white on black (0x0F).
        text_put(&mut vga, 0, 0, 0xDB, 0x0F);
        // The mode 03h BIOS ATC palette maps attribute 0x0F to DAC index 0x3F;
        // the pel mask is all-pass, so a clear pixel scans out as 0.
        let top = vga.render_text_row(0); // char row 0, font line 0
        assert_eq!(
            &top[0..9],
            &[BIOS_TEXT_WHITE; 9],
            "all 9 columns of 0xDB are foreground"
        );
        assert_eq!(top[8], top[7], "the 9th column replicates the 8th for 0xDB");
        // The same glyph holds across all 16 scanlines of the character row.
        let bottom = vga.render_text_row(15); // font line 15, still char row 0
        assert_eq!(
            &bottom[0..9],
            &[BIOS_TEXT_WHITE; 9],
            "0xDB stays solid across 16 scanlines"
        );
        // A non-box glyph clears its 9th column to the background. 0xFF is outside
        // 0xC0-0xDF; load it as a full-8-column block via a custom glyph row.
        vga.text_memory[0] = 0xFF;
        let row = vga.render_text_row(0);
        assert_eq!(
            row[8], 0,
            "a glyph outside 0xC0-0xDF blanks the 9th column (inter-char gap)"
        );
    }

    #[test]
    fn text_scanout_maps_attribute_through_the_palette_to_dac() {
        let mut vga = Vga::default();
        // 0xDB lit, foreground nibble = 1, so the pixel color is palette[1].
        text_put(&mut vga, 0, 0, 0xDB, 0x01);
        vga.attr.palette[1] = 0x2A; // map foreground index 1 -> DAC 42
        assert_eq!(
            vga.render_text_row(0)[0],
            0x2A,
            "foreground scans out at the live palette entry"
        );
        // Reprogramming the palette entry changes the scanout.
        vga.attr.palette[1] = 9;
        assert_eq!(
            vga.render_text_row(0)[0],
            9,
            "a changed palette entry reaches the scanout"
        );
    }

    #[test]
    fn text_scanout_blink_toggles_foreground_only_when_enabled() {
        let mut vga = Vga::default();
        // Blink enabled (AC Mode Control 10h bit 3); attribute 0x8F has the blink
        // bit set and a white foreground.
        vga.attr.mode_control = 0x08;
        text_put(&mut vga, 0, 0, 0xDB, 0x8F);
        // Show phase: foreground renders as the BIOS text-white DAC entry.
        vga.frames = 0;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "show phase renders the foreground"
        );
        // Hide phase: the foreground collapses to the background (DAC 0).
        vga.frames = 16;
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "hide phase collapses the foreground to the background"
        );

        // Blink disabled: attribute bit 7 is background intensity, not blink, so
        // the foreground never collapses.
        vga.attr.mode_control = 0x00;
        vga.frames = 0;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "no blink: foreground on show phase"
        );
        vga.frames = 16;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "no blink: foreground stays on the would-be hide phase"
        );
        // And the background now reads bit 7 as intensity (background index 8),
        // then maps it through the mode 03h BIOS ATC palette.
        text_put(&mut vga, 0, 0, b' ', 0x80); // blank glyph, bit-7 background
        assert_eq!(
            vga.render_text_row(0)[0],
            0x38,
            "with blink off, attribute bit 7 selects background intensity 8"
        );
    }

    #[test]
    fn text_scanout_presents_a_720x400_raster() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F);
        let raster = vga.render_full_frame();
        assert_eq!(raster.width, 720, "mode-03h text is 720 dots wide");
        assert_eq!(raster.height, 449, "the full frame is vtotal scanlines");
        // 400 active rows, top-justified: row 0 carries the glyph, row 400 is the
        // border (overscan, default black).
        assert_eq!(
            raster.pixels[0], BIOS_TEXT_WHITE,
            "top-left active pixel is the foreground"
        );
        let border = 400 * 720;
        assert_eq!(
            raster.pixels[border], 0,
            "scanline 400 is the border, not active"
        );
    }

    #[test]
    fn mode03_vgabios_pixel_pan_default_keeps_text_origin_unshifted() {
        let mut vga = Vga::default();
        assert_eq!(vga.attr.pixel_pan, 8, "mode 03h BIOS default AC13");
        text_put(&mut vga, 0, 0, 0xDB, 0x0F);
        text_put(&mut vga, 0, 1, b' ', 0x0F);
        let row = vga.render_text_row(0);
        assert_eq!(row[0], BIOS_TEXT_WHITE);
        assert_eq!(row[8], BIOS_TEXT_WHITE);
        assert_eq!(row[9], 0);

        vga.attr.pixel_pan = 1;
        let row = vga.render_text_row(0);
        assert_eq!(row[0], BIOS_TEXT_WHITE);
        assert_eq!(row[8], 0);
    }

    #[test]
    fn text_40_column_mode_uses_cga_geometry_and_stride() {
        let mut vga = Vga::default();
        vga.set_text_mode_columns(40);
        text_put(&mut vga, 1, 0, 0xDB, 0x0F);

        let frame = vga.frame();
        assert_eq!(frame.columns, 40);
        assert_eq!(frame.rows, 25);
        assert_eq!(frame.cells.len(), 40 * 25);
        assert_eq!(frame.cells[40].character, 0xDB);
        assert_eq!(vga.render_text_row(8)[0], 15);

        let raster = vga.render_full_frame();
        assert_eq!(raster.width, 320);
        assert_eq!(raster.height, 262);
    }

    #[test]
    fn text_cga_80_column_mode_uses_8x8_640x200_geometry() {
        let mut vga = Vga::default();
        vga.set_cga_80_text_mode();
        text_put(&mut vga, 1, 0, 0xDB, 0x0F);

        assert_eq!(vga.cga_mode_control(), 0x2D);
        let frame = vga.frame();
        assert_eq!(frame.columns, 80);
        assert_eq!(frame.rows, 25);
        assert_eq!(frame.cells.len(), 80 * 25);
        assert_eq!(frame.cells[80].character, 0xDB);
        assert_eq!(vga.render_text_row(8)[0], 15);

        let raster = vga.render_full_frame();
        assert_eq!(raster.width, 640);
        assert_eq!(raster.height, 262);
    }

    #[test]
    fn cga_text_mode_03_is_80_column_color_text() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x03));
        text_put(&mut vga, 1, 0, 0xDB, 0x0F);

        assert_eq!(vga.cga_mode_control(), 0x29);
        assert_eq!(vga.frame().columns, 80);
        assert_eq!(vga.render_full_frame().width, 640);
    }

    #[test]
    fn cga_text_start_address_wraps_at_the_16kb_window() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x02));
        text_put(&mut vga, 0, 0, b'A', 0x07);
        vga.crtc.start_address = 0x2000;
        assert_eq!(vga.frame().cells[0].character, b'A');

        vga.set_text_mode();
        text_put(&mut vga, 0, 0, b'A', 0x07);
        vga.text_memory[0x4000] = b'Z';
        vga.text_memory[0x4001] = 0x07;
        vga.crtc.start_address = 0x2000;
        assert_eq!(vga.frame().cells[0].character, b'Z');
    }

    #[test]
    fn cga_text_blink_uses_3d8_bit5_not_vga_attribute_control() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));
        text_put(&mut vga, 0, 0, 0xDB, 0x8F);

        assert_eq!(vga.attr.mode_control & 0x08, 0);
        assert_eq!(vga.render_text_row(0)[0], 15);
        vga.frames = 16;
        assert_eq!(vga.render_text_row(0)[0], 0);

        assert!(vga.write_port(0x3D8, CGA_MODE_VIDEO_ENABLE));
        assert_eq!(vga.render_text_row(0)[0], 15);
    }

    #[test]
    fn cga_text_colors_ignore_vga_attribute_palette_and_pel_mask() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));
        vga.attr.palette[0x0E] = CGA_BLACK;
        vga.attr.palette[0x01] = CGA_BLACK;
        vga.pel_mask = 0x00;
        text_put(&mut vga, 0, 0, 0xDB, 0x1E);
        text_put(&mut vga, 0, 1, b' ', 0x1E);

        let row = vga.render_text_row(0);
        assert_eq!(row[0], CGA_YELLOW);
        assert_eq!(row[8], 1);
    }

    #[test]
    fn cga_text_ignores_vga_sequencer_character_width() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x02));
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        text_put(&mut vga, 0, 1, 0xDB, 0x0F);

        assert!(vga.write_port(0x3C4, 0x01));
        assert!(vga.write_port(0x3C5, 0x00));

        let row = vga.render_text_row(0);
        assert_eq!(row[7], CGA_BLACK);
        assert_eq!(row[8], CGA_WHITE);
    }

    #[test]
    fn cga_text_ignores_vga_attribute_pixel_pan() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x02));
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        text_put(&mut vga, 0, 1, 0xDB, 0x0F);

        vga.read_status1();
        assert!(vga.write_port(0x3C0, 0x20 | 0x13));
        assert!(vga.write_port(0x3C0, 0x07));

        let row = vga.render_text_row(0);
        assert_eq!(row[1], CGA_BLACK);
        assert_eq!(row[8], CGA_WHITE);
    }

    #[test]
    fn cga_text_uses_fixed_rom_font_not_vga_font_maps() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));
        for row in 0..8usize {
            vga.font[0][0xDB * 32 + row] = 0x00;
            vga.font[1][0xDB * 32 + row] = 0x00;
        }
        vga.seq.char_map_select = 0x04; // VGA dual-font state: map A 0, map B 1
        text_put(&mut vga, 0, 0, 0xDB, 0x08);

        assert_eq!(vga.render_text_row(0)[0], 8);
    }

    #[test]
    fn cga_graphics_text_uses_fixed_rom_font_not_vga_font_maps() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_mode(0x04));
        vga.load_font_table(0, 0xDB, 8, &[0; 8]);

        assert_eq!(vga.active_font_glyph_row(0xDB, 0), 0xFF);
    }

    #[test]
    fn cga_text_border_uses_color_select_register() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));

        vga.write_port(0x3D9, 0x05);
        let raster = vga.render_full_frame();
        let border = (raster.height as usize - 1) * raster.width as usize;
        assert_eq!(raster.pixels[border], 5);

        vga.set_overscan(0x0A);
        assert_eq!(vga.cga_color_select(), 0x0A);
        let raster = vga.render_full_frame();
        let border = (raster.height as usize - 1) * raster.width as usize;
        assert_eq!(raster.pixels[border], 10);
    }

    #[test]
    fn cga_video_disable_blanks_the_border() {
        let mut vga = Vga::default();
        assert!(vga.set_cga_text_mode(0x01));
        assert!(vga.write_port(0x3D9, 0x05));
        assert!(vga.write_port(0x3D8, CGA_MODE_BLINK));

        let raster = vga.render_full_frame();
        let border = (raster.height as usize - 1) * raster.width as usize;
        assert_eq!(raster.pixels[border], CGA_BLACK);

        assert!(vga.set_cga_mode(0x04));
        assert!(vga.write_port(0x3D9, 0x05));
        assert!(vga.write_port(0x3D8, CGA_MODE_GRAPHICS));

        let raster = vga.render_full_frame();
        let border = (raster.height as usize - 1) * raster.width as usize;
        assert_eq!(raster.pixels[border], CGA_BLACK);
    }

    #[test]
    fn font_store_is_writable_per_table() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0x41, 0x0F); // 'A', white on black
        // Make table 0's 'A' blank and table 1's 'A' solid across the glyph rows.
        for row in 0..16usize {
            vga.font[0][0x41 * 32 + row] = 0x00;
            vga.font[1][0x41 * 32 + row] = 0xFF;
        }
        // Table 0 (default): the glyph is blank, so the pixel is the background.
        assert_eq!(vga.active_font_table(), 0);
        assert_eq!(vga.render_text_row(0)[0], 0, "table 0 'A' is blank");
        // Selecting table 1 shows its own solid glyph. Set map B = table 1 too so
        // the cell stays in 256-glyph mode (map A == map B); otherwise the two
        // distinct maps would engage 512-glyph mode and consume attr bit 3.
        vga.seq.char_map_select = 0x01 | 0x04; // map-A bit 0, map-B bit 2 -> table 1
        assert_eq!(vga.active_font_table(), 1);
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "table 1 'A' is solid -> foreground"
        );
    }

    #[test]
    fn sequencer_char_map_select_picks_the_active_font() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0x41, 0x0F);
        // Table 4 is selected by map-A bit 2 (Sequencer index 3 bit 4).
        for row in 0..16usize {
            vga.font[0][0x41 * 32 + row] = 0x00;
            vga.font[4][0x41 * 32 + row] = 0xFF;
        }
        // Writing the Sequencer Character Map Select (index 3) through the port
        // switches the active table.
        vga.write_port(0x3C4, 0x03);
        vga.write_port(0x3C5, 0x00); // SR3 = 0 -> table 0 (blank)
        assert_eq!(vga.active_font_table(), 0);
        assert_eq!(vga.render_text_row(0)[0], 0);
        vga.write_port(0x3C4, 0x03);
        // SR3 = 0x30: map-A bit 4 (table 4) and map-B bit 5 (table 4), so the cell
        // stays 256-glyph (map A == map B) and does not consume attr bit 3.
        vga.write_port(0x3C5, 0x10 | 0x20); // -> table 4 (solid)
        assert_eq!(vga.active_font_table(), 4);
        assert_eq!(vga.render_text_row(0)[0], BIOS_TEXT_WHITE);
    }

    #[test]
    fn text_cursor_renders_reverse_video_on_the_cursor_cell() {
        let mut vga = Vga::default();
        // Two blank cells, white on black (0x0F); the cursor sits on cell (0,0).
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        text_put(&mut vga, 0, 1, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x00; // full block: scanlines 0..15
        vga.cursor_end = 0x0F;
        vga.frames = 0; // show phase
        let row = vga.render_text_row(0);
        // Reverse video on a blank cell swaps the background (where the blank
        // glyph reads) to the foreground, so the cursor cell is solid fg.
        assert_eq!(
            row[0], BIOS_TEXT_WHITE,
            "cursor cell scans out as the foreground (reverse video on a blank)"
        );
        // The neighbouring blank cell is not the cursor, so it stays the
        // background (0).
        assert_eq!(
            row[9], 0,
            "a non-cursor blank cell scans out as the background"
        );
    }

    #[test]
    fn text_cursor_respects_start_and_end_scanlines() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x0E; // scanlines 14..15
        vga.cursor_end = 0x0F;
        vga.frames = 0;
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "scanline 0 is outside [14,15]: no swap"
        );
        assert_eq!(
            vga.render_text_row(14)[0],
            BIOS_TEXT_WHITE,
            "scanline 14 swaps"
        );
        assert_eq!(
            vga.render_text_row(15)[0],
            BIOS_TEXT_WHITE,
            "scanline 15 swaps"
        );
    }

    #[test]
    fn text_cursor_disable_bit_hides_it() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x20; // bit 5 set: cursor off (start line 0 ignored)
        vga.cursor_end = 0x0F;
        vga.frames = 0;
        for line in [0u32, 7, 15] {
            assert_eq!(
                vga.render_text_row(line)[0],
                0,
                "disable bit: no swap on any scanline"
            );
        }
    }

    #[test]
    fn text_cursor_blinks_on_the_frame_phase() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x00;
        vga.cursor_end = 0x0F;
        vga.frames = 0; // show phase: cursor visible
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "show phase: cursor swaps"
        );
        vga.frames = 16; // hide phase: cursor hidden
        assert_eq!(vga.render_text_row(0)[0], 0, "hide phase: no swap");
    }

    #[test]
    fn text_cursor_wrap_shape_covers_two_regions() {
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x0E; // start line 14
        vga.cursor_end = 0x01; // end line 1: start > end wraps to two regions
        vga.frames = 0;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "wrap: scanline 0 swaps"
        );
        assert_eq!(
            vga.render_text_row(1)[0],
            BIOS_TEXT_WHITE,
            "wrap: scanline 1 swaps"
        );
        assert_eq!(vga.render_text_row(7)[0], 0, "wrap: scanline 7 does not");
        assert_eq!(
            vga.render_text_row(14)[0],
            BIOS_TEXT_WHITE,
            "wrap: scanline 14 swaps"
        );
        assert_eq!(
            vga.render_text_row(15)[0],
            BIOS_TEXT_WHITE,
            "wrap: scanline 15 swaps"
        );
    }

    #[test]
    fn text_start_address_scrolls_the_display_origin() {
        // The 32 KB aperture holds eight 4096-byte pages. Page 1 starts at cell
        // 0x800 (byte 4096). Scrolling the start address down one page moves the
        // displayed cell (0,0) to read the glyph written on page 1.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F); // page 0 cell 0: solid block
        // Page 1 cell 0 = cell index 0x800 = byte 0x1000.
        let page1_cell0 = 0x800usize;
        vga.write_u8(page1_cell0 * 2, b' ').unwrap(); // blank glyph, distinct from 0xDB
        vga.write_u8(page1_cell0 * 2 + 1, 0x0F).unwrap();
        // Start address is a cell/word address (byte offset = start * 2), so the
        // BIOS page-flip value page * 0x800 maps straight onto it.
        vga.crtc.start_address = 0x800;
        // With the origin scrolled to page 1, cell (0,0) reads the blank glyph
        // there, so the top-left pixel is the background (0), not the solid
        // block foreground that page 0 holds.
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "origin scrolled to page 1 reads page 1's blank glyph"
        );
        // Scrolling back to page 0 restores the solid block.
        vga.crtc.start_address = 0;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "origin back at page 0 reads page 0's solid block"
        );
    }

    #[test]
    fn text_start_address_below_the_split_starts_from_zero() {
        // Line Compare reloads the display address to 0 at and below the split
        // line, so a scrolled start address affects only the top region; the
        // bottom region always starts from offset 0 (FreeVGA crtcreg.htm 18h).
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F); // offset 0: solid block (foreground)
        vga.crtc.start_address = 0x800; // scroll the top region to page 1 (blank)
        vga.crtc.line_compare = 7; // split after char row 0 (8 scanlines, 0..7)
        // Top region (scanline 0..=7): origin scrolled to page 1 -> background.
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "top region reads the scrolled (blank) origin"
        );
        // First split line: address reloads to 0, so the solid block at offset 0
        // is shown again.
        assert_eq!(
            vga.render_text_row(8)[0],
            BIOS_TEXT_WHITE,
            "below-split region starts from offset 0 (solid block)"
        );
    }

    #[test]
    fn text_memory_aperture_is_32kb_eight_pages() {
        // Growing VGA_TEXT_MEMORY_SIZE to 32768 lets the B8000 aperture reach all
        // eight 4096-byte pages. Each page's last cell (row 24, col 79 = cell
        // 1999 within the page) must be writable through the bus read/write path
        // and stay within bounds.
        let mut vga = Vga::default();
        let page7_last_cell = 0x800 * 7 + 1999; // page 7, last visible cell
        let byte = page7_last_cell * 2;
        assert!(
            byte < VGA_TEXT_MEMORY_SIZE,
            "page 7 last cell is inside the 32 KB aperture"
        );
        vga.write_u8(byte, 0xDB).unwrap();
        vga.write_u8(byte + 1, 0x0F).unwrap();
        assert_eq!(
            vga.read_u8(byte).unwrap(),
            0xDB,
            "writable byte round-trips"
        );
        assert_eq!(
            vga.read_u8(VGA_TEXT_MEMORY_SIZE - 1).unwrap_or(0xFF),
            0x07,
            "the very last byte of the 32 KB aperture is reachable"
        );
    }

    #[test]
    fn frame_cell_view_follows_the_start_address() {
        // The headless cell view (frame) reads the visible page from the
        // start-address origin, matching the pixel scanout. Scrolling to page 1
        // makes frame() report page 1's cell (0,0), not page 0's.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, b'A', 0x07); // page 0 cell 0 = 'A'
        let page1_cell0 = 0x800usize;
        vga.write_u8(page1_cell0 * 2, b'Z').unwrap(); // page 1 cell 0 = 'Z'
        vga.write_u8(page1_cell0 * 2 + 1, 0x07).unwrap();
        assert_eq!(
            vga.frame().cells[0].character,
            b'A',
            "page 0 visible by default"
        );
        vga.crtc.start_address = 0x800;
        assert_eq!(
            vga.frame().cells[0].character,
            b'Z',
            "page 1 visible after scrolling the origin"
        );
        assert_eq!(
            vga.frame().cells.len(),
            VGA_TEXT_COLUMNS * VGA_TEXT_ROWS,
            "frame reports exactly one visible 80x25 page"
        );
    }

    #[test]
    fn text_pel_pan_shifts_the_column_origin() {
        // AC 13h (pixel panning) shifts the whole text row left by `pan` pels.
        // With 0xDB (solid box) in cell 0 and blanks after, a pan of 1 moves the
        // lit/blank boundary one pel left: output[8] goes from cell 0's 9th column
        // (lit) to cell 1's first pel (blank).
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F); // cell 0: solid, 9 lit pels
        vga.attr.pixel_pan = 0;
        assert_eq!(
            vga.render_text_row(0)[8],
            BIOS_TEXT_WHITE,
            "pan=0: cell 0's 9th column is lit at output[8]"
        );
        vga.attr.pixel_pan = 1;
        let row = vga.render_text_row(0);
        assert_eq!(
            row[0], BIOS_TEXT_WHITE,
            "pan=1: cell 0 still leads the row (its pel 1 now at output[0])"
        );
        assert_eq!(
            row[8], 0,
            "pan=1: the column origin shifted left by one pel, so output[8] reads cell 1's blank"
        );
    }

    #[test]
    fn text_pel_pan_below_split_forces_zero_when_enabled() {
        // AC 10h bit 5 ("pixel panning mode") forces pel-pan to 0 below the line
        // compare split (FreeVGA crtcreg.htm 18h), so the bottom region is not
        // panned even when 13h is non-zero. Above the split the pan applies.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F);
        vga.attr.pixel_pan = 1;
        vga.attr.mode_control |= 0x20; // bit 5: force pan to 0 below the split
        vga.crtc.line_compare = 7; // split after char row 0 (scanlines 0..7 above)
        // Above the split: pan=1 shifts, so output[8] is cell 1's blank (0).
        assert_eq!(
            vga.render_text_row(0)[8],
            0,
            "above the split the pel-pan applies"
        );
        // Below the split (origin reloads to 0, char row 0): pan forced to 0, so
        // cell 0's 9th column is lit at output[8].
        assert_eq!(
            vga.render_text_row(8)[8],
            BIOS_TEXT_WHITE,
            "below the split AC 10h bit 5 forces pel-pan to 0"
        );
    }

    #[test]
    fn text_pel_pan_9dot_replicates_the_shifted_box_glyph() {
        // A 9-dot box glyph's 9th column replicates the 8th; when panned, that
        // replicate must shift with the cell. Compare a box glyph (0xDB) against a
        // non-box glyph with the same 8 solid pels: at pan=1 the shifted 9th
        // column lands at output[7], lit for the box (replicate) and a gap (0) for
        // the non-box.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F); // box glyph: 8 solid pels + replicated 9th
        vga.attr.pixel_pan = 1;
        assert_eq!(
            vga.render_text_row(0)[7],
            BIOS_TEXT_WHITE,
            "0xDB's replicated 9th column shifts into output[7] and stays lit"
        );
        // Replace cell 0 with a non-box glyph that is solid in pels 0..7 (0xFF) but
        // outside the 0xC0-0xDF box range, so its 9th column is the background.
        // Char 0x01's font slot starts at byte 0x01 * 32 = 32.
        for row in 0..16usize {
            vga.font[0][32 + row] = 0xFF;
        }
        text_put(&mut vga, 0, 0, 0x01, 0x0F);
        assert_eq!(
            vga.render_text_row(0)[7],
            0,
            "non-box glyph's shifted 9th column is a gap, not a replicate"
        );
    }

    #[test]
    fn text_preset_row_scan_offsets_the_first_font_line() {
        // CRTC 08h bits 4-0 (preset row scan) scroll the display up within the
        // character row, so the first displayed scanline reads a later font line.
        // Load a glyph that is solid only on font line 0; a preset of 1 moves the
        // solid line off the top scanline.
        let mut vga = Vga::default();
        let ch = 0x01usize; // char 0x01: font line 0 solid, lines 1..15 clear
        vga.font[0][ch * 32] = 0xFF;
        text_put(&mut vga, 0, 0, 0x01, 0x0F);
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "preset 0: font line 0 is the first displayed scanline (solid)"
        );
        vga.crtc.preset_row_scan = 0x01; // scroll up one scanline
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "preset 1: first displayed scanline reads font line 1 (clear)"
        );
    }

    #[test]
    fn text_byte_pan_shifts_whole_cells() {
        // CRTC 08h bits 6-5 (byte pan) add a byte offset to the start address. In
        // 9-dot text (2 bytes per cell) a byte pan of 2 shifts one whole cell.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x0F); // cell 0: solid (pel 0 lit)
        text_put(&mut vga, 0, 1, b' ', 0x0F); // cell 1: blank (pel 0 bg)
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "byte pan 0: pel 0 reads cell 0 (solid)"
        );
        vga.crtc.preset_row_scan = 0x02 << 5; // byte pan 2 (bits 6-5 = 10)
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "byte pan 2: pel 0 reads cell 1 (blank), one whole cell shifted"
        );
    }

    #[test]
    fn text_preset_row_resets_below_the_split() {
        // Below the line-compare split the preset row scan resets to 0 (FreeVGA
        // crtcreg.htm 18h), so the vertical sub-row scroll applies only to the top
        // region. The same glyph (solid on font line 0) shows the clear line above
        // the split and the solid line below it.
        let mut vga = Vga::default();
        let ch = 0x01usize; // char 0x01: font line 0 solid, rest clear
        vga.font[0][ch * 32] = 0xFF;
        text_put(&mut vga, 0, 0, 0x01, 0x0F);
        text_put(&mut vga, 1, 0, 0x01, 0x0F); // row 1 for the below-split region
        vga.crtc.preset_row_scan = 0x01; // preset 1
        vga.crtc.line_compare = 15; // split after the first 16-scanline char row
        // Top region (scanline 0): preset applies, so pel 0 reads font line 1 (clear).
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "top region: preset row scan offsets the font line"
        );
        // Below-split region (scanline 16, char row 0, font line 0): preset reset
        // to 0, so pel 0 reads font line 0 (solid).
        assert_eq!(
            vga.render_text_row(16)[0],
            BIOS_TEXT_WHITE,
            "below-split region: preset row scan resets to 0 (font line 0 solid)"
        );
    }

    #[test]
    fn char_map_b_decode_picks_the_second_font_table() {
        // The Sequencer Character Map Select map-B field (bits 2, 3, 5) decodes to
        // a table index with the same shape as map A. Verify each bit and the
        // composite against active_font_table_b.
        let mut vga = Vga::default();
        vga.seq.char_map_select = 0x04; // map-B bit 0 (SR3 bit 2) -> table 1
        assert_eq!(vga.active_font_table_b(), 1);
        vga.seq.char_map_select = 0x08; // map-B bit 1 (SR3 bit 3) -> table 2
        assert_eq!(vga.active_font_table_b(), 2);
        vga.seq.char_map_select = 0x20; // map-B bit 2 (SR3 bit 5) -> table 4
        assert_eq!(vga.active_font_table_b(), 4);
        vga.seq.char_map_select = 0x2C; // all three map-B bits -> table 7
        assert_eq!(vga.active_font_table_b(), 7);
    }

    #[test]
    fn attribute_bit_3_selects_the_font_in_512_char_mode() {
        // With two distinct font tables selected (map A != map B), attribute bit 3
        // picks the font per cell: set -> map B, clear -> map A. Load table 0's
        // glyph blank and table 1's solid, select map A = 0 / map B = 1.
        let mut vga = Vga::default();
        let ch = 0x41usize;
        for row in 0..16usize {
            vga.font[0][ch * 32 + row] = 0x00; // table 0: blank
            vga.font[1][ch * 32 + row] = 0xFF; // table 1: solid
        }
        text_put(&mut vga, 0, 0, 0x41, 0x07); // bit 3 clear -> map A (blank)
        // map A = table 0 (SR3 bit 0 clear), map B = table 1 (SR3 bit 2 set).
        vga.seq.char_map_select = 0x04; // map A 0, map B 1 -> dual-font active
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "bit 3 clear: map A glyph (table 0, blank)"
        );
        // Set bit 3 -> map B (table 1, solid). fg is masked to 8 colors now, so the
        // solid glyph reads palette[attr & 0x07] = palette[7] = 7 (not 15).
        text_put(&mut vga, 0, 0, 0x41, 0x0F); // bit 3 set -> map B
        assert_eq!(
            vga.render_text_row(0)[0],
            7,
            "bit 3 set: map B glyph (table 1, solid); fg masked to 8 colors"
        );
    }

    #[test]
    fn int10_11h_loads_two_fonts_for_512_char_text() {
        // Loading two fonts into distinct tables and selecting them via the
        // Character Map Select engages 512-glyph mode end-to-end. This mirrors the
        // AH=11h font-load path: load_font_table into table 0 and table 1, then
        // set_char_map_select so map A = 0 and map B = 1.
        let mut vga = Vga::default();
        let ch = 0x42usize; // 'B'
        // Table 0: 'B' blank; table 1: 'B' solid (two glyphs).
        let blank = vec![0x00u8; 16];
        let solid = vec![0xFFu8; 16];
        vga.load_font_table(0, ch as u16, 16, &blank);
        vga.load_font_table(1, ch as u16, 16, &solid);
        // Map A = 0, map B = 1 (SR3 bit 2 set for map B value 1).
        vga.set_char_map_select(0x04);
        text_put(&mut vga, 0, 0, 0x42, 0x07); // bit 3 clear -> map A (blank)
        assert_eq!(vga.render_text_row(0)[0], 0, "map A 'B' is blank");
        text_put(&mut vga, 0, 0, 0x42, 0x0F); // bit 3 set -> map B (solid)
        assert_eq!(
            vga.render_text_row(0)[0],
            7,
            "map B 'B' is solid (fg masked to 8 colors in 512-char mode)"
        );
    }

    #[test]
    fn text_cursor_skew_delays_the_cursor_onset() {
        // The Cursor Skew (0Bh bits 6-5) delays the cursor onset by that many
        // character clocks, so the cursor appears `skew` cells to the right of the
        // cursor location. With cursor_offset 0 and skew 1, the cursor fires on
        // cell 1 instead of cell 0.
        let mut vga = Vga::default();
        // Two blank cells; cursor configured as a full block on scanline 0.
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        text_put(&mut vga, 0, 1, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x00; // full block
        vga.cursor_end = 0x0F | (0x01 << 5); // end line 15 + skew 1
        vga.frames = 0; // show phase
        let row = vga.render_text_row(0);
        // Cell 0 (pels 0..8): not the skewed cursor (it moved to cell 1).
        assert_eq!(row[0], 0, "skew 1: cell 0 is not the cursor");
        // Cell 1 (pel 9 onward): the cursor, swapped to foreground.
        assert_eq!(row[9], BIOS_TEXT_WHITE, "skew 1: cursor delayed to cell 1");
    }

    #[test]
    fn text_cursor_skew_three_is_max_delay_not_disabled() {
        // Per A5, a skew of 3 is the maximum delay (3 char clocks), not a disable.
        // The disable is the separate 0Ah bit 5. With cursor_offset 0 and skew 3,
        // the cursor fires on cell 3.
        let mut vga = Vga::default();
        for col in 0..5 {
            text_put(&mut vga, 0, col, b' ', 0x0F);
        }
        vga.cursor_offset = 0;
        vga.cursor_start = 0x00; // full block, not disabled (bit 5 clear)
        vga.cursor_end = 0x0F | (0x03 << 5); // end line 15 + skew 3
        vga.frames = 0; // show phase
        let row = vga.render_text_row(0);
        assert_eq!(row[0], 0, "skew 3: cell 0 not the cursor");
        assert_eq!(
            row[3 * 9],
            BIOS_TEXT_WHITE,
            "skew 3: cursor delayed to cell 3 (max delay, not disabled)"
        );
    }

    #[test]
    fn attribute_blink_runs_at_the_hardware_cadence() {
        // The attribute blink hides the foreground for 16 frames, then shows it for
        // 16 (period 32), driven by the vertical-retrace frame counter. A blink
        // attribute cell toggles at that cadence; a non-blink cell never toggles.
        let mut vga = Vga::default();
        vga.attr.mode_control = 0x08; // blink enabled
        text_put(&mut vga, 0, 0, 0xDB, 0x8F); // blink bit set, white fg
        // Frames 0..15: show phase (fg visible).
        for f in [0u64, 1, 7, 15] {
            vga.frames = f;
            assert_eq!(
                vga.render_text_row(0)[0],
                BIOS_TEXT_WHITE,
                "frame {f}: show phase, foreground visible"
            );
        }
        // Frames 16..31: hide phase (fg collapses to bg).
        for f in [16u64, 17, 24, 31] {
            vga.frames = f;
            assert_eq!(
                vga.render_text_row(0)[0],
                0,
                "frame {f}: hide phase, foreground collapsed"
            );
        }
        // Frame 32: the period repeats, back to show.
        vga.frames = 32;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "frame 32: period repeats (show)"
        );
    }

    #[test]
    fn text_cursor_blinks_at_the_hardware_cadence() {
        // The hardware cursor blinks on the same 16-on/16-off cadence as the
        // attribute blink, sharing the one frame-counter phase. The cursor is
        // visible on frames 0..15 and hidden on 16..31, period 32.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, b' ', 0x0F);
        vga.cursor_offset = 0;
        vga.cursor_start = 0x00; // full block
        vga.cursor_end = 0x0F;
        for f in [0u64, 5, 15] {
            vga.frames = f;
            assert_eq!(
                vga.render_text_row(0)[0],
                BIOS_TEXT_WHITE,
                "frame {f}: cursor visible (show phase)"
            );
        }
        for f in [16u64, 20, 31] {
            vga.frames = f;
            assert_eq!(
                vga.render_text_row(0)[0],
                0,
                "frame {f}: cursor hidden (hide phase)"
            );
        }
        vga.frames = 32;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "frame 32: period repeats"
        );
    }

    #[test]
    fn sequencer_reset_and_screen_off_blank_output_and_status() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0x80; // plane 0, pixel 0 -> index 1 when output is enabled.

        assert_eq!(vga.seq.reset, 0x03);
        assert_eq!(vga.render_full_frame().pixels[0], 1);
        assert_eq!(vga.read_status1() & 0x01, 0);

        vga.write_port(0x3C4, 0x00);
        vga.write_port(0x3C5, 0x02); // asynchronous reset asserted (bit 0 clear)
        assert_eq!(vga.seq.reset, 0x02);
        assert_eq!(vga.render_full_frame().pixels[0], 0);
        assert_eq!(vga.read_status1() & 0x01, 0x01);

        vga.write_port(0x3C5, 0x03); // both reset bits (index 0 still selected)
        assert_eq!(vga.seq.reset, 0x03);
        assert_eq!(vga.render_full_frame().pixels[0], 1);

        vga.write_port(0x3C4, 0x01);
        vga.write_port(0x3C5, 0x20); // Clocking Mode bit 5: screen off.
        assert_eq!(vga.render_full_frame().pixels[0], 0);
        assert_eq!(vga.read_status1() & 0x01, 0x01);
    }

    #[test]
    fn display_refresh_control_blanks_output_and_status() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0x80; // plane 0, pixel 0 -> index 1 when refresh is enabled.

        assert!(vga.display_refresh_enabled());
        assert_eq!(vga.render_full_frame().pixels[0], 1);
        assert_eq!(vga.read_status1() & 0x01, 0);

        vga.set_display_refresh_enabled(false);
        assert_eq!(vga.render_full_frame().pixels[0], 0);
        assert_eq!(vga.read_status1() & 0x01, 0x01);

        vga.set_display_refresh_enabled(true);
        assert_eq!(vga.render_full_frame().pixels[0], 1);
    }

    #[test]
    fn input_status0_reports_misc_selected_switch_sense_and_retrace() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        let htotal = htotal_dots(&vga.crtc);
        vga.beam = htotal * (vga.crtc.vdisp_end as u64); // active off, not in retrace
        for (select, expected) in [(0, 0x00), (1, 0x10), (2, 0x10), (3, 0x00)] {
            assert!(vga.write_port(0x3C2, (select << 2) | 0x03));
            assert_eq!(
                vga.read_port(0x3C2).unwrap() & 0x10,
                expected,
                "bit 4 reports colour-display sense bit {select}"
            );
        }
        assert_eq!(
            vga.read_port(0x3C2).unwrap() & 0x80,
            0x00,
            "bit 7 clear outside vertical retrace"
        );
        // Park the beam in vertical retrace: bit 7 (CRT interrupt status) sets.
        vga.beam = htotal * (vga.crtc.vretrace_start as u64);
        let retrace = vga.read_port(0x3C2).unwrap();
        assert_eq!(retrace & 0x80, 0x80, "bit 7 set during vertical retrace");
    }

    #[test]
    fn color_select_folds_into_the_dac_index_when_bit7_clear() {
        // AC Mode Control 10h bit 7 clear: the full 6-bit palette value is DAC bits 5-0,
        // and Color Select 14h bits 3-2 supply DAC bits 7-6.
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
            *b = 0xFF; // every pixel is attribute index 1
        }
        vga.attr.palette[1] = 0x05; // 6-bit palette value 0b00_0101
        vga.attr.mode_control = 0x00; // bit 7 clear
        vga.attr.color_select = 0x0F; // bits 3-2 = 11 -> DAC bits 7-6
        // DAC = 0b11_00_0101 = 0xC5 (palette bits 5-4 untouched).
        assert_eq!(vga.render_active_row(0)[0], 0xC5);
        // Color Select 0 leaves the bare 6-bit palette value.
        vga.attr.color_select = 0x00;
        assert_eq!(vga.render_active_row(0)[0], 0x05);
    }

    #[test]
    fn color_select_replaces_palette_bits_5_4_when_bit7_set() {
        // AC Mode Control 10h bit 7 set: palette bits 5-4 are replaced by Color
        // Select bits 1-0, and Color Select bits 3-2 supply DAC bits 7-6.
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
            *b = 0xFF;
        }
        vga.attr.palette[1] = 0x3A; // 0b11_1010; bits 5-4 (0b11) get replaced
        vga.attr.mode_control = 0x80; // bit 7 set
        vga.attr.color_select = 0x06; // bits 1-0 = 10 -> P5/P4; bits 3-2 = 01 -> DAC 7-6
        // DAC = bits 7-6 (01) | bits 5-4 (10) | palette bits 3-0 (1010) = 0b01_10_1010 = 0x6A.
        assert_eq!(vga.render_active_row(0)[0], 0x6A);
    }

    #[test]
    fn color_select_folds_into_text_foreground() {
        // The text path routes the same fold: a foreground palette value picks up
        // the Color Select high bits.
        let mut vga = Vga::default();
        text_put(&mut vga, 0, 0, 0xDB, 0x01); // solid glyph, fg index 1
        vga.attr.palette[1] = 0x01;
        vga.attr.mode_control = 0x00; // bit 7 clear (and blink off)
        vga.attr.color_select = 0x0C; // bits 3-2 -> DAC 7-6
        // DAC = 0b11_00_0001 = 0xC1.
        assert_eq!(vga.render_text_row(0)[0], 0xC1);
    }

    #[test]
    fn color_plane_enable_masks_vga_text_and_planar_indexes() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        for plane in 0..VGA_PLANES {
            vga.vram[plane * VGA_PLANE_SIZE] = 0x80;
        }
        vga.attr.plane_enable = 0x05;
        assert_eq!(vga.render_active_row(0)[0], 0x05);
        vga.attr.plane_enable = 0x00;
        assert_eq!(vga.render_active_row(0)[0], 0x00);

        let mut text = Vga::default();
        text_put(&mut text, 0, 0, 0xDB, 0x0F);
        text.attr.plane_enable = 0x05;
        assert_eq!(text.render_text_row(0)[0], 0x05);
        text.attr.plane_enable = 0x00;
        assert_eq!(text.render_text_row(0)[0], 0x00);
    }

    #[test]
    fn feature_control_round_trips_3ca_with_color_and_mono_writes() {
        let mut vga = Vga::default();
        assert_eq!(vga.read_port(0x3CA), Some(0x00), "powers up at 0");
        assert!(vga.write_port(0x3DA, 0x0A)); // colour write address
        assert_eq!(vga.read_port(0x3CA), Some(0x0A));
        assert!(vga.write_port(0x3C2, vga.misc_output & !0x01));
        assert!(vga.write_port(0x3BA, 0x05)); // mono alias of the same register
        assert_eq!(vga.read_port(0x3CA), Some(0x05));
    }

    #[test]
    fn video_subsystem_enable_round_trips_3c3() {
        let mut vga = Vga::default();
        assert_eq!(vga.read_port(0x3C3), Some(0x01), "powers up enabled");
        assert!(vga.write_port(0x3C3, 0x00));
        assert_eq!(vga.read_port(0x3C3), Some(0x00));
        // Only bit 0 is stored.
        assert!(vga.write_port(0x3C3, 0xFF));
        assert_eq!(vga.read_port(0x3C3), Some(0x01));
    }

    #[test]
    fn dac_state_reports_the_armed_access_mode() {
        let mut vga = Vga::default();
        // Powers up armed for a write (3C8 path): state 0b00.
        assert_eq!(vga.read_port(0x3C7), Some(0x00));
        // A read-index write (3C7) arms a read: state 0b11.
        assert!(vga.write_port(0x3C7, 5));
        assert_eq!(vga.read_port(0x3C7), Some(0x03));
        let _ = vga.read_port(0x3C9);
        assert_eq!(vga.read_port(0x3C7), Some(0x03));
        // A write-index write (3C8) arms a write again: state 0b00.
        assert!(vga.write_port(0x3C8, 7));
        assert_eq!(vga.read_port(0x3C7), Some(0x00));
        assert!(vga.write_port(0x3C9, 0x2A));
        assert_eq!(vga.read_port(0x3C7), Some(0x00));
    }

    #[test]
    fn set_mode_installs_the_two_color_640_modes_0f_and_11() {
        let mut vga = Vga::default();
        // 0Fh shares 10h's 640x350 timing.
        assert!(vga.set_mode(0x0F));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.crtc.vdisp_end, 350);
        assert_eq!(vga.active_mode(), VideoMode::Planar);
        assert_eq!(vga.seq.map_mask, 0x0F);
        assert_eq!(vga.misc_output & 0xC1, 0x80);
        assert_eq!(CrtcTiming::mode_0fh(), CrtcTiming::mode_10h());
        // 11h shares 12h's 640x480 timing.
        assert!(vga.set_mode(0x11));
        assert_eq!(vga.raster_width(), 640);
        assert_eq!(vga.crtc.vdisp_end, 480);
        assert_eq!(vga.seq.map_mask, 0x0F);
        assert_eq!(vga.misc_output & 0xC1, 0xC1);
        assert_eq!(CrtcTiming::mode_11h(), CrtcTiming::mode_12h());
    }

    #[test]
    fn mode_0fh_scanout_uses_vgabios_monochrome_attribute_table() {
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x0F));
        assert_eq!(vga.seq.map_mask, 0x0F);

        vga.cpu_write(0, 0x80);
        assert_eq!(vga.plane_byte(0, 0), 0x80);
        assert_eq!(vga.plane_byte(1, 0), 0x80);
        assert_eq!(vga.plane_byte(2, 0), 0x80);
        assert_eq!(vga.plane_byte(3, 0), 0x80);
        assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
        assert_eq!(vga.render_active_row(0)[0], 0x08);

        vga.vram[0] = 0;
        vga.vram[2 * VGA_PLANE_SIZE] = 0;
        vga.seq.map_mask = 0x01;
        vga.cpu_write(0, 0x80);
        assert_eq!(vga.planar_read_pixel(0, 0), 0x03);

        vga.vram[0] = 0;
        vga.vram[2 * VGA_PLANE_SIZE] = 0;
        vga.seq.map_mask = 0x04;
        vga.cpu_write(0, 0x80);
        assert_eq!(vga.planar_read_pixel(0, 0), 0x0C);

        vga.vram[0] = 0;
        vga.vram[2 * VGA_PLANE_SIZE] = 0;
        vga.seq.map_mask = 0x05;

        let plane0 = 0;
        let plane2 = 2 * VGA_PLANE_SIZE;
        vga.vram[plane0] = 0x80;
        assert_eq!(vga.planar_read_pixel(0, 0), 0x03);
        assert_eq!(vga.render_active_row(0)[0], 0x08);

        vga.vram[plane0] = 0;
        vga.vram[plane2] = 0x80;
        assert_eq!(vga.planar_read_pixel(0, 0), 0x0C);
        vga.frames = 0;
        assert_eq!(vga.render_active_row(0)[0], 0x00);
        vga.frames = 16;
        assert_eq!(vga.render_active_row(0)[0], 0x00);

        vga.vram[plane0] = 0x80;
        vga.frames = 16;
        assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
        assert_eq!(vga.render_active_row(0)[0], 0x08);

        assert!(vga.planar_write_pixel(1, 0, 0x0C, false));
        assert_eq!(vga.planar_read_pixel(1, 0), 0x0C);
    }

    #[test]
    fn mode_11h_scanout_uses_map0_like_mode6() {
        let mut vga = Vga::default();
        assert!(vga.set_mode(0x11));
        assert_eq!(vga.seq.map_mask, 0x0F);

        vga.cpu_write(0, 0x80);
        assert_eq!(vga.plane_byte(0, 0), 0x80);
        assert_eq!(vga.plane_byte(1, 0), 0x80);
        assert_eq!(vga.plane_byte(2, 0), 0x80);
        assert_eq!(vga.plane_byte(3, 0), 0x80);
        assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
        assert_eq!(vga.render_active_row(0)[0], 0x3F);
        vga.vram[0] = 0;

        for plane in 1..VGA_PLANES {
            vga.vram[plane * VGA_PLANE_SIZE] = 0x80;
        }
        assert_eq!(vga.planar_read_pixel(0, 0), 0x00);
        assert_eq!(vga.render_active_row(0)[0], 0x00);

        vga.vram[0] = 0x80;
        assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
        assert_eq!(vga.render_active_row(0)[0], 0x3F);

        assert!(vga.planar_write_pixel(1, 0, 0x0F, false));
        assert_eq!(vga.planar_read_pixel(1, 0), 0x0F);
        assert_eq!(vga.plane_byte(1, 0) & 0x40, 0);
    }

    #[test]
    fn palette_address_source_tracks_the_3c0_index_bit5() {
        // The 3C0 index bit 5 (Palette Address Source) is decoded and read back. It powers
        // up set (the mode-set default), clears on an index write with bit 5 clear, and
        // sets again on an index write with bit 5 set.
        let mut vga = Vga::default();
        assert!(vga.attr.pas, "PAS powers up set");
        vga.read_status1(); // reset the flip-flop to the index phase
        vga.write_port(0x3C0, 0x00); // index 0, bit 5 clear -> PAS off
        assert!(!vga.attr.pas);
        vga.read_status1();
        vga.write_port(0x3C0, 0x20); // index 0 with bit 5 set -> PAS on
        assert!(vga.attr.pas);
    }

    #[test]
    fn palette_address_source_clear_blanks_render_and_status() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
            *b = 0xFF;
        }
        vga.attr.palette[1] = 5;
        vga.attr.overscan = 7;

        let lit = vga.render_full_frame();
        let w = lit.width as usize;
        let border = vga.crtc.vdisp_end as usize * w;
        assert_eq!(lit.pixels[0], 5);
        assert_eq!(lit.pixels[border], 7);

        vga.read_status1();
        vga.write_port(0x3C0, 0x00); // PAS clear
        assert!(!vga.attr.pas);
        vga.beam = 0;
        assert_eq!(vga.read_status1() & 0x01, 0x01);

        let blank = vga.render_full_frame();
        assert_eq!(blank.pixels[0], 0);
        assert_eq!(blank.pixels[border], 0);

        vga.read_status1();
        vga.write_port(0x3C0, 0x20); // PAS set again
        assert!(vga.attr.pas);
        let restored = vga.render_full_frame();
        assert_eq!(restored.pixels[0], 5);
    }

    #[test]
    fn palette_address_source_bit_does_not_leak_into_the_attr_index() {
        // Bit 5 of the 3C0 index drives PAS but is masked off the stored index, so
        // the following data write still lands on the low-5-bit register.
        let mut vga = Vga::default();
        vga.read_status1(); // index phase
        vga.write_port(0x3C0, 0x20 | 0x13); // PAS on + index 0x13 (pixel pan)
        assert_eq!(vga.attr.index, 0x13);
        assert!(vga.attr.pas);
        vga.write_port(0x3C0, 0x07); // data: pixel_pan = 7
        assert_eq!(vga.attr.pixel_pan, 0x07);
    }
}
