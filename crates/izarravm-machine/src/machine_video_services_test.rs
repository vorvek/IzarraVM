// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn int10_0bh_sets_border_overscan() {
    // mov ax,0b00h; mov bx,0005h; int 10h; hlt  (AH=0Bh, BH=0 border, BL=5)
    let rom = rom_with_code(&[0xb8, 0x00, 0x0b, 0xbb, 0x05, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().overscan(), 5);
}

#[test]
fn int10_0bh_sets_cga_background_and_palette() {
    // mode 04h; AH=0Bh/BH=0 background blue + high intensity; AH=0Bh/BH=1 palette 1.
    let rom = rom_with_code(&[
        0xb8, 0x04, 0x00, 0xcd, 0x10, 0xb8, 0x00, 0x0b, 0xbb, 0x11, 0x00, 0xcd, 0x10, 0xb8, 0x00,
        0x0b, 0xbb, 0x01, 0x01, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.memory.read_u16(0x44c).unwrap(), 0x4000);
    machine.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(&raster.pixels[0..4], &[1, 11, 13, 15]);
}

#[test]
fn int10_1003_toggles_cga_text_blink_bit() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0001);
    machine.handle_int10();
    assert_ne!(machine.video().cga_mode_control() & 0x20, 0);

    machine.cpu.registers.set_eax(0x1003);
    machine.cpu.registers.set_ebx(0x0000);
    machine.handle_int10();
    assert_eq!(machine.video().cga_mode_control() & 0x20, 0);

    machine.cpu.registers.set_eax(0x1003);
    machine.cpu.registers.set_ebx(0x0001);
    machine.handle_int10();
    assert_ne!(machine.video().cga_mode_control() & 0x20, 0);
}

#[test]
fn int10_cga_bda_latches_track_bios_control_writes() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0006);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x1A);
    assert_eq!(machine.read_physical_u8(0x466), 0x0F);

    machine.cpu.registers.set_eax(0x0B00);
    machine.cpu.registers.set_ebx(0x0011);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x466), 0x11);

    machine.cpu.registers.set_eax(0x0B00);
    machine.cpu.registers.set_ebx(0x0101);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x466), 0x31);

    machine.cpu.registers.set_eax(0x0002);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x2D);

    machine.cpu.registers.set_eax(0x1003);
    machine.cpu.registers.set_ebx(0x0000);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x0D);
}

#[test]
fn int10_non_cga_mode_set_clears_cga_bda_latches() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0006);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x1A);
    assert_eq!(machine.read_physical_u8(0x466), 0x0F);

    machine.cpu.registers.set_eax(0x000D);
    machine.handle_int10();

    assert_eq!(machine.read_physical_u8(0x465), 0);
    assert_eq!(machine.read_physical_u8(0x466), 0);
    machine.cpu.registers.set_eax(0x1B00);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x20), 0);
    assert_eq!(machine.read_physical_u8(0x21), 0);
}

#[test]
fn int10_ah05_sets_the_text_page_via_start_address() {
    // mov ax,0501h; int 10h; hlt  (AH=05h, AL=1 -> display page 1)
    let rom = rom_with_code(&[0xb8, 0x01, 0x05, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // Page 1 sits at byte 4096 = cell 2048. AH=05h routes through
    // set_start_address (the vretrace latch), so the value is buffered in
    // pending_start before the next frame boundary applies it.
    assert_eq!(
        machine.video().pending_start_address(),
        Some(2048),
        "AH=05h page 1 buffers start address 2048 (cell)"
    );
    assert_eq!(
        machine.video().crtc_start_address(),
        0,
        "start address applies at the next vretrace, not mid-frame"
    );
}

#[test]
fn int10_ah05_uses_40_column_page_stride() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0001);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), Some(1024));
    assert_eq!(machine.read_physical_u8(0x462), 1);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 2048);

    machine.cpu.registers.set_eax(0x0F00);
    machine.cpu.registers.set_ebx(0);
    machine.handle_int10();
    assert_eq!((machine.cpu.registers.ebx() >> 8) as u8, 1);
}

#[test]
fn int10_text_services_use_the_selected_cga_text_page() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0001);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    machine.cpu.registers.set_eax(0x0200); // cursor page 1, row 0 col 0
    machine.cpu.registers.set_ebx(0x0100);
    machine.cpu.registers.set_edx(0);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0950); // write 'P'/attr 1E on page 1
    machine.cpu.registers.set_ebx(0x011E);
    machine.cpu.registers.set_ecx(1);
    machine.handle_int10();

    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b' ');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2048), b'P');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2049), 0x1E);

    machine.cpu.registers.set_eax(0x0800);
    machine.cpu.registers.set_ebx(0x0100);
    machine.handle_int10();
    assert_eq!(machine.cpu.registers.eax() as u16, 0x1E50);

    machine.cpu.registers.set_eax(0x0300);
    machine.cpu.registers.set_ebx(0x0100);
    machine.handle_int10();
    assert_eq!(machine.cpu.registers.edx() as u16, 0);
}

#[test]
fn int10_mode02_wraps_display_pages_at_the_cga_16kb_window() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0002);
    machine.handle_int10();

    machine.cpu.registers.set_eax(0x0503);
    machine.handle_int10();
    assert_eq!(machine.video().pending_start_address(), Some(0x1800));
    assert_eq!(machine.read_physical_u8(0x462), 3);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0x3000);

    machine.cpu.registers.set_eax(0x0504);
    machine.handle_int10();
    assert_eq!(machine.video().pending_start_address(), Some(0));
    assert_eq!(machine.read_physical_u8(0x462), 0);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0);
}

#[test]
fn int10_ah05_ignores_cga_graphics_single_page() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0004);
    machine.handle_int10();
    machine.video_mut().cga_write(0, 0b01_01_01_01);

    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), None);
    assert_eq!(machine.video().crtc_start_address(), 0);
    assert_eq!(machine.read_physical_u8(0x462), 0);
    assert_eq!(&machine.video().render_cga_row(0)[0..4], &[2, 2, 2, 2]);
}

#[test]
fn int10_ah05_selects_ega_graphics_display_page() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x000D);
    machine.handle_int10();
    machine.write_physical_u8(0x000A_0000 + 0x2000, 0x80);

    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), Some(0x2000));
    assert_eq!(machine.read_physical_u8(0x462), 1);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0x2000);

    machine.advance_devices(600_000);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.pixels[0], 0x17);

    machine.cpu.registers.set_eax(0x0012);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), Some(0));
    assert_eq!(machine.read_physical_u8(0x462), 0);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0);
}

#[test]
fn int10_ah05_page_flip_scrolls_through_the_machine() {
    // Drive a full AH=05h page flip end-to-end: pre-seed page 0 and page 1
    // with distinct glyphs, call the BIOS service for page 1, run a frame
    // so the latch applies, and confirm the pixel scanout reads page 1.
    //   mov ax,0501h ; AH=05h, AL=1 (display page 1)
    //   int 10h
    //   hlt
    let rom = rom_with_code(&[0xb8, 0x01, 0x05, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Page 0 cell 0 = 'A'; page 1 cell 0 (cell 2048, byte 4096) = 'Z'.
    let video = machine.video_mut();
    video.write_u8(0, b'A').unwrap();
    video.write_u8(1, 0x0F).unwrap();
    video.write_u8(4096, b'Z').unwrap();
    video.write_u8(4097, 0x0F).unwrap();

    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    // The latch is buffered; the start address has not applied yet.
    let video = machine.video_mut();
    assert_eq!(
        video.frame().cells[0].character,
        b'A',
        "before vretrace the displayed page is still 0"
    );
    // Advance one frame so finalize_frame applies the buffered start address.
    let dots = video.frame_dots();
    video.advance(dots);
    assert_eq!(
        video.frame().cells[0].character,
        b'Z',
        "after vretrace the displayed page scrolls to page 1"
    );
}

#[test]
fn int10_10h_sets_palette_register() {
    // mov ax,1000h; mov bx,0901h; int 10h; hlt  (AH=10h AL=00, BL=1, BH=9)
    let rom = rom_with_code(&[0xb8, 0x00, 0x10, 0xbb, 0x01, 0x09, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().attr_palette_reg(1), 9);
}

#[test]
fn int10_10h_sets_individual_dac() {
    // mov ax,1010h; mov bx,0028h; mov dx,3f00h; mov cx,0000h; int 10h; hlt
    // (AH=10h AL=10, BX=40, DH=63 R, CH=0 G, CL=0 B)
    let rom = rom_with_code(&[
        0xb8, 0x10, 0x10, 0xbb, 0x28, 0x00, 0xba, 0x00, 0x3f, 0xb9, 0x00, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().dac_entry(40), [63, 0, 0]);
}

#[test]
fn int10_10h_sets_dac_block() {
    // ES:DX -> a 3-triple buffer at 1000:0000 (physical 0x10000).
    // mov ax,1000h; mov es,ax; mov dx,0; mov ax,1012h; mov bx,000ah; mov cx,3; int 10h; hlt
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x10, 0x8e, 0xc0, 0xba, 0x00, 0x00, 0xb8, 0x12, 0x10, 0xbb, 0x0a, 0x00, 0xb9,
        0x03, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // The three triples at 0x10000: red, green, blue.
    for (i, &b) in [63u8, 0, 0, 0, 63, 0, 0, 0, 63].iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().dac_entry(10), [63, 0, 0]);
    assert_eq!(machine.video().dac_entry(11), [0, 63, 0]);
    assert_eq!(machine.video().dac_entry(12), [0, 0, 63]);
}

#[test]
fn int10_10h_gets_dac_block() {
    // AL=17 reads CX DAC entries starting at BX into ES:DX.
    // mov ax,1000h; mov es,ax; mov dx,0; mov ax,1017h; mov bx,000ah; mov cx,3; int 10h; hlt
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x10, 0x8e, 0xc0, 0xba, 0x00, 0x00, 0xb8, 0x17, 0x10, 0xbb, 0x0a, 0x00, 0xb9,
        0x03, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Seed DAC entries 10/11/12 with known values, then let the readback run.
    machine.video_mut().set_dac_entry(10, 12, 34, 56);
    machine.video_mut().set_dac_entry(11, 1, 2, 3);
    machine.video_mut().set_dac_entry(12, 63, 63, 63);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The handler wrote CX*3 bytes to 0x10000.
    assert_eq!(machine.read_physical_u8(0x1_0000), 12);
    assert_eq!(machine.read_physical_u8(0x1_0001), 34);
    assert_eq!(machine.read_physical_u8(0x1_0002), 56);
    assert_eq!(machine.read_physical_u8(0x1_0006), 63);
    assert_eq!(machine.read_physical_u8(0x1_0007), 63);
    assert_eq!(machine.read_physical_u8(0x1_0008), 63);
}

#[test]
fn int10_10h_reads_overscan() {
    // AL=01 sets the overscan to BH=0x2A, then AL=08 reads it back into BH.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1001);
    m.cpu.registers.set_ebx(0x2A00); // BH = 0x2A
    m.handle_int10();
    m.cpu.registers.set_eax(0x1008);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!((m.cpu.registers.ebx() as u16 >> 8) as u8, 0x2A);
}

#[test]
fn int10_1001_sets_cga_graphics_intensity() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x1001);
    m.cpu.registers.set_ebx(0x1100);
    m.handle_int10();

    m.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    let raster = m.video_mut().render_full_frame();
    assert_eq!(&raster.pixels[0..4], &[1, 10, 12, 14]);
}

#[test]
fn int10_1000_11_sets_cga_overscan_register() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x1000);
    m.cpu.registers.set_ebx(0x1111);
    m.handle_int10();

    m.cpu.registers.set_eax(0x1007);
    m.cpu.registers.set_ebx(0x0011);
    m.handle_int10();
    assert_eq!((m.cpu.registers.ebx() >> 8) as u8, 0x11);

    m.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    let raster = m.video_mut().render_full_frame();
    assert_eq!(&raster.pixels[0..4], &[1, 10, 12, 14]);
}

#[test]
fn int10_10h_reads_cga_color_select_low_bits() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    assert!(m.video_mut().write_port(0x3D9, 0x3F));

    m.cpu.registers.set_eax(0x1008);
    m.cpu.registers.set_ebx(0);
    m.handle_int10();
    assert_eq!((m.cpu.registers.ebx() >> 8) as u8, 0x1F);

    m.cpu.registers.set_eax(0x1009);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1000));
    m.cpu.registers.set_edx(0);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x1_0010), 0x1F);
}

#[test]
fn int10_10h_reads_all_palette_registers() {
    // AL=09 writes the 16 palette registers + overscan to ES:DX. Mode 03h
    // starts from the VGABios text Attribute Controller table, followed by
    // overscan 0.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1009);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1000));
    m.cpu.registers.set_edx(0x0000);
    m.handle_int10();
    let expected = [
        0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E,
        0x3F,
    ];
    for (i, value) in expected.into_iter().enumerate() {
        assert_eq!(m.read_physical_u8(0x1_0000 + i as u32), value);
    }
    assert_eq!(
        m.read_physical_u8(0x1_0010),
        0,
        "overscan trails the 16 regs"
    );
}

#[test]
fn int10_10h_sums_dac_block_to_gray() {
    // AL=1B sums BX..BX+CX DAC entries to gray with NTSC luma weights.
    let mut m = int15_machine(16);
    m.video_mut().set_dac_entry(5, 63, 0, 0); // pure red
    m.video_mut().set_dac_entry(6, 0, 63, 0); // pure green
    m.cpu.registers.set_eax(0x101B);
    m.cpu.registers.set_ebx(0x0005); // start at index 5
    m.cpu.registers.set_ecx(0x0002); // two entries
    m.handle_int10();
    // Red gray = 63*77>>8 = 18; green gray = 63*151>>8 = 37. Each entry is now
    // an equal-component gray.
    let [r5, g5, b5] = m.video().dac_entry(5);
    assert_eq!((r5, g5, b5), (18, 18, 18));
    let [r6, g6, b6] = m.video().dac_entry(6);
    assert_eq!((r6, g6, b6), (37, 37, 37));
}

#[test]
fn int10_10h_reads_dac_page_state_default() {
    // AL=1A reports the power-up DAC paging state: mode 0 (BL), page 0 (BH).
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x101A);
    m.cpu.registers.set_ebx(0xFFFF);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
}

#[test]
fn int10_10h_sets_and_reads_pel_mask() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1018);
    m.cpu.registers.set_ebx(0x120F);
    m.handle_int10();
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0x0F));

    m.cpu.registers.set_eax(0x1019);
    m.cpu.registers.set_ebx(0xAB00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0xAB0F);
}

#[test]
fn int10_10h_selects_and_reports_dac_color_pages() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();

    // Attribute palette register 1 selects DAC low bits 5, then a pixel with
    // colour 1 scans out through the colour-page state below.
    m.cpu.registers.set_eax(0x1000);
    m.cpu.registers.set_ebx(0x0501);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0C01);
    m.cpu.registers.set_ebx(0);
    m.cpu.registers.set_ecx(0);
    m.cpu.registers.set_edx(0);
    m.handle_int10();

    // Mode 0: four 64-colour pages. Page 3 supplies DAC bits 7-6.
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0301);
    m.handle_int10();
    m.cpu.registers.set_eax(0x101A);
    m.cpu.registers.set_ebx(0xFFFF);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0300);
    assert_eq!(m.video_mut().render_full_frame().pixels[0], 0xC5);

    // Mode 1: sixteen 16-colour pages. Page 6 supplies DAC bits 7-4.
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0601);
    m.handle_int10();
    m.cpu.registers.set_eax(0x101A);
    m.cpu.registers.set_ebx(0);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0601);
    assert_eq!(m.video_mut().render_full_frame().pixels[0], 0x65);
}

#[test]
fn overlay_color_key_gates_on_the_primary_pixel() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32, pitch 2560
    // Primary at (10, 20) holds the key; (11, 20) holds an occluding window pixel.
    let key = 0x0011_2233u32;
    let occluder = 0x0044_5566u32;
    let p0 = 20 * 2560 + 10 * 4;
    let p1 = 20 * 2560 + 11 * 4;
    for (i, b) in key.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(MARGO_LFB_BASE + p0 + i as u32, b);
    }
    for (i, b) in occluder.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(MARGO_LFB_BASE + p1 + i as u32, b);
    }
    // YUY2 source: Y0=235 (white), Y1=16 (black).
    let src = 0x0020_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 4);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2);
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 2);
    write_mmio_reg(&mut machine, 0x60, key); // OVL_COLORKEY
    write_mmio_reg(&mut machine, 0x40, 1 | (1 << 3)); // ENABLE + KEY_EN, FORMAT YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Where the primary equals the key, the overlay shows (white).
    assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff);
    // Where another value occludes the key, the overlay is hidden and the
    // decoded primary pixel (0x00445566 in X8R8G8B8) remains.
    assert_eq!(argb[20 * 640 + 11], 0x0044_5566);
}

#[test]
fn overlay_yuy2_composites_through_the_apertures() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32
    // One YUY2 group offscreen (2 MiB in, past the 32bpp visible surface):
    // Y0=235 (white), U=128, Y1=16 (black), V=128. Byte order Y0, U, Y1, V.
    let src = 0x0020_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

    write_mmio_reg(&mut machine, 0x44, src); // OVL_SRC_Y (packed surface)
    write_mmio_reg(&mut machine, 0x48, 4); // OVL_SRC_PITCH
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2); // OVL_SRC_DIM: w=2, h=1
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY: x=10, y=20
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 2); // OVL_DST_DIM: w=2, h=1 (1:1)
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, FORMAT YUY2, no key

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff); // Y0 -> white
    assert_eq!(argb[20 * 640 + 11], 0x0000_0000); // Y1 -> black
}

#[test]
fn overlay_scales_by_point_sampling() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a);
    // The same one YUY2 group, scaled 2x horizontally: dst width 4.
    let src = 0x0020_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 4);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2); // src w=2, h=1
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4); // dst w=4, h=1 (2x)
    write_mmio_reg(&mut machine, 0x40, 1);

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // sx = dx * src_w / dst_w = dx * 2 / 4 = dx / 2:
    // dst 0,1 sample src pixel 0 (white); dst 2,3 sample src pixel 1 (black).
    assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff);
    assert_eq!(argb[20 * 640 + 11], 0x00ff_ffff);
    assert_eq!(argb[20 * 640 + 12], 0x0000_0000);
    assert_eq!(argb[20 * 640 + 13], 0x0000_0000);
}

#[test]
fn overlay_yv12_upsamples_chroma_through_the_apertures() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32
    // YV12 source, 2x2. Y plane (pitch 2): [16, 235; 16, 235]. A single shared
    // chroma sample (U=128, V=255) covers the whole 2x2 block (4:2:0 upsample).
    let yp = 0x0020_0000u32;
    let up = 0x0020_1000u32;
    let vp = 0x0020_2000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + yp, 16); // (0,0)
    machine.write_physical_u8(MARGO_LFB_BASE + yp + 1, 235); // (1,0)
    machine.write_physical_u8(MARGO_LFB_BASE + yp + 2, 16); // (0,1)
    machine.write_physical_u8(MARGO_LFB_BASE + yp + 3, 235); // (1,1)
    machine.write_physical_u8(MARGO_LFB_BASE + up, 128); // U plane
    machine.write_physical_u8(MARGO_LFB_BASE + vp, 255); // V plane

    write_mmio_reg(&mut machine, 0x44, yp); // OVL_SRC_Y
    write_mmio_reg(&mut machine, 0x48, 2); // OVL_SRC_PITCH (Y plane)
    write_mmio_reg(&mut machine, 0x4c, (2 << 16) | 2); // OVL_SRC_DIM: 2x2
    write_mmio_reg(&mut machine, 0x50, up); // OVL_SRC_U
    write_mmio_reg(&mut machine, 0x54, vp); // OVL_SRC_V
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY
    write_mmio_reg(&mut machine, 0x5c, (2 << 16) | 2); // OVL_DST_DIM: 2x2 (1:1)
    write_mmio_reg(&mut machine, 0x40, 1 | (1 << 1)); // ENABLE + FORMAT YV12

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Y=16 with (U=128, V=255) -> 0x00cb0000; Y=235 -> 0x00ff98ff. The same
    // chroma sample applies across the 2x2 block.
    assert_eq!(argb[20 * 640 + 10], 0x00cb_0000);
    assert_eq!(argb[20 * 640 + 11], 0x00ff_98ff);
    assert_eq!(argb[21 * 640 + 10], 0x00cb_0000);
    assert_eq!(argb[21 * 640 + 11], 0x00ff_98ff);
}

#[test]
fn overlay_yv12_chroma_traversal_addresses_each_cell() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32
    // 4x4 YV12 source with a flat Y of 128, so each output pixel's color is set
    // solely by which 2x2 chroma cell it samples. The 2x2 chroma grid (chroma
    // pitch = Y pitch / 2 = 2) holds a distinct (U, V) per cell, so this proves
    // cx = sx/2, cy = sy/2, and the chroma-plane stride, which the 2x2 test (only
    // cell 0,0) does not exercise.
    let yp = 0x0020_0000u32;
    let up = 0x0020_1000u32;
    let vp = 0x0020_2000u32;
    for i in 0..16u32 {
        machine.write_physical_u8(MARGO_LFB_BASE + yp + i, 128);
    }
    // Chroma cells indexed cy * 2 + cx.
    let us = [128u8, 128, 255, 255];
    let vs = [128u8, 255, 128, 255];
    for i in 0..4u32 {
        machine.write_physical_u8(MARGO_LFB_BASE + up + i, us[i as usize]);
        machine.write_physical_u8(MARGO_LFB_BASE + vp + i, vs[i as usize]);
    }

    write_mmio_reg(&mut machine, 0x44, yp); // OVL_SRC_Y
    write_mmio_reg(&mut machine, 0x48, 4); // OVL_SRC_PITCH (Y plane)
    write_mmio_reg(&mut machine, 0x4c, (4 << 16) | 4); // OVL_SRC_DIM: 4x4
    write_mmio_reg(&mut machine, 0x50, up); // OVL_SRC_U
    write_mmio_reg(&mut machine, 0x54, vp); // OVL_SRC_V
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY
    write_mmio_reg(&mut machine, 0x5c, (4 << 16) | 4); // OVL_DST_DIM: 4x4 (1:1)
    write_mmio_reg(&mut machine, 0x40, 1 | (1 << 1)); // ENABLE + FORMAT YV12

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Cell (0,0) U=128 V=128 -> gray; two pixels in the same cell share it.
    assert_eq!(argb[20 * 640 + 10], 0x0082_8282);
    assert_eq!(argb[21 * 640 + 11], 0x0082_8282);
    // Cell (1,0) U=128 V=255.
    assert_eq!(argb[20 * 640 + 12], 0x00ff_1b82);
    // Cell (0,1) U=255 V=128.
    assert_eq!(argb[22 * 640 + 10], 0x0082_51ff);
    // Cell (1,1) U=255 V=255.
    assert_eq!(argb[22 * 640 + 12], 0x00ff_00ff);
}

#[test]
fn pusher_runs_a_fill_packet_from_the_ring() {
    let mut machine = test_machine();
    // A command ring in system RAM that issues one FILL: a 2x2 rect of 0xAB at
    // (x=1, y=1) on a depth-1 surface, pitch 8, base 0. Mirrors the guide's
    // fill_via_pusher: header words are (count << 16) | method.
    let ring_base = 0x0001_0000u32;
    let ring: [u32; 16] = [
        (3 << 16) | 0x0100,
        0, // DST_BASE = 0
        8, // DST_PITCH = 8
        0, // SRC_BASE = 0 (unused by FILL)
        (1 << 16) | 0x0110,
        1, // DEPTH = 1
        (1 << 16) | 0x0114,
        (1 << 16) | 1, // DST_XY: y=1, x=1
        (1 << 16) | 0x011c,
        (2 << 16) | 2, // DIM: h=2, w=2
        (1 << 16) | 0x0120,
        0xab, // FG_COLOR = 0xAB
        (1 << 16) | 0x0128,
        0xf0, // ROP = PATCOPY
        (1 << 16) | 0x0150,
        0x01, // COMMAND = FILL
    ];
    for (i, word) in ring.iter().enumerate() {
        for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
        }
    }
    let put = (ring.len() * 4) as u32; // 64

    write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE (4 KiB, power of two)
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

    // One device tick drives the pump; the FILL applies immediately.
    machine.advance_devices(1);

    // The fill landed in VRAM (read back through the LFB).
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8 + 1), 0xab); // (1,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 8 + 2), 0xab); // (2,2)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0x00); // (0,0) untouched
    // The ring drained: GET reached PUT.
    assert_eq!(read_mmio_reg(&mut machine, 0x90), put);
}

#[test]
fn pusher_does_not_spin_on_a_malformed_ring() {
    let mut machine = test_machine();
    // A non-power-of-two size with a PUT that the (get + 4) % size orbit never
    // reaches, over zeroed RAM (every header decodes to method 0, count 0, so no
    // COMMAND ever sets busy_ns). Without the word budget this would spin forever.
    write_mmio_reg(&mut machine, 0x84, 0x0001_0000); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 10); // PUSH_SIZE: not a multiple of 4
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, 1); // PUSH_PUT = 1 (never on the orbit)

    // Must return rather than hang. GET stays within the ring.
    machine.advance_devices(1);
    assert!(read_mmio_reg(&mut machine, 0x90) < 10);
}

#[test]
fn pusher_get_trails_put_until_commands_complete() {
    let mut machine = test_machine();
    // Two single-pixel FILLs in the ring. Common setup (DST_BASE, DST_PITCH,
    // DEPTH, ROP) first, then per-fill DST_XY, DIM, FG_COLOR, COMMAND: 0xAA at
    // (1,1) and 0xBB at (3,3). Header words are (count << 16) | method.
    let ring_base = 0x0001_0000u32;
    let ring: [u32; 23] = [
        // Common setup: 7 words.
        (2 << 16) | 0x0100,
        0, // DST_BASE = 0
        8, // DST_PITCH = 8
        (1 << 16) | 0x0110,
        1, // DEPTH = 1
        (1 << 16) | 0x0128,
        0xf0, // ROP = PATCOPY
        // Fill 1: 8 words (cumulative 15 words = 60 bytes after this).
        (1 << 16) | 0x0114,
        (1 << 16) | 1, // DST_XY: y=1, x=1
        (1 << 16) | 0x011c,
        (1 << 16) | 1, // DIM: h=1, w=1
        (1 << 16) | 0x0120,
        0xaa, // FG_COLOR = 0xAA
        (1 << 16) | 0x0150,
        0x01, // COMMAND = FILL
        // Fill 2: 8 words (cumulative 23 words = 92 bytes = PUT).
        (1 << 16) | 0x0114,
        (3 << 16) | 3, // DST_XY: y=3, x=3
        (1 << 16) | 0x011c,
        (1 << 16) | 1, // DIM: h=1, w=1
        (1 << 16) | 0x0120,
        0xbb, // FG_COLOR = 0xBB
        (1 << 16) | 0x0150,
        0x01, // COMMAND = FILL
    ];
    for (i, word) in ring.iter().enumerate() {
        for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
        }
    }
    let put = (ring.len() * 4) as u32; // 92
    let after_fill1 = 15 * 4u32; // 60: offset just past fill 1's COMMAND packet

    write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

    // One tick: the pump consumes the setup plus fill 1, which sets busy_ns and
    // stalls the pump. GET trails PUT, fill 1 landed, fill 2 has not run yet.
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x90), after_fill1); // GET lags PUT
    assert_ne!(read_mmio_reg(&mut machine, 0x90), put);
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8 + 1), 0xaa); // (1,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3 * 8 + 3), 0x00); // (3,3) not yet

    // Enough ticks to drain fill 1's busy_ns (a 1-pixel fill is 105 ns; 10
    // clocks at 22 MHz = ~454 ns), letting the pump consume fill 2.
    machine.advance_devices(10);
    assert_eq!(read_mmio_reg(&mut machine, 0x90), put); // GET caught up
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3 * 8 + 3), 0xbb); // (3,3) now
}

#[test]
fn pusher_streams_color_expand_data_through_the_ring() {
    let mut machine = test_machine();
    // The pusher arms COLOR_EXPAND_DATA and then streams its MONO_DATA words from
    // the ring. This works only because the pump gates on busy_ns (arming leaves
    // busy_ns at 0, so the pump keeps feeding the stream) rather than STATUS.BUSY.
    // An 8x2 glyph at (0,0), depth 1, pitch 8, FG 0xAB, BG 0x00, ROP SRCCOPY: row
    // 0 bits 0xA0 (x=0,2 set), row 1 bits 0x50 (x=1,3 set); MONO_DATA is MSB-first
    // in the high byte. Each MONO_DATA word is its own packet (the port is a single
    // register at 0x0160, so a count>1 run would scatter to 0x0164 and beyond).
    let ring_base = 0x0001_0000u32;
    let ring: [u32; 22] = [
        (2 << 16) | 0x0100,
        0, // DST_BASE = 0
        8, // DST_PITCH = 8
        (1 << 16) | 0x0110,
        1, // DEPTH = 1
        (1 << 16) | 0x0114,
        0, // DST_XY = (0, 0)
        (1 << 16) | 0x011c,
        (2 << 16) | 8, // DIM: h=2, w=8
        (2 << 16) | 0x0120,
        0xab, // FG_COLOR
        0x00, // BG_COLOR
        (1 << 16) | 0x0128,
        0xcc, // ROP = SRCCOPY (S = expanded pixel)
        (1 << 16) | 0x0130,
        0, // FLAGS = 0 (clear bits painted with BG)
        (1 << 16) | 0x0150,
        0x03, // COMMAND = COLOR_EXPAND_DATA (arms the stream; no busy_ns yet)
        (1 << 16) | 0x0160,
        0xa000_0000, // MONO_DATA row 0: bits 0xA0 in the high byte
        (1 << 16) | 0x0160,
        0x5000_0000, // MONO_DATA row 1: bits 0x50 in the high byte
    ];
    for (i, word) in ring.iter().enumerate() {
        for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
        }
    }
    let put = (ring.len() * 4) as u32; // 88

    write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

    machine.advance_devices(1);

    // Row 0: set bits at x=0,2 -> 0xAB; clear bits -> 0x00 (BG).
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0xab); // (0,0)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 1), 0x00); // (1,0)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2), 0xab); // (2,0)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3), 0x00); // (3,0)
    // Row 1: set bits at x=1,3 -> 0xAB.
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8), 0x00); // (0,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 9), 0xab); // (1,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 10), 0x00); // (2,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 11), 0xab); // (3,1)
    // The whole ring drained.
    assert_eq!(read_mmio_reg(&mut machine, 0x90), put);
}

#[test]
fn mode_x_a0000_writes_route_to_the_planar_datapath() {
    let mut machine = test_machine();
    // Mode 13h then unchained (chain-4 off).
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Map mask = plane 2, full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x04); // plane 2
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000 + 5, 0x9C);
    assert_eq!(machine.video().plane_byte(2, 5), 0x9C);
    // An offset past the old 64000-byte mode-13h cap is reachable in the 64 KB
    // unchained planar window.
    machine.write_physical_u8(0x000A_0000 + 0xFB00, 0x3C);
    assert_eq!(machine.video().plane_byte(2, 0xFB00), 0x3C);
    // Read back through the bus read path: select plane 2 as the read-map source,
    // then the A0000 reads return the bytes written above (proving cpu_read routes
    // through the 64 KB window too, including past the old 64000-byte cap).
    machine.video_mut().write_port(0x3CE, 0x04); // GC Read Map Select
    machine.video_mut().write_port(0x3CF, 0x02); // plane 2
    assert_eq!(machine.read_physical_u8(0x000A_0000 + 5), 0x9C);
    assert_eq!(machine.read_physical_u8(0x000A_0000 + 0xFB00), 0x3C);
}

#[test]
fn gc06_moved_aperture_routes_graphics_access_to_the_vga() {
    // Mode 13h programs GC06 to the standard 64 KB A0000 graphics window, so
    // an A0000 write lands in the chain-4 plane (offset 6 -> plane 2,
    // plane-offset 1). Then move the aperture to the 32 KB B8000 window and
    // confirm a B8000 access now routes to the VGA, while the default A0000
    // path stays exactly as it was.
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);

    // Default aperture: A0000 access routes to the chain-4 datapath unchanged.
    machine.write_physical_u8(0x000A_0000 + 6, 0xA5);
    assert_eq!(
        machine.video().plane_byte(2, 1),
        0xA5,
        "default A0000 window still routes to the VGA"
    );

    // Move the aperture to B8000 (GC06 memory map select = 0b11, a 32 KB
    // window): write index 06h then value 0b1100.
    machine.video_mut().write_port(0x3CE, 0x06);
    machine.video_mut().write_port(0x3CF, 0b1100);
    let ap = machine.video().gfx_aperture();
    assert_eq!((ap.base, ap.length), (0x000B_8000, 0x0000_8000));

    // A B8000 access in the moved window routes to the VGA chain-4 datapath.
    // Offset 10 -> plane 10 & 3 = 2, plane-offset 10 >> 2 = 2.
    machine.write_physical_u8(0x000B_8000 + 10, 0x7E);
    assert_eq!(
        machine.video().plane_byte(2, 2),
        0x7E,
        "the moved B8000 window routes to the VGA, not the text buffer"
    );
    // Read-back through the moved window returns the byte from the plane.
    assert_eq!(machine.read_physical_u8(0x000B_8000 + 10), 0x7E);
}

#[test]
fn gc06_map_select_00_routes_the_128kb_graphics_aperture() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3CE, 0x06);
    machine.video_mut().write_port(0x3CF, 0x01); // graphics, A0000-BFFFF

    machine.write_physical_u8(VGA_TEXT_BASE + 10, 0x6D);

    let mirrored_offset = 0x8000 + 10;
    assert_eq!(
        machine
            .video()
            .plane_byte(mirrored_offset & 3, mirrored_offset >> 2),
        0x6D,
        "B8000 in map-select 00 routes through the mirrored VGA graphics window"
    );
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 10), 0x6D);
    assert_eq!(
        machine.read_physical_u8(VGA_MODE13H_BASE + mirrored_offset as u32),
        0x6D,
        "the second 64 KB host half mirrors the same plane window"
    );
}

#[test]
fn gc06_default_aperture_keeps_text_routing_at_b8000() {
    // In text mode the B8000 window is the character buffer regardless of GC06;
    // the moved-aperture routing only applies to graphics modes. Writing a
    // char/attr pair at B8000 must reach the text buffer, not a VGA plane.
    let mut machine = test_machine();
    machine.write_physical_u8(VGA_TEXT_BASE, b'Z');
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'Z');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 1), 0x0F);
}

#[test]
fn mode_x_320x240_through_the_machine() {
    let mut machine = test_machine();
    // Mode 13h, then unchained mode X.
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Abrash's 320x240 vertical timing through the CRTC ports.
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
        machine.video_mut().write_port(0x3D4, idx);
        machine.video_mut().write_port(0x3D5, val);
    }
    // Draw a pixel at column 6: plane 6 & 3 = 2, plane offset 6 >> 2 = 1.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x04); // map mask = plane 2
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000 + 1, 0xC2); // plane 2, offset 1; bits 6-7 set prove no 6-bit mask
    // Complete a frame (mode-X 320x240 frame is ~421 600 dots; 500 000 clocks is
    // ~503 500 dots, enough to cross one frame and present).
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 527, "320x240 vertical total");
    // Column 6 of row 0 scans out the drawn 0xC2, as the 8-bit DAC index directly.
    assert_eq!(
        raster.pixels[6], 0xC2,
        "mode-X pixel scans out at its column with its full 8-bit value"
    );
}

#[test]
fn mode_x_line_compare_split_through_the_machine() {
    let mut machine = test_machine();
    // Mode 13h, then unchained mode X.
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Abrash's 320x240 vertical timing through the CRTC ports (Black Book Listing
    // 47.1): double-scanned, 240 source rows over 480 scanlines.
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
        machine.video_mut().write_port(0x3D4, idx);
        machine.video_mut().write_port(0x3D5, val);
    }
    // Program a split at scan-counter line 200. The 320x240 bang sets 07h bit 4
    // (line-compare bit 8) and 09h bit 6 (line-compare bit 9); rewrite both with
    // their other overflow / max-scan bits intact but those two line-compare bits
    // clear, then the low byte. The kept bits reproduce vtotal 527, vdisp_end 480
    // and keep double-scan on; only line-compare bits 8 and 9 are forced to 0.
    machine.video_mut().write_port(0x3D4, 0x07);
    machine.video_mut().write_port(0x3D5, 0x2E); // overflow minus line-compare bit 8
    machine.video_mut().write_port(0x3D4, 0x09);
    machine.video_mut().write_port(0x3D5, 0x01); // max scan 1 (double-scan), bit 6 clear
    machine.video_mut().write_port(0x3D4, 0x18);
    machine.video_mut().write_port(0x3D5, 0xC8); // line compare low 8 = 200
    // Mark the status panel: plane 0, offset 0 (pixel 0 of any scanline reading
    // offset 0). 0xC2 has bits above 0x3F set, proving the 8-bit DAC index is read
    // directly with no attribute 6-bit mask.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000, 0xC2);
    // Scroll the top region to cleared VRAM, buffered until the next vertical
    // retrace. Two frame periods: the first latches the start address, the second
    // renders with it (the vretrace latch is exercised the same way as the 16-color
    // split test).
    machine.video_mut().write_port(0x3D4, 0x0C);
    machine.video_mut().write_port(0x3D5, 0x40); // start address high = 0x40
    machine.video_mut().write_port(0x3D4, 0x0D);
    machine.video_mut().write_port(0x3D5, 0x00); // start address low = 0x00 -> 0x4000
    machine.advance_devices(500_000);
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320, "mode-X width");
    let w = raster.width as usize;
    // A top scanline (50 < 200) reads the scrolled, cleared region: 0.
    assert_eq!(
        raster.pixels[50 * w],
        0,
        "top region is scrolled to cleared VRAM"
    );
    // The first split scanline (201 = line_compare + 1) reads offset 0 (the marked
    // status panel), as the full 8-bit DAC index.
    assert_eq!(
        raster.pixels[201 * w],
        0xC2,
        "split region reads offset 0 at the full 8-bit value"
    );
}

#[test]
fn mode_x_pel_pan_smooth_scroll_through_the_machine() {
    let mut machine = test_machine();
    // Mode 13h, then unchained mode X.
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Abrash's 320x240 vertical timing through the CRTC ports (Black Book
    // Listing 47.1): double-scanned, 240 source rows over 480 scanlines.
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
        machine.video_mut().write_port(0x3D4, idx);
        machine.video_mut().write_port(0x3D5, val);
    }
    // Distinct bytes per plane at plane offset 0 (values above 0x3F prove the
    // 8-bit-direct DAC index is scanned out, not masked to 6 bits).
    let plane_byte: [u8; 4] = [0x40, 0x50, 0x60, 0x70];
    for (plane, &val) in plane_byte.iter().enumerate() {
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 1u8 << plane); // map mask = this plane
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF, write mode 0
        machine.write_physical_u8(0x000A_0000, val);
    }
    // For each pel-pan 1..3, reset the attribute flip-flop, write AC index 0x13
    // then the pan value, run two frame periods, and assert the leftmost column
    // scans out plane `pan` at plane offset 0: the fine-shifted pixel, not plane 0.
    for pan in 1u8..=3 {
        machine.video_mut().read_status1(); // reset attr flip-flop to index mode
        machine.video_mut().write_port(0x3C0, 0x33); // attr index 0x13, PAS on
        machine.video_mut().write_port(0x3C0, pan); // pel-pan value
        // Pel-pan is live (not latched): it takes effect at the scanline of the
        // write, so the in-progress frame's early rows still hold the prior pan.
        // Two frame periods flush that frame and then render a clean one whose row
        // zero is scanned after the write.
        machine.advance_devices(500_000); // flush the in-progress (mixed-pan) frame
        machine.advance_devices(500_000); // render a full frame with the new pan
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(
            raster.pixels[0], plane_byte[pan as usize],
            "pel-pan {pan} scans out plane {pan} at the leftmost column"
        );
    }
}

#[test]
fn mode13h_320x200_through_the_machine() {
    let mut machine = test_machine();
    // INT 10h AH=00h AL=13h installs chained mode 13h; set_mode13h is its
    // programmatic equivalent (the INT path is proven by
    // int10_mode13h_routes_a000_through_chain4).
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Chain-4 routes the A0000 byte at offset 6 to plane 6 & 3 = 2 at plane
    // offset 6 >> 2 = 1. 0xC2 has bits above 0x3F, proving no 6-bit mask.
    machine.write_physical_u8(0x000A_0000 + 6, 0xC2);
    // Complete a frame (the standard mode-13h frame is ~359 200 dots; 500 000
    // clocks is ~503 500 dots, enough to cross one frame and present).
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 449, "mode-13h vertical total");
    // Column 6 of row 0 scans out the written 0xC2, as the 8-bit DAC index
    // directly.
    assert_eq!(
        raster.pixels[6], 0xC2,
        "mode-13h pixel scans out at its column with its full 8-bit value"
    );
}

#[test]
fn mode13h_pel_pan_smooth_scroll_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Chain-4 writes the byte at A0000 offset p straight to plane p at plane
    // offset 0, so four writes at offsets 0..3 mark one distinct byte per plane
    // there (values above 0x3F prove the 8-bit-direct DAC index is scanned out,
    // not masked to 6 bits).
    let plane_byte: [u8; 4] = [0x40, 0x50, 0x60, 0x70];
    for (plane, &val) in plane_byte.iter().enumerate() {
        machine.write_physical_u8(0x000A_0000 + plane as u32, val);
    }
    // For each pel-pan 1..3, reset the attribute flip-flop, write AC index 0x13
    // then the pan value, run two frame periods, and assert the leftmost column
    // scans out plane `pan` at plane offset 0: the fine-shifted pixel.
    for pan in 1u8..=3 {
        machine.video_mut().read_status1(); // reset attr flip-flop to index mode
        machine.video_mut().write_port(0x3C0, 0x33); // attr index 0x13, PAS on
        machine.video_mut().write_port(0x3C0, pan); // pel-pan value
        // Pel-pan is live (not latched): it takes effect at the scanline of the
        // write, so the in-progress frame's early rows still hold the prior pan.
        // Two frame periods flush that frame and then render a clean one whose row
        // zero is scanned after the write.
        machine.advance_devices(500_000); // flush the in-progress (mixed-pan) frame
        machine.advance_devices(500_000); // render a full frame with the new pan
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(
            raster.pixels[0], plane_byte[pan as usize],
            "pel-pan {pan} scans out plane {pan} at the leftmost column"
        );
    }
}

#[test]
fn mode13h_line_compare_split_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // A split at scan-counter line 200, well inside the 400 active scanlines.
    // Preserve the other vertical-timing bits in 07h/09h while clearing the
    // line-compare high bits; those registers are live timing on VGA hardware.
    machine.video_mut().write_port(0x3D4, 0x07);
    let r07 = machine.video_mut().read_port(0x3D5).unwrap_or(0);
    machine.video_mut().write_port(0x3D5, r07 & !0x10); // clear line-compare bit 8
    machine.video_mut().write_port(0x3D4, 0x09);
    let r09 = machine.video_mut().read_port(0x3D5).unwrap_or(0);
    machine.video_mut().write_port(0x3D5, r09 & !0x40); // clear line-compare bit 9
    machine.video_mut().write_port(0x3D4, 0x18);
    machine.video_mut().write_port(0x3D5, 200); // line compare low byte = 200
    // Mark plane 0, offset 0 (pixel 0 of any scanline reading offset 0). 0xC2
    // has bits above 0x3F, proving the 8-bit DAC index is read directly.
    machine.write_physical_u8(0x000A_0000, 0xC2); // chain-4: plane 0, offset 0
    // Scroll the top region to cleared VRAM, buffered until the next vertical
    // retrace. Two frame periods: the first latches the start address, the second
    // renders with it.
    machine.video_mut().write_port(0x3D4, 0x0C);
    machine.video_mut().write_port(0x3D5, 0x40); // start address high = 0x40
    machine.video_mut().write_port(0x3D4, 0x0D);
    machine.video_mut().write_port(0x3D5, 0x00); // start address low -> 0x4000
    machine.advance_devices(500_000);
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320, "mode-13h width");
    let w = raster.width as usize;
    // A top scanline (50 < 200) reads the scrolled, cleared region: 0.
    assert_eq!(
        raster.pixels[50 * w],
        0,
        "top region is scrolled to cleared VRAM"
    );
    // The first split scanline (201 = line_compare + 1) reads offset 0 (the
    // marked byte), as the full 8-bit DAC index.
    assert_eq!(
        raster.pixels[201 * w],
        0xC2,
        "split region reads offset 0 at the full 8-bit value"
    );
}

#[test]
fn overlay_quantizes_to_16bpp_display_without_dither() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16 (R5G6B5)
    // A uniform gray YUY2 source (Y=130, U=128, V=128 -> yuv_to_argb = 0x858585),
    // 4 pixels (2 packed groups: Y0,U,Y1,V), offscreen at 1 MiB.
    let src = 0x0010_0000u32;
    for g in 0..2u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
    }
    write_mmio_reg(&mut machine, 0x44, src); // OVL_SRC_Y
    write_mmio_reg(&mut machine, 0x48, 8); // OVL_SRC_PITCH
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4); // OVL_SRC_DIM: 4x1
    write_mmio_reg(&mut machine, 0x58, 0); // OVL_DST_XY: (0, 0)
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4); // OVL_DST_DIM: 4x1 (1:1)
    write_mmio_reg(&mut machine, 0x0c, 0); // CONTROL: DITHER_EN off
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // On a 16bpp display the overlay is reduced to R5G6B5 and bit-expanded back:
    // 0x858585 -> 0x848684 (R/B truncate to 0x84, G to 0x86), uniform (no dither).
    for (x, &pixel) in argb.iter().enumerate().take(4) {
        assert_eq!(pixel, 0x0084_8684, "pixel {x}");
    }
}

#[test]
fn overlay_orders_dither_on_a_16bpp_display() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16
    let src = 0x0010_0000u32;
    for g in 0..2u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130);
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128);
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130);
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128);
    }
    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 8);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x58, 0);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Row 0 Bayer cells are 0, 8, 2, 10. For gray 0x858585 the R/B (5-bit) jump
    // a step where the cell offset pushes 133 past the 17th code: cells 8 and 10
    // dither up to 0x8C, cells 0 and 2 stay at 0x84. G (6-bit) stays 0x86.
    assert_eq!(argb[0], 0x0084_8684); // cell 0
    assert_eq!(argb[1], 0x008c_868c); // cell 8
    assert_eq!(argb[2], 0x0084_8684); // cell 2
    assert_eq!(argb[3], 0x008c_868c); // cell 10
}

#[test]
fn overlay_dithers_on_a_15bpp_display() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x110); // 640x480x15 (X1R5G5B5): all channels 5-bit
    let src = 0x0010_0000u32;
    for g in 0..2u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
    }
    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 8);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x58, 0);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // 15bpp makes G 5-bit too (unlike 16bpp's 6-bit G), so a dithered-up pixel is
    // gray 0x8C8C8C, not 0x8C868C. Row 0 cells 0, 8, 2, 10 -> 0x84, 0x8C, 0x84, 0x8C.
    assert_eq!(argb[0], 0x0084_8484); // cell 0: truncated gray
    assert_eq!(argb[1], 0x008c_8c8c); // cell 8: dithered up
    assert_eq!(argb[2], 0x0084_8484); // cell 2
    assert_eq!(argb[3], 0x008c_8c8c); // cell 10
}

#[test]
fn overlay_dither_is_locked_to_screen_position() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16
    // Uniform gray YUY2 source, 4x4 (4 rows x 2 packed groups = 8 groups), offscreen.
    let src = 0x0010_0000u32;
    for g in 0..8u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
    }
    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 8); // src pitch: 2 groups per row
    write_mmio_reg(&mut machine, 0x4c, (4 << 16) | 4); // OVL_SRC_DIM: 4x4
    write_mmio_reg(&mut machine, 0x58, (2 << 16) | 1); // OVL_DST_XY: x=1, y=2 (non-aligned)
    write_mmio_reg(&mut machine, 0x5c, (4 << 16) | 4); // OVL_DST_DIM: 4x4 (1:1)
    write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // The dither cell is BAYER[screen_y & 3][screen_x & 3] in ABSOLUTE screen
    // coordinates, not destination-relative. If it were dst-relative, screen (1,2)
    // would be cell 0 (0x848684); screen-locked it is BAYER[2][1] = 11.
    assert_eq!(argb[2 * 640 + 1], 0x008c_868c); // screen (1,2): cell 11
    assert_eq!(argb[2 * 640 + 4], 0x0084_8684); // screen (4,2): cell 3
    assert_eq!(argb[5 * 640 + 2], 0x008c_8a8c); // screen (2,5): cell 14
}
