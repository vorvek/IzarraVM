// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const BIOS_TEXT_WHITE: u8 = 0x3F;

#[test]
fn cga_320x200_decodes_a_byte_msb_first() {
    // Mode 04h, default color select (palette 0, low intensity): foreground
    // colors are green(2)/red(4)/brown(6), background is 0.
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    // 0b00_01_10_11: px0 = 0 (bg), px1 = 1 (green), px2 = 2 (red), px3 = 3 (brown).
    let decoded = vga.cga.decode_byte_320x200(0b00_01_10_11);
    assert_eq!(decoded, [CGA_BLACK, CGA_GREEN, CGA_RED, CGA_BROWN]);
    // 0b11_10_01_00: the reverse order.
    let decoded = vga.cga.decode_byte_320x200(0b11_10_01_00);
    assert_eq!(decoded, [CGA_BROWN, CGA_RED, CGA_GREEN, CGA_BLACK]);
}

#[test]
fn cga_color_select_picks_the_palette() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    // Palette 1 (bit 5), low intensity: cyan(3)/magenta(5)/light gray(7).
    vga.write_port(0x3D9, 0x20);
    assert_eq!(
        vga.cga.decode_byte_320x200(0b00_01_10_11),
        [CGA_BLACK, CGA_CYAN, CGA_MAGENTA, CGA_LIGHT_GRAY]
    );
    // Palette 1 with intensity (bit 4 + bit 5): light cyan/light magenta/white.
    vga.write_port(0x3D9, 0x30);
    assert_eq!(
        vga.cga.decode_byte_320x200(0b00_01_10_11),
        [CGA_BLACK, CGA_LIGHT_CYAN, CGA_LIGHT_MAGENTA, CGA_WHITE]
    );
    // Palette 0 with intensity (bit 4 only): light green/light red/yellow.
    vga.write_port(0x3D9, 0x10);
    assert_eq!(
        vga.cga.decode_byte_320x200(0b00_01_10_11),
        [CGA_BLACK, CGA_LIGHT_GREEN, CGA_LIGHT_RED, CGA_YELLOW]
    );
    // The background nibble (bits 0-3) sets pixel value 0.
    vga.write_port(0x3D9, 0x01); // background = blue(1)
    assert_eq!(vga.cga.decode_byte_320x200(0b00_00_00_00)[0], 1);
    let raster = vga.render_full_frame();
    let border = (raster.height as usize - 1) * raster.width as usize;
    assert_eq!(raster.pixels[border], 1);
}

#[test]
fn cga_mode_05h_forces_the_alternate_palette() {
    // Mode 05h ignores the palette-select bit and uses cyan/red/white. With
    // intensity off the canonical IBM/DOSBox set is cyan(3)/red(4)/light
    // gray(7).
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x05));
    vga.write_port(0x3D9, 0x20); // palette-select bit is ignored in mode 05h
    assert_eq!(
        vga.cga.decode_byte_320x200(0b00_01_10_11),
        [CGA_BLACK, CGA_CYAN, CGA_RED, CGA_LIGHT_GRAY]
    );
    // With intensity (bit 4): light cyan/light red/white.
    vga.write_port(0x3D9, 0x10);
    assert_eq!(
        vga.cga.decode_byte_320x200(0b00_01_10_11),
        [CGA_BLACK, CGA_LIGHT_CYAN, CGA_LIGHT_RED, CGA_WHITE]
    );
}

#[test]
fn cga_interleave_addresses_odd_lines_in_the_high_bank() {
    // The even/odd interleave: scanline 0 reads framebuffer offset 0x0000,
    // scanline 1 reads offset 0x2000, scanline 2 reads 0x0050 (80 bytes), and
    // scanline 3 reads 0x2050.
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    // Place a distinctive byte at the start of each bank's first two rows.
    vga.cga_write(0x0000, 0b01_01_01_01); // even bank, row 0: value 1 -> green
    vga.cga_write(0x2000, 0b10_10_10_10); // odd bank, row 0: value 2 -> red
    vga.cga_write(0x0050, 0b11_11_11_11); // even bank, row 1 (line 2)
    vga.cga_write(0x2050, 0b00_01_10_11); // odd bank, row 1 (line 3)
    // Scanline 1 (odd) must read from 0x2000: every pixel is value 2 -> red.
    let line1 = vga.render_cga_row(1);
    assert_eq!(&line1[0..4], &[CGA_RED; 4]);
    // Scanline 0 (even) reads 0x0000: value 1 -> green for every pixel,
    // confirming bank selection by scanline parity.
    let line0 = vga.render_cga_row(0);
    assert_eq!(&line0[0..4], &[CGA_GREEN; 4]);
    // Scanline 2 (even, second row) reads 0x0050: value 3 -> brown.
    let line2 = vga.render_cga_row(2);
    assert_eq!(&line2[0..4], &[CGA_BROWN; 4]);
    // Scanline 3 (odd, second row) reads 0x2050: bg/green/red/brown.
    let line3 = vga.render_cga_row(3);
    assert_eq!(&line3[0..4], &[CGA_BLACK, CGA_GREEN, CGA_RED, CGA_BROWN]);
}

#[test]
fn cga_graphics_scanout_honors_start_address_and_wraps() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    vga.cga_write(0x0000, 0b01_01_01_01);
    vga.cga_write(0x0001, 0b10_10_10_10);
    assert_eq!(&vga.render_cga_row(0)[0..4], &[CGA_GREEN; 4]);

    vga.crtc.start_address = 1;
    assert_eq!(&vga.render_cga_row(0)[0..4], &[CGA_RED; 4]);

    vga.crtc.start_address = (CGA_FB_SIZE - 1) as u32;
    vga.cga_write(CGA_FB_SIZE - 1, 0b11_11_11_11);
    assert_eq!(&vga.render_cga_row(0)[0..4], &[CGA_BROWN; 4]);
}

#[test]
fn cga_640x200_unpacks_one_bit_per_pixel() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x06));
    assert_eq!(vga.crtc_regs.r00, 0x38);
    assert_eq!(vga.crtc_regs.r01, 0x28);
    assert_eq!(vga.crtc.char_width, 16);
    assert_eq!(vga.crtc.hdisp_end, 640);
    assert_eq!(htotal_dots(&vga.crtc), 912);
    assert_eq!(vga.active_mode(), VideoMode::Cga);
    // BIOS mode 06h starts white-on-black; 0b10101010 lights every other pixel.
    vga.cga_write(0x0000, 0b1010_1010);
    let line0 = vga.render_cga_row(0);
    assert_eq!(&line0[0..8], &[15, 0, 15, 0, 15, 0, 15, 0]);

    vga.write_port(0x3D9, 0x00);
    assert_eq!(&vga.render_cga_row(0)[0..8], &[0; 8]);

    vga.write_port(0x3D9, 0x04);
    assert_eq!(&vga.render_cga_row(0)[0..8], &[4, 0, 4, 0, 4, 0, 4, 0]);
}

#[test]
fn cga_pixel_helpers_pack_and_xor_raw_pixel_values() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    assert!(vga.cga_write_pixel(2, 1, 3, false));
    assert_eq!(vga.cga_read(0x2000), 0x0C);
    assert_eq!(vga.cga_read_pixel(2, 1), 3);
    assert_eq!(vga.render_cga_row(1)[2], CGA_BROWN);

    assert!(vga.cga_write_pixel(2, 1, 1, true));
    assert_eq!(vga.cga_read_pixel(2, 1), 2);
    assert_eq!(vga.render_cga_row(1)[2], CGA_RED);

    assert!(vga.set_cga_mode(0x06));
    assert!(vga.cga_write_pixel(9, 0, 1, false));
    assert_eq!(vga.cga_read(1), 0x40);
    assert_eq!(vga.cga_read_pixel(9, 0), 1);
    assert_eq!(vga.render_cga_row(0)[9], CGA_WHITE);
}

#[test]
fn cga_mode_set_installs_geometry_and_mode() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    assert_eq!(vga.active_mode(), VideoMode::Cga);
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(htotal_dots(&vga.crtc), 456);
    assert_eq!(vga.crtc.vdisp_end, 200);
    assert_eq!(vga.cga_mode_control(), 0x0A);
    // An unimplemented number leaves the mode untouched.
    assert!(!vga.set_cga_mode(0x09));
}

#[test]
fn cga_mode_control_switches_graphics_decode_without_clearing_fb() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    vga.cga_write(0, 0b10_00_00_00);
    assert_eq!(vga.render_cga_row(0)[0], CGA_RED);

    assert!(vga.write_port(0x3D8, 0x1A));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.crtc.char_width, 16);
    assert_eq!(vga.crtc_regs.r01, 0x28);
    assert_eq!(htotal_dots(&vga.crtc), 912);
    assert_eq!(vga.cga_read(0), 0b10_00_00_00);
    assert_eq!(vga.render_cga_row(0)[0], CGA_BLACK);

    vga.write_port(0x3D9, 0x0F);
    assert_eq!(vga.render_cga_row(0)[0], CGA_WHITE);

    assert!(vga.write_port(0x3D8, 0x02));
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.crtc.char_width, 8);
    assert_eq!(htotal_dots(&vga.crtc), 456);
    assert_eq!(vga.render_cga_row(0)[0], CGA_BLACK);
}

#[test]
fn cga_mode_control_switches_text_width_and_blanks_without_clearing() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));
    text_put(&mut vga, 0, 0, 0xDB, 0x0F);

    assert_eq!(vga.cga_mode_control(), 0x28);
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);

    assert!(vga.write_port(0x3D8, 0x20));
    assert_eq!(vga.render_text_row(0)[0], CGA_BLACK);
    assert_eq!(vga.read_u8(0).unwrap(), 0xDB);

    assert!(vga.write_port(0x3D8, 0x29));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.frame().columns, 80);
    assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);
}

#[test]
fn cga_mode_control_switches_between_text_and_graphics_without_clearing() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));
    text_put(&mut vga, 0, 0, 0b10_00_00_00, 0x0F);

    assert!(vga.write_port(0x3D8, 0x0A));
    assert_eq!(vga.active_mode(), VideoMode::Cga);
    assert_eq!(vga.render_cga_row(0)[0], CGA_RED);
    vga.cga_write(0, b'T');

    assert!(vga.write_port(0x3D8, 0x28));
    assert_eq!(vga.active_mode(), VideoMode::Text);
    assert_eq!(vga.frame().columns, 40);
    assert_eq!(vga.frame().cells[0].character, b'T');
}

#[test]
fn cga_light_pen_ports_set_clear_status_and_latch_position() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    assert_eq!(vga.read_port(0x3DA).unwrap() & 0x06, 0x04);

    vga.advance(htotal_dots(&vga.crtc) * 16 + 80);
    assert_eq!(vga.read_port(0x3DC), Some(0xFF));
    assert_eq!(vga.read_port(0x3DA).unwrap() & 0x06, 0x06);

    vga.write_port(0x3D4, 0x10);
    assert_eq!(vga.read_port(0x3D5), Some(0x01));
    vga.write_port(0x3D4, 0x11);
    assert_eq!(vga.read_port(0x3D5), Some(0x4A));

    assert_eq!(vga.read_port(0x3DB), Some(0xFF));
    assert_eq!(vga.read_port(0x3DA).unwrap() & 0x06, 0x04);
}

#[test]
fn cga_light_pen_graphics_column_has_cga_precision() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    vga.advance(htotal_dots(&vga.crtc) * 11 + 95);
    assert_eq!(vga.read_port(0x3DC), Some(0xFF));
    assert_eq!(vga.cga_light_pen_report(), Some((94, 10, 1, 11)));
    assert_eq!(vga.read_port(0x3DB), Some(0xFF));

    assert!(vga.set_cga_mode(0x06));
    vga.advance(htotal_dots(&vga.crtc) * 11 + 95);
    assert_eq!(vga.read_port(0x3DC), Some(0xFF));
    assert_eq!(vga.cga_light_pen_report(), Some((92, 10, 1, 5)));
}

#[test]
fn cga_crtc_ports_decode_3d0_through_3d7_aliases() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    for (index_port, data_port, value) in [
        (0x3D0, 0x3D1, 0x20),
        (0x3D2, 0x3D3, 0x21),
        (0x3D4, 0x3D5, 0x22),
        (0x3D6, 0x3D7, 0x23),
    ] {
        assert!(vga.write_port(index_port, 0x01));
        assert!(vga.write_port(data_port, value));
        assert_eq!(vga.crtc_index, 0x01);
        assert_eq!(vga.read_port(index_port), None);
        assert_eq!(vga.read_port(data_port), None);
    }

    assert_eq!(vga.raster_width(), 0x23 * 8);
}

#[test]
fn cga_crtc_timing_and_cursor_shape_registers_are_write_only() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    for (index, value) in [(0x01, 0x20), (0x09, 0x01), (0x0A, 0x06), (0x0B, 0x07)] {
        assert!(vga.write_port(0x3D4, index));
        assert!(vga.write_port(0x3D5, value));
        assert_eq!(vga.read_port(0x3D5), None, "CGA CRTC register {index:02X}");
    }
}

#[test]
fn cga_crtc_index_is_a_5_bit_pointer() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    assert!(vga.write_port(0x3D4, 0x8E));
    assert!(vga.write_port(0x3D5, 0x12));
    assert!(vga.write_port(0x3D4, 0x8F));
    assert!(vga.write_port(0x3D5, 0x34));

    assert_eq!(vga.crtc_index, 0x0F);
    assert_eq!(vga.read_port(0x3D4), None);
    assert!(vga.write_port(0x3D4, 0x0E));
    assert_eq!(vga.read_port(0x3D5), Some(0x12));
    assert!(vga.write_port(0x3D4, 0x0F));
    assert_eq!(vga.read_port(0x3D5), Some(0x34));
}

#[test]
fn cga_control_registers_are_6_bit_latches() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    assert!(vga.write_port(0x3D8, 0xFF));
    assert_eq!(vga.cga_mode_control(), 0x3F);
    assert!(vga.write_port(0x3D9, 0xFF));
    assert_eq!(vga.cga_color_select(), 0x3F);
}

#[test]
fn cga_crtc_address_high_registers_are_6_bit_fields() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    assert!(vga.write_port(0x3D4, 0x0C));
    assert!(vga.write_port(0x3D5, 0xFF));
    assert!(vga.write_port(0x3D4, 0x0D));
    assert!(vga.write_port(0x3D5, 0xEE));
    assert!(vga.write_port(0x3D4, 0x0C));
    assert_eq!(vga.crtc_start_register(), 0x3FEE);
    assert_eq!(vga.read_port(0x3D5), None);
    assert!(vga.write_port(0x3D4, 0x0D));
    assert_eq!(vga.read_port(0x3D5), None);

    assert!(vga.write_port(0x3D4, 0x0E));
    assert!(vga.write_port(0x3D5, 0xFF));
    assert!(vga.write_port(0x3D4, 0x0F));
    assert!(vga.write_port(0x3D5, 0xAA));
    assert!(vga.write_port(0x3D4, 0x0E));
    assert_eq!(vga.read_port(0x3D5), Some(0x3F));
    assert!(vga.write_port(0x3D4, 0x0F));
    assert_eq!(vga.read_port(0x3D5), Some(0xAA));
}

#[test]
fn cga_crtc_timing_registers_mask_to_6845_field_widths() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    for index in 0x03..=0x09 {
        assert!(vga.write_port(0x3D4, index));
        assert!(vga.write_port(0x3D5, 0xF0));
    }

    assert_eq!(vga.crtc_regs.r03, 0x00);
    assert_eq!(vga.crtc_regs.r04, 0x70);
    assert_eq!(vga.crtc_regs.r05, 0x10);
    assert_eq!(vga.crtc_regs.r06, 0x70);
    assert_eq!(vga.crtc_regs.r07, 0x70);
    assert_eq!(vga.crtc_regs.r08, 0x00);
    assert_eq!(vga.crtc_regs.r09, 0x10);
    assert_eq!(vga.crtc.max_scan, 0x10);
}

#[test]
fn cga_cursor_mode_zero_does_not_blink() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x02));
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x00;
    vga.cursor_end = 0x07;
    vga.frames = 16;

    assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);
}

#[test]
fn cga_cursor_end_ignores_vga_skew_bits() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x02));
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.frames = 0;

    assert!(vga.write_port(0x3D4, 0x0A));
    assert!(vga.write_port(0x3D5, 0x00));
    assert!(vga.write_port(0x3D4, 0x0B));
    assert!(vga.write_port(0x3D5, 0x67));

    assert_eq!(vga.cursor_end, 0x07);
    assert_eq!(vga.render_text_row(0)[0], CGA_WHITE);
}

#[test]
fn cga_status_reports_display_inactive_when_video_is_disabled() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    assert_eq!(vga.read_port(0x3DA).unwrap() & 0x01, 0x00);

    assert!(vga.write_port(0x3D8, CGA_MODE_GRAPHICS));
    assert_eq!(vga.read_port(0x3DA).unwrap() & 0x01, 0x01);

    assert!(vga.write_port(0x3D8, CGA_MODE_GRAPHICS | CGA_MODE_VIDEO_ENABLE));
    assert_eq!(vga.read_port(0x3DA).unwrap() & 0x01, 0x00);
}

#[test]
fn cga_crtc_registers_can_retune_80_column_text_to_160x100() {
    let mut vga = Vga::default();
    vga.set_text_mode();
    assert_eq!(vga.raster_width(), 720);

    assert!(vga.write_port(0x3D8, 0x01)); // 80-column text, video disabled
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.render_text_row(0)[0], CGA_BLACK);

    for (index, value) in [(0x04, 0x7F), (0x06, 0x64), (0x07, 0x70), (0x09, 0x01)] {
        vga.write_port(0x3D4, index);
        vga.write_port(0x3D5, value);
    }
    text_put(&mut vga, 99, 0, 0xDB, 0x0F);

    assert!(vga.write_port(0x3D8, 0x09)); // preserve CRTC retune, enable video
    assert_eq!(vga.crtc.max_scan, 1);
    assert_eq!(vga.crtc.vtotal, 262);
    assert_eq!(vga.crtc.vdisp_end, 200);
    assert_eq!(vga.crtc.vretrace_start, 224);
    assert_eq!(vga.frame().columns, 80);

    vga.write_port(0x3D4, 0x09);
    assert_eq!(vga.read_port(0x3D5), None);
    assert_eq!(vga.render_text_row(198)[0], CGA_WHITE);

    let raster = vga.render_full_frame();
    assert_eq!(raster.width, 640);
    assert_eq!(raster.height, 262);
    assert_eq!(raster.pixels[200 * raster.width as usize], CGA_BLACK);
}

#[test]
fn cga_crtc_horizontal_registers_drive_width_and_survive_video_enable() {
    let mut vga = Vga::default();
    vga.set_text_mode();

    assert!(vga.write_port(0x3D8, 0x01)); // 80-column text, video disabled
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(htotal_dots(&vga.crtc), 912);

    for (index, value) in [(0x00, 0x63), (0x01, 0x28)] {
        vga.write_port(0x3D4, index);
        vga.write_port(0x3D5, value);
    }

    assert_eq!(vga.raster_width(), 320);
    assert_eq!(htotal_dots(&vga.crtc), 800);
    assert_eq!(vga.frame().columns, 40);

    assert!(vga.write_port(0x3D8, 0x09)); // enable only; keep manual R0/R1
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(htotal_dots(&vga.crtc), 800);
    vga.write_port(0x3D4, 0x01);
    assert_eq!(vga.read_port(0x3D5), None);
}

#[test]
fn cga_crtc_horizontal_displayed_drives_graphics_row_stride() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    vga.write_port(0x3D4, 0x01);
    vga.write_port(0x3D5, 0x20); // 32 displayed chars: 256 pixels, 64 bytes/row
    assert_eq!(vga.raster_width(), 256);

    vga.cga_write(80, 0b10_10_10_10); // old fixed stride would read this
    vga.cga_write(64, 0b01_01_01_01); // live 64-byte stride reads this
    assert_eq!(vga.cga_read_pixel(0, 2), 1);
    assert_eq!(vga.render_cga_row(2)[0], CGA_GREEN);
}

#[test]
fn cga_640x200_crtc_displayed_uses_16_dot_characters() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x06));

    vga.write_port(0x3D4, 0x01);
    vga.write_port(0x3D5, 0x20); // 32 displayed chars: 512 pixels in high-res CGA.
    assert_eq!(vga.crtc.char_width, 16);
    assert_eq!(vga.raster_width(), 512);

    vga.cga_write(80, 0x00); // old fixed 80-byte stride would read this
    vga.cga_write(64, 0x80); // live 64-byte stride reads this
    assert_eq!(vga.cga_read_pixel(0, 2), 1);
    assert_eq!(vga.render_cga_row(2)[0], CGA_WHITE);
}

#[test]
fn display_offset_applies_byte_word_dword_transforms() {
    // Byte mode (CR17 bit 6 = 1): identity, wrapped at 64 KB.
    assert_eq!(display_offset(0xE3, 0x00, 0x1234), 0x1234);
    assert_eq!(display_offset(0xE3, 0x00, 0x1_0005), 0x0005); // 64 KB counter wrap
    assert_eq!(
        display_counter(0xE3, 0x00, 0x1000, 3),
        0x1003,
        "normal address clock increments every character"
    );
    assert_eq!(
        display_counter(0xEB, 0x00, 0x1000, 3),
        0x1001,
        "CR17 bit 3 divides the address clock by two"
    );
    assert_eq!(
        display_counter(0xE3, 0x20, 0x1000, 7),
        0x1001,
        "CR14 bit 5 divides the address clock by four"
    );
    // Word mode, 16-bit wrap (CR17 = 0xA3: bit 6 = 0, bit 5 = 1): rotate left 1,
    // MA15 into bit 0.
    assert_eq!(display_offset(0xA3, 0x00, 0x4001), 0x8002); // MA15 = 0
    assert_eq!(display_offset(0xA3, 0x00, 0x8000), 0x0001); // MA15 = 1 -> bit 0
    // Word mode, 14-bit wrap (CR17 = 0x83: bit 6 = 0, bit 5 = 0): MA13 into bit 0.
    assert_eq!(display_offset(0x83, 0x00, 0x2000), 0x4001); // MA13 = 1 -> bit 0
    // Doubleword mode (CR14 bit 6 = 1): shift left two, forcing MA0/MA1 low.
    assert_eq!(display_offset(0xA3, 0x40, 0x3000), 0xC000);
    assert_eq!(
        display_offset_row(0xA0, 0x40, 0, 3),
        0x6000,
        "CR17 bits 0/1 still substitute row-scan bits in doubleword mode"
    );
    // Byte mode wins over the doubleword bit.
    assert_eq!(display_offset(0xE3, 0x40, 0x1234), 0x1234);
    assert_eq!(
        display_offset_row(0xE0, 0x00, 0, 3),
        0x6000,
        "CR17 bits 0/1 clear substitute row-scan bits into address bits 13/14"
    );
}

#[test]
fn crtc_addressing_registers_are_wired_and_default_per_mode() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // 16-color planar modes power up in byte mode (CR17 = 0xE3).
    assert_eq!(vga.crtc.mode_control, 0xE3);
    assert_eq!(vga.crtc.underline_loc, 0x00);
    // A guest write through the CRTC ports updates the live registers.
    vga.write_port(0x3D4, 0x17); // CRTC index 17h
    vga.write_port(0x3D5, 0xA3); // word mode
    assert_eq!(vga.crtc.mode_control, 0xA3);
    vga.write_port(0x3D4, 0x14); // CRTC index 14h
    vga.write_port(0x3D5, 0x40); // doubleword bit
    assert_eq!(vga.crtc.underline_loc, 0x40);
}

#[test]
fn crtc_address_generation_bits_affect_scanout() {
    let mut vga = Vga::default();
    vga.set_mode_0dh(); // double-scanned, so row 0 and row 1 share source row 0.
    vga.crtc.mode_control &= !0x01; // substitute row-scan bit 0 for address bit 13.
    vga.vram[0] = 0x80;
    vga.vram[VGA_PLANE_SIZE + 0x2000] = 0x80;

    assert_eq!(vga.render_active_row(0)[0], 0x01);
    assert_eq!(vga.render_active_row(1)[0], 0x02);

    let mut divided = Vga::default();
    assert!(divided.set_mode(0x10));
    divided.crtc.mode_control |= 0x08; // divide address clock by two.
    divided.vram[0] = 0x80;
    divided.vram[VGA_PLANE_SIZE + 1] = 0x80;
    assert_eq!(divided.render_active_row(0)[0], 0x01);
    assert_eq!(
        divided.render_active_row(0)[8],
        0x01,
        "second byte column still reads offset 0 when the address clock is /2"
    );
    assert_eq!(divided.render_active_row(0)[16], 0x02);
}

#[test]
fn line_compare_registers_assemble_ten_bits_and_default_per_mode() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // 16-color planar modes power up with the split disabled (line compare 0x3FF).
    assert_eq!(vga.crtc.line_compare, 0x3FF);
    // Assemble a split at scanline 0x150: low byte via 18h, bit 8 set via the
    // Overflow register 07h bit 4, bit 9 cleared via the Maximum Scan Line 09h bit 6.
    vga.write_port(0x3D4, 0x18);
    vga.write_port(0x3D5, 0x50);
    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x10); // bit 4 set -> line compare bit 8 = 1
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x00); // bit 6 clear -> line compare bit 9 = 0
    assert_eq!(vga.crtc.line_compare, 0x150);
    // Clearing the overflow bit 4 drops line compare bit 8.
    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x00);
    assert_eq!(vga.crtc.line_compare, 0x050);
}

#[test]
fn beam_position_tracks_dots_in_scan_counter_units() {
    let t = CrtcTiming::mode_0dh();
    let htotal = (t.htotal_chars * t.char_width) as u64; // 800
    let dots = htotal * 5 + 10; // 5 full lines + 10 dots
    assert_eq!(beam_line(&t, dots), 5);
    assert_eq!(beam_dot(&t, dots), 10);
    assert!(beam_display_enable(&t, dots)); // line 5 < 400, dot 10 < 320
    assert!(!beam_vretrace(&t, dots)); // 5 < vretrace_start 412
}

#[test]
fn advance_rolls_over_one_frame_in_o1() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    let frame = vga.frame_dots();
    vga.advance(frame * 2 + 7); // just past two frames in one call
    assert_eq!(vga.beam_dots(), 7); // (2*frame+7) mod frame
    assert_eq!(vga.frames_completed(), 2);
}

#[test]
fn dots_until_vretrace_start_measures_to_the_edge_from_any_beam_position() {
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x13));
    let htotal = htotal_dots(&vga.crtc);
    let frame = vga.frame_dots();
    let edge = u64::from(vga.crtc.vretrace_start) * htotal;

    // From the top of the frame the distance is the edge position itself.
    vga.beam = 0;
    assert_eq!(vga.dots_until_vretrace_start(), Some(edge));

    // One dot before the edge.
    vga.beam = edge - 1;
    assert_eq!(vga.dots_until_vretrace_start(), Some(1));

    // Exactly ON the edge (inside the window): the NEXT edge, a full frame
    // ahead, never zero. This is the termination guarantee edge-aware
    // schedulers rely on.
    vga.beam = edge;
    assert_eq!(vga.dots_until_vretrace_start(), Some(frame));

    // Inside the window, one line in: next frame's edge.
    vga.beam = edge + htotal;
    assert_eq!(vga.dots_until_vretrace_start(), Some(frame - htotal));

    // Advancing by the reported distance lands the beam on the edge, where
    // the vretrace status bit reads set.
    vga.beam = 12_345 % frame;
    let to_edge = vga.dots_until_vretrace_start().unwrap();
    vga.advance(to_edge);
    assert!(
        beam_vretrace(&vga.crtc, vga.beam),
        "beam must land inside the vretrace window"
    );
    assert_eq!(vga.beam, edge);
}

#[test]
fn boots_with_defined_frame_dots_and_zeroed_vram() {
    let vga = Vga::default();
    assert_eq!(vga.vram.len(), VGA_PLANAR_SIZE);
    assert!(vga.vram.iter().all(|&b| b == 0));
    // frame_dots must be non-zero at boot (default text timing) so the
    // per-instruction beam advance never divides by zero. (Spec §3/§6.)
    assert!(
        vga.frame_dots() > 0,
        "frame_dots must be defined before any mode-set"
    );
}

#[test]
fn write_mode_0_applies_rotate_setreset_logic_and_bitmask() {
    // Latches preloaded to 0xFF on all planes; write 0x0F with bit mask 0xF0,
    // copy logic, no set/reset. Result per plane = (data & mask) | (latch & !mask)
    // = (0x0F & 0xF0) | (0xFF & 0x0F) = 0x00 | 0x0F = 0x0F.
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let gc = GfxController {
        bit_mask: 0xF0,
        ..Default::default()
    };
    let latches = [0xFFu8; VGA_PLANES];
    write_planes(&mut planes, 0x0F, &gc, &latches);
    for p in &planes {
        assert_eq!(p[0], 0x0F);
    }
}

#[test]
fn write_mode_0_set_reset_substitutes_color_per_plane() {
    // Enable set/reset on all planes, set/reset value = 0b1010 (planes 1 and 3).
    // With full bit mask and copy, each enabled plane writes its set/reset bit
    // expanded to 0xFF or 0x00.
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let gc = GfxController {
        bit_mask: 0xFF,
        enable_set_reset: 0x0F,
        set_reset: 0b1010,
        ..Default::default()
    };
    let latches = [0u8; VGA_PLANES];
    write_planes(&mut planes, 0x00, &gc, &latches);
    assert_eq!(planes[0][0], 0x00);
    assert_eq!(planes[1][0], 0xFF);
    assert_eq!(planes[2][0], 0x00);
    assert_eq!(planes[3][0], 0xFF);
}

#[test]
fn write_mode_1_copies_latches_to_planes() {
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let gc = GfxController {
        write_mode: 1,
        ..Default::default()
    };
    let latches = [0x12, 0x34, 0x56, 0x78];
    write_planes(&mut planes, 0x00, &gc, &latches); // data ignored in WM1
    for plane in 0..VGA_PLANES {
        assert_eq!(planes[plane][0], latches[plane]);
    }
}

#[test]
fn write_mode_2_expands_color_nibble_per_plane() {
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let gc = GfxController {
        write_mode: 2,
        bit_mask: 0xFF,
        ..Default::default()
    };
    let latches = [0u8; VGA_PLANES];
    write_planes(&mut planes, 0b0101, &gc, &latches); // planes 0 and 2 set
    assert_eq!(planes[0][0], 0xFF);
    assert_eq!(planes[1][0], 0x00);
    assert_eq!(planes[2][0], 0xFF);
    assert_eq!(planes[3][0], 0x00);
}

#[test]
fn write_mode_3_uses_set_reset_color_with_rotated_bitmask() {
    // Effective mask = bit_mask (0xFF) & rotated data (0xF0, rotate=0) = 0xF0.
    // Set/Reset 0b0011 -> planes 0,1 color 0xFF, planes 2,3 color 0x00.
    // Result = (color & 0xF0) | (latch 0 & 0x0F).
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let gc = GfxController {
        write_mode: 3,
        set_reset: 0b0011,
        bit_mask: 0xFF,
        rotate: 0,
        ..Default::default()
    };
    let latches = [0u8; VGA_PLANES];
    write_planes(&mut planes, 0xF0, &gc, &latches);
    assert_eq!(planes[0][0], 0xF0);
    assert_eq!(planes[1][0], 0xF0);
    assert_eq!(planes[2][0], 0x00);
    assert_eq!(planes[3][0], 0x00);
}

#[test]
fn read_mode_0_returns_selected_plane_and_loads_latches() {
    let planes = [[0x11u8; 1], [0x22u8; 1], [0x33u8; 1], [0x44u8; 1]];
    let gc = GfxController {
        read_map: 2,
        ..Default::default()
    };
    let mut latches = [0u8; VGA_PLANES];
    let byte = read_planes(&planes, &gc, &mut latches);
    assert_eq!(byte, 0x33);
    assert_eq!(latches, [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn read_mode_1_color_compares_each_bit() {
    let planes = [[0xFFu8; 1], [0x00u8; 1], [0xFFu8; 1], [0x00u8; 1]];
    let gc = GfxController {
        read_mode: 1,
        color_dont_care: 0x0F, // care about all four planes
        color_compare: 0b0101, // planes 0 and 2 set, 1 and 3 clear
        ..Default::default()
    };
    let mut latches = [0u8; VGA_PLANES];
    let byte = read_planes(&planes, &gc, &mut latches);
    assert_eq!(byte, 0xFF); // every bit position matches the pattern
}

#[test]
fn cpu_write_then_read_round_trips_through_latches() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.cpu_write(0x10, 0xA5);
    vga.gc.read_map = 0;
    assert_eq!(vga.cpu_read(0x10), 0xA5);
    assert_eq!(vga.latches, [0xA5; VGA_PLANES]);
}

#[test]
fn map_mask_gates_which_planes_are_written() {
    let mut vga = Vga::default();
    vga.seq.memory_mode = 0x04; // sequential planar addressing
    vga.seq.map_mask = 0b0001; // only plane 0
    vga.gc.bit_mask = 0xFF;
    vga.cpu_write(0, 0xFF);
    assert_eq!(vga.plane_byte(0, 0), 0xFF);
    assert_eq!(vga.plane_byte(1, 0), 0x00);
}

#[test]
fn odd_even_write_routes_cpu_addresses_to_plane_pairs() {
    let mut vga = Vga::default();
    vga.seq.memory_mode = 0x02; // bit 2 clear: odd/even writes enabled
    vga.seq.map_mask = 0x0F;
    vga.gc.bit_mask = 0xFF;

    vga.cpu_write(0, 0xA5);
    vga.cpu_write(1, 0x5A);

    assert_eq!(vga.plane_byte(0, 0), 0xA5);
    assert_eq!(vga.plane_byte(2, 0), 0xA5);
    assert_eq!(vga.plane_byte(1, 0), 0x5A);
    assert_eq!(vga.plane_byte(3, 0), 0x5A);
    assert_eq!(
        vga.plane_byte(0, 1),
        0x00,
        "odd/even addressing advances the plane offset every two CPU bytes"
    );
}

#[test]
fn odd_even_read_uses_address_parity_and_read_map_pair() {
    let mut vga = Vga::default();
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x10); // GC05 bit 4: odd/even read mode
    assert_eq!(vga.read_port(0x3CF), Some(0x10));
    vga.write_port(0x3CE, 0x06);
    vga.write_port(0x3CF, 0x03); // graphics + chain odd/even

    vga.vram[0] = 0x10;
    vga.vram[VGA_PLANE_SIZE] = 0x11;
    vga.vram[2 * VGA_PLANE_SIZE] = 0x20;
    vga.vram[3 * VGA_PLANE_SIZE] = 0x21;
    vga.vram[1] = 0x30;
    vga.vram[VGA_PLANE_SIZE + 1] = 0x31;

    vga.gc.read_map = 0;
    assert_eq!(vga.cpu_read(0), 0x10);
    assert_eq!(vga.cpu_read(1), 0x11);
    assert_eq!(vga.cpu_read(3), 0x31);

    vga.gc.read_map = 2;
    assert_eq!(vga.cpu_read(0), 0x20);
    assert_eq!(vga.cpu_read(1), 0x21);
    assert_eq!(vga.latches, [0x10, 0x11, 0x20, 0x21]);
}

#[test]
fn catch_up_is_incremental_and_zero_when_beam_has_not_moved_a_line() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * 3 + 5); // beam at line 3
    let drawn = vga.catch_up();
    assert_eq!(drawn, 3); // lines 0,1,2 rendered
    let drawn_again = vga.catch_up();
    assert_eq!(drawn_again, 0); // no line crossed since
}

#[test]
fn advance_past_a_frame_finalizes_a_presented_buffer() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    assert!(vga.take_presented().is_none());
    vga.advance(vga.frame_dots() + 10); // cross one frame
    assert!(vga.presented_ready());
}

#[test]
fn presented_frame_carries_active_visible_height_below_the_beam_total() {
    // The host crops to the active region for display: `display_height`
    // (vdisp_end) is the visible image; `height` stays the full beam frame
    // (vtotal) including the retrace/border the monitor never shows. Cropping
    // to display_height is what drops the black bottom bar before aspect-fill.
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(vga.frame_dots() + 10);
    let raster = vga.take_presented().unwrap();
    assert_eq!(
        raster.height, vga.crtc.vtotal,
        "height is the full beam frame"
    );
    assert_eq!(
        raster.display_height, vga.crtc.vdisp_end,
        "display_height is the visible active region"
    );
    assert!(
        raster.display_height < raster.height,
        "the vertical blanking/border is excluded from the visible region"
    );
    assert_eq!(raster.pixels.len(), (raster.width * raster.height) as usize);
}

#[test]
fn short_display_end_top_justifies_with_shortfall_at_bottom() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.crtc.vdisp_end = 199;
    vga.crtc.vtotal = 525;
    vga.crtc.vblank_start = 245;
    vga.crtc.vblank_end = 520;
    vga.crtc.vretrace_start = 247;
    vga.crtc.vretrace_end = 249;
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
        *b = 0xFF;
    } // plane 0 set
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    let raster = vga.render_full_frame();
    let w = raster.width as usize;
    assert_ne!(
        raster.pixels[0], 0,
        "row 0 should be active (top-justified)"
    );
    let last = (raster.height as usize - 1) * w;
    assert_eq!(
        raster.pixels[last], 0,
        "bottom row is border/blank, not active"
    );
}

#[test]
fn pixel_pan_shifts_the_active_row_left() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.vram[0] = 0x80; // pixel 0 set in plane 0
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.attr.pixel_pan = 0;
    let row0 = vga.render_active_row(0);
    vga.attr.pixel_pan = 1;
    let row1 = vga.render_active_row(0);
    assert_eq!(row1[0], row0[1], "pan=1 shifts the row one pixel left");
}

#[test]
fn start_address_write_applies_next_frame_not_mid_frame() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * 100); // beam mid-frame, line 100
    vga.set_start_address(0x2000); // buffered, not active yet
    assert_eq!(
        vga.crtc.start_address, 0,
        "start address unchanged this frame"
    );
    vga.advance(vga.frame_dots()); // cross the frame boundary
    assert_eq!(vga.crtc.start_address, 0x2000, "applied on the next frame");
}

#[test]
fn start_address_write_during_retrace_still_applies_next_frame() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * (vga.crtc.vretrace_start as u64 + 1));
    vga.set_start_address(0x4000);
    vga.advance(vga.frame_dots());
    assert_eq!(vga.crtc.start_address, 0x4000, "no two-frame lag");
}

#[test]
fn mode_set_discards_a_stale_pending_start_address() {
    // A BIOS mode set reprograms the start address, so a page-flip latch
    // from the prior mode must not be applied at the next frame boundary.
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.set_start_address(0x2000); // flip latched, not yet applied
    vga.set_mode13h();
    vga.advance(vga.frame_dots());
    assert_eq!(vga.crtc.start_address, 0, "13h scanout starts at 0");

    vga.set_start_address(0x4000);
    vga.set_mode_0dh();
    vga.advance(vga.frame_dots());
    assert_eq!(vga.crtc.start_address, 0, "0Dh scanout starts at 0");
}

#[test]
fn cga_start_address_registers_are_write_only_but_latch_pending_value() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));

    vga.write_port(0x3D4, 0x0C);
    vga.write_port(0x3D5, 0x12);
    vga.write_port(0x3D4, 0x0D);
    vga.write_port(0x3D5, 0x34);

    assert_eq!(vga.crtc_start_address(), 0);
    assert_eq!(vga.pending_start_address(), Some(0x1234));
    assert_eq!(vga.crtc_start_register(), 0x1234);
    vga.write_port(0x3D4, 0x0C);
    assert_eq!(vga.read_port(0x3D5), None);
    vga.write_port(0x3D4, 0x0D);
    assert_eq!(vga.read_port(0x3D5), None);
}

#[test]
fn gc_and_seq_ports_round_trip_and_catch_up_runs_first() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * 4); // beam at line 4
    vga.write_port(0x3CE, 8); // GC index 8 = bit mask
    vga.write_port(0x3CF, 0x0F);
    assert_eq!(vga.gc.bit_mask, 0x0F);
    assert_eq!(vga.last_line, 4); // the write caught up through line 4
}

#[test]
fn gc06_memory_map_select_decodes_four_apertures() {
    // Memory Map Select (bits 3-2) picks the CPU aperture window.
    let mut vga = Vga::default();
    for (sel, base, length) in [
        (0b00u8, 0xA_0000u32, 0x2_0000u32), // A0000-BFFFF, 128K
        (0b01, 0xA_0000, 0x1_0000),         // A0000-AFFFF, 64K
        (0b10, 0xB_0000, 0x0_8000),         // B0000-B7FFF, 32K
        (0b11, 0xB_8000, 0x0_8000),         // B8000-BFFFF, 32K
    ] {
        vga.write_port(0x3CE, 0x06); // GC index 06h
        vga.write_port(0x3CF, sel << 2);
        let ap = vga.gfx_aperture();
        assert_eq!(ap.base, base, "base for map select {sel:#04b}");
        assert_eq!(ap.length, length, "length for map select {sel:#04b}");
    }
}

#[test]
fn gc06_graphics_and_chain_odd_even_flags_read_back() {
    let mut vga = Vga::default();
    vga.write_port(0x3CE, 0x06);
    // bit 0 graphics, bit 1 chain odd/even, both set.
    vga.write_port(0x3CF, 0x03);
    let ap = vga.gfx_aperture();
    assert!(ap.graphics, "bit 0 set selects graphics mode");
    assert!(ap.chain_odd_even, "bit 1 set enables chain odd/even");
    // The raw register reads back through 3CF (low 4 bits stored).
    assert_eq!(vga.read_port(0x3CF), Some(0x03));

    // Clearing both flags reads back as alphanumeric, no chaining.
    vga.write_port(0x3CF, 0x00);
    let ap = vga.gfx_aperture();
    assert!(!ap.graphics);
    assert!(!ap.chain_odd_even);
}

#[test]
fn horizontal_crtc_timing_registers_round_trip() {
    // Indices 00h-05h are the horizontal timing group; each reads back the
    // exact byte written, including the split-field registers 03h and 05h.
    let mut vga = Vga::default();
    let writes = [
        (0x00u8, 0x5Fu8),
        (0x01, 0x4F),
        (0x02, 0x50),
        (0x03, 0x82),
        (0x04, 0x54),
        (0x05, 0x80),
    ];
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x00);
    for (index, value) in writes {
        vga.write_port(0x3D4, index);
        vga.write_port(0x3D5, value);
    }
    for (index, value) in writes {
        vga.write_port(0x3D4, index);
        assert_eq!(
            vga.read_port(0x3D5),
            Some(value),
            "horizontal CRTC index {index:#04x} round-trips"
        );
    }
}

#[test]
fn crtc_11h_write_protect_locks_registers_00h_through_07h() {
    let mut vga = Vga::default();
    vga.crtc_regs.r00 = 0x5F;
    vga.crtc_regs.r07 = 0x00;
    vga.crtc.line_compare = 0;

    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x80);
    vga.write_port(0x3D4, 0x00);
    vga.write_port(0x3D5, 0x77);
    assert_eq!(vga.crtc_regs.r00, 0x5F);

    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x10);
    assert_eq!(vga.crtc_regs.r07, 0x00);
    assert_eq!(vga.crtc.line_compare & 0x100, 0x100);

    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x00);
    vga.write_port(0x3D4, 0x00);
    vga.write_port(0x3D5, 0x77);
    assert_eq!(vga.crtc_regs.r00, 0x77);
}

#[test]
fn attribute_flipflop_alternates_index_then_data() {
    let mut vga = Vga::default();
    vga.read_status1(); // reset flip-flop to "index"
    vga.write_port(0x3C0, 0x13); // pixel pan index
    vga.write_port(0x3C0, 0x02); // value
    assert_eq!(vga.attr.pixel_pan, 0x02);
}

#[test]
fn register_banged_mode13h_entry_flips_the_personality() {
    // DOS Quake 1.06 sets 320x200x256 by writing the full register set, no
    // INT 10h: chain-4 on (SEQ 04h bit 3), then ATC 10h with graphics
    // (bit 0) + 8-bit color (bit 6). The ATC mode-control write decides.
    let mut vga = Vga::default();
    assert_eq!(vga.active_mode(), VideoMode::Text);
    vga.write_port(0x3C4, 0x04); // SEQ index: memory mode
    vga.write_port(0x3C5, 0x0E); // chain-4 on
    vga.read_status1(); // reset the ATC flip-flop
    vga.write_port(0x3C0, 0x10); // ATC index: mode control
    vga.write_port(0x3C0, 0x41); // graphics + 8-bit color
    assert_eq!(vga.active_mode(), VideoMode::Mode13h);
}

#[test]
fn atc_graphics_bits_without_chain4_stay_text() {
    // A text-mode guest (or an INT 10h state restore) re-writing ATC 10h
    // with the graphics bits must NOT flip the personality unless the
    // Graphics Controller is actually set up for 256-color graphics. Chain-4
    // off alone (with no GC 256-color/graphics bits) is not enough — a stray
    // ATC write in text mode must stay text.
    let mut vga = Vga::default();
    vga.read_status1();
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41); // graphics + 8-bit, but GC not set up
    assert_eq!(vga.active_mode(), VideoMode::Text);
}

#[test]
fn register_banged_mode_x_320x240_entry_from_text() {
    // TSUMERA (Borland 32RTM) sets 320x240 unchained mode X by banging the
    // full VGA register set from text mode, no INT 10h: SEQ memory-mode with
    // chain-4 OFF, the GC 256-color graphics bits, the guest's own 240-line
    // vertical CRTC timing, then ATC 10h graphics+8bit (the decision write).
    // The personality must flip to ModeX with the GUEST's geometry (480
    // scanlines double-scanned to 240) — not stay text (blank screen), nor
    // snap to canonical 320x200 (loses 40 lines).
    let mut vga = Vga::default();
    assert_eq!(vga.active_mode(), VideoMode::Text);

    // SEQ memory mode 0x06: extended + odd/even disabled, chain-4 OFF.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    // GC: 256-color shift (index 05h bit 6) + graphics mode / A0000 (index 06h).
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x40);
    vga.write_port(0x3CE, 0x06);
    vga.write_port(0x3CF, 0x05);
    // CRTC: the guest's own 320x240 mode-X timing (unlock 11h protect first).
    for (idx, val) in [
        (0x11u8, 0x2cu8),
        (0x00, 0x5f),
        (0x01, 0x4f),
        (0x02, 0x50),
        (0x03, 0x82),
        (0x04, 0x54),
        (0x05, 0x80),
        (0x06, 0x0b),
        (0x07, 0x3e),
        (0x09, 0xc0),
        (0x10, 0xea),
        (0x11, 0xac),
        (0x12, 0xdf),
        (0x13, 0x28),
        (0x15, 0xe7),
        (0x16, 0x05),
        (0x17, 0xe3),
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }
    // ATC 10h = graphics (bit 0) + 8-bit color (bit 6): the decision write.
    vga.read_status1();
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41);

    assert_eq!(vga.active_mode(), VideoMode::ModeX);
    assert_eq!(vga.raster_width(), 320);
    // Guest vertical timing captured from text-mode CRTC writes (not canonical
    // 320x200): VDE 0x1df + 1 = 480 scanlines, double-scanned (09h bit 7) to 240.
    assert_eq!(vga.crtc.vdisp_end, 480);
    assert!(vga.crtc.double_scan);
}

#[test]
fn default_attr_palette_is_identity() {
    // Real VGA powers up with ATC palette register N = N, so a 4-bit plane
    // index maps straight to DAC N.
    let attr = Attribute::default();
    assert_eq!(
        attr.palette,
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
    assert_eq!(attr.plane_enable & 0x0F, 0x0F);
}

#[test]
fn misc_output_round_trips_3c2_3cc() {
    let mut vga = Vga::default();
    assert!(vga.write_port(0x3C2, 0x42));
    assert_eq!(vga.read_port(0x3CC), Some(0x42));
}

#[test]
fn misc_output_clock_select_drives_dot_clock() {
    let mut vga = Vga::default();
    assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_28_HZ);

    assert!(vga.write_port(0x3C2, 0x00));
    assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_25_HZ);
    assert!(vga.write_port(0x3C2, 0x04));
    assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_28_HZ);
    assert!(vga.write_port(0x3C2, 0x08));
    assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_25_HZ);

    vga.set_mode13h();
    assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_25_HZ);
    vga.set_text_mode();
    assert_eq!(vga.dot_clock_hz(), VGA_DOT_CLOCK_28_HZ);
}

#[test]
fn misc_output_ios_selects_crtc_status_and_feature_ports() {
    let mut vga = Vga::default();

    assert!(vga.write_port(0x3D4, 0x0C));
    assert_eq!(vga.read_port(0x3D4), Some(0x0C));
    assert_eq!(vga.read_port(0x3B4), None);
    assert!(vga.read_port(0x3DA).is_some());
    assert_eq!(vga.read_port(0x3BA), None);
    assert!(vga.write_port(0x3DA, 0x0A));
    assert_eq!(vga.read_port(0x3CA), Some(0x0A));

    assert!(vga.write_port(0x3C2, vga.misc_output & !0x01));
    assert!(!vga.write_port(0x3D4, 0x0A));
    assert!(vga.write_port(0x3B4, 0x0A));
    assert_eq!(vga.read_port(0x3B4), Some(0x0A));
    assert_eq!(vga.read_port(0x3D4), None);
    assert!(vga.write_port(0x3B5, 0x05));
    assert_eq!(vga.cursor_start, 0x05);
    assert!(vga.read_port(0x3BA).is_some());
    assert_eq!(vga.read_port(0x3DA), None);
    assert!(vga.write_port(0x3BA, 0x05));
    assert_eq!(vga.read_port(0x3CA), Some(0x05));
}

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

#[test]
fn pel_mask_round_trips_3c6() {
    let mut vga = Vga::default();
    assert!(vga.write_port(0x3C6, 0x0F));
    assert_eq!(vga.read_port(0x3C6), Some(0x0F));
}

#[test]
fn atc_readback_3c1_returns_indexed_register() {
    let mut vga = Vga::default();
    vga.read_status1(); // reset the 3C0 flip-flop to "address"
    vga.write_port(0x3C0, 0x13); // address: select the Pixel Pan register
    vga.write_port(0x3C0, 0x07); // data: pixel_pan = 7
    // 3C1 reads back the register selected by the last index write.
    assert_eq!(vga.read_port(0x3C1), Some(0x07));
}

#[test]
fn pel_mask_masks_the_dac_index_in_render() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Plane 0 set everywhere so every pixel is the 4-bit index 1.
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
        *b = 0xFF;
    }
    vga.attr.palette[1] = 0x2A; // ATC maps index 1 -> DAC 42
    vga.pel_mask = 0xFF;
    let full = vga.render_active_row(0);
    assert_eq!(full[0], 0x2A, "no mask: index 1 reaches DAC 42");
    vga.pel_mask = 0x0F;
    let masked = vga.render_active_row(0);
    assert_eq!(
        masked[0], 0x0A,
        "pel mask 0x0F folds DAC 42 to the low nibble"
    );
}

#[test]
fn mid_frame_palette_change_splits_the_raster_at_the_beam_row() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Active content = attribute index 1 everywhere (plane 0 set).
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
        *b = 0xFF;
    }
    vga.attr.palette = core::array::from_fn(|i| i as u8); // index 1 -> DAC 1
    // Run to counter line 50, then repaint palette[1] = 9 via the attribute port.
    vga.advance(htotal_dots(&vga.crtc) * 50);
    // Index 1 with bit 5 (Palette Address Source) set keeps the display on
    // while the palette register is rewritten, so the screen does not blank.
    vga.write_port(0x3C0, 0x20 | 0x01); // attr index 1, PAS on
    vga.write_port(0x3C0, 9); // palette[1] = 9
    // Finish the frame.
    vga.advance(vga.frame_dots());
    let raster = vga.take_presented().unwrap();
    let w = raster.width as usize;
    assert_eq!(raster.pixels[0], 1, "above the split uses the old palette");
    let below = 120 * w; // raster row 120 (counter line 120, > split at 50)
    assert_eq!(
        raster.pixels[below], 9,
        "below the split uses the new palette"
    );
}

#[test]
fn status1_reports_beam_and_resets_attribute_flipflop() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Park the beam in vertical retrace.
    let htotal = htotal_dots(&vga.crtc);
    vga.beam = htotal * (vga.crtc.vretrace_start as u64);
    let status = vga.read_status1();
    assert_eq!(status & 0x08, 0x08); // bit 3 vertical retrace
    assert_eq!(status & 0x01, 0x01); // bit 0 display disabled (in retrace)
    // Reading 3DA resets the attribute address/data flip-flop to "address".
    assert!(!vga.attr.flip_flop_data);
}

#[test]
fn status1_reports_attribute_video_status_mux_bits() {
    let mut vga = Vga::default();
    vga.set_mode13h();

    for (color, mux, expected) in [
        (0x04, 0x00, 0x20), // mux 00: status bits 5/4 = colour bits 2/0
        (0x01, 0x00, 0x10),
        (0x20, 0x10, 0x20), // mux 01: colour bits 5/4
        (0x10, 0x10, 0x10),
        (0x08, 0x20, 0x20), // mux 10: colour bits 3/1
        (0x02, 0x20, 0x10),
        (0x80, 0x30, 0x20), // mux 11: colour bits 7/6
        (0x40, 0x30, 0x10),
    ] {
        vga.beam = 0;
        if vga.active_mode() == VideoMode::Mode13h {
            vga.cpu_write_chain4(0, color);
        } else {
            vga.vram[0] = color;
        }
        vga.attr.plane_enable = 0x0F | mux;
        assert_eq!(vga.read_status1() & 0x30, expected);
    }

    let htotal = htotal_dots(&vga.crtc);
    vga.beam = htotal * u64::from(vga.crtc.vretrace_start);
    if vga.active_mode() == VideoMode::Mode13h {
        vga.cpu_write_chain4(0, 0xC0);
    } else {
        vga.vram[0] = 0xC0;
    }
    vga.attr.plane_enable = 0x3F;
    assert_eq!(vga.read_status1() & 0x30, 0x00);
}

#[test]
fn status_mux_single_pixel_sample_matches_the_full_row_render() {
    // Differential oracle for the thinned video_status_mux_bits: the old
    // implementation rendered the ENTIRE row (render_*_row, a Vec per
    // read) and indexed the beam's pixel; the new one samples exactly one
    // pel through the shared per-pixel/per-cell helpers. The row renderers
    // are still production code (catch_up/render_scanline loop over them),
    // so this oracle IS the old computation, recomputed verbatim, and the
    // sweep asserts bit identity across the four raster modes, beams in
    // every scanline region (active, horizontal blank, vertical blank,
    // retrace), all four mux selects, split screen, pel pan, and
    // non-trivial VRAM/text content.
    fn oracle_mux_bits(vga: &Vga, beam: u64) -> u8 {
        if vga.is_cga_personality() || !beam_display_enable(&vga.crtc, beam) {
            return 0;
        }
        let line = beam_line(&vga.crtc, beam);
        let dot = beam_dot(&vga.crtc, beam) as usize;
        let color = match vga.mode {
            VideoMode::Mode13h | VideoMode::ModeX => vga.render_256color_row(line)[dot],
            VideoMode::Text => vga.render_text_row(line)[dot],
            VideoMode::Planar => vga.render_active_row(line)[dot],
            VideoMode::Cga | VideoMode::Hercules => 0,
        };
        let pair = match (vga.attr.plane_enable >> 4) & 0x03 {
            0x00 => (((color >> 2) & 1) << 1) | (color & 1),
            0x01 => (color >> 4) & 0x03,
            0x02 => (((color >> 3) & 1) << 1) | ((color >> 1) & 1),
            _ => (color >> 6) & 0x03,
        };
        pair << 4
    }

    // Boxed: two additional by-value Vga fixtures overflowed the debug
    // test-thread stack (each Vga carries VRAM inline).
    let mut fixtures: Vec<(&str, Box<Vga>)> = Vec::new();

    // A text fixture with varied char/attr content (blink and
    // 512-glyph-relevant attribute bits included via the *7 stride) and a
    // cursor parked mid-screen so the cursor-swap path is sampled.
    fn text_fixture() -> Box<Vga> {
        let mut text = Box::new(Vga::default());
        for i in 0..(80 * 25) {
            text.write_u8(i * 2, (i % 251) as u8).unwrap();
            text.write_u8(i * 2 + 1, (i * 7 % 256) as u8).unwrap();
        }
        text.set_cursor_offset(80 * 12 + 33);
        text
    }

    // Text mode (the default): 9-dot cells, no pan.
    fixtures.push(("text", text_fixture()));

    // 9-dot text with a nonzero pel pan: kills a pan-dropped-from-inversion
    // mutation in text_pixel that the pan-free text fixture cannot see
    // (spec-review finding; that mutation survived the whole video suite).
    let mut text_pan = text_fixture();
    text_pan.attr.pixel_pan = 3;
    fixtures.push(("text 9-dot+pan3", text_pan));

    // Mismatched geometry (spec-review finding): 8-dot Sequencer clocking
    // under the unchanged 720-dot mode-3 CRTC, with pan 5. The renderer's
    // cell loop then covers only dots 0..(text_columns+1)*8 - 5 = 643; dots
    // 643..719 stay at the row Vec's initialized 0, and the sampler's
    // placement-domain guard (dc > text_columns -> 0) must agree instead of
    // resolving an aperture-wrapped cell.
    let mut text_8dot = text_fixture();
    text_8dot.seq.clocking_mode |= 0x01; // 8-dot cells, CRTC not retuned
    text_8dot.attr.pixel_pan = 5;
    fixtures.push(("text 8-dot+pan5 on the 720-dot CRTC", text_8dot));

    // Chained mode 13h with a line-compare split mid-screen.
    let mut m13 = Box::new(Vga::default());
    m13.set_mode13h();
    m13.crtc.line_compare = 100;
    fixtures.push(("mode13h+split", m13));

    // Planar 16-color (mode 0Dh) with a nonzero fine pel pan.
    let mut planar = Box::new(Vga::default());
    planar.set_mode_0dh();
    planar.attr.pixel_pan = 2;
    fixtures.push(("planar+pan", planar));

    // Unchained mode X (chain-4 cleared from 13h).
    let mut modex = Box::new(Vga::default());
    modex.set_mode13h();
    modex.write_port(0x3C4, 0x04);
    modex.write_port(0x3C5, 0x06);
    assert_eq!(modex.active_mode(), VideoMode::ModeX);
    fixtures.push(("modeX", modex));

    for (name, mut vga) in fixtures {
        // Non-trivial plane content for the graphics paths (the text path
        // reads text_memory + font, filled above).
        for (i, b) in vga.vram.iter_mut().enumerate() {
            *b = ((i as u32).wrapping_mul(2654435761) >> 24) as u8;
        }
        if vga.active_mode() == VideoMode::Mode13h {
            // Fill linear the same for HLE fast path test content.
            for (i, b) in vga.mode13_linear.iter_mut().enumerate() {
                *b = ((i as u32).wrapping_mul(2654435761) >> 24) as u8;
            }
        }
        let htotal = htotal_dots(&vga.crtc);
        // Active lines swept at EVERY dot (a sparse dot grid provably lacks
        // teeth: a px-derivation mutation in the text sampler survived one),
        // spanning the top, both sides of the middle/split, and the last
        // active line, so every per-cell pel position and both split
        // regions are compared.
        let full_lines = [
            0,
            1,
            vga.crtc.vdisp_end / 2,
            vga.crtc.vdisp_end / 2 + 1,
            vga.crtc.vdisp_end - 1,
        ];
        // Out-of-display probes: the mux must read 0 through both paths.
        let blank_probes = [
            (vga.crtc.vdisp_end, 0),      // vertical blank
            (vga.crtc.vretrace_start, 5), // retrace
            (vga.crtc.vtotal - 1, 7),     // bottom of the frame
            (0, vga.crtc.hdisp_end),      // horizontal blank
            (1, htotal as u32 - 1),       // end of scanline
        ];
        for mux in [0x00u8, 0x10, 0x20, 0x30] {
            vga.attr.plane_enable = 0x0F | mux;
            for line in full_lines {
                for dot in 0..vga.crtc.hdisp_end {
                    let beam = u64::from(line) * htotal + u64::from(dot);
                    assert_eq!(
                        vga.video_status_mux_bits(beam),
                        oracle_mux_bits(&vga, beam),
                        "{name}: single-pixel mux sample diverged from the \
                             full row render at line {line} dot {dot} mux {mux:#04X}"
                    );
                }
            }
            for (line, dot) in blank_probes {
                let beam = u64::from(line) * htotal + u64::from(dot);
                assert_eq!(
                    vga.video_status_mux_bits(beam),
                    oracle_mux_bits(&vga, beam),
                    "{name}: blank/retrace probe diverged at line {line} \
                         dot {dot} mux {mux:#04X}"
                );
            }
        }
    }
}

#[test]
fn mode_set_resets_beam_and_reports_planar_geometry() {
    let mut vga = Vga::default();
    vga.advance(12345); // dirty the beam in text mode
    vga.set_mode_0dh();
    assert_eq!(vga.beam_dots(), 0);
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.frame_dots(), CrtcTiming::mode_0dh().frame_dots());
}

#[test]
fn text_mode_defaults_to_blank_80x25_screen() {
    let text = Vga::default();
    let frame = text.frame();

    assert_eq!(frame.columns, 80);
    assert_eq!(frame.rows, 25);
    assert_eq!(frame.cells.len(), 2000);
    assert!(frame.line_string(0).is_empty());
    assert_eq!((text.cursor_start, text.cursor_end), (0x0E, 0x0F));
}

#[test]
fn text_memory_write_updates_frame_cell() {
    let mut text = Vga::default();
    text.write_u8(0, b'V').unwrap();
    text.write_u8(1, 0x0a).unwrap();

    let frame = text.frame();
    assert_eq!(frame.cells[0].character, b'V');
    assert_eq!(frame.cells[0].attribute, 0x0a);
    assert_eq!(frame.line_string(0), "V");
}

#[test]
fn mode13h_chain4_write_routes_byte_n_to_plane_n_mod_4() {
    let mut video = Vga::default();
    video.set_mode13h();
    // Chain-4 writes byte 123 (0x7B) to plane 123 & 3 = 3 at plane offset
    // 123 >> 2 = 30, bypassing the planar datapath. The other planes at that
    // plane offset stay clear.
    video.cpu_write_chain4(123, 0x2a);
    assert_eq!(
        video.plane_byte(3, 30),
        0x2a,
        "byte 123 lands in plane 3 @ 30"
    );
    for plane in 0..VGA_PLANES {
        if plane == 3 {
            continue;
        }
        assert_eq!(
            video.plane_byte(plane, 30),
            0,
            "plane {plane} at offset 30 is untouched"
        );
    }
    // The chain-4 read selects the same plane/offset, so it round-trips.
    assert_eq!(video.cpu_read_chain4(123), 0x2a);
    // The shared 256-color scanout reads plane 123 & 3 = 3 at plane offset
    // 123 >> 2 = 30 as pixel 123, so the raster carries the written byte.
    assert_eq!(
        video.render_256color_row(0)[123],
        0x2a,
        "pixel 123 scans out the chain-4 written byte"
    );
}

#[test]
fn mode13h_linear_scanout_is_limited_to_the_stock_layout() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    assert!(video.canonical_mode13_linear_scanout());

    video.attr.pixel_pan = 1;
    assert!(!video.canonical_mode13_linear_scanout());
    video.attr.pixel_pan = 0;

    video.crtc.preset_row_scan = 1;
    assert!(!video.canonical_mode13_linear_scanout());
    video.crtc.preset_row_scan = 0;

    video.crtc.line_compare = 100;
    assert!(!video.canonical_mode13_linear_scanout());
    video.crtc.line_compare = CrtcTiming::mode13h().line_compare;

    video.crtc.mode_control ^= 0x40;
    assert!(!video.canonical_mode13_linear_scanout());
}

#[test]
fn mode13h_noncanonical_scanout_reads_authoritative_planar_vram() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    video.cpu_write_chain4(1, 0x22);
    video.mode13_linear[1] = 0xee;
    video.attr.pixel_pan = 1;

    assert_eq!(video.render_256color_row(0)[0], 0x22);
}

#[test]
fn planar_write_invalidates_mode13h_linear_scanout() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    video.cpu_write(0, 0x5a);
    video.mode13_linear[0] = 0xee;

    assert!(!video.canonical_mode13_linear_scanout());
    assert_eq!(video.render_256color_row(0)[0], video.vram[0]);
}

#[test]
fn mode13h_no_clear_transition_keeps_planar_vram_authoritative() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    video.write_port(0x3C4, 0x04);
    video.write_port(0x3C5, 0x06);
    video.cpu_write(0, 0x5a);

    video.set_mode13h_with_clear(false);
    video.mode13_linear[0] = 0xee;

    assert!(!video.canonical_mode13_linear_scanout());
    assert_eq!(video.render_256color_row(0)[0], video.vram[0]);
}

#[test]
fn crtc_cursor_ports_track_offset() {
    let mut text = Vga::default();
    assert!(text.write_port(0x03d4, 0x0e));
    assert!(text.write_port(0x03d5, 0x12));
    assert!(text.write_port(0x03d4, 0x0f));
    assert!(text.write_port(0x03d5, 0x34));

    assert_eq!(text.cursor_offset, 0x1234);
    assert_eq!(text.read_port(0x03d5), Some(0x34));
}

#[test]
fn cursor_shape_registers_round_trip() {
    let mut vga = Vga::default();
    assert!(vga.write_port(0x3D4, 0x0A));
    assert!(vga.write_port(0x3D5, 0x0E)); // start scanline 14
    assert!(vga.write_port(0x3D4, 0x0B));
    assert!(vga.write_port(0x3D5, 0x0F)); // end scanline 15

    assert_eq!(vga.cursor_start, 0x0E);
    assert_eq!(vga.cursor_end, 0x0F);
    // Readback through the CRTC data port.
    assert!(vga.write_port(0x3D4, 0x0A));
    assert_eq!(vga.read_port(0x3D5), Some(0x0E));
    assert!(vga.write_port(0x3D4, 0x0B));
    assert_eq!(vga.read_port(0x3D5), Some(0x0F));
}

#[test]
fn set_mode_selects_mode13h() {
    let mut video = Vga::default();
    assert_eq!(video.active_mode(), VideoMode::Text);
    assert!(video.set_mode(0x13));
    assert_eq!(video.active_mode(), VideoMode::Mode13h);
}

#[test]
fn ega_modes_load_the_matching_bios_dac_palette() {
    // Mode 13h keeps the 256-color palette3: brown sits at index 6 directly
    // and the gray ramp at 0x10..0x1F. (This is the value an EGA mode used
    // to wrongly inherit, turning brown attributes gray.)
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x13));
    assert_eq!(vga.dac.entry(0x06), [0x2a, 0x15, 0x00]);
    assert_eq!(vga.dac.entry(0x14), [0x0e, 0x0e, 0x0e]); // gray ramp, not brown

    // Mode 10h loads palette2, the EGA 64-color decode. Its default
    // attribute map sends color 6 -> 0x14 and the bright eight -> 0x38..0x3F,
    // so those entries must hold real colors, not the gray ramp.
    vga.set_mode(0x10);
    assert_eq!(vga.dac.entry(0x14), [0x2a, 0x15, 0x00], "0x10 brown");
    assert_eq!(vga.dac.entry(0x38), [0x15, 0x15, 0x15], "0x10 dark gray");
    assert_eq!(vga.dac.entry(0x3f), [0x3f, 0x3f, 0x3f], "0x10 white");

    // Mode 0Dh (CGA 320x200, the Monkey Island mode) loads palette1: brown at
    // 6 and the bright eight at 0x10..0x17 (this mode's attribute targets).
    vga.set_mode(0x0D);
    assert_eq!(vga.dac.entry(0x06), [0x2a, 0x15, 0x00], "0x0D brown");
    assert_eq!(vga.dac.entry(0x10), [0x15, 0x15, 0x15], "0x0D bright black");
    assert_eq!(vga.dac.entry(0x17), [0x3f, 0x3f, 0x3f], "0x0D bright white");
}

#[test]
fn vga_text_mode3_resolves_remapped_colors_through_palette2() {
    // Mode 03h drives text colors through the EGA attribute remap (color 6 ->
    // 0x14, bright eight -> 0x38..0x3F, white -> 0x3F), so the DAC must be
    // palette2 or those land on the 256-color ramps: white came out pink,
    // brown gray, dark gray blue. Resolve the final RGB the remap produces.
    let mut vga = Vga::default();
    vga.set_text_mode();
    assert_eq!(vga.dac.rgb888(0x3f), (0xff, 0xff, 0xff), "bright white");
    assert_eq!(vga.dac.rgb888(0x14), (0xaa, 0x55, 0x00), "brown");
    assert_eq!(vga.dac.rgb888(0x38), (0x55, 0x55, 0x55), "dark gray");

    // CGA text (modes 00h-02h) uses direct RGBI color numbers, which must stay
    // the standard 16 at entries 0..15 (palette3): white is index 15, not 0x3F.
    assert!(vga.set_cga_text_mode(0x01));
    assert_eq!(
        vga.dac.rgb888(0x0f),
        (0xff, 0xff, 0xff),
        "CGA white at index 15"
    );
    assert_eq!(
        vga.dac.rgb888(0x06),
        (0xaa, 0x55, 0x00),
        "CGA brown at index 6"
    );
}

#[test]
fn mode13h_mode_set_installs_chain4_and_a000_graphics_defaults() {
    let mut video = Vga::default();
    assert!(video.set_mode(0x13));

    assert_eq!(video.seq.map_mask, 0x0F);
    assert_eq!(video.seq.memory_mode, 0x0E);
    assert_eq!(video.gc.bit_mask, 0xFF);
    assert_eq!(video.gc.color_dont_care, 0x0F);
    let ap = video.gfx_aperture();
    assert_eq!((ap.base, ap.length), (0x000A_0000, 0x0001_0000));
    assert!(ap.graphics);
}

#[test]
fn dac_write_then_read_round_trips() {
    let mut video = Vga::default();
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
    let mut video = Vga::default();
    video.write_port(0x03c8, 1);
    video.write_port(0x03c9, 63); // R
    video.write_port(0x03c9, 0); // G
    video.write_port(0x03c9, 0); // B
    assert_eq!(video.palette_argb()[1], 0x00FF_0000);
}

#[test]
fn mode_0dh_raster_height_equals_vtotal_not_doubled() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // One raster row per scanline: vtotal (449), not vtotal * scan_factor.
    assert_eq!(vga.raster_height(), 449);
}

#[test]
fn double_scan_holds_each_source_row_for_two_scanlines() {
    let mut vga = Vga::default();
    vga.set_mode_0dh(); // doubled mode
    // Source row 0 has pixel 0 set in plane 0; source row 1 (byte pitch
    // offset*2 = 40) is clear.
    vga.vram[0] = 0x80;
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    let r0 = vga.render_active_row(0);
    let r1 = vga.render_active_row(1);
    let r2 = vga.render_active_row(2);
    assert_eq!(r0, r1, "scanlines 0 and 1 read the same source row");
    assert_ne!(r0, r2, "scanline 2 reads the next source row");
    assert_eq!(r0[0], 1, "source row 0 pixel 0 is attribute index 1");
    assert_eq!(r2[0], 0, "source row 1 pixel 0 is attribute index 0");
}

#[test]
fn preset_row_scan_offsets_graphics_source_rows() {
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x10));
    let pitch = (vga.crtc.offset * 2) as usize;
    vga.vram[0] = 0x80; // row 0, plane 0 -> index 1
    vga.vram[VGA_PLANE_SIZE + pitch] = 0x80; // row 1, plane 1 -> index 2

    assert_eq!(vga.render_active_row(0)[0], 0x01);
    vga.crtc.preset_row_scan = 0x01;
    assert_eq!(vga.render_active_row(0)[0], 0x02);
    vga.crtc.line_compare = 0;
    assert_eq!(
        vga.render_active_row(3)[0],
        0x01,
        "preset row resets below the line-compare split"
    );
    vga.crtc.line_compare = u32::MAX;
    vga.crtc.preset_row_scan = 0x20;
    vga.vram[0] = 0x80; // row 0, byte 0, plane 0 -> index 1
    vga.vram[VGA_PLANE_SIZE] = 0x00;
    vga.vram[VGA_PLANE_SIZE + 1] = 0x80; // row 0, byte 1, plane 1 -> index 2
    assert_eq!(vga.render_active_row(0)[0], 0x02);
    vga.crtc.line_compare = 0;
    vga.attr.mode_control = 0x20;
    assert_eq!(
        vga.render_active_row(3)[0],
        0x01,
        "byte pan resets below the split when AC 10h bit 5 requests it"
    );

    let mut mode13h = Vga::default();
    mode13h.set_mode13h();
    // Keep the derived linear cache in sync with planar VRAM.
    mode13h.cpu_write_chain4(0, 0x11);
    mode13h.cpu_write_chain4(320, 0x22);
    mode13h.crtc.preset_row_scan = 0x02;
    assert_eq!(mode13h.render_256color_row(0)[0], 0x22);
    mode13h.crtc.preset_row_scan = 0x20;
    mode13h.cpu_write_chain4(4, 0x33);
    assert_eq!(mode13h.render_256color_row(0)[0], 0x33);
}

#[test]
fn set_mode_selects_geometry_for_each_graphics_number() {
    let mut vga = Vga::default();

    assert!(vga.set_mode(0x0E));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.raster_height(), 449); // 0Eh vtotal 449; 200 rows double-scanned to 400 active
    assert_eq!(vga.active_mode(), VideoMode::Planar);

    assert!(vga.set_mode(0x10));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.raster_height(), 449); // 640x350, vtotal 449

    assert!(vga.set_mode(0x12));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.raster_height(), 525); // 640x480, vtotal 525

    assert!(vga.set_mode(0x0D));
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.raster_height(), 449);

    assert!(vga.set_mode(0x13));
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.raster_height(), 449);
    assert_eq!(vga.active_mode(), VideoMode::Mode13h);

    assert!(!vga.set_mode(0x99)); // unknown number leaves a false result
}

#[test]
fn bios_mode_sets_seed_vgabios_crtc_readback() {
    fn assert_regs(vga: &Vga, mode: u8, expected: &[(u8, u8)]) {
        for &(index, value) in expected {
            assert_eq!(
                vga.crtc_register_latch(index),
                value,
                "mode {mode:02X} CRTC {index:02X}"
            );
        }
    }

    let mut vga = Vga::default();
    assert_regs(
        &vga,
        0x03,
        &[
            (0x04, 0x55),
            (0x05, 0x81),
            (0x09, 0x4F),
            (0x13, 0x28),
            (0x14, 0x1F),
            (0x15, 0x96),
            (0x17, 0xA3),
        ],
    );

    for (mode, expected) in [
        (
            0x0D,
            &[
                (0x00, 0x2D),
                (0x01, 0x27),
                (0x04, 0x2B),
                (0x09, 0xC0),
                (0x13, 0x14),
                (0x15, 0x96),
                (0x16, 0xB9),
            ][..],
        ),
        (0x0E, &[(0x00, 0x5F), (0x01, 0x4F), (0x13, 0x28)][..]),
        (0x0F, &[(0x09, 0x40), (0x14, 0x0F), (0x15, 0x63)][..]),
        (0x10, &[(0x09, 0x40), (0x14, 0x0F), (0x15, 0x63)][..]),
        (
            0x11,
            &[(0x06, 0x0B), (0x07, 0x3E), (0x10, 0xEA), (0x16, 0x04)][..],
        ),
        (
            0x12,
            &[(0x06, 0x0B), (0x07, 0x3E), (0x10, 0xEA), (0x16, 0x04)][..],
        ),
    ] {
        assert!(vga.set_mode(mode));
        assert_regs(&vga, mode, expected);
    }

    vga.set_mode13h();
    assert_regs(
        &vga,
        0x13,
        &[(0x09, 0x41), (0x13, 0x28), (0x14, 0x40), (0x17, 0xA3)],
    );
}

#[test]
fn bios_mode_sets_seed_vgabios_sequencer_readback() {
    fn seq_reg(vga: &mut Vga, index: u8) -> u8 {
        assert!(vga.write_port(0x3C4, index));
        vga.read_port(0x3C5).unwrap()
    }

    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.set_text_mode();
    assert_eq!(seq_reg(&mut vga, 1), 0x00);
    assert_eq!(seq_reg(&mut vga, 2), 0x03);
    assert_eq!(seq_reg(&mut vga, 4), 0x02);

    for (mode, clocking_mode, memory_mode) in [
        (0x0D, 0x09, 0x06),
        (0x0E, 0x01, 0x06),
        (0x0F, 0x01, 0x06),
        (0x10, 0x01, 0x06),
        (0x11, 0x01, 0x06),
        (0x12, 0x01, 0x06),
    ] {
        assert!(vga.set_mode(mode));
        assert_eq!(seq_reg(&mut vga, 1), clocking_mode, "mode {mode:02X}");
        assert_eq!(seq_reg(&mut vga, 2), 0x0F, "mode {mode:02X}");
        assert_eq!(seq_reg(&mut vga, 3), 0x00, "mode {mode:02X}");
        assert_eq!(seq_reg(&mut vga, 4), memory_mode, "mode {mode:02X}");
    }

    vga.set_mode13h();
    assert_eq!(seq_reg(&mut vga, 1), 0x01);
    assert_eq!(seq_reg(&mut vga, 2), 0x0F);
    assert_eq!(seq_reg(&mut vga, 4), 0x0E);
}

#[test]
fn bios_mode_sets_seed_vgabios_graphics_controller_readback() {
    fn gc_reg(vga: &mut Vga, index: u8) -> u8 {
        assert!(vga.write_port(0x3CE, index));
        vga.read_port(0x3CF).unwrap()
    }

    let mut vga = Vga::default();
    assert_eq!(gc_reg(&mut vga, 5), 0x10);
    assert_eq!(gc_reg(&mut vga, 6), 0x0E);
    assert_eq!(gc_reg(&mut vga, 7), 0x0F);
    assert_eq!(gc_reg(&mut vga, 8), 0xFF);

    for mode in 0x0D..=0x12 {
        assert!(vga.set_mode(mode));
        assert_eq!(gc_reg(&mut vga, 5), 0x00, "mode {mode:02X}");
        assert_eq!(gc_reg(&mut vga, 6), 0x05, "mode {mode:02X}");
        assert_eq!(gc_reg(&mut vga, 7), 0x0F, "mode {mode:02X}");
        assert_eq!(gc_reg(&mut vga, 8), 0xFF, "mode {mode:02X}");
    }

    vga.set_mode13h();
    assert_eq!(gc_reg(&mut vga, 5), 0x40);
    assert_eq!(gc_reg(&mut vga, 6), 0x05);
    assert_eq!(gc_reg(&mut vga, 7), 0x0F);
    assert_eq!(gc_reg(&mut vga, 8), 0xFF);

    vga.set_text_mode();
    assert_eq!(gc_reg(&mut vga, 5), 0x10);
    assert_eq!(gc_reg(&mut vga, 6), 0x0E);
}

#[test]
fn bios_graphics_modes_seed_vgabios_attribute_controller_readback() {
    fn attr_reg(vga: &mut Vga, index: u8) -> u8 {
        vga.read_status1();
        assert!(vga.write_port(0x3C0, 0x20 | (index & 0x1F)));
        vga.read_port(0x3C1).unwrap()
    }

    let mut vga = Vga::default();
    assert_eq!(attr_reg(&mut vga, 0x06), 0x14, "mode 03H AC06");
    assert_eq!(attr_reg(&mut vga, 0x08), 0x38, "mode 03H AC08");
    assert_eq!(attr_reg(&mut vga, 0x0F), 0x3F, "mode 03H AC0F");
    assert_eq!(attr_reg(&mut vga, 0x10), 0x0C, "mode 03H AC10");
    assert_eq!(attr_reg(&mut vga, 0x12), 0x0F, "mode 03H AC12");
    assert_eq!(attr_reg(&mut vga, 0x13), 0x08, "mode 03H AC13");
    assert_eq!(attr_reg(&mut vga, 0x14), 0x00, "mode 03H AC14");

    for (mode, expected) in [
        (0x0D, &[(0x08, 0x10), (0x10, 0x01), (0x12, 0x0F)][..]),
        (0x0E, &[(0x08, 0x10), (0x10, 0x01), (0x12, 0x0F)][..]),
        (
            0x0F,
            &[(0x01, 0x08), (0x04, 0x18), (0x10, 0x01), (0x12, 0x01)][..],
        ),
        (
            0x10,
            &[(0x06, 0x14), (0x08, 0x38), (0x10, 0x01), (0x12, 0x0F)][..],
        ),
        (
            0x11,
            &[(0x01, 0x3F), (0x0F, 0x3F), (0x10, 0x01), (0x12, 0x0F)][..],
        ),
        (
            0x12,
            &[(0x06, 0x14), (0x08, 0x38), (0x10, 0x01), (0x12, 0x0F)][..],
        ),
    ] {
        assert!(vga.set_mode(mode));
        for &(index, value) in expected {
            assert_eq!(
                attr_reg(&mut vga, index),
                value,
                "mode {mode:02X} AC{index:02X}"
            );
        }
        assert_eq!(attr_reg(&mut vga, 0x13), 0x00, "mode {mode:02X} AC13");
        assert_eq!(attr_reg(&mut vga, 0x14), 0x00, "mode {mode:02X} AC14");
    }

    vga.set_mode13h();
    assert_eq!(attr_reg(&mut vga, 0x0F), 0x0F);
    assert_eq!(attr_reg(&mut vga, 0x10), 0x41);
    assert_eq!(attr_reg(&mut vga, 0x12), 0x0F);
    assert_eq!(attr_reg(&mut vga, 0x14), 0x00);
}

#[test]
fn planar_mode_set_installs_writeable_graphics_defaults() {
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x10));

    vga.cpu_write(0, 0xA5);
    for plane in 0..VGA_PLANES {
        vga.gc.read_map = plane as u8;
        assert_eq!(vga.cpu_read(0), 0xA5);
    }
}

#[test]
fn word_mode_render_rotates_the_address() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.crtc.mode_control = 0xA3; // force word mode (bit 6 = 0), 16-bit wrap
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    // The second character (byte_col 1) has counter ma = 1. Word mode maps ma = 1
    // to plane offset 2 ((1 << 1) | 0); byte mode would read offset 1. Mark only
    // offset 2, so a correct word-mode read shows index 1 at pixel 8.
    vga.vram[2] = 0x80; // bit 7 -> the first pixel of that character
    let row = vga.render_active_row(0);
    assert_eq!(
        row[8], 1,
        "word mode reads plane offset 2 for the 2nd character"
    );
    assert_eq!(row[0], 0, "char 0 (offset 0) is clear");
}

#[test]
fn byte_mode_wrap_scanout_equals_top_of_vram() {
    let mut vga = Vga::default();
    vga.set_mode_0dh(); // byte mode (CR17 = 0xE3)
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    // Distinct mark at the very top of VRAM (plane 0 offset 0): pixels 0..7 = index 1.
    vga.vram[0] = 0xFF;
    // Reference row from start_address 0: its first 8 pixels come from offset 0.
    vga.crtc.start_address = 0;
    let top = vga.render_active_row(0);
    assert_eq!(
        &top[0..8],
        &[1u8; 8],
        "top-of-VRAM byte renders 8 pixels of index 1"
    );
    // Start 8 bytes before the 64 KB wrap: byte_col 0..7 read 0xFFF8..0xFFFF (clear),
    // byte_col 8 wraps to offset 0 (the marked byte). So pixels 64..71 must equal
    // the top-of-VRAM pixels, not tear.
    vga.crtc.start_address = 0xFFF8;
    let wrapped = vga.render_active_row(0);
    assert_eq!(
        &wrapped[0..64],
        &[0u8; 64],
        "pre-wrap pixels read the cleared tail"
    );
    assert_eq!(
        &wrapped[64..72],
        &top[0..8],
        "wrapped scanout pixels equal the top-of-VRAM pixels at the seam"
    );
}

#[test]
fn line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
    // A distinct byte per plane-0 offset so each source row is recognizable.
    fn pattern(off: usize) -> u8 {
        ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
    }
    // Reference renderer: no split (line compare stays 0x3FF), configurable scroll
    // and pel-pan, rendering one row.
    fn reference(s: u32, pan: u8, row: u32) -> Vec<u8> {
        let mut r = Vga::default();
        r.set_mode(0x12);
        r.attr.palette = core::array::from_fn(|i| i as u8);
        for off in 0..VGA_PLANE_SIZE {
            r.vram[off] = pattern(off);
        }
        r.crtc.start_address = s;
        r.attr.pixel_pan = pan;
        r.render_active_row(row)
    }

    let mut vga = Vga::default();
    vga.set_mode(0x12); // 640x480, not double-scanned, offset 40 (byte pitch 80)
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    for off in 0..VGA_PLANE_SIZE {
        vga.vram[off] = pattern(off);
    }
    let start = 0x1000u32;
    let split = 300u32;
    vga.crtc.start_address = start;
    vga.crtc.line_compare = split;
    vga.attr.pixel_pan = 3;
    vga.attr.mode_control = 0x20; // bit 5: pel-pan up to line compare only

    // Top row 200 (<= split): scrolled by `start`, panned by 3.
    assert_eq!(
        vga.render_active_row(200),
        reference(start, 3, 200),
        "top region renders scrolled and pel-panned"
    );
    // First split scanline (split+1): source row 0 from offset 0, pel-pan forced 0.
    assert_eq!(
        vga.render_active_row(split + 1),
        reference(0, 0, 0),
        "first split line renders source row 0 from offset 0 with pel-pan forced to 0"
    );
    // Split region row k: source row k from offset 0, pel-pan forced 0.
    assert_eq!(
        vga.render_active_row(split + 11),
        reference(0, 0, 10),
        "split region row k renders source row k from offset 0"
    );
}

#[test]
fn line_compare_split_starts_on_the_line_after_the_match() {
    let split = 100u32;
    let mut vga = Vga::default();
    vga.set_mode(0x12);
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.vram[0] = 0xFF; // offset 0 marked: index 1 across pixels 0..7
    // Scroll the top region past the marked byte so the top reads cleared VRAM.
    vga.crtc.start_address = 0x4000;
    vga.crtc.line_compare = split;
    // The matching scanline is the last top line: reads start_address (clear) -> 0.
    assert_eq!(
        vga.render_active_row(split)[0],
        0,
        "scanline == line_compare is still the top region"
    );
    // The next scanline is the first split line: reads offset 0 (marked) -> 1.
    assert_eq!(
        vga.render_active_row(split + 1)[0],
        1,
        "scanline line_compare+1 is the first split line, from offset 0"
    );
}

#[test]
fn ega_line_compare_split_starts_two_scanlines_later() {
    let split = 100u32;
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x10));
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.vram[0] = 0xFF; // offset 0 marked: index 1 across pixels 0..7
    vga.crtc.start_address = 0x4000;
    vga.crtc.line_compare = split;

    assert_eq!(vga.render_active_row(split + 1)[0], 0);
    assert_eq!(vga.render_active_row(split + 2)[0], 0);
    assert_eq!(vga.render_active_row(split + 3)[0], 1);
}

#[test]
fn ega_line_compare_compares_against_the_scan_counter_line_in_a_doubled_mode() {
    let mut vga = Vga::default();
    vga.set_mode_0dh(); // double-scanned: 400 active scanlines, source rows 0..200
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.vram[0] = 0xFF; // offset 0 marked -> index 1
    // Split at scan-counter line 320. The source row counter only reaches ~200, so a
    // split here can only match if the comparison is in scan-counter units.
    let split = 320u32;
    vga.crtc.start_address = 0x4000; // top region reads cleared VRAM
    vga.crtc.line_compare = split;
    assert_eq!(
        vga.render_active_row(320)[0],
        0,
        "scanline 320 == line_compare is the last top line"
    );
    assert_eq!(
        vga.render_active_row(321)[0],
        0,
        "EGA split is delayed two scanlines after the VGA threshold"
    );
    assert_eq!(
        vga.render_active_row(322)[0],
        0,
        "EGA split is still delayed on the second scanline after the match"
    );
    // Scanlines 323 and 324 are the first two split scanlines: the same doubled
    // source row 0, read from offset 0.
    assert_eq!(
        vga.render_active_row(323)[0],
        1,
        "first split scanline, offset 0"
    );
    assert_eq!(
        vga.render_active_row(324)[0],
        1,
        "second scanline holds the same doubled source row 0"
    );
}

#[test]
fn pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
    // Render the first split-region row (offset 0) with a non-uniform byte so a
    // pel-pan shift is visible. `mode_control` carries Attribute index 10h, `pan`
    // the pel-pan value.
    fn render(mode_control: u8, pan: u8) -> Vec<u8> {
        let mut vga = Vga::default();
        vga.set_mode(0x12);
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0b0101_0101; // alternating pixels in source row 0
        vga.crtc.line_compare = 100;
        vga.attr.pixel_pan = pan;
        vga.attr.mode_control = mode_control;
        vga.render_active_row(101) // first split line: source row 0, offset 0
    }
    // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
    assert_eq!(
        render(0x20, 1),
        render(0x20, 0),
        "Attribute 10h bit 5 set forces split-region pel-pan to 0"
    );
    // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
    assert_ne!(
        render(0x00, 1),
        render(0x00, 0),
        "Attribute 10h bit 5 clear pans the split region"
    );
}

#[test]
fn wide_mode_assembles_four_bit_index_across_the_full_line() {
    let mut vga = Vga::default();
    vga.set_mode(0x12); // 640 wide, not doubled
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    // Column 639 is byte 79, bit 0. Set that bit in all four planes so the
    // assembled index is 0b1111 = 15.
    for plane in 0..VGA_PLANES {
        vga.vram[plane * VGA_PLANE_SIZE + 79] = 0x01;
    }
    let row = vga.render_active_row(0);
    assert_eq!(row.len(), 640);
    assert_eq!(row[639], 15, "column 639 reads bit 0 of all four planes");
    assert_eq!(row[0], 0, "column 0 is clear");
}

#[test]
fn guest_crtc_bang_retunes_mode_x_to_320x240() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // enter mode X, 320x200 base
    assert_eq!(vga.raster_height(), 449);
    // Abrash's 320x240 vertical timing (Black Book Listing 47.1), index then data.
    for (idx, val) in [
        (0x06u8, 0x0Du8), // vertical total
        (0x07, 0x3E),     // overflow (high bits)
        (0x09, 0x41),     // max scan line: 2 scanlines per row
        (0x10, 0xEA),     // vretrace start
        (0x11, 0xAC),     // vretrace end + protect
        (0x12, 0xDF),     // vertical display end
        (0x15, 0xE7),     // vblank start
        (0x16, 0x06),     // vblank end
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }
    assert_eq!(vga.crtc.vtotal, 527, "527 total scanlines");
    assert_eq!(vga.crtc.vdisp_end, 480, "480 active scanlines");
    assert_eq!(vga.crtc.max_scan, 1);
    assert!(
        vga.crtc.double_scan,
        "double-scanned: 240 source rows over 480 lines"
    );
    assert_eq!(vga.raster_height(), 527);
}

#[test]
fn planar_vertical_crtc_writes_recompute_ega_timing() {
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x10));
    assert_eq!(vga.crtc.vdisp_end, 350);
    assert_eq!(vga.raster_height(), 449);

    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x00);
    for (idx, val) in [
        (0x06u8, 0x0Du8),
        (0x07, 0x3E),
        (0x09, 0x41),
        (0x10, 0xEA),
        (0x11, 0xAC),
        (0x12, 0xDF),
        (0x15, 0xE7),
        (0x16, 0x06),
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
        vga.write_port(0x3D4, idx);
        assert_eq!(vga.read_port(0x3D5), Some(val));
    }

    assert_eq!(vga.crtc.vtotal, 527);
    assert_eq!(vga.crtc.vdisp_end, 480);
    assert_eq!(vga.crtc.vblank_start, 487);
    assert_eq!(vga.crtc.vretrace_start, 490);
    assert_eq!(vga.crtc.max_scan, 1);
    assert!(vga.crtc.double_scan);
    assert_eq!(vga.raster_height(), 527);
}

#[test]
fn mode13h_vertical_crtc_writes_recompute_timing() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    assert_eq!(vga.raster_height(), 449);

    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x00);
    for (idx, val) in [
        (0x06u8, 0x0Du8),
        (0x07, 0x3E),
        (0x09, 0x41),
        (0x10, 0xEA),
        (0x11, 0xAC),
        (0x12, 0xDF),
        (0x15, 0xE7),
        (0x16, 0x06),
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }

    assert_eq!(vga.crtc.vtotal, 527);
    assert_eq!(vga.crtc.vdisp_end, 480);
    assert_eq!(vga.crtc.max_scan, 1);
    assert!(vga.crtc.double_scan);
    assert_eq!(vga.raster_height(), 527);
}

#[test]
fn clearing_chain4_in_mode13h_enters_and_leaves_mode_x() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    assert_eq!(vga.active_mode(), VideoMode::Mode13h);
    // Sequencer Memory Mode (04h) written with chain-4 (bit 3) cleared enters
    // unchained 256-color (mode X) from chained mode 13h.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert_eq!(vga.active_mode(), VideoMode::ModeX);
    // The unchained 320x200 base geometry: 320 wide, vtotal 449, offset 40.
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.raster_height(), 449);
    assert_eq!(vga.crtc.offset, 40);
    // Writing 04h with chain-4 set again reverts to chained mode 13h.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x0E);
    assert_eq!(vga.active_mode(), VideoMode::Mode13h);
}

#[test]
fn mode_x_scanout_is_column_interleaved_8bit_direct() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // mode X, 320x200 base
    // Distinct full bytes in planes 0..3 at plane offset 0. 0x40 also proves the
    // byte is not masked to 6 bits (0x40 & 0x3F would be 0).
    vga.vram[0] = 0x10; // plane 0, offset 0
    vga.vram[VGA_PLANE_SIZE] = 0x20;
    vga.vram[2 * VGA_PLANE_SIZE] = 0x30;
    vga.vram[3 * VGA_PLANE_SIZE] = 0x40;
    vga.vram[1] = 0x11; // plane 0, offset 1: pixel 4 must read this
    let row = vga.render_256color_row(0);
    // Pixels 0..3 are planes 0..3 at offset 0, as full 8-bit DAC indices.
    assert_eq!(&row[0..4], &[0x10, 0x20, 0x30, 0x40]);
    assert_eq!(row[4], 0x11, "pixel 4 wraps to plane 0 at plane offset 1");
}

#[test]
fn mode_x_pel_pan_shifts_the_column_origin_by_the_pan_value() {
    // A distinct byte per plane and plane offset so every column is recognizable;
    // values reach above 0x3F, re-proving the 8-bit-direct DAC read.
    fn byte(plane: usize, off: usize) -> u8 {
        ((plane as u32 * 0x11 + off as u32 * 7 + 0x40) & 0xFF) as u8
    }
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // mode X, 320x200 double-scanned base
    for plane in 0..VGA_PLANES {
        for off in 0..VGA_PLANE_SIZE {
            vga.vram[plane * VGA_PLANE_SIZE + off] = byte(plane, off);
        }
    }
    vga.attr.pixel_pan = 0;
    let reference = vga.render_256color_row(0); // top line, no split forcing
    for pan in 1..=3u8 {
        vga.attr.pixel_pan = pan;
        let row = vga.render_256color_row(0);
        for x in 0..(reference.len() - pan as usize) {
            assert_eq!(
                row[x],
                reference[x + pan as usize],
                "pan {pan} shifts the row so column x reads the pan-0 column x+pan"
            );
        }
    }
}

#[test]
fn mode_x_pel_pan_rotates_the_plane_sequence() {
    // Distinct bytes per plane at plane offset 0 (values above 0x3F prove the
    // 8-bit-direct DAC read); other offsets stay cleared.
    let plane0_byte: [u8; VGA_PLANES] = [0x40, 0x50, 0x60, 0x70];
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // mode X
    for (plane, &b) in plane0_byte.iter().enumerate() {
        vga.vram[plane * VGA_PLANE_SIZE] = b;
    }
    // With pan N (0..3), column 0 reads plane N at plane offset 0: the
    // (0,1,2,3) origin rotates to (N, N+1, ...).
    for pan in 0..VGA_PLANES as u8 {
        vga.attr.pixel_pan = pan;
        let row = vga.render_256color_row(0);
        assert_eq!(
            row[0], plane0_byte[pan as usize],
            "pan {pan} rotates column 0 to plane {pan} at plane offset 0"
        );
    }
}

#[test]
fn mode_x_pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
    // Below the CRTC Line Compare split, render the first split row (source row 0
    // at plane offset 0) with distinct bytes per plane so a pel-pan shift is
    // visible. `mode_control` carries Attribute index 10h, `pan` the pel-pan value.
    fn render(mode_control: u8, pan: u8) -> Vec<u8> {
        let mut vga = Vga::default();
        vga.set_mode13h();
        vga.write_port(0x3C4, 0x04);
        vga.write_port(0x3C5, 0x06); // mode X
        let plane0_byte: [u8; VGA_PLANES] = [0x40, 0x50, 0x60, 0x70];
        for (i, &b) in plane0_byte.iter().enumerate() {
            vga.cpu_write_chain4(i, b);
        }
        vga.crtc.line_compare = 100;
        vga.attr.pixel_pan = pan;
        vga.attr.mode_control = mode_control;
        vga.render_256color_row(101) // first split line: below_split, source row 0, offset 0
    }
    // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
    assert_eq!(
        render(0x20, 1),
        render(0x20, 0),
        "Attribute 10h bit 5 set forces split-region pel-pan to 0"
    );
    // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
    assert_ne!(
        render(0x00, 1),
        render(0x00, 0),
        "Attribute 10h bit 5 clear pans the split region"
    );
}

#[test]
fn mode_x_page_flip_reads_the_selected_page() {
    // Checks render_256color_row's row_base arithmetic directly; the start-address
    // vretrace latch is exercised end to end in the machine test (slice-5 task 5).
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    let page1 = 0x3E80usize; // 16000 plane-bytes: a 320x200 page
    vga.vram[0] = 0xAA; // page 0, plane 0, offset 0
    vga.vram[page1] = 0x55; // page 1, plane 0, offset 0
    assert_eq!(vga.render_256color_row(0)[0], 0xAA, "start 0 reads page 0");
    vga.crtc.start_address = page1 as u32;
    assert_eq!(
        vga.render_256color_row(0)[0],
        0x55,
        "start at page 1 reads page 1"
    );
}

#[test]
fn mode_x_line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
    // A distinct byte per plane offset so each source row is recognizable. The
    // values reach above 0x3F, which also proves mode X reads the full 8-bit DAC
    // index directly (no attribute 6-bit mask).
    fn pattern(off: usize) -> u8 {
        ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
    }
    // Reference renderer with line compare left at the 0x3FF default (disabled):
    // produces a single scrolled row via the mode-X scanout.
    fn reference(start: u32, row: u32) -> Vec<u8> {
        let mut r = Vga::default();
        r.set_mode13h();
        r.write_port(0x3C4, 0x04);
        r.write_port(0x3C5, 0x06); // mode X, 320x200 base, double-scanned
        for plane in 0..VGA_PLANES {
            for off in 0..VGA_PLANE_SIZE {
                r.vram[plane * VGA_PLANE_SIZE + off] = pattern(off);
            }
        }
        r.crtc.start_address = start;
        r.render_256color_row(row)
    }

    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // mode X, double-scanned: source_row = counter_line / 2
    for plane in 0..VGA_PLANES {
        for off in 0..VGA_PLANE_SIZE {
            vga.vram[plane * VGA_PLANE_SIZE + off] = pattern(off);
        }
    }
    let start = 0x1000u32;
    let split = 300u32;
    vga.crtc.start_address = start;
    vga.crtc.line_compare = split;

    // Top row 200 (<= split): source row 100, scrolled by start.
    assert_eq!(
        vga.render_256color_row(200),
        reference(start, 200),
        "top region renders scrolled by start_address"
    );
    // First split scanline (split + 1): source row 0 from offset 0.
    assert_eq!(
        vga.render_256color_row(split + 1),
        reference(0, 0),
        "first split line renders source row 0 from offset 0"
    );
    // Deeper split scanline: (counter_line - (split + 1)) / 2 = 10, so source
    // row 10 from offset 0 matches the reference's source row 10 (row 20 / 2).
    assert_eq!(
        vga.render_256color_row(split + 21),
        reference(0, 20),
        "split region row 10 renders source row 10 from offset 0"
    );
}

#[test]
fn mode_x_line_compare_split_starts_on_the_line_after_the_match() {
    let split = 100u32;
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // mode X
    vga.vram[0] = 0xFF; // plane 0, offset 0 marked (pixel 0)
    // Scroll the top region past the marked byte so the top reads cleared VRAM.
    vga.crtc.start_address = 0x4000;
    vga.crtc.line_compare = split;
    // The matching scanline is the last top line: reads start_address (clear).
    assert_eq!(
        vga.render_256color_row(split)[0],
        0,
        "scanline == line_compare is still the top region"
    );
    // The next scanline is the first split line: reads offset 0 (marked).
    assert_eq!(
        vga.render_256color_row(split + 1)[0],
        0xFF,
        "scanline line_compare + 1 is the first split line, from offset 0"
    );
}

#[test]
fn mode_x_line_compare_compares_in_scan_counter_units_double_scanned() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // mode X
    // Abrash's 320x240 vertical timing (Black Book Listing 47.1): double-scanned,
    // 240 source rows over 480 scanlines. Same bang as the guest-CRTC retune test.
    for (idx, val) in [
        (0x06u8, 0x0Du8),
        (0x07, 0x3E),
        (0x09, 0x41),
        (0x10, 0xEA),
        (0x11, 0xAC),
        (0x12, 0xDF),
        (0x15, 0xE7),
        (0x16, 0x06),
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }
    vga.vram[0] = 0xFF; // plane 0, offset 0 marked (pixel 0)
    // Split at scan-counter line 400. The source row counter only reaches 240, so
    // a split here can only match if the comparison is in scan-counter units, not
    // divided by the double-scan factor.
    let split = 400u32;
    vga.crtc.start_address = 0x4000; // top region reads cleared VRAM
    vga.crtc.line_compare = split;
    assert_eq!(
        vga.render_256color_row(400)[0],
        0,
        "scanline 400 == line_compare is the last top line"
    );
    // Scanlines 401 and 402 are the first two split scanlines: the same doubled
    // source row 0, read from offset 0.
    assert_eq!(
        vga.render_256color_row(401)[0],
        0xFF,
        "first split scanline, offset 0"
    );
    assert_eq!(
        vga.render_256color_row(402)[0],
        0xFF,
        "second scanline holds the same doubled source row 0"
    );
}

#[test]
fn render_scanline_dispatches_to_the_mode_x_scanout() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    vga.vram[0] = 0x7E;
    let raster = vga.render_full_frame();
    assert_eq!(
        raster.pixels[0], 0x7E,
        "row 0 pixel 0 is plane 0 offset 0, 8-bit direct"
    );
}

#[test]
fn mode13h_scanout_is_column_interleaved_8bit_direct() {
    // Chain-4 routes the A0000 byte at offset N to plane N & 3 at plane
    // offset N >> 2, so four writes at offsets 0..3 land one byte per plane
    // at plane offset 0, and the write at offset 4 lands in plane 0 at plane
    // offset 1. The shared scanout then reads pixel x as plane x & 3 at plane
    // offset x >> 2, so the raster carries each written byte as the full 8-bit
    // DAC index (0x40 has bits above 0x3F, proving no 6-bit mask).
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.cpu_write_chain4(0, 0x10); // plane 0, offset 0 -> pixel 0
    vga.cpu_write_chain4(1, 0x20); // plane 1, offset 0 -> pixel 1
    vga.cpu_write_chain4(2, 0x30); // plane 2, offset 0 -> pixel 2
    vga.cpu_write_chain4(3, 0x40); // plane 3, offset 0 -> pixel 3
    vga.cpu_write_chain4(4, 0x11); // plane 0, offset 1 -> pixel 4
    let row = vga.render_256color_row(0);
    assert_eq!(&row[0..4], &[0x10, 0x20, 0x30, 0x40]);
    assert_eq!(row[4], 0x11, "pixel 4 wraps to plane 0 at plane offset 1");
}

#[test]
fn mode13h_pel_pan_shifts_the_column_origin_by_the_pan_value() {
    // A distinct byte per plane and plane offset so every column is
    // recognizable; values reach above 0x3F, re-proving the 8-bit-direct DAC
    // read. Pel-pan is masked to 0-3 (one plane per pel; a pan of 4 equals a
    // start-address bump), so pan 1..3 shifts the row by that many pixels and
    // pan 4 folds to 0.
    fn byte(plane: usize, off: usize) -> u8 {
        ((plane as u32 * 0x11 + off as u32 * 7 + 0x40) & 0xFF) as u8
    }
    let mut vga = Vga::default();
    vga.set_mode13h();
    // Fill through chain-4 so the linear cache and planar VRAM agree.
    for l in 0..0x10000usize {
        let plane = l & 3;
        let off = l >> 2;
        if off < VGA_PLANE_SIZE {
            vga.cpu_write_chain4(l, byte(plane, off));
        }
    }
    vga.crtc.start_address = 0;
    vga.attr.pixel_pan = 0;
    let reference = vga.render_256color_row(0); // top line, no split forcing
    for pan in 1..=3u8 {
        vga.attr.pixel_pan = pan;
        let row = vga.render_256color_row(0);
        for x in 0..(reference.len() - pan as usize) {
            assert_eq!(
                row[x],
                reference[x + pan as usize],
                "pan {pan} shifts the row so column x reads the pan-0 column x+pan"
            );
        }
    }
    // Pel-pan 4 is masked to 0 (& 0x03), so it reproduces the pan-0 row rather
    // than shifting by four pixels.
    vga.attr.pixel_pan = 4;
    assert_eq!(
        vga.render_256color_row(0),
        reference,
        "pan 4 folds to 0 under the 0-3 mask"
    );
    // The four-pixel shift a true pan 4 would perform is reached by bumping the
    // start address by one plane-offset unit instead: start + 1 at pan 0 equals
    // the pan-0 row shifted by four columns. This is the smooth-scroll loop
    // boundary (pan 0->3, then start + 1).
    vga.attr.pixel_pan = 0;
    vga.crtc.start_address = 1;
    let scrolled = vga.render_256color_row(0);
    for x in 0..(reference.len() - 4) {
        assert_eq!(
            scrolled[x],
            reference[x + 4],
            "start + 1 at pan 0 scans out the pan-0 row shifted by four columns"
        );
    }
}

#[test]
fn mode13h_pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
    // Below the CRTC Line Compare split, render the first split row (source row
    // 0 at plane offset 0) with distinct bytes per plane so a pel-pan shift is
    // visible. `mode_control` carries Attribute index 10h, `pan` the pel-pan
    // value.
    fn render(mode_control: u8, pan: u8) -> Vec<u8> {
        let mut vga = Vga::default();
        vga.set_mode13h();
        let plane0_byte: [u8; VGA_PLANES] = [0x40, 0x50, 0x60, 0x70];
        for (i, &b) in plane0_byte.iter().enumerate() {
            vga.cpu_write_chain4(i, b);
        }
        vga.crtc.line_compare = 100;
        vga.attr.pixel_pan = pan;
        vga.attr.mode_control = mode_control;
        vga.render_256color_row(101) // first split line: below_split, source row 0, offset 0
    }
    // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
    assert_eq!(
        render(0x20, 1),
        render(0x20, 0),
        "Attribute 10h bit 5 set forces split-region pel-pan to 0"
    );
    // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
    assert_ne!(
        render(0x00, 1),
        render(0x00, 0),
        "Attribute 10h bit 5 clear pans the split region"
    );
}

#[test]
fn mode13h_line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
    // A distinct byte per plane offset so each source row is recognizable. The
    // values reach above 0x3F, which also proves mode 13h reads the full 8-bit
    // DAC index directly (no attribute 6-bit mask).
    fn pattern(off: usize) -> u8 {
        ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
    }
    // Reference renderer with line compare left at the 0x3FF default (disabled):
    // produces a single scrolled row via the shared 256-color scanout.
    fn reference(start: u32, row: u32) -> Vec<u8> {
        let mut r = Vga::default();
        r.set_mode13h();
        for linear in 0..0x10000 {
            r.cpu_write_chain4(linear, pattern(linear >> 2));
        }
        r.crtc.start_address = start;
        r.render_256color_row(row)
    }

    let mut vga = Vga::default();
    vga.set_mode13h(); // 320x200, double-scanned: source_row = counter_line / 2
    for linear in 0..0x10000 {
        vga.cpu_write_chain4(linear, pattern(linear >> 2));
    }
    let start = 0x1000u32;
    let split = 300u32;
    vga.crtc.start_address = start;
    vga.crtc.line_compare = split;

    // Top row 200 (<= split): source row 100, scrolled by start.
    assert_eq!(
        vga.render_256color_row(200),
        reference(start, 200),
        "top region renders scrolled by start_address"
    );
    // First split scanline (split + 1): source row 0 from offset 0.
    assert_eq!(
        vga.render_256color_row(split + 1),
        reference(0, 0),
        "first split line renders source row 0 from offset 0"
    );
    // Deeper split scanline: (counter_line - (split + 1)) / 2 = 10, so source
    // row 10 from offset 0 matches the reference's source row 10 (row 20 / 2).
    assert_eq!(
        vga.render_256color_row(split + 21),
        reference(0, 20),
        "split region row 10 renders source row 10 from offset 0"
    );
}

/// Write a character/attribute pair into a text cell (row, col).
fn text_put(vga: &mut Vga, row: usize, col: usize, ch: u8, attr: u8) {
    let i = row * vga.text_columns + col;
    vga.write_u8(i * 2, ch).unwrap();
    vga.write_u8(i * 2 + 1, attr).unwrap();
}

#[test]
fn text_scanout_renders_cp437_glyph_rows_at_9x16() {
    let mut vga = Vga {
        cursor_start: 0x20,
        ..Default::default()
    };
    // 0xDB is the solid full block (all-ones rows); white on black (0x0F).
    text_put(&mut vga, 0, 0, 0xDB, 0x0F);
    // The mode 03h BIOS ATC palette maps attribute 0x0F to DAC index 0x3F;
    // the pel mask is all-pass, so a clear pixel scans out as 0.
    let top = vga.render_text_row(0); // char row 0, font line 0
    assert_eq!(
        &top[0..9],
        &[BIOS_TEXT_WHITE; 9],
        "all 9 columns of 0xDB are foreground"
    );
    assert_eq!(top[8], top[7], "the 9th column replicates the 8th for 0xDB");
    // The same glyph holds across all 16 scanlines of the character row.
    let bottom = vga.render_text_row(15); // font line 15, still char row 0
    assert_eq!(
        &bottom[0..9],
        &[BIOS_TEXT_WHITE; 9],
        "0xDB stays solid across 16 scanlines"
    );
    // A non-box glyph clears its 9th column to the background. 0xFF is outside
    // 0xC0-0xDF; load it as a full-8-column block via a custom glyph row.
    vga.text_memory[0] = 0xFF;
    let row = vga.render_text_row(0);
    assert_eq!(
        row[8], 0,
        "a glyph outside 0xC0-0xDF blanks the 9th column (inter-char gap)"
    );
}

#[test]
fn text_scanout_maps_attribute_through_the_palette_to_dac() {
    let mut vga = Vga::default();
    // 0xDB lit, foreground nibble = 1, so the pixel color is palette[1].
    text_put(&mut vga, 0, 0, 0xDB, 0x01);
    vga.attr.palette[1] = 0x2A; // map foreground index 1 -> DAC 42
    assert_eq!(
        vga.render_text_row(0)[0],
        0x2A,
        "foreground scans out at the live palette entry"
    );
    // Reprogramming the palette entry changes the scanout.
    vga.attr.palette[1] = 9;
    assert_eq!(
        vga.render_text_row(0)[0],
        9,
        "a changed palette entry reaches the scanout"
    );
}

#[test]
fn text_scanout_blink_toggles_foreground_only_when_enabled() {
    let mut vga = Vga::default();
    // Blink enabled (AC Mode Control 10h bit 3); attribute 0x8F has the blink
    // bit set and a white foreground.
    vga.attr.mode_control = 0x08;
    text_put(&mut vga, 0, 0, 0xDB, 0x8F);
    // Show phase: foreground renders as the BIOS text-white DAC entry.
    vga.frames = 0;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "show phase renders the foreground"
    );
    // Hide phase: the foreground collapses to the background (DAC 0).
    vga.frames = 16;
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "hide phase collapses the foreground to the background"
    );

    // Blink disabled: attribute bit 7 is background intensity, not blink, so
    // the foreground never collapses.
    vga.attr.mode_control = 0x00;
    vga.frames = 0;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "no blink: foreground on show phase"
    );
    vga.frames = 16;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "no blink: foreground stays on the would-be hide phase"
    );
    // And the background now reads bit 7 as intensity (background index 8),
    // then maps it through the mode 03h BIOS ATC palette.
    text_put(&mut vga, 0, 0, b' ', 0x80); // blank glyph, bit-7 background
    assert_eq!(
        vga.render_text_row(0)[0],
        0x38,
        "with blink off, attribute bit 7 selects background intensity 8"
    );
}

#[test]
fn text_scanout_presents_a_720x400_raster() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F);
    let raster = vga.render_full_frame();
    assert_eq!(raster.width, 720, "mode-03h text is 720 dots wide");
    assert_eq!(raster.height, 449, "the full frame is vtotal scanlines");
    // 400 active rows, top-justified: row 0 carries the glyph, row 400 is the
    // border (overscan, default black).
    assert_eq!(
        raster.pixels[0], BIOS_TEXT_WHITE,
        "top-left active pixel is the foreground"
    );
    let border = 400 * 720;
    assert_eq!(
        raster.pixels[border], 0,
        "scanline 400 is the border, not active"
    );
}

#[test]
fn mode03_vgabios_pixel_pan_default_keeps_text_origin_unshifted() {
    let mut vga = Vga::default();
    assert_eq!(vga.attr.pixel_pan, 8, "mode 03h BIOS default AC13");
    text_put(&mut vga, 0, 0, 0xDB, 0x0F);
    text_put(&mut vga, 0, 1, b' ', 0x0F);
    let row = vga.render_text_row(0);
    assert_eq!(row[0], BIOS_TEXT_WHITE);
    assert_eq!(row[8], BIOS_TEXT_WHITE);
    assert_eq!(row[9], 0);

    vga.attr.pixel_pan = 1;
    let row = vga.render_text_row(0);
    assert_eq!(row[0], BIOS_TEXT_WHITE);
    assert_eq!(row[8], 0);
}

#[test]
fn text_40_column_mode_uses_cga_geometry_and_stride() {
    let mut vga = Vga::default();
    vga.set_text_mode_columns(40);
    text_put(&mut vga, 1, 0, 0xDB, 0x0F);

    let frame = vga.frame();
    assert_eq!(frame.columns, 40);
    assert_eq!(frame.rows, 25);
    assert_eq!(frame.cells.len(), 40 * 25);
    assert_eq!(frame.cells[40].character, 0xDB);
    assert_eq!(vga.render_text_row(8)[0], 15);

    let raster = vga.render_full_frame();
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 262);
}

#[test]
fn text_cga_80_column_mode_uses_8x8_640x200_geometry() {
    let mut vga = Vga::default();
    vga.set_cga_80_text_mode();
    text_put(&mut vga, 1, 0, 0xDB, 0x0F);

    assert_eq!(vga.cga_mode_control(), 0x2D);
    let frame = vga.frame();
    assert_eq!(frame.columns, 80);
    assert_eq!(frame.rows, 25);
    assert_eq!(frame.cells.len(), 80 * 25);
    assert_eq!(frame.cells[80].character, 0xDB);
    assert_eq!(vga.render_text_row(8)[0], 15);

    let raster = vga.render_full_frame();
    assert_eq!(raster.width, 640);
    assert_eq!(raster.height, 262);
}

#[test]
fn cga_text_mode_03_is_80_column_color_text() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x03));
    text_put(&mut vga, 1, 0, 0xDB, 0x0F);

    assert_eq!(vga.cga_mode_control(), 0x29);
    assert_eq!(vga.frame().columns, 80);
    assert_eq!(vga.render_full_frame().width, 640);
}

#[test]
fn cga_text_start_address_wraps_at_the_16kb_window() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x02));
    text_put(&mut vga, 0, 0, b'A', 0x07);
    vga.crtc.start_address = 0x2000;
    assert_eq!(vga.frame().cells[0].character, b'A');

    vga.set_text_mode();
    text_put(&mut vga, 0, 0, b'A', 0x07);
    vga.text_memory[0x4000] = b'Z';
    vga.text_memory[0x4001] = 0x07;
    vga.crtc.start_address = 0x2000;
    assert_eq!(vga.frame().cells[0].character, b'Z');
}

#[test]
fn cga_text_blink_uses_3d8_bit5_not_vga_attribute_control() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));
    text_put(&mut vga, 0, 0, 0xDB, 0x8F);

    assert_eq!(vga.attr.mode_control & 0x08, 0);
    assert_eq!(vga.render_text_row(0)[0], 15);
    vga.frames = 16;
    assert_eq!(vga.render_text_row(0)[0], 0);

    assert!(vga.write_port(0x3D8, CGA_MODE_VIDEO_ENABLE));
    assert_eq!(vga.render_text_row(0)[0], 15);
}

#[test]
fn cga_text_colors_ignore_vga_attribute_palette_and_pel_mask() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));
    vga.attr.palette[0x0E] = CGA_BLACK;
    vga.attr.palette[0x01] = CGA_BLACK;
    vga.pel_mask = 0x00;
    text_put(&mut vga, 0, 0, 0xDB, 0x1E);
    text_put(&mut vga, 0, 1, b' ', 0x1E);

    let row = vga.render_text_row(0);
    assert_eq!(row[0], CGA_YELLOW);
    assert_eq!(row[8], 1);
}

#[test]
fn cga_text_ignores_vga_sequencer_character_width() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x02));
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    text_put(&mut vga, 0, 1, 0xDB, 0x0F);

    assert!(vga.write_port(0x3C4, 0x01));
    assert!(vga.write_port(0x3C5, 0x00));

    let row = vga.render_text_row(0);
    assert_eq!(row[7], CGA_BLACK);
    assert_eq!(row[8], CGA_WHITE);
}

#[test]
fn cga_text_ignores_vga_attribute_pixel_pan() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x02));
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    text_put(&mut vga, 0, 1, 0xDB, 0x0F);

    vga.read_status1();
    assert!(vga.write_port(0x3C0, 0x20 | 0x13));
    assert!(vga.write_port(0x3C0, 0x07));

    let row = vga.render_text_row(0);
    assert_eq!(row[1], CGA_BLACK);
    assert_eq!(row[8], CGA_WHITE);
}

#[test]
fn cga_text_uses_fixed_rom_font_not_vga_font_maps() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));
    for row in 0..8usize {
        vga.font[0][0xDB * 32 + row] = 0x00;
        vga.font[1][0xDB * 32 + row] = 0x00;
    }
    vga.seq.char_map_select = 0x04; // VGA dual-font state: map A 0, map B 1
    text_put(&mut vga, 0, 0, 0xDB, 0x08);

    assert_eq!(vga.render_text_row(0)[0], 8);
}

#[test]
fn cga_graphics_text_uses_fixed_rom_font_not_vga_font_maps() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04));
    vga.load_font_table(0, 0xDB, 8, &[0; 8]);

    assert_eq!(vga.active_font_glyph_row(0xDB, 0), 0xFF);
}

#[test]
fn cga_text_border_uses_color_select_register() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));

    vga.write_port(0x3D9, 0x05);
    let raster = vga.render_full_frame();
    let border = (raster.height as usize - 1) * raster.width as usize;
    assert_eq!(raster.pixels[border], 5);

    vga.set_overscan(0x0A);
    assert_eq!(vga.cga_color_select(), 0x0A);
    let raster = vga.render_full_frame();
    let border = (raster.height as usize - 1) * raster.width as usize;
    assert_eq!(raster.pixels[border], 10);
}

#[test]
fn cga_video_disable_blanks_the_border() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_text_mode(0x01));
    assert!(vga.write_port(0x3D9, 0x05));
    assert!(vga.write_port(0x3D8, CGA_MODE_BLINK));

    let raster = vga.render_full_frame();
    let border = (raster.height as usize - 1) * raster.width as usize;
    assert_eq!(raster.pixels[border], CGA_BLACK);

    assert!(vga.set_cga_mode(0x04));
    assert!(vga.write_port(0x3D9, 0x05));
    assert!(vga.write_port(0x3D8, CGA_MODE_GRAPHICS));

    let raster = vga.render_full_frame();
    let border = (raster.height as usize - 1) * raster.width as usize;
    assert_eq!(raster.pixels[border], CGA_BLACK);
}

#[test]
fn font_store_is_writable_per_table() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0x41, 0x0F); // 'A', white on black
    // Make table 0's 'A' blank and table 1's 'A' solid across the glyph rows.
    for row in 0..16usize {
        vga.font[0][0x41 * 32 + row] = 0x00;
        vga.font[1][0x41 * 32 + row] = 0xFF;
    }
    // Table 0 (default): the glyph is blank, so the pixel is the background.
    assert_eq!(vga.active_font_table(), 0);
    assert_eq!(vga.render_text_row(0)[0], 0, "table 0 'A' is blank");
    // Selecting table 1 shows its own solid glyph. Set map B = table 1 too so
    // the cell stays in 256-glyph mode (map A == map B); otherwise the two
    // distinct maps would engage 512-glyph mode and consume attr bit 3.
    vga.seq.char_map_select = 0x01 | 0x04; // map-A bit 0, map-B bit 2 -> table 1
    assert_eq!(vga.active_font_table(), 1);
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "table 1 'A' is solid -> foreground"
    );
}

#[test]
fn sequencer_char_map_select_picks_the_active_font() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0x41, 0x0F);
    // Table 4 is selected by map-A bit 2 (Sequencer index 3 bit 4).
    for row in 0..16usize {
        vga.font[0][0x41 * 32 + row] = 0x00;
        vga.font[4][0x41 * 32 + row] = 0xFF;
    }
    // Writing the Sequencer Character Map Select (index 3) through the port
    // switches the active table.
    vga.write_port(0x3C4, 0x03);
    vga.write_port(0x3C5, 0x00); // SR3 = 0 -> table 0 (blank)
    assert_eq!(vga.active_font_table(), 0);
    assert_eq!(vga.render_text_row(0)[0], 0);
    vga.write_port(0x3C4, 0x03);
    // SR3 = 0x30: map-A bit 4 (table 4) and map-B bit 5 (table 4), so the cell
    // stays 256-glyph (map A == map B) and does not consume attr bit 3.
    vga.write_port(0x3C5, 0x10 | 0x20); // -> table 4 (solid)
    assert_eq!(vga.active_font_table(), 4);
    assert_eq!(vga.render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn text_cursor_renders_reverse_video_on_the_cursor_cell() {
    let mut vga = Vga::default();
    // Two blank cells, white on black (0x0F); the cursor sits on cell (0,0).
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    text_put(&mut vga, 0, 1, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x00; // full block: scanlines 0..15
    vga.cursor_end = 0x0F;
    vga.frames = 0; // show phase
    let row = vga.render_text_row(0);
    // Reverse video on a blank cell swaps the background (where the blank
    // glyph reads) to the foreground, so the cursor cell is solid fg.
    assert_eq!(
        row[0], BIOS_TEXT_WHITE,
        "cursor cell scans out as the foreground (reverse video on a blank)"
    );
    // The neighbouring blank cell is not the cursor, so it stays the
    // background (0).
    assert_eq!(
        row[9], 0,
        "a non-cursor blank cell scans out as the background"
    );
}

#[test]
fn text_cursor_respects_start_and_end_scanlines() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x0E; // scanlines 14..15
    vga.cursor_end = 0x0F;
    vga.frames = 0;
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "scanline 0 is outside [14,15]: no swap"
    );
    assert_eq!(
        vga.render_text_row(14)[0],
        BIOS_TEXT_WHITE,
        "scanline 14 swaps"
    );
    assert_eq!(
        vga.render_text_row(15)[0],
        BIOS_TEXT_WHITE,
        "scanline 15 swaps"
    );
}

#[test]
fn text_cursor_disable_bit_hides_it() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x20; // bit 5 set: cursor off (start line 0 ignored)
    vga.cursor_end = 0x0F;
    vga.frames = 0;
    for line in [0u32, 7, 15] {
        assert_eq!(
            vga.render_text_row(line)[0],
            0,
            "disable bit: no swap on any scanline"
        );
    }
}

#[test]
fn text_cursor_blinks_on_the_frame_phase() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x00;
    vga.cursor_end = 0x0F;
    vga.frames = 0; // show phase: cursor visible
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "show phase: cursor swaps"
    );
    vga.frames = 16; // hide phase: cursor hidden
    assert_eq!(vga.render_text_row(0)[0], 0, "hide phase: no swap");
}

#[test]
fn text_cursor_wrap_shape_covers_two_regions() {
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x0E; // start line 14
    vga.cursor_end = 0x01; // end line 1: start > end wraps to two regions
    vga.frames = 0;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "wrap: scanline 0 swaps"
    );
    assert_eq!(
        vga.render_text_row(1)[0],
        BIOS_TEXT_WHITE,
        "wrap: scanline 1 swaps"
    );
    assert_eq!(vga.render_text_row(7)[0], 0, "wrap: scanline 7 does not");
    assert_eq!(
        vga.render_text_row(14)[0],
        BIOS_TEXT_WHITE,
        "wrap: scanline 14 swaps"
    );
    assert_eq!(
        vga.render_text_row(15)[0],
        BIOS_TEXT_WHITE,
        "wrap: scanline 15 swaps"
    );
}

#[test]
fn text_start_address_scrolls_the_display_origin() {
    // The 32 KB aperture holds eight 4096-byte pages. Page 1 starts at cell
    // 0x800 (byte 4096). Scrolling the start address down one page moves the
    // displayed cell (0,0) to read the glyph written on page 1.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F); // page 0 cell 0: solid block
    // Page 1 cell 0 = cell index 0x800 = byte 0x1000.
    let page1_cell0 = 0x800usize;
    vga.write_u8(page1_cell0 * 2, b' ').unwrap(); // blank glyph, distinct from 0xDB
    vga.write_u8(page1_cell0 * 2 + 1, 0x0F).unwrap();
    // Start address is a cell/word address (byte offset = start * 2), so the
    // BIOS page-flip value page * 0x800 maps straight onto it.
    vga.crtc.start_address = 0x800;
    // With the origin scrolled to page 1, cell (0,0) reads the blank glyph
    // there, so the top-left pixel is the background (0), not the solid
    // block foreground that page 0 holds.
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "origin scrolled to page 1 reads page 1's blank glyph"
    );
    // Scrolling back to page 0 restores the solid block.
    vga.crtc.start_address = 0;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "origin back at page 0 reads page 0's solid block"
    );
}

#[test]
fn text_start_address_below_the_split_starts_from_zero() {
    // Line Compare reloads the display address to 0 at and below the split
    // line, so a scrolled start address affects only the top region; the
    // bottom region always starts from offset 0 (FreeVGA crtcreg.htm 18h).
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F); // offset 0: solid block (foreground)
    vga.crtc.start_address = 0x800; // scroll the top region to page 1 (blank)
    vga.crtc.line_compare = 7; // split after char row 0 (8 scanlines, 0..7)
    // Top region (scanline 0..=7): origin scrolled to page 1 -> background.
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "top region reads the scrolled (blank) origin"
    );
    // First split line: address reloads to 0, so the solid block at offset 0
    // is shown again.
    assert_eq!(
        vga.render_text_row(8)[0],
        BIOS_TEXT_WHITE,
        "below-split region starts from offset 0 (solid block)"
    );
}

#[test]
fn text_memory_aperture_is_32kb_eight_pages() {
    // Growing VGA_TEXT_MEMORY_SIZE to 32768 lets the B8000 aperture reach all
    // eight 4096-byte pages. Each page's last cell (row 24, col 79 = cell
    // 1999 within the page) must be writable through the bus read/write path
    // and stay within bounds.
    let mut vga = Vga::default();
    let page7_last_cell = 0x800 * 7 + 1999; // page 7, last visible cell
    let byte = page7_last_cell * 2;
    assert!(
        byte < VGA_TEXT_MEMORY_SIZE,
        "page 7 last cell is inside the 32 KB aperture"
    );
    vga.write_u8(byte, 0xDB).unwrap();
    vga.write_u8(byte + 1, 0x0F).unwrap();
    assert_eq!(
        vga.read_u8(byte).unwrap(),
        0xDB,
        "writable byte round-trips"
    );
    assert_eq!(
        vga.read_u8(VGA_TEXT_MEMORY_SIZE - 1).unwrap_or(0xFF),
        0x07,
        "the very last byte of the 32 KB aperture is reachable"
    );
}

#[test]
fn frame_cell_view_follows_the_start_address() {
    // The headless cell view (frame) reads the visible page from the
    // start-address origin, matching the pixel scanout. Scrolling to page 1
    // makes frame() report page 1's cell (0,0), not page 0's.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, b'A', 0x07); // page 0 cell 0 = 'A'
    let page1_cell0 = 0x800usize;
    vga.write_u8(page1_cell0 * 2, b'Z').unwrap(); // page 1 cell 0 = 'Z'
    vga.write_u8(page1_cell0 * 2 + 1, 0x07).unwrap();
    assert_eq!(
        vga.frame().cells[0].character,
        b'A',
        "page 0 visible by default"
    );
    vga.crtc.start_address = 0x800;
    assert_eq!(
        vga.frame().cells[0].character,
        b'Z',
        "page 1 visible after scrolling the origin"
    );
    assert_eq!(
        vga.frame().cells.len(),
        VGA_TEXT_COLUMNS * VGA_TEXT_ROWS,
        "frame reports exactly one visible 80x25 page"
    );
}

#[test]
fn text_pel_pan_shifts_the_column_origin() {
    // AC 13h (pixel panning) shifts the whole text row left by `pan` pels.
    // With 0xDB (solid box) in cell 0 and blanks after, a pan of 1 moves the
    // lit/blank boundary one pel left: output[8] goes from cell 0's 9th column
    // (lit) to cell 1's first pel (blank).
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F); // cell 0: solid, 9 lit pels
    vga.attr.pixel_pan = 0;
    assert_eq!(
        vga.render_text_row(0)[8],
        BIOS_TEXT_WHITE,
        "pan=0: cell 0's 9th column is lit at output[8]"
    );
    vga.attr.pixel_pan = 1;
    let row = vga.render_text_row(0);
    assert_eq!(
        row[0], BIOS_TEXT_WHITE,
        "pan=1: cell 0 still leads the row (its pel 1 now at output[0])"
    );
    assert_eq!(
        row[8], 0,
        "pan=1: the column origin shifted left by one pel, so output[8] reads cell 1's blank"
    );
}

#[test]
fn text_pel_pan_below_split_forces_zero_when_enabled() {
    // AC 10h bit 5 ("pixel panning mode") forces pel-pan to 0 below the line
    // compare split (FreeVGA crtcreg.htm 18h), so the bottom region is not
    // panned even when 13h is non-zero. Above the split the pan applies.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F);
    vga.attr.pixel_pan = 1;
    vga.attr.mode_control |= 0x20; // bit 5: force pan to 0 below the split
    vga.crtc.line_compare = 7; // split after char row 0 (scanlines 0..7 above)
    // Above the split: pan=1 shifts, so output[8] is cell 1's blank (0).
    assert_eq!(
        vga.render_text_row(0)[8],
        0,
        "above the split the pel-pan applies"
    );
    // Below the split (origin reloads to 0, char row 0): pan forced to 0, so
    // cell 0's 9th column is lit at output[8].
    assert_eq!(
        vga.render_text_row(8)[8],
        BIOS_TEXT_WHITE,
        "below the split AC 10h bit 5 forces pel-pan to 0"
    );
}

#[test]
fn text_pel_pan_9dot_replicates_the_shifted_box_glyph() {
    // A 9-dot box glyph's 9th column replicates the 8th; when panned, that
    // replicate must shift with the cell. Compare a box glyph (0xDB) against a
    // non-box glyph with the same 8 solid pels: at pan=1 the shifted 9th
    // column lands at output[7], lit for the box (replicate) and a gap (0) for
    // the non-box.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F); // box glyph: 8 solid pels + replicated 9th
    vga.attr.pixel_pan = 1;
    assert_eq!(
        vga.render_text_row(0)[7],
        BIOS_TEXT_WHITE,
        "0xDB's replicated 9th column shifts into output[7] and stays lit"
    );
    // Replace cell 0 with a non-box glyph that is solid in pels 0..7 (0xFF) but
    // outside the 0xC0-0xDF box range, so its 9th column is the background.
    // Char 0x01's font slot starts at byte 0x01 * 32 = 32.
    for row in 0..16usize {
        vga.font[0][32 + row] = 0xFF;
    }
    text_put(&mut vga, 0, 0, 0x01, 0x0F);
    assert_eq!(
        vga.render_text_row(0)[7],
        0,
        "non-box glyph's shifted 9th column is a gap, not a replicate"
    );
}

#[test]
fn text_preset_row_scan_offsets_the_first_font_line() {
    // CRTC 08h bits 4-0 (preset row scan) scroll the display up within the
    // character row, so the first displayed scanline reads a later font line.
    // Load a glyph that is solid only on font line 0; a preset of 1 moves the
    // solid line off the top scanline.
    let mut vga = Vga::default();
    let ch = 0x01usize; // char 0x01: font line 0 solid, lines 1..15 clear
    vga.font[0][ch * 32] = 0xFF;
    text_put(&mut vga, 0, 0, 0x01, 0x0F);
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "preset 0: font line 0 is the first displayed scanline (solid)"
    );
    vga.crtc.preset_row_scan = 0x01; // scroll up one scanline
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "preset 1: first displayed scanline reads font line 1 (clear)"
    );
}

#[test]
fn text_byte_pan_shifts_whole_cells() {
    // CRTC 08h bits 6-5 (byte pan) add a byte offset to the start address. In
    // 9-dot text (2 bytes per cell) a byte pan of 2 shifts one whole cell.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x0F); // cell 0: solid (pel 0 lit)
    text_put(&mut vga, 0, 1, b' ', 0x0F); // cell 1: blank (pel 0 bg)
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "byte pan 0: pel 0 reads cell 0 (solid)"
    );
    vga.crtc.preset_row_scan = 0x02 << 5; // byte pan 2 (bits 6-5 = 10)
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "byte pan 2: pel 0 reads cell 1 (blank), one whole cell shifted"
    );
}

#[test]
fn text_preset_row_resets_below_the_split() {
    // Below the line-compare split the preset row scan resets to 0 (FreeVGA
    // crtcreg.htm 18h), so the vertical sub-row scroll applies only to the top
    // region. The same glyph (solid on font line 0) shows the clear line above
    // the split and the solid line below it.
    let mut vga = Vga::default();
    let ch = 0x01usize; // char 0x01: font line 0 solid, rest clear
    vga.font[0][ch * 32] = 0xFF;
    text_put(&mut vga, 0, 0, 0x01, 0x0F);
    text_put(&mut vga, 1, 0, 0x01, 0x0F); // row 1 for the below-split region
    vga.crtc.preset_row_scan = 0x01; // preset 1
    vga.crtc.line_compare = 15; // split after the first 16-scanline char row
    // Top region (scanline 0): preset applies, so pel 0 reads font line 1 (clear).
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "top region: preset row scan offsets the font line"
    );
    // Below-split region (scanline 16, char row 0, font line 0): preset reset
    // to 0, so pel 0 reads font line 0 (solid).
    assert_eq!(
        vga.render_text_row(16)[0],
        BIOS_TEXT_WHITE,
        "below-split region: preset row scan resets to 0 (font line 0 solid)"
    );
}

#[test]
fn char_map_b_decode_picks_the_second_font_table() {
    // The Sequencer Character Map Select map-B field (bits 2, 3, 5) decodes to
    // a table index with the same shape as map A. Verify each bit and the
    // composite against active_font_table_b.
    let mut vga = Vga::default();
    vga.seq.char_map_select = 0x04; // map-B bit 0 (SR3 bit 2) -> table 1
    assert_eq!(vga.active_font_table_b(), 1);
    vga.seq.char_map_select = 0x08; // map-B bit 1 (SR3 bit 3) -> table 2
    assert_eq!(vga.active_font_table_b(), 2);
    vga.seq.char_map_select = 0x20; // map-B bit 2 (SR3 bit 5) -> table 4
    assert_eq!(vga.active_font_table_b(), 4);
    vga.seq.char_map_select = 0x2C; // all three map-B bits -> table 7
    assert_eq!(vga.active_font_table_b(), 7);
}

#[test]
fn attribute_bit_3_selects_the_font_in_512_char_mode() {
    // With two distinct font tables selected (map A != map B), attribute bit 3
    // picks the font per cell: set -> map B, clear -> map A. Load table 0's
    // glyph blank and table 1's solid, select map A = 0 / map B = 1.
    let mut vga = Vga::default();
    let ch = 0x41usize;
    for row in 0..16usize {
        vga.font[0][ch * 32 + row] = 0x00; // table 0: blank
        vga.font[1][ch * 32 + row] = 0xFF; // table 1: solid
    }
    text_put(&mut vga, 0, 0, 0x41, 0x07); // bit 3 clear -> map A (blank)
    // map A = table 0 (SR3 bit 0 clear), map B = table 1 (SR3 bit 2 set).
    vga.seq.char_map_select = 0x04; // map A 0, map B 1 -> dual-font active
    assert_eq!(
        vga.render_text_row(0)[0],
        0,
        "bit 3 clear: map A glyph (table 0, blank)"
    );
    // Set bit 3 -> map B (table 1, solid). fg is masked to 8 colors now, so the
    // solid glyph reads palette[attr & 0x07] = palette[7] = 7 (not 15).
    text_put(&mut vga, 0, 0, 0x41, 0x0F); // bit 3 set -> map B
    assert_eq!(
        vga.render_text_row(0)[0],
        7,
        "bit 3 set: map B glyph (table 1, solid); fg masked to 8 colors"
    );
}

#[test]
fn int10_11h_loads_two_fonts_for_512_char_text() {
    // Loading two fonts into distinct tables and selecting them via the
    // Character Map Select engages 512-glyph mode end-to-end. This mirrors the
    // AH=11h font-load path: load_font_table into table 0 and table 1, then
    // set_char_map_select so map A = 0 and map B = 1.
    let mut vga = Vga::default();
    let ch = 0x42usize; // 'B'
    // Table 0: 'B' blank; table 1: 'B' solid (two glyphs).
    let blank = vec![0x00u8; 16];
    let solid = vec![0xFFu8; 16];
    vga.load_font_table(0, ch as u16, 16, &blank);
    vga.load_font_table(1, ch as u16, 16, &solid);
    // Map A = 0, map B = 1 (SR3 bit 2 set for map B value 1).
    vga.set_char_map_select(0x04);
    text_put(&mut vga, 0, 0, 0x42, 0x07); // bit 3 clear -> map A (blank)
    assert_eq!(vga.render_text_row(0)[0], 0, "map A 'B' is blank");
    text_put(&mut vga, 0, 0, 0x42, 0x0F); // bit 3 set -> map B (solid)
    assert_eq!(
        vga.render_text_row(0)[0],
        7,
        "map B 'B' is solid (fg masked to 8 colors in 512-char mode)"
    );
}

#[test]
fn text_cursor_skew_delays_the_cursor_onset() {
    // The Cursor Skew (0Bh bits 6-5) delays the cursor onset by that many
    // character clocks, so the cursor appears `skew` cells to the right of the
    // cursor location. With cursor_offset 0 and skew 1, the cursor fires on
    // cell 1 instead of cell 0.
    let mut vga = Vga::default();
    // Two blank cells; cursor configured as a full block on scanline 0.
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    text_put(&mut vga, 0, 1, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x00; // full block
    vga.cursor_end = 0x0F | (0x01 << 5); // end line 15 + skew 1
    vga.frames = 0; // show phase
    let row = vga.render_text_row(0);
    // Cell 0 (pels 0..8): not the skewed cursor (it moved to cell 1).
    assert_eq!(row[0], 0, "skew 1: cell 0 is not the cursor");
    // Cell 1 (pel 9 onward): the cursor, swapped to foreground.
    assert_eq!(row[9], BIOS_TEXT_WHITE, "skew 1: cursor delayed to cell 1");
}

#[test]
fn text_cursor_skew_three_is_max_delay_not_disabled() {
    // Per A5, a skew of 3 is the maximum delay (3 char clocks), not a disable.
    // The disable is the separate 0Ah bit 5. With cursor_offset 0 and skew 3,
    // the cursor fires on cell 3.
    let mut vga = Vga::default();
    for col in 0..5 {
        text_put(&mut vga, 0, col, b' ', 0x0F);
    }
    vga.cursor_offset = 0;
    vga.cursor_start = 0x00; // full block, not disabled (bit 5 clear)
    vga.cursor_end = 0x0F | (0x03 << 5); // end line 15 + skew 3
    vga.frames = 0; // show phase
    let row = vga.render_text_row(0);
    assert_eq!(row[0], 0, "skew 3: cell 0 not the cursor");
    assert_eq!(
        row[3 * 9],
        BIOS_TEXT_WHITE,
        "skew 3: cursor delayed to cell 3 (max delay, not disabled)"
    );
}

#[test]
fn attribute_blink_runs_at_the_hardware_cadence() {
    // The attribute blink hides the foreground for 16 frames, then shows it for
    // 16 (period 32), driven by the vertical-retrace frame counter. A blink
    // attribute cell toggles at that cadence; a non-blink cell never toggles.
    let mut vga = Vga::default();
    vga.attr.mode_control = 0x08; // blink enabled
    text_put(&mut vga, 0, 0, 0xDB, 0x8F); // blink bit set, white fg
    // Frames 0..15: show phase (fg visible).
    for f in [0u64, 1, 7, 15] {
        vga.frames = f;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "frame {f}: show phase, foreground visible"
        );
    }
    // Frames 16..31: hide phase (fg collapses to bg).
    for f in [16u64, 17, 24, 31] {
        vga.frames = f;
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "frame {f}: hide phase, foreground collapsed"
        );
    }
    // Frame 32: the period repeats, back to show.
    vga.frames = 32;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "frame 32: period repeats (show)"
    );
}

#[test]
fn text_cursor_blinks_at_the_hardware_cadence() {
    // The hardware cursor blinks on the same 16-on/16-off cadence as the
    // attribute blink, sharing the one frame-counter phase. The cursor is
    // visible on frames 0..15 and hidden on 16..31, period 32.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, b' ', 0x0F);
    vga.cursor_offset = 0;
    vga.cursor_start = 0x00; // full block
    vga.cursor_end = 0x0F;
    for f in [0u64, 5, 15] {
        vga.frames = f;
        assert_eq!(
            vga.render_text_row(0)[0],
            BIOS_TEXT_WHITE,
            "frame {f}: cursor visible (show phase)"
        );
    }
    for f in [16u64, 20, 31] {
        vga.frames = f;
        assert_eq!(
            vga.render_text_row(0)[0],
            0,
            "frame {f}: cursor hidden (hide phase)"
        );
    }
    vga.frames = 32;
    assert_eq!(
        vga.render_text_row(0)[0],
        BIOS_TEXT_WHITE,
        "frame 32: period repeats"
    );
}

#[test]
fn sequencer_reset_and_screen_off_blank_output_and_status() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.vram[0] = 0x80; // plane 0, pixel 0 -> index 1 when output is enabled.

    assert_eq!(vga.seq.reset, 0x03);
    assert_eq!(vga.render_full_frame().pixels[0], 1);
    assert_eq!(vga.read_status1() & 0x01, 0);

    vga.write_port(0x3C4, 0x00);
    vga.write_port(0x3C5, 0x02); // asynchronous reset asserted (bit 0 clear)
    assert_eq!(vga.seq.reset, 0x02);
    assert_eq!(vga.render_full_frame().pixels[0], 0);
    assert_eq!(vga.read_status1() & 0x01, 0x01);

    vga.write_port(0x3C5, 0x03); // both reset bits (index 0 still selected)
    assert_eq!(vga.seq.reset, 0x03);
    assert_eq!(vga.render_full_frame().pixels[0], 1);

    vga.write_port(0x3C4, 0x01);
    vga.write_port(0x3C5, 0x20); // Clocking Mode bit 5: screen off.
    assert_eq!(vga.render_full_frame().pixels[0], 0);
    assert_eq!(vga.read_status1() & 0x01, 0x01);
}

#[test]
fn display_refresh_control_blanks_output_and_status() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.vram[0] = 0x80; // plane 0, pixel 0 -> index 1 when refresh is enabled.

    assert!(vga.display_refresh_enabled());
    assert_eq!(vga.render_full_frame().pixels[0], 1);
    assert_eq!(vga.read_status1() & 0x01, 0);

    vga.set_display_refresh_enabled(false);
    assert_eq!(vga.render_full_frame().pixels[0], 0);
    assert_eq!(vga.read_status1() & 0x01, 0x01);

    vga.set_display_refresh_enabled(true);
    assert_eq!(vga.render_full_frame().pixels[0], 1);
}

#[test]
fn input_status0_reports_misc_selected_switch_sense_and_retrace() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    let htotal = htotal_dots(&vga.crtc);
    vga.beam = htotal * (vga.crtc.vdisp_end as u64); // active off, not in retrace
    for (select, expected) in [(0, 0x00), (1, 0x10), (2, 0x10), (3, 0x00)] {
        assert!(vga.write_port(0x3C2, (select << 2) | 0x03));
        assert_eq!(
            vga.read_port(0x3C2).unwrap() & 0x10,
            expected,
            "bit 4 reports colour-display sense bit {select}"
        );
    }
    assert_eq!(
        vga.read_port(0x3C2).unwrap() & 0x80,
        0x00,
        "bit 7 clear outside vertical retrace"
    );
    // Park the beam in vertical retrace: bit 7 (CRT interrupt status) sets.
    vga.beam = htotal * (vga.crtc.vretrace_start as u64);
    let retrace = vga.read_port(0x3C2).unwrap();
    assert_eq!(retrace & 0x80, 0x80, "bit 7 set during vertical retrace");
}

#[test]
fn color_select_folds_into_the_dac_index_when_bit7_clear() {
    // AC Mode Control 10h bit 7 clear: the full 6-bit palette value is DAC bits 5-0,
    // and Color Select 14h bits 3-2 supply DAC bits 7-6.
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
        *b = 0xFF; // every pixel is attribute index 1
    }
    vga.attr.palette[1] = 0x05; // 6-bit palette value 0b00_0101
    vga.attr.mode_control = 0x00; // bit 7 clear
    vga.attr.color_select = 0x0F; // bits 3-2 = 11 -> DAC bits 7-6
    // DAC = 0b11_00_0101 = 0xC5 (palette bits 5-4 untouched).
    assert_eq!(vga.render_active_row(0)[0], 0xC5);
    // Color Select 0 leaves the bare 6-bit palette value.
    vga.attr.color_select = 0x00;
    assert_eq!(vga.render_active_row(0)[0], 0x05);
}

#[test]
fn color_select_replaces_palette_bits_5_4_when_bit7_set() {
    // AC Mode Control 10h bit 7 set: palette bits 5-4 are replaced by Color
    // Select bits 1-0, and Color Select bits 3-2 supply DAC bits 7-6.
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
        *b = 0xFF;
    }
    vga.attr.palette[1] = 0x3A; // 0b11_1010; bits 5-4 (0b11) get replaced
    vga.attr.mode_control = 0x80; // bit 7 set
    vga.attr.color_select = 0x06; // bits 1-0 = 10 -> P5/P4; bits 3-2 = 01 -> DAC 7-6
    // DAC = bits 7-6 (01) | bits 5-4 (10) | palette bits 3-0 (1010) = 0b01_10_1010 = 0x6A.
    assert_eq!(vga.render_active_row(0)[0], 0x6A);
}

#[test]
fn color_select_folds_into_text_foreground() {
    // The text path routes the same fold: a foreground palette value picks up
    // the Color Select high bits.
    let mut vga = Vga::default();
    text_put(&mut vga, 0, 0, 0xDB, 0x01); // solid glyph, fg index 1
    vga.attr.palette[1] = 0x01;
    vga.attr.mode_control = 0x00; // bit 7 clear (and blink off)
    vga.attr.color_select = 0x0C; // bits 3-2 -> DAC 7-6
    // DAC = 0b11_00_0001 = 0xC1.
    assert_eq!(vga.render_text_row(0)[0], 0xC1);
}

#[test]
fn color_plane_enable_masks_vga_text_and_planar_indexes() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    for plane in 0..VGA_PLANES {
        vga.vram[plane * VGA_PLANE_SIZE] = 0x80;
    }
    vga.attr.plane_enable = 0x05;
    assert_eq!(vga.render_active_row(0)[0], 0x05);
    vga.attr.plane_enable = 0x00;
    assert_eq!(vga.render_active_row(0)[0], 0x00);

    let mut text = Vga::default();
    text_put(&mut text, 0, 0, 0xDB, 0x0F);
    text.attr.plane_enable = 0x05;
    assert_eq!(text.render_text_row(0)[0], 0x05);
    text.attr.plane_enable = 0x00;
    assert_eq!(text.render_text_row(0)[0], 0x00);
}

#[test]
fn feature_control_round_trips_3ca_with_color_and_mono_writes() {
    let mut vga = Vga::default();
    assert_eq!(vga.read_port(0x3CA), Some(0x00), "powers up at 0");
    assert!(vga.write_port(0x3DA, 0x0A)); // colour write address
    assert_eq!(vga.read_port(0x3CA), Some(0x0A));
    assert!(vga.write_port(0x3C2, vga.misc_output & !0x01));
    assert!(vga.write_port(0x3BA, 0x05)); // mono alias of the same register
    assert_eq!(vga.read_port(0x3CA), Some(0x05));
}

#[test]
fn video_subsystem_enable_round_trips_3c3() {
    let mut vga = Vga::default();
    assert_eq!(vga.read_port(0x3C3), Some(0x01), "powers up enabled");
    assert!(vga.write_port(0x3C3, 0x00));
    assert_eq!(vga.read_port(0x3C3), Some(0x00));
    // Only bit 0 is stored.
    assert!(vga.write_port(0x3C3, 0xFF));
    assert_eq!(vga.read_port(0x3C3), Some(0x01));
}

#[test]
fn dac_state_reports_the_armed_access_mode() {
    let mut vga = Vga::default();
    // Powers up armed for a write (3C8 path): state 0b00.
    assert_eq!(vga.read_port(0x3C7), Some(0x00));
    // A read-index write (3C7) arms a read: state 0b11.
    assert!(vga.write_port(0x3C7, 5));
    assert_eq!(vga.read_port(0x3C7), Some(0x03));
    let _ = vga.read_port(0x3C9);
    assert_eq!(vga.read_port(0x3C7), Some(0x03));
    // A write-index write (3C8) arms a write again: state 0b00.
    assert!(vga.write_port(0x3C8, 7));
    assert_eq!(vga.read_port(0x3C7), Some(0x00));
    assert!(vga.write_port(0x3C9, 0x2A));
    assert_eq!(vga.read_port(0x3C7), Some(0x00));
}

#[test]
fn set_mode_installs_the_two_color_640_modes_0f_and_11() {
    let mut vga = Vga::default();
    // 0Fh shares 10h's 640x350 timing.
    assert!(vga.set_mode(0x0F));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.crtc.vdisp_end, 350);
    assert_eq!(vga.active_mode(), VideoMode::Planar);
    assert_eq!(vga.seq.map_mask, 0x0F);
    assert_eq!(vga.misc_output & 0xC1, 0x80);
    assert_eq!(CrtcTiming::mode_0fh(), CrtcTiming::mode_10h());
    // 11h shares 12h's 640x480 timing.
    assert!(vga.set_mode(0x11));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.crtc.vdisp_end, 480);
    assert_eq!(vga.seq.map_mask, 0x0F);
    assert_eq!(vga.misc_output & 0xC1, 0xC1);
    assert_eq!(CrtcTiming::mode_11h(), CrtcTiming::mode_12h());
}

#[test]
fn mode_0fh_scanout_uses_vgabios_monochrome_attribute_table() {
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x0F));
    assert_eq!(vga.seq.map_mask, 0x0F);

    vga.cpu_write(0, 0x80);
    assert_eq!(vga.plane_byte(0, 0), 0x80);
    assert_eq!(vga.plane_byte(1, 0), 0x80);
    assert_eq!(vga.plane_byte(2, 0), 0x80);
    assert_eq!(vga.plane_byte(3, 0), 0x80);
    assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
    assert_eq!(vga.render_active_row(0)[0], 0x08);

    vga.vram[0] = 0;
    vga.vram[2 * VGA_PLANE_SIZE] = 0;
    vga.seq.map_mask = 0x01;
    vga.cpu_write(0, 0x80);
    assert_eq!(vga.planar_read_pixel(0, 0), 0x03);

    vga.vram[0] = 0;
    vga.vram[2 * VGA_PLANE_SIZE] = 0;
    vga.seq.map_mask = 0x04;
    vga.cpu_write(0, 0x80);
    assert_eq!(vga.planar_read_pixel(0, 0), 0x0C);

    vga.vram[0] = 0;
    vga.vram[2 * VGA_PLANE_SIZE] = 0;
    vga.seq.map_mask = 0x05;

    let plane0 = 0;
    let plane2 = 2 * VGA_PLANE_SIZE;
    vga.vram[plane0] = 0x80;
    assert_eq!(vga.planar_read_pixel(0, 0), 0x03);
    assert_eq!(vga.render_active_row(0)[0], 0x08);

    vga.vram[plane0] = 0;
    vga.vram[plane2] = 0x80;
    assert_eq!(vga.planar_read_pixel(0, 0), 0x0C);
    vga.frames = 0;
    assert_eq!(vga.render_active_row(0)[0], 0x00);
    vga.frames = 16;
    assert_eq!(vga.render_active_row(0)[0], 0x00);

    vga.vram[plane0] = 0x80;
    vga.frames = 16;
    assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
    assert_eq!(vga.render_active_row(0)[0], 0x08);

    assert!(vga.planar_write_pixel(1, 0, 0x0C, false));
    assert_eq!(vga.planar_read_pixel(1, 0), 0x0C);
}

#[test]
fn mode_11h_scanout_uses_map0_like_mode6() {
    let mut vga = Vga::default();
    assert!(vga.set_mode(0x11));
    assert_eq!(vga.seq.map_mask, 0x0F);

    vga.cpu_write(0, 0x80);
    assert_eq!(vga.plane_byte(0, 0), 0x80);
    assert_eq!(vga.plane_byte(1, 0), 0x80);
    assert_eq!(vga.plane_byte(2, 0), 0x80);
    assert_eq!(vga.plane_byte(3, 0), 0x80);
    assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
    assert_eq!(vga.render_active_row(0)[0], 0x3F);
    vga.vram[0] = 0;

    for plane in 1..VGA_PLANES {
        vga.vram[plane * VGA_PLANE_SIZE] = 0x80;
    }
    assert_eq!(vga.planar_read_pixel(0, 0), 0x00);
    assert_eq!(vga.render_active_row(0)[0], 0x00);

    vga.vram[0] = 0x80;
    assert_eq!(vga.planar_read_pixel(0, 0), 0x0F);
    assert_eq!(vga.render_active_row(0)[0], 0x3F);

    assert!(vga.planar_write_pixel(1, 0, 0x0F, false));
    assert_eq!(vga.planar_read_pixel(1, 0), 0x0F);
    assert_eq!(vga.plane_byte(1, 0) & 0x40, 0);
}

#[test]
fn palette_address_source_tracks_the_3c0_index_bit5() {
    // The 3C0 index bit 5 (Palette Address Source) is decoded and read back. It powers
    // up set (the mode-set default), clears on an index write with bit 5 clear, and
    // sets again on an index write with bit 5 set.
    let mut vga = Vga::default();
    assert!(vga.attr.pas, "PAS powers up set");
    vga.read_status1(); // reset the flip-flop to the index phase
    vga.write_port(0x3C0, 0x00); // index 0, bit 5 clear -> PAS off
    assert!(!vga.attr.pas);
    vga.read_status1();
    vga.write_port(0x3C0, 0x20); // index 0 with bit 5 set -> PAS on
    assert!(vga.attr.pas);
}

#[test]
fn palette_address_source_clear_blanks_render_and_status() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() {
        *b = 0xFF;
    }
    vga.attr.palette[1] = 5;
    vga.attr.overscan = 7;

    let lit = vga.render_full_frame();
    let w = lit.width as usize;
    let border = vga.crtc.vdisp_end as usize * w;
    assert_eq!(lit.pixels[0], 5);
    assert_eq!(lit.pixels[border], 7);

    vga.read_status1();
    vga.write_port(0x3C0, 0x00); // PAS clear
    assert!(!vga.attr.pas);
    vga.beam = 0;
    assert_eq!(vga.read_status1() & 0x01, 0x01);

    let blank = vga.render_full_frame();
    assert_eq!(blank.pixels[0], 0);
    assert_eq!(blank.pixels[border], 0);

    vga.read_status1();
    vga.write_port(0x3C0, 0x20); // PAS set again
    assert!(vga.attr.pas);
    let restored = vga.render_full_frame();
    assert_eq!(restored.pixels[0], 5);
}

#[test]
fn palette_address_source_bit_does_not_leak_into_the_attr_index() {
    // Bit 5 of the 3C0 index drives PAS but is masked off the stored index, so
    // the following data write still lands on the low-5-bit register.
    let mut vga = Vga::default();
    vga.read_status1(); // index phase
    vga.write_port(0x3C0, 0x20 | 0x13); // PAS on + index 0x13 (pixel pan)
    assert_eq!(vga.attr.index, 0x13);
    assert!(vga.attr.pas);
    vga.write_port(0x3C0, 0x07); // data: pixel_pan = 7
    assert_eq!(vga.attr.pixel_pan, 0x07);
}
