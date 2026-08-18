// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_video::{DISTIRA_REG_FB_WIDTH, SST_SWAPBUFFER_CMD};

#[test]
fn machine_advances_the_vga_beam_with_cpu_clocks() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    let before = machine.video().beam_dots();
    // 10 000 CPU clocks at 22 MHz with a 25.175 MHz dot clock advances
    // roughly 11 443 dots, well above zero.
    machine.advance_devices(10_000);
    assert!(machine.video().beam_dots() != before || machine.video().frames_completed() > 0);
}

#[test]
fn display_refresh_matches_the_vga_mode() {
    let mut machine = test_machine();
    // Mode 0Dh is a ~359 200-dot frame at the 25.175 MHz dot clock, i.e.
    // ~70 Hz, the classic VGA graphics refresh.
    machine.set_vga_mode_0dh();
    let hz = machine.display_refresh_hz();
    assert!((hz - 70.0).abs() < 1.0, "expected ~70 Hz, got {hz}");
    // Mode 12h (640x480, 525 lines) is the 60 Hz timing.
    machine.set_vga_mode(0x12);
    let hz = machine.display_refresh_hz();
    assert!((hz - 60.0).abs() < 1.0, "expected ~60 Hz, got {hz}");
}

#[test]
fn display_refresh_uses_misc_output_clock_select() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    let clock25 = machine.display_refresh_hz();
    assert!(machine.video_mut().write_port(0x3C2, 0x04));
    let clock28 = machine.display_refresh_hz();

    assert!(clock28 > clock25);
    assert!(
        (clock28 / clock25 - 28_322_000.0 / 25_175_000.0).abs() < 0.01,
        "expected refresh ratio to follow Misc Output clock select"
    );
}

#[test]
fn video_facade_preserves_presented_and_headless_scanout_behavior() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    machine.video_mut().set_dac_entry(1, 63, 0, 0);
    machine.write_physical_u8(VGA_MODE13H_BASE, 1);

    assert_eq!(machine.active_video_mode(), VideoMode::Mode13h);
    let initial_sequence = machine.frame_sequence();
    let (captured, captured_width, captured_height) = machine.capture_frame_argb();
    assert_eq!((captured_width, captured_height), (320, 400));
    assert_eq!(captured[0], 0x00ff_0000);

    machine.advance_devices(600_000);
    assert!(machine.frame_sequence() > initial_sequence);
    let (presented, presented_width, presented_height) = machine
        .presented_frame_argb()
        .expect("a raster completed during the 600 000-clock advance");
    assert_eq!((presented_width, presented_height), (320, 400));
    assert_eq!(presented[0], 0x00ff_0000);

    let first = machine
        .presented_frame_update()
        .expect("first cached frame");
    let unchanged = machine
        .presented_frame_update()
        .expect("unchanged cached frame");
    assert!(unchanged.changed_rows.is_empty());
    assert!(std::sync::Arc::ptr_eq(&first.words, &unchanged.words));

    let (_, full_width, full_height) = machine.frame_argb();
    assert_eq!(full_width, 320);
    assert!(full_height > presented_height);
}

#[test]
fn margo_test_pattern_is_owned_by_the_video_facade() {
    let mut machine = test_machine();
    machine.load_margo_test_pattern();

    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    let display = machine.margo().display();
    assert_eq!((display.width, display.height), (640, 480));
    assert_eq!(machine.margo().read_vram_u8(0), 0);
    assert_eq!(machine.margo().read_vram_u8(1), 1);
    assert_eq!(machine.margo().read_vram_u8(display.pitch as usize), 1);
}

#[test]
fn graphics_mode_reporting_follows_the_active_vega_engine() {
    let mut machine = test_machine();
    assert!(!machine.is_graphics_mode());

    assert!(machine.set_vga_mode(0x13));
    assert!(machine.is_graphics_mode());

    machine.video_mut().set_text_mode();
    assert!(!machine.is_graphics_mode());

    machine.set_margo_mode_640x480x8();
    assert!(machine.is_graphics_mode());

    let width = 2_u32.to_le_bytes();
    for (byte, value) in width.into_iter().enumerate() {
        machine.write_physical_u8(
            DISTIRA_MMIO_BASE + DISTIRA_REG_FB_WIDTH as u32 + byte as u32,
            value,
        );
    }
    for byte in 0..4 {
        machine.write_physical_u8(DISTIRA_MMIO_BASE + SST_SWAPBUFFER_CMD as u32 + byte, 0);
    }
    assert_eq!(machine.active_display(), ActiveDisplay::Distira);
    assert!(machine.is_graphics_mode());

    assert!(machine.set_vga_mode(0x13));
    machine.video_mut().set_text_mode();
    assert!(!machine.is_graphics_mode());
}

#[test]
fn planar_mode_presents_a_vga_raster() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    // Mode 0Dh frame is ~359 200 dots; 600 000 CPU clocks at 22 MHz yields
    // ~686 600 dot clocks, enough to complete at least one full frame.
    machine.advance_devices(600_000);
    assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
    assert!(machine.vga_raster().is_some());
}

#[test]
fn text_mode_scanout_through_the_machine() {
    let mut machine = test_machine();
    // A CP437 cell at B8000:0 (the solid block 0xDB) with a white-on-black
    // attribute, written through the bus so it routes to text_memory.
    machine.write_physical_u8(VGA_TEXT_BASE, 0xDB);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    // Mode 03h maps white text through DAC index 0x3F.
    machine.video_mut().set_dac_entry(0x3F, 63, 0, 0);
    // Enough CPU time to finalize at least one frame.
    machine.advance_devices(600_000);
    assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
    let raster = machine.vga_raster().expect("text presents a VgaRaster");
    assert_eq!(raster.width, 720);
    assert_eq!(raster.pixels[0], 0x3F);
    assert_eq!(machine.palette_argb()[0x3F], 0x00FF_0000);
}

#[test]
fn video_subsystem_enable_gates_legacy_apertures_through_the_machine() {
    let mut machine = test_machine();

    machine.write_physical_u8(VGA_TEXT_BASE, b'T');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');
    assert!(machine.video_mut().write_port(0x3C3, 0x00));
    machine.write_physical_u8(VGA_TEXT_BASE, b'R');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'R');
    assert!(machine.video_mut().write_port(0x3C3, 0x01));
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');

    machine.video_mut().set_mode13h();
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x12);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
    assert!(machine.video_mut().write_port(0x3C3, 0x00));
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x34);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x34);
    assert!(machine.video_mut().write_port(0x3C3, 0x01));
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
}

#[test]
fn misc_output_ram_enable_gates_legacy_apertures_through_the_machine() {
    let mut machine = test_machine();

    machine.write_physical_u8(VGA_TEXT_BASE, b'T');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc & !0x02));
    assert!(machine.video().video_subsystem_enabled());
    assert!(!machine.video().video_memory_enabled());
    machine.write_physical_u8(VGA_TEXT_BASE, b'R');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'R');
    {
        let mut bus = machine.make_bus();
        assert_eq!(bus.read_io(0x3C3, BusWidth::Byte, 0, false).unwrap(), 1);
        assert_eq!(
            bus.read_io(0x3CC, BusWidth::Byte, 0, false).unwrap() & 0x02,
            0
        );
    }
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc | 0x02));
    assert!(machine.video().video_memory_enabled());
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');

    machine.video_mut().set_mode13h();
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x12);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc & !0x02));
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x34);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x34);
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc | 0x02));
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
}

#[test]
fn mode7_routes_b000_text_window_through_the_machine() {
    // mov ax,0007h; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x07, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(machine.video().raster_width(), 720);
    assert_eq!(machine.video().raster_height(), 449);
    assert_eq!(machine.read_physical_u8(0x449), 0x07);
    assert_eq!(machine.read_physical_u16(0x463), 0x03B4);
    assert_eq!(machine.read_physical_u8(0x485), 14);

    machine.write_physical_u8(VGA_MONO_TEXT_BASE, 0xDB);
    machine.write_physical_u8(VGA_MONO_TEXT_BASE + 1, 0x0F);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE), 0xDB);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE + 1), 0x0F);

    machine.advance_devices(600_000);
    let raster = machine.vga_raster().expect("mode 7 presents a VgaRaster");
    assert_eq!(raster.pixels[0], 0x0F);
}

#[test]
fn cga_graphics_routes_b800_to_the_framebuffer() {
    let mut machine = test_machine();
    // Enter CGA mode 04h (320x200x4) the way INT 10h AH=00 AL=04 would.
    machine.video_mut().set_cga_mode(0x04);
    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    // A byte written to B800:0000 lands in the CGA framebuffer, not the text
    // buffer. 0b00_01_10_11 decodes to bg/green/red/brown on the default
    // palette (green=2, red=4, brown=6).
    machine.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b00_01_10_11);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 262);
    // The first four pixels of scanline 0.
    assert_eq!(&raster.pixels[0..4], &[0, 2, 4, 6]);
}

#[test]
fn cga_odd_scanline_reads_the_high_bank_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_cga_mode(0x04);
    // Scanline 1 of a CGA frame reads framebuffer offset 0x2000 (the odd bank).
    // Write there through the B800 aperture and confirm it scans out on line 1.
    machine.write_physical_u8(VGA_TEXT_BASE + 0x2000, 0b01_01_01_01);
    let raster = machine.video_mut().render_full_frame();
    // Row 1 starts at offset width*1.
    let row1 = &raster.pixels[320..320 + 4];
    assert_eq!(row1, &[2, 2, 2, 2]); // value 1 -> green(2)
}

#[test]
fn cga_mode_control_switches_b800_routing_through_the_machine() {
    let mut machine = test_machine();
    assert!(machine.video_mut().set_cga_text_mode(0x01));
    machine.write_physical_u8(VGA_TEXT_BASE, b'T');
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x0A, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.video().render_cga_row(0)[0], 2);
    machine.write_physical_u8(VGA_TEXT_BASE, 0b01_01_01_01);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b01_01_01_01);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x28, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b01_01_01_01);
}

#[test]
fn cga_mode_and_color_select_ports_are_output_only_through_the_bus() {
    let mut machine = test_machine();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x0A, false).unwrap();
        bus.write_io(0x3D9, BusWidth::Byte, 0x35, false).unwrap();
        assert_eq!(bus.read_io(0x3D8, BusWidth::Byte, 0, false).unwrap(), 0xFF);
        assert_eq!(bus.read_io(0x3D9, BusWidth::Byte, 0, false).unwrap(), 0xFF);
    }

    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.video().cga_color_select(), 0x35);
}

#[test]
fn cga_crtc_alias_ports_route_through_video_bus() {
    let mut machine = test_machine();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x0A, false).unwrap();
        bus.write_io(0x3D0, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x3D1, BusWidth::Byte, 0x20, false).unwrap();
        assert_eq!(bus.read_io(0x3D2, BusWidth::Byte, 0, false).unwrap(), 0xFF);
        assert_eq!(bus.read_io(0x3D3, BusWidth::Byte, 0, false).unwrap(), 0xFF);

        bus.write_io(0x3D6, BusWidth::Byte, 0x0A, false).unwrap();
        bus.write_io(0x3D7, BusWidth::Byte, 0x06, false).unwrap();
        assert_eq!(bus.read_io(0x3D4, BusWidth::Byte, 0, false).unwrap(), 0xFF);
        assert_eq!(bus.read_io(0x3D5, BusWidth::Byte, 0, false).unwrap(), 0xFF);

        bus.write_io(0x3D4, BusWidth::Byte, 0x0E, false).unwrap();
        bus.write_io(0x3D5, BusWidth::Byte, 0x12, false).unwrap();
        assert_eq!(bus.read_io(0x3D5, BusWidth::Byte, 0, false).unwrap(), 0x12);
    }

    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.video().raster_width(), 256);
}

#[test]
fn cga_text_b800_window_mirrors_16kb_through_the_machine() {
    let mut machine = test_machine();
    assert!(machine.video_mut().set_cga_text_mode(0x01));
    machine.write_physical_u8(VGA_TEXT_BASE, b'A');
    machine.write_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32, b'B');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'B');
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32),
        b'B'
    );

    machine.video_mut().set_text_mode();
    machine.write_physical_u8(VGA_TEXT_BASE, b'A');
    machine.write_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32, b'V');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'A');
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32),
        b'V'
    );
}

#[test]
fn hercules_graphics_routes_b0000_and_b8000_through_the_machine() {
    // Real Hercules software sets BIOS mode 07h (MDA-compatible text) and
    // then bangs ports 3B8h/3BFh directly: there is no INT 10h graphics
    // mode number for it.
    let mut machine = test_machine();
    machine.video_mut().set_mono_text_mode();

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3BF, BusWidth::Byte, 0x01, false).unwrap(); // allow graphics
        bus.write_io(0x3B8, BusWidth::Byte, 0x0A, false).unwrap(); // GRPH + video enable
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Hercules);
    assert!(machine.is_graphics_mode());
    assert_eq!(machine.video().raster_width(), 720);
    assert_eq!(machine.video().raster_height(), 370);

    // Page 0 lives at B0000 and is always addressable.
    machine.write_physical_u8(VGA_MONO_TEXT_BASE, 0b1000_0000);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE), 0b1000_0000);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.pixels[0], 1);

    // Page 1 (B8000) is not yet paged in: a write there does not land in
    // the Hercules framebuffer (falls through to the flat RAM array
    // underneath, like any other unclaimed MMIO window in this bus), so
    // it is invisible to the Hercules scanout.
    assert!(!machine.video().hgc_page1_addressable());
    machine.write_physical_u8(VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32, 0xFF);
    assert_eq!(machine.video_mut().hgc_read(HGC_FB_SIZE), 0);

    // Page in the second bank through 3BFh and flip Mode Control's page
    // select (bit 7): the CRTC now scans out B8000 instead of B0000.
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3BF, BusWidth::Byte, 0x03, false).unwrap(); // allow graphics + page 1
        bus.write_io(0x3B8, BusWidth::Byte, 0x8A, false).unwrap(); // GRPH + video + page select
    }
    machine.write_physical_u8(VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32, 0b0100_0000);
    assert_eq!(
        machine.read_physical_u8(VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32),
        0b0100_0000
    );
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.pixels[0], 0); // page 0's bit no longer scanned out
    assert_eq!(raster.pixels[1], 1); // page 1's bit shows instead
}

#[test]
fn hercules_config_switch_refuses_graphics_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_mono_text_mode();

    // 3B8h GRPH with no 3BFh unlock: the card stays in text mode, and the
    // Hercules 64K graphics window does not decode (falls through to the
    // ordinary mono text B0000 aperture instead).
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3B8, BusWidth::Byte, 0x0A, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    machine.write_physical_u8(VGA_MONO_TEXT_BASE, 0xDB);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE), 0xDB);
}

#[test]
fn hercules_detection_status_port_survives_the_machine_bus() {
    let mut machine = test_machine();
    machine.video_mut().set_mono_text_mode();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3BF, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x3B8, BusWidth::Byte, 0x0A, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Hercules);

    let mut bus = machine.make_bus();
    let outside_vsync = bus.read_io(0x3BA, BusWidth::Byte, 0, false).unwrap() & 0x80;
    assert_eq!(outside_vsync, 0x80);
}

#[test]
fn int10_11h_loads_user_font() {
    // A 2-glyph user font (two solid 8x16 blocks) at ES:BP = 4000h:0,
    // overwriting 'A' and 'B'. AL=00 loads it; BH=16 bytes/char, BL=0
    // (table 0), CX=2, DX=41h.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbd, 0x00, 0x00, // mov bp, 0
        0xb9, 0x02, 0x00, // mov cx, 2
        0xba, 0x41, 0x00, // mov dx, 41h (first char 'A')
        0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
        0xb8, 0x00, 0x11, // mov ax, 1100h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.write_guest_block(0x40000, &[0xFF; 32]); // two solid glyphs
    // Display cell 0 = 'A', white on black.
    machine.write_physical_u8(VGA_TEXT_BASE, 0x41);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    // The custom 'A' is solid, so its top row scans out as the foreground.
    // The stock 'A' would be blank on the top row (background), so this
    // confirms the user font loaded and renders.
    assert_eq!(machine.video().render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn int43_points_at_current_font_table() {
    let mut machine = int15_machine(16);
    let off = read_u16(&mut machine, 0x43 * 4);
    let seg = read_u16(&mut machine, 0x43 * 4 + 2);
    let table = (u32::from(seg) << 4) + u32::from(off);
    assert_eq!(seg, (VGA_BIOS_BASE >> 4) as u16);
    assert_eq!(off, VGA_BIOS_FONT_TABLE_OFF);
    assert_eq!(table, VGA_BIOS_INT43_FONT_ADDR);
    assert_eq!(
        machine.read_physical_u8(table + 0x41 * 16 + 7),
        izarravm_video::font::VGAFONT_8X16[0x41 * 16 + 7]
    );

    machine.cpu.registers.set_eax(0x1130);
    machine.cpu.registers.set_ebx(0x0100); // BH=01h: INT 43h pointer
    machine.handle_int10();
    assert_eq!(
        machine.cpu.registers.segment(SegmentIndex::Es).selector,
        (VGA_BIOS_BASE >> 4) as u16
    );
    assert_eq!(machine.cpu.registers.ebp() as u16, VGA_BIOS_FONT_TABLE_OFF);
    assert_eq!(machine.cpu.registers.ecx() as u16, 16);
    assert_eq!(machine.cpu.registers.edx() as u8, 24);

    machine.write_guest_block(0x40000, &[0xFF; 16]);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    machine.cpu.registers.set_ebp(0);
    machine.cpu.registers.set_ecx(1);
    machine.cpu.registers.set_edx(0x41);
    machine.cpu.registers.set_ebx(0x1000); // BH=16, BL=0
    machine.cpu.registers.set_eax(0x1100);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(table + 0x41 * 16), 0xFF);
}

#[test]
fn bios_table_vectors_1d_1e_1f_point_at_seeded_tables() {
    let mut machine = int15_machine(16);

    let int1d = {
        let off = read_u16(&mut machine, 0x1d * 4);
        let seg = read_u16(&mut machine, 0x1d * 4 + 2);
        (u32::from(seg) << 4) + u32::from(off)
    };
    assert_eq!(int1d, VGA_BIOS_INT1D_VIDEO_TABLE_ADDR);
    assert_eq!(
        machine.read_physical_u8(int1d + 2 * 16 + 1),
        0x50,
        "INT 1Dh mode 02h table is 80-column text"
    );

    let int1e = {
        let off = read_u16(&mut machine, 0x1e * 4);
        let seg = read_u16(&mut machine, 0x1e * 4 + 2);
        (u32::from(seg) << 4) + u32::from(off)
    };
    assert_eq!(int1e, BIOS_DISKETTE_PARAMETER_TABLE_ADDR);
    assert_eq!(
        machine.read_physical_u8(int1e + 4),
        0x12,
        "default diskette table describes 18 sectors per track"
    );

    let int1f = {
        let off = read_u16(&mut machine, 0x1f * 4);
        let seg = read_u16(&mut machine, 0x1f * 4 + 2);
        (u32::from(seg) << 4) + u32::from(off)
    };
    assert_eq!(int1f, VGA_BIOS_INT1F_FONT_ADDR);
    assert_eq!(
        machine.read_physical_u8(int1f + (0xc4 - 0x80) * 8),
        izarravm_video::font::VGAFONT_8X8[0xc4 * 8]
    );
}

#[test]
fn int44_points_at_rom_8x8_font_table() {
    let mut machine = int15_machine(16);
    let off = read_u16(&mut machine, 0x44 * 4);
    let seg = read_u16(&mut machine, 0x44 * 4 + 2);
    let table = (u32::from(seg) << 4) + u32::from(off);
    assert_eq!(seg, (VGA_BIOS_BASE >> 4) as u16);
    assert_eq!(off, VGA_BIOS_INT44_FONT_OFF);
    assert_eq!(table, VGA_BIOS_INT44_FONT_ADDR);
    assert_eq!(
        machine.read_physical_u8(table + 0x41 * 8 + 4),
        izarravm_video::font::VGAFONT_8X8[0x41 * 8 + 4]
    );
}

#[test]
fn int10_11h_loads_rom_8x16() {
    // First a custom load blanks glyph 0xDB (AL=00); then AL=04 reloads the
    // ROM 8x16 font, restoring the solid full block.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbd, 0x00, 0x00, // mov bp, 0
        0xb9, 0x01, 0x00, // mov cx, 1
        0xba, 0xdb, 0x00, // mov dx, 0DBh (full block)
        0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
        0xb8, 0x00, 0x11, // mov ax, 1100h (user font)
        0xcd, 0x10, // int 10h
        0xbb, 0x00, 0x10, // mov bx, 1000h
        0xb8, 0x04, 0x11, // mov ax, 1104h (ROM 8x16)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.write_guest_block(0x40000, &[0x00; 16]); // a blank glyph for 0xDB
    machine.write_physical_u8(VGA_TEXT_BASE, 0xDB);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    // The ROM reload restored the solid full block; without it the custom
    // blank load would leave the top row as the background (0).
    assert_eq!(machine.video().render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn int10_11h_caps_a_pathological_glyph_count() {
    // CX = 0xFFFF with BH = 16 would read ~16 MB byte-at-a-time. The handler
    // caps the read at 256 glyphs (codes fold modulo 256), so the call still
    // loads the first glyph and returns promptly without stalling or
    // over-allocating.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbd, 0x00, 0x00, // mov bp, 0
        0xb9, 0xff, 0xff, // mov cx, 0FFFFh
        0xba, 0x41, 0x00, // mov dx, 41h ('A')
        0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
        0xb8, 0x00, 0x11, // mov ax, 1100h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    // A solid glyph for 'A' at the first 16 bytes; the rest of the 64 KB
    // page stays zero, so capping the read also proves only the real glyph
    // data is consulted.
    machine.write_guest_block(0x40000, &[0xFF; 16]);
    machine.write_physical_u8(VGA_TEXT_BASE, 0x41);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The first glyph (solid) loaded and renders as the foreground.
    assert_eq!(machine.video().render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn int10_teletype_and_cursor() {
    let rom = rom_with_code(&[
        0xB8, 0x03, 0x00, 0xCD, 0x10, // set text mode 03h (homes cursor)
        0xB4, 0x0E, 0xB0, b'H', 0xCD, 0x10, // AH=0Eh teletype 'H'
        0xB4, 0x0E, 0xB0, b'i', 0xCD, 0x10, // AH=0Eh teletype 'i'
        0xB4, 0x03, 0xB7, 0x00, 0xCD, 0x10, // AH=03h get cursor (page 0)
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // 'H' then 'i' landed at row 0 cols 0,1; cursor now at row 0 col 2.
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'H');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2), b'i');
    let dx = machine.cpu().registers.edx() as u16;
    assert_eq!(dx, 0x0002, "DH=row 0, DL=col 2");
}

#[test]
fn int10_01_updates_cga_hardware_cursor_shape() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0002);
    machine.handle_int10();
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(machine.video().render_text_row(0)[0], 0);

    machine.cpu.registers.set_eax(0x0100);
    machine.cpu.registers.set_ecx(0x0007);
    machine.handle_int10();
    assert_eq!(machine.video().render_text_row(0)[0], 15);

    machine.cpu.registers.set_eax(0x0300);
    machine.cpu.registers.set_ebx(0);
    machine.handle_int10();
    assert_eq!(machine.cpu.registers.ecx() as u16, 0x0007);
}

#[test]
fn int10_text_services_use_40_column_mode_stride() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0001);
    m.handle_int10();
    assert_eq!(m.video().frame().columns, 40);
    assert_eq!(m.video_mut().render_full_frame().width, 320);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x0800);

    m.write_guest_block(0x4000, b"ABCD");
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x4000);
    m.cpu.registers.set_eax(0x1301);
    m.cpu.registers.set_ebx(0x001E);
    m.cpu.registers.set_ecx(4);
    m.cpu.registers.set_edx(38);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 38 * 2), b'A');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 39 * 2), b'B');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 40 * 2), b'C');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 41 * 2), b'D');
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 0x0102);

    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(39);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0E5A);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 39 * 2), b'Z');
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 0x0100);
    assert_eq!(m.video().frame().cursor_offset, 40);

    m.cpu.registers.set_eax(0x0F00);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() as u16) >> 8, 40);
}

#[test]
fn int10_mode02_uses_cga_80_text_geometry_and_mode03_stays_vga() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0002);
    m.handle_int10();
    assert_eq!(m.video().frame().columns, 80);
    assert_eq!(m.video_mut().render_full_frame().width, 640);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000);
    assert_eq!(m.read_physical_u8(0x485), 8);

    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.video().frame().columns, 80);
    assert_eq!(m.video_mut().render_full_frame().width, 720);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000);
    assert_eq!(m.read_physical_u8(0x485), 16);

    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x4000);
    assert_eq!(m.read_physical_u8(0x485), 8);
}

#[test]
fn int10_scroll_window_up_blanks_bottom() {
    // No mode set here: setting a text mode clears the framebuffer, which
    // would wipe the marker the host seeds below before the scroll runs.
    let rom = rom_with_code(&[
        0xB8, 0x01, 0x06, // mov ax,0601h (AH=06h scroll up 1 line)
        0xB7, 0x07, // mov bh,07h (fill attr)
        0xB9, 0x00, 0x00, // mov cx,0000h (top-left 0,0)
        0xBA, 0x4F, 0x18, // mov dx,184Fh (bottom-right row 24 col 79)
        0xCD, 0x10, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    // Put a non-space at row 1 col 0; after scroll-up by 1 it lands at row 0.
    machine.write_physical_u8(VGA_TEXT_BASE + 80 * 2, b'X');
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE),
        b'X',
        "row 1 scrolled to row 0"
    );
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + 24 * 80 * 2),
        b' ',
        "bottom row blanked"
    );
}

#[test]
fn int10_scroll_window_down_blanks_top() {
    // No mode set here: setting a text mode clears the framebuffer, which
    // would wipe the marker the host seeds below before the scroll runs.
    let rom = rom_with_code(&[
        0xB8, 0x01, 0x07, // mov ax,0701h (AH=07h scroll down 1 line)
        0xB7, 0x07, // mov bh,07h (fill attr)
        0xB9, 0x00, 0x00, // mov cx,0000h (top-left 0,0)
        0xBA, 0x4F, 0x18, // mov dx,184Fh (bottom-right row 24 col 79)
        0xCD, 0x10, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    // Put a non-space at row 0 col 0; after scroll-down by 1 it lands at row 1.
    machine.write_physical_u8(VGA_TEXT_BASE, b'Y');
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + 80 * 2),
        b'Y',
        "row 0 scrolled to row 1"
    );
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE),
        b' ',
        "top row blanked"
    );
}

#[test]
fn int10_scroll_subwindow_up() {
    // No mode set here: setting a text mode clears the framebuffer, which
    // would wipe the marker the host seeds below before the scroll runs.
    // CX = top-left, DX = bottom-right; for each, the high byte is the row
    // and the low byte is the column: CX=(row<<8)|col, DX=(row<<8)|col.
    let rom = rom_with_code(&[
        0xB8, 0x01, 0x06, // mov ax,0601h (AH=06h scroll up 1 line)
        0xB7, 0x07, // mov bh,07h (fill attr)
        0xB9, 0x04, 0x01, // mov cx,0104h (top-left row 1 col 4)
        0xBA, 0x0A, 0x03, // mov dx,030Ah (bottom-right row 3 col 10)
        0xCD, 0x10, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    // Marker inside the window at row 2 col 5; after scroll-up by 1 it lands
    // at row 1 col 5.
    machine.write_physical_u8(VGA_TEXT_BASE + ((2 * 80) + 5) * 2, b'W');
    // Sentinels in cells outside the window (the framebuffer is otherwise
    // pre-blanked with spaces, so seed distinct bytes to prove the scroll's
    // row and column clamping never wrote here): row 0 col 0 is above the
    // window, row 2 col 0 is left of the col-4 left edge.
    machine.write_physical_u8(VGA_TEXT_BASE, b'A');
    machine.write_physical_u8(VGA_TEXT_BASE + (2 * 80) * 2, b'B');
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + (80 + 5) * 2),
        b'W',
        "row 2 col 5 scrolled to row 1 col 5"
    );
    // A cell above the window (row 0 col 0) is untouched.
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE),
        b'A',
        "row 0 col 0 outside window left untouched"
    );
    // A cell to the left of the window (row 2 col 0, left edge is col 4) is
    // untouched.
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + (2 * 80) * 2),
        b'B',
        "row 2 col 0 left of window left untouched"
    );
}

#[test]
fn a0000_writes_route_to_the_planar_datapath_in_mode_0dh() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    // Enable plane 0 only, copy write mode, full bit mask, via the VGA ports.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x05);
    machine.video_mut().write_port(0x3CF, 0x00); // write mode 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    // Write a byte to A0000 through the machine memory path.
    machine.write_physical_u8(0x000A_0000, 0xFF);
    // Plane 0 byte 0 should now be 0xFF (planar datapath), confirming routing.
    assert_eq!(machine.video().plane_byte(0, 0), 0xFF);
}

#[test]
fn copper_bar_split_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    // Set up so A0000 writes fill plane 0 (attribute index 1) with a full bit
    // mask. Write mode 0 is the reset default.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    // Fill the visible region of plane 0 (offset 0..8000 covers 200 lines * 40
    // bytes) through the machine memory path — exercises the A0000 routing.
    for off in 0..8000u32 {
        machine.write_physical_u8(0x000A_0000 + off, 0xFF);
    }
    // Identity attribute palette so index 1 -> DAC 1. Reading 3DA resets the
    // flip-flop to "index" first; each entry is an index write then a value
    // write, so after 16 entries the flip-flop is back in "index" mode.
    machine.video_mut().read_status1(); // reset attr flip-flop
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i); // index, PAS on
        machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
    }
    // Advance to roughly counter line 50, change palette[1] -> 9, then finish
    // the frame. dots = clocks * VGA_DOT_HZ / clock_hz (~1.007 dots/clock);
    // 39_700 clocks ≈ 39_980 dots ≈ counter line 49 (htotal 800).
    machine.advance_devices(39_700);
    // The flip-flop is in "index" mode here (even number of writes above).
    machine.video_mut().write_port(0x3C0, 0x21); // attr index 1, PAS on
    machine.video_mut().write_port(0x3C0, 9); // palette[1] = 9
    machine.advance_devices(400_000); // complete the frame
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize;
    // The principle: a contiguous top region uses the old palette (DAC 1) and a
    // lower region uses the new palette (DAC 9), separated by the beam row at
    // the time of the palette change. Scan for that transition rather than
    // hard-coding the split row, so the test survives small timing drift.
    assert_eq!(raster.pixels[0], 1, "top of frame uses the old palette");
    let height = raster.height as usize;
    let mut split = None;
    for row in 0..height {
        let p = raster.pixels[row * w];
        if p == 9 {
            split = Some(row);
            break;
        }
        assert_eq!(p, 1, "row {row} above the split must use the old palette");
    }
    let split = split.expect("a row using the new palette exists below the split");
    // The split must land in the active region (200 raster rows of content),
    // not at the very top or beyond the visible area.
    assert!(
        (1..200).contains(&split),
        "split row {split} should fall inside the active picture"
    );
    // Every active row at or below the split uses the new palette.
    for row in split..200 {
        assert_eq!(
            raster.pixels[row * w],
            9,
            "row {row} below the split must use the new palette"
        );
    }
}

#[test]
fn line_compare_split_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh(); // double-scanned byte mode
    // A0000 writes fill plane 0 with a full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    // Mark the top of VRAM (plane 0 offset 0) with bit 7 only: pixel 0 set, the rest
    // clear. The split region reads this; a non-uniform byte also detects a
    // wrongly-applied pel-pan below the split.
    machine.write_physical_u8(0x000A_0000, 0x80);
    // Identity attribute palette so index 1 -> DAC 1. read_status1 resets the
    // flip-flop to "index"; 16 entries * 2 writes leaves it in "index" mode.
    machine.video_mut().read_status1();
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i); // index, PAS on
        machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
    }
    // Lock pel-pan below the split (Attribute Mode Control 10h bit 5) and pan the
    // top by 4. The flip-flop is in "index" mode here.
    machine.video_mut().write_port(0x3C0, 0x30); // attr index 0x10, PAS on
    machine.video_mut().write_port(0x3C0, 0x20); // bit 5: pel-pan up to line compare
    machine.video_mut().write_port(0x3C0, 0x33); // attr index 0x13, PAS on
    machine.video_mut().write_port(0x3C0, 0x04); // pan 4
    // Program a split at scan-counter line 100. The mode default line compare is
    // 0x3FF, so the overflow (07h) bit 8 and max-scan (09h) bit 9 must be cleared.
    // The 09h write touches only line compare bit 9, not the double-scan bit.
    machine.video_mut().write_port(0x3D4, 0x07);
    machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 8 = 0
    machine.video_mut().write_port(0x3D4, 0x09);
    machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 9 = 0
    machine.video_mut().write_port(0x3D4, 0x18);
    machine.video_mut().write_port(0x3D5, 0x64); // line compare low 8 bits = 100
    // Scroll the top region to a cleared area of VRAM (start address 0x4000),
    // buffered until the next vertical retrace.
    machine.video_mut().write_port(0x3D4, 0x0C);
    machine.video_mut().write_port(0x3D5, 0x40); // start address high
    machine.video_mut().write_port(0x3D4, 0x0D);
    machine.video_mut().write_port(0x3D5, 0x00); // start address low
    // First frame latches the buffered start address; the second renders with it.
    machine.advance_devices(400_000);
    machine.advance_devices(400_000);
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize; // 320
    // A top scanline (50 < 100) reads the scrolled, cleared region: index 0.
    assert_eq!(
        raster.pixels[50 * w],
        0,
        "top region is scrolled to cleared VRAM"
    );
    assert_eq!(
        raster.pixels[101 * w],
        0,
        "EGA keeps two extra scanlines in the top region"
    );
    // The first EGA split scanline (103 = line_compare + 3) reads offset 0
    // (the marked byte), with pel-pan forced to 0 below the split.
    assert_eq!(
        raster.pixels[103 * w],
        1,
        "split region reads offset 0 with pel-pan forced to 0"
    );
}

#[test]
fn display_address_wrap_seam_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh(); // byte mode
    // Plane 0 datapath: map mask plane 0, full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01);
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF);
    // Mark the top of VRAM: plane 0 offset 0 = 0xFF (pixels 0..7 -> attribute index 1).
    machine.write_physical_u8(0x000A_0000, 0xFF);
    // Identity palette so index 1 -> DAC 1.
    machine.video_mut().read_status1(); // reset attr flip-flop to index
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i);
        machine.video_mut().write_port(0x3C0, i);
    }
    // Set start_address = 0xFFF8 through the CRTC ports (buffered until vretrace).
    machine.video_mut().write_port(0x3D4, 0x0C); // start address high
    machine.video_mut().write_port(0x3D5, 0xFF);
    machine.video_mut().write_port(0x3D4, 0x0D); // start address low
    machine.video_mut().write_port(0x3D5, 0xF8);
    // First frame latches the buffered start address; the second renders with it.
    machine.advance_devices(400_000);
    machine.advance_devices(400_000);
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize; // 320
    // Row 0: pixels 0..63 read 0xFFF8..0xFFFF (clear), pixels 64..71 wrap to offset 0.
    assert_eq!(raster.pixels[0], 0, "pre-wrap pixel reads the cleared tail");
    assert_eq!(
        raster.pixels[64], 1,
        "wrapped scanout pixel equals the top-of-VRAM pixel (no tear)"
    );
    // Sanity: still on row 0 of the active area.
    assert!(w >= 72);
}

#[test]
fn frame_generation_tracks_graphics_writes() {
    let mut machine = test_machine();

    // Text mode (the power-up default) is never memoized: cursor/attribute blink
    // toggles with no guest write, so the gen must be None so the GUI keeps
    // re-rendering.
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(
        machine.frame_generation(),
        None,
        "text mode is not memoizable (blink)"
    );

    // A graphics mode (mode 13h) is a pure function of guest writes, so it gets a
    // generation key.
    machine.video_mut().set_mode13h();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    let gen0 = machine
        .frame_generation()
        .expect("graphics mode has a generation");

    // Stable across repeated calls with no intervening writes (so a static screen
    // stays a cache hit).
    assert_eq!(
        machine.frame_generation(),
        Some(gen0),
        "no write -> same generation"
    );
    assert_eq!(
        machine.frame_generation(),
        Some(gen0),
        "still stable on a third call"
    );

    // A write into the VGA memory aperture changes the key (the framebuffer moved).
    machine.write_physical_u8(0xA0000, 0x2A);
    let gen1 = machine.frame_generation().expect("still graphics");
    assert_ne!(gen1, gen0, "a VRAM write bumps the generation");

    // ...and is stable again afterward.
    assert_eq!(
        machine.frame_generation(),
        Some(gen1),
        "stable after the VRAM write"
    );

    // A VGA register / DAC port write (a palette change is the classic graphics-mode
    // output change with no VRAM write) changes the key. 0x3C8/0x3C9 are the DAC
    // write-index / data ports.
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3C8, BusWidth::Byte, 0x00, false).unwrap(); // DAC write index 0
        bus.write_io(0x3C9, BusWidth::Byte, 0x3F, false).unwrap(); // red component
    }
    let gen2 = machine.frame_generation().expect("still graphics");
    assert_ne!(gen2, gen1, "a VGA port write bumps the generation");

    // A mode / resolution change always moves the key (the raster dims are folded
    // into the key).
    assert!(machine.set_vga_mode(0x12)); // 640x480 planar (raster 640x525)
    assert_eq!(machine.video().active_mode(), VideoMode::Planar);
    let gen3 = machine.frame_generation().expect("planar is graphics");
    assert_ne!(gen3, gen2, "a resolution change moves the generation");

    // Returning to text mode drops back to None.
    machine.video_mut().set_text_mode();
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(
        machine.frame_generation(),
        None,
        "back in text mode -> not memoizable"
    );
}

#[test]
fn presented_frame_generation_waits_for_the_matching_completed_raster() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h_with_clear(true);
    let frame_dots = machine.video().frame_dots();
    machine.video_mut().advance(frame_dots);
    let presented_before = machine
        .presented_frame_generation()
        .expect("the first graphics raster completed");
    let live_before = machine.frame_generation().unwrap();

    machine.write_physical_u8(0xA0000, 0x2A);
    assert_ne!(machine.frame_generation(), Some(live_before));
    assert_eq!(
        machine.presented_frame_generation(),
        Some(presented_before),
        "an in-progress write does not relabel the prior raster"
    );

    machine.video_mut().advance(frame_dots);
    assert_ne!(
        machine.presented_frame_generation(),
        Some(presented_before),
        "the generation moves when the matching raster completes"
    );
}

// ---------------------------------------------------------------------------
// Defect E8, at its source.
//
// `presented_frame_argb` answers "the most recently completed display frame".
// There are two moments when there is none: before the first frame of the run,
// and between a mode set and the first raster of the new mode — every mode set
// drops the presented frame on purpose, so a consumer is never handed a frame
// with the previous mode's geometry.
//
// It used to answer both with a hardcoded one-pixel black image. The stage-1
// sweep archived 30 of them, one in each of 30 games, and they read as data:
// a 1x1 frame is vacuously "one solid colour", which is the blank-screen
// signature the classifier looks for. `None` is the only honest answer, and it
// is the one `presented_frame_generation` beside it has always given.
// ---------------------------------------------------------------------------

#[test]
fn a_machine_that_has_completed_no_frame_presents_nothing() {
    let machine = test_machine();
    assert!(
        machine.presented_frame_argb().is_none(),
        "a frame that does not exist must not be substituted"
    );
}

#[test]
fn a_mode_set_leaves_no_presented_frame_until_the_next_raster_completes() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h_with_clear(true);
    let frame_dots = machine.video().frame_dots();
    machine.video_mut().advance(frame_dots);

    let (words, width, height) = machine
        .presented_frame_argb()
        .expect("the first mode 13h raster completed");
    assert_eq!((width, height), (320, 400));
    assert_eq!(words.len(), width * height);

    // The mode set drops it. Until the beam finishes a frame there is nothing
    // to present, and that window is up to a whole frame period. Mode 12h is
    // 640x480, so the frame that eventually arrives also proves the geometry
    // followed the mode instead of a stale raster surviving the switch.
    assert!(machine.set_vga_mode(0x12));
    assert!(
        machine.presented_frame_argb().is_none(),
        "the frame from the previous mode is gone and the new one is not drawn"
    );

    let frame_dots = machine.video().frame_dots();
    machine.video_mut().advance(frame_dots);
    let (words, width, height) = machine
        .presented_frame_argb()
        .expect("the first mode 12h raster completed");
    assert_eq!((width, height), (640, 480));
    assert_eq!(words.len(), width * height);
}

/// The guard that names the defect. Whatever `presented_frame_argb` returns, it
/// is never a frame too small to be a screen: the smallest mode the Vega BIOS
/// presents is 320x200, and the archived defect was 1x1.
#[test]
fn a_presented_frame_is_never_smaller_than_a_real_video_mode() {
    let mut machine = test_machine();
    for mode in [0x13u8, 0x0D, 0x12, 0x10] {
        assert!(machine.set_vga_mode(mode), "mode {mode:#04x}");
        // Sample across the whole frame, including the window right after the
        // mode set where the old code substituted a one-pixel image.
        let frame_dots = machine.video().frame_dots();
        for step in 0..8 {
            if step > 0 {
                machine.video_mut().advance(frame_dots / 4);
            }
            if let Some((words, width, height)) = machine.presented_frame_argb() {
                assert!(
                    width * height >= 320 * 200,
                    "mode {mode:#04x} presented a {width}x{height} frame"
                );
                assert_eq!(words.len(), width * height);
            }
        }
    }
}

#[test]
fn frame_generation_tracks_same_dims_mode_switch() {
    // Mode 13h and mode 0Dh are both 320x449 raster, so the dimension fold in
    // frame_generation cannot tell them apart. A program switching between them
    // (INT 10h AH=00h, no intervening VRAM write) must still move the key, or the
    // host frame cache would freeze on the switch. The mode-set helpers bump the
    // content gen to cover this.
    let mut machine = int15_machine(16);
    machine.video_mut().set_mode13h();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    let dims_before = (
        machine.video().raster_width(),
        machine.video().raster_height(),
    );
    let before = machine
        .frame_generation()
        .expect("mode 13h is a graphics mode");

    assert!(machine.video_mut().set_mode(0x0D)); // 320x200x16 planar, same raster dims
    let dims_after = (
        machine.video().raster_width(),
        machine.video().raster_height(),
    );
    assert_eq!(
        dims_before, dims_after,
        "13h and 0Dh share raster dims, so the dims fold cannot move the key"
    );
    let after = machine
        .frame_generation()
        .expect("mode 0Dh is a graphics mode");
    assert_ne!(
        after, before,
        "a same-dims graphics-to-graphics mode switch must still bump the generation"
    );
}

#[test]
fn frame_generation_tracks_hle_bios_graphics_writes() {
    // Mode 13h, INT 10h AH=0Ch WRITE PIXEL (AL=color, CX=col, DX=row, BH=page).
    let mut machine = int15_machine(16);
    machine.video_mut().set_mode13h();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    let before = machine
        .frame_generation()
        .expect("mode 13h is a graphics mode");

    machine.cpu.registers.set_eax(0x0C2A); // AH=0Ch, AL=0x2A
    machine.cpu.registers.set_ebx(0x0000); // BH=page 0
    machine.cpu.registers.set_ecx(10); // column
    machine.cpu.registers.set_edx(20); // row
    machine.handle_int10();
    let after = machine.frame_generation().expect("still mode 13h");
    assert_ne!(
        after, before,
        "INT 10h AH=0Ch write-pixel must bump the generation (HLE bypasses the bus)"
    );

    // CGA graphics (mode 04h), INT 10h AH=0Eh TELETYPE — draws a glyph as pixels.
    let mut cga = int15_machine(16);
    cga.cpu.registers.set_eax(0x0004); // set CGA graphics mode 04h
    cga.handle_int10();
    assert_eq!(cga.video().active_mode(), VideoMode::Cga);
    let dims_before = (cga.video().raster_width(), cga.video().raster_height());
    let before = cga
        .frame_generation()
        .expect("CGA graphics has a generation");

    cga.cpu.registers.set_eax(0x0E41); // AH=0Eh TTY, AL='A'
    cga.cpu.registers.set_ebx(0x0002); // BH=page 0, BL=color 2
    cga.handle_int10();
    let dims_after = (cga.video().raster_width(), cga.video().raster_height());
    assert_eq!(
        dims_before, dims_after,
        "dims unchanged, so the dims fold can't mask the bump"
    );
    let after = cga.frame_generation().expect("still CGA graphics");
    assert_ne!(
        after, before,
        "INT 10h AH=0Eh teletype in CGA graphics must bump the generation"
    );

    // A palette change via INT 10h AH=10h AL=10h (set one DAC register) in mode 13h
    // — the classic graphics output change with no VRAM write — must bump too.
    let mut pal = int15_machine(16);
    pal.video_mut().set_mode13h();
    let before = pal.frame_generation().expect("mode 13h graphics");
    pal.cpu.registers.set_eax(0x1010); // AH=10h, AL=10h set DAC register
    pal.cpu.registers.set_ebx(0x0005); // BX = DAC index 5
    pal.cpu.registers.set_ecx(0x3F00); // CH=green, CL=blue
    pal.cpu.registers.set_edx(0x3F00); // DH=red
    pal.handle_int10();
    let after = pal.frame_generation().expect("still mode 13h");
    assert_ne!(
        after, before,
        "INT 10h AH=10h palette write must bump the generation"
    );
}

#[test]
fn set_vga_mode_selects_graphics_geometry_per_number() {
    let mut machine = test_machine();

    assert!(machine.set_vga_mode(0x0E));
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 449);

    assert!(machine.set_vga_mode(0x12));
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 525);

    assert!(machine.set_vga_mode(0x13));
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    assert_eq!(machine.video().raster_width(), 320);
    assert_eq!(machine.video().raster_height(), 449);

    assert!(!machine.set_vga_mode(0x99));
}

#[test]
fn int10_paradise_special_mode_selects_existing_mode() {
    let mut machine = int15_machine(16);

    machine.cpu.registers.set_eax(0x007E);
    machine.cpu.registers.set_ebx(320);
    machine.cpu.registers.set_ecx(200);
    machine.cpu.registers.set_edx(256);
    machine.handle_int10();
    assert_eq!((machine.cpu.registers.eax() as u16) & 0x00FF, 0x007E);
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7E);
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    assert_eq!(machine.read_physical_u8(0x449), 0x13);

    machine.cpu.registers.set_eax(0x007E);
    machine.cpu.registers.set_ebx(800);
    machine.cpu.registers.set_ecx(600);
    machine.cpu.registers.set_edx(16);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x00);
}

#[test]
fn int10_paradise_extended_status_and_registers() {
    let mut machine = int15_machine(16);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0A5A);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x1A00);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);
    assert_eq!((machine.cpu.registers.ebx() as u16) & 0x00FF, 0x005A);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0100);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0200);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);
    assert_eq!((machine.cpu.registers.ebx() as u16) & 0x00FF, 0x0001);
    assert_eq!(machine.cpu.registers.ecx() as u16, 0x0401);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0700);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x449), 0x03);
    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0200);
    machine.handle_int10();
    assert_eq!((machine.cpu.registers.ebx() as u16) & 0x00FF, 0x0000);

    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    machine.cpu.registers.set_edi(0x0100);
    machine.write_physical_u8(0x40100, 0xFF);
    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x6100);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);
    assert_eq!(machine.read_physical_u8(0x40100), 0x00);
}

#[test]
fn int10_ega_modes_publish_bda_geometry() {
    let mut machine = int15_machine(16);

    for (mode, height, page_size) in [
        (0x0D, 8, 0x2000),
        (0x0E, 8, 0x4000),
        (0x0F, 14, 0x8000),
        (0x10, 14, 0x8000),
        (0x11, 16, 0x0000),
        (0x12, 16, 0x0000),
    ] {
        machine.cpu.registers.set_eax(mode);
        machine.handle_int10();
        assert_eq!(machine.read_physical_u8(0x485), height, "mode {mode:02X}");
        assert_eq!(
            machine.read_physical_u16(0x44C),
            page_size,
            "mode {mode:02X}"
        );
    }
}

#[test]
fn int10_sets_mode_12h_then_draws_and_presents_640x480() {
    // mov ax, 0012h; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x12, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 525);

    // Draw attribute index 1 into the first byte of plane 0 (first 8 pixels of
    // the top row) through the A0000 datapath, with an identity palette.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000, 0xFF);
    machine.video_mut().read_status1(); // reset attr flip-flop to index
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i); // index, PAS on
        machine.video_mut().write_port(0x3C0, i); // palette[i] = i
    }

    // A 12h frame is 800 * 525 = 420 000 dots; 600 000 clocks (~604 000 dots)
    // completes at least one frame.
    machine.advance_devices(600_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 640);
    assert_eq!(raster.height, 525);
    assert_eq!(raster.pixels[0], 1, "top-left pixel is attribute index 1");
}

#[test]
fn int10_sets_ega_mode_0fh_through_planar_dispatch() {
    // mov ax,000Fh; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x0f, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().active_mode(), VideoMode::Planar);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 449);
    assert_eq!(machine.read_physical_u8(0x449), 0x0f);
    assert_eq!(machine.read_physical_u16(0x463), 0x03B4);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3B4, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(0x3B5, BusWidth::Byte, 0x12, false).unwrap();
        bus.write_io(0x3B4, BusWidth::Byte, 0x0D, false).unwrap();
        bus.write_io(0x3B5, BusWidth::Byte, 0x34, false).unwrap();
        assert!(bus.read_io(0x3BA, BusWidth::Byte, 0, false).is_ok());
    }
    assert_eq!(machine.video().pending_start_address(), Some(0x1234));
}

#[test]
fn int10_vga_graphics_modes_honor_clear_and_preserve_flag() {
    let mut machine = int15_machine(16);

    machine.cpu.registers.set_eax(0x0013);
    machine.handle_int10();
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x5a);
    machine.cpu.registers.set_eax(0x0093);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x5a);
    machine.cpu.registers.set_eax(0x0013);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x00);

    machine.cpu.registers.set_eax(0x0010);
    machine.handle_int10();
    machine.video_mut().cpu_write(0, 0xa5);
    assert_eq!(machine.video().plane_byte(0, 0), 0xa5);
    machine.cpu.registers.set_eax(0x0090);
    machine.handle_int10();
    assert_eq!(machine.video().plane_byte(0, 0), 0xa5);
    machine.cpu.registers.set_eax(0x0010);
    machine.handle_int10();
    assert_eq!(machine.video().plane_byte(0, 0), 0x00);
}

#[test]
fn int10_returns_to_text_mode() {
    // mov ax,0013h; int 10h; mov ax,0003h; int 10h; hlt
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, 0xcd, 0x10, 0xb8, 0x03, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    // Stamp a recognizable pattern into the text buffer before the toggles.
    machine.video_mut().write_u8(0, b'X').unwrap();
    machine.video_mut().write_u8(1, 0x4e).unwrap();
    machine
        .video_mut()
        .write_u8(VGA_TEXT_MEMORY_SIZE - 2, b'Y')
        .unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // Returning to text hands the display back to the VGA core text path
    // (now a raster) and clears the Margo latch.
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    // set_text_mode blanks the buffer to spaces with the 0x07 attribute.
    assert_eq!(machine.video().read_u8(0).unwrap(), b' ');
    assert_eq!(machine.video().read_u8(1).unwrap(), 0x07);
    assert_eq!(
        machine.video().read_u8(VGA_TEXT_MEMORY_SIZE - 2).unwrap(),
        b' '
    );
}

/// The delta path and the contract path must present the SAME picture in
/// canonical Mode 13h.
///
/// Mode 13h does not present from the index raster at all: it presents from the
/// VGA's own incrementally maintained ARGB cache, cropped to the display
/// height. A delta path that re-derives the frame from `last_presented` and the
/// DAC would be a second definition of what is on screen, agreeing only by
/// coincidence. This pins the agreement.
#[test]
fn mode13h_presents_identically_through_both_frame_paths() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    machine.video_mut().set_dac_entry(1, 63, 0, 0);
    machine.video_mut().set_dac_entry(2, 0, 63, 0);
    machine.write_physical_u8(VGA_MODE13H_BASE, 1);
    machine.write_physical_u8(VGA_MODE13H_BASE + 320 * 7 + 9, 2);
    machine.advance_devices(600_000);

    let (words, width, height) = machine
        .presented_frame_argb()
        .expect("a raster completed during the advance");
    let update = machine
        .presented_frame_update()
        .expect("the delta path answers wherever the contract path does");

    assert_eq!((update.width, update.height), (width, height));
    assert_eq!(update.words.as_slice(), words.as_slice());
}

/// A guest may program the vertical display end PAST the vertical total, which
/// leaves a short raster: `vdisp_end` rows are claimed but only `vtotal` rows
/// were ever rendered. `recompute_vertical_timing` honours the guest's bytes
/// without clamping, and register-banging 256-colour titles reach this while
/// they retune a tweaked mode.
///
/// `presented_frame_argb` answers by truncating to what was rendered. The delta
/// path must present exactly that, and in particular must not answer `None`:
/// that is the placeholder-frame failure mode from the other side -- a screen
/// that goes dark for as long as the guest holds the timing, in a moment where
/// there IS a frame to show.
#[test]
fn a_short_raster_presents_identically_through_both_frame_paths() {
    let mut machine = test_machine();
    assert!(machine.set_vga_mode(0x13));
    machine.video_mut().set_dac_entry(1, 63, 0, 0);
    machine.write_physical_u8(VGA_MODE13H_BASE, 1);
    // Vertical display end 457 (r12 + the overflow bit already set by Mode 13h,
    // + 1) against a vertical total that stays at 449: the protect bit in r11
    // keeps 00h-07h out, so the guest moves the active region past the end of
    // the raster without moving the raster.
    assert!(machine.video_mut().write_port(0x3D4, 0x12));
    assert!(machine.video_mut().write_port(0x3D5, 200));
    machine.advance_devices(600_000);

    let (words, width, height) = machine
        .presented_frame_argb()
        .expect("a raster completed after the retune");
    assert_eq!((width, height), (320, 457));
    assert_eq!(
        words.len(),
        320 * 449,
        "the raster is short: fewer words than width * height"
    );

    let update = machine
        .presented_frame_update()
        .expect("the delta path answers wherever the contract path does");
    assert_eq!((update.width, update.height), (width, height));
    assert_eq!(update.words.as_slice(), words.as_slice());

    // And a second call must not diff a frame it cannot index row by row.
    let again = machine
        .presented_frame_update()
        .expect("the short raster still presents");
    assert_eq!(again.words.as_slice(), words.as_slice());
}
