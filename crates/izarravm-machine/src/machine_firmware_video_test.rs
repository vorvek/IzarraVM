// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn rejects_non_64k_roms() {
    let err = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), [0u8; 8]).unwrap_err();

    assert!(matches!(err, MachineError::InvalidRomSize(8)));
}

#[test]
fn first_instruction_fetch_uses_386_reset_vector() {
    let mut machine = test_machine();
    let reason = machine.run_cycles(32).unwrap();

    assert_ne!(reason, StopReason::Halted);
    assert_eq!(
        machine.bus_trace().cycles()[0].kind,
        BusAccessKind::InstructionPrefetch
    );
    assert_eq!(machine.bus_trace().cycles()[0].address, 0xffff_fff0);
}

#[test]
fn unaligned_dword_splits_into_byte_bus_cycles() {
    let mut machine = test_machine();
    {
        let mut bus = machine.make_bus();
        bus.write_memory(
            0x101,
            BusWidth::Dword,
            0x1234_5678,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }

    let writes = machine
        .bus_trace()
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::DataWrite)
        .count();
    assert_eq!(writes, 4);
}

#[test]
fn test_rom_reaches_deterministic_text_screen() {
    let mut machine = test_machine();
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    let frame = machine.screen_text();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(frame.line_string(0), "RESET VECTOR + BIOS INT10 PASS");
    assert_eq!(frame.line_string(1), "B8000 DIRECT TEXT PASS");
    assert_eq!(frame.line_string(2), "PROTECTED MODE FLAT SEGMENTS PASS");
    assert_eq!(frame.line_string(3), "PAGING + B8000 ALIAS PASS");
    assert_eq!(frame.line_string(4), "RING0 PAGE FAULT HANDLER PASS");
    assert!(
        machine
            .bus_trace()
            .cycles()
            .iter()
            .any(|cycle| cycle.kind == BusAccessKind::PageWalkRead)
    );
    assert!(machine.cpu().is_protected_mode());
    assert!(machine.cpu().is_paging_enabled());
}

#[test]
fn int10_mode13h_routes_a000_through_chain4() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0xa0, // mov ax, a000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x7b, 0x00, // mov di, 007bh
        0xb0, 0x2a, // mov al, 2ah
        0xaa, // stosb
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.set_bus_trace_detailed(true);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    // Chain-4 routes the A0000 byte at offset 0x7B to plane 0x7B & 3 = 3 at
    // plane offset 0x7B >> 2 = 30.
    assert_eq!(machine.video().plane_byte(3, 30), 0x2a);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert!(machine.is_graphics_mode());
    assert!(machine.bus_trace().cycles().iter().any(|cycle| {
        cycle.kind == BusAccessKind::InterruptAcknowledge && cycle.address == 0x10
    }));
}

#[test]
fn unittester_exit_command_stops_with_the_guest_code() {
    // index=REG_EXIT; data=42; command=CMD_EXIT.
    let rom = rom_with_code(&[
        0xB0, 0x0C, 0xE6, 0xE4, // mov al,12; out 0E4h,al  (index = REG_EXIT)
        0xB0, 0x2A, 0xE6, 0xE5, // mov al,42; out 0E5h,al  (exit code 42)
        0xB0, 0x03, 0xE6, 0xE6, // mov al,3;  out 0E6h,al  (CMD_EXIT)
        0xF4, // hlt (not reached)
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::TestExit { code: 42 });
}

#[test]
fn unittester_crc_command_matches_the_rust_helper() {
    // Program a 2x2 rectangle and issue CMD_CRC; the run loop computes it and
    // stores it at REG_CRC, where the guest (here, a bus read) can read it.
    let rom = rom_with_code(&[
        0xB0, 0x00, 0xE6, 0xE4, // index = REG_X (0)
        0xB0, 0x00, 0xE6, 0xE5, // X lo
        0xB0, 0x00, 0xE6, 0xE5, // X hi
        0xB0, 0x00, 0xE6, 0xE5, // Y lo
        0xB0, 0x00, 0xE6, 0xE5, // Y hi
        0xB0, 0x02, 0xE6, 0xE5, // W lo = 2
        0xB0, 0x00, 0xE6, 0xE5, // W hi
        0xB0, 0x02, 0xE6, 0xE5, // H lo = 2
        0xB0, 0x00, 0xE6, 0xE5, // H hi
        0xB0, 0x01, 0xE6, 0xE6, // CMD_CRC
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    let reported = with_bus(&mut machine, |bus| {
        bus.write_io(0xE4, BusWidth::Byte, 8, false).unwrap(); // index = REG_CRC
        let mut crc = [0u8; 4];
        for byte in &mut crc {
            *byte = bus.read_io(0xE5, BusWidth::Byte, 0, false).unwrap() as u8;
        }
        u32::from_le_bytes(crc)
    });
    assert_eq!(reported, machine.screen_crc32(0, 0, 2, 2));
}

#[test]
fn int10_ah0f_reports_mode_after_set() {
    // Set mode 13h, then AH=0Fh returns AL=mode, AH=columns.
    let rom = rom_with_code(&[
        0xB8, 0x13, 0x00, 0xCD, 0x10, // mov ax,0013h; int 10h (set mode 13h)
        0xB4, 0x0F, 0xCD, 0x10, // mov ah,0Fh; int 10h (get mode)
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax & 0xff, 0x13, "AL = current mode");
    assert_eq!(ax >> 8, 40, "AH = column count for mode 13h");
}

#[test]
fn int10_00_returns_vgabios_mode_class_code() {
    let mut m = int15_machine(16);

    for (mode, returned_al) in [
        (0x00u8, 0x30u8),
        (0x04, 0x30),
        (0x06, 0x3F),
        (0x0D, 0x20),
        (0x13, 0x20),
        (0x84, 0x30),
    ] {
        m.cpu.registers.set_eax(u32::from(mode));
        m.handle_int10();

        assert_eq!(m.cpu.registers.eax() as u8, returned_al, "mode {mode:02X}");
    }
    assert_eq!(m.read_physical_u8(0x449), 0x84);
}

#[test]
fn int10_00_tracks_no_clear_in_bda_video_control() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x008D);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x8D);
    assert_eq!(m.read_physical_u8(0x487), 0xE0);

    m.cpu.registers.set_eax(0x0093);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x93);
    assert_eq!(m.read_physical_u8(0x487), 0xE0);

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x0D);
    assert_eq!(m.read_physical_u8(0x487), 0x60);
}

#[test]
fn boot_image_starts_at_bios_loaded_boot_sector() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    machine.set_bus_trace_detailed(true);

    let reason = machine.run_cycles(16).unwrap();

    assert_ne!(reason, StopReason::Halted);
    assert_eq!(
        machine.bus_trace().cycles()[0].address,
        BOOT_SECTOR_ADDRESS as u32
    );
}

#[test]
fn boot_image_emits_serial_records_and_result_block() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();

    // The budget covers the timer test's idle (ten ticks of about 11932 PIT
    // clocks, near 2.5M CPU clocks) plus the setup, matching the headless runner.
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    let serial = machine.serial_text();
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert!(serial.contains("PASS boot.stage2"));
    assert!(serial.contains("PASS video.cga_graphics"));
    assert!(serial.contains("PASS video.ega_planar"));
    assert!(serial.contains("PASS video.vga_mode13h"));
    assert_eq!(
        usize::from(results.declared_record_count),
        results.records.len()
    );
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "video.vga_text"
    }));
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "video.vga_mode13h"
    }));
    for name in ["video.cga_graphics", "video.ega_planar"] {
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass && record.name == name
        }));
    }
    // Chain-4 routes the linear byte at offset N to plane N & 3 at plane
    // offset N >> 2, so the boot image's three drawn pixels land as:
    // 0 -> plane 0 @ 0, 319 -> plane 3 @ 79, 63680 -> plane 0 @ 15920.
    assert_eq!(machine.video().plane_byte(0, 0), 0x2a);
    assert_eq!(machine.video().plane_byte(3, 79), 0x13);
    assert_eq!(machine.video().plane_byte(0, 15920), 0x7f);
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "sound.sb_16bit_dma"
    }));
}

#[test]
fn boot_suite_timer_passes_at_native_200mhz() {
    // The boot suite is wall-time-bound: the timer test waits for ten IRQ0
    // edges and the PIT runs at a fixed rate regardless of the CPU clock. At
    // the 200 MHz native default the cycle budget must scale (clock_hz / 5,
    // about 200 ms) or the timer test never reaches its tick target.
    let profile = MachineProfile {
        cpu: GswMode::Gsw586,
        memory_mib: 16,
        video: VideoCard::Et4000Ax,
        sound_blaster: SoundBlasterConfig::default(),
        wss: WssConfig::default(),
        wait_states: WaitStateProfile::default(),
        address_pipelining: false,
        cache_enabled: false,
    };
    let budget = profile.cpu.clock_rate().clocks_for_fraction_floor(1, 5);
    let mut machine =
        Machine::new_boot_image(profile, izarravm_firmware::X86_BOOT_TEST_IMAGE).unwrap();

    let reason = machine.run_until_halt_or_cycles(budget).unwrap();
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

    assert_eq!(reason, StopReason::Halted);
    let timer = results
        .records
        .iter()
        .find(|record| record.name == "timer.irq0")
        .expect("timer.irq0 record present");
    assert_eq!(
        timer.status,
        izarravm_firmware::SuiteRecordStatus::Pass,
        "timer.irq0 must pass at 200 MHz with the scaled budget"
    );
}

#[test]
fn margo_apertures_route_through_the_bus() {
    let mut machine = test_machine();

    // LFB: write a byte at the aperture base + 5, read it back.
    let lfb = MARGO_LFB_BASE + 5;
    machine.write_physical_u8(lfb, 0x9c);
    assert_eq!(machine.read_physical_u8(lfb), 0x9c);

    // MMIO: the ID register reads the Margo magic.
    let id = u32::from(machine.read_physical_u8(MARGO_MMIO_BASE))
        | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 1)) << 8)
        | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 2)) << 16)
        | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 3)) << 24);
    assert_eq!(id, MARGO_ID_VALUE);
}

#[test]
fn vga_mode_set_clears_a_latched_margo_display() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Host path latches Margo as the active display.
    machine.set_margo_mode_640x480x8();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);

    // A guest VGA mode-set must hand the display back to VGA.
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
}

#[test]
fn int42_relocated_video_handler_uses_int10_service() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x42, // int 42h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.memory.read_u8(0x449).unwrap(), 0x13);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
}

#[test]
fn host_mode_set_selects_margo_lfb() {
    let mut machine = test_machine();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);

    machine.set_margo_mode_640x480x8();

    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert_eq!(machine.margo().display().width, 640);
    assert_eq!(machine.margo().display().height, 480);
}

#[test]
fn int13_read_places_sector_in_memory() {
    // A 720 KB image whose first sector starts with a recognizable marker.
    let mut img = vec![0u8; 737_280];
    img[0] = 0xEB;
    img[1] = 0x55;
    // Stub: ES=0, BX=0x2000, read 1 sector at CHS(0,0,1) of drive 0 via INT 13h,
    // then halt. AX=0x0201 (AH=02 read, AL=01 sector), CX=0x0001 (cyl 0,
    // sector 1), DX=0x0000 (head 0, drive A:). The buffer sits well clear of
    // the IRET stub the BIOS keeps near 0x0600.
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xC0, // mov es, ax
        0xBB, 0x00, 0x20, // mov bx, 0x2000
        0xB8, 0x01, 0x02, // mov ax, 0x0201
        0xB9, 0x01, 0x00, // mov cx, 0x0001
        0xBA, 0x00, 0x00, // mov dx, 0x0000
        0xCD, 0x13, // int 13h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The sector bytes landed at physical 0x2000.
    assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
    assert_eq!(machine.read_physical_u8(0x2001), 0x55);
    // AH cleared, AL reports one sector read, CF clear on success.
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax >> 8, 0x00);
    assert_eq!(ax & 0xff, 0x01);
    let flags = machine.cpu().registers.eflags;
    assert_eq!(flags & 0x0001, 0, "CF must be clear after a good read");
}

#[test]
fn int40_relocated_floppy_handler_uses_disk_service() {
    let mut img = vec![0u8; 737_280];
    img[0] = 0xEB;
    img[1] = 0x40;
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xC0, // mov es, ax
        0xBB, 0x00, 0x20, // mov bx, 0x2000
        0xB8, 0x01, 0x02, // mov ax, 0x0201
        0xB9, 0x01, 0x00, // mov cx, 0x0001
        0xBA, 0x00, 0x00, // mov dx, 0x0000
        0xCD, 0x40, // int 40h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
    assert_eq!(machine.read_physical_u8(0x2001), 0x40);
    assert_eq!(machine.cpu().registers.eflags & 0x0001, 0);
}

#[test]
fn int10_pixel_write_read_round_trips_in_mode13h() {
    let mut m = int15_machine(16);
    m.video_mut().set_mode13h();
    // AH=0Ch write pixel: AL=colour 0x43 (bit7 clear = plain write), CX=col 5,
    // DX=row 2 -> framebuffer offset 2*320+5.
    m.cpu.registers.set_eax(0x0C43);
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    // AH=0Dh read the same pixel back into AL.
    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    assert_eq!(
        m.cpu.registers.eax() as u8,
        0x43,
        "pixel reads back its colour"
    );
    // Mode 13h is a 256-color mode: AL is the full 8-bit colour, bit 7 included,
    // with no XOR. Writing 0x8F stores colour 0x8F (143), not an XOR.
    m.cpu.registers.set_eax(0x0C8F); // colour 0x8F, bit7 part of the value
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    assert_eq!(
        m.cpu.registers.eax() as u8,
        0x8F,
        "high colours write directly, no bit-7 XOR in 256-colour mode"
    );
}

#[test]
fn int10_pixel_write_read_round_trips_in_cga_graphics() {
    let mut m = int15_machine(16);
    m.video_mut().set_cga_mode(0x04);

    // Mode 04h packs four 2-bit pixels per byte. Pixel (2,1) lives in the odd
    // bank at B800:2000 bits 3:2.
    m.cpu.registers.set_eax(0x0C03);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(1);
    m.handle_int10();
    assert_eq!(m.video().cga_read(0x2000), 0x0C);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(1);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 3);

    // In CGA modes AL bit 7 means XOR the low colour bits with the existing
    // pixel, so 3 xor 1 becomes 2.
    m.cpu.registers.set_eax(0x0C81);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 2);

    m.video_mut().set_cga_mode(0x06);
    m.cpu.registers.set_eax(0x0C01);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(0);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 1);
}

#[test]
fn int10_pixel_write_read_round_trips_in_ega_planar() {
    let mut m = int15_machine(16);
    assert!(m.video_mut().set_mode(0x0D));

    m.cpu.registers.set_eax(0x0C0B);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(3);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(9, 3), 0x0B);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(3);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x0B);
    assert_eq!(m.video().render_active_row(6)[9], 0x13);

    m.cpu.registers.set_eax(0x0C82);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x09);
}

#[test]
fn int10_pixel_read_write_uses_ega_graphics_page() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();

    m.cpu.registers.set_eax(0x0C0B);
    m.cpu.registers.set_ebx(0x0100);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(3);
    m.handle_int10();

    assert_eq!(m.video().planar_read_pixel(9, 3), 0x00);
    assert_eq!(m.video().planar_read_pixel_at(0x2000, 9, 3), 0x0B);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x00);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x0B);
}

#[test]
fn ega_graphics_brown_and_bright_colors_render_through_the_dac() {
    // End-to-end guard for the EGA/CGA palette: a guest draws brown (color 6)
    // and the bright eight in an EGA graphics mode, and the composited frame
    // (the same pixels the unit-tester CRC hashes) must show the real RGB, not
    // the 256-color palette3 gray/color ramps that brown and 0x38-0x3F land on.
    // The boot-suite video checks only touch safe colors, so they missed this.
    //
    // Per pixel: AH=0Ch AL=color BH=0 (page) CX=col DX=row, then INT 10h.
    fn draw(code: &mut Vec<u8>, color: u8, col: u16, row: u16) {
        code.extend_from_slice(&[0xB8, color, 0x0C]); // mov ax, 0x0C00 | color
        code.extend_from_slice(&[0xBB, 0x00, 0x00]); // mov bx, 0 (page 0)
        code.push(0xB9);
        code.extend_from_slice(&col.to_le_bytes()); // mov cx, col
        code.push(0xBA);
        code.extend_from_slice(&row.to_le_bytes()); // mov dx, row
        code.extend_from_slice(&[0xCD, 0x10]); // int 0x10
    }

    // Color number -> expected 0x00RRGGBB, the same in both modes: brown, dark
    // gray, bright blue, yellow, bright white, and light gray as a control that
    // was already correct (it never used a remapped DAC entry).
    let samples: [(u8, u32); 6] = [
        (6, 0x00AA_5500),
        (8, 0x0055_5555),
        (9, 0x0055_55FF),
        (14, 0x00FF_FF55),
        (15, 0x00FF_FFFF),
        (7, 0x00AA_AAAA),
    ];

    // Mode 10h (640x350, palette2 via the EGA attribute remap) is 1:1; mode 0Dh
    // (320x200, palette1, the Monkey Island mode) is double-scanned, so source
    // row R lands on output raster rows 2R and 2R+1.
    for (mode, row, scan) in [(0x10u8, 100u16, 1usize), (0x0Du8, 50u16, 2usize)] {
        let mut code = vec![0xB8, mode, 0x00, 0xCD, 0x10]; // mov ax,00<mode>h; int 10h
        for (i, (color, _)) in samples.iter().enumerate() {
            draw(&mut code, *color, 10 + i as u16 * 10, row);
        }
        code.push(0xF4); // hlt

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            rom_with_code(&code),
        )
        .unwrap();
        assert_eq!(
            machine.run_until_halt_or_cycles(5_000_000).unwrap(),
            StopReason::Halted,
            "mode {mode:#04x} guest ran to hlt"
        );
        // Present two whole frames so the final render is a clean full frame of
        // the drawn VRAM (advance only resets the scanline cursor past one frame).
        let dots = machine.video_mut().frame_dots();
        machine.video_mut().advance(dots * 2);

        let (frame, width, _height) = machine.frame_argb();
        let raster_row = row as usize * scan;
        for (i, (color, want)) in samples.iter().enumerate() {
            let col = 10 + i * 10;
            let got = frame[raster_row * width + col];
            assert_eq!(
                got, *want,
                "mode {mode:#04x} color {color}: got {got:#08x}, want {want:#08x}"
            );
        }
    }
}

#[test]
fn int10_mode_set_bit7_preserves_cga_framebuffer() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    m.video_mut().cga_write(0, 0b01_10_11_00);
    assert!(m.video_mut().write_port(0x3D9, 0x31));

    m.cpu.registers.set_eax(0x0084);
    m.handle_int10();

    assert_eq!(m.video().active_mode(), VideoMode::Cga);
    assert_eq!(m.video().cga_read(0), 0b01_10_11_00);
    assert_eq!(m.video().cga_color_select(), 0x00);
    assert_eq!(m.memory.read_u8(0x449).unwrap(), 0x84);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x4000);

    m.cpu.registers.set_eax(0x0F00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x84);

    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    assert_eq!(m.video().cga_read(0), 0);
}

#[test]
fn int10_09_draws_and_xors_font_glyphs_in_cga_graphics() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0003);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 3);
    assert_eq!(m.video().cga_read_pixel(7, 7), 3);

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0081);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 2);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 0);
}

#[test]
fn int10_09_space_erases_cga_graphics_cell() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    for y in 0..8u16 {
        for x in 0..8u16 {
            assert!(m.video_mut().cga_write_pixel(x, y, 3, false));
        }
    }

    m.cpu.registers.set_eax(0x0920);
    m.cpu.registers.set_ebx(0x0002);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(0, 0), 0);
    assert_eq!(m.video().cga_read_pixel(7, 7), 0);

    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0020);
}

#[test]
fn int10_08_recognizes_white_cga_graphics_font_patterns() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0003);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x03DB);

    m.video_mut().set_cga_mode(0x04);
    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0002);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x02DB);

    m.video_mut().set_cga_mode(0x06);
    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0001);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x01DB);
}

#[test]
fn int10_cga_graphics_uses_int1f_font_for_high_chars() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.write_guest_block(0x40000, &[0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01]);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebp(0);
    m.cpu.registers.set_eax(0x1120);
    m.handle_int10();
    assert_eq!(m.read_physical_u16(0x1F * 4), 0);
    assert_eq!(m.read_physical_u16(0x1F * 4 + 2), 0x4000);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
    m.cpu.registers.set_ebp(0xFFFF);
    m.cpu.registers.set_ecx(0);
    m.cpu.registers.set_edx(0);
    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0x4000);
    assert_eq!(m.cpu.registers.ebp() as u16, 0);
    assert_eq!(m.cpu.registers.ecx() as u16, 8);
    assert_eq!(m.cpu.registers.edx() as u8, 24);

    m.cpu.registers.set_eax(0x0980);
    m.cpu.registers.set_ebx(0x0002);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(0, 0), 2);
    assert_eq!(m.video().cga_read_pixel(1, 0), 0);
    assert_eq!(m.video().cga_read_pixel(1, 1), 2);

    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0280);
}

#[test]
fn int10_1130_returns_readable_font_info_pointers() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Es).selector,
        VGA_BIOS_SEGMENT
    );
    assert_eq!(m.cpu.registers.ebp() as u16, VGA_BIOS_FONT_TABLE_OFF);

    m.cpu.registers.set_eax(0x0010);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0600);
    m.cpu.registers.set_ecx(0xBEEF);
    m.cpu.registers.set_edx(0xAB00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 14);
    assert_eq!(m.cpu.registers.edx() as u8, 24);
    let ptr = (u32::from(BIOS_ROM_SEGMENT) << 4) + u32::from(BIOS_FONT_8X16_ROM_OFFSET);
    assert_eq!(
        m.read_physical_u8(ptr + u32::from(b'A') * 16 + 7),
        font::VGAFONT_8X16[usize::from(b'A') * 16 + 7]
    );

    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0200);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebp() as u16, BIOS_FONT_8X14_ROM_OFFSET);
    let ptr = (u32::from(BIOS_ROM_SEGMENT) << 4) + u32::from(BIOS_FONT_8X14_ROM_OFFSET);
    assert_eq!(
        m.read_physical_u8(ptr + u32::from(b'A') * 14 + 6),
        font::VGAFONT_8X14[usize::from(b'A') * 14 + 6]
    );

    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0400);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebp() as u16, BIOS_FONT_8X8_HIGH_ROM_OFFSET);
    let ptr = (u32::from(BIOS_ROM_SEGMENT) << 4) + u32::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET);
    assert_eq!(m.read_physical_u8(ptr), font::VGAFONT_8X8[128 * 8]);
}

#[test]
fn int10_04_reports_cga_light_pen_latch() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    let line_dots = m.video().frame_dots() / u64::from(m.video().raster_height());
    m.video_mut().advance(line_dots * 16 + 80);
    assert_eq!(m.video_mut().read_port(0x3DC), Some(0xFF));

    m.cpu.registers.set_eax(0x0400);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 1);
    assert_eq!(m.cpu.registers.ebx() as u16, 80);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x1000);
    assert_eq!(m.cpu.registers.edx() as u16, 0x020A);

    assert_eq!(m.video_mut().read_port(0x3DB), Some(0xFF));
    m.cpu.registers.set_eax(0x0400);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0);
}

#[test]
fn int10_teletype_draws_and_scrolls_cga_graphics_text() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x0EDB);
    m.cpu.registers.set_ebx(0x0002);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 2);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 1);

    m.video_mut().cga_write_pixel(0, 8, 3, false);
    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(24 << 8);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0E0A);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 3);
    assert_eq!(m.video().cga_read_pixel(0, 192), 0);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 24 << 8);
}

#[test]
fn int10_scroll_window_moves_cga_graphics_pixels_by_character_rows() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    assert!(m.video_mut().cga_write_pixel(8, 16, 2, false)); // window row 2, col 1
    assert!(m.video_mut().cga_write_pixel(0, 16, 1, false)); // outside window
    m.cpu.registers.set_eax(0x0601);
    m.cpu.registers.set_ebx(0x0300);
    m.cpu.registers.set_ecx((1 << 8) | 1);
    m.cpu.registers.set_edx((3 << 8) | 2);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(8, 8), 2);
    assert_eq!(m.video().cga_read_pixel(8, 24), 3);
    assert_eq!(m.video().cga_read_pixel(0, 16), 1);

    m.video_mut().set_cga_mode(0x04);
    assert!(m.video_mut().cga_write_pixel(8, 16, 2, false));
    m.cpu.registers.set_eax(0x0701);
    m.cpu.registers.set_ebx(0x0100);
    m.cpu.registers.set_ecx((1 << 8) | 1);
    m.cpu.registers.set_edx((3 << 8) | 2);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(8, 24), 2);
    assert_eq!(m.video().cga_read_pixel(8, 8), 1);
}

#[test]
fn int10_scroll_window_clear_fills_cga_graphics_window_only() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    assert!(m.video_mut().cga_write_pixel(8, 8, 1, false));
    assert!(m.video_mut().cga_write_pixel(0, 8, 3, false));
    m.cpu.registers.set_eax(0x0600);
    m.cpu.registers.set_ebx(0x0200);
    m.cpu.registers.set_ecx((1 << 8) | 1);
    m.cpu.registers.set_edx((2 << 8) | 2);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(8, 8), 2);
    assert_eq!(m.video().cga_read_pixel(16, 16), 2);
    assert_eq!(m.video().cga_read_pixel(0, 8), 3);
}

#[test]
fn int10_13_draws_attributed_string_in_cga_graphics() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    m.write_guest_block(0x4000, &[0xDB, 0x01, 0xDB, 0x02]);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x4000);
    m.cpu.registers.set_eax(0x1303);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(0);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(0, 0), 1);
    assert_eq!(m.video().cga_read_pixel(8, 0), 2);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 2);
}

#[test]
fn int10_ega_graphics_text_services_draw_visible_planar_glyphs() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0012);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 29);
    assert_eq!(m.read_physical_u8(0x485), 16);

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x000C);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 0), 0x0C);

    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0CDB);

    m.write_guest_block(0x6000, &[0xDB, 0x05]);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x6000);
    m.cpu.registers.set_eax(0x1303);
    m.cpu.registers.set_ecx(1);
    m.cpu.registers.set_edx((1 << 8) | 1);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(8, 16), 0x05);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), (1 << 8) | 2);

    assert!(m.video_mut().planar_write_pixel(0, 16, 3, false));
    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(29 << 8);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0E0A);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 0), 3);
    assert_eq!(m.video().planar_read_pixel(0, 29 * 16), 0);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 29 << 8);
}

#[test]
fn int10_ega_graphics_text_services_use_bh_page() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();

    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_ebx(0x0100);
    m.cpu.registers.set_edx(0);
    m.handle_int10();

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0105);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();

    assert_eq!(m.video().planar_read_pixel(0, 0), 0);
    assert_eq!(m.video().planar_read_pixel_at(0x2000, 0, 0), 5);

    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0020);

    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x05DB);
}

#[test]
fn int10_ega_graphics_font_services_feed_planar_text_output() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0012);
    m.handle_int10();

    let mut font = vec![0u8; 256 * 16];
    font[usize::from(b'A') * 16] = 0x80;
    m.write_guest_block(0x7000, &font);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x7000);
    m.cpu.registers.set_eax(0x1121);
    m.cpu.registers.set_ebx(0x0000);
    m.cpu.registers.set_ecx(16);
    m.cpu.registers.set_edx(30);
    m.handle_int10();

    assert_eq!(m.read_physical_u8(0x484), 29);
    assert_eq!(m.read_physical_u8(0x485), 16);
    m.cpu.registers.set_eax(0x0941);
    m.cpu.registers.set_ebx(0x0007);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 0), 7);
    assert_eq!(m.video().planar_read_pixel(1, 0), 0);

    m.cpu.registers.set_eax(0x1123);
    m.cpu.registers.set_ebx(0x0003);
    m.cpu.registers.set_edx(0);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 42);
    assert_eq!(m.read_physical_u8(0x485), 8);

    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(42 << 8);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0EDB);
    m.cpu.registers.set_ebx(0x0002);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 42 * 8), 2);

    m.cpu.registers.set_eax(0x1122);
    m.cpu.registers.set_ebx(0x0002);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 24);
    assert_eq!(m.read_physical_u8(0x485), 14);

    m.cpu.registers.set_eax(0x1124);
    m.cpu.registers.set_ebx(0x0000);
    m.cpu.registers.set_edx(30);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 29);
    assert_eq!(m.read_physical_u8(0x485), 16);
}

#[test]
fn int10_write_string_places_chars_and_attr_in_text_buffer() {
    let mut m = int15_machine(16);
    m.video_mut().set_text_mode();
    // Place a 3-char string "Hi!" at ES:BP = 0x0000:0x4000 (physical 0x4000).
    m.write_physical_u8(0x4000, b'H');
    m.write_physical_u8(0x4001, b'i');
    m.write_physical_u8(0x4002, b'!');
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x4000);
    // AH=13h AL=01 (advance cursor, no attr bytes), BL=attr 0x1E, CX=3,
    // DH=row 4, DL=col 10.
    m.cpu.registers.set_eax(0x1301);
    m.cpu.registers.set_ebx(0x001E);
    m.cpu.registers.set_ecx(3);
    m.cpu.registers.set_edx((4 << 8) | 10);
    m.handle_int10();
    // The chars and attribute landed at row 4, col 10.. of the text buffer.
    let base = (4 * 80 + 10) * 2;
    assert_eq!(m.video().read_u8(base).unwrap(), b'H');
    assert_eq!(m.video().read_u8(base + 1).unwrap(), 0x1E);
    assert_eq!(m.video().read_u8(base + 2).unwrap(), b'i');
    assert_eq!(m.video().read_u8(base + 4).unwrap(), b'!');
    // AL bit 0 set leaves the BDA cursor at the end of the string (col 13).
    assert_eq!(m.memory.read_u16(0x450).unwrap(), (4 << 8) | 13);
}

#[test]
fn int10_write_string_honors_interleaved_attribute_bytes() {
    let mut m = int15_machine(16);
    m.video_mut().set_text_mode();
    // AL bit 1 set: the source is char,attr,char,attr. "Ab" with attrs 0x12,0x34.
    m.write_physical_u8(0x5000, b'A');
    m.write_physical_u8(0x5001, 0x12);
    m.write_physical_u8(0x5002, b'b');
    m.write_physical_u8(0x5003, 0x34);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x5000);
    m.cpu.registers.set_eax(0x1302); // AL bit1 = interleaved attrs, bit0 clear
    m.cpu.registers.set_ebx(0x0000);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(0); // row 0, col 0
    m.handle_int10();
    assert_eq!(m.video().read_u8(0).unwrap(), b'A');
    assert_eq!(m.video().read_u8(1).unwrap(), 0x12);
    assert_eq!(m.video().read_u8(2).unwrap(), b'b');
    assert_eq!(m.video().read_u8(3).unwrap(), 0x34);
}

#[test]
fn int10_save_restore_state_round_trips_the_bda_block() {
    let mut m = int15_machine(16);
    // AL=00 reports the buffer size in 64-byte blocks (99 bytes -> 2 blocks).
    m.cpu.registers.set_eax(0x1C00);
    m.cpu.registers.set_ecx(0x0002);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 2, "two 64-byte blocks");
    assert_eq!(m.cpu.registers.eax() as u8, 0x1C);
    m.cpu.registers.set_eax(0x1C00);
    m.cpu.registers.set_ecx(0x0007);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 15, "full state block count");
    // Mark the BDA edge bytes, save into ES:BX, change them, then restore.
    let _ = m.memory.write_u8(0x449, 0x12);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER, 0x1234);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, 0xabcd);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_eax(0x1C01); // save
    m.cpu.registers.set_ecx(0x0002);
    m.handle_int10();
    // Corrupt the live BDA, then restore it from the saved buffer.
    let _ = m.memory.write_u8(0x449, 0x99);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER, 0);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, 0);
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_eax(0x1C02); // restore
    m.handle_int10();
    assert_eq!(m.memory.read_u8(0x449).unwrap(), 0x12, "BDA mode restored");
    assert_eq!(
        m.memory.read_u16(BDA_VIDEO_SAVE_POINTER).unwrap(),
        0x1234,
        "video-save pointer offset restored"
    );
    assert_eq!(
        m.memory.read_u16(BDA_VIDEO_SAVE_POINTER + 2).unwrap(),
        0xabcd,
        "video-save pointer segment restored"
    );
}

#[test]
fn int10_save_restore_state_round_trips_hardware_registers() {
    let mut m = int15_machine(16);
    m.video_mut().write_port(0x3C4, 0x02);
    m.video_mut().write_port(0x3C5, 0x05);
    m.video_mut().write_port(0x3D4, 0x0A);
    m.video_mut().write_port(0x3D5, 0x12);
    m.video_mut().write_port(0x3CE, 0x08);
    m.video_mut().write_port(0x3CF, 0xA5);
    m.video_mut().set_attr_register(0x12, 0x06);
    m.video_mut().write_port(0x3DA, 0x77);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.video_mut().write_port(0x3C4, 0x02);
    m.video_mut().write_port(0x3C5, 0x0F);
    m.video_mut().write_port(0x3D4, 0x0A);
    m.video_mut().write_port(0x3D5, 0x01);
    m.video_mut().write_port(0x3CE, 0x08);
    m.video_mut().write_port(0x3CF, 0x5A);
    m.video_mut().set_attr_register(0x12, 0x00);
    m.video_mut().write_port(0x3DA, 0x11);

    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    m.video_mut().write_port(0x3C4, 0x02);
    assert_eq!(m.video_mut().read_port(0x3C5), Some(0x05));
    assert_eq!(color_crtc_reg(&mut m, 0x0A), 0x12);
    m.video_mut().write_port(0x3CE, 0x08);
    assert_eq!(m.video_mut().read_port(0x3CF), Some(0xA5));
    assert_eq!(m.video().attr_register(0x12), 0x06);
    assert_eq!(m.video_mut().read_port(0x3CA), Some(0x77));
}

#[test]
fn int10_save_restore_state_round_trips_cga_output_only_registers() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.video_mut().write_port(0x3D8, 0x0A);
    m.video_mut().write_port(0x3D9, 0x35);
    for (index, value) in [(0x01, 0x20), (0x09, 0x01), (0x0A, 0x06), (0x0B, 0x07)] {
        m.video_mut().write_port(0x3D4, index);
        m.video_mut().write_port(0x3D5, value);
    }

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.video_mut().write_port(0x3D8, 0x1A);
    m.video_mut().write_port(0x3D9, 0x00);
    for (index, value) in [(0x01, 0x28), (0x09, 0x07), (0x0A, 0x01), (0x0B, 0x02)] {
        m.video_mut().write_port(0x3D4, index);
        m.video_mut().write_port(0x3D5, value);
    }

    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    assert_eq!(m.video().active_mode(), VideoMode::Cga);
    assert_eq!(m.video().cga_mode_control(), 0x0A);
    assert_eq!(m.video().cga_color_select(), 0x35);
    assert_eq!(m.video().crtc_register_latch(0x01), 0x20);
    assert_eq!(m.video().crtc_register_latch(0x09), 0x01);
    assert_eq!(m.video().crtc_register_latch(0x0A), 0x06);
    assert_eq!(m.video().crtc_register_latch(0x0B), 0x07);
    assert_eq!(m.video().crtc_index_latch(), 0x0B);
    assert_eq!(m.video().raster_width(), 256);
    m.video_mut().write_port(0x3D4, 0x01);
    assert_eq!(m.video_mut().read_port(0x3D5), None);
}

#[test]
fn int10_save_restore_state_reenters_cga_text_from_planar_mode() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0002);
    m.handle_int10();
    m.video_mut().write_port(0x3D9, 0x15);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6400);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.video().active_mode(), VideoMode::Planar);

    m.cpu.registers.set_ebx(0x6400);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    assert_eq!(m.video().active_mode(), VideoMode::Text);
    assert!(m.video().is_cga_personality());
    assert_eq!(m.video().cga_mode_control(), 0x2D);
    assert_eq!(m.video().cga_color_select(), 0x15);
    assert_eq!(m.video().raster_width(), 640);
    m.video_mut().write_port(0x3D4, 0x01);
    assert_eq!(m.video_mut().read_port(0x3D5), None);
}

#[test]
fn int10_save_restore_state_round_trips_dac_without_grayscale_summing() {
    let mut m = int15_machine(16);
    m.video_mut().set_grayscale_summing_enabled(false);
    m.video_mut().set_dac_entry(5, 1, 2, 3);
    m.video_mut().write_port(0x3C6, 0x0F);
    m.video_mut().set_attr_register(0x14, 0x0C);
    m.video_mut().write_port(0x3C8, 0x22);
    m.video_mut().set_grayscale_summing_enabled(true);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x7000);
    m.cpu.registers.set_ecx(0x0004);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.video_mut().set_dac_entry(5, 63, 0, 0);
    m.video_mut().write_port(0x3C6, 0xFF);
    m.video_mut().set_attr_register(0x14, 0x00);
    m.video_mut().write_port(0x3C8, 0x00);

    m.cpu.registers.set_ebx(0x7000);
    m.cpu.registers.set_ecx(0x0004);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    assert_eq!(m.video().dac_entry(5), [1, 2, 3]);
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0x0F));
    assert_eq!(m.video().attr_register(0x14), 0x0C);
    assert_eq!(m.video_mut().read_port(0x3C8), Some(0x22));
    assert!(m.video().grayscale_summing_enabled());
}

#[test]
fn int15_c0_reports_honest_feature_byte() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC000);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "AH = 00 on success"
    );
    // ES:BX points at the seeded config table.
    let es = m.cpu.registers.segment(SegmentIndex::Es).base;
    let bx = m.cpu.registers.ebx() as u16;
    let addr = es + u32::from(bx);
    let len = m.read_guest_word(addr);
    assert_eq!(len, 8, "table reports 8 bytes following");
    assert_eq!(m.read_physical_u8(addr + 2), 0xFC, "AT-class model byte");
    let feature1 = m.read_physical_u8(addr + 5);
    assert_eq!(feature1 & 0x40, 0x40, "second PIC present");
    assert_eq!(feature1 & 0x20, 0x20, "RTC present");
    assert_eq!(feature1 & 0x04, 0x04, "EBDA allocated");
    assert_eq!(
        feature1 & 0x10,
        0x00,
        "no AH=4Fh keyboard-intercept callout"
    );
    assert_eq!(feature1 & 0x08, 0x00, "wait-for-event not supported");
    assert_eq!(feature1 & 0x02, 0x00, "ISA bus, not Micro Channel");
}

#[test]
fn int15_c1_returns_ebda_segment_and_size_byte() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC100);
    m.handle_int15();
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Es).selector,
        0x9FC0,
        "ES = EBDA segment"
    );
    // The EBDA size byte at 0x9FC00 reports 1 KB, and INT 12h dropped to 639.
    assert_eq!(m.memory.read_u8(0x9FC00).unwrap(), 1, "EBDA size = 1 KB");
    assert_eq!(
        m.memory.read_u16(0x413).unwrap(),
        639,
        "conventional lowered"
    );
}

#[test]
fn int13_ah05_format_track_fills_with_f6() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 720 KB, 9 spt
    // AH=05 AL=9 sectors, CH=3 (track 3), DH=1 (head 1), DL=0 (A:).
    m.cpu.registers.set_eax(0x0509);
    m.cpu.registers.set_ecx(0x0300); // CH=3, CL=0
    m.cpu.registers.set_edx(0x0100); // DH=1, DL=0
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "AH = 00 on success"
    );
    // The BDA last-disk-status byte records success. (CF rides the IRET frame,
    // which a direct handler call has no real stack for; AH and 0040:0041 carry
    // the result either way.)
    assert_eq!(
        m.memory.read_u8(0x441).unwrap(),
        0x00,
        "disk status = success"
    );
    // A CHS read of that track returns the 0xF6 filler.
    let sector = m
        .floppy
        .as_ref()
        .unwrap()
        .read_sector(3, 1, 1)
        .unwrap()
        .to_vec();
    assert_eq!(sector[0], 0xF6);
    assert_eq!(sector[511], 0xF6);
}

#[test]
fn int13_ah05_format_track_rejects_bad_track_and_fixed_disk() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 80 cylinders, 2 heads
    // Track 80 is off an 80-cylinder disk: AH=0Ch bad track.
    m.cpu.registers.set_eax(0x0509);
    m.cpu.registers.set_ecx(0x5000); // CH=0x50 = 80
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x0C, "bad-track error");
    assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x0C, "status = bad track");
    // The track was not formatted: its first sector is still zero, not 0xF6.
    assert_eq!(
        m.floppy.as_ref().unwrap().read_sector(0, 0, 1).unwrap()[0],
        0x00
    );
    // A fixed-disk unit (DL>=0x80) reports no such drive (AH=0x80).
    m.cpu.registers.set_eax(0x0509);
    m.cpu.registers.set_ecx(0x0000);
    m.cpu.registers.set_edx(0x0080); // DL = 0x80
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x80, "no fixed disk");
    assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x80, "status = no drive");
}

#[test]
fn int13_ah16_reports_floppy_not_changed() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    prime_dos_int_frame(&mut m);

    m.cpu.registers.set_eax(0x1600);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x00, "status = success");
    assert_eq!(dos_int_flags(&m) & 0x0001, 0, "CF clear");
}

#[test]
fn int13_ah17_validates_floppy_format_class() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 720 KB

    m.cpu.registers.set_eax(0x1704);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "720 KB accepted");

    m.cpu.registers.set_eax(0x1703);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x0c, "1.2 MB rejected");
    assert_eq!(
        m.memory.read_u8(0x441).unwrap(),
        0x0c,
        "status = unsupported media"
    );
}

#[test]
fn int13_ah18_returns_diskette_parameter_table_for_current_media() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 80 cyl, 18 spt
    prime_dos_int_frame(&mut m);

    m.cpu.registers.set_eax(0x1800);
    m.cpu.registers.set_ecx(0x4f12); // max cylinder 79, 18 sectors
    m.cpu.registers.set_edx(0x0000);
    m.cpu.registers.set_edi(0xCAFE_1234);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(dos_int_flags(&m) & 0x0001, 0, "CF clear");
    let es = m.cpu.registers.segment(SegmentIndex::Es).base;
    let di = m.cpu.registers.edi() as u16;
    assert_eq!(
        es + u32::from(di),
        BIOS_DISKETTE_PARAMETER_TABLE_ADDR,
        "ES:DI points at the DPT"
    );
}
