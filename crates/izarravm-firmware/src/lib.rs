pub const I386DX25_TEST_ROM: &[u8] = include_bytes!("../roms/i386dx25-test.bin");
pub const I386DX25_TEST_ROM_SOURCE: &str = include_str!("../roms/i386dx25-test.asm");
pub const X86_BOOT_TEST_IMAGE: &[u8] = include_bytes!("../roms/boot-suite/izarravm-test.img");
pub const X86_BOOT_TEST_BOOT_SOURCE: &str = include_str!("../roms/boot-suite/boot.asm");
pub const X86_BOOT_TEST_STAGE2_SOURCE: &str = include_str!("../roms/boot-suite/stage2.asm");
pub const X86_BOOT_TEST_RESULTS_SOURCE: &str = include_str!("../roms/boot-suite/results.inc");
pub const HELLO_COM: &[u8] = include_bytes!("../roms/dos/hello.com");
pub const HELLO_COM_SOURCE: &str = include_str!("../roms/dos/hello.asm");

pub const I386DX25_TEST_ROM_SIZE: usize = 64 * 1024;
pub const X86_BOOT_TEST_IMAGE_SIZE: usize = 1440 * 1024;
pub const X86_BOOT_RESULT_BLOCK_ADDRESS: usize = 0x9000;
pub const X86_BOOT_RESULT_MAGIC: &[u8; 4] = b"VDTS";

pub fn test_rom() -> &'static [u8] {
    I386DX25_TEST_ROM
}

pub fn boot_test_image() -> &'static [u8] {
    X86_BOOT_TEST_IMAGE
}

pub fn hello_com() -> &'static [u8] {
    HELLO_COM
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
}
