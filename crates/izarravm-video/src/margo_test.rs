// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn read_reg_u32(margo: &Margo, offset: usize) -> u32 {
    (0..4)
        .map(|i| u32::from(margo.read_mmio_u8(offset + i)) << (8 * i))
        .fold(0, |a, b| a | b)
}

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
    // Distinct values in every lane prove the byte recombination, not just
    // a single shift.
    margo.write_mmio_u8(REG_DISP_START, 0x01);
    margo.write_mmio_u8(REG_DISP_START + 1, 0x02);
    margo.write_mmio_u8(REG_DISP_START + 2, 0x03);
    margo.write_mmio_u8(REG_DISP_START + 3, 0x04);
    assert_eq!(read_reg_u32(&margo, REG_DISP_START), 0x0403_0201);
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
fn blit_registers_round_trip() {
    let mut margo = Margo::default();
    // Distinct values in each lane prove byte recombination.
    margo.write_mmio_u8(REG_DST_BASE, 0x11);
    margo.write_mmio_u8(REG_DST_BASE + 1, 0x22);
    margo.write_mmio_u8(REG_DST_BASE + 2, 0x33);
    margo.write_mmio_u8(REG_DST_BASE + 3, 0x44);
    assert_eq!(read_reg_u32(&margo, REG_DST_BASE), 0x4433_2211);

    // A different blit register is independent.
    margo.write_mmio_u8(REG_FG_COLOR, 0xab);
    assert_eq!(read_reg_u32(&margo, REG_FG_COLOR), 0x0000_00ab);
    assert_eq!(read_reg_u32(&margo, REG_DST_BASE), 0x4433_2211);
}

#[test]
fn fill_writes_a_solid_rectangle_depth_1() {
    let mut vram = vec![0u8; 64];
    // pitch 8, 2x2 rectangle at (x=1, y=1), color 0xAB, solid (ROP 0xF0).
    let p = FillParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        dst_x: 1,
        dst_y: 1,
        width: 2,
        height: 2,
        fg_color: 0x0000_00ab,
        rop: 0xf0,
        clip: Clip::default(),
    };
    let pixels = fill(&mut vram, &p);
    assert_eq!(pixels, 4);
    // Rows y=1 and y=2, columns x=1,2 -> offsets 9,10 and 17,18.
    assert_eq!(vram[9], 0xab);
    assert_eq!(vram[10], 0xab);
    assert_eq!(vram[17], 0xab);
    assert_eq!(vram[18], 0xab);
    // Neighbours stay zero.
    assert_eq!(vram[8], 0x00);
    assert_eq!(vram[11], 0x00);
}

#[test]
fn fill_writes_depth_2_and_4_pixels() {
    let mut vram = vec![0u8; 64];
    // depth 2: one pixel at (0,0), color 0x1234 -> low 2 bytes little-endian.
    let p2 = FillParams {
        dst_base: 0,
        dst_pitch: 16,
        depth: 2,
        dst_x: 0,
        dst_y: 0,
        width: 1,
        height: 1,
        fg_color: 0x0000_1234,
        rop: 0xf0,
        clip: Clip::default(),
    };
    fill(&mut vram, &p2);
    assert_eq!(vram[0], 0x34);
    assert_eq!(vram[1], 0x12);
    assert_eq!(vram[2], 0x00);

    // depth 4: one pixel at offset 16, color 0xDEADBEEF.
    let p4 = FillParams {
        dst_base: 16,
        dst_pitch: 16,
        depth: 4,
        dst_x: 0,
        dst_y: 0,
        width: 1,
        height: 1,
        fg_color: 0xdead_beef,
        rop: 0xf0,
        clip: Clip::default(),
    };
    fill(&mut vram, &p4);
    assert_eq!(&vram[16..20], &[0xef, 0xbe, 0xad, 0xde]);
}

#[test]
fn fill_xor_rop_inverts_the_destination() {
    let mut vram = vec![0xffu8; 16];
    let p = FillParams {
        dst_base: 0,
        dst_pitch: 4,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 2,
        height: 1,
        fg_color: 0x0000_000f,
        rop: 0x5a, // PATINVERT: dst ^= fg
        clip: Clip::default(),
    };
    fill(&mut vram, &p);
    assert_eq!(vram[0], 0xf0); // 0xff ^ 0x0f
    assert_eq!(vram[1], 0xf0);
    assert_eq!(vram[2], 0xff); // outside the 2-wide rect
}

#[test]
fn fill_skips_out_of_bounds_without_wrapping() {
    let mut vram = vec![0u8; 16];
    // A rectangle that runs off the end of the store. base 14, pitch 4,
    // depth 1, 4 wide x 1 high -> offsets 14,15,16,17. 16 and 17 are out.
    let p = FillParams {
        dst_base: 14,
        dst_pitch: 4,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 4,
        height: 1,
        fg_color: 0x0000_0077,
        rop: 0xf0,
        clip: Clip::default(),
    };
    fill(&mut vram, &p);
    assert_eq!(vram[14], 0x77);
    assert_eq!(vram[15], 0x77);
    assert_eq!(vram[0], 0x00); // not wrapped to the start
}

#[test]
fn fill_rejects_invalid_depth() {
    let mut vram = vec![0u8; 16];
    let p = FillParams {
        dst_base: 0,
        dst_pitch: 4,
        depth: 3, // not 1, 2, or 4
        dst_x: 0,
        dst_y: 0,
        width: 2,
        height: 2,
        fg_color: 0x0000_00ff,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(fill(&mut vram, &p), 0);
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn fill_caps_iterations_at_the_store_size() {
    let mut vram = vec![0u8; 16];
    // A pathological DIM must not spin: capped at vram.len() iterations.
    let p = FillParams {
        dst_base: 0,
        dst_pitch: 4,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 4000,
        height: 4000,
        fg_color: 0x0000_0001,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(fill(&mut vram, &p), 16);
}

#[test]
fn fill_skips_extreme_coordinates_without_overflow() {
    let mut vram = vec![0u8; 64];
    // Adversarial guest registers: every pixel is far out of the store.
    // Must not panic; nothing is written.
    let p = FillParams {
        dst_base: u32::MAX,
        dst_pitch: u32::MAX,
        depth: 4,
        dst_x: u32::MAX,
        dst_y: u32::MAX,
        width: 8,
        height: 8,
        fg_color: 0xdead_beef,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(fill(&mut vram, &p), 0);
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn pattern_adjacent_fills_tile_seamlessly() {
    // Two 8x1 fills on the same row, the second starting at x=8. Because the phase
    // is absolute, x=8 has phase 8 & 7 = 0, the same column as x=0, so the two
    // fills meet as one continuous tile grid.
    let mut vram = vec![0u8; 256];
    let pat_base = 128usize;
    for r in 0..8 {
        for c in 0..8 {
            vram[pat_base + r * 8 + c] = (r * 8 + c + 1) as u8;
        }
    }
    let fill_a = PatternParams {
        dst_base: 0,
        dst_pitch: 64,
        pat_base: pat_base as u32,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 8,
        height: 1,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &fill_a), 8);
    let fill_b = PatternParams {
        dst_base: 0,
        dst_pitch: 64,
        pat_base: pat_base as u32,
        depth: 1,
        dst_x: 8,
        dst_y: 0,
        width: 8,
        height: 1,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &fill_b), 8);
    // Tile row 0 is values 1..=8; columns 0..16 are that row repeated twice.
    let expected: Vec<u8> = (0..16u32).map(|x| ((x & 7) + 1) as u8).collect();
    assert_eq!(&vram[0..16], &expected[..]);
}

#[test]
fn pattern_patinvert_xors_the_pattern_into_the_destination() {
    // ROP 0x5A = D ^ P. A uniform tile of 0x0F over a 0xFF destination yields 0xF0.
    let mut vram = vec![0xffu8; 256];
    let pat_base = 128usize;
    for b in 0..64 {
        vram[pat_base + b] = 0x0f;
    }
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 8,
        pat_base: pat_base as u32,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 4,
        height: 1,
        rop: 0x5a,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 4);
    assert_eq!(&vram[0..4], &[0xf0, 0xf0, 0xf0, 0xf0]); // 0xff ^ 0x0f
    assert_eq!(vram[4], 0xff); // outside the 4-wide rect
}

#[test]
fn pattern_colorkey_skips_matching_pattern_pixels() {
    // A hatch: column 0 of every tile row is 0xAA (the stroke), the rest 0x11 (the
    // background). With COLORKEY = 0x11 and COLORKEY_EN, only the strokes paint;
    // the background keys through, leaving the destination untouched.
    let mut vram = vec![0u8; 256];
    let pat_base = 128usize;
    for r in 0..8 {
        for c in 0..8 {
            vram[pat_base + r * 8 + c] = if c == 0 { 0xaa } else { 0x11 };
        }
    }
    vram[0..8].fill(0x55); // pre-set the destination row so a kept pixel is visible
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 8,
        pat_base: pat_base as u32,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 8,
        height: 1,
        rop: 0xf0,
        colorkey: 0x11,
        colorkey_en: true,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 1); // only column 0 (0xAA) is written
    assert_eq!(vram[0], 0xaa); // stroke painted
    assert_eq!(&vram[1..8], &[0x55; 7]); // background kept (keyed through)
}

#[test]
fn pattern_clip_confines_the_fill() {
    // Tile cell (r, c) = r*8 + c + 1. Clip to x in [1, 3): only columns 1 and 2 of
    // the destination row are written, each with its absolute-phase tile value.
    let mut vram = vec![0u8; 256];
    let pat_base = 128usize;
    for r in 0..8 {
        for c in 0..8 {
            vram[pat_base + r * 8 + c] = (r * 8 + c + 1) as u8;
        }
    }
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 8,
        pat_base: pat_base as u32,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 4,
        height: 1,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip {
            enabled: true,
            x0: 1,
            y0: 0,
            x1: 3,
            y1: 1,
        },
    };
    assert_eq!(pattern(&mut vram, &p), 2);
    assert_eq!(vram[0], 0); // x=0 clipped out
    assert_eq!(vram[1], 2); // (1,0) tile[0][1] = 0*8+1+1
    assert_eq!(vram[2], 3); // (2,0) tile[0][2]
    assert_eq!(vram[3], 0); // x=3 clipped out (BR exclusive)
}

#[test]
fn pattern_tiles_depth_2_pixels() {
    // depth 2: pattern row pitch is 8 * 2 = 16 bytes. Tile cell (r, c) holds the
    // 16-bit value 0x1000 | (r*8 + c). A 2x2 fill proves the row pitch, the
    // little-endian read, and the phase across both axes at depth 2.
    let mut vram = vec![0u8; 512];
    let pat_base = 256usize;
    for r in 0..8 {
        for c in 0..8 {
            let v: u16 = 0x1000 | (r * 8 + c) as u16;
            let off = pat_base + r * 16 + c * 2;
            vram[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }
    }
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 4,
        pat_base: pat_base as u32,
        depth: 2,
        dst_x: 0,
        dst_y: 0,
        width: 2,
        height: 2,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 4);
    assert_eq!(&vram[0..2], &0x1000u16.to_le_bytes()); // (0,0) tile[0][0]
    assert_eq!(&vram[2..4], &0x1001u16.to_le_bytes()); // (1,0) tile[0][1]
    assert_eq!(&vram[4..6], &0x1008u16.to_le_bytes()); // (0,1) tile[1][0]
    assert_eq!(&vram[6..8], &0x1009u16.to_le_bytes()); // (1,1) tile[1][1]
}

#[test]
fn pattern_tiles_depth_4_pixels() {
    // depth 4: pattern row pitch is 8 * 4 = 32 bytes. Tile cell (r, c) holds the
    // 32-bit value 0x1000_0000 | (r*8 + c). A 2x2 fill proves the row pitch, the
    // little-endian read of all four bytes, and the phase across both axes at
    // depth 4.
    let mut vram = vec![0u8; 1024];
    let pat_base = 256usize;
    for r in 0..8 {
        for c in 0..8 {
            let v: u32 = 0x1000_0000 | (r * 8 + c) as u32;
            let off = pat_base + r * 32 + c * 4;
            vram[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 8,
        pat_base: pat_base as u32,
        depth: 4,
        dst_x: 0,
        dst_y: 0,
        width: 2,
        height: 2,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 4);
    assert_eq!(&vram[0..4], &0x1000_0000u32.to_le_bytes()); // (0,0) tile[0][0]
    assert_eq!(&vram[4..8], &0x1000_0001u32.to_le_bytes()); // (1,0) tile[0][1]
    assert_eq!(&vram[8..12], &0x1000_0008u32.to_le_bytes()); // (0,1) tile[1][0]
    assert_eq!(&vram[12..16], &0x1000_0009u32.to_le_bytes()); // (1,1) tile[1][1]
}

#[test]
fn pattern_skips_out_of_store_pixels_without_wrapping() {
    // Store 16 bytes. Tile row 0 lives at bytes 8..16 (cells 1..=8); rows 1..7 are
    // off-store but unused here. dst_base 14, pitch 4, 4-wide: offsets 14, 15 are
    // in; 16, 17 are out and skipped, not wrapped to the start.
    let mut vram = vec![0u8; 16];
    for c in 0..8 {
        vram[8 + c] = (c + 1) as u8;
    }
    let p = PatternParams {
        dst_base: 14,
        dst_pitch: 4,
        pat_base: 8,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 4,
        height: 1,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 2);
    assert_eq!(vram[14], 1); // (0,0) tile[0][0]
    assert_eq!(vram[15], 2); // (1,0) tile[0][1]
    assert_eq!(vram[0], 0); // not wrapped to the start
}

#[test]
fn pattern_rejects_invalid_depth() {
    let mut vram = vec![0u8; 64];
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 8,
        pat_base: 0,
        depth: 3, // not 1, 2, or 4
        dst_x: 0,
        dst_y: 0,
        width: 2,
        height: 2,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 0);
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn pattern_skips_extreme_coordinates_without_overflow() {
    // Adversarial guest registers: base, pitch, pat_base, and coordinates all
    // u32::MAX. Must not panic; nothing is written.
    let mut vram = vec![0u8; 64];
    let p = PatternParams {
        dst_base: u32::MAX,
        dst_pitch: u32::MAX,
        pat_base: u32::MAX,
        depth: 4,
        dst_x: u32::MAX,
        dst_y: u32::MAX,
        width: 8,
        height: 8,
        rop: 0xf0,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 0);
    assert!(vram.iter().all(|&b| b == 0));
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

#[test]
fn pattern_tiles_with_surface_origin_phase() {
    // An 8x8 tile whose cell (r, c) holds value r*8 + c + 1 (1..=64, so no cell is
    // zero and a written pixel is always distinguishable from the cleared store).
    // Filling a 10x10 rectangle at DST_XY (3, 2) must pick the pattern cell from
    // the ABSOLUTE destination coordinate: pixel (x, y) -> tile[y & 7][x & 7]. If
    // the phase were relative to the fill's start, (3, 2) would be tile[0][0] = 1,
    // not tile[2][3] = 20.
    let mut vram = vec![0u8; 1024];
    let pat_base = 512usize; // clear of the destination rectangle (offsets 67..364)
    for r in 0..8 {
        for c in 0..8 {
            vram[pat_base + r * 8 + c] = (r * 8 + c + 1) as u8;
        }
    }
    let p = PatternParams {
        dst_base: 0,
        dst_pitch: 32,
        pat_base: pat_base as u32,
        depth: 1,
        dst_x: 3,
        dst_y: 2,
        width: 10,
        height: 10,
        rop: 0xf0, // PATCOPY: result = P
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(pattern(&mut vram, &p), 100);
    assert_eq!(vram[2 * 32 + 3], 20); // (3,2)  tile[2][3] = 2*8+3+1
    assert_eq!(vram[2 * 32 + 10], 19); // (10,2) tile[2][2], x wraps at 8
    assert_eq!(vram[9 * 32 + 3], 12); // (3,9)  tile[1][3], y wraps at 8
    assert_eq!(vram[11 * 32 + 11], 28); // (11,11) tile[3][3]
    assert_eq!(vram[2 * 32 + 2], 0); // (2,2) left of dst_x, untouched
    assert_eq!(vram[32 + 3], 0); // (3,1) above dst_y, untouched
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
fn command_line_draws_and_sets_busy() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_LINE_START, 0); // (0,0)
    write_reg(&mut margo, REG_LINE_END, 3); // (3,0): horizontal 4-pixel line
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_ROP, 0xf0);
    write_reg(&mut margo, REG_COMMAND, 0x05); // LINE

    for off in 0..4 {
        assert_eq!(margo.read_vram_u8(off), 0xab);
    }
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
}

#[test]
fn command_line_busy_drains_at_the_line_rate() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_LINE_START, 0);
    write_reg(&mut margo, REG_LINE_END, 3); // 4-pixel line
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_ROP, 0xf0);
    write_reg(&mut margo, REG_COMMAND, 0x05);

    // 4 pixels -> busy_ns = 100 + 4*10 = 140. One ns short still reads busy.
    margo.advance_busy(139);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    margo.advance_busy(1);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn command_fill_writes_vram_and_sets_busy() {
    let mut margo = Margo::default();
    setup_fill(&mut margo);
    write_reg(&mut margo, REG_COMMAND, 0x01); // FILL

    // VRAM is filled immediately.
    assert_eq!(margo.read_vram_u8(9), 0xab); // y=1, x=1: pitch*y+x = 8+1
    assert_eq!(margo.read_vram_u8(18), 0xab); // y=2, x=2: 8*2+2
    // STATUS.BUSY is set.
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
}

#[test]
fn advance_busy_drains_to_idle() {
    let mut margo = Margo::default();
    setup_fill(&mut margo);
    write_reg(&mut margo, REG_COMMAND, 0x01);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);

    // 4 pixels: busy_ns = 100 + 4*5 = 120. One ns short still reads busy.
    margo.advance_busy(119);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    margo.advance_busy(1);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn unknown_command_is_a_no_op() {
    let mut margo = Margo::default();
    setup_fill(&mut margo);
    write_reg(&mut margo, REG_COMMAND, 0x07); // unused command code
    // No VRAM change and no busy time: offset 9 is the first pixel FILL would write.
    assert_eq!(margo.read_vram_u8(9), 0x00);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn control_reset_clears_busy() {
    let mut margo = Margo::default();
    setup_fill(&mut margo);
    write_reg(&mut margo, REG_COMMAND, 0x01);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);

    write_reg(&mut margo, REG_CONTROL, 0x01); // RESET
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
    // RESET is self-clearing.
    assert_eq!(read_reg_u32(&margo, REG_CONTROL) & 1, 0);
}

#[test]
fn command_copy_moves_vram_and_sets_busy() {
    let mut margo = Margo::default();
    margo.write_vram_u8(0, 0x55); // source pixel (0,0), pitch 8
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_SRC_BASE, 0);
    write_reg(&mut margo, REG_SRC_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, (1 << 16) | 4); // y=1, x=4
    write_reg(&mut margo, REG_SRC_XY, 0); // (0,0)
    write_reg(&mut margo, REG_DIM, (1 << 16) | 1); // 1x1
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x02); // COPY

    assert_eq!(margo.read_vram_u8(8 + 4), 0x55); // (4,1) got the source byte
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
}

#[test]
fn command_copy_busy_drains_at_the_copy_rate() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_SRC_BASE, 0);
    write_reg(&mut margo, REG_SRC_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 2 << 16); // y=2, x=0 (no overlap with src)
    write_reg(&mut margo, REG_SRC_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 2); // 2x1 = 2 pixels
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x02);

    // 2 pixels -> busy_ns = 100 + 2*10 = 120. One ns short still reads busy.
    margo.advance_busy(119);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    margo.advance_busy(1);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn copy_moves_a_non_overlapping_rectangle_depth_1() {
    // pitch 8. Source 2x2 at (0,0) holds distinct bytes; copy it to (4,2).
    let mut vram = vec![0u8; 64];
    vram[0] = 0xa1; // (0,0)
    vram[1] = 0xa2; // (1,0)
    vram[8] = 0xa3; // (0,1)
    vram[9] = 0xa4; // (1,1)
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 8,
        src_base: 0,
        src_pitch: 8,
        depth: 1,
        dst_x: 4,
        dst_y: 2,
        src_x: 0,
        src_y: 0,
        width: 2,
        height: 2,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    let pixels = copy(&mut vram, &p);
    assert_eq!(pixels, 4);
    // Destination (4,2)=20, (5,2)=21, (4,3)=28, (5,3)=29.
    assert_eq!(vram[20], 0xa1);
    assert_eq!(vram[21], 0xa2);
    assert_eq!(vram[28], 0xa3);
    assert_eq!(vram[29], 0xa4);
    // Source untouched.
    assert_eq!(vram[0], 0xa1);
}

#[test]
fn copy_moves_depth_2_and_4_pixels() {
    let mut vram = vec![0u8; 64];
    // depth 2: source pixel at (0,0) = 0x1234, copy to (4,0).
    vram[0] = 0x34;
    vram[1] = 0x12;
    let p2 = CopyParams {
        dst_base: 0,
        dst_pitch: 32,
        src_base: 0,
        src_pitch: 32,
        depth: 2,
        dst_x: 4,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 1,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p2), 1);
    assert_eq!(&vram[8..10], &[0x34, 0x12]); // (4,0) at depth 2 = offset 8

    // depth 4: source pixel at (0,0) = 0xDEADBEEF, copy to (2,0).
    let mut vram = vec![0u8; 64];
    vram[0..4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    let p4 = CopyParams {
        dst_base: 0,
        dst_pitch: 32,
        src_base: 0,
        src_pitch: 32,
        depth: 4,
        dst_x: 2,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 1,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p4), 1);
    assert_eq!(&vram[8..12], &[0xef, 0xbe, 0xad, 0xde]); // (2,0) at depth 4 = offset 8
}

#[test]
fn copy_color_key_skips_matching_source_pixels() {
    // Source row [0x05, 0x07] at (0,0); key 0x05 is transparent.
    let mut vram = vec![0u8; 32];
    vram[0] = 0x05;
    vram[1] = 0x07;
    // Pre-fill the destination so a skipped pixel is visibly left alone.
    vram[8] = 0xee;
    vram[9] = 0xee;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 8,
        src_base: 0,
        src_pitch: 8,
        depth: 1,
        dst_x: 0,
        dst_y: 1,
        src_x: 0,
        src_y: 0,
        width: 2,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0x05,
        colorkey_en: true,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p), 1); // only the non-keyed pixel written
    assert_eq!(vram[8], 0xee); // keyed source 0x05 -> destination untouched
    assert_eq!(vram[9], 0x07); // non-keyed source copied
}

#[test]
fn copy_color_key_matches_full_pixel_at_depth_2() {
    // depth 2, key 0x1234. A source pixel equal to the key is skipped; a pixel
    // sharing only the high byte is copied (proves the compare uses both bytes).
    let mut vram = vec![0u8; 32];
    // src (0,0) = 0x1234 (keyed), src (1,0) = 0x1299 (not keyed), pitch 16.
    vram[0] = 0x34;
    vram[1] = 0x12;
    vram[2] = 0x99;
    vram[3] = 0x12;
    // Destination row at y=1 pre-filled so a skip is visible.
    vram[16] = 0xee;
    vram[17] = 0xee;
    vram[18] = 0xee;
    vram[19] = 0xee;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 16,
        src_base: 0,
        src_pitch: 16,
        depth: 2,
        dst_x: 0,
        dst_y: 1,
        src_x: 0,
        src_y: 0,
        width: 2,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0x1234,
        colorkey_en: true,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p), 1); // only the non-keyed pixel written
    // Keyed pixel (0x1234) skipped -> destination (0,1) bytes untouched.
    assert_eq!(&vram[16..18], &[0xee, 0xee]);
    // Non-keyed pixel (0x1299) copied -> destination (1,1) bytes = 0x1299.
    assert_eq!(&vram[18..20], &[0x99, 0x12]);
}

#[test]
fn copy_skips_out_of_bounds_source_and_destination() {
    // Source partly off the store: src base 14, 4 wide at depth 1 -> offsets
    // 14,15,16,17; 16 and 17 are out, so only two pixels are readable.
    let mut vram = vec![0u8; 16];
    vram[14] = 0x71;
    vram[15] = 0x72;
    let p_src = CopyParams {
        dst_base: 0,
        dst_pitch: 16,
        src_base: 14,
        src_pitch: 16,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 4,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p_src), 2);
    assert_eq!(vram[0], 0x71);
    assert_eq!(vram[1], 0x72);

    // Destination partly off the store: same idea, dst base 14.
    let mut vram = vec![0u8; 16];
    vram[0] = 0x81;
    vram[1] = 0x82;
    vram[2] = 0x83;
    vram[3] = 0x84;
    let p_dst = CopyParams {
        dst_base: 14,
        dst_pitch: 16,
        src_base: 0,
        src_pitch: 16,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 4,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p_dst), 2); // offsets 14,15 in; 16,17 out
    assert_eq!(vram[14], 0x81);
    assert_eq!(vram[15], 0x82);
}

#[test]
fn copy_rejects_invalid_depth() {
    let mut vram = vec![0u8; 16];
    vram[0] = 0xaa;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 4,
        src_base: 0,
        src_pitch: 4,
        depth: 3, // not 1, 2, or 4
        dst_x: 1,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 1,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p), 0);
    assert_eq!(vram[1], 0x00); // nothing written
}

#[test]
fn copy_caps_iterations_at_the_store_size() {
    let mut vram = vec![0u8; 16];
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 4,
        src_base: 0,
        src_pitch: 4,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 4000,
        height: 4000,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    // A pathological DIM must not spin; the loop is capped at vram.len().
    // With src==dst every considered pixel is a no-op move, so written tracks
    // the in-bounds count up to the cap.
    assert_eq!(copy(&mut vram, &p), 16);
}

#[test]
fn copy_skips_extreme_coordinates_without_overflow() {
    let mut vram = vec![0u8; 64];
    let p = CopyParams {
        dst_base: u32::MAX,
        dst_pitch: u32::MAX,
        src_base: u32::MAX,
        src_pitch: u32::MAX,
        depth: 4,
        dst_x: u32::MAX,
        dst_y: u32::MAX,
        src_x: u32::MAX,
        src_y: u32::MAX,
        width: 8,
        height: 8,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p), 0); // must not panic; nothing written
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn copy_overlap_down_does_not_corrupt() {
    // pitch 4. Rows 0,1,2 hold distinct bytes. Copy the 4x2 rect at (0,0) down
    // one row to (0,1): row 1 must become row 0's old bytes, row 2 row 1's.
    let mut vram = vec![0u8; 32];
    for i in 0..4 {
        vram[i] = 1; // row 0
        vram[4 + i] = 2; // row 1
        vram[8 + i] = 3; // row 2
    }
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 4,
        src_base: 0,
        src_pitch: 4,
        depth: 1,
        dst_x: 0,
        dst_y: 1,
        src_x: 0,
        src_y: 0,
        width: 4,
        height: 2,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    copy(&mut vram, &p);
    assert_eq!(&vram[4..8], &[1, 1, 1, 1]); // row 1 = old row 0
    assert_eq!(&vram[8..12], &[2, 2, 2, 2]); // row 2 = old row 1, not corrupted
    assert_eq!(&vram[0..4], &[1, 1, 1, 1]); // row 0 untouched
}

#[test]
fn copy_overlap_right_does_not_corrupt() {
    // One row [1,2,3,4,5,6,7,8]. Copy the 4-wide rect at x=0 to x=1.
    let mut vram = vec![0u8; 8];
    for (i, slot) in vram.iter_mut().enumerate() {
        *slot = (i + 1) as u8;
    }
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 8,
        src_base: 0,
        src_pitch: 8,
        depth: 1,
        dst_x: 1,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 4,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    copy(&mut vram, &p);
    // Destination x=1..4 takes source x=0..3 = [1,2,3,4]; x=0 and x>=5 untouched.
    assert_eq!(&vram[0..8], &[1, 1, 2, 3, 4, 6, 7, 8]);
}

#[test]
fn copy_overlap_diagonal_does_not_corrupt() {
    // pitch 4. Copy the 3x2 rect at (0,0) to (1,1): both axes shift positive,
    // so both must traverse in reverse.
    let mut vram = vec![0u8; 16];
    // Row 0: [1,2,3,_], row 1: [4,5,6,_].
    vram[0] = 1;
    vram[1] = 2;
    vram[2] = 3;
    vram[4] = 4;
    vram[5] = 5;
    vram[6] = 6;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 4,
        src_base: 0,
        src_pitch: 4,
        depth: 1,
        dst_x: 1,
        dst_y: 1,
        src_x: 0,
        src_y: 0,
        width: 3,
        height: 2,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    copy(&mut vram, &p);
    // dst row 1 (offset 4) cols 1..4 = src row 0 [1,2,3]; dst row 2 (offset 8)
    // cols 1..4 = src row 1 [4,5,6].
    assert_eq!(&vram[5..8], &[1, 2, 3]);
    assert_eq!(&vram[9..12], &[4, 5, 6]);
    // Source row 0 is unchanged where the destination did not overwrite it.
    assert_eq!(vram[0], 1);
}

#[test]
fn color_expand_mem_expands_to_fg_and_bg_depth_1() {
    // Source byte 0xA0 = 1010_0000: cols 0 and 2 set, cols 1 and 3 clear
    // (MSB first). Expand a 4x1 rect; set bits take FG 0xAB, clear bits BG 0xCD.
    let mut vram = vec![0u8; 64];
    vram[0] = 0xa0; // monochrome source row, src_base 0
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 4);
    assert_eq!(&vram[16..20], &[0xab, 0xcd, 0xab, 0xcd]);
}

#[test]
fn color_expand_mem_transparent_skips_clear_bits() {
    // Same 0xA0 source; transparent leaves clear-bit destinations untouched.
    let mut vram = vec![0u8; 64];
    vram[0] = 0xa0;
    for slot in &mut vram[16..20] {
        *slot = 0xee; // pre-fill so a skip is visible
    }
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: true,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 2); // only the two set bits
    assert_eq!(&vram[16..20], &[0xab, 0xee, 0xab, 0xee]);
}

#[test]
fn color_expand_mem_handles_depth_2_and_4() {
    // depth 2: source 0x80 (col 0 set, col 1 clear) over a 2x1 rect.
    let mut vram = vec![0u8; 64];
    vram[0] = 0x80;
    let p2 = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 32,
            depth: 2,
            dst_x: 0,
            dst_y: 0,
            width: 2,
            height: 1,
            fg_color: 0x1234,
            bg_color: 0x5678,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p2), 2);
    assert_eq!(&vram[16..18], &[0x34, 0x12]); // col 0 -> FG, little-endian
    assert_eq!(&vram[18..20], &[0x78, 0x56]); // col 1 -> BG

    // depth 4: source 0x80 (col 0 set) over a 1x1 rect -> FG 0xDEADBEEF.
    let mut vram = vec![0u8; 64];
    vram[0] = 0x80;
    let p4 = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 32,
            depth: 4,
            dst_x: 0,
            dst_y: 0,
            width: 1,
            height: 1,
            fg_color: 0xdead_beef,
            bg_color: 0,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p4), 1);
    assert_eq!(&vram[16..20], &[0xef, 0xbe, 0xad, 0xde]);
}

#[test]
fn color_expand_mem_crosses_a_source_byte_boundary() {
    // src_x = 7 puts col 0 at bit 7 (byte 0, LSB) and col 1 at bit 8 (byte 1,
    // MSB). Byte 0 = 0x01 (col 0 set), byte 1 = 0x00 (col 1 clear).
    let mut vram = vec![0u8; 64];
    vram[0] = 0x01;
    vram[1] = 0x00;
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 2,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 2,
        src_x: 7,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 2);
    assert_eq!(&vram[16..18], &[0xab, 0xcd]); // col 0 set -> FG, col 1 clear -> BG
}

#[test]
fn color_expand_mem_skips_off_store_source_and_dest() {
    // Destination runs off the store: base 14, 4 wide at depth 1 -> dst offsets
    // 14,15,16,17; 16 and 17 are out. Source byte 0xF0 sets all four cols.
    let mut vram = vec![0u8; 16];
    vram[0] = 0xf0;
    let p_dst = ExpandMemParams {
        common: ExpandParams {
            dst_base: 14,
            dst_pitch: 16,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p_dst), 2); // offsets 14,15 in
    assert_eq!(vram[14], 0xab);
    assert_eq!(vram[15], 0xab);

    // Source off the store: src_base beyond the end -> every pixel skipped.
    let mut vram = vec![0u8; 16];
    let p_src = ExpandMemParams {
        common: ExpandParams {
            dst_base: 0,
            dst_pitch: 16,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 100,
        src_pitch: 16,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p_src), 0);
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn color_expand_mem_rejects_invalid_depth() {
    let mut vram = vec![0u8; 16];
    vram[0] = 0xff;
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 0,
            dst_pitch: 4,
            depth: 3, // not 1, 2, or 4
            dst_x: 1,
            dst_y: 0,
            width: 1,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 8,
        src_pitch: 4,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 0);
    assert_eq!(vram[1], 0x00); // nothing written
}

#[test]
fn color_expand_mem_caps_iterations_at_the_store_size() {
    // Pathological DIM: an all-clear source with BG 0 and dst_base 0 writes 0
    // over 0 for every in-bounds pixel, so the count equals the cap (vram.len()).
    let mut vram = vec![0u8; 64];
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 0,
            dst_pitch: 4000,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 4000,
            height: 4000,
            fg_color: 0xab,
            bg_color: 0,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 0,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 64);
}

#[test]
fn color_expand_mem_skips_extreme_coordinates_without_overflow() {
    let mut vram = vec![0u8; 64];
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: u32::MAX,
            dst_pitch: u32::MAX,
            depth: 4,
            dst_x: u32::MAX,
            dst_y: u32::MAX,
            width: 8,
            height: 8,
            fg_color: 0xdead_beef,
            bg_color: 0,
            transparent: false,
            rop: 0xcc,
            clip: Clip::default(),
        },
        src_base: u32::MAX,
        src_pitch: u32::MAX,
        src_x: u32::MAX,
        src_y: u32::MAX,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 0); // must not panic
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn bg_color_round_trips() {
    let mut margo = Margo::default();
    margo.write_mmio_u8(REG_BG_COLOR, 0x11);
    margo.write_mmio_u8(REG_BG_COLOR + 1, 0x22);
    margo.write_mmio_u8(REG_BG_COLOR + 2, 0x33);
    margo.write_mmio_u8(REG_BG_COLOR + 3, 0x44);
    assert_eq!(read_reg_u32(&margo, REG_BG_COLOR), 0x4433_2211);
}

#[test]
fn command_expand_mem_writes_vram_and_sets_busy() {
    let mut margo = Margo::default();
    margo.write_vram_u8(0, 0x80); // source row: col 0 set, col 1 clear
    write_reg(&mut margo, REG_DST_BASE, 16);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_SRC_BASE, 0);
    write_reg(&mut margo, REG_SRC_PITCH, 1);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0); // (0,0)
    write_reg(&mut margo, REG_SRC_XY, 0); // (0,0)
    write_reg(&mut margo, REG_DIM, (1 << 16) | 2); // h=1, w=2
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x04); // COLOR_EXPAND_MEM

    assert_eq!(margo.read_vram_u8(16), 0xab); // col 0 set -> FG
    assert_eq!(margo.read_vram_u8(17), 0xcd); // col 1 clear -> BG
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
}

#[test]
fn command_expand_mem_busy_drains_at_the_expand_rate() {
    let mut margo = Margo::default();
    // All-clear source, opaque: a 4x1 rect writes 4 BG pixels.
    write_reg(&mut margo, REG_DST_BASE, 16);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_SRC_BASE, 0);
    write_reg(&mut margo, REG_SRC_PITCH, 1);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_SRC_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 4); // 4x1 = 4 pixels
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x04);

    // 4 pixels -> busy_ns = 100 + 4*5 = 120. One ns short still reads busy.
    margo.advance_busy(119);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    margo.advance_busy(1);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn command_expand_data_arms_and_reports_busy() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 8); // h=1, w=8 -> 1 word
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03); // COLOR_EXPAND_DATA

    // Armed: BUSY set before any data word, nothing drawn yet.
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    assert_eq!(margo.read_vram_u8(0), 0x00);
}

#[test]
fn expand_data_word_paints_fg_and_bg_msb_first() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 4); // h=1, w=4 -> 1 word/row
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03);

    // 0xA0000000: bits 31 and 29 set -> cols 0 and 2 set, cols 1 and 3 clear.
    write_reg(&mut margo, REG_MONO_DATA, 0xa000_0000);

    assert_eq!(margo.read_vram_u8(0), 0xab); // col 0 set
    assert_eq!(margo.read_vram_u8(1), 0xcd); // col 1 clear
    assert_eq!(margo.read_vram_u8(2), 0xab); // col 2 set
    assert_eq!(margo.read_vram_u8(3), 0xcd); // col 3 clear
    // Stream complete (one word) -> BUSY now reflects the cost tail.
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
}

#[test]
fn expand_data_continues_a_wide_row_across_words() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 64);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 40); // h=1, w=40 -> 2 words/row
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03);

    write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // word 0: col 0 set
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // armed, one word left
    write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // word 1: col 32 set

    assert_eq!(margo.read_vram_u8(0), 0xab); // col 0 set (word 0, bit 31)
    assert_eq!(margo.read_vram_u8(1), 0xcd); // col 1 clear
    assert_eq!(margo.read_vram_u8(32), 0xab); // col 32 set (word 1, bit 31)
    assert_eq!(margo.read_vram_u8(33), 0xcd); // col 33 clear
}

#[test]
fn expand_data_holds_busy_through_the_stream_then_drains() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (2 << 16) | 8); // h=2, w=8 -> 1 word/row, 2 words
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // armed

    write_reg(&mut margo, REG_MONO_DATA, 0); // row 0 (all clear -> all BG)
    // Mid-stream BUSY is the armed flag, not a timer: a huge clock advance
    // cannot clear it before the last word.
    margo.advance_busy(1_000_000);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);

    write_reg(&mut margo, REG_MONO_DATA, 0); // row 1: last word completes the stream
    // 16 pixels written (8x2, opaque) -> tail busy_ns = 100 + 16*5 = 180.
    margo.advance_busy(179);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    margo.advance_busy(1);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn expand_data_transparent_skips_clear_bits() {
    let mut margo = Margo::default();
    margo.write_vram_u8(1, 0xee); // col 1 destination pre-filled
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 2); // h=1, w=2
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0x04); // EXPAND_TRANSPARENT
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03);

    write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // col 0 set, col 1 clear

    assert_eq!(margo.read_vram_u8(0), 0xab); // col 0 set -> FG
    assert_eq!(margo.read_vram_u8(1), 0xee); // col 1 clear -> left untouched
}

#[test]
fn expand_data_reset_aborts_the_stream() {
    let mut margo = Margo::default();
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (2 << 16) | 8); // 2 words
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03);
    write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000); // row 0: cols 0..7 set
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // armed, one word left

    write_reg(&mut margo, REG_CONTROL, 0x01); // RESET aborts the stream
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);

    // Row 1 was never fed; a further MONO_DATA write is now ignored.
    write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000);
    assert_eq!(margo.read_vram_u8(8), 0x00); // row 1 (offset 8) stays clear
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn expand_data_ignores_mono_data_when_idle() {
    let mut margo = Margo::default();
    margo.write_vram_u8(0, 0x11);
    write_reg(&mut margo, REG_MONO_DATA, 0xffff_ffff); // nothing armed
    assert_eq!(margo.read_vram_u8(0), 0x11); // untouched
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0); // not busy
}

#[test]
fn a_new_command_abandons_an_in_flight_expand_stream() {
    let mut margo = Margo::default();
    // Arm a 2-word DATA stream targeting row 0 and row 1 at base 0, pitch 8.
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (2 << 16) | 8); // h=2, w=8 -> 2 words
    write_reg(&mut margo, REG_FG_COLOR, 0xab);
    write_reg(&mut margo, REG_BG_COLOR, 0xcd);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0xcc); // SRCCOPY
    write_reg(&mut margo, REG_COMMAND, 0x03);
    write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000); // feed only row 0; under-run

    // A synchronous FILL now starts a new operation and abandons the stream.
    setup_fill(&mut margo);
    write_reg(&mut margo, REG_COMMAND, 0x01);

    // BUSY is driven only by the FILL's modeled time now, not a pinned stream.
    margo.advance_busy(1_000_000);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);

    // The abandoned stream must not resume: a later MONO_DATA write is ignored.
    let row1 = margo.read_vram_u8(8); // row 1, col 0 of the original stream target
    write_reg(&mut margo, REG_MONO_DATA, 0xff00_0000);
    assert_eq!(margo.read_vram_u8(8), row1);
}

#[test]
fn line_draws_a_shallow_line_endpoints_inclusive() {
    // (0,0) -> (4,2), pitch 8, depth 1. Bresenham plots (0,0),(1,1),(2,1),(3,2),
    // (4,2): offsets 0, 9, 10, 19, 20. Both endpoints are drawn.
    let mut vram = vec![0u8; 64];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 4,
        y1: 2,
        fg_color: 0xab,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 5);
    for off in [0usize, 9, 10, 19, 20] {
        assert_eq!(vram[off], 0xab, "expected line pixel at offset {off}");
    }
    assert_eq!(vram[1], 0x00); // (1,0) is not on the line
}

#[test]
fn line_draws_a_steep_line() {
    // (0,0) -> (2,4), pitch 8: (0,0),(1,1),(1,2),(2,3),(2,4) -> offsets
    // 0, 9, 17, 26, 34 (covers the y-major Bresenham branch).
    let mut vram = vec![0u8; 64];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 2,
        y1: 4,
        fg_color: 0xcd,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 5);
    for off in [0usize, 9, 17, 26, 34] {
        assert_eq!(vram[off], 0xcd, "expected line pixel at offset {off}");
    }
}

#[test]
fn line_draws_horizontal_and_vertical_runs() {
    // Horizontal (0,1) -> (3,1), pitch 8: offsets 8, 9, 10, 11.
    let mut vram = vec![0u8; 64];
    let h = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 1,
        x1: 3,
        y1: 1,
        fg_color: 0x11,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &h), 4);
    assert_eq!(&vram[8..12], &[0x11, 0x11, 0x11, 0x11]);

    // Vertical (1,0) -> (1,3), pitch 8: offsets 1, 9, 17, 25.
    let mut vram = vec![0u8; 64];
    let v = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 1,
        y0: 0,
        x1: 1,
        y1: 3,
        fg_color: 0x22,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &v), 4);
    for off in [1usize, 9, 17, 25] {
        assert_eq!(vram[off], 0x22);
    }
}

#[test]
fn line_draws_a_45_degree_diagonal() {
    // (0,0) -> (3,3), pitch 8: one pixel per step at offsets 0, 9, 18, 27.
    let mut vram = vec![0u8; 64];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 3,
        y1: 3,
        fg_color: 0x33,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 4);
    for off in [0usize, 9, 18, 27] {
        assert_eq!(vram[off], 0x33);
    }
}

#[test]
fn line_degenerate_plots_one_pixel() {
    // LINE_START == LINE_END plots exactly the one pixel (5,5), offset 45.
    let mut vram = vec![0u8; 64];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 5,
        y0: 5,
        x1: 5,
        y1: 5,
        fg_color: 0x44,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 1);
    assert_eq!(vram[45], 0x44);
}

#[test]
fn line_reversed_direction_covers_negative_steps() {
    // (3,3) -> (0,0): both sx and sy are negative. A diagonal is symmetric, so it
    // plots the same pixels as (0,0) -> (3,3): offsets 0, 9, 18, 27.
    let mut vram = vec![0u8; 64];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 3,
        y0: 3,
        x1: 0,
        y1: 0,
        fg_color: 0xab,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 4);
    for off in [0usize, 9, 18, 27] {
        assert_eq!(vram[off], 0xab);
    }
}

#[test]
fn line_writes_depth_2_and_4_pixels() {
    // depth 2 horizontal 2-pixel line, FG 0x1234 little-endian, pitch 16.
    let mut vram = vec![0u8; 64];
    let p2 = LineParams {
        dst_base: 0,
        dst_pitch: 16,
        depth: 2,
        x0: 0,
        y0: 0,
        x1: 1,
        y1: 0,
        fg_color: 0x1234,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p2), 2);
    assert_eq!(&vram[0..2], &[0x34, 0x12]); // (0,0)
    assert_eq!(&vram[2..4], &[0x34, 0x12]); // (1,0) at depth 2 = offset 2

    // depth 4 single point at (1,0) = offset 4, FG 0xDEADBEEF.
    let mut vram = vec![0u8; 64];
    let p4 = LineParams {
        dst_base: 0,
        dst_pitch: 16,
        depth: 4,
        x0: 1,
        y0: 0,
        x1: 1,
        y1: 0,
        fg_color: 0xdead_beef,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p4), 1);
    assert_eq!(&vram[4..8], &[0xef, 0xbe, 0xad, 0xde]);
}

#[test]
fn line_xor_rop_draws_and_erases() {
    // Horizontal (0,0) -> (3,0) at offsets 0..4 over a 0xFF background; ROP 0x5A
    // XORs 0x0F in, and a second identical draw restores the background.
    let mut vram = vec![0xffu8; 32];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 3,
        y1: 0,
        fg_color: 0x0f,
        rop: 0x5a,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 4);
    assert_eq!(&vram[0..4], &[0xf0, 0xf0, 0xf0, 0xf0]); // 0xff ^ 0x0f
    assert_eq!(line(&mut vram, &p), 4);
    assert_eq!(&vram[0..4], &[0xff, 0xff, 0xff, 0xff]); // restored
}

#[test]
fn line_skips_out_of_store_pixels() {
    // Vertical (0,0) -> (0,3), pitch 8, store 16: offsets 0, 8 are in; 16, 24 out.
    let mut vram = vec![0u8; 16];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 3,
        fg_color: 0xab,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 2);
    assert_eq!(vram[0], 0xab);
    assert_eq!(vram[8], 0xab);
}

#[test]
fn line_rejects_invalid_depth() {
    let mut vram = vec![0u8; 16];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 4,
        depth: 3, // not 1, 2, or 4
        x0: 0,
        y0: 0,
        x1: 2,
        y1: 0,
        fg_color: 0xff,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 0);
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn line_skips_extreme_offsets_without_overflow() {
    // 16-bit coordinates (as run_line supplies) with an extreme base and pitch:
    // every offset saturates past the store, nothing is written, no panic.
    let mut vram = vec![0u8; 64];
    let p = LineParams {
        dst_base: u32::MAX,
        dst_pitch: u32::MAX,
        depth: 4,
        x0: 0xffff,
        y0: 0xffff,
        x1: 0,
        y1: 0,
        fg_color: 0xdead_beef,
        rop: 0xf0,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 0);
    assert!(vram.iter().all(|&b| b == 0));
}

#[test]
fn fill_applies_rop3_pattern_and_dest_codes() {
    // Single pixel (0,0), pitch 4, depth 1, over dest 0x3C with FG 0x0F.
    // FILL has no source (S = 0), so only P/D codes are meaningful.
    let cases: [(u8, u8); 5] = [
        (0xf0, 0x0f),        // PATCOPY -> P (FG)
        (0x55, !0x3cu8),     // DSTINVERT -> ~D
        (0x5a, 0x3c ^ 0x0f), // PATINVERT -> D ^ P
        (0x00, 0x00),        // BLACKNESS
        (0xff, 0xff),        // WHITENESS
    ];
    for (rop, expected) in cases {
        let mut vram = vec![0u8; 16];
        vram[0] = 0x3c;
        let p = FillParams {
            dst_base: 0,
            dst_pitch: 4,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 1,
            height: 1,
            fg_color: 0x0f,
            rop,
            clip: Clip::default(),
        };
        assert_eq!(fill(&mut vram, &p), 1);
        assert_eq!(vram[0], expected, "rop {rop:#x}");
    }
}

#[test]
fn fill_clips_to_the_rectangle() {
    // 4x1 fill at y=0, cols 0..3, pitch 8; clip to x in [1, 3), y in [0, 1).
    let mut vram = vec![0u8; 16];
    let p = FillParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        dst_x: 0,
        dst_y: 0,
        width: 4,
        height: 1,
        fg_color: 0xab,
        rop: 0xf0,
        clip: Clip {
            enabled: true,
            x0: 1,
            y0: 0,
            x1: 3,
            y1: 1,
        },
    };
    assert_eq!(fill(&mut vram, &p), 2); // only x = 1, 2
    assert_eq!(vram[0], 0x00); // x = 0 clipped
    assert_eq!(vram[1], 0xab);
    assert_eq!(vram[2], 0xab);
    assert_eq!(vram[3], 0x00); // x = 3 clipped (BR exclusive)
}

#[test]
fn line_applies_rop3_against_the_destination() {
    // Horizontal 3-pixel line over a 0xFF background; DSTINVERT (0x55) -> 0x00.
    let mut vram = vec![0xffu8; 8];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 2,
        y1: 0,
        fg_color: 0,
        rop: 0x55,
        clip: Clip::default(),
    };
    assert_eq!(line(&mut vram, &p), 3);
    assert_eq!(&vram[0..3], &[0x00, 0x00, 0x00]);
    assert_eq!(vram[3], 0xff); // outside the line
}

#[test]
fn line_clips_to_the_rectangle() {
    // Horizontal line cols 0..4 at y=0; clip to x in [1, 3).
    let mut vram = vec![0u8; 8];
    let p = LineParams {
        dst_base: 0,
        dst_pitch: 8,
        depth: 1,
        x0: 0,
        y0: 0,
        x1: 4,
        y1: 0,
        fg_color: 0xab,
        rop: 0xf0,
        clip: Clip {
            enabled: true,
            x0: 1,
            y0: 0,
            x1: 3,
            y1: 1,
        },
    };
    assert_eq!(line(&mut vram, &p), 2); // only x = 1, 2
    assert_eq!(&vram[0..5], &[0x00, 0xab, 0xab, 0x00, 0x00]);
}

#[test]
fn rop3_evaluates_the_named_codes() {
    // Distinct multi-bit operands so the test exercises the bitwise evaluation.
    let (p, s, d) = (0xf0u32, 0xccu32, 0xaau32);
    assert_eq!(rop3(0x00, p, s, d), 0); // BLACKNESS
    assert_eq!(rop3(0xff, p, s, d), u32::MAX); // WHITENESS
    assert_eq!(rop3(0xcc, p, s, d), s); // SRCCOPY
    assert_eq!(rop3(0xf0, p, s, d), p); // PATCOPY
    assert_eq!(rop3(0x55, p, s, d), !d); // DSTINVERT
    assert_eq!(rop3(0x5a, p, s, d), d ^ p); // PATINVERT
    assert_eq!(rop3(0x66, p, s, d), d ^ s); // SRCINVERT
    assert_eq!(rop3(0x88, p, s, d), d & s); // SRCAND
    assert_eq!(rop3(0xee, p, s, d), d | s); // SRCPAINT
}

#[test]
fn copy_overlap_down_left_does_not_corrupt() {
    // pitch 4. Source 3x2 rect at (1,0); copy it down-left to (0,1). Rows must
    // reverse (dst below src) while columns stay forward (dst left of src).
    let mut vram = vec![0u8; 16];
    // src row 0 at offsets 1,2,3; src row 1 at offsets 5,6,7.
    vram[1] = 1;
    vram[2] = 2;
    vram[3] = 3;
    vram[5] = 4;
    vram[6] = 5;
    vram[7] = 6;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 4,
        src_base: 0,
        src_pitch: 4,
        depth: 1,
        dst_x: 0,
        dst_y: 1,
        src_x: 1,
        src_y: 0,
        width: 3,
        height: 2,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    copy(&mut vram, &p);
    // dst row 1 (offsets 4,5,6) = src row 0 [1,2,3]; dst row 2 (offsets 8,9,10)
    // = src row 1 [4,5,6], uncorrupted by the row overlap.
    assert_eq!(&vram[4..7], &[1, 2, 3]);
    assert_eq!(&vram[8..11], &[4, 5, 6]);
}

#[test]
fn copy_applies_rop3_source_and_dest() {
    // Source 0xCC at (0,0), dest 0xAA at (4,0), pitch 8. SRCINVERT (0x66) -> D^S.
    let mut vram = vec![0u8; 16];
    vram[0] = 0xcc;
    vram[4] = 0xaa;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 8,
        src_base: 0,
        src_pitch: 8,
        depth: 1,
        dst_x: 4,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 1,
        height: 1,
        fg_color: 0,
        rop: 0x66,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p), 1);
    assert_eq!(vram[4], 0xaa ^ 0xcc); // D ^ S
}

#[test]
fn copy_clips_to_the_rectangle() {
    // Source row [1,2,3,4] at (0,0); copy to (0,1) cols 0..4; clip to x in [1,3).
    let mut vram = vec![0u8; 16];
    vram[0] = 1;
    vram[1] = 2;
    vram[2] = 3;
    vram[3] = 4;
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 8,
        src_base: 0,
        src_pitch: 8,
        depth: 1,
        dst_x: 0,
        dst_y: 1,
        src_x: 0,
        src_y: 0,
        width: 4,
        height: 1,
        fg_color: 0,
        rop: 0xcc,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip {
            enabled: true,
            x0: 1,
            y0: 1,
            x1: 3,
            y1: 2,
        },
    };
    assert_eq!(copy(&mut vram, &p), 2); // dest x = 1, 2 only
    assert_eq!(vram[8], 0); // dest (0,1) clipped
    assert_eq!(vram[9], 2); // dest (1,1) = src x=1
    assert_eq!(vram[10], 3); // dest (2,1) = src x=2
    assert_eq!(vram[11], 0); // dest (3,1) clipped
}

#[test]
fn copy_applies_rop3_at_depth_2() {
    // Source pixel 0x1234, dest pixel 0xABCD, depth 2, SRCINVERT (0x66): D ^ S.
    // 0x1234 ^ 0xABCD = 0xB9F9, stored little-endian.
    let mut vram = vec![0u8; 32];
    vram[0] = 0x34; // src (0,0) low byte
    vram[1] = 0x12; // src (0,0) high byte
    vram[4] = 0xcd; // dst (2,0) low byte  (pitch 16 -> offset 2*depth = 4)
    vram[5] = 0xab; // dst (2,0) high byte
    let p = CopyParams {
        dst_base: 0,
        dst_pitch: 16,
        src_base: 0,
        src_pitch: 16,
        depth: 2,
        dst_x: 2,
        dst_y: 0,
        src_x: 0,
        src_y: 0,
        width: 1,
        height: 1,
        fg_color: 0,
        rop: 0x66,
        colorkey: 0,
        colorkey_en: false,
        clip: Clip::default(),
    };
    assert_eq!(copy(&mut vram, &p), 1);
    let result = u16::from_le_bytes([vram[4], vram[5]]);
    assert_eq!(result, 0x1234u16 ^ 0xabcdu16); // 0xB9F9
}

#[test]
fn color_expand_mem_applies_rop3() {
    // Source bit set -> S = FG 0x0F; dest 0xAA; SRCINVERT (0x66) -> D ^ S.
    let mut vram = vec![0u8; 64];
    vram[0] = 0x80; // mono source, col 0 set
    vram[16] = 0xaa; // dest pixel
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 1,
            height: 1,
            fg_color: 0x0f,
            bg_color: 0,
            transparent: false,
            rop: 0x66,
            clip: Clip::default(),
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 1);
    assert_eq!(vram[16], 0xaa ^ 0x0f);
}

#[test]
fn color_expand_mem_clips() {
    // Source 0xC0 (cols 0,1 set) expanded to dest cols 0,1; clip to x in [1,2).
    let mut vram = vec![0u8; 64];
    vram[0] = 0xc0;
    let p = ExpandMemParams {
        common: ExpandParams {
            dst_base: 16,
            dst_pitch: 8,
            depth: 1,
            dst_x: 0,
            dst_y: 0,
            width: 2,
            height: 1,
            fg_color: 0xab,
            bg_color: 0xcd,
            transparent: false,
            rop: 0xcc,
            clip: Clip {
                enabled: true,
                x0: 1,
                y0: 0,
                x1: 2,
                y1: 1,
            },
        },
        src_base: 0,
        src_pitch: 1,
        src_x: 0,
        src_y: 0,
    };
    assert_eq!(color_expand_mem(&mut vram, &p), 1); // only col 1
    assert_eq!(vram[16], 0x00); // col 0 clipped
    assert_eq!(vram[17], 0xab); // col 1 set -> S = FG, 0xcc writes S
}

#[test]
fn expand_data_applies_rop3() {
    // Streamed DATA must honor ROP too: arm with SRCINVERT, dest 0xAA, set bit.
    let mut margo = Margo::default();
    margo.write_vram_u8(0, 0xaa);
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 1); // 1x1
    write_reg(&mut margo, REG_FG_COLOR, 0x0f);
    write_reg(&mut margo, REG_FLAGS, 0);
    write_reg(&mut margo, REG_ROP, 0x66); // SRCINVERT: D ^ S
    write_reg(&mut margo, REG_COMMAND, 0x03);
    write_reg(&mut margo, REG_MONO_DATA, 0x8000_0000); // one word, col 0 set
    assert_eq!(margo.read_vram_u8(0), 0xaa ^ 0x0f);
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
fn command_pattern_fill_tiles_and_sets_busy() {
    let mut margo = Margo::default();
    // Seed an 8x8 tile at VRAM offset 4096 (clear of the destination), depth 1,
    // cell (r, c) = r*8 + c + 1.
    let pat_base = 4096u32;
    for r in 0..8u32 {
        for c in 0..8u32 {
            margo.write_vram_u8((pat_base + r * 8 + c) as usize, (r * 8 + c + 1) as u8);
        }
    }
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 32);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_PAT_BASE, pat_base);
    write_reg(&mut margo, REG_DST_XY, (2 << 16) | 3); // (x=3, y=2)
    write_reg(&mut margo, REG_DIM, (10 << 16) | 10); // 10x10
    write_reg(&mut margo, REG_ROP, 0xf0); // PATCOPY
    write_reg(&mut margo, REG_COMMAND, 0x06); // PATTERN_FILL

    assert_eq!(margo.read_vram_u8(2 * 32 + 3), 20); // (3,2) tile[2][3]
    assert_eq!(margo.read_vram_u8(2 * 32 + 10), 19); // (10,2) tile[2][2]
    assert_eq!(margo.read_vram_u8(9 * 32 + 3), 12); // (3,9) tile[1][3]
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1); // BUSY set
}

#[test]
fn command_pattern_fill_busy_drains_at_the_fill_rate() {
    let mut margo = Margo::default();
    let pat_base = 4096u32;
    for b in 0..64u32 {
        margo.write_vram_u8((pat_base + b) as usize, 0xcd);
    }
    write_reg(&mut margo, REG_DST_BASE, 0);
    write_reg(&mut margo, REG_DST_PITCH, 8);
    write_reg(&mut margo, REG_DEPTH, 1);
    write_reg(&mut margo, REG_PAT_BASE, pat_base);
    write_reg(&mut margo, REG_DST_XY, 0);
    write_reg(&mut margo, REG_DIM, (1 << 16) | 4); // 4x1
    write_reg(&mut margo, REG_ROP, 0xf0);
    write_reg(&mut margo, REG_COMMAND, 0x06);

    // 4 pixels -> busy_ns = 100 + 4*5 = 120. One ns short still reads busy.
    margo.advance_busy(119);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 1);
    margo.advance_busy(1);
    assert_eq!(read_reg_u32(&margo, REG_STATUS) & 1, 0);
}

#[test]
fn pat_base_register_round_trips() {
    let mut margo = Margo::default();
    // Distinct values in each lane prove byte recombination through the store.
    margo.write_mmio_u8(REG_PAT_BASE, 0x11);
    margo.write_mmio_u8(REG_PAT_BASE + 1, 0x22);
    margo.write_mmio_u8(REG_PAT_BASE + 2, 0x33);
    margo.write_mmio_u8(REG_PAT_BASE + 3, 0x44);
    assert_eq!(read_reg_u32(&margo, REG_PAT_BASE), 0x4433_2211);
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
