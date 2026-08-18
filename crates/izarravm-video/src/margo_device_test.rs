// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn reports_identity_caps_and_display() {
    let mut margo = Margo::default();
    assert_eq!(read_reg_u32(&margo, REG_ID), MARGO_ID_VALUE);
    assert_eq!(read_reg_u32(&margo, REG_CAPS), MARGO_CAPS_VALUE);

    margo.set_mode_640x480x8();
    assert_eq!(read_reg_u32(&margo, REG_DISP_WIDTH), 640);
    assert_eq!(read_reg_u32(&margo, REG_DISP_HEIGHT), 480);
    assert_eq!(read_reg_u32(&margo, REG_DISP_BPP), 8);
    assert_eq!(read_reg_u32(&margo, REG_DISP_PITCH), 640);
}

#[test]
fn disp_start_is_writable_byte_by_byte() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    // Distinct values in every lane prove the byte recombination, not just
    // a single shift.
    margo.write_mmio_u8(REG_DISP_START, 0x01);
    margo.write_mmio_u8(REG_DISP_START + 1, 0x02);
    margo.write_mmio_u8(REG_DISP_START + 2, 0x03);
    margo.write_mmio_u8(REG_DISP_START + 3, 0x04);
    assert_eq!(read_reg_u32(&margo, REG_DISP_START), 0x0403_0201);
    assert_eq!(margo.display().start, 0, "scanout keeps the active latch");
    assert!(margo.display_start_pending());
    margo.advance_frames(1);
    assert_eq!(margo.display().start, 0x0403_0201);
    assert!(!margo.display_start_pending());
}

#[test]
fn checked_display_start_requires_a_complete_visible_page() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    let page = 640usize * 480;

    assert!(margo.program_display_start(page as u32));
    assert!(margo.display_start_pending());
    assert!(!margo.program_display_start((MARGO_VRAM_SIZE - page + 1) as u32));

    margo.advance_frames(1);
    assert_eq!(margo.display().start, page as u32);
}

#[test]
fn disp_dimensions_are_read_only_to_the_bus() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    margo.write_mmio_u8(REG_DISP_WIDTH, 0); // ignored
    assert_eq!(read_reg_u32(&margo, REG_DISP_WIDTH), 640);
}

#[test]
fn vram_reads_and_writes() {
    let mut margo = Margo::default();
    margo.write_vram_u8(100, 0xab);
    assert_eq!(margo.read_vram_u8(100), 0xab);
    assert_eq!(margo.vram().len(), MARGO_VRAM_SIZE);
}

#[test]
fn visible_surface_tracks_the_mode() {
    let mut margo = Margo::default();
    assert!(margo.visible_surface().is_empty()); // no mode set yet

    margo.set_mode_640x480x8();
    margo.write_vram_u8(0, 0x11);
    let last = 640 * 480 - 1;
    margo.write_vram_u8(last, 0x22);
    // A byte just past the visible surface must not appear in it.
    margo.write_vram_u8(640 * 480, 0x33);

    let surface = margo.visible_surface();
    assert_eq!(surface.len(), 640 * 480);
    assert_eq!(surface[0], 0x11);
    assert_eq!(surface[last], 0x22);
}

#[test]
fn set_mode_looks_up_the_table() {
    let mut margo = Margo::default();
    assert!(margo.set_mode(0x103));
    assert_eq!(margo.display().mode, 0x103);
    assert_eq!(margo.display().width, 800);
    assert_eq!(margo.display().height, 600);
    assert_eq!(margo.display().bpp, 8);
    assert_eq!(margo.display().pitch, 800);
}

#[test]
fn set_mode_rejects_modes_outside_the_table() {
    let mut margo = Margo::default();
    assert!(!margo.set_mode(0x112)); // 640x480x24 packed, not in the table
    assert_eq!(margo.display(), MargoDisplay::default());
}

#[test]
fn vbe_mode_lookup_finds_table_entries() {
    assert_eq!(
        vbe_mode(0x105).map(|m| (m.width, m.height)),
        Some((1024, 768))
    );
    assert!(vbe_mode(0x999).is_none());
}

#[test]
fn overlay_registers_round_trip() {
    let mut margo = Margo::default();
    // Distinct values in each lane prove byte recombination through the store.
    margo.write_mmio_u8(REG_OVL_CTRL, 0x11);
    margo.write_mmio_u8(REG_OVL_CTRL + 1, 0x22);
    margo.write_mmio_u8(REG_OVL_CTRL + 2, 0x33);
    margo.write_mmio_u8(REG_OVL_CTRL + 3, 0x44);
    assert_eq!(read_reg_u32(&margo, REG_OVL_CTRL), 0x4433_2211);
    // Registers across the block are independent.
    write_reg(&mut margo, REG_OVL_SRC_Y, 0x0020_0000);
    write_reg(&mut margo, REG_OVL_COLORKEY, 0x00ff_00ff);
    assert_eq!(read_reg_u32(&margo, REG_OVL_SRC_Y), 0x0020_0000);
    assert_eq!(read_reg_u32(&margo, REG_OVL_COLORKEY), 0x00ff_00ff);
    assert_eq!(read_reg_u32(&margo, REG_OVL_CTRL), 0x4433_2211);
}

#[test]
fn pusher_registers_round_trip() {
    let mut margo = Margo::default();
    // Distinct values in each lane prove byte recombination through the store.
    margo.write_mmio_u8(REG_PUSH_CTRL, 0x11);
    margo.write_mmio_u8(REG_PUSH_CTRL + 1, 0x22);
    margo.write_mmio_u8(REG_PUSH_CTRL + 2, 0x33);
    margo.write_mmio_u8(REG_PUSH_CTRL + 3, 0x44);
    assert_eq!(read_reg_u32(&margo, REG_PUSH_CTRL), 0x4433_2211);
    // The other R/W registers are independent across the block.
    write_reg(&mut margo, REG_PUSH_BASE, 0x0001_0000);
    write_reg(&mut margo, REG_PUSH_SIZE, 0x0000_1000);
    write_reg(&mut margo, REG_PUSH_PUT, 0x0000_0040);
    assert_eq!(read_reg_u32(&margo, REG_PUSH_BASE), 0x0001_0000);
    assert_eq!(read_reg_u32(&margo, REG_PUSH_SIZE), 0x0000_1000);
    assert_eq!(read_reg_u32(&margo, REG_PUSH_PUT), 0x0000_0040);
    // PUSH_GET is read-only to the bus: a CPU write is ignored.
    write_reg(&mut margo, REG_PUSH_GET, 0xdead_beef);
    assert_eq!(read_reg_u32(&margo, REG_PUSH_GET), 0);
}

#[test]
fn cursor_registers_round_trip() {
    let mut margo = Margo::default();
    // Distinct values in each lane prove byte recombination through the store.
    margo.write_mmio_u8(REG_CURSOR_POS, 0x11);
    margo.write_mmio_u8(REG_CURSOR_POS + 1, 0x22);
    margo.write_mmio_u8(REG_CURSOR_POS + 2, 0x33);
    margo.write_mmio_u8(REG_CURSOR_POS + 3, 0x44);
    assert_eq!(read_reg_u32(&margo, REG_CURSOR_POS), 0x4433_2211);
    // Each cursor register is independent.
    write_reg(&mut margo, REG_CURSOR_CTRL, 0x1);
    write_reg(&mut margo, REG_CURSOR_ADDR, 0x0001_0000);
    write_reg(&mut margo, REG_CURSOR_FG, 0x00ab);
    write_reg(&mut margo, REG_CURSOR_BG, 0x00cd);
    assert_eq!(read_reg_u32(&margo, REG_CURSOR_CTRL), 0x1);
    assert_eq!(read_reg_u32(&margo, REG_CURSOR_ADDR), 0x0001_0000);
    assert_eq!(read_reg_u32(&margo, REG_CURSOR_FG), 0x00ab);
    assert_eq!(read_reg_u32(&margo, REG_CURSOR_BG), 0x00cd);
    assert_eq!(read_reg_u32(&margo, REG_CURSOR_POS), 0x4433_2211); // unchanged
}

#[test]
fn set_mode_pitch_uses_whole_byte_pixels() {
    let mut margo = Margo::default();
    margo.set_mode(0x110); // 640x480x15
    assert_eq!(margo.display().bpp, 15);
    assert_eq!(margo.display().pitch, 1280); // 640 * 2, not 640 * 15 / 8
    margo.set_mode(0x111); // 640x480x16
    assert_eq!(margo.display().pitch, 1280);
    margo.set_mode(0x14a); // 640x480x32
    assert_eq!(margo.display().pitch, 2560);
}

#[test]
fn pixel_format_describes_direct_color_layouts() {
    assert!(pixel_format(8).is_none()); // indexed, not direct color
    let f16 = pixel_format(16).unwrap();
    assert_eq!((f16.r.pos, f16.r.size), (11, 5));
    assert_eq!((f16.g.pos, f16.g.size), (5, 6));
    assert_eq!((f16.b.pos, f16.b.size), (0, 5));
    let f15 = pixel_format(15).unwrap();
    assert_eq!((f15.r.pos, f15.r.size), (10, 5));
    assert_eq!((f15.x.pos, f15.x.size), (15, 1));
    let f32 = pixel_format(32).unwrap();
    assert_eq!((f32.r.pos, f32.r.size), (16, 8));
    assert_eq!((f32.x.pos, f32.x.size), (24, 8));
}

#[test]
fn decode_argb_handles_each_format() {
    let palette = {
        let mut p = [0u32; 256];
        p[7] = 0x0012_3456;
        p
    };
    // 8bpp indexed: straight palette lookup.
    assert_eq!(decode_argb(8, 7, &palette), 0x0012_3456);
    // 16bpp R5G6B5: red, green, blue, white, black.
    assert_eq!(decode_argb(16, 0xf800, &palette), 0x00ff_0000);
    assert_eq!(decode_argb(16, 0x07e0, &palette), 0x0000_ff00);
    assert_eq!(decode_argb(16, 0x001f, &palette), 0x0000_00ff);
    assert_eq!(decode_argb(16, 0xffff, &palette), 0x00ff_ffff);
    assert_eq!(decode_argb(16, 0x0000, &palette), 0x0000_0000);
    // 15bpp X1R5G5B5: red, green, blue; the X bit is ignored.
    assert_eq!(decode_argb(15, 0x7c00, &palette), 0x00ff_0000);
    assert_eq!(decode_argb(15, 0x03e0, &palette), 0x0000_ff00);
    assert_eq!(decode_argb(15, 0x001f, &palette), 0x0000_00ff);
    assert_eq!(decode_argb(15, 0x8000 | 0x7c00, &palette), 0x00ff_0000);
    // 32bpp X8R8G8B8: the X byte is ignored.
    assert_eq!(decode_argb(32, 0x0034_5678, &palette), 0x0034_5678);
    assert_eq!(decode_argb(32, 0xff34_5678, &palette), 0x0034_5678);
}

#[test]
fn scanout_argb_decodes_the_visible_surface() {
    let palette = [0u32; 256];
    let mut margo = Margo::default();
    // No mode set yet: empty scanout.
    assert!(margo.scanout_argb(&palette).is_empty());

    margo.set_mode(0x111); // 640x480x16, pitch 1280
    // Red pixel at (3, 2): offset 2*1280 + 3*2 = 2566; R5G6B5 red = 0xf800 LE.
    margo.write_vram_u8(2566, 0x00);
    margo.write_vram_u8(2567, 0xf8);
    // Green pixel at (0, 0): 0x07e0 LE.
    margo.write_vram_u8(0, 0xe0);
    margo.write_vram_u8(1, 0x07);

    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb.len(), 640 * 480);
    assert_eq!(argb[2 * 640 + 3], 0x00ff_0000);
    assert_eq!(argb[0], 0x0000_ff00);
    assert_eq!(argb[1], 0x0000_0000); // untouched pixel
}

#[test]
fn scanout_argb_decodes_32bpp_pixels() {
    let palette = [0u32; 256];
    let mut margo = Margo::default();
    margo.set_mode(0x14a); // 640x480x32, pitch 2560
    // X8R8G8B8 0xff345678 at (1, 1): offset 1*2560 + 1*4 = 2564; X byte ignored.
    for (i, b) in 0xff34_5678u32.to_le_bytes().into_iter().enumerate() {
        margo.write_vram_u8(2564 + i, b);
    }
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb.len(), 640 * 480);
    assert_eq!(argb[640 + 1], 0x0034_5678);
}

#[test]
fn visible_writes_report_only_the_rows_that_changed() {
    let mut margo = Margo::default();
    assert!(margo.set_mode(0x101));
    let settled = margo.content_generation();

    margo.write_vram_u8(3 * 640 + 17, 0x2a);

    assert_eq!(
        margo.changed_rows_since(settled),
        std::iter::once(3..4).collect::<Vec<_>>()
    );
    let changed = margo.content_generation();
    margo.write_vram_u8(3 * 640 + 17, 0x2a);
    assert_eq!(margo.content_generation(), changed);
    assert!(margo.changed_rows_since(changed).is_empty());
}

#[test]
fn scanout_argb_rows_reuses_storage_and_leaves_clean_rows_untouched() {
    let mut palette = [0u32; 256];
    palette[0x2a] = 0x0011_2233;
    let mut margo = Margo::default();
    assert!(margo.set_mode(0x101));
    let mut argb = vec![0x00aa_55aa; 640 * 480];
    let capacity = argb.capacity();

    margo.write_vram_u8(3 * 640 + 17, 0x2a);
    let changed = 3..4;
    margo.scanout_argb_rows(&palette, std::slice::from_ref(&changed), &mut argb);

    assert_eq!(argb.capacity(), capacity);
    assert_eq!(argb[3 * 640 + 17], 0x0011_2233);
    assert_eq!(argb[2 * 640 + 17], 0x00aa_55aa);
    assert_eq!(argb[4 * 640 + 17], 0x00aa_55aa);
}

#[test]
fn cursor_composites_the_four_and_xor_results() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    // Identity palette: 8bpp index i decodes to ARGB i, so values are self-evident.
    let mut palette = [0u32; 256];
    for (i, slot) in palette.iter_mut().enumerate() {
        *slot = i as u32;
    }
    // Bitmap offscreen, beyond every mode's visible surface (1 MiB into VRAM).
    let addr = 0x10_0000u32;
    // Row 0, byte 0. AND plane sets cx2,cx3 (0x20|0x10=0x30); XOR plane sets cx1,cx3
    // (0x40|0x10=0x50). So cx0=(0,0)->BG, cx1=(0,1)->FG, cx2=(1,0)->transparent,
    // cx3=(1,1)->invert.
    margo.write_vram_u8(addr as usize, 0x30);
    margo.write_vram_u8(addr as usize + 512, 0x50);
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    write_reg(&mut margo, REG_CURSOR_POS, 0); // (0, 0)
    write_reg(&mut margo, REG_CURSOR_FG, 0x30);
    write_reg(&mut margo, REG_CURSOR_BG, 0x20);
    write_reg(&mut margo, REG_CURSOR_CTRL, 1); // ENABLE
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[0], 0x20); // cx0 (0,0) -> BG
    assert_eq!(argb[1], 0x30); // cx1 (0,1) -> FG
    assert_eq!(argb[2], 0x00); // cx2 (1,0) -> transparent (surface stays 0)
    assert_eq!(argb[3], 0x00ff_ffff); // cx3 (1,1) -> invert of 0
    assert_eq!(argb[64], 0x00); // sx=64 is outside the 64-wide cursor
}

#[test]
fn cursor_addresses_planes_msb_first() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    let mut palette = [0u32; 256];
    for (i, slot) in palette.iter_mut().enumerate() {
        *slot = i as u32;
    }
    let addr = 0x10_0000u32;
    // FG pixel at cursor (cx=9, cy=3): XOR byte = cy*8 + cx/8 = 25, mask = 0x80 >> 1
    // = 0x40. AND clear. BG = 0 so only the FG pixel differs from the surface.
    margo.write_vram_u8(addr as usize + 512 + 25, 0x40);
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    write_reg(&mut margo, REG_CURSOR_POS, 0);
    write_reg(&mut margo, REG_CURSOR_FG, 0x30);
    write_reg(&mut margo, REG_CURSOR_BG, 0x00);
    write_reg(&mut margo, REG_CURSOR_CTRL, 1);
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[3 * 640 + 9], 0x30); // (cx=9, cy=3) -> FG
    assert_eq!(argb[3 * 640 + 8], 0x00); // cx=8 (same byte, mask 0x80) is BG=0, not FG
}

#[test]
fn cursor_position_is_signed_and_clips_top_left() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    let mut palette = [0u32; 256];
    for (i, slot) in palette.iter_mut().enumerate() {
        *slot = i as u32;
    }
    let addr = 0x10_0000u32;
    // FG pixel at cursor (cx=5, cy=5): XOR byte = 5*8 + 0 = 40, mask = 0x80 >> 5 = 0x04.
    margo.write_vram_u8(addr as usize + 512 + 40, 0x04);
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    // Signed position x = -3, y = -2: cursor (5,5) -> screen (2, 3); the cursor's
    // top-left pixels map to negative screen coords and must be clipped, not wrapped.
    let pos = (((-2i32) as u16 as u32) << 16) | ((-3i32) as u16 as u32);
    write_reg(&mut margo, REG_CURSOR_POS, pos);
    write_reg(&mut margo, REG_CURSOR_FG, 0x30);
    write_reg(&mut margo, REG_CURSOR_BG, 0x00);
    write_reg(&mut margo, REG_CURSOR_CTRL, 1);
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[3 * 640 + 2], 0x30); // cursor (5,5) -> screen (2,3)
}

#[test]
fn cursor_clips_at_the_right_and_bottom_edges() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    let mut palette = [0u32; 256];
    for (i, slot) in palette.iter_mut().enumerate() {
        *slot = i as u32;
    }
    let addr = 0x10_0000u32;
    // FG (XOR set, AND clear) at cursor (0,0), (2,0), and (0,2).
    margo.write_vram_u8(addr as usize + 512, 0xa0); // XOR byte 0: cx0 (0x80) + cx2 (0x20)
    margo.write_vram_u8(addr as usize + 512 + 16, 0x80); // XOR byte 16: (cy=2, cx=0)
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    // Bottom-right corner. Cursor (0,0) -> (639,479) on-screen; (2,0) -> x=641 and
    // (0,2) -> y=481 are off-screen. If not clipped, those would index out of bounds
    // and panic, so a passing test proves the right/bottom clip.
    write_reg(&mut margo, REG_CURSOR_POS, (479 << 16) | 639);
    write_reg(&mut margo, REG_CURSOR_FG, 0x30);
    write_reg(&mut margo, REG_CURSOR_BG, 0x00);
    write_reg(&mut margo, REG_CURSOR_CTRL, 1);
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[479 * 640 + 639], 0x30); // the on-screen corner pixel is FG
}

#[test]
fn cursor_disabled_leaves_scanout_untouched() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    let mut palette = [0u32; 256];
    for (i, slot) in palette.iter_mut().enumerate() {
        *slot = i as u32;
    }
    let addr = 0x10_0000u32;
    margo.write_vram_u8(addr as usize, 0x30); // AND plane
    margo.write_vram_u8(addr as usize + 512, 0x50); // XOR plane
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    write_reg(&mut margo, REG_CURSOR_FG, 0x30);
    write_reg(&mut margo, REG_CURSOR_BG, 0x20);
    // CURSOR_CTRL left at 0 (disabled).
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[0], 0x00);
    assert_eq!(argb[1], 0x00);
}

#[test]
fn cursor_colors_decode_in_hi_color() {
    let mut margo = Margo::default();
    margo.set_mode(0x111); // 640x480x16, R5G6B5
    let palette = [0u32; 256]; // unused at 16bpp
    let addr = 0x10_0000u32;
    // FG pixel at cursor (0,0): XOR plane byte 0 bit 0x80 (cx0), AND clear.
    margo.write_vram_u8(addr as usize + 512, 0x80);
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    write_reg(&mut margo, REG_CURSOR_POS, 0);
    write_reg(&mut margo, REG_CURSOR_FG, 0xf800); // pure red in R5G6B5
    write_reg(&mut margo, REG_CURSOR_BG, 0x0000);
    write_reg(&mut margo, REG_CURSOR_CTRL, 1);
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[0], 0x00ff_0000); // FG decoded through the display format
}

#[test]
fn yuv_to_argb_matches_bt601_reference_vectors() {
    // Studio-swing ITU-R BT.601: Y 16..=235, chroma 16..=240.
    assert_eq!(yuv_to_argb(16, 128, 128), 0x0000_0000); // black
    assert_eq!(yuv_to_argb(235, 128, 128), 0x00ff_ffff); // white
    assert_eq!(yuv_to_argb(128, 128, 128), 0x0082_8282); // mid gray (130)
    assert_eq!(yuv_to_argb(235, 128, 255), 0x00ff_98ff); // red clamps high
    assert_eq!(yuv_to_argb(128, 255, 128), 0x0082_51ff); // blue clamps high
    assert_eq!(yuv_to_argb(16, 128, 16), 0x0000_5b00); // red clamps low to 0
}

#[test]
fn quantize_channel_truncates_and_dithers() {
    // 0x85 = 133. 5-bit (R/B): truncate 133>>3=16 -> expand 132 (0x84).
    assert_eq!(quantize_channel(133, 5, 0, false), 0x84);
    // 5-bit with the top Bayer cell: offset 15/2=7, (133+7)>>3=17 -> expand 140 (0x8C).
    assert_eq!(quantize_channel(133, 5, 15, true), 0x8c);
    // Dither cell 0 adds no offset, so it equals the truncated value.
    assert_eq!(quantize_channel(133, 5, 0, true), 0x84);
    // 6-bit (G in 16bpp): 133>>2=33 -> expand 134 (0x86).
    assert_eq!(quantize_channel(133, 6, 0, false), 0x86);
    // Clamp: 255 + offset saturates at 255, never overflows the 5-bit code.
    assert_eq!(quantize_channel(255, 5, 15, true), 0xff);
    // Zero stays zero.
    assert_eq!(quantize_channel(0, 5, 0, false), 0x00);
}

#[test]
fn cursor_skips_an_off_store_bitmap_without_panic() {
    let mut margo = Margo::default();
    margo.set_mode_640x480x8();
    let palette = [0u32; 256];
    // CURSOR_ADDR is 4 bytes from the end of the store: the AND plane bytes at
    // addr+0..+3 are in VRAM, but the XOR plane at addr+512 is off-store, so the
    // bounds check skips every cursor pixel as transparent. Must not panic or wrap.
    let addr = (MARGO_VRAM_SIZE as u32) - 4;
    write_reg(&mut margo, REG_CURSOR_ADDR, addr);
    write_reg(&mut margo, REG_CURSOR_POS, 0);
    write_reg(&mut margo, REG_CURSOR_FG, 0x30);
    write_reg(&mut margo, REG_CURSOR_BG, 0x20);
    write_reg(&mut margo, REG_CURSOR_CTRL, 1);
    let argb = margo.scanout_argb(&palette);
    assert_eq!(argb[0], 0x00); // xor plane off-store -> every pixel skipped -> surface 0
}

#[test]
fn proprietary_mode_0x150_is_320x240x8() {
    let mode = vbe_mode(0x150).expect("0x150 present in the mode table");
    assert_eq!((mode.width, mode.height, mode.bpp), (320, 240, 8));

    let mut margo = Margo::default();
    assert!(margo.set_mode(0x150), "set_mode(0x150) succeeds");
    let display = margo.display();
    assert_eq!((display.width, display.height, display.bpp), (320, 240, 8));
    assert_eq!(display.pitch, 320); // 320 * 1 byte per pixel
}
