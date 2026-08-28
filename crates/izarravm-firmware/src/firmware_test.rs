// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn codepage_fonts_blob_has_five_pages_three_sizes() {
    assert_eq!(CODEPAGE_FONTS.len(), 48_640); // 5 * (4096 + 3584 + 2048)
}

#[test]
fn codepage_fonts_cp437_matches_shipped_font() {
    use izarravm_video::font::{VGAFONT_8X8, VGAFONT_8X14, VGAFONT_8X16};
    // Block 0 = CP437. 8x16 at [0..4096], 8x14 at [4096..7680], 8x8 at [7680..9728].
    assert_eq!(&CODEPAGE_FONTS[0..4096], &VGAFONT_8X16[..]);
    assert_eq!(&CODEPAGE_FONTS[4096..7680], &VGAFONT_8X14[..]);
    assert_eq!(&CODEPAGE_FONTS[7680..9728], &VGAFONT_8X8[..]);
}

#[test]
fn test_rom_is_64k_and_has_reset_far_jump() {
    assert_eq!(I386DX25_TEST_ROM.len(), I386DX25_TEST_ROM_SIZE);
    assert_eq!(
        &I386DX25_TEST_ROM[0xfff0..0xfff5],
        &[0xea, 0x00, 0x00, 0x00, 0xf0]
    );
}

#[test]
fn kbd_bios_is_64k() {
    assert_eq!(KBD_BIOS.len(), I386DX25_TEST_ROM_SIZE);
}

#[test]
fn izarra_flash_is_256k_with_shadowed_reset() {
    let flash = izarra_bios();
    assert_eq!(flash.len(), IZARRA_FLASH_SIZE);
    // The CPU-shadowed view is the top 64 KiB; its reset vector still far-jumps
    // to ROM_SEG:0000. Offset 0xFFF0 within the top 64 KiB:
    let shadow = &flash[flash.len() - 64 * 1024..];
    assert_eq!(&shadow[0xfff0..0xfff5], &[0xea, 0x00, 0x00, 0x00, 0xf0]);
    // The lower bytes are pad.
    assert!(flash[..flash.len() - 64 * 1024].iter().all(|&b| b == 0));
}

#[test]
fn izarra_bios_carries_v301_version_string() {
    let needle = b"Izarra-BIOS v3.01 - 1997";
    assert!(
        IZARRA_BIOS.windows(needle.len()).any(|w| w == needle),
        "v3.01 version string not found in the ROM"
    );
}

#[test]
fn guest_tools_name_code_3_386_slow() {
    for (name, image, needle) in [
        ("GSWMODE.COM", GSWMODE_COM, b"386-slow".as_slice()),
        ("GSWMODE.COM", GSWMODE_COM, b"use '386-slow'".as_slice()),
    ] {
        assert!(
            image.windows(needle.len()).any(|window| window == needle),
            "{name} does not contain {}",
            String::from_utf8_lossy(needle)
        );
    }
    assert!(!GSWMODE_COM_SOURCE.contains("GSWMODE 286|"));
    assert!(!TOKAEMM_SYS_SOURCE.contains("cpu 286"));
}

#[test]
fn tokacd_is_an_8086_mscdex_character_driver() {
    assert!(TOKACD_SYS.len() < 8 * 1024);
    assert_eq!(&TOKACD_SYS[0..4], &[0xff; 4]);
    assert_eq!(u16::from_le_bytes([TOKACD_SYS[4], TOKACD_SYS[5]]), 0xc800);
    assert_eq!(&TOKACD_SYS[10..18], b"TOKACD01");
    assert_eq!(&TOKACD_SYS[18..22], &[0, 0, 0, 1]);
    assert!(TOKACD_SYS_SOURCE.contains("cpu 8086"));
    assert!(TOKACD_SYS_SOURCE.contains("times 512 db 0"));
}

#[test]
fn cdtest_fixture_requires_the_rom_device_header() {
    assert!(CDTEST_COM_SOURCE.contains("mov ax, 0x1500"));
    assert!(CDTEST_COM_SOURCE.contains("mov ax, 0x1501"));
    assert!(CDTEST_COM_SOURCE.contains("db 'TOKACD01'"));
    assert!(CDTEST_COM_SOURCE.contains("D:\\PROBE.TXT"));
}

#[test]
fn cd_request_fixtures_call_the_driver_entries_directly() {
    assert!(CDPROT_COM_SOURCE.contains("call far [strategy_ptr]"));
    assert!(CDPROT_COM_SOURCE.contains("call far [interrupt_ptr]"));
    assert!(CDPROT_COM_SOURCE.contains("mov ax, 0x1501"));
    assert!(CDPROT_COM_SOURCE.contains("mov al, 128"));
    assert!(CDAUDIO_COM_SOURCE.contains("mov al, 132"));
    assert!(CDAUDIO_COM_SOURCE.contains("mov al, 133"));
    assert!(CDAUDIO_COM_SOURCE.contains("mov al, 136"));
}

#[test]
fn izarra_bios_boot_menu_uses_canonical_cpu_mode_names() {
    let names = [
        b"586\0".as_slice(),
        b"486\0".as_slice(),
        b"386\0".as_slice(),
        b"386-slow\0".as_slice(),
    ]
    .concat();
    assert!(
        IZARRA_BIOS
            .windows(names.len())
            .any(|window| window == names),
        "canonical CPU mode rows not found in the Izarra BIOS ROM"
    );
}

#[test]
fn izarra_bios_reset_far_jump() {
    // The reset vector at 0xFFF0 far-jumps to ROM_SEG:0000 (reset at offset 0).
    assert_eq!(
        &IZARRA_BIOS[0xfff0..0xfff5],
        &[0xea, 0x00, 0x00, 0x00, 0xf0]
    );
}

#[test]
fn izarra_bios_reserves_machine_injection_windows() {
    for range in [
        0xea00..0xea30,
        0xf060..0xf079,
        0xf080..0xf087,
        0xf200..0xf400,
    ] {
        assert!(
            IZARRA_BIOS[range.clone()].iter().all(|byte| *byte == 0),
            "machine injection window {range:?} is not empty"
        );
    }
}

#[test]
fn izarra_bios_embeds_8x8_font() {
    // Glyphs '@' (0x40) and 'A' (0x41) from VGAFONT_8X8, byte-for-byte. A
    // contiguous 16-byte match proves the font copy did not drift.
    let at_and_a: [u8; 16] = [
        0x7c, 0xc6, 0xde, 0xde, 0xde, 0xc0, 0x78, 0x00, // '@'
        0x30, 0x78, 0xcc, 0xcc, 0xfc, 0xcc, 0xcc, 0x00, // 'A'
    ];
    assert!(
        IZARRA_BIOS.windows(16).any(|window| window == at_and_a),
        "8x8 font glyphs @/A not found in the Izarra BIOS ROM"
    );
}

#[test]
fn izarra_bios_int16_dispatch_has_enhanced_aliases() {
    let functions = [
        0x00, 0x10, 0x01, 0x11, 0x02, 0x12, 0x04, 0x05, 0x03, 0x09, 0x0a, 0x92,
    ];
    for (name, rom) in [
        ("izarra-bios.bin", IZARRA_BIOS),
        ("kbd-bios.bin", KBD_BIOS),
        ("kbd-resident.bin", KBD_RESIDENT_BIOS),
    ] {
        let start = rom
            .windows(3)
            .position(|window| window == [0x80, 0xfc, 0x00])
            .unwrap_or_else(|| panic!("INT 16h dispatch missing from {name}"));
        let dispatch = &rom[start..start + 128.min(rom.len() - start)];
        let mut cursor = 0;
        for function in functions {
            let relative = dispatch[cursor..]
                .windows(3)
                .position(|window| window == [0x80, 0xfc, function])
                .unwrap_or_else(|| panic!("INT 16h AH={function:02X} missing from {name}"));
            cursor += relative + 3;
        }
        assert!(
            rom.windows(3).any(|window| window == [0xb4, 0x80, 0xcf]),
            "INT 16h AH=92h handler missing from {name}"
        );
    }
}

#[test]
fn izarra_bios_int16_enhanced_handlers_have_distinct_behavior() {
    // The enhanced functions are real handlers, not aliases. Three assembled
    // signatures prove it, and they must appear in both keyboard ROMs (the
    // izbios-kbd.inc core in the full BIOS and the byte-for-byte kbd-bios-core.inc
    // the resident DOS ROM uses), so this checks each ROM for all three.
    //
    // 1. AH=12h extended shift status reads BOTH flag bytes: push ds; mov bx,40h;
    //    mov ds,bx; mov al,[17h] (KB_FLAGS); mov ah,[18h] (KB_FLAGS_1); pop ds.
    //    The legacy AH=02h handler instead clears AH (xor ah,ah), so a sequence
    //    that loads AH from 0x18 can only be the AH=12h path.
    let flags12: &[u8] = &[
        0x1e, // push ds
        0xbb, 0x40, 0x00, // mov bx, 0x0040
        0x8e, 0xdb, // mov ds, bx
        0xa0, 0x17, 0x00, // mov al, [0x0017]  (KB_FLAGS -> AL)
        0x8a, 0x26, 0x18, 0x00, // mov ah, [0x0018]  (KB_FLAGS_1 -> AH)
        0x1f, // pop ds
    ];
    // 2. Legacy read collapses the 0xE0 gray-key marker to AL=0, restores the
    //    caller's BX (the INT 16h contract preserves every register but
    //    AX/FLAGS), then irets: cmp al,0xe0; jne +2; xor al,al; pop bx; iret.
    let read_collapse: &[u8] = &[0x3c, 0xe0, 0x75, 0x02, 0x30, 0xc0, 0x5b, 0xcf];
    // 3. Legacy peek collapses it the same way and edits the saved FLAGS image:
    //    cmp al,0xe0; jne +2; xor al,al; push bp; mov bp,sp;
    //    and word [bp+6],0xffbe; pop bp; iret.
    let peek_collapse: &[u8] = &[
        0x3c, 0xe0, 0x75, 0x02, 0x30, 0xc0, 0x55, 0x89, 0xe5, 0x83, 0x66, 0x06, 0xbe, 0x5d, 0xcf,
    ];

    let roms: [(&str, &[u8]); 2] = [
        ("izarra-bios.bin", IZARRA_BIOS),
        ("kbd-resident.bin", super::KBD_RESIDENT_BIOS),
    ];
    for (name, rom) in roms {
        for (label, sig) in [
            ("AH=12h two-byte flags read", flags12),
            ("legacy read 0xE0 collapse", read_collapse),
            ("legacy peek 0xE0 collapse", peek_collapse),
        ] {
            assert!(
                rom.windows(sig.len()).any(|window| window == sig),
                "{name} is missing the {label} sequence"
            );
        }
    }
}

#[test]
fn kbd_resident_header_offsets_are_in_bounds() {
    let image = super::KBD_RESIDENT_BIOS;
    let int09 = u16::from_le_bytes([image[0], image[1]]) as usize;
    let int16 = u16::from_le_bytes([image[2], image[3]]) as usize;
    assert!(int09 >= 4 && int09 < image.len(), "int09 offset in image");
    assert!(int16 >= 4 && int16 < image.len(), "int16 offset in image");
    // The resident is mapped as the synthetic BIOS ROM at F000:0000 and only
    // has to stay below the service-return IRET at offset 0xF000. The 17
    // imported layout tables push it past the old conservative 4 KB mark,
    // which was never a real load limit (it is not a TSR; nothing loads it
    // into conventional memory).
    assert!(
        image.len() < 0xF000,
        "resident BIOS fits below the F000 IRET"
    );
}

#[test]
fn boot_test_image_is_1440k_and_bootable() {
    assert_eq!(X86_BOOT_TEST_IMAGE.len(), X86_BOOT_TEST_IMAGE_SIZE);
    assert_eq!(&X86_BOOT_TEST_IMAGE[510..512], &[0x55, 0xaa]);
}

#[test]
fn parses_checked_in_result_block_from_boot_image_stage2() {
    let mut memory = vec![0; 128 * 1024];
    let stage2 = &X86_BOOT_TEST_IMAGE[512..512 + 8192];
    memory[0x8000..0x8000 + stage2.len()].copy_from_slice(stage2);

    let source_block_offset = stage2
        .windows(X86_BOOT_RESULT_MAGIC.len())
        .position(|window| window == X86_BOOT_RESULT_MAGIC)
        .unwrap();
    let source_block = &stage2[source_block_offset..source_block_offset + 512];
    memory[X86_BOOT_RESULT_BLOCK_ADDRESS..X86_BOOT_RESULT_BLOCK_ADDRESS + 512]
        .copy_from_slice(source_block);

    let results = parse_result_block(&memory).unwrap();
    assert_eq!(
        usize::from(results.declared_record_count),
        results.records.len()
    );
    assert!(results.records.iter().any(|record| {
        record.status == SuiteRecordStatus::Pass && record.name == "video.vga_text"
    }));
    assert!(
        results.records.iter().any(|record| {
            record.status == SuiteRecordStatus::Fail && record.name == "sound.opl3"
        })
    );
}

#[test]
fn neurketa_image_is_a_full_floppy() {
    assert_eq!(neurketa_image().len(), X86_BOOT_TEST_IMAGE_SIZE);
    // The boot sector ends in the 0xAA55 signature.
    let image = neurketa_image();
    assert_eq!(&image[510..512], &[0x55, 0xAA]);
}

#[test]
fn type_com_fixture_is_present() {
    assert!(!TYPE_COM.is_empty());
    assert_eq!(TYPE_COM[0], 0xb8); // mov ax, imm16 (the AH=3Dh open setup)
}

#[test]
fn relocchk_exe_fixture_carries_the_span_crossing_reloc_table() {
    assert_eq!(&RELOCCHK_EXE[0..2], b"MZ");
    // e_crlc at offset 6: 130 fixups = four full 32-entry kernel spans plus a
    // 2-entry remainder, the shape the katea_run guest row depends on.
    let e_crlc = u16::from_le_bytes([RELOCCHK_EXE[6], RELOCCHK_EXE[7]]);
    assert_eq!(e_crlc, 130, "the fixture must keep its span-crossing count");
    let e_lfarlc = u16::from_le_bytes([RELOCCHK_EXE[24], RELOCCHK_EXE[25]]);
    assert_eq!(
        e_lfarlc, 0x40,
        "reloc table at 0x40, inside the 1 KB header"
    );
}

#[test]
fn exehello_exe_fixture_is_a_valid_mz() {
    assert!(EXEHELLO_EXE.len() > 0x1c);
    assert_eq!(&EXEHELLO_EXE[0..2], b"MZ");
    // e_crlc at offset 6: at least one relocation, the load-bearing DS load.
    let e_crlc = u16::from_le_bytes([EXEHELLO_EXE[6], EXEHELLO_EXE[7]]);
    assert!(e_crlc >= 1, "fixture must carry a relocation, got {e_crlc}");
}
