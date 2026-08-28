// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The IzarraCD host-side redirector: the INT 2Fh AH=11h file-I/O surface for
//! the CD drive, served from the host's `cdiso::IsoIndex` instead of a guest
//! resident.
//!
//! The contract mirrors the IZCDEX.COM (SHSUCDX 3.09) redirector field by
//! field, extracted from SHSUCDX 3.09's `shsucdx.nsm` (the vendored tree
//! left with slice CD-3; git history holds it) and verified
//! against the Toka-DOS kernel (`kernel.asm` SDA layout, `dosfns.c` call
//! sites). The kernel calls these functions itself for any operation on a
//! drive whose CDS carries the network flag; the arguments live in the SDA at
//! fixed offsets from the DOS data segment (the DOS 4+ layout — the kernel
//! hardcodes SDA format 1):
//!
//! | what | DOS_DS offset |
//! | --- | --- |
//! | current DTA far pointer | 0x32C |
//! | first filename buffer (ASCIZ, fully qualified) | 0x3BE |
//! | search data block (21 bytes) | 0x4BE |
//! | search attribute mask | 0x56D |
//! | extended-open action / mode words | 0x5FD / 0x601 |
//!
//! The redirector arms with that segment (`arm_cd_redirector`); in slice CD-1
//! only tests arm it, in CD-2 the kernel's boot-time claim does. Errors
//! return through CF and AX exactly as IZCDEX returned them; the SDA error
//! fields are untouched (IZCDEX never wrote them either).

use super::*;

// SDA field offsets from the DOS data segment's paragraph base.
const SDA_DTA_PTR: u32 = 0x32C;
const SDA_FILENAME1: u32 = 0x3BE;
const SDA_SDB: u32 = 0x4BE;
const SDA_SEARCH_ATTR: u32 = 0x56D;
const SDA_EXT_ACTION: u32 = 0x5FD;
const SDA_EXT_MODE: u32 = 0x601;

// SFT field offsets (undoc.mac `struc SFT`; FreeDOS sft.h agrees).
const SFT_REFCNT: u32 = 0x00;
const SFT_MODE: u32 = 0x02;
const SFT_ATTRIB: u32 = 0x04;
const SFT_FLAGS: u32 = 0x05;
const SFT_TIME: u32 = 0x0D;
const SFT_SIZE: u32 = 0x11;
const SFT_POS: u32 = 0x15;
const SFT_LBA: u32 = 0x19; // IZCDEX overlays the ISO extent LBA here (".FBN")
const SFT_NAME: u32 = 0x20;

// Search data block layout (21 bytes).
const SDB_DRIVE: usize = 0x00;
const SDB_TEMPLATE: usize = 0x01;
const SDB_SATTR: usize = 0x0C;
const SDB_ENTRY: usize = 0x0D;
const SDB_PARENT: usize = 0x0F;
const SDB_REMAIN: usize = 0x13;
const SDB_BYTES: usize = 0x15;

// Found-dirent block, written at DTA+0x15. Bytes 0x0C..0x16 and 0x1A..0x1C
// are cache metadata in IZCDEX and stay untouched here too (the kernel
// pre-zeroes the block).
const FDB_AT_DTA: u32 = 0x15;
const FDB_TIME: u32 = 0x16;
const FDB_SIZE: u32 = 0x1C;

// DOS error codes, as IZCDEX returned them.
const ERR_FILE_NOT_FOUND: u16 = 0x0002;
const ERR_PATH_NOT_FOUND: u16 = 0x0003;
const ERR_ACCESS_DENIED: u16 = 0x0005;
const ERR_NO_MORE_FILES: u16 = 0x0012;
const ERR_NOT_READY: u16 = 0x0015;

/// The searched-after cursor state one redirector search carries between
/// FindFirst and FindNext, packed into the SDB the kernel round-trips through
/// the caller's DTA. `entry` is the ordinal of the NEXT `IsoDir` entry to
/// test; `parent` is the directory's extent LBA; `remain` is 0 while live and
/// 0xFFFF when exhausted (the IZCDEX sentinel).
struct SearchCursor {
    drive: u8,
    template: [u8; 11],
    sattr: u8,
    entry: u16,
    parent: u32,
    remain: u16,
}

impl SearchCursor {
    fn to_bytes(&self) -> [u8; SDB_BYTES] {
        let mut out = [0u8; SDB_BYTES];
        out[SDB_DRIVE] = self.drive | 0xC0;
        out[SDB_TEMPLATE..SDB_TEMPLATE + 11].copy_from_slice(&self.template);
        out[SDB_SATTR] = self.sattr;
        out[SDB_ENTRY..SDB_ENTRY + 2].copy_from_slice(&self.entry.to_le_bytes());
        out[SDB_PARENT..SDB_PARENT + 4].copy_from_slice(&self.parent.to_le_bytes());
        out[SDB_REMAIN..SDB_REMAIN + 2].copy_from_slice(&self.remain.to_le_bytes());
        out
    }

    fn from_bytes(bytes: &[u8]) -> SearchCursor {
        SearchCursor {
            drive: bytes[SDB_DRIVE] & 0x3F,
            template: bytes[SDB_TEMPLATE..SDB_TEMPLATE + 11].try_into().unwrap(),
            sattr: bytes[SDB_SATTR],
            entry: u16::from_le_bytes(bytes[SDB_ENTRY..SDB_ENTRY + 2].try_into().unwrap()),
            parent: u32::from_le_bytes(bytes[SDB_PARENT..SDB_PARENT + 4].try_into().unwrap()),
            remain: u16::from_le_bytes(bytes[SDB_REMAIN..SDB_REMAIN + 2].try_into().unwrap()),
        }
    }
}

/// `?` in a template matches any character in that position.
fn name_matches(name: &[u8; 11], template: &[u8; 11]) -> bool {
    name.iter()
        .zip(template.iter())
        .all(|(&n, &t)| t == b'?' || t == n)
}

/// The IZCDEX attribute test: every bit the entry carries must be allowed by
/// the mask, with read-only always allowed ("read-only should always match").
fn attr_matches(attr: u8, sattr: u8) -> bool {
    attr & !(sattr | 0x01) == 0
}

impl Machine {
    /// Arm the host CD redirector with the guest kernel's DOS data segment.
    /// Every SDA/SDB/DTA address derives from it. Tests arm this directly;
    /// the Toka-DOS kernel's boot-time drive claim arms it in guest boots.
    pub fn arm_cd_redirector(&mut self, dos_data_segment: u16) {
        self.cd_redirector_dos_ds = Some(dos_data_segment);
    }

    /// Disarm the host CD redirector.
    pub fn disarm_cd_redirector(&mut self) {
        self.cd_redirector_dos_ds = None;
    }

    /// Bytes the redirector Read handler has delivered. The CD-path
    /// equivalent of `cd_pio_byte_count` for the retired PIO path.
    pub fn cd_redirector_read_bytes(&self) -> u64 {
        self.cd_redirector_read_bytes
    }

    fn dos_ds_linear(&self) -> Option<u32> {
        self.cd_redirector_dos_ds.map(|seg| u32::from(seg) << 4)
    }

    fn redirector_drive(&self) -> u8 {
        CD_DRIVE_NUMBER
    }

    /// Read the SDA first-filename buffer: the fully qualified ASCIZ path the
    /// kernel resolved before calling the redirector. `None` when the path
    /// names another drive.
    fn redirector_path(&mut self, ds: u32) -> Option<String> {
        let path = self.read_guest_linear_asciiz_lossy(ds + SDA_FILENAME1, 0x80);
        let bytes = path.as_bytes();
        let drive_letter = b'A' + self.redirector_drive();
        if bytes.first().map(|b| b.to_ascii_uppercase()) != Some(drive_letter)
            || bytes.get(1) != Some(&b':')
        {
            return None;
        }
        Some(path)
    }

    /// The caller's transfer buffer: the far pointer the SDA DTA field holds
    /// (the kernel points it at the user buffer for Read and at the SDA
    /// search state for the find calls), as a guest linear address.
    fn redirector_dta(&mut self, ds: u32) -> u32 {
        let ptr = self.read_guest_linear_block(ds + SDA_DTA_PTR, 4);
        let off = u16::from_le_bytes([ptr[0], ptr[1]]);
        let seg = u16::from_le_bytes([ptr[2], ptr[3]]);
        (u32::from(seg) << 4).wrapping_add(u32::from(off))
    }

    /// The SFT the caller passed in ES:DI, as a guest linear address, when
    /// its device-info word names our drive.
    fn redirector_sft(&mut self) -> Option<u32> {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let sft = es.wrapping_add(u32::from(self.cpu.registers.edi() as u16));
        let flags = self.read_guest_linear_block(sft + SFT_FLAGS, 1)[0];
        (flags & 0x3F == self.redirector_drive()).then_some(sft)
    }

    fn redirector_ok(&mut self, ax: u16) {
        self.set_ax(ax);
        self.set_int_frame_carry(false);
    }

    fn redirector_fail(&mut self, code: u16) {
        self.set_ax(code);
        self.set_int_frame_carry(true);
    }

    /// Serve one INT 2Fh AH=11h call for the CD drive. Returns false when the
    /// redirector is not armed, the call is not one it implements, or the
    /// operation names another drive — the caller then falls back to the
    /// absent-redirector refusal, which is what the kernel's own default
    /// handler answered.
    pub(super) fn handle_cd_redirector(&mut self, ax: u16) -> bool {
        let Some(ds) = self.dos_ds_linear() else {
            return false;
        };
        match ax {
            // ChDir: validate that the path names a directory. The kernel
            // writes the CDS path text itself after a success.
            0x1105 => {
                let Some(path) = self.redirector_path(ds) else {
                    return false;
                };
                match self.cd_iso_index() {
                    Some(index) if index.lookup_dir(&path).is_some() => self.redirector_ok(0),
                    _ => self.redirector_fail(ERR_PATH_NOT_FOUND),
                }
                true
            }
            // Close: drop the SFT reference count, clamped at zero.
            0x1106 => {
                let Some(sft) = self.redirector_sft() else {
                    return false;
                };
                let refcnt = self.read_linear_u16(sft + SFT_REFCNT);
                if refcnt > 0 {
                    self.write_linear_u16(sft + SFT_REFCNT, refcnt - 1);
                }
                self.redirector_ok(0);
                true
            }
            0x1108 => {
                let Some(sft) = self.redirector_sft() else {
                    return false;
                };
                self.redirector_read(ds, sft);
                true
            }
            // GetSpace: ES:DI is the CDS; drive identity comes from its path
            // text. AL=1 sector/cluster, AH=0 media id, BX=total clusters,
            // CX=2048 bytes/sector, DX=0 free (a CD is full by definition).
            0x110C => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let cds = es.wrapping_add(u32::from(self.cpu.registers.edi() as u16));
                let letter = self.read_guest_linear_block(cds, 1)[0].to_ascii_uppercase();
                if letter != b'A' + self.redirector_drive() {
                    return false;
                }
                let sectors = self.cd_iso_index().map_or(0, |index| index.volume_sectors);
                self.set_bx(sectors.min(0xFFFF) as u16);
                self.set_cx(2048);
                self.set_dx(0);
                self.redirector_ok(0x0001);
                true
            }
            // GetAttr: AX=attributes, BX:DI=size, CX=time, DX=date.
            0x110F => {
                let Some(path) = self.redirector_path(ds) else {
                    return false;
                };
                let looked = self.redirector_lookup(&path);
                match looked {
                    Ok(entry) => {
                        self.set_bx((entry.size >> 16) as u16);
                        let edi =
                            (self.cpu.registers.edi() & !0xFFFF) | u32::from(entry.size as u16);
                        self.cpu.registers.set_edi(edi);
                        self.set_cx(entry.dos_time);
                        self.set_dx(entry.dos_date);
                        self.redirector_ok(u16::from(entry.attr));
                    }
                    Err(code) => self.redirector_fail(code),
                }
                true
            }
            0x1116 | 0x112E => {
                let Some(path) = self.redirector_path(ds) else {
                    return false;
                };
                // The SFT is fresh here — its device-info word is filled BY
                // the open — so the drive identity comes from the path alone.
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let sft = es.wrapping_add(u32::from(self.cpu.registers.edi() as u16));
                self.redirector_open(ds, sft, &path, ax == 0x112E);
                true
            }
            0x111B => {
                let Some(path) = self.redirector_path(ds) else {
                    return false;
                };
                self.redirector_find_first(ds, &path);
                true
            }
            0x111C => {
                let sdb = self.read_guest_linear_block(ds + SDA_SDB, SDB_BYTES);
                let cursor = SearchCursor::from_bytes(&sdb);
                if cursor.drive != self.redirector_drive() {
                    return false;
                }
                self.redirector_find_next(ds, cursor);
                true
            }
            // Seek from end: DX:AX = file size + signed CX:DX. The file
            // position is not moved (the kernel stores the result itself).
            0x1121 => {
                let Some(sft) = self.redirector_sft() else {
                    return false;
                };
                let size = self.read_linear_u32(sft + SFT_SIZE);
                let offset = ((self.cpu.registers.ecx() as u16 as u32) << 16)
                    | u32::from(self.cpu.registers.edx() as u16);
                let position = size.wrapping_add(offset);
                self.set_dx((position >> 16) as u16);
                self.redirector_ok(position as u16);
                true
            }
            _ => false,
        }
    }

    /// Resolve a redirector path to its entry. The root itself answers as a
    /// plain directory with no size or stamp (IZCDEX returned its cached
    /// volume entry there; nothing consumes those fields through GetAttr).
    fn redirector_lookup(&mut self, path: &str) -> Result<cdiso::IsoEntry, u16> {
        let Some(index) = self.cd_iso_index() else {
            return Err(ERR_NOT_READY);
        };
        if let Some(entry) = index.lookup(path) {
            return Ok(entry.clone());
        }
        if index.lookup_dir(path).is_some() {
            return Ok(cdiso::IsoEntry {
                name: *b"           ",
                attr: 0x10,
                lba: 0,
                size: 0,
                dos_time: 0,
                dos_date: 0,
                subdir: None,
            });
        }
        // IZCDEX's Lookup reported a missing final component as 02h and a
        // missing/invalid directory component as 03h.
        let parent = match path.rfind('\\') {
            Some(at) => &path[..at],
            None => "",
        };
        if index.lookup_dir(parent).is_some() {
            Err(ERR_FILE_NOT_FOUND)
        } else {
            Err(ERR_PATH_NOT_FOUND)
        }
    }

    /// Open (1116h) and extended open (112Eh): reject write access, then fill
    /// the SFT the way IZCDEX filled it.
    fn redirector_open(&mut self, ds: u32, sft: u32, path: &str, extended: bool) {
        if extended {
            let action = self.read_guest_linear_block(ds + SDA_EXT_ACTION, 1)[0];
            let mode = self.read_guest_linear_block(ds + SDA_EXT_MODE, 1)[0];
            // Action bit 1 = truncate/replace an existing file; mode bit 0 =
            // write access. A CD grants neither.
            if action & 0x02 != 0 || mode & 0x01 != 0 {
                return self.redirector_fail(ERR_ACCESS_DENIED);
            }
        } else {
            // The kernel pushes the open mode word before INT 2Fh, so it sits
            // above the INT frame: SS:SP+6. Write access (bit 0) is denied;
            // read-write (mode 2) passes, as it did through IZCDEX.
            let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
            let sp = self.cpu.registers.esp() as u16;
            let mode = self.read_guest_word(ss + u32::from(sp.wrapping_add(6)));
            if mode & 0x0001 != 0 {
                return self.redirector_fail(ERR_ACCESS_DENIED);
            }
        }
        let entry = match self.redirector_lookup(path) {
            Ok(entry) if !entry.is_dir() => entry,
            Ok(_) => return self.redirector_fail(ERR_FILE_NOT_FOUND),
            Err(code) => return self.redirector_fail(code),
        };
        self.write_guest_linear_block(sft + SFT_NAME, &entry.name);
        let mode = self.read_guest_linear_block(sft + SFT_MODE, 1)[0];
        self.write_guest_linear_block(sft + SFT_MODE, &[mode | 0x02]);
        self.write_guest_linear_block(sft + SFT_ATTRIB, &[entry.attr]);
        let flags = 0x8040u16 | u16::from(self.redirector_drive());
        self.write_linear_u16(sft + SFT_FLAGS, flags);
        self.write_linear_u16(sft + SFT_TIME, entry.dos_time);
        self.write_linear_u16(sft + SFT_TIME + 2, entry.dos_date);
        self.write_linear_u32(sft + SFT_SIZE, entry.size);
        self.write_linear_u32(sft + SFT_POS, 0);
        self.write_linear_u32(sft + SFT_LBA, entry.lba);
        self.redirector_ok(0);
    }

    /// Read (1108h): CX bytes from the SFT position into the caller's buffer
    /// (the SDA DTA points at it), clamped to the file size. Whole sectors
    /// come straight from the image; the position advances by what was
    /// delivered and CX reports it (0 at EOF).
    fn redirector_read(&mut self, ds: u32, sft: u32) {
        let size = self.read_linear_u32(sft + SFT_SIZE);
        let pos = self.read_linear_u32(sft + SFT_POS);
        let lba0 = self.read_linear_u32(sft + SFT_LBA);
        let want = u32::from(self.cpu.registers.ecx() as u16);
        let remaining = size.saturating_sub(pos);
        let count = want.min(remaining);
        let buffer = self.redirector_dta(ds);

        let mut delivered = 0u32;
        while delivered < count {
            let at = pos + delivered;
            let lba = lba0 + (at >> 11);
            let in_sector = (at & 0x7FF) as usize;
            let chunk = ((count - delivered) as usize).min(cdimage::DATA_SECTOR - in_sector);
            let Some(sector) = self
                .ide
                .device()
                .image()
                .and_then(|img| img.read_data_sector(lba))
            else {
                return self.redirector_fail(ERR_NOT_READY);
            };
            self.write_guest_linear_block(
                buffer + delivered,
                &sector[in_sector..in_sector + chunk],
            );
            delivered += chunk as u32;
        }
        self.write_linear_u32(sft + SFT_POS, pos + delivered);
        self.cd_redirector_read_bytes += u64::from(delivered);
        self.set_cx(delivered as u16);
        self.redirector_ok(0);
    }

    fn redirector_find_first(&mut self, ds: u32, path: &str) {
        let sattr = self.read_guest_linear_block(ds + SDA_SEARCH_ATTR, 1)[0];
        let (dir_part, name_part) = match path.rfind('\\') {
            Some(at) => (&path[..at], &path[at + 1..]),
            None => ("", path),
        };
        let template = cdiso::fcb_pattern(name_part.as_bytes());
        let Some((parent, label)) = self.cd_iso_index().and_then(|index| {
            index
                .lookup_dir(dir_part)
                .map(|dir| (dir.extent_lba, index.volume_label))
        }) else {
            return self.redirector_fail(ERR_PATH_NOT_FOUND);
        };
        // A search for exactly the volume-label attribute returns the label
        // from the PVD volume identifier and ends the search.
        if sattr == 0x08 {
            let cursor = SearchCursor {
                drive: self.redirector_drive(),
                template,
                sattr,
                entry: 0,
                parent,
                remain: 0xFFFF,
            };
            self.write_guest_linear_block(ds + SDA_SDB, &cursor.to_bytes());
            let dta = self.redirector_dta(ds);
            let mut head = [0u8; 12];
            head[..11].copy_from_slice(&label);
            head[11] = 0x08;
            self.write_guest_linear_block(dta + FDB_AT_DTA, &head);
            self.write_guest_linear_block(dta + FDB_AT_DTA + FDB_TIME, &[0u8; 10]);
            return self.redirector_ok(0);
        }
        let cursor = SearchCursor {
            drive: self.redirector_drive(),
            template,
            sattr,
            entry: 0,
            parent,
            remain: 0,
        };
        self.redirector_scan(ds, cursor, ERR_NO_MORE_FILES);
    }

    fn redirector_find_next(&mut self, ds: u32, cursor: SearchCursor) {
        if cursor.remain == 0xFFFF || cursor.sattr == 0x08 {
            return self.redirector_fail(ERR_NO_MORE_FILES);
        }
        self.redirector_scan(ds, cursor, ERR_NO_MORE_FILES);
    }

    /// Advance a search cursor to its next match. Writes the SDB back in
    /// either outcome, then the found-dirent block on a match.
    fn redirector_scan(&mut self, ds: u32, mut cursor: SearchCursor, miss: u16) {
        let matched = self.cd_iso_index().and_then(|index| {
            let dir = index.dir_by_lba(cursor.parent)?;
            dir.entries
                .iter()
                .enumerate()
                .skip(cursor.entry as usize)
                .find(|(_, entry)| {
                    name_matches(&entry.name, &cursor.template)
                        && attr_matches(entry.attr, cursor.sattr)
                })
                .map(|(ordinal, entry)| (ordinal, entry.clone()))
        });
        match matched {
            Some((ordinal, entry)) => {
                // The cursor field is 16-bit; a directory long enough to
                // overflow it (only a malformed disc) ends the search after
                // this match instead of wrapping and re-serving entries.
                match u16::try_from(ordinal + 1) {
                    Ok(next) => {
                        cursor.entry = next;
                        cursor.remain = 0;
                    }
                    Err(_) => {
                        cursor.entry = u16::MAX;
                        cursor.remain = 0xFFFF;
                    }
                }
                self.write_guest_linear_block(ds + SDA_SDB, &cursor.to_bytes());
                let dta = self.redirector_dta(ds);
                let mut head = [0u8; 12];
                head[..11].copy_from_slice(&entry.name);
                head[11] = entry.attr;
                self.write_guest_linear_block(dta + FDB_AT_DTA, &head);
                let mut stamp = [0u8; 4];
                stamp[..2].copy_from_slice(&entry.dos_time.to_le_bytes());
                stamp[2..].copy_from_slice(&entry.dos_date.to_le_bytes());
                self.write_guest_linear_block(dta + FDB_AT_DTA + FDB_TIME, &stamp);
                self.write_guest_linear_block(
                    dta + FDB_AT_DTA + FDB_SIZE,
                    &entry.size.to_le_bytes(),
                );
                self.redirector_ok(0);
            }
            None => {
                cursor.remain = 0xFFFF;
                self.write_guest_linear_block(ds + SDA_SDB, &cursor.to_bytes());
                self.redirector_fail(miss);
            }
        }
    }

    fn read_linear_u16(&mut self, linear: u32) -> u16 {
        let bytes = self.read_guest_linear_block(linear, 2);
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    fn read_linear_u32(&mut self, linear: u32) -> u32 {
        let bytes = self.read_guest_linear_block(linear, 4);
        u32::from_le_bytes(bytes.try_into().unwrap())
    }

    fn write_linear_u16(&mut self, linear: u32, value: u16) {
        self.write_guest_linear_block(linear, &value.to_le_bytes());
    }

    fn write_linear_u32(&mut self, linear: u32, value: u32) {
        self.write_guest_linear_block(linear, &value.to_le_bytes());
    }
}
