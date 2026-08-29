// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn catch_up_mid_frame(vga: &mut Vga) {
    vga.advance(vga.frame_dots());
    let line_dots = vga.frame_dots() / u64::from(vga.crtc.vtotal);
    vga.advance(line_dots * 100);
    vga.read_status1();
    assert_eq!(vga.last_line, 100);
}

fn finish_current_frame(vga: &mut Vga) {
    vga.advance(vga.frame_dots() - vga.beam_dots());
}

fn direct_mode_x(plane: u8) -> Vga {
    let mut vga = Vga::default();
    vga.set_mode13h_with_clear(true);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 1 << plane);
    assert_eq!(vga.active_mode(), VideoMode::ModeX);
    assert_eq!(vga.mode_x_direct_write_plane(), Some(usize::from(plane)));
    vga
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
fn mode13h_argb_cache_converts_only_the_dirty_direct_page() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    video.advance(video.frame_dots());
    let (initial, width, _, _) = video
        .cached_mode13h_presented_argb()
        .expect("canonical Mode 13h frame is cacheable");
    assert_eq!(video.mode13h_last_converted_pixels(), initial.len());

    let offset = 0x1000;
    video.note_mode13h_direct_write(offset, 1);
    video.mode13_linear[offset] = 0x2A;
    video.finish_mode13h_direct_batch();
    video.advance(video.frame_dots());
    let (updated, _, _, _) = video
        .cached_mode13h_presented_argb()
        .expect("updated canonical frame stays cacheable");

    assert_eq!(
        video.mode13h_last_converted_pixels(),
        0x1000 * 2,
        "one 4 KiB source page updates its two double-scanned rows"
    );
    assert_eq!(updated[0], initial[0], "untouched page remains cached");
    let source_row = offset / width;
    let x = offset % width;
    assert_eq!(
        updated[(source_row * 2) * width + x],
        video.palette_argb()[0x2A]
    );
    assert_eq!(
        updated[(source_row * 2 + 1) * width + x],
        video.palette_argb()[0x2A]
    );
}

#[test]
fn mode13h_argb_cache_settles_a_direct_write_after_catch_up() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    video.advance(video.frame_dots());
    let (initial, _, _, _) = video
        .cached_mode13h_presented_argb()
        .expect("initial frame is cacheable");

    let line_dots = video.frame_dots() / u64::from(video.crtc.vtotal);
    video.advance(line_dots * 100);
    video.read_status1();
    assert_eq!(video.last_line, 100);
    video.note_mode13h_direct_write(0, 1);
    video.mode13_linear[0] = 0x2A;
    video.finish_mode13h_direct_batch();

    video.advance(video.frame_dots() - video.beam_dots());
    let (split, _, _, _) = video
        .cached_mode13h_presented_argb()
        .expect("split frame remains cacheable");
    assert_eq!(split[0], initial[0], "past scanline keeps its old pixel");

    video.advance(video.frame_dots());
    let (settled, _, _, _) = video
        .cached_mode13h_presented_argb()
        .expect("settled frame remains cacheable");
    assert_eq!(settled[0], video.palette_argb()[0x2A]);
    assert_eq!(
        video.mode13h_last_converted_pixels(),
        0x1000 * 2,
        "the retained dirty page updates once more on the settled frame"
    );
}

#[test]
fn completed_raster_generation_moves_through_split_and_settled_chain4_frames() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    catch_up_mid_frame(&mut video);
    let initial_generation = video.last_presented().unwrap().generation;

    video.cpu_write_chain4(0, 0x2A);
    assert_eq!(video.graphics_settle_frames, 2);
    assert_eq!(
        video.last_presented().unwrap().generation,
        initial_generation,
        "a live write must not relabel the completed raster"
    );

    finish_current_frame(&mut video);
    let split_generation = video.last_presented().unwrap().generation;
    assert_eq!(split_generation, video.content_gen());
    assert_ne!(split_generation, initial_generation);
    assert_eq!(video.graphics_settle_frames, 1);

    video.advance(video.frame_dots());
    let settled_generation = video.last_presented().unwrap().generation;
    assert_ne!(settled_generation, split_generation);
    assert_eq!(video.graphics_settle_frames, 0);

    video.advance(video.frame_dots());
    assert_eq!(
        video.last_presented().unwrap().generation,
        settled_generation,
        "an unchanged completed raster keeps its generation"
    );
}

#[test]
fn planar_and_cga_mid_frame_writes_arm_two_completed_rasters() {
    let mut planar = Vga::default();
    assert!(planar.set_mode(0x0D));
    catch_up_mid_frame(&mut planar);
    planar.cpu_write(0, 0x5A);
    assert_eq!(planar.graphics_settle_frames, 2);
    finish_current_frame(&mut planar);
    assert_eq!(planar.graphics_settle_frames, 1);
    planar.advance(planar.frame_dots());
    assert_eq!(planar.graphics_settle_frames, 0);

    let mut cga = Vga::default();
    assert!(cga.set_cga_mode(0x04));
    catch_up_mid_frame(&mut cga);
    cga.cga_write(0, 0x6C);
    assert_eq!(cga.graphics_settle_frames, 2);
    finish_current_frame(&mut cga);
    assert_eq!(cga.graphics_settle_frames, 1);
    cga.advance(cga.frame_dots());
    assert_eq!(cga.graphics_settle_frames, 0);
}

#[test]
fn normal_write_preserves_settle_armed_by_a_direct_mode13_write() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    catch_up_mid_frame(&mut video);

    video.note_mode13h_direct_write(0, 1);
    video.mode13_linear[0] = 0x11;
    video.finish_mode13h_direct_batch();
    assert_eq!(video.graphics_settle_frames, 2);

    video.cpu_write_chain4(1, 0x22);
    assert_eq!(
        video.graphics_settle_frames, 2,
        "the ordinary mutator must not cancel the direct-write settle"
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
fn hle_pixel_pan_materializes_direct_mode13_pixels_before_disabling_the_mapping() {
    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    assert_eq!(video.direct_write_token(), 1);
    assert!(video.mode13h_direct_page_ptr(0).is_some());
    video.note_mode13h_direct_write(0, 4);
    video.mode13_linear[..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    assert!(video.mode13_linear_authoritative);
    assert_eq!(video.vram[0], 0);
    assert_eq!(video.vram[VGA_PLANE_SIZE], 0);

    video.set_attr_register(0x00, 0x07);
    assert!(video.mode13_linear_authoritative);
    video.set_attr_register(0x13, 0);
    assert!(video.mode13_linear_authoritative);

    video.set_attr_register(0x13, 1);

    assert_eq!(video.direct_write_token(), 0);
    assert!(!video.mode13_linear_authoritative);
    assert_eq!(video.vram[0], 0x11);
    assert_eq!(video.vram[VGA_PLANE_SIZE], 0x22);
    assert_eq!(video.vram[2 * VGA_PLANE_SIZE], 0x33);
    assert_eq!(video.vram[3 * VGA_PLANE_SIZE], 0x44);
    assert_eq!(&video.render_256color_row(0)[..3], &[0x22, 0x33, 0x44]);
}

#[test]
fn hle_char_height_materializes_direct_mode13_pixels_only_when_it_changes() {
    let mut unchanged = Vga::default();
    unchanged.set_mode13h_with_clear(true);
    unchanged.note_mode13h_direct_write(0, 1);
    unchanged.mode13_linear[0] = 0x5a;
    assert!(unchanged.mode13_linear_authoritative);
    unchanged.set_char_height(unchanged.char_height());
    assert_eq!(unchanged.direct_write_token(), 1);
    assert!(unchanged.mode13_linear_authoritative);

    let mut video = Vga::default();
    video.set_mode13h_with_clear(true);
    assert!(video.mode13h_direct_page_ptr(0).is_some());
    video.note_mode13h_direct_write(0, 4);
    video.mode13_linear[..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    assert!(video.mode13_linear_authoritative);
    assert_eq!(video.vram[0], 0);
    assert_eq!(video.vram[VGA_PLANE_SIZE], 0);

    video.set_char_height(8);

    assert_eq!(video.direct_write_token(), 0);
    assert!(!video.mode13_linear_authoritative);
    assert_eq!(video.char_height(), 8);
    assert_eq!(video.vram[0], 0x11);
    assert_eq!(video.vram[VGA_PLANE_SIZE], 0x22);
    assert_eq!(video.vram[2 * VGA_PLANE_SIZE], 0x33);
    assert_eq!(video.vram[3 * VGA_PLANE_SIZE], 0x44);
    assert_eq!(
        &video.render_256color_row(0)[..4],
        &[0x11, 0x22, 0x33, 0x44]
    );
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
    // Mode 13h's BIOS register set leaves the CRTC write protect (11h bit 7)
    // SET, so registers 00h-07h are read-only until the guest clears it. Real
    // silicon refuses these writes without this, and so does the model.
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x0E);
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
fn guest_crtc_bang_retunes_mode_x_to_360x240() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // enter mode X, 320x200 base
    assert_eq!(vga.raster_width(), 320);
    // Mode 13h's BIOS register set leaves the CRTC write protect (11h bit 7)
    // SET, so registers 00h-07h are read-only until the guest clears it. Real
    // silicon refuses these writes without this, and so does the model.
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x0E);
    // Abrash's wide mode X (Black Book ch.47): the 28.322 MHz dot clock with 90
    // character clocks of active display. A 256-color pixel takes two dot
    // clocks, so 90 * 8 / 2 = 360 pixels, and offset 45 gives the matching 90
    // bytes per plane per row. DOS Quake's 360x240 mode is this register set.
    vga.write_port(0x3C2, 0xE7); // Misc Output: 28.322 MHz dot clock
    for (idx, val) in [
        (0x00u8, 0x6Bu8), // horizontal total: 112 character clocks
        (0x01, 0x59),     // horizontal display end: 90 character clocks
        (0x02, 0x5A),
        (0x03, 0x8E),
        (0x04, 0x5E),
        (0x05, 0x8A),
        (0x06, 0x0D), // vertical total
        (0x07, 0x3E), // overflow (high bits)
        (0x09, 0x41), // max scan line: 2 scanlines per row
        (0x10, 0xEA), // vretrace start
        (0x11, 0xAC), // vretrace end + protect
        (0x12, 0xDF), // vertical display end
        (0x13, 0x2D), // offset 45: 90 bytes per plane per row
        (0x15, 0xE7), // vblank start
        (0x16, 0x06), // vblank end
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }
    assert_eq!(vga.raster_width(), 360, "360 active pixels per row");
    assert_eq!(vga.crtc.offset, 45, "90 bytes per plane per row");
    assert_eq!(vga.crtc.vdisp_end, 480, "480 active scanlines");
    assert_eq!(vga.raster_height(), 527);
    assert!(vga.crtc.double_scan, "240 source rows over 480 scanlines");
    // 112 * 8 dots * 527 lines at 28.322 MHz is the mode's 60 Hz refresh.
    assert_eq!(vga.frame_dots(), 112 * 8 * 527);
}

/// Psycho Pinball (Codemasters, 1994, DOS/4GW) sets BIOS mode 13h and then
/// plays one register table that programs the WHOLE CRTC BEFORE it clears
/// chain-4 in Sequencer Memory Mode. The table is at file offset 433720 of
/// `_P_.EXE` as (port, index, value) triplets, and its two mode tables are the
/// two tests below. Clearing chain-4 changes the memory decode, not the display
/// timing, so the mode X entry must keep the CRTC bytes the guest already
/// wrote. Re-seeding the canonical 320x200 register set there threw the game's
/// own vertical timing away and cropped every frame.
///
/// This first table keeps mode 13h's 449-line frame but clears the double-scan
/// (09h = 00h) and lowers the vertical display end to 370 source rows, so the
/// active picture is 320x370.
#[test]
fn tall_crtc_bang_before_the_chain4_clear_survives_the_mode_x_entry() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    // Misc Output: 28.322 MHz dot clock, colour I/O, RAM enabled.
    vga.write_port(0x3C2, 0xA7);
    for (idx, val) in [
        (0x00u8, 0x70u8), // horizontal total
        (0x01, 0x4F),     // horizontal display end: 80 character clocks
        (0x02, 0x50),
        (0x03, 0x8E),
        (0x04, 0x5E),
        (0x05, 0x8A),
        (0x06, 0xBF), // vertical total: mode 13h's 449 lines
        (0x07, 0x1F), // overflow (high bits)
        (0x08, 0x00),
        (0x09, 0x00), // max scan line 0: the double-scan is CLEARED
        (0x10, 0x8C), // vretrace start
        (0x11, 0x70), // vretrace end, write protect off
        (0x12, 0x71), // vertical display end: 370 with the overflow bit
        (0x13, 0x28), // offset 40: 80 bytes per plane per row
        (0x14, 0x00),
        (0x15, 0x6F), // vblank start
        (0x16, 0xB9), // vblank end
        (0x17, 0xE3), // mode control
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }
    // Only NOW does the guest unchain: 8-dot characters, then extended memory
    // with odd/even off and chain-4 (bit 3) CLEARED.
    vga.write_port(0x3C4, 0x01);
    vga.write_port(0x3C5, 0x01);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    // Graphics Controller: 256-colour shift, graphics at A0000.
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x40);
    vga.write_port(0x3CE, 0x06);
    vga.write_port(0x3CF, 0x05);
    // Attribute Controller: graphics + 8-bit colour, no pel pan.
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41);
    vga.write_port(0x3C0, 0x13);
    vga.write_port(0x3C0, 0x00);

    assert_eq!(vga.active_mode(), VideoMode::ModeX);
    assert_eq!(vga.raster_width(), 320, "320 active pixels per row");
    assert_eq!(vga.crtc.vtotal, 449, "449 total scanlines");
    assert_eq!(vga.crtc.vdisp_end, 370, "370 active scanlines");
    assert!(
        !vga.crtc.double_scan,
        "the guest cleared the double-scan: 370 source rows, not 185"
    );
    assert_eq!(vga.crtc.offset, 40, "80 bytes per plane per row");
    assert_eq!(
        vga.render_full_frame().display_height,
        370,
        "the host crops to the guest's 370 rows, not to 200"
    );
}

/// The game's second table, same order: the whole CRTC, then the chain-4 clear.
/// This one is Abrash's canonical 320x240 mode Y (Black Book Listing 47.1), so
/// it is the same register set `guest_crtc_bang_retunes_mode_x_to_320x240`
/// writes AFTER the unchain. Both orders must reach the same geometry.
#[test]
fn mode_y_crtc_bang_before_the_chain4_clear_survives_the_mode_x_entry() {
    let mut vga = Vga::default();
    vga.set_mode13h();
    // Mode 13h leaves the CRTC write protect set, and this table changes 06h and
    // 07h, so something must clear it first. The game's tall table does exactly
    // that (it writes 11h = 70h); this test clears it directly so the two tables
    // stay independent.
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x0E);
    vga.write_port(0x3C2, 0xE3); // Misc Output: 25.175 MHz dot clock
    for (idx, val) in [
        (0x00u8, 0x5Fu8),
        (0x01, 0x4F),
        (0x02, 0x50),
        (0x03, 0x82),
        (0x04, 0x54),
        (0x05, 0x80),
        (0x06, 0x0D), // vertical total: 527 lines
        (0x07, 0x3E), // overflow (high bits)
        (0x08, 0x00),
        (0x09, 0x41), // max scan line: 2 scanlines per row
        (0x10, 0xEA),
        (0x11, 0xAC), // vretrace end + write protect
        (0x12, 0xDF), // vertical display end: 480
        (0x13, 0x28),
        (0x14, 0x00),
        (0x15, 0xE7),
        (0x16, 0x06),
        (0x17, 0xE3),
    ] {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, val);
    }
    vga.write_port(0x3C4, 0x01);
    vga.write_port(0x3C5, 0x01);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x40);
    vga.write_port(0x3CE, 0x06);
    vga.write_port(0x3CF, 0x05);
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41);
    vga.write_port(0x3C0, 0x13);
    vga.write_port(0x3C0, 0x00);

    assert_eq!(vga.active_mode(), VideoMode::ModeX);
    assert_eq!(vga.crtc.vtotal, 527, "527 total scanlines");
    assert_eq!(vga.crtc.vdisp_end, 480, "480 active scanlines");
    assert!(
        vga.crtc.double_scan,
        "240 source rows over 480 scanlines"
    );
    assert_eq!(vga.raster_width(), 320);
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

/// A CRTC write that leaves the raster the same size must not erase the
/// scanlines the beam has already drawn.
///
/// Psycho Pinball rewrites its vertical CRTC group once per frame while a table
/// is in play, close to the end of the frame. Every one of those writes
/// reallocated the work raster to zeros, so the frame published at the next
/// vertical retrace held only the handful of lines drawn after the write. The
/// screen was black while video memory held the table: measured at 4109 wipes
/// of a drawn raster against 4361 published frames, and 5589 non-zero pixels
/// per published frame out of 117760.
///
/// Real silicon has no such buffer. Rewriting a register with the value it
/// already holds changes nothing the beam is doing.
#[test]
fn a_same_size_crtc_write_keeps_the_scanlines_already_drawn() {
    let mut vga = direct_mode_x(0);
    vga.vram.fill(0x2A);
    // One full frame, so the work raster holds the pattern on every line.
    vga.advance(vga.frame_dots());
    let full = vga
        .last_presented()
        .expect("a completed frame")
        .pixels
        .iter()
        .filter(|&&index| index != 0)
        .count();
    assert!(full > 0, "the pattern reaches the raster at all");

    // Draw all but the last line of the next frame, then rewrite a vertical
    // CRTC register with the value it already holds -- the guest's own table
    // replay. Only one line is left for the finalize to draw, so a wipe here
    // costs the whole picture.
    let line_dots = vga.frame_dots() / u64::from(vga.crtc.vtotal);
    vga.advance(vga.frame_dots());
    vga.advance(line_dots * u64::from(vga.crtc.vtotal - 1));
    vga.read_status1();
    assert_eq!(vga.last_line, vga.crtc.vtotal - 1);

    let unchanged = vga.crtc_regs.r12;
    let vtotal = vga.crtc.vtotal;
    let vdisp = vga.crtc.vdisp_end;
    vga.write_port(0x3D4, 0x12);
    vga.write_port(0x3D5, unchanged);
    assert_eq!(vga.crtc.vtotal, vtotal, "the write changed no geometry");
    assert_eq!(vga.crtc.vdisp_end, vdisp, "the write changed no geometry");

    finish_current_frame(&mut vga);
    let published = vga
        .last_presented()
        .expect("a completed frame")
        .pixels
        .iter()
        .filter(|&&index| index != 0)
        .count();
    assert_eq!(
        published, full,
        "a write that resizes nothing keeps every scanline already drawn"
    );
}

/// A mode change must not publish the previous mode's scanlines, even when the
/// two modes happen to share a raster size.
///
/// CGA 320x200 graphics and CGA 40x25 text are both 320x262, and the CGA
/// personality's mode-control path (port 3D8h) sets the mode and resizes the
/// work raster WITHOUT resetting the render cursor. Reusing a same-size buffer
/// there would carry the old mode's rows into the first published frame of the
/// new one -- the classic flash of the previous screen. Keeping the raster is
/// right for a timing recompute at a CONSTANT mode; it is wrong across a mode
/// change, and the two predicates are not the same.
#[test]
fn a_mode_change_at_the_same_raster_size_does_not_publish_the_old_rows() {
    let mut vga = Vga::default();
    assert!(vga.set_cga_mode(0x04)); // 320x200 graphics, 320x262 raster
    let size = (vga.raster_width(), vga.raster_height());
    // Fill both interleaved banks so every graphics scanline renders non-zero.
    for offset in 0..CGA_FB_SIZE {
        vga.cga_write(offset, 0b11_11_11_11);
    }
    vga.advance(vga.frame_dots());
    assert!(
        vga.last_presented()
            .expect("a graphics frame")
            .pixels
            .iter()
            .any(|&index| index != 0),
        "the CGA framebuffer reaches the raster"
    );

    // 100 lines into the next frame, switch to 40x25 text through 3D8h: video
    // enabled, graphics bit CLEAR, 40 columns. The text page is blank and its
    // attributes are zero, so every correctly rendered row is black.
    let line_dots = vga.frame_dots() / u64::from(vga.crtc.vtotal);
    vga.advance(vga.frame_dots());
    vga.advance(line_dots * 100);
    vga.read_status1();
    assert_eq!(vga.last_line, 100, "the beam drew 100 graphics rows");

    vga.write_port(0x3D8, 0x08);
    assert_eq!(vga.active_mode(), VideoMode::Text);
    assert_eq!(
        (vga.raster_width(), vga.raster_height()),
        size,
        "the two modes share a raster size, which is the whole point"
    );

    finish_current_frame(&mut vga);
    let frame = vga.last_presented().expect("a text frame");
    // Rows 0-99 are the ones the beam drew in the OLD mode. They must not reach
    // the published frame. Rows 100 upwards were drawn after the switch and are
    // the new mode's own output -- lit here because B8000 is shared between the
    // CGA framebuffer and the text page, so the fill above is now text.
    let width = frame.width as usize;
    let stale: Vec<usize> = (0..100)
        .filter(|row| {
            frame.pixels[row * width..(row + 1) * width]
                .iter()
                .any(|&index| index != 0)
        })
        .collect();
    assert!(
        stale.is_empty(),
        "rows drawn in the old mode reached the published frame: {stale:?}"
    );
}

#[test]
fn mode_x_direct_write_page_tracks_the_selected_plane() {
    let mut vga = direct_mode_x(0);
    let plane0 = vga.vram.as_mut_ptr();
    assert_eq!(vga.direct_write_token(), 2);
    assert_eq!(
        vga.direct_write_page_ptr(0x1000),
        Some(plane0.wrapping_add(0x1000))
    );

    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x04);
    assert_eq!(vga.direct_write_token(), 4);
    assert_eq!(
        vga.direct_write_page_ptr(0x1000),
        Some(plane0.wrapping_add(2 * VGA_PLANE_SIZE + 0x1000))
    );
}

#[test]
fn mode_x_direct_write_requires_the_transparent_planar_datapath() {
    let cases: &[(&str, u16, u8, u8)] = &[
        ("chain-4 disabled", 0x3C4, 0x04, 0x0E),
        ("sequential addressing", 0x3C4, 0x04, 0x02),
        ("one map-mask plane", 0x3C4, 0x02, 0x03),
        ("write mode zero", 0x3CE, 0x05, 0x01),
        ("rotate zero", 0x3CE, 0x03, 0x01),
        ("logical replace", 0x3CE, 0x03, 0x08),
        ("set/reset disabled", 0x3CE, 0x01, 0x01),
        ("full bit mask", 0x3CE, 0x08, 0xFE),
        ("A000 aperture", 0x3CE, 0x06, 0x09),
        ("graphics aperture", 0x3CE, 0x06, 0x04),
    ];
    for &(name, index_port, index, value) in cases {
        let mut vga = direct_mode_x(0);
        vga.write_port(index_port, index);
        vga.write_port(index_port + 1, value);
        assert_eq!(vga.mode_x_direct_write_plane(), None, "{name}");
    }

    let mut no_plane = direct_mode_x(0);
    no_plane.write_port(0x3C4, 0x02);
    no_plane.write_port(0x3C5, 0x00);
    assert_eq!(no_plane.mode_x_direct_write_plane(), None);

    let mut subsystem_off = direct_mode_x(0);
    subsystem_off.write_port(0x3C3, 0x00);
    assert_eq!(subsystem_off.mode_x_direct_write_plane(), None);

    let mut memory_off = direct_mode_x(0);
    memory_off.write_port(0x3C2, memory_off.misc_output & !0x02);
    assert_eq!(memory_off.mode_x_direct_write_plane(), None);
}

#[test]
fn mode_x_direct_write_batch_invalidates_linear_cache_and_bumps_once() {
    let mut vga = direct_mode_x(3);
    let before = vga.content_gen();
    assert!(vga.mode13_linear_valid);

    vga.note_direct_write_pages(0b11);
    assert!(!vga.mode13_linear_valid);
    assert_eq!(vga.content_gen(), before);
    vga.finish_direct_write_batch();
    assert_eq!(vga.content_gen(), before + 1);
    vga.finish_direct_write_batch();
    assert_eq!(vga.content_gen(), before + 1);
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
    // Checks render_256color_row's row_base arithmetic directly. The machine test
    // exercises the start-address vretrace latch end to end.
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
