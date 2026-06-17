use izarravm_core::VideoCard;
use thiserror::Error;

pub mod margo;
pub mod vga;
pub use vga::{Vga, VGA_PLANAR_SIZE};

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
    Planar,
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
    fn placeholder_adapter_tracks_selected_card() {
        let adapter = PlaceholderVideoAdapter::new(VideoCard::Et4000Ax);
        assert_eq!(adapter.card(), VideoCard::Et4000Ax);
        assert_eq!(adapter.framebuffer().indexed_pixels.len(), 64_000);
    }
}
