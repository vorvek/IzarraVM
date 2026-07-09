// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn folds_name_and_extension_uppercased_and_padded() {
    let n = unique_name(Path::new("readme.txt"), false, &mut Vec::new());
    assert_eq!(&n, b"README  TXT");
}

#[test]
fn a_directory_name_with_a_dot_keeps_it_in_the_stem() {
    // A directory must not split a bogus extension off the dot.
    let n = unique_name(Path::new("my.dir"), true, &mut Vec::new());
    assert_eq!(&n, b"MYDIR      ");
}

#[test]
fn siblings_collide_into_tilde_suffixes() {
    let mut used = Vec::new();
    let a = unique_name(Path::new("longname.txt"), false, &mut used);
    let b = unique_name(Path::new("longname.txt"), false, &mut used);
    assert_eq!(&a, b"LONGNAMETXT");
    assert_eq!(&b, b"LONGNA~1TXT", "the second sibling gets a ~1 suffix");
    assert_ne!(a, b);
}

#[test]
fn illegal_characters_become_underscores() {
    let n = unique_name(Path::new("a+b=c.dat"), false, &mut Vec::new());
    assert_eq!(&n, b"A_B_C   DAT");
}
