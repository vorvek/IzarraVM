// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
