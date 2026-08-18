// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Machine {
    pub fn screen_text(&self) -> TextFrame {
        self.vega.screen_text()
    }

    pub fn is_graphics_mode(&self) -> bool {
        self.vega.is_graphics_mode()
    }

    #[cfg(test)]
    pub(crate) fn margo(&self) -> &Margo {
        self.vega.margo()
    }

    #[cfg(test)]
    pub(crate) fn margo_mut(&mut self) -> &mut Margo {
        self.vega.margo_mut()
    }

    #[cfg(test)]
    pub(crate) fn video(&self) -> &Vga {
        self.vega.legacy()
    }

    #[cfg(test)]
    pub(crate) fn video_mut(&mut self) -> &mut Vga {
        self.mark_direct_map_changed();
        self.vega.legacy_mut()
    }

    pub fn set_vga_mode_0dh(&mut self) {
        self.vega.legacy_mut().set_mode_0dh();
        self.mark_direct_map_changed();
    }

    /// Select a VGA graphics mode by its INT 10h number from the host side. Returns
    /// false for an unimplemented number. On success it hands the display back to
    /// the VGA core by clearing the Margo latch.
    pub fn set_vga_mode(&mut self, mode: u8) -> bool {
        self.set_vga_mode_with_clear(mode, false)
    }

    fn set_vga_mode_with_clear(&mut self, mode: u8, clear: bool) -> bool {
        let ok = self.vega.legacy_mut().set_mode_with_clear(mode, clear);
        if ok {
            self.vega.select_legacy();
            self.mark_direct_map_changed();
        }
        ok
    }

    /// Whether the Margo linear-framebuffer display is the active output (the GUI
    /// renders it instead of the VGA text/graphics core). A VGA mode set via INT
    /// 10h clears this latch. Exposed so a test can assert the BIOS hands the
    /// display back to VGA text before booting an OS.
    pub fn margo_active(&self) -> bool {
        self.vega.margo_active()
    }

    pub(super) fn int10_set_mode_number(&mut self, requested_mode: u8) -> bool {
        let mode = requested_mode & 0x7F;
        let clear = requested_mode & 0x80 == 0;
        match mode {
            0x0D..=0x13 => {
                if !self.set_vga_mode_with_clear(mode, clear) {
                    return false;
                }
                let cols = if matches!(mode, 0x0D | 0x13) { 40 } else { 80 };
                self.set_bda_video_mode(requested_mode, cols, Self::video_text_rows(mode));
            }
            0x04..=0x06 => {
                self.vega.legacy_mut().set_cga_mode_with_clear(mode, clear);
                self.vega.select_legacy();
                let cols = if mode == 0x06 { 80 } else { 40 };
                self.set_bda_video_mode(requested_mode, cols, Self::video_text_rows(mode));
            }
            0x00..=0x03 | 0x07 => {
                self.vega.select_legacy();
                let cols: u16 = if mode <= 0x01 { 40 } else { 80 };
                if mode == 0x07 {
                    self.vega.legacy_mut().set_mono_text_mode();
                } else if let Some(scanlines) = self.text_scanline_override {
                    let _ = self
                        .vega
                        .legacy_mut()
                        .set_color_text_mode_scanlines(mode, scanlines, clear);
                } else if mode <= 0x02 {
                    let _ = self
                        .vega
                        .legacy_mut()
                        .set_cga_text_mode_with_clear(mode, clear);
                } else {
                    self.vega
                        .legacy_mut()
                        .set_text_mode_columns(usize::from(cols));
                }
                self.set_bda_video_mode(requested_mode, cols, Self::video_text_rows(mode));
            }
            _ => return false,
        }
        self.set_eax_al(Self::video_mode_set_return_al(mode));
        true
    }

    /// Service the host side of an `INT 10h` after the instruction retires.
    /// The CPU registers are intact here: a software interrupt only pushes
    /// flags/CS/IP.
    pub(super) fn handle_int10(&mut self) {
        let direct_write_before = self.vega.direct_write_identity();
        // Unconditional, NOT behind the identity compare below: the CGA and text arms of
        // int10_set_mode_number reach legacy_mut() without moving the identity at all, yet a
        // text-mode set re-points what physical B8000/A0000 alias. The CPU-side gate keeps the
        // breadth free for guests that hold no aperture code.
        self.aperture_content_changed = true;
        self.handle_int10_inner();
        if self.vega.direct_write_identity() != direct_write_before {
            self.mark_direct_data_map_changed();
        }
    }

    fn handle_int10_inner(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let bh = (bx >> 8) as u8;
        let bl = bx as u8;
        if matches!(ax, 0x0070 | 0x6F05) {
            return;
        }
        if matches!(ax, 0x6A00..=0x6A02) {
            self.int10_dgis(ax);
            return;
        }
        if ah == 0x00 {
            if al == 0x7e {
                self.int10_paradise_set_special_mode();
                return;
            }
            if al == 0x7f {
                self.int10_paradise_extended(bh, bl);
                return;
            }
            if self.int10_set_mode_number(al) {
                return;
            }
        }
        if ah == 0x05 {
            // INT 10h AH=05h SELECT ACTIVE DISPLAY PAGE (RBIL INTERRUP.A:2162).
            // AL is the page number. CGA graphics modes have only page 0; text
            // modes page by moving the CRTC start address in character cells.
            // EGA planar graphics modes page in byte-address units.
            let mode = self.read_physical_u8(0x449) & 0x7F;
            if matches!(mode, 0x04..=0x06) {
                let _ = self.write_guest_ram_u8(0x462, 0);
                let _ = self.write_guest_ram_u16(0x44e, 0);
                return;
            }
            if let Some((page, page_start)) = self.ega_graphics_page_start(mode, al) {
                self.vega.legacy_mut().set_start_address(page_start);
                let _ = self.write_guest_ram_u8(0x462, page);
                let _ = self.write_guest_ram_u16(0x44e, page_start as u16);
                return;
            }
            let page = self.normalize_text_page(al);
            let stride = self.text_page_stride();
            let page_start = usize::from(page) * stride;
            self.vega
                .legacy_mut()
                .set_start_address((page_start / 2) as u32);
            let _ = self.write_guest_ram_u8(0x462, page);
            let _ = self.write_guest_ram_u16(0x44e, page_start as u16);
            let pos = self.cursor_pos(page);
            self.set_hardware_cursor_for_page(page, pos);
            return;
        }
        if ah == 0x0b {
            match bh {
                // BH=0: BL is the border/overscan color. In CGA graphics it also
                // sets the 3D9h background/foreground nibble plus intensity.
                0x00 => {
                    self.vega.legacy_mut().set_overscan(bl);
                    if self.vega.legacy_mut().active_mode() == VideoMode::Cga {
                        let current = self.vega.legacy_mut().cga_color_select();
                        let _ = self
                            .vega
                            .legacy_mut()
                            .write_port(0x3D9, (current & !0x1F) | (bl & 0x1F));
                    }
                }
                // BH=1: BL bit0 selects CGA palette 0 vs 1 for 320x200x4.
                0x01 => {
                    let current = self.vega.legacy_mut().cga_color_select();
                    let _ = self
                        .vega
                        .legacy_mut()
                        .write_port(0x3D9, (current & !0x20) | ((bl & 1) << 5));
                }
                _ => {}
            }
            if self.vega.legacy_mut().is_cga_personality() {
                self.sync_bda_cga_latches();
            }
            return;
        }
        if ah == 0x0c {
            self.int10_write_pixel(al);
            return;
        }
        if ah == 0x0d {
            self.int10_read_pixel();
            return;
        }
        if ah == 0x04 {
            self.int10_read_light_pen();
            return;
        }
        if ah == 0x10 {
            self.handle_int10_palette(al);
            return;
        }
        if ah == 0x11 {
            self.handle_int10_font(al);
            return;
        }
        if ah == 0x12 {
            self.handle_int10_alternate(al, bl);
            return;
        }
        if ah == 0x13 {
            self.int10_write_string();
            return;
        }
        if ah == 0x15 {
            // Convertible display parameters: no alternate physical display.
            self.set_ax(0x0000);
            return;
        }
        if ah == 0x1c {
            self.int10_save_restore_state(al);
            return;
        }
        if matches!(ah, 0x70 | 0x71) {
            // Tandy 1000 RAM address queries. This VGA profile has no Tandy planes.
            self.set_ax(0x0000);
            self.set_bx(0x0000);
            self.set_cx(0x0000);
            self.set_dx(0x0000);
            return;
        }
        if ah == 0xbf {
            // Compaq switchable display extensions. AL=03 reports no switchable VDU;
            // the other subfunctions preserve registers as absent hardware.
            if al == 0x03 {
                self.set_bx(0x0000);
                self.set_cx(0x0000);
                self.set_dx(0x0000);
            }
            return;
        }
        if ah == 0xfa {
            // Microsoft mouse EGA register interface installation check.
            self.set_bx(0x0000);
            return;
        }
        if matches!(
            ah,
            0x14 | 0x40..=0x4e | 0x72 | 0x73 | 0x80..=0x82 | 0xf0..=0xf7 | 0xfe | 0xff
        ) {
            return;
        }
        if matches!(
            ah,
            0x01 | 0x02 | 0x03 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0A | 0x0E
        ) {
            self.handle_int10_text(ah);
            return;
        }
        if ah == 0x0f {
            let mode = self.read_physical_u8(0x449);
            let cols = self.read_guest_word(0x44a);
            let eax = (self.cpu.registers.eax() & !0xFFFF)
                | (u32::from(cols & 0xff) << 8)
                | u32::from(mode);
            self.cpu.registers.set_eax(eax);
            let page = self.read_physical_u8(0x462);
            let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(page) << 8);
            self.cpu.registers.set_ebx(ebx);
            return;
        }
        if ah == 0x1a {
            // AH=1Ah display combination code. AL=00h reads and AL=01h writes
            // the BDA DCC byte, the same storage AH=1Bh reports.
            self.set_eax_al(0x1A);
            match al {
                0x00 => {
                    let dcc = self.read_physical_u8(0x48A);
                    self.set_bx(u16::from(dcc));
                }
                0x01 => {
                    let _ = self.write_guest_ram_u8(0x48A, bl);
                }
                _ => {}
            }
            return;
        }
        if ah == 0x1b {
            // AH=1Bh functionality/state information (VGA). Fills the 64-byte block at
            // ES:DI and returns AL=1Bh so callers detect a VGA BIOS.
            self.int10_state_info();
            return;
        }
        if ah == 0x4f {
            self.handle_vbe(al);
        }
    }

    fn int10_dgis(&mut self, ax: u16) {
        match ax {
            // DGIS inquire: no DGIS devices installed.
            0x6A00 => {
                self.set_bx(0x0000);
                self.set_cx(0x0000);
            }
            // DGIS redirect output: cannot redirect to a non-DGIS device.
            0x6A01 => self.set_cx(0x0000),
            // DGIS current output device: the current display is the BIOS VGA.
            0x6A02 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
                let edi = self.cpu.registers.edi() & !0xFFFF;
                self.cpu.registers.set_edi(edi);
            }
            _ => {}
        }
    }

    fn uses_mono_crtc_base(&self) -> bool {
        self.memory.read_u16(0x463).unwrap_or(0x03D4) == 0x03B4
    }

    fn active_display_combination_code(&self) -> u8 {
        if self.uses_mono_crtc_base() {
            0x07
        } else {
            0x08
        }
    }

    /// INT 10h AH=1Bh. Writes the 64-byte video state-information block at ES:DI with the
    /// live mode, geometry, CGA latch shadows, and display-combination fields, plus
    /// a static functionality table pointer. Limit: only the commonly-read fields
    /// are populated; the VGA-present check that programs run only tests AL == 0x1B.
    ///
    /// ES:DI is the caller's LINEAR address, not a physical one: detection code
    /// calls this beside VBE 4F00h and out of the same DPMI transfer buffer, so
    /// it reaches this service from a UMB that a memory manager maps
    /// non-identity. See `write_guest_linear_block`.
    fn int10_state_info(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let addr = es.wrapping_add(u32::from(di));
        let mode = self.read_physical_u8(0x449);
        let cols = self.read_guest_word(0x44a);
        let page = self.read_physical_u8(0x462);
        let rows_minus_1 = self.read_physical_u8(0x484);
        let page_size = self.read_guest_word(0x44c);
        let page_start = self.read_guest_word(0x44e);
        let cursor_type = self.read_guest_word(0x460);
        let char_height = self.read_guest_word(0x485);
        let mut block = [0u8; 64];
        block[0..2].copy_from_slice(&INT10_FUNCTIONALITY_TABLE_OFFSET.to_le_bytes());
        block[2..4].copy_from_slice(&VGA_BIOS_SEGMENT.to_le_bytes());
        block[4] = mode;
        block[5..7].copy_from_slice(&cols.to_le_bytes());
        block[0x07..0x09].copy_from_slice(&page_size.to_le_bytes());
        block[0x09..0x0B].copy_from_slice(&page_start.to_le_bytes());
        for offset in 0..16 {
            block[0x0B + offset] = self.read_physical_u8(0x450 + offset as u32);
        }
        block[0x1B..0x1D].copy_from_slice(&cursor_type.to_le_bytes());
        block[0x1D] = page;
        block[0x1E..0x20].copy_from_slice(&self.read_guest_word(0x463).to_le_bytes());
        block[0x20] = self.read_physical_u8(0x465); // CGA mode-control shadow
        block[0x21] = self.read_physical_u8(0x466); // CGA color-select shadow
        block[0x22] = rows_minus_1.wrapping_add(1); // rows on screen
        block[0x23..0x25].copy_from_slice(&char_height.to_le_bytes());
        block[0x25] = self.read_physical_u8(0x48A);
        block[0x27..0x29].copy_from_slice(&Self::video_color_count(mode).to_le_bytes());
        block[0x29] = self.video_page_count(mode); // pages
        block[0x2A] = self.video_scanline_code(mode);
        self.write_guest_linear_block(addr, &block);
        self.set_eax_al(0x1B);
    }

    /// INT 10h AH=12h BL=30h: record the BIOS's preferred scanline count for
    /// the *next* mode set. This is BDA/mode-set policy bookkeeping only (feeds
    /// `text_scanlines_for_mode`/`video_char_height` below) and is independent
    /// of `Vga::set_char_height`, which reprograms the live CRTC Maximum Scan
    /// Line register from AH=11h font-load calls.
    fn set_selected_text_scanlines(&mut self, al: u8) -> bool {
        let mut flags = self.read_physical_u8(0x489);
        let mut switches = self.read_physical_u8(0x488) & 0xF0;
        flags &= !0x90;
        match al {
            0x00 => {
                flags |= 0x80; // 200 scan lines
                switches |= 0x08;
                self.text_scanline_override = Some(200);
            }
            0x01 => {
                switches |= 0x09;
                self.text_scanline_override = Some(350);
            }
            0x02 => {
                flags |= 0x10; // 400 scan lines
                switches |= 0x09;
                self.text_scanline_override = Some(400);
            }
            _ => return false,
        }
        let _ = self.write_guest_ram_u8(0x488, switches);
        let _ = self.write_guest_ram_u8(0x489, flags);
        true
    }

    fn text_scanlines_for_mode(&self, mode: u8) -> u16 {
        self.text_scanline_override
            .unwrap_or(if (mode & 0x7F) <= 0x02 { 200 } else { 400 })
    }

    fn video_color_count(mode: u8) -> u16 {
        match mode & 0x7F {
            0x04 | 0x05 => 4,
            0x06 | 0x07 | 0x0F | 0x11 => 2,
            0x13 => 256,
            _ => 16,
        }
    }

    fn video_scanline_code(&self, mode: u8) -> u8 {
        match mode & 0x7F {
            0x00..=0x03 => match self.text_scanlines_for_mode(mode) {
                200 => 0,
                350 => 1,
                400 => 2,
                _ => 2,
            },
            0x07 | 0x0F | 0x10 => 1, // 350 active scan lines
            0x11 | 0x12 => 3,        // 480 active scan lines
            0x04..=0x06 | 0x13 => 0, // 200 active scan lines
            _ => 2,                  // VGA text modes default to 400
        }
    }

    /// Record the current video mode in the BDA so apps that read it directly
    /// (and INT 10h AH=0Fh) see a sane state. Columns and rows are the text-cell
    /// geometry the BIOS publishes for the mode.
    fn set_bda_video_mode(&mut self, mode: u8, columns: u16, rows: u8) {
        let _ = self.write_guest_ram_u8(0x449, mode);
        let _ = self.write_guest_ram_u16(0x44a, columns);
        let page_size = self.video_page_size(mode);
        let _ = self.write_guest_ram_u16(0x44c, page_size);
        let _ = self.write_guest_ram_u8(0x484, rows.saturating_sub(1));
        let char_height = self.video_char_height(mode);
        let _ = self.write_guest_ram_u16(0x485, u16::from(char_height));
        let _ = self.write_guest_ram_u16(0x44e, 0);
        let _ = self.write_guest_ram_u8(0x462, 0);
        for page in 0..8usize {
            let _ = self.write_guest_ram_u16(0x450 + page * 2, 0);
        }
        let _ = self.write_guest_ram_u16(0x463, Self::video_crtc_base_port(mode));
        let _ = self.write_guest_ram_u8(0x487, 0x60 | (mode & 0x80));
        let display_combination = self.active_display_combination_code();
        let _ = self.write_guest_ram_u8(0x48A, display_combination);
        let _ = (|| {
            self.write_guest_ram_u16(
                BDA_VIDEO_SAVE_POINTER,
                INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET,
            )?;
            self.write_guest_ram_u16(BDA_VIDEO_SAVE_POINTER + 2, VGA_BIOS_SEGMENT)
        })();
        if let Some(mode_control) = Self::cga_bda_mode_control(mode) {
            let _ = self.write_guest_ram_u8(0x465, mode_control);
            let _ = self.write_guest_ram_u8(0x466, Self::cga_bda_color_select(mode));
        } else {
            let _ = self.write_guest_ram_u8(0x465, 0);
            let _ = self.write_guest_ram_u8(0x466, 0);
        }
    }

    fn int10_paradise_set_special_mode(&mut self) {
        let width = self.cpu.registers.ebx() as u16;
        let height = self.cpu.registers.ecx() as u16;
        let colors = self.cpu.registers.edx() as u16;
        let mode = match (width, height, colors) {
            (40, 25, 16) => Some(0x00),
            (80, 25, 16) => Some(0x03),
            (80, 25, 0) => Some(0x07),
            (320, 200, 4) => Some(0x04),
            (640, 200, 0 | 2) => Some(0x06),
            (320, 200, 16) => Some(0x0D),
            (640, 200, 16) => Some(0x0E),
            (640, 350, 0 | 2) => Some(0x0F),
            (640, 350, 16) => Some(0x10),
            (640, 480, 0 | 2) => Some(0x11),
            (640, 480, 16) => Some(0x12),
            (320, 200, 256) => Some(0x13),
            _ => None,
        };

        let ok = match mode {
            Some(mode) => self.int10_set_mode_number(mode),
            None => false,
        };
        if ok {
            self.set_eax_al(0x7E);
            self.set_bh(0x7E);
        } else {
            self.set_bh(0x00);
        }
    }

    fn int10_paradise_extended(&mut self, bh: u8, bl: u8) {
        let ok = match bh {
            0x00 => {
                self.paradise_non_vga = false;
                true
            }
            0x01 => {
                self.paradise_non_vga = true;
                true
            }
            0x02 => {
                self.set_bl(u8::from(self.paradise_non_vga));
                let used = self.int10_current_vram_units();
                self.set_cx((4 << 8) | u16::from(used));
                true
            }
            0x03 | 0x29..=0x2F | 0x60 | 0xA5 | 0xA6 => true,
            0x04 => {
                self.paradise_non_vga = true;
                self.int10_set_mode_number(0x07)
            }
            0x05 => {
                self.paradise_non_vga = true;
                true
            }
            0x06 => {
                self.paradise_non_vga = false;
                self.int10_set_mode_number(0x07)
            }
            0x07 => {
                self.paradise_non_vga = false;
                self.int10_set_mode_number(0x03)
            }
            0x0A..=0x0F => {
                self.paradise_regs[usize::from(bh - 0x0A)] = bl;
                true
            }
            0x1A..=0x1F => {
                self.set_bl(self.paradise_regs[usize::from(bh - 0x1A)]);
                true
            }
            0x61 => {
                let addr = self
                    .cpu
                    .registers
                    .segment(SegmentIndex::Es)
                    .base
                    .wrapping_add(u32::from(self.cpu.registers.edi() as u16));
                self.write_physical_u8(addr, 0);
                true
            }
            _ => false,
        };

        if ok {
            self.set_eax_al(0x7F);
            self.set_bh(0x7F);
        } else {
            self.set_bh(0x00);
        }
    }

    fn int10_current_vram_units(&mut self) -> u8 {
        match self.read_physical_u8(0x449) {
            0x12 => 3,
            0x0F..=0x11 => 2,
            _ => 1,
        }
    }

    /// INT 10h AH=12h alternate function select. The common VGA calls are mostly
    /// BIOS policy latches: report the configured adapter for BL=10h and mirror
    /// supported toggles into the VGA BDA bytes at 0040:0087-0089.
    fn handle_int10_alternate(&mut self, al: u8, bl: u8) {
        match bl {
            // BL=10h: return EGA/VGA configuration information.
            0x10 => {
                let switch_data = self.read_physical_u8(0x488);
                let mode = u8::from(self.uses_mono_crtc_base());
                let memory = 0x03u8; // 256 KiB installed
                let feature = (switch_data >> 4) & 0x0f;
                let switches = switch_data & 0x0f;
                self.set_bx((u16::from(mode) << 8) | u16::from(memory));
                self.set_cx((u16::from(feature) << 8) | u16::from(switches));
            }
            // BL=20h installs the video BIOS print-screen hook. The ROM print-screen
            // body is not modeled; accepting the call matches VGA BIOS probes.
            0x20 => {}
            // BL=30h: select text-mode scanline count for the next mode set.
            0x30 if al <= 0x02 && self.set_selected_text_scanlines(al) => {
                self.set_eax_al(0x12);
            }
            0x30 => {}
            // BL=31h: default palette loading on mode set.
            0x31 if al <= 0x01 => {
                self.vega
                    .legacy_mut()
                    .set_default_palette_loading_enabled(al == 0x00);
                let mut flags = self.read_physical_u8(0x489);
                if al == 0x01 {
                    flags |= 0x08; // no palette load
                } else {
                    flags &= !0x08;
                }
                self.write_physical_u8(0x489, flags);
                self.set_eax_al(0x12);
            }
            // BL=32h: video memory/register addressing.
            0x32 if al <= 0x01 => {
                let misc = self.vega.legacy_mut().read_port(0x3CC).unwrap_or(0x67);
                let misc = if al == 0x00 {
                    misc | 0x02
                } else {
                    misc & !0x02
                };
                let _ = self.vega.legacy_mut().write_port(0x3C2, misc);
                self.set_eax_al(0x12);
            }
            // BL=33h: gray-scale summing policy.
            0x33 if al <= 0x01 => {
                self.vega
                    .legacy_mut()
                    .set_grayscale_summing_enabled(al == 0x00);
                let mut flags = self.read_physical_u8(0x489);
                if al == 0x00 {
                    flags |= 0x02; // gray scaling enabled
                } else {
                    flags &= !0x02;
                }
                self.write_physical_u8(0x489, flags);
                self.set_eax_al(0x12);
            }
            // BL=34h: cursor emulation/scaling policy. This BIOS tracks both the
            // EGA/VGA video-control inhibit bit at 0040:0087 and the mode-set
            // control latch at 0040:0089 used by cursor-shape scaling.
            0x34 if al <= 0x01 => {
                let mut control = self.read_physical_u8(0x487);
                let mut flags = self.read_physical_u8(0x489);
                if al == 0x01 {
                    control |= 0x01;
                    flags &= !0x01;
                } else {
                    control &= !0x01;
                    flags |= 0x01;
                }
                self.write_physical_u8(0x487, control);
                self.write_physical_u8(0x489, flags);
                self.set_eax_al(0x12);
            }
            // BL=35h display switch: no second adapter is modeled, but the VGA BIOS
            // acknowledges the call.
            0x35 if al <= 0x03 => self.set_eax_al(0x12),
            // BL=36h refresh control.
            0x36 if al <= 0x01 => {
                self.vega
                    .legacy_mut()
                    .set_display_refresh_enabled(al == 0x00);
                self.set_eax_al(0x12);
            }
            _ => {}
        }
    }

    fn cga_bda_mode_control(mode: u8) -> Option<u8> {
        match mode & 0x7F {
            0x00 => Some(0x2C),
            0x01 => Some(0x28),
            0x02 => Some(0x2D),
            0x03 => Some(0x29),
            0x04 => Some(0x0A),
            0x05 => Some(0x0E),
            0x06 => Some(0x1A),
            _ => None,
        }
    }

    fn video_crtc_base_port(mode: u8) -> u16 {
        match mode & 0x7F {
            0x07 | 0x0F => 0x03B4,
            _ => 0x03D4,
        }
    }

    fn cga_bda_color_select(mode: u8) -> u8 {
        if mode & 0x7F == 0x06 { 0x0F } else { 0x00 }
    }

    fn video_mode_set_return_al(mode: u8) -> u8 {
        match mode & 0x7F {
            0x06 => 0x3F,
            0x00..=0x05 | 0x07 => 0x30,
            _ => 0x20,
        }
    }

    fn sync_bda_cga_latches(&mut self) {
        let mode_control = self.vega.legacy_mut().cga_mode_control();
        let _ = self.write_guest_ram_u8(0x465, mode_control);
        let color_select = self.vega.legacy_mut().cga_color_select();
        let _ = self.write_guest_ram_u8(0x466, color_select);
    }

    fn video_page_size(&self, mode: u8) -> u16 {
        let mode = mode & 0x7F;
        match mode {
            0x00 | 0x01 => 0x0800,
            0x02 | 0x03 | 0x07 => 0x1000,
            0x0D => 0x2000,
            0x0E => 0x4000,
            0x0F | 0x10 => 0x8000,
            0x11 | 0x12 => 0x0000,
            0x04..=0x06 => 0x4000,
            0x13 => 320 * 200,
            _ => 0x1000,
        }
    }

    fn video_text_rows(mode: u8) -> u8 {
        match mode & 0x7F {
            0x11 | 0x12 => 30,
            _ => 25,
        }
    }

    fn video_char_height(&self, mode: u8) -> u8 {
        match mode & 0x7F {
            0x00..=0x03 => match self.text_scanlines_for_mode(mode) {
                200 => 8,
                350 => 14,
                400 => 16,
                _ => 16,
            },
            0x04..=0x06 | 0x0D | 0x0E | 0x13 => 8,
            0x07 | 0x0F | 0x10 => 14,
            _ => 16,
        }
    }

    fn text_columns(&mut self) -> usize {
        self.read_guest_word(0x44a).clamp(1, 80) as usize
    }

    fn text_rows(&mut self) -> usize {
        (usize::from(self.read_physical_u8(0x484)) + 1).clamp(1, 60)
    }

    fn text_page_stride(&mut self) -> usize {
        let size = self.read_guest_word(0x44c) as usize;
        if size != 0 {
            size
        } else if self.text_columns() <= 40 {
            0x0800
        } else {
            VGA_TEXT_PAGE_STRIDE
        }
    }

    fn text_aperture_size(&self) -> usize {
        if self.vega.legacy().is_cga_personality() {
            CGA_FB_SIZE
        } else {
            VGA_TEXT_MEMORY_SIZE
        }
    }

    fn text_page_count(&mut self) -> u8 {
        (self.text_aperture_size() / self.text_page_stride()).clamp(1, 8) as u8
    }

    fn ega_graphics_page_count(&self, mode: u8) -> Option<u8> {
        let mode = mode & 0x7F;
        match mode {
            0x0D..=0x10 => Some(
                ((VGA_PLANAR_WINDOW_SIZE as usize) / usize::from(self.video_page_size(mode)))
                    .clamp(1, 8) as u8,
            ),
            0x11 | 0x12 => Some(1),
            _ => None,
        }
    }

    fn ega_graphics_page_start(&self, mode: u8, page: u8) -> Option<(u8, u32)> {
        let page = page % self.ega_graphics_page_count(mode)?;
        Some((
            page,
            u32::from(page) * u32::from(self.video_page_size(mode)),
        ))
    }

    fn video_page_count(&mut self, mode: u8) -> u8 {
        self.ega_graphics_page_count(mode)
            .unwrap_or_else(|| self.text_page_count())
    }

    fn normalize_text_page(&mut self, page: u8) -> u8 {
        page % self.text_page_count()
    }

    fn normalize_bios_page(&mut self, page: u8) -> u8 {
        match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga => 0,
            VideoMode::Planar => {
                let mode = self.read_physical_u8(0x449);
                self.ega_graphics_page_start(mode, page)
                    .map(|(page, _)| page)
                    .unwrap_or(0)
            }
            _ => self.normalize_text_page(page),
        }
    }

    fn active_bios_page(&mut self) -> u8 {
        let page = self.read_physical_u8(0x462);
        self.normalize_bios_page(page)
    }

    fn text_page_base(&mut self, page: u8) -> usize {
        let page = self.normalize_text_page(page);
        usize::from(page) * self.text_page_stride()
    }

    fn text_offset(&mut self, page: u8, row: usize, col: usize) -> usize {
        let page = self.normalize_text_page(page);
        let columns = self.text_columns();
        let stride = self.text_page_stride();
        usize::from(page) * stride + (row * columns + col) * 2
    }

    fn cursor_pos(&mut self, page: u8) -> u16 {
        let page = self.normalize_bios_page(page);
        self.memory
            .read_u16(0x450 + usize::from(page) * 2)
            .unwrap_or(0)
    }

    fn set_cursor_pos(&mut self, page: u8, pos: u16) {
        let page = self.normalize_bios_page(page);
        let _ = self.write_guest_ram_u16(0x450 + usize::from(page) * 2, pos);
        if !self.is_bios_graphics_text_mode() && page == self.active_bios_page() {
            self.set_hardware_cursor_for_page(page, pos);
        }
    }

    fn set_hardware_cursor_for_page(&mut self, page: u8, pos: u16) {
        let columns = self.text_columns();
        let row = usize::from(pos >> 8);
        let col = usize::from(pos & 0x00ff);
        let base_cells = self.text_page_base(page) / 2;
        self.vega
            .legacy_mut()
            .set_cursor_offset((base_cells + row * columns + col) as u16);
    }

    fn bios_cursor_shape(&mut self, cx: u16) -> (u16, u8, u8) {
        let request_start = ((cx >> 8) as u8) & 0x3F;
        let request_end = (cx as u8) & 0x1F;
        let bda_shape = (u16::from(request_start) << 8) | u16::from(request_end);
        let mut hardware_start = request_start;
        let mut hardware_end = request_end;
        let mode_set_control = self.read_physical_u8(0x489);
        let char_height = self.read_guest_word(0x485);

        if mode_set_control & 0x01 != 0
            && char_height > 8
            && request_end < 8
            && request_start < 0x20
        {
            let scaled_end = ((u16::from(request_end) + 1) * char_height / 8).saturating_sub(1);
            let scaled_start = if u16::from(request_end) != u16::from(request_start) + 1 {
                ((u16::from(request_start) + 1) * char_height / 8).saturating_sub(1)
            } else {
                ((u16::from(request_end) + 1) * char_height / 8).saturating_sub(2)
            };
            hardware_start = scaled_start as u8;
            hardware_end = scaled_end as u8;
        }

        (bda_shape, hardware_start, hardware_end)
    }

    /// INT 10h AH=0Ch WRITE GRAPHICS PIXEL. AL = colour (bit 7 XORs in CGA/EGA
    /// packed-pixel modes), CX = column, DX = row. Mode 13h stores the full byte;
    /// CGA modes write packed raw pixel values into B800's interleaved framebuffer.
    /// EGA/VGA planar modes write the 4-bit colour into the four planes.
    fn int10_write_pixel(&mut self, al: u8) {
        let col = self.cpu.registers.ecx() as u16;
        let row = self.cpu.registers.edx() as u16;
        let page = ((self.cpu.registers.ebx() as u16) >> 8) as u8;
        match self.vega.legacy_mut().active_mode() {
            VideoMode::Mode13h => {
                let offset = usize::from(row) * 320 + usize::from(col);
                if offset < 320 * 200 {
                    // Mode 13h is a 256-color mode: AL is the full 8-bit pixel
                    // value, bit 7 included, with no XOR.
                    self.vega.legacy_mut().cpu_write_chain4(offset, al);
                }
            }
            VideoMode::Cga => {
                let _ = self
                    .vega
                    .legacy_mut()
                    .cga_write_pixel(col, row, al & 0x7F, al & 0x80 != 0);
            }
            VideoMode::Planar => {
                let mode = self.read_physical_u8(0x449);
                let start = self
                    .ega_graphics_page_start(mode, page)
                    .map(|(_, start)| start)
                    .unwrap_or(0);
                let _ = self.vega.legacy_mut().planar_write_pixel_at(
                    start,
                    col,
                    row,
                    al & 0x0F,
                    al & 0x80 != 0,
                );
            }
            _ => {}
        }
    }

    /// INT 10h AH=0Dh READ GRAPHICS PIXEL. CX = column, DX = row; returns AL = the
    /// pixel colour at `row*320 + col`. CGA modes return the raw packed pixel
    /// value (0..3 or 0..1), not the resolved DAC index. Planar modes return the
    /// four plane bits as a 0..15 colour.
    fn int10_read_pixel(&mut self) {
        let col = self.cpu.registers.ecx() as u16;
        let row = self.cpu.registers.edx() as u16;
        let page = ((self.cpu.registers.ebx() as u16) >> 8) as u8;
        let color = match self.vega.legacy_mut().active_mode() {
            VideoMode::Mode13h => {
                let offset = usize::from(row) * 320 + usize::from(col);
                if offset < 320 * 200 {
                    self.vega.legacy_mut().cpu_read_chain4(offset)
                } else {
                    0
                }
            }
            VideoMode::Cga => self.vega.legacy_mut().cga_read_pixel(col, row),
            VideoMode::Planar => {
                let mode = self.read_physical_u8(0x449);
                let start = self
                    .ega_graphics_page_start(mode, page)
                    .map(|(_, start)| start)
                    .unwrap_or(0);
                self.vega.legacy_mut().planar_read_pixel_at(start, col, row)
            }
            _ => 0,
        };
        self.set_eax_al(color);
    }

    /// INT 10h AH=04h READ LIGHT PEN POSITION. CGA-compatible only; VGA BIOSes
    /// report this as unsupported by leaving the trigger flag clear.
    fn int10_read_light_pen(&mut self) {
        let Some((pixel_col, pixel_row, char_row, char_col)) =
            self.vega.legacy_mut().cga_light_pen_report()
        else {
            self.set_eax_ah(0);
            return;
        };
        self.set_eax_ah(1);
        self.set_bx(pixel_col);
        self.set_cx(u16::from(pixel_row) << 8);
        self.set_dx((u16::from(char_row) << 8) | u16::from(char_col));
    }

    /// INT 10h AH=13h WRITE STRING. AL = write mode (bit 0 advance cursor, bit 1
    /// the source carries interleaved attribute bytes), BH = page, BL =
    /// attribute/color when bit 1 is clear, CX = character count, DH/DL = start
    /// row/col, ES:BP = the string. Text and EGA graphics modes write the
    /// requested page; CGA graphics remains single-page.
    /// The cursor is left at the end only when AL bit 0 is set.
    fn int10_write_string(&mut self) {
        let al = self.cpu.registers.eax() as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let page = self.normalize_bios_page((bx >> 8) as u8);
        let bl = bx as u8;
        let count = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let mut row = usize::from((dx >> 8) as u8);
        let mut col = usize::from(dx as u8);
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bp = self.cpu.registers.ebp() as u16;
        let mut src = es.wrapping_add(u32::from(bp));
        let with_attr = al & 0x02 != 0;
        let columns = self.text_columns();
        let rows = self.text_rows();
        for _ in 0..count {
            let ch = self.read_physical_u8(src);
            src += 1;
            let attr = if with_attr {
                let a = self.read_physical_u8(src);
                src += 1;
                a
            } else {
                bl
            };
            // Control characters move the cursor without placing a glyph, the way
            // the BIOS write-string handles CR/LF/BS/BEL.
            match ch {
                b'\r' => col = 0,
                b'\n' => row += 1,
                0x08 => col = col.saturating_sub(1),
                0x07 => {}
                _ => {
                    if row < rows && col < columns {
                        self.write_bios_char_cell(page, row, col, ch, attr);
                    }
                    col += 1;
                    if col >= columns {
                        col = 0;
                        row += 1;
                    }
                }
            }
            while row >= rows {
                self.scroll_text_up(page);
                row -= 1;
            }
        }
        // AL bit 0: leave the cursor at the end of the string; otherwise the caller
        // keeps its prior cursor (the BDA cursor is untouched).
        if al & 0x01 != 0 {
            let row = row.min(rows - 1) as u16;
            let col = col.min(columns - 1) as u16;
            self.set_cursor_pos(page, (row << 8) | col);
        }
    }

    fn int10_state_size_bytes(cx: u16) -> usize {
        let mut size = 0;
        if cx & 0x0001 != 0 {
            size += INT10_STATE_HARDWARE_LEN;
        }
        if cx & 0x0002 != 0 {
            size += INT10_STATE_BDA_LEN;
        }
        if cx & 0x0004 != 0 {
            size += INT10_STATE_DAC_LEN;
        }
        size
    }

    fn int10_state_size_blocks(cx: u16) -> u16 {
        let size = Self::int10_state_size_bytes(cx);
        size.div_ceil(64) as u16
    }

    fn save_video_hardware_state(&mut self, dst: u32) {
        let crtc_addr = self.read_guest_word(0x463);
        let crtc_addr: u16 = if crtc_addr == 0x03B4 { 0x03B4 } else { 0x03D4 };
        let mut block = Vec::with_capacity(INT10_STATE_HARDWARE_LEN);

        block.push(self.vega.legacy_mut().read_port(0x3C4).unwrap_or(0));
        block.push(self.vega.legacy_mut().crtc_index_latch());
        block.push(self.vega.legacy_mut().read_port(0x3CE).unwrap_or(0));
        self.vega.legacy_mut().read_status1();
        block.push(self.vega.legacy_mut().read_port(0x3C0).unwrap_or(0x20));
        block.push(self.vega.legacy_mut().read_port(0x3CA).unwrap_or(0));

        for index in 1..=4 {
            let _ = self.vega.legacy_mut().write_port(0x3C4, index);
            block.push(self.vega.legacy_mut().read_port(0x3C5).unwrap_or(0));
        }
        let _ = self.vega.legacy_mut().write_port(0x3C4, 0);
        block.push(self.vega.legacy_mut().read_port(0x3C5).unwrap_or(0));

        for index in 0..=0x18 {
            block.push(self.vega.legacy_mut().crtc_register_latch(index));
        }

        let ar_index = block[3];
        for index in 0..=0x13 {
            self.vega.legacy_mut().read_status1();
            let _ = self
                .vega
                .legacy_mut()
                .write_port(0x3C0, index | (ar_index & 0x20));
            block.push(self.vega.legacy_mut().read_port(0x3C1).unwrap_or(0));
        }
        self.vega.legacy_mut().read_status1();

        for index in 0..=0x08 {
            let _ = self.vega.legacy_mut().write_port(0x3CE, index);
            block.push(self.vega.legacy_mut().read_port(0x3CF).unwrap_or(0));
        }

        block.extend_from_slice(&crtc_addr.to_le_bytes());
        if self.vega.legacy_mut().is_cga_personality() {
            block.extend_from_slice(&INT10_STATE_CGA_LATCH_MARKER);
            block.push(self.vega.legacy_mut().cga_mode_control());
            block.push(self.vega.legacy_mut().cga_color_select());
        } else {
            block.extend_from_slice(&[0; 4]); // VGA latches are not CPU-readable.
        }
        debug_assert_eq!(block.len(), INT10_STATE_HARDWARE_LEN);
        self.write_guest_linear_block(dst, &block);
    }

    fn restore_video_hardware_state(&mut self, src: u32) {
        let block = self.read_guest_linear_block(src, INT10_STATE_HARDWARE_LEN);
        if block.len() != INT10_STATE_HARDWARE_LEN {
            return;
        }
        let crtc_addr = u16::from_le_bytes([block[0x40], block[0x41]]);
        let crtc_addr = if crtc_addr == 0x03B4 { 0x03B4 } else { 0x03D4 };
        let misc = self.vega.legacy_mut().read_port(0x3CC).unwrap_or(0x67);
        let misc = (misc & !0x01) | u8::from(crtc_addr == 0x03D4);
        let _ = self.vega.legacy_mut().write_port(0x3C2, misc);
        if block[INT10_STATE_CGA_LATCH_OFFSET..INT10_STATE_CGA_LATCH_OFFSET + 2]
            == INT10_STATE_CGA_LATCH_MARKER
        {
            let _ = self
                .vega
                .legacy_mut()
                .write_port(0x3D8, block[INT10_STATE_CGA_LATCH_OFFSET + 2]);
            let _ = self
                .vega
                .legacy_mut()
                .write_port(0x3D9, block[INT10_STATE_CGA_LATCH_OFFSET + 3]);
        }

        let mut offset = 5;
        for index in 1..=4 {
            let _ = self.vega.legacy_mut().write_port(0x3C4, index);
            let _ = self.vega.legacy_mut().write_port(0x3C5, block[offset]);
            offset += 1;
        }
        let _ = self.vega.legacy_mut().write_port(0x3C4, 0);
        let _ = self.vega.legacy_mut().write_port(0x3C5, block[offset]);
        offset += 1;

        let _ = self.vega.legacy_mut().write_port(crtc_addr, 0x11);
        let _ = self.vega.legacy_mut().write_port(crtc_addr + 1, 0x00);
        for index in 0..=0x18 {
            let value = block[offset + index as usize];
            if index != 0x11 {
                let _ = self.vega.legacy_mut().write_port(crtc_addr, index);
                let _ = self.vega.legacy_mut().write_port(crtc_addr + 1, value);
            }
        }
        let crtc_offset = offset;
        offset += 0x19;
        let _ = self.vega.legacy_mut().write_port(crtc_addr, 0x11);
        let _ = self
            .vega
            .legacy_mut()
            .write_port(crtc_addr + 1, block[crtc_offset + 0x11]);

        let ar_index = block[3];
        for index in 0..=0x13 {
            self.vega.legacy_mut().read_status1();
            let _ = self
                .vega
                .legacy_mut()
                .write_port(0x3C0, index | (ar_index & 0x20));
            let _ = self.vega.legacy_mut().write_port(0x3C0, block[offset]);
            offset += 1;
        }
        self.vega.legacy_mut().read_status1();
        let _ = self.vega.legacy_mut().write_port(0x3C0, ar_index);
        self.vega.legacy_mut().read_status1();

        for index in 0..=0x08 {
            let _ = self.vega.legacy_mut().write_port(0x3CE, index);
            let _ = self.vega.legacy_mut().write_port(0x3CF, block[offset]);
            offset += 1;
        }

        let _ = self.vega.legacy_mut().write_port(0x3C4, block[0]);
        let _ = self.vega.legacy_mut().write_port(crtc_addr, block[1]);
        let _ = self.vega.legacy_mut().write_port(0x3CE, block[2]);
        let _ = self
            .vega
            .legacy_mut()
            .write_port(crtc_addr - 4 + 0x0A, block[4]);
    }

    fn save_video_dac_state(&mut self, dst: u32) {
        let mut block = Vec::with_capacity(INT10_STATE_DAC_LEN);
        block.push(self.vega.legacy_mut().read_port(0x3C7).unwrap_or(0));
        block.push(self.vega.legacy_mut().read_port(0x3C8).unwrap_or(0));
        block.push(self.vega.legacy_mut().read_port(0x3C6).unwrap_or(0xFF));
        block.extend(self.vega.legacy_mut().dac_block_bytes(0, 256));
        block.push(self.vega.legacy_mut().attr_register(0x14));
        debug_assert_eq!(block.len(), INT10_STATE_DAC_LEN);
        self.write_guest_linear_block(dst, &block);
    }

    fn restore_video_dac_state(&mut self, src: u32) {
        let block = self.read_guest_linear_block(src, INT10_STATE_DAC_LEN);
        if block.len() != INT10_STATE_DAC_LEN {
            return;
        }
        let _ = self.vega.legacy_mut().write_port(0x3C6, block[2]);
        let grayscale = self.vega.legacy_mut().grayscale_summing_enabled();
        self.vega.legacy_mut().set_grayscale_summing_enabled(false);
        for index in 0..=255usize {
            let base = 3 + index * 3;
            self.vega.legacy_mut().set_dac_entry(
                index as u8,
                block[base],
                block[base + 1],
                block[base + 2],
            );
        }
        self.vega
            .legacy_mut()
            .set_grayscale_summing_enabled(grayscale);
        self.vega
            .legacy_mut()
            .set_attr_register(0x14, block[INT10_STATE_DAC_LEN - 1]);
        let _ = self.vega.legacy_mut().write_port(0x3C8, block[1]);
    }

    /// INT 10h AH=1Ch SAVE/RESTORE VIDEO STATE. AL=00 returns the buffer size in
    /// 64-byte blocks (BX), AL=01 saves the requested state into ES:BX, AL=02 restores
    /// it. CX is the requested-state bitmap: bit 0 hardware registers, bit 1 BDA,
    /// bit 2 DAC/palette.
    ///
    /// ES:BX is a LINEAR address, the caller's own. The BDA end of the copy is
    /// not: 0449h is the BIOS data area, which this service owns and addresses
    /// physically the way every other BDA access in this file does. Only the
    /// caller's side of each copy goes through the page walk. Requesting all
    /// three states saves over 900 bytes, so the caller's block straddles a page
    /// boundary for most placements and the per-page split matters here.
    fn int10_save_restore_state(&mut self, al: u8) {
        const BDA_VIDEO_START: u32 = 0x449;
        match al {
            0x00 => {
                let cx = self.cpu.registers.ecx() as u16;
                self.set_bx(Self::int10_state_size_blocks(cx));
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                let cx = self.cpu.registers.ecx() as u16;
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let mut dst = es.wrapping_add(u32::from(bx));
                if cx & 0x0001 != 0 {
                    self.save_video_hardware_state(dst);
                    dst = dst.wrapping_add(INT10_STATE_HARDWARE_LEN as u32);
                }
                if cx & 0x0002 != 0 {
                    let block = self.read_guest_block(BDA_VIDEO_START, INT10_STATE_BDA_LEN);
                    self.write_guest_linear_block(dst, &block);
                    dst = dst.wrapping_add(INT10_STATE_BDA_LEN as u32);
                }
                if cx & 0x0004 != 0 {
                    self.save_video_dac_state(dst);
                }
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            0x02 => {
                let cx = self.cpu.registers.ecx() as u16;
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let mut from = es.wrapping_add(u32::from(bx));
                if cx & 0x0001 != 0 {
                    self.restore_video_hardware_state(from);
                    from = from.wrapping_add(INT10_STATE_HARDWARE_LEN as u32);
                }
                if cx & 0x0002 != 0 {
                    let block = self.read_guest_linear_block(from, INT10_STATE_BDA_LEN);
                    self.write_guest_block(BDA_VIDEO_START, &block);
                    from = from.wrapping_add(INT10_STATE_BDA_LEN as u32);
                }
                if cx & 0x0004 != 0 {
                    self.restore_video_dac_state(from);
                }
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            _ => self.set_int_frame_carry(true),
        }
    }

    /// INT 10h text-mode output and cursor services. Text and EGA graphics modes
    /// use BH/page-aware BDA cursor slots; CGA graphics remains single-page.
    pub(super) fn handle_int10_text(&mut self, ah: u8) {
        let ax = self.cpu.registers.eax() as u16;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let page = self.normalize_bios_page((bx >> 8) as u8);
        let bl = bx as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let dl = dx as u8;
        let dh = (dx >> 8) as u8;
        let columns = self.text_columns();
        let rows = self.text_rows();
        match ah {
            // AH=01h set cursor shape: store the BIOS request in the BDA and
            // program the modeled CRTC cursor shape, including VGA cursor
            // emulation scaling for legacy 8-scanline requests.
            0x01 => {
                let (bda_shape, start, end) = self.bios_cursor_shape(cx);
                let _ = self.write_guest_ram_u16(0x460, bda_shape);
                self.vega.legacy_mut().set_cursor_shape(start, end);
            }
            // AH=02h set cursor position: DH=row, DL=col.
            0x02 => {
                self.set_cursor_pos(page, (u16::from(dh) << 8) | u16::from(dl));
            }
            // AH=03h get cursor position and shape.
            0x03 => {
                let pos = self.cursor_pos(page);
                let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(pos);
                self.cpu.registers.set_edx(edx);
                let shape = self.read_guest_word(0x460);
                let shape = if shape == 0 { 0x0607 } else { shape };
                let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(shape);
                self.cpu.registers.set_ecx(ecx);
            }
            // AH=06h/07h scroll the window up/down. AL=0 blanks it.
            0x06 | 0x07 => self.scroll_window(ah == 0x06, al, bx >> 8, cx, dx),
            // AH=08h read char+attr at the cursor.
            0x08 => {
                let pos = self.cursor_pos(page);
                let row = usize::from(pos >> 8);
                let col = usize::from(pos & 0xff);
                let (ch, at) = if self.is_bios_graphics_text_mode() {
                    self.read_graphics_char(page, row, col)
                } else {
                    let off = self.text_offset(page, row, col);
                    (
                        self.vega.legacy_mut().read_u8(off).unwrap_or(b' '),
                        self.vega.legacy_mut().read_u8(off + 1).unwrap_or(0x07),
                    )
                };
                let eax =
                    (self.cpu.registers.eax() & !0xFFFF) | (u32::from(at) << 8) | u32::from(ch);
                self.cpu.registers.set_eax(eax);
            }
            // AH=09h write char+attr, AH=0Ah write char only, CX times, no advance.
            0x09 | 0x0A => {
                let pos = self.cursor_pos(page);
                let row = usize::from(pos >> 8);
                let col = usize::from(pos & 0xff);
                for i in 0..usize::from(cx) {
                    let target_col = col + i;
                    if row >= rows || target_col >= columns {
                        break;
                    }
                    if self.is_bios_graphics_text_mode() {
                        self.draw_graphics_char(page, row, target_col, al, bl);
                    } else {
                        let off = self.text_offset(page, row, target_col);
                        let _ = self.vega.legacy_mut().write_u8(off, al);
                        if ah == 0x09 {
                            let _ = self.vega.legacy_mut().write_u8(off + 1, bl);
                        }
                    }
                }
            }
            // AH=0Eh teletype.
            0x0E => self.teletype_char_attr(al, bl, page),
            _ => {}
        }
    }

    fn write_bios_char_cell(&mut self, page: u8, row: usize, col: usize, ch: u8, attr: u8) {
        if self.is_bios_graphics_text_mode() {
            self.draw_graphics_char(page, row, col, ch, attr);
        } else {
            let off = self.text_offset(page, row, col);
            let _ = self.vega.legacy_mut().write_u8(off, ch);
            let _ = self.vega.legacy_mut().write_u8(off + 1, attr);
        }
    }

    fn is_bios_graphics_text_mode(&self) -> bool {
        matches!(
            self.vega.legacy().active_mode(),
            VideoMode::Cga | VideoMode::Planar
        )
    }

    fn graphics_text_cell_height(&mut self) -> usize {
        match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga => 8,
            VideoMode::Planar => usize::from(self.read_physical_u8(0x485)).clamp(1, 32),
            _ => 16,
        }
    }

    fn graphics_page_start(&mut self, page: u8) -> u32 {
        if self.vega.legacy_mut().active_mode() != VideoMode::Planar {
            return 0;
        }
        let mode = self.read_physical_u8(0x449);
        self.ega_graphics_page_start(mode, page)
            .map(|(_, start)| start)
            .unwrap_or(0)
    }

    fn graphics_write_pixel(&mut self, page: u8, x: u16, y: u16, color: u8, xor: bool) -> bool {
        match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga => self.vega.legacy_mut().cga_write_pixel(x, y, color, xor),
            VideoMode::Planar => {
                let start = self.graphics_page_start(page);
                self.vega
                    .legacy_mut()
                    .planar_write_pixel_at(start, x, y, color, xor)
            }
            _ => false,
        }
    }

    fn graphics_read_pixel(&mut self, page: u8, x: u16, y: u16) -> u8 {
        match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga => self.vega.legacy_mut().cga_read_pixel(x, y),
            VideoMode::Planar => {
                let start = self.graphics_page_start(page);
                self.vega.legacy_mut().planar_read_pixel_at(start, x, y)
            }
            _ => 0,
        }
    }

    fn draw_graphics_char(&mut self, page: u8, row: usize, col: usize, ch: u8, color: u8) {
        let x0 = col * 8;
        let cell_height = self.graphics_text_cell_height();
        let y0 = row * cell_height;
        let xor = color & 0x80 != 0;
        let fg = match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga => color & 0x7F,
            VideoMode::Planar => color & 0x0F,
            _ => color,
        };
        for gy in 0..cell_height {
            let bits = self.graphics_glyph_row(ch, gy);
            for gx in 0..8usize {
                let lit = bits & (0x80 >> gx) != 0;
                if xor {
                    if lit {
                        let _ = self.graphics_write_pixel(
                            page,
                            (x0 + gx) as u16,
                            (y0 + gy) as u16,
                            fg,
                            true,
                        );
                    }
                } else {
                    let _ = self.graphics_write_pixel(
                        page,
                        (x0 + gx) as u16,
                        (y0 + gy) as u16,
                        if lit { fg } else { 0 },
                        false,
                    );
                }
            }
        }
    }

    fn graphics_glyph_row(&mut self, ch: u8, row: usize) -> u8 {
        if self.vega.legacy_mut().active_mode() != VideoMode::Cga || ch < 0x80 {
            return self.vega.legacy_mut().active_font_glyph_row(ch, row);
        }
        let offset = self.read_guest_word(0x1F * 4);
        let segment = self.read_guest_word(0x1F * 4 + 2);
        if offset == 0 && segment == 0 {
            return self.vega.legacy_mut().active_font_glyph_row(ch, row);
        }
        let base = u32::from(segment) * 16 + u32::from(offset);
        self.read_physical_u8(base + u32::from(ch - 0x80) * 8 + row.min(7) as u32)
    }

    fn read_graphics_char(&mut self, page: u8, row: usize, col: usize) -> (u8, u8) {
        let x0 = col * 8;
        let cell_height = self.graphics_text_cell_height();
        let y0 = row * cell_height;
        if row >= self.text_rows() || x0 + 8 > self.vega.legacy_mut().raster_width() as usize {
            return (0, 0);
        }
        let max_fg = match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga if self.vega.legacy_mut().raster_width() >= 640 => 1,
            VideoMode::Cga => 3,
            VideoMode::Planar => 15,
            _ => 0,
        };
        for fg in 1..=max_fg {
            let present = (0..cell_height).any(|gy| {
                (0..8usize).any(|gx| {
                    self.graphics_read_pixel(page, (x0 + gx) as u16, (y0 + gy) as u16) == fg
                })
            });
            if !present {
                continue;
            }
            for ch in 0..=u8::MAX {
                let mut matches = true;
                for gy in 0..cell_height {
                    let mut row_bits = 0u8;
                    for gx in 0..8usize {
                        if self.graphics_read_pixel(page, (x0 + gx) as u16, (y0 + gy) as u16) == fg
                        {
                            row_bits |= 0x80 >> gx;
                        }
                    }
                    if row_bits != self.graphics_glyph_row(ch, gy) {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    return (ch, fg);
                }
            }
        }
        (b' ', 0)
    }

    /// Scroll a text window. `up` selects direction; `lines`==0 blanks the whole
    /// window. `attr` fills the vacated rows; `cx`=top-left (CH row, CL col),
    /// `dx`=bottom-right (DH row, DL col). Clamped to the active text screen.
    fn scroll_window(&mut self, up: bool, lines: u8, attr: u16, cx: u16, dx: u16) {
        let attr = attr as u8;
        let page = self.active_bios_page();
        let columns = self.text_columns();
        let rows = self.text_rows();
        let top = usize::from((cx >> 8) as u8).min(rows - 1);
        let left = usize::from(cx as u8).min(columns - 1);
        let bottom = usize::from((dx >> 8) as u8).min(rows - 1).max(top);
        let right = usize::from(dx as u8).min(columns - 1).max(left);
        let height = bottom - top + 1;
        let n = if lines == 0 {
            height
        } else {
            usize::from(lines)
        };
        if self.is_bios_graphics_text_mode() {
            self.scroll_graphics_window(page, up, n, attr, top, left, bottom, right);
            return;
        }
        if n >= height {
            for row in top..=bottom {
                self.blank_text_row(page, row, left, right, attr);
            }
            return;
        }
        if up {
            for row in top..=(bottom - n) {
                self.copy_text_row(page, row + n, row, left, right, attr);
            }
            for row in (bottom - n + 1)..=bottom {
                self.blank_text_row(page, row, left, right, attr);
            }
        } else {
            for row in ((top + n)..=bottom).rev() {
                self.copy_text_row(page, row - n, row, left, right, attr);
            }
            for row in top..(top + n) {
                self.blank_text_row(page, row, left, right, attr);
            }
        }
    }

    /// Copy a span of text cells from `src_row` to `dst_row` (inclusive columns).
    fn copy_text_row(
        &mut self,
        page: u8,
        src_row: usize,
        dst_row: usize,
        left: usize,
        right: usize,
        attr: u8,
    ) {
        for col in left..=right {
            let src = self.text_offset(page, src_row, col);
            let dst = self.text_offset(page, dst_row, col);
            let b0 = self.vega.legacy_mut().read_u8(src).unwrap_or(b' ');
            let b1 = self.vega.legacy_mut().read_u8(src + 1).unwrap_or(attr);
            let _ = self.vega.legacy_mut().write_u8(dst, b0);
            let _ = self.vega.legacy_mut().write_u8(dst + 1, b1);
        }
    }

    /// Blank a span of text cells to spaces with `attr` (inclusive columns).
    fn blank_text_row(&mut self, page: u8, row: usize, left: usize, right: usize, attr: u8) {
        for col in left..=right {
            let off = self.text_offset(page, row, col);
            let _ = self.vega.legacy_mut().write_u8(off, b' ');
            let _ = self.vega.legacy_mut().write_u8(off + 1, attr);
        }
    }

    /// INT 10h AH=10h: set/get the ATC palette registers and the DAC. Covers the
    /// set/get forms for the attribute palette (00/01/02/03/07/08/09) and the DAC
    /// (10/12/13/15/17/18/19/1A/1B). Register conventions per RBIL (INT 10/AH=10h).
    ///
    /// The ES:DX block of the 02/09/12/17 forms is the caller's LINEAR address:
    /// a palette or DAC block loaded by a program running in V86 under a memory
    /// manager can sit in non-identity-mapped upper memory. See
    /// `write_guest_linear_block`.
    fn handle_int10_palette(&mut self, al: u8) {
        let bx = self.cpu.registers.ebx() as u16;
        let bl = bx as u8;
        let bh = (bx >> 8) as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let ch = (cx >> 8) as u8;
        let cl = cx as u8;
        let dx = self.cpu.registers.edx() as u16;
        let dh = (dx >> 8) as u8;
        let es_base = self.cpu.registers.segment(SegmentIndex::Es).base;
        let es_dx = es_base.wrapping_add(u32::from(dx));
        match al {
            // AL=00: set individual Attribute register. BL=index, BH=value.
            0x00 => {
                self.vega.legacy_mut().set_attr_register(bl, bh);
                if self.vega.legacy_mut().is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=01: set overscan/border color. BH=value (overlap with AH=0Bh).
            0x01 => {
                self.vega.legacy_mut().set_overscan(bh);
                if self.vega.legacy_mut().is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=02: set all 16 palette registers and overscan from ES:DX (17 bytes).
            0x02 => {
                let block = self.read_guest_linear_block(es_dx, 17);
                for i in 0..16u8 {
                    self.vega
                        .legacy_mut()
                        .set_attr_palette_reg(i, block[i as usize]);
                }
                self.vega.legacy_mut().set_overscan(block[16]);
                if self.vega.legacy_mut().is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=03: BL=0 enables bright backgrounds, BL=1 enables blink.
            0x03 => {
                self.vega
                    .legacy_mut()
                    .set_text_blink_enabled(bl & 0x01 != 0);
                if self.vega.legacy_mut().is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=07: get individual Attribute register. BL=index -> BH.
            0x07 => {
                let value = self.vega.legacy_mut().attr_register(bl);
                let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(value) << 8);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=08: read overscan/border color -> BH.
            0x08 => {
                let value = self.vega.legacy_mut().overscan();
                let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(value) << 8);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=09: read all 16 palette registers + overscan into ES:DX (17 bytes).
            0x09 => {
                let mut block = [0u8; 17];
                for (i, slot) in block.iter_mut().take(16).enumerate() {
                    *slot = self.vega.legacy_mut().attr_palette_reg(i as u8);
                }
                block[16] = self.vega.legacy_mut().overscan();
                self.write_guest_linear_block(es_dx, &block);
            }
            // AL=10: set individual DAC register. BX=index, DH=R, CH=G, CL=B.
            0x10 => self.vega.legacy_mut().set_dac_entry(bx as u8, dh, ch, cl),
            // AL=12: set a block of DAC registers. BX=start, CX=count, ES:DX -> RGB triples.
            0x12 => {
                let bytes = self.read_guest_linear_block(es_dx, cx as usize * 3);
                let entries: Vec<[u8; 3]> =
                    bytes.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                self.vega.legacy_mut().set_dac_block(bx as u8, &entries);
            }
            // AL=13: select DAC colour-page mode/page. BL=0 picks four 64-colour
            // pages (BH=0) vs sixteen 16-colour pages (BH=1); BL=1 selects page.
            0x13 => match bl {
                0x00 => {
                    let mut mode_control = self.vega.legacy_mut().attr_register(0x10);
                    if bh & 0x01 != 0 {
                        mode_control |= 0x80;
                    } else {
                        mode_control &= !0x80;
                    }
                    self.vega.legacy_mut().set_attr_register(0x10, mode_control);
                }
                0x01 => {
                    let color_select = self.vega.legacy_mut().attr_register(0x14);
                    let page = if self.vega.legacy_mut().attr_register(0x10) & 0x80 != 0 {
                        bh & 0x0F
                    } else {
                        (color_select & 0x03) | ((bh & 0x03) << 2)
                    };
                    self.vega.legacy_mut().set_attr_register(0x14, page);
                }
                _ => {}
            },
            // AL=15: get individual DAC register. BX=index -> DH=R, CH=G, CL=B.
            0x15 => {
                let [r, g, b] = self.vega.legacy_mut().dac_entry(bx as u8);
                let edx = (self.cpu.registers.edx() & !0xFF00) | (u32::from(r) << 8);
                self.cpu.registers.set_edx(edx);
                let ecx_new =
                    (self.cpu.registers.ecx() & !0xFFFF) | (u32::from(g) << 8) | u32::from(b);
                self.cpu.registers.set_ecx(ecx_new);
            }
            // AL=17: get a block of DAC registers. BX=start, CX=count -> ES:DX.
            0x17 => {
                let bytes = self.vega.legacy_mut().dac_block_bytes(bx as u8, cx);
                self.write_guest_linear_block(es_dx, &bytes);
            }
            // AL=18: set PEL mask. BL=value.
            0x18 => {
                let _ = self.vega.legacy_mut().write_port(0x3C6, bl);
            }
            // AL=19: read PEL mask -> BL.
            0x19 => {
                let value = self.vega.legacy_mut().read_port(0x3C6).unwrap_or(0xFF);
                let ebx = (self.cpu.registers.ebx() & !0xFF) | u32::from(value);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=1A: read DAC page state -> BL=paging mode, BH=current page.
            0x1A => {
                let mode = u8::from(self.vega.legacy_mut().attr_register(0x10) & 0x80 != 0);
                let color_select = self.vega.legacy_mut().attr_register(0x14);
                let page = if mode == 0 {
                    (color_select >> 2) & 0x03
                } else {
                    color_select & 0x0F
                };
                let ebx =
                    (self.cpu.registers.ebx() & !0xFFFF) | (u32::from(page) << 8) | u32::from(mode);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=1B: sum a block of DAC registers to gray scale. BX=start, CX=count.
            // The NTSC luma weights (30% R, 59% G, 11% B) collapse each entry to a
            // single gray level, the way the BIOS gray-scale-summing routine does.
            0x1B => {
                let start = bx as u8;
                for offset in 0..cx {
                    let index = start.wrapping_add(offset as u8);
                    let [r, g, b] = self.vega.legacy_mut().dac_entry(index);
                    let gray =
                        ((u16::from(r) * 77 + u16::from(g) * 151 + u16::from(b) * 28) >> 8) as u8;
                    self.vega
                        .legacy_mut()
                        .set_dac_entry(index, gray, gray, gray);
                }
            }
            _ => {}
        }
    }

    /// INT 10h AH=11h: the character-generator font services (RBIL). AL=00/10
    /// loads a user font at ES:BP (CX glyphs, DX first char, BH bytes/char, BL
    /// block); AL=01/11, 02/12, 04/14 load the ROM 8x14, 8x8, 8x16 fonts (BL
    /// block); AL=03 sets the block specifier (BL -> Sequencer index 3). The 1x
    /// variants also reprogram the CRTC character height. AL=20 installs the
    /// 8x8 CGA graphics-character pointer at INT 1Fh; AL=21-24 select the
    /// planar graphics-mode BIOS text font and row grid; AL=30 returns the
    /// requested font pointer (BH=00..07) plus the live BDA font height/rows.
    /// Text-font register conventions verified against the LGPL VGABios
    /// `biosfn_load_text_*`; graphics-font register conventions follow RBIL.
    ///
    /// The user-font source at ES:BP (AL=00/10 and AL=21) is the caller's
    /// LINEAR address; the INT 43h font image this service publishes is not,
    /// because that lives in the video BIOS image the emulator owns. See
    /// `read_guest_linear_block`.
    fn handle_int10_font(&mut self, al: u8) {
        let bx = self.cpu.registers.ebx() as u16;
        let bl = bx as u8;
        let bh = (bx >> 8) as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let table = self.vega.legacy_mut().char_map_table(bl);
        match al {
            0x00 | 0x10 => {
                let bp = self.cpu.registers.ebp() as u16;
                let es_base = self.cpu.registers.segment(SegmentIndex::Es).base;
                // load_font_table folds character codes modulo 256, so any
                // glyphs beyond the first 256 only rewrite earlier codes. Cap
                // the read there to keep a pathological CX (a u16 up to 65535)
                // from stalling the emulator with up to ~16 million
                // byte-at-a-time bus reads plus a multi-megabyte allocation.
                let count = (cx as usize).min(256);
                let bytes = self.read_guest_linear_block(
                    es_base.wrapping_add(u32::from(bp)),
                    count * bh as usize,
                );
                self.vega
                    .legacy_mut()
                    .load_font_table(table, dx, bh, &bytes);
                self.set_int43_pointer(self.cpu.registers.segment(SegmentIndex::Es).selector, bp);
                if al >= 0x10 {
                    self.vega.legacy_mut().set_char_height(bh);
                }
                self.publish_int43_font_table();
            }
            0x01 | 0x11 => {
                self.vega.legacy_mut().load_rom_font(table, 14);
                self.set_int43_pointer(BIOS_ROM_SEGMENT, BIOS_FONT_8X14_ROM_OFFSET);
                if al >= 0x10 {
                    self.vega.legacy_mut().set_char_height(14);
                }
                self.publish_int43_font_table();
            }
            0x02 | 0x12 => {
                self.vega.legacy_mut().load_rom_font(table, 8);
                self.set_int43_pointer(BIOS_ROM_SEGMENT, BIOS_FONT_8X8_ROM_OFFSET);
                if al >= 0x10 {
                    self.vega.legacy_mut().set_char_height(8);
                }
                self.publish_int43_font_table();
            }
            0x04 | 0x14 => {
                self.vega.legacy_mut().load_rom_font(table, 16);
                self.set_int43_pointer(BIOS_ROM_SEGMENT, BIOS_FONT_8X16_ROM_OFFSET);
                if al >= 0x10 {
                    self.vega.legacy_mut().set_char_height(16);
                }
                self.publish_int43_font_table();
            }
            0x03 => {
                self.vega.legacy_mut().set_char_map_select(bl);
                self.publish_int43_font_table();
            }
            0x20 => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
                let bp = self.cpu.registers.ebp() as u16;
                let _ = self.write_guest_ram_u16(0x1F * 4, bp);
                let _ = self.write_guest_ram_u16(0x1F * 4 + 2, es);
            }
            0x21 => {
                let bp = self.cpu.registers.ebp() as u16;
                let es_base = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bytes_per_char = cx.clamp(1, 32) as u8;
                let bytes = self.read_guest_linear_block(
                    es_base.wrapping_add(u32::from(bp)),
                    256 * usize::from(bytes_per_char),
                );
                self.vega.legacy_mut().set_char_map_select(0);
                self.vega
                    .legacy_mut()
                    .load_font_table(0, 0, bytes_per_char, &bytes);
                self.set_graphics_font_grid(bytes_per_char, bl, dx as u8);
            }
            0x22 => self.load_rom_graphics_font(14, bl, dx as u8),
            0x23 => self.load_rom_graphics_font(8, bl, dx as u8),
            0x24 => self.load_rom_graphics_font(16, bl, dx as u8),
            0x30 => {
                if bh == 0x01 {
                    self.publish_int43_font_table();
                }
                self.int10_font_info(bh);
            }
            _ => {}
        }
    }

    fn set_int43_pointer(&mut self, segment: u16, offset: u16) {
        let _ = self.write_guest_ram_u16(0x43 * 4, offset);
        let _ = self.write_guest_ram_u16(0x43 * 4 + 2, segment);
    }

    fn publish_int43_font_table(&mut self) {
        let height = self.vega.legacy_mut().char_height();
        let table = self.vega.legacy_mut().active_font_table();
        let bytes = self.vega.legacy_mut().font_table_image(table, height);
        self.write_guest_block(VGA_BIOS_INT43_FONT_ADDR, &bytes);
        self.set_int43_pointer(VGA_BIOS_SEGMENT, VGA_BIOS_FONT_TABLE_OFF);
        let _ = self.write_guest_ram_u8(0x485, height);
    }

    fn int10_font_info(&mut self, specifier: u8) {
        let Some((segment, offset)) = self.font_info_pointer(specifier) else {
            return;
        };
        self.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(segment));
        self.cpu
            .registers
            .set_ebp((self.cpu.registers.ebp() & !0xFFFF) | u32::from(offset));
        let char_height = self.read_physical_u8(0x485).max(1);
        self.set_cx(u16::from(char_height));
        let rows_minus_1 = self.read_physical_u8(0x484);
        let edx = (self.cpu.registers.edx() & !0xFF) | u32::from(rows_minus_1);
        self.cpu.registers.set_edx(edx);
    }

    fn font_info_pointer(&mut self, specifier: u8) -> Option<(u16, u16)> {
        Some(match specifier {
            0x00 => (
                self.read_guest_word(0x1F * 4 + 2),
                self.read_guest_word(0x1F * 4),
            ),
            0x01 => (
                self.read_guest_word(0x43 * 4 + 2),
                self.read_guest_word(0x43 * 4),
            ),
            0x02 | 0x05 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X14_ROM_OFFSET),
            0x03 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X8_ROM_OFFSET),
            0x04 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X8_HIGH_ROM_OFFSET),
            0x06 | 0x07 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X16_ROM_OFFSET),
            _ => return None,
        })
    }

    fn load_rom_graphics_font(&mut self, height: u8, row_specifier: u8, user_rows: u8) {
        self.vega.legacy_mut().set_char_map_select(0);
        self.vega.legacy_mut().load_rom_font(0, height);
        self.set_graphics_font_grid(height, row_specifier, user_rows);
    }

    fn set_graphics_font_grid(&mut self, bytes_per_char: u8, row_specifier: u8, user_rows: u8) {
        if self.vega.legacy_mut().active_mode() != VideoMode::Planar {
            return;
        }
        let rows = match row_specifier {
            0 => user_rows,
            1 => 14,
            2 => 25,
            3 => 43,
            _ => self.text_rows() as u8,
        }
        .clamp(1, 60);
        let _ = self.write_guest_ram_u8(0x484, rows - 1);
        let _ = self.write_guest_ram_u16(0x485, u16::from(bytes_per_char));
    }

    /// Write one character to the VGA text screen at the BDA cursor, advancing it
    /// with CR, LF, backspace, tab, and bottom-of-screen scroll, the way the BIOS
    /// teletype (INT 10h AH=0Eh) does. Attribute 0x07 is light grey on black.
    pub(super) fn teletype_char(&mut self, byte: u8) {
        let page = self.active_bios_page();
        self.teletype_char_attr(byte, 0x07, page);
    }

    fn teletype_char_attr(&mut self, byte: u8, attr: u8, page: u8) {
        let page = self.normalize_bios_page(page);
        let cursor = self.cursor_pos(page);
        let columns = self.text_columns();
        let mut col = usize::from(cursor & 0x00ff);
        let mut row = usize::from(cursor >> 8);
        match byte {
            b'\r' => col = 0,
            b'\n' => row += 1,
            0x08 => col = col.saturating_sub(1), // backspace
            0x07 => {}                           // bell: no visible effect
            b'\t' => {
                col = (col + 8) & !7;
                if col >= columns {
                    col = 0;
                    row += 1;
                }
            }
            _ => {
                self.write_bios_char_cell(page, row, col, byte, attr);
                col += 1;
                if col >= columns {
                    col = 0;
                    row += 1;
                }
            }
        }
        while row >= self.text_rows() {
            self.scroll_text_up(page);
            row -= 1;
        }
        self.set_cursor_pos(page, ((row as u16) << 8) | col as u16);
    }

    /// Scroll the active text screen up one line, clearing the bottom row to
    /// spaces with the normal attribute.
    fn scroll_text_up(&mut self, page: u8) {
        if self.is_bios_graphics_text_mode() {
            self.scroll_graphics_text_up(page);
            return;
        }
        let base = self.text_page_base(page);
        let columns = self.text_columns();
        let rows = self.text_rows();
        let row_bytes = columns * 2;
        for offset in 0..((rows - 1) * row_bytes) {
            let byte = self
                .vega
                .legacy_mut()
                .read_u8(base + offset + row_bytes)
                .unwrap_or(b' ');
            let _ = self.vega.legacy_mut().write_u8(base + offset, byte);
        }
        let last = base + (rows - 1) * row_bytes;
        for col in 0..columns {
            let _ = self.vega.legacy_mut().write_u8(last + col * 2, b' ');
            let _ = self.vega.legacy_mut().write_u8(last + col * 2 + 1, 0x07);
        }
    }

    fn scroll_graphics_text_up(&mut self, page: u8) {
        let columns = self.text_columns();
        let rows = self.text_rows();
        self.scroll_graphics_window(page, true, 1, 0, 0, 0, rows - 1, columns - 1);
    }

    #[allow(clippy::too_many_arguments)]
    fn scroll_graphics_window(
        &mut self,
        page: u8,
        up: bool,
        lines: usize,
        color: u8,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
    ) {
        let cell_height = self.graphics_text_cell_height();
        let x0 = (left * 8) as u16;
        let x1 = ((right + 1) * 8).min(self.vega.legacy_mut().raster_width() as usize) as u16;
        let y0 = (top * cell_height) as u16;
        let y1 = ((bottom + 1) * cell_height) as u16;
        let height = bottom - top + 1;
        let fill = match self.vega.legacy_mut().active_mode() {
            VideoMode::Cga => color & 0x7F,
            VideoMode::Planar => color & 0x0F,
            _ => color,
        };

        if lines >= height {
            for y in y0..y1 {
                for x in x0..x1 {
                    let _ = self.graphics_write_pixel(page, x, y, fill, false);
                }
            }
            return;
        }

        let shift = (lines * cell_height) as u16;
        if up {
            for y in y0..(y1 - shift) {
                for x in x0..x1 {
                    let color = self.graphics_read_pixel(page, x, y + shift);
                    let _ = self.graphics_write_pixel(page, x, y, color, false);
                }
            }
            for y in (y1 - shift)..y1 {
                for x in x0..x1 {
                    let _ = self.graphics_write_pixel(page, x, y, fill, false);
                }
            }
        } else {
            for y in (y0 + shift..y1).rev() {
                for x in x0..x1 {
                    let color = self.graphics_read_pixel(page, x, y - shift);
                    let _ = self.graphics_write_pixel(page, x, y, color, false);
                }
            }
            for y in y0..(y0 + shift) {
                for x in x0..x1 {
                    let _ = self.graphics_write_pixel(page, x, y, fill, false);
                }
            }
        }
    }

    /// VBE (`INT 10h`, `AH=4Fh`). `function` is `AL`. Unimplemented functions
    /// leave `AX` unchanged, so `AL != 0x4F` signals "not supported" to the guest.
    fn handle_vbe(&mut self, function: u8) {
        match function {
            0x00 => self.vbe_controller_info(),
            0x01 => self.vbe_mode_info(),
            0x02 => self.vbe_set_mode(),
            0x03 => self.vbe_current_mode(),
            0x05 => self.vbe_window_control(),
            0x07 => self.vbe_display_start(),
            0x08 => self.vbe_dac_format(),
            0x09 => self.vbe_palette_data(),
            0x0a => self.vbe_protected_mode_interface(),
            _ => {}
        }
    }

    /// VBE 2.0 function 0Ah: hand back the protected-mode interface block. The
    /// caller far-calls the routines inside it directly, with no INT 10h
    /// involved, so unlike every other function here the answer is an ADDRESS
    /// of real code rather than an emulated effect -- see `izbios-vbepm.inc`.
    ///
    /// NASCAR Racing 2 is why this exists. It asks 4F00h, then 4F0Ah, and treats
    /// an unsupported 4F0Ah as "no VESA driver" and exits, without ever trying
    /// to set a mode.
    fn vbe_protected_mode_interface(&mut self) {
        // BL selects the subfunction; only 00h (return the table) is defined.
        if self.cpu.registers.ebx() as u8 != 0x00 {
            self.set_vbe_status(0x014f);
            return;
        }
        let offset = izarravm_firmware::IZARRA_BIOS_VBE_PM_OFFSET;
        let length = izarravm_firmware::izarra_bios_vbe_pm_len();
        self.cpu.registers.set_segment(
            SegmentIndex::Es,
            SegmentRegister::real(izarravm_firmware::IZARRA_BIOS_SEG),
        );
        self.cpu.registers.set_edi(u32::from(offset));
        self.cpu
            .registers
            .set_ecx((self.cpu.registers.ecx() & 0xffff_0000) | u32::from(length));
        self.set_vbe_status(0x004f);
    }

    fn vbe_controller_info(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
        let di = self.cpu.registers.edi() as u16;
        let mut block = [0u8; 256];
        block[0x00..0x04].copy_from_slice(b"VESA");
        block[0x04..0x06].copy_from_slice(&0x0200u16.to_le_bytes()); // VbeVersion
        block[0x0a..0x0e].copy_from_slice(&1u32.to_le_bytes()); // 6/8-bit DAC switching
        block[0x12..0x14].copy_from_slice(&64u16.to_le_bytes()); // TotalMemory: 64 * 64 KB = 4 MB

        // OemStringPtr, into the ROM. A card-identification string is what
        // SciTech's tools and several games read to decide what they are talking
        // to, and a null pointer here is a far pointer to the interrupt table.
        let oem_ptr = (u32::from(izarravm_firmware::IZARRA_BIOS_SEG) << 16)
            | u32::from(izarravm_firmware::izarra_bios_vbe_oem_string_offset());
        block[0x06..0x0a].copy_from_slice(&oem_ptr.to_le_bytes());

        // The mode list lives in the block's reserved scratch area at 0x22, where
        // period BIOSes put it. It USED TO sit at 0x14, which is fine in a VBE
        // 1.2 block and is OemSoftwareRev + the three OEM pointers in a VBE 2.0
        // one -- so a VBE2 client read mode numbers as far pointers. The three
        // OEM pointers stay null, which the spec allows; what it does not allow
        // is them holding 0x0100, 0x0101, 0x0103.
        //
        // VideoModePtr is a real-mode far pointer the guest decodes as seg:off,
        // so it carries the ES selector, not the linear base. vbe_block_linear()
        // uses the base for the write-side address, which the page walk then
        // resolves; in real mode base = selector << 4.
        let list_offset = di.wrapping_add(0x22);
        let video_mode_ptr = (u32::from(es) << 16) | u32::from(list_offset);
        block[0x0e..0x12].copy_from_slice(&video_mode_ptr.to_le_bytes());

        let mut pos = 0x22;
        for mode in MARGO_VBE_MODES {
            block[pos..pos + 2].copy_from_slice(&mode.number.to_le_bytes());
            pos += 2;
        }
        block[pos..pos + 2].copy_from_slice(&0xffffu16.to_le_bytes());

        let addr = self.vbe_block_linear();
        self.write_guest_linear_block(addr, &block);
        self.set_vbe_status(0x004f);
    }

    /// Set the `AX` low word to a VBE status (`0x004F` ok, `0x014F` failed),
    /// preserving the high word.
    fn set_vbe_status(&mut self, status: u16) {
        let eax = (self.cpu.registers.eax() & 0xffff_0000) | u32::from(status);
        self.cpu.registers.set_eax(eax);
    }

    fn vbe_set_mode(&mut self) {
        let request = self.cpu.registers.ebx() as u16;
        if self.vega.set_vbe_mode(request) {
            self.vega.legacy_mut().set_dac_component_bits(6);
            self.set_vbe_status(0x004f);
        } else {
            self.set_vbe_status(0x014f);
        }
    }

    fn vbe_current_mode(&mut self) {
        let mode = self.vega.current_vbe_mode().unwrap_or(0x0003);
        let ebx = (self.cpu.registers.ebx() & 0xffff_0000) | u32::from(mode);
        self.cpu.registers.set_ebx(ebx);
        self.set_vbe_status(0x004f);
    }

    fn vbe_window_control(&mut self) {
        let bx = self.cpu.registers.ebx() as u16;
        let bank = self.cpu.registers.edx() as u16;
        match self.vega.vbe_window_control(bx, bank) {
            Ok(selected) => {
                let edx = (self.cpu.registers.edx() & 0xffff_0000) | u32::from(selected);
                self.cpu.registers.set_edx(edx);
                self.set_vbe_status(0x004f);
            }
            Err(status) => self.set_vbe_status(status),
        }
    }

    fn vbe_display_start(&mut self) {
        if !self.vega.margo_active() {
            self.set_vbe_status(0x014f);
            return;
        }

        match self.cpu.registers.ebx() as u8 {
            0x00 | 0x80 => {
                let x = self.cpu.registers.ecx() as u16;
                let y = self.cpu.registers.edx() as u16;
                if !self.vega.program_display_start_xy(x, y) {
                    self.set_vbe_status(0x014f);
                    return;
                }
                if self.cpu.registers.ebx() as u8 == 0x80 {
                    self.stall_until_margo_frame();
                }
                self.set_vbe_status(0x004f);
            }
            0x01 => {
                let display = self.vega.margo_display();
                let depth = bytes_per_pixel(display.bpp).max(1);
                let (x, y) = if display.pitch == 0 {
                    (0, 0)
                } else {
                    (
                        (display.start % display.pitch) / depth,
                        display.start / display.pitch,
                    )
                };
                self.cpu
                    .registers
                    .set_ebx(self.cpu.registers.ebx() & !0xff00);
                self.cpu.registers.set_ecx(x);
                self.cpu.registers.set_edx(y);
                self.set_vbe_status(0x004f);
            }
            _ => self.set_vbe_status(0x014f),
        }
    }

    fn vbe_dac_format(&mut self) {
        if !self.vega.margo_active() || self.vega.margo_display().bpp != 8 {
            self.set_vbe_status(0x034f);
            return;
        }

        let bits = match self.cpu.registers.ebx() as u8 {
            0x00 => {
                let requested = ((self.cpu.registers.ebx() >> 8) & 0xff) as u8;
                if requested >= 8 {
                    8
                } else if requested >= 6 {
                    6
                } else {
                    self.set_vbe_status(0x014f);
                    return;
                }
            }
            0x01 => self.vega.legacy_mut().dac_component_bits(),
            _ => {
                self.set_vbe_status(0x014f);
                return;
            }
        };
        self.vega.legacy_mut().set_dac_component_bits(bits);
        let ebx = (self.cpu.registers.ebx() & !0xff00) | (u32::from(bits) << 8);
        self.cpu.registers.set_ebx(ebx);
        self.set_vbe_status(0x004f);
    }

    fn vbe_palette_data(&mut self) {
        if !self.vega.margo_active() || self.vega.margo_display().bpp != 8 {
            self.set_vbe_status(0x034f);
            return;
        }

        let start = self.cpu.registers.edx() as u16;
        let count = self.cpu.registers.ecx() as u16;
        if usize::from(start) + usize::from(count) > DAC_ENTRIES {
            self.set_vbe_status(0x014f);
            return;
        }
        let address = self.vbe_block_linear();
        match self.cpu.registers.ebx() as u8 {
            0x00 | 0x80 => {
                let entries = self.read_guest_linear_block(address, usize::from(count) * 4);
                if self.cpu.registers.ebx() as u8 == 0x80 {
                    self.stall_until_margo_frame();
                }
                for (offset, entry) in entries.chunks_exact(4).enumerate() {
                    self.vega.legacy_mut().set_dac_entry(
                        (usize::from(start) + offset) as u8,
                        entry[2],
                        entry[1],
                        entry[0],
                    );
                }
                self.set_vbe_status(0x004f);
            }
            0x01 => {
                let mut entries = Vec::with_capacity(usize::from(count) * 4);
                for offset in 0..count {
                    let [r, g, b] = self.vega.legacy_mut().dac_entry((start + offset) as u8);
                    entries.extend_from_slice(&[b, g, r, 0]);
                }
                self.write_guest_linear_block(address, &entries);
                self.set_vbe_status(0x004f);
            }
            _ => self.set_vbe_status(0x014f),
        }
    }

    /// `ES:DI` of the caller's info block, as a guest LINEAR address.
    ///
    /// Linear, not physical: the caller may be a DPMI client whose transfer
    /// buffer was allocated out of upper memory, and a memory manager maps that
    /// region non-identity. Every use of this must go through
    /// `write_guest_linear_block` / `read_guest_linear_block`.
    fn vbe_block_linear(&self) -> u32 {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        es.wrapping_add(u32::from(di))
    }

    fn vbe_mode_info(&mut self) {
        let mode = self.cpu.registers.ecx() as u16 & 0x01ff;
        let Some(info) = vbe_mode(mode) else {
            self.set_vbe_status(0x014f);
            return;
        };
        let pitch = (info.width * bytes_per_pixel(info.bpp)) as u16;
        let mut block = [0u8; 256];
        block[0x00..0x02].copy_from_slice(&0x009bu16.to_le_bytes()); // ModeAttributes: supported, color, graphics, linear-fb
        block[0x02] = 0x07; // WinAAttributes: present, readable, writable
        block[0x04..0x06].copy_from_slice(&64u16.to_le_bytes()); // WinGranularity in KiB
        block[0x06..0x08].copy_from_slice(&64u16.to_le_bytes()); // WinSize in KiB
        block[0x08..0x0a].copy_from_slice(&0xa000u16.to_le_bytes()); // WinASegment
        // WinB is absent. WinFuncPtr remains null because this HLE exposes bank
        // switching through INT 10h 4F05h, not a directly callable ROM thunk.
        block[0x10..0x12].copy_from_slice(&pitch.to_le_bytes()); // BytesPerScanLine
        block[0x12..0x14].copy_from_slice(&(info.width as u16).to_le_bytes()); // XResolution
        block[0x14..0x16].copy_from_slice(&(info.height as u16).to_le_bytes()); // YResolution
        block[0x18] = 1; // NumberOfPlanes
        block[0x19] = info.bpp as u8; // BitsPerPixel
        block[0x1a] = 1; // NumberOfBanks for packed-pixel modes
        // MemoryModel: 04h packed pixel for the indexed modes, 06h direct colour
        // for 15/16/32bpp. The RGB mask fields below are only defined for 06h,
        // so a client that trusts the model reads an 8bpp mode's masks as absent
        // and a hi-colour mode's masks as meaningful. Reporting 04h throughout
        // told a VBE 1.2 client the card had no direct-colour mode at all.
        block[0x1b] = if pixel_format(info.bpp).is_some() {
            6
        } else {
            4
        };
        if let Some(fmt) = pixel_format(info.bpp) {
            block[0x1f] = fmt.r.size as u8; // RedMaskSize
            block[0x20] = fmt.r.pos as u8; // RedFieldPosition
            block[0x21] = fmt.g.size as u8; // GreenMaskSize
            block[0x22] = fmt.g.pos as u8; // GreenFieldPosition
            block[0x23] = fmt.b.size as u8; // BlueMaskSize
            block[0x24] = fmt.b.pos as u8; // BlueFieldPosition
            block[0x25] = fmt.x.size as u8; // RsvdMaskSize
            block[0x26] = fmt.x.pos as u8; // RsvdFieldPosition
        }
        block[0x28..0x2c].copy_from_slice(&MARGO_LFB_BASE.to_le_bytes()); // PhysBasePtr
        let addr = self.vbe_block_linear();
        self.write_guest_linear_block(addr, &block);
        self.set_vbe_status(0x004f);
    }

    #[cfg(test)]
    pub(crate) fn set_margo_mode_640x480x8(&mut self) {
        self.vega.set_margo_mode_640x480x8();
    }

    /// Select Margo's 640x480x8 output and fill it with the built-in diagonal
    /// gradient used by the command-line debug option.
    pub fn load_margo_test_pattern(&mut self) {
        self.vega.load_margo_test_pattern();
    }

    pub fn active_display(&self) -> ActiveDisplay {
        // Every VGA mode (text, planar, mode X, mode 13h) now presents a raster
        // through the core. VEGA also exposes Margo's linear framebuffer and
        // Distira's Voodoo-style front buffer as alternate scanout paths.
        self.vega.active_display()
    }

    /// The active mode of the legacy VGA-compatible scanout path.
    pub fn active_video_mode(&self) -> VideoMode {
        self.vega.active_video_mode()
    }

    pub fn margo_display(&self) -> Option<crate::MargoDisplay> {
        self.vega.margo_active().then(|| self.vega.margo_display())
    }

    /// Monotonic legacy timing sequence used by the host renderer to pace frame
    /// publication without borrowing the VGA implementation.
    pub fn frame_sequence(&self) -> u64 {
        self.vega.frame_sequence()
    }

    /// Emulated vertical refresh of the active display, in Hz. The host uses
    /// this to pace repaints to the guest's frame rate (mode 13h is ~70 Hz,
    /// mode 12h ~60 Hz). Clamped to a sane range so a CRTC reprogram caught
    /// mid-mode-set (a zero or absurd frame size) can't yield a degenerate
    /// repaint interval. Margo's linear framebuffer has no beam model, so it
    /// reports a plain 60 Hz.
    pub fn display_refresh_hz(&self) -> f64 {
        self.vega.display_refresh_hz()
    }

    #[cfg(test)]
    pub(crate) fn vga_raster(&self) -> Option<VgaRaster> {
        self.vega.vga_raster()
    }

    #[cfg(test)]
    pub(crate) fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        self.vega.palette_argb()
    }

    /// The active display as native-resolution `0x00RRGGBB` words plus
    /// `(width, height)`. Legacy VGA keeps the complete beam raster here for
    /// unit-tester CRC compatibility, including rows outside the visible image.
    pub fn frame_argb(&self) -> (Vec<u32>, usize, usize) {
        let start = self.host_profile.start();
        let frame = self.vega.frame_argb();
        self.host_profile
            .record(MachineProfilePhaseKind::VideoConversion, start);
        frame
    }

    /// The most recently completed display frame, cropped exactly as the GUI
    /// presents it and converted to native `0x00RRGGBB` words, or `None` when no
    /// frame has completed yet.
    ///
    /// `None` happens before the run's first raster and for up to one frame
    /// period after every mode set. Pair it with
    /// [`Self::presented_frame_generation`], which has always returned `None` in
    /// the same situations.
    pub fn presented_frame_argb(&self) -> Option<(Vec<u32>, usize, usize)> {
        let start = self.host_profile.start();
        let frame = self.vega.presented_frame_argb();
        self.host_profile
            .record(MachineProfilePhaseKind::VideoConversion, start);
        frame
    }

    pub fn presented_frame_update(&self) -> Option<crate::PresentedFrameUpdate> {
        let start = self.host_profile.start();
        let frame = self.vega.presented_frame_update();
        self.host_profile
            .record(MachineProfilePhaseKind::VideoConversion, start);
        frame
    }

    pub fn video_host_metrics(&self) -> crate::VideoHostMetricsSnapshot {
        self.vega.host_metrics()
    }

    /// Render the current display state immediately for a headless capture.
    /// Legacy VGA output is cropped to its visible rows; accelerated scanouts
    /// use their current front buffers.
    pub fn capture_frame_argb(&mut self) -> (Vec<u32>, usize, usize) {
        let start = self.host_profile.start();
        let frame = self.vega.capture_frame_argb();
        self.host_profile
            .record(MachineProfilePhaseKind::VideoConversion, start);
        frame
    }

    /// An O(1) live content-generation key for graphics mutations.
    ///
    /// Returns `Some(key)` only when the output is a pure function of guest writes —
    /// the active display is the VGA raster AND the mode is a graphics mode (mode 13h,
    /// planar, mode X, CGA graphics). The key changes iff the graphics-mode output
    /// could change, so a consumer that re-renders only when the key moves can never
    /// show a stale frame, while idling on a static screen. It folds every input that
    /// can change the output: the Vga `content_gen` (bumped inside every Vga display
    /// mutator — VRAM writers, register/DAC writes, and the start-address latch — so
    /// it catches writes from BOTH the CPU bus AND the HLE BIOS INT 10h services that
    /// mutate the legacy Adapter directly, regardless of caller), plus the raster dimensions
    /// (so a mode or resolution change always moves the key).
    ///
    /// Returns `None` for text mode (time-based cursor/attribute blink toggles with no
    /// guest write, so writes alone cannot capture it) and Distira. Margo combines
    /// its row-damage generation with the legacy DAC generation. Consumers of
    /// [`Self::presented_frame_argb`] should use
    /// [`Self::presented_frame_generation`] so the key and raster are finalized
    /// together. Pure `&self`: no rendering, no timing side effects.
    pub fn frame_generation(&self) -> Option<u64> {
        self.vega.frame_generation()
    }

    /// Generation paired with the most recently completed graphics raster.
    /// Unlike [`Self::frame_generation`], writes to an in-progress frame do not
    /// move this key until that raster is finalized.
    pub fn presented_frame_generation(&self) -> Option<u64> {
        self.vega.presented_frame_generation()
    }

    /// zlib/IEEE CRC-32 of a framebuffer rectangle, each pixel hashed as its four
    /// `0x00RRGGBB` bytes (little-endian). The rectangle is clamped to the frame;
    /// one fully outside it hashes nothing (CRC of empty input, 0). This is the
    /// value the unit tester returns at `REG_CRC`, and a handy Rust-side check
    /// for the boot suite.
    pub fn screen_crc32(&mut self, x: u16, y: u16, w: u16, h: u16) -> u32 {
        self.vega.screen_crc32(x, y, w, h)
    }

    /// Set where the unit tester's Snapshot command writes PPM frames. `None`
    /// (the default) makes Snapshot a no-op. Each Snapshot overwrites this path.
    // Limit: single path, overwrite. Add an index suffix if a test ever needs
    // to capture multiple frames in one run.
    pub fn set_test_snapshot_path(&mut self, path: Option<std::path::PathBuf>) {
        self.test_snapshot_path = path;
    }

    /// Write the current frame to `path` as a binary PPM (P6). PPM keeps a PNG
    /// encoder out of the dependency tree for a baseline-capture convenience; any
    /// image viewer or `pnmtopng` opens it.
    pub(super) fn write_snapshot_ppm(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let (words, width, height) = self.frame_argb();
        let mut out = Vec::with_capacity(width * height * 3 + 32);
        write!(out, "P6\n{width} {height}\n255\n")?;
        for &word in &words {
            out.push((word >> 16) as u8); // R
            out.push((word >> 8) as u8); // G
            out.push(word as u8); // B
        }
        std::fs::write(path, out)
    }
}
