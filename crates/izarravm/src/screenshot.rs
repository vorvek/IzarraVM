// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::error::Error;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::{OffsetDateTime, UtcOffset};

const SCREENSHOTS_DIR: &str = "screenshots";

#[derive(Clone)]
pub(super) struct ScreenshotFrame {
    words: Arc<Vec<u32>>,
    width: usize,
    height: usize,
}

impl ScreenshotFrame {
    pub(super) fn new(words: Arc<Vec<u32>>, width: usize, height: usize) -> Self {
        Self {
            words,
            width,
            height,
        }
    }

    /// `gamma` is the live `monitor_gamma` preference: the saved PNG must show
    /// the same picture the window did, so a screenshot goes through
    /// `display_transform` exactly like the CRT shader's `CrtStyle::Off`
    /// path. `screendump.rs` and every headless `--*-ppm` writer stay raw on
    /// purpose (design section 4.4) and must never call this.
    pub(super) fn save(
        &self,
        directory: &Path,
        gamma: Option<f32>,
    ) -> Result<PathBuf, ScreenshotError> {
        save_png_at(self, directory, local_now(), gamma)
    }
}

#[derive(Debug)]
pub(super) enum ScreenshotError {
    InvalidFrame {
        width: usize,
        height: usize,
        pixels: usize,
    },
    Io(io::Error),
    Encode(png::EncodingError),
}

impl fmt::Display for ScreenshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFrame {
                width,
                height,
                pixels,
            } => write!(
                f,
                "invalid framebuffer: {width}x{height} needs a matching pixel buffer, got {pixels} pixels"
            ),
            Self::Io(err) => err.fmt(f),
            Self::Encode(err) => err.fmt(f),
        }
    }
}

impl Error for ScreenshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidFrame { .. } => None,
            Self::Io(err) => Some(err),
            Self::Encode(err) => Some(err),
        }
    }
}

impl From<io::Error> for ScreenshotError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<png::EncodingError> for ScreenshotError {
    fn from(err: png::EncodingError) -> Self {
        Self::Encode(err)
    }
}

pub(crate) fn screenshots_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(SCREENSHOTS_DIR)
}

fn local_now() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    UtcOffset::current_local_offset().map_or(now, |offset| now.to_offset(offset))
}

fn filename_stem(now: OffsetDateTime) -> String {
    let month = u8::from(now.month());
    format!(
        "IzarraVM_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}-{:03}",
        now.year(),
        month,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}

fn save_png_at(
    frame: &ScreenshotFrame,
    directory: &Path,
    now: OffsetDateTime,
    gamma: Option<f32>,
) -> Result<PathBuf, ScreenshotError> {
    save_png_with_stem(frame, directory, &filename_stem(now), gamma)
}

fn save_png_with_stem(
    frame: &ScreenshotFrame,
    directory: &Path,
    stem: &str,
    gamma: Option<f32>,
) -> Result<PathBuf, ScreenshotError> {
    let expected = frame.width.checked_mul(frame.height);
    let width = u32::try_from(frame.width).ok();
    let height = u32::try_from(frame.height).ok();
    if frame.width == 0
        || frame.height == 0
        || expected != Some(frame.words.len())
        || width.is_none()
        || height.is_none()
    {
        return Err(ScreenshotError::InvalidFrame {
            width: frame.width,
            height: frame.height,
            pixels: frame.words.len(),
        });
    }
    let (width, height) = (width.unwrap(), height.unwrap());

    let mut rgba = Vec::with_capacity(frame.words.len() * 4);
    crate::crt::pack_argb_rows(&frame.words, frame.width, 0..frame.height, &mut rgba);
    // Apply the same present-time correction the window shows, per channel.
    // pack_argb_rows itself stays untouched: it is also the GPU-upload path
    // in crt.rs, and touching its signature would widen this change's blast
    // radius for no reason. Alpha (the 4th byte of each pixel) is always
    // 0xff and is left alone.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        for channel in &mut pixel[..3] {
            *channel = crate::display_transform::display_transform(*channel, gamma);
        }
    }

    std::fs::create_dir_all(directory)?;
    let (path, file) = reserve_destination(directory, stem)?;
    let result = encode_png(file, width, height, &rgba);
    if let Err(err) = result {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(path)
}

fn reserve_destination(directory: &Path, stem: &str) -> io::Result<(PathBuf, File)> {
    for collision in 0..=9_999u32 {
        let name = if collision == 0 {
            format!("{stem}.png")
        } else {
            format!("{stem}_{collision:03}.png")
        };
        let path = directory.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many screenshots have the same timestamp",
    ))
}

fn encode_png(file: File, width: u32, height: u32, rgba: &[u8]) -> Result<(), ScreenshotError> {
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
#[path = "screenshot_test.rs"]
mod tests;
