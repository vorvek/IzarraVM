// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! A count of every distinct video geometry a guest programmed, and how many
//! times it programmed each.
//!
//! WHY THIS EXISTS. A defect that erased the raster once a frame lived in the
//! VGA core while every fixture-scoreboard row stayed green, because no row
//! replayed its CRTC per frame. Geometry alone would not have caught it either.
//! The entry COUNT would: a guest that rewrites its register table each frame
//! reads in the thousands where a guest that sets a mode once reads two or
//! three.
//!
//! WHY THE KEY IS THE CRTC'S OWN NUMBERS AND NOT A PIXEL COUNT.
//! `Vga::raster_height` is `vtotal`, the whole frame including blanking, and
//! double scan is a separate flag. Standard mode 13h is 200 visible lines
//! double scanned, and the aspect-defeating variant some games use is 199 lines
//! single scanned; both present 400 raster lines. A presented height cannot
//! separate them and the CRTC fields can.
//!
//! The map is a `BTreeMap` and not a `HashMap`, because the census gets PINNED.
//! Two runs that record the same geometries in a different order have to
//! produce the same list, or a pin would fail on ordering alone.

use std::collections::BTreeMap;

use crate::VideoMode;

/// Bits per pixel of a mode's framebuffer. Derived from the mode rather than
/// stored beside it, so the two can never disagree.
pub fn bits_per_pixel(mode: VideoMode) -> u8 {
    match mode {
        // Four planes, one bit each.
        VideoMode::Text | VideoMode::Planar => 4,
        VideoMode::Mode13h | VideoMode::ModeX => 8,
        // 320x200x4 colours. Mode 06h's 640x200x2 variant is 1 bpp and shares
        // this mode value, so the geometry in the key is what separates them.
        VideoMode::Cga => 2,
        VideoMode::Hercules => 1,
    }
}

/// One distinct geometry the guest asked the VGA for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModeCensusKey {
    pub mode: VideoMode,
    /// Horizontal display end, in pixels.
    pub hdisp_end: u32,
    /// Vertical display end, in scanlines. The field an aspect-defeating mode
    /// moves, and the reason a presented pixel height is not enough.
    pub vdisp_end: u32,
    /// Vertical total, including blanking.
    pub vtotal: u32,
    pub double_scan: bool,
    /// The line-compare register sits inside the visible area, so the guest is
    /// drawing a split screen. A status panel under a scrolling playfield is
    /// the usual shape.
    pub line_compare_active: bool,
    pub bpp: u8,
}

/// One distinct frame size the guest asked Distira for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DistiraCensusKey {
    pub width: u32,
    pub height: u32,
}

/// Every VGA geometry the guest programmed, against its count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModeCensus {
    entries: BTreeMap<ModeCensusKey, u64>,
}

impl ModeCensus {
    /// Count one programming of this geometry.
    pub fn record(&mut self, key: ModeCensusKey) {
        *self.entries.entry(key).or_default() += 1;
    }

    /// Every geometry and its count, in a stable order.
    pub fn entries(&self) -> impl Iterator<Item = (&ModeCensusKey, &u64)> {
        self.entries.iter()
    }
}

/// Every Distira frame size the guest programmed, against its count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistiraCensus {
    entries: BTreeMap<DistiraCensusKey, u64>,
}

impl DistiraCensus {
    /// Count one programming of this frame size.
    pub fn record(&mut self, key: DistiraCensusKey) {
        *self.entries.entry(key).or_default() += 1;
    }

    /// Every frame size and its count, in a stable order.
    pub fn entries(&self) -> impl Iterator<Item = (&DistiraCensusKey, &u64)> {
        self.entries.iter()
    }
}

#[cfg(test)]
#[path = "mode_census_test.rs"]
mod tests;
