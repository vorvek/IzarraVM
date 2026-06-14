use thiserror::Error;
use virtualdos_core::VideoCard;

pub const MODE13H_WIDTH: u32 = 320;
pub const MODE13H_HEIGHT: u32 = 200;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VideoError {
    #[error("framebuffer dimensions must be non-zero")]
    EmptyFramebuffer,
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
    fn placeholder_adapter_tracks_selected_card() {
        let adapter = PlaceholderVideoAdapter::new(VideoCard::Et4000W32p);
        assert_eq!(adapter.card(), VideoCard::Et4000W32p);
        assert_eq!(adapter.framebuffer().indexed_pixels.len(), 64_000);
    }
}
