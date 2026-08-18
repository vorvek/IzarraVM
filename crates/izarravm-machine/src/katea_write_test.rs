// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::fat32::{FAT32_EOC, fat32_dir_entry};
use std::collections::HashSet;

fn name83(s: &str) -> [u8; 11] {
    let mut n = [b' '; 11];
    let (b, x) = s.split_once('.').unwrap_or((s, ""));
    n[..b.len()].copy_from_slice(b.as_bytes());
    n[8..8 + x.len()].copy_from_slice(x.as_bytes());
    n
}

#[test]
fn parse_dir_reads_entries_and_stops_at_free() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&fat32_dir_entry(&name83("A.TXT"), 0x20, 7, 0, 0, 123));
    bytes.extend_from_slice(&fat32_dir_entry(&name83("SUB"), 0x10, 9, 0, 0, 0));
    bytes.extend_from_slice(&[0u8; 32]); // free -> stop
    bytes.extend_from_slice(&fat32_dir_entry(&name83("Z.TXT"), 0x20, 99, 0, 0, 1));
    let es = parse_dir(&bytes);
    assert_eq!(es.len(), 2, "stops at the free entry");
    assert_eq!(es[0].name, name83("A.TXT"));
    assert_eq!(es[0].first_cluster, 7);
    assert_eq!(es[0].size, 123);
    assert_eq!(es[1].first_cluster, 9);
    assert_eq!(es[1].attr & 0x10, 0x10);
}

#[test]
fn classify_skips_dots_lfn_volume_and_system_files() {
    let mut sys = HashSet::new();
    sys.insert(name83("KERNEL.SYS"));
    let mk = |n: &str, attr: u8, fc: u32, sz: u32| DirEntry {
        name: name83(n),
        attr,
        first_cluster: fc,
        size: sz,
    };
    // A real FAT `.` / `..` entry has name[0] == 0x2E (the test helper's
    // `name83(".")` would yield an all-spaces name, which is NOT what reconcile
    // sees from live directory clusters), so build the dot bytes explicitly.
    let dot = DirEntry {
        name: {
            let mut n = [b' '; 11];
            n[0] = b'.';
            n
        },
        attr: 0x10,
        first_cluster: 2,
        size: 0,
    };
    let dotdot = DirEntry {
        name: {
            let mut n = [b' '; 11];
            n[0] = b'.';
            n[1] = b'.';
            n
        },
        attr: 0x10,
        first_cluster: 2,
        size: 0,
    };
    assert_eq!(classify(&dot, &sys), EntryAction::Skip, "real `.` entry");
    assert_eq!(
        classify(&dotdot, &sys),
        EntryAction::Skip,
        "real `..` entry"
    );
    // A blank first byte (a malformed/empty slot) is skipped defensively.
    assert_eq!(
        classify(&mk(" ", 0x20, 9, 1), &sys),
        EntryAction::Skip,
        "blank name"
    );
    assert_eq!(classify(&mk("X", 0x0F, 0, 0), &sys), EntryAction::Skip); // LFN
    assert_eq!(classify(&mk("LABEL", 0x08, 0, 0), &sys), EntryAction::Skip); // vol label
    assert_eq!(
        classify(&mk("KERNEL.SYS", 0x20, 3, 9), &sys),
        EntryAction::Skip
    );
    assert_eq!(
        classify(&mk("GAMES", 0x10, 5, 0), &sys),
        EntryAction::MakeDir {
            name: name83("GAMES"),
            first_cluster: 5
        }
    );
    assert_eq!(
        classify(&mk("NEW.TXT", 0x20, 7, 11), &sys),
        EntryAction::MakeFile {
            name: name83("NEW.TXT"),
            first_cluster: 7,
            size: 11
        }
    );
}

#[test]
fn chain_follows_to_eoc_and_holds_on_break() {
    // 5 -> 6 -> EOC.
    let fat = |c: u32| match c {
        5 => 6,
        6 => FAT32_EOC,
        _ => 0,
    };
    assert_eq!(chain(5, 16, fat), Some(vec![5, 6]));
    // A free entry mid-chain -> hold (None).
    let broken = |c: u32| if c == 5 { 0 } else { FAT32_EOC };
    assert_eq!(chain(5, 16, broken), None);
    // first < 2 -> empty chain (empty file).
    assert_eq!(chain(0, 16, fat), Some(Vec::new()));
    // Non-terminating -> hold.
    let loopy = |_c: u32| 5u32;
    assert_eq!(chain(5, 8, loopy), None);
}

#[test]
fn fingerprint_differs_on_same_length_content_change() {
    assert_ne!(fingerprint(b"line1\r\n"), fingerprint(b"line2\r\n"));
    assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
}

#[test]
fn atomic_write_replaces_and_leaves_no_temp() {
    let dir = std::env::temp_dir().join(format!("katea_aw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("OUT.TXT");
    std::fs::write(&target, b"old").unwrap();
    atomic_write(&target, b"new content").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new content");
    // No stray temp file remains.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("kattmp"))
        .collect();
    assert!(leftovers.is_empty(), "no .kattmp left behind");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_guest_name_that_would_escape_the_mount_is_skipped() {
    let system = HashSet::new();
    // `A\..\..\` + `BAT` joins to a path two levels above the mounted folder.
    let escape = DirEntry {
        name: *b"A\\..\\..\\BAT",
        attr: 0x20,
        first_cluster: 5,
        size: 8,
    };
    assert_eq!(classify(&escape, &system), EntryAction::Skip);
    // Forward slashes escape just as well, and a directory is no different.
    let escape_dir = DirEntry {
        name: *b"../../..   ",
        attr: 0x10,
        first_cluster: 5,
        size: 0,
    };
    assert_eq!(classify(&escape_dir, &system), EntryAction::Skip);
    let embedded_dot = DirEntry {
        name: *b"A.B     TXT",
        attr: 0x20,
        first_cluster: 5,
        size: 8,
    };
    assert_eq!(classify(&embedded_dot, &system), EntryAction::Skip);
    // Every byte fat_name can synthesize still classifies normally.
    let ordinary = DirEntry {
        name: *b"A-_~{}!#TXT",
        attr: 0x20,
        first_cluster: 5,
        size: 8,
    };
    assert_eq!(
        classify(&ordinary, &system),
        EntryAction::MakeFile {
            name: *b"A-_~{}!#TXT",
            first_cluster: 5,
            size: 8,
        }
    );
}
