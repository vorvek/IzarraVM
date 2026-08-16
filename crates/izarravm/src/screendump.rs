// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Periodic headless screen sampling for `--hdd-folder`, off unless
//! `--screen-dump-dir` names a directory.
//!
//! A corpus sweep runs thousands of games nobody watches, and the one question
//! no counter answers is whether the picture ever changed. This samples the
//! presented frame on a guest-clock schedule and writes an index line per
//! sample: the frame hash, the active display and video mode, and the non-blank
//! glyph count in text mode. A run whose hash never moves is parked on a menu
//! or hung, however busy its counters look.
//!
//! Two deliberate choices. It reads `presented_frame_argb`, which borrows the
//! machine immutably and leaves the in-progress raster alone;
//! `capture_frame_argb` re-renders the whole frame at capture-time register
//! state and would corrupt the next presented frame's top rows. And it writes a
//! PPM only when the hash moves, because 60 samples of a 640x480 frame is 55 MB
//! per game and almost all of it is the same picture twice.
//!
//! It slices the run, so a dumping run is a diagnostic and not a benchmark row.

use std::io::Write;
use std::path::{Path, PathBuf};

use izarravm_core::MASTER_CLOCK_HZ;
use izarravm_machine::{ActiveDisplay, Machine, VideoMode};

pub struct ScreenDumper {
    dir: PathBuf,
    /// Guest clocks between samples, the run-slice length while armed.
    pub slice: u64,
    index: std::io::BufWriter<std::fs::File>,
    samples: usize,
    last_hash: u64,
}

impl ScreenDumper {
    /// `slice_clocks` is the guest-clock interval between samples, already
    /// converted by the caller through the persona's own `ClockRate`.
    pub fn new(dir: &Path, slice_clocks: u64) -> std::io::Result<ScreenDumper> {
        std::fs::create_dir_all(dir)?;
        let slice = slice_clocks.max(1_000_000);
        let index = std::fs::File::create(dir.join("screens.jsonl"))?;
        Ok(ScreenDumper {
            dir: dir.to_path_buf(),
            slice,
            index: std::io::BufWriter::new(index),
            samples: 0,
            last_hash: u64::MAX,
        })
    }

    /// Sample after a completed run slice. Errors are reported and swallowed:
    /// a full disk must not take the run down.
    pub fn after_slice(&mut self, machine: &Machine) {
        if let Err(error) = self.sample(machine) {
            eprintln!("screen-dump: {error}");
        }
    }

    fn sample(&mut self, machine: &Machine) -> std::io::Result<()> {
        let (pixels, width, height) = machine.presented_frame_argb();
        let hash = fnv1a(&pixels);
        let display = machine.active_display();
        let mode = (display == ActiveDisplay::VgaRaster).then(|| machine.active_video_mode());
        let glyphs = (mode == Some(VideoMode::Text)).then(|| {
            machine
                .screen_text()
                .as_text()
                .chars()
                .filter(|c| !c.is_whitespace())
                .count()
        });
        let changed = hash != self.last_hash;
        let name = format!("{:04}.ppm", self.samples);
        if changed {
            write_ppm(&self.dir.join(&name), &pixels, width, height)?;
            self.last_hash = hash;
        }
        let master_ticks = machine.master_ticks();
        writeln!(
            self.index,
            "{{\"i\":{},\"master_ticks\":{},\"guest_ms\":{},\"display\":\"{}\",\
             \"video_mode\":{},\"hash\":\"{:016x}\",\"changed\":{},\"ppm\":{},\"text_glyphs\":{}}}",
            self.samples,
            master_ticks,
            master_ticks.saturating_mul(1000) / MASTER_CLOCK_HZ,
            display_name(display),
            match mode {
                Some(mode) => format!("\"{}\"", mode_name(mode)),
                None => "null".to_string(),
            },
            hash,
            changed,
            if changed {
                format!("\"{name}\"")
            } else {
                "null".to_string()
            },
            match glyphs {
                Some(count) => count.to_string(),
                None => "null".to_string(),
            },
        )?;
        self.index.flush()?;
        self.samples += 1;
        Ok(())
    }

    pub fn finish(mut self) {
        let _ = self.index.flush();
    }
}

fn display_name(display: ActiveDisplay) -> &'static str {
    match display {
        ActiveDisplay::VgaRaster => "vga",
        ActiveDisplay::MargoLfb => "margo",
        ActiveDisplay::Distira => "distira",
    }
}

fn mode_name(mode: VideoMode) -> &'static str {
    match mode {
        VideoMode::Text => "text",
        VideoMode::Mode13h => "mode13h",
        VideoMode::Planar => "planar",
        VideoMode::ModeX => "modex",
        VideoMode::Cga => "cga",
        VideoMode::Hercules => "hercules",
    }
}

fn fnv1a(pixels: &[u32]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &pixel in pixels {
        for byte in pixel.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn write_ppm(path: &Path, pixels: &[u32], width: usize, height: usize) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "P6\n{width} {height}\n255\n")?;
    for &color in pixels {
        out.write_all(&[(color >> 16) as u8, (color >> 8) as u8, color as u8])?;
    }
    out.flush()
}
