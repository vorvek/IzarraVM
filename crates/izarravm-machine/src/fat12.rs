//! Synthesize a 1.44 MB FAT12 floppy image from a host folder.
//!
//! The output is a data disk: a valid BPB and the 0xAA55 boot-sector signature,
//! but no bootstrap code. Files and subdirectories under the input folder are
//! laid down read-only. In-session guest writes land in the in-memory image and
//! are not synced back to the host folder (see the ceiling note on `build_fat12`).

use std::fs;
use std::path::Path;

const SECTOR: usize = 512;
const TOTAL_SECTORS: usize = 2880;
const IMAGE_SIZE: usize = SECTOR * TOTAL_SECTORS; // 1,474,560
const RESERVED_SECTORS: usize = 1;
const NUM_FATS: usize = 2;
const SECTORS_PER_FAT: usize = 9;
const ROOT_ENTRIES: usize = 224;
const DIR_ENTRY_SIZE: usize = 32;
const ROOT_DIR_SECTORS: usize = (ROOT_ENTRIES * DIR_ENTRY_SIZE) / SECTOR; // 14
const SECTORS_PER_CLUSTER: usize = 1;

/// First data sector (cluster 2 maps here).
const FIRST_DATA_SECTOR: usize = RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT + ROOT_DIR_SECTORS; // 33
const DATA_SECTORS: usize = TOTAL_SECTORS - FIRST_DATA_SECTOR;
/// Highest usable cluster number. Cluster numbering starts at 2.
const MAX_CLUSTER: usize = DATA_SECTORS + 1;

const ATTR_DIRECTORY: u8 = 0x10;

const FAT12_EOC: u16 = 0xFFF;

/// One staged directory: a flat list of entries to be written into a directory
/// region, plus the cluster that holds the region (None for the fixed root).
struct DirStage {
    /// 32-byte directory entries, already formatted.
    entries: Vec<[u8; DIR_ENTRY_SIZE]>,
    /// Cluster this directory lives in, or None for the fixed-size root.
    cluster: Option<u16>,
}

/// Writer state threaded through the recursive walk.
struct Builder {
    /// 12-bit FAT entries, indexed by cluster number. Index 0/1 are reserved.
    fat: Vec<u16>,
    /// Allocated data clusters, keyed by cluster number, each one sector of data.
    cluster_data: Vec<(u16, Vec<u8>)>,
    next_free: usize,
    /// Directories whose entries still need to be flushed to disk.
    dirs: Vec<DirStage>,
}

impl Builder {
    fn new() -> Self {
        Self {
            fat: vec![0u16; MAX_CLUSTER + 1],
            cluster_data: Vec::new(),
            next_free: 2,
            dirs: Vec::new(),
        }
    }

    fn free_clusters(&self) -> usize {
        MAX_CLUSTER + 1 - self.next_free
    }

    /// Allocate a chain of `n` clusters, set the FAT links, and return the head.
    /// Caller fills in the cluster data sectors afterwards. Returns None if there
    /// is not enough free space.
    fn alloc_chain(&mut self, n: usize) -> Option<Vec<u16>> {
        if n == 0 || self.free_clusters() < n {
            return None;
        }
        let mut chain = Vec::with_capacity(n);
        for _ in 0..n {
            chain.push(self.next_free as u16);
            self.next_free += 1;
        }
        for w in chain.windows(2) {
            self.fat[usize::from(w[0])] = w[1];
        }
        self.fat[usize::from(*chain.last().unwrap())] = FAT12_EOC;
        Some(chain)
    }

    /// Store `data` across an allocated chain, one sector per cluster.
    fn store_data(&mut self, chain: &[u16], data: &[u8]) {
        for (i, &cl) in chain.iter().enumerate() {
            let start = i * SECTOR;
            let end = (start + SECTOR).min(data.len());
            let mut sector = vec![0u8; SECTOR];
            if start < data.len() {
                sector[..end - start].copy_from_slice(&data[start..end]);
            }
            self.cluster_data.push((cl, sector));
        }
    }
}

/// Build a 1.44 MB FAT12 image from the files under `root`.
///
/// Subdirectories are recursed. 8.3 names are uppercased, illegal characters are
/// stripped, long names are truncated, and collisions are resolved with `~1`,
/// `~2` suffixes. Files larger than the remaining free space are skipped with a
/// log line; the total is capped at disk capacity.
///
/// Ceiling: the image is read-mostly. Guest writes during a session stay in the
/// in-memory image and are not written back to the host folder.
pub fn build_fat12(root: &Path) -> Result<Vec<u8>, String> {
    let mut b = Builder::new();

    // Build the root directory and recurse. The root has a fixed region, so its
    // own cluster is 0 (the "no cluster" sentinel) and it is staged with no
    // owning cluster.
    let root_entries = build_dir(&mut b, root, 0)?;
    b.dirs.push(DirStage {
        entries: root_entries,
        cluster: None,
    });

    Ok(assemble(&b))
}

/// Walk one host directory, emitting its FAT clusters and child directories, and
/// return the formatted 32-byte entries for this directory. `self_cluster` is the
/// data cluster that holds this directory (0 for the fixed-size root); it is the
/// value a child's ".." entry points at.
fn build_dir(
    b: &mut Builder,
    dir: &Path,
    self_cluster: u16,
) -> Result<Vec<[u8; DIR_ENTRY_SIZE]>, String> {
    let mut entries: Vec<[u8; DIR_ENTRY_SIZE]> = Vec::new();
    let mut used_names: Vec<[u8; 11]> = Vec::new();

    let read = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    // Sort for a deterministic layout.
    let mut children: Vec<_> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect::<Vec<_>>();
    children.sort();

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
                eprintln!("fat12: skipping {}: {e}", path.display());
                continue;
            }
        };

        if meta.is_dir() {
            // Allocate this subdirectory's own cluster before recursing so its
            // children's ".." entries can point back at it.
            let Some(chain) = b.alloc_chain(1) else {
                eprintln!("fat12: out of space, skipping directory {}", path.display());
                continue;
            };
            let dir_cluster = chain[0];

            let mut child_entries = build_dir(b, &path, dir_cluster)?;

            // A subdirectory starts with "." and ".." entries.
            let dot = dir_entry(b".          ", ATTR_DIRECTORY, dir_cluster, 0);
            // ".." points at the parent (this directory). The root is cluster 0.
            let dotdot = dir_entry(b"..         ", ATTR_DIRECTORY, self_cluster, 0);
            let mut full = vec![dot, dotdot];
            full.append(&mut child_entries);
            b.dirs.push(DirStage {
                entries: full,
                cluster: Some(dir_cluster),
            });

            let name = unique_name(&path, true, &mut used_names);
            entries.push(dir_entry(&name, ATTR_DIRECTORY, dir_cluster, 0));
        } else if meta.is_file() {
            let size = meta.len();
            let clusters_needed = size.div_ceil(SECTOR as u64) as usize;
            let clusters_needed = clusters_needed.max(if size == 0 { 0 } else { 1 });
            if clusters_needed > b.free_clusters() {
                eprintln!(
                    "fat12: out of space, skipping {} ({} bytes)",
                    path.display(),
                    size
                );
                continue;
            }
            let data = match fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("fat12: skipping {}: {e}", path.display());
                    continue;
                }
            };
            let head = if data.is_empty() {
                0
            } else {
                let Some(chain) = b.alloc_chain(clusters_needed) else {
                    eprintln!("fat12: out of space, skipping {}", path.display());
                    continue;
                };
                let head = chain[0];
                b.store_data(&chain, &data);
                head
            };
            let name = unique_name(&path, false, &mut used_names);
            entries.push(dir_entry(&name, 0x20, head, data.len() as u32));
        }
    }

    Ok(entries)
}

/// Compose a unique 8.3 name for `path`, recording it so later siblings collide
/// against it.
fn unique_name(path: &Path, is_dir: bool, used: &mut Vec<[u8; 11]>) -> [u8; 11] {
    let raw = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let base = make_83(raw, is_dir);
    if !used.iter().any(|u| u == &base) {
        used.push(base);
        return base;
    }
    // Resolve with ~1, ~2, ... suffixes on the base name.
    for n in 1..=999u32 {
        let candidate = make_83_with_tilde(raw, is_dir, n);
        if !used.iter().any(|u| u == &candidate) {
            used.push(candidate);
            return candidate;
        }
    }
    // Exhausted; reuse the base (extremely unlikely with 224 entries).
    used.push(base);
    base
}

/// Map a host name to a padded 8.3 field: 8 bytes name, 3 bytes extension,
/// uppercased, illegal characters stripped.
fn make_83(raw: &str, is_dir: bool) -> [u8; 11] {
    let (stem, ext) = split_stem_ext(raw, is_dir);
    pack_83(&stem, &ext)
}

/// Like `make_83`, but force a `~n` suffix into the stem (truncating to fit).
fn make_83_with_tilde(raw: &str, is_dir: bool, n: u32) -> [u8; 11] {
    let (stem, ext) = split_stem_ext(raw, is_dir);
    let tail = format!("~{n}");
    let keep = 8usize.saturating_sub(tail.len());
    let mut stem2: String = stem.chars().take(keep).collect();
    stem2.push_str(&tail);
    pack_83(&stem2, &ext)
}

/// Split a raw name into a cleaned uppercase stem and extension.
fn split_stem_ext(raw: &str, is_dir: bool) -> (String, String) {
    // Directories ignore any dot for the extension split; treat the whole name
    // as the stem so "my.dir" does not get a bogus extension.
    let (stem_raw, ext_raw) = if is_dir {
        (raw, "")
    } else {
        match raw.rfind('.') {
            Some(i) if i > 0 => (&raw[..i], &raw[i + 1..]),
            _ => (raw, ""),
        }
    };
    let stem = clean(stem_raw);
    let ext = clean(ext_raw);
    (stem, ext)
}

/// Strip characters illegal in an 8.3 name and uppercase the rest.
fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let c = c.to_ascii_uppercase();
            if is_legal_83(c) {
                Some(c)
            } else if c == ' ' || c == '.' {
                None
            } else {
                // Replace any other stray byte with an underscore.
                Some('_')
            }
        })
        .collect()
}

fn is_legal_83(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '('
                | ')'
                | '-'
                | '@'
                | '^'
                | '_'
                | '`'
                | '{'
                | '}'
                | '~'
        )
}

/// Pad a stem (<=8) and extension (<=3) into the canonical 11-byte field.
fn pack_83(stem: &str, ext: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let stem: Vec<u8> = stem.bytes().take(8).collect();
    let ext: Vec<u8> = ext.bytes().take(3).collect();
    if stem.is_empty() {
        // A name that cleaned away to nothing still needs a stem.
        out[0] = b'_';
    } else {
        out[..stem.len()].copy_from_slice(&stem);
    }
    out[8..8 + ext.len()].copy_from_slice(&ext);
    out
}

/// Format one 32-byte directory entry.
fn dir_entry(name83: &[u8], attr: u8, cluster: u16, size: u32) -> [u8; DIR_ENTRY_SIZE] {
    let mut e = [0u8; DIR_ENTRY_SIZE];
    e[..11].copy_from_slice(&name83[..11]);
    e[11] = attr;
    // Bytes 12..26 (reserved, times, dates) stay zero. FAT12 ignores them.
    e[26..28].copy_from_slice(&cluster.to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// Lay out the reserved sector, the two FATs, the root directory, and the data
/// area into a finished image.
fn assemble(b: &Builder) -> Vec<u8> {
    let mut img = vec![0u8; IMAGE_SIZE];

    write_boot_sector(&mut img);

    // Encode the FAT once, then mirror it.
    let fat_bytes = encode_fat12(&b.fat);
    for fat_index in 0..NUM_FATS {
        let off = (RESERVED_SECTORS + fat_index * SECTORS_PER_FAT) * SECTOR;
        img[off..off + fat_bytes.len()].copy_from_slice(&fat_bytes);
    }

    // Root directory region.
    let root_off = (RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT) * SECTOR;

    // Flush staged directories: the root into its fixed region, others into
    // their data cluster.
    for dir in &b.dirs {
        match dir.cluster {
            None => {
                // Root: up to ROOT_ENTRIES entries.
                let mut off = root_off;
                for (i, e) in dir.entries.iter().enumerate() {
                    if i >= ROOT_ENTRIES {
                        break;
                    }
                    img[off..off + DIR_ENTRY_SIZE].copy_from_slice(e);
                    off += DIR_ENTRY_SIZE;
                }
            }
            Some(cl) => {
                let sec = cluster_to_sector(cl);
                let mut off = sec * SECTOR;
                // One cluster holds 16 entries; a directory that overflows is
                // truncated (the ceiling for this slice).
                let cap = SECTOR / DIR_ENTRY_SIZE;
                for (i, e) in dir.entries.iter().enumerate() {
                    if i >= cap {
                        break;
                    }
                    img[off..off + DIR_ENTRY_SIZE].copy_from_slice(e);
                    off += DIR_ENTRY_SIZE;
                }
            }
        }
    }

    // Data clusters.
    for (cl, data) in &b.cluster_data {
        let sec = cluster_to_sector(*cl);
        let off = sec * SECTOR;
        img[off..off + SECTOR].copy_from_slice(data);
    }

    img
}

fn cluster_to_sector(cluster: u16) -> usize {
    FIRST_DATA_SECTOR + (usize::from(cluster) - 2) * SECTORS_PER_CLUSTER
}

/// Encode the cluster array into packed 12-bit FAT entries. Entry 0 holds the
/// media descriptor (0xF0) plus 0xFFF; entry 1 is 0xFFF.
fn encode_fat12(fat: &[u16]) -> Vec<u8> {
    let mut entries = fat.to_vec();
    // Reserved entries: media descriptor 0xF0 means FAT[0] = 0x0FF0, FAT[1] holds
    // the end-of-chain marker 0x0FFF.
    entries[0] = 0x0FF0;
    entries[1] = 0x0FFF;

    let mut out = vec![0u8; SECTORS_PER_FAT * SECTOR];
    let mut byte = 0usize;
    let mut i = 0usize;
    while i + 1 < entries.len() && byte + 2 < out.len() {
        let a = entries[i] & 0x0FFF;
        let b = entries[i + 1] & 0x0FFF;
        out[byte] = (a & 0xFF) as u8;
        out[byte + 1] = (((a >> 8) & 0x0F) as u8) | (((b & 0x0F) as u8) << 4);
        out[byte + 2] = ((b >> 4) & 0xFF) as u8;
        byte += 3;
        i += 2;
    }
    out
}

/// Write the reserved boot sector: a jump stub, an OEM name, the BPB, and the
/// 0xAA55 signature. No bootstrap code (this is a data disk).
fn write_boot_sector(img: &mut [u8]) {
    let s = &mut img[..SECTOR];
    // JMP short over the BPB, then NOP, as DOS expects at offset 0.
    s[0] = 0xEB;
    s[1] = 0x3C;
    s[2] = 0x90;
    // OEM name, 8 bytes at offset 3.
    s[3..11].copy_from_slice(b"IZARRA10");

    // BPB at offset 11.
    s[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes()); // bytes/sector
    s[13] = SECTORS_PER_CLUSTER as u8; // sectors/cluster
    s[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes()); // reserved
    s[16] = NUM_FATS as u8; // number of FATs
    s[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes()); // root entries
    s[19..21].copy_from_slice(&(TOTAL_SECTORS as u16).to_le_bytes()); // total sectors (16-bit)
    s[21] = 0xF0; // media descriptor: 1.44 MB
    s[22..24].copy_from_slice(&(SECTORS_PER_FAT as u16).to_le_bytes()); // sectors/FAT
    s[24..26].copy_from_slice(&18u16.to_le_bytes()); // sectors/track (1.44 MB geometry)
    s[26..28].copy_from_slice(&2u16.to_le_bytes()); // heads
    s[28..32].copy_from_slice(&0u32.to_le_bytes()); // hidden sectors
    s[32..36].copy_from_slice(&0u32.to_le_bytes()); // large total sectors (unused)

    // Extended BPB (FAT12).
    s[36] = 0x00; // drive number (A:)
    s[37] = 0x00; // reserved
    s[38] = 0x29; // extended boot signature
    s[39..43].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // volume id
    s[43..54].copy_from_slice(b"NO NAME    "); // volume label, 11 bytes
    s[54..62].copy_from_slice(b"FAT12   "); // file system type, 8 bytes

    // Boot signature at the end of the sector.
    s[510] = 0x55;
    s[511] = 0xAA;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the 12-bit FAT entry for a cluster.
    fn fat_entry(img: &[u8], cluster: usize) -> u16 {
        let fat_off = RESERVED_SECTORS * SECTOR;
        let i = cluster * 3 / 2;
        let lo = img[fat_off + i] as u16;
        let hi = img[fat_off + i + 1] as u16;
        if cluster & 1 == 0 {
            lo | ((hi & 0x0F) << 8)
        } else {
            (lo >> 4) | (hi << 4)
        }
    }

    /// Locate a root entry by its 11-byte 8.3 name and return (first_cluster, size).
    fn find_root_entry(img: &[u8], name11: &str) -> Option<(u16, u32)> {
        let root_off = (RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT) * SECTOR;
        let name = name11.as_bytes();
        for i in 0..ROOT_ENTRIES {
            let off = root_off + i * DIR_ENTRY_SIZE;
            let e = &img[off..off + DIR_ENTRY_SIZE];
            if e[0] == 0x00 {
                break; // no more entries
            }
            if &e[..11] == name {
                let cluster = u16::from_le_bytes([e[26], e[27]]);
                let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
                return Some((cluster, size));
            }
        }
        None
    }

    /// Recover a file's bytes by following its FAT cluster chain.
    fn read_file_by_chain(img: &[u8], first_cluster: u16, size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cl = first_cluster;
        while (2..0xFF0).contains(&cl) {
            let sec = cluster_to_sector(cl);
            let off = sec * SECTOR;
            out.extend_from_slice(&img[off..off + SECTOR]);
            let next = fat_entry(img, usize::from(cl));
            cl = next;
        }
        out.truncate(size as usize);
        out
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fat12_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn image_has_valid_bpb_and_signature() {
        let dir = temp_dir("bpb");
        std::fs::write(dir.join("A.TXT"), b"x").unwrap();
        let img = build_fat12(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(img.len(), 1_474_560);
        // Bytes per sector.
        assert_eq!(u16::from_le_bytes([img[11], img[12]]), 512);
        // Sectors per cluster.
        assert_eq!(img[13], 1);
        // Reserved sectors.
        assert_eq!(u16::from_le_bytes([img[14], img[15]]), 1);
        // Number of FATs.
        assert_eq!(img[16], 2);
        // Sectors per FAT.
        assert_eq!(u16::from_le_bytes([img[22], img[23]]), 9);
        // Root entries.
        assert_eq!(u16::from_le_bytes([img[17], img[18]]), 224);
        // Total sectors.
        assert_eq!(u16::from_le_bytes([img[19], img[20]]), 2880);
        // Media descriptor.
        assert_eq!(img[21], 0xF0);
        // Boot signature 0xAA55 at offset 510 (0x55, 0xAA in byte order).
        assert_eq!(&img[510..512], &[0x55, 0xAA]);
    }

    #[test]
    fn known_file_round_trips_through_cluster_chain() {
        let dir = temp_dir("roundtrip");
        // Larger than one sector so it spans a multi-cluster chain.
        let payload: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("hello.txt"), &payload).unwrap();
        let img = build_fat12(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let (cluster, size) = find_root_entry(&img, "HELLO   TXT").expect("HELLO.TXT in root");
        assert_eq!(size as usize, payload.len());
        let recovered = read_file_by_chain(&img, cluster, size);
        assert_eq!(recovered, payload);
    }

    #[test]
    fn small_file_round_trips() {
        let dir = temp_dir("small");
        std::fs::write(dir.join("HELLO.TXT"), b"hello world").unwrap();
        let img = build_fat12(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let (cluster, size) = find_root_entry(&img, "HELLO   TXT").expect("HELLO.TXT in root");
        let recovered = read_file_by_chain(&img, cluster, size);
        assert_eq!(recovered, b"hello world");
    }

    #[test]
    fn subdirectory_file_is_reachable() {
        let dir = temp_dir("subdir");
        let sub = dir.join("GAMES");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("READ.ME"), b"in a subdir").unwrap();
        let img = build_fat12(&dir).unwrap();

        // The subdirectory has a root entry with the DIRECTORY attribute.
        let root_off = (RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT) * SECTOR;
        let mut dir_cluster = None;
        for i in 0..ROOT_ENTRIES {
            let off = root_off + i * DIR_ENTRY_SIZE;
            let e = &img[off..off + DIR_ENTRY_SIZE];
            if e[0] == 0 {
                break;
            }
            if &e[..11] == b"GAMES      " {
                assert_eq!(e[11] & ATTR_DIRECTORY, ATTR_DIRECTORY);
                dir_cluster = Some(u16::from_le_bytes([e[26], e[27]]));
            }
        }
        let dir_cluster = dir_cluster.expect("GAMES directory entry");
        std::fs::remove_dir_all(&dir).ok();

        // Read the subdirectory's region and find READ.ME inside it.
        let sec = cluster_to_sector(dir_cluster);
        let region = &img[sec * SECTOR..sec * SECTOR + SECTOR];
        // Entry 0 = ".", entry 1 = "..", then the file.
        assert_eq!(&region[..11], b".          ");
        assert_eq!(&region[32..43], b"..         ");
        let mut found = None;
        for i in 0..(SECTOR / DIR_ENTRY_SIZE) {
            let e = &region[i * DIR_ENTRY_SIZE..i * DIR_ENTRY_SIZE + DIR_ENTRY_SIZE];
            if &e[..11] == b"READ    ME " {
                let cl = u16::from_le_bytes([e[26], e[27]]);
                let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
                found = Some((cl, size));
            }
        }
        let (cl, size) = found.expect("READ.ME inside GAMES");
        let recovered = read_file_by_chain(&img, cl, size);
        assert_eq!(recovered, b"in a subdir");
    }

    #[test]
    fn collisions_resolve_with_tilde_suffix() {
        let dir = temp_dir("collide");
        // Two names that both clean to the same 8.3 stem.
        std::fs::write(dir.join("longname1.txt"), b"one").unwrap();
        std::fs::write(dir.join("longname2.txt"), b"two").unwrap();
        let img = build_fat12(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // First keeps the truncated 8.3 name (8-char stem fills the field with
        // no padding), the second collides and gets a ~1 form.
        let first = find_root_entry(&img, "LONGNAMETXT");
        let second = find_root_entry(&img, "LONGNA~1TXT");
        assert!(first.is_some(), "first truncated name present");
        assert!(second.is_some(), "collision resolved with ~1");
    }

    #[test]
    fn oversized_file_is_skipped() {
        let dir = temp_dir("toobig");
        // A file larger than the whole disk capacity is skipped; a small one is kept.
        let big = vec![0u8; IMAGE_SIZE + SECTOR];
        std::fs::write(dir.join("BIG.BIN"), &big).unwrap();
        std::fs::write(dir.join("OK.TXT"), b"kept").unwrap();
        let img = build_fat12(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            find_root_entry(&img, "BIG     BIN").is_none(),
            "big file skipped"
        );
        assert!(
            find_root_entry(&img, "OK      TXT").is_some(),
            "small file kept"
        );
    }
}
