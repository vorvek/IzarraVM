// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::path::PathBuf;

#[test]
fn folds_files_and_dirs_and_resolves_collisions_and_reverse_lookup() {
    let mut t = NameTable::new();
    // Reserve a system name so a host file can't alias it.
    t.reserve(*b"KERNEL  SYS");
    let a = t.add_host(&PathBuf::from("/h/Readme.txt"), false);
    let b = t.add_host(&PathBuf::from("/h/readme.txt"), false); // collides
    let d = t.add_host(&PathBuf::from("/h/My.Games"), true); // a directory
    assert_eq!(&a, b"README  TXT");
    assert_eq!(&b, b"README~1TXT");
    assert_eq!(&d, b"MYGAMES    ");
    // Reverse: the folded name maps back to the host path (for M2 writes).
    assert_eq!(t.host_path(&a), Some(&PathBuf::from("/h/Readme.txt")));
    assert_eq!(t.host_path(b"KERNEL  SYS"), None); // a reserved name has no host path
}
