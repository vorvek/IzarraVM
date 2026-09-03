// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The legacy VGA core: 256 KB planar VRAM, the VGA register blocks, a
//! cycle-coupled beam clock, and a catch-up rasterizer. This is Margo's
//! VGA-compatibility personality (one chip, one frame store, one RAMDAC).
//!
//! It also carries the text personality: the 80x25 character buffer, the
//! RAMDAC, and the CRTC text cursor. Chained mode 13h routes through the same
//! raster engine as the planar and mode-X paths; chain-4 only rewrites the CPU
//! write/read decode.

use std::cell::{Cell, RefCell};

use izarravm_bus::PageAlignedBytes;

use crate::{
    DAC_ENTRIES, Dac, ModeCensus, ModeCensusKey, TextCell, TextFrame, VGA_MODE13H_BASE,
    VGA_MONO_TEXT_BASE, VGA_PLANAR_WINDOW_SIZE, VGA_TEXT_BASE, VGA_TEXT_COLUMNS,
    VGA_TEXT_MEMORY_SIZE, VGA_TEXT_ROWS, VideoError, VideoMode, bits_per_pixel,
};
mod datapath;
mod legacy;
mod scanout;
mod timing;

pub use datapath::{
    GfxAperture, GfxController, display_counter, display_offset, display_offset_row, read_planes,
    write_planes,
};
use legacy::{CGA_BLACK, CGA_WHITE, HGC_MODE_GRAPHICS, HGC_MODE_VIDEO_ENABLE};
#[cfg(test)]
use legacy::{
    CGA_BROWN, CGA_CYAN, CGA_GREEN, CGA_LIGHT_CYAN, CGA_LIGHT_GRAY, CGA_LIGHT_GREEN,
    CGA_LIGHT_MAGENTA, CGA_LIGHT_RED, CGA_MAGENTA, CGA_RED, CGA_YELLOW, HGC_MODE_PAGE1,
};
pub use legacy::{
    CGA_BYTES_PER_LINE, CGA_FB_SIZE, CGA_ODD_BANK, Cga, CgaMode, HGC_BANK_SIZE, HGC_BYTES_PER_LINE,
    HGC_FB_SIZE, HGC_PAGE1_OFFSET, Hgc,
};
pub use scanout::VgaRaster;
pub use timing::{
    Attribute, CrtcRegs, CrtcTiming, Sequencer, beam_display_enable, beam_dot, beam_hsync,
    beam_line, beam_vretrace, dots_until_status1_bit_change, dots_until_vretrace_start,
    htotal_dots, status1_geometry_bits,
};
use timing::{
    VGA_COLOR_SWITCH_SENSE, VGA_DOT_CLOCK_25_HZ, VGA_DOT_CLOCK_28_HZ, vgabios_attr_regs,
    vgabios_crtc_regs, vgabios_gc_regs, vgabios_seq_regs,
};

pub const VGA_PLANE_SIZE: usize = 64 * 1024;
pub const VGA_PLANES: usize = 4;
pub const VGA_PLANAR_SIZE: usize = VGA_PLANE_SIZE * VGA_PLANES; // 256 KB

const CGA_MODE_80_COLUMNS: u8 = 0x01;
const CGA_MODE_GRAPHICS: u8 = 0x02;
const CGA_MODE_BW: u8 = 0x04;
const CGA_MODE_VIDEO_ENABLE: u8 = 0x08;
const CGA_MODE_HIGH_RES: u8 = 0x10;
const CGA_MODE_BLINK: u8 = 0x20;
const MODE13_DIRECT_PAGES: usize = VGA_PLANAR_WINDOW_SIZE as usize / 0x1000;

#[derive(Debug, Clone)]
struct Mode13ArgbCache {
    words: Vec<u32>,
    palette: [u32; DAC_ENTRIES],
    width: usize,
    height: usize,
    display_height: usize,
    frame: u64,
    valid: bool,
    converted_pixels: usize,
}

impl Default for Mode13ArgbCache {
    fn default() -> Self {
        Self {
            words: Vec::new(),
            palette: [0; DAC_ENTRIES],
            width: 0,
            height: 0,
            display_height: 0,
            frame: 0,
            valid: false,
            converted_pixels: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Vga {
    // Both VRAM buffers back `DirectPage` pointers, so both live in 4096-aligned windows —
    // the one-lookup store table's tag bits need every direct-page host pointer page-aligned,
    // and `vram` (the Mode X path) is doom's, not just `mode13_linear` (chained 13h). A plain
    // Vec here silently degrades every aperture store to the slow path.
    pub(crate) vram: PageAlignedBytes,
    // Derived cache for the canonical chained Mode 13h layout. Planar VRAM is
    // authoritative; any write through a planar decode invalidates this cache.
    pub(crate) mode13_linear: PageAlignedBytes,
    pub(crate) mode13_linear_valid: bool,
    // Direct CPU pages write the linear cache without passing through a byte
    // mutator. Keep it authoritative until a planar or noncanonical path needs
    // the four VGA planes, then materialize all 64 KiB once.
    pub(crate) mode13_linear_authoritative: bool,
    mode13_dirty_pages: Cell<u16>,
    mode13_direct_batch_dirty: bool,
    graphics_settle_frames: u8,
    /// The mode the `work` raster was last sized or cleared for. `resize_work`
    /// keeps a same-size buffer only while this still matches the active mode.
    work_mode: VideoMode,
    /// Every geometry this guest programmed, and how many times. Recorded in
    /// `resize_work`, which every CRTC timing recompute calls, so a guest that
    /// replays its register table each frame is visible as a large count.
    mode_census: ModeCensus,
    mode13_argb_full_dirty: Cell<bool>,
    mode13_argb_cache: RefCell<Mode13ArgbCache>,
    pub(crate) crtc: CrtcTiming,
    pub(crate) crtc_regs: CrtcRegs,
    pub(crate) seq: Sequencer,
    pub(crate) gc: GfxController,
    pub(crate) attr: Attribute,
    pub(crate) latches: [u8; VGA_PLANES],
    pub(crate) beam: u64,
    pub(crate) last_line: u32,
    pub(crate) frames: u64,
    /// True while a Voodoo-style device owns the monitor cable this tick, set
    /// each call by `advance_cable_aware`. Gates the per-pixel render work in
    /// `finalize_frame`/`catch_up` only -- it never touches beam timing, the
    /// frame counter above, or any guest-visible register.
    pub(crate) skip_render: bool,
    /// Text rows actually rendered by `render_scanline`'s `VideoMode::Text`
    /// arm. A diagnostic counter, not gameplay state: it is what makes "zero
    /// rows rendered while the cable is elsewhere" a checkable fact instead of
    /// an assertion about wall time.
    pub(crate) text_rows_rendered: u64,
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
    pub(crate) hgc: Hgc,
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
        for cell in text_memory.as_chunks_mut::<2>().0 {
            cell[0] = b' ';
            cell[1] = 0x07;
        }

        let mut vga = Self {
            vram: PageAlignedBytes::zeroed(VGA_PLANAR_SIZE),
            mode13_linear: PageAlignedBytes::zeroed(0x10000),
            mode13_linear_valid: true,
            mode13_linear_authoritative: false,
            mode13_dirty_pages: Cell::new(0),
            mode13_direct_batch_dirty: false,
            graphics_settle_frames: 0,
            work_mode: VideoMode::Text,
            mode_census: ModeCensus::default(),
            mode13_argb_full_dirty: Cell::new(true),
            mode13_argb_cache: RefCell::new(Mode13ArgbCache::default()),
            crtc: CrtcTiming::text_03h(),
            crtc_regs: CrtcRegs::default(),
            seq: Sequencer::default(),
            gc: GfxController::default(),
            attr: Attribute::default(),
            latches: [0; VGA_PLANES],
            beam: 0,
            last_line: 0,
            frames: 0,
            skip_render: false,
            text_rows_rendered: 0,
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
            hgc: Hgc::default(),
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

    /// Dots from the current beam position to the next vertical-retrace START
    /// edge (the first dot of scanline `vretrace_start`). Pure geometry from the
    /// live `CrtcTiming` and the beam; the beam itself is not moved.
    ///
    /// If the beam sits at or past the edge on this frame (on the edge, inside
    /// the retrace window, or in the bottom border below it), the result is the
    /// NEXT frame's edge, up to a full frame ahead, so the returned distance is
    /// always >= 1. Returns `None` when the CRTC is not programmed (zero-dot
    /// frame) or the retrace edge falls outside the frame, so callers skip
    /// edge-aware scheduling.
    pub fn dots_until_vretrace_start(&self) -> Option<u64> {
        timing::dots_until_vretrace_start(&self.crtc, self.beam)
    }

    /// Dots from a caller-supplied beam to the first positive transition of
    /// Input Status 1 bit 3 (vertical retrace) or bit 0 (display inactive).
    /// `target` is the value after the transition. Geometry and output-enable
    /// state are inspected without moving the live beam.
    pub fn dots_until_status1_bit_change_from(
        &self,
        beam: u64,
        bit: u8,
        target: bool,
    ) -> Option<u64> {
        timing::dots_until_status1_bit_change(&self.crtc, beam, bit, target, || {
            self.display_forced_inactive()
        })
    }

    /// The VGA core's own "the display is not refreshing at all" verdict, from
    /// the registers outside the CRTC that can blank the screen. Margo's VBE
    /// modes reach none of these, which is why the geometry helpers take it as a
    /// parameter instead of reading it themselves.
    fn display_forced_inactive(&self) -> bool {
        !self.display_refresh_enabled
            || !self.attr.pas
            || !self.sequencer_outputs_enabled()
            || (self.is_cga_personality() && self.cga.mode_control & CGA_MODE_VIDEO_ENABLE == 0)
    }

    /// Whether 0x3DA is the active color Input Status 1 alias.
    pub fn color_status1_port_active(&self) -> bool {
        self.status1_port_selected(0x3da)
    }

    /// Whether `port` is the ACTIVE Input Status 1 alias (0x3DA in a color
    /// setup, 0x3BA in a mono one). The inactive alias is not decoded at all --
    /// no bits, and no side effects either.
    ///
    /// Public because Margo's scanout timing owns the BITS of a status read
    /// while a VBE mode is on screen, but the VGA core still owns which PORT is
    /// decoded and what a read does to the chip; splitting those two would let
    /// the aliasing rule drift between the two display owners.
    pub fn status1_port_active(&self, port: u16) -> bool {
        self.status1_port_selected(port)
    }

    /// Input Status 0's display-switch sense bit (bit 4), which is a wired strap
    /// selected by Misc Output and has nothing to do with the beam. Exposed for
    /// the same reason as `status1_port_active`: while Margo is scanning out,
    /// bit 7 of 0x3C2 comes from Margo's frame clock but bit 4 still comes from
    /// here.
    pub fn status0_switch_sense_bits(&self) -> u8 {
        if self.switch_sense_bit() { 0x10 } else { 0x00 }
    }

    pub fn dot_clock_hz(&self) -> u64 {
        match (self.misc_output >> 2) & 0x03 {
            0x01 => VGA_DOT_CLOCK_28_HZ,
            // External/reserved VGA clocks fall back to the stock 25 MHz clock.
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
    /// table the cell is 256-glyph and bit 3 stays foreground intensity.
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
    /// source so they stay in lockstep.
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
    /// Distinct from the machine's `set_selected_text_scanlines` (AH=12h BL=30h),
    /// which only records BIOS mode-set policy in the BDA and does not touch
    /// this CRTC register.
    pub fn set_char_height(&mut self, height: u8) {
        let max_scan = u32::from(height.saturating_sub(1));
        if self.crtc.max_scan != max_scan {
            self.sync_mode13_planar();
            self.crtc.max_scan = max_scan;
        }
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

    fn rebuild_mode13_linear(&mut self) {
        if self.mode13_linear_valid {
            return;
        }
        for linear in 0..self.mode13_linear.len() {
            let plane = linear & 3;
            let plane_offset = linear >> 2;
            self.mode13_linear[linear] = self.vram[plane * VGA_PLANE_SIZE + plane_offset];
        }
        self.mode13_linear_valid = true;
    }

    fn sync_mode13_planar(&mut self) {
        if !self.mode13_linear_authoritative {
            return;
        }
        for (linear, &value) in self.mode13_linear.iter().enumerate() {
            let plane = linear & 3;
            let plane_offset = linear >> 2;
            self.vram[plane * VGA_PLANE_SIZE + plane_offset] = value;
        }
        self.mode13_linear_authoritative = false;
    }

    fn canonical_mode13_layout(&self) -> bool {
        self.mode == VideoMode::Mode13h
            && self.seq.memory_mode & 0x08 != 0
            && self.attr.pixel_pan == 0
            && self.crtc == CrtcTiming::mode13h()
    }

    /// Whether the CPU can use the stock chained Mode 13h aperture as flat
    /// memory. The machine uses this to invalidate cached direct pages only
    /// when a VGA write actually changes their availability.
    /// Whether the mode13 linear surface is the authoritative copy. Exposed so a
    /// test can assert that a Margo-banked write did NOT set it -- the silent
    /// half of the dirty-notification routing, which sets this from Margo offsets
    /// when the guest was in mode 13h before its 4F02.
    pub fn mode13_linear_authoritative(&self) -> bool {
        self.mode13_linear_authoritative
    }

    pub fn mode13h_direct_page_available(&self) -> bool {
        let aperture = self.gc.aperture();
        self.canonical_mode13_layout()
            && self.pending_start.is_none()
            && self.video_memory_enabled()
            && aperture.base == VGA_MODE13H_BASE
            && aperture.length == VGA_PLANAR_WINDOW_SIZE
            && aperture.graphics
    }

    fn mode_x_direct_write_plane(&self) -> Option<usize> {
        let aperture = self.gc.aperture();
        let mask = self.seq.map_mask;
        (self.mode == VideoMode::ModeX
            && self.seq.memory_mode & 0x08 == 0
            && self.seq.memory_mode & 0x04 != 0
            && mask.is_power_of_two()
            && mask <= 0x08
            && self.gc.write_mode == 0
            && self.gc.rotate == 0
            && self.gc.logic == 0
            && self.gc.enable_set_reset == 0
            && self.gc.bit_mask == 0xff
            && self.video_memory_enabled()
            && aperture.base == VGA_MODE13H_BASE
            && aperture.length == VGA_PLANAR_WINDOW_SIZE
            && aperture.graphics)
            .then(|| mask.trailing_zeros() as usize)
    }

    /// The index register in force for `port`, sampled BEFORE a write so an indexed data write can
    /// be attributed to the register it actually lands on. Zero for a port that is not an indexed
    /// data port. Diagnostic only: `write_port` reads these fields itself and does not call this.
    pub fn port_index_selector(&self, port: u16) -> u8 {
        match port {
            0x3C5 => self.seq_index,
            0x3CF => self.gc_index,
            0x3C0 => self.attr.index,
            0x3D1 | 0x3D3 | 0x3D5 | 0x3D7 if self.is_cga_personality() => self.crtc_index,
            port if self.crtc_data_port_selected(port) => self.crtc_index,
            _ => 0,
        }
    }

    /// Stable description of the current direct VGA write mapping. Zero means
    /// no mapping, one is canonical Mode 13h, and values two through five select
    /// a Mode X plane. The machine compares this across register writes so a
    /// cached page cannot keep targeting an old plane.
    pub fn direct_write_token(&self) -> u8 {
        if self.mode13h_direct_page_available() {
            1
        } else {
            self.mode_x_direct_write_plane()
                .map_or(0, |plane| plane as u8 + 2)
        }
    }

    /// Return one directly writable VGA aperture page when the current write
    /// datapath is a transparent byte copy. Mode X points at the sole plane
    /// selected by the map mask. Reads still use the VGA handler and latches.
    pub fn direct_write_page_ptr(&mut self, offset: usize) -> Option<*mut u8> {
        if self.mode13h_direct_page_available() {
            return self.mode13h_direct_page_ptr(offset);
        }
        let plane = self.mode_x_direct_write_plane()?;
        if offset
            .checked_add(0x1000)
            .is_none_or(|end| end > VGA_PLANE_SIZE)
        {
            return None;
        }
        self.sync_mode13_planar();
        Some(self.vram[plane * VGA_PLANE_SIZE + offset..].as_mut_ptr())
    }

    /// Return one page of the canonical chained Mode 13h aperture. The backing
    /// buffer is stable for the lifetime of this VGA instance. Callers must use
    /// `note_mode13h_direct_write` before mutating the returned page.
    pub fn mode13h_direct_page_ptr(&mut self, offset: usize) -> Option<*mut u8> {
        if !self.mode13h_direct_page_available()
            || offset
                .checked_add(0x1000)
                .is_none_or(|end| end > self.mode13_linear.len())
        {
            return None;
        }
        self.rebuild_mode13_linear();
        Some(self.mode13_linear[offset..].as_mut_ptr())
    }

    /// Mark a write made through a direct Mode 13h page. Content generation is
    /// committed by `finish_mode13h_direct_batch`, once for the whole bus batch.
    pub fn note_mode13h_direct_write(&mut self, offset: usize, bytes: usize) {
        debug_assert!(self.mode13h_direct_page_available());
        self.note_direct_write(offset, bytes);
    }

    /// Mark a write made through any direct VGA page. Content generation is
    /// committed by `finish_direct_write_batch`, once for the whole bus batch.
    pub fn note_direct_write(&mut self, offset: usize, bytes: usize) {
        debug_assert_ne!(self.direct_write_token(), 0);
        if bytes == 0 || offset >= self.mode13_linear.len() {
            return;
        }
        let last = offset
            .saturating_add(bytes - 1)
            .min(self.mode13_linear.len() - 1);
        let first_page = offset >> 12;
        let last_page = last >> 12;
        let mut dirty = self.mode13_dirty_pages.get();
        for page in first_page..=last_page.min(MODE13_DIRECT_PAGES - 1) {
            dirty |= 1 << page;
        }
        self.note_direct_write_pages(dirty);
    }

    /// Mark the exact set of canonical Mode 13h pages written by one native CPU block.
    pub fn note_mode13h_direct_pages(&mut self, dirty_pages: u16) {
        debug_assert!(self.mode13h_direct_page_available());
        self.note_direct_write_pages(dirty_pages);
    }

    /// Mark the aperture pages written through either canonical Mode 13h or the
    /// transparent single-plane Mode X path.
    pub fn note_direct_write_pages(&mut self, dirty_pages: u16) {
        let token = self.direct_write_token();
        debug_assert_ne!(token, 0);
        if dirty_pages == 0 {
            return;
        }
        if token == 1 {
            self.mode13_dirty_pages
                .set(self.mode13_dirty_pages.get() | dirty_pages);
            self.mode13_linear_valid = true;
            self.mode13_linear_authoritative = true;
        } else {
            self.mode13_linear_valid = false;
            self.mode13_linear_authoritative = false;
        }
        self.mode13_direct_batch_dirty = true;
        self.arm_graphics_settle();
    }

    pub fn finish_mode13h_direct_batch(&mut self) {
        self.finish_direct_write_batch();
    }

    pub fn finish_direct_write_batch(&mut self) {
        if self.mode13_direct_batch_dirty {
            self.content_gen = self.content_gen.wrapping_add(1);
            self.mode13_direct_batch_dirty = false;
        }
    }

    /// Install a planar mode's timing and reset the beam to the top of frame.
    fn set_planar_mode(&mut self, mode: u8, timing: CrtcTiming, clear: bool) {
        self.sync_mode13_planar();
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
            self.mode13_linear.fill(0);
            self.mode13_linear_valid = true;
            self.mode13_linear_authoritative = false;
        }
        self.presented = None; // drop any stale frame from a prior mode
        self.pending_start = None; // the mode set reprograms the start address
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

    pub fn plane_byte(&self, plane: usize, offset: usize) -> u8 {
        if self.mode13_linear_authoritative && plane < VGA_PLANES && offset < 0x4000 {
            return self.mode13_linear[(offset << 2) | plane];
        }
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
        self.sync_mode13_planar();
        self.mode13_linear_valid = false;
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
        self.sync_mode13_planar();
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
        self.sync_mode13_planar();
        self.mode13_linear_valid = false;
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
        let cacheable = self.mode == VideoMode::Mode13h && offset < self.mode13_linear.len();
        if cacheable {
            self.mode13_linear[offset] = data;
        } else if offset >> 2 < VGA_PLANE_SIZE {
            self.mode13_linear_valid = false;
        }
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
        if self.mode13_linear_authoritative && offset < self.mode13_linear.len() {
            return self.mode13_linear[offset];
        }
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
        self.sync_mode13_planar();
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

    /// Read the Hercules CRT status register (port 3BAh). Unlike the VGA/MDA
    /// status1 layout this shares the 3BAh address with, real Hercules hardware
    /// puts unrelated bits here (seasip.info "Hercules Graphics Card Plus
    /// Notes"):
    ///
    /// Bit 0: horizontal retrace active.
    /// Bit 1: light pen switch triggered (this core has no light pen; always 0).
    /// Bit 3: video pixel output (the current beam position is lit, i.e. the
    ///   framebuffer bit at the beam's dot is set) -- only meaningful while the
    ///   beam is in the active display area.
    /// Bits 6-4: card ID (0 here; this core does not claim HGC+ id 001b).
    /// Bit 7: vertical sync, active LOW (0 during vsync, 1 otherwise) -- the
    ///   inverse polarity of the VGA/CGA status1 vertical-retrace bit, and the
    ///   bit the classic HGC detection loop polls for a 0->1 (or 1->0) edge.
    fn read_hgc_status(&mut self) -> u8 {
        self.catch_up();
        let beam = self.beam;
        self.hgc_status_bits(beam)
    }

    /// Pure HGC 3BAh bit computation off a caller-supplied beam, mirroring the
    /// `status1_bits` split so the lazy port-read path can pass a predicted
    /// beam (a detection loop polling bit 7 must see it toggle within a lazy
    /// batch). The bit-3 pixel sample renders a full row per read; HGC polls
    /// are not a performance target, so no per-pixel sampler exists for it.
    fn hgc_status_bits(&self, beam: u64) -> u8 {
        let mut status = 0u8;
        if beam_hsync(&self.crtc, beam) {
            status |= 0x01;
        }
        if beam_display_enable(&self.crtc, beam) {
            let line = beam_line(&self.crtc, beam);
            let dot = beam_dot(&self.crtc, beam) as usize;
            if self.render_hgc_row(line).get(dot).copied().unwrap_or(0) != 0 {
                status |= 0x08;
            }
        }
        if !beam_vretrace(&self.crtc, beam) {
            status |= 0x80;
        }
        status
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
    ///
    /// Composed of `status1_side_effects` (the two guest-visible mutations: the
    /// raster catch-up and the attribute flip-flop reset) plus `status1_bits`
    /// (the pure bit computation off `self.beam`). The lazy port-read path
    /// (`MachineBus::read_io`, Approximate timing class) calls the same two
    /// pieces but passes a predicted beam position to `status1_bits` instead of
    /// `self.beam`, so both callers share exactly one bit-computation
    /// implementation.
    pub fn read_status1(&mut self) -> u8 {
        if self.is_hercules_personality() {
            return self.read_hgc_status();
        }
        self.status1_side_effects();
        let beam = self.beam;
        self.status1_bits(beam)
    }

    /// The guest-visible side effects of a 3DA/3BA read: catch the raster up to
    /// the live beam (like a register write) and reset the Attribute Controller
    /// address/data flip-flop. Every 3DA/3BA read performs these regardless of
    /// timing class or lazy/non-lazy dispatch; only the returned status BITS
    /// differ between the accurate (`self.beam`) and lazy (predicted beam) paths.
    pub fn status1_side_effects(&mut self) {
        self.catch_up(); // a 3DA read catches the raster up, like a register write
        self.attr.flip_flop_data = false; // reading 3DA resets the flip-flop
    }

    /// Pure bit computation for Input Status Register 1, off a caller-supplied
    /// beam dot position instead of the live `self.beam`. The lazy port-read
    /// path (Approximate timing class) calls this with `MachineBus::predicted_beam()`
    /// after running `status1_side_effects`; `read_status1` calls it with the
    /// live `self.beam` unchanged. `video_status_mux_bits` (the DAC pixel
    /// readback bits) recomputes its color live from current VRAM/register
    /// state for the given beam rather than reading the `catch_up`-rendered
    /// `self.work` buffer, so it is equally valid for a beam ahead of what
    /// `catch_up` has actually rendered.
    pub fn status1_bits(&self, beam: u64) -> u8 {
        // Bits 0 and 3 are pure geometry plus the blanking gates; the shared
        // helper is what keeps them identical to the Margo path and to the
        // analytic peek that predicts their transitions.
        let mut status =
            timing::status1_geometry_bits(&self.crtc, beam, self.display_forced_inactive());
        if self.is_cga_personality() {
            if self.cga.light_pen_triggered {
                status |= 0x02;
            }
            status |= 0x04; // no light pen switch is pressed/attached
        }
        status |= self.video_status_mux_bits(beam);
        status
    }

    /// Read Input Status Register 0 (port 3C2h).
    ///
    /// Bit 4: the display switch sense bit selected by Misc Output bits 3-2.
    /// Bit 7: vertical retrace active (the CRT interrupt status the BIOS polls).
    ///
    /// Composed of `catch_up` (the only guest-visible side effect of a 3C2
    /// read) plus `status0_bits` (the pure bit computation), mirroring
    /// `read_status1`/`status1_bits` so the lazy port-read path shares the same
    /// bit logic.
    pub fn read_status0(&mut self) -> u8 {
        self.catch_up(); // a 3C2 read catches the raster up, like 3DA
        let beam = self.beam;
        self.status0_bits(beam)
    }

    /// Pure bit computation for Input Status Register 0, off a caller-supplied
    /// beam dot position. See `status1_bits` for why this is safe to call with a
    /// beam ahead of the live `self.beam`.
    pub fn status0_bits(&self, beam: u64) -> u8 {
        let mut status = 0u8;
        if self.switch_sense_bit() {
            status |= 0x10;
        }
        if beam_vretrace(&self.crtc, beam) {
            status |= 0x80; // vertical retrace -> CRT interrupt status
        }
        status
    }

    /// Lazy-path status-port read for the Approximate timing class:
    /// handles exactly 3DA/3BA/3C2, the same three ports `read_port` routes to
    /// `read_status1`/`read_status0`, but computes the returned bits from a
    /// caller-supplied predicted beam (`MachineBus::predicted_beam()`) instead
    /// of the live `self.beam`. The handled arms perform the identical
    /// guest-visible side effects a non-lazy read would
    /// (`status1_side_effects`/`catch_up`). A poll on the currently-inactive
    /// status1 alias (e.g. 3BA in a color setup) returns `None` and performs
    /// NO side effects at all, exactly matching `read_port`'s existing
    /// `status1_port_selected` gating, where the inactive alias never reaches
    /// `read_status1` either. Also returns `None` for any other port (never
    /// reached by the caller, which dispatches by static port number before
    /// calling this).
    pub fn read_status_port_lazy(&mut self, port: u16, beam: u64) -> Option<u8> {
        match port {
            0x3C2 => {
                self.catch_up();
                Some(self.status0_bits(beam))
            }
            port if self.status1_port_selected(port) => {
                if self.is_hercules_personality() {
                    // HGC has no attribute flip-flop; the catch-up is the only
                    // side effect, and the bits come from the predicted beam
                    // like the VGA path so lazy poll loops observe the toggle.
                    self.catch_up();
                    return Some(self.hgc_status_bits(beam));
                }
                self.status1_side_effects();
                Some(self.status1_bits(beam))
            }
            _ => None,
        }
    }

    /// Write to a VGA I/O port. Calls `catch_up()` first so any lines already
    /// past the beam are rendered with the previous register state before the
    /// new value takes effect. Returns `true` if the port is handled.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.catch_up();
        let changes_mode13_layout = matches!(port, 0x3C2 | 0x3C3 | 0x3C5 | 0x3CF | 0x3D8 | 0x3B8)
            || self.crtc_data_port_selected(port)
            || (port == 0x3C0 && self.attr.flip_flop_data);
        if changes_mode13_layout {
            self.sync_mode13_planar();
        }
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
            // Hercules Mode Control (3B8h) and Configuration Switch (3BFh) are
            // specific mono-alias addresses on real hardware: they decode
            // regardless of the Misc Output color-emulation bit, unlike the
            // 3B4/3B5/3BA <-> 3D4/3D5/3DA aliasing pairs above.
            0x3B8 => {
                self.write_hgc_mode_control(value);
                true
            }
            0x3BF => {
                self.hgc.config_switch = value & 0x03;
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
            _ => {} // full timing is programmed via set_mode_0dh
        }
        // Always capture the raw vertical-timing bytes, even in text mode: a
        // register-banging 256-color guest (TSUMERA sets 320x240 mode X this way)
        // programs the full CRTC BEFORE the ATC write that flips the personality,
        // so these must survive to be decoded when the mode enters. (0x07/0x09/
        // 0x11 are already stored by the main match above, with their line-compare
        // side effects.)
        match index {
            0x06 => self.crtc_regs.r06 = value,
            0x10 => self.crtc_regs.r10 = value,
            0x12 => self.crtc_regs.r12 = value,
            0x15 => self.crtc_regs.r15 = value,
            0x16 => self.crtc_regs.r16 = value,
            _ => {}
        }
        // Graphics modes honor guest vertical CRTC timing. The absolute fields are
        // derived in recompute_vertical_timing; line-compare bits are handled above.
        // Text keeps its own timing (the recompute is gated), so the raw stores
        // above are inert there until a later mode flip decodes them.
        if matches!(
            self.mode,
            VideoMode::Planar | VideoMode::Mode13h | VideoMode::ModeX
        ) && matches!(index, 0x06 | 0x07 | 0x09 | 0x10 | 0x11 | 0x12 | 0x15 | 0x16)
        {
            self.recompute_vertical_timing();
        }
        // Mode X also honors the guest's horizontal group: a tweaked 256-color
        // mode carries its width in 00h/01h (see recompute_horizontal_timing for
        // why the other modes keep their table values).
        if self.mode == VideoMode::ModeX && matches!(index, 0x00 | 0x01) {
            self.recompute_horizontal_timing();
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
                0x10 => {
                    self.attr.mode_control = value;
                    self.maybe_enter_256color_from_registers();
                }
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
            self.mode13_linear.fill(0);
            self.mode13_linear_valid = true;
            self.mode13_linear_authoritative = false;
        }
        self.presented = None; // drop any stale frame from a prior mode
        self.pending_start = None; // the mode set reprograms the start address
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
        self.sync_mode13_planar();
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

    /// Write the Hercules Mode Control register (port 3B8h). Real HGC software
    /// always sets BIOS mode 07h first (MDA-compatible 80x25 mono text, already
    /// installed by `set_mono_text_mode`) and then bangs this port directly --
    /// there was never an INT 10h graphics mode number for Hercules graphics.
    /// The GRPH bit (bit 1) only takes effect once the Configuration Switch
    /// (3BFh) has set its allow-graphics bit; otherwise the card stays in
    /// whatever text/blank state it was in, matching real hardware where 3BFh
    /// gates what 3B8h may do. Video-enable (bit 3), blink (bit 5), and page
    /// select (bit 7) are always latched, even while graphics is refused.
    fn write_hgc_mode_control(&mut self, value: u8) {
        self.hgc.mode_control = value;
        if value & HGC_MODE_GRAPHICS != 0 && self.hgc.graphics_allowed() {
            if self.mode != VideoMode::Hercules {
                self.crtc = CrtcTiming::hgc_720x348();
                self.crtc_regs = CrtcRegs::from_timing(self.crtc);
                self.set_misc_mode_bits(1, false, 0x02);
                self.mode = VideoMode::Hercules;
                self.presented = None;
                self.pending_start = None;
                self.reset_palette_defaults(0x07);
                self.install_hgc_phosphor_palette();
                self.resize_work();
            }
        } else if self.mode == VideoMode::Hercules {
            // GRPH cleared (or graphics not/no-longer allowed): fall back to the
            // mono text personality, matching what real HGC software does after
            // it drops out of graphics (re-issue mode 07h).
            self.set_mono_text_mode();
        }
    }

    /// Install a monochrome phosphor DAC preset for Hercules graphics: index 0
    /// is black (background), index 1 is the classic P39 long-persistence green
    /// phosphor. Only DAC indices 0/1 are ever sampled by the 1bpp Hercules
    /// scanout (see `render_hgc_row`), so this is the whole palette that
    /// matters; text mode 07h is untouched (it keeps its own identity palette).
    fn install_hgc_phosphor_palette(&mut self) {
        if !self.default_palette_loading_enabled {
            return;
        }
        self.dac.set_entry(0, 0x00, 0x00, 0x00);
        self.dac.set_entry(1, 0x08, 0x2A, 0x0C); // P39 green phosphor
    }

    fn write_cga_mode_control(&mut self, value: u8) {
        self.sync_mode13_planar();
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
            self.graphics_settle_frames = 0;
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
        self.sync_mode13_planar();
        self.cursor_offset = 0;
        if clear {
            if self.crtc.char_width == 8 {
                for cell in self.cga.fb.as_chunks_mut::<2>().0 {
                    cell[0] = b' ';
                    cell[1] = 0x07;
                }
            } else {
                for cell in self.text_memory.as_chunks_mut::<2>().0 {
                    cell[0] = b' ';
                    cell[1] = 0x07;
                }
            }
        }
        self.beam = 0;
        self.last_line = 0;
        self.seq.reset = 0x03;
        self.mode = VideoMode::Text;
        self.graphics_settle_frames = 0;
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

    pub fn is_hercules_personality(&self) -> bool {
        self.mode == VideoMode::Hercules
    }

    /// True while a B8000 access should reach the second Hercules page: the
    /// Configuration Switch (3BFh) has paged it in. Consulted by the machine
    /// bus so it can decode B0000-B7FFF (page 0, always addressable in this
    /// personality) separately from B8000-BFFFF (page 1, gated).
    pub fn hgc_page1_addressable(&self) -> bool {
        self.hgc.page1_enabled()
    }

    /// Write one byte into the 64K Hercules graphics window: `offset` is
    /// B0000-relative (0..0x10000), page 0 at 0..0x8000, page 1 at
    /// 0x8000..0x10000. Both pages are simultaneously addressable on real
    /// hardware; only the CRTC scanout (`render_hgc_row`) is limited to one at
    /// a time. The four-way scanline interleave lives in the layout the guest
    /// writes, so the store is a flat copy, matching how `cga_write` works.
    pub fn hgc_write(&mut self, offset: usize, value: u8) {
        self.bump_content_gen();
        if let Some(slot) = self.hgc.fb.get_mut(offset & (HGC_FB_SIZE * 2 - 1)) {
            *slot = value;
        }
    }

    /// Read one byte from the 64K Hercules graphics window (see `hgc_write`).
    pub fn hgc_read(&self, offset: usize) -> u8 {
        self.hgc
            .fb
            .get(offset & (HGC_FB_SIZE * 2 - 1))
            .copied()
            .unwrap_or(0)
    }

    /// Assemble one Hercules graphics scanline into 720 DAC indices (index 0
    /// black, index 1 the phosphor color). The HGC's four-way interleave maps
    /// display scanline `y` to framebuffer bank `(y & 3) * HGC_BANK_SIZE`
    /// within the active page, generalizing CGA's two-bank even/odd scheme to
    /// four banks; 90 bytes/scanline, 1 bit per pixel, MSB first.
    pub fn render_hgc_row(&self, counter_line: u32) -> Vec<u8> {
        let width = HGC_BYTES_PER_LINE * 8;
        if self.hgc.mode_control & HGC_MODE_VIDEO_ENABLE == 0 {
            return vec![0u8; width];
        }
        let y = counter_line as usize;
        let bank = (y & 3) * HGC_BANK_SIZE;
        let base = self.hgc.active_page() * HGC_PAGE1_OFFSET;
        let row_base = base + bank + (y >> 2) * HGC_BYTES_PER_LINE;
        let mut row = vec![0u8; width];
        for byte_col in 0..HGC_BYTES_PER_LINE {
            let byte = self.hgc.fb.get(row_base + byte_col).copied().unwrap_or(0);
            for bit in 0..8 {
                let x = byte_col * 8 + bit;
                if x < width {
                    row[x] = u8::from((byte >> (7 - bit)) & 1 != 0);
                }
            }
        }
        row
    }

    pub fn hgc_mode_control(&self) -> u8 {
        self.hgc.mode_control
    }

    pub fn hgc_config_switch(&self) -> u8 {
        self.hgc.config_switch
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

    /// Derive the active pixel width and the horizontal total in `crtc` from the
    /// raw register bytes in `crtc_regs`. Used only while unchained (mode X).
    ///
    /// Horizontal Total (00h, +5) and End Horizontal Display (01h, +1) count
    /// CHARACTER clocks, and a 256-color pixel takes two dot clocks (ATC Mode
    /// Control 10h bit 6, 8-bit color), so the active width is
    /// `(r01 + 1) * char_width / 2`: the standard 80 characters give 320, and
    /// Abrash's wide mode X (Black Book ch.47) gives 360 from 90 characters at
    /// the 28.322 MHz clock. DOS Quake's 360x200/240/400/480 modes are that
    /// register set. Without this decode a 360-wide guest kept the canonical
    /// 320, so the scanout dropped the rightmost 40 columns of every row and the
    /// host stretched the surviving 320 across the whole 4:3 screen.
    ///
    /// Scoped to mode X on purpose. Chained mode 13h cannot exceed 320 pixels in
    /// its 64 KB aperture, and the planar and text modes run a Sequencer
    /// dot-clock divide (Clocking Mode 01h bit 3) that their timing tables
    /// already fold into `htotal_chars`, so decoding 00h there would halve
    /// `frame_dots` and double their refresh rate. Widen the scope only behind a
    /// fixture that needs it.
    fn recompute_horizontal_timing(&mut self) {
        let disp_chars = (u32::from(self.crtc_regs.r01) + 1).max(1);
        let dots_per_pixel = if self.attr.mode_control & 0x40 != 0 {
            2
        } else {
            1
        };
        self.crtc.htotal_chars = (u32::from(self.crtc_regs.r00) + 5).max(disp_chars);
        self.crtc.hdisp_end = (disp_chars * self.crtc.char_width / dots_per_pixel).max(1);
        self.resize_work();
    }

    /// Register-banged 256-color entry. Real silicon has no mode numbers: writing
    /// the standard 256-color register set (ATC mode-control graphics bit 0 +
    /// 8-bit color bit 6) IS the mode change. The ATC mode-control write is the
    /// last register in the conventional modeset order, so it is the decision
    /// point. Only the personality/scanout state flips — the guest's own
    /// SEQ/GC/ATC/CRTC values stay untouched (no VGABIOS readback seeding, no
    /// palette reset: a register-banging guest loads its own DAC).
    ///
    /// Two families ride this, discriminated by SEQ memory-mode chain-4:
    /// - Chain-4 ON → chained mode 13h. DOS Quake 1.06 sets 320x200x256 this way
    ///   (its vgamodes register tables, no INT 10h). Canonical 320x200 timing.
    /// - Chain-4 OFF, with the GC actually set up for 256-color graphics →
    ///   unchained mode X / mode Y. TSUMERA (Borland 32RTM) sets 320x240 this
    ///   way. The guest's own vertical CRTC timing is honored so a 240-line mode
    ///   renders full height. The GC-256/graphics requirement keeps a stray ATC
    ///   write in a text-mode guest from spuriously flipping the personality.
    ///
    /// Horizontal timing follows the same split: the chained family installs the
    /// canonical 320-wide values (a 64 KB aperture cannot hold a wider chained
    /// mode), while the unchained family decodes the guest's horizontal CRTC
    /// registers, so a nonstandard-width mode X keeps its width. The symmetric
    /// register-banged EXIT to text is not derived — every known title restores
    /// text via INT 10h.
    fn maybe_enter_256color_from_registers(&mut self) {
        let graphics = self.attr.mode_control & 0x01 != 0;
        let eight_bit = self.attr.mode_control & 0x40 != 0;
        if !graphics || !eight_bit || !matches!(self.mode, VideoMode::Text | VideoMode::Planar) {
            return;
        }
        let chain4 = self.seq.memory_mode & 0x08 != 0;
        if chain4 {
            // Chained mode 13h: canonical 320x200. Reseed the raw CRTC bytes from
            // the canonical timing before the recompute — a Text-origin bang's
            // vertical CRTC writes are captured now but 320x200 is the correct
            // chained base regardless, and this keeps Quake byte-identical.
            self.bump_content_gen();
            self.crtc = CrtcTiming::mode13h();
            self.crtc_regs = CrtcRegs::from_timing(self.crtc);
            self.recompute_vertical_timing();
            self.mode = VideoMode::Mode13h;
        } else {
            // Unchained mode X / mode Y — only when the Graphics Controller is
            // genuinely in 256-color graphics (256-shift + graphics/A0000 select),
            // so chain-4-off alone (a stray text-mode ATC write) does not flip.
            let gc_256_graphics = self.gc.mode_flags & 0x40 != 0 && self.gc.misc & 0x01 != 0;
            if !gc_256_graphics {
                return;
            }
            // Keep the guest's captured CRTC timing (the recomputes decode it) so
            // a 320x240 mode Y keeps its 240 lines instead of snapping to 200, and
            // a 360-wide mode keeps its width instead of snapping to 320.
            self.bump_content_gen();
            self.crtc = CrtcTiming::mode_x();
            self.recompute_vertical_timing();
            self.recompute_horizontal_timing();
            self.mode = VideoMode::ModeX;
        }
        self.beam = 0;
        self.last_line = 0;
        self.presented = None;
        self.pending_start = None;
        self.resize_work();
    }

    /// Enter unchained 256-color (mode X / mode Y) with the 320x200 base. The guest
    /// retunes the geometry by reprogramming the vertical CRTC timing while here.
    fn enter_mode_x(&mut self) {
        self.sync_mode13_planar();
        // seq.memory_mode already holds the chain-4-off value from the write_seq
        // call that triggered this entry, so it is not reseeded here.
        //
        // The raw CRTC bytes are NOT reseeded either. Clearing chain-4 changes
        // the CPU write decode, not the display timing, so the CRTC keeps
        // whatever the guest last wrote. Psycho Pinball programs its whole
        // 320x370 vertical timing BEFORE it clears the bit, and re-seeding the
        // canonical 320x200 register set here threw that timing away and cropped
        // the bottom 170 rows of every frame. The timing table below is the
        // 320x200 base a guest that wrote nothing still gets: mode 13h's table
        // is byte-identical to it, and `set_mode13h` is the only way into the
        // mode this entry comes from, so the recompute below reproduces the base
        // exactly when the guest has been quiet.
        //
        // The reseed also cleared the CRTC write protect as a side effect, since
        // the 320x200 register set carries 11h = 0Eh. Mode 13h's BIOS table
        // carries 11h = 8Eh, so the protect is SET on entry here, and a guest
        // that wants to retune registers 00h-07h must clear it first -- which is
        // what it must do on real silicon too.
        self.crtc = CrtcTiming::mode_x();
        // The mode is set BEFORE the recompute, because the recompute sizes the
        // work raster and `resize_work` keeps a same-size buffer only while the
        // mode is unchanged. Setting it afterwards left the buffer labelled with
        // the mode being left, so the next same-size recompute cleared a raster
        // it should have kept.
        self.mode = VideoMode::ModeX;
        self.recompute_vertical_timing(); // derives the vertical fields and sizes work
        self.beam = 0;
        self.last_line = 0;
        self.presented = None;
    }

    pub fn active_mode(&self) -> VideoMode {
        self.mode
    }

    /// Every geometry this guest programmed, against its count.
    pub fn mode_census(&self) -> &ModeCensus {
        &self.mode_census
    }

    /// Text rows rendered so far by `render_scanline`'s text-mode arm. Stays
    /// flat while a Voodoo-style device owns the monitor cable
    /// (`advance_cable_aware(_, true)`), because that raster is elided --
    /// nobody will ever scan it out.
    pub fn text_rows_rendered(&self) -> u64 {
        self.text_rows_rendered
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
        self.arm_graphics_settle();
        self.mode13_argb_full_dirty.set(true);
        self.mode13_direct_batch_dirty = false;
        self.content_gen = self.content_gen.wrapping_add(1);
    }

    #[inline]
    fn arm_graphics_settle(&mut self) {
        if !self.is_text_mode() && (self.last_line != 0 || self.beam != 0) {
            self.graphics_settle_frames = self.graphics_settle_frames.max(2);
        }
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
            0x13 => {
                let pixel_pan = value & 0x0F;
                if self.attr.pixel_pan != pixel_pan {
                    self.sync_mode13_planar();
                    self.attr.pixel_pan = pixel_pan;
                }
            }
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

    pub fn dac_component_bits(&self) -> u8 {
        self.dac.component_bits()
    }

    pub fn set_dac_component_bits(&mut self, bits: u8) -> bool {
        if self.dac.component_bits() == bits {
            return true;
        }
        if !self.dac.set_component_bits(bits) {
            return false;
        }
        self.bump_content_gen();
        true
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
            let gray = Self::gray_component(self.dac.component_bits(), r, g, b);
            self.dac.set_entry(index, gray, gray, gray);
        }
    }

    fn dac_entry_for_write(&self, r: u8, g: u8, b: u8) -> [u8; 3] {
        if self.grayscale_summing_enabled {
            let gray = Self::gray_component(self.dac.component_bits(), r, g, b);
            [gray, gray, gray]
        } else {
            [r, g, b]
        }
    }

    fn gray_component(bits: u8, r: u8, g: u8, b: u8) -> u8 {
        let mask = if bits == 8 { 0xff } else { 0x3f };
        ((u16::from(r & mask) * 77 + u16::from(g & mask) * 151 + u16::from(b & mask) * 28) >> 8)
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

    /// Whether [`Self::cached_mode13h_presented_argb`] would answer with a
    /// frame, without building or cloning one.
    ///
    /// Exists so a caller can ASK the question cheaply before deciding which of
    /// two presentation paths to take. Answering it by calling the cache itself
    /// costs a full-frame `Vec` clone, which is the opposite of what a chooser
    /// wants. The two must agree, so both are written against the same pair of
    /// conditions and nothing else.
    pub fn presents_cached_mode13h_argb(&self) -> bool {
        self.canonical_mode13_layout() && self.last_presented().is_some()
    }

    /// Palette-map the latest completed canonical Mode 13h frame. The cache is
    /// full-sized so callers can keep their existing crop behavior.
    pub fn cached_mode13h_presented_argb(&self) -> Option<(Vec<u32>, usize, usize, usize)> {
        if !self.canonical_mode13_layout() {
            return None;
        }
        let raster = self.last_presented()?;
        let width = raster.width as usize;
        let height = raster.height as usize;
        let display_height = raster.display_height.min(raster.height) as usize;
        let palette = self.palette_argb();
        let frame = self.frames_completed();
        let dirty_pages = self.mode13_dirty_pages.get();
        let full_dirty = self.mode13_argb_full_dirty.get();
        let mut cache = self.mode13_argb_cache.borrow_mut();
        let shape_changed = !cache.valid
            || cache.width != width
            || cache.height != height
            || cache.words.len() != raster.pixels.len();
        let frame_advanced = !cache.valid || cache.frame != frame;
        let palette_changed = !cache.valid || cache.palette != palette;

        if cache.valid && !frame_advanced && !palette_changed {
            return Some((cache.words.clone(), width, height, display_height));
        }

        let full = shape_changed || palette_changed || full_dirty;
        let converted = if full {
            cache.words.resize(raster.pixels.len(), 0);
            for (word, &index) in cache.words.iter_mut().zip(&raster.pixels) {
                *word = palette[usize::from(index)];
            }
            raster.pixels.len()
        } else {
            let scan_factor = self.scan_factor() as usize;
            let visible_bytes = crate::MODE13H_MEMORY_SIZE.min(self.mode13_linear.len());
            let mut converted = 0;
            for page in 0..MODE13_DIRECT_PAGES {
                if dirty_pages & (1u16 << page) == 0 {
                    continue;
                }
                let start = page * 0x1000;
                let end = (start + 0x1000).min(visible_bytes);
                for linear in start..end {
                    let source_row = linear / width;
                    let x = linear % width;
                    for duplicate in 0..scan_factor {
                        let row = source_row * scan_factor + duplicate;
                        if row >= display_height {
                            break;
                        }
                        let pixel = row * width + x;
                        cache.words[pixel] = palette[usize::from(raster.pixels[pixel])];
                        converted += 1;
                    }
                }
            }
            converted
        };

        cache.palette = palette;
        cache.width = width;
        cache.height = height;
        cache.display_height = display_height;
        cache.frame = frame;
        cache.valid = true;
        cache.converted_pixels = converted;
        if frame_advanced && self.graphics_settle_frames == 0 {
            self.mode13_dirty_pages.set(0);
            self.mode13_argb_full_dirty.set(false);
        }
        Some((cache.words.clone(), width, height, display_height))
    }

    #[cfg(test)]
    pub(crate) fn mode13h_last_converted_pixels(&self) -> usize {
        self.mode13_argb_cache.borrow().converted_pixels
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
/// selects map B (set) or map A (clear) in 512-glyph mode.
fn char_map_b_decode(spec: u8) -> usize {
    char_map_decode(spec, 2, 3, 5)
}

#[cfg(test)]
#[path = "vga_test.rs"]
mod tests;
