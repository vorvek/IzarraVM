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
fn extraction_is_content_addressed_and_repairs_a_corrupt_cache() {
    let root = tempfile::tempdir().unwrap();
    let path = materialize_embedded_soundfont_in(root.path()).unwrap();

    assert_eq!(path.file_name().unwrap(), FILE_NAME);
    assert!(file_matches(&path, EMBEDDED_SOUNDFONT).unwrap());
    fs::write(&path, b"corrupt").unwrap();

    let repaired = materialize_embedded_soundfont_in(root.path()).unwrap();
    assert_eq!(repaired, path);
    assert!(file_matches(&repaired, EMBEDDED_SOUNDFONT).unwrap());
}
