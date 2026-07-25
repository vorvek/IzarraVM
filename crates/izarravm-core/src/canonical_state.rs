// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::num::{NonZeroU16, NonZeroU32};

use thiserror::Error;

/// Magic for the canonical state container. This identifies the framing only.
pub const CANONICAL_STATE_CONTAINER_MAGIC: [u8; 8] = *b"IZCSTATE";
/// Major framing version. A change that old readers cannot frame increments this.
pub const CANONICAL_STATE_CONTAINER_MAJOR: u16 = 1;
/// Minor framing version. Readers accept later minors with the same major.
pub const CANONICAL_STATE_CONTAINER_MINOR: u16 = 0;

const HEADER_LEN: usize = 20;
const SECTION_HEADER_LEN: usize = 16;
const HEADER_FLAGS_OFFSET: usize = 12;
const SECTION_COUNT_OFFSET: usize = 16;
const OPTIONAL_SECTION_FLAG: u16 = 1;

/// A nonzero section identifier assigned by the state schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalSectionId(NonZeroU32);

impl CanonicalSectionId {
    /// Creates an identifier. Zero is reserved by the container format.
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// A nonzero, section-local payload schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalSectionVersion(NonZeroU16);

impl CanonicalSectionVersion {
    /// Creates a section version. Zero is reserved by the container format.
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Whether a later schema reader may skip a section it does not recognize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalSectionRequirement {
    Required,
    Optional,
}

impl CanonicalSectionRequirement {
    const fn flags(self) -> u16 {
        match self {
            Self::Required => 0,
            Self::Optional => OPTIONAL_SECTION_FLAG,
        }
    }

    const fn from_flags(flags: u16) -> Option<Self> {
        match flags {
            0 => Some(Self::Required),
            OPTIONAL_SECTION_FLAG => Some(Self::Optional),
            _ => None,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CanonicalStateError {
    #[error("canonical state allocation failed")]
    AllocationFailed,
    #[error("canonical state length or offset overflowed")]
    LengthOverflow,
    #[error("canonical state has too many sections")]
    SectionCountOverflow,
    #[error("section {next:#010x} does not follow {previous:#010x}")]
    SectionOutOfOrder { previous: u32, next: u32 },
    #[error("canonical state header is truncated")]
    TruncatedHeader,
    #[error("canonical state magic is invalid")]
    InvalidMagic,
    #[error("canonical state container major version {found} is unsupported")]
    UnsupportedMajorVersion { found: u16 },
    #[error("canonical state header flags {found:#010x} are unsupported")]
    UnsupportedHeaderFlags { found: u32 },
    #[error("section header {index} is truncated")]
    TruncatedSectionHeader { index: u32 },
    #[error("section identifier zero is reserved")]
    ReservedSectionId,
    #[error("section {section_id:#010x} uses reserved schema version zero")]
    ReservedSectionVersion { section_id: u32 },
    #[error("section {section_id:#010x} flags {found:#06x} are unsupported")]
    UnsupportedSectionFlags { section_id: u32, found: u16 },
    #[error(
        "section {section_id:#010x} payload is truncated: declared {declared} bytes, {remaining} remain"
    )]
    TruncatedSectionPayload {
        section_id: u32,
        declared: u64,
        remaining: u64,
    },
    #[error("canonical state has {count} trailing bytes")]
    TrailingBytes { count: usize },
}

/// Writes one section payload directly into the container output.
///
/// Collection ordering and explicit enum tags belong to each section schema.
/// This writer does not canonicalize maps, paths, or other owner data.
pub struct CanonicalFieldWriter<'a> {
    bytes: &'a mut Vec<u8>,
}

impl CanonicalFieldWriter<'_> {
    fn append(&mut self, value: &[u8]) -> Result<(), CanonicalStateError> {
        reserve(self.bytes, value.len())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    pub fn write_u8(&mut self, value: u8) -> Result<(), CanonicalStateError> {
        self.append(&[value])
    }

    pub fn write_i8(&mut self, value: i8) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_u16(&mut self, value: u16) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_i16(&mut self, value: i16) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_u32(&mut self, value: u32) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_i32(&mut self, value: i32) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_u64(&mut self, value: u64) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_i64(&mut self, value: i64) -> Result<(), CanonicalStateError> {
        self.append(&value.to_le_bytes())
    }

    pub fn write_bool(&mut self, value: bool) -> Result<(), CanonicalStateError> {
        self.write_u8(u8::from(value))
    }

    /// Writes the raw IEEE 754 bits of a 32-bit float.
    pub fn write_f32(&mut self, value: f32) -> Result<(), CanonicalStateError> {
        self.write_u32(value.to_bits())
    }

    /// Writes the raw IEEE 754 bits of a 64-bit float.
    pub fn write_f64(&mut self, value: f64) -> Result<(), CanonicalStateError> {
        self.write_u64(value.to_bits())
    }

    /// Writes bytes without a length field. The section schema supplies the size.
    pub fn write_raw_bytes(&mut self, value: &[u8]) -> Result<(), CanonicalStateError> {
        self.append(value)
    }

    /// Writes a u64 byte count followed by the bytes.
    pub fn write_len_prefixed_bytes(&mut self, value: &[u8]) -> Result<(), CanonicalStateError> {
        let encoded_len =
            u64::try_from(value.len()).map_err(|_| CanonicalStateError::LengthOverflow)?;
        let additional = checked_add(8, value.len())?;
        reserve(self.bytes, additional)?;
        self.bytes.extend_from_slice(&encoded_len.to_le_bytes());
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    /// Writes a schema-owned u32 enum or variant tag.
    pub fn write_tag(&mut self, value: u32) -> Result<(), CanonicalStateError> {
        self.write_u32(value)
    }

    /// Writes a u64 collection count. Elements follow in schema-defined order.
    pub fn write_count(&mut self, value: u64) -> Result<(), CanonicalStateError> {
        self.write_u64(value)
    }
}

/// Builds a canonical, length-delimited state container in one byte vector.
///
/// The container version describes framing, not a complete machine-state schema.
/// A returned error from a section callback removes that section's bytes and
/// leaves ordering state unchanged. Panics are not part of this guarantee.
pub struct CanonicalStateWriter {
    bytes: Vec<u8>,
    section_count: u32,
    last_section: Option<u32>,
}

impl CanonicalStateWriter {
    pub fn new() -> Result<Self, CanonicalStateError> {
        let mut bytes = Vec::new();
        reserve(&mut bytes, HEADER_LEN)?;
        bytes.extend_from_slice(&CANONICAL_STATE_CONTAINER_MAGIC);
        bytes.extend_from_slice(&CANONICAL_STATE_CONTAINER_MAJOR.to_le_bytes());
        bytes.extend_from_slice(&CANONICAL_STATE_CONTAINER_MINOR.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        Ok(Self {
            bytes,
            section_count: 0,
            last_section: None,
        })
    }

    pub fn section<F>(
        &mut self,
        id: CanonicalSectionId,
        version: CanonicalSectionVersion,
        requirement: CanonicalSectionRequirement,
        write: F,
    ) -> Result<(), CanonicalStateError>
    where
        F: FnOnce(&mut CanonicalFieldWriter<'_>) -> Result<(), CanonicalStateError>,
    {
        let id = id.get();
        if let Some(previous) = self.last_section
            && id <= previous
        {
            return Err(CanonicalStateError::SectionOutOfOrder { previous, next: id });
        }
        let next_count = self
            .section_count
            .checked_add(1)
            .ok_or(CanonicalStateError::SectionCountOverflow)?;
        let section_start = self.bytes.len();
        let result = (|| {
            reserve(&mut self.bytes, SECTION_HEADER_LEN)?;
            self.bytes.extend_from_slice(&id.to_le_bytes());
            self.bytes.extend_from_slice(&version.get().to_le_bytes());
            self.bytes
                .extend_from_slice(&requirement.flags().to_le_bytes());
            let length_offset = self.bytes.len();
            self.bytes.extend_from_slice(&0u64.to_le_bytes());
            let payload_start = self.bytes.len();
            write(&mut CanonicalFieldWriter {
                bytes: &mut self.bytes,
            })?;
            let payload_len = self
                .bytes
                .len()
                .checked_sub(payload_start)
                .ok_or(CanonicalStateError::LengthOverflow)?;
            let payload_len =
                u64::try_from(payload_len).map_err(|_| CanonicalStateError::LengthOverflow)?;
            let length_end = checked_add(length_offset, 8)?;
            self.bytes[length_offset..length_end].copy_from_slice(&payload_len.to_le_bytes());
            Ok(())
        })();
        if let Err(error) = result {
            self.bytes.truncate(section_start);
            return Err(error);
        }
        self.section_count = next_count;
        self.last_section = Some(id);
        Ok(())
    }

    /// Finishes the container. The consumed writer cannot accept later sections.
    pub fn finish(mut self) -> Result<Vec<u8>, CanonicalStateError> {
        let count_end = checked_add(SECTION_COUNT_OFFSET, 4)?;
        self.bytes[SECTION_COUNT_OFFSET..count_end]
            .copy_from_slice(&self.section_count.to_le_bytes());
        CanonicalStateView::parse(&self.bytes)?;
        Ok(self.bytes)
    }
}

/// One borrowed section from a validated canonical container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalSection<'a> {
    id: CanonicalSectionId,
    version: CanonicalSectionVersion,
    requirement: CanonicalSectionRequirement,
    payload: &'a [u8],
}

impl<'a> CanonicalSection<'a> {
    pub const fn id(self) -> CanonicalSectionId {
        self.id
    }

    pub const fn version(self) -> CanonicalSectionVersion {
        self.version
    }

    pub const fn requirement(self) -> CanonicalSectionRequirement {
        self.requirement
    }

    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }
}

/// A framing-validated view of a canonical state container.
///
/// All minor versions with the supported major use the same framing rules.
/// A later schema validator decides whether required section IDs are known.
#[derive(Debug)]
pub struct CanonicalStateView<'a> {
    minor_version: u16,
    sections: Vec<CanonicalSection<'a>>,
}

impl<'a> CanonicalStateView<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CanonicalStateError> {
        if bytes.len() < HEADER_LEN {
            return Err(CanonicalStateError::TruncatedHeader);
        }
        if bytes[..8] != CANONICAL_STATE_CONTAINER_MAGIC {
            return Err(CanonicalStateError::InvalidMagic);
        }
        let major = read_u16(bytes, 8)?;
        if major != CANONICAL_STATE_CONTAINER_MAJOR {
            return Err(CanonicalStateError::UnsupportedMajorVersion { found: major });
        }
        let minor_version = read_u16(bytes, 10)?;
        let header_flags = read_u32(bytes, HEADER_FLAGS_OFFSET)?;
        if header_flags != 0 {
            return Err(CanonicalStateError::UnsupportedHeaderFlags {
                found: header_flags,
            });
        }
        let section_count = read_u32(bytes, SECTION_COUNT_OFFSET)?;
        let mut cursor = HEADER_LEN;
        let mut sections = Vec::new();
        let mut last_section = None;
        for index in 0..section_count {
            let header_end = checked_add(cursor, SECTION_HEADER_LEN)?;
            if header_end > bytes.len() {
                return Err(CanonicalStateError::TruncatedSectionHeader { index });
            }
            let raw_id = read_u32(bytes, cursor)?;
            let id =
                CanonicalSectionId::new(raw_id).ok_or(CanonicalStateError::ReservedSectionId)?;
            if let Some(previous) = last_section
                && raw_id <= previous
            {
                return Err(CanonicalStateError::SectionOutOfOrder {
                    previous,
                    next: raw_id,
                });
            }
            let raw_version = read_u16(bytes, cursor + 4)?;
            let version = CanonicalSectionVersion::new(raw_version)
                .ok_or(CanonicalStateError::ReservedSectionVersion { section_id: raw_id })?;
            let raw_flags = read_u16(bytes, cursor + 6)?;
            let requirement = CanonicalSectionRequirement::from_flags(raw_flags).ok_or(
                CanonicalStateError::UnsupportedSectionFlags {
                    section_id: raw_id,
                    found: raw_flags,
                },
            )?;
            let declared = read_u64(bytes, cursor + 8)?;
            let payload_len =
                usize::try_from(declared).map_err(|_| CanonicalStateError::LengthOverflow)?;
            let payload_start = header_end;
            let payload_end = payload_start
                .checked_add(payload_len)
                .ok_or(CanonicalStateError::LengthOverflow)?;
            if payload_end > bytes.len() {
                let remaining = u64::try_from(bytes.len().saturating_sub(payload_start))
                    .map_err(|_| CanonicalStateError::LengthOverflow)?;
                return Err(CanonicalStateError::TruncatedSectionPayload {
                    section_id: raw_id,
                    declared,
                    remaining,
                });
            }
            reserve(&mut sections, 1)?;
            sections.push(CanonicalSection {
                id,
                version,
                requirement,
                payload: &bytes[payload_start..payload_end],
            });
            cursor = payload_end;
            last_section = Some(raw_id);
        }
        if cursor != bytes.len() {
            return Err(CanonicalStateError::TrailingBytes {
                count: bytes.len() - cursor,
            });
        }
        Ok(Self {
            minor_version,
            sections,
        })
    }

    pub const fn minor_version(&self) -> u16 {
        self.minor_version
    }

    pub fn sections(&self) -> &[CanonicalSection<'a>] {
        &self.sections
    }
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), CanonicalStateError> {
    values
        .len()
        .checked_add(additional)
        .ok_or(CanonicalStateError::LengthOverflow)?;
    values
        .try_reserve(additional)
        .map_err(|_| CanonicalStateError::AllocationFailed)
}

fn checked_add(left: usize, right: usize) -> Result<usize, CanonicalStateError> {
    left.checked_add(right)
        .ok_or(CanonicalStateError::LengthOverflow)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, CanonicalStateError> {
    let end = checked_add(offset, 2)?;
    let raw = bytes
        .get(offset..end)
        .ok_or(CanonicalStateError::TruncatedHeader)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, CanonicalStateError> {
    let end = checked_add(offset, 4)?;
    let raw = bytes
        .get(offset..end)
        .ok_or(CanonicalStateError::TruncatedHeader)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, CanonicalStateError> {
    let end = checked_add(offset, 8)?;
    let raw = bytes
        .get(offset..end)
        .ok_or(CanonicalStateError::TruncatedHeader)?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

#[cfg(test)]
#[path = "canonical_state_test.rs"]
mod tests;
