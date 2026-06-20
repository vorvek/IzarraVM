pub const I386DX25_TEST_ROM: &[u8] = include_bytes!("../roms/i386dx25-test.bin");
pub const I386DX25_TEST_ROM_SOURCE: &str = include_str!("../roms/i386dx25-test.asm");
pub const X86_BOOT_TEST_IMAGE: &[u8] = include_bytes!("../roms/boot-suite/izarravm-test.img");
pub const X86_BOOT_TEST_BOOT_SOURCE: &str = include_str!("../roms/boot-suite/boot.asm");
pub const X86_BOOT_TEST_STAGE2_SOURCE: &str = include_str!("../roms/boot-suite/stage2.asm");
pub const X86_BOOT_TEST_RESULTS_SOURCE: &str = include_str!("../roms/boot-suite/results.inc");
pub const HELLO_COM: &[u8] = include_bytes!("../roms/dos/hello.com");
pub const HELLO_COM_SOURCE: &str = include_str!("../roms/dos/hello.asm");
pub const ECHO_COM: &[u8] = include_bytes!("../roms/dos/echo.com");
pub const ECHO_COM_SOURCE: &str = include_str!("../roms/dos/echo.asm");
pub const TYPE_COM: &[u8] = include_bytes!("../roms/dos/type.com");
pub const TYPE_COM_SOURCE: &str = include_str!("../roms/dos/type.asm");
pub const EXEHELLO_EXE: &[u8] = include_bytes!("../roms/dos/exehello.exe");
pub const EXEHELLO_EXE_SOURCE: &str = include_str!("../roms/dos/exehello.asm");
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

/// The Toka-DOS ROM image: a packed blob of the OS system files (ICOMMAND, the
/// boot record, and the tools) that the machine lays down onto the C: drive.
/// It lives in the motherboard BOOT.rom alongside the BIOS and Belunza.
pub const TOKA_DOS_ROM: &[u8] = include_bytes!("../roms/tokados.rom");

/// The slice of the 2 MB BOOT.rom reserved for Toka-DOS. The BIOS keeps its
/// 64 KiB and Belunza gets the rest. The fit test fails loudly if the packed
/// OS ever outgrows this.
pub const TOKA_DOS_ROM_BUDGET: usize = 1024 * 1024;

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
    IZARRA_BIOS
}

pub fn boot_test_image() -> &'static [u8] {
    X86_BOOT_TEST_IMAGE
}

pub fn toka_dos_rom() -> &'static [u8] {
    TOKA_DOS_ROM
}

/// The Toka-DOS system files as owned (DOS 8.3 name, bytes) pairs, ready to hand
/// to `izarravm_dos::toka_dos_install`. Parses the embedded ROM; panics only if
/// the checked-in blob is malformed, which the fit test would already catch.
pub fn toka_dos_system_files() -> Vec<(String, Vec<u8>)> {
    toka_rom::files(TOKA_DOS_ROM)
        .expect("embedded tokados.rom is well formed")
        .into_iter()
        .map(|file| (file.name, file.data.to_vec()))
        .collect()
}

/// Reader for the packed Toka-DOS ROM. The format is a small table of contents
/// followed by concatenated file data:
///
/// ```text
/// 0  4    magic "TOKA"
/// 4  2    version u16 LE (1)
/// 6  2    file count u16 LE
/// 8  2    reserved (0)
/// 10 n*20 directory entries
/// ...     file data
///
/// directory entry (20 bytes):
///   0  11  name, 8.3 packed and space padded (e.g. "ICOMMAND COM")
///   11 1   flags (bit0 = system file)
///   12 4   data offset from start of blob (u32 LE)
///   16 4   data length (u32 LE)
/// ```
pub mod toka_rom {
    /// The fixed on-disk sizes the reader and packer must agree on.
    pub const HEADER_LEN: usize = 10;
    pub const ENTRY_LEN: usize = 20;
    pub const NAME_LEN: usize = 11;
    pub const MAGIC: &[u8; 4] = b"TOKA";
    pub const VERSION: u16 = 1;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RomError {
        BadMagic,
        BadVersion(u16),
        TruncatedDirectory,
        DataOutOfRange,
    }

    /// A single file in the ROM: its DOS 8.3 name, flag byte, and data slice.
    #[derive(Debug, Clone)]
    pub struct File<'a> {
        pub name: String,
        pub flags: u8,
        pub data: &'a [u8],
    }

    /// Decode an 8.3 name field ("ICOMMAND COM") into "ICOMMAND.COM". A blank
    /// extension yields just the base name.
    fn decode_8_3(raw: &[u8]) -> String {
        let base = String::from_utf8_lossy(&raw[0..8]).trim_end().to_string();
        let ext = String::from_utf8_lossy(&raw[8..11]).trim_end().to_string();
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    }

    /// Parse every file in the ROM, validating the header and each entry's bounds.
    pub fn files(rom: &[u8]) -> Result<Vec<File<'_>>, RomError> {
        if rom.len() < HEADER_LEN || &rom[0..4] != MAGIC {
            return Err(RomError::BadMagic);
        }
        let version = u16::from_le_bytes([rom[4], rom[5]]);
        if version != VERSION {
            return Err(RomError::BadVersion(version));
        }
        let count = u16::from_le_bytes([rom[6], rom[7]]) as usize;
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            let entry = HEADER_LEN + index * ENTRY_LEN;
            let raw = rom
                .get(entry..entry + ENTRY_LEN)
                .ok_or(RomError::TruncatedDirectory)?;
            let name = decode_8_3(&raw[0..NAME_LEN]);
            let flags = raw[11];
            let off = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]) as usize;
            let len = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]) as usize;
            let data = rom
                .get(off..off + len)
                .ok_or(RomError::DataOutOfRange)?;
            out.push(File { name, flags, data });
        }
        Ok(out)
    }
}

pub fn hello_com() -> &'static [u8] {
    HELLO_COM
}

pub fn echo_com() -> &'static [u8] {
    ECHO_COM
}

pub fn exehello_exe() -> &'static [u8] {
    EXEHELLO_EXE
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
    fn izarra_bios_is_64k() {
        assert_eq!(IZARRA_BIOS.len(), I386DX25_TEST_ROM_SIZE);
    }

    #[test]
    fn toka_rom_parses_and_fits() {
        let rom = toka_dos_rom();
        assert_eq!(&rom[0..4], toka_rom::MAGIC);
        assert!(
            rom.len() <= TOKA_DOS_ROM_BUDGET,
            "tokados.rom is {} bytes, over the {} byte budget",
            rom.len(),
            TOKA_DOS_ROM_BUDGET
        );
        // Every entry must decode and its data slice must be in bounds.
        let files = toka_rom::files(rom).expect("tokados.rom parses");
        for file in &files {
            assert!(!file.name.is_empty(), "ROM file has an empty name");
        }
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
    fn kbd_resident_header_offsets_are_in_bounds() {
        let image = super::KBD_RESIDENT_BIOS;
        let int09 = u16::from_le_bytes([image[0], image[1]]) as usize;
        let int16 = u16::from_le_bytes([image[2], image[3]]) as usize;
        assert!(int09 >= 4 && int09 < image.len(), "int09 offset in image");
        assert!(int16 >= 4 && int16 < image.len(), "int16 offset in image");
        assert!(image.len() < 4096, "resident BIOS stays small");
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
}
