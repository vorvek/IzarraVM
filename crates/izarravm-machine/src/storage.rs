// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Machine {
    /// Mount a raw floppy image into drive A:. The geometry is derived from the
    /// image length; an unrecognized size returns an error and leaves any
    /// previously mounted image in place.
    pub fn mount_floppy(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let floppy = floppy::Floppy::from_image(bytes)?;
        let geometry = floppy.geometry();
        self.floppy = Some(floppy);
        self.set_equipment_floppy(true);
        self.fdc.set_media_geometry(Some(geometry));
        Ok(())
    }

    /// Track drive A: in the BDA equipment word (0040:0010) that INT 11h returns. Bit 0 is
    /// the floppy-installed flag and bits 7-6 the drive count minus one; with one drive
    /// modeled, present means bit 0 set with bits 7-6 clear, absent means both cleared.
    fn set_equipment_floppy(&mut self, present: bool) {
        let mut word = self.memory.read_u16(0x410).unwrap_or(BIOS_EQUIPMENT_WORD);
        if present {
            word = (word & !0x00C0) | 0x0001;
        } else {
            word &= !0x00C1;
        }
        let _ = self.memory.write_u16(0x410, word);
    }

    /// Eject the A: floppy, returning its current image bytes (including any
    /// in-session writes) so the caller can flush them back to disk. Returns
    /// None when the drive is empty.
    pub fn eject_floppy(&mut self) -> Option<Vec<u8>> {
        let bytes = self.floppy.take().map(|f| f.bytes().to_vec());
        self.set_equipment_floppy(false);
        self.fdc.set_media_geometry(None);
        bytes
    }

    /// Whether the mounted A: floppy took a guest write this session. The host
    /// flushes the image back to its source IMG only when this is true, so an
    /// unwritten disk is ejected without rewriting the file. False when the drive
    /// is empty.
    pub fn floppy_dirty(&self) -> bool {
        self.floppy.as_ref().is_some_and(|f| f.dirty)
    }

    /// Monotonic access counts for drives A: (floppy) and C: (host). The GUI
    /// samples these per frame and flashes a drive LED when one advances.
    pub fn drive_access_counts(&self) -> (u64, u64) {
        (self.floppy_accesses, self.c_accesses)
    }

    /// Monotonic CD-ROM access count. The GUI samples this to flash the optical
    /// drive's access LED; it advances on every data read the ATAPI device serves.
    pub fn cd_access_count(&self) -> u64 {
        self.cd_accesses
    }

    /// Mount a CD image into the ATAPI drive. The image is a parsed `CdImage`
    /// built by the caller from an ISO or a CUE/BIN pair, so the machine stays
    /// agnostic to the host file layout.
    pub fn mount_cd(&mut self, image: CdImage) {
        self.ide.device_mut().insert(image);
    }

    /// Eject the CD, leaving the ATAPI drive empty.
    pub fn eject_cd(&mut self) {
        self.ide.device_mut().eject();
    }

    /// Mount a flat hard-disk image as the primary master (C:). The geometry is
    /// derived from the image length, padded up to a whole sector. INT 13h
    /// DL>=0x80 and the primary-channel ports then serve it. Seeds the BDA fixed-
    /// disk count to 1 so a guest reading 0040:0075 sees the drive.
    pub fn mount_hdd(&mut self, bytes: Vec<u8>) {
        self.bmide.reset_primary();
        self.ata = Some(ata::AtaDisk::new(bytes));
        let _ = self.publish_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
    }

    /// Mount a host folder as C: through Katea with extra InMemory system files
    /// overlaid on top of the standard payload. Each entry in `overrides` replaces
    /// an existing payload file of the same name (case-insensitive, e.g. a custom
    /// AUTOEXEC.BAT) or is appended (e.g. a runner tool). Overlaid files win 8.3
    /// collisions and are never written to the host folder.
    pub fn mount_hdd_folder_with(
        &mut self,
        dir: &std::path::Path,
        overrides: Vec<(String, Vec<u8>)>,
    ) -> std::io::Result<()> {
        // The system payload comes from the committed image; HELLO.TXT is the
        // static demo file, dropped here so the host folder supplies the user files.
        let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
        let mut system_files: Vec<(String, Vec<u8>)> = payload
            .files
            .into_iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("HELLO.TXT"))
            .collect();
        apply_overrides(&mut system_files, overrides);

        // The recursive tree volume walks `dir` (metadata only) overlaying the
        // system files at the root, and serves FAT/dir sectors on demand + file
        // data lazily. The boot sectors carry the dynamic geometry derived from
        // the folder.
        let volume =
            katea_tree::KateaTreeVolume::new(&payload.mbr, &payload.vbr, dir, &system_files)?;
        self.bmide.reset_primary();
        self.ata = Some(ata::AtaDisk::from_host_folder(volume));
        let _ = self.publish_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
        Ok(())
    }

    /// Mount a host folder in user-folder mode and add in-memory system
    /// overrides. System binaries such as `GLIDE2X.OVL` land in `C:\DOS`. Host
    /// files remain visible in their own directories, so DOS's normal
    /// current-directory-before-PATH lookup keeps a game-local file first.
    pub fn mount_hdd_folder_with_user_overrides(
        &mut self,
        dir: &std::path::Path,
        overrides: Vec<(String, Vec<u8>)>,
    ) -> std::io::Result<()> {
        let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
        // Seed the user-owned config from the payload we already hold (parse the
        // image once), before `user_folder_overlay` below consumes `payload.files`.
        ensure_user_config(
            dir,
            &payload_file(&payload, "CONFIG.SYS"),
            &payload_file(&payload, "AUTOEXEC.BAT"),
        )?;
        let mut system_files = user_folder_overlay(payload.files);
        apply_overrides(&mut system_files, overrides);
        let volume =
            katea_tree::KateaTreeVolume::new(&payload.mbr, &payload.vbr, dir, &system_files)?;
        self.bmide.reset_primary();
        self.ata = Some(ata::AtaDisk::from_host_folder(volume));
        self.katea_root = Some(dir.to_path_buf());
        let _ = self.publish_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
        Ok(())
    }

    /// Mount a host folder as C: through Katea in user-folder mode. The GUI and
    /// `--hdd-folder` use this when there are no extra global fallback files.
    pub fn mount_hdd_folder(&mut self, dir: &std::path::Path) -> std::io::Result<()> {
        self.mount_hdd_folder_with_user_overrides(dir, Vec::new())
    }

    /// Mount a synthesized FAT32 volume as drive C: for the DOS absolute-disk
    /// interface. INT 25h reads its sectors; INT 26h writes are write-protected
    /// (the volume is read-only). Build one with `build_fat32`.
    pub fn mount_fat32(&mut self, volume: Fat32Volume) {
        self.fat32_c = Some(volume);
    }

    /// Eject the hard disk, returning its current image bytes (including any
    /// in-session writes) so the caller can flush them back. None when no disk is
    /// mounted OR when the disk is a read-only host-folder facade (which has no
    /// flushable image — returning its empty `bytes()` would persist a 0-byte file).
    /// Clears the BDA fixed-disk count.
    pub fn eject_hdd(&mut self) -> Option<Vec<u8>> {
        self.bmide.reset_primary();
        if let Some(disk) = self.ata.as_mut() {
            disk.reconcile_host_folder(); // final pass for a host folder; no-op for images
        }
        let bytes = self
            .ata
            .take()
            .filter(ata::AtaDisk::is_image)
            .map(|d| d.bytes().to_vec());
        let _ = self.clear_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 0);
        bytes
    }

    /// Force a final reconcile of a mounted Katea host folder, materializing any
    /// overlay-held files to the host. Safe to call when no folder is mounted (a
    /// no-op for an image-backed disk or no disk). Used by the headless `--hdd-
    /// folder` path and the e2e test before reading the host folder back.
    pub fn flush_hdd_folder(&mut self) {
        if let Some(disk) = self.ata.as_mut() {
            disk.reconcile_host_folder();
        }
    }

    /// Whether the mounted hard disk took a guest write this session, so the host
    /// flushes the image back only when it changed. False when no disk is mounted.
    pub fn hdd_dirty(&self) -> bool {
        self.ata.as_ref().is_some_and(|d| d.dirty)
    }

    fn publish_fixed_disk_parameter_table(&mut self) -> Result<(), BusError> {
        let Some(disk) = self.ata.as_ref() else {
            return self.clear_fixed_disk_parameter_table();
        };
        let cylinders = disk.cylinders().min(u32::from(u16::MAX)) as u16;
        let heads = disk.heads().min(u32::from(u8::MAX)) as u8;
        let spt = disk.sectors_per_track().min(u32::from(u8::MAX)) as u8;
        let base = BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize;
        self.memory.write_u16(base, cylinders)?;
        self.memory.write_u8(base + 2, heads)?;
        self.memory.write_u16(base + 3, 0)?; // reduced write current, XT only
        self.memory.write_u16(base + 5, 0)?; // write precompensation
        self.memory.write_u8(base + 7, 0)?; // ECC burst length, XT only
        self.memory
            .write_u8(base + 8, if heads > 8 { 0x08 } else { 0x00 })?;
        self.memory.write_u8(base + 9, 0)?; // standard timeout, XT only
        self.memory.write_u8(base + 10, 0)?; // formatting timeout, XT only
        self.memory.write_u8(base + 11, 0)?; // drive-check timeout, XT only
        self.memory.write_u16(base + 12, cylinders)?;
        self.memory.write_u8(base + 14, spt)?;
        self.memory.write_u8(base + 15, 0)?;
        self.memory.write_u16(
            0x41 * 4,
            (BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR & 0x0F) as u16,
        )?;
        self.memory.write_u16(
            0x41 * 4 + 2,
            (BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR >> 4) as u16,
        )?;
        Ok(())
    }

    fn clear_fixed_disk_parameter_table(&mut self) -> Result<(), BusError> {
        for i in 0..16 {
            self.memory
                .write_u8(BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize + i, 0)?;
        }
        self.memory.write_u16(0x41 * 4, 0)?;
        self.memory.write_u16(0x41 * 4 + 2, 0)
    }

    /// Whether a disc is currently mounted in the ATAPI drive.
    pub fn cd_loaded(&self) -> bool {
        self.ide.device().is_loaded()
    }

    pub(super) fn icdex_cd_drive_number(&self) -> Option<u8> {
        // The ATAPI CD-ROM sits at a fixed DOS drive letter (D: = 3). With the
        // Rust DOS kernel retired there are no CONFIG.SYS block-device drivers
        // that could shift it, so the CD drive is always the first loaded block
        // drive. "No disc" is still a present drive, so this reports the letter
        // unconditionally (the install check keys off the ATAPI channel existing).
        Some(CD_DRIVE_NUMBER)
    }

    pub(super) fn icdex_drive_matches(&self, drive: u16) -> bool {
        self.icdex_cd_drive_number()
            .is_some_and(|cd_drive| drive == u16::from(cd_drive))
    }

    pub(super) fn icdex_es_bx(&self) -> u32 {
        self.cpu.registers.segment(SegmentIndex::Es).base + (self.cpu.registers.ebx() as u16) as u32
    }

    pub(super) fn icdex_fail(&mut self, code: u16) {
        self.set_ax(code);
        self.set_int_frame_carry(true);
    }

    pub(super) fn icdex_iso_dir_record(&self, path: &str) -> Result<Vec<u8>, u16> {
        let image = self.ide.device().image().ok_or(0x0015u16)?;
        let pvd = image.read_data_sector(16).ok_or(0x0015u16)?;
        if pvd[0] != 0x01 || &pvd[1..6] != b"CD001" {
            return Err(0x0015);
        }
        let root_len = usize::from(pvd[156]);
        if root_len == 0 || 156 + root_len > pvd.len() {
            return Err(0x0015);
        }
        let mut record = pvd[156..156 + root_len].to_vec();
        let mut normalized = path.replace('/', "\\");
        if normalized.as_bytes().get(1) == Some(&b':') {
            normalized.drain(..2);
        }
        for component in normalized.split('\\').filter(|part| !part.is_empty()) {
            if record.get(25).copied().unwrap_or(0) & 0x02 == 0 {
                return Err(0x0002);
            }
            record = icdex_iso_child_record(image, &record, component).ok_or(0x0002u16)?;
        }
        Ok(record)
    }

    pub(super) fn write_icdex_canonical_dir_record(&mut self, dst: u32, record: &[u8]) {
        let mut out = [0u8; 285];
        if record.len() < 34 {
            self.write_guest_block(dst, &out);
            return;
        }
        let lba = u32::from_le_bytes(record[2..6].try_into().unwrap());
        let len = u32::from_le_bytes(record[10..14].try_into().unwrap());
        let blocks = len
            .div_ceil(cdimage::DATA_SECTOR as u32)
            .min(u32::from(u16::MAX)) as u16;
        out[0] = record[1];
        out[1..5].copy_from_slice(&lba.to_le_bytes());
        out[5..7].copy_from_slice(&blocks.to_le_bytes());
        out[7..11].copy_from_slice(&len.to_le_bytes());
        out[0x0b..0x12].copy_from_slice(&record[18..25]);
        out[0x12] = record[25];
        out[0x13] = record[26];
        out[0x14] = record[27];
        out[0x15..0x17].copy_from_slice(&record[28..30]);
        let (name, version) = icdex_iso_name_and_version(record);
        let name_len = name.len().min(37);
        out[0x17] = name_len as u8;
        out[0x18..0x18 + name_len].copy_from_slice(&name[..name_len]);
        out[0x3e..0x40].copy_from_slice(&version.to_le_bytes());
        let name_record_len = usize::from(record[32]);
        let sys_start = 33 + name_record_len + usize::from(name_record_len % 2 == 0);
        if sys_start < record.len() {
            let sys = &record[sys_start..];
            let sys_len = sys.len().min(220);
            out[0x40] = sys_len as u8;
            out[0x41..0x41 + sys_len].copy_from_slice(&sys[..sys_len]);
        }
        self.write_guest_block(dst, &out);
    }

    /// Execute one CD-ROM device driver request whose header begins at linear
    /// `header`. Decodes the command code and the per-command fields (see RBIL
    /// table 02597) and drives the ATAPI device, writing data back to the
    /// transfer address and the status word back into the header. Supports the
    /// CD commands a game uses: READ LONG (0x80), SEEK (0x83), PLAY AUDIO (0x84),
    /// STOP (0x85), RESUME (0x88), and IOCTL INPUT (0x03) device-status queries.
    pub(super) fn icdex_device_request(&mut self, header: u32) {
        let command = self.read_physical_u8(header + 2);
        // Status word at offset 3: bit 8 = done, bit 15 = error, low byte = code.
        let mut status: u16 = 0x0100; // done
        match command {
            // READ LONG: read `count` sectors starting at the given sector into
            // the transfer address. Addressing mode 0 = HSG (LBA), 1 = Red Book.
            0x80 => {
                let addr_mode = self.read_physical_u8(header + 0x0D);
                let xfer = self.read_guest_dword(header + 0x0E);
                let count = self.read_guest_word(header + 0x12);
                let start = self.read_guest_dword(header + 0x14);
                let lba = self.driver_addr_to_lba(addr_mode, start);
                let mut ok = true;
                for i in 0..u32::from(count) {
                    match self
                        .ide
                        .device()
                        .image()
                        .and_then(|img| img.read_data_sector(lba + i))
                    {
                        Some(sector) => {
                            self.write_guest_block(
                                xfer.wrapping_add(i * cdimage::DATA_SECTOR as u32),
                                &sector,
                            );
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                self.cd_accesses += 1;
                if !ok {
                    status = 0x8000 | 0x0100 | 0x000F; // error + done, sector not found
                }
            }
            // SEEK: advisory; accept it (the timing model does not need it).
            0x83 => {}
            // PLAY AUDIO: start playback at the given sector for `count` sectors.
            0x84 => {
                let addr_mode = self.read_physical_u8(header + 0x0D);
                let start = self.read_guest_dword(header + 0x0E);
                let count = self.read_guest_dword(header + 0x12);
                let lba = self.driver_addr_to_lba(addr_mode, start);
                let mut cdb = [0u8; 12];
                cdb[0] = 0x45; // PLAY AUDIO(10)
                cdb[2..6].copy_from_slice(&lba.to_be_bytes());
                let frames = count.min(u32::from(u16::MAX)) as u16;
                cdb[7..9].copy_from_slice(&frames.to_be_bytes());
                if matches!(self.ide.device_mut().execute(&cdb), atapi::CmdResult::Error) {
                    status = 0x8000 | 0x0100 | 0x000F;
                }
            }
            // STOP AUDIO.
            0x85 => {
                let mut cdb = [0u8; 12];
                cdb[0] = 0x4E;
                let _ = self.ide.device_mut().execute(&cdb);
            }
            // RESUME AUDIO.
            0x88 => {
                let mut cdb = [0u8; 12];
                cdb[0] = 0x4B;
                cdb[8] = 0x01; // resume bit
                let _ = self.ide.device_mut().execute(&cdb);
            }
            // IOCTL INPUT and any other command: report done with no data. A
            // real driver answers control-block queries here; a game that only
            // needs the data/audio path tolerates a benign success.
            _ => {}
        }
        // Write the status word back into the header (offset 3).
        let _ = self.memory.write_u16(header as usize + 3, status);
    }

    /// Convert a CD device-driver address (HSG LBA when `addr_mode` == 0, packed
    /// Red Book frame/second/minute when 1) to a logical LBA.
    fn driver_addr_to_lba(&self, addr_mode: u8, raw: u32) -> u32 {
        if addr_mode == 0 {
            raw // HSG = logical sector number = LBA
        } else {
            // Red Book packed as frame/second/minute/unused in the low bytes.
            let frame = raw as u8;
            let second = (raw >> 8) as u8;
            let minute = (raw >> 16) as u8;
            cdimage::msf_to_lba(minute, second, frame)
        }
    }

    /// Service the host side of an `INT 13h` disk request. Only floppy A: (DL=0)
    /// is backed, by the mounted image. CHS to LBA uses the mounted media
    /// geometry, so a 720 KB disk reads with 9 sectors per track and a 1.44 MB
    /// disk with 18. Status is returned through AH and the carry flag the way a
    /// real BIOS reports it: CF clear and AH=0 on success, CF set with an error
    /// code in AH on failure.
    pub(super) fn handle_int13(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let dx = self.cpu.registers.edx() as u16;
        let dl = dx as u8;

        // Fixed-disk path: DL bit 7 selects a hard drive (0x80 = C:). Serviced
        // before the floppy early-return so a guest with no floppy but a mounted
        // hard disk still boots. EDD AH=41h-48h dispatch here too. Only taken when
        // a hard disk is actually mounted: with no disk the call falls through to
        // the no-op return below, which the firmware boot suite relies on (it
        // places its second stage in memory directly and issues INT 13h AH=02 with
        // DL=0x80 and carry pre-cleared, expecting a no-op success).
        if dl >= 0x80 && self.ata.is_some() {
            self.int13_hdd(ah, dl);
            return;
        }

        match ah {
            // AH=16h detect disk change, AH=17h set disk type for format, and
            // AH=18h set media type for format are meaningful even as probe calls.
            0x16 => {
                self.int13_floppy_change_status(dl);
                return;
            }
            0x17 => {
                self.int13_floppy_set_disk_type_for_format(dl);
                return;
            }
            0x18 => {
                self.int13_floppy_set_media_type_for_format(dl);
                return;
            }
            _ => {}
        }

        // With no floppy image mounted there is no drive to service. Leave the
        // registers and the IRET FLAGS image untouched so the guest sees the same
        // result the bare IRET stub gave before this handler existed.
        if self.floppy.is_none() {
            return;
        }

        match ah {
            // AH=00 reset disk system: the heads recalibrate back to track 0,
            // which steps the drive and takes time.
            0x00 => {
                let ticks = self
                    .floppy
                    .as_mut()
                    .map_or(0, |f| f.access_duration_ticks(0, 0));
                self.stall_for_master_ticks(ticks);
                self.set_eax_ah(0x00);
                self.set_disk_status(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=01 get last disk status. The documented result register is AH; PS/2
            // BIOSes mirror the status into AL as well. CF reflects a nonzero (error)
            // status. The status byte itself lives in BDA 0040:0041.
            0x01 => {
                let status = self.read_physical_u8(0x441);
                self.set_eax_ah(status);
                self.set_eax_al(status);
                self.set_int_frame_carry(status != 0);
            }
            // AH=02 read sectors, AH=03 write sectors. AL = sector count, CH/CL
            // carry the cylinder and sector (CL bits 0-5 sector, bits 6-7 the
            // cylinder high bits), DH = head, DL = drive, ES:BX = buffer.
            0x02 | 0x03 => self.int13_transfer(ah, dl),
            // AH=04 verify sectors: read without copying, report sectors checked.
            0x04 => self.int13_verify(dl),
            // AH=05 format track: fill the addressed track with the format filler.
            0x05 => self.int13_format_track(dl),
            // AH=08 read drive parameters. Report the mounted media geometry.
            0x08 => self.int13_drive_parameters(dl),
            // AH=15 get DASD type for a floppy. A mounted floppy reports AH=01
            // (no change-line), an absent floppy AH=00 (no such drive). DL>=0x80
            // never reaches here: the fixed-disk path handled it above.
            0x15 => {
                let mounted = dl == 0x00 && self.floppy.is_some();
                self.set_eax_ah(if mounted { 0x01 } else { 0x00 });
                self.set_disk_status(0x00);
                self.set_int_frame_carry(false);
            }
            // Genuinely unknown subfunctions report invalid-function, the way a
            // real BIOS does, instead of a false success.
            _ => {
                self.set_eax_ah(0x01);
                self.set_disk_status(0x01);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// Record the INT 13h result in BDA 0040:0041 (last disk status) so AH=01h can
    /// report it. 0x00 is success; any other value is the error code.
    fn set_disk_status(&mut self, status: u8) {
        let _ = self.memory.write_u8(0x441, status);
    }

    /// AH=04h verify: confirm the requested sectors are readable without copying
    /// them into the caller buffer. AL returns the count verified.
    fn int13_verify(&mut self, dl: u8) {
        if dl != 0x00 || self.floppy.is_none() {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let ax = self.cpu.registers.eax() as u16;
        let count = ax as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sector = cl & 0x3f;
        let cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let head = (self.cpu.registers.edx() as u16 >> 8) as u8;
        let mut done = 0u8;
        for i in 0..count {
            let present = self
                .floppy
                .as_ref()
                .and_then(|f| f.read_sector(cyl, head, sector + i))
                .is_some();
            if !present {
                break;
            }
            done += 1;
        }
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=05h format track. AL = sectors per track to format, CH = cylinder, DH =
    /// head, ES:BX = a list of 4-byte address-field records (C,H,R,N). Only floppy
    /// A: is backed; the records describe the standard sequential layout this drive
    /// already uses, so the cylinder/head address is taken from CH/DH and every
    /// sector of that track is filled with the DOS format filler 0xF6. Limit:
    /// the address-field records are not parsed for nonstandard interleave or sector
    /// sizes; the in-memory image is a fixed-geometry linear array, so a track is
    /// formatted by zero-fill of its sectors at the mounted geometry.
    fn int13_format_track(&mut self, dl: u8) {
        // No fixed-disk path: any hard-disk unit reports no such drive.
        if dl >= 0x80 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        // Only floppy A: is backed.
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let al = self.cpu.registers.eax() as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let ch = (cx >> 8) as u8;
        let cl = cx as u8;
        let cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let head = (self.cpu.registers.edx() as u16 >> 8) as u8;
        // A track off the mounted media, or a sector count past the media's
        // sectors-per-track, is a bad-track request (AH=0Ch).
        if cyl >= geom.cylinders || head >= geom.heads || al > geom.sectors {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
            return;
        }
        self.floppy_accesses += 1;
        let ok = self
            .floppy
            .as_mut()
            .map(|f| f.format_track(cyl, head, 0xf6))
            .unwrap_or(false);
        // Charge the seek to the formatted track plus a full-track write.
        let bytes = usize::from(geom.sectors) * 512;
        let ticks = self
            .floppy
            .as_mut()
            .map_or(0, |f| f.access_duration_ticks(cyl, bytes));
        self.stall_for_master_ticks(ticks);
        if ok {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
        }
    }

    /// Carry out the AH=02 read / AH=03 write half of INT 13h.
    fn int13_transfer(&mut self, ah: u8, dl: u8) {
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            // No media backs the request: report a timeout the way an empty
            // drive would.
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        // Only floppy A: is backed.
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let _ = geom;

        let ax = self.cpu.registers.eax() as u16;
        let count = ax as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sector = cl & 0x3f;
        let cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let head = (self.cpu.registers.edx() as u16 >> 8) as u8;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bx = self.cpu.registers.ebx() as u16;
        let buffer = es.wrapping_add(u32::from(bx));

        // The drive is being serviced: flash the GUI access LED.
        self.floppy_accesses += 1;

        let mut done: u8 = 0;
        for i in 0..count {
            // Multi-sector transfers advance within the current track only. A
            // booter that crosses a track boundary in one call would need
            // cross-track wrap added here.
            let sec = sector + i;
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            if ah == 0x02 {
                let data = self
                    .floppy
                    .as_ref()
                    .and_then(|f| f.read_sector(cyl, head, sec))
                    .map(<[u8]>::to_vec);
                match data {
                    Some(bytes) => self.write_guest_block(addr, &bytes),
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, 512);
                let wrote = self
                    .floppy
                    .as_mut()
                    .map(|f| f.write_sector(cyl, head, sec, &bytes))
                    .unwrap_or(false);
                if !wrote {
                    break;
                }
            }
            done += 1;
        }

        // Charge the drive's mechanical time for the access: seek from the head's
        // tracked position, rotational latency, and the transfer of the sectors
        // moved. This is what makes a load take wall-clock time (see stall_clocks)
        // instead of completing instantly.
        if done > 0 {
            let bytes = usize::from(done) * 512;
            let ticks = self
                .floppy
                .as_mut()
                .map_or(0, |f| f.access_duration_ticks(cyl, bytes));
            self.stall_for_master_ticks(ticks);
        }

        // The ROM BIOS boot path reads A: CHS 0/0/1 to 0000:7C00 with INT 13h,
        // then far-jumps there. Once that sector is in place, a self-booting disk
        // owns DOS-family interrupts through its IVT handlers. Host-side Toka-DOS
        // and IZEMM must stand down before the sector's first INT 21h/2Fh/66h.
        if ah == 0x02
            && done > 0
            && cyl == 0
            && head == 0
            && sector == 1
            && buffer == BOOT_SECTOR_ADDRESS as u32
        {
            self.booter_inert = true;
        }

        // AL returns the number of sectors actually transferred.
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            // Sector not found / read error.
            self.set_eax_ah(0x04);
            self.set_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// Carry out the AH=08 read-drive-parameters half of INT 13h.
    fn int13_drive_parameters(&mut self, dl: u8) {
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let max_cyl = geom.cylinders.saturating_sub(1);
        // CL: sectors per track in bits 0-5, cylinder high bits in 6-7.
        let cl = (geom.sectors & 0x3f) | (((max_cyl >> 8) as u8 & 0x03) << 6);
        let ch = (max_cyl & 0xff) as u8;
        let cx = (u16::from(ch) << 8) | u16::from(cl);
        let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cx);
        self.cpu.registers.set_ecx(ecx);
        // DH = max head index, DL = number of floppy drives, read from the
        // equipment word so it tracks the mounted drives rather than a fixed 1.
        let dx = (u16::from(geom.heads.saturating_sub(1)) << 8) | u16::from(self.floppy_count());
        let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(dx);
        self.cpu.registers.set_edx(edx);
        // BL = drive type (0x03 = 720 KB, 0x04 = 1.44 MB).
        let ebx = (self.cpu.registers.ebx() & !0xFF) | u32::from(geom.drive_type);
        self.cpu.registers.set_ebx(ebx);
        self.set_eax_ah(0x00);
        self.set_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// AH=16h detect disk change. Izarra's in-memory drive has no change line, so
    /// a mounted A: reports unchanged. An absent or non-wired drive reports not
    /// ready.
    fn int13_floppy_change_status(&mut self, dl: u8) {
        if dl == 0x00 && self.floppy.is_some() {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=17h set disk type for format. The mounted image fixes the media
    /// geometry, so this validates that the requested format class matches it.
    fn int13_floppy_set_disk_type_for_format(&mut self, dl: u8) {
        let al = self.cpu.registers.eax() as u8;
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }

        let supported = match al {
            0x01 | 0x02 => geom.cylinders == 40 && geom.sectors <= 9,
            0x03 => geom.cylinders == 80 && geom.sectors == 15,
            0x04 => geom.cylinders == 80 && geom.sectors == 9,
            _ => false,
        };
        if supported {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=18h set media type for format. Returns ES:DI pointing at the resident
    /// diskette parameter table when the requested cylinder and sector geometry
    /// matches the mounted image.
    fn int13_floppy_set_media_type_for_format(&mut self, dl: u8) {
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        if dl != 0x00 {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
            return;
        }

        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let requested_max_cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let requested_sectors = cl & 0x3f;
        if requested_max_cyl != geom.cylinders.saturating_sub(1)
            || requested_sectors != geom.sectors
        {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
            return;
        }

        let table = BIOS_DISKETTE_PARAMETER_TABLE_ADDR;
        let seg = (table >> 4) as u16;
        let off = (table & 0x0f) as u16;
        self.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
        let edi = (self.cpu.registers.edi() & !0xFFFF) | u32::from(off);
        self.cpu.registers.set_edi(edi);
        self.set_eax_ah(0x00);
        self.set_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// INT 13h fixed-disk path (DL>=0x80). Only the first drive (0x80 = C:) is
    /// backed; any other unit reports no-such-drive. Status follows the AT BIOS
    /// convention: AH = result code (0 success), CF set on error. EDD AH=41h-48h
    /// extends this for LBA access.
    fn int13_hdd(&mut self, ah: u8, dl: u8) {
        // Only unit 0x80 is wired. A higher unit, or no mounted disk, reports
        // invalid-drive (AH=0x01) with carry set, the way a BIOS does for an
        // absent fixed disk. The EDD install check (AH=41h) on an absent drive
        // also lands here through the default arm.
        if dl != 0x80 || self.ata.is_none() {
            self.int13_hdd_error(0x01);
            return;
        }
        match ah {
            // AH=00 reset disk system: a no-op success on this model (no real
            // recalibrate cost is charged for the hard disk).
            0x00 => self.int13_hdd_ok(),
            // AH=01 get last status from BDA 0040:0074 (the fixed-disk status byte).
            0x01 => {
                let status = self.read_physical_u8(0x474);
                self.set_eax_ah(status);
                self.set_int_frame_carry(status != 0);
            }
            // AH=02 read, AH=03 write sectors via CHS.
            0x02 | 0x03 => self.int13_hdd_transfer(ah),
            // AH=04 verify: confirm the run is in range without copying.
            0x04 => self.int13_hdd_verify(),
            // AH=06/07 and AH=1A are controller-level format calls. The fixed
            // disk image has no low-level format or defect table state.
            0x06 | 0x07 | 0x1A => self.int13_hdd_error(0x01),
            // AH=08 get drive parameters: CHS geometry packed into CX/DH, fixed-
            // disk count in DL.
            0x08 => self.int13_hdd_parameters(),
            // AH=09 init drive pair, AH=0C seek, AH=0D alternate reset, AH=11
            // recalibrate, AH=19 park heads: all succeed with no data movement.
            0x09 | 0x0C | 0x0D | 0x11 | 0x19 => self.int13_hdd_ok(),
            // AH=0A/0B read/write long: transfer 512 data bytes plus synthetic
            // ECC bytes per sector.
            0x0A | 0x0B => self.int13_hdd_long_transfer(ah),
            // AH=0E/0F access an XT controller sector buffer, which this ATA model
            // does not expose.
            0x0E | 0x0F => self.int13_hdd_error(0x01),
            // AH=10 test drive ready, AH=14 controller diagnostic: ready/OK.
            0x10 | 0x14 => self.int13_hdd_ok(),
            // AH=12 controller RAM diagnostic, AH=13 drive diagnostic.
            0x12 | 0x13 => self.int13_hdd_diagnostic_ok(),
            // AH=15 get DASD type: AH=03 (fixed disk), and the total sector count
            // in CX:DX.
            0x15 => self.int13_hdd_dasd(),
            // EDD install check.
            0x41 => self.int13_edd_install_check(),
            // EDD extended read, write, and verify via a Disk Address Packet at
            // DS:SI.
            0x42..=0x44 => self.int13_edd_transfer(ah),
            // EDD get extended drive parameters into a result buffer at DS:SI.
            0x48 => self.int13_edd_drive_params(),
            // Genuinely unknown subfunctions report invalid-function.
            _ => self.int13_hdd_error(0x01),
        }
    }

    /// Record the fixed-disk INT 13h result in BDA 0040:0074 so AH=01h can report
    /// it. Floppies use 0040:0041; the hard disk has its own status byte.
    fn set_fixed_disk_status(&mut self, status: u8) {
        let _ = self.memory.write_u8(0x474, status);
    }

    /// Common success for a fixed-disk control call: AH=0, status byte 0, CF clear.
    fn int13_hdd_ok(&mut self) {
        self.set_eax_ah(0x00);
        self.set_fixed_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// Common failure for a fixed-disk call.
    fn int13_hdd_error(&mut self, status: u8) {
        self.set_eax_ah(status);
        self.set_fixed_disk_status(status);
        self.set_int_frame_carry(true);
    }

    /// AH=12h/13h diagnostics return AL=0 when the controller or drive test
    /// completes successfully.
    fn int13_hdd_diagnostic_ok(&mut self) {
        self.set_eax_al(0x00);
        self.int13_hdd_ok();
    }

    /// Read the CHS address out of the INT 13h register layout: CH = cylinder low
    /// 8 bits, CL bits 6-7 = cylinder high 2 bits, CL bits 0-5 = sector (1-based),
    /// DH = head.
    fn int13_chs(&self) -> (u32, u32, u32) {
        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sector = u32::from(cl & 0x3f);
        let cyl = u32::from(ch) | (u32::from(cl & 0xc0) << 2);
        let head = u32::from((self.cpu.registers.edx() as u16 >> 8) as u8);
        (cyl, head, sector)
    }

    /// Advance the shared machine clock for sectors moved directly by an INT
    /// 13h fixed-disk service. Port-driven ATA commands schedule this deadline
    /// themselves, so only the BIOS path calls this helper.
    fn stall_for_hdd_sectors(&mut self, sectors: u32) {
        self.stall_for_master_ticks(ata::pio_transfer_ticks(sectors));
    }

    /// AH=02/03 CHS read/write against the mounted hard disk. ES:BX is the buffer;
    /// AL is the sector count. AL returns the count actually moved.
    fn int13_hdd_transfer(&mut self, ah: u8) {
        let count = self.cpu.registers.eax() as u8;
        let (cyl, head, sector) = self.int13_chs();
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bx = self.cpu.registers.ebx() as u16;
        let buffer = es.wrapping_add(u32::from(bx));

        let Some(start_lba) = self
            .ata
            .as_ref()
            .and_then(|d| d.chs_to_lba(cyl, head, sector))
        else {
            // Address off the disk: sector-not-found (AH=0x04), CF set.
            self.set_eax_al(0);
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
            return;
        };
        self.c_accesses += 1;
        let mut done: u8 = 0;
        for i in 0..count {
            let lba = start_lba + u32::from(i);
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            if ah == 0x02 {
                let data = self.ata.as_ref().and_then(|d| d.read_lba(lba));
                match data {
                    Some(bytes) => self.write_guest_block(addr, &bytes),
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, 512);
                let wrote = self
                    .ata
                    .as_mut()
                    .map(|d| d.write_lba(lba, &bytes))
                    .unwrap_or(false);
                if !wrote {
                    break;
                }
            }
            done += 1;
        }
        // A CHS read of LBA 0 (the MBR) to 0000:7C00 is a fixed-disk boot. Mirror
        // the INT 19h ATA branch: only a sector carrying the 55AA boot signature is
        // a real OS, so stand the HLE Toka-DOS and IZEMM down (the booted OS then
        // owns the DOS interrupts via its IVT). Without the signature the boot ROM
        // falls back to the HLE C: shim, which needs the HLE live, so leave it set.
        // Unlike the floppy, INT 13h stays intercepted so Katea keeps serving I/O.
        if ah == 0x02 && done > 0 && start_lba == 0 && buffer == BOOT_SECTOR_ADDRESS as u32 {
            let signed = self.read_physical_u8(buffer + 510) == 0x55
                && self.read_physical_u8(buffer + 511) == 0xAA;
            if signed {
                self.booter_inert = true;
            }
        }
        self.stall_for_hdd_sectors(u32::from(done));
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=0Ah/0Bh read/write long. A classic BIOS moves sector data followed by
    /// controller ECC bytes. The ATA image stores only 512-byte sectors, so reads
    /// append four zero ECC bytes and writes ignore the caller's ECC trailer.
    fn int13_hdd_long_transfer(&mut self, ah: u8) {
        const LONG_SECTOR_BYTES: u32 = ata::SECTOR as u32 + 4;
        const ZERO_ECC: [u8; 4] = [0; 4];

        let count = self.cpu.registers.eax() as u8;
        let (cyl, head, sector) = self.int13_chs();
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bx = self.cpu.registers.ebx() as u16;
        let buffer = es.wrapping_add(u32::from(bx));

        let Some(start_lba) = self
            .ata
            .as_ref()
            .and_then(|d| d.chs_to_lba(cyl, head, sector))
        else {
            self.set_eax_al(0);
            self.int13_hdd_error(0x04);
            return;
        };

        self.c_accesses += 1;
        let mut done: u8 = 0;
        for i in 0..count {
            let lba = start_lba + u32::from(i);
            let addr = buffer.wrapping_add(u32::from(i) * LONG_SECTOR_BYTES);
            if ah == 0x0A {
                let data = self.ata.as_ref().and_then(|d| d.read_lba(lba));
                match data {
                    Some(bytes) => {
                        self.write_guest_block(addr, &bytes);
                        self.write_guest_block(addr.wrapping_add(ata::SECTOR as u32), &ZERO_ECC);
                    }
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, ata::SECTOR);
                let wrote = self
                    .ata
                    .as_mut()
                    .map(|d| d.write_lba(lba, &bytes))
                    .unwrap_or(false);
                if !wrote {
                    break;
                }
            }
            done += 1;
        }

        self.stall_for_hdd_sectors(u32::from(done));
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.int13_hdd_error(0x04);
        }
    }

    /// AH=04 verify: confirm the run is readable without copying it. AL returns
    /// the count verified.
    fn int13_hdd_verify(&mut self) {
        let count = self.cpu.registers.eax() as u8;
        let (cyl, head, sector) = self.int13_chs();
        let Some(start_lba) = self
            .ata
            .as_ref()
            .and_then(|d| d.chs_to_lba(cyl, head, sector))
        else {
            self.set_eax_al(0);
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
            return;
        };
        self.c_accesses += 1;
        let mut done: u8 = 0;
        for i in 0..count {
            let readable = self
                .ata
                .as_ref()
                .and_then(|d| d.read_lba(start_lba + u32::from(i)))
                .is_some();
            if !readable {
                break;
            }
            done += 1;
        }
        self.stall_for_hdd_sectors(u32::from(done));
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=08 get drive parameters. CH = cylinder low 8 bits, CL bits 6-7 =
    /// cylinder high 2 bits, CL bits 0-5 = max sector; DH = max head index; DL =
    /// number of fixed disks; BL = drive type. The reported maximum cylinder is
    /// the count minus one, the way a BIOS hands back the last valid index.
    fn int13_hdd_parameters(&mut self) {
        let Some(disk) = self.ata.as_ref() else {
            self.set_eax_ah(0x01);
            self.set_int_frame_carry(true);
            return;
        };
        // BIOS caps the reported cylinders at 1024 (the 10-bit CHS field), so a
        // large disk's geometry stays addressable through the legacy path even
        // though the full capacity needs LBA.
        let max_cyl = disk.cylinders().min(1024).saturating_sub(1) as u16;
        let max_head = (disk.heads().saturating_sub(1)) as u8;
        let sectors = disk.sectors_per_track() as u8;
        let cl = (sectors & 0x3f) | (((max_cyl >> 8) as u8 & 0x03) << 6);
        let ch = (max_cyl & 0xff) as u8;
        let cx = (u16::from(ch) << 8) | u16::from(cl);
        let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cx);
        self.cpu.registers.set_ecx(ecx);
        // DH = max head index, DL = number of fixed disks (1). BL drive type 0 for
        // a fixed disk (the type byte is floppy-only; hard disks report 0).
        let dx = (u16::from(max_head) << 8) | 0x0001;
        let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(dx);
        self.cpu.registers.set_edx(edx);
        let ebx = self.cpu.registers.ebx() & !0xFF;
        self.cpu.registers.set_ebx(ebx);
        self.set_eax_ah(0x00);
        self.set_fixed_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// AH=15 get DASD type: AH=03 marks a fixed disk, and CX:DX carries the total
    /// sector count (CX high word, DX low word). CF clear.
    fn int13_hdd_dasd(&mut self) {
        let total = self.ata.as_ref().map_or(0, |d| d.total_sectors());
        let cx = (total >> 16) as u16;
        let dx = total as u16;
        self.set_cx(cx);
        self.set_dx(dx);
        self.set_eax_ah(0x03); // fixed disk present
        self.set_int_frame_carry(false);
    }

    /// EDD AH=41h install check. On a present drive: carry clear, BX=0xAA55, AH=
    /// 0x30 (EDD version 3.0), CX bit 0 set (extended disk access supported). The
    /// 0xAA55 in BX is the magic a caller checks to confirm the extensions exist.
    fn int13_edd_install_check(&mut self) {
        self.set_bx(0xAA55);
        self.set_eax_ah(0x30); // version 3.0
        self.set_cx(0x0001); // extended disk access support
        self.set_int_frame_carry(false);
    }

    /// EDD AH=42h/43h/44h extended read, write, and verify. The Disk Address
    /// Packet at DS:SI holds the block count and the 64-bit starting LBA; reads
    /// and writes use the seg:off transfer buffer inside the packet. Only the low
    /// 32 bits of the LBA are honored. Limit: the 64-bit-flat-buffer form (DAP
    /// bytes 16-23 when the seg:off is 0xFFFF:0xFFFF) is not decoded; lift by
    /// reading the wide pointer.
    fn int13_edd_transfer(&mut self, ah: u8) {
        let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
        let si = self.cpu.registers.esi() as u16;
        let dap = ds.wrapping_add(u32::from(si));
        let packet = self.read_guest_block(dap, 16);
        // Byte 0 = packet size (16 or 24). Byte 2 = block count. Bytes 4-7 = the
        // transfer buffer as offset (4-5) then segment (6-7). Bytes 8-15 = the
        // starting LBA, little-endian.
        let count = u16::from_le_bytes([packet[2], packet[3]]);
        let buf_off = u16::from_le_bytes([packet[4], packet[5]]);
        let buf_seg = u16::from_le_bytes([packet[6], packet[7]]);
        let lba = u32::from_le_bytes([packet[8], packet[9], packet[10], packet[11]]);
        let buffer = (u32::from(buf_seg) << 4).wrapping_add(u32::from(buf_off));

        let total = self.ata.as_ref().map_or(0, |d| d.total_sectors());
        if lba.saturating_add(u32::from(count)) > total {
            // Out of range: AH=0x04 (sector not found), CF set, and the DAP block
            // count is rewritten to the number actually transferred (zero here).
            self.set_dap_blocks(dap, 0);
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
            return;
        }
        self.c_accesses += 1;
        let mut done: u16 = 0;
        for i in 0..count {
            let l = lba + u32::from(i);
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            match ah {
                0x42 => {
                    let data = self.ata.as_ref().and_then(|d| d.read_lba(l));
                    match data {
                        Some(bytes) => self.write_guest_block(addr, &bytes),
                        None => break,
                    }
                }
                0x43 => {
                    let bytes = self.read_guest_block(addr, 512);
                    let wrote = self
                        .ata
                        .as_mut()
                        .map(|d| d.write_lba(l, &bytes))
                        .unwrap_or(false);
                    if !wrote {
                        break;
                    }
                }
                0x44 => {
                    if self.ata.as_ref().and_then(|d| d.read_lba(l)).is_none() {
                        break;
                    }
                }
                _ => unreachable!("EDD transfer dispatch validates AH"),
            }
            done += 1;
        }
        self.stall_for_hdd_sectors(u32::from(done));
        // EDD writes the count actually moved back into the DAP block-count field.
        self.set_dap_blocks(dap, done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// Rewrite the Disk Address Packet block-count field (offset 2) with the
    /// sectors actually transferred, the way EDD reports partial completion.
    fn set_dap_blocks(&mut self, dap: u32, blocks: u16) {
        let bytes = blocks.to_le_bytes();
        self.write_guest_block(dap + 2, &bytes);
    }

    /// EDD AH=48h get extended drive parameters. The result buffer at DS:SI takes
    /// the EDD 1.x layout: word 0 = buffer size, word 2 = info flags, dwords for
    /// the CHS geometry, qword 16 = total sectors, word 24 = bytes per sector.
    fn int13_edd_drive_params(&mut self) {
        let Some(disk) = self.ata.as_ref() else {
            self.set_eax_ah(0x01);
            self.set_int_frame_carry(true);
            return;
        };
        let total = u64::from(disk.total_sectors());
        let cylinders = disk.cylinders();
        let heads = disk.heads();
        let spt = disk.sectors_per_track();

        let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
        let si = self.cpu.registers.esi() as u16;
        let dst = ds.wrapping_add(u32::from(si));

        let mut buf = [0u8; 26];
        buf[0..2].copy_from_slice(&26u16.to_le_bytes()); // buffer size (EDD 1.x)
        buf[2..4].copy_from_slice(&0x0002u16.to_le_bytes()); // info: geometry valid
        buf[4..8].copy_from_slice(&cylinders.to_le_bytes()); // physical cylinders
        buf[8..12].copy_from_slice(&heads.to_le_bytes()); // physical heads
        buf[12..16].copy_from_slice(&spt.to_le_bytes()); // sectors per track
        buf[16..24].copy_from_slice(&total.to_le_bytes()); // total sectors (qword)
        buf[24..26].copy_from_slice(&(ata::SECTOR as u16).to_le_bytes()); // bytes/sector
        self.write_guest_block(dst, &buf);
        self.set_eax_ah(0x00);
        self.set_fixed_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// Number of floppy drives the BDA equipment word advertises (0040:0010): bit 0
    /// is the floppy-installed flag, bits 7-6 are the drive count minus one. INT 13h
    /// AH=08h reports this in DL so it tracks the mounted drives.
    fn floppy_count(&self) -> u8 {
        let word = self.memory.read_u16(0x410).unwrap_or(BIOS_EQUIPMENT_WORD);
        if word & 0x0001 == 0 {
            0
        } else {
            ((word >> 6) & 0x03) as u8 + 1
        }
    }
}
