// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn vbe_set_mode_selects_a_margo_mode() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (LFB)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert_eq!(machine.margo().display().width, 640);
    assert_eq!(machine.margo().display().height, 480);
}

#[test]
fn vbe_set_mode_then_vga_mode_follows_the_display() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
        0xcd, 0x10, // int 10h
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The VGA mode-set hands the display back to VGA, but the 4F02 call must
    // still have set the Margo mode (width stays set; only margo_active clears).
    assert_eq!(machine.margo().display().width, 640);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
}

#[test]
fn vbe_set_mode_accepts_hi_color_modes() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x11, 0x41, // mov bx, 0111h | 4000h (640x480x16, linear frame buffer)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert_eq!(machine.margo().display().bpp, 16);
}

#[test]
fn vbe_current_mode_returns_the_set_mode() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
        0xcd, 0x10, // int 10h
        0xb8, 0x03, 0x4f, // mov ax, 4F03h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.cpu().registers.ebx() as u16, 0x0101);
}

#[test]
fn vbe_mode_info_fills_the_block() {
    // ES = 0x4000 -> physical 0x40000, DI = 0.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x01, 0x01, // mov cx, 0101h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(read_u16(&mut machine, base + 0x10), 640); // BytesPerScanLine
    assert_eq!(read_u16(&mut machine, base + 0x12), 640); // XResolution
    assert_eq!(read_u16(&mut machine, base + 0x14), 480); // YResolution
    assert_eq!(machine.read_physical_u8(base + 0x19), 8); // BitsPerPixel
    assert_eq!(read_u32(&mut machine, base + 0x28), MARGO_LFB_BASE); // PhysBasePtr
}

#[test]
fn vbe_controller_info_fills_the_block() {
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x00, 0x4f, // mov ax, 4F00h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(machine.read_physical_u8(base), b'V');
    assert_eq!(machine.read_physical_u8(base + 1), b'E');
    assert_eq!(machine.read_physical_u8(base + 2), b'S');
    assert_eq!(machine.read_physical_u8(base + 3), b'A');
    assert_eq!(read_u16(&mut machine, base + 0x04), 0x0200); // VbeVersion
    assert_eq!(read_u16(&mut machine, base + 0x12), 64); // TotalMemory (64 KB units)
    // OemStringPtr and Capabilities are intentionally left zero.
    assert_eq!(read_u32(&mut machine, base + 0x06), 0); // OemStringPtr
    assert_eq!(read_u32(&mut machine, base + 0x0a), 0); // Capabilities

    // VideoModePtr (seg:off) must point at the mode list, which lists every
    // entry in MARGO_VBE_MODES (8bpp then hi-color then true-color) and ends
    // with the 0xffff terminator.
    let ptr = read_u32(&mut machine, base + 0x0e);
    let list = (((ptr >> 16) & 0xffff) << 4) + (ptr & 0xffff);
    let expected = [
        0x0100, 0x0101, 0x0150, 0x0103, 0x0105, 0x0110, 0x0111, 0x0113, 0x0114, 0x0116, 0x0117,
        0x014a, 0x014c, 0x014e, 0xffff,
    ];
    for (i, &mode) in expected.iter().enumerate() {
        assert_eq!(read_u16(&mut machine, list + (i * 2) as u32), mode);
    }
}

#[test]
fn vbe_mode_info_rejects_unknown_modes() {
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x12, 0x01, // mov cx, 0112h (640x480x24, packed 24-bit not provided)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x014f);
}

#[test]
fn copy_through_the_mmio_aperture_moves_vram_and_times_busy() {
    let mut machine = test_machine();
    // Seed a 2x2 source rectangle at (0, 0), pitch 640, depth 1, through the LFB.
    machine.write_physical_u8(MARGO_LFB_BASE, 0xa1); // (0,0)
    machine.write_physical_u8(MARGO_LFB_BASE + 1, 0xa2); // (1,0)
    machine.write_physical_u8(MARGO_LFB_BASE + 640, 0xa3); // (0,1)
    machine.write_physical_u8(MARGO_LFB_BASE + 641, 0xa4); // (1,1)

    // Copy it to (10, 10) on the same surface (no overlap).
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x108, 0); // SRC_BASE
    write_mmio_reg(&mut machine, 0x10c, 640); // SRC_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, (10 << 16) | 10); // DST_XY: y=10, x=10
    write_mmio_reg(&mut machine, 0x118, 0); // SRC_XY: (0,0)
    write_mmio_reg(&mut machine, 0x11c, (2 << 16) | 2); // DIM: h=2, w=2
    write_mmio_reg(&mut machine, 0x128, 0xcc); // ROP: SRCCOPY
    write_mmio_reg(&mut machine, 0x130, 0); // FLAGS: none
    write_mmio_reg(&mut machine, 0x150, 0x02); // COMMAND: COPY

    // Destination corners hold the source bytes (read back through the LFB).
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 10 * 640 + 10),
        0xa1
    );
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 11 * 640 + 11),
        0xa4
    );
    // BUSY is set right after the command.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // 4 pixels -> busy_ns = 100 + 4*10 = 140 ns. At 22 MHz (45.4545 ns/clock),
    // three clocks (136 ns drained) leave it busy; the fourth clears it.
    machine.advance_devices(3);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn dos_com_prints_string_and_exits() {
    // org 0x100: mov ah,9; mov dx,0x010c; int 21; mov ax,4c00; int 21; db 'Hi$'
    let com: &[u8] = &[
        0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i', b'$',
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"Hi");
}

#[test]
fn dos_com_exit_code_is_carried_through() {
    // org 0x100: mov ax,4c07; int 21
    let com: &[u8] = &[0xb8, 0x07, 0x4c, 0xcd, 0x21];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 7 });
    assert!(machine.program_output().is_empty());
}

#[test]
fn fill_through_the_mmio_aperture_writes_vram_and_times_busy() {
    let mut machine = test_machine();
    // Latch a 5x4 fill at (3, 2), pitch 640, depth 1, color 0xAB, solid.
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, (2 << 16) | 3); // DST_XY: y=2, x=3
    write_mmio_reg(&mut machine, 0x11c, (4 << 16) | 5); // DIM: h=4, w=5
    write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

    // VRAM filled (read the top-left filled pixel back through the LFB).
    let pixel = MARGO_LFB_BASE + 2 * 640 + 3;
    assert_eq!(machine.read_physical_u8(pixel), 0xab);
    // BUSY is set right after the command.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // 20 pixels -> busy_ns = 100 + 20*5 = 200 ns. At 22 MHz (45.4545 ns/clock),
    // four clocks (181 ns drained) leave it busy; the fifth clears it.
    machine.advance_devices(4);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn dos_com_runs_the_committed_hello_fixture() {
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::HELLO_COM,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"Hello, world!\r\n");
}

#[test]
fn dos_exe_runs_with_relocation_applied() {
    // The committed .EXE loads DS from a relocated segment reference, then
    // prints via AH=09h. Correct output is only possible if load_exe applied
    // the relocation (otherwise DS is the link-time base and the bytes
    // diverge), so this doubles as the end-to-end relocation check.
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::EXEHELLO_EXE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(
        machine.program_output(),
        b"Hello from a relocated .EXE!\r\n"
    );
}

#[test]
fn dos_com_ah06_zf_reaches_the_guest() {
    // org 0x100: AH=06h DL=0xFF; INT 21h; JZ empty; echo AL via AH=02h; else '!'
    // Proves ZF returned by AH=06h survives the IRET (it is written to the pushed
    // FLAGS image, not just live eflags which the IRET would discard).
    let com: &[u8] = &[
        0xb4, 0x06, 0xb2, 0xff, 0xcd, 0x21, 0x74, 0x08, 0x88, 0xc2, 0xb4, 0x02, 0xcd, 0x21, 0xeb,
        0x06, 0xb2, 0x21, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];

    let mut available =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    available.set_program_stdin(b"X");
    assert_eq!(
        available.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(available.program_output(), b"X"); // char path taken, AL echoed

    let mut empty =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    assert_eq!(
        empty.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(empty.program_output(), b"!"); // empty path taken (ZF=1)
}

#[test]
fn dos_com_echoes_input() {
    // org 0x100: AH=01h; INT 21h (x2, each echoes); AH=4Ch exit
    let com: &[u8] = &[
        0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    machine.set_program_stdin(b"hi");
    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.program_output(), b"hi");
}

#[test]
fn color_expand_data_through_the_mmio_aperture_draws_a_glyph_and_times_busy() {
    let mut machine = test_machine();
    // draw_glyph_8x8: an 8x8 glyph expanded at (10, 5), pitch 640, depth 1,
    // FG 0xAB, EXPAND_TRANSPARENT so clear bits leave the zeroed background.
    // Row 0 = 0x80 (only the leftmost pixel), row 1 = 0x01 (only the rightmost),
    // proving MSB-first ordering; the rest are blank.
    let glyph: [u8; 8] = [0x80, 0x01, 0, 0, 0, 0, 0, 0];

    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, (5 << 16) | 10); // DST_XY: y=5, x=10
    write_mmio_reg(&mut machine, 0x11c, (8 << 16) | 8); // DIM: 8x8
    write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(&mut machine, 0x130, 0x04); // FLAGS: EXPAND_TRANSPARENT
    write_mmio_reg(&mut machine, 0x128, 0xcc); // ROP: SRCCOPY (S = expanded pixel)
    write_mmio_reg(&mut machine, 0x150, 0x03); // COMMAND: COLOR_EXPAND_DATA

    // Armed: BUSY set before any data, nothing drawn yet.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 10),
        0x00
    );

    // Stream the eight rows; the bits go in the high byte, MSB first.
    for (row, &bits) in glyph.iter().enumerate() {
        write_mmio_reg(&mut machine, 0x160, u32::from(bits) << 24); // MONO_DATA
        if row < 7 {
            // Still armed until the final word arrives.
            assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        }
    }

    // Set bits painted FG; clear bits left untouched over the zeroed background.
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 10),
        0xab
    ); // row 0, col 0
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 6 * 640 + 17),
        0xab
    ); // row 1, col 7
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 11),
        0x00
    ); // row 0, col 1 clear
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 6 * 640 + 10),
        0x00
    ); // row 1, col 0 clear

    // 2 pixels written -> busy_ns = 100 + 2*5 = 110 ns. At 22 MHz (45.4545 ns/clock),
    // two clocks (90 ns drained) leave it busy; the third clears it.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(2);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn line_through_the_mmio_aperture_draws_and_times_busy() {
    let mut machine = test_machine();
    // draw_line: a horizontal 5-pixel line at y=5 from x=10 to x=14, pitch 640,
    // depth 1, FG 0xAB. ROP 0xF0 (PATCOPY) draws solid; LINE has no source, so
    // the pattern (FG) is the right input, not SRCCOPY.
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x13c, (5 << 16) | 10); // LINE_START: (10,5)
    write_mmio_reg(&mut machine, 0x140, (5 << 16) | 14); // LINE_END: (14,5)
    write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY (solid; LINE has no source)
    write_mmio_reg(&mut machine, 0x150, 0x05); // COMMAND: LINE

    // The five pixels (x=10..14, y=5) are set; the pixel just left is not.
    for x in 10u32..=14 {
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + x), 0xab);
    }
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 9), 0x00);
    // BUSY set right after the command.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // 5 pixels -> busy_ns = 100 + 5*10 = 150 ns. At 22 MHz (45.4545 ns/clock),
    // three clocks (136 ns drained) leave it busy; the fourth clears it.
    machine.advance_devices(3);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn pattern_fill_through_the_mmio_aperture_tiles_and_times_busy() {
    let mut machine = test_machine();
    // Seed an 8x8 tile in offscreen VRAM (offset 0x10000, clear of the
    // destination) through the LFB: cell (r, c) = r*8 + c + 1, depth 1.
    let pat_base = 0x1_0000u32;
    for r in 0..8u32 {
        for c in 0..8u32 {
            machine.write_physical_u8(MARGO_LFB_BASE + pat_base + r * 8 + c, (r * 8 + c + 1) as u8);
        }
    }
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x144, pat_base); // PAT_BASE
    write_mmio_reg(&mut machine, 0x114, (2 << 16) | 3); // DST_XY: (x=3, y=2)
    write_mmio_reg(&mut machine, 0x11c, (4 << 16) | 4); // DIM: 4x4
    write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY (P = pattern, no source)
    write_mmio_reg(&mut machine, 0x150, 0x06); // COMMAND: PATTERN_FILL

    // Absolute-phase tiling: dst (x, y) -> tile[y & 7][x & 7] = (y & 7)*8 + (x & 7) + 1.
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 3), 20); // (3,2) tile[2][3]
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 6), 23); // (6,2) tile[2][6]
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 3), 44); // (3,5) tile[5][3]
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 2), 0); // left of the rect
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1); // BUSY set

    // 16 pixels -> busy_ns = 100 + 16*5 = 180 ns. At 22 MHz (45.4545 ns/clock),
    // three clocks (136 ns drained) leave it busy; the fourth clears it.
    machine.advance_devices(3);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn clipped_xor_fill_through_the_mmio_aperture() {
    let mut machine = test_machine();
    // Seed x=0..3 at y=0 with 0xFF through the LFB.
    for x in 0u32..4 {
        machine.write_physical_u8(MARGO_LFB_BASE + x, 0xff);
    }
    // FILL the 4x1 row with FG 0x0F through ROP 0x5A (PATINVERT: D ^ P), but clip
    // to x in [0, 3): x=0,1,2 are XORed, x=3 is left alone.
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, 0); // DST_XY: (0,0)
    write_mmio_reg(&mut machine, 0x11c, (1 << 16) | 4); // DIM: 4x1
    write_mmio_reg(&mut machine, 0x120, 0x0f); // FG_COLOR
    write_mmio_reg(&mut machine, 0x128, 0x5a); // ROP: PATINVERT
    write_mmio_reg(&mut machine, 0x134, 0); // CLIP_TL: (0,0)
    write_mmio_reg(&mut machine, 0x138, (1 << 16) | 3); // CLIP_BR: (3,1) exclusive
    write_mmio_reg(&mut machine, 0x130, 0x2); // FLAGS: CLIP_EN
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0xf0); // 0xff ^ 0x0f
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 1), 0xf0);
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2), 0xf0);
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3), 0xff); // clipped, untouched
    // 3 pixels written -> busy_ns = 100 + 3*5 = 115 ns. At 40 ns/clock, two clocks
    // (80 ns) leave it busy; the third clears it.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(2);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn vbe_mode_info_reports_hicolor_masks() {
    // ES = 0x4000 -> physical 0x40000, DI = 0, mode 0x0111 (R5G6B5).
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x11, 0x01, // mov cx, 0111h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(read_u16(&mut machine, base + 0x10), 1280); // BytesPerScanLine = 640 * 2
    assert_eq!(machine.read_physical_u8(base + 0x19), 16); // BitsPerPixel
    assert_eq!(machine.read_physical_u8(base + 0x1f), 5); // RedMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x20), 11); // RedFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x21), 6); // GreenMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x22), 5); // GreenFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x23), 5); // BlueMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x24), 0); // BlueFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x25), 0); // RsvdMaskSize (R5G6B5 has none)
}

#[test]
fn vbe_mode_info_reports_15bpp_masks() {
    // Mode 0x0110 (X1R5G5B5): five-bit channels plus a one-bit reserved field.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x10, 0x01, // mov cx, 0110h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(read_u16(&mut machine, base + 0x10), 1280); // BytesPerScanLine = 640 * 2
    assert_eq!(machine.read_physical_u8(base + 0x19), 15); // BitsPerPixel
    assert_eq!(machine.read_physical_u8(base + 0x1f), 5); // RedMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x20), 10); // RedFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x21), 5); // GreenMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x22), 5); // GreenFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x23), 5); // BlueMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x24), 0); // BlueFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x25), 1); // RsvdMaskSize (the X bit)
    assert_eq!(machine.read_physical_u8(base + 0x26), 15); // RsvdFieldPosition
}

#[test]
fn hicolor_scanout_decodes_through_the_lfb_aperture() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16, pitch 1280
    // Red pixel (0xf800) at (3, 2): offset 2*1280 + 3*2 = 2566.
    machine.write_physical_u8(MARGO_LFB_BASE + 2566, 0x00);
    machine.write_physical_u8(MARGO_LFB_BASE + 2567, 0xf8);

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    assert_eq!(argb[2 * 640 + 3], 0x00ff_0000);
}

#[test]
fn hardware_cursor_composites_through_the_apertures() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16 (R5G6B5)
    // Seed the cursor planes offscreen (1 MiB in, past the 16bpp visible surface)
    // through the LFB. FG pixel at cursor (0,0): XOR plane byte 0 bit 0x80, AND clear.
    let addr = 0x10_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + addr + 512, 0x80);
    write_mmio_reg(&mut machine, 0x2c, addr); // CURSOR_ADDR
    write_mmio_reg(&mut machine, 0x30, (5 << 16) | 3); // CURSOR_POS: (x=3, y=5)
    write_mmio_reg(&mut machine, 0x34, 0xf800); // CURSOR_FG = pure red
    write_mmio_reg(&mut machine, 0x38, 0x0000); // CURSOR_BG
    write_mmio_reg(&mut machine, 0x28, 1); // CURSOR_CTRL = ENABLE

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Cursor pixel (0,0) lands at the positioned screen pixel (3, 5), proving the
    // packed CURSOR_POS encoding routes through the aperture.
    assert_eq!(argb[5 * 640 + 3], 0x00ff_0000); // FG decoded as red at (3,5)
    assert_eq!(argb[0], 0x0000_0000); // the origin is outside the cursor: black surface
}
