pub const I386DX25_TEST_ROM: &[u8] = include_bytes!("../roms/i386dx25-test.bin");
pub const I386DX25_TEST_ROM_SOURCE: &str = include_str!("../roms/i386dx25-test.asm");
pub const X86_BOOT_TEST_IMAGE: &[u8] = include_bytes!("../roms/boot-suite/izarravm-test.img");
pub const X86_BOOT_TEST_BOOT_SOURCE: &str = include_str!("../roms/boot-suite/boot.asm");
pub const X86_BOOT_TEST_STAGE2_SOURCE: &str = include_str!("../roms/boot-suite/stage2.asm");
pub const X86_BOOT_TEST_RESULTS_SOURCE: &str = include_str!("../roms/boot-suite/results.inc");
pub const NEURKETA_IMAGE: &[u8] = include_bytes!("../roms/neurketa/neurketa.img");
pub const NEURKETA_STAGE2_SOURCE: &str = include_str!("../roms/neurketa/neurketa-stage2.asm");
pub const HELLO_COM: &[u8] = include_bytes!("../roms/dos/hello.com");
pub const HELLO_COM_SOURCE: &str = include_str!("../roms/dos/hello.asm");
pub const ECHO_COM: &[u8] = include_bytes!("../roms/dos/echo.com");
pub const ECHO_COM_SOURCE: &str = include_str!("../roms/dos/echo.asm");
pub const TYPE_COM: &[u8] = include_bytes!("../roms/dos/type.com");
pub const TYPE_COM_SOURCE: &str = include_str!("../roms/dos/type.asm");
pub const RUNNER_COM: &[u8] = include_bytes!("../roms/dos/runner.com");
pub const RUNNER_COM_SOURCE: &str = include_str!("../roms/dos/runner.asm");
pub const EXIT42_COM: &[u8] = include_bytes!("../roms/dos/exit42.com");
pub const EXIT42_COM_SOURCE: &str = include_str!("../roms/dos/exit42.asm");
pub const EXEHELLO_EXE: &[u8] = include_bytes!("../roms/dos/exehello.exe");
pub const EXEHELLO_EXE_SOURCE: &str = include_str!("../roms/dos/exehello.asm");
/// The freestanding Dhrystone 2.1 benchmark, built as a small-model DOS .EXE.
/// It carries no C runtime: the records are static, the run count is fixed at
/// 10000, and the result is a 16-bit self-check fold reported to the Lotura
/// unit-tester device. Load it with `Machine::new_raw_program` and read the
/// result back with `Machine::bench_iterations` (10000) and `bench_aux` (the
/// fold). It is a .EXE rather than a .COM so the MZ relocations place its global
/// variables in the data segment instead of overwriting the code.
pub const DHRYSTONE_EXE: &[u8] = include_bytes!("../roms/neurketa-c/dhrystone.exe");
/// The freestanding Whetstone double-precision FP benchmark .EXE: the floating-point
/// oracle for the per-mode `fp_timing` factor. Same harness contract as Dhrystone
/// (load with `Machine::new_raw_program`, read `bench_iterations` = the sweep count
/// and `bench_aux` = the FP self-check fold). See roms/neurketa-c/whetstone.c.
pub const WHETSTONE_EXE: &[u8] = include_bytes!("../roms/neurketa-c/whetstone.exe");
pub const KBD_BIOS: &[u8] = include_bytes!("../roms/kbd-bios.bin");
pub const KBD_BIOS_SOURCE: &str = include_str!("../roms/kbd-bios.asm");
pub const KBD_RESIDENT_BIOS: &[u8] = include_bytes!("../roms/kbd-resident.bin");
pub const KBD_RESIDENT_BIOS_SOURCE: &str = include_str!("../roms/kbd-resident.asm");
/// Segment the resident keyboard BIOS loads at (F000:0000). The INT 09h/16h
/// handlers run with CS set to this and use cs-relative table lookups, so the
/// installer must place the image at this segment's offset 0.
pub const KBD_RESIDENT_BIOS_SEG: u16 = 0xf000;
pub const IZARRA_BIOS: &[u8] = include_bytes!("../roms/izarra-bios.bin");
pub const IZARRA_BIOS_SOURCE: &str = include_str!("../roms/izarra-bios.asm");

/// The five code-page fonts (437, 850, 860, 863, 865), each at 8x16, 8x14, then
/// 8x8. Code-page-major: block `cp` at `cp * 9728`, sizes at 0 / 4096 / 7680.
/// The machine banks one page at a time into a 4 KB window (0xC4000) when the
/// guest writes a selector to Lotura port 0xE7; the BIOS then copies that page
/// into the VGA character generator.
pub const CODEPAGE_FONTS: &[u8] = include_bytes!("../roms/codepage-fonts.bin");

/// The izarra flash chip is 256 KiB. The board shadows only the top 64 KiB to
/// 0xF0000, exactly like a period board where the BIOS shadow is a slice of a
/// larger flash. The lower 192 KiB is reserved (room for uncompressed art, a
/// VGA option ROM, etc.) and is not CPU-addressable.
pub const IZARRA_FLASH_SIZE: usize = 256 * 1024;

static IZARRA_FLASH: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    let mut flash = vec![0u8; IZARRA_FLASH_SIZE];
    let top = IZARRA_FLASH_SIZE - IZARRA_BIOS.len();
    flash[top..].copy_from_slice(IZARRA_BIOS);
    flash
});

/// The Toka-DOS hard-disk image: a partitioned, bootable FAT32 disk image with a
/// standard MBR, one primary FAT32-LBA partition, and a complete Toka-DOS system
/// (KERNEL.SYS, COMMAND.COM, CONFIG.SYS, AUTOEXEC.BAT, HELLO.TXT). Mount with
/// `Machine::mount_hdd`; INT 19h boots LBA 0 (the MBR), which chains to the
/// partition's FAT32 VBR. Built by scripts/build-freedos-hdd-image.py.
pub const TOKADOS_HDD_IMG: &[u8] = include_bytes!("../roms/tokados-hdd.img");

pub const I386DX25_TEST_ROM_SIZE: usize = 64 * 1024;
pub const X86_BOOT_TEST_IMAGE_SIZE: usize = 1440 * 1024;
pub const X86_BOOT_RESULT_BLOCK_ADDRESS: usize = 0x9000;
pub const X86_BOOT_RESULT_MAGIC: &[u8; 4] = b"VDTS";

pub fn test_rom() -> &'static [u8] {
    I386DX25_TEST_ROM
}

pub fn kbd_bios() -> &'static [u8] {
    KBD_BIOS
}

pub fn kbd_resident_bios() -> &'static [u8] {
    KBD_RESIDENT_BIOS
}

pub fn izarra_bios() -> &'static [u8] {
    &IZARRA_FLASH
}

pub fn boot_test_image() -> &'static [u8] {
    X86_BOOT_TEST_IMAGE
}

/// The Neurketa benchmark boot image: a 1.44 MiB floppy that boots a 16-bit
/// loader plus the Sieve payload. Run with `Machine::new_boot_image`, preload
/// the selector with `Machine::set_bench_selector`, and read the results back
/// with `Machine::bench_iterations` / `bench_aux` after the `TestExit` stop.
pub fn neurketa_image() -> &'static [u8] {
    NEURKETA_IMAGE
}

pub fn tokados_hdd_img() -> &'static [u8] {
    TOKADOS_HDD_IMG
}

pub fn hello_com() -> &'static [u8] {
    HELLO_COM
}

pub fn echo_com() -> &'static [u8] {
    ECHO_COM
}

/// The `--katea-run` harness: EXECs the named program, captures its exit code, and
/// reports it to the unit-tester exit port. Overlaid onto C: as `RUNNER.COM`.
pub fn runner_com() -> &'static [u8] {
    RUNNER_COM
}

/// A test program that terminates with DOS exit code 42; the katea-run e2e fixture.
pub fn exit42_com() -> &'static [u8] {
    EXIT42_COM
}

pub fn exehello_exe() -> &'static [u8] {
    EXEHELLO_EXE
}

pub fn dhrystone_exe() -> &'static [u8] {
    DHRYSTONE_EXE
}

pub fn whetstone_exe() -> &'static [u8] {
    WHETSTONE_EXE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteRecordStatus {
    Begin,
    Pass,
    Fail,
    Measure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteRecord {
    pub status: SuiteRecordStatus,
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteResults {
    pub version: u16,
    pub declared_record_count: u16,
    pub payload_len: u16,
    pub checksum: u16,
    pub records: Vec<SuiteRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuiteParseError {
    MissingMagic,
    TruncatedHeader,
    TruncatedPayload,
    InvalidUtf8,
    ChecksumMismatch { expected: u16, actual: u16 },
    UnknownRecordStatus(String),
}

impl std::fmt::Display for SuiteParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMagic => formatter.write_str("missing boot-suite result magic"),
            Self::TruncatedHeader => formatter.write_str("truncated boot-suite result header"),
            Self::TruncatedPayload => formatter.write_str("truncated boot-suite result payload"),
            Self::InvalidUtf8 => formatter.write_str("boot-suite result payload is not UTF-8"),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "boot-suite result checksum mismatch: expected {expected:#06x}, got {actual:#06x}"
            ),
            Self::UnknownRecordStatus(status) => {
                write!(formatter, "unknown boot-suite record status '{status}'")
            }
        }
    }
}

impl std::error::Error for SuiteParseError {}

pub fn parse_result_block(memory: &[u8]) -> Result<SuiteResults, SuiteParseError> {
    if memory.len() < X86_BOOT_RESULT_BLOCK_ADDRESS + 12 {
        return Err(SuiteParseError::TruncatedHeader);
    }

    let block = &memory[X86_BOOT_RESULT_BLOCK_ADDRESS..];
    if &block[0..4] != X86_BOOT_RESULT_MAGIC {
        return Err(SuiteParseError::MissingMagic);
    }

    let version = read_u16(&block[4..6])?;
    let declared_record_count = read_u16(&block[6..8])?;
    let payload_len = read_u16(&block[8..10])?;
    let checksum = read_u16(&block[10..12])?;
    let payload_start = 12;
    let payload_end = payload_start + usize::from(payload_len);
    if block.len() < payload_end {
        return Err(SuiteParseError::TruncatedPayload);
    }

    let payload = &block[payload_start..payload_end];
    let actual = additive_checksum(payload);
    if actual != checksum {
        return Err(SuiteParseError::ChecksumMismatch {
            expected: checksum,
            actual,
        });
    }

    let text = std::str::from_utf8(payload).map_err(|_| SuiteParseError::InvalidUtf8)?;
    Ok(SuiteResults {
        version,
        declared_record_count,
        payload_len,
        checksum,
        records: parse_records(text)?,
    })
}

pub fn parse_serial_records(text: &str) -> Result<Vec<SuiteRecord>, SuiteParseError> {
    parse_records(text)
}

fn parse_records(text: &str) -> Result<Vec<SuiteRecord>, SuiteParseError> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_record)
        .collect()
}

fn parse_record(line: &str) -> Result<SuiteRecord, SuiteParseError> {
    let mut parts = line.splitn(3, ' ');
    let status = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default().to_owned();
    let value = parts.next().map(str::to_owned);
    let status = match status {
        "BEGIN" => SuiteRecordStatus::Begin,
        "PASS" => SuiteRecordStatus::Pass,
        "FAIL" => SuiteRecordStatus::Fail,
        "MEASURE" => SuiteRecordStatus::Measure,
        other => return Err(SuiteParseError::UnknownRecordStatus(other.to_owned())),
    };

    Ok(SuiteRecord {
        status,
        name,
        value,
    })
}

fn read_u16(bytes: &[u8]) -> Result<u16, SuiteParseError> {
    let bytes: [u8; 2] = bytes
        .try_into()
        .map_err(|_| SuiteParseError::TruncatedHeader)?;
    Ok(u16::from_le_bytes(bytes))
}

fn additive_checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
}

#[cfg(test)]
mod tests {
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
    fn izarra_bios_reset_far_jump() {
        // The reset vector at 0xFFF0 far-jumps to ROM_SEG:0000 (reset at offset 0).
        assert_eq!(
            &IZARRA_BIOS[0xfff0..0xfff5],
            &[0xea, 0x00, 0x00, 0x00, 0xf0]
        );
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
        // The INT 16h dispatch routes each function to its own handler: AH=00h/10h
        // (legacy/enhanced read), 01h/11h (legacy/enhanced peek), 02h/12h (legacy
        // flags / extended shift status), 04h keyclick, 05h buffer write, then
        // 03h/09h/0Ah (set typematic, get functionality, get keyboard id). Each
        // arm is a `cmp ah, imm8` (opcode 80 FC). Only AH=00h/10h reach their
        // nearby read handlers with a short `je rel8` (74); every later handler
        // sits past the grown read/peek/flags code, so NASM emits the near
        // `je rel16` form (0F 84).
        // Assert the whole chain appears in order, ending in the bare iret
        // fall-through. Runtime coverage of this handler is infeasible without
        // booting the full ROM into a guest stub (the DOS-program test harness
        // installs a different keyboard ROM, kbd-bios-core.inc), so this asserts
        // the assembled bytes. Re-derive the displacements from the rebuilt .bin
        // (read the bytes at the dispatch site) whenever a handler is added.
        let dispatch: &[u8] = &[
            0x80, 0xfc, 0x00, // cmp ah, 0x00 (read)
            0x74, 0x45, //       je .read
            0x80, 0xfc, 0x10, // cmp ah, 0x10 (enhanced read)
            0x74, 0x75, //       je .read16
            0x80, 0xfc, 0x01, // cmp ah, 0x01 (peek)
            0x0f, 0x84, 0x9d, 0x00, // je .peek
            0x80, 0xfc, 0x11, // cmp ah, 0x11 (enhanced peek)
            0x0f, 0x84, 0xc2, 0x00, // je .peek16
            0x80, 0xfc, 0x02, // cmp ah, 0x02 (flags)
            0x0f, 0x84, 0xe1, 0x00, // je .flags
            0x80, 0xfc, 0x12, // cmp ah, 0x12 (extended shift status)
            0x0f, 0x84, 0xe7, 0x00, // je .flags12
            0x80, 0xfc, 0x04, // cmp ah, 0x04 (PCjr keyclick)
            0x0f, 0x84, 0xef, 0x00, // je .keyclick
            0x80, 0xfc, 0x05, // cmp ah, 0x05 (buffer write)
            0x0f, 0x84, 0xe9, 0x00, // je .bufwrite
            0x80, 0xfc, 0x03, // cmp ah, 0x03 (set typematic rate and delay)
            0x0f, 0x84, 0x10, 0x01, // je .typematic
            0x80, 0xfc, 0x09, // cmp ah, 0x09 (get keyboard functionality)
            0x0f, 0x84, 0x4f, 0x01, // je .funcs
            0x80, 0xfc, 0x0a, // cmp ah, 0x0a (get keyboard id)
            0x0f, 0x84, 0x4d, 0x01, // je .kbid
            0xcf, //             iret (unhandled fall-through)
        ];
        assert!(
            IZARRA_BIOS
                .windows(dispatch.len())
                .any(|window| window == dispatch),
            "INT 16h enhanced-function dispatch not found in the Izarra BIOS ROM"
        );
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
        // 2. Legacy read collapses the 0xE0 gray-key marker to AL=0 before iret:
        //    cmp al,0xe0; jne +2; xor al,al; iret.
        let read_collapse: &[u8] = &[0x3c, 0xe0, 0x75, 0x02, 0x30, 0xc0, 0xcf];
        // 3. Legacy peek collapses it the same way and edits the saved FLAGS image:
        //    cmp al,0xe0; jne +2; xor al,al; push bp; mov bp,sp;
        //    and word [bp+6],0xffbe; pop bp; iret.
        let peek_collapse: &[u8] = &[
            0x3c, 0xe0, 0x75, 0x02, 0x30, 0xc0, 0x55, 0x89, 0xe5, 0x83, 0x66, 0x06, 0xbe, 0x5d,
            0xcf,
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
        assert!(results.records.iter().any(|record| {
            record.status == SuiteRecordStatus::Fail && record.name == "sound.opl3"
        }));
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
    fn exehello_exe_fixture_is_a_valid_mz() {
        assert!(EXEHELLO_EXE.len() > 0x1c);
        assert_eq!(&EXEHELLO_EXE[0..2], b"MZ");
        // e_crlc at offset 6: at least one relocation, the load-bearing DS load.
        let e_crlc = u16::from_le_bytes([EXEHELLO_EXE[6], EXEHELLO_EXE[7]]);
        assert!(e_crlc >= 1, "fixture must carry a relocation, got {e_crlc}");
    }

    #[test]
    fn dhrystone_exe_starts_with_mz() {
        assert_eq!(&dhrystone_exe()[0..2], &[0x4D, 0x5A]);
    }

    #[test]
    fn whetstone_exe_starts_with_mz() {
        assert_eq!(&whetstone_exe()[0..2], &[0x4D, 0x5A]);
    }
}
