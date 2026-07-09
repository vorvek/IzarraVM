// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn mono_text_mode_uses_b000_9x14_720x350() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();

    assert_eq!(vga.active_mode(), VideoMode::Text);
    assert_eq!(vga.text_memory_base(), VGA_MONO_TEXT_BASE);
    assert_eq!(vga.raster_width(), 720);
    assert_eq!(vga.raster_height(), 449);
    assert_eq!(vga.crtc.vdisp_end, 350);
    assert_eq!(vga.crtc.max_scan, 13);
    assert_eq!(vga.misc_output & 0xCD, 0x84);
    assert_eq!(vga.cursor_start, 0x0C);
    assert_eq!(vga.cursor_end, 0x0D);

    text_put(&mut vga, 0, 0, 0xDB, 0x0F);
    assert_eq!(vga.render_text_row(0)[0], 0x0F);
}

// -- Hercules Graphics Card (HGC) personality --
//
// Real Hercules software always sets BIOS mode 07h first (MDA-compatible
// 80x25 mono text), then bangs ports 3B8h/3BFh directly to switch to
// 720x348 graphics: there was never an INT 10h mode number for it.

#[test]
fn hgc_config_switch_gates_the_graphics_bit() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();

    // 3B8h GRPH is refused: 3BFh has not allowed it yet, so the card stays
    // in text mode.
    assert!(vga.write_port(0x3B8, 0x02));
    assert_eq!(vga.active_mode(), VideoMode::Text);
    assert_eq!(vga.hgc_mode_control(), 0x02); // the latch still stores it

    // Unlock graphics through the config switch, then re-issue the mode
    // control write: now it takes effect.
    assert!(vga.write_port(0x3BF, 0x01));
    assert!(vga.write_port(0x3B8, 0x0A)); // GRPH + video enable
    assert_eq!(vga.active_mode(), VideoMode::Hercules);
    assert_eq!(vga.raster_width(), 720);
    assert_eq!(vga.raster_height(), 370);
    assert_eq!(vga.crtc.vdisp_end, 348);

    // Dropping GRPH falls back to mono text.
    assert!(vga.write_port(0x3B8, 0x08));
    assert_eq!(vga.active_mode(), VideoMode::Text);
}

#[test]
fn hgc_page1_only_addressable_once_config_switch_enables_it() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01); // allow graphics, page 1 still not enabled
    vga.write_port(
        0x3B8,
        HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE | HGC_MODE_PAGE1,
    );
    assert_eq!(vga.active_mode(), VideoMode::Hercules);
    // Mode Control asked for page 1, but 3BFh never enabled it: scanout
    // stays on page 0.
    vga.hgc_write(0, 0xFF); // page 0, byte 0
    vga.hgc_write(HGC_FB_SIZE, 0xAA); // page 1, byte 0 (still writable/readable as RAM)
    assert_eq!(vga.render_hgc_row(0)[0], 1); // page 0's bit shows

    vga.write_port(0x3BF, 0x03); // allow graphics + enable page 1
    vga.write_port(
        0x3B8,
        HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE | HGC_MODE_PAGE1,
    );
    assert_eq!(vga.render_hgc_row(0)[0], 1); // page 1's 0xAA -> bit 7 set
    assert_eq!(vga.render_hgc_row(0)[1], 0); // 0xAA bit 6 clear
}

#[test]
fn hgc_mode_control_page_select_flips_scanout_page() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x03); // allow graphics + enable page 1
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE); // page 0

    vga.hgc_write(0, 0b1000_0000); // page 0 scanline 0, first pixel lit
    vga.hgc_write(HGC_FB_SIZE, 0b0100_0000); // page 1 scanline 0, second pixel lit
    assert_eq!(vga.render_hgc_row(0)[0], 1);
    assert_eq!(vga.render_hgc_row(0)[1], 0);

    vga.write_port(
        0x3B8,
        HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE | HGC_MODE_PAGE1,
    );
    assert_eq!(vga.render_hgc_row(0)[0], 0);
    assert_eq!(vga.render_hgc_row(0)[1], 1);
}

#[test]
fn hgc_four_bank_interleave_maps_scanlines_to_the_right_offsets() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01);
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE);

    // Scanlines 0,1,2,3 map to banks 0x0000,0x2000,0x4000,0x6000; scanline
    // 4 wraps back to bank 0 at the next row (byte HGC_BYTES_PER_LINE in).
    vga.hgc_write(0, 0x80); // bank 0, row 0, byte 0 -> scanline 0
    vga.hgc_write(HGC_BANK_SIZE, 0x40); // bank 1, row 0, byte 0 -> scanline 1
    vga.hgc_write(HGC_BANK_SIZE * 2, 0x20); // bank 2, row 0, byte 0 -> scanline 2
    vga.hgc_write(HGC_BANK_SIZE * 3, 0x10); // bank 3, row 0, byte 0 -> scanline 3
    vga.hgc_write(HGC_BYTES_PER_LINE, 0x08); // bank 0, row 1, byte 0 -> scanline 4

    assert_eq!(vga.render_hgc_row(0)[0], 1);
    assert_eq!(vga.render_hgc_row(0)[1], 0);
    assert_eq!(vga.render_hgc_row(1)[1], 1);
    assert_eq!(vga.render_hgc_row(2)[2], 1);
    assert_eq!(vga.render_hgc_row(3)[3], 1);
    assert_eq!(vga.render_hgc_row(4)[4], 1);
}

#[test]
fn hgc_720x348_raster_geometry() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01);
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE);

    assert_eq!(vga.raster_width(), 720);
    let raster = vga.render_full_frame();
    assert_eq!(raster.width, 720);
    assert_eq!(raster.height, 370);
    assert_eq!(raster.display_height, 348);
}

#[test]
fn hgc_video_disable_blanks_the_scanout() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01);
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE);
    vga.hgc_write(0, 0xFF);
    assert_eq!(vga.render_hgc_row(0)[0], 1);

    vga.write_port(0x3B8, HGC_MODE_GRAPHICS); // clear video enable
    assert_eq!(vga.render_hgc_row(0)[0], 0);
}

#[test]
fn hgc_phosphor_palette_installs_green_on_black() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01);
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE);

    assert_eq!(vga.dac_entry(0), [0x00, 0x00, 0x00]);
    assert_eq!(vga.dac_entry(1), [0x08, 0x2A, 0x0C]);
}

#[test]
fn hgc_status_port_bit7_is_the_classic_detection_poll_target() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01);
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE);

    // The classic HGC-detection idiom polls 3BAh bit 7 in a tight loop
    // (up to ~0x8000 iterations) and declares the card present once the
    // bit is observed to change. Confirm both states are reachable and
    // that polling from outside vsync eventually observes the toggle.
    assert!(vga.dots_until_vretrace_start().is_some());

    vga.advance(0); // beam at dot 0: not in vretrace
    let initial = vga.read_port(0x3BA).unwrap() & 0x80;
    assert_eq!(initial, 0x80); // bit 7 high outside vsync

    // Jump the beam into the vertical retrace window.
    let into_vretrace = u64::from(vga.crtc.vretrace_start) * htotal_dots(&vga.crtc);
    vga.advance(into_vretrace);
    let during = vga.read_port(0x3BA).unwrap() & 0x80;
    assert_eq!(during, 0x00); // bit 7 low during vsync: detection sees the edge
    assert_ne!(initial, during);
}

#[test]
fn hgc_status_port_bit0_tracks_horizontal_retrace() {
    let mut vga = Vga::default();
    vga.set_mono_text_mode();
    vga.write_port(0x3BF, 0x01);
    vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE);

    vga.advance(0);
    assert_eq!(vga.read_port(0x3BA).unwrap() & 0x01, 0x00); // active display: no hsync

    let past_active = u64::from(vga.crtc.hdisp_end) + 4;
    vga.advance(past_active);
    assert_eq!(vga.read_port(0x3BA).unwrap() & 0x01, 0x01); // past hdisp_end: hsync
}

#[test]
fn hgc_ports_decode_regardless_of_color_emulation_bit() {
    let mut vga = Vga::default();
    // Force color emulation on (as if a color card were also present);
    // the Hercules-specific 3B8/3BF addresses must still decode, unlike
    // the shared 3B4/3B5/3BA aliasing pair.
    vga.write_port(0x3C2, vga.misc_output | 0x01);
    assert!(vga.write_port(0x3BF, 0x01));
    assert!(vga.write_port(0x3B8, HGC_MODE_GRAPHICS | HGC_MODE_VIDEO_ENABLE));
    assert_eq!(vga.active_mode(), VideoMode::Hercules);
}
