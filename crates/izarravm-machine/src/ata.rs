// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Image-backed ATA hard disk on the primary IDE channel.
//!
//! The disk is the primary master at command block 0x1F0-0x1F7, control block
//! 0x3F6, IRQ14, the conventional home for the boot drive (C:). The secondary
//! channel keeps the ATAPI CD-ROM (`ide.rs`). Only the master is populated;
//! selecting the slave reads back not-present and aborts commands.
//!
//! The image is a flat sector array. The geometry is derived from its length:
//! a fixed 16 heads and 63 sectors per track (the BIOS-translation default every
//! early-90s drive used), with the cylinder count filling out the rest. The same
//! image is addressed three ways and all map to one linear offset: CHS for INT
//! 13h legacy calls, LBA28 for the ATA task file, and LBA48-style packets for
//! EDD, though only the low 28 bits are honored here.
//!
//! PIO commands keep their immediate data-port path. Bus-master DMA commands arm
//! a request which the PIIX4-compatible BMIDE engine completes. PIO command and
//! sector boundaries are scheduled on the machine master clock. Completion
//! raises IRQ14 unless nIEN masks it.
//!
//! Limit: one master, no slave. The channel models a single drive 0; a slave
//! select reads not-present. Lift by holding two `AtaDisk`s per channel and
//! routing on the drive bit.
//! Limit: LBA28 only, no LBA48. The capacity caps at 2^28-1 sectors (128 GB),
//! plenty for the era. Lift by decoding the READ/WRITE SECTORS EXT (0x24/0x34)
//! commands and the high-order LBA bytes.

use izarravm_core::MASTER_CLOCK_HZ;
use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};

/// Primary-channel command-block base (0x1F0-0x1F7).
pub const PRIMARY_CMD_BASE: u16 = 0x1F0;
/// Primary-channel control/alt-status port.
pub const PRIMARY_CTRL: u16 = 0x3F6;
/// The IRQ the primary channel raises on command completion.
pub const PRIMARY_IRQ: u8 = 14;

/// One PIO sector is 512 bytes.
pub const SECTOR: usize = 512;

/// Fixed translated geometry. 16 heads and 63 sectors per track is the standard
/// BIOS translation every IDE drive of the era reported, so the cylinder count is
/// the only image-dependent value.
const HEADS: u32 = 16;
const SECTORS_PER_TRACK: u32 = 63;
/// Fixed per-command overhead before the first byte moves.
///
/// ZERO by design. The Izarra3000's storage profile is "16.7 MB/s of data on
/// command": the machine's disk is host-backed and has no platter, no head and no
/// rotational position, so there is nothing for a seek-and-settle charge to
/// model. It was 100 us, and that number dominated: 98.7% of the guest's
/// fixed-disk reads in a Duke Nukem 3D load are SINGLE-SECTOR, where 100 us of
/// latency against 30.6 us of transfer made the effective rate 3.9 MB/s -- a
/// four-fold understatement of the spec the rest of the model implements. Kept as
/// a named constant, not deleted, because it is where a future drive model with
/// real geometry would put its seek time.
const COMMAND_LATENCY_TICKS: u64 = 0;
const PIO_BYTES_PER_SECOND: u64 = 16_700_000;
const DMA_COMMAND_LATENCY_TICKS: u64 = MASTER_CLOCK_HZ / 10_000;

/// Slice 9C-pre (`dev_docs/2026-09-05-device-timing-slice9-design.md` §6): the
/// primary-channel analogue of `ide::ATA_POLL_RUN`, reusing the same value --
/// this is a PORT, not an independent tuning. See `ide.rs`'s doc comment for
/// the full derivation; it is not repeated here because the rationale is a
/// property of the shared poll-skip shape, not of either channel.
pub(crate) const ATA_POLL_RUN: u32 = 16;

/// The primary-channel analogue of `ide::ATA_POLL_FLOOR_TICKS`, same value
/// (20 us of guest time) for the same reason: see `ide.rs`.
pub(crate) const ATA_POLL_FLOOR_TICKS: u64 = MASTER_CLOCK_HZ / 50_000;

fn pio_sector_ticks() -> u64 {
    (SECTOR as u128 * MASTER_CLOCK_HZ as u128).div_ceil(PIO_BYTES_PER_SECOND as u128) as u64
}

/// Time from command acceptance through the final sector boundary for a PIO
/// media transfer. The BIOS fixed-disk path uses the same disk and deadline as
/// the ATA task-file path, without charging for guest data-port instructions.
pub(crate) fn pio_transfer_ticks(sectors: u32) -> u64 {
    pio_transfer_ticks_cached(sectors, 0)
}

/// The same charge, less the sectors that came out of the host-side sector cache.
///
/// A cache hit charges NOTHING. The medium was never touched: no command was
/// issued, no bytes crossed the cable, and the bytes were already in host memory.
/// That is the same accounting SMARTDRV's INT 13h hook produced on real hardware,
/// where a hit returned without the drive ever seeing the request, and it is the
/// only charge that keeps the model's story straight -- charging a fraction of a
/// transfer for a transfer that did not happen would be a number with nothing
/// behind it. `hits` is clamped to `sectors` so a miscounted delta can only ever
/// under-credit.
pub(crate) fn pio_transfer_ticks_cached(sectors: u32, hits: u32) -> u64 {
    if sectors == 0 {
        return 0;
    }
    let charged = u64::from(sectors.saturating_sub(hits.min(sectors)));
    COMMAND_LATENCY_TICKS.saturating_add(pio_sector_ticks().saturating_mul(charged))
}

pub(crate) fn dma_transfer_ticks(request: AtaDmaRequest) -> u64 {
    let data_ticks = (request.byte_len() as u128 * MASTER_CLOCK_HZ as u128)
        .div_ceil(request.bytes_per_second as u128);
    DMA_COMMAND_LATENCY_TICKS.saturating_add(data_ticks.min(u64::MAX as u128) as u64)
}

/// ATA status register bits.
mod status {
    pub const ERR: u8 = 0x01; // error: consult the error register
    pub const DRQ: u8 = 0x08; // data request: a PIO word is ready on the data port
    pub const DSC: u8 = 0x10; // device seek complete
    pub const DRDY: u8 = 0x40; // device ready
    pub const BSY: u8 = 0x80; // command in progress
}

/// ATA error register bits used by the abort path.
mod error {
    pub const ABRT: u8 = 0x04; // command aborted
}

/// Where the disk's sectors come from. A flat image holds the whole disk in RAM
/// (the today path: a mounted .img); a host-folder facade serves sectors lazily
/// from a `KateaTreeVolume` over a host directory tree, so a huge/deep folder
/// never lands in memory. Writes to the facade route into the volume's in-memory
/// overlay and are reconciled to host files on command completion.
#[derive(Debug)]
enum Backing {
    /// A flat sector array, addressed by `lba * SECTOR`.
    Image(Vec<u8>),
    /// A lazy FAT32 view over a recursive host-folder tree. Boxed because the
    /// volume is large relative to the `Vec` it sits beside in the enum.
    HostFolder(Box<crate::katea_tree::KateaTreeVolume>),
}

/// What the data port is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Idle: no PIO buffer in flight.
    Idle,
    /// Draining a read buffer to the host (device-to-host).
    DataIn,
    /// Filling one write sector from the host (host-to-device).
    DataOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingAction {
    PrepareIdentify,
    PrepareRead,
    PrepareWrite,
    CommitWrite,
    CompleteOk,
    Abort,
    Initialize { sectors: u8, heads: u8 },
    SetFeatures { feature: u8, mode: u8 },
    Diagnostic,
    CheckPower,
    FlushCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingCommand {
    ticks_remaining: u64,
    action: PendingAction,
}

/// Transfer direction from the ATA device's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtaDmaDirection {
    DeviceToMemory,
    MemoryToDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmaMode {
    None,
    Multiword(u8),
    Ultra(u8),
}

impl DmaMode {
    fn bytes_per_second(self) -> Option<u64> {
        match self {
            Self::None => None,
            Self::Multiword(0) => Some(4_200_000),
            Self::Multiword(1) => Some(13_300_000),
            Self::Multiword(2) | Self::Ultra(0) => Some(16_700_000),
            Self::Ultra(1) => Some(25_000_000),
            Self::Ultra(2) => Some(33_300_000),
            Self::Multiword(_) | Self::Ultra(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AtaDmaRequest {
    pub(crate) direction: AtaDmaDirection,
    pub(crate) lba: u32,
    pub(crate) sectors: u32,
    pub(crate) bytes_per_second: u64,
}

impl AtaDmaRequest {
    pub(crate) fn byte_len(self) -> usize {
        self.sectors as usize * SECTOR
    }
}

/// An ATA hard disk and its task-file register set. The sectors come from either
/// a flat image or a lazy host-folder facade (see `Backing`).
#[derive(Debug)]
pub struct AtaDisk {
    backing: Backing,
    cylinders: u32,
    /// True after any guest write, so the host flushes the image back to disk.
    pub dirty: bool,

    // ATA task-file registers.
    features: u8,
    sector_count: u8,
    lba_low: u8,    // LBA bits 0-7, or sector number in CHS
    lba_mid: u8,    // LBA bits 8-15, or cylinder low in CHS
    lba_high: u8,   // LBA bits 16-23, or cylinder high in CHS
    drive_head: u8, // bit 6 = LBA select, bit 4 = drive (0 master), bits 0-3 = LBA 24-27 / head
    status: u8,
    error: u8,
    /// INITIALIZE DEVICE PARAMETERS programs the logical sectors-per-track and
    /// heads the host wants to use for CHS translation. Defaults to the derived
    /// geometry. These values apply only to task-file CHS commands; INT 13h uses
    /// the public derived geometry so the BIOS and shared medium stay stable.
    logical_sectors: u8,
    logical_heads: u8,

    /// nIEN (control register bit 1): interrupts disabled while set.
    interrupts_disabled: bool,
    /// PIO transfer phase and the buffer it drains or fills.
    phase: Phase,
    buffer: Vec<u8>,
    buffer_pos: usize,
    /// Current PIO sector and sectors remaining in the command.
    pio_lba: u32,
    pio_sectors_remaining: u32,
    /// Command or sector boundary waiting on the machine master clock.
    pending_command: Option<PendingCommand>,
    /// Set when a command completes or a PIO sector becomes ready.
    irq_pending: bool,
    /// DMA command waiting for the PIIX4 bus-master engine.
    dma_request: Option<AtaDmaRequest>,
    /// Transfer mode selected through SET FEATURES. Izarra3000 powers on in
    /// UDMA2, matching its 1997 storage profile.
    dma_mode: DmaMode,
    /// Bytes moved by the last data command, for the GUI access LED.
    last_access_bytes: usize,
    /// Host-side sector cache under every addressing form. `RefCell` because a
    /// lookup mutates LRU order on a `&self` read path. Host-side state, and
    /// deliberately outside canonical capture — see `sector_cache`.
    cache: std::cell::RefCell<crate::sector_cache::SectorCache>,

    // ------------------------------------------------------------------
    // Slice 9C-pre: device-armed poll skip, device half. HOST BOOKKEEPING,
    // never canonical. The shape is `ide.rs`'s `note_alt_status_read` /
    // `poll_skip_blocked` mechanism ported to the primary channel; see that
    // file's "Device-armed poll skip, device half" comment for the reasoning
    // this only summarizes. The bus half (the timing-class gate, the
    // `DeviceTimingProfile::ata` family check, and the batch-end actuation)
    // lives in `bus.rs` / `run.rs`, gated OFF by default so this whole
    // mechanism is dark while `IZARRAVM_DEVICE_TIMING` leaves `ata` unarmed.
    // ------------------------------------------------------------------
    /// Consecutive alt-status reads seen inside the current CPU batch.
    alt_status_run: u32,
    /// Suppresses further arming until the next `schedule` or a committed
    /// skip. Set by a batch-end decline whose DEVICE-bounded target fell
    /// under the floor, and by `write_port` / `soft_reset`.
    poll_skip_blocked: bool,
    /// The arming threshold and the minimum skip, in master ticks. Read here
    /// rather than from the environment per access.
    poll_run_threshold: u32,
    poll_floor_ticks: u64,
}

/// Whether new disks get a live sector cache. Read once per disk from
/// `IZARRAVM_HDD_CACHE`; the default is ON because a host-backed disk really is
/// instant and the cache is what makes the charged model say so on a re-read.
/// `=0` is the A/B control leg.
fn sector_cache_enabled() -> bool {
    std::env::var("IZARRAVM_HDD_CACHE").as_deref() != Ok("0")
}

impl AtaDisk {
    /// Mount a flat sector image, padding up to a whole sector if needed. The
    /// geometry is derived from the padded length.
    pub fn new(mut image: Vec<u8>) -> Self {
        if !image.len().is_multiple_of(SECTOR) {
            let pad = SECTOR - (image.len() % SECTOR);
            image.resize(image.len() + pad, 0);
        }
        let total_sectors = (image.len() / SECTOR) as u32;
        Self::with_backing(Backing::Image(image), total_sectors)
    }

    /// Mount a lazy host-folder tree facade as the disk. The geometry is derived
    /// from the volume's whole-disk sector count, the same way `new` derives it
    /// from an image length, so the BIOS sees the same CHS translation either way.
    pub fn from_host_folder(volume: crate::katea_tree::KateaTreeVolume) -> Self {
        let total_sectors = volume.total_sectors();
        Self::with_backing(Backing::HostFolder(Box::new(volume)), total_sectors)
    }

    /// Shared constructor: derive the cylinder count from the sector count and
    /// initialize the task-file registers to their reset state.
    fn with_backing(backing: Backing, total_sectors: u32) -> Self {
        // Cylinders fill out whatever the head/track product leaves. At least one
        // so an empty image still presents a one-cylinder disk rather than zero.
        let per_cyl = HEADS * SECTORS_PER_TRACK;
        let cylinders = (total_sectors / per_cyl).max(1);
        Self {
            backing,
            cylinders,
            dirty: false,
            features: 0,
            sector_count: 1,
            lba_low: 1,
            lba_mid: 0,
            lba_high: 0,
            drive_head: 0,
            status: status::DRDY | status::DSC,
            error: 0,
            logical_sectors: SECTORS_PER_TRACK as u8,
            logical_heads: HEADS as u8,
            interrupts_disabled: false,
            phase: Phase::Idle,
            buffer: Vec::new(),
            buffer_pos: 0,
            pio_lba: 0,
            pio_sectors_remaining: 0,
            pending_command: None,
            irq_pending: false,
            dma_request: None,
            dma_mode: DmaMode::Ultra(2),
            last_access_bytes: 0,
            cache: std::cell::RefCell::new(crate::sector_cache::SectorCache::new(
                sector_cache_enabled(),
            )),
            alt_status_run: 0,
            poll_skip_blocked: false,
            poll_run_threshold: ATA_POLL_RUN,
            poll_floor_ticks: ATA_POLL_FLOOR_TICKS,
        }
    }

    /// Total addressable sectors (LBA28 capacity), capped at the 28-bit ceiling.
    pub fn total_sectors(&self) -> u32 {
        let sectors = match &self.backing {
            Backing::Image(image) => (image.len() / SECTOR) as u32,
            Backing::HostFolder(volume) => volume.total_sectors(),
        };
        sectors.min((1 << 28) - 1)
    }

    /// Cylinder count of the derived geometry.
    pub fn cylinders(&self) -> u32 {
        self.cylinders
    }

    /// Logical heads of the derived geometry (always 16 here).
    pub fn heads(&self) -> u32 {
        HEADS
    }

    /// Logical sectors per track of the derived geometry (always 63 here).
    pub fn sectors_per_track(&self) -> u32 {
        SECTORS_PER_TRACK
    }

    /// The backing image bytes, including any in-session writes, for flush-back.
    /// A host-folder facade has no flat image to flush; it returns an empty slice
    /// and never sets `dirty` (the flush caller is gated on `dirty`), so the empty
    /// slice is never written back.
    pub fn bytes(&self) -> &[u8] {
        match &self.backing {
            Backing::Image(image) => image,
            Backing::HostFolder(_) => &[],
        }
    }

    /// Whether this disk is a flat image (vs a lazy host-folder facade). Only an
    /// image exposes flat bytes; host-folder handles are flushed separately, so
    /// callers that persist `bytes()` must skip it (see `Machine::eject_hdd`).
    pub fn is_image(&self) -> bool {
        matches!(self.backing, Backing::Image(_))
    }

    /// What the Katea read path has done since mount, or None for an
    /// image-backed disk, which never touches the host filesystem to serve one.
    pub fn katea_storage_counters(&self) -> Option<crate::katea_tree::KateaStorageCounters> {
        match &self.backing {
            Backing::Image(_) => None,
            Backing::HostFolder(volume) => Some(volume.storage_counters()),
        }
    }

    /// The synthesized FAT32 geometry, or None for an image-backed disk.
    pub fn katea_geometry_report(&self) -> Option<crate::katea_tree::KateaGeometryReport> {
        match &self.backing {
            Backing::Image(_) => None,
            Backing::HostFolder(volume) => Some(volume.geometry_report()),
        }
    }

    /// Answer a B3 FAT-position hypercall, or None for an image-backed disk —
    /// the hypercall serves only the Katea host-folder volume.
    pub fn map_fat_chain(
        &self,
        start: u32,
        steps: u32,
    ) -> Option<crate::katea_tree::ChainMapOutcome> {
        match &self.backing {
            Backing::Image(_) => None,
            Backing::HostFolder(volume) => Some(volume.map_chain(start, steps)),
        }
    }

    /// Run the host-folder reconcile pass. A no-op for an image-backed disk.
    /// The machine calls this at eject/flush so anything held in the overlay is a
    /// final-pass materialized to the host folder.
    pub fn reconcile_host_folder(&mut self) {
        if let Backing::HostFolder(volume) = &mut self.backing {
            volume.reconcile();
        }
    }

    pub(crate) fn commit_guest_write_batch(
        &mut self,
        route: crate::katea_tree::GuestWriteRoute,
    ) -> crate::katea_tree::CommitGuestWriteResult {
        match &mut self.backing {
            Backing::Image(_) => crate::katea_tree::CommitGuestWriteResult::Projected,
            Backing::HostFolder(volume) => volume.commit_guest_write_batch(route),
        }
    }

    pub(crate) fn note_guest_read_batch(
        &self,
        route: crate::katea_tree::GuestStorageRoute,
        sectors: u64,
        wait_ticks: u64,
    ) {
        if let Backing::HostFolder(volume) = &self.backing {
            volume.note_guest_read_batch(route, sectors, wait_ticks);
        }
    }

    pub(crate) fn note_guest_write_wait(
        &self,
        route: crate::katea_tree::GuestStorageRoute,
        wait_ticks: u64,
    ) {
        if let Backing::HostFolder(volume) = &self.backing {
            volume.note_guest_write_wait(route, wait_ticks);
        }
    }

    fn flush_guest_writes(&mut self) -> crate::katea_tree::CommitGuestWriteResult {
        match &mut self.backing {
            Backing::Image(_) => crate::katea_tree::CommitGuestWriteResult::Projected,
            Backing::HostFolder(volume) => volume.flush_guest_writes(),
        }
    }

    /// Read one whole 512-byte sector at `lba`, or None if past the end. The facade
    /// synthesizes sectors on demand, so this returns an owned array rather than a
    /// borrow into a backing buffer.
    pub fn read_lba(&self, lba: u32) -> Option<[u8; SECTOR]> {
        // The host-side sector cache sits HERE, under every addressing form, so
        // CHS, LBA28 and EDD share one residency set and one hit/miss counter.
        // A hit skips the backing entirely; the caller reads the counter delta to
        // learn what to charge (see `pio_transfer_ticks_cached`).
        //
        // CHARGE ASYMMETRY, deliberate: only the BIOS fixed-disk service reads
        // that delta. The ATA task-file and BMIDE DMA paths take cache hits for
        // their DATA -- there is one residency set and it sits under all of them,
        // which is what keeps the bytes a guest sees independent of how it asked
        // -- but they still price their transfers with the UNCACHED formula,
        // because they schedule their own deadlines at command time, before any
        // sector has been looked up. That matches the thing being modelled:
        // SMARTDRV hooked INT 13h and nothing below it, so a driver talking to
        // the controller directly saw the drive's real cost. Widening the credit
        // to those paths means moving their deadline scheduling after the
        // transfer, which is a different change with its own timing risk.
        if let Some(hit) = self.cache.borrow_mut().get(lba) {
            return Some(hit);
        }
        let served = self.read_lba_uncached(lba)?;
        // A DEGRADED read is served but never remembered. Its bytes are the zero
        // fallback for a host-side failure, not content, and caching them would
        // turn a transient failure permanent: every later read of this LBA would
        // hit the cache and get zeros even after the host file came back. The
        // backing's own retry design assumes the next read reaches it again (a
        // failed read drops the cached host handle so the next sector re-opens).
        if !served.degraded {
            self.cache.borrow_mut().put(lba, &served.bytes);
        }
        Some(served.bytes)
    }

    /// Sector-cache hits and misses since mount. The fixed-disk service reads the
    /// hit counter before and after a transfer to price it; nothing else in the
    /// machine depends on these.
    pub fn sector_cache_hits(&self) -> u64 {
        self.cache.borrow().hits()
    }

    pub fn sector_cache_misses(&self) -> u64 {
        self.cache.borrow().misses()
    }

    pub(crate) fn begin_read_command(&self, start_lba: u32, sectors: u32) {
        if let Backing::HostFolder(volume) = &self.backing {
            volume.begin_read_command(start_lba, sectors);
        }
    }

    /// Region of `lba` on a Katea volume. Image-backed disks have no FAT layout
    /// the census can name, so they report `Other`.
    pub(crate) fn lba_region(&self, lba: u32) -> crate::katea_tree::LbaRegion {
        match &self.backing {
            Backing::HostFolder(volume) => volume.lba_region(lba),
            Backing::Image(_) => crate::katea_tree::LbaRegion::Other,
        }
    }

    pub(crate) fn end_read_command(&self) {
        if let Backing::HostFolder(volume) = &self.backing {
            volume.end_read_command();
        }
    }

    /// Read straight from the backing, bypassing the cache. Split out so the
    /// cache has exactly one filler and the backing exactly one reader.
    ///
    /// Carries the backing's degraded flag through unchanged: an image read is
    /// either in range or it is not, so it never degrades; the Katea volume
    /// reports a failed host read that came back as zeros.
    fn read_lba_uncached(&self, lba: u32) -> Option<crate::katea_tree::SectorRead> {
        match &self.backing {
            Backing::Image(image) => {
                let off = lba as usize * SECTOR;
                image.get(off..off + SECTOR).map(|s| {
                    let mut out = [0u8; SECTOR];
                    out.copy_from_slice(s);
                    crate::katea_tree::SectorRead {
                        bytes: out,
                        degraded: false,
                    }
                })
            }
            Backing::HostFolder(volume) => {
                (lba < volume.total_sectors()).then(|| volume.read_sector_checked(lba))
            }
        }
    }

    /// Overwrite one whole 512-byte sector at `lba`. Returns false if past the end
    /// or `data` is short. An image writes in place; a host-folder facade routes the
    /// write into its overlay and reconciles on command completion.
    pub fn write_lba(&mut self, lba: u32, data: &[u8]) -> bool {
        if data.len() < SECTOR {
            return false;
        }
        match &mut self.backing {
            Backing::Image(image) => {
                let off = lba as usize * SECTOR;
                if off + SECTOR > image.len() {
                    return false;
                }
                image[off..off + SECTOR].copy_from_slice(&data[..SECTOR]);
            }
            Backing::HostFolder(volume) => {
                if lba >= volume.total_sectors() {
                    return false;
                }
                let mut sector = [0u8; SECTOR];
                sector.copy_from_slice(&data[..SECTOR]);
                volume.write_sector(lba, &sector);
            }
        }
        // Write-through, which is also the invalidation: the guest's bytes are
        // the new truth for this sector, so storing them leaves nothing stale to
        // serve. Both backings return exactly these bytes on the next read --
        // the image writes them in place and the Katea overlay reads back out of
        // the same overlay the write just landed in.
        let mut stored = [0u8; SECTOR];
        stored.copy_from_slice(&data[..SECTOR]);
        self.cache.borrow_mut().put(lba, &stored);
        self.dirty = true;
        true
    }

    /// Translate a 1-based CHS address through the derived geometry to an LBA, or
    /// None if it is off the disk. INT 13h hands CHS; this is the bridge to the
    /// linear image.
    pub fn chs_to_lba(&self, cyl: u32, head: u32, sector: u32) -> Option<u32> {
        if sector == 0 || sector > SECTORS_PER_TRACK || head >= HEADS || cyl >= self.cylinders {
            return None;
        }
        Some((cyl * HEADS + head) * SECTORS_PER_TRACK + (sector - 1))
    }

    fn task_file_chs_to_lba(&self, cyl: u32, head: u32, sector: u32) -> Option<u32> {
        let heads = u32::from(self.logical_heads);
        let sectors = u32::from(self.logical_sectors);
        if heads == 0 || sectors == 0 || head >= heads || sector == 0 || sector > sectors {
            return None;
        }
        let lba = cyl
            .checked_mul(heads)?
            .checked_add(head)?
            .checked_mul(sectors)?
            .checked_add(sector - 1)?;
        (lba < self.total_sectors()).then_some(lba)
    }

    /// Take the pending IRQ (the machine forwards it to the PIC). nIEN suppresses
    /// the forward, matching a channel with interrupts masked.
    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending && !self.interrupts_disabled;
        self.irq_pending = false;
        pending
    }

    pub(crate) fn irq_enabled(&self) -> bool {
        !self.interrupts_disabled
    }

    /// Take and clear the access-byte count for the GUI LED.
    pub fn take_access_bytes(&mut self) -> usize {
        let bytes = self.last_access_bytes;
        self.last_access_bytes = 0;
        bytes
    }

    /// Whether a port belongs to the primary channel.
    pub fn owns_port(port: u16) -> bool {
        (PRIMARY_CMD_BASE..=PRIMARY_CMD_BASE + 7).contains(&port) || port == PRIMARY_CTRL
    }

    // ------------------------------------------------------------------
    // Slice 9C-pre: device-armed poll skip, device half. See the struct
    // field doc comments above and `ide.rs`'s "Device-armed poll skip,
    // device half" block for the mechanism this ports.
    // ------------------------------------------------------------------

    /// Count one alt-status read and answer "arm the skip on this read".
    ///
    /// Called ONLY from the bus's primary-channel arm, and only when
    /// `DeviceTimingProfile::ata` is armed on this machine -- unlike the
    /// ATAPI mechanism, this whole path is DARK by default (the design's
    /// slice-9 knob-unset identity bar), not merely gated by a separate
    /// enable flag.
    pub(crate) fn note_alt_status_read(&mut self) -> bool {
        // Nothing pending: a read outside any wait must not carry a run
        // across a completion boundary.
        if self.pending_command.is_none() {
            self.alt_status_run = 0;
            return false;
        }
        if self.poll_skip_blocked {
            self.alt_status_run = 0;
            return false;
        }
        self.alt_status_run = self.alt_status_run.saturating_add(1);
        if self.alt_status_run < self.poll_run_threshold {
            return false;
        }
        // The arm-time floor is evaluated against the channel's OWN
        // deadline, the only one the bus half can see cheaply, so a wait
        // that is intrinsically not worth a batch break never costs one.
        let armed = self
            .ticks_until_completion()
            .is_some_and(|ticks| ticks >= self.poll_floor_ticks);
        if armed {
            self.alt_status_run = 0;
        }
        armed
    }

    /// Clear the run counter at a CPU batch boundary, or on any read that is
    /// not an alt-status poll.
    pub(crate) fn reset_alt_status_run(&mut self) {
        self.alt_status_run = 0;
    }

    /// Bound the device-edge pathology: at most ONE wasted batch break per
    /// pending command. Cleared by `schedule` and by a committed skip.
    pub(crate) fn block_poll_skip(&mut self) {
        self.poll_skip_blocked = true;
    }

    pub(crate) fn clear_poll_skip_block(&mut self) {
        self.poll_skip_blocked = false;
    }

    /// Test seam: the latch state.
    #[cfg(test)]
    pub(crate) fn poll_skip_blocked(&self) -> bool {
        self.poll_skip_blocked
    }

    #[cfg(test)]
    pub(crate) fn alt_status_run_for_test(&self) -> u32 {
        self.alt_status_run
    }

    /// Test seam: schedule an arbitrary pending completion without going
    /// through a real ATA command, so a fixture can inject a latency
    /// `COMMAND_LATENCY_TICKS` does not carry today. Reuses the same
    /// `CompleteOk` boundary RECALIBRATE/SEEK already schedule through, so the
    /// completion behaviour (DRDY|DSC, IRQ) is the real one, not a stub.
    #[cfg(test)]
    pub(crate) fn schedule_test_pending(&mut self, ticks: u64) {
        self.schedule(PendingAction::CompleteOk, ticks);
    }

    /// Test seam: pin the arming threshold and the minimum skip so a fixture
    /// can isolate the batch-end floor check from the arm-time one, the same
    /// way `ide::IdeChannel::configure_poll_skip` does for the ATAPI channel.
    #[cfg(test)]
    pub(crate) fn configure_poll_skip_for_test(&mut self, run: u32, floor_ticks: u64) {
        self.poll_run_threshold = run.max(1);
        self.poll_floor_ticks = floor_ticks;
    }

    /// Whether the master device is selected (drive bit 4 == 0).
    fn master_selected(&self) -> bool {
        self.drive_head & 0x10 == 0
    }

    /// Whether LBA addressing is selected (drive/head bit 6).
    fn lba_mode(&self) -> bool {
        self.drive_head & 0x40 != 0
    }

    /// Read one byte from a channel port. The data register drains the read
    /// buffer; the rest return their task-file values.
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        if port == PRIMARY_CTRL {
            // Alt status: the status register without clearing the IRQ latch.
            return Some(self.status);
        }
        if !(PRIMARY_CMD_BASE..=PRIMARY_CMD_BASE + 7).contains(&port) {
            return None;
        }
        // Any OTHER read on the channel breaks the alt-status run, mirroring
        // `ide::IdeChannel::read_port`: the arm means "N alt-status reads
        // with no other I/O to the channel", so a status, error or data
        // register read is not part of a poll wait. The PRIMARY_CTRL arm
        // above returns before this; its counting is the bus half's
        // `note_alt_status_read` call.
        self.reset_alt_status_run();
        let reg = port - PRIMARY_CMD_BASE;
        let value = match reg {
            0 => self.read_data_byte(),
            1 => self.error,
            2 => self.sector_count,
            3 => self.lba_low,
            4 => self.lba_mid,
            5 => self.lba_high,
            6 => self.drive_head,
            7 => {
                // Reading the status register clears the pending interrupt latch
                // on hardware; the machine has already (or will) forward it.
                self.irq_pending = false;
                self.status
            }
            _ => 0xFF,
        };
        Some(value)
    }

    /// Write one byte to a channel port. Word writes to the data register split
    /// into two byte writes at the bus layer, so the PIO buffer is byte-fed.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        // A write is not a poll, and a control-port write can trigger SRST,
        // which clears `pending_command` mid-batch -- mirroring
        // `ide::IdeChannel::write_port`'s two guards. Ordering is
        // load-bearing: `schedule` clears the latch, and `write_command`
        // below reaches `schedule`, so writing a new command re-enables
        // skipping for that command.
        self.reset_alt_status_run();
        self.poll_skip_blocked = true;
        if port == PRIMARY_CTRL {
            // Device control: bit 1 = nIEN, bit 2 = SRST (soft reset).
            self.interrupts_disabled = value & 0x02 != 0;
            if value & 0x04 != 0 {
                self.soft_reset();
            }
            return true;
        }
        if !(PRIMARY_CMD_BASE..=PRIMARY_CMD_BASE + 7).contains(&port) {
            return false;
        }
        let reg = port - PRIMARY_CMD_BASE;
        if self.status & status::BSY != 0 && reg != 0 {
            return true;
        }
        match reg {
            0 => self.write_data_byte(value),
            1 => self.features = value,
            2 => self.sector_count = value,
            3 => self.lba_low = value,
            4 => self.lba_mid = value,
            5 => self.lba_high = value,
            6 => self.drive_head = value,
            7 => self.write_command(value),
            _ => {}
        }
        true
    }

    fn soft_reset(&mut self) {
        // Same disposition as `write_port`'s guard: nothing armed before a
        // reset may be honoured after it.
        self.reset_alt_status_run();
        self.poll_skip_blocked = true;
        self.phase = Phase::Idle;
        self.dma_request = None;
        self.pending_command = None;
        self.buffer.clear();
        self.buffer_pos = 0;
        self.pio_sectors_remaining = 0;
        self.status = status::DRDY | status::DSC;
        // Diagnostic code 0x01: device 0 passed (ATA 9.1). An ATA disk leaves the
        // signature registers at 0x00 sector-count/LBA-low and 0x0000 cylinder,
        // the way a non-packet device does, so the host can tell it from ATAPI.
        self.error = 0x01;
        self.sector_count = 0x01;
        self.lba_low = 0x01;
        self.lba_mid = 0x00;
        self.lba_high = 0x00;
        self.irq_pending = false;
    }

    /// Decode the command's starting LBA from the task file (LBA28 or CHS) and the
    /// sector count (0 means 256, the ATA convention).
    fn command_lba(&self) -> Option<(u32, u32)> {
        let count = if self.sector_count == 0 {
            256
        } else {
            u32::from(self.sector_count)
        };
        let lba = if self.lba_mode() {
            u32::from(self.lba_low)
                | (u32::from(self.lba_mid) << 8)
                | (u32::from(self.lba_high) << 16)
                | (u32::from(self.drive_head & 0x0F) << 24)
        } else {
            let cyl = u32::from(self.lba_mid) | (u32::from(self.lba_high) << 8);
            let head = u32::from(self.drive_head & 0x0F);
            let sector = u32::from(self.lba_low);
            self.task_file_chs_to_lba(cyl, head, sector)?
        };
        Some((lba, count))
    }

    fn write_command(&mut self, command: u8) {
        // The command register is not accepted while a command or PIO buffer is
        // in flight. SRST remains available through the control port.
        if self.dma_request.is_some() || self.pending_command.is_some() || self.phase != Phase::Idle
        {
            return;
        }
        if !self.master_selected() {
            // No slave device: any command to it aborts after command latency.
            self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS);
            return;
        }
        match command {
            0xEC => self.schedule(PendingAction::PrepareIdentify, COMMAND_LATENCY_TICKS),
            0x20 | 0x21 => self.read_sectors(),
            0x30 | 0x31 => self.write_sectors(),
            0xC8 | 0xC9 => self.begin_dma(AtaDmaDirection::DeviceToMemory),
            0xCA | 0xCB => self.begin_dma(AtaDmaDirection::MemoryToDevice),
            // READ/WRITE MULTIPLE behave like the single-sector PIO forms here:
            // the model has no per-block interrupt, so each sector still drains
            // through the data port. Limit: no multi-count block size, lift by
            // honoring the SET MULTIPLE MODE block and interrupting per block.
            0xC4 => self.read_sectors(),
            0xC5 => self.write_sectors(),
            // RECALIBRATE (0x10-0x1F): seek to cylinder 0, complete with DSC.
            0x10..=0x1F => self.schedule(PendingAction::CompleteOk, COMMAND_LATENCY_TICKS),
            // SEEK (0x70-0x7F): the HLE medium has no mechanical head state.
            0x70..=0x7F => self.schedule(PendingAction::CompleteOk, COMMAND_LATENCY_TICKS),
            // INITIALIZE DEVICE PARAMETERS (0x91): set the logical CHS the host
            // wants. sector_count = sectors per track, drive_head low nibble + 1
            // = heads. Accept and ack.
            0x91 => self.schedule(
                PendingAction::Initialize {
                    sectors: self.sector_count,
                    heads: (self.drive_head & 0x0F) + 1,
                },
                COMMAND_LATENCY_TICKS,
            ),
            // SET FEATURES (0xEF): transfer-mode selection is guest-visible in
            // IDENTIFY. Other established feature toggles remain acknowledged.
            0xEF => self.schedule(
                PendingAction::SetFeatures {
                    feature: self.features,
                    mode: self.sector_count,
                },
                COMMAND_LATENCY_TICKS,
            ),
            // EXECUTE DEVICE DIAGNOSTIC (0x90): report device 0 passed (0x01).
            0x90 => self.schedule(PendingAction::Diagnostic, COMMAND_LATENCY_TICKS),
            // CHECK POWER MODE reports active. IDLE and STANDBY have no mechanical
            // state; FLUSH CACHE projects metadata and flushes host-file handles.
            0xE5 => self.schedule(PendingAction::CheckPower, COMMAND_LATENCY_TICKS),
            0xE2 | 0xE3 => self.schedule(PendingAction::CompleteOk, COMMAND_LATENCY_TICKS),
            0xE7 => self.schedule(PendingAction::FlushCache, COMMAND_LATENCY_TICKS),
            // NOP (0x00) always aborts on hardware, never a silent success.
            _ => self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS),
        }
    }

    fn schedule(&mut self, action: PendingAction, ticks: u64) {
        // A NEW pending command gets a fresh chance at the skip: the latch
        // bounds ONE wasted batch break per command, mirroring
        // `ide::IdeChannel::schedule`.
        self.poll_skip_blocked = false;
        self.phase = Phase::Idle;
        self.status = status::BSY;
        self.error = 0;
        self.irq_pending = false;
        self.pending_command = Some(PendingCommand {
            ticks_remaining: ticks.max(1),
            action,
        });
    }

    /// Master ticks until the next guest-visible PIO command or sector boundary.
    pub(crate) fn ticks_until_completion(&self) -> Option<u64> {
        self.pending_command
            .as_ref()
            .map(|pending| pending.ticks_remaining)
    }

    pub(crate) fn ticks_until_irq(&self) -> Option<u64> {
        self.pending_command
            .as_ref()
            .filter(|pending| pending.action != PendingAction::PrepareWrite)
            .map(|pending| pending.ticks_remaining)
    }

    /// Advance a pending PIO operation on the fixed machine timeline.
    pub(crate) fn advance_master_ticks(&mut self, ticks: u64) {
        let Some(pending) = self.pending_command.as_mut() else {
            return;
        };
        if ticks < pending.ticks_remaining {
            pending.ticks_remaining -= ticks;
            return;
        }
        let action = self.pending_command.take().unwrap().action;
        self.finish_pending(action);
    }

    fn finish_pending(&mut self, action: PendingAction) {
        // A completion boundary ends the wait the run was counting. Arms
        // that follow belong to the NEXT command, not this one.
        self.reset_alt_status_run();
        match action {
            PendingAction::PrepareIdentify => self.prepare_identify(),
            PendingAction::PrepareRead => self.prepare_read_sector(),
            PendingAction::PrepareWrite => self.prepare_write_sector(false),
            PendingAction::CommitWrite => self.commit_write_sector(),
            PendingAction::CompleteOk => self.complete_ok(),
            PendingAction::Abort => self.abort(),
            PendingAction::Initialize { sectors, heads } => {
                self.logical_sectors = sectors;
                self.logical_heads = heads;
                self.complete_ok();
            }
            PendingAction::SetFeatures { feature, mode } => self.apply_set_features(feature, mode),
            PendingAction::Diagnostic => {
                self.error = 0x01;
                self.status = status::DRDY | status::DSC;
                self.raise_irq();
            }
            PendingAction::CheckPower => {
                self.sector_count = 0xFF;
                self.complete_ok();
            }
            PendingAction::FlushCache => {
                if self.flush_guest_writes()
                    == crate::katea_tree::CommitGuestWriteResult::HostIoFailure
                {
                    self.abort();
                } else {
                    self.complete_ok();
                }
            }
        }
    }

    /// Complete a non-data command: DRDY|DSC, clear ERR, raise the IRQ.
    fn complete_ok(&mut self) {
        self.phase = Phase::Idle;
        self.dma_request = None;
        self.pending_command = None;
        self.status = status::DRDY | status::DSC;
        self.error = 0;
        self.raise_irq();
    }

    /// Abort a command: DRDY|ERR with ABRT in the error register.
    fn abort(&mut self) {
        self.phase = Phase::Idle;
        self.dma_request = None;
        self.pending_command = None;
        self.buffer.clear();
        self.buffer_pos = 0;
        self.pio_sectors_remaining = 0;
        self.status = status::DRDY | status::ERR;
        self.error = error::ABRT;
        self.raise_irq();
    }

    /// IDENTIFY DEVICE (0xEC): present the 256-word identify block as a read PIO
    /// buffer, then raise DRQ and IRQ when it is ready.
    fn prepare_identify(&mut self) {
        self.buffer = identify_block(
            self.cylinders,
            HEADS,
            SECTORS_PER_TRACK,
            self.total_sectors(),
            self.dma_mode,
        );
        self.buffer_pos = 0;
        self.phase = Phase::DataIn;
        self.status = status::DRDY | status::DRQ | status::DSC;
        self.error = 0;
        self.raise_irq();
    }

    /// READ SECTORS validates the task file, then schedules the first sector.
    fn read_sectors(&mut self) {
        let Some((lba, count)) = self.command_lba() else {
            self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS);
            return;
        };
        let end = lba.saturating_add(count);
        if end > self.total_sectors() {
            self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS);
            return;
        }
        self.pio_lba = lba;
        self.pio_sectors_remaining = count;
        self.last_access_bytes = 0;
        self.schedule(PendingAction::PrepareRead, pio_transfer_ticks(1));
    }

    /// DELIBERATELY UNBATCHED. The other read paths declare their whole range
    /// and let a host-folder backing coalesce it; this one must not.
    ///
    /// A PIO command serves one sector per DRQ, and the guest drains each one
    /// through the data port before the next is scheduled. A coalesced window
    /// would therefore have to survive from this sector's read until the guest
    /// has drained every sector it covers -- an interval bounded by GUEST
    /// execution, not by a host-side call. Two things follow, and either alone
    /// disqualifies it:
    ///
    /// - The window's whole safety argument is that a host folder cannot change
    ///   underneath it, which holds only because every other declared range is
    ///   opened and closed inside a single host-side function. Across a PIO
    ///   drain the folder is live for as long as the guest takes.
    /// - The command has no single exit. It ends in `read_data_byte` when the
    ///   last sector drains, in `abort`, or in `soft_reset`, and a guest that
    ///   simply stops draining ends it nowhere at all. A range left open there
    ///   would serve stale bytes to whatever read next.
    ///
    /// The win was also the smallest of the three: PIO is the path DOS does not
    /// take. Lifting this means giving the command a single owned lifetime, not
    /// adding a call here.
    fn prepare_read_sector(&mut self) {
        let Some(sector) = self.read_lba(self.pio_lba) else {
            self.abort();
            return;
        };
        self.buffer = sector.to_vec();
        self.buffer_pos = 0;
        self.phase = Phase::DataIn;
        self.status = status::DRDY | status::DRQ | status::DSC;
        self.error = 0;
        self.last_access_bytes = self.last_access_bytes.saturating_add(SECTOR);
        self.raise_irq();
    }

    /// WRITE SECTORS validates the task file, then schedules the first DRQ.
    fn write_sectors(&mut self) {
        let Some((lba, count)) = self.command_lba() else {
            self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS);
            return;
        };
        let end = lba.saturating_add(count);
        if end > self.total_sectors() {
            self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS);
            return;
        }
        self.pio_lba = lba;
        self.pio_sectors_remaining = count;
        self.last_access_bytes = 0;
        self.schedule(PendingAction::PrepareWrite, COMMAND_LATENCY_TICKS);
    }

    fn prepare_write_sector(&mut self, raise_irq: bool) {
        self.buffer = vec![0u8; SECTOR];
        self.buffer_pos = 0;
        self.phase = Phase::DataOut;
        self.status = status::DRDY | status::DRQ | status::DSC;
        self.error = 0;
        if raise_irq {
            self.raise_irq();
        }
    }

    fn begin_dma(&mut self, direction: AtaDmaDirection) {
        let Some(bytes_per_second) = self.dma_mode.bytes_per_second() else {
            self.abort();
            return;
        };
        let Some((lba, sectors)) = self.command_lba() else {
            self.abort();
            return;
        };
        if lba.saturating_add(sectors) > self.total_sectors() {
            self.abort();
            return;
        }
        self.phase = Phase::Idle;
        self.buffer.clear();
        self.buffer_pos = 0;
        self.error = 0;
        self.status = status::BSY;
        self.irq_pending = false;
        self.dma_request = Some(AtaDmaRequest {
            direction,
            lba,
            sectors,
            bytes_per_second,
        });
    }

    fn apply_set_features(&mut self, feature: u8, mode: u8) {
        if feature != 0x03 {
            self.complete_ok();
            return;
        }
        self.dma_mode = match mode {
            0x00 | 0x01 | 0x08..=0x0C => DmaMode::None,
            0x20..=0x22 => DmaMode::Multiword(mode - 0x20),
            0x40..=0x42 => DmaMode::Ultra(mode - 0x40),
            _ => {
                self.abort();
                return;
            }
        };
        self.complete_ok();
    }

    pub(crate) fn pending_dma(&self) -> Option<AtaDmaRequest> {
        self.dma_request
    }

    /// Assemble the whole device-to-memory payload.
    ///
    /// The sectors are contiguous and their count is known before the first one
    /// is looked up, and the entire loop runs inside one host-side call with no
    /// guest-visible step between sectors, so it declares its range and lets a
    /// host-folder backing coalesce the reads. Unlike PIO (see
    /// `prepare_read_sector`), there is no window here for the guest to observe
    /// or interrupt.
    ///
    /// The declared range is closed on every exit, including the short-read
    /// path, so a partially built payload cannot leave a window behind for the
    /// next command to read stale bytes out of.
    pub(crate) fn read_dma_payload(&self) -> Option<Vec<u8>> {
        let request = self.dma_request?;
        if request.direction != AtaDmaDirection::DeviceToMemory {
            return None;
        }
        self.begin_read_command(request.lba, request.sectors);
        let mut payload = Vec::with_capacity(request.byte_len());
        let mut complete = true;
        for lba in request.lba..request.lba + request.sectors {
            match self.read_lba(lba) {
                Some(bytes) => payload.extend_from_slice(&bytes),
                None => {
                    complete = false;
                    break;
                }
            }
        }
        self.end_read_command();
        complete.then_some(payload)
    }

    pub(crate) fn complete_dma_read(&mut self, bytes: usize) {
        let Some(request) = self.dma_request else {
            return;
        };
        if request.direction != AtaDmaDirection::DeviceToMemory || bytes != request.byte_len() {
            self.abort();
            return;
        }
        self.note_guest_read_batch(
            crate::katea_tree::GuestStorageRoute::Dma,
            u64::from(request.sectors),
            dma_transfer_ticks(request),
        );
        self.last_access_bytes = bytes;
        self.advance_dma_task_file(request);
        self.complete_ok();
    }

    pub(crate) fn complete_dma_write(&mut self, data: &[u8]) -> bool {
        let Some(request) = self.dma_request else {
            return false;
        };
        if request.direction != AtaDmaDirection::MemoryToDevice || data.len() != request.byte_len()
        {
            self.abort();
            return false;
        }
        for (index, sector) in data.as_chunks::<SECTOR>().0.iter().enumerate() {
            if !self.write_lba(request.lba + index as u32, sector) {
                self.abort();
                return false;
            }
        }
        self.note_guest_write_wait(
            crate::katea_tree::GuestStorageRoute::Dma,
            dma_transfer_ticks(request),
        );
        if self.commit_guest_write_batch(crate::katea_tree::GuestWriteRoute::Dma)
            == crate::katea_tree::CommitGuestWriteResult::HostIoFailure
        {
            self.abort();
            return false;
        }
        self.last_access_bytes = data.len();
        self.advance_dma_task_file(request);
        self.complete_ok();
        true
    }

    pub(crate) fn abort_dma(&mut self) {
        if self.dma_request.is_some() {
            self.abort();
        }
    }

    fn read_data_byte(&mut self) -> u8 {
        if self.phase != Phase::DataIn {
            return 0;
        }
        let byte = self.buffer.get(self.buffer_pos).copied().unwrap_or(0);
        self.buffer_pos += 1;
        if self.buffer_pos >= self.buffer.len() {
            self.phase = Phase::Idle;
            self.buffer.clear();
            self.buffer_pos = 0;
            if self.pio_sectors_remaining > 0 {
                self.pio_sectors_remaining -= 1;
                self.advance_pio_task_file();
            }
            if self.pio_sectors_remaining > 0 {
                self.schedule(PendingAction::PrepareRead, pio_sector_ticks());
            } else {
                let sectors = self.last_access_bytes / SECTOR;
                self.note_guest_read_batch(
                    crate::katea_tree::GuestStorageRoute::Pio,
                    sectors as u64,
                    pio_transfer_ticks(sectors as u32),
                );
                self.status = status::DRDY | status::DSC;
            }
        }
        byte
    }

    fn write_data_byte(&mut self, value: u8) {
        if self.phase != Phase::DataOut {
            return;
        }
        if self.buffer_pos < self.buffer.len() {
            self.buffer[self.buffer_pos] = value;
            self.buffer_pos += 1;
        }
        if self.buffer_pos >= self.buffer.len() {
            self.phase = Phase::Idle;
            self.schedule(PendingAction::CommitWrite, pio_sector_ticks());
        }
    }

    fn commit_write_sector(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        self.buffer_pos = 0;
        if !self.write_lba(self.pio_lba, &buffer) {
            self.abort();
            return;
        }
        self.last_access_bytes = self.last_access_bytes.saturating_add(SECTOR);
        if self.pio_sectors_remaining > 0 {
            self.pio_sectors_remaining -= 1;
            self.advance_pio_task_file();
        }
        if self.pio_sectors_remaining > 0 {
            self.prepare_write_sector(true);
        } else {
            self.phase = Phase::Idle;
            self.status = status::DRDY | status::DSC;
            self.error = 0;
            let sectors = self.last_access_bytes / SECTOR;
            self.note_guest_write_wait(
                crate::katea_tree::GuestStorageRoute::Pio,
                pio_transfer_ticks(sectors as u32),
            );
            if self.commit_guest_write_batch(crate::katea_tree::GuestWriteRoute::Pio)
                == crate::katea_tree::CommitGuestWriteResult::HostIoFailure
            {
                self.abort();
                return;
            }
            self.raise_irq();
        }
    }

    fn advance_pio_task_file(&mut self) {
        self.sector_count = self.sector_count.wrapping_sub(1);
        self.pio_lba = self.pio_lba.saturating_add(1);
        self.set_task_file_address(self.pio_lba);
    }

    fn advance_dma_task_file(&mut self, request: AtaDmaRequest) {
        self.sector_count = self.sector_count.wrapping_sub(request.sectors as u8);
        self.set_task_file_address(request.lba.saturating_add(request.sectors));
    }

    fn set_task_file_address(&mut self, lba: u32) {
        if self.lba_mode() {
            self.lba_low = lba as u8;
            self.lba_mid = (lba >> 8) as u8;
            self.lba_high = (lba >> 16) as u8;
            self.drive_head = (self.drive_head & 0xf0) | ((lba >> 24) as u8 & 0x0f);
        } else {
            let cylinder_size = HEADS * SECTORS_PER_TRACK;
            let cylinder = lba / cylinder_size;
            let in_cylinder = lba % cylinder_size;
            let head = in_cylinder / SECTORS_PER_TRACK;
            let sector = in_cylinder % SECTORS_PER_TRACK + 1;
            self.lba_low = sector as u8;
            self.lba_mid = cylinder as u8;
            self.lba_high = (cylinder >> 8) as u8;
            self.drive_head = (self.drive_head & 0xf0) | head as u8;
        }
    }

    fn raise_irq(&mut self) {
        self.irq_pending = true;
    }
}

/// Build the 256-word (512-byte) IDENTIFY DEVICE block for the derived geometry.
/// The fields a BIOS and DOS driver read: word 0 general config, words 1/3/6 the
/// default CHS, words 60-61 the LBA28 capacity, the model string byte-swapped per
/// ATA. Limit: SMART and the 48-bit capacity words stay zero.
fn identify_block(
    cylinders: u32,
    heads: u32,
    sectors: u32,
    total_lba: u32,
    dma_mode: DmaMode,
) -> Vec<u8> {
    let mut words = [0u16; 256];
    // Word 0 general configuration: bit 6 = fixed (non-removable) device, bit 15
    // clear marks an ATA (not ATAPI) device. 0x0040 is the value a fixed ATA disk
    // reports.
    words[0] = 0x0040;
    words[1] = cylinders.min(0xFFFF) as u16; // default cylinders
    words[3] = heads as u16; // default heads
    words[6] = sectors as u16; // default sectors per track
    // Word 49 capabilities: LBA and DMA are implemented.
    words[49] = 0x0300;
    // The current CHS translation and Ultra DMA word are valid. Advanced PIO
    // timing words stay invalid rather than advertising cycle data we do not use.
    words[53] = 0x0005;
    words[54] = cylinders.min(0xFFFF) as u16;
    words[55] = heads as u16;
    words[56] = sectors as u16;
    let current_capacity = cylinders.saturating_mul(heads).saturating_mul(sectors);
    words[57] = (current_capacity & 0xFFFF) as u16;
    words[58] = (current_capacity >> 16) as u16;
    // Words 60-61: total addressable sectors in LBA28 mode (little-endian dword,
    // low word first).
    words[60] = (total_lba & 0xFFFF) as u16;
    words[61] = (total_lba >> 16) as u16;
    // Multiword DMA modes 0-2 and Ultra DMA modes 0-2 are supported. The high
    // byte reports the one mode selected through SET FEATURES.
    words[63] = 0x0007;
    words[80] = 0x0010; // ATA/ATAPI-4
    words[88] = 0x0007;
    match dma_mode {
        DmaMode::Multiword(mode) => words[63] |= 1 << (8 + mode),
        DmaMode::Ultra(mode) => words[88] |= 1 << (8 + mode),
        DmaMode::None => {}
    }
    put_string(&mut words[10..20], "IZARRA-HD-0001"); // serial number
    put_string(&mut words[23..27], "1.0 "); // firmware revision
    put_string(&mut words[27..47], "Izarra Hard Disk"); // model number

    let mut bytes = Vec::with_capacity(512);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

/// Write an ASCII string into an ATA word field with the byte-swap ATA uses (the
/// first char goes in the high byte of the first word). Space-padded.
fn put_string(words: &mut [u16], text: &str) {
    let src = text.as_bytes();
    let byte_at = |i: usize| -> u8 { src.get(i).copied().unwrap_or(b' ') };
    for (i, w) in words.iter_mut().enumerate() {
        let hi = byte_at(i * 2);
        let lo = byte_at(i * 2 + 1);
        *w = (u16::from(hi) << 8) | u16::from(lo);
    }
}

/// Borrowed ATA primary-channel controller state for canonical comparison.
///
/// The payload holds the guest-visible task-file registers, latches, and the
/// transfer continuation (PIO buffer, cursors, pending command, armed DMA
/// request). Media content (`Backing`), the host flush flag, GUI telemetry,
/// and the mount-derived cylinder count stay out; capture cross-checks the
/// cylinder count against the derived geometry instead.
///
/// Two producers mutate the disk and never observe each other's register
/// state, by design. The port path drives the task-file protocol. The INT 13h
/// HLE reads and writes content directly through `read_lba`/`write_lba` and
/// charges its time through a synchronous master-clock stall, which can
/// complete an in-flight task-file command mid-service; it never touches the
/// registers, phase, or pending command. Mount, eject, and the BMIDE primary
/// reset are host-side producers that replace the whole device.
///
/// Determinism scope: payload bytes are deterministic given identical guest
/// history AND identical backing. A host-folder (Katea) facade makes the
/// whole machine host-referencing, this section included: sector reads fill
/// the PIO buffer from host files and IDENTIFY bakes host-derived geometry.
pub(crate) struct CanonicalAtaDisk<'a> {
    disk: Option<&'a AtaDisk>,
}

impl<'a> CanonicalAtaDisk<'a> {
    pub(crate) fn new(disk: Option<&'a AtaDisk>) -> Self {
        Self { disk }
    }

    /// Writes version 1 of the ATA controller payload: 66 bytes idle, 578
    /// mid-sector. The buffer is serialized unconditionally, independent of
    /// phase: in the CommitWrite window the guest's not-yet-committed sector
    /// lives only here, with phase already back at Idle.
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        let Some(disk) = self.disk else {
            return out.write_bool(false);
        };
        out.write_bool(true)?;
        out.write_u8(disk.features)?;
        out.write_u8(disk.sector_count)?;
        out.write_u8(disk.lba_low)?;
        out.write_u8(disk.lba_mid)?;
        out.write_u8(disk.lba_high)?;
        out.write_u8(disk.drive_head)?;
        out.write_u8(disk.status)?;
        out.write_u8(disk.error)?;
        out.write_u8(disk.logical_sectors)?;
        out.write_u8(disk.logical_heads)?;
        out.write_bool(disk.interrupts_disabled)?;
        let (dma_tag, dma_value) = match disk.dma_mode {
            DmaMode::None => (0u8, 0u8),
            DmaMode::Multiword(mode) => (1, mode),
            DmaMode::Ultra(mode) => (2, mode),
        };
        out.write_u8(dma_tag)?;
        out.write_u8(dma_value)?;
        out.write_bool(disk.irq_pending)?;
        out.write_u8(match disk.phase {
            Phase::Idle => 0,
            Phase::DataIn => 1,
            Phase::DataOut => 2,
        })?;
        out.write_len_prefixed_bytes(&disk.buffer)?;
        out.write_u32(disk.buffer_pos as u32)?;
        out.write_u32(disk.pio_lba)?;
        out.write_u32(disk.pio_sectors_remaining)?;
        // Fixed 12-byte pending-command record: zeros when absent or when a
        // variant carries no arguments, so golden offsets never move.
        let (present, ticks, action, arg0, arg1) = match disk.pending_command {
            None => (false, 0, 0u8, 0u8, 0u8),
            Some(PendingCommand {
                ticks_remaining,
                action,
            }) => {
                let (tag, arg0, arg1) = match action {
                    PendingAction::PrepareIdentify => (0, 0, 0),
                    PendingAction::PrepareRead => (1, 0, 0),
                    PendingAction::PrepareWrite => (2, 0, 0),
                    PendingAction::CommitWrite => (3, 0, 0),
                    PendingAction::CompleteOk => (4, 0, 0),
                    PendingAction::Abort => (5, 0, 0),
                    PendingAction::Initialize { sectors, heads } => (6, sectors, heads),
                    PendingAction::SetFeatures { feature, mode } => (7, feature, mode),
                    PendingAction::Diagnostic => (8, 0, 0),
                    PendingAction::CheckPower => (9, 0, 0),
                    PendingAction::FlushCache => (10, 0, 0),
                };
                (true, ticks_remaining, tag, arg0, arg1)
            }
        };
        out.write_bool(present)?;
        out.write_u64(ticks)?;
        out.write_u8(action)?;
        out.write_u8(arg0)?;
        out.write_u8(arg1)?;
        // Fixed 18-byte armed-DMA record, same zero-fill convention.
        let (present, direction, lba, sectors, rate) = match disk.dma_request {
            None => (false, 0u8, 0, 0, 0),
            Some(request) => (
                true,
                match request.direction {
                    AtaDmaDirection::DeviceToMemory => 0,
                    AtaDmaDirection::MemoryToDevice => 1,
                },
                request.lba,
                request.sectors,
                request.bytes_per_second,
            ),
        };
        out.write_bool(present)?;
        out.write_u8(direction)?;
        out.write_u32(lba)?;
        out.write_u32(sectors)?;
        out.write_u64(rate)
    }
}

#[cfg(test)]
#[path = "ata_test.rs"]
mod tests;
