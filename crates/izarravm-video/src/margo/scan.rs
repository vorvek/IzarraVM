// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Margo's scanout timing: the display clock a VBE mode runs on.
//!
//! Margo is a house part, so this is its datasheet made executable rather than a
//! model of measured silicon. The specification is in
//! `docs/vega/vega-technical-reference.md` section 9; the three facts this
//! module exists to make true are:
//!
//! * Margo scans out at EXACTLY [`MARGO_FRAME_HZ`], in every mode. The pixel
//!   clock is whatever makes the mode's total dots come out at that rate, which
//!   is why `dot_clock_hz` is derived here instead of being a table column.
//! * A guest may poll that rate through Input Status 1 (0x3DA) while a VBE mode
//!   is on screen. Before this module existed, 0x3DA answered from the legacy
//!   VGA CRTC, which a `4F02h` mode set never touches -- so a guest that paced
//!   on 0x3DA and flipped with `4F07h` ran two clocks about 17% apart.
//! * Frame phase zero is the FIRST ACTIVE DISPLAY DOT, which is the instant a
//!   queued `DISP_START` becomes the origin being scanned out. That is not a
//!   detail: it is what makes "the vertical retrace a guest polls for" and "the
//!   frame boundary that latches a page flip" the same clock, one whole
//!   blanking interval apart, in that order, the way real hardware behaves.
//!
//! The geometry helpers themselves are the VGA core's -- literally the same
//! functions, not a parallel implementation -- so the two display owners cannot
//! drift on what "in retrace" means or on when the next edge falls.

use super::MargoDisplay;
use crate::vga::{CrtcTiming, dots_until_status1_bit_change, dots_until_vretrace_start};

/// Margo's frame rate, in Hz. Exact, not nominal: the reference specifies 60.000
/// Hz and the emulator honors it, so the mode table's pixel clocks come out a
/// few tenths of a percent off the VESA DMT figures they are otherwise taken
/// from (640x480 runs 25.200 MHz here against DMT's 25.175 MHz at 59.94 Hz).
///
/// THE SINGLE SOURCE. `Timeline` drives `Margo::advance_frames` -- the
/// `DISP_START` and `4F09h/80h` palette latch -- off this same constant, and the
/// beam this module reports is derived from that same phase accumulator. Two
/// copies of this number is exactly the bug class this module was written to
/// close.
pub const MARGO_FRAME_HZ: u64 = 60;

/// The scanout geometry and pixel clock in force for a Margo display mode.
///
/// Cheap to construct (a `match` and a struct literal) and carries no state, so
/// callers build it per query rather than caching it next to the mode -- there
/// is then no cache to invalidate when a mode set moves the geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MargoScanTiming {
    crtc: CrtcTiming,
}

impl MargoScanTiming {
    /// The timing for a mode's DISPLAYED geometry.
    ///
    /// Keyed on what the monitor sees, not on the frame-store surface: mode
    /// `0x150` stores 320x240 and is line-doubled to the display (section 3), so
    /// it scans out on 640x480 timing and a guest polling retrace in it sees
    /// 640x480's blanking, which is what the monitor is actually doing.
    pub fn for_display(display: MargoDisplay) -> Self {
        Self {
            crtc: crtc_for(display.width, display.height),
        }
    }

    /// Total dots per frame: `htotal * vtotal`. The beam this type's methods take
    /// is a dot position in `0..frame_dots()`, measured from the first active
    /// display dot.
    pub fn frame_dots(&self) -> u64 {
        self.crtc.frame_dots()
    }

    /// The pixel clock this mode runs at, in Hz. Derived, never tabulated: it is
    /// whatever puts `frame_dots()` dots into one [`MARGO_FRAME_HZ`] frame, so
    /// the refresh rate cannot drift away from the frame clock that latches
    /// `DISP_START` no matter what the geometry table says.
    pub fn dot_clock_hz(&self) -> u64 {
        self.frame_dots() * MARGO_FRAME_HZ
    }

    /// Input Status 1 (0x3DA) as Margo drives it.
    ///
    /// Only bits 0 (display inactive) and 3 (vertical retrace) are meaningful.
    /// Bits 1-2 are the CGA light-pen bits, which this part has no pin for, and
    /// bits 4-5 are the VGA's diagnostic DAC-output mux, which reads the legacy
    /// attribute-controller path and has nothing to look at while Margo owns the
    /// screen. All four read 0, which is what a VGA in a color graphics mode
    /// reports for 1-2 anyway.
    ///
    /// The VGA blanking gates (sequencer screen-off, attribute-controller PAS,
    /// CGA video enable) are NOT consulted: a `4F02h` mode set does not run
    /// through them, and honoring a stale one would let a legacy mode's leftover
    /// screen-off bit blank a VBE mode's status register.
    pub fn status1_bits(&self, beam: u64) -> u8 {
        crate::vga::status1_geometry_bits(&self.crtc, beam, false)
    }

    /// Input Status 0 (0x3C2) bit 7, the CRT-interrupt vertical-retrace status,
    /// as Margo drives it. Bit 4 is a wired display-switch strap and stays the
    /// VGA core's to report; this deliberately returns only bit 7 so the caller
    /// has to compose the two rather than silently dropping one.
    pub fn status0_vretrace_bits(&self, beam: u64) -> u8 {
        if self.status1_bits(beam) & 0x08 != 0 {
            0x80
        } else {
            0x00
        }
    }

    /// Dots from `beam` to the next transition of Input Status 1 `bit` to
    /// `target`. The analytic peek; see `dots_until_status1_bit_change`, which
    /// this and the VGA core share.
    pub fn dots_until_status1_bit_change_from(
        &self,
        beam: u64,
        bit: u8,
        target: bool,
    ) -> Option<u64> {
        dots_until_status1_bit_change(&self.crtc, beam, bit, target, || false)
    }

    /// Dots from `beam` to the next vertical-retrace start edge.
    pub fn dots_until_vretrace_start(&self, beam: u64) -> Option<u64> {
        dots_until_vretrace_start(&self.crtc, beam)
    }

    #[cfg(test)]
    pub(crate) fn crtc(&self) -> CrtcTiming {
        self.crtc
    }
}

/// Scanout geometry for a displayed resolution.
///
/// The four listed shapes are the industry timings for those resolutions at
/// 60 Hz -- 640x480 and 640x400 are the IBM VGA's own (and 640x480's row is
/// character-for-character `CrtcTiming::mode_12h`, which is the same monitor
/// signal); 800x600 and 1024x768 are VESA DMT's totals and sync positions. Only
/// the pixel clock differs from those standards, because Margo runs them all at
/// exactly [`MARGO_FRAME_HZ`] (see `dot_clock_hz`).
///
/// The fallback arm exists so a mode added to the table later still scans out
/// somewhere sane instead of dividing by a zero-dot frame: 25% horizontal
/// overhead and 9.4% vertical, which reproduces 640x480's 800x525 exactly, with
/// a two-line sync pulse a quarter of the way into the vertical blanking.
fn crtc_for(width: u32, height: u32) -> CrtcTiming {
    let (htotal, vtotal, vretrace_start, vretrace_end) = match (width, height) {
        // Mode 0x150: 320x240 stored, line-doubled to a 640x480 display.
        (320, 240) | (640, 480) => (800, 525, 490, 492),
        (640, 400) => (800, 449, 412, 414),
        (800, 600) => (1056, 628, 601, 605),
        (1024, 768) => (1344, 806, 771, 777),
        _ => {
            let htotal = (width * 5 / 4).next_multiple_of(8).max(8);
            let vtotal = (height * 35 / 32).max(height + 2);
            let vretrace_start = height + (vtotal - height) / 4;
            (
                htotal,
                vtotal,
                vretrace_start,
                (vretrace_start + 2).min(vtotal),
            )
        }
    };
    let (hdisp_end, vdisp_end) = match (width, height) {
        (320, 240) => (640, 480), // as displayed, see above
        _ => (width, height),
    };
    CrtcTiming {
        htotal_chars: htotal / 8,
        char_width: 8,
        hdisp_end,
        vtotal,
        vdisp_end,
        // Blanking brackets the sync pulse. Nothing in the status path reads
        // these two (bit 0 follows display-enable, bit 3 follows retrace), but
        // leaving them at a legacy mode's values would be a trap for the next
        // reader.
        vblank_start: vdisp_end,
        vblank_end: vtotal,
        vretrace_start,
        vretrace_end,
        // The rest of `CrtcTiming` describes VGA character-cell and
        // attribute-path behavior that a Margo linear mode has no equivalent
        // for. These values are inert here: the only functions this module calls
        // read htotal/vtotal/hdisp_end/vdisp_end/vretrace_*.
        max_scan: 0,
        double_scan: false,
        start_address: 0,
        offset: 0,
        mode_control: 0xE3,
        underline_loc: 0x00,
        line_compare: 0x3FF,
        preset_row_scan: 0,
    }
}
