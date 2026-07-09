// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
