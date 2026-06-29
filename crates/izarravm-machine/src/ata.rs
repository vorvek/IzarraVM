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
//! Transfers are PIO, the path every DOS driver and the BIOS use. A command runs
//! synchronously inside the port write, so BSY is never observed; the host sees
//! DRDY|DRQ when a data buffer is ready and DRDY alone when idle. Completion
//! raises IRQ14 unless nIEN masks it.
//!
//! Limit: one master, no slave. The channel models a single drive 0; a slave
//! select reads not-present. Lift by holding two `AtaDisk`s per channel and
//! routing on the drive bit.
//! Limit: LBA28 only, no LBA48. The capacity caps at 2^28-1 sectors (128 GB),
//! plenty for the era. Lift by decoding the READ/WRITE SECTORS EXT (0x24/0x34)
//! commands and the high-order LBA bytes.

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

/// ATA status register bits.
mod status {
    pub const ERR: u8 = 0x01; // error: consult the error register
    pub const DRQ: u8 = 0x08; // data request: a PIO word is ready on the data port
    pub const DSC: u8 = 0x10; // device seek complete
    pub const DRDY: u8 = 0x40; // device ready
    // BSY (0x80) is never set: each command runs synchronously inside the port
    // write, so the host never observes a busy window.
}

/// ATA error register bits used by the abort path.
mod error {
    pub const ABRT: u8 = 0x04; // command aborted
}

/// Where the disk's sectors come from. A flat image holds the whole disk in RAM
/// (the today path: a mounted .img); a host-folder facade serves sectors lazily
/// from a `KateaVolume` over a host directory, so a huge folder never lands in
/// memory. The facade is read-only in M0: writes are no-ops.
#[derive(Debug)]
enum Backing {
    /// A flat sector array, addressed by `lba * SECTOR`.
    Image(Vec<u8>),
    /// A lazy FAT32 view over a host folder. Boxed because `KateaVolume` is large
    /// relative to the `Vec` it sits beside in the enum.
    HostFolder(Box<crate::katea_volume::KateaVolume>),
}

/// What the data port is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Idle: no PIO buffer in flight.
    Idle,
    /// Draining a read buffer to the host (device-to-host).
    DataIn,
    /// Filling a write buffer from the host (host-to-device); flushed to the
    /// image when the programmed sector count is satisfied.
    DataOut,
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
    /// geometry. Limit: stored but CHS reads use the derived geometry, so a
    /// host that reprograms a nonstandard translation is not honored; lift by
    /// routing chs_to_lba through these.
    logical_sectors: u8,
    logical_heads: u8,

    /// nIEN (control register bit 1): interrupts disabled while set.
    interrupts_disabled: bool,
    /// PIO transfer phase and the buffer it drains or fills.
    phase: Phase,
    buffer: Vec<u8>,
    buffer_pos: usize,
    /// For a multi-sector write, the first LBA of the in-flight buffer.
    write_lba: u32,
    /// Set on command completion so the machine forwards IRQ14 to the PIC.
    irq_pending: bool,
    /// Bytes moved by the last data command, for the GUI access LED.
    last_access_bytes: usize,
}

impl AtaDisk {
    /// Mount a flat sector image, padding up to a whole sector if needed. The
    /// geometry is derived from the padded length.
    pub fn new(mut image: Vec<u8>) -> Self {
        if image.len() % SECTOR != 0 {
            let pad = SECTOR - (image.len() % SECTOR);
            image.resize(image.len() + pad, 0);
        }
        let total_sectors = (image.len() / SECTOR) as u32;
        Self::with_backing(Backing::Image(image), total_sectors)
    }

    /// Mount a lazy host-folder facade as the disk. The geometry is derived from
    /// the volume's whole-disk sector count, the same way `new` derives it from an
    /// image length, so the BIOS sees the same CHS translation either way.
    pub fn from_host_folder(volume: crate::katea_volume::KateaVolume) -> Self {
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
            write_lba: 0,
            irq_pending: false,
            last_access_bytes: 0,
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

    /// Read one whole 512-byte sector at `lba`, or None if past the end. The facade
    /// synthesizes sectors on demand, so this returns an owned array rather than a
    /// borrow into a backing buffer.
    pub fn read_lba(&self, lba: u32) -> Option<[u8; SECTOR]> {
        match &self.backing {
            Backing::Image(image) => {
                let off = lba as usize * SECTOR;
                image.get(off..off + SECTOR).map(|s| {
                    let mut out = [0u8; SECTOR];
                    out.copy_from_slice(s);
                    out
                })
            }
            Backing::HostFolder(volume) => {
                (lba < volume.total_sectors()).then(|| volume.read_sector(lba))
            }
        }
    }

    /// Overwrite one whole 512-byte sector at `lba`. Returns false if past the
    /// end or `data` is short. A host-folder facade is read-only in M0, so writes
    /// to it always fail.
    pub fn write_lba(&mut self, lba: u32, data: &[u8]) -> bool {
        // ponytail: the write-back engine (syncing guest writes to host files) is
        // M1 work; M0 is read-only, so a folder-backed write is simply rejected.
        let Backing::Image(image) = &mut self.backing else {
            return false;
        };
        let off = lba as usize * SECTOR;
        if data.len() < SECTOR || off + SECTOR > image.len() {
            return false;
        }
        image[off..off + SECTOR].copy_from_slice(&data[..SECTOR]);
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

    /// Take the pending IRQ (the machine forwards it to the PIC). nIEN suppresses
    /// the forward, matching a channel with interrupts masked.
    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending && !self.interrupts_disabled;
        self.irq_pending = false;
        pending
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
        self.phase = Phase::Idle;
        self.buffer.clear();
        self.buffer_pos = 0;
        self.status = status::DRDY | status::DSC;
        // Diagnostic code 0x01: device 0 passed (ATA 9.1). An ATA disk leaves the
        // signature registers at 0x00 sector-count/LBA-low and 0x0000 cylinder,
        // the way a non-packet device does, so the host can tell it from ATAPI.
        self.error = 0x01;
        self.sector_count = 0x01;
        self.lba_low = 0x01;
        self.lba_mid = 0x00;
        self.lba_high = 0x00;
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
            self.chs_to_lba(cyl, head, sector)?
        };
        Some((lba, count))
    }

    fn write_command(&mut self, command: u8) {
        if !self.master_selected() {
            // No slave device: any command to it aborts.
            self.abort();
            return;
        }
        match command {
            0xEC => self.identify_device(),
            0x20 | 0x21 => self.read_sectors(),
            0x30 | 0x31 => self.write_sectors(),
            // READ/WRITE MULTIPLE behave like the single-sector PIO forms here:
            // the model has no per-block interrupt, so each sector still drains
            // through the data port. Limit: no multi-count block size, lift by
            // honoring the SET MULTIPLE MODE block and interrupting per block.
            0xC4 => self.read_sectors(),
            0xC5 => self.write_sectors(),
            // RECALIBRATE (0x10-0x1F): seek to cylinder 0, complete with DSC.
            0x10..=0x1F => self.complete_ok(),
            // INITIALIZE DEVICE PARAMETERS (0x91): set the logical CHS the host
            // wants. sector_count = sectors per track, drive_head low nibble + 1
            // = heads. Accept and ack.
            0x91 => {
                self.logical_sectors = self.sector_count;
                self.logical_heads = (self.drive_head & 0x0F) + 1;
                self.complete_ok();
            }
            // SET FEATURES (0xEF): transfer-mode and cache knobs. Acknowledge.
            0xEF => self.complete_ok(),
            // EXECUTE DEVICE DIAGNOSTIC (0x90): report device 0 passed (0x01).
            0x90 => {
                self.error = 0x01;
                self.complete_ok();
            }
            // CHECK POWER MODE (0xE5) / IDLE (0xE3) / standby ack: report active.
            0xE5 => {
                self.sector_count = 0xFF; // 0xFF = active/idle
                self.complete_ok();
            }
            // NOP (0x00) always aborts on hardware, never a silent success.
            _ => self.abort(),
        }
    }

    /// Complete a non-data command: DRDY|DSC, clear ERR, raise the IRQ.
    fn complete_ok(&mut self) {
        self.phase = Phase::Idle;
        self.status = status::DRDY | status::DSC;
        self.error = 0;
        self.raise_irq();
    }

    /// Abort a command: DRDY|ERR with ABRT in the error register.
    fn abort(&mut self) {
        self.phase = Phase::Idle;
        self.buffer.clear();
        self.buffer_pos = 0;
        self.status = status::DRDY | status::ERR;
        self.error = error::ABRT;
        self.raise_irq();
    }

    /// IDENTIFY DEVICE (0xEC): present the 256-word identify block as a read PIO
    /// buffer, DRQ raised, IRQ on completion.
    fn identify_device(&mut self) {
        self.buffer = identify_block(
            self.cylinders,
            HEADS,
            SECTORS_PER_TRACK,
            self.total_sectors(),
        );
        self.buffer_pos = 0;
        self.phase = Phase::DataIn;
        self.status = status::DRDY | status::DRQ | status::DSC;
        self.error = 0;
        self.raise_irq();
    }

    /// READ SECTORS (0x20/0x21): gather the requested run into the read buffer and
    /// arm the data-in drain. An out-of-range run aborts.
    fn read_sectors(&mut self) {
        let Some((lba, count)) = self.command_lba() else {
            self.abort();
            return;
        };
        let end = lba.saturating_add(count);
        if end > self.total_sectors() {
            self.abort();
            return;
        }
        let mut buf = Vec::with_capacity(count as usize * SECTOR);
        for l in lba..end {
            match self.read_lba(l) {
                Some(s) => buf.extend_from_slice(&s),
                None => {
                    self.abort();
                    return;
                }
            }
        }
        self.last_access_bytes = buf.len();
        self.buffer = buf;
        self.buffer_pos = 0;
        self.phase = Phase::DataIn;
        self.status = status::DRDY | status::DRQ | status::DSC;
        self.error = 0;
        // The host drains the first sector immediately; the IRQ signals the buffer
        // is ready, matching a real drive that interrupts when data is available.
        self.raise_irq();
    }

    /// WRITE SECTORS (0x30/0x31): arm the data-out phase. The host writes the
    /// sector bytes through the data port; the buffer flushes to the image once
    /// the programmed count is filled.
    fn write_sectors(&mut self) {
        let Some((lba, count)) = self.command_lba() else {
            self.abort();
            return;
        };
        let end = lba.saturating_add(count);
        if end > self.total_sectors() {
            self.abort();
            return;
        }
        self.write_lba = lba;
        self.buffer = vec![0u8; count as usize * SECTOR];
        self.buffer_pos = 0;
        self.phase = Phase::DataOut;
        // DRQ up, awaiting the host's data. No IRQ yet: WRITE SECTORS interrupts
        // on completion, after the host has fed the data, not before.
        self.status = status::DRDY | status::DRQ | status::DSC;
        self.error = 0;
    }

    fn read_data_byte(&mut self) -> u8 {
        if self.phase != Phase::DataIn {
            return 0;
        }
        let byte = self.buffer.get(self.buffer_pos).copied().unwrap_or(0);
        self.buffer_pos += 1;
        if self.buffer_pos >= self.buffer.len() {
            // Whole transfer complete: drop DRQ, go idle.
            self.phase = Phase::Idle;
            self.buffer.clear();
            self.buffer_pos = 0;
            self.status = status::DRDY | status::DSC;
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
            // The host has fed the whole run: flush each sector to the image.
            let sectors = self.buffer.len() / SECTOR;
            let buffer = std::mem::take(&mut self.buffer);
            for i in 0..sectors {
                let slice = &buffer[i * SECTOR..(i + 1) * SECTOR];
                self.write_lba(self.write_lba + i as u32, slice);
            }
            self.last_access_bytes = buffer.len();
            self.buffer_pos = 0;
            self.phase = Phase::Idle;
            self.status = status::DRDY | status::DSC;
            self.error = 0;
            // Completion raises the IRQ, the way a drive signals the write done.
            self.raise_irq();
        }
    }

    fn raise_irq(&mut self) {
        self.irq_pending = true;
    }
}

/// Build the 256-word (512-byte) IDENTIFY DEVICE block for the derived geometry.
/// The fields a BIOS and DOS driver read: word 0 general config, words 1/3/6 the
/// default CHS, words 60-61 the LBA28 capacity, the model string byte-swapped per
/// ATA. Limit: only the words a real probe reads are filled; SMART, UDMA, and
/// the 48-bit capacity words stay zero.
fn identify_block(cylinders: u32, heads: u32, sectors: u32, total_lba: u32) -> Vec<u8> {
    let mut words = [0u16; 256];
    // Word 0 general configuration: bit 6 = fixed (non-removable) device, bit 15
    // clear marks an ATA (not ATAPI) device. 0x0040 is the value a fixed ATA disk
    // reports.
    words[0] = 0x0040;
    words[1] = cylinders.min(0xFFFF) as u16; // default cylinders
    words[3] = heads as u16; // default heads
    words[6] = sectors as u16; // default sectors per track
    // Word 49 capabilities: bit 9 LBA supported, bit 8 DMA supported (cosmetic).
    words[49] = 0x0300;
    // Words 53/54-58: the current CHS translation is valid (bit 0 of word 53),
    // echoing the default geometry.
    words[53] = 0x0001;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk whose first byte of each sector is a marker derived from the LBA.
    fn marked_disk(sectors: usize) -> AtaDisk {
        let mut bytes = vec![0u8; sectors * SECTOR];
        for s in 0..sectors {
            bytes[s * SECTOR] = (s as u8).wrapping_add(0x10);
        }
        AtaDisk::new(bytes)
    }

    /// Set up the task file for an LBA28 access at `lba` of `count` sectors.
    fn program_lba(disk: &mut AtaDisk, lba: u32, count: u8) {
        disk.write_port(PRIMARY_CMD_BASE + 2, count); // sector count
        disk.write_port(PRIMARY_CMD_BASE + 3, lba as u8); // LBA 0-7
        disk.write_port(PRIMARY_CMD_BASE + 4, (lba >> 8) as u8); // LBA 8-15
        disk.write_port(PRIMARY_CMD_BASE + 5, (lba >> 16) as u8); // LBA 16-23
        disk.write_port(PRIMARY_CMD_BASE + 6, 0x40 | ((lba >> 24) as u8 & 0x0F)); // LBA mode + 24-27
    }

    #[test]
    fn geometry_is_16_heads_63_spt() {
        // 16 * 63 = 1008 sectors per cylinder; 4032 sectors is 4 cylinders.
        let disk = marked_disk(4032);
        assert_eq!(disk.heads(), 16);
        assert_eq!(disk.sectors_per_track(), 63);
        assert_eq!(disk.cylinders(), 4);
        assert_eq!(disk.total_sectors(), 4032);
    }

    #[test]
    fn chs_round_trips_to_lba() {
        let disk = marked_disk(4032);
        // CHS(0,0,1) is LBA 0; sector is 1-based.
        assert_eq!(disk.chs_to_lba(0, 0, 1), Some(0));
        // CHS(0,1,1) is the start of the second track: head 1 * 63 spt.
        assert_eq!(disk.chs_to_lba(0, 1, 1), Some(63));
        // CHS(1,0,1) is the start of the second cylinder: 16 heads * 63 spt.
        assert_eq!(disk.chs_to_lba(1, 0, 1), Some(16 * 63));
        // Sector 0 and an oversize sector are invalid.
        assert_eq!(disk.chs_to_lba(0, 0, 0), None);
        assert_eq!(disk.chs_to_lba(0, 0, 64), None);
    }

    #[test]
    fn identify_round_trips_geometry() {
        let disk_sectors = 4032usize;
        let mut disk = marked_disk(disk_sectors);
        disk.write_port(PRIMARY_CMD_BASE + 7, 0xEC); // IDENTIFY DEVICE
        assert_eq!(disk.status & status::DRQ, status::DRQ);
        let mut block = Vec::with_capacity(512);
        for _ in 0..512 {
            block.push(disk.read_port(PRIMARY_CMD_BASE).unwrap());
        }
        let word = |i: usize| u16::from_le_bytes([block[i * 2], block[i * 2 + 1]]);
        assert_eq!(word(0), 0x0040); // fixed ATA device
        assert_eq!(word(1), 4); // cylinders
        assert_eq!(word(3), 16); // heads
        assert_eq!(word(6), 63); // sectors per track
        let lba = u32::from(word(60)) | (u32::from(word(61)) << 16);
        assert_eq!(lba, disk_sectors as u32);
        // The drain dropped DRQ and returned to ready.
        assert_eq!(disk.status & status::DRQ, 0);
        assert_eq!(disk.status & status::DRDY, status::DRDY);
        // Completion raised the IRQ.
        assert!(disk.take_irq());
    }

    #[test]
    fn pio_write_then_read_round_trips_a_sector() {
        let mut disk = marked_disk(64);
        // WRITE one sector at LBA 5 with a recognizable pattern.
        program_lba(&mut disk, 5, 1);
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x30); // WRITE SECTORS
        assert_eq!(disk.status & status::DRQ, status::DRQ);
        let mut pattern = [0u8; SECTOR];
        for (i, b) in pattern.iter_mut().enumerate() {
            *b = (i as u8) ^ 0xA5;
        }
        for b in pattern {
            disk.write_port(PRIMARY_CMD_BASE, b);
        }
        // The write completed: DRQ dropped, IRQ raised.
        assert_eq!(disk.status & status::DRQ, 0);
        assert!(disk.take_irq());
        assert!(disk.dirty);

        // READ it back through the data port.
        program_lba(&mut disk, 5, 1);
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
        assert_eq!(disk.status & status::DRQ, status::DRQ);
        let mut out = [0u8; SECTOR];
        for slot in out.iter_mut() {
            *slot = disk.read_port(PRIMARY_CMD_BASE).unwrap();
        }
        assert_eq!(out, pattern);
        assert_eq!(disk.status & status::DRQ, 0);
    }

    #[test]
    fn read_past_end_aborts() {
        let mut disk = marked_disk(8);
        program_lba(&mut disk, 8, 1); // LBA 8 on an 8-sector disk
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x20);
        assert_eq!(disk.status & status::ERR, status::ERR);
        assert_eq!(disk.error, error::ABRT);
        assert_eq!(disk.status & status::DRQ, 0);
    }

    #[test]
    fn slave_select_aborts_commands() {
        let mut disk = marked_disk(8);
        disk.write_port(PRIMARY_CMD_BASE + 6, 0x10); // select slave (drive bit 4)
        disk.write_port(PRIMARY_CMD_BASE + 7, 0xEC); // IDENTIFY to the absent slave
        assert_eq!(disk.status & status::ERR, status::ERR);
        assert_eq!(disk.error, error::ABRT);
    }

    #[test]
    fn nien_suppresses_the_irq() {
        let mut disk = marked_disk(8);
        disk.write_port(PRIMARY_CTRL, 0x02); // nIEN
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
        assert!(!disk.take_irq());
    }

    #[test]
    fn sector_count_zero_means_256() {
        let mut disk = marked_disk(300);
        program_lba(&mut disk, 0, 0); // count 0 -> 256 sectors
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
        // 256 sectors are buffered; draining them all returns to ready.
        assert_eq!(disk.status & status::DRQ, status::DRQ);
        for _ in 0..(256 * SECTOR) {
            disk.read_port(PRIMARY_CMD_BASE);
        }
        assert_eq!(disk.status & status::DRQ, 0);
    }

    #[test]
    fn chs_addressing_reads_the_right_sector() {
        let mut disk = marked_disk(4032);
        // CHS(0,1,1) is LBA 63; the marker there is 63 + 0x10.
        disk.write_port(PRIMARY_CMD_BASE + 2, 1); // count 1
        disk.write_port(PRIMARY_CMD_BASE + 3, 1); // sector number (1-based)
        disk.write_port(PRIMARY_CMD_BASE + 4, 0); // cylinder low
        disk.write_port(PRIMARY_CMD_BASE + 5, 0); // cylinder high
        disk.write_port(PRIMARY_CMD_BASE + 6, 0x01); // CHS mode (bit 6 clear), head 1
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
        let first = disk.read_port(PRIMARY_CMD_BASE).unwrap();
        assert_eq!(first, 63u8.wrapping_add(0x10));
    }

    #[test]
    fn initialize_device_parameters_acks() {
        let mut disk = marked_disk(8);
        disk.write_port(PRIMARY_CMD_BASE + 2, 63); // sectors per track
        disk.write_port(PRIMARY_CMD_BASE + 6, 0x0F); // 15+1 = 16 heads
        disk.write_port(PRIMARY_CMD_BASE + 7, 0x91); // INITIALIZE DEVICE PARAMETERS
        assert_eq!(disk.status & status::ERR, 0);
        assert_eq!(disk.status & status::DRDY, status::DRDY);
        assert!(disk.take_irq());
    }
}
