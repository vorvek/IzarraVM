// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn read_reg_u32(margo: &Margo, offset: usize) -> u32 {
    (0..4)
        .map(|i| u32::from(margo.read_mmio_u8(offset + i)) << (8 * i))
        .fold(0, |a, b| a | b)
}

// Write a 32-bit register through the byte-granular MMIO path.
fn write_reg(margo: &mut Margo, offset: usize, value: u32) {
    for (i, b) in value.to_le_bytes().into_iter().enumerate() {
        margo.write_mmio_u8(offset + i, b);
    }
}

fn setup_fill(margo: &mut Margo) {
    write_reg(margo, REG_DST_BASE, 0);
    write_reg(margo, REG_DST_PITCH, 8);
    write_reg(margo, REG_DEPTH, 1);
    write_reg(margo, REG_DST_XY, (1 << 16) | 1); // y=1, x=1
    write_reg(margo, REG_DIM, (2 << 16) | 2); // h=2, w=2
    write_reg(margo, REG_FG_COLOR, 0x0000_00ab);
    write_reg(margo, REG_ROP, 0xf0);
}

#[path = "margo_blitter_test.rs"]
mod blitter;
#[path = "margo_device_test.rs"]
mod device;

/// Every Margo mode must scan out at EXACTLY `MARGO_FRAME_HZ`. That is the
/// property `dot_clock_hz` is derived to guarantee, and the property the whole
/// 0x3DA fix rests on: the retrace a guest polls and the frame boundary that
/// latches `DISP_START` are the same clock only if the geometry closes at 60.000
/// Hz in every mode, not just the one the table was written around.
#[test]
fn every_margo_mode_scans_out_at_exactly_the_frame_rate() {
    for mode in MARGO_VBE_MODES {
        let mut margo = Margo::default();
        assert!(margo.set_mode(mode.number));
        let scan = MargoScanTiming::for_display(margo.display());
        let frame_dots = scan.frame_dots();
        assert_ne!(
            frame_dots, 0,
            "mode {:#05x} has a zero-dot frame",
            mode.number
        );
        assert_eq!(
            scan.dot_clock_hz(),
            frame_dots * MARGO_FRAME_HZ,
            "mode {:#05x}",
            mode.number
        );
        assert_eq!(
            scan.dot_clock_hz() / frame_dots,
            MARGO_FRAME_HZ,
            "mode {:#05x} does not close at {MARGO_FRAME_HZ} Hz",
            mode.number
        );

        // The active area has to fit inside the frame, or the display-enable and
        // retrace bits describe a monitor signal the mode could not produce.
        let crtc = scan.crtc();
        assert!(crtc.vdisp_end <= crtc.vtotal, "mode {:#05x}", mode.number);
        assert!(
            u64::from(crtc.hdisp_end) <= crtc.htotal_chars as u64 * crtc.char_width as u64,
            "mode {:#05x}",
            mode.number
        );
        assert!(
            crtc.vdisp_end <= crtc.vretrace_start
                && crtc.vretrace_start < crtc.vretrace_end
                && crtc.vretrace_end <= crtc.vtotal,
            "mode {:#05x}: retrace must sit inside the blanking interval",
            mode.number
        );
    }
}

/// Mode 0x150 stores 320x240 but is line-doubled to the display, so it scans out
/// on the same monitor signal as 640x480 -- a guest polling retrace in the OEM
/// POST mode must see 640x480's blanking, not a half-size frame.
#[test]
fn the_line_doubled_oem_mode_scans_out_on_the_640x480_signal() {
    let mut oem = Margo::default();
    assert!(oem.set_mode(0x150));
    let mut vga = Margo::default();
    assert!(vga.set_mode(0x101));
    assert_eq!(
        MargoScanTiming::for_display(oem.display()),
        MargoScanTiming::for_display(vga.display())
    );
}

/// `dots_until_vretrace_start` must always make progress, from anywhere in the
/// frame including from inside the retrace window itself -- that is what makes a
/// pacing loop ("advance to the edge, run a little, repeat") terminate.
#[test]
fn margo_vretrace_edge_distance_always_makes_progress() {
    let mut margo = Margo::default();
    assert!(margo.set_mode(0x101));
    let scan = MargoScanTiming::for_display(margo.display());
    let frame_dots = scan.frame_dots();
    let edge = u64::from(scan.crtc().vretrace_start)
        * (scan.crtc().htotal_chars as u64 * scan.crtc().char_width as u64);

    for beam in [0, 1, edge - 1, edge, edge + 1, frame_dots - 1] {
        let dots = scan
            .dots_until_vretrace_start(beam)
            .expect("640x480 has a usable retrace edge");
        assert!(dots >= 1 && dots <= frame_dots, "beam {beam} -> {dots}");
        assert_eq!(
            (beam + dots) % frame_dots,
            edge,
            "the distance from beam {beam} must land exactly on the retrace edge"
        );
    }
}
