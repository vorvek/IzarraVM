// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn section_id(value: u32) -> CanonicalSectionId {
    CanonicalSectionId::new(value).unwrap()
}

fn section_version(value: u16) -> CanonicalSectionVersion {
    CanonicalSectionVersion::new(value).unwrap()
}

fn golden_container() -> Vec<u8> {
    let mut writer = CanonicalStateWriter::new().unwrap();
    writer
        .section(
            section_id(1),
            section_version(1),
            CanonicalSectionRequirement::Required,
            |out| {
                out.write_u8(0x12)?;
                out.write_i8(-2)?;
                out.write_u16(0x3456)?;
                out.write_i16(-3)?;
                out.write_u32(0x789a_bcde)?;
                out.write_i32(-4)?;
                out.write_u64(0x0123_4567_89ab_cdef)?;
                out.write_i64(-5)?;
                out.write_bool(true)?;
                out.write_bool(false)?;
                out.write_f32(0.0)?;
                out.write_f32(-0.0)?;
                out.write_f32(f32::from_bits(0x7f80_0001))?;
                out.write_f64(f64::INFINITY)?;
                out.write_f64(f64::from_bits(0x7ff8_0000_0000_0042))?;
                out.write_raw_bytes(&[0xaa, 0xbb])?;
                out.write_len_prefixed_bytes(&[0xcc, 0xdd])?;
                out.write_tag(0x1020_3040)?;
                Ok(())
            },
        )
        .unwrap();
    writer
        .section(
            section_id(0x0001_0000),
            section_version(2),
            CanonicalSectionRequirement::Optional,
            |_| Ok(()),
        )
        .unwrap();
    writer.finish().unwrap()
}

#[test]
fn writer_has_exact_golden_bytes() {
    let payload = [
        0x12, 0xfe, 0x56, 0x34, 0xfd, 0xff, 0xde, 0xbc, 0x9a, 0x78, 0xfc, 0xff, 0xff, 0xff, 0xef,
        0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, 0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x01, 0x00, 0x80, 0x7f, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x7f, 0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, 0x7f,
        0xaa, 0xbb, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xcc, 0xdd, 0x40, 0x30, 0x20,
        0x10,
    ];
    assert_eq!(payload.len(), 76);
    let mut expected = Vec::new();
    expected.extend_from_slice(b"IZCSTATE");
    expected.extend_from_slice(&[1, 0, 0, 0]);
    expected.extend_from_slice(&[0, 0, 0, 0]);
    expected.extend_from_slice(&[2, 0, 0, 0]);
    expected.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
    expected.extend_from_slice(&[76, 0, 0, 0, 0, 0, 0, 0]);
    expected.extend_from_slice(&payload);
    expected.extend_from_slice(&[0, 0, 1, 0, 2, 0, 1, 0]);
    expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(golden_container(), expected);
}

#[test]
fn reader_borrows_sections_and_accepts_later_minor() {
    let mut bytes = golden_container();
    bytes[10..12].copy_from_slice(&37u16.to_le_bytes());
    let state = CanonicalStateView::parse(&bytes).unwrap();
    assert_eq!(state.minor_version(), 37);
    assert_eq!(state.sections().len(), 2);
    assert_eq!(state.sections()[0].id().get(), 1);
    assert_eq!(state.sections()[0].version().get(), 1);
    assert_eq!(
        state.sections()[0].requirement(),
        CanonicalSectionRequirement::Required
    );
    assert_eq!(state.sections()[0].payload().len(), 76);
    assert_eq!(state.sections()[1].id().get(), 0x0001_0000);
    assert_eq!(
        state.sections()[1].requirement(),
        CanonicalSectionRequirement::Optional
    );
    assert!(state.sections()[1].payload().is_empty());
}

#[test]
fn empty_container_is_valid() {
    let bytes = CanonicalStateWriter::new().unwrap().finish().unwrap();
    assert_eq!(
        bytes,
        [
            b'I', b'Z', b'C', b'S', b'T', b'A', b'T', b'E', 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    );
    assert!(
        CanonicalStateView::parse(&bytes)
            .unwrap()
            .sections()
            .is_empty()
    );
}

#[test]
fn returned_error_rolls_back_bytes_and_ordering() {
    let mut failed = CanonicalStateWriter::new().unwrap();
    failed
        .section(
            section_id(1),
            section_version(1),
            CanonicalSectionRequirement::Required,
            |out| out.write_u8(9),
        )
        .unwrap();
    let before = failed.bytes.clone();
    let error = failed
        .section(
            section_id(2),
            section_version(1),
            CanonicalSectionRequirement::Required,
            |out| {
                out.write_u8(10)?;
                Err(CanonicalStateError::LengthOverflow)
            },
        )
        .unwrap_err();
    assert_eq!(error, CanonicalStateError::LengthOverflow);
    assert_eq!(failed.bytes, before);
    failed
        .section(
            section_id(2),
            section_version(1),
            CanonicalSectionRequirement::Required,
            |out| out.write_u8(11),
        )
        .unwrap();

    let mut clean = CanonicalStateWriter::new().unwrap();
    clean
        .section(
            section_id(1),
            section_version(1),
            CanonicalSectionRequirement::Required,
            |out| out.write_u8(9),
        )
        .unwrap();
    clean
        .section(
            section_id(2),
            section_version(1),
            CanonicalSectionRequirement::Required,
            |out| out.write_u8(11),
        )
        .unwrap();
    assert_eq!(failed.finish().unwrap(), clean.finish().unwrap());
}

#[test]
fn writer_rejects_duplicate_and_descending_sections_before_writing() {
    for next in [2, 1] {
        let mut writer = CanonicalStateWriter::new().unwrap();
        writer
            .section(
                section_id(2),
                section_version(1),
                CanonicalSectionRequirement::Required,
                |_| Ok(()),
            )
            .unwrap();
        let before = writer.bytes.clone();
        assert_eq!(
            writer
                .section(
                    section_id(next),
                    section_version(1),
                    CanonicalSectionRequirement::Required,
                    |_| Ok(()),
                )
                .unwrap_err(),
            CanonicalStateError::SectionOutOfOrder { previous: 2, next }
        );
        assert_eq!(writer.bytes, before);
    }
    assert!(CanonicalSectionId::new(0).is_none());
    assert!(CanonicalSectionVersion::new(0).is_none());
}

#[test]
fn reader_rejects_bad_header_fields() {
    let bytes = golden_container();
    assert_eq!(
        CanonicalStateView::parse(&bytes[..HEADER_LEN - 1]).unwrap_err(),
        CanonicalStateError::TruncatedHeader
    );
    let mut bad = bytes.clone();
    bad[0] ^= 1;
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::InvalidMagic
    );
    let mut bad = bytes.clone();
    bad[8..10].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::UnsupportedMajorVersion { found: 2 }
    );
    let mut bad = bytes;
    bad[HEADER_FLAGS_OFFSET..HEADER_FLAGS_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::UnsupportedHeaderFlags { found: 1 }
    );
}

#[test]
fn reader_rejects_bad_section_fields() {
    let bytes = golden_container();
    let mut bad = bytes.clone();
    bad[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::ReservedSectionId
    );
    let mut bad = bytes.clone();
    bad[HEADER_LEN + 4..HEADER_LEN + 6].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::ReservedSectionVersion { section_id: 1 }
    );
    let mut bad = bytes.clone();
    bad[HEADER_LEN + 6..HEADER_LEN + 8].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::UnsupportedSectionFlags {
            section_id: 1,
            found: 2
        }
    );
    let second = HEADER_LEN + SECTION_HEADER_LEN + 76;
    let mut bad = bytes;
    bad[second..second + 4].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        CanonicalStateView::parse(&bad).unwrap_err(),
        CanonicalStateError::SectionOutOfOrder {
            previous: 1,
            next: 1
        }
    );
}

#[test]
fn reader_rejects_truncation_count_mismatch_and_trailing_bytes() {
    let bytes = golden_container();
    let mut truncated_header = bytes[..HEADER_LEN + SECTION_HEADER_LEN + 76 + 15].to_vec();
    assert_eq!(
        CanonicalStateView::parse(&truncated_header).unwrap_err(),
        CanonicalStateError::TruncatedSectionHeader { index: 1 }
    );

    let mut truncated_payload = bytes.clone();
    truncated_payload.truncate(HEADER_LEN + SECTION_HEADER_LEN + 75);
    assert_eq!(
        CanonicalStateView::parse(&truncated_payload).unwrap_err(),
        CanonicalStateError::TruncatedSectionPayload {
            section_id: 1,
            declared: 76,
            remaining: 75
        }
    );

    truncated_header[SECTION_COUNT_OFFSET..SECTION_COUNT_OFFSET + 4]
        .copy_from_slice(&3u32.to_le_bytes());
    assert!(matches!(
        CanonicalStateView::parse(&truncated_header),
        Err(CanonicalStateError::TruncatedSectionHeader { .. })
    ));

    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        CanonicalStateView::parse(&trailing).unwrap_err(),
        CanonicalStateError::TrailingBytes { count: 1 }
    );
}

#[test]
fn checked_growth_failures_are_reported() {
    assert_eq!(
        checked_add(usize::MAX, 1),
        Err(CanonicalStateError::LengthOverflow)
    );
    let mut bytes = Vec::<u8>::new();
    assert_eq!(
        reserve(&mut bytes, usize::MAX),
        Err(CanonicalStateError::AllocationFailed)
    );
    bytes.push(0);
    assert_eq!(
        reserve(&mut bytes, usize::MAX),
        Err(CanonicalStateError::LengthOverflow)
    );
}
