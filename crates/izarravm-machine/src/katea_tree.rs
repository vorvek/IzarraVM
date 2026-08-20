// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Recursive, lazy host-folder directory tree for the Katea controller.
//! Generalizes the flat `KateaVolume` into a full FAT32 directory tree whose
//! FAT and directory sectors are computed on demand and whose guest writes are
//! projected incrementally to host files.

// `KateaTreeVolume` is consumed by the ATA `HostFolder` backing (`ata.rs`) and
// `mount_hdd_folder` (`lib.rs`). A few
// items remain reachable only from this module's `#[cfg(test)]` tests; each
// carries a narrow `#[allow(dead_code)]` at its definition: `tree()` and the
// `tree` field it reads, and the free `dir_sector` (the per-volume read path
// inlines the same logic in `data_sector`). The `tree` field is also the seam
// the write engine reads at construction.

use crate::fat32::{FAT32_EOC, fat32_dir_entry, fat32_fsinfo_sector};
use crate::katea_names::NameTable;
use crate::katea_volume::{
    ATTR_ARCHIVE, BACKUP_BOOT_SECTOR, BACKUP_FSINFO_SECTOR, FAT0_MEDIA, FSINFO_SECTOR, FileSource,
    NUM_FATS, PART_START, PART_TYPE_FAT32_LBA, RESERVED_SECTORS, ROOT_CLUSTER, SECTOR,
    fat_size_sectors, lba_to_chs, sectors_per_cluster, stamp_fat32_bpb,
};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Cap recursion so a pathological tree (or an undetected loop) can't run away;
/// also roughly the depth DOS's 64-char path limit allows.
const MAX_DEPTH: usize = 32;

/// Cap on cached open host write handles. A guest writes one or two files at a
/// time; this is only large enough that a normal working set never evicts.
const MAX_HOST_WRITE_HANDLES: usize = 64;

/// One host range read is bounded to the default 86Box IDE cache window.
const HOST_READ_COMMAND_WINDOW_SECTORS: u32 = 64;

/// Ceiling on one read-ahead fill. 86Box's IDE path reads a whole ATA command
/// (up to 256 sectors) into a persistent per-drive 128 KiB buffer; this is the
/// same shape, one notch larger, because a DOS game's asset load arrives as a
/// long run of separate 8-sector (one 4 KiB cluster) INT 13h commands rather
/// than as one big command, so the buffer has to span *commands* to pay off.
///
/// A FILL NEVER STARTS THIS BIG. It starts at the extent the command actually
/// asked for -- one sector when nothing declared a command at all, which is what
/// a reconcile gather looks like -- and doubles only when the previous fill for
/// that same path was consumed to its exact end. Any other offset (a backward
/// seek, a jump, a first touch) resets it to the command extent.
///
/// That is the whole amplification argument. A fill larger than the command is
/// only ever granted to a path that has already been read sequentially that far,
/// so the bytes read ahead are paid for by bytes already served: physical bytes
/// stay within roughly twice logical bytes, the overshoot being the last fill of
/// a stream that stops early. A flat fill has no such bound -- a single-sector
/// probe outside any command would pull 256 KiB for 512 bytes, and an
/// alternating or backward pattern would do it on every read.
///
/// The ramp is per path (see [`HostReadAhead::next_fill`]) and there are several
/// slots, so two interleaved files each keep their own place and their own ramp.
const HOST_READAHEAD_MAX_BYTES: u64 = 256 * 1024;

/// Read-ahead slots. Four, for the reason the handle cache holds eight: an asset
/// load interleaves a handful of files (the executable, an overlay, a PAK), and
/// a single slot would miss on every alternation -- the worst case for a ramp,
/// because a miss is also what resets it.
const HOST_READAHEAD_SLOTS: usize = 4;

/// Cap on cached open host READ handles. Symmetric with
/// [`MAX_HOST_WRITE_HANDLES`], but far smaller: DOS reads a handful of files at a
/// time (the executable, its overlay, a PAK), so eight entries cover a real
/// working set while keeping the linear scan below trivially short.
const MAX_HOST_READ_HANDLES: usize = 8;

#[derive(Debug)]
struct HostReadWindow {
    start_lba: u32,
    bytes: Vec<u8>,
}

/// Bytes read ahead of the guest out of one host file, surviving command
/// boundaries. Keyed by path plus the byte range it covers, so a hit is exact:
/// a sector is served from here only when this buffer holds the very bytes that
/// a fresh `seek`+`read` of `path` at that offset would return.
///
/// INVALIDATION: every host mutation funnels through
/// [`KateaTreeVolume::invalidate_host_reads`], which drops this buffer (scoped
/// to a path where the mutation is scoped to one). See that method for why the
/// funnel is complete.
#[derive(Debug)]
struct HostReadAhead {
    path: PathBuf,
    /// Byte offset in `path` that `bytes[0]` came from.
    start: u64,
    bytes: Vec<u8>,
    /// What the next fill for this path is allowed to reach, if that fill starts
    /// at exactly `start + bytes.len()`. Twice the fill that produced this
    /// buffer, capped at [`HOST_READAHEAD_MAX_BYTES`]. A fill at any other offset
    /// ignores it and takes the command extent instead. See
    /// [`HOST_READAHEAD_MAX_BYTES`] for the amplification argument this carries.
    next_fill: u64,
}

/// One cluster-chain walk's view of the FAT, resolved a SECTOR at a time.
///
/// A FAT32 sector holds 128 entries, and a chain walks its clusters in ascending
/// order, so one resolve answers up to 128 steps. See
/// [`KateaTreeVolume::fat_entry_walked`] for what a resolve costs and why the
/// per-cluster version it replaced made the projection pass scale with the
/// mounted folder.
///
/// The cursor is only ever alive inside one walk, and nothing writes to the store
/// or the host during a walk, so what it caches cannot go stale under it.
#[derive(Default)]
struct FatWalk {
    /// The FAT-copy-relative sector index the cursor currently describes.
    within: Option<u32>,
    /// The guest's bytes for that sector, when the store holds it. `None` means
    /// the base view answers, and the base view needs no sector at all --
    /// `ClusterIndex::fat_entry` is a single `HashSet` probe. Boxed so the cursor
    /// itself stays pointer-sized on the walk's stack frame.
    overlay: Option<Box<[u8; SECTOR]>>,
    /// Every FAT sector this walk resolved, in resolve order. This is the walk's
    /// COMPLETE dependency set -- see [`ChainMemo`], which keeps it so a later
    /// pass can prove the chain cannot have changed.
    sectors: Vec<u32>,
    /// Set when a resolve failed to read a guest-written FAT sector back and the
    /// walk therefore ran on substituted zeros. Such a walk is never memoized:
    /// its answer is a function of an I/O failure rather than of the volume.
    degraded: bool,
}

/// A memoized cluster chain, and exactly what would have to change to make it
/// wrong.
///
/// WHY. Even with the per-sector walk cursor, the projection pass still stepped
/// every cluster of every live file's chain three to four times over, and did it
/// again on the next pass whether or not anything had changed. Measured: a worst
/// pass of 0.96 ms on a 47 MB folder against 9.68 ms on a 498 MB one -- the same
/// tenfold ratio as the allocated cluster count, for a write that touched one
/// file.
///
/// WHEN IT IS STILL VALID. A chain from `first` is fully determined by the FAT
/// entries at its own clusters. Each of those comes from one of exactly two
/// places: the store's overlay for the FAT sector holding it, or the base view.
///
/// * The base view never changes. `fat` and `geo` are built in `new` and never
///   mutated.
/// * The overlay changes only through `SectorStore::insert`, whose sole caller is
///   `write_sector`, which routes every FAT-region LBA through
///   `note_metadata_write` -- the site that stamps `fat_sector_epoch`. It can
///   also change through `SectorStore::acknowledge`, whose sole caller
///   acknowledges a projected file's DATA sectors and never a metadata one
///   (asserted there).
///
/// So if no sector in `sectors` has been stamped since `epoch`, every entry the
/// walk read still reads the same, and the walk would produce the same chain.
/// The dependency set is complete because it records EVERY resolve, including
/// the ones the base view answered: a sector that held no guest bytes then and
/// holds some now is stamped, and stamping is what expires the memo.
///
/// EXTENSION IS COVERED. A chain that has grown had its old last cluster's FAT
/// entry rewritten from EOC to a link, and that entry is in a sector the walk
/// read, so the memo expires. A chain that has been freed or re-pointed is the
/// same argument.
///
/// WHAT A HIT DOES TO THE READ-ERROR BRACKETS, which is a liveness question
/// rather than a staleness one and so is not answered by anything above.
/// `reconcile_mode` and phase 3 bracket `store.read_errors()` around their reads
/// (`:2877`, `:2905`, and per-file at the `MakeFile` arm) so a chain that could
/// not be read back is HELD rather than acted on. A memo hit performs no store
/// read, so a FAT sector that has become unreadable SINCE the memo was taken no
/// longer trips those brackets and no longer holds the file.
///
/// That is safe, and it is safe in the strong direction. A memo can only be
/// current if nothing has written its sectors, and a sector nothing has written
/// is one whose value we already hold correctly -- the walk that produced the
/// memo read it successfully. So the memoized chain IS the chain a successful
/// fresh read would return, and proceeding on the true chain is strictly better
/// than holding because we could not re-read a value we already knew. The
/// brackets exist to stop the pass acting on ZEROS substituted for a failed
/// read; a memo hit never substitutes anything.
///
/// The converse direction is closed by `FatWalk::degraded`: a walk that DID hit
/// a substituted-zero sector, or a link past FAT copy 0, is never memoized, so
/// no memo is ever derived from an I/O failure in the first place.
#[derive(Debug)]
struct ChainMemo {
    /// `katea_write::chain`'s answer, `None` included: a held chain is as worth
    /// memoizing as a good one, and for the same reason.
    result: Option<Vec<u32>>,
    /// The FAT sectors the walk read, FAT-copy-relative.
    sectors: Vec<u32>,
    /// `fat_epoch` when the walk ran.
    epoch: u64,
}

/// The keys claiming one cluster.
///
/// A `Vec` per cluster would be one heap allocation per cluster of the mounted
/// folder -- 128,000 of them on a 498 MB fixture -- for a list that is almost
/// always exactly one key long. The full scan it replaces stored a bare key in
/// its `HashMap` and allocated nothing, and the index has to be able to build
/// itself at least as cheaply as the scan it removes: the first measurement of
/// term A had the cold build at 1.7x the scan, and this was most of the
/// difference.
///
/// `Many` is reached only when a guest points two directory entries into one
/// cluster, which is exactly the case the guard exists for and is rare.
#[derive(Debug)]
enum Claimants {
    One((u32, [u8; 11])),
    Many(Vec<(u32, [u8; 11])>),
}

impl Claimants {
    fn len(&self) -> usize {
        match self {
            Claimants::One(_) => 1,
            Claimants::Many(v) => v.len(),
        }
    }

    /// The sole claimant, when there is exactly one.
    fn only(&self) -> Option<(u32, [u8; 11])> {
        match self {
            Claimants::One(k) => Some(*k),
            Claimants::Many(v) if v.len() == 1 => Some(v[0]),
            Claimants::Many(_) => None,
        }
    }

    fn push(&mut self, key: (u32, [u8; 11])) {
        match self {
            Claimants::One(existing) => *self = Claimants::Many(vec![*existing, key]),
            Claimants::Many(v) => v.push(key),
        }
    }

    /// Remove one occurrence of `key`; returns the new length.
    fn remove(&mut self, key: (u32, [u8; 11])) -> usize {
        match self {
            Claimants::One(existing) => {
                if *existing == key {
                    *self = Claimants::Many(Vec::new());
                    0
                } else {
                    1
                }
            }
            Claimants::Many(v) => {
                if let Some(at) = v.iter().position(|k| *k == key) {
                    v.swap_remove(at);
                }
                v.len()
            }
        }
    }
}

/// What dates a chain walk: `fat_epoch` when it ran, and the FAT sectors it
/// read. See [`ChainMemo`] for why those two are exactly the walk's dependency
/// set, and [`KateaTreeVolume::chain_token_is_current`] for the test.
type ChainToken = (u64, Vec<u32>);

/// One live directory entry's contribution to the claim index, and the token
/// that says the contribution is still what a fresh walk would produce.
///
/// The token is exactly [`ChainMemo`]'s: `epoch` is `fat_epoch` at the walk and
/// `sectors` the walk's FAT dependency set. Held here rather than looked up in
/// the memo because the memo is keyed by first cluster and is evictable at
/// [`CHAIN_MEMO_MAX_ENTRIES`], while a claim is keyed by directory ENTRY and must
/// outlive that -- a claim that vanished when a memo was evicted would be a
/// RETRACTION, which is the one direction the guard cannot afford (see
/// [`KateaTreeVolume::insert_claim`]).
#[derive(Debug)]
struct ClaimUnit {
    first_cluster: u32,
    epoch: u64,
    sectors: Vec<u32>,
}

/// What one directory entry KEY currently claims.
///
/// `units` is one per live entry with this key -- normally exactly one, but a
/// guest can write two directory entries with the same 8.3 name and the guard's
/// definition treats them as one file whose clusters are the UNION of theirs
/// (see `ambiguous_by_full_scan`'s property 2). `clusters` is that union, sorted
/// and deduplicated, so the key appears at most once in each `claimants` list.
#[derive(Debug)]
struct KeyClaim {
    units: Vec<ClaimUnit>,
    clusters: Vec<u32>,
    /// How many of `clusters` are contested -- claimed by another live key, or
    /// registered as a directory cluster. The key is ambiguous exactly when this
    /// is non-zero.
    ///
    /// A COUNT, not a sticky flag. A flag set by one pass and never cleared
    /// would be order-dependent and would not be cost-only: one permanent
    /// over-flag keeps a key in `blocked_projection_keys`, which makes
    /// `metadata_projection_pending` return `true` for the rest of the session,
    /// which pins a reconcile on every command and moves
    /// `metadata_projection_passes`. Reference counting is what makes the
    /// incremental answer EQUAL to the full scan's rather than a superset of it.
    bad: u32,
}

/// Passes between forced refreshes of any one key.
///
/// Insurance, not correctness: the invalidation hooks are what make the index
/// right, and this bounds how long an unanticipated miss could persist. It is
/// AMORTISED -- each pass forcibly refreshes `1/N` of the live keys in rotation
/// rather than rebuilding everything every Nth pass. A periodic full rebuild
/// would land in `projection_max_ns`, which is the metric the whole slice is
/// graded on, and would make the worst pass a property of the rebuild rather
/// than of the work; and at any N large enough to be cheap it would never fire
/// at all on a 14-pass row.
const CLAIM_REFRESH_PERIOD: usize = 64;

/// Whether a directory's byte image ends where FAT says it should.
///
/// `katea_write::parse_dir` stops at the first entry whose first byte is 0x00,
/// which in a well-formed FAT directory means "this slot and every slot after it
/// is free". A TORN directory -- one whose middle sector the guest has zeroed but
/// not yet rewritten, which is what an installer growing a directory produces --
/// looks identical from `parse_dir`'s side: it stops early and silently hides
/// every entry behind the hole.
///
/// The signature is detectable, and it is the only one there is: a live or
/// deleted entry (first byte anything but 0x00) sitting AFTER the first 0x00.
/// DOS never writes that; a half-written directory does.
///
/// Used only to decide whether the incremental claim index may trust this pass's
/// live set. The reconcile phases themselves are unchanged and still treat the
/// truncated parse exactly as they always have.
fn directory_image_is_whole(bytes: &[u8]) -> bool {
    let mut terminated = false;
    for entry in bytes.chunks_exact(32) {
        match (terminated, entry[0]) {
            (false, 0x00) => terminated = true,
            (true, 0x00) => {}
            (true, _) => return false,
            (false, _) => {}
        }
    }
    true
}

/// Chain memo entries kept before the whole map is dropped.
///
/// The map is keyed by first cluster, so a well-behaved mount holds one entry
/// per live file and the total of their chains is bounded by the volume's
/// cluster count. What is NOT bounded by that is a guest that keeps creating
/// files at fresh clusters: each leaves an entry behind that nothing prunes.
/// Clearing wholesale at a ceiling costs one cold pass and cannot be wrong --
/// the memo is only ever an optimization over walking.
const CHAIN_MEMO_MAX_ENTRIES: usize = 65_536;

#[derive(Clone, Copy)]
struct HostReadContext<'a> {
    lba: u32,
    command_sectors: u32,
    counters: &'a KateaCounterCells,
    cache: &'a std::cell::RefCell<Vec<(PathBuf, File)>>,
    window: &'a std::cell::RefCell<Option<HostReadWindow>>,
    /// Read-ahead slots, MRU last. Empty when the read-ahead is disarmed.
    readahead: &'a std::cell::RefCell<Vec<HostReadAhead>>,
    /// Ceiling on one fill, or 0 when `IZARRAVM_HDD_READAHEAD=0` disarmed it.
    readahead_max: u64,
    /// First data-region LBA of the volume. Carried only to assert the window's
    /// range invariant where the window is filled -- see the `debug_assert` in
    /// [`read_host_span`], which is what lets [`KateaTreeVolume::fat_entry_walked`]
    /// resolve a FAT sector without consulting the window at all.
    first_data_lba: u32,
}

/// DOS device names Win32 still resolves as character devices. Per Microsoft's
/// "Naming Files, Paths, and Namespaces", these are reserved *in every
/// directory* -- `C:\mount\CON` is the console, not a file -- and the rule
/// applies to the name followed by any extension as well, so `NUL.TXT` is also
/// `NUL`. Matching is case-insensitive. COM0 and LPT0 are on Microsoft's
/// current list and cost nothing to include.
#[cfg(windows)]
const WIN32_RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The suffix that lifts a guest name off a Win32 device name. `+` is legal in a
/// Win32 file name and illegal in an 8.3 short name -- `fat_name::is_legal_83`
/// excludes it and `katea_write::classify` now rejects it -- so no name the
/// guest can write ever folds onto a mangled one. That is what keeps a guest
/// that creates both `CON` and some other name from having them collide on one
/// host file.
#[cfg(windows)]
const DEVICE_NAME_ESCAPE: char = '+';

/// The host file name for one guest 8.3 name. Everywhere a host path is derived
/// from guest directory bytes goes through here.
///
/// On Windows a bare `decode_83` would hand `OpenOptions::open` a device path
/// for a guest file called CON or NUL: the open succeeds against the console or
/// the null device, so the guest's bytes go to a terminal or vanish, and its
/// reads come back as zeros. The guard belongs here rather than in `classify`
/// because rejecting the *entry* would read as "this file disappeared" and
/// delete a real host file of that name -- which a non-Windows host can hold
/// perfectly well. So non-Windows keeps `decode_83` verbatim.
fn host_child_name(name: &[u8; 11]) -> String {
    let decoded = crate::katea_volume::decode_83(name);
    #[cfg(windows)]
    {
        let (stem, ext) = match decoded.split_once('.') {
            Some((stem, ext)) => (stem, Some(ext)),
            None => (decoded.as_str(), None),
        };
        if WIN32_RESERVED_DEVICE_NAMES
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        {
            return match ext {
                Some(ext) => format!("{stem}{DEVICE_NAME_ESCAPE}.{ext}"),
                None => format!("{stem}{DEVICE_NAME_ESCAPE}"),
            };
        }
    }
    decoded
}

/// Floor on the synthesized partition, in sectors: 532,481, which is exactly one
/// sector past where fatgen103's `DskTableFAT32` leaves 512-byte clusters behind
/// for 4 KiB ones. Every derived volume is at least this big, so every derived
/// volume gets `spc >= 8`.
///
/// This replaced a data-cluster floor of 94,742, which reproduced the flat static
/// image's proven-bootable 96,256-sector geometry — and, with it, that geometry's
/// `spc = 1`. 512-byte clusters are the degenerate case: a 44 MB file becomes an
/// ~86,600-entry FAT chain, DOS re-walks that chain on every seek, and 86.2% of
/// all sectors a measured Duke Nukem 3D benchmark read were FAT sectors. No 1997
/// tool would have produced it either — FORMAT.COM sized clusters off this same
/// table, and 512-byte clusters were reserved for volumes under 260 MB, which
/// FAT32 was not used on. Flooring the PARTITION rather than the cluster count is
/// what keeps the derivation self-consistent: `fat32_geometry_for` still requires
/// `sectors_per_cluster(part_sectors) == spc`, and a partition at or above this
/// floor satisfies it at `spc = 8` without any special case.
///
/// The bootability the old floor was protecting is not a property of that one
/// geometry: it comes from `fat_size_sectors` using the FreeDOS kernel's own
/// `CalculateFATData` formula, which holds at any `spc`. It is re-established by
/// test at the new size (`geometry_bounds_a_huge_folder_and_reproduces_m0_for_a_small_one`
/// pins the derived numbers, `no_folder_size_derives_five_hundred_twelve_byte_clusters`
/// states the property) and by every hdd-folder fixture in the scoreboard, all
/// of which boot through this geometry.
const MIN_PART_SECTORS: u32 = 532_481;

#[derive(Debug)]
pub(crate) struct TreeFile {
    pub name: [u8; 11],
    pub source: FileSource, // InMemory (system files) or HostFile (lazy)
    pub first_cluster: u32, // assigned after the tree is built
    pub cluster_count: u32,
}

#[derive(Debug)]
pub(crate) struct TreeSubdir {
    pub name: [u8; 11],
    pub dir: TreeDir,
}

#[derive(Debug, Default)]
pub(crate) struct TreeDir {
    pub files: Vec<TreeFile>,
    pub subdirs: Vec<TreeSubdir>,
    pub first_cluster: u32, // this directory's first cluster (root = 2)
    pub cluster_count: u32,
    pub parent_first_cluster: u32, // for `..`; 0 when the parent is the root
    /// This directory's host-filesystem path (root = the mounted folder). Used by
    /// the write engine to know where to materialize files created here.
    pub host_path: std::path::PathBuf,
}

#[derive(Debug)]
pub(crate) struct HostTree {
    pub root: TreeDir,
}

/// The Toka-DOS system binaries that the overlay places in a synthetic `C:\DOS`
/// folder rather than the boot-drive root, so the host-folder root isn't buried
/// under system files. This is the executable subset of the committed image's
/// `dos_files` (`scripts/build-freedos-hdd-image.py`). NOT here — and therefore
/// left at the root — are the boot/kernel-required files (KERNEL.SYS, CONFIG.SYS,
/// AUTOEXEC.BAT), LICENSE.TXT (the kernel signon points at `C:\LICENSE.TXT`), and
/// any user or runner file (which stays where it was dropped). HELLO.TXT is
/// excluded too: it is a data file, so a caller-supplied one stays at the root
/// where the guest expects it (the image's own demo HELLO.TXT is filtered out of
/// the overlay before it ever reaches here). Matched case-insensitively against the
/// overlay's 8.3 names.
const DOS_FOLDER_BINARIES: &[&str] = &[
    "COMMAND.COM",
    "TOKAMOUS.COM",
    "TOKAEMM.SYS",
    "TOKACD.SYS",
    "IZCDEX.COM",
    "GSWMODE.COM",
    "UNHALT.COM",
    "SNDCTRL.COM",
    "SNDMIXER.COM",
    "MOVE.EXE",
    "SORT.EXE",
    "MEM.EXE",
    "ATTRIB.EXE",
    "CHOICE.EXE",
    "MORE.EXE",
    "FIND.EXE",
    "LABEL.EXE",
    "DELTREE.COM",
    "XCOPY.EXE",
    "EDIT.COM",
    "GLIDE2X.OVL",
];

/// Build the tree from a host folder, overlaying the in-memory system files (so the
/// disk still boots). The Toka-DOS binaries land in a synthetic `C:\DOS` subdir;
/// KERNEL.SYS / CONFIG.SYS / AUTOEXEC.BAT / LICENSE.TXT and any user/runner file
/// stay at the root. Metadata only — never reads host file contents. Cluster fields
/// are zero here; cluster assignment happens after construction.
pub(crate) fn build_tree(root: &Path, system_files: &[(String, Vec<u8>)]) -> HostTree {
    let mut names = NameTable::new();
    let mut dir = TreeDir {
        host_path: root.to_path_buf(),
        ..TreeDir::default()
    };
    // The synthetic C:\DOS folder. Its files are all InMemory and covered by
    // `system_names`, so reconcile classifies them Skip and never materializes them:
    // this folder never touches the host filesystem unless the guest itself writes a
    // non-system file into C:\DOS.
    // A guest write into C:\DOS would try to materialize under root/DOS/<file>;
    // atomic_write holds it if that host directory is absent. That is sufficient
    // while this system folder remains read-only.
    let mut dos = TreeDir {
        host_path: root.join("DOS"),
        ..TreeDir::default()
    };

    // System files, with their canonical 8.3 names, split by placement.
    for (name, bytes) in system_files {
        let n = fold_literal_83(name);
        let file = TreeFile {
            name: n,
            source: FileSource::InMemory(bytes.clone()),
            first_cluster: 0,
            cluster_count: 0,
        };
        if DOS_FOLDER_BINARIES
            .iter()
            .any(|b| name.eq_ignore_ascii_case(b))
        {
            dos.files.push(file);
        } else {
            names.reserve(n); // root system files must not be shadowed by a host file
            dir.files.push(file);
        }
    }

    // Attach the C:\DOS folder (reserving its name at the root so a host subfolder
    // named DOS can't collide), but only when it actually holds binaries — an empty
    // overlay (e.g. a test with no system files) leaves the root free of a stray DOS.
    if !dos.files.is_empty() {
        let dos_name = fold_literal_83("DOS");
        names.reserve(dos_name);
        dir.subdirs.push(TreeSubdir {
            name: dos_name,
            dir: dos,
        });
    }

    walk_into(root, &mut dir, &mut names, 1);
    HostTree { root: dir }
}

// `walk_into` is both the root entry (names already holds the system
// reservations) and the per-subdirectory recursion; each subdirectory gets its
// own fresh `NameTable` at the call site.
fn walk_into(host: &Path, dir: &mut TreeDir, names: &mut NameTable, depth: usize) {
    if depth > MAX_DEPTH {
        // A too-deep folder is truncated rather than recursed forever; warn once
        // at the cap so the loss is not silent.
        eprintln!("katea: directory tree deeper than {MAX_DEPTH}; truncating");
        return;
    }
    let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(host) {
        Ok(rd) => rd.filter_map(Result::ok).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        // metadata (not symlink_metadata): we already skip symlinks below.
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue; // No symlink following, which also avoids loops.
        }
        let path = e.path();
        if ft.is_dir() {
            let name = names.add_host(&path, true);
            let mut child = TreeDir {
                host_path: path.clone(),
                ..TreeDir::default()
            };
            let mut child_names = NameTable::new(); // a fresh table per directory
            walk_into(&path, &mut child, &mut child_names, depth + 1);
            dir.subdirs.push(TreeSubdir { name, dir: child });
        } else if ft.is_file() {
            let Ok(md) = e.metadata() else { continue };
            // Skip any file too large for a FAT32 32-bit size/cluster span, exactly
            // as `KateaVolume::new` does (`katea_volume.rs`): a `>= 4 GiB` file
            // can't be represented (the directory `size` field is u32), and letting
            // it through would also clamp `size` and overflow cluster sizing. A
            // 4 GiB unit-test fixture is impractical, so this mirrors the flat
            // volume's checked behavior.
            if md.len() >= u32::MAX as u64 {
                eprintln!(
                    "katea: skipping {} (>= 4 GiB, not FAT32-representable)",
                    path.display()
                );
                continue;
            }
            let name = names.add_host(&path, false);
            dir.files.push(TreeFile {
                name,
                source: FileSource::HostFile {
                    path,
                    len: md.len(),
                },
                first_cluster: 0,
                cluster_count: 0,
            });
        }
        // Non-regular (device/fifo/etc.) is neither dir nor file -> skipped.
    }
}

/// Fold a known-valid 8.3 system file name like "KERNEL.SYS" to the 11-byte
/// field (split on the dot, uppercase, space-pad). The caller guarantees 8.3.
fn fold_literal_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let b = base.as_bytes();
    let x = ext.as_bytes();
    out[..b.len().min(8)].copy_from_slice(&b[..b.len().min(8)]);
    out[8..8 + x.len().min(3)].copy_from_slice(&x[..x.len().min(3)]);
    out
}

/// One sector as served, plus whether serving it DEGRADED.
///
/// `degraded` is true when `bytes` is the all-zero fallback this module
/// substitutes for a host-side failure — a spilled guest write whose payload
/// could not be read back, a host file that vanished, or one that was truncated
/// underneath a live handle — rather than the sector's real content.
///
/// It exists because of what sits ABOVE this module: `AtaDisk` keeps a host-side
/// LRU sector cache and fills it from whatever the backing returns. Without this
/// flag a transient failure would be made PERMANENT — the zeros would be cached
/// and served on every later read of that LBA even after the host file came
/// back, defeating the retry design this module already has (a failed read drops
/// the cached handle so the next sector re-opens, and `reconcile` brackets read
/// errors rather than acting on them). The disk skips the cache fill when this
/// is set.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SectorRead {
    pub(crate) bytes: [u8; SECTOR],
    pub(crate) degraded: bool,
}

impl SectorRead {
    /// A sector served from real content.
    fn ok(bytes: [u8; SECTOR]) -> Self {
        Self {
            bytes,
            degraded: false,
        }
    }

    /// The zero fallback for a failed read.
    fn degraded() -> Self {
        Self {
            bytes: [0u8; SECTOR],
            degraded: true,
        }
    }
}

/// The synthesized volume's FAT32 geometry, for the profile report.
///
/// `spc` is the load-time number: it is DERIVED from the folder's size, so a
/// fixture folder and the committed image can disagree, and at `spc = 1` a
/// 44 MB file's cluster chain is ~90,000 FAT entries that DOS re-walks per seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KateaGeometryReport {
    pub sectors_per_cluster: u8,
    pub fat_sectors: u32,
    pub partition_sectors: u32,
    pub total_sectors: u32,
    pub count_of_clusters: u32,
}

/// The synthesized disk's geometry, derived from the tree's cluster needs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Geometry {
    pub spc: u8,
    pub fatsz: u32,
    pub part_start: u32, // = PART_START
    pub part_sectors: u32,
    pub total_sectors: u32,     // whole disk
    pub first_data_sector: u32, // partition-relative; cluster 2 begins here
    pub count_of_clusters: u32,
}

/// Entries a directory needs: its files + subdirs, plus `.`/`..` for non-root.
fn entry_count(dir: &TreeDir, is_root: bool) -> u32 {
    let dot = if is_root { 0 } else { 2 };
    dot + dir.files.len() as u32 + dir.subdirs.len() as u32
}

/// Clusters a chain of `bytes` needs at this cluster size (>=1, even for empty).
fn clusters_for(bytes: u64, cluster_bytes: u32) -> u32 {
    (bytes.div_ceil(u64::from(cluster_bytes)) as u32).max(1)
}

/// The FAT32 data-cluster ceiling (the largest valid `count_of_clusters`); a host
/// folder demanding more than this can't be a single FAT32 volume. Per fatgen103
/// the FAT32 region tops out at `0x0FFF_FFF5` clusters; we cap at the
/// conservative `0x0FFF_FFF4` (the kernel/Windows ceiling) so the run-of-1
/// next-cluster encoding never collides with the reserved EOC/bad markers.
const FAT32_MAX_CLUSTERS: u64 = 0x0FFF_FFF4;

/// Sum the cluster needs of the whole tree, pick the geometry that fits, then
/// assign first_cluster/cluster_count across the tree depth-first. The root is
/// cluster 2.
///
/// The cluster size (`spc`) the FAT and partition are sized with must match the
/// one `sectors_per_cluster` derives from the final partition size, or the BPB
/// is internally inconsistent and the disk won't boot. We reach that fixed point
/// by computing the *final* partition size each iteration (not a padded guess)
/// and re-deriving `spc` from it; if it disagrees we adopt the larger and redo.
/// `sectors_per_cluster`'s table is monotonic in size and the partition only
/// grows with `spc`, so the loop climbs the table at most a few steps and stops.
pub(crate) fn allocate(tree: &mut HostTree) -> Result<Geometry, std::io::Error> {
    // Recompute the cluster demand for whatever cluster size the loop is trying:
    // bigger clusters pack the same bytes into fewer clusters, so the demand
    // shrinks as `spc` climbs (a 500 GB folder needs ~1e9 clusters at spc=1 but
    // only ~16M at spc=64). Computed in `u64` so a huge folder can't overflow.
    let geo = fat32_geometry_for(|cluster_bytes| tree_cluster_demand(tree, cluster_bytes))?;
    // Assign clusters now that geometry is fixed.
    let cluster_bytes = u32::from(geo.spc) * SECTOR as u32;
    let mut next = ROOT_CLUSTER; // 2
    assign_dir(&mut tree.root, true, 0, &mut next, cluster_bytes);
    Ok(geo)
}

/// The largest FAT32 cluster size, in sectors. `sectors_per_cluster` tops out
/// here; once we are at this band there is no larger band to climb to, so a folder
/// that still doesn't fit is genuinely too large for one FAT32 volume.
const MAX_SPC: u8 = 64;

/// The next-larger valid `spc` band after `spc` (1 -> 8 -> 16 -> 32 -> 64),
/// mirroring `sectors_per_cluster`'s table. Used when a candidate band can't hold
/// the data: climb to the next band rather than erroring, unless already at the
/// top (`MAX_SPC`).
fn next_spc(spc: u8) -> u8 {
    match spc {
        1 => 8,
        8 => 16,
        16 => 32,
        _ => MAX_SPC, // 32 -> 64, and 64 stays 64 (the caller stops there)
    }
}

/// Pick a self-consistent, boot-valid FAT32 geometry for a tree. `demand_at`
/// returns the tree's data-cluster demand at a given cluster size in bytes; the
/// loop re-queries it each iteration because bigger clusters need fewer of them.
/// Returns `Err` only when the folder doesn't fit even at the largest cluster size
/// (`MAX_SPC` = 64, i.e. roughly > 8 TB of data) — `count_of_clusters` past
/// `FAT32_MAX_CLUSTERS` or a sector count past `u32::MAX` at a *smaller* `spc` just
/// means "climb to a bigger cluster", not "fail". All sizing is done in `u64`; the
/// final values are range-checked once before being narrowed into `u32`.
///
/// The cluster size (`spc`) the FAT and partition are sized with must match the
/// one `sectors_per_cluster` derives from the final partition size, or the BPB is
/// internally inconsistent and the disk won't boot. We reach that fixed point by
/// computing the *final* partition size each iteration (not a padded guess) and
/// re-deriving `spc` from it; if it disagrees we adopt the larger and redo. Both
/// climbs are monotone in `spc` and the per-spc partition size is ~invariant in
/// `spc`, so the loop climbs the table at most a few steps and stops.
fn fat32_geometry_for(demand_at: impl Fn(u32) -> u64) -> Result<Geometry, std::io::Error> {
    let too_large =
        || std::io::Error::other("Katea: host folder too large for a single FAT32 volume");
    let mut spc: u8 = 1;
    loop {
        // The demand for THIS cluster size: the only honest figure to size from.
        let used_data = demand_at(u32::from(spc) * SECTOR as u32);
        // Need a valid FAT32; pad with headroom (25%) so DIR shows free space and
        // writes have room, and floor the whole partition at MIN_PART_SECTORS so
        // no derived volume lands on the degenerate 512-byte-cluster band. The
        // floor is expressed in clusters here because that is what this loop
        // sizes with; at spc=1 it forces a partition the table reads back as
        // spc=8, and the self-consistency check below climbs there in one step.
        // All in u64 so the +25% can't overflow before the checks below.
        let needed = used_data.max(1);
        let floor_clusters = u64::from(MIN_PART_SECTORS).div_ceil(u64::from(spc));
        let count_of_clusters = (needed + needed / 4).max(floor_clusters);
        // Too many clusters / too many data sectors for THIS band: if a bigger
        // cluster size exists, climb to it (it shrinks the cluster count); only at
        // the top band (MAX_SPC) does this mean the folder is genuinely too large.
        let data_sectors = count_of_clusters * u64::from(spc);
        if count_of_clusters > FAT32_MAX_CLUSTERS || data_sectors > u64::from(u32::MAX) {
            if spc < MAX_SPC {
                spc = next_spc(spc);
                continue;
            }
            return Err(too_large());
        }
        let data_sectors = data_sectors as u32;
        // Size the FAT from the whole partition it lives in, exactly as the flat volume does
        // (`fat_size_sectors(PART_SECTORS, spc)`): the formula's divisor accounts
        // for the FAT sectors, so passing the full partition is self-correcting.
        // We do not know `fatsz` until we size the partition, and the partition
        // size includes the FAT — so close the loop by re-deriving `fatsz` from
        // the partition built with the previous estimate until it is stable (it
        // settles in one or two steps because the data region dominates).
        let mut fatsz = fat_size_sectors(u32::from(RESERVED_SECTORS) + data_sectors, spc);
        loop {
            let part = u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * fatsz + data_sectors;
            let next_fatsz = fat_size_sectors(part, spc);
            if next_fatsz == fatsz {
                break;
            }
            fatsz = next_fatsz;
        }
        let used = u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * fatsz;
        // Validate the partition (and whole-disk) sector counts fit u32 in u64
        // before narrowing; same climb-vs-error rule as above.
        let part_sectors = u64::from(used) + u64::from(data_sectors);
        let total_sectors = u64::from(PART_START) + part_sectors;
        if total_sectors > u64::from(u32::MAX) {
            if spc < MAX_SPC {
                spc = next_spc(spc);
                continue;
            }
            return Err(too_large());
        }
        let part_sectors = part_sectors as u32;
        // Self-consistency: the spc the table picks for THIS partition must equal
        // the spc we sized with. If not, climb to it and recompute from scratch.
        let derived = sectors_per_cluster(part_sectors);
        if derived != spc {
            spc = derived;
            continue;
        }
        let geo = Geometry {
            spc,
            fatsz,
            part_start: PART_START,
            part_sectors,
            total_sectors: total_sectors as u32,
            first_data_sector: used,
            count_of_clusters: count_of_clusters as u32,
        };
        debug_assert_eq!(sectors_per_cluster(geo.part_sectors), geo.spc);
        return Ok(geo);
    }
}

/// Total data clusters the tree consumes (directories + files), for sizing.
/// Summed in `u64` so a multi-terabyte host folder can't overflow before the
/// caller (`fat32_geometry_for`) checks it against `FAT32_MAX_CLUSTERS`.
fn tree_cluster_demand(tree: &HostTree, cluster_bytes: u32) -> u64 {
    fn dir_demand(dir: &TreeDir, is_root: bool, cluster_bytes: u32) -> u64 {
        let mut n = u64::from(clusters_for(
            u64::from(entry_count(dir, is_root)) * 32,
            cluster_bytes,
        ));
        for f in &dir.files {
            n += u64::from(clusters_for(f.source.len(), cluster_bytes));
        }
        for s in &dir.subdirs {
            n += dir_demand(&s.dir, false, cluster_bytes);
        }
        n
    }
    dir_demand(&tree.root, true, cluster_bytes)
}

/// Depth-first: assign this directory's chain, then its files' chains, then
/// recurse into subdirectories. `parent` is the parent dir's first cluster.
fn assign_dir(dir: &mut TreeDir, is_root: bool, parent: u32, next: &mut u32, cluster_bytes: u32) {
    dir.first_cluster = *next;
    dir.parent_first_cluster = parent;
    dir.cluster_count = clusters_for(u64::from(entry_count(dir, is_root)) * 32, cluster_bytes);
    *next += dir.cluster_count;
    for f in &mut dir.files {
        f.first_cluster = *next;
        f.cluster_count = clusters_for(f.source.len(), cluster_bytes);
        *next += f.cluster_count;
    }
    for s in &mut dir.subdirs {
        let parent_fc = dir.first_cluster;
        assign_dir(&mut s.dir, false, parent_fc, next, cluster_bytes);
    }
}

/// The computed-on-demand FAT. Every chain is assigned as one *contiguous
/// run* of clusters, so a used cluster's FAT entry is simply `c + 1` unless `c`
/// is the last cluster of its run (then EOC), and any cluster the tree never
/// touched is free (0). We therefore store only the set of run-end clusters and
/// `next_free` (the first never-allocated cluster) and derive every FAT entry —
/// and any FAT sector — from those, so RAM scales with the chain count rather
/// than the disk size.
#[derive(Debug)]
pub(crate) struct ClusterIndex {
    next_free: u32,           // first cluster never allocated
    chain_ends: HashSet<u32>, // last cluster of every chain (-> EOC)
}

impl ClusterIndex {
    pub(crate) fn build(tree: &HostTree, _geo: &Geometry) -> Self {
        let mut chain_ends = HashSet::new();
        let mut next_free = ROOT_CLUSTER;
        fn visit(dir: &TreeDir, ends: &mut HashSet<u32>, next_free: &mut u32) {
            push_run(dir.first_cluster, dir.cluster_count, ends, next_free);
            for f in &dir.files {
                push_run(f.first_cluster, f.cluster_count, ends, next_free);
            }
            for s in &dir.subdirs {
                visit(&s.dir, ends, next_free);
            }
        }
        fn push_run(first: u32, count: u32, ends: &mut HashSet<u32>, next_free: &mut u32) {
            if count == 0 {
                return;
            }
            ends.insert(first + count - 1);
            *next_free = (*next_free).max(first + count);
        }
        visit(&tree.root, &mut chain_ends, &mut next_free);
        Self {
            next_free,
            chain_ends,
        }
    }

    /// The FAT entry value for cluster `c` (28-bit).
    pub(crate) fn fat_entry(&self, c: u32) -> u32 {
        match c {
            0 => FAT0_MEDIA,
            1 => FAT32_EOC,
            _ if c < self.next_free => {
                if self.chain_ends.contains(&c) {
                    FAT32_EOC
                } else {
                    c + 1
                }
            }
            _ => 0, // free
        }
    }

    /// One 512-byte sector of a FAT copy: the `sector`-th sector holds entries
    /// `[sector*128 .. sector*128+128)`. Past the entries it is zero.
    pub(crate) fn fat_sector(&self, sector: u32, _geo: &Geometry) -> [u8; SECTOR] {
        let mut out = [0u8; SECTOR];
        let base = sector * 128; // 128 FAT32 entries per 512B sector
        for i in 0..128u32 {
            let v = (self.fat_entry(base + i) & 0x0FFF_FFFF).to_le_bytes();
            let off = (i as usize) * 4;
            out[off..off + 4].copy_from_slice(&v);
        }
        out
    }

    pub(crate) fn next_free(&self) -> u32 {
        self.next_free
    }
}

/// The FAT subdirectory attribute (ATTR_DIRECTORY); files use `ATTR_ARCHIVE`.
const ATTR_SUBDIR: u8 = 0x10;

/// Build the 32-byte directory entries for `dir` in directory order:
/// `.`/`..` first (non-root only), then files (archive attr), then
/// subdirectories (0x10). `.` points at this directory's own first cluster and
/// `..` at the parent's; a subdir entry points at the child's first cluster.
fn dir_entries(dir: &TreeDir, is_root: bool) -> Vec<[u8; 32]> {
    let mut out: Vec<[u8; 32]> = Vec::new();
    if !is_root {
        let dot = *b".          ";
        out.push(fat32_dir_entry(
            &dot,
            ATTR_SUBDIR,
            dir.first_cluster,
            0,
            0,
            0,
        ));
        // Canonical FAT (fatgen103 6.5; cf. `fat32::fat32_dot_entries`): `..` points
        // at the parent's first cluster, EXCEPT when the parent is the root, where
        // it must be 0 (the root has no real cluster number to name). The root is
        // always cluster 2 (ROOT_CLUSTER), so a parent of 2 means "root".
        let dotdot = *b"..         ";
        let dotdot_cluster = if dir.parent_first_cluster == ROOT_CLUSTER {
            0
        } else {
            dir.parent_first_cluster
        };
        out.push(fat32_dir_entry(
            &dotdot,
            ATTR_SUBDIR,
            dotdot_cluster,
            0,
            0,
            0,
        ));
    }
    for f in &dir.files {
        let size = u32::try_from(f.source.len()).unwrap_or(u32::MAX);
        out.push(fat32_dir_entry(
            &f.name,
            ATTR_ARCHIVE,
            f.first_cluster,
            0,
            0,
            size,
        ));
    }
    for s in &dir.subdirs {
        out.push(fat32_dir_entry(
            &s.name,
            ATTR_SUBDIR,
            s.dir.first_cluster,
            0,
            0,
            0,
        ));
    }
    out
}

/// One 512-byte sector (the `sector`-th, 16 entries) of `dir`'s directory data,
/// zero-padded past the last entry. `sector` indexes into the directory's entry
/// list 16 entries at a time, so the >16-entry (multi-cluster) case is served by
/// the later sectors.
///
/// Test-only: the live read path serves directory sectors via `data_sector`,
/// which inlines this slice math over the precomputed `FlatDir::entries`. The
/// module tests exercise this standalone helper directly.
#[allow(dead_code)]
pub(crate) fn dir_sector(dir: &TreeDir, is_root: bool, sector: u32) -> [u8; SECTOR] {
    let entries = dir_entries(dir, is_root);
    let mut out = [0u8; SECTOR];
    let start = (sector as usize) * 16;
    for i in 0..16usize {
        if let Some(e) = entries.get(start + i) {
            out[i * 32..i * 32 + 32].copy_from_slice(e);
        }
    }
    out
}

/// A flattened directory: just its precomputed 32-byte entries. The entries are
/// small (32 bytes each), so holding them costs RAM proportional to the entry
/// count, not the disk size. The cluster span this directory occupies lives in
/// the `runs` table, so it is not duplicated here.
#[derive(Debug)]
struct FlatDir {
    entries: Vec<[u8; 32]>,
}

/// A flattened file: its (cloned) source and byte size. The source is
/// `FileSource::HostFile { path, len }` for host files (lazy, no slurp) or
/// `InMemory` for the overlaid system files. Its cluster span lives in `runs`.
#[derive(Debug)]
struct FlatFile {
    source: FileSource,
    size: u32,
}

/// What a cluster run holds, indexing into `dirs`/`files` (no pointers).
#[derive(Debug)]
enum Role {
    Dir(usize),
    File(usize),
}

/// A lazy whole-disk FAT32 volume over a recursive host-folder tree.
/// The sibling of the flat `KateaVolume`, generalized to a full directory tree:
/// FAT and directory sectors are computed on demand and file data is read lazily,
/// so RAM scales with the entry count rather than the disk or file sizes.
///
/// The struct is **pointer-free**: it owns only `Vec`s (the flattened dirs/files
/// and the sorted cluster-run table), the two stamped boot sectors, the geometry,
/// and the cluster index. `tree` is kept whole for tests and writes; it is
/// not consulted by `read_sector` — that path resolves a cluster through `runs`.
#[derive(Debug)]
pub(crate) struct KateaTreeVolume {
    /// The owned tree; kept for tests and writes, not read by `read_sector`.
    #[allow(dead_code)]
    tree: HostTree,
    geo: Geometry,
    /// LBA 0: the MBR with the partition entry + 0x55AA stamped in.
    mbr: [u8; SECTOR],
    /// The FAT32 VBR (at PART_START) with the BPB stamped over the boot code.
    vbr: [u8; SECTOR],
    /// FSInfo free-cluster count, served at both FSInfo sectors.
    free_count: u32,
    /// FSInfo next-free hint (= `ClusterIndex::next_free`).
    next_free: u32,
    /// The computed FAT (run-end set + next_free); generates any FAT sector.
    fat: ClusterIndex,
    /// Flattened directories, indexed by `Role::Dir`.
    dirs: Vec<FlatDir>,
    /// Flattened files, indexed by `Role::File`.
    files: Vec<FlatFile>,
    /// `(first_cluster, last_cluster, role)` runs, sorted by `first_cluster`.
    runs: Vec<(u32, u32, Role)>,
    /// Guest writes land here and reads consult it first. A bounded RAM cache over
    /// a spill file (`katea_store`), so RAM tracks the cache size rather than the
    /// bytes written this session. Presence is exact and outlives eviction, which
    /// is what `reconcile`'s touched-this-session tests read.
    store: crate::katea_store::SectorStore,
    /// Directory first-cluster -> its host-filesystem path. Seeded from the tree;
    /// extended on guest MKDIR. Reconcile materializes a file in this directory to
    /// `host_path / 8.3-name`.
    dir_paths: HashMap<u32, PathBuf>,
    /// What Katea believes currently exists on the host, keyed by
    /// `(parent dir first-cluster, folded 8.3 name)`. Seeded from the tree at
    /// mount (every host file + subdir) and maintained by reconcile. Subsumes the
    /// `existing_files` (host_path for case-correct overwrite) and `materialized`
    /// (content-fingerprint dedupe). The root dir has no entry (it has no parent).
    mirrored: HashMap<(u32, [u8; 11]), MirrorEntry>,
    /// Current valid guest file chains. Projected payload sectors can leave the
    /// overlay because reads resolve these clusters back through the host file.
    projected_files: HashMap<(u32, [u8; 11]), ProjectedFile>,
    projected_clusters: HashMap<u32, ((u32, [u8; 11]), u32)>,
    blocked_projection_keys: HashSet<(u32, [u8; 11])>,
    /// Open host handles reused by positioned writes until guest flush or eject.
    host_write_handles: HashMap<PathBuf, File>,
    directory_clusters: HashMap<u32, u32>,
    batch_lbas: HashSet<u32>,
    /// Guest data sectors no projection could map, indexed by the data cluster
    /// that holds them. Flat, this was one set of every such sector ever
    /// written, rebuilt and re-examined on every commit: a sequential install
    /// whose payload outruns its FAT paid a sweep proportional to everything
    /// written so far, so the session cost grew with the square of its size.
    /// A sector only stops being unmapped when its own cluster becomes
    /// projectable, so a commit needs the clusters it touched and the clusters
    /// a projection just claimed -- never the whole set.
    unmapped_by_cluster: HashMap<u32, HashSet<u32>>,
    /// `unmapped_by_cluster`'s total, maintained rather than recounted. Every
    /// entry is pending by construction: the only path that clears a sector's
    /// pending bit (`stream_file_overlay`) drops it from the index in the same
    /// breath.
    unmapped_sectors: u64,
    /// Clusters whose held sectors have earned another look, recorded by the
    /// three events that can change a held sector's verdict without the next
    /// commit writing anywhere near it: `install_projection` gives a cluster an
    /// owner, the live scan drops a key's block, and a failed host write leaves
    /// sectors held whose cluster was projectable all along. The next commit
    /// revisits exactly these and nothing else.
    newly_projectable_clusters: HashSet<u32>,
    batch_sector_writes: u64,
    directory_dirty: bool,
    metadata_reconcile_pending: bool,
    fat_dirty_clusters: HashSet<u32>,
    host_io_failed: bool,
    /// Folded 8.3 names of the InMemory boot files; never materialized to the host.
    system_names: HashSet<[u8; 11]>,
    /// How many candidate sectors `stream_projected_batch` has examined. The
    /// unmapped index exists to keep this proportional to the commit rather
    /// than to the session, and a test reads it to prove that.
    #[cfg(test)]
    candidate_lbas_examined: u64,
    /// Files whose bytes `reconcile` has re-read from the disk. Counts the work the
    /// `last_gather` skip exists to avoid, so a test can prove the skip fires.
    gathers: u64,
    /// Exact file states whose most recent gather or host write failed. An inline
    /// reconcile retries only the same state; a later guest write makes it an
    /// ordinary changing file again and lets a later pass retry it.
    retry_gathers: HashMap<(u32, [u8; 11]), FileState>,
    #[cfg(test)]
    gathered_bytes: u64,
    #[cfg(test)]
    atomic_writes: u64,
    #[cfg(test)]
    atomic_write_bytes: u64,
    /// Read-path attribution for the boot profiler. `Cell`s because `read_sector`
    /// is `&self`; the emulation thread owns the volume, so no sharing is implied.
    /// One cell PER FIELD -- see [`katea_counter_block`] for why the single
    /// whole-block `Cell` this replaced was a 352-byte copy per increment.
    counters: KateaCounterCells,
    /// One open read handle for the host file most recently served, so a
    /// sequential read pays one `File::open` instead of one per 512-byte sector.
    /// A single entry is the right size: DOS reads one file at a time through one
    /// handle, so the hit rate on a game load is ~100%, and a second entry would
    /// only pay off for an access pattern DOS cannot produce.
    ///
    /// `RefCell` for the same reason `counters` is a `Cell`: `read_sector` is
    /// `&self` and the emulation thread owns the volume.
    ///
    /// INVALIDATION: `invalidate_host_reads` clears this, and every host mutation
    /// calls it. A stale handle matters because `File::open` shares delete and
    /// rename, so a replaced file would otherwise keep reading its pre-write
    /// contents.
    ///
    /// A small LRU rather than the single entry it started as: one entry is right
    /// for a pure sequential load but degrades to an open-per-sector as soon as
    /// two files interleave (a program reading its own overlay, or a read landing
    /// between two projections), which is exactly the pattern an asset load
    /// produces. MRU is the last element; the scan is over at most
    /// [`MAX_HOST_READ_HANDLES`] paths.
    host_read_cache: std::cell::RefCell<Vec<(PathBuf, File)>>,
    /// Bytes coalesced only within the active guest read command. The window is
    /// discarded at command completion, so it cannot make an unrequested sector
    /// stale across two INT 13h calls.
    host_read_window: std::cell::RefCell<Option<HostReadWindow>>,
    /// Bytes read ahead of the guest, surviving command boundaries. An LRU of
    /// [`HOST_READAHEAD_SLOTS`] slots, MRU last. See [`HostReadAhead`].
    host_readahead: std::cell::RefCell<Vec<HostReadAhead>>,
    read_command_end_lba: std::cell::Cell<Option<u32>>,
    command_read_batch_enabled: bool,
    /// The read-ahead's fill ceiling, or 0 when `IZARRAVM_HDD_READAHEAD=0`
    /// disarmed it. Read once at mount so an A/B is same-binary.
    readahead_max_bytes: u64,
    /// Memo of the base-view FAT sector most recently synthesized, keyed by its
    /// FAT-relative sector index.
    ///
    /// `ClusterIndex::fat_sector` builds a 512-byte sector by evaluating 128
    /// `fat_entry` values, each a `HashSet<u32>` probe under SipHash, so one
    /// `KateaTreeVolume::fat_entry` call costs ~128 hashed lookups. A cluster
    /// chain walk calls it once per cluster, in ascending cluster order, so 128
    /// consecutive calls land in the same FAT sector: one entry captures the whole
    /// win and a second would never be used. The projection pass walks chains for
    /// every file on the volume three times over, which is what made a one-file
    /// write cost hundreds of milliseconds on a 47 MB folder.
    ///
    /// SOUND BY CONSTRUCTION: `fat` and `geo` are built in `new` and never
    /// mutated, so `fat.fat_sector(within, &geo)` is a pure function of `within`.
    /// The memo sits strictly *below* the overlay: `read_sector_checked` still
    /// consults `store` first and still reports its read errors, so a guest-written
    /// FAT sector never reaches this cache and the bytes served are identical.
    fat_sector_cache: std::cell::RefCell<Option<(u32, [u8; SECTOR])>>,
    /// How many base-view FAT sectors have actually been synthesized. The memo
    /// exists to keep this proportional to the FAT region a pass touches rather
    /// than to the clusters it walks, and a test reads it to prove that.
    #[cfg(test)]
    fat_sector_builds: std::cell::Cell<u64>,
    /// How many times a [`FatWalk`] cursor has had to RESOLVE a FAT sector --
    /// that is, ask the store whether the guest wrote it. The whole point of the
    /// cursor is that this is proportional to the FAT sectors a walk crosses
    /// (one per 128 clusters) rather than to the clusters it steps through, and a
    /// test reads it to prove that.
    #[cfg(test)]
    fat_walk_resolves: std::cell::Cell<u64>,
    /// Cluster chains already walked, keyed by first cluster. See [`ChainMemo`]
    /// for the validity argument and [`CHAIN_MEMO_MAX_ENTRIES`] for the bound.
    /// `RefCell` because `chain_of` is `&self`.
    chain_memo: std::cell::RefCell<HashMap<u32, ChainMemo>>,
    /// Monotone counter of guest writes to the FAT region. Also the "nothing at
    /// all has changed" fast path: a memo taken at the current epoch needs no
    /// per-sector check.
    fat_epoch: u64,
    /// The epoch at which each FAT sector was last written by the guest,
    /// FAT-copy-relative (so a write to either copy expires both, which is the
    /// conservative direction). Absent means never.
    fat_sector_epoch: HashMap<u32, u64>,
    /// The incremental claim index: what each live directory entry claims, and
    /// the reverse map. See [`KeyClaim`] and
    /// [`KateaTreeVolume::ambiguous_incrementally`].
    claim_by_key: HashMap<(u32, [u8; 11]), KeyClaim>,
    claimants: HashMap<u32, Claimants>,
    /// False when the index cannot be trusted and the next pass must rebuild it
    /// from a full scan. Set by every case in which we do not know what changed.
    claims_valid: bool,
    /// Pass counter, so the amortised forced refresh rotates deterministically
    /// rather than depending on `HashMap` iteration order.
    claims_pass: usize,
    /// `IZARRAVM_KATEA_INCREMENTAL_CLAIMS=0` forces the full scan every pass:
    /// the same-binary A/B control leg, and the field bisect if a mounted folder
    /// ever comes back wrong. Read once at mount.
    incremental_claims: bool,
    /// `IZARRAVM_KATEA_CLAIMS_VERIFY=1` computes BOTH and prefers the full scan
    /// on any disagreement, saying so loudly. Slower by construction; it is what
    /// lets a real fixture row grade the index, which no unit test can.
    claims_verify: bool,
    /// Chain walks actually performed, and memo hits, since mount. The memo
    /// exists to keep the first proportional to what CHANGED rather than to the
    /// mounted folder, and a test reads both to prove it.
    #[cfg(test)]
    chain_walks: std::cell::Cell<u64>,
    #[cfg(test)]
    chain_memo_hits: std::cell::Cell<u64>,
    /// Claim insertions performed, and live keys skipped because their claim was
    /// still current. The whole point of the index is that the first is
    /// proportional to what CHANGED; a test reads both.
    #[cfg(test)]
    claim_inserts: std::cell::Cell<u64>,
    #[cfg(test)]
    claim_skips: std::cell::Cell<u64>,
    /// Disagreements the verify harness has seen. Behaviourally invisible (the
    /// harness prefers the reference), so only a test can observe one.
    #[cfg(test)]
    claim_divergences: std::cell::Cell<u64>,
    /// Test-only fault injection: stop stamping `fat_sector_epoch`, so the claim
    /// index goes stale on purpose. There is no production path that does this;
    /// it exists to prove the FAILURE DIRECTION is a hold and not a clobber.
    #[cfg(test)]
    suppress_epoch_stamps: bool,
    /// Test-only: turn OFF the amortised forced refresh. It exists as insurance
    /// against a hook nobody anticipated, and that is exactly what makes it a
    /// hazard in a fixture: on a volume with a handful of keys the rotation
    /// reaches every one of them within a few passes, so a fixture aimed at ONE
    /// invalidation hook silently passes when that hook is deleted, because the
    /// insurance re-derived the claim anyway. Four such mutants survived before
    /// this switch existed. Every hook-isolating fixture turns it off; the
    /// fixture for the refresh itself leaves it on.
    #[cfg(test)]
    amortised_refresh: bool,
    /// Arms the FAT / directory / free-space region census. Read once at mount
    /// from `IZARRAVM_KATEA_REGION_CENSUS=1`, and OFF by default: those two
    /// counters were investigation residue, and an increment is still a branch
    /// and a read-modify-write on the per-sector read path. House discipline is
    /// that a default-off instrument is gated at its call site, not inside the
    /// helper it calls.
    region_census: bool,
}

/// Whether a new volume counts FAT/directory/free-space sectors. See
/// [`KateaTreeVolume::region_census`].
fn region_census_enabled() -> bool {
    std::env::var("IZARRAVM_KATEA_REGION_CENSUS").as_deref() == Ok("1")
}

/// Enabled by default; `IZARRAVM_KATEA_INCREMENTAL_CLAIMS=0` forces the full
/// anti-clobber scan every pass. Same-binary control leg and field bisect.
fn incremental_claims_enabled() -> bool {
    std::env::var("IZARRAVM_KATEA_INCREMENTAL_CLAIMS").as_deref() != Ok("0")
}

/// Off by default; `IZARRAVM_KATEA_CLAIMS_VERIFY=1` computes the incremental
/// answer AND the full scan every pass and prefers the full scan on any
/// disagreement. Deliberately slow.
fn claims_verify_enabled() -> bool {
    std::env::var("IZARRAVM_KATEA_CLAIMS_VERIFY").as_deref() == Ok("1")
}

/// Enabled by default; `=0` provides a same-binary benchmark control.
fn command_read_batch_enabled() -> bool {
    std::env::var("IZARRAVM_HDD_COMMAND_READ_BATCH").as_deref() != Ok("0")
}

/// The read-ahead's fill ceiling, or 0 when it is disarmed. Enabled by default;
/// `IZARRAVM_HDD_READAHEAD=0` provides a same-binary control.
fn readahead_max_bytes() -> u64 {
    if std::env::var("IZARRAVM_HDD_READAHEAD").as_deref() == Ok("0") {
        0
    } else {
        HOST_READAHEAD_MAX_BYTES
    }
}

/// Declare the Katea counter block ONCE, in two shapes.
///
/// [`KateaStorageCounters`] is the public, plain, `Copy` report the boot profiler
/// subtracts field by field. [`KateaCounterCells`] is what the volume actually
/// stores: the same names, each its own `Cell<u64>`.
///
/// The split exists because the counters used to live in a single
/// `Cell<KateaStorageCounters>`, and `Cell::get` + `Cell::set` around one `+= 1`
/// is a read-modify-write of the WHOLE block -- 44 `u64` fields, 352 bytes,
/// copied twice per bump. There are bumps on the per-sector read path, inside
/// `read_host_span`, and (before the FAT walk was fixed) once per cluster of
/// every chain walk in the projection pass, so the block copy was ~2.1 KB per
/// served sector and ~704 bytes per FAT entry. Per-field cells make a bump one
/// 8-byte read-modify-write of one cell, and nothing else in the block is
/// touched.
///
/// Generating both from one field list is what keeps them from drifting: a new
/// counter cannot be added to the report without also getting its cell, and
/// `snapshot` cannot forget it.
macro_rules! katea_counter_block {
    ($( $(#[$meta:meta])* $name:ident ),* $(,)?) => {
        /// What the Katea host-folder read path did, for the boot profiler's disk phases.
        ///
        /// Deliberately counters rather than a `MachineProfilePhaseKind`: the host reads
        /// below happen inside the INT 13h service, which is already timed as `SoftInt`,
        /// so a phase would nest and double-count in `classified_wall_ns`.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct KateaStorageCounters {
            $( $(#[$meta])* pub $name: u64, )*
        }

        /// The live counters, one `Cell` per field. See [`katea_counter_block`].
        #[derive(Debug, Default)]
        pub(crate) struct KateaCounterCells {
            $( pub(crate) $name: std::cell::Cell<u64>, )*
        }

        impl KateaCounterCells {
            /// The plain report, for `storage_counters` and the profile emitters.
            pub(crate) fn snapshot(&self) -> KateaStorageCounters {
                KateaStorageCounters { $( $name: self.$name.get(), )* }
            }
        }
    };
}

/// Add `n` to one counter cell, saturating. The saturation matters at exactly
/// one place -- `host_bytes` and friends accumulate sizes -- and costs nothing
/// where it does not.
#[inline]
fn bump(cell: &std::cell::Cell<u64>, n: u64) {
    cell.set(cell.get().saturating_add(n));
}

katea_counter_block! {
    /// Sectors the facade RESOLVED, from any source.
    ///
    /// Not "served to the guest": most of these are not guest reads at all. The
    /// reconcile pass resolves a FAT sector per cluster of every chain it walks,
    /// and on a 498 MB folder that was 3.6 M of the 3.7 M counted. Since the
    /// cluster-chain memo landed, a walk served from the memo resolves NONE --
    /// which is why this counter fell 78-96% on the measured rows with every
    /// guest-visible number identical to the digit.
    ///
    /// So it is a work counter for THIS module, not a measure of guest I/O
    /// (`int13_read_sectors` is that), and it is not comparable across builds
    /// whose resolution strategy differs. Nothing gates or pins it; all five
    /// readers display it.
    sector_reads,
    /// Sectors whose bytes came out of a host file.
    host_file_reads,
    /// Logical host-file bytes served to the guest.
    host_bytes,
    /// Physical host read calls and bytes after command-local coalescing.
    host_read_operations,
    host_read_bytes,
    /// Wall nanoseconds spent in physical host reads. Command-window hits do
    /// not enter this counter.
    host_wall_ns,
    /// The longest SINGLE `read_host_span` this session, in nanoseconds.
    ///
    /// A sum cannot see a hitch: 784 ms of projection spread over a minute is
    /// invisible, and 784 ms in one synchronous pass is a visible freeze. The max
    /// is what separates the two, and it is what the read-ahead and the projection
    /// scaling are graded against. Zero new syscalls: this reads the `Instant`
    /// pair the sum already computes.
    host_read_max_ns,
    /// Sectors served out of the cross-command read-ahead buffer (a memcpy, no
    /// host I/O). Derivable from `host_file_reads - host_read_operations` only
    /// while the command window is also in play; counted directly so a test can
    /// name the mechanism it is asserting on.
    host_readahead_hits,
    /// Read-ahead fills: physical host reads that pulled more than the command
    /// asked for, so a later command could be served from RAM.
    host_readahead_fills,
    /// Cluster-run table entries scanned to resolve data sectors. `data_sector`
    /// binary-searches the sorted `runs`, so this is now `log2(runs)` per sector
    /// rather than the tree-size-dependent linear walk it was built to expose.
    run_scan_steps,
    /// Sectors served out of the synthesized FAT region. ZERO unless
    /// `IZARRAVM_KATEA_REGION_CENSUS=1` armed the volume: this is a diagnostic,
    /// gated at its call site so an ordinary run never pays for it.
    ///
    /// The FAT is generated in memory, so these cost no host I/O at all -- but
    /// each one is still a whole guest INT 13h call, and a high count against
    /// `host_file_reads` is the signature of DOS re-walking a cluster chain
    /// rather than of the guest asking for data. That signature is what the
    /// 512-byte-cluster geometry bug looked like from here.
    fat_sector_reads,
    /// Sectors served out of the data region that were NOT file bytes: directory
    /// clusters and free space. Separating these from the FAT count is what tells
    /// a chain walk apart from a directory rescan. Same gate, same default: zero
    /// unless `IZARRAVM_KATEA_REGION_CENSUS=1`.
    dir_or_free_sector_reads,
    /// Host `File::open` calls. Distinct from `host_file_reads` (which counts
    /// SECTORS served from a host file) because the one-entry handle cache makes
    /// them differ: their ratio is what says the cache is working. Before the
    /// cache they were equal by construction.
    host_file_opens,
    sector_writes,
    int13_read_commands,
    int13_read_sectors,
    int13_read_wait_ticks,
    int13_write_commands,
    int13_write_sectors,
    int13_write_wait_ticks,
    pio_read_commands,
    pio_read_sectors,
    pio_read_wait_ticks,
    pio_write_commands,
    pio_write_sectors,
    pio_write_wait_ticks,
    dma_read_commands,
    dma_read_sectors,
    dma_read_wait_ticks,
    dma_write_commands,
    dma_write_sectors,
    dma_write_wait_ticks,
    overlay_resident_sectors,
    overlay_pending_sectors,
    pending_unmapped_sectors,
    spill_operations,
    spill_bytes,
    spill_wall_ns,
    projection_operations,
    projection_bytes,
    projection_wall_ns,
    /// The longest SINGLE projection operation this session, in nanoseconds --
    /// one `commit_guest_write_batch` or one `flush_guest_writes`, each of which
    /// runs synchronously on the emulation thread. See [`Self::host_read_max_ns`]
    /// for why the sum is not enough.
    projection_max_ns,
    metadata_projection_passes,
    host_write_failures,
    /// Directory entries currently HELD by the anti-clobber guard, as a gauge
    /// rather than a total: the set they live in (`blocked_projection_keys`) is
    /// meant to drain, and a non-zero reading at the end of a run means some file
    /// was never projected. Exported because a criterion that cannot be read is
    /// not a criterion.
    blocked_projection_keys,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GuestStorageRoute {
    Int13,
    Pio,
    Dma,
}

pub(crate) type GuestWriteRoute = GuestStorageRoute;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitGuestWriteResult {
    Projected,
    Deferred,
    HostIoFailure,
}

/// Katea's belief about one host-side entry (file or dir): where it lives, the
/// guest cluster its directory entry points at, whether it is a directory, and the
/// last content fingerprint we materialized (None until first written / for a dir).
#[derive(Clone, Debug)]
struct MirrorEntry {
    host_path: PathBuf,
    /// The guest-side first cluster this entry occupies (drives delete/rename detection).
    first_cluster: u32,
    /// True when this entry is a directory.
    is_dir: bool,
    last_fingerprint: Option<u64>,
    /// What the last completed decision for this entry saw, so a later pass can
    /// prove the file cannot have changed and skip re-reading it. `None` means
    /// "always gather", which is the safe default for a fresh or held entry.
    last_gather: Option<Gather>,
}

/// The inputs to one file's gathered bytes, as of the last pass that finished a
/// decision about it (a successful materialize, or a fingerprint match).
///
/// The gathered bytes are a function of the cluster chain, the declared size, the
/// store's copy of any written chain sector, and the base view under any chain
/// sector the guest never wrote. `all_present` records that the fourth input was
/// absent, which is what lets the other three stand in for the whole function.
/// Only ever compared field by field, never trusted as content.
#[derive(Clone, Copy, Debug)]
struct Gather {
    /// The directory entry's declared size at that decision.
    size: u32,
    /// A hash of the cluster chain at that decision.
    chain_id: u64,
    /// The store's write counter at that decision.
    seq: u64,
    /// Whether every sector the gather read came from the store rather than the
    /// base view. When false the skip is never taken, because reconcile's own
    /// `atomic_write` can change the host file that the base view reads from, and
    /// no watermark can see that happen.
    all_present: bool,
}

/// The guest-side identity and write watermark of a file decision. Unlike
/// [`Gather::seq`], `chain_seq` is the current maximum over this file's chain,
/// not the store's global sequence at the end of an earlier decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileState {
    first_cluster: u32,
    size: u32,
    chain_id: u64,
    chain_seq: u64,
}

/// Why reconcile is running. ATA command completion must not mistake an
/// interleaved metadata write for a growing file becoming quiescent. Explicit
/// flush and eject calls, on the other hand, must materialize everything now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconcileMode {
    AfterWrite,
    Final,
}

/// Hash a cluster chain into a comparable id. Cheap and session-only, like
/// `katea_write::fingerprint`: never persisted, only compared within one run.
fn chain_id(chain: &[u32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    chain.hash(&mut h);
    h.finish()
}

/// One live directory entry seen during the gather phase.
struct LiveEntry {
    dir_cluster: u32,
    name: [u8; 11],
    first_cluster: u32,
    size: u32,
    is_dir: bool,
}

#[derive(Clone, Debug)]
struct ProjectedFile {
    path: PathBuf,
    size: u32,
    chain: Vec<u32>,
    /// Payload has been projected past the size the guest's directory entry last
    /// declared. DOS writes clusters long before it publishes the closing size,
    /// so while this is set the entry's size is stale by construction and must
    /// never be used to shorten the host file: the bytes past it have already
    /// been acknowledged out of the store and exist nowhere else. Cleared as
    /// soon as a directory entry catches up to (or past) the projected extent.
    extended_past_directory: bool,
}

struct PendingStream {
    path: PathBuf,
    key: (u32, [u8; 11]),
    first_cluster: u32,
    size: u32,
    chain: Vec<u32>,
    state: FileState,
    sectors: Option<Vec<(u32, u64)>>,
    set_len: bool,
}

/// One file the reconcile pass has decided to materialize: where to write it, the
/// bytes, its `(dir first-cluster, folded name)` fingerprint key, and the content
/// fingerprint recorded on a successful write. Collected during the read phase and
/// drained afterwards so no `&self` borrow is held across the host I/O.
struct PendingWrite {
    path: PathBuf,
    data: Vec<u8>,
    key: (u32, [u8; 11]),
    fingerprint: u64,
    first_cluster: u32,
    /// The exact state whose host write is being attempted. Kept as a retry marker
    /// when `atomic_write` fails.
    state: FileState,
    /// What this gather saw, recorded on the mirror entry only if the write
    /// lands. A held or failed write leaves the entry's watermark alone, so the
    /// next pass gathers again.
    gather: Gather,
    chain: Vec<u32>,
}

/// A disappeared mirrored entry (file or dir) whose clusters are now claimed by
/// exactly one fresh live entry: a rename/move. The host path at `old_path` is
/// `fs::rename`d to `new_path`, and the mirror re-keyed from `old_key` to
/// `new_key`. Collected during the read phase and applied afterwards so no `&self`
/// borrow spans the I/O.
struct PendingRename {
    old_key: (u32, [u8; 11]),
    new_key: (u32, [u8; 11]),
    old_path: PathBuf,
    new_path: PathBuf,
    first_cluster: u32,
    is_dir: bool,
}

/// One disappeared entry the reconcile pass will remove from the host: the mirror
/// key, the host path, the cluster (to drop from `dir_paths` for a dir), and whether
/// it is a directory (remove_dir vs remove_file).
struct PendingDelete {
    key: (u32, [u8; 11]),
    path: PathBuf,
    first_cluster: u32,
    is_dir: bool,
}

impl KateaTreeVolume {
    /// Build the whole-disk view from the boot sectors, a host folder, and the
    /// in-memory system files overlaid at the root. Construction walks metadata
    /// only — it never reads host file *contents* (those are read lazily, one
    /// 512-byte span at a time, in `read_sector`).
    pub(crate) fn new(
        mbr: &[u8; SECTOR],
        vbr: &[u8; SECTOR],
        host_root: &Path,
        system_files: &[(String, Vec<u8>)],
    ) -> Result<Self, std::io::Error> {
        let mut tree = build_tree(host_root, system_files);
        let geo = allocate(&mut tree)?;
        let fat = ClusterIndex::build(&tree, &geo);
        let next_free = fat.next_free();
        // Used data clusters are 2..next_free; the rest of the addressable range
        // is free. `saturating_sub` guards the (impossible) empty-disk underflow.
        let free_count = geo
            .count_of_clusters
            .saturating_sub(next_free - ROOT_CLUSTER);

        // --- MBR: stamp the single partition entry + signature, with the dynamic
        // partition size (mirrors KateaVolume::new but `geo`-driven). ----------
        let mut mbr_out = *mbr;
        let pe = 0x1BE; // first partition entry
        mbr_out[pe] = 0x80; // active / bootable
        mbr_out[pe + 1..pe + 4].copy_from_slice(&lba_to_chs(geo.part_start));
        mbr_out[pe + 4] = PART_TYPE_FAT32_LBA;
        mbr_out[pe + 5..pe + 8].copy_from_slice(&lba_to_chs(geo.part_start + geo.part_sectors - 1));
        mbr_out[pe + 8..pe + 12].copy_from_slice(&geo.part_start.to_le_bytes()); // RelSect
        mbr_out[pe + 12..pe + 16].copy_from_slice(&geo.part_sectors.to_le_bytes()); // NumSect
        mbr_out[0x1FE] = 0x55;
        mbr_out[0x1FF] = 0xAA;

        // --- VBR: stamp the FAT32 BPB over the boot code, keeping the boot code.
        let mut vbr_out = *vbr;
        stamp_fat32_bpb(
            &mut vbr_out,
            geo.spc,
            geo.fatsz,
            geo.part_start,
            geo.part_sectors,
        );

        // --- seed the write maps from the (allocated) tree ----------------------
        let mut dir_paths: HashMap<u32, PathBuf> = HashMap::new();
        let mut mirrored: HashMap<(u32, [u8; 11]), MirrorEntry> = HashMap::new();
        let mut projected_files = HashMap::new();
        let mut projected_clusters = HashMap::new();
        fn seed(
            dir: &TreeDir,
            dir_paths: &mut HashMap<u32, PathBuf>,
            mirrored: &mut HashMap<(u32, [u8; 11]), MirrorEntry>,
            projected_files: &mut HashMap<(u32, [u8; 11]), ProjectedFile>,
            projected_clusters: &mut HashMap<u32, ((u32, [u8; 11]), u32)>,
        ) {
            dir_paths.insert(dir.first_cluster, dir.host_path.clone());
            for f in &dir.files {
                if let FileSource::HostFile { path, .. } = &f.source {
                    let key = (dir.first_cluster, f.name);
                    mirrored.insert(
                        key,
                        MirrorEntry {
                            host_path: path.clone(),
                            first_cluster: f.first_cluster,
                            is_dir: false,
                            last_fingerprint: None,
                            last_gather: None,
                        },
                    );
                    let chain: Vec<u32> =
                        (f.first_cluster..f.first_cluster + f.cluster_count).collect();
                    for (index, &cluster) in chain.iter().enumerate() {
                        projected_clusters.insert(cluster, (key, index as u32));
                    }
                    projected_files.insert(
                        key,
                        ProjectedFile {
                            path: path.clone(),
                            size: u32::try_from(f.source.len()).unwrap_or(u32::MAX),
                            chain,
                            extended_past_directory: false,
                        },
                    );
                }
            }
            for s in &dir.subdirs {
                mirrored.insert(
                    (dir.first_cluster, s.name),
                    MirrorEntry {
                        host_path: s.dir.host_path.clone(),
                        first_cluster: s.dir.first_cluster,
                        is_dir: true,
                        last_fingerprint: None,
                        last_gather: None,
                    },
                );
                seed(
                    &s.dir,
                    dir_paths,
                    mirrored,
                    projected_files,
                    projected_clusters,
                );
            }
        }
        seed(
            &tree.root,
            &mut dir_paths,
            &mut mirrored,
            &mut projected_files,
            &mut projected_clusters,
        );
        let mut system_names: HashSet<[u8; 11]> = system_files
            .iter()
            .map(|(name, _)| fold_literal_83(name))
            .collect();
        // The synthetic C:\DOS folder build_tree creates for the system binaries must
        // also never be materialized as a real host directory: protect its name so
        // reconcile's classify() skips the DOS subdir entry (exactly the condition
        // under which build_tree attaches the folder).
        if system_files.iter().any(|(name, _)| {
            DOS_FOLDER_BINARIES
                .iter()
                .any(|b| name.eq_ignore_ascii_case(b))
        }) {
            system_names.insert(fold_literal_83("DOS"));
        }

        // --- flatten the tree into dirs/files + the sorted run table -----------
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut runs = Vec::new();
        flatten(&tree.root, true, &mut dirs, &mut files, &mut runs);
        runs.sort_by_key(|r| r.0);
        let directory_clusters = runs
            .iter()
            .filter(|(_, _, role)| matches!(role, Role::Dir(_)))
            .flat_map(|(first, last, _)| (*first..=*last).map(|cluster| (cluster, *first)))
            .collect();

        let mut volume = Self {
            tree,
            geo,
            mbr: mbr_out,
            vbr: vbr_out,
            free_count,
            next_free,
            fat,
            dirs,
            files,
            runs,
            store: crate::katea_store::SectorStore::new(),
            dir_paths,
            mirrored,
            projected_files,
            projected_clusters,
            blocked_projection_keys: HashSet::new(),
            host_write_handles: HashMap::new(),
            directory_clusters,
            batch_lbas: HashSet::new(),
            unmapped_by_cluster: HashMap::new(),
            unmapped_sectors: 0,
            newly_projectable_clusters: HashSet::new(),
            batch_sector_writes: 0,
            directory_dirty: false,
            metadata_reconcile_pending: false,
            fat_dirty_clusters: HashSet::new(),
            host_io_failed: false,
            system_names,
            gathers: 0,
            retry_gathers: HashMap::new(),
            #[cfg(test)]
            candidate_lbas_examined: 0,
            #[cfg(test)]
            gathered_bytes: 0,
            #[cfg(test)]
            atomic_writes: 0,
            #[cfg(test)]
            atomic_write_bytes: 0,
            counters: KateaCounterCells::default(),
            host_read_cache: std::cell::RefCell::new(Vec::new()),
            host_read_window: std::cell::RefCell::new(None),
            host_readahead: std::cell::RefCell::new(Vec::new()),
            read_command_end_lba: std::cell::Cell::new(None),
            command_read_batch_enabled: command_read_batch_enabled(),
            readahead_max_bytes: readahead_max_bytes(),
            fat_sector_cache: std::cell::RefCell::new(None),
            #[cfg(test)]
            fat_sector_builds: std::cell::Cell::new(0),
            #[cfg(test)]
            fat_walk_resolves: std::cell::Cell::new(0),
            chain_memo: std::cell::RefCell::new(HashMap::new()),
            fat_epoch: 0,
            fat_sector_epoch: HashMap::new(),
            #[cfg(test)]
            chain_walks: std::cell::Cell::new(0),
            #[cfg(test)]
            chain_memo_hits: std::cell::Cell::new(0),
            claim_by_key: HashMap::new(),
            claimants: HashMap::new(),
            claims_valid: false,
            claims_pass: 0,
            incremental_claims: incremental_claims_enabled(),
            claims_verify: claims_verify_enabled(),
            #[cfg(test)]
            claim_inserts: std::cell::Cell::new(0),
            #[cfg(test)]
            claim_skips: std::cell::Cell::new(0),
            #[cfg(test)]
            claim_divergences: std::cell::Cell::new(0),
            #[cfg(test)]
            suppress_epoch_stamps: false,
            #[cfg(test)]
            amortised_refresh: true,
            region_census: region_census_enabled(),
        };
        volume.prime_claim_index();
        Ok(volume)
    }

    /// Build the anti-clobber claim index once, HERE, at mount.
    ///
    /// The index cannot make the FIRST derivation cheap -- pass one has to decide
    /// ambiguity over every live file, and that is O(clusters in the folder)
    /// however it is written. What it can decide is WHERE that cost lands. Left
    /// to the first projection pass it lands in `projection_max_ns`, which is the
    /// synchronous-freeze metric this whole slice exists to move: measured, the
    /// cold build made the worst pass on a 498 MB folder 14.4 ms, WORSE than the
    /// 8.5 ms it was replacing, while every other pass fell to under 2 ms.
    ///
    /// Mount is where that cost belongs. It is already O(folder) -- `walk_into`
    /// recurses the host tree and `seed` inserts one `projected_clusters` entry
    /// per cluster -- it happens once, and it happens while the user is waiting
    /// for a drive to appear rather than mid-game.
    ///
    /// No new derivation: this runs the same `gather_live` +
    /// `ambiguous_incrementally` the passes run, so there is nothing extra to
    /// prove correct. The set it computes is discarded; only the index is kept.
    /// A folder that arrives with an incomplete directory simply leaves the index
    /// cold, and the first pass rebuilds it exactly as it would have.
    fn prime_claim_index(&mut self) {
        if !self.incremental_claims {
            return;
        }
        let (live, complete) = self.gather_live();
        if !complete {
            return;
        }
        let _ = self.ambiguous_incrementally(&live);
    }

    /// The synthesized geometry, for the profile report. Derived at mount from
    /// the host folder's size; nothing about it changes during a run.
    pub(crate) fn geometry_report(&self) -> KateaGeometryReport {
        KateaGeometryReport {
            sectors_per_cluster: self.geo.spc,
            fat_sectors: self.geo.fatsz,
            partition_sectors: self.geo.part_sectors,
            total_sectors: self.geo.total_sectors,
            count_of_clusters: self.geo.count_of_clusters,
        }
    }

    /// Arm the region census without going through the environment, so a test
    /// can drive both legs of the gate in one process. Production arms it once
    /// at mount from `IZARRAVM_KATEA_REGION_CENSUS`.
    #[cfg(test)]
    pub(crate) fn arm_region_census(&mut self) {
        self.region_census = true;
    }

    /// What the read path has done since mount. See [`KateaStorageCounters`].
    pub(crate) fn storage_counters(&self) -> KateaStorageCounters {
        let mut counters = self.counters.snapshot();
        let store = self.store.counters();
        counters.overlay_resident_sectors = store.resident_sectors;
        counters.overlay_pending_sectors = store.pending_sectors;
        counters.pending_unmapped_sectors = self.unmapped_sectors;
        counters.spill_operations = store.spill_operations;
        counters.spill_bytes = store.spill_bytes;
        counters.spill_wall_ns = store.spill_wall_ns;
        counters.blocked_projection_keys = self.blocked_projection_keys.len() as u64;
        counters
    }

    #[cfg(test)]
    pub(crate) fn candidate_lbas_examined(&self) -> u64 {
        self.candidate_lbas_examined
    }

    #[cfg(test)]
    pub(crate) fn reset_candidate_census(&mut self) {
        self.candidate_lbas_examined = 0;
    }

    /// Base-view FAT sectors actually synthesized since mount. See
    /// [`Self::fat_sector_cache`].
    #[cfg(test)]
    pub(crate) fn fat_sector_builds(&self) -> u64 {
        self.fat_sector_builds.get()
    }

    #[cfg(test)]
    pub(crate) fn reset_fat_sector_builds(&self) {
        self.fat_sector_builds.set(0);
    }

    /// FAT sectors a walk cursor has resolved since mount. See
    /// [`Self::fat_walk_resolves`].
    #[cfg(test)]
    pub(crate) fn fat_walk_resolves(&self) -> u64 {
        self.fat_walk_resolves.get()
    }

    #[cfg(test)]
    pub(crate) fn reset_fat_walk_resolves(&self) {
        self.fat_walk_resolves.set(0);
    }

    /// Drop every memoized chain, so a fixture measuring what a WALK costs
    /// measures a walk. `prime_claim_index` warms the memo at mount, which is the
    /// point of it, and a fixture that did not clear it would be timing a
    /// `HashMap` lookup.
    #[cfg(test)]
    pub(crate) fn clear_chain_memo(&self) {
        self.chain_memo.borrow_mut().clear();
    }

    /// One FAT entry through a fresh walk cursor, so a differential test can
    /// compare the walked path against `fat_entry` cluster by cluster.
    #[cfg(test)]
    pub(crate) fn fat_entry_via_walk(&self, c: u32) -> u32 {
        let mut walk = FatWalk::default();
        self.fat_entry_walked(c, &mut walk)
    }

    /// The reconcile path's chain walk, for the differential test.
    #[cfg(test)]
    pub(crate) fn chain_via_walk(&self, first: u32) -> Option<Vec<u32>> {
        self.chain_of(first)
    }

    /// Chain walks actually performed and memo hits, since mount. See
    /// [`ChainMemo`].
    #[cfg(test)]
    pub(crate) fn chain_walk_counts(&self) -> (u64, u64) {
        (self.chain_walks.get(), self.chain_memo_hits.get())
    }

    #[cfg(test)]
    pub(crate) fn reset_chain_walk_counts(&self) {
        self.chain_walks.set(0);
        self.chain_memo_hits.set(0);
    }

    /// Whether a chain starting at `first` is currently memoized at all. The
    /// fixture for the out-of-FAT-copy-0 arm needs to see the ABSENCE of a memo,
    /// which no behavioural assertion can show on its own.
    #[cfg(test)]
    pub(crate) fn chain_memo_holds(&self, first: u32) -> bool {
        self.chain_memo.borrow().contains_key(&first)
    }

    /// The reference ambiguity set over the volume's CURRENT live entries, for
    /// the fixtures that grade any faster implementation against it.
    #[cfg(test)]
    pub(crate) fn ambiguous_reference(&self) -> HashSet<(u32, [u8; 11])> {
        let (live, _) = self.gather_live();
        self.ambiguous_by_full_scan(&live)
    }

    /// The incremental answer over the volume's CURRENT live entries, plus
    /// `gather_live`'s completeness verdict, for the differential fixtures.
    #[cfg(test)]
    pub(crate) fn ambiguous_incremental_for_test(&mut self) -> (HashSet<(u32, [u8; 11])>, bool) {
        let (live, complete) = self.gather_live();
        (self.ambiguous_files(&live, complete), complete)
    }

    /// Claim insertions performed and live keys skipped since the last reset.
    #[cfg(test)]
    pub(crate) fn claim_counts(&self) -> (u64, u64) {
        (self.claim_inserts.get(), self.claim_skips.get())
    }

    #[cfg(test)]
    pub(crate) fn reset_claim_counts(&self) {
        self.claim_inserts.set(0);
        self.claim_skips.set(0);
    }

    /// Whether the claim index is currently trusted.
    #[cfg(test)]
    pub(crate) fn claims_valid(&self) -> bool {
        self.claims_valid
    }

    /// Force the full anti-clobber scan without going through the environment,
    /// so a test can drive both arms of the gate in one process.
    #[cfg(test)]
    pub(crate) fn disarm_incremental_claims(&mut self) {
        self.incremental_claims = false;
    }

    /// Arm the verify harness in-process: every internal `ambiguous_files` call
    /// computes both answers and counts any disagreement.
    #[cfg(test)]
    pub(crate) fn arm_claims_verify(&mut self) {
        self.claims_verify = true;
    }

    /// Disagreements between the incremental index and the full scan since mount.
    /// The verify harness prefers the reference and rebuilds, so a divergence is
    /// invisible to behaviour; a test has to read the count to see it.
    #[cfg(test)]
    pub(crate) fn claim_divergences(&self) -> u64 {
        self.claim_divergences.get()
    }

    /// Turn off the amortised forced refresh, so a fixture measures the hook it
    /// is aiming at rather than the insurance. See
    /// [`Self::amortised_refresh`].
    #[cfg(test)]
    pub(crate) fn disable_amortised_refresh(&mut self) {
        self.amortised_refresh = false;
    }

    /// Stop stamping `fat_sector_epoch`, simulating an invalidation hook that
    /// missed. The failure-direction fixture uses it to make the index stale ON
    /// PURPOSE and assert the outcome is a hold rather than a clobber.
    #[cfg(test)]
    pub(crate) fn suppress_fat_epoch_stamps(&mut self) {
        self.suppress_epoch_stamps = true;
    }

    /// The same live set the reference was computed over, as
    /// `(dir_cluster, name, first_cluster, is_dir)`, so a test can re-derive the
    /// definition independently instead of re-running the implementation.
    #[cfg(test)]
    pub(crate) fn live_entries_for_test(&self) -> Vec<(u32, [u8; 11], u32, bool)> {
        self.gather_live()
            .0
            .into_iter()
            .map(|e| (e.dir_cluster, e.name, e.first_cluster, e.is_dir))
            .collect()
    }

    /// Whether `cluster` is currently registered as a directory cluster.
    #[cfg(test)]
    pub(crate) fn is_directory_cluster(&self, cluster: u32) -> bool {
        self.directory_clusters.contains_key(&cluster)
    }

    /// How many host read handles are cached, for the LRU bound test.
    #[cfg(test)]
    pub(crate) fn host_read_handles(&self) -> usize {
        self.host_read_cache.borrow().len()
    }

    /// Whether a read handle for `path` is cached.
    #[cfg(test)]
    pub(crate) fn host_read_handle_cached(&self, path: &Path) -> bool {
        self.host_read_cache
            .borrow()
            .iter()
            .any(|(cached, _)| cached == path)
    }

    /// Whether a read-ahead slot currently holds bytes of `path`.
    #[cfg(test)]
    pub(crate) fn readahead_holds(&self, path: &Path) -> bool {
        self.host_readahead
            .borrow()
            .iter()
            .any(|ahead| ahead.path == path)
    }

    /// Disarm the read-ahead without going through the environment, so a test can
    /// drive both legs of the `IZARRAVM_HDD_READAHEAD` gate in one process.
    #[cfg(test)]
    pub(crate) fn disarm_readahead(&mut self) {
        self.readahead_max_bytes = 0;
        self.host_readahead.borrow_mut().clear();
    }

    pub(crate) fn begin_read_command(&self, start_lba: u32, sectors: u32) {
        self.host_read_window.replace(None);
        if !self.command_read_batch_enabled {
            self.read_command_end_lba.set(None);
            return;
        }
        self.read_command_end_lba.set(Some(
            start_lba
                .saturating_add(sectors)
                .min(self.geo.total_sectors),
        ));
    }

    pub(crate) fn end_read_command(&self) {
        self.read_command_end_lba.set(None);
        self.host_read_window.replace(None);
    }

    fn command_span_sectors(&self, lba: u32, contiguous: u32) -> u32 {
        self.read_command_end_lba
            .get()
            .filter(|end| lba < *end)
            .map(|end| {
                (end - lba)
                    .min(contiguous)
                    .clamp(1, HOST_READ_COMMAND_WINDOW_SECTORS)
            })
            .unwrap_or(1)
    }

    fn host_read_context(&self, lba: u32, command_sectors: u32) -> HostReadContext<'_> {
        HostReadContext {
            lba,
            command_sectors,
            counters: &self.counters,
            cache: &self.host_read_cache,
            window: &self.host_read_window,
            readahead: &self.host_readahead,
            readahead_max: self.readahead_max_bytes,
            first_data_lba: self.geo.part_start + self.geo.first_data_sector,
        }
    }

    /// Drop every cached view of the host filesystem that a mutation could have
    /// invalidated: open read handles, the command window, and the read-ahead
    /// buffer. `path` scopes the drop to one host file; `None` drops everything.
    ///
    /// COMPLETENESS. Five things mutate a host file this volume reads.
    ///
    /// Four are inside this module: `katea_write::atomic_write`,
    /// `std::fs::rename`, the deletes, and `stream_file_overlay`'s positioned
    /// write / `set_len`. The first three all run inside `reconcile_mode`, which
    /// calls this unscoped on entry (as it always has) and again, scoped, at each
    /// mutation -- the scoped calls matter because phase 3 *reads* file bytes and
    /// then rewrites them within the same pass, so an entry-only drop would let a
    /// handle opened by the gather serve pre-write bytes afterwards.
    /// `stream_file_overlay` is the fourth and is also reachable from
    /// `stream_projected_batch`, outside any reconcile, so it calls this itself.
    ///
    /// The fifth is outside: `Dos::katea_repair` rewrites CONFIG.SYS and
    /// AUTOEXEC.BAT in the mounted folder and renames the originals aside. It
    /// does not call this and does not need to -- it finishes by calling
    /// `mount_hdd_folder`, which builds a whole new volume, so every cache here
    /// is dropped with the old one. That is the only safe way to reach a mounted
    /// folder from outside this module, and it is worth naming because a future
    /// caller that skipped the re-mount would be serving stale bytes with nothing
    /// in this file to tell it so.
    ///
    /// `create_dir_all` is deliberately not on the list: a directory is never a
    /// read source here, and its *contents* are synthesized from the guest's own
    /// FAT and directory sectors, never read back off the host.
    fn invalidate_host_reads(&self, path: Option<&Path>) {
        match path {
            Some(path) => {
                self.host_read_cache
                    .borrow_mut()
                    .retain(|(cached, _)| cached != path);
                self.host_readahead
                    .borrow_mut()
                    .retain(|ahead| ahead.path != path);
                // The command window is LBA-keyed, not path-keyed, so it cannot
                // be scoped; it is at most one command's worth of bytes and the
                // next physical read refills it.
                self.host_read_window.replace(None);
            }
            None => {
                self.host_read_cache.borrow_mut().clear();
                self.host_readahead.borrow_mut().clear();
                self.host_read_window.replace(None);
            }
        }
    }

    fn read_command_window_sector(&self, lba: u32) -> Option<SectorRead> {
        let slot = self.host_read_window.borrow();
        let cached = slot.as_ref()?;
        let offset = usize::try_from(lba.checked_sub(cached.start_lba)?)
            .ok()?
            .checked_mul(SECTOR)?;
        let available = cached.bytes.len().checked_sub(offset)?.min(SECTOR);
        if available == 0 {
            return None;
        }
        let mut bytes = [0u8; SECTOR];
        bytes[..available].copy_from_slice(&cached.bytes[offset..offset + available]);
        drop(slot);
        bump(&self.counters.host_file_reads, 1);
        bump(&self.counters.host_bytes, available as u64);
        Some(SectorRead::ok(bytes))
    }

    /// How many file bodies `reconcile` has re-read. Test-only: the live path only
    /// ever increments it. See `gathers`.
    #[allow(dead_code)]
    pub(crate) fn gathers(&self) -> u64 {
        self.gathers
    }

    #[cfg(test)]
    pub(crate) fn gathered_bytes(&self) -> u64 {
        self.gathered_bytes
    }

    #[cfg(test)]
    pub(crate) fn atomic_writes(&self) -> u64 {
        self.atomic_writes
    }

    #[cfg(test)]
    pub(crate) fn atomic_write_bytes(&self) -> u64 {
        self.atomic_write_bytes
    }

    /// The whole-disk sector count, so the ATA layer can derive its geometry.
    pub(crate) fn total_sectors(&self) -> u32 {
        self.geo.total_sectors
    }

    /// The owned tree for tests and writes; `read_sector` does not use it.
    #[allow(dead_code)]
    pub(crate) fn tree(&self) -> &HostTree {
        &self.tree
    }

    /// The 28-bit FAT entry for cluster `c`, reading the overlay-shadowed FAT (so
    /// guest-written chain links are honored) and falling back to the tree's
    /// computed FAT. Built on `read_sector`, which already consults the overlay.
    ///
    /// A CHAIN WALK MUST NOT COME THROUGH HERE. Every call materializes a whole
    /// 512-byte sector BY VALUE to read four bytes of it, on top of the full
    /// `read_sector_checked` dispatch, and the reconcile pass walks every live
    /// file's chain three times over. Use [`Self::fat_entry_walked`] with a
    /// [`FatWalk`] cursor, or the [`Self::chain_of`] wrapper that owns one. This
    /// entry point survives for the single-entry probes (`freed` in phase 2, the
    /// tests) where a cursor would have nothing to amortize over.
    pub(crate) fn fat_entry(&self, c: u32) -> u32 {
        let byte = c as usize * 4;
        let fat_sector_rel = u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let lba = self.geo.part_start + fat_sector_rel;
        let off = byte % SECTOR;
        let sec = self.read_sector(lba);
        u32::from_le_bytes([sec[off], sec[off + 1], sec[off + 2], sec[off + 3]]) & 0x0FFF_FFFF
    }

    /// The same 28-bit FAT entry, resolved through a walk cursor.
    ///
    /// WHY. `fat_entry` was the dominant per-cluster cost of every chain walk in
    /// the projection pass, and the pass therefore scaled with the size of the
    /// MOUNTED FOLDER rather than with the size of the write. Per cluster it paid
    /// a `SectorStore` lookup, a `RefCell` borrow for the command window, a
    /// 512-byte copy out of the FAT memo, another into `SectorRead`, and a third
    /// out of it -- to read four bytes. Measured anchor: a 47 MB folder
    /// (~12,000 clusters) cost a 1.81 ms worst pass, which extrapolates to
    /// ~19 ms at 500 MB, back into the freeze class.
    ///
    /// WHAT THIS DOES INSTEAD. The cursor resolves ONE FAT SECTOR -- 128 clusters
    /// -- and holds the answer:
    ///
    /// * if the guest has written that FAT sector, the cursor owns its bytes and
    ///   every entry in it is a four-byte read out of a buffer already in hand;
    /// * if it has not, there is no sector at all. The base-view FAT is a pure
    ///   function of the immutable `fat`/`geo` pair, and
    ///   `ClusterIndex::fat_entry(c)` is exactly the value
    ///   `fat.fat_sector(c / 128)[4 * (c % 128) ..]` would hold -- the sector
    ///   synthesis fills entry `i` of sector `s` from `fat_entry(s * 128 + i)`.
    ///   So the entry comes from one `HashSet` probe and no 512 bytes move.
    ///
    /// IDENTICAL ANSWERS. The same two sources in the same order as
    /// `read_sector_checked`: the store's overlay, then the base view. The
    /// command window is deliberately not consulted, and cannot matter: it is
    /// filled at exactly one site, always from a data-region LBA (asserted there),
    /// so it can never cover a FAT sector.
    ///
    /// IDENTICAL COUNTERS. `sector_reads` is still bumped once per cluster, and
    /// the `IZARRAVM_KATEA_REGION_CENSUS` FAT-sector counter once per cluster
    /// served from the base view, exactly as the old path did -- so no profile
    /// field moves. The store's `read_errors` counter is the one number that
    /// changes: a failed spill read is now registered once per FAT SECTOR rather
    /// than once per cluster in it. Every reader compares it for INEQUALITY
    /// against a snapshot ("did anything fail in this bracket?"), so one is as
    /// good as 128.
    ///
    /// A cluster whose entry falls outside FAT copy 0 is handed to `fat_entry`
    /// verbatim, so even a corrupt or guest-crafted link resolves exactly as
    /// before.
    fn fat_entry_walked(&self, c: u32, walk: &mut FatWalk) -> u32 {
        let byte = c as usize * 4;
        let within = (byte / SECTOR) as u32;
        if within >= self.geo.fatsz {
            // Outside FAT copy 0 entirely: a corrupt or guest-crafted link. The
            // old path answers it verbatim, so the VALUE is exactly what it
            // always was.
            //
            // `degraded` is load-bearing here, not defensive tidiness, and it is
            // the adversarial-guest case. `fat_entry` resolves this cluster
            // through an LBA in FAT copy 1 (or past the FAT region entirely) --
            // an overlay sector the cursor never resolved and therefore never
            // recorded in `walk.sectors`. A memo built from such a walk would
            // depend on a sector outside its own dependency set, and a later
            // guest write to that sector would not expire it. Marking the walk
            // degraded is what keeps it out of the memo. See
            // `a_link_past_fat_copy_0_is_never_memoized`.
            walk.degraded = true;
            return self.fat_entry(c);
        }
        let off = byte % SECTOR;
        bump(&self.counters.sector_reads, 1);
        if walk.within != Some(within) {
            #[cfg(test)]
            self.fat_walk_resolves
                .set(self.fat_walk_resolves.get().saturating_add(1));
            walk.sectors.push(within);
            let lba = self.geo.part_start + u32::from(RESERVED_SECTORS) + within;
            walk.overlay = match self.store.get(lba) {
                Ok(Some(bytes)) => Some(Box::new(bytes)),
                Ok(None) => None,
                Err(e) => {
                    // Same message, same substitute bytes, same held chain: the
                    // store has already counted the error and `read_sector`
                    // dropped the degraded flag and served zeros.
                    eprintln!("katea: guest-written sector {lba} could not be read back: {e}");
                    walk.degraded = true;
                    Some(Box::new([0u8; SECTOR]))
                }
            };
            walk.within = Some(within);
        }
        match &walk.overlay {
            Some(sector) => {
                u32::from_le_bytes([
                    sector[off],
                    sector[off + 1],
                    sector[off + 2],
                    sector[off + 3],
                ]) & 0x0FFF_FFFF
            }
            None => {
                // Region census: default-off instrument, gated at the call site,
                // and per CLUSTER because that is what the sector-reading path it
                // replaces counted.
                if self.region_census {
                    bump(&self.counters.fat_sector_reads, 1);
                }
                self.fat.fat_entry(c) & 0x0FFF_FFFF
            }
        }
    }

    /// Follow the cluster chain starting at `first`, memoized.
    ///
    /// Every chain walk in the reconcile path goes through here, so none of them
    /// pays `fat_entry`'s per-cluster sector copy -- and none of them re-walks a
    /// chain whose FAT sectors nothing has written since the last walk. The
    /// result is `katea_write::chain`'s, unchanged, in both cases; see
    /// [`ChainMemo`] for why the second one is the same answer and not merely a
    /// plausible one.
    fn chain_of(&self, first: u32) -> Option<Vec<u32>> {
        if let Some(memo) = self.chain_memo.borrow().get(&first)
            && self.chain_memo_is_current(memo)
        {
            #[cfg(test)]
            self.chain_memo_hits
                .set(self.chain_memo_hits.get().saturating_add(1));
            return memo.result.clone();
        }
        #[cfg(test)]
        self.chain_walks
            .set(self.chain_walks.get().saturating_add(1));
        let mut walk = FatWalk::default();
        let result = crate::katea_write::chain(first, self.max_chain(), |c| {
            self.fat_entry_walked(c, &mut walk)
        });
        if !walk.degraded {
            let mut memo = self.chain_memo.borrow_mut();
            if memo.len() >= CHAIN_MEMO_MAX_ENTRIES && !memo.contains_key(&first) {
                memo.clear();
            }
            memo.insert(
                first,
                ChainMemo {
                    result: result.clone(),
                    sectors: walk.sectors,
                    epoch: self.fat_epoch,
                },
            );
        }
        result
    }

    /// Would re-walking this memo's chain read the same FAT entries it did?
    ///
    /// The whole-volume test first: with no FAT write at all since the memo was
    /// taken, nothing needs checking. Otherwise every sector the walk read is
    /// compared against the epoch at which it was last written -- a handful of
    /// probes for a chain of any length, because a FAT sector covers 128
    /// clusters.
    fn chain_memo_is_current(&self, memo: &ChainMemo) -> bool {
        self.chain_token_is_current(memo.epoch, &memo.sectors)
    }

    /// The same currency test over a token held somewhere other than the memo.
    fn chain_token_is_current(&self, epoch: u64, sectors: &[u32]) -> bool {
        if self.fat_epoch == epoch {
            return true;
        }
        sectors
            .iter()
            .all(|within| self.fat_sector_epoch.get(within).copied().unwrap_or(0) <= epoch)
    }

    /// A chain walk plus the token that dates it, for the claim index.
    ///
    /// `None` for the token means the walk was DEGRADED -- it read substituted
    /// zeros for an unreadable spilled FAT sector, or followed a link past FAT
    /// copy 0 through a sector it never recorded. Such a walk is not memoized and
    /// must not be claimed either: its answer is a function of an I/O failure or
    /// of a dependency outside its own set.
    fn chain_with_token(&self, first: u32) -> (Option<Vec<u32>>, Option<ChainToken>) {
        if let Some(memo) = self.chain_memo.borrow().get(&first)
            && self.chain_memo_is_current(memo)
        {
            #[cfg(test)]
            self.chain_memo_hits
                .set(self.chain_memo_hits.get().saturating_add(1));
            return (
                memo.result.clone(),
                Some((memo.epoch, memo.sectors.clone())),
            );
        }
        #[cfg(test)]
        self.chain_walks
            .set(self.chain_walks.get().saturating_add(1));
        let mut walk = FatWalk::default();
        let result = crate::katea_write::chain(first, self.max_chain(), |c| {
            self.fat_entry_walked(c, &mut walk)
        });
        if walk.degraded {
            return (result, None);
        }
        let token = (self.fat_epoch, walk.sectors.clone());
        let mut memo = self.chain_memo.borrow_mut();
        if memo.len() >= CHAIN_MEMO_MAX_ENTRIES && !memo.contains_key(&first) {
            memo.clear();
        }
        memo.insert(
            first,
            ChainMemo {
                result: result.clone(),
                sectors: walk.sectors,
                epoch: self.fat_epoch,
            },
        );
        (result, Some(token))
    }

    /// Absolute LBA of a data cluster's first sector. The reconcile pass and the
    /// tests resolve cluster -> LBA this way; the read path goes the other way
    /// (LBA -> cluster) inside `read_sector`.
    pub(crate) fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.geo.part_start
            + self.geo.first_data_sector
            + (cluster - ROOT_CLUSTER) * u32::from(self.geo.spc)
    }

    /// The maximum cluster-chain length we will follow before treating a chain as
    /// corrupt (a cyclic/garbled FAT must not loop forever). The data region can
    /// never exceed the cluster count, so this is a safe ceiling.
    fn max_chain(&self) -> usize {
        self.geo.count_of_clusters as usize + 2
    }

    /// Whether `c` is a valid data cluster (`2 ..= count_of_clusters + 1`). A FAT
    /// link outside this range is a corrupt or guest-crafted pointer; reconcile
    /// holds such a chain rather than computing an out-of-range LBA (which would
    /// overflow `u32` in debug for large cluster sizes — and reads garbage in
    /// release). Conservative: never materialize from an out-of-range chain.
    fn cluster_in_range(&self, c: u32) -> bool {
        (ROOT_CLUSTER..=self.geo.count_of_clusters + 1).contains(&c)
    }

    fn is_metadata_sector(&self, lba: u32) -> bool {
        if lba < self.geo.part_start + self.geo.first_data_sector {
            return true;
        }
        let rel = lba - self.geo.part_start - self.geo.first_data_sector;
        let cluster = ROOT_CLUSTER + rel / u32::from(self.geo.spc);
        self.directory_clusters.contains_key(&cluster)
    }

    fn note_metadata_write(&mut self, lba: u32) -> bool {
        if lba < self.geo.part_start {
            return true;
        }
        let rel = lba - self.geo.part_start;
        let reserved = u32::from(RESERVED_SECTORS);
        let fat_end = reserved + u32::from(NUM_FATS) * self.geo.fatsz;
        if (reserved..fat_end).contains(&rel) {
            let within = (rel - reserved) % self.geo.fatsz;
            let first_cluster = within.saturating_mul((SECTOR / 4) as u32);
            self.fat_dirty_clusters
                .extend(first_cluster..first_cluster + (SECTOR / 4) as u32);
            // THE CHAIN MEMO'S ONLY INVALIDATION SITE. `write_sector` is the sole
            // caller of `SectorStore::insert` and routes every FAT-region LBA
            // through here, so stamping the sector here is what makes
            // [`ChainMemo`]'s dependency test sound. Taken modulo `fatsz`, so a
            // write to either FAT copy expires memos that read the first -- the
            // conservative direction, and free.
            #[cfg(test)]
            if self.suppress_epoch_stamps {
                return true;
            }
            self.fat_epoch += 1;
            self.fat_sector_epoch.insert(within, self.fat_epoch);
            return true;
        }
        if rel < self.geo.first_data_sector {
            return true;
        }
        let cluster = ROOT_CLUSTER + (rel - self.geo.first_data_sector) / u32::from(self.geo.spc);
        if self.directory_clusters.contains_key(&cluster) {
            self.directory_dirty = true;
            return true;
        }
        false
    }

    fn install_projection(
        &mut self,
        key: (u32, [u8; 11]),
        path: PathBuf,
        size: u32,
        chain: Vec<u32>,
        extended_past_directory: bool,
    ) {
        self.blocked_projection_keys.remove(&key);
        if let Some(old) = self.projected_files.remove(&key) {
            for cluster in old.chain {
                if self
                    .projected_clusters
                    .get(&cluster)
                    .is_some_and(|(owner, _)| *owner == key)
                {
                    self.projected_clusters.remove(&cluster);
                }
            }
        }
        for (index, &cluster) in chain.iter().enumerate() {
            self.projected_clusters.insert(cluster, (key, index as u32));
            // This cluster's held sectors have an owner now, so the next commit
            // owes them a second look even if it writes nowhere near them.
            if self.unmapped_by_cluster.contains_key(&cluster) {
                self.newly_projectable_clusters.insert(cluster);
            }
        }
        self.projected_files.insert(
            key,
            ProjectedFile {
                path,
                size,
                chain,
                extended_past_directory,
            },
        );
    }

    fn remove_projection(&mut self, key: (u32, [u8; 11])) {
        self.blocked_projection_keys.remove(&key);
        if let Some(old) = self.projected_files.remove(&key) {
            self.host_write_handles.remove(&old.path);
            for cluster in old.chain {
                if self
                    .projected_clusters
                    .get(&cluster)
                    .is_some_and(|(owner, _)| *owner == key)
                {
                    self.projected_clusters.remove(&cluster);
                }
            }
        }
    }

    fn stream_file_overlay(&mut self, mut pending: PendingStream) -> std::io::Result<()> {
        let spc = u32::from(self.geo.spc);
        // `set_len` marks a size that came from the guest's directory entry; a
        // streamed batch carries the extent its own writes reached instead.
        // Reconcile the two before a byte moves, because the entry can be
        // arbitrarily stale: DOS publishes the closing size last, so a directory
        // write for any sibling entry drags this file through reconcile against
        // a size from before its payload existed. Truncating to that size would
        // destroy payload already acknowledged out of the store, and a later
        // size update would then extend the file with zeros -- guest-visible
        // loss with no way back. Deliberate truncation still lands: it arrives
        // as a *changed* entry size while the projection is not extended.
        let existing = self
            .projected_files
            .get(&pending.key)
            .map(|file| (file.size, file.extended_past_directory));
        let mut set_len = pending.set_len;
        // Compare at sector granularity. A streamed extent is rounded up to the
        // sector that carried it, so a closing size inside that last sector is
        // the entry catching up, not a shrink -- only a size that would drop a
        // whole acknowledged sector is the stale-entry case.
        let declared_sectors = u64::from(pending.size).div_ceil(SECTOR as u64) * SECTOR as u64;
        let extended_past_directory = match existing {
            Some((existing_size, true))
                if pending.set_len && declared_sectors < u64::from(existing_size) =>
            {
                pending.size = existing_size;
                set_len = false;
                true
            }
            _ if pending.set_len => false,
            Some((existing_size, existing_extended)) => {
                existing_extended || pending.size > existing_size
            }
            None => false,
        };
        let mut spans: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut acknowledged = Vec::new();
        let positions = match pending.sectors.as_ref() {
            Some(sectors) => sectors.clone(),
            None => {
                let mut sectors = Vec::new();
                for (cluster_index, cluster) in pending.chain.iter().copied().enumerate() {
                    let base = self.cluster_to_lba(cluster);
                    for sector_in_cluster in 0..spc {
                        sectors.push((
                            base + sector_in_cluster,
                            (cluster_index as u64 * u64::from(spc) + u64::from(sector_in_cluster))
                                * SECTOR as u64,
                        ));
                    }
                }
                sectors
            }
        };
        let mut positions = positions;
        positions.sort_unstable_by_key(|(_, offset)| *offset);
        for (lba, offset) in positions {
            if !self.store.is_pending(lba) {
                continue;
            }
            let Some(sector) = self.store.get(lba)? else {
                continue;
            };
            let valid = u64::from(pending.size)
                .saturating_sub(offset)
                .min(SECTOR as u64) as usize;
            if valid == 0 {
                continue;
            }
            match spans.last_mut() {
                Some((span_offset, bytes)) if *span_offset + bytes.len() as u64 == offset => {
                    bytes.extend_from_slice(&sector[..valid]);
                }
                _ => spans.push((offset, sector[..valid].to_vec())),
            }
            if valid == SECTOR {
                acknowledged.push(lba);
            }
        }

        // The write-through path is reachable from `stream_projected_batch` with
        // no reconcile around it, so it is its own invalidation point: anything
        // cached for this path is about to describe the file as it was before
        // these spans land. Scoped, so a read-ahead over another file survives.
        //
        // Before the write, not after: a `?` on any step below returns early with
        // the file possibly part-written, and a cache left standing across that
        // return would serve pre-write bytes for the rest of the session.
        self.invalidate_host_reads(Some(&pending.path));

        // The handle cache exists to spare a reopen per command on the file
        // being written, which is one or two files at a time. Without a cap a
        // session that touches thousands of files holds a descriptor for every
        // one of them until eject, so drop the whole cache once it grows past
        // any plausible working set. Closing is the only thing lost: every
        // handle is unbuffered, so nothing is pending inside one.
        if self.host_write_handles.len() >= MAX_HOST_WRITE_HANDLES
            && !self.host_write_handles.contains_key(&pending.path)
        {
            self.host_write_handles.clear();
        }
        let file = match self.host_write_handles.entry(pending.path.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&pending.path)?,
            ),
        };
        for (offset, bytes) in &spans {
            file.seek(SeekFrom::Start(*offset))?;
            file.write_all(bytes)?;
        }
        if set_len {
            file.set_len(u64::from(pending.size))?;
        }

        let bytes = spans
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum::<u64>();
        bump(
            &self.counters.projection_operations,
            spans.len() as u64 + u64::from(set_len),
        );
        bump(&self.counters.projection_bytes, bytes);

        for lba in acknowledged {
            // `acknowledge` is the other way the store's answer for an LBA can
            // change, and this is its only caller. [`ChainMemo`] is sound only
            // because what it drops is always a projected file's DATA sectors,
            // never a FAT one -- the memo has no hook here and needs none.
            debug_assert!(
                lba >= self.geo.part_start + self.geo.first_data_sector,
                "acknowledged a metadata sector ({lba}): the chain memo has no \
                 invalidation hook for that"
            );
            self.store.acknowledge(lba);
            self.forget_unmapped(lba);
        }
        self.install_projection(
            pending.key,
            pending.path.clone(),
            pending.size,
            pending.chain,
            extended_past_directory,
        );
        self.mirrored.insert(
            pending.key,
            MirrorEntry {
                host_path: pending.path,
                first_cluster: pending.first_cluster,
                is_dir: false,
                last_fingerprint: None,
                last_gather: Some(Gather {
                    size: pending.state.size,
                    chain_id: pending.state.chain_id,
                    seq: self.store.seq(),
                    all_present: true,
                }),
            },
        );
        self.retry_gathers.remove(&pending.key);
        Ok(())
    }

    fn stream_projected_batch(&mut self) -> std::io::Result<bool> {
        type ProjectedBatch = HashMap<(u32, [u8; 11]), (u32, Vec<(u32, u64)>)>;

        let spc = u32::from(self.geo.spc);
        let mut files = ProjectedBatch::new();
        // The candidate set is this command's own writes plus the held sectors
        // that could have changed status since the last commit -- the ones in a
        // cluster this command touched, and the ones in a cluster a projection
        // has claimed. Every other held sector is in a cluster nothing has
        // touched and no projection owns, so it would reach the same verdict it
        // reached last time. Sweeping them anyway is what made the held set's
        // cost quadratic in a session that keeps writing unprojectable payload.
        // Borrowed out and put back rather than cloned: with nothing held, this
        // path is the whole of a sequential install's per-command work, and a
        // set copy per command is a real share of it.
        let batch = std::mem::take(&mut self.batch_lbas);
        let mut held_candidates: Vec<u32> = Vec::new();
        if self.unmapped_by_cluster.is_empty() {
            self.newly_projectable_clusters.clear();
        } else {
            let mut clusters: HashSet<u32> = self.newly_projectable_clusters.drain().collect();
            clusters.extend(batch.iter().filter_map(|lba| self.data_cluster_of(*lba)));
            for cluster in &clusters {
                if let Some(held) = self.unmapped_by_cluster.get(cluster) {
                    held_candidates.extend(held.iter().copied().filter(|lba| !batch.contains(lba)));
                }
            }
        }
        #[cfg(test)]
        {
            self.candidate_lbas_examined = self
                .candidate_lbas_examined
                .saturating_add((batch.len() + held_candidates.len()) as u64);
        }
        for lba in batch.iter().copied().chain(held_candidates.iter().copied()) {
            if !self.store.is_pending(lba) || self.is_metadata_sector(lba) {
                continue;
            }
            let rel = lba - self.geo.part_start - self.geo.first_data_sector;
            let cluster = ROOT_CLUSTER + rel / spc;
            let sector_in_cluster = rel % spc;
            let Some((key, cluster_index)) = self.projected_clusters.get(&cluster).copied() else {
                self.note_unmapped(lba);
                continue;
            };
            if self.blocked_projection_keys.contains(&key) {
                self.note_unmapped(lba);
                continue;
            }
            let offset = (u64::from(cluster_index) * u64::from(spc) + u64::from(sector_in_cluster))
                * SECTOR as u64;
            let current_size = self.projected_files.get(&key).map_or(0, |file| file.size);
            let end = if offset < u64::from(current_size) {
                current_size
            } else {
                u32::try_from(offset + SECTOR as u64).unwrap_or(u32::MAX)
            };
            let entry = files.entry(key).or_insert_with(|| (end, Vec::new()));
            entry.0 = entry.0.max(end);
            entry.1.push((lba, offset));
        }

        self.batch_lbas = batch;
        let mut projected = false;
        for (key, (end, sectors)) in files {
            let Some(file) = self.projected_files.get(&key).cloned() else {
                continue;
            };
            let size = file.size.max(end);
            let state = FileState {
                first_cluster: file.chain.first().copied().unwrap_or(0),
                size,
                chain_id: chain_id(&file.chain),
                chain_seq: file
                    .chain
                    .iter()
                    .map(|cluster| self.store.max_seq_in(self.cluster_to_lba(*cluster), spc))
                    .max()
                    .unwrap_or(0),
            };
            self.stream_file_overlay(PendingStream {
                path: file.path,
                key,
                first_cluster: state.first_cluster,
                size,
                chain: file.chain,
                state,
                sectors: Some(sectors),
                set_len: false,
            })?;
            projected = true;
        }
        Ok(projected)
    }

    fn refresh_changed_fat_projections(&mut self) {
        if self.fat_dirty_clusters.is_empty() {
            return;
        }
        // Ask the reverse index which projections own a dirtied cluster rather
        // than rescanning every projected chain. The two select the same keys --
        // `projected_clusters` holds exactly the clusters of every projected
        // chain -- but scanning cost the total cluster count of the whole mount
        // on every command that touched the FAT, which is most commands of a
        // sequential install.
        let affected_keys: HashSet<(u32, [u8; 11])> = self
            .fat_dirty_clusters
            .iter()
            .filter_map(|cluster| self.projected_clusters.get(cluster))
            .map(|(key, _)| *key)
            .collect();
        let affected: Vec<_> = affected_keys
            .into_iter()
            .filter_map(|key| {
                self.projected_files
                    .get(&key)
                    .map(|file| (key, file.clone()))
            })
            .collect();
        for (key, file) in affected {
            let first = file.chain.first().copied().unwrap_or(0);
            let Some(chain) = self.chain_of(first) else {
                self.blocked_projection_keys.insert(key);
                continue;
            };
            if chain.iter().any(|cluster| !self.cluster_in_range(*cluster)) {
                self.blocked_projection_keys.insert(key);
                continue;
            }
            let conflicts: Vec<_> = chain
                .iter()
                .filter_map(|cluster| {
                    self.projected_clusters
                        .get(cluster)
                        .map(|(owner, _)| *owner)
                })
                .filter(|owner| *owner != key)
                .collect();
            if !conflicts.is_empty() {
                self.blocked_projection_keys.insert(key);
                self.blocked_projection_keys.extend(conflicts);
                continue;
            }
            self.install_projection(
                key,
                file.path,
                file.size,
                chain,
                file.extended_past_directory,
            );
        }
    }

    /// Commit every open host handle to the media.
    ///
    /// `sync_all`, not `flush`: `File::flush` is a documented no-op, because a
    /// `File` has no userspace buffer to push. The guest reaches this through
    /// ATA FLUSH CACHE (0xE7), which on real hardware promises the write cache
    /// has reached the platter -- reporting success after a no-op would be a
    /// durability claim this disk never made good on.
    ///
    /// The cost is scoped to the explicit-flush path by its callers: only
    /// `flush_guest_writes` calls this, and only 0xE7 and the final
    /// reconcile at flush/eject call that. The per-command commit path
    /// (`commit_guest_write_batch`) never syncs, so a sequential install still
    /// pays one write per span and nothing else.
    fn flush_write_handles(&mut self) -> std::io::Result<()> {
        let mut operations = 0u64;
        for file in self.host_write_handles.values_mut() {
            file.sync_all()?;
            operations = operations.saturating_add(1);
        }
        if operations > 0 {
            bump(&self.counters.projection_operations, operations);
        }
        Ok(())
    }

    fn metadata_projection_pending(&self) -> bool {
        if !self.blocked_projection_keys.is_empty() {
            return true;
        }
        let errors_before = self.store.read_errors();
        let (live, _complete) = self.gather_live();
        if self.store.read_errors() != errors_before {
            return true;
        }
        let live_keys: HashSet<_> = live
            .iter()
            .map(|entry| (entry.dir_cluster, entry.name))
            .collect();
        if self.mirrored.keys().any(|key| !live_keys.contains(key)) {
            return true;
        }
        live.into_iter().any(|entry| {
            let key = (entry.dir_cluster, entry.name);
            let Some(mirror) = self.mirrored.get(&key) else {
                return true;
            };
            if mirror.first_cluster != entry.first_cluster || mirror.is_dir != entry.is_dir {
                return true;
            }
            if entry.is_dir {
                return false;
            }
            let Some(projected) = self.projected_files.get(&key) else {
                return true;
            };
            if projected.size != entry.size {
                return true;
            }
            let Some(chain) = self.chain_of(entry.first_cluster) else {
                return true;
            };
            chain != projected.chain
        })
    }

    /// Read-only walk of every known directory, collecting its live entries (files
    /// and subdirs, skipping dots/LFN/volume-label/system names). The basis for
    /// detecting disappearances (delete/rename) in `reconcile`.
    ///
    /// The `bool` is whether every directory it visited was gathered TO
    /// COMPLETION. Three paths drop a whole directory's entries without
    /// `store.read_errors()` moving:
    ///
    /// * its chain will not resolve (`chain_of` returns `None`);
    /// * a chain link is outside the data region;
    /// * `parse_dir` stops at a zeroed entry with live entries behind it -- the
    ///   hazard `reconcile_mode`'s own entry comment names, and exactly what an
    ///   installer zeroing a directory cluster mid-write produces.
    ///
    /// None of those is an error to this function: an absent entry reads as a
    /// deletion, which is what phase 2 is for and is guarded there by the "chain
    /// freed and unclaimed" test. But the incremental claim index would read it
    /// as a RETRACTION, and a retraction on unreliable evidence is the one
    /// direction the anti-clobber guard cannot afford. So the verdict is reported
    /// and `ambiguous_files` falls back to the full scan when it is false.
    ///
    /// A directory that exists but is not in `dir_paths` is not "incomplete" --
    /// it is not visited at all, by design (a just-MKDIR'd subdir is registered
    /// by the materialize phase and gathered on the next pass), and the full scan
    /// does not see it either.
    fn gather_live(&self) -> (Vec<LiveEntry>, bool) {
        let spc = u32::from(self.geo.spc);
        let cluster_bytes = spc as usize * SECTOR;
        let mut work: Vec<u32> = self.dir_paths.keys().copied().collect();
        let mut seen: HashSet<u32> = HashSet::new();
        let mut out = Vec::new();
        let mut complete = true;
        while let Some(dir_cluster) = work.pop() {
            if !seen.insert(dir_cluster) {
                continue;
            }
            let Some(dir_chain) = self.chain_of(dir_cluster) else {
                complete = false;
                continue;
            };
            if dir_chain.iter().any(|&c| !self.cluster_in_range(c)) {
                complete = false;
                continue;
            }
            let mut dir_bytes = Vec::with_capacity(dir_chain.len() * cluster_bytes);
            for c in &dir_chain {
                let base = self.cluster_to_lba(*c);
                for s in 0..spc {
                    dir_bytes.extend_from_slice(&self.read_sector(base + s));
                }
            }
            if !directory_image_is_whole(&dir_bytes) {
                complete = false;
            }
            for e in crate::katea_write::parse_dir(&dir_bytes) {
                match crate::katea_write::classify(&e, &self.system_names) {
                    crate::katea_write::EntryAction::Skip => {}
                    crate::katea_write::EntryAction::MakeDir {
                        name,
                        first_cluster,
                    } => {
                        out.push(LiveEntry {
                            dir_cluster,
                            name,
                            first_cluster,
                            size: 0,
                            is_dir: true,
                        });
                        // Only descend into dirs Katea already knows; a just-MKDIR'd
                        // subdir is registered by the materialize phase first, then
                        // gathered on the next pass.
                        if self.dir_paths.contains_key(&first_cluster) {
                            work.push(first_cluster);
                        }
                    }
                    crate::katea_write::EntryAction::MakeFile {
                        name,
                        first_cluster,
                        size,
                    } => {
                        out.push(LiveEntry {
                            dir_cluster,
                            name,
                            first_cluster,
                            size,
                            is_dir: false,
                        });
                    }
                }
            }
        }
        (out, complete)
    }

    /// Reconcile the overlay to the host folder: walk every known directory and
    /// atomically materialize each *complete, touched, changed* 8.3 file, and
    /// create host subdirectories for MKDIR'd entries. Conservative — an
    /// incomplete or ambiguous entry is held in the overlay and retried next pass.
    /// This entry point is the forced final pass used by explicit flush/eject.
    pub(crate) fn reconcile(&mut self) {
        let _ = self.flush_guest_writes();
    }

    /// Reconcile after a successful ATA write command.
    #[cfg(test)]
    pub(crate) fn reconcile_after_write(&mut self) -> CommitGuestWriteResult {
        self.commit_guest_write_batch(GuestWriteRoute::Pio)
    }

    pub(crate) fn commit_guest_write_batch(
        &mut self,
        route: GuestWriteRoute,
    ) -> CommitGuestWriteResult {
        let projection_started = std::time::Instant::now();
        let sectors = self.batch_sector_writes;
        let projection_operations_before = self.counters.projection_operations.get();
        match route {
            GuestWriteRoute::Int13 => {
                bump(&self.counters.int13_write_commands, 1);
                bump(&self.counters.int13_write_sectors, sectors);
            }
            GuestWriteRoute::Pio => {
                bump(&self.counters.pio_write_commands, 1);
                bump(&self.counters.pio_write_sectors, sectors);
            }
            GuestWriteRoute::Dma => {
                bump(&self.counters.dma_write_commands, 1);
                bump(&self.counters.dma_write_sectors, sectors);
            }
        }
        self.host_io_failed = false;

        if self.directory_dirty
            || (self.metadata_reconcile_pending && !self.fat_dirty_clusters.is_empty())
        {
            bump(&self.counters.metadata_projection_passes, 1);
            // Two whole-tree passes: `reconcile_mode` gathers every live entry
            // and `metadata_projection_pending` gathers them again to decide
            // whether the next command owes another pass. An incremental
            // dirty-set was the obvious answer and was measured instead of
            // taken. On the 17 MiB install row both passes cost 0.000 ms of a
            // 34.5 ms projection phase -- that install dirties a directory
            // three times. On an install shaped to dirty one on every command
            // (64 files, one commit each), the two gathers were 2.4% of the
            // row's 25.4 ms while the host writes they lead to were 94%. The
            // walks are cheap because they read synthesized metadata out of
            // memory; the projection is expensive because it writes files.
            // Restructuring this would put the projection model's invariants at
            // risk to buy back a fiftieth of a row, so it was not done.
            self.reconcile_mode(ReconcileMode::AfterWrite);
            self.metadata_reconcile_pending =
                self.host_io_failed || self.metadata_projection_pending();
        } else {
            self.refresh_changed_fat_projections();
            match self.stream_projected_batch() {
                Ok(_) => {}
                Err(e) => {
                    // Metadata sectors are excluded: `stream_projected_batch`
                    // has always skipped them, and the index is keyed by data
                    // cluster, which a FAT or directory sector does not have.
                    let stranded: Vec<u32> = self
                        .batch_lbas
                        .iter()
                        .copied()
                        .filter(|lba| self.store.is_pending(*lba) && !self.is_metadata_sector(*lba))
                        .collect();
                    for lba in stranded {
                        self.note_unmapped(lba);
                    }
                    // A held sector's cluster can be projectable already -- what
                    // failed is the host write, which is not a projection event.
                    // `stream_projected_batch` drained the revisit tickets at its
                    // top and then aborted, so re-arm every cluster still holding
                    // a sector. The keys are precise, not over-broad: anything
                    // that projected before the abort was already dropped by
                    // `forget_unmapped`.
                    let held: Vec<u32> = self.unmapped_by_cluster.keys().copied().collect();
                    self.newly_projectable_clusters.extend(held);
                    self.note_host_write_failure();
                    eprintln!("katea: projecting a guest write batch failed: {e}");
                }
            }
        }

        self.batch_lbas.clear();
        self.batch_sector_writes = 0;
        self.directory_dirty = false;
        self.fat_dirty_clusters.clear();
        let projected = self.counters.projection_operations.get() > projection_operations_before;
        let result = if self.host_io_failed {
            CommitGuestWriteResult::HostIoFailure
        } else if projected {
            CommitGuestWriteResult::Projected
        } else {
            CommitGuestWriteResult::Deferred
        };
        self.note_projection_wall(projection_started);
        result
    }

    pub(crate) fn note_guest_read_batch(
        &self,
        route: GuestStorageRoute,
        sectors: u64,
        wait_ticks: u64,
    ) {
        match route {
            GuestStorageRoute::Int13 => {
                bump(&self.counters.int13_read_commands, 1);
                bump(&self.counters.int13_read_sectors, sectors);
                bump(&self.counters.int13_read_wait_ticks, wait_ticks);
            }
            GuestStorageRoute::Pio => {
                bump(&self.counters.pio_read_commands, 1);
                bump(&self.counters.pio_read_sectors, sectors);
                bump(&self.counters.pio_read_wait_ticks, wait_ticks);
            }
            GuestStorageRoute::Dma => {
                bump(&self.counters.dma_read_commands, 1);
                bump(&self.counters.dma_read_sectors, sectors);
                bump(&self.counters.dma_read_wait_ticks, wait_ticks);
            }
        }
    }

    pub(crate) fn note_guest_write_wait(&self, route: GuestStorageRoute, wait_ticks: u64) {
        match route {
            GuestStorageRoute::Int13 => bump(&self.counters.int13_write_wait_ticks, wait_ticks),
            GuestStorageRoute::Pio => bump(&self.counters.pio_write_wait_ticks, wait_ticks),
            GuestStorageRoute::Dma => bump(&self.counters.dma_write_wait_ticks, wait_ticks),
        }
    }

    pub(crate) fn flush_guest_writes(&mut self) -> CommitGuestWriteResult {
        let projection_started = std::time::Instant::now();
        let projection_operations_before = self.counters.projection_operations.get();
        self.host_io_failed = false;
        bump(&self.counters.metadata_projection_passes, 1);
        self.reconcile_mode(ReconcileMode::Final);
        self.metadata_reconcile_pending = self.host_io_failed || self.metadata_projection_pending();
        if let Err(e) = self.flush_write_handles() {
            self.note_host_write_failure();
            eprintln!("katea: flushing host files failed: {e}");
        }
        self.batch_lbas.clear();
        self.batch_sector_writes = 0;
        self.directory_dirty = false;
        self.fat_dirty_clusters.clear();
        let projected = self.counters.projection_operations.get() > projection_operations_before;
        let result = if self.host_io_failed {
            CommitGuestWriteResult::HostIoFailure
        } else if projected {
            CommitGuestWriteResult::Projected
        } else {
            CommitGuestWriteResult::Deferred
        };
        self.note_projection_wall(projection_started);
        result
    }

    fn note_host_write_failure(&mut self) {
        self.host_io_failed = true;
        bump(&self.counters.host_write_failures, 1);
    }

    fn note_projection_operation(&self, bytes: u64) {
        bump(&self.counters.projection_operations, 1);
        bump(&self.counters.projection_bytes, bytes);
    }

    fn note_projection_wall(&self, started: std::time::Instant) {
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        bump(&self.counters.projection_wall_ns, elapsed);
        // The sum says how much of the session went into projection; the max says
        // whether any single pass was long enough for the user to see it as a
        // freeze. Same `Instant` pair, no extra clock read.
        self.counters
            .projection_max_ns
            .set(self.counters.projection_max_ns.get().max(elapsed));
    }

    /// THE ANTI-CLOBBER GUARD, AS A DEFINITION.
    ///
    /// A file is ambiguous when some cluster of its chain is not unambiguously
    /// its own. Phase 3 holds such a file (`blocked_projection_keys`) instead of
    /// materializing it, which is what stops Katea writing one guest file's bytes
    /// into another guest file's host path when the guest's FAT has two directory
    /// entries pointing into the same clusters. Writing `chain(k)` for this
    /// entry's chain when the walk returns `Some`:
    ///
    /// > **ambiguous = { k ∈ LiveFiles : ∃ c ∈ chain(k) with
    /// > c ∈ directory_clusters, or ∃ k′ ∈ LiveFiles, k′ ≠ k, c ∈ chain(k′) }**
    ///
    /// Three properties of the loop below that the definition captures and that
    /// any faster implementation has to reproduce:
    ///
    /// 1. **Order-independence.** `HashMap::insert` keeps the last writer, so
    ///    three files claiming one cluster flag pairwise on each insert and all
    ///    three end up in the set, whatever order `live` is in.
    /// 2. **A file does not collide with itself.** `other != key` — so two
    ///    directory entries with the SAME key (a guest can write duplicate 8.3
    ///    names) union their chains rather than flagging each other, and a chain
    ///    that revisits a cluster is impossible anyway (it would not terminate,
    ///    and `katea_write::chain` returns `None`).
    /// 3. **A held chain contributes nothing.** A `None` walk is skipped here and
    ///    held at the `MakeFile` arm instead.
    ///
    /// This is deliberately a named function rather than an inline loop: it is
    /// the REFERENCE the incremental claim index is graded against, both by
    /// fixtures and, on a live row, by `IZARRAVM_KATEA_CLAIMS_VERIFY=1`. If the
    /// two ever disagree, this one is right.
    ///
    /// Cost: O(clusters in the mounted folder), every pass, for a write that
    /// touched one file. Measured at 4.92 ms per pass on a 498 MB folder — 77% of
    /// the projection wall.
    fn ambiguous_by_full_scan(&self, live: &[LiveEntry]) -> HashSet<(u32, [u8; 11])> {
        let mut cluster_claims: HashMap<u32, (u32, [u8; 11])> = HashMap::new();
        let mut ambiguous_files = HashSet::new();
        for entry in live
            .iter()
            .filter(|entry| !entry.is_dir && entry.first_cluster >= 2)
        {
            let key = (entry.dir_cluster, entry.name);
            let Some(chain) = self.chain_of(entry.first_cluster) else {
                continue;
            };
            for cluster in chain {
                if self.directory_clusters.contains_key(&cluster) {
                    ambiguous_files.insert(key);
                }
                if let Some(other) = cluster_claims.insert(cluster, key)
                    && other != key
                {
                    ambiguous_files.insert(other);
                    ambiguous_files.insert(key);
                }
            }
        }
        ambiguous_files
    }

    /// Abandon the incremental claim index. The next pass rebuilds it from a full
    /// scan.
    ///
    /// Called from every place where we do not know WHAT changed, which is the
    /// only safe reading of "something changed": a degraded walk, a directory
    /// cluster being un-registered, and a `gather_live` that dropped a directory.
    /// Cannot be wrong, only cold.
    fn poison_claims(&mut self) {
        self.claims_valid = false;
        self.claim_by_key.clear();
        self.claimants.clear();
    }

    /// Register `cluster` as belonging to directory `owner`, flagging any live
    /// claimant if that makes the cluster contested for the first time.
    ///
    /// THE UNDER-FLAG HAZARD THIS CLOSES. A file's chain can be perfectly
    /// unchanged -- its FAT token still current, so the index never revisits it
    /// -- while a cluster it holds BECOMES a directory cluster underneath it.
    /// Nothing in the FAT epoch sees that, so the growth side of
    /// `directory_clusters` needs its own hook. It is O(clusters added), which is
    /// one directory chain, not the mount.
    ///
    /// The shrink side (a directory deleted, `retain` in phase 2) removes
    /// ambiguity, and removing a flag is the dangerous direction, so it poisons
    /// instead. Directory deletes are rare; one cold pass is the right price.
    fn register_directory_cluster(&mut self, cluster: u32, owner: u32) {
        if self.directory_clusters.insert(cluster, owner).is_some() {
            return; // already a directory cluster; contested-ness unchanged
        }
        let Some(list) = self.claimants.get(&cluster) else {
            return;
        };
        if list.len() >= 2 {
            return; // already contested by the multi-claimant rule
        }
        let Some(key) = list.only() else {
            return; // no live claimant: nothing to flag
        };
        if let Some(claim) = self.claim_by_key.get_mut(&key) {
            claim.bad += 1;
        }
    }

    /// Drop `key`'s claims, decrementing any other key that stops being contested
    /// as a result.
    fn retract_claim(&mut self, key: (u32, [u8; 11])) {
        let Some(claim) = self.claim_by_key.remove(&key) else {
            return;
        };
        for cluster in claim.clusters {
            let dir = self.directory_clusters.contains_key(&cluster);
            let Some(list) = self.claimants.get_mut(&cluster) else {
                continue;
            };
            let old_len = list.len();
            let new_len = list.remove(key);
            // Contested is `>= 2 claimants OR a directory cluster`. The only
            // transition a retraction can make is 2 -> 1 with no directory
            // overlap, and then exactly one key stops being contested here.
            if !dir
                && old_len == 2
                && new_len == 1
                && let Some(other) = list.only()
                && let Some(c) = self.claim_by_key.get_mut(&other)
            {
                c.bad = c.bad.saturating_sub(1);
            }
            if new_len == 0 {
                self.claimants.remove(&cluster);
            }
        }
    }

    /// Walk every live entry with this key, union their chains, and record the
    /// claim. Returns false if any walk was degraded, in which case the caller
    /// must poison: a claim whose dependency set is incomplete is exactly the
    /// stale claim the guard cannot afford.
    fn insert_claim(&mut self, key: (u32, [u8; 11]), firsts: &[u32]) -> bool {
        #[cfg(test)]
        self.claim_inserts
            .set(self.claim_inserts.get().saturating_add(1));
        let mut units = Vec::with_capacity(firsts.len());
        let mut clusters: Vec<u32> = Vec::new();
        for first in firsts {
            let (result, token) = self.chain_with_token(*first);
            let Some((epoch, sectors)) = token else {
                return false;
            };
            units.push(ClaimUnit {
                first_cluster: *first,
                epoch,
                sectors,
            });
            if let Some(chain) = result {
                clusters.extend(chain);
            }
        }
        // One entry per cluster per KEY, so the key appears at most once in each
        // `claimants` list -- which is what makes the 2 -> 1 transition in
        // `retract_claim` exact. A single chain cannot repeat a cluster (that
        // would not terminate and `katea_write::chain` returns `None`), so the
        // sort is needed ONLY to collapse an overlap between two entries sharing
        // a name, which is the rare duplicate-8.3-name case. Skipping it in the
        // single-entry case is what keeps the cold build to one pass over the
        // chain rather than a sort of it.
        if units.len() > 1 {
            clusters.sort_unstable();
            clusters.dedup();
        }

        let mut bad = 0u32;
        let mut newly_contested: Vec<(u32, [u8; 11])> = Vec::new();
        for cluster in &clusters {
            let dir = self.directory_clusters.contains_key(cluster);
            match self.claimants.get_mut(cluster) {
                None => {
                    self.claimants.insert(*cluster, Claimants::One(key));
                    if dir {
                        bad += 1;
                    }
                }
                Some(list) => {
                    let old_len = list.len();
                    let old_contested = old_len >= 2 || dir;
                    // The one key that was here alone becomes contested now.
                    if !old_contested
                        && old_len == 1
                        && let Some(other) = list.only()
                    {
                        newly_contested.push(other);
                    }
                    list.push(key);
                    if list.len() >= 2 || dir {
                        bad += 1;
                    }
                }
            }
        }
        for other in newly_contested {
            if let Some(c) = self.claim_by_key.get_mut(&other) {
                c.bad += 1;
            }
        }
        self.claim_by_key.insert(
            key,
            KeyClaim {
                units,
                clusters,
                bad,
            },
        );
        true
    }

    /// The anti-clobber guard, evaluated incrementally.
    ///
    /// Same set as [`Self::ambiguous_by_full_scan`], which is the definition;
    /// this reaches it by touching only the keys whose contribution can have
    /// changed. See [`KeyClaim::bad`] for why it is reference-counted rather than
    /// flagged, and [`Self::register_directory_cluster`] /
    /// [`Self::poison_claims`] for the two invalidation sources the FAT epoch
    /// does not cover.
    ///
    /// `dirs_complete` is `gather_live`'s verdict on its own output. It is not a
    /// nicety: `gather_live` has three paths that drop a whole directory without
    /// `store.read_errors()` moving -- a chain that will not resolve, a chain
    /// link out of range, and `parse_dir` stopping at a zeroed entry with live
    /// entries behind it, which is precisely what an installer zeroing a
    /// directory cluster mid-write produces. Retracting keys off such a live set
    /// would be a retraction on unreliable evidence, so instead the pass falls
    /// back to the full scan (which sees the same incomplete `live`, so the
    /// answer stays exactly the reference's) and rebuilds next pass.
    fn ambiguous_files(
        &mut self,
        live: &[LiveEntry],
        dirs_complete: bool,
    ) -> HashSet<(u32, [u8; 11])> {
        if !self.incremental_claims || !dirs_complete {
            if !dirs_complete {
                self.poison_claims();
            }
            return self.ambiguous_by_full_scan(live);
        }
        let incremental = self.ambiguous_incrementally(live);
        if !self.claims_verify {
            return incremental;
        }
        let reference = self.ambiguous_by_full_scan(live);
        if incremental != reference {
            eprintln!(
                "katea: INCREMENTAL CLAIM INDEX DIVERGED -- incremental {} keys, reference {} \
                 keys; using the reference and rebuilding. This is a bug; re-run with \
                 IZARRAVM_KATEA_INCREMENTAL_CLAIMS=0 to bypass the index entirely.",
                incremental.len(),
                reference.len()
            );
            #[cfg(test)]
            self.claim_divergences
                .set(self.claim_divergences.get().saturating_add(1));
            self.poison_claims();
        }
        reference
    }

    fn ambiguous_incrementally(&mut self, live: &[LiveEntry]) -> HashSet<(u32, [u8; 11])> {
        // The live entries the guard reasons about, keyed, in `live` order so the
        // amortised refresh rotation is deterministic.
        let mut order: Vec<(u32, [u8; 11])> = Vec::new();
        let mut units: HashMap<(u32, [u8; 11]), Vec<u32>> = HashMap::new();
        for entry in live.iter().filter(|e| !e.is_dir && e.first_cluster >= 2) {
            let key = (entry.dir_cluster, entry.name);
            let slot = units.entry(key).or_insert_with(|| {
                order.push(key);
                Vec::new()
            });
            slot.push(entry.first_cluster);
        }

        if !self.claims_valid {
            self.claim_by_key.clear();
            self.claimants.clear();
            self.claims_valid = true;
        }

        // Retract keys that are no longer live. Sound because `dirs_complete`
        // held: every directory was gathered to completion, so a key missing from
        // `units` really is gone.
        let stale: Vec<(u32, [u8; 11])> = self
            .claim_by_key
            .keys()
            .filter(|key| !units.contains_key(*key))
            .copied()
            .collect();
        for key in stale {
            self.retract_claim(key);
        }

        self.claims_pass = self.claims_pass.wrapping_add(1);
        let rotation = self.claims_pass % CLAIM_REFRESH_PERIOD;
        let mut poisoned = false;
        for (index, key) in order.iter().enumerate() {
            let firsts = &units[key];
            #[cfg_attr(not(test), allow(unused_mut))]
            let mut forced = index % CLAIM_REFRESH_PERIOD == rotation;
            #[cfg(test)]
            if !self.amortised_refresh {
                forced = false;
            }
            let current = !forced
                && self.claim_by_key.get(key).is_some_and(|claim| {
                    claim.units.len() == firsts.len()
                        && claim.units.iter().zip(firsts.iter()).all(|(unit, first)| {
                            unit.first_cluster == *first
                                && self.chain_token_is_current(unit.epoch, &unit.sectors)
                        })
                });
            if current {
                #[cfg(test)]
                self.claim_skips
                    .set(self.claim_skips.get().saturating_add(1));
                continue;
            }
            self.retract_claim(*key);
            if !self.insert_claim(*key, firsts) {
                poisoned = true;
                break;
            }
        }
        if poisoned {
            self.poison_claims();
            return self.ambiguous_by_full_scan(live);
        }

        order
            .into_iter()
            .filter(|key| self.claim_by_key.get(key).is_some_and(|c| c.bad > 0))
            .collect()
    }

    fn reconcile_mode(&mut self, mode: ReconcileMode) {
        // Drop every cached read view before touching the host. This pass is the
        // funnel for `atomic_write`, `fs::rename` and the deletes, and
        // `File::open` shares delete and rename on Windows, so a rewritten file
        // would otherwise keep serving its pre-write contents through a handle
        // that is still perfectly valid and now points at the wrong bytes. Each
        // individual mutation below invalidates its own path again, because this
        // pass reads file bytes between here and there.
        self.invalidate_host_reads(None);
        self.read_command_end_lba.set(None);
        // A guest-written sector that cannot be read back reads as zeros, and zeros
        // are not a safe input here: phase 2 reads a zeroed FAT entry as "chain
        // freed" and deletes the host file, `parse_dir` stops at the first zero
        // byte and hides live entries, and a zeroed data sector would be written
        // straight into a real host file. So every decision below is bracketed by a
        // snapshot of the store's read-error count, and any unit whose reads failed
        // is held for the next pass instead of acted on. Checking once on entry
        // would not do: the failure is discovered mid-pass, and it is this pass that
        // would do the damage.
        let errors_at_entry = self.store.read_errors();

        // PHASE 1: gather all live entries (read-only).
        let (live, dirs_complete) = self.gather_live();
        if self.store.read_errors() != errors_at_entry {
            // The live set is what every later phase reasons against, so a hole in
            // it cannot be scoped to one chain. Do nothing at all this pass.
            eprintln!("katea: skipping reconcile after a failed read of guest-written data");
            return;
        }
        let ambiguous_files = self.ambiguous_files(&live, dirs_complete);
        if self.store.read_errors() != errors_at_entry {
            eprintln!("katea: skipping reconcile after a failed FAT ownership read");
            return;
        }
        let live_keys: HashSet<(u32, [u8; 11])> =
            live.iter().map(|l| (l.dir_cluster, l.name)).collect();
        // The live scan is complete and trustworthy here. Drop retry markers for
        // entries that disappeared; doing this before the read-error check above
        // could let an incomplete scan discard the only immediate retry state.
        self.retry_gathers.retain(|key, _| live_keys.contains(key));
        let unblocked: Vec<(u32, [u8; 11])> = self
            .blocked_projection_keys
            .iter()
            .filter(|key| !live_keys.contains(*key))
            .copied()
            .collect();
        for key in unblocked {
            self.blocked_projection_keys.remove(&key);
            // Dropping the block is the other way a held sector can become
            // projectable, so the clusters it frees owe the next commit a look.
            if let Some(file) = self.projected_files.get(&key) {
                let claimed: Vec<u32> = file
                    .chain
                    .iter()
                    .copied()
                    .filter(|cluster| self.unmapped_by_cluster.contains_key(cluster))
                    .collect();
                self.newly_projectable_clusters.extend(claimed);
            }
        }
        // first_cluster -> the live entries claiming it (for rename matching in phase 2).
        let mut live_by_cluster: HashMap<u32, Vec<(u32, [u8; 11], bool)>> = HashMap::new();
        for l in &live {
            live_by_cluster.entry(l.first_cluster).or_default().push((
                l.dir_cluster,
                l.name,
                l.is_dir,
            ));
        }

        // PHASE 2: disappearances — a mirrored entry no longer live is a rename/move
        // (a fresh entry claims its clusters), a delete (chain freed, no claimant), or
        // held. Collected read-only, then applied. `handled` tells phase 3 to skip an
        // entry already moved/renamed here.
        let mut deletes: Vec<PendingDelete> = Vec::new();
        let mut renames: Vec<PendingRename> = Vec::new();
        let mut handled: HashSet<(u32, [u8; 11])> = HashSet::new();
        for (key, m) in &self.mirrored {
            if live_keys.contains(key) {
                continue; // still live
            }
            // Rename/move: exactly one FRESH live entry (not already mirrored) holds
            // this entry's clusters, with a real cluster and matching kind.
            if m.first_cluster >= 2
                && let Some(claimants) = live_by_cluster.get(&m.first_cluster)
            {
                let fresh: Vec<&(u32, [u8; 11], bool)> = claimants
                    .iter()
                    .filter(|(d, n, is_dir)| {
                        *is_dir == m.is_dir
                                && !self.mirrored.contains_key(&(*d, *n))
                                // Already taken as another disappearance's rename target
                                // this pass (only reachable under guest-aliased clusters);
                                // hold rather than clobber the first rename's destination.
                                && !handled.contains(&(*d, *n))
                    })
                    .collect();
                if fresh.len() == 1 {
                    let &(ndir, nname, _) = fresh[0];
                    if let Some(host_dir) = self.dir_paths.get(&ndir).cloned() {
                        let new_path = host_dir.join(host_child_name(&nname));
                        renames.push(PendingRename {
                            old_key: *key,
                            new_key: (ndir, nname),
                            old_path: m.host_path.clone(),
                            new_path,
                            first_cluster: m.first_cluster,
                            is_dir: m.is_dir,
                        });
                        handled.insert((ndir, nname));
                        continue;
                    }
                }
            }
            // Delete: empty file/dir, or chain freed with no claimant. Else HOLD.
            // `fat_entry` reads a sector, so bracket it: an unreadable FAT sector
            // reads as zeros, which looks exactly like a freed chain and would
            // delete a real host file on the strength of an I/O failure.
            let errors_before = self.store.read_errors();
            let freed = m.first_cluster < 2 || self.fat_entry(m.first_cluster) == 0;
            if self.store.read_errors() != errors_before {
                eprintln!(
                    "katea: holding {} after a failed read of its FAT chain",
                    m.host_path.display()
                );
                continue; // hold this entry, retry next pass
            }
            let claimed = live_by_cluster.contains_key(&m.first_cluster);
            if freed && !claimed {
                deletes.push(PendingDelete {
                    key: *key,
                    path: m.host_path.clone(),
                    first_cluster: m.first_cluster,
                    is_dir: m.is_dir,
                });
            }
        }
        // Apply renames first (so a move's source path still exists), then deletes.
        for r in renames {
            self.host_write_handles.remove(&r.old_path);
            self.invalidate_host_reads(Some(&r.old_path));
            self.invalidate_host_reads(Some(&r.new_path));
            if let Err(e) = std::fs::rename(&r.old_path, &r.new_path) {
                self.note_host_write_failure();
                eprintln!(
                    "katea: rename {} -> {} failed: {e}",
                    r.old_path.display(),
                    r.new_path.display()
                );
                continue;
            }
            self.note_projection_operation(0);
            let projected = self.projected_files.get(&r.old_key).cloned();
            self.remove_projection(r.old_key);
            if let Some(projected) = projected {
                self.install_projection(
                    r.new_key,
                    r.new_path.clone(),
                    projected.size,
                    projected.chain,
                    projected.extended_past_directory,
                );
            }
            self.mirrored.remove(&r.old_key);
            self.mirrored.insert(
                r.new_key,
                MirrorEntry {
                    host_path: r.new_path.clone(),
                    first_cluster: r.first_cluster,
                    is_dir: r.is_dir,
                    last_fingerprint: None,
                    // A rename moves the host path, so the base view under this
                    // chain moves with it. Carrying a watermark across would let a
                    // later pass skip a gather on the strength of the old path.
                    last_gather: None,
                },
            );
            if r.is_dir {
                self.dir_paths.insert(r.first_cluster, r.new_path);
            }
        }
        // Files before dirs, so a just-emptied dir's host files are gone before we
        // try to remove the (now-empty) host dir. (bool Ord: false(file) < true(dir).)
        deletes.sort_by_key(|d| d.is_dir);
        for d in deletes {
            self.host_write_handles.remove(&d.path);
            self.invalidate_host_reads(Some(&d.path));
            let res = if d.is_dir {
                std::fs::remove_dir(&d.path) // fails (held) if the host dir is non-empty
            } else {
                std::fs::remove_file(&d.path)
            };
            if let Err(e) = res
                && d.path.exists()
            {
                self.note_host_write_failure();
                eprintln!("katea: delete {} failed: {e}", d.path.display());
                continue; // hold
            }
            self.note_projection_operation(0);
            self.mirrored.remove(&d.key);
            self.remove_projection(d.key);
            if d.is_dir {
                self.dir_paths.remove(&d.first_cluster);
                self.directory_clusters
                    .retain(|_, owner| *owner != d.first_cluster);
                // A cluster leaving `directory_clusters` REMOVES ambiguity, and
                // removing a flag is the direction the guard cannot take on
                // trust. Rare (a directory delete), so pay one cold pass.
                self.poison_claims();
            }
        }

        let spc = u32::from(self.geo.spc);
        let cluster_bytes = spc as usize * SECTOR;

        // PHASE 3: materialize creates/overwrites/grows + mkdir.
        // Fixpoint over known directories: MKDIR registers a new directory which is
        // pushed onto the worklist so its files are materialized in the same pass.
        let mut work: Vec<u32> = self.dir_paths.keys().copied().collect();
        let mut seen: HashSet<u32> = HashSet::new();

        while let Some(dir_cluster) = work.pop() {
            if !seen.insert(dir_cluster) {
                continue;
            }
            let Some(host_dir) = self.dir_paths.get(&dir_cluster).cloned() else {
                continue;
            };

            // Read the directory's full bytes by following its own cluster chain.
            // Bracketed: a zeroed directory sector would make `parse_dir` stop early
            // and hide live entries, which phase 2 would read as disappearances.
            let dir_errors_before = self.store.read_errors();
            let Some(dir_chain) = self.chain_of(dir_cluster) else {
                continue; // a corrupt directory chain: hold the whole directory
            };
            if dir_chain.iter().any(|&c| !self.cluster_in_range(c)) {
                continue; // a chain link outside the data region: hold this dir
            }
            // Grows here and is pruned only when a directory is deleted, so a
            // directory whose chain SHRINKS leaves its freed clusters behind. A
            // file that later reuses one has those sectors read as metadata:
            // held in the store rather than projected, and the file marked
            // ambiguous. Conservative -- nothing is lost, the final reconcile
            // still writes it -- but it is a permanent per-session leak.
            //
            // The projection-cost follow-up measured it and left it. The leak
            // is bounded by the clusters a directory has ever held, which is
            // tiny next to a mount's data clusters, and a stranded cluster does
            // not feed the held-sector index: `write_sector` files a sector
            // read as metadata under neither the batch's projectable path nor
            // the held set. So it costs a map entry and an occasional
            // re-materialize, not a scaling term. The honest fix needs the
            // dirty-set the paragraph above declined to build.
            for cluster in &dir_chain {
                self.register_directory_cluster(*cluster, dir_cluster);
            }
            let mut dir_bytes = Vec::with_capacity(dir_chain.len() * cluster_bytes);
            for c in &dir_chain {
                let base = self.cluster_to_lba(*c);
                for s in 0..spc {
                    dir_bytes.extend_from_slice(&self.read_sector(base + s));
                }
            }
            if self.store.read_errors() != dir_errors_before {
                // This directory's own bytes are unreliable, so nothing derived from
                // them can be trusted. Hold the whole directory for the next pass.
                eprintln!("katea: holding a directory after a failed read of guest-written data");
                continue;
            }
            let dir_written = dir_chain
                .iter()
                .any(|c| self.store.was_written_span(self.cluster_to_lba(*c), spc));

            // Decide every entry (read-only); collect actions, then apply.
            let mut mkdirs: Vec<(u32, [u8; 11], u32, std::path::PathBuf)> = Vec::new();
            let mut writes: Vec<PendingWrite> = Vec::new();
            let mut streams: Vec<PendingStream> = Vec::new();
            // Entries whose bytes matched what the host already holds: nothing to
            // write, but the decision is complete, so date them.
            let mut watermarks: Vec<((u32, [u8; 11]), Gather)> = Vec::new();
            // Counted here and folded in below: the decision loop holds `self`
            // shared, so it cannot touch `self.gathers` directly.
            let mut gathers = 0u64;
            #[cfg(test)]
            let mut gathered_bytes = 0u64;

            for e in crate::katea_write::parse_dir(&dir_bytes) {
                match crate::katea_write::classify(&e, &self.system_names) {
                    crate::katea_write::EntryAction::Skip => {}
                    crate::katea_write::EntryAction::MakeDir {
                        name,
                        first_cluster,
                    } => {
                        if handled.contains(&(dir_cluster, name)) {
                            continue; // already renamed in phase 2 (symmetry with MakeFile)
                        }
                        let path = host_dir.join(host_child_name(&name));
                        mkdirs.push((dir_cluster, name, first_cluster, path));
                    }
                    crate::katea_write::EntryAction::MakeFile {
                        name,
                        first_cluster,
                        size,
                    } => {
                        // Bracket the whole decision, starting before the first read
                        // it makes. A zeroed FAT sector would already end the chain
                        // walk below as "incomplete" and hold, but relying on that
                        // would make this file's safety depend on the shape of the
                        // corruption rather than on the failure itself.
                        let errors_before = self.store.read_errors();
                        let Some(fchain) = self.chain_of(first_cluster) else {
                            continue; // incomplete/corrupt chain: hold
                        };
                        if fchain.iter().any(|&c| !self.cluster_in_range(c)) {
                            continue; // chain references an out-of-range cluster: hold
                        }
                        let capacity = fchain.len() as u64 * cluster_bytes as u64;
                        if u64::from(size) > capacity {
                            continue; // not enough clusters yet: hold
                        }
                        // Touched this session? Non-empty files must have written
                        // data; a brand-new name in a written directory counts even
                        // when empty. Untouched tree/system files are skipped. This
                        // asks the store's historical bits, which remain after a
                        // projected sector is acknowledged.
                        let data_written = fchain
                            .iter()
                            .any(|c| self.store.was_written_span(self.cluster_to_lba(*c), spc));
                        let is_new = !self.mirrored.contains_key(&(dir_cluster, name));
                        if handled.contains(&(dir_cluster, name)) {
                            continue; // already moved/renamed in phase 2
                        }
                        if !(data_written || (dir_written && is_new)) {
                            continue;
                        }

                        // `data_written` never resets, so without this a file the
                        // guest touched once is re-read in full on every later pass
                        // for the rest of the session, just to have the fingerprint
                        // below reject it. The gathered bytes are a function of the
                        // chain, the size, the store's copy of any written chain
                        // sector, and the base view under any unwritten one. If the
                        // first two are unchanged, no chunk the chain touches has
                        // been written since, and the last gather read nothing from
                        // the base view, then the bytes are identical and the
                        // fingerprint would match: same outcome, no read.
                        let chain_now = chain_id(&fchain);
                        let chain_seq = fchain
                            .iter()
                            .map(|c| self.store.max_seq_in(self.cluster_to_lba(*c), spc))
                            .max()
                            .unwrap_or(0);
                        let key = (dir_cluster, name);
                        if ambiguous_files.contains(&key) {
                            self.blocked_projection_keys.insert(key);
                            continue;
                        }
                        let last_gather = self.mirrored.get(&key).and_then(|m| m.last_gather);
                        if let Some(g) = last_gather
                            && g.all_present
                            && g.size == size
                            && g.chain_id == chain_now
                            && chain_seq <= g.seq
                        {
                            self.retry_gathers.remove(&key);
                            continue; // provably unchanged since the last decision
                        }

                        let state = FileState {
                            first_cluster,
                            size,
                            chain_id: chain_now,
                            chain_seq,
                        };
                        let retry_exact = self.retry_gathers.get(&key) == Some(&state);
                        if mode == ReconcileMode::AfterWrite && !retry_exact {
                            let path = self
                                .mirrored
                                .get(&key)
                                .map(|m| m.host_path.clone())
                                .unwrap_or_else(|| host_dir.join(host_child_name(&name)));
                            streams.push(PendingStream {
                                path,
                                key,
                                first_cluster,
                                size,
                                chain: fchain,
                                state,
                                sectors: None,
                                set_len: true,
                            });
                            continue;
                        }

                        // Gather the file bytes (store-then-base), truncate to size.
                        // `all_present` records whether the base view contributed:
                        // if it did, the bytes can change without any guest write
                        // (our own atomic_write rewrites the very host file the base
                        // view reads), so the skip above must not fire next pass.
                        let decided_at = self.store.seq();
                        gathers += 1;
                        #[cfg(test)]
                        {
                            gathered_bytes += u64::from(size);
                        }
                        let mut all_present = true;
                        let mut data = Vec::with_capacity(size as usize);
                        'gather: for c in &fchain {
                            let base = self.cluster_to_lba(*c);
                            for s in 0..spc {
                                if data.len() >= size as usize {
                                    break 'gather;
                                }
                                all_present &= self.store.was_written(base + s);
                                data.extend_from_slice(&self.read_sector(base + s));
                            }
                        }
                        data.truncate(size as usize);
                        if self.store.read_errors() != errors_before {
                            // A guest-written sector of this file could not be read
                            // back, so `data` holds zeros where its bytes should be.
                            // Writing that to the host file would destroy real
                            // content: hold this file and retry next pass.
                            eprintln!(
                                "katea: holding {} after a failed read of its own data",
                                crate::katea_volume::decode_83(&name)
                            );
                            self.retry_gathers.insert(key, state);
                            continue;
                        }

                        let fp = crate::katea_write::fingerprint(&data);
                        let gather = Gather {
                            size,
                            chain_id: chain_now,
                            seq: decided_at,
                            all_present,
                        };
                        if self
                            .mirrored
                            .get(&(dir_cluster, name))
                            .and_then(|m| m.last_fingerprint)
                            == Some(fp)
                        {
                            // Unchanged since last pass. Date the entry so the skip
                            // above can spare the next pass this same re-read.
                            watermarks.push((key, gather));
                            continue;
                        }
                        let host_path = self
                            .mirrored
                            .get(&(dir_cluster, name))
                            .map(|m| m.host_path.clone())
                            .unwrap_or_else(|| host_dir.join(host_child_name(&name)));
                        writes.push(PendingWrite {
                            path: host_path,
                            data,
                            key: (dir_cluster, name),
                            fingerprint: fp,
                            first_cluster,
                            state,
                            gather,
                            chain: fchain,
                        });
                    }
                }
            }

            // Apply mutations + host I/O (no `self` borrow held across reads now).
            self.gathers += gathers;
            #[cfg(test)]
            {
                self.gathered_bytes += gathered_bytes;
            }
            for (key, gather) in watermarks {
                if let Some(m) = self.mirrored.get_mut(&key) {
                    m.last_gather = Some(gather);
                }
                self.retry_gathers.remove(&key);
            }
            for (parent, name, first_cluster, path) in mkdirs {
                if let Err(e) = std::fs::create_dir_all(&path) {
                    self.note_host_write_failure();
                    eprintln!("katea: mkdir {} failed: {e}", path.display());
                    continue;
                }
                self.note_projection_operation(0);
                self.dir_paths.entry(first_cluster).or_insert(path.clone());
                self.mirrored.entry((parent, name)).or_insert(MirrorEntry {
                    host_path: path,
                    first_cluster,
                    is_dir: true,
                    last_fingerprint: None,
                    last_gather: None,
                });
                if !seen.contains(&first_cluster) {
                    work.push(first_cluster);
                }
            }
            for stream in streams {
                let key = stream.key;
                let state = stream.state;
                if let Err(e) = self.stream_file_overlay(stream) {
                    self.note_host_write_failure();
                    eprintln!(
                        "katea: streaming {} failed: {e}",
                        crate::katea_volume::decode_83(&key.1)
                    );
                    self.retry_gathers.insert(key, state);
                }
            }
            for w in writes {
                #[cfg(test)]
                {
                    self.atomic_writes += 1;
                    self.atomic_write_bytes += w.data.len() as u64;
                }
                self.host_write_handles.remove(&w.path);
                self.invalidate_host_reads(Some(&w.path));
                match crate::katea_write::atomic_write(&w.path, &w.data) {
                    Ok(()) => {
                        self.note_projection_operation(w.data.len() as u64);
                        self.install_projection(
                            w.key,
                            w.path.clone(),
                            w.gather.size,
                            w.chain.clone(),
                            // A whole-file rewrite from the guest's own declared
                            // size: the host file now matches the entry exactly.
                            false,
                        );
                        self.mirrored.insert(
                            w.key,
                            MirrorEntry {
                                host_path: w.path.clone(),
                                first_cluster: w.first_cluster,
                                is_dir: false,
                                last_fingerprint: Some(w.fingerprint),
                                // Must be carried: this rebuilds the entry
                                // wholesale, so dropping the watermark here would
                                // silently disable the skip on every write.
                                last_gather: Some(w.gather),
                            },
                        );
                        self.retry_gathers.remove(&w.key);
                    }
                    Err(e) => {
                        self.note_host_write_failure();
                        // Hold on failure: the real host file is untouched; retry
                        // next pass. atomic_write guarantees no torn file.
                        eprintln!("katea: materialize {} failed: {e}", w.path.display());
                        self.retry_gathers.insert(w.key, w.state);
                    }
                }
            }
        }
    }

    /// Store one guest-written sector. Reads return it until projection can safely
    /// acknowledge it or the volume is dropped. Pending payload may spill to disk.
    pub(crate) fn write_sector(&mut self, lba: u32, data: &[u8; SECTOR]) {
        let metadata = self.note_metadata_write(lba);
        self.store.insert(lba, data);
        self.batch_lbas.insert(lba);
        self.batch_sector_writes = self.batch_sector_writes.saturating_add(1);
        if !metadata && !self.batch_lba_is_projected(lba) {
            self.note_unmapped(lba);
        }
        bump(&self.counters.sector_writes, 1);
    }

    /// The data cluster holding `lba`, or `None` for a sector below the data
    /// region (the MBR, the VBR, the FATs). The unmapped index is keyed by this,
    /// so a sector without one never enters it.
    fn data_cluster_of(&self, lba: u32) -> Option<u32> {
        let first_data = self.geo.part_start + self.geo.first_data_sector;
        if lba < first_data {
            return None;
        }
        Some(ROOT_CLUSTER + (lba - first_data) / u32::from(self.geo.spc))
    }

    /// Hold `lba` in the unmapped index. Idempotent: a sector re-examined by a
    /// later commit and still unprojectable is already there.
    fn note_unmapped(&mut self, lba: u32) {
        let Some(cluster) = self.data_cluster_of(lba) else {
            return;
        };
        if self
            .unmapped_by_cluster
            .entry(cluster)
            .or_default()
            .insert(lba)
        {
            self.unmapped_sectors = self.unmapped_sectors.saturating_add(1);
        }
    }

    /// Drop `lba` from the unmapped index, because its payload has reached the
    /// host and the store no longer holds it. Called from the one site that
    /// acknowledges a sector, so the index never outlives what it describes.
    fn forget_unmapped(&mut self, lba: u32) {
        let Some(cluster) = self.data_cluster_of(lba) else {
            return;
        };
        let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.unmapped_by_cluster.entry(cluster)
        else {
            return;
        };
        if entry.get_mut().remove(&lba) {
            self.unmapped_sectors = self.unmapped_sectors.saturating_sub(1);
        }
        if entry.get().is_empty() {
            entry.remove();
        }
    }

    fn batch_lba_is_projected(&self, lba: u32) -> bool {
        if lba < self.geo.part_start + self.geo.first_data_sector {
            return false;
        }
        let rel = lba - self.geo.part_start - self.geo.first_data_sector;
        let cluster = ROOT_CLUSTER + rel / u32::from(self.geo.spc);
        self.projected_clusters.contains_key(&cluster)
    }

    /// Read one whole-disk sector by absolute LBA. Resolves entirely from
    /// in-memory metadata except for `HostFile` data and spilled guest writes,
    /// read on demand. Out-of-range or unmapped sectors read back as zeros.
    ///
    /// Drops the degraded flag; use [`read_sector_checked`](Self::read_sector_checked)
    /// when the caller intends to REMEMBER the bytes.
    pub(crate) fn read_sector(&self, lba: u32) -> [u8; SECTOR] {
        self.read_sector_checked(lba).bytes
    }

    /// The same read, saying whether it degraded. See [`SectorRead`].
    pub(crate) fn read_sector_checked(&self, lba: u32) -> SectorRead {
        bump(&self.counters.sector_reads, 1);
        match self.store.get(lba) {
            Ok(Some(s)) => return SectorRead::ok(s),
            Ok(None) => {}
            Err(e) => {
                // The sector exists but its payload could not be read back. Never
                // fall through to the base view: that would silently regress a
                // guest-written sector to its pre-write content. Zeros match how
                // this module already treats an unreadable host file, and the
                // store's error count makes `reconcile` hold this chain rather
                // than act on what we are about to return.
                eprintln!("katea: guest-written sector {lba} could not be read back: {e}");
                return SectorRead::degraded();
            }
        }
        if let Some(served) = self.read_command_window_sector(lba) {
            return served;
        }
        if lba == 0 {
            return SectorRead::ok(self.mbr);
        }
        if lba < self.geo.part_start {
            return SectorRead::ok([0u8; SECTOR]);
        }
        let rel = lba - self.geo.part_start; // partition-relative sector

        // Reserved area: VBR (0), FSInfo (1), backup boot (6), backup FSInfo (7).
        if rel == 0 || rel == u32::from(BACKUP_BOOT_SECTOR) {
            return SectorRead::ok(self.vbr);
        }
        if rel == u32::from(FSINFO_SECTOR) || rel == u32::from(BACKUP_FSINFO_SECTOR) {
            return SectorRead::ok(fat32_fsinfo_sector(self.free_count, self.next_free));
        }

        // FAT region: NUM_FATS identical copies, each `fatsz` long.
        let reserved = u32::from(RESERVED_SECTORS);
        let fat_end = reserved + u32::from(NUM_FATS) * self.geo.fatsz;
        if (reserved..fat_end).contains(&rel) {
            // Region census: default-off instrument, gated at the call site so an
            // ordinary run never pays the read-modify-write of the counter block.
            if self.region_census {
                bump(&self.counters.fat_sector_reads, 1);
            }
            let within = (rel - reserved) % self.geo.fatsz;
            return SectorRead::ok(self.base_fat_sector(within));
        }

        // Data region: cluster 2 begins at `first_data_sector`.
        if rel >= self.geo.first_data_sector {
            let data_lba = rel - self.geo.first_data_sector;
            let spc = u32::from(self.geo.spc);
            let cluster = ROOT_CLUSTER + data_lba / spc;
            let sector_in_cluster = data_lba % spc;
            return self.data_sector(cluster, sector_in_cluster);
        }

        SectorRead::ok([0u8; SECTOR])
    }

    /// The base-view FAT sector `within` sectors into a FAT copy, memoized.
    ///
    /// Byte-identical to `self.fat.fat_sector(within, &self.geo)` by construction:
    /// `fat` and `geo` are immutable after `new`, so that call is a pure function
    /// of `within` and any two evaluations agree. The memo therefore cannot change
    /// what any read ordering observes -- it only decides whether the same bytes
    /// are recomputed or copied. See [`Self::fat_sector_cache`] for why it pays.
    fn base_fat_sector(&self, within: u32) -> [u8; SECTOR] {
        if let Some((cached_within, bytes)) = self.fat_sector_cache.borrow().as_ref()
            && *cached_within == within
        {
            return *bytes;
        }
        let bytes = self.fat.fat_sector(within, &self.geo);
        #[cfg(test)]
        self.fat_sector_builds
            .set(self.fat_sector_builds.get().saturating_add(1));
        self.fat_sector_cache.replace(Some((within, bytes)));
        bytes
    }

    /// Resolve one data-region sector by finding the run owning `cluster`, then
    /// serving directory entries or lazy file bytes. A cluster in no run is free
    /// space (zeros).
    fn data_sector(&self, cluster: u32, sector_in_cluster: u32) -> SectorRead {
        if let Some((key, cluster_index)) = self.projected_clusters.get(&cluster)
            && let Some(projected) = self.projected_files.get(key)
        {
            let spc = u32::from(self.geo.spc);
            let byte_off = (u64::from(*cluster_index) * u64::from(spc)
                + u64::from(sector_in_cluster))
                * SECTOR as u64;
            let lba = self.cluster_to_lba(cluster) + sector_in_cluster;
            let span = self.command_span_sectors(lba, spc - sector_in_cluster);
            return read_host_span(
                &projected.path,
                byte_off,
                projected.size,
                self.host_read_context(lba, span),
            );
        }
        // `runs` is sorted by `first_cluster` (the constructor sorts it after
        // `flatten`) and its ranges are disjoint and non-empty: `assign_dir` hands
        // out clusters from one monotonically increasing counter, and
        // `clusters_for` floors every count at 1, so `last >= first` always and no
        // two runs overlap. Those three facts are exactly what makes a binary
        // search return the same run the old linear `find` did -- for a cluster in
        // some run, that run is the unique one, and for a cluster in none, the
        // candidate fails the `last` test and it reads back as free space.
        //
        // This was a linear walk of the whole table on EVERY data sector, so its
        // cost grew with the folder rather than with the bytes the guest asked
        // for: measured at 778 steps per sector for a file deep in an 879-file
        // tree, i.e. 28.4 M steps to load one 18 MB PAK, against 7.9 steps for a
        // root-level file in a three-file folder.
        let mut steps = 0u64;
        let candidate = self.runs.partition_point(|(first, _, _)| {
            steps += 1;
            *first <= cluster
        });
        let found = candidate
            .checked_sub(1)
            .and_then(|i| self.runs.get(i))
            .filter(|(_, last, _)| cluster <= *last);
        bump(&self.counters.run_scan_steps, steps);
        let Some(run) = found else {
            // Region census: default-off, gated at the call site (see the FAT arm).
            if self.region_census {
                bump(&self.counters.dir_or_free_sector_reads, 1);
            }
            return SectorRead::ok([0u8; SECTOR]); // free space
        };
        let cluster_off = cluster - run.0; // cluster index within the run
        let spc = u32::from(self.geo.spc);
        match &run.2 {
            Role::Dir(id) => {
                // Region census: default-off, gated at the call site (see the FAT arm).
                if self.region_census {
                    bump(&self.counters.dir_or_free_sector_reads, 1);
                }
                let d = &self.dirs[*id];
                let sector_in_dir = cluster_off * spc + sector_in_cluster;
                let mut out = [0u8; SECTOR];
                let start = (sector_in_dir as usize) * 16;
                for i in 0..16usize {
                    if let Some(e) = d.entries.get(start + i) {
                        out[i * 32..i * 32 + 32].copy_from_slice(e);
                    }
                }
                SectorRead::ok(out)
            }
            Role::File(id) => {
                let f = &self.files[*id];
                let byte_off = u64::from(cluster_off) * u64::from(spc) * SECTOR as u64
                    + u64::from(sector_in_cluster) * SECTOR as u64;
                let lba = self.cluster_to_lba(cluster) + sector_in_cluster;
                let contiguous = (run.1 - cluster) * spc + spc - sector_in_cluster;
                let span = self.command_span_sectors(lba, contiguous);
                read_source_span(
                    &f.source,
                    byte_off,
                    f.size,
                    self.host_read_context(lba, span),
                )
            }
        }
    }
}

/// Flatten the tree depth-first into `dirs`/`files` + the cluster-run table. The
/// recursion order matches `assign_dir` (dir chain, then its files, then its
/// subdirs), but `read_sector` searches `runs` by cluster, so the order is not
/// load-bearing for reads — only for keeping each entity's run contiguous.
fn flatten(
    dir: &TreeDir,
    is_root: bool,
    dirs: &mut Vec<FlatDir>,
    files: &mut Vec<FlatFile>,
    runs: &mut Vec<(u32, u32, Role)>,
) {
    let id = dirs.len();
    dirs.push(FlatDir {
        entries: dir_entries(dir, is_root),
    });
    runs.push((
        dir.first_cluster,
        dir.first_cluster + dir.cluster_count - 1,
        Role::Dir(id),
    ));
    for f in &dir.files {
        let fid = files.len();
        files.push(FlatFile {
            source: clone_source(&f.source),
            size: u32::try_from(f.source.len()).unwrap_or(u32::MAX),
        });
        runs.push((
            f.first_cluster,
            f.first_cluster + f.cluster_count - 1,
            Role::File(fid),
        ));
    }
    for s in &dir.subdirs {
        flatten(&s.dir, false, dirs, files, runs);
    }
}

/// `FileSource` is not `Clone` (it holds a `Vec`); clone it explicitly.
fn clone_source(s: &FileSource) -> FileSource {
    match s {
        FileSource::InMemory(v) => FileSource::InMemory(v.clone()),
        FileSource::HostFile { path, len } => FileSource::HostFile {
            path: path.clone(),
            len: *len,
        },
    }
}

/// Read one 512-byte span at `byte_off` from a source, zero-padding past `size`.
/// Same contract as `katea_volume::read_source_span`: a `HostFile` opens,
/// seeks, and reads exactly the in-file portion on demand; an I/O error logs and
/// reads back as zeros so a vanished/shrunk host file can't panic the guest.
fn read_source_span(
    source: &FileSource,
    byte_off: u64,
    size: u32,
    context: HostReadContext<'_>,
) -> SectorRead {
    let mut out = [0u8; SECTOR];
    let valid = u64::from(size).saturating_sub(byte_off).min(SECTOR as u64) as usize;
    if valid == 0 {
        return SectorRead::ok(out);
    }
    match source {
        FileSource::InMemory(v) => {
            let start = byte_off as usize;
            // `valid` derives from the declared `size`; clamp it to what the backing
            // Vec actually holds so a size that disagrees with `v.len()` can never
            // panic the read. The padded tail stays zero.
            let avail = valid.min(v.len().saturating_sub(start));
            out[..avail].copy_from_slice(&v[start..start + avail]);
        }
        FileSource::HostFile { path, .. } => {
            return read_host_span(path, byte_off, size, context);
        }
    }
    SectorRead::ok(out)
}

fn read_host_span(
    path: &Path,
    byte_off: u64,
    size: u32,
    context: HostReadContext<'_>,
) -> SectorRead {
    let mut out = [0u8; SECTOR];
    let valid = u64::from(size).saturating_sub(byte_off).min(SECTOR as u64) as usize;
    if valid == 0 {
        return SectorRead::ok(out);
    }
    // A read-ahead hit is a memcpy out of RAM and nothing else: no open, no seek,
    // no read, and no entry in the wall counters -- the same accounting the
    // command window already gets, for the same reason. The range test is exact
    // (same path, and the whole sector inside the buffered byte range), so the
    // bytes are the bytes a fresh positioned read would have returned, provided
    // the buffer has not gone stale. `invalidate_host_reads` is what provides
    // that; see it for why the funnel is complete.
    // A read-ahead hit, and the ramp the next miss will use, both come out of the
    // slot for this path. `slot` is its index, MRU-promoted on use.
    let armed = context.readahead_max > 0;
    let mut ramp = 0u64;
    if armed {
        let mut slots = context.readahead.borrow_mut();
        if let Some(index) = slots.iter().position(|ahead| ahead.path == path) {
            let ahead = &slots[index];
            let offset = byte_off.checked_sub(ahead.start);
            if let Some(off) = offset
                && let Ok(off) = usize::try_from(off)
                && off.saturating_add(valid) <= ahead.bytes.len()
            {
                out[..valid].copy_from_slice(&ahead.bytes[off..off + valid]);
                let ahead = slots.remove(index);
                slots.push(ahead);
                drop(slots);
                bump(&context.counters.host_file_reads, 1);
                bump(&context.counters.host_bytes, valid as u64);
                bump(&context.counters.host_readahead_hits, 1);
                return SectorRead::ok(out);
            }
            // A miss. Only a miss that starts at exactly the end of this path's
            // buffer is a continuing sequential stream and earns a bigger fill;
            // anything else is a seek and takes the command extent.
            if offset == Some(ahead.bytes.len() as u64) {
                ramp = ahead.next_fill;
            }
        }
    }

    // Below the hit test, not above it. A hit does no host I/O, does not enter
    // `host_wall_ns`, and returns before this value is ever read -- so taking the
    // timestamp first was a `QueryPerformanceCounter` bought and thrown away on
    // every hit (duke486 E2: 10,625 of them). From here on the function always
    // reaches the tally, so the clock read is always used.
    let started = std::time::Instant::now();

    let mut opened = 0u64;
    let mut operations = 0u64;
    let mut physical_bytes = 0u64;
    let mut filled = 0u64;
    let mut degraded = false;
    // The command window may only ever cover sectors this command asked for AND
    // that are LBA-contiguous in this file, which is what `command_sectors`
    // encodes. The read-ahead is under no such limit: it is keyed by file byte
    // offset, so it stays correct however the guest's clusters are laid out --
    // which is why it, and not the window, is allowed to run past the command.
    let requested = u64::from(context.command_sectors.max(1)) * SECTOR as u64;
    let fill = requested.max(ramp.min(context.readahead_max));
    let read_len = u64::from(size).saturating_sub(byte_off).min(fill) as usize;
    // Armed, everything read is buffered, even a lone sector: that is what gives
    // the next sequential read something to ramp off. Disarmed, the buffer is
    // allocated only when the command window will use it, exactly as before.
    let mut batch = (armed || read_len > valid).then(|| Vec::with_capacity(read_len));

    let mut cache = context.cache.borrow_mut();
    match cache.iter().position(|(cached, _)| cached == path) {
        Some(index) => {
            // Promote to MRU so the eviction below takes the coldest path.
            let entry = cache.remove(index);
            cache.push(entry);
        }
        None => match File::open(path) {
            Ok(file) => {
                opened = 1;
                if cache.len() >= MAX_HOST_READ_HANDLES {
                    cache.remove(0);
                }
                cache.push((path.to_path_buf(), file));
            }
            Err(e) => eprintln!("katea: open {}: {e}", path.display()),
        },
    }
    match cache.last_mut().filter(|(cached, _)| cached == path) {
        Some((_, file)) => {
            operations = 1;
            let read = file.seek(SeekFrom::Start(byte_off)).and_then(|_| {
                if let Some(batch) = &mut batch {
                    file.take(read_len as u64).read_to_end(batch)
                } else {
                    file.read_exact(&mut out[..valid]).map(|_| valid)
                }
            });
            match read {
                Ok(read_bytes) if read_bytes >= valid => {
                    physical_bytes = read_bytes as u64;
                    if let Some(mut batch) = batch {
                        if read_bytes < read_len {
                            batch.truncate(read_bytes / SECTOR * SECTOR);
                        }
                        out[..valid].copy_from_slice(&batch[..valid]);
                        if armed {
                            // The read-ahead owns the buffer, by move. It covers
                            // everything the LBA-keyed window would have (same
                            // starting offset, never shorter than the command
                            // extent) and is keyed more precisely, so filling
                            // both would be a copy for a redundant lookup.
                            // A FILL is a read that went past what the command
                            // asked for. Counting anything longer than the
                            // single served sector would count an ordinary
                            // command-extent read as read-ahead and flatter the
                            // hits-per-fill ratio, and this counter is an
                            // acceptance instrument.
                            if batch.len() as u64 > requested {
                                filled = 1;
                            }
                            let next_fill = (batch.len() as u64)
                                .saturating_mul(2)
                                .min(context.readahead_max);
                            let mut slots = context.readahead.borrow_mut();
                            slots.retain(|ahead| ahead.path != path);
                            if slots.len() >= HOST_READAHEAD_SLOTS {
                                slots.remove(0);
                            }
                            slots.push(HostReadAhead {
                                path: path.to_path_buf(),
                                start: byte_off,
                                bytes: batch,
                                next_fill,
                            });
                        } else if batch.len() > valid {
                            // RANGE INVARIANT. The window is LBA-keyed with no
                            // file identity, and it is consulted BEFORE the
                            // region dispatch in `read_sector_checked`. It is
                            // safe only because it is filled here and nowhere
                            // else, and here `context.lba` always names a data
                            // sector of a projected file. Asserting it at the
                            // fill site is what makes the claim mechanical
                            // rather than a comment: `fat_entry_walked` reads
                            // FAT-region sectors without consulting the window,
                            // and this is why it may.
                            debug_assert!(
                                context.lba >= context.first_data_lba,
                                "the command window must never cover metadata: \
                                 lba {} is below the data region at {}",
                                context.lba,
                                context.first_data_lba
                            );
                            context.window.replace(Some(HostReadWindow {
                                start_lba: context.lba,
                                bytes: batch,
                            }));
                        }
                    }
                }
                Ok(read_bytes) => {
                    physical_bytes = read_bytes as u64;
                    eprintln!(
                        "katea: read {} @ {byte_off}: unexpected EOF",
                        path.display()
                    );
                    degraded = true;
                    cache.pop();
                    context.window.replace(None);
                    context.readahead.borrow_mut().retain(|a| a.path != path);
                }
                Err(e) => {
                    eprintln!("katea: read {} @ {byte_off}: {e}", path.display());
                    // `read_exact` leaves the buffer UNSPECIFIED when it fails,
                    // and it fails after filling part of it when the host file
                    // was truncated mid-sector. Zero the sector rather than hand
                    // the guest a half-sector of real bytes with a zero tail:
                    // the degraded contract is "no content", and the pre-batching
                    // path reset `out` here for exactly this reason.
                    out = [0u8; SECTOR];
                    degraded = true;
                    cache.pop();
                    context.window.replace(None);
                    context.readahead.borrow_mut().retain(|a| a.path != path);
                }
            }
        }
        None => degraded = true,
    }
    drop(cache);
    let elapsed = crate::duration_ns_u64(started.elapsed());
    bump(&context.counters.host_file_reads, 1);
    bump(&context.counters.host_file_opens, opened);
    bump(&context.counters.host_bytes, valid as u64);
    bump(&context.counters.host_read_operations, operations);
    bump(&context.counters.host_read_bytes, physical_bytes);
    bump(&context.counters.host_readahead_fills, filled);
    bump(&context.counters.host_wall_ns, elapsed);
    context
        .counters
        .host_read_max_ns
        .set(context.counters.host_read_max_ns.get().max(elapsed));
    SectorRead {
        bytes: out,
        degraded,
    }
}

#[cfg(test)]
#[path = "katea_tree_test.rs"]
mod tests;
