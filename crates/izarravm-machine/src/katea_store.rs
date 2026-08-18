// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The bounded guest-write store behind `katea_tree`'s host-folder disk.
//!
//! Guest writes initially land outside the mounted folder's static run table.
//! The projection layer acknowledges payload sectors after writing them to host
//! files, but FAT, directory, incomplete, and ambiguous writes remain here.
//! Keeping all pending payload in RAM would make memory grow with an unmapped
//! write burst, which is what this module exists to stop.
//!
//! The store keeps a bounded RAM cache of recently written sectors and spills the
//! rest to a scratch file, so RAM tracks the cache size rather than the write
//! volume. What stays in RAM per written region is pending, historical-touch,
//! and placement metadata only.
//!
//! The contract is byte-for-byte the one a plain `HashMap<u32, [u8; 512]>` would
//! give: [`SectorStore::get`] returns exactly what [`SectorStore::insert`] last
//! stored, and [`SectorStore::was_written`] answers "did the guest ever write
//! this sector this session" for every sector ever inserted, whether or not its
//! payload is still in RAM. The reconcile pass in `katea_tree` depends on both.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::katea_volume::SECTOR;

/// Sectors held in RAM. Chosen to land exactly on a hashbrown bucket boundary:
/// the table sizes itself to a power-of-two bucket count and resizes once a
/// table is 7/8 full, so 28672 live entries fit 32768 buckets with no room to
/// spare and the table can never double. One sector past this (as capacity
/// 32768 would be) forces 65536 buckets, which costs 33 MiB of table for 16 MiB
/// of payload and never shrinks back. 14 MiB of payload in a 16.5 MiB table.
pub(crate) const CACHE_SECTORS: usize = 28672;

/// Sectors covered by one [`Chunk`], and therefore by one spill slab: 128 KiB,
/// which is also the largest ATA multi-sector transfer, so one guest write
/// command touches at most two chunks.
const CHUNK_SECTORS: u32 = 256;

/// Bytes of spill file one chunk's slab occupies.
const CHUNK_BYTES: u64 = CHUNK_SECTORS as u64 * SECTOR as u64;

/// `Chunk::slab` before the chunk's spill space has been allocated.
const UNALLOCATED: u64 = u64::MAX;

/// The chunk covering `lba`, and the sector's index inside it.
fn chunk_id(lba: u32) -> u32 {
    lba / CHUNK_SECTORS
}

fn chunk_offset(lba: u32) -> u32 {
    lba % CHUNK_SECTORS
}

/// One 128 KiB span of the disk that the guest has written into: which of its
/// sectors exist, where its payload lives in the spill file, and when it was last
/// written.
///
/// This is the whole reason RAM stays bounded. 48 bytes describe 128 KiB of guest
/// writes, so the metadata for a write-heavy session is roughly a thousandth of
/// the payload, and it is allocated per *touched* span rather than per disk
/// sector, so an untouched volume costs nothing.
#[derive(Debug)]
struct Chunk {
    /// Byte offset of this chunk's slab in the spill file, or [`UNALLOCATED`]
    /// until the first sector of the chunk is evicted.
    slab: u64,
    /// One bit per sector whose latest guest bytes still live in this store.
    present: [u8; (CHUNK_SECTORS / 8) as usize],
    /// One bit per sector ever written this session. Reconcile uses this history
    /// after a projected payload has been acknowledged and removed.
    written: [u8; (CHUNK_SECTORS / 8) as usize],
    /// The store's write counter as of the last write anywhere in this chunk.
    /// Reconcile uses it to skip re-reading files that cannot have changed.
    last_seq: u64,
}

impl Chunk {
    fn new() -> Self {
        Self {
            slab: UNALLOCATED,
            present: [0; (CHUNK_SECTORS / 8) as usize],
            written: [0; (CHUNK_SECTORS / 8) as usize],
            last_seq: 0,
        }
    }

    fn is_present(&self, lba: u32) -> bool {
        let i = chunk_offset(lba);
        self.present[(i / 8) as usize] & (1 << (i % 8)) != 0
    }

    fn set_present(&mut self, lba: u32) {
        let i = chunk_offset(lba);
        self.present[(i / 8) as usize] |= 1 << (i % 8);
        self.written[(i / 8) as usize] |= 1 << (i % 8);
    }

    fn clear_present(&mut self, lba: u32) {
        let i = chunk_offset(lba);
        self.present[(i / 8) as usize] &= !(1 << (i % 8));
    }

    fn was_written(&self, lba: u32) -> bool {
        let i = chunk_offset(lba);
        self.written[(i / 8) as usize] & (1 << (i % 8)) != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SectorStoreCounters {
    pub resident_sectors: u64,
    pub pending_sectors: u64,
    pub spill_operations: u64,
    pub spill_bytes: u64,
    pub spill_wall_ns: u64,
}

/// Read exactly `buf.len()` bytes at `off`. Positioned reads take `&File`, which
/// is what lets the read path stay `&self`.
#[cfg(windows)]
fn read_exact_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0usize;
    while done < buf.len() {
        match file.seek_read(&mut buf[done..], off + done as u64) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "katea spill: short read",
                ));
            }
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, buf: &[u8], off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0usize;
    while done < buf.len() {
        match file.seek_write(&buf[done..], off + done as u64) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "katea spill: short write",
                ));
            }
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, off)
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, off)
}

/// A unique spill name per store in this process.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Guest-written sectors: a bounded RAM cache over a spill file, with exact
/// presence metadata.
///
/// Invariant, on which everything else rests:
///
/// > if a sector's presence bit is set, then either it is in `cache`, or its
/// > slab in the spill file holds its current bytes.
///
/// Eviction is the only thing that can break it, so eviction removes a sector
/// from the cache only after its spill write has succeeded. Every failure path
/// keeps the sector in RAM instead (see `broken`), trading the bound for
/// correctness rather than the other way round.
#[derive(Debug)]
pub(crate) struct SectorStore {
    /// Resident payload, keyed by LBA, valued by (write sequence, bytes).
    cache: HashMap<u32, (u64, [u8; SECTOR])>,
    /// Write sequence to LBA, so the front is the least recently written sector.
    /// Reads never promote, so this is FIFO by write, and a rewrite moves its
    /// sector to the back.
    order: BTreeMap<u64, u32>,
    /// Presence and placement per touched 128 KiB of disk.
    chunks: HashMap<u32, Chunk>,
    /// Created on the first eviction, so a session that fits in RAM never makes
    /// a file.
    spill: Option<File>,
    /// Where the spill lives, for `Drop`: `File` does not remember its path.
    path: PathBuf,
    /// Spill length, always a whole number of slabs. The file only ever grows at
    /// its end, so no filesystem is asked to zero-fill a hole bigger than a slab.
    spill_len: u64,
    /// Monotone write counter. Orders eviction and dates each chunk.
    seq: u64,
    /// Resident sector ceiling.
    capacity: usize,
    /// Set after any spill failure. Eviction stops for the session and the cache
    /// is allowed past `capacity`: unbounded RAM is bad, but losing a sector the
    /// guest can still read is worse.
    broken: bool,
    /// Spill reads that failed. Reconcile compares snapshots of this around each
    /// decision so a failed read holds that chain instead of feeding zeros into a
    /// delete or an overwrite. `Atomic` only because reads take `&self`.
    read_errors: AtomicU64,
    spill_operations: u64,
    spill_bytes: u64,
    spill_wall_ns: u64,
    /// Test-only fault injection: make every spill read fail. Breaking the file on
    /// disk is not a usable fault model, because the next eviction extends the file
    /// again and the lost range comes back as a hole full of zeroes rather than as
    /// an error, which is the one thing this store must never do.
    #[cfg(test)]
    fail_reads: bool,
}

impl SectorStore {
    pub(crate) fn new() -> Self {
        Self::with_capacity(CACHE_SECTORS)
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("izarravm-katea-spill-{}-{id}.tmp", std::process::id());
        Self {
            cache: HashMap::new(),
            order: BTreeMap::new(),
            chunks: HashMap::new(),
            spill: None,
            path: std::env::temp_dir().join(name),
            spill_len: 0,
            seq: 0,
            capacity: capacity.max(1),
            broken: false,
            #[cfg(test)]
            fail_reads: false,
            read_errors: AtomicU64::new(0),
            spill_operations: 0,
            spill_bytes: 0,
            spill_wall_ns: 0,
        }
    }

    /// Point the spill at `path` instead of the temp directory. Tests use this to
    /// drive the failure paths (an unusable path must never lose a sector).
    #[cfg(test)]
    pub(crate) fn set_spill_path(&mut self, path: &std::path::Path) {
        self.path = path.to_path_buf();
    }

    /// The bytes last written to `lba`, or `None` if the guest never wrote it (so
    /// the caller should serve the computed base view).
    ///
    /// `Err` means the payload exists but could not be read back. The caller must
    /// not treat that as "never written": zeros are not a safe substitute for a
    /// sector the guest wrote.
    pub(crate) fn get(&self, lba: u32) -> io::Result<Option<[u8; SECTOR]>> {
        if let Some((_, data)) = self.cache.get(&lba) {
            return Ok(Some(*data));
        }
        let Some(chunk) = self.chunks.get(&chunk_id(lba)) else {
            return Ok(None);
        };
        if !chunk.is_present(lba) {
            return Ok(None);
        }
        // The sector is present but not resident, so it is on disk from here on.
        #[cfg(test)]
        if self.fail_reads {
            self.read_errors.fetch_add(1, Ordering::Relaxed);
            return Err(io::Error::other("katea spill: injected read failure"));
        }
        // Present but not resident means it was evicted, so the slab and the file
        // must exist. If they do not, the invariant is broken and the honest
        // answer is an error, never the base view.
        let spilled = match (self.spill.as_ref(), chunk.slab) {
            (Some(file), slab) if slab != UNALLOCATED => {
                let mut out = [0u8; SECTOR];
                read_exact_at(
                    file,
                    &mut out,
                    slab + u64::from(chunk_offset(lba)) * SECTOR as u64,
                )
                .map(|()| out)
            }
            _ => Err(io::Error::other(format!(
                "katea spill: sector {lba} is marked written but has no payload"
            ))),
        };
        match spilled {
            Ok(out) => Ok(Some(out)),
            Err(e) => {
                self.read_errors.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// Store one guest-written sector. Reads of `lba` return `data` until the
    /// volume is dropped.
    pub(crate) fn insert(&mut self, lba: u32, data: &[u8; SECTOR]) {
        self.seq += 1;
        let seq = self.seq;
        {
            let chunk = self.chunks.entry(chunk_id(lba)).or_insert_with(Chunk::new);
            chunk.set_present(lba);
            chunk.last_seq = seq;
        }
        // Evict before inserting a new key, never after. `HashMap::insert` grows
        // the table inside the call, so inserting first would hand hashbrown one
        // entry too many and double the bucket count permanently, defeating the
        // whole point of CACHE_SECTORS. An overwrite of a resident sector needs no
        // room, so it skips this.
        if !self.cache.contains_key(&lba) {
            while self.cache.len() >= self.capacity && !self.broken && self.evict_one() {}
        }
        if let Some((old_seq, _)) = self.cache.insert(lba, (seq, *data)) {
            // Drop the sector's previous position, or `order` grows without bound
            // on rewrites and eviction would rank a hot sector by its first write.
            self.order.remove(&old_seq);
        }
        self.order.insert(seq, lba);
    }

    /// Did the guest ever write `lba` this session? True regardless of whether the
    /// payload is still resident, which is what makes eviction invisible to
    /// reconcile's "was this chain touched" tests.
    pub(crate) fn was_written(&self, lba: u32) -> bool {
        self.chunks
            .get(&chunk_id(lba))
            .is_some_and(|c| c.was_written(lba))
    }

    pub(crate) fn is_pending(&self, lba: u32) -> bool {
        self.chunks
            .get(&chunk_id(lba))
            .is_some_and(|c| c.is_present(lba))
    }

    pub(crate) fn acknowledge(&mut self, lba: u32) {
        let old_seq = self.cache.remove(&lba).map(|(seq, _)| seq);
        if let Some(seq) = old_seq {
            self.order.remove(&seq);
        }
        if let Some(chunk) = self.chunks.get_mut(&chunk_id(lba)) {
            chunk.clear_present(lba);
        }
    }

    pub(crate) fn counters(&self) -> SectorStoreCounters {
        let pending_sectors = self
            .chunks
            .values()
            .map(|chunk| {
                chunk
                    .present
                    .iter()
                    .map(|byte| u64::from(byte.count_ones()))
                    .sum::<u64>()
            })
            .sum();
        SectorStoreCounters {
            resident_sectors: self.cache.len() as u64,
            pending_sectors,
            spill_operations: self.spill_operations,
            spill_bytes: self.spill_bytes,
            spill_wall_ns: self.spill_wall_ns,
        }
    }

    /// The write counter as of the last write to any sector in `[first, first +
    /// count)`, or 0 if none was ever written.
    ///
    /// Chunk-granular, so it over-approximates: a write to a neighbour in the same
    /// 128 KiB can make an untouched range look newer than it is. That direction
    /// is safe (it costs an unnecessary re-read); the opposite would not be.
    pub(crate) fn max_seq_in(&self, first: u32, count: u32) -> u64 {
        if count == 0 {
            return 0; // an empty span was never written
        }
        let last = first.saturating_add(count - 1);
        (chunk_id(first)..=chunk_id(last))
            .filter_map(|id| self.chunks.get(&id))
            .map(|c| c.last_seq)
            .max()
            .unwrap_or(0)
    }

    /// The current write counter, for dating a decision made now.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// Spill reads that have failed so far. Reconcile snapshots this around each
    /// decision unit.
    pub(crate) fn read_errors(&self) -> u64 {
        self.read_errors.load(Ordering::Relaxed)
    }

    /// Live bytes held, for the memory-bound tests. An estimate of the payload and
    /// metadata actually stored, not of table slack.
    #[cfg(test)]
    pub(crate) fn ram_bytes(&self) -> usize {
        self.cache.len() * (SECTOR + 16) + self.order.len() * 24 + self.chunks.len() * 88
    }

    /// Resident sector count, for the bound tests.
    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Where the spill lives. Tests break it to drive the read-failure path.
    #[cfg(test)]
    pub(crate) fn spill_path(&self) -> &std::path::Path {
        &self.path
    }

    /// Make every read of a spilled sector fail from now on. Resident sectors are
    /// unaffected, which is what lets a test hold one chain's payload in RAM while
    /// another chain's becomes unreadable.
    #[cfg(test)]
    pub(crate) fn fail_spill_reads(&mut self) {
        self.fail_reads = true;
    }

    /// End test fault injection so a reconcile retry can read the spill again.
    #[cfg(test)]
    pub(crate) fn restore_spill_reads(&mut self) {
        self.fail_reads = false;
    }

    /// Write the least recently written sector out to the spill and drop it from
    /// RAM. Returns false when nothing was evicted, which also ends the caller's
    /// loop.
    fn evict_one(&mut self) -> bool {
        let Some((&victim_seq, &victim_lba)) = self.order.iter().next() else {
            return false;
        };
        let Some(&(_, data)) = self.cache.get(&victim_lba) else {
            // `order` and `cache` disagreeing means a bug in this module, not an
            // I/O problem. Drop the stale ordering entry and carry on, but say so:
            // silently absorbing it would let the RAM bound fail with no trace.
            eprintln!("katea: eviction order held sector {victim_lba}, which is not cached");
            self.order.remove(&victim_seq);
            return true;
        };
        match self.spill_sector(victim_lba, &data) {
            Ok(()) => {
                self.order.remove(&victim_seq);
                self.cache.remove(&victim_lba);
                true
            }
            Err(e) => {
                eprintln!(
                    "katea: spilling to {} failed: {e}; keeping guest writes in memory for this session",
                    self.path.display()
                );
                self.broken = true;
                false
            }
        }
    }

    /// Place one sector in the spill file, creating the file and the chunk's slab
    /// on demand. The sector stays in the cache unless this returns `Ok`.
    fn spill_sector(&mut self, lba: u32, data: &[u8; SECTOR]) -> io::Result<()> {
        let started = std::time::Instant::now();
        if self.spill.is_none() {
            // Truncate rather than create_new: the name carries our pid, so an
            // existing file is an orphan from a dead process, never a live one.
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(true);
            #[cfg(all(windows, not(test)))]
            {
                use std::os::windows::fs::OpenOptionsExt;
                const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
                options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
            }
            let file = options.open(&self.path)?;
            #[cfg(all(unix, not(test)))]
            let _ = std::fs::remove_file(&self.path);
            self.spill = Some(file);
            self.spill_len = 0;
        }
        let id = chunk_id(lba);
        let slab = match self.chunks.get(&id).map(|c| c.slab) {
            Some(slab) if slab != UNALLOCATED => slab,
            _ => {
                // Append a slab at the end of the file. Extending only at the end
                // keeps any filesystem-side zero-fill down to one slab, unlike an
                // LBA-addressed sparse image, which NTFS would zero up to the write
                // offset. Recorded only once the extension succeeds.
                let base = self.spill_len;
                let end = base + CHUNK_BYTES;
                let Some(file) = self.spill.as_ref() else {
                    return Err(io::Error::other("katea spill: file vanished"));
                };
                file.set_len(end)?;
                self.spill_len = end;
                match self.chunks.get_mut(&id) {
                    Some(chunk) => chunk.slab = base,
                    None => return Err(io::Error::other("katea spill: chunk vanished")),
                }
                base
            }
        };
        let Some(file) = self.spill.as_ref() else {
            return Err(io::Error::other("katea spill: file vanished"));
        };
        let result = write_all_at(
            file,
            data,
            slab + u64::from(chunk_offset(lba)) * SECTOR as u64,
        );
        self.spill_operations = self.spill_operations.saturating_add(1);
        self.spill_bytes = self.spill_bytes.saturating_add(SECTOR as u64);
        self.spill_wall_ns = self
            .spill_wall_ns
            .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        result
    }
}

impl Drop for SectorStore {
    fn drop(&mut self) {
        if self.spill.is_none() {
            return;
        }
        // Close the handle first. `Drop::drop` runs before the fields are dropped,
        // and Windows refuses to delete a path that still has an open handle, so
        // removing before this take() would leave the scratch file behind on every
        // clean exit.
        drop(self.spill.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
#[path = "katea_store_test.rs"]
mod tests;
