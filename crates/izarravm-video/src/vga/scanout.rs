// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
    /// Content generation captured with this finalized raster.
    pub generation: u64,
    pub pixels: Vec<u8>, // DAC indices; renderer resolves through the Dac
}

/// Per-scanline text scanout parameters decoded once per row by
/// `Vga::text_row_scan` for the single-pixel `text_pixel` sampler
/// (`render_text_row` keeps its own fused prologue; see
/// `graphics_row_geometry` for why). See those methods for the register
/// semantics behind each field.
struct TextRowScan {
    cga_text: bool,
    char_row: usize,
    font_line: usize,
    char_width: usize,
    pan: usize,
    byte_pan: usize,
    start_cells: usize,
    blink_enabled: bool,
    blink_hide_phase: bool,
    table_a: usize,
    table_b: usize,
    dual_font: bool,
    cursor_disabled: bool,
    cursor_hidden: bool,
    cursor_byte: usize,
    start_line: usize,
    end_line: usize,
    text_aperture_size: usize,
}

/// One resolved text cell's pixel-generation inputs (`Vga::text_cell_pixels`):
/// enough to produce any of the cell's `char_width` pixels without re-reading
/// the character/attribute pair or the font.
struct TextCellPixels {
    fg: u8,
    bg: u8,
    glyph_row: u8,
    hide_fg: bool,
    extend_ninth: bool,
}

impl TextCellPixels {
    /// The cell's pel at column `px` (0-based within the cell). Bit 7 of the
    /// glyph row is the leftmost pel; the 9th column (px == 8, 9-dot mode)
    /// replicates the 8th (bit 0) for the box-drawing glyphs 0xC0-0xDF and is
    /// the background otherwise.
    #[inline]
    fn pixel(&self, px: usize) -> u8 {
        let lit = if px < 8 {
            (self.glyph_row >> (7 - px)) & 1 != 0
        } else {
            self.extend_ninth && (self.glyph_row & 0x01 != 0)
        };
        if lit && !self.hide_fg {
            self.fg
        } else {
            self.bg
        }
    }
}

impl Vga {
    pub fn raster_width(&self) -> u32 {
        self.crtc.hdisp_end
    }

    /// Scanlines per source row (the double-scan factor). For every mode this
    /// slice supports this equals `max_scan + 1`, the form the spec and the
    /// conformance doc use for the source divide; a triple-scan mode would have
    /// to read `max_scan` directly.
    pub(super) fn scan_factor(&self) -> u32 {
        if self.crtc.double_scan { 2 } else { 1 }
    }

    /// Full visible frame height in raster lines. One raster row per scanline, so
    /// this is `vtotal`; double-scan divides the source address (see
    /// `render_active_row`) rather than multiplying the output.
    pub fn raster_height(&self) -> u32 {
        self.crtc.vtotal
    }

    /// Size the work raster to the active geometry, keeping what the beam has
    /// already drawn when the size does not change.
    ///
    /// Every recompute of the CRTC timing calls this, and a guest that rewrites
    /// its register table each frame therefore calls it each frame. Discarding
    /// the raster on those writes cost Psycho Pinball its whole picture: it
    /// replays the table late in the frame, so the frame published at the next
    /// vertical retrace held only the lines drawn after the write. Real silicon
    /// has no such buffer -- rewriting a register with the value it already
    /// holds changes nothing the beam is doing. A genuine size change still
    /// starts from a cleared buffer, because none of the old lines apply to the
    /// new geometry.
    pub(super) fn resize_work(&mut self) {
        let pixels = (self.raster_width() * self.raster_height()) as usize;
        if self.work.len() != pixels {
            self.work = vec![0; pixels];
        } else if self.work_mode != self.mode {
            // Same size, different mode. The rows in the buffer belong to the
            // mode being left, and the CGA personality's mode-control path
            // resizes WITHOUT resetting the render cursor, so keeping them would
            // publish the old mode's picture in the new mode's first frame. CGA
            // 320x200 graphics and CGA 40x25 text are both 320x262, so the pair
            // is reachable rather than hypothetical.
            self.work.fill(0);
        }
        self.work_mode = self.mode;
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

    pub(super) fn text_byte(&self, offset: usize) -> u8 {
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
    pub(super) fn text_cell_base(&self, start_cells: usize, char_row: usize, col: usize) -> usize {
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

    pub(super) fn sequencer_outputs_enabled(&self) -> bool {
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

    pub(super) fn planar_logical_attr_index(&self, plane_bits: u8) -> u8 {
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

    pub(super) fn planar_storage_bits(&self, color: u8) -> u8 {
        match self.planar_bios_mode {
            0x0F => u8::from(color & 0x01 != 0) | (u8::from(color & 0x04 != 0) << 2),
            0x11 => u8::from(color & 0x01 != 0),
            _ => color & 0x0F,
        }
    }

    /// Off `beam` rather than `self.beam` so `status1_bits`'s lazy caller can
    /// pass a predicted beam position. Recomputes the pixel color live from
    /// current VRAM/register state (`render_*_row` read no cached raster), so
    /// it never depends on `catch_up` having already rendered the given line.
    ///
    /// DECISION (lazy reads vs frame-latched state): the sampled pixel depends
    /// on state latched at frame boundaries -- `crtc.start_address` is latched
    /// by `finalize_frame` (via the pending-start vretrace latch) and the text
    /// cursor/attribute blink phase derives from `self.frames`. A lazy read
    /// whose predicted beam wrapped past a frame boundary the device has not
    /// actually advanced through yet computes these bits with the PREVIOUS
    /// frame's latch and blink phase. Accepted as-is, no compensation: the
    /// divergence is confined to the diagnostic mux bits 4-5 (never the
    /// vretrace/display-enable bits games poll), it is bounded by the ~1ms
    /// Approximate-class batch cap (well under a frame), and it has the same
    /// acceptance shape as the documented dot-clock retroactivity decision on
    /// the lazy arm in MachineBus::read_io.
    pub(super) fn video_status_mux_bits(&self, beam: u64) -> u8 {
        if self.is_cga_personality()
            || self.is_hercules_personality()
            || !beam_display_enable(&self.crtc, beam)
        {
            return 0;
        }
        let line = beam_line(&self.crtc, beam);
        let dot = beam_dot(&self.crtc, beam) as usize;
        // Sample ONLY the one pel under the beam, through the same shared
        // per-pixel/per-cell implementations the full row renderers loop over
        // (`active_pixel`/`color256_pixel`/`text_cell_pixels`), so the sampled
        // value is bit-identical to `render_*_row(line)[dot]` -- pinned by
        // `status_mux_single_pixel_sample_matches_the_full_row_render` -- with
        // no per-read row render or heap allocation (measured ~10x of a 3DA
        // poll's wall cost before this). `beam_display_enable` above guarantees
        // dot < hdisp_end, the exact domain the row indexing covered.
        let color = match self.mode {
            VideoMode::Mode13h | VideoMode::ModeX => {
                let (row_base, row_scan, below_split) = self.graphics_row_geometry(line);
                let pan = (self.pel_pan(below_split) & 0x03) as u32;
                self.color256_pixel(row_base, row_scan, pan, dot)
            }
            VideoMode::Text => self.text_pixel(line, dot),
            VideoMode::Planar => {
                let (row_base, row_scan, below_split) = self.graphics_row_geometry(line);
                let pan = self.pel_pan(below_split);
                self.active_pixel(row_base, row_scan, pan, dot)
            }
            VideoMode::Cga | VideoMode::Hercules => 0,
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
    ///
    /// A parallel per-pixel implementation exists for the lazy ISR1 mux sampler
    /// (`graphics_row_geometry` + `active_pixel`); kept in sync by
    /// `status_mux_single_pixel_sample_matches_the_full_row_render`. Edits to
    /// this loop's arithmetic must update both.
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

    /// Per-row scanout geometry for the SINGLE-PIXEL samplers (16-color planar
    /// and 256-color): the Line Compare split (CRTC 18h + 07h.4 + 09h.6, in
    /// scan-counter units so it is not divided by the double-scan factor), the
    /// origin, the row scan (with preset row scan), and the row base address.
    /// Mirrors the fused prologues of `render_active_row`/`render_256color_row`
    /// exactly. The row renderers keep their own FUSED bodies rather than
    /// looping over the sampler helpers: routing their per-pixel hot loops
    /// through these functions measured a 3.6-26.5 percent per-scanline wall
    /// regression (interleaved A/B, worst on the slow modes, which render the
    /// most scanlines per wall second). Renderer/sampler equality is pinned by
    /// `status_mux_single_pixel_sample_matches_the_full_row_render`'s full-line
    /// differential sweeps plus the scanout goldens.
    /// Returns (row_base, row_scan, below_split); the caller derives its own
    /// pel-pan (the 256-color path masks it to 0-3).
    fn graphics_row_geometry(&self, counter_line: u32) -> (u32, u32, bool) {
        let below_split = self.below_split(counter_line);
        let (start, first_line) = self.split_origin(counter_line);
        // The split branch returns first_line = line_compare + 1 and is taken
        // only when counter_line > line_compare, so counter_line >= first_line
        // holds: the subtraction never underflows.
        let row_scan = counter_line - first_line + self.preset_row_scan(below_split);
        let source_row = row_scan / self.scan_factor();
        let row_base = start + source_row * self.crtc.offset * 2 + self.byte_pan(below_split);
        (row_base, row_scan, below_split)
    }

    /// One 16-color planar pixel at column `x` of the row described by
    /// (`row_base`, `row_scan`, `pan`). Sampler-only (the ISR1 video-status
    /// mux); `render_active_row` keeps its own fused copy of this arithmetic
    /// (see `graphics_row_geometry` for why), and divergence between the two
    /// is pinned by the differential sweep test.
    #[inline]
    fn active_pixel(&self, row_base: u32, row_scan: u32, pan: usize, x: usize) -> u8 {
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
        self.dac_index(self.attr_lookup(self.planar_scanout_attr_index(index)))
    }

    /// The linear cache represents only the stock 320x200 Mode 13h address
    /// layout. Register-banged layouts use the planar address generator below.
    pub(super) fn canonical_mode13_linear_scanout(&self) -> bool {
        self.mode13_linear_valid && self.canonical_mode13_layout()
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
    ///
    /// A parallel per-pixel implementation exists for the lazy ISR1 mux sampler
    /// (`graphics_row_geometry` + `color256_pixel`); kept in sync by
    /// `status_mux_single_pixel_sample_matches_the_full_row_render`. Edits to
    /// this loop's arithmetic must update both.
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
        let linear = self.canonical_mode13_linear_scanout();
        let mut row = vec![0u8; width];
        for (x, slot) in row.iter_mut().enumerate() {
            let x_eff = x as u32 + pan;
            let val = if linear {
                let l = ((row_base as usize) << 2) + (x_eff as usize);
                if l < self.mode13_linear.len() {
                    self.mode13_linear[l] & self.pel_mask
                } else {
                    0
                }
            } else {
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
                self.vram[plane * VGA_PLANE_SIZE + off] & self.pel_mask
            };
            *slot = val;
        }
        row
    }

    /// One 256-color pixel at column `x` of the row described by (`row_base`,
    /// `row_scan`, `pan`). Sampler-only (the ISR1 video-status mux);
    /// `render_256color_row` keeps its own fused copy of this arithmetic (see
    /// `graphics_row_geometry` for why), and divergence between the two is
    /// pinned by the differential sweep test.
    #[inline]
    fn color256_pixel(&self, row_base: u32, row_scan: u32, pan: u32, x: usize) -> u8 {
        let x_eff = x as u32 + pan;
        if self.canonical_mode13_linear_scanout() {
            let l = ((row_base as usize) << 2) + (x_eff as usize);
            if l < self.mode13_linear.len() {
                return self.mode13_linear[l] & self.pel_mask;
            } else {
                return 0;
            }
        }
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
        self.vram[plane * VGA_PLANE_SIZE + off] & self.pel_mask
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
    ///
    /// A parallel per-pixel implementation exists for the lazy ISR1 mux sampler
    /// (`text_row_scan` + `text_cell_pixels` + `text_pixel`); kept in sync by
    /// `status_mux_single_pixel_sample_matches_the_full_row_render`. Edits to
    /// this prologue or cell loop's arithmetic must update both.
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
        // read wraps at the 32 KB text aperture (FreeVGA 0Dh wrap behavior).
        let below_split = self.below_split(counter_line);
        let (start, first_line) = self.split_origin(counter_line);
        // split_origin returns first_line <= counter_line in both branches, so the
        // subtraction never underflows.
        let rel = counter_line - first_line;
        // CRTC Preset Row Scan (08h, FreeVGA crtcreg.htm): bits 4-0 offset the
        // first displayed font scanline (vertical sub-row smooth scroll), bits 6-5
        // are the byte pan added to the start address. Below the line-compare split
        // the preset always resets to 0; the byte pan resets to 0 below the split
        // only when AC 10h bit 5 is set (FreeVGA 18h).
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
        // split (FreeVGA crtcreg.htm 18h).
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
        // read this single source.
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
            // 8 colors.
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
            // 0Bh; IBM VGA, not the clone "skew 3 = off" variant). The skew,
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

    /// One text-mode pixel at column `x` of `counter_line`, byte-identical to
    /// `render_text_row(counter_line)[x]` for every x < hdisp_end: the row
    /// renderer places cell `dc`'s pel `px` at `x = dc*char_width + px - pan`,
    /// each visible x written by exactly one (dc, px) pair, so inverting that
    /// placement gives `dc = (x + pan) / char_width`, `px = (x + pan) %
    /// char_width`. Sampler-only (the ISR1 video-status mux calls this instead
    /// of rendering, and heap-allocating, a whole row to read one pixel);
    /// `render_text_row` keeps its own fused cell loop (see
    /// `graphics_row_geometry` for the measured reason), and divergence between
    /// the two is pinned by the differential sweep test.
    fn text_pixel(&self, counter_line: u32, x: usize) -> u8 {
        let cga_text = self.is_cga_text_mode();
        if cga_text && self.cga.mode_control & CGA_MODE_VIDEO_ENABLE == 0 {
            return CGA_BLACK;
        }
        let p = self.text_row_scan(counter_line);
        let shifted = x + p.pan;
        let dc = shifted / p.char_width;
        // Placement-domain guard: the row renderer only draws cells dc in
        // 0..=text_columns, so any x whose inverted cell index lands beyond
        // that (reachable when hdisp_end exceeds (text_columns+1)*char_width -
        // pan, e.g. 8-dot Sequencer clocking under the 720-dot mode-3 CRTC)
        // stays at the row Vec's initialized 0. Without this the inversion
        // would answer for positions the renderer never writes, resolving an
        // aperture-wrapped cell instead.
        if dc > self.text_columns {
            return 0;
        }
        let px = shifted % p.char_width;
        self.text_cell_pixels(&p, dc).pixel(px)
    }

    /// Per-scanline text scanout parameters for the single-pixel `text_pixel`
    /// sampler, mirroring the fused prologue of `render_text_row` exactly (see
    /// `graphics_row_geometry` for why the renderer keeps its own copy).
    fn text_row_scan(&self, counter_line: u32) -> TextRowScan {
        let cga_text = self.is_cga_text_mode();
        let rows_per_char = self.crtc.max_scan + 1;
        // The display origin scrolls with the CRTC Start Address (0C/0Dh). Above
        // the line-compare split the origin is `start_address`; at and below the
        // split the counter reloads to 0 (split_origin). Mode 03h is word mode
        // (CR17 bit 6 clear), so `start_address` is a word/cell address, the same
        // units as the CRTC cursor location (0E/0Fh): a displayed cell at
        // (char_row, col) has the absolute cell index `start + char_row*offset +
        // col` and reads the char/attr byte pair at that cell index * 2. The byte
        // read wraps at the 32 KB text aperture (FreeVGA 0Dh wrap behavior).
        let below_split = self.below_split(counter_line);
        let (start, first_line) = self.split_origin(counter_line);
        // split_origin returns first_line <= counter_line in both branches, so the
        // subtraction never underflows.
        let rel = counter_line - first_line;
        // CRTC Preset Row Scan (08h, FreeVGA crtcreg.htm): bits 4-0 offset the
        // first displayed font scanline (vertical sub-row smooth scroll), bits 6-5
        // are the byte pan added to the start address. Below the line-compare split
        // the preset always resets to 0; the byte pan resets to 0 below the split
        // only when AC 10h bit 5 is set (FreeVGA 18h).
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
        // split (FreeVGA crtcreg.htm 18h).
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
        // read this single source.
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
        TextRowScan {
            cga_text,
            char_row,
            font_line,
            char_width,
            pan,
            byte_pan,
            start_cells,
            blink_enabled,
            blink_hide_phase,
            table_a,
            table_b,
            dual_font,
            cursor_disabled,
            cursor_hidden,
            cursor_byte,
            start_line,
            end_line,
            text_aperture_size,
        }
    }

    /// Resolve displayed cell `dc` of the scanline described by `p` into its
    /// pixel-generation inputs (fg/bg colors, glyph row, blink hide, 9th-column
    /// extension). Sampler-only, mirroring the fused per-cell body of
    /// `render_text_row` exactly (see `graphics_row_geometry` for why the
    /// renderer keeps its own copy); divergence is pinned by the differential
    /// sweep test.
    fn text_cell_pixels(&self, p: &TextRowScan, dc: usize) -> TextCellPixels {
        // Absolute cell index (char/attr pair) scrolled by the start address;
        // the CRTC byte pan (08h bits 6-5) adds a byte offset to the origin,
        // so a pan of 2 shifts one whole cell and a pan of 1 lands on the
        // attribute byte (the real-hardware half-cell scramble).
        let base = (self.text_cell_base(p.start_cells, p.char_row, dc) + p.byte_pan)
            % p.text_aperture_size;
        let char_byte = self.text_byte(base);
        let attr = self.text_byte(base + 1);
        let blink_attr = attr & 0x80 != 0;
        // 512-glyph mode: when the Sequencer selects two distinct font tables
        // (map A != map B), attribute bit 3 becomes the per-cell font selector
        // and is no longer foreground intensity, so the foreground is masked to
        // 8 colors.
        let font_select = (attr >> 3) & 1 != 0;
        let font_table = if p.dual_font && font_select {
            p.table_b
        } else {
            p.table_a
        };
        let fg_index = if p.dual_font {
            (attr & 0x07) as usize
        } else {
            (attr & 0x0F) as usize
        };
        let bg_index = if p.blink_enabled && blink_attr {
            ((attr >> 4) & 0x07) as usize
        } else {
            ((attr >> 4) & 0x0F) as usize
        };
        let mut fg = if p.cga_text {
            fg_index as u8
        } else {
            self.dac_index(self.attr_lookup(fg_index as u8))
        };
        let mut bg = if p.cga_text {
            bg_index as u8
        } else {
            self.dac_index(self.attr_lookup(bg_index as u8))
        };
        let hide_fg = p.blink_enabled && blink_attr && p.blink_hide_phase;
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
        // 0Bh; IBM VGA, not the clone "skew 3 = off" variant). The skew,
        // cursor byte, disable bit, and scanline range are decoded once per
        // scanline in text_row_scan.
        let cursor_here = base == p.cursor_byte;
        let in_range = if p.start_line <= p.end_line {
            p.font_line >= p.start_line && p.font_line <= p.end_line
        } else {
            p.font_line >= p.start_line || p.font_line <= p.end_line
        };
        if cursor_here && !p.cursor_disabled && in_range && !p.cursor_hidden {
            std::mem::swap(&mut fg, &mut bg);
        }
        // VGA reads the active writable font table. CGA has a fixed 8x8 ROM
        // character generator, not VGA plane-2 font RAM.
        let glyph_row = if p.cga_text {
            crate::font::VGAFONT_8X8[char_byte as usize * 8 + p.font_line.min(7)]
        } else {
            self.font[font_table][char_byte as usize * 32 + p.font_line.min(31)]
        };
        let extend_ninth = (0xC0..=0xDF).contains(&char_byte);
        TextCellPixels {
            fg,
            bg,
            glyph_row,
            hide_fg,
            extend_ninth,
        }
    }

    fn region_color(&self, scan_line: u32) -> u8 {
        // scan_line in counter units; caller guarantees scan_line >= vdisp_end.
        if self.is_cga_personality() && self.cga.mode_control & CGA_MODE_VIDEO_ENABLE == 0 {
            return CGA_BLACK;
        }
        if self.is_hercules_personality() && self.hgc.mode_control & HGC_MODE_VIDEO_ENABLE == 0 {
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
            if self.mode == VideoMode::Hercules {
                return CGA_BLACK;
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
                    VideoMode::Hercules => self.render_hgc_row(counter_line),
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
            generation: self.content_gen,
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
        let mut presented = if self.work.is_empty() {
            None
        } else {
            Some(VgaRaster {
                width: self.raster_width(),
                height: self.raster_height(),
                display_height: self.crtc.vdisp_end,
                generation: 0,
                pixels: self.work.clone(),
            })
        };
        if let Some(addr) = self.pending_start.take() {
            // A start-address latch changes the scanout origin with no VRAM/register
            // write of its own, so bump the content generation here (only when it
            // actually moves) so the host dirty-framebuffer cache re-renders.
            if addr != self.crtc.start_address {
                self.bump_content_gen();
            }
            self.crtc.start_address = addr; // latched for the next frame
        }
        if self.graphics_settle_frames != 0 {
            self.content_gen = self.content_gen.wrapping_add(1);
            self.graphics_settle_frames -= 1;
        }
        if let Some(raster) = &mut presented {
            raster.generation = self.content_gen;
        }
        if presented.is_some() {
            self.presented = presented;
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
}
