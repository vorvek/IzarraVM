//! The legacy VGA core: 256 KB planar VRAM, the VGA register blocks, a
//! cycle-coupled beam clock, and a catch-up rasterizer. This is Margo's
//! VGA-compatibility personality (one chip, one frame store, one RAMDAC).
//!
//! It also carries the text/Mode-13h personality: the 80x25 character buffer,
//! the linear Mode 13h framebuffer, the RAMDAC, and the CRTC text cursor.

use crate::{
    DAC_ENTRIES, Dac, Framebuffer, TextCell, TextFrame, VGA_TEXT_COLUMNS, VGA_TEXT_MEMORY_SIZE,
    VGA_TEXT_ROWS, VideoError, VideoMode,
};

pub const VGA_PLANE_SIZE: usize = 64 * 1024;
pub const VGA_PLANES: usize = 4;
pub const VGA_PLANAR_SIZE: usize = VGA_PLANE_SIZE * VGA_PLANES; // 256 KB

/// CRTC vertical/horizontal timing, in scan-counter (undoubled) units.
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
}

impl CrtcTiming {
    /// Standard 80x25 text (mode 03h): 70 Hz, 9-dot chars. Boot default so the
    /// beam math is valid before any graphics mode-set.
    pub fn text_03h() -> Self {
        Self {
            htotal_chars: 100, char_width: 9, hdisp_end: 720,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 15, double_scan: false,
            start_address: 0, offset: 80,
        }
    }

    /// Mode 0Dh: 320x200x16 planar, 70 Hz, double-scanned, 8-dot chars.
    pub fn mode_0dh() -> Self {
        Self {
            htotal_chars: 100, char_width: 8, hdisp_end: 320,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 1, double_scan: true,
            start_address: 0, offset: 20,
        }
    }

    /// Total dots per frame = htotal_dots * vtotal (scan-counter lines).
    pub fn frame_dots(&self) -> u64 {
        (self.htotal_chars * self.char_width) as u64 * self.vtotal as u64
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
    pub palette: [u8; 16], // idx 0..15
    pub mode_control: u8,  // idx 0x10
    pub overscan: u8,      // idx 0x11
    pub plane_enable: u8,  // idx 0x12
    pub pixel_pan: u8,     // idx 0x13, low 4 bits
    pub color_select: u8,  // idx 0x14
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
    // Legacy text/Mode-13h/RAMDAC/cursor personality, folded in from VgaTextMode.
    pub(crate) text_memory: [u8; VGA_TEXT_MEMORY_SIZE],
    pub(crate) mode13h: Framebuffer,
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
            mode13h: Framebuffer::mode13h(),
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

    /// Switch to mode 0Dh timing and reset the beam to the top of frame.
    pub fn set_mode_0dh(&mut self) {
        self.crtc = CrtcTiming::mode_0dh();
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Planar;
        self.presented = None; // drop any stale frame from a prior mode
        self.resize_work();
    }

    pub fn raster_width(&self) -> u32 {
        self.crtc.hdisp_end
    }

    fn scan_factor(&self) -> u32 { if self.crtc.double_scan { 2 } else { 1 } }

    /// Full visible frame height in raster (doubled) lines: the whole vtotal span,
    /// with blank rendered black so a short raster's letterbox is present.
    pub fn raster_height(&self) -> u32 {
        self.crtc.vtotal * self.scan_factor()
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

    /// Assemble one active scanline (`src_line` in undoubled active space) into
    /// `hdisp_end` DAC indices, applying pel-pan and the attribute palette.
    pub fn render_active_row(&self, src_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        let pan = (self.attr.pixel_pan & 0x0F) as usize;
        let mut row = vec![0u8; width];
        // byte pitch per plane = 2 * offset register
        let row_base = self.crtc.start_address as usize
            + src_line as usize * self.crtc.offset as usize * 2;
        for (x, slot) in row.iter_mut().enumerate() {
            let px = x + pan;
            let byte = px / 8;
            let bit = 7 - (px % 8);
            let off = (row_base + byte) % VGA_PLANE_SIZE;
            let mut index = 0u8;
            for plane in 0..VGA_PLANES {
                let b = self.vram[plane * VGA_PLANE_SIZE + off];
                index |= ((b >> bit) & 1) << plane;
            }
            *slot = self.attr.palette[index as usize] & 0x3F;
        }
        row
    }

    fn region_color(&self, scan_line: u32) -> u8 {
        // scan_line in undoubled counter space; caller guarantees scan_line >= vdisp_end.
        if scan_line < self.crtc.vblank_start || scan_line >= self.crtc.vblank_end {
            self.attr.overscan & 0x3F // border = overscan color
        } else {
            0 // vertical blank = black
        }
    }

    /// Render one undoubled counter line, emitting `scan_factor` doubled raster
    /// rows. `catch_up` and `render_full_frame` both work in counter lines, the
    /// same space the beam counts in.
    fn render_scanline(&mut self, counter_line: u32) {
        let factor = self.scan_factor();
        let width = self.raster_width() as usize;
        let pixels = if counter_line < self.crtc.vdisp_end {
            self.render_active_row(counter_line)
        } else {
            vec![self.region_color(counter_line); width]
        };
        for sub in 0..factor {
            let raster_line = counter_line * factor + sub;
            let dst = raster_line as usize * width;
            if dst + width <= self.work.len() {
                self.work[dst..dst + width].copy_from_slice(&pixels);
            }
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
        VgaRaster { width: w, height: h, pixels: self.work.clone() }
    }

    fn finalize_frame(&mut self) {
        // Render the lines the beam has not yet crossed, with the current register
        // state, so a mid-frame change shows below the seam.
        while self.last_line < self.crtc.vtotal {
            self.render_scanline(self.last_line);
            self.last_line += 1;
        }
        // `work` is sized only in planar mode; text/13h leave it empty, so a frame
        // built from it would have a pixel count that mismatches width*height. Only
        // publish a raster when there is real planar content to show.
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
            0x3C4 => { self.seq_index = value; true }
            0x3C5 => { let idx = self.seq_index; self.write_seq(idx, value); true }
            0x3C7 => { self.dac.set_read_index(value); true }
            0x3C8 => { self.dac.set_write_index(value); true }
            0x3C9 => { self.dac.write_data(value); true }
            0x3CE => { self.gc_index = value; true }
            0x3CF => { let idx = self.gc_index; self.write_gc(idx, value); true }
            0x3D4 => { self.crtc_index = value; true }
            0x3D5 => { let idx = self.crtc_index; self.write_crtc(idx, value); true }
            0x3C0 => { self.write_attr(value); true }
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
            0x04 => self.seq.memory_mode = value,
            _ => {}
        }
    }

    fn write_gc(&mut self, index: u8, value: u8) {
        match index {
            0x00 => self.gc.set_reset = value & 0x0F,
            0x01 => self.gc.enable_set_reset = value & 0x0F,
            0x02 => self.gc.color_compare = value & 0x0F,
            0x03 => { self.gc.rotate = value & 7; self.gc.logic = (value >> 3) & 3; }
            0x04 => self.gc.read_map = value & 3,
            0x05 => { self.gc.write_mode = value & 3; self.gc.read_mode = (value >> 3) & 1; }
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
            _ => {} // full timing programmed via set_mode_0dh in slice 1
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

    pub fn mode13h_framebuffer(&self) -> &Framebuffer {
        &self.mode13h
    }

    pub fn read_mode13h_u8(&self, offset: usize) -> Result<u8, VideoError> {
        self.mode13h
            .indexed_pixels
            .get(offset)
            .copied()
            .ok_or(VideoError::Mode13hOutOfBounds { offset })
    }

    pub fn write_mode13h_u8(&mut self, offset: usize, value: u8) -> Result<(), VideoError> {
        let slot = self
            .mode13h
            .indexed_pixels
            .get_mut(offset)
            .ok_or(VideoError::Mode13hOutOfBounds { offset })?;
        *slot = value;
        Ok(())
    }

    pub fn set_mode13h(&mut self) {
        self.mode13h = Framebuffer::mode13h();
        self.mode = VideoMode::Mode13h;
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
                if (gc.set_reset >> plane) & 1 != 0 { 0xFF } else { 0x00 } // WM3 color
            }
            _ => {
                // WM0: set/reset substitution where enabled, else rotated data.
                if (gc.enable_set_reset >> plane) & 1 != 0 {
                    if (gc.set_reset >> plane) & 1 != 0 { 0xFF } else { 0x00 }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beam_position_tracks_dots_in_scan_counter_units() {
        let t = CrtcTiming::mode_0dh();
        let htotal = (t.htotal_chars * t.char_width) as u64; // 800
        let dots = htotal * 5 + 10; // 5 full lines + 10 dots
        assert_eq!(beam_line(&t, dots), 5);
        assert_eq!(beam_dot(&t, dots), 10);
        assert!(beam_display_enable(&t, dots)); // line 5 < 400, dot 10 < 320
        assert!(!beam_vretrace(&t, dots));       // 5 < vretrace_start 412
    }

    #[test]
    fn advance_rolls_over_one_frame_in_o1() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        let frame = vga.frame_dots();
        vga.advance(frame * 2 + 7); // just past two frames in one call
        assert_eq!(vga.beam_dots(), 7);          // (2*frame+7) mod frame
        assert_eq!(vga.frames_completed(), 2);
    }

    #[test]
    fn boots_with_defined_frame_dots_and_zeroed_vram() {
        let vga = Vga::default();
        assert_eq!(vga.vram.len(), VGA_PLANAR_SIZE);
        assert!(vga.vram.iter().all(|&b| b == 0));
        // frame_dots must be non-zero at boot (default text timing) so the
        // per-instruction beam advance never divides by zero. (Spec §3/§6.)
        assert!(vga.frame_dots() > 0, "frame_dots must be defined before any mode-set");
    }

    #[test]
    fn write_mode_0_applies_rotate_setreset_logic_and_bitmask() {
        // Latches preloaded to 0xFF on all planes; write 0x0F with bit mask 0xF0,
        // copy logic, no set/reset. Result per plane = (data & mask) | (latch & !mask)
        // = (0x0F & 0xF0) | (0xFF & 0x0F) = 0x00 | 0x0F = 0x0F.
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.bit_mask = 0xF0;
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
        let mut gc = GfxController::default();
        gc.bit_mask = 0xFF;
        gc.enable_set_reset = 0x0F;
        gc.set_reset = 0b1010;
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
        let mut gc = GfxController::default();
        gc.write_mode = 1;
        let latches = [0x12, 0x34, 0x56, 0x78];
        write_planes(&mut planes, 0x00, &gc, &latches); // data ignored in WM1
        for plane in 0..VGA_PLANES {
            assert_eq!(planes[plane][0], latches[plane]);
        }
    }

    #[test]
    fn write_mode_2_expands_color_nibble_per_plane() {
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.write_mode = 2;
        gc.bit_mask = 0xFF;
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
        let mut gc = GfxController::default();
        gc.write_mode = 3;
        gc.set_reset = 0b0011;
        gc.bit_mask = 0xFF;
        gc.rotate = 0;
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
        let mut gc = GfxController::default();
        gc.read_map = 2;
        let mut latches = [0u8; VGA_PLANES];
        let byte = read_planes(&planes, &gc, &mut latches);
        assert_eq!(byte, 0x33);
        assert_eq!(latches, [0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn read_mode_1_color_compares_each_bit() {
        let planes = [[0xFFu8; 1], [0x00u8; 1], [0xFFu8; 1], [0x00u8; 1]];
        let mut gc = GfxController::default();
        gc.read_mode = 1;
        gc.color_dont_care = 0x0F; // care about all four planes
        gc.color_compare = 0b0101; // planes 0 and 2 set, 1 and 3 clear
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
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() { *b = 0xFF; } // plane 0 set
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        let raster = vga.render_full_frame();
        let w = raster.width as usize;
        assert_ne!(raster.pixels[0], 0, "row 0 should be active (top-justified)");
        let last = (raster.height as usize - 1) * w;
        assert_eq!(raster.pixels[last], 0, "bottom row is border/blank, not active");
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
        assert_eq!(vga.crtc.start_address, 0, "start address unchanged this frame");
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
        for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() { *b = 0xFF; }
        vga.attr.palette = core::array::from_fn(|i| i as u8); // index 1 -> DAC 1
        // Run to counter line 50, then repaint palette[1] = 9 via the attribute port.
        vga.advance(htotal_dots(&vga.crtc) * 50);
        vga.write_port(0x3C0, 0x01); // attr index 1
        vga.write_port(0x3C0, 9);    // palette[1] = 9
        // Finish the frame.
        vga.advance(vga.frame_dots());
        let raster = vga.take_presented().unwrap();
        let w = raster.width as usize;
        assert_eq!(raster.pixels[0], 1, "above the split uses the old palette");
        let below = 120 * w; // raster row 120 (counter line 60, > split at 50)
        assert_eq!(raster.pixels[below], 9, "below the split uses the new palette");
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
    fn mode13h_memory_write_updates_framebuffer() {
        let mut video = Vga::default();
        video.set_mode13h();
        video.write_mode13h_u8(123, 0x2a).unwrap();

        assert_eq!(video.mode13h_framebuffer().indexed_pixels[123], 0x2a);
        assert_eq!(video.read_mode13h_u8(123).unwrap(), 0x2a);
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
}
