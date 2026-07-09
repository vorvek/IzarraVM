// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
