//! The legacy VGA core: 256 KB planar VRAM, the VGA register blocks, a
//! cycle-coupled beam clock, and a catch-up rasterizer. This is Margo's
//! VGA-compatibility personality (one chip, one frame store, one RAMDAC).
//!
//! It also carries the text personality: the 80x25 character buffer, the
//! RAMDAC, and the CRTC text cursor. Chained mode 13h routes through the same
//! raster engine as the planar and mode-X paths; chain-4 only rewrites the CPU
//! write/read decode.

use crate::{
    DAC_ENTRIES, Dac, TextCell, TextFrame, VGA_TEXT_COLUMNS, VGA_TEXT_MEMORY_SIZE, VGA_TEXT_ROWS,
    VideoError, VideoMode,
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
    pub mode_control: u8,  // CRTC index 17h
    pub underline_loc: u8, // CRTC index 14h
    pub line_compare: u32, // assembled 10-bit value: CRTC 18h + 07h.4 + 09h.6
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
        }
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
        }
    }

    /// Total dots per frame = htotal_dots * vtotal (scan-counter lines).
    pub fn frame_dots(&self) -> u64 {
        (self.htotal_chars * self.char_width) as u64 * self.vtotal as u64
    }
}

/// Raw CRTC vertical-timing register bytes, honored only while unchained (mode X)
/// so the geometry follows whatever the guest programs. Seeded at mode-X entry and
/// derived into `CrtcTiming` by `recompute_vertical_timing`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrtcRegs {
    pub r06: u8, // vertical total (low 8)
    pub r07: u8, // overflow (high bits of several fields)
    pub r09: u8, // maximum scan line (double-scan, max_scan, line-compare bit 9)
    pub r10: u8, // vertical retrace start (low 8)
    pub r11: u8, // vertical retrace end (low-nibble compare)
    pub r12: u8, // vertical display end (low 8)
    pub r15: u8, // vertical blank start (low 8)
    pub r16: u8, // vertical blank end (8-bit compare)
}

impl CrtcRegs {
    /// The 320x200 unchained register set, matching `CrtcTiming::mode_x()`.
    pub fn mode_x_320x200() -> Self {
        Self {
            r06: 0xBF,
            r07: 0x1F,
            r09: 0x41,
            r10: 0x9C,
            r11: 0x0E,
            r12: 0x8F,
            r15: 0x97,
            r16: 0xBA,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Sequencer {
    pub map_mask: u8,    // idx 2, low 4 bits
    pub memory_mode: u8, // idx 4
}

/// Attribute Controller register block (3C0/3C1).
#[derive(Debug, Clone, Copy, Default)]
pub struct Attribute {
    pub palette: [u8; 16],    // idx 0..15
    pub mode_control: u8,     // idx 0x10
    pub overscan: u8,         // idx 0x11
    pub plane_enable: u8,     // idx 0x12
    pub pixel_pan: u8,        // idx 0x13, low 4 bits
    pub color_select: u8,     // idx 0x14
    pub flip_flop_data: bool, // false = next 3C0 write is an index
    pub index: u8,
}

/// The pixel-perfect raster the host pulls. Square pixels, no aspect correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgaRaster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // DAC indices; renderer resolves through the Dac
}

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
    pub(crate) dac: Dac,
    pub(crate) cursor_offset: u16,
    pub(crate) mode: VideoMode,
}

impl Default for Vga {
    fn default() -> Self {
        let mut text_memory = [0; VGA_TEXT_MEMORY_SIZE];
        for cell in text_memory.chunks_exact_mut(2) {
            cell[0] = b' ';
            cell[1] = 0x07;
        }

        Self {
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
            dac: Dac::default(),
            cursor_offset: 0,
            mode: VideoMode::Text,
        }
    }
}

impl Vga {
    pub fn frame_dots(&self) -> u64 {
        self.crtc.frame_dots()
    }

    pub fn beam_dots(&self) -> u64 {
        self.beam
    }

    pub fn frames_completed(&self) -> u64 {
        self.frames
    }

    /// Install a planar mode's timing and reset the beam to the top of frame.
    fn set_planar_mode(&mut self, timing: CrtcTiming) {
        self.crtc = timing;
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Planar;
        self.presented = None; // drop any stale frame from a prior mode
        self.resize_work();
    }

    /// Switch to mode 0Dh. Kept as an alias so existing callers do not churn;
    /// new call sites can use `set_mode(0x0D)`.
    pub fn set_mode_0dh(&mut self) {
        self.set_planar_mode(CrtcTiming::mode_0dh());
    }

    /// Select a planar mode by its INT 10h number. Returns false for a number this
    /// slice does not implement, leaving the current mode untouched.
    pub fn set_mode(&mut self, mode: u8) -> bool {
        let timing = match mode {
            0x0D => CrtcTiming::mode_0dh(),
            0x0E => CrtcTiming::mode_0eh(),
            0x10 => CrtcTiming::mode_10h(),
            0x12 => CrtcTiming::mode_12h(),
            _ => return false,
        };
        self.set_planar_mode(timing);
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

    /// Display-address origin for one scanline, honoring the CRTC Line Compare
    /// split (Abrash, Graphics Programming Black Book ch.30). Returns
    /// `(start_base, first_line)`: above the split the start address scrolls the
    /// region; at and below `line_compare + 1` the address counter reloads to 0
    /// and row counting restarts there. The comparison is in scan-counter units
    /// and is not divided by the double-scan factor, so `first_line` is already
    /// in counter units and is divided by `scan_factor` by the caller.
    fn split_origin(&self, counter_line: u32) -> (u32, u32) {
        if counter_line > self.crtc.line_compare {
            (0, self.crtc.line_compare + 1)
        } else {
            (self.crtc.start_address, 0)
        }
    }

    /// Effective horizontal pel-pan for one scanline, honoring the Attribute Mode
    /// Control (10h) bit 5 "enable pixel panning up to line compare" forcing
    /// (RBIL PORTS.B table P0664): below the CRTC Line Compare split the pan is
    /// forced to 0 when bit 5 is set. Returns the raw 13h value masked to 0-15;
    /// the mode-X caller masks further to 0-3.
    fn pel_pan(&self, below_split: bool) -> usize {
        if below_split && (self.attr.mode_control & 0x20 != 0) {
            0
        } else {
            (self.attr.pixel_pan & 0x0F) as usize
        }
    }

    /// Assemble one active scanline into `hdisp_end` DAC indices, applying pel-pan
    /// and the attribute palette. `counter_line` is the scanline in scan-counter
    /// units; double-scan maps it to source row `counter_line / scan_factor`, so a
    /// doubled mode holds each VRAM row for two scanlines.
    pub fn render_active_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        // Line Compare split (CRTC 18h + 07h.4 + 09h.6). Abrash, Graphics Programming
        // Black Book ch.30: the scanline matching line compare is the last line above
        // the split; the split starts on the following line and reloads the display
        // address counter to 0. The comparison is in scan-counter units, the same units
        // the beam and the other vertical timing registers use, so it is not divided by
        // the double-scan factor.
        let below_split = counter_line > self.crtc.line_compare;
        let (start, first_line) = self.split_origin(counter_line);
        let pan = self.pel_pan(below_split);
        let source_row = (counter_line - first_line) / self.scan_factor();
        // The per-scanline counter increment is offset*2 in every addressing mode; the
        // byte/word/doubleword transform lives in display_offset, not the stride.
        let row_base = start + source_row * self.crtc.offset * 2;
        let mut row = vec![0u8; width];
        for (x, slot) in row.iter_mut().enumerate() {
            let px = x + pan;
            let byte = px / 8;
            let bit = 7 - (px % 8);
            let ma = row_base + byte as u32;
            let off = display_offset(self.crtc.mode_control, self.crtc.underline_loc, ma);
            let mut index = 0u8;
            for plane in 0..VGA_PLANES {
                let b = self.vram[plane * VGA_PLANE_SIZE + off];
                index |= ((b >> bit) & 1) << plane;
            }
            *slot = self.attr.palette[index as usize] & 0x3F;
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
        let below_split = counter_line > self.crtc.line_compare;
        let (start, first_line) = self.split_origin(counter_line);
        // The split branch returns first_line = line_compare + 1 and is taken only when
        // counter_line > line_compare, so counter_line >= first_line holds: the
        // subtraction never underflows.
        let source_row = (counter_line - first_line) / self.scan_factor();
        let row_base = start + source_row * self.crtc.offset * 2;
        // Mode-X pel-pan: one plane per pel, so the fine range is 0-3 (a pan of 4
        // equals a start-address bump). The below-split forcing is shared with the
        // 16-color path through pel_pan.
        let pan = (self.pel_pan(below_split) & 0x03) as u32;
        let mut row = vec![0u8; width];
        for (x, slot) in row.iter_mut().enumerate() {
            let x_eff = x as u32 + pan;
            let plane = (x_eff & 3) as usize;
            let ma = row_base + (x_eff >> 2);
            let off = display_offset(self.crtc.mode_control, self.crtc.underline_loc, ma);
            *slot = self.vram[plane * VGA_PLANE_SIZE + off];
        }
        row
    }

    fn region_color(&self, scan_line: u32) -> u8 {
        // scan_line in counter units; caller guarantees scan_line >= vdisp_end.
        if scan_line < self.crtc.vblank_start || scan_line >= self.crtc.vblank_end {
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
        let pixels = if counter_line < self.crtc.vdisp_end {
            match self.mode {
                VideoMode::Mode13h | VideoMode::ModeX => self.render_256color_row(counter_line),
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
        // `work` is sized by every graphics mode-set (planar, mode X, and mode
        // 13h all install timing and resize); text leaves it empty, so a frame
        // built from it would have a pixel count that mismatches width*height.
        // Only publish a raster when there is real graphics content to show.
        if !self.work.is_empty() {
            self.presented = Some(VgaRaster {
                width: self.raster_width(),
                height: self.raster_height(),
                pixels: self.work.clone(),
            });
        }
        if let Some(addr) = self.pending_start.take() {
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

    pub fn cpu_write(&mut self, offset: usize, data: u8) {
        if offset >= VGA_PLANE_SIZE {
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
        let planes = self.plane_slice_mut(offset);
        let gc = self.gc;
        read_planes(&planes, &gc, &mut self.latches)
    }

    /// Chained mode-13h CPU write: chain-4 (Sequencer Memory Mode 04h bit 3)
    /// routes byte N straight to plane `N & 3` at plane-offset `N >> 2`, bypassing
    /// the planar datapath (map mask, write mode, bit mask, latches). This is the
    /// CPU write decode that mode X turns off; the CRTC display scanout reads the
    /// same four-plane VRAM either way (Abrash, Graphics Programming Black Book
    /// ch.47).
    pub fn cpu_write_chain4(&mut self, offset: usize, data: u8) {
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

    /// Read Input Status Register 1 (port 3DAh).
    ///
    /// Bit 0: display disabled (beam is in blank or retrace).
    /// Bit 3: vertical retrace active.
    ///
    /// Reading this register also resets the Attribute Controller address/data
    /// flip-flop so that the next write to 3C0 is treated as an index.
    pub fn read_status1(&mut self) -> u8 {
        self.catch_up(); // a 3DA read catches the raster up, like a register write
        self.attr.flip_flop_data = false; // reading 3DA resets the flip-flop
        let mut status = 0u8;
        if !beam_display_enable(&self.crtc, self.beam) {
            status |= 0x01; // display disabled (blank or retrace)
        }
        if beam_vretrace(&self.crtc, self.beam) {
            status |= 0x08; // vertical retrace
        }
        status
    }

    /// Write to a VGA I/O port. Calls `catch_up()` first so any lines already
    /// past the beam are rendered with the previous register state before the
    /// new value takes effect. Returns `true` if the port is handled.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.catch_up();
        match port {
            0x3C4 => {
                self.seq_index = value;
                true
            }
            0x3C5 => {
                let idx = self.seq_index;
                self.write_seq(idx, value);
                true
            }
            0x3C7 => {
                self.dac.set_read_index(value);
                true
            }
            0x3C8 => {
                self.dac.set_write_index(value);
                true
            }
            0x3C9 => {
                self.dac.write_data(value);
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
            0x3D4 => {
                self.crtc_index = value;
                true
            }
            0x3D5 => {
                let idx = self.crtc_index;
                self.write_crtc(idx, value);
                true
            }
            0x3C0 => {
                self.write_attr(value);
                true
            }
            _ => false,
        }
    }

    /// Read from a VGA I/O port. Returns `Some(value)` for handled ports.
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x3C8 => Some(self.dac.write_index()),
            0x3C9 => Some(self.dac.read_data()),
            0x3D4 => Some(self.crtc_index),
            0x3D5 => match self.crtc_index {
                0x0E => Some((self.cursor_offset >> 8) as u8),
                0x0F => Some(self.cursor_offset as u8),
                _ => Some(0),
            },
            0x3DA => Some(self.read_status1()),
            _ => None,
        }
    }

    fn write_seq(&mut self, index: u8, value: u8) {
        match index {
            0x02 => self.seq.map_mask = value & 0x0F,
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
            }
            0x07 => self.gc.color_dont_care = value & 0x0F,
            0x08 => self.gc.bit_mask = value,
            _ => {}
        }
    }

    fn write_crtc(&mut self, index: u8, value: u8) {
        match index {
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
            0x13 => self.crtc.offset = u32::from(value),
            0x14 => self.crtc.underline_loc = value,
            0x17 => self.crtc.mode_control = value,
            0x18 => self.crtc.line_compare = (self.crtc.line_compare & !0xFF) | u32::from(value),
            0x07 => {
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x100) | (u32::from((value >> 4) & 1) << 8);
            }
            0x09 => {
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x200) | (u32::from((value >> 6) & 1) << 9);
            }
            _ => {} // full timing programmed via set_mode_0dh in slice 1
        }
        // While unchained (mode X), honor the guest's vertical CRTC timing so the
        // geometry follows the registers it programs (Abrash's bang yields 320x240).
        // Seeded at mode-X entry; the absolute fields are derived in
        // recompute_vertical_timing. The line-compare bits (07h/09h/18h) are handled
        // by the match above and left untouched here.
        if self.mode == VideoMode::ModeX
            && matches!(index, 0x06 | 0x07 | 0x09 | 0x10 | 0x11 | 0x12 | 0x15 | 0x16)
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
        self.text_memory
            .get(offset)
            .copied()
            .ok_or(VideoError::TextMemoryOutOfBounds { offset })
    }

    pub fn write_u8(&mut self, offset: usize, value: u8) -> Result<(), VideoError> {
        let slot = self
            .text_memory
            .get_mut(offset)
            .ok_or(VideoError::TextMemoryOutOfBounds { offset })?;
        *slot = value;
        Ok(())
    }

    /// Switch to chained mode 13h, installing the standard 320x200 70Hz timing
    /// and routing the scanout through the shared raster engine (the same path
    /// as the planar and mode-X modes). Chain-4 is the mode-13h-specific CPU
    /// write decode; the CRTC display scanout is shared with mode X.
    pub fn set_mode13h(&mut self) {
        self.crtc = CrtcTiming::mode13h();
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Mode13h;
        self.presented = None; // drop any stale frame from a prior mode
        self.resize_work();
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

    pub fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        let mut out = [0u32; DAC_ENTRIES];
        for (index, slot) in out.iter_mut().enumerate() {
            let (r, g, b) = self.dac.rgb888(index as u8);
            *slot = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
        out
    }

    pub fn frame(&self) -> TextFrame {
        let cells = self
            .text_memory
            .chunks_exact(2)
            .map(|cell| TextCell {
                character: cell[0],
                attribute: cell[1],
            })
            .collect();

        TextFrame {
            columns: VGA_TEXT_COLUMNS,
            rows: VGA_TEXT_ROWS,
            cells,
            cursor_offset: self.cursor_offset,
        }
    }
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
    pub color_dont_care: u8,  // idx 7
    pub bit_mask: u8,         // idx 8
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
/// See `dev_docs/reference/vga/crtc-addressing.md`.
pub fn display_offset(mode_control: u8, underline_loc: u8, ma: u32) -> usize {
    let addr = if mode_control & 0x40 != 0 {
        ma // byte mode (CR17 bit 6): identity
    } else if underline_loc & 0x40 != 0 {
        // doubleword mode (CR14 bit 6): rotate left 2, MA13 -> bit 1, MA12 -> bit 0.
        // These bit positions are pending validation against an unbroken reference
        // mirror; no in-scope 16-color planar workload exercises doubleword mode.
        (ma << 2) | ((ma >> 12) & 0x3)
    } else {
        // word mode: rotate left 1, MA15 (CR17 bit 5 = 1) or MA13 (= 0) -> bit 0
        let wrap_bit = if mode_control & 0x20 != 0 { 15 } else { 13 };
        (ma << 1) | ((ma >> wrap_bit) & 1)
    };
    (addr as usize) % VGA_PLANE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_offset_applies_byte_word_dword_transforms() {
        // Byte mode (CR17 bit 6 = 1): identity, wrapped at 64 KB.
        assert_eq!(display_offset(0xE3, 0x00, 0x1234), 0x1234);
        assert_eq!(display_offset(0xE3, 0x00, 0x1_0005), 0x0005); // 64 KB counter wrap
        // Word mode, 16-bit wrap (CR17 = 0xA3: bit 6 = 0, bit 5 = 1): rotate left 1,
        // MA15 into bit 0.
        assert_eq!(display_offset(0xA3, 0x00, 0x4001), 0x8002); // MA15 = 0
        assert_eq!(display_offset(0xA3, 0x00, 0x8000), 0x0001); // MA15 = 1 -> bit 0
        // Word mode, 14-bit wrap (CR17 = 0x83: bit 6 = 0, bit 5 = 0): MA13 into bit 0.
        assert_eq!(display_offset(0x83, 0x00, 0x2000), 0x4001); // MA13 = 1 -> bit 0
        // Doubleword mode (CR14 bit 6 = 1, word base): rotate left 2, MA13 -> bit 1,
        // MA12 -> bit 0.
        assert_eq!(display_offset(0xA3, 0x40, 0x3000), 0xC003);
        // Byte mode wins over the doubleword bit.
        assert_eq!(display_offset(0xE3, 0x40, 0x1234), 0x1234);
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
        vga.seq.map_mask = 0x0F; // all planes enabled
        vga.gc.write_mode = 0;
        vga.gc.bit_mask = 0xFF;
        vga.cpu_write(0x10, 0xA5);
        vga.gc.read_map = 0;
        assert_eq!(vga.cpu_read(0x10), 0xA5);
        assert_eq!(vga.latches, [0xA5; VGA_PLANES]);
    }

    #[test]
    fn map_mask_gates_which_planes_are_written() {
        let mut vga = Vga::default();
        vga.seq.map_mask = 0b0001; // only plane 0
        vga.gc.bit_mask = 0xFF;
        vga.cpu_write(0, 0xFF);
        assert_eq!(vga.plane_byte(0, 0), 0xFF);
        assert_eq!(vga.plane_byte(1, 0), 0x00);
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
    fn attribute_flipflop_alternates_index_then_data() {
        let mut vga = Vga::default();
        vga.read_status1(); // reset flip-flop to "index"
        vga.write_port(0x3C0, 0x13); // pixel pan index
        vga.write_port(0x3C0, 0x02); // value
        assert_eq!(vga.attr.pixel_pan, 0x02);
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
        vga.write_port(0x3C0, 0x01); // attr index 1
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
    fn set_mode13h_switches_active_mode() {
        let mut video = Vga::default();
        assert_eq!(video.active_mode(), VideoMode::Text);
        video.set_mode13h();
        assert_eq!(video.active_mode(), VideoMode::Mode13h);
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
    fn set_mode_selects_geometry_for_each_planar_number() {
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

        assert!(!vga.set_mode(0x99)); // unknown number leaves a false result
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
    fn line_compare_compares_against_the_scan_counter_line_in_a_doubled_mode() {
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
        // Scanlines 321 and 322 are the first two split scanlines: the same doubled
        // source row 0, read from offset 0.
        assert_eq!(
            vga.render_active_row(321)[0],
            1,
            "first split scanline, offset 0"
        );
        assert_eq!(
            vga.render_active_row(322)[0],
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
}
