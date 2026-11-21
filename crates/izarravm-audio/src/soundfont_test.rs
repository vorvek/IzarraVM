// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn embedded_bank_has_the_vendored_identity() {
    assert_eq!(EMBEDDED_SOUNDFONT.len(), 14_563_174);
    assert_eq!(&EMBEDDED_SOUNDFONT[..4], b"RIFF");
    assert_eq!(&EMBEDDED_SOUNDFONT[8..12], b"sfbk");
    assert_eq!(EMBEDDED_SOUNDFONT_SHA256.len(), 64);
}

#[test]
fn cache_root_prefers_the_configured_folder_over_temp() {
    let temp = Path::new("tmp");
    let configured = Path::new("state").join("soundfonts");
    assert_eq!(cache_root(Some(&configured), temp), configured);
    assert_eq!(
        cache_root(None, temp),
        temp.join("izarravm").join("soundfonts")
    );
}

#[test]
fn an_unwritable_folder_is_an_error_the_caller_can_fall_back_from() {
    let root = tempfile::tempdir().unwrap();
    let blocker = root.path().join("file");
    fs::write(&blocker, b"x").unwrap();
    assert!(materialize_embedded_soundfont_in(&blocker.join("soundfonts")).is_err());
}

#[test]
fn extraction_is_content_addressed_and_repairs_a_corrupt_cache() {
    let root = tempfile::tempdir().unwrap();
    let path = materialize_embedded_soundfont_in(&root.path().join("soundfonts")).unwrap();

    assert_eq!(path.file_name().unwrap(), FILE_NAME);
    assert!(file_matches(&path, EMBEDDED_SOUNDFONT).unwrap());
    fs::write(&path, b"corrupt").unwrap();

    let repaired = materialize_embedded_soundfont_in(&root.path().join("soundfonts")).unwrap();
    assert_eq!(repaired, path);
    assert!(file_matches(&repaired, EMBEDDED_SOUNDFONT).unwrap());
}

#[test]
fn a_reader_longer_than_the_expected_bytes_does_not_match_and_does_not_panic() {
    // A file that grows after the length check yields more bytes than expected.
    let expected = vec![7u8; 70_000];
    let mut longer = expected.clone();
    longer.extend_from_slice(&[1, 2, 3]);
    assert!(!reader_matches(std::io::Cursor::new(longer), &expected).unwrap());
    assert!(reader_matches(std::io::Cursor::new(expected.clone()), &expected).unwrap());
    let shorter = expected[..69_999].to_vec();
    assert!(!reader_matches(std::io::Cursor::new(shorter), &expected).unwrap());
}
