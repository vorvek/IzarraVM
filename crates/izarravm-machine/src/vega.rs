// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! VEGA video-card ownership and guest-visible routing.

use izarravm_bus::{BusWidth, Memory};
use izarravm_video::{
    CGA_FB_SIZE, DAC_ENTRIES, Distira, HGC_FB_SIZE, MARGO_MMIO_SIZE, MARGO_VRAM_SIZE, Margo,
    MargoDisplay, TextFrame, VGA_MODE13H_BASE, VGA_MONO_TEXT_BASE, VGA_PLANAR_WINDOW_SIZE,
    VGA_TEXT_MEMORY_SIZE, Vga, VgaRaster, VideoMode,
};

use crate::video_params::{
    DISTIRA_PCI_BAR_SIZE, DISTIRA_PCI_DEVICE_ID, DISTIRA_PCI_LFB_OFFSET, DISTIRA_PCI_REVISION,
    DISTIRA_PCI_TEX_OFFSET, DISTIRA_PCI_VENDOR_ID,
};
use crate::{ActiveDisplay, DISTIRA_MMIO_BASE, MARGO_LFB_BASE, MARGO_MMIO_BASE};

#[derive(Debug)]
pub(crate) struct Vega {
    vga: Box<Vga>,
    margo: Margo,
    distira: Distira,
    margo_active: bool,
    margo_linear: bool,
    margo_bank: u16,
    distira_command: u16,
    distira_mem_base: u32,
    distira_init_enable: u32,
}

impl Default for Vega {
    fn default() -> Self {
        Self {
            vga: Box::new(Vga::default()),
            margo: Margo::default(),
            distira: Distira::new(),
            margo_active: false,
            margo_linear: false,
            margo_bank: 0,
            // Izarra has no PCI BIOS yet, so Distira powers on with its fixed
            // BAR decoded. Guest drivers may still rewrite command and BAR0.
            distira_command: 0x0002,
            distira_mem_base: DISTIRA_MMIO_BASE & !(DISTIRA_PCI_BAR_SIZE - 1),
            distira_init_enable: 0,
        }
    }
}

impl Vega {
    pub(crate) fn legacy(&self) -> &Vga {
        &self.vga
    }

    pub(crate) fn legacy_mut(&mut self) -> &mut Vga {
        &mut self.vga
    }

    #[cfg(test)]
    pub(crate) fn margo(&self) -> &Margo {
        &self.margo
    }

    #[cfg(test)]
    pub(crate) fn margo_mut(&mut self) -> &mut Margo {
        &mut self.margo
    }

    pub(crate) fn select_legacy(&mut self) {
        self.margo_active = false;
        self.distira.disable_display();
    }

    pub(crate) fn set_vbe_mode(&mut self, request: u16) -> bool {
        let mode = request & 0x01ff;
        if !self.margo.set_mode(mode) {
            return false;
        }
        self.margo_active = true;
        self.margo_linear = request & 0x4000 != 0;
        self.margo_bank = 0;
        true
    }

    pub(crate) fn current_vbe_mode(&self) -> Option<u16> {
        self.margo_active
            .then(|| self.margo.display().mode | if self.margo_linear { 0x4000 } else { 0 })
    }

    pub(crate) fn vbe_window_control(&mut self, bx: u16, bank: u16) -> Result<u16, u16> {
        if !self.margo_active {
            return Err(0x014f);
        }
        if self.margo_linear {
            return Err(0x034f);
        }
        if bx as u8 != 0 {
            return Err(0x014f);
        }
        match (bx >> 8) as u8 {
            0x00 => {
                self.margo_bank = bank;
                Ok(bank)
            }
            0x01 => Ok(self.margo_bank),
            _ => Err(0x014f),
        }
    }

    pub(crate) fn set_margo_mode_640x480x8(&mut self) {
        self.margo.set_mode_640x480x8();
        self.margo_active = true;
        self.margo_linear = true;
        self.margo_bank = 0;
        self.distira.disable_display();
    }

    pub(crate) fn load_margo_test_pattern(&mut self) {
        self.set_margo_mode_640x480x8();
        let display = self.margo.display();
        let width = display.width as usize;
        let height = display.height as usize;
        let pitch = display.pitch as usize;
        let vram = self.margo.vram_mut();
        for y in 0..height {
            for x in 0..width {
                vram[y * pitch + x] = ((x + y) & 0xff) as u8;
            }
        }
    }

    pub(crate) fn active_display(&self) -> ActiveDisplay {
        if self.distira.display_enabled() {
            ActiveDisplay::Distira
        } else if self.margo_active {
            ActiveDisplay::MargoLfb
        } else {
            ActiveDisplay::VgaRaster
        }
    }

    pub(crate) fn margo_display(&self) -> MargoDisplay {
        self.margo.display()
    }

    pub(crate) fn margo_active(&self) -> bool {
        self.margo_active
    }

    pub(crate) fn active_video_mode(&self) -> VideoMode {
        self.vga.active_mode()
    }

    pub(crate) fn mode13_direct_page_available(&self) -> bool {
        !self.margo_banked_window_at(VGA_MODE13H_BASE) && self.vga.mode13h_direct_page_available()
    }

    pub(crate) fn mode13_direct_page(&mut self, physical_page: u32) -> Option<*mut u8> {
        if !self.mode13_direct_page_available() {
            return None;
        }
        let offset = physical_page.checked_sub(VGA_MODE13H_BASE)? as usize;
        if offset & 0x0fff != 0 {
            return None;
        }
        self.vga.mode13h_direct_page_ptr(offset)
    }

    pub(crate) fn direct_write_token(&self) -> u8 {
        if self.margo_banked_window_at(VGA_MODE13H_BASE) {
            0
        } else {
            self.vga.direct_write_token()
        }
    }

    pub(crate) fn direct_write_page(&mut self, physical_page: u32) -> Option<*mut u8> {
        if self.direct_write_token() == 0 {
            return None;
        }
        let offset = physical_page.checked_sub(VGA_MODE13H_BASE)? as usize;
        if offset & 0x0fff != 0 {
            return None;
        }
        self.vga.direct_write_page_ptr(offset)
    }

    pub(crate) fn note_direct_write(&mut self, address: u32, bytes: usize) {
        let Some(offset) = address.checked_sub(VGA_MODE13H_BASE) else {
            return;
        };
        self.vga.note_direct_write(offset as usize, bytes);
    }

    pub(crate) fn note_direct_write_pages(&mut self, dirty_pages: u16) {
        self.vga.note_direct_write_pages(dirty_pages);
    }

    pub(crate) fn finish_direct_write_batch(&mut self) {
        self.vga.finish_direct_write_batch();
    }

    pub(crate) fn frame_sequence(&self) -> u64 {
        self.vga.frames_completed()
    }

    pub(crate) fn program_display_start(&mut self, start: u32) -> bool {
        self.margo.program_display_start(start)
    }

    pub(crate) fn display_start_pending(&self) -> bool {
        self.margo.display_start_pending()
    }

    pub(crate) fn port_disabled(&self, port: u16) -> bool {
        !self.vga.video_subsystem_enabled() && port != 0x3c3 && (0x3b0..=0x3df).contains(&port)
    }

    pub(crate) fn port_enabled(&self, port: u16) -> bool {
        self.vga.video_subsystem_enabled() || port == 0x3c3
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        self.vga.read_port(port)
    }

    pub(crate) fn read_status_port_lazy(&mut self, port: u16, beam: u64) -> Option<u8> {
        self.vga.read_status_port_lazy(port, beam)
    }

    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.vga.write_port(port, value)
    }

    pub(crate) fn beam_dots(&self) -> u64 {
        self.vga.beam_dots()
    }

    pub(crate) fn dot_clock_hz(&self) -> u64 {
        self.vga.dot_clock_hz()
    }

    pub(crate) fn frame_dots(&self) -> u64 {
        self.vga.frame_dots()
    }

    pub(crate) fn dots_until_vretrace_start(&self) -> Option<u64> {
        self.vga.dots_until_vretrace_start()
    }

    #[cfg(feature = "jit")]
    pub(crate) fn poll_skip_status1_port_active(&self) -> bool {
        self.port_enabled(0x3da)
            && self.vga.color_status1_port_active()
            && !self.vga.is_hercules_personality()
    }

    #[cfg(feature = "jit")]
    pub(crate) fn status1_bits(&self, beam: u64) -> u8 {
        self.vga.status1_bits(beam)
    }

    #[cfg(feature = "jit")]
    pub(crate) fn status1_side_effects(&mut self) {
        self.vga.status1_side_effects();
    }

    #[cfg(feature = "jit")]
    pub(crate) fn dots_until_status1_bit_change_from(
        &self,
        beam: u64,
        bit: u8,
        target: bool,
    ) -> Option<u64> {
        self.vga
            .dots_until_status1_bit_change_from(beam, bit, target)
    }

    pub(crate) fn advance(
        &mut self,
        margo_nanoseconds: u64,
        margo_frames: u64,
        distira_lines: u64,
        vga_dots: u64,
    ) {
        self.margo.advance_busy(margo_nanoseconds);
        self.margo.advance_frames(margo_frames);
        self.distira.advance_frame_phase(distira_lines);
        self.vga.advance(vga_dots);
    }

    pub(crate) fn screen_text(&self) -> TextFrame {
        self.vga.frame()
    }

    pub(crate) fn is_graphics_mode(&self) -> bool {
        match self.active_display() {
            ActiveDisplay::MargoLfb | ActiveDisplay::Distira => true,
            ActiveDisplay::VgaRaster => matches!(
                self.vga.active_mode(),
                VideoMode::Mode13h
                    | VideoMode::Planar
                    | VideoMode::ModeX
                    | VideoMode::Cga
                    | VideoMode::Hercules
            ),
        }
    }

    pub(crate) fn display_refresh_hz(&self) -> f64 {
        let hz = match self.active_display() {
            ActiveDisplay::VgaRaster => match self.vga.frame_dots() {
                0 => 60.0,
                dots => self.vga.dot_clock_hz() as f64 / dots as f64,
            },
            ActiveDisplay::MargoLfb | ActiveDisplay::Distira => 60.0,
        };
        hz.clamp(50.0, 120.0)
    }

    pub(crate) fn vga_raster(&self) -> Option<VgaRaster> {
        self.vga.last_presented().cloned()
    }

    pub(crate) fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        self.vga.palette_argb()
    }

    pub(crate) fn frame_argb(&self) -> (Vec<u32>, usize, usize) {
        match self.active_display() {
            ActiveDisplay::VgaRaster => {
                if let Some((words, width, height, _)) = self.vga.cached_mode13h_presented_argb() {
                    return (words, width, height);
                }
                let palette = self.palette_argb();
                match self.vga_raster() {
                    Some(raster) => {
                        let words = raster
                            .pixels
                            .iter()
                            .map(|&index| palette[usize::from(index)])
                            .collect();
                        (words, raster.width as usize, raster.height as usize)
                    }
                    None => (vec![0], 1, 1),
                }
            }
            ActiveDisplay::MargoLfb => {
                let display = self.margo.display();
                let (width, height) = (display.width as usize, display.height as usize);
                let palette = self.palette_argb();
                (self.margo.scanout_argb(&palette), width, height)
            }
            ActiveDisplay::Distira => {
                let display = self.distira.display();
                let (width, height) = (display.width as usize, display.height as usize);
                (self.distira.scanout_argb(), width, height)
            }
        }
    }

    pub(crate) fn presented_frame_argb(&self) -> (Vec<u32>, usize, usize) {
        if self.active_display() != ActiveDisplay::VgaRaster {
            return self.frame_argb();
        }

        if let Some((mut words, width, _, display_height)) =
            self.vga.cached_mode13h_presented_argb()
        {
            words.truncate(width.saturating_mul(display_height));
            return (words, width, display_height);
        }

        let palette = self.palette_argb();
        let Some(raster) = self.vga.last_presented() else {
            return (vec![0], 1, 1);
        };
        let width = raster.width as usize;
        let height = if raster.display_height == 0 {
            raster.height as usize
        } else {
            raster.display_height as usize
        };
        let visible = &raster.pixels[..width.saturating_mul(height).min(raster.pixels.len())];
        let words = visible
            .iter()
            .map(|&index| palette[usize::from(index)])
            .collect();
        (words, width, height)
    }

    pub(crate) fn capture_frame_argb(&mut self) -> (Vec<u32>, usize, usize) {
        if self.active_display() != ActiveDisplay::VgaRaster {
            return self.frame_argb();
        }

        let raster = self.vga.render_full_frame();
        let width = raster.width as usize;
        let height = raster.display_height.min(raster.height) as usize;
        let palette = self.palette_argb();
        let words = raster.pixels[..width * height]
            .iter()
            .map(|&index| palette[usize::from(index)])
            .collect();
        (words, width, height)
    }

    pub(crate) fn frame_generation(&self) -> Option<u64> {
        if self.active_display() != ActiveDisplay::VgaRaster || self.vga.is_text_mode() {
            return None;
        }
        Some(Self::frame_generation_key(
            self.vga.content_gen(),
            self.vga.raster_width(),
            self.vga.raster_height(),
        ))
    }

    pub(crate) fn presented_frame_generation(&self) -> Option<u64> {
        if self.active_display() != ActiveDisplay::VgaRaster || self.vga.is_text_mode() {
            return None;
        }
        let raster = self.vga.last_presented()?;
        Some(Self::frame_generation_key(
            raster.generation,
            raster.width,
            raster.height,
        ))
    }

    fn frame_generation_key(generation: u64, width: u32, height: u32) -> u64 {
        const K: u64 = 0x9e37_79b9_7f4a_7c15;
        generation
            .wrapping_mul(K)
            .wrapping_add(u64::from(width).wrapping_mul(0x0001_0000_0001))
            .wrapping_add(u64::from(height).wrapping_mul(0x1_0000_0001_0000))
    }

    pub(crate) fn screen_crc32(&mut self, x: u16, y: u16, w: u16, h: u16) -> u32 {
        let (words, frame_w, frame_h) = self.frame_argb();
        let x = usize::from(x);
        let y = usize::from(y);
        let x_end = x.saturating_add(usize::from(w)).min(frame_w);
        let y_end = y.saturating_add(usize::from(h)).min(frame_h);
        let mut bytes = Vec::new();
        for row in y..y_end {
            for col in x..x_end {
                bytes.extend_from_slice(&words[row * frame_w + col].to_le_bytes());
            }
        }
        crate::unittester::crc32(&bytes)
    }

    pub(crate) fn drain_distira_fifo(&mut self) {
        self.distira.drain_fifo();
    }

    pub(crate) fn pump_pusher(&mut self, memory: &Memory) {
        let p = self.margo.pusher();
        if !p.enabled || p.size == 0 {
            return;
        }
        let mut get = p.get;
        let mut budget = (p.size / 4) as u64;
        while self.margo.busy_ns() == 0 && get != p.put && budget > 0 {
            let header = read_ring_word(memory, p.base, p.size, get);
            let method = (header & 0xffff) as usize;
            let count = header >> 16;
            get = (get + 4) % p.size;
            budget -= 1;
            let mut i = 0u32;
            while i < count && get != p.put && budget > 0 {
                let data = read_ring_word(memory, p.base, p.size, get);
                for byte in 0..4 {
                    self.margo.write_mmio_u8(
                        method + (i as usize) * 4 + byte,
                        (data >> (8 * byte)) as u8,
                    );
                }
                get = (get + 4) % p.size;
                budget -= 1;
                i += 1;
            }
            self.margo.set_pusher_get(get);
        }
    }

    pub(crate) fn read_wide_memory(&mut self, address: u32, width: BusWidth) -> Option<u32> {
        let bytes = width.bytes() as usize;
        if let Some(offset) = self.distira_lfb_offset(address, bytes) {
            let offset = if width == BusWidth::Byte {
                offset
            } else {
                offset & !1
            };
            return Some(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => u32::from(self.distira.read_lfb_u16(offset)),
                BusWidth::Dword => self.distira.read_lfb_u32(offset),
            });
        }
        if self.distira_texture_offset(address, bytes).is_some() {
            return Some(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => 0xffff,
                BusWidth::Dword => u32::MAX,
            });
        }
        None
    }

    pub(crate) fn write_wide_memory(&mut self, address: u32, width: BusWidth, value: u32) -> bool {
        let bytes = width.bytes() as usize;
        if let Some(offset) = self.distira_lfb_offset(address, bytes) {
            let offset = if width == BusWidth::Byte {
                offset
            } else {
                offset & !1
            };
            match width {
                BusWidth::Byte => {}
                BusWidth::Word => self.distira.write_lfb_u16(offset, value as u16),
                BusWidth::Dword => self.distira.write_lfb_u32(offset, value),
            }
            return true;
        }
        if let Some(offset) = self.distira_texture_offset(address, bytes) {
            if width == BusWidth::Dword {
                self.distira.write_texture_u32(offset, value);
            }
            return true;
        }
        false
    }

    pub(crate) fn read_memory(&mut self, address: u32, out: &mut [u8]) -> bool {
        let width = out.len();
        if width == 0 {
            return false;
        }

        if self.margo_banked_window_at(address) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = address
                    .checked_add(index as u32)
                    .and_then(|address| self.margo_banked_window_offset(address))
                    .map(|offset| self.margo.read_vram_u8(offset))
                    .unwrap_or(0xff);
            }
            return true;
        }

        if let Some(offset) = self.legacy_gfx_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = match self.vga.active_mode() {
                    VideoMode::Mode13h => self.vga.cpu_read_chain4(offset + index),
                    _ => self.vga.cpu_read(offset + index),
                };
            }
            return true;
        }

        if let Some(offset) = self.hercules_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.vga.hgc_read(offset + index);
            }
            return true;
        }

        if let Some(offset) = self.text_offset(address, width) {
            let cga_window = self.vga.is_cga_personality();
            let cga_graphics = self.vga.active_mode() == VideoMode::Cga;
            for (index, byte) in out.iter_mut().enumerate() {
                let byte_offset = if cga_window {
                    (offset + index) & (CGA_FB_SIZE - 1)
                } else {
                    offset + index
                };
                *byte = if cga_graphics {
                    self.vga.cga_read(byte_offset)
                } else {
                    self.vga.read_u8(byte_offset).unwrap_or(0xff)
                };
            }
            return true;
        }

        if self.vga.video_memory_enabled()
            && let Some(offset) = planar_offset(address, width)
        {
            match self.vga.active_mode() {
                VideoMode::Planar | VideoMode::ModeX => {
                    for (index, byte) in out.iter_mut().enumerate() {
                        *byte = self.vga.cpu_read(offset + index);
                    }
                    return true;
                }
                VideoMode::Mode13h => {
                    for (index, byte) in out.iter_mut().enumerate() {
                        *byte = self.vga.cpu_read_chain4(offset + index);
                    }
                    return true;
                }
                VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => {}
            }
        }

        if let Some(offset) = margo_lfb_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.margo.read_vram_u8(offset + index);
            }
            return true;
        }
        if let Some(offset) = margo_mmio_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.margo.read_mmio_u8(offset + index);
            }
            return true;
        }
        if let Some(offset) = self.distira_lfb_offset(address, width) {
            match width {
                1 => out[0] = 0xff,
                2 => out.copy_from_slice(&self.distira.read_lfb_u16(offset & !1).to_le_bytes()),
                4 => out.copy_from_slice(&self.distira.read_lfb_u32(offset & !1).to_le_bytes()),
                _ => {
                    for (index, byte) in out.iter_mut().enumerate() {
                        *byte = self.distira.read_lfb_u8(offset + index);
                    }
                }
            }
            return true;
        }
        if self.distira_texture_offset(address, width).is_some() {
            out.fill(0xff);
            return true;
        }
        if let Some(offset) = self.distira_mmio_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.distira.read_mmio_u8(offset + index);
            }
            return true;
        }
        false
    }

    pub(crate) fn write_memory_u8(&mut self, address: u32, value: u8) -> bool {
        if self.margo_banked_window_at(address) {
            if let Some(offset) = self.margo_banked_window_offset(address) {
                self.margo.write_vram_u8(offset, value);
            }
            return true;
        }
        if let Some(offset) = self.legacy_gfx_offset(address, 1) {
            match self.vga.active_mode() {
                VideoMode::Mode13h => self.vga.cpu_write_chain4(offset, value),
                _ => self.vga.cpu_write(offset, value),
            }
            return true;
        }
        if let Some(offset) = self.hercules_offset(address, 1) {
            self.vga.hgc_write(offset, value);
            return true;
        }
        if let Some(offset) = self.text_offset(address, 1) {
            let offset = if self.vga.is_cga_personality() {
                offset & (CGA_FB_SIZE - 1)
            } else {
                offset
            };
            if self.vga.active_mode() == VideoMode::Cga {
                self.vga.cga_write(offset, value);
            } else {
                let _ = self.vga.write_u8(offset, value);
            }
            return true;
        }
        if self.vga.video_memory_enabled()
            && let Some(offset) = planar_offset(address, 1)
        {
            match self.vga.active_mode() {
                VideoMode::Planar | VideoMode::ModeX => {
                    self.vga.cpu_write(offset, value);
                    return true;
                }
                VideoMode::Mode13h => {
                    self.vga.cpu_write_chain4(offset, value);
                    return true;
                }
                VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => {}
            }
        }
        if let Some(offset) = margo_lfb_offset(address, 1) {
            self.margo.write_vram_u8(offset, value);
            return true;
        }
        if let Some(offset) = margo_mmio_offset(address, 1) {
            self.margo.write_mmio_u8(offset, value);
            return true;
        }
        if self.distira_lfb_offset(address, 1).is_some()
            || self.distira_texture_offset(address, 1).is_some()
        {
            return true;
        }
        if let Some(offset) = self.distira_mmio_offset(address, 1) {
            self.distira.write_mmio_u8(offset, value);
            return true;
        }
        false
    }

    pub(crate) fn owns_memory(&self, address: u32, width: usize) -> bool {
        self.margo_banked_window_at(address)
            || self.legacy_gfx_offset(address, width).is_some()
            || self.hercules_offset(address, width).is_some()
            || self.text_offset(address, width).is_some()
            || (self.vga.video_memory_enabled() && planar_offset(address, width).is_some())
            || margo_lfb_offset(address, width).is_some()
            || margo_mmio_offset(address, width).is_some()
            || self.distira_lfb_offset(address, width).is_some()
            || self.distira_texture_offset(address, width).is_some()
            || self.distira_mmio_offset(address, width).is_some()
    }

    pub(crate) fn memory_decode_key(&self) -> Option<u32> {
        self.distira_memory_enabled()
            .then_some(self.distira_mem_base)
    }

    pub(crate) fn memory_bar_overlaps(&self, start: usize, end: usize) -> bool {
        if !self.distira_memory_enabled() {
            return false;
        }
        let bar_start = u64::from(self.distira_mem_base);
        let bar_end = bar_start + u64::from(DISTIRA_PCI_BAR_SIZE);
        (start as u64) < bar_end && bar_start < end as u64
    }

    pub(crate) fn pci_read_config_byte(&self, offset: u32) -> u8 {
        match offset {
            0x00 => (DISTIRA_PCI_VENDOR_ID & 0xff) as u8,
            0x01 => (DISTIRA_PCI_VENDOR_ID >> 8) as u8,
            0x02 => (DISTIRA_PCI_DEVICE_ID & 0xff) as u8,
            0x03 => (DISTIRA_PCI_DEVICE_ID >> 8) as u8,
            0x04 => self.distira_command as u8,
            0x05 => (self.distira_command >> 8) as u8,
            0x08 => DISTIRA_PCI_REVISION,
            0x09 => 0x00,
            0x0a => 0x00,
            0x0b => 0x04,
            0x0e => 0x00,
            0x10..=0x13 => ((self.distira_mem_base >> ((offset - 0x10) * 8)) & 0xff) as u8,
            0x40..=0x43 => ((self.distira_init_enable >> ((offset - 0x40) * 8)) & 0xff) as u8,
            _ => 0x00,
        }
    }

    pub(crate) fn pci_write_config_byte(&mut self, offset: u32, value: u8) {
        match offset {
            0x04 => {
                self.distira_command = (self.distira_command & !0x0002) | u16::from(value & 0x02);
            }
            0x10..=0x12 => {}
            0x13 => self.distira_mem_base = u32::from(value) << 24,
            0x40..=0x43 => {
                let shift = (offset - 0x40) * 8;
                self.distira_init_enable =
                    (self.distira_init_enable & !(0xff << shift)) | (u32::from(value) << shift);
            }
            _ => {}
        }
        self.distira.set_init_enable(self.distira_init_enable);
    }

    fn distira_memory_enabled(&self) -> bool {
        self.distira_command & 0x0002 != 0 && self.distira_mem_base != 0
    }

    pub(crate) fn distira_bar_offset(&self, address: u32, width: usize) -> Option<u32> {
        if !self.distira_memory_enabled() {
            return None;
        }
        let offset = address.checked_sub(self.distira_mem_base)?;
        let end = offset.checked_add(width as u32)?;
        (end <= DISTIRA_PCI_BAR_SIZE).then_some(offset)
    }

    pub(crate) fn distira_mmio_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        (offset < DISTIRA_PCI_LFB_OFFSET && offset + width as u32 <= DISTIRA_PCI_LFB_OFFSET)
            .then_some(offset as usize)
    }

    pub(crate) fn distira_lfb_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if (DISTIRA_PCI_LFB_OFFSET..DISTIRA_PCI_TEX_OFFSET).contains(&offset)
            && offset + width as u32 <= DISTIRA_PCI_TEX_OFFSET
        {
            Some((offset - DISTIRA_PCI_LFB_OFFSET) as usize)
        } else {
            None
        }
    }

    pub(crate) fn distira_texture_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if offset >= DISTIRA_PCI_TEX_OFFSET && offset + width as u32 <= DISTIRA_PCI_BAR_SIZE {
            Some((offset - DISTIRA_PCI_TEX_OFFSET) as usize)
        } else {
            None
        }
    }

    fn legacy_gfx_offset(&self, address: u32, width: usize) -> Option<usize> {
        if !self.vga.video_memory_enabled() {
            return None;
        }
        match self.vga.active_mode() {
            VideoMode::Planar | VideoMode::ModeX | VideoMode::Mode13h => {
                let aperture = self.vga.gfx_aperture();
                legacy_gfx_aperture_offset(aperture.base, aperture.length, address, width)
            }
            VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => None,
        }
    }

    fn text_offset(&self, address: u32, width: usize) -> Option<usize> {
        if self.vga.is_hercules_personality() || !self.vga.video_memory_enabled() {
            return None;
        }
        text_offset(self.vga.text_memory_base(), address, width)
    }

    fn hercules_offset(&self, address: u32, width: usize) -> Option<usize> {
        if !self.vga.video_memory_enabled() || !self.vga.is_hercules_personality() {
            return None;
        }
        let end = VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32 * 2;
        if !(VGA_MONO_TEXT_BASE..end).contains(&address) || address + width as u32 > end {
            return None;
        }
        let offset = (address - VGA_MONO_TEXT_BASE) as usize;
        if offset >= HGC_FB_SIZE && !self.vga.hgc_page1_addressable() {
            return None;
        }
        Some(offset)
    }

    fn margo_banked_window_at(&self, address: u32) -> bool {
        self.margo_active
            && !self.margo_linear
            && (VGA_MODE13H_BASE..VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE).contains(&address)
    }

    fn margo_banked_window_offset(&self, address: u32) -> Option<usize> {
        if !self.margo_banked_window_at(address) {
            return None;
        }
        let bank = usize::from(self.margo_bank) * VGA_PLANAR_WINDOW_SIZE as usize;
        let offset = bank + (address - VGA_MODE13H_BASE) as usize;
        (offset < MARGO_VRAM_SIZE).then_some(offset)
    }
}

fn read_ring_word(memory: &Memory, base: u32, size: u32, off: u32) -> u32 {
    let start = off as usize % size as usize;
    if start + 4 <= size as usize
        && let Some(slice) = memory
            .as_slice()
            .get(base as usize + start..base as usize + start + 4)
    {
        return u32::from_le_bytes(slice.try_into().unwrap());
    }
    let mut bytes = [0u8; 4];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let ring_offset = (off as usize + index) % size as usize;
        *byte = memory
            .as_slice()
            .get(base as usize + ring_offset)
            .copied()
            .unwrap_or(0);
    }
    u32::from_le_bytes(bytes)
}

fn text_offset(base: u32, address: u32, width: usize) -> Option<usize> {
    let end = base + VGA_TEXT_MEMORY_SIZE as u32;
    if (base..end).contains(&address) && address + width as u32 <= end {
        Some((address - base) as usize)
    } else {
        None
    }
}

fn planar_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE;
    if (VGA_MODE13H_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - VGA_MODE13H_BASE) as usize)
    } else {
        None
    }
}

fn legacy_gfx_aperture_offset(base: u32, length: u32, address: u32, width: usize) -> Option<usize> {
    let end = base + length;
    if !(base..end).contains(&address) || address + width as u32 > end {
        return None;
    }
    let offset = ((address - base) % VGA_PLANAR_WINDOW_SIZE) as usize;
    (offset + width <= VGA_PLANAR_WINDOW_SIZE as usize).then_some(offset)
}

fn margo_lfb_offset(address: u32, width: usize) -> Option<usize> {
    let end = MARGO_LFB_BASE + MARGO_VRAM_SIZE as u32;
    if (MARGO_LFB_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - MARGO_LFB_BASE) as usize)
    } else {
        None
    }
}

fn margo_mmio_offset(address: u32, width: usize) -> Option<usize> {
    let end = MARGO_MMIO_BASE + MARGO_MMIO_SIZE as u32;
    if (MARGO_MMIO_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - MARGO_MMIO_BASE) as usize)
    } else {
        None
    }
}
