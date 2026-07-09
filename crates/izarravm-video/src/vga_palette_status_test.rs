// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
