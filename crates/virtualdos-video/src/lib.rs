use thiserror::Error;
use virtualdos_core::VideoCard;

pub const MODE13H_WIDTH: u32 = 320;
pub const MODE13H_HEIGHT: u32 = 200;
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

    pub fn read_port(&self, port: u16) -> Option<u8> {
        match port {
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
            _ => false,
        }
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
        assert_eq!(framebuffer.indexed_pixels.len(), 64_000);
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
        let adapter = PlaceholderVideoAdapter::new(VideoCard::Et4000W32p);
        assert_eq!(adapter.card(), VideoCard::Et4000W32p);
        assert_eq!(adapter.framebuffer().indexed_pixels.len(), 64_000);
    }
}
