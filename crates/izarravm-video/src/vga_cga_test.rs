// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
