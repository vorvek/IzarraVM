use izarravm_core::VideoCard;
use thiserror::Error;

pub mod margo;

pub use margo::{
    MARGO_ID_VALUE, MARGO_MMIO_SIZE, MARGO_VBE_MODES, MARGO_VRAM_SIZE, Margo, MargoDisplay,
    VbeMode, vbe_mode,
};

pub const MODE13H_WIDTH: u32 = 320;
pub const MODE13H_HEIGHT: u32 = 200;
pub const MODE13H_MEMORY_SIZE: usize = 64_000;
pub const VGA_MODE13H_BASE: u32 = 0x000a_0000;
pub const VGA_TEXT_BASE: u32 = 0x000b_8000;
pub const VGA_TEXT_COLUMNS: usize = 80;
pub const VGA_TEXT_ROWS: usize = 25;
pub const VGA_TEXT_CELL_BYTES: usize = 2;
pub const VGA_TEXT_MEMORY_SIZE: usize = VGA_TEXT_COLUMNS * VGA_TEXT_ROWS * VGA_TEXT_CELL_BYTES;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VideoError {
    #[error("framebuffer dimensions must be non-zero")]
    EmptyFramebuffer,
    #[error("VGA text memory offset {offset:#x} is outside the text buffer")]
    TextMemoryOutOfBounds { offset: usize },
    #[error("VGA Mode 13h offset {offset:#x} is outside the framebuffer")]
    Mode13hOutOfBounds { offset: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub indexed_pixels: Vec<u8>,
}

impl Framebuffer {
    pub fn new_indexed8(width: u32, height: u32) -> Result<Self, VideoError> {
        if width == 0 || height == 0 {
            return Err(VideoError::EmptyFramebuffer);
        }

        Ok(Self {
            width,
            height,
            indexed_pixels: vec![0; (width * height) as usize],
        })
    }

    pub fn mode13h() -> Self {
        Self::new_indexed8(MODE13H_WIDTH, MODE13H_HEIGHT).expect("Mode 13h dimensions are non-zero")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoMode {
    #[default]
    Text,
    Mode13h,
}

pub const DAC_ENTRIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dac {
    palette: [[u8; 3]; DAC_ENTRIES], // 6-bit components, 0..=63
    write_index: u8,
    read_index: u8,
    write_component: u8, // 0,1,2 -> R,G,B
    read_component: u8,
}

impl Default for Dac {
    fn default() -> Self {
        // Provisional grayscale ramp so any index renders visibly. Loading the
        // exact stock VGA default palette is a follow-up slice.
        let mut palette = [[0u8; 3]; DAC_ENTRIES];
        for (index, entry) in palette.iter_mut().enumerate() {
            let level = (index >> 2) as u8; // 0..=63
            *entry = [level, level, level];
        }
        Self {
            palette,
            write_index: 0,
            read_index: 0,
            write_component: 0,
            read_component: 0,
        }
    }
}

impl Dac {
    pub fn set_write_index(&mut self, index: u8) {
        self.write_index = index;
        self.write_component = 0;
    }

    pub fn set_read_index(&mut self, index: u8) {
        self.read_index = index;
        self.read_component = 0;
    }

    pub fn write_data(&mut self, value: u8) {
        self.palette[self.write_index as usize][self.write_component as usize] = value & 0x3f;
        self.write_component += 1;
        if self.write_component == 3 {
            self.write_component = 0;
            self.write_index = self.write_index.wrapping_add(1);
        }
    }

    pub fn read_data(&mut self) -> u8 {
        let value = self.palette[self.read_index as usize][self.read_component as usize];
        self.read_component += 1;
        if self.read_component == 3 {
            self.read_component = 0;
            self.read_index = self.read_index.wrapping_add(1);
        }
        value
    }

    pub fn write_index(&self) -> u8 {
        self.write_index
    }

    pub fn rgb888(&self, index: u8) -> (u8, u8, u8) {
        let [r, g, b] = self.palette[index as usize];
        (expand6(r), expand6(g), expand6(b))
    }
}

fn expand6(component: u8) -> u8 {
    // 6-bit (0..=63) to 8-bit, replicating the high bits into the low.
    (component << 2) | (component >> 4)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCell {
    pub character: u8,
    pub attribute: u8,
}

impl Default for TextCell {
    fn default() -> Self {
        Self {
            character: b' ',
            attribute: 0x07,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFrame {
    pub columns: usize,
    pub rows: usize,
    pub cells: Vec<TextCell>,
    pub cursor_offset: u16,
}

impl TextFrame {
    pub fn line_string(&self, row: usize) -> String {
        let start = row * self.columns;
        let end = start + self.columns;
        self.cells[start..end]
            .iter()
            .map(|cell| match cell.character {
                0 => ' ',
                byte => char::from(byte),
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    pub fn as_text(&self) -> String {
        (0..self.rows)
            .map(|row| self.line_string(row))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgaTextMode {
    memory: [u8; VGA_TEXT_MEMORY_SIZE],
    mode13h: Framebuffer,
    mode: VideoMode,
    dac: Dac,
    crtc_index: u8,
    cursor_offset: u16,
}

impl Default for VgaTextMode {
    fn default() -> Self {
        let mut memory = [0; VGA_TEXT_MEMORY_SIZE];
        for cell in memory.chunks_exact_mut(2) {
            cell[0] = b' ';
            cell[1] = 0x07;
        }

        Self {
            memory,
            mode13h: Framebuffer::mode13h(),
            mode: VideoMode::Text,
            dac: Dac::default(),
            crtc_index: 0,
            cursor_offset: 0,
        }
    }
}

impl VgaTextMode {
    pub fn read_u8(&self, offset: usize) -> Result<u8, VideoError> {
        self.memory
            .get(offset)
            .copied()
            .ok_or(VideoError::TextMemoryOutOfBounds { offset })
    }

    pub fn write_u8(&mut self, offset: usize, value: u8) -> Result<(), VideoError> {
        let slot = self
            .memory
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

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x03c8 => Some(self.dac.write_index()),
            0x03c9 => Some(self.dac.read_data()),
            0x03d4 => Some(self.crtc_index),
            0x03d5 => match self.crtc_index {
                0x0e => Some((self.cursor_offset >> 8) as u8),
                0x0f => Some(self.cursor_offset as u8),
                _ => Some(0),
            },
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x03d4 => {
                self.crtc_index = value;
                true
            }
            0x03d5 => {
                match self.crtc_index {
                    0x0e => {
                        self.cursor_offset =
                            (self.cursor_offset & 0x00ff) | (u16::from(value) << 8);
                    }
                    0x0f => {
                        self.cursor_offset = (self.cursor_offset & 0xff00) | u16::from(value);
                    }
                    _ => {}
                }
                true
            }
            0x03c7 => {
                self.dac.set_read_index(value);
                true
            }
            0x03c8 => {
                self.dac.set_write_index(value);
                true
            }
            0x03c9 => {
                self.dac.write_data(value);
                true
            }
            _ => false,
        }
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
            .memory
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

pub trait VideoAdapter {
    fn card(&self) -> VideoCard;
    fn framebuffer(&self) -> &Framebuffer;
    fn set_mode13h(&mut self);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderVideoAdapter {
    card: VideoCard,
    framebuffer: Framebuffer,
}

impl PlaceholderVideoAdapter {
    pub fn new(card: VideoCard) -> Self {
        Self {
            card,
            framebuffer: Framebuffer::mode13h(),
        }
    }
}

impl VideoAdapter for PlaceholderVideoAdapter {
    fn card(&self) -> VideoCard {
        self.card
    }

    fn framebuffer(&self) -> &Framebuffer {
        &self.framebuffer
    }

    fn set_mode13h(&mut self) {
        self.framebuffer = Framebuffer::mode13h();
    }
}

pub fn preferred_wgpu_backends() -> wgpu::Backends {
    wgpu::Backends::all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode13h_framebuffer_has_expected_size() {
        let framebuffer = Framebuffer::mode13h();
        assert_eq!(framebuffer.width, 320);
        assert_eq!(framebuffer.height, 200);
        assert_eq!(framebuffer.indexed_pixels.len(), MODE13H_MEMORY_SIZE);
    }

    #[test]
    fn text_mode_defaults_to_blank_80x25_screen() {
        let text = VgaTextMode::default();
        let frame = text.frame();

        assert_eq!(frame.columns, 80);
        assert_eq!(frame.rows, 25);
        assert_eq!(frame.cells.len(), 2000);
        assert!(frame.line_string(0).is_empty());
    }

    #[test]
    fn text_memory_write_updates_frame_cell() {
        let mut text = VgaTextMode::default();
        text.write_u8(0, b'V').unwrap();
        text.write_u8(1, 0x0a).unwrap();

        let frame = text.frame();
        assert_eq!(frame.cells[0].character, b'V');
        assert_eq!(frame.cells[0].attribute, 0x0a);
        assert_eq!(frame.line_string(0), "V");
    }

    #[test]
    fn mode13h_memory_write_updates_framebuffer() {
        let mut video = VgaTextMode::default();
        video.set_mode13h();
        video.write_mode13h_u8(123, 0x2a).unwrap();

        assert_eq!(video.mode13h_framebuffer().indexed_pixels[123], 0x2a);
        assert_eq!(video.read_mode13h_u8(123).unwrap(), 0x2a);
    }

    #[test]
    fn crtc_cursor_ports_track_offset() {
        let mut text = VgaTextMode::default();
        assert!(text.write_port(0x03d4, 0x0e));
        assert!(text.write_port(0x03d5, 0x12));
        assert!(text.write_port(0x03d4, 0x0f));
        assert!(text.write_port(0x03d5, 0x34));

        assert_eq!(text.cursor_offset, 0x1234);
        assert_eq!(text.read_port(0x03d5), Some(0x34));
    }

    #[test]
    fn placeholder_adapter_tracks_selected_card() {
        let adapter = PlaceholderVideoAdapter::new(VideoCard::Et4000Ax);
        assert_eq!(adapter.card(), VideoCard::Et4000Ax);
        assert_eq!(adapter.framebuffer().indexed_pixels.len(), 64_000);
    }

    #[test]
    fn set_mode13h_switches_active_mode() {
        let mut video = VgaTextMode::default();
        assert_eq!(video.active_mode(), VideoMode::Text);
        video.set_mode13h();
        assert_eq!(video.active_mode(), VideoMode::Mode13h);
    }

    #[test]
    fn dac_write_then_read_round_trips() {
        let mut video = VgaTextMode::default();
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
        let mut video = VgaTextMode::default();
        video.write_port(0x03c8, 1);
        video.write_port(0x03c9, 63); // R
        video.write_port(0x03c9, 0); // G
        video.write_port(0x03c9, 0); // B
        assert_eq!(video.palette_argb()[1], 0x00FF_0000);
    }
}
