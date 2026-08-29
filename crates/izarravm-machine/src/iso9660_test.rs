// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::cdimage::CdImage;
use std::fs;

/// Build a small tree: root/a.txt, root/sub/b.txt, root/sub/nested/c.txt.
fn tiny_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"hello from a").unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("b.txt"), b"contents of b, a bit longer this time").unwrap();
    let nested = sub.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("c.txt"), vec![0x7Au8; 3000]).unwrap(); // spans a sector
    dir
}

#[test]
fn pvd_parses_at_lba_16() {
    let dir = tiny_tree();
    let built = build(dir.path()).unwrap();
    let pvd_off = 16 * SECTOR;
    assert_eq!(built.meta[pvd_off], 0x01);
    assert_eq!(&built.meta[pvd_off + 1..pvd_off + 6], b"CD001");
}

#[test]
fn terminator_parses_at_lba_17() {
    let dir = tiny_tree();
    let built = build(dir.path()).unwrap();
    let off = 17 * SECTOR;
    assert_eq!(built.meta[off], 0xFF);
    assert_eq!(&built.meta[off + 1..off + 6], b"CD001");
}

#[test]
fn root_directory_lists_the_top_level_entries() {
    let dir = tiny_tree();
    let built = build(dir.path()).unwrap();
    let pvd_off = 16 * SECTOR;
    let root_len = usize::from(built.meta[pvd_off + 156]);
    let root_record = &built.meta[pvd_off + 156..pvd_off + 156 + root_len];
    let root_lba = u32::from_le_bytes(root_record[2..6].try_into().unwrap());

    let sector = &built.meta[root_lba as usize * SECTOR..(root_lba as usize + 1) * SECTOR];
    // Walk the directory records byte-exactly against the documented
    // layout: self (name [0]), parent (name [1]), then children.
    let mut offset = 0usize;
    let mut names = Vec::new();
    while offset < sector.len() {
        let len = usize::from(sector[offset]);
        if len == 0 {
            break;
        }
        let name_len = usize::from(sector[offset + 32]);
        let name = &sector[offset + 33..offset + 33 + name_len];
        names.push(name.to_vec());
        offset += len;
    }
    assert_eq!(names[0], vec![0u8]);
    assert_eq!(names[1], vec![1u8]);
    let rest: Vec<String> = names[2..]
        .iter()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    assert!(rest.contains(&"A.TXT;1".to_string()), "{rest:?}");
    assert!(rest.contains(&"SUB".to_string()), "{rest:?}");
}

#[test]
fn file_extents_are_ordered_and_after_metadata() {
    let dir = tiny_tree();
    let built = build(dir.path()).unwrap();
    let meta_sectors = (built.meta.len() / SECTOR) as u32;
    for extent in &built.extents {
        assert!(
            extent.start_lba >= meta_sectors,
            "file extent must start after the metadata region"
        );
    }
    // total_sectors covers metadata plus every extent.
    let last_extent_end = built
        .extents
        .iter()
        .map(|e| e.start_lba + e.sectors)
        .max()
        .unwrap_or(meta_sectors);
    assert_eq!(built.total_sectors, last_extent_end);
}

#[test]
fn a_small_folder_is_well_under_the_capacity_guard() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("x.bin"), b"tiny").unwrap();
    assert!(build(dir.path()).is_ok());
}

#[test]
fn refuses_a_folder_over_the_650mb_capacity_guard() {
    // A single sparse file just over the cap is enough to trip dir_size's
    // total without actually writing 650MB of content to disk.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.bin");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
    drop(file);
    let err = build(dir.path()).expect_err("over-capacity folder must be refused");
    assert!(err.contains("650"), "{err}");
}

#[test]
fn a_file_whose_content_round_trips_through_cdimage_read_data_sector() {
    let dir = tiny_tree();
    let built = build(dir.path()).unwrap();
    let image = CdImage::from_folder(built).unwrap();

    // Find the extent for c.txt (3000 bytes, spans a sector boundary).
    let c_path = dir.path().join("sub").join("nested").join("c.txt");
    let bytes = fs::read(&c_path).unwrap();
    assert_eq!(bytes.len(), 3000);

    // Locate c.txt's extent by re-walking the directory tree through the
    // image's own data sectors (root -> SUB -> NESTED -> C.TXT), proving
    // the metadata and the lazy file backing agree.
    let root_lba = root_lba_of(&image);
    let sub_record = find_child(&image, root_lba, b"SUB").expect("SUB not found");
    let sub_lba = u32::from_le_bytes(sub_record[2..6].try_into().unwrap());
    let nested_record = find_child(&image, sub_lba, b"NESTED").expect("NESTED not found");
    let nested_lba = u32::from_le_bytes(nested_record[2..6].try_into().unwrap());
    let file_record = find_child(&image, nested_lba, b"C.TXT;1").expect("C.TXT;1 not found");
    let file_lba = u32::from_le_bytes(file_record[2..6].try_into().unwrap());
    let file_len = u32::from_le_bytes(file_record[10..14].try_into().unwrap());
    assert_eq!(file_len, 3000);

    // Sector 0 of the file: bytes 0..2048.
    let sector0 = image.read_data_sector(file_lba).unwrap();
    assert_eq!(&sector0[..], &bytes[0..2048]);
    // Sector 1: bytes 2048..3000, zero-padded tail.
    let sector1 = image.read_data_sector(file_lba + 1).unwrap();
    assert_eq!(&sector1[..3000 - 2048], &bytes[2048..3000]);
    assert!(sector1[3000 - 2048..].iter().all(|&b| b == 0));
}

fn root_lba_of(image: &CdImage) -> u32 {
    let pvd = image.read_data_sector(16).unwrap();
    u32::from_le_bytes(pvd[156 + 2..156 + 6].try_into().unwrap())
}

/// Walk one directory's sector(s) looking for a child record by exact
/// name-field match. Mirrors `icdex_iso_child_record`'s byte layout.
fn find_child(image: &CdImage, dir_lba: u32, wanted: &[u8]) -> Option<Vec<u8>> {
    for sector_index in 0..4u32 {
        let Some(sector) = image.read_data_sector(dir_lba + sector_index) else {
            break;
        };
        let mut offset = 0usize;
        while offset < sector.len() {
            let len = usize::from(sector[offset]);
            if len == 0 {
                break;
            }
            let name_len = usize::from(sector[offset + 32]);
            let name = &sector[offset + 33..offset + 33 + name_len];
            if name == wanted {
                return Some(sector[offset..offset + len].to_vec());
            }
            offset += len;
        }
    }
    None
}

/// Build a tree of two multi-sector files, so a sequential read crosses an
/// extent boundary from one host file into the next. `build` orders extents by
/// LBA, and the byte patterns differ per file, so a read that landed in the
/// wrong extent shows up as wrong CONTENT and not merely as a wrong open count.
fn two_file_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // 8 sectors each. The pattern is a function of the byte's index within its
    // own file, so no two positions in either file share a value by accident.
    let first: Vec<u8> = (0..8 * 2048).map(|i| (i % 251) as u8).collect();
    let second: Vec<u8> = (0..8 * 2048)
        .map(|i| ((i % 241) as u8).wrapping_add(3))
        .collect();
    fs::write(dir.path().join("first.bin"), &first).unwrap();
    fs::write(dir.path().join("second.bin"), &second).unwrap();
    dir
}

/// A folder mount used to pay a host `File::open`, a seek and a `Vec`
/// allocation for EVERY 2048-byte sector, so a game streaming from a
/// folder-mounted disc opened the same file thousands of times a second. The
/// HDD path solved this with a handle LRU; this is the same fix one device over.
///
/// The cache is invisible to a value comparison — the sectors are byte-identical
/// with it and without it — so the open COUNT is the only thing that can show it
/// works. A count on its own is not evidence either: "K sequential sectors cost
/// one open" reads the same whether the cache works or the read path never ran.
/// So this takes the count TWICE from one binary, one flag apart, and the
/// uncached arm is what gives the cached arm meaning.
///
/// It also asserts the BYTES, over a range that crosses from one host file into
/// the next. An open count alone would be satisfied by a cache that serves the
/// wrong sector from the right handle.
#[test]
fn a_folder_mount_opens_each_host_file_once_not_once_per_sector() {
    let dir = two_file_tree();
    let sectors: Vec<u32> = {
        let built = build(dir.path()).unwrap();
        let meta_sectors = (built.meta.len() / SECTOR) as u32;
        // Every file sector on the disc, in LBA order, crossing the boundary
        // between the two extents partway through.
        (meta_sectors..built.total_sectors).collect()
    };
    assert!(
        sectors.len() >= 16,
        "expected both 8-sector files on the disc, got {} sectors",
        sectors.len()
    );

    // UNCACHED ARM: the behaviour before this fix. One open per sector.
    let uncached = CdImage::from_folder(build(dir.path()).unwrap()).unwrap();
    assert!(
        uncached.disable_read_cache_for_test(),
        "a folder mount must have a cache to disable"
    );
    let mut uncached_bytes = Vec::new();
    for &lba in &sectors {
        uncached_bytes.push(uncached.read_data_sector(lba).unwrap());
    }
    let uncached_opens = uncached.host_file_opens().unwrap();
    assert_eq!(
        uncached_opens,
        sectors.len() as u64,
        "the uncached arm must open once per sector; without this number the \
         cached arm below proves nothing"
    );

    // CACHED ARM: same reads, same binary, cache on.
    let cached = CdImage::from_folder(build(dir.path()).unwrap()).unwrap();
    let mut cached_bytes = Vec::new();
    for &lba in &sectors {
        cached_bytes.push(cached.read_data_sector(lba).unwrap());
    }
    let cached_opens = cached.host_file_opens().unwrap();
    assert_eq!(
        cached_opens, 2,
        "two host files, read in LBA order, must cost exactly two opens \
         (uncached arm took {uncached_opens})"
    );

    // And the bytes must be identical across the boundary, or the count above
    // is measuring a cache that serves the wrong sector cheaply.
    assert_eq!(
        cached_bytes, uncached_bytes,
        "cached and uncached reads must return identical sectors"
    );

    // Independently of both arms: the content must match the host files, so a
    // cache that returned the same wrong bytes twice cannot pass.
    let first = fs::read(dir.path().join("first.bin")).unwrap();
    let second = fs::read(dir.path().join("second.bin")).unwrap();
    let flat: Vec<u8> = cached_bytes.concat();
    let first_at = flat
        .windows(first.len())
        .position(|w| w == first.as_slice())
        .expect("first.bin's bytes must appear in the disc's file sectors");
    let second_at = flat
        .windows(second.len())
        .position(|w| w == second.as_slice())
        .expect("second.bin's bytes must appear in the disc's file sectors");
    assert_ne!(
        first_at, second_at,
        "the two files must land at different offsets"
    );
}

/// The LRU has to evict, and an evicted path has to be re-openable. With more
/// files in flight than slots, a strictly round-robin access pattern is the
/// worst case: every read misses. That is allowed to be slow, but it must still
/// be CORRECT, and the count must reflect the misses rather than hiding them.
#[test]
fn a_folder_mount_reopens_a_path_the_lru_evicted() {
    let dir = tempfile::tempdir().unwrap();
    // Six files against four slots.
    for index in 0..6u8 {
        let body: Vec<u8> = (0..2048).map(|i| (i as u8).wrapping_add(index)).collect();
        fs::write(dir.path().join(format!("f{index}.bin")), &body).unwrap();
    }
    let built = build(dir.path()).unwrap();
    let meta_sectors = (built.meta.len() / SECTOR) as u32;
    let total = built.total_sectors;
    let image = CdImage::from_folder(built).unwrap();

    // Round-robin over every file sector, three passes.
    let mut first_pass = Vec::new();
    for pass in 0..3 {
        for lba in meta_sectors..total {
            let sector = image.read_data_sector(lba).unwrap();
            if pass == 0 {
                first_pass.push(sector);
            } else {
                let index = (lba - meta_sectors) as usize;
                assert_eq!(
                    sector, first_pass[index],
                    "sector {lba} changed on pass {pass}: an evicted handle was \
                     re-opened at the wrong offset"
                );
            }
        }
    }
    assert!(
        image.host_file_opens().unwrap() >= 6,
        "six distinct files must have been opened at least once each"
    );
}
