// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Build an in-memory ISO9660 metadata image from a host folder.
//!
//! This mirrors the read-only "synthesize a filesystem from a folder" approach
//! `fat12.rs` used for the floppy (now retired) and `fat32.rs` still uses for
//! the hard disk, but targets ISO9660 (ISO level 1: 8.3-ish `NAME;1` short
//! names, no Rock Ridge, no Joliet, no boot catalog) so it can be mounted as a
//! CD-ROM through the existing `CdImage`/ATAPI path.
//!
//! Only the metadata (PVD, path tables, directory records) is materialized in
//! memory. File contents are **not** copied in: [`build`] returns the metadata
//! bytes plus an ordered list of `(start_lba, sector_count, host_path, byte_len)`
//! extents that [`crate::cdimage::CdImage`] reads lazily from the host file.
//!
//! Directory records use the same field layout the existing IZCDEX reader in
//! `lib.rs` already parses (`icdex_iso_child_record`, `icdex_iso_dir_record`):
//! LE u32 extent LBA at offset 2, LE u32 size at offset 10, a 7-byte recording
//! date/time at offset 18, flags at offset 25, LE/BE volume-sequence words at
//! 28/30, name length at offset 32, name bytes from offset 33.

use crate::fat_name;
use std::fs;
use std::path::{Path, PathBuf};

/// Bytes in one logical sector (matches `cdimage::DATA_SECTOR`).
const SECTOR: usize = 2048;
/// Maximum disc capacity this builder will synthesize: a standard 74-minute
/// (~650 MB) CD-ROM. Folders larger than this are refused with a friendly
/// error rather than silently truncated.
pub const MAX_IMAGE_BYTES: u64 = 650 * 1024 * 1024;

const ATTR_DIRECTORY: u8 = 0x02;

/// One file's extent in the synthesized image: the LBA range it occupies and
/// where to read its bytes from on the host.
#[derive(Debug, Clone)]
pub struct FileExtent {
    pub start_lba: u32,
    pub sectors: u32,
    pub host_path: PathBuf,
    pub len: u64,
}

/// The result of building a folder into ISO9660 metadata: the sector-aligned
/// metadata image (PVD, path tables, directory records, terminator) and the
/// ordered file extents that sit right after it.
#[derive(Debug)]
pub struct BuiltImage {
    /// Metadata sectors only (LBA 0..meta_sectors), sized to a whole number of
    /// 2048-byte sectors.
    pub meta: Vec<u8>,
    /// File extents, sorted by `start_lba`, immediately following the metadata.
    pub extents: Vec<FileExtent>,
    /// Total disc sectors (metadata + all file extents).
    pub total_sectors: u32,
}

/// One child discovered while walking a host directory, before its extent LBA
/// is known.
enum Child {
    Dir {
        name: Vec<u8>,
        node_index: usize,
    },
    File {
        name: Vec<u8>,
        host_path: PathBuf,
        len: u64,
    },
}

/// Build ISO9660 metadata for `root`. Returns an error if the folder's total
/// content exceeds [`MAX_IMAGE_BYTES`]; files or directories whose name cannot
/// be folded into a unique ISO level-1 short name are skipped (logged to
/// stderr, matching the old FAT12 folder builder's convention).
pub fn build(root: &Path) -> Result<BuiltImage, String> {
    let total_len = dir_size(root).map_err(|e| format!("scanning {}: {e}", root.display()))?;
    if total_len > MAX_IMAGE_BYTES {
        return Err(format!(
            "folder is {:.1} MB, over the {:.0} MB CD-ROM capacity",
            total_len as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES as f64 / (1024.0 * 1024.0)
        ));
    }

    // Walk the tree, allocating one `nodes` slot per directory (root first,
    // index 0) and collecting each directory's children.
    let mut nodes: Vec<Vec<Child>> = Vec::new();
    nodes.push(Vec::new());
    walk_dir(root, 0, &mut nodes)?;

    // Layout: PVD (1 sector, LBA 16) + volume descriptor set terminator (1
    // sector, LBA 17) + little-endian path table + big-endian path table
    // (each rounded to a sector) + one extent per directory, then files.
    let pvd_lba = 16u32;
    let term_lba = 17u32;
    let mut cursor = 18u32;

    let path_table_le_lba = cursor;
    let path_table_bytes = build_path_table_bytes(&nodes);
    let path_table_sectors = sectors_for(path_table_bytes.len());
    cursor += path_table_sectors;
    let path_table_be_lba = cursor;
    cursor += path_table_sectors;

    // Assign each directory's own extent LBA and sector count. Directory sizes
    // depend on the records they contain, which depend on children's assigned
    // LBAs (for the directory records naming subdirectories) and lengths (for
    // files) -- but not on grandchildren's LBAs, so a single pass in node
    // index order (parents before children, since a child node index is
    // always allocated after its parent starts walking) works as long as we
    // assign directory LBAs before formatting any directory's record bytes.
    let mut dir_lba = vec![0u32; nodes.len()];
    let mut dir_sectors = vec![0u32; nodes.len()];
    for (i, children) in nodes.iter().enumerate() {
        // A directory's own extent holds "." + ".." + one record per child.
        let mut size = dir_record_len(1) * 2; // "." and ".."
        for child in children {
            let name_len = match child {
                Child::Dir { name, .. } => name.len(),
                Child::File { name, .. } => name.len(),
            };
            size += dir_record_len(name_len);
        }
        let sectors = sectors_for(size);
        dir_lba[i] = cursor;
        dir_sectors[i] = sectors;
        cursor += sectors;
    }

    // Now assign file extents, contiguous after all directory extents. Each
    // directory's file children get a parallel `file_lbas[i]` entry (0 for a
    // Dir child, unused) so the record-formatting pass below can look a
    // file's resolved LBA up by position without re-deriving it.
    let mut extents: Vec<FileExtent> = Vec::new();
    let mut file_lbas: Vec<Vec<u32>> = Vec::with_capacity(nodes.len());
    for children in &nodes {
        let mut lbas = Vec::with_capacity(children.len());
        for child in children {
            match child {
                Child::File { host_path, len, .. } => {
                    let sectors = sectors_for(*len as usize).max(1);
                    lbas.push(cursor);
                    extents.push(FileExtent {
                        start_lba: cursor,
                        sectors,
                        host_path: host_path.clone(),
                        len: *len,
                    });
                    cursor += sectors;
                }
                Child::Dir { .. } => lbas.push(0),
            }
        }
        file_lbas.push(lbas);
    }

    let total_sectors = cursor;

    // Format each directory's record bytes now that every LBA is known. Each
    // directory needs its parent's LBA/size for the ".." entry.
    let parent_of = parent_indices(&nodes);

    let mut dir_records: Vec<Vec<u8>> = Vec::with_capacity(nodes.len());
    for i in 0..nodes.len() {
        let self_lba = dir_lba[i];
        let self_len = (dir_sectors[i] as usize) * SECTOR;
        let parent_lba = dir_lba[parent_of[i]];
        let parent_len = (dir_sectors[parent_of[i]] as usize) * SECTOR;

        let mut buf = Vec::new();
        buf.extend(dir_record(self_lba, self_len as u32, ATTR_DIRECTORY, &[0]));
        buf.extend(dir_record(
            parent_lba,
            parent_len as u32,
            ATTR_DIRECTORY,
            &[1],
        ));
        for (child, &lba) in nodes[i].iter().zip(file_lbas[i].iter()) {
            match child {
                Child::Dir { name, node_index } => {
                    let clba = dir_lba[*node_index];
                    let clen = (dir_sectors[*node_index] as usize) * SECTOR;
                    buf.extend(dir_record(clba, clen as u32, ATTR_DIRECTORY, name));
                }
                Child::File { name, len, .. } => {
                    buf.extend(dir_record(lba, *len as u32, 0x00, name));
                }
            }
        }
        // Pad the directory extent to a whole number of sectors; directory
        // records never span a sector boundary in ISO9660, but this simple
        // builder never lets one get close enough for that to matter given
        // each entry is well under 2048 bytes.
        buf.resize((dir_sectors[i] as usize) * SECTOR, 0);
        dir_records.push(buf);
    }

    // Assemble the metadata image: LBA 0 through the end of the last
    // directory's extent (every file extent starts right after this).
    let last_dir = nodes.len() - 1;
    let meta_len_sectors = dir_lba[last_dir] + dir_sectors[last_dir];
    let mut meta = vec![0u8; (meta_len_sectors as usize) * SECTOR];

    // Path tables (built once we know directory LBAs).
    let path_le = build_path_table(&nodes, &dir_lba, &parent_of, true);
    let path_be = build_path_table(&nodes, &dir_lba, &parent_of, false);
    write_at(&mut meta, path_table_le_lba, &path_le);
    write_at(&mut meta, path_table_be_lba, &path_be);

    for i in 0..nodes.len() {
        write_at(&mut meta, dir_lba[i], &dir_records[i]);
    }

    let root_record = dir_record(
        dir_lba[0],
        (dir_sectors[0] as usize * SECTOR) as u32,
        ATTR_DIRECTORY,
        &[0],
    );
    let pvd = build_pvd(
        total_sectors,
        &root_record,
        path_table_bytes.len() as u32,
        path_table_le_lba,
        path_table_be_lba,
    );
    write_at(&mut meta, pvd_lba, &pvd);
    write_at(&mut meta, term_lba, &build_terminator());

    Ok(BuiltImage {
        meta,
        extents,
        total_sectors,
    })
}

/// Recursively sum the byte length of every regular file under `dir`.
fn dir_size(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path())?;
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

/// Walk one host directory into `nodes[node_index]`, recursing into
/// subdirectories (each gets its own freshly pushed `nodes` slot). Entries
/// whose name cannot be uniquely folded to an ISO level-1 short name, or that
/// fail to read, are skipped with a log line.
fn walk_dir(dir: &Path, node_index: usize, nodes: &mut Vec<Vec<Child>>) -> Result<(), String> {
    let read = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    let mut children: Vec<_> = read.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    children.sort();

    let mut used_names: Vec<[u8; 11]> = Vec::new();
    let mut staged: Vec<Child> = Vec::new();
    // Subdirectories discovered here are queued and walked after this
    // directory's own child list is finalized, so sibling ordering in
    // `nodes[node_index]` stays stable regardless of recursion.
    let mut pending_dirs: Vec<(usize, PathBuf)> = Vec::new();

    for path in children {
        let raw = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if raw.is_empty() {
            continue;
        }
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("iso9660: skipping {}: {e}", path.display());
                continue;
            }
        };

        if meta.is_dir() {
            let short = fat_name::unique_name(&path, true, &mut used_names);
            let Some(name) = iso_name_from_83(&short, true) else {
                eprintln!(
                    "iso9660: skipping directory {} (name cannot be folded uniquely)",
                    path.display()
                );
                continue;
            };
            let child_index = nodes.len();
            nodes.push(Vec::new());
            pending_dirs.push((child_index, path.clone()));
            staged.push(Child::Dir {
                name,
                node_index: child_index,
            });
        } else if meta.is_file() {
            let short = fat_name::unique_name(&path, false, &mut used_names);
            let Some(name) = iso_name_from_83(&short, false) else {
                eprintln!(
                    "iso9660: skipping {} (name cannot be folded uniquely)",
                    path.display()
                );
                continue;
            };
            staged.push(Child::File {
                name,
                host_path: path,
                len: meta.len(),
            });
        }
    }

    nodes[node_index] = staged;
    for (child_index, path) in pending_dirs {
        walk_dir(&path, child_index, nodes)?;
    }
    Ok(())
}

/// Turn a packed 8.3 `fat_name` field (11 bytes, space-padded) into an ISO
/// level-1 short name: `STEM.EXT;1` for a file (or `STEM;1` with no dot if the
/// extension is empty), `STEM` (no version) for a directory. Returns None if
/// the resulting name would be empty.
fn iso_name_from_83(short: &[u8; 11], is_dir: bool) -> Option<Vec<u8>> {
    let stem: Vec<u8> = short[0..8].iter().copied().filter(|&b| b != b' ').collect();
    let ext: Vec<u8> = short[8..11]
        .iter()
        .copied()
        .filter(|&b| b != b' ')
        .collect();
    if stem.is_empty() {
        return None;
    }
    let mut name = stem;
    if !ext.is_empty() {
        name.push(b'.');
        name.extend_from_slice(&ext);
    }
    if !is_dir {
        name.extend_from_slice(b";1");
    }
    Some(name)
}

/// Sectors needed to hold `len` bytes, rounded up (0 bytes still needs 0
/// sectors, callers `.max(1)` where a directory or file always needs at least
/// one).
fn sectors_for(len: usize) -> u32 {
    len.div_ceil(SECTOR) as u32
}

/// Byte length of one directory record for a name of `name_len` bytes,
/// including the padding byte that keeps every record an even length.
fn dir_record_len(name_len: usize) -> usize {
    let base = 33 + name_len;
    base + (base % 2)
}

/// Format one ISO9660 directory record. `name` is the raw name bytes (already
/// including `;1` for files, or `[0]`/`[1]` for the self/parent entries).
fn dir_record(lba: u32, len: u32, flags: u8, name: &[u8]) -> Vec<u8> {
    let name_len = name.len();
    let mut record = vec![0u8; dir_record_len(name_len)];
    record[0] = record.len() as u8; // length of this record
    record[1] = 0; // extended attribute record length
    record[2..6].copy_from_slice(&lba.to_le_bytes());
    record[6..10].copy_from_slice(&lba.to_be_bytes());
    record[10..14].copy_from_slice(&len.to_le_bytes());
    record[14..18].copy_from_slice(&len.to_be_bytes());
    // Recording date/time (7 bytes: years-since-1900, month, day, hour, min,
    // sec, GMT offset in 15-min intervals). Not meaningful for a synthesized
    // disc; zeroed like the rest of the record.
    record[18..25].copy_from_slice(&[0, 1, 1, 0, 0, 0, 0]);
    record[25] = flags;
    record[26] = 0; // file unit size (non-interleaved)
    record[27] = 0; // interleave gap size (non-interleaved)
    record[28..30].copy_from_slice(&1u16.to_le_bytes()); // volume sequence number
    record[30..32].copy_from_slice(&1u16.to_be_bytes());
    record[32] = name_len as u8;
    record[33..33 + name_len].copy_from_slice(name);
    record
}

/// Map each node index to its parent node index (root maps to itself).
fn parent_indices(nodes: &[Vec<Child>]) -> Vec<usize> {
    let mut parent = vec![0usize; nodes.len()];
    for (i, children) in nodes.iter().enumerate() {
        for child in children {
            if let Child::Dir { node_index, .. } = child {
                parent[*node_index] = i;
            }
        }
    }
    parent
}

/// Concatenated LE-format path table bytes, used only to size the LE/BE path
/// table regions before directory LBAs are assigned (the path table's own
/// byte length does not depend on directory LBAs, only on name lengths).
fn build_path_table_bytes(nodes: &[Vec<Child>]) -> Vec<u8> {
    let mut len = 0usize;
    // Root entry: name length 1 (the 0x00 root marker), no padding beyond the
    // fixed 8-byte header.
    len += path_table_entry_len(1);
    for children in nodes {
        for child in children {
            if let Child::Dir { name, .. } = child {
                len += path_table_entry_len(name.len());
            }
        }
    }
    vec![0u8; len]
}

fn path_table_entry_len(name_len: usize) -> usize {
    8 + name_len + (name_len % 2)
}

/// Build the LE or BE path table. Entries are emitted in node-index order
/// (root first, then each directory in the order it was discovered), which
/// satisfies ISO9660's "parent must precede child" path-table ordering since
/// a child node index is always greater than its parent's.
fn build_path_table(
    nodes: &[Vec<Child>],
    dir_lba: &[u32],
    parent_of: &[usize],
    little_endian: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    // Path-table parent numbers are 1-based indices into this same table.
    // Entries are emitted in plain node-index order (root = index 0 = number
    // 1), and since a child node index is always greater than its parent's,
    // that order already keeps every parent's number assigned before any
    // child references it as `path_table_number(parent_of[i])`.
    let path_table_number = |node_index: usize| node_index as u16 + 1;

    for i in 0..nodes.len() {
        let name: Vec<u8> = if i == 0 {
            vec![0]
        } else {
            dir_own_name(nodes, i)
        };
        let name_len = name.len() as u8;
        let mut entry = vec![0u8; path_table_entry_len(name.len())];
        entry[0] = name_len;
        entry[1] = 0; // extended attribute record length
        if little_endian {
            entry[2..6].copy_from_slice(&dir_lba[i].to_le_bytes());
            entry[6..8].copy_from_slice(&path_table_number(parent_of[i]).to_le_bytes());
        } else {
            entry[2..6].copy_from_slice(&dir_lba[i].to_be_bytes());
            entry[6..8].copy_from_slice(&path_table_number(parent_of[i]).to_be_bytes());
        }
        entry[8..8 + name.len()].copy_from_slice(&name);
        out.extend_from_slice(&entry);
    }
    out
}

/// Find the name a directory node was given by its parent's child list.
fn dir_own_name(nodes: &[Vec<Child>], node_index: usize) -> Vec<u8> {
    for children in nodes {
        for child in children {
            if let Child::Dir {
                name,
                node_index: idx,
            } = child
                && *idx == node_index
            {
                return name.clone();
            }
        }
    }
    Vec::new()
}

/// Build the 2048-byte Primary Volume Descriptor at LBA 16.
fn build_pvd(
    total_sectors: u32,
    root_record: &[u8],
    path_table_size: u32,
    path_table_le_lba: u32,
    path_table_be_lba: u32,
) -> Vec<u8> {
    let mut pvd = vec![0u8; SECTOR];
    pvd[0] = 0x01; // type: primary volume descriptor
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 0x01; // version
    write_ascii_padded(&mut pvd[8..40], "IZARRAVM"); // system identifier
    write_ascii_padded(&mut pvd[40..72], "IZARRAVM_FOLDER"); // volume identifier
    pvd[80..84].copy_from_slice(&total_sectors.to_le_bytes());
    pvd[84..88].copy_from_slice(&total_sectors.to_be_bytes());
    pvd[120..122].copy_from_slice(&1u16.to_le_bytes()); // volume set size
    pvd[122..124].copy_from_slice(&1u16.to_be_bytes());
    pvd[124..126].copy_from_slice(&1u16.to_le_bytes()); // volume sequence number
    pvd[126..128].copy_from_slice(&1u16.to_be_bytes());
    pvd[128..130].copy_from_slice(&(SECTOR as u16).to_le_bytes()); // logical block size
    pvd[130..132].copy_from_slice(&(SECTOR as u16).to_be_bytes());
    pvd[132..136].copy_from_slice(&path_table_size.to_le_bytes());
    pvd[136..140].copy_from_slice(&path_table_size.to_be_bytes());
    pvd[140..144].copy_from_slice(&path_table_le_lba.to_le_bytes()); // type-L path table
    // Bytes 144-147 (optional type-L path table) stay zero.
    pvd[148..152].copy_from_slice(&path_table_be_lba.to_be_bytes()); // type-M path table
    // Bytes 152-155 (optional type-M path table) stay zero.
    let root_len = root_record.len().min(34);
    pvd[156..156 + root_len].copy_from_slice(&root_record[..root_len]);
    write_ascii_padded(&mut pvd[190..318], ""); // volume set identifier
    write_ascii_padded(&mut pvd[318..446], "IZARRAVM"); // publisher identifier
    write_ascii_padded(&mut pvd[446..574], "IZARRAVM"); // data preparer identifier
    write_ascii_padded(&mut pvd[574..702], ""); // application identifier
    pvd[881] = 1; // file structure version
    pvd
}

fn build_terminator() -> Vec<u8> {
    let mut term = vec![0u8; SECTOR];
    term[0] = 0xFF; // volume descriptor set terminator
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 0x01;
    term
}

fn write_ascii_padded(field: &mut [u8], text: &str) {
    for slot in field.iter_mut() {
        *slot = b' ';
    }
    for (slot, b) in field.iter_mut().zip(text.bytes()) {
        *slot = b;
    }
}

fn write_at(meta: &mut [u8], lba: u32, data: &[u8]) {
    let off = lba as usize * SECTOR;
    let end = (off + data.len()).min(meta.len());
    if off < meta.len() {
        meta[off..end].copy_from_slice(&data[..end - off]);
    }
}

#[cfg(test)]
#[path = "iso9660_test.rs"]
mod tests;
