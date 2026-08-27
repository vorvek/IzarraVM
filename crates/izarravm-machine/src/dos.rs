// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Machine {
    /// Enter or leave booter-inert mode. When set, the Toka-DOS HLE and IZEMM stop
    /// intercepting the DOS/memory-manager interrupts (0x20/0x21/0x25/0x26/0x29/
    /// 0x2F), so a self-booting disk's own handlers run through the IVT;
    /// the BIOS services stay intercepted. The booter track sets this; nothing
    /// auto-detects a booter yet.
    pub fn set_booter_inert(&mut self, inert: bool) {
        self.booter_inert = inert;
    }

    /// Whether booter-inert mode is active.
    pub fn booter_inert(&self) -> bool {
        self.booter_inert
    }

    /// Build a machine with a DOS-format program loaded and ready to run, with
    /// no DOS kernel behind it — only `handle_raw_program_int`'s minimal
    /// terminate/console-I/O surface services interrupts. For tests and
    /// benchmarks that need a quick runnable machine, not C: drive access.
    pub fn new_raw_program(profile: MachineProfile, image: &[u8]) -> Result<Self, MachineError> {
        // No CMOS on the raw-program path (there is no BIOS POST and no
        // cmos.bin), so the MPU port is the power-on default rather than
        // anything SNDCTRL.COM might have persisted.
        let env_entries = sound_blaster_env_entries(&profile.sound_blaster, WAVETABLE_MPU_BASE);
        let mut rom = vec![0u8; BIOS_ROM_SIZE];
        let kb = izarravm_firmware::kbd_resident_bios();
        rom[..kb.len()].copy_from_slice(kb);
        let mut machine = Self::base(profile, CpuGsw::default(), rom)?;
        install_boot_memory(&mut machine.memory, machine.active_mode)?;
        machine.install_keyboard_bios()?;
        machine.program_runtime = true;

        let entry = raw_program::load_program(image, &mut machine.memory, DOS_LOAD_SEGMENT)?;
        machine.apply_raw_program_entry(entry);
        let prog_top = machine
            .memory
            .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)?;
        let entries: Vec<(&str, &str)> = env_entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        raw_program::place_environment(&mut machine.memory, DOS_LOAD_SEGMENT, prog_top, &entries)?;

        // Seed PIT counter 0 the way the BIOS POST leaves it, so a program that
        // polls the timer for a delay doesn't spin forever.
        {
            let mut bus = machine.make_bus();
            let _ = bus.write_io(0x43, BusWidth::Byte, 0x34, false);
            let _ = bus.write_io(0x40, BusWidth::Byte, 0x00, false);
            let _ = bus.write_io(0x40, BusWidth::Byte, 0x00, false);
        }
        Ok(machine)
    }

    /// Accumulated console output for a `new_raw_program` machine.
    pub fn program_output(&self) -> &[u8] {
        &self.program_output
    }

    /// Seed the BDA keyboard ring with input bytes for a `new_raw_program`
    /// machine's character-input calls. Holds up to 15 bytes.
    pub fn set_program_stdin(&mut self, bytes: &[u8]) {
        const KBD_BDA_BASE: usize = 0x400;
        const KBD_HEAD: usize = 0x1a;
        const KBD_TAIL: usize = 0x1c;
        const KBD_RING_START: u16 = 0x1e;
        const KBD_RING_END: u16 = 0x3e;
        debug_assert!(bytes.len() < 16, "keyboard ring holds 15 entries");
        let _ = self.write_guest_ram_u16(KBD_BDA_BASE + KBD_HEAD, KBD_RING_START);
        let mut off = KBD_RING_START;
        for &b in bytes {
            let _ = self.write_guest_ram_u16(KBD_BDA_BASE + off as usize, u16::from(b));
            off += 2;
            if off >= KBD_RING_END {
                off = KBD_RING_START;
            }
        }
        let _ = self.write_guest_ram_u16(KBD_BDA_BASE + KBD_TAIL, off);
    }

    /// Set the CPU to a loaded raw program's entry from its six-field
    /// `raw_program::ProgramEntry` (CS/DS/ES/SS + IP/SP).
    fn apply_raw_program_entry(&mut self, entry: raw_program::ProgramEntry) {
        let r = &mut self.cpu.registers;
        r.set_segment(SegmentIndex::Cs, SegmentRegister::real(entry.cs));
        r.set_segment(SegmentIndex::Ds, SegmentRegister::real(entry.ds));
        r.set_segment(SegmentIndex::Es, SegmentRegister::real(entry.es));
        r.set_segment(SegmentIndex::Ss, SegmentRegister::real(entry.ss));
        r.eip = u32::from(entry.ip);
        r.set_esp(u32::from(entry.sp));
        r.eflags = 0x0000_0202; // IF set: DOS programs start with interrupts on
    }

    /// Install the resident keyboard BIOS for the DOS machine: point IVT[09h] and
    /// IVT[16h] at the handlers in the BIOS ROM (mapped at F000:0000), clear the
    /// BDA ring, program the PIC, and unmask IRQ1. IF is set at program entry so
    /// the ISR can run while a program polls for input.
    fn install_keyboard_bios(&mut self) -> Result<(), MachineError> {
        let kb = izarravm_firmware::kbd_resident_bios();
        let seg = izarravm_firmware::KBD_RESIDENT_BIOS_SEG;
        let int09 = u16::from_le_bytes([kb[0], kb[1]]);
        let int16 = u16::from_le_bytes([kb[2], kb[3]]);
        self.memory.write_u16(0x09 * 4, int09)?;
        self.memory.write_u16(0x09 * 4 + 2, seg)?;
        self.memory.write_u16(0x16 * 4, int16)?;
        self.memory.write_u16(0x16 * 4 + 2, seg)?;
        // BDA keyboard ring: head = tail = ring start, shift flags = 0.
        self.memory.write_u16(0x41a, 0x1e)?;
        self.memory.write_u16(0x41c, 0x1e)?;
        self.memory.write_u8(0x417, 0)?;
        // Program the 8259 pair (master IRQ0..7 -> INT 08h..0Fh), then mask all
        // but IRQ1 on the master so an unhandled timer INT cannot fire.
        {
            let mut bus = self.make_bus();
            for (port, value) in [
                (0x20u16, 0x11u16),
                (0x21, 0x08),
                (0x21, 0x04),
                (0x21, 0x01),
                (0xa0, 0x11),
                (0xa1, 0x70),
                (0xa1, 0x02),
                (0xa1, 0x01),
                (0x21, 0xfd), // master IMR: unmask IRQ1 only
                (0xa1, 0xff), // slave IMR: all masked
            ] {
                bus.write_io(port, BusWidth::Byte, u32::from(value), false)?;
            }
        }
        Ok(())
    }

    pub(super) fn handle_absent_resident_api(&mut self, vector: u8) {
        match vector {
            0x5C => {
                let ncb = self.cpu.registers.segment(SegmentIndex::Es).base
                    + (self.cpu.registers.ebx() as u16) as u32;
                self.write_physical_u8(ncb + 1, 0xFB);
                self.set_eax_al(0xFB);
            }
            // 60h/68h/6Fh had absent-API answers here until POST stopped
            // seeding their vectors (defect E2). With the vectors null the
            // stub landing that posts them cannot happen, so the arms were
            // dead code and are gone.
            0x7A => self.handle_absent_int7a(),
            0x86 | 0xE4 => {}
            _ => {}
        }
    }

    fn handle_absent_int7a(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let bx = self.cpu.registers.ebx() as u16;
        match (ax, bx) {
            (0x0001 | 0x07D0, _) => {
                self.set_ax(0);
                self.set_bx(0);
                self.set_cx(0);
                self.set_dx(0);
            }
            (0x0200, 0) => {
                self.set_bx(0);
                self.set_cx(0);
                self.set_dx(0);
            }
            (_, 0x0010) => self.set_eax_al(0xF0),
            (_, 0x000A) => {}
            _ => self.set_eax_al(0xFF),
        }
    }

    /// Service the DOS-owned and IZCDEX functions of `INT 2Fh` (the multiplex
    /// interrupt) as HLE bridges. Unrecognized AX values fall through unchanged
    /// so other INT 2Fh consumers are unaffected. Returns true if the bridge
    /// handled the call.
    pub(super) fn handle_int2f(&mut self) -> bool {
        let ax = self.cpu.registers.eax() as u16;
        match ax {
            // DOS-owned install checks for resident utilities we do not load.
            // Report "not installed, OK to install" rather than falling through
            // with stale register contents.
            0x0100 | 0x0500 | 0x1000 | 0x1400 | 0x6400 | 0x7A00 | 0xAA00 | 0xAD00 | 0xB000
            | 0xF700 => {
                self.set_eax_al(0x00);
                true
            }
            0x2300 | 0x2E00 => {
                self.set_eax_ah(0x00);
                true
            }
            0xB700 => {
                self.set_ax(0x0000);
                true
            }
            0xB800 => {
                self.set_eax_ah(0x00);
                self.set_bx(0x0000);
                true
            }
            0xB803 => {
                let (seg, off) = self.network_post_address;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
                self.set_bx(off);
                true
            }
            0xB804 => {
                self.network_post_address = (
                    self.cpu.registers.segment(SegmentIndex::Es).selector,
                    self.cpu.registers.ebx() as u16,
                );
                true
            }
            // ASSIGN is absent. DOSINTS documents AH=0 for not installed, while
            // RBIL documents AL=0; clear AX to satisfy both probes. AX=0601h would
            // return ASSIGN's work area if installed, so return a null segment.
            0x0600 => {
                self.set_ax(0x0000);
                true
            }
            0x0601 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
                true
            }
            // PRINT is not resident. Its service calls report a normal DOS invalid
            // function error instead of falling through with whatever CF/AX the
            // caller happened to have.
            0x0101..=0x0105 => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            0x1401..=0x1404 | 0x14FE | 0x14FF => {
                self.set_eax_al(0x01);
                true
            }
            0xB001 => {
                self.set_eax_al(0x00);
                true
            }
            0xB701 | 0xB702 | 0xB809 | 0xF701 => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            ax if ax & 0xFF00 == 0x2300 || ax & 0xFF00 == 0x2E00 => {
                self.set_eax_ah(0x00);
                true
            }
            // Redirector/IFSFUNC calls. Toka-DOS does not load a network
            // redirector or installable filesystem helper here. Hooks that only
            // notify the redirector are no-ops; unsupported remote operations fail
            // with the documented invalid-function result instead of leaking stale
            // caller state.
            0x111D | 0x1122 => true,
            0x1120 => {
                self.set_int_frame_carry(false);
                true
            }
            0x1101..=0x111C | 0x111E | 0x111F | 0x1121 | 0x1123..=0x112F => {
                // The armed IzarraCD redirector serves the CD drive's file
                // I/O host-side; anything else keeps the absent-redirector
                // refusal, which is also what the kernel's own default
                // returned.
                if !self.handle_cd_redirector(ax) {
                    self.set_ax(0x0001);
                    self.set_int_frame_carry(true);
                }
                true
            }
            // Critical-error helper: no resident message override is installed, so
            // expansion requests fail instead of falling through with stale flags.
            ax if ax & 0xFF00 == 0x0500 => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            // DOS 3.2+ disk-interrupt handler hook. Return the previous handler
            // pair, then remember the caller's new DS:DX and ES:BX values.
            ax if ax & 0xFF00 == 0x1300 => {
                let new_handler = (
                    self.cpu.registers.segment(SegmentIndex::Ds).selector,
                    self.cpu.registers.edx() as u16,
                );
                let new_restore = (
                    self.cpu.registers.segment(SegmentIndex::Es).selector,
                    self.cpu.registers.ebx() as u16,
                );
                let old_handler = self.dos_disk_handler;
                let old_restore = self.dos_disk_restore;
                self.dos_disk_handler = new_handler;
                self.dos_disk_restore = new_restore;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Ds, SegmentRegister::real(old_handler.0));
                self.set_dx(old_handler.1);
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(old_restore.0));
                self.set_bx(old_restore.1);
                true
            }
            // Network-redirector / IZCDEX installation check (RBIL INTERRUP.K,
            // INT 2F/AX=1100h). The caller pushes a DADAh marker, runs INT 2Fh,
            // and a present IZCDEX returns AL=FFh and replaces the pushed word
            // with ADADh. A strict probe checks that the word changed, so we
            // rewrite it. The INT pushed IP, CS, FLAGS over the marker, so the
            // marker sits at SS:SP+6. Without that marker this is the plain
            // network-redirector install check, and no redirector is loaded.
            0x1100 => {
                let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
                let sp = self.cpu.registers.esp() as u16;
                let marker_addr = ss + u32::from(sp.wrapping_add(6));
                if self.read_guest_word(marker_addr) == 0xDADA {
                    let _ = self.write_guest_ram_u16(marker_addr as usize, 0xADAD);
                    self.set_eax_al(0xFF);
                } else {
                    self.set_eax_al(0x00);
                }
                true
            }
            // CD-ROM installation check: BX = number of CD drives, CX = first
            // drive letter (0 = A:).
            0x1500 => {
                // One CD drive is present if any D:..Z: letter remains after
                // CONFIG.SYS block drivers. No disc is still a present drive.
                // AL=FFh marks the extensions installed, as IZCDEX reported.
                let cd_drive = self.icdex_cd_drive_number();
                let bx = u16::from(cd_drive.is_some());
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | u32::from(bx);
                self.cpu.registers.set_ebx(ebx);
                let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cd_drive.unwrap_or(0));
                self.cpu.registers.set_ecx(ecx);
                if cd_drive.is_some() {
                    self.set_eax_al(0xFF);
                }
                true
            }
            // Get drive device list: ES:BX -> 5 bytes per drive (subunit + driver
            // header far pointer). One entry: subunit 0, the IzarraCD ROM device
            // header (`TOKACD01`) — a caller may far-call its strategy/interrupt
            // entries directly and reach the host through the doorbell.
            0x1501 => {
                if self.icdex_cd_drive_number().is_some() {
                    let addr = self.icdex_es_bx();
                    let mut entry = [0u8; 5];
                    entry[1..3].copy_from_slice(&CD_DEVICE_HEADER_OFF.to_le_bytes());
                    entry[3..5].copy_from_slice(&CD_DEVICE_HEADER_SEG.to_le_bytes());
                    self.write_guest_linear_block(addr, &entry);
                }
                true
            }
            // Metadata filenames from the ISO primary volume descriptor:
            // 1502h copyright file, 1503h abstract file, 1504h bibliographic
            // file. Each is a 37-byte field in the PVD, returned NUL-terminated
            // in the caller's 38-byte buffer. No medium or no PVD fails with
            // the drive-not-ready code.
            0x1502..=0x1504 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let pvd = self
                    .ide
                    .device()
                    .image()
                    .and_then(|img| img.read_data_sector(16));
                match pvd {
                    Some(pvd) if pvd[0] == 0x01 && &pvd[1..6] == b"CD001" => {
                        let field_at = match ax {
                            0x1502 => 776, // copyright_file_id
                            0x1503 => 813, // abstract_file_id
                            _ => 850,      // bibliographic_file_id
                        };
                        let mut out = [0u8; 38];
                        out[..37].copy_from_slice(&pvd[field_at..field_at + 37]);
                        // The ISO field is space-padded; DOS callers expect an
                        // ASCIZ name, so trim the padding before terminating.
                        let end = out[..37]
                            .iter()
                            .rposition(|&b| b != b' ' && b != 0)
                            .map_or(0, |i| i + 1);
                        out[end..].fill(0);
                        self.write_guest_linear_block(self.icdex_es_bx(), &out);
                        self.set_int_frame_carry(false);
                    }
                    _ => self.icdex_fail(0x0015),
                }
                true
            }
            // Read ISO volume descriptor N. VTOC index 0 maps to LBA 16.
            0x1505 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let lba = 16u32.wrapping_add(u32::from(self.cpu.registers.edx() as u16));
                match self
                    .ide
                    .device()
                    .image()
                    .and_then(|img| img.read_data_sector(lba))
                {
                    Some(sector) => {
                        let descriptor_type = sector[0];
                        self.write_guest_linear_block(self.icdex_es_bx(), &sector);
                        self.set_ax(u16::from(descriptor_type));
                        self.set_int_frame_carry(false);
                    }
                    None => self.icdex_fail(0x0015),
                }
                true
            }
            // Absolute CD data read. SI:DI is the starting LBA, DX is the sector
            // count, and ES:BX receives contiguous 2048-byte sectors.
            0x1508 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let lba = ((self.cpu.registers.esi() as u16 as u32) << 16)
                    | u32::from(self.cpu.registers.edi() as u16);
                let count = self.cpu.registers.edx() as u16;
                let mut addr = self.icdex_es_bx();
                let mut ok = true;
                for sector_index in 0..u32::from(count) {
                    match self
                        .ide
                        .device()
                        .image()
                        .and_then(|img| img.read_data_sector(lba + sector_index))
                    {
                        Some(sector) => {
                            self.write_guest_linear_block(addr, &sector);
                            addr = addr.wrapping_add(cdimage::DATA_SECTOR as u32);
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    self.cd_accesses += u64::from(count != 0);
                    self.set_int_frame_carry(false);
                } else {
                    self.icdex_fail(0x0015);
                }
                true
            }
            // Absolute write is reserved for writable optical media. No such device
            // is modeled, so report invalid function for a valid CD drive.
            0x1509 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                } else {
                    self.icdex_fail(0x0001);
                }
                true
            }
            // Get CD-ROM drive letters: ES:BX -> one byte per drive letter, the
            // drive number (0 = A:). One CD drive.
            0x150D => {
                if let Some(cd_drive) = self.icdex_cd_drive_number() {
                    let addr = self.icdex_es_bx();
                    self.write_guest_linear_block(addr, &[cd_drive]);
                }
                true
            }
            // Drive check: BX = ADADh signals IZCDEX present; AX nonzero if the
            // drive in CX is a supported CD-ROM.
            0x150B => {
                let cx = self.cpu.registers.ecx() as u16;
                let supported = u16::from(
                    self.icdex_cd_drive_number()
                        .is_some_and(|drive| cx == u16::from(drive)),
                );
                let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(supported);
                self.cpu.registers.set_eax(eax);
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0xADAD;
                self.cpu.registers.set_ebx(ebx);
                true
            }
            // Reserved CD-ROM debug toggles. No debugger is modeled, but accepting
            // the calls matches their no-result contract.
            0x1506 | 0x1507 => true,
            // Reserved by MSCDEX. Consume it so probes do not fall through with
            // stale interrupt state.
            0x150A => {
                self.set_int_frame_carry(false);
                true
            }
            // Get IZCDEX version: BH = major, BL = minor. Report 2.30, the
            // version the IZCDEX.COM redirector reported.
            0x150C => {
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0x021E; // 2.30
                self.cpu.registers.set_ebx(ebx);
                true
            }
            // Get/set volume descriptor preference. Default is primary volume
            // descriptor; a caller may request supplementary descriptors.
            0x150E => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let bx = self.cpu.registers.ebx() as u16;
                match bx & 0x00FF {
                    0x0000 => {
                        self.set_dx(self.icdex_vd_preference);
                        self.set_int_frame_carry(false);
                    }
                    0x0001 => {
                        let dx = self.cpu.registers.edx() as u16;
                        let primary = (dx >> 8) as u8;
                        let supplementary = dx as u8;
                        if (primary == 0x01 || primary == 0x02)
                            && (supplementary == 0x00 || supplementary == 0x01)
                        {
                            self.icdex_vd_preference = dx;
                            self.set_int_frame_carry(false);
                        } else {
                            self.icdex_fail(0x0001);
                        }
                    }
                    // BL=02h: the IZCDEX/DOSLFN Joliet toggle. IZCDEX stored
                    // and compared the raw BH byte; mirror that. No
                    // supplementary-descriptor parse exists host-side, so the
                    // byte is stored and acknowledged only.
                    0x0002 => {
                        self.icdex_joliet = (bx >> 8) as u8;
                        self.set_int_frame_carry(false);
                    }
                    _ => self.icdex_fail(0x0001),
                }
                true
            }
            // Get an ISO9660 directory entry. CH bit 0 selects MSCDEX's canonical
            // structure; clear means a direct raw directory-record copy.
            //
            // TWO caller pointers with two different conventions. The ASCIZ path
            // comes IN at ES:BX, the same block address the rest of this handler
            // uses. The record goes OUT at SI:DI, which is a real-mode
            // segment:offset pair held in registers that are not segment
            // registers, so it is built by shifting SI rather than from a
            // descriptor base. Both are guest LINEAR addresses; see
            // `icdex_es_bx`.
            0x150F => {
                let drive = self.cpu.registers.ecx() as u8;
                if !self.icdex_drive_matches(u16::from(drive)) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let path_at = self.icdex_es_bx();
                let path = self.read_guest_linear_asciiz_lossy(path_at, 255);
                let copy_canonical = (self.cpu.registers.ecx() as u16 >> 8) & 1 != 0;
                let dst = (u32::from(self.cpu.registers.esi() as u16) << 4)
                    + u32::from(self.cpu.registers.edi() as u16);
                match self.icdex_iso_dir_record(&path) {
                    Ok(record) => {
                        if copy_canonical {
                            self.write_icdex_canonical_dir_record(dst, &record);
                        } else {
                            let mut out = [0u8; 255];
                            let len = usize::from(record[0]).min(record.len()).min(out.len());
                            out[..len].copy_from_slice(&record[..len]);
                            self.write_guest_linear_block(dst, &out);
                        }
                        self.set_ax(0x0001);
                        self.set_int_frame_carry(false);
                    }
                    Err(code) => self.icdex_fail(code),
                }
                true
            }
            // Windows enhanced-mode installation check. This is a plain DOS box,
            // so neither Windows/386 nor Windows 3.x enhanced mode is active.
            0x1600 => {
                self.set_eax_al(0x00);
                true
            }
            // DPMI mode/install checks. Memory services (XMS/UMB/EMS) are the guest
            // TOKAEMM driver's, and there is no DPMI host yet, so report
            // real-mode/no-host with AX nonzero.
            0x1686 | 0x1687 => {
                self.set_ax(0x0001);
                true
            }
            // Release current VM time-slice (AX=1680h). There is no host-side
            // scheduler to yield to in the HLE, but DOS 5+ reports support by
            // clearing AL; doing so keeps idle loops from treating the function
            // as absent.
            0x1680 => {
                self.set_eax_al(0x00);
                true
            }
            // Send device driver request: ES:BX -> a CD-ROM device driver request
            // header. CX = drive number. Dispatch it to the ATAPI device.
            0x1510 => {
                let cx = self.cpu.registers.ecx() as u16;
                if self
                    .icdex_cd_drive_number()
                    .is_none_or(|drive| cx != u16::from(drive))
                {
                    // Invalid drive: CF set, AX = 000Fh.
                    let eax = (self.cpu.registers.eax() & !0xFFFF) | 0x000F;
                    self.cpu.registers.set_eax(eax);
                    self.set_int_frame_carry(true);
                    return true;
                }
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let header = es.wrapping_add(u32::from(bx));
                self.icdex_device_request(header);
                self.set_int_frame_carry(false);
                true
            }
            _ => false,
        }
    }

    /// The minimal interrupt surface for a `new_raw_program` machine: INT 20h
    /// and AH=4Ch terminate; AH=01h/02h/06h/09h console I/O; anything else
    /// returns DOS's "invalid function" convention (CF=1, AX=0007h) instead
    /// of doing nothing silently. It provides no file I/O, critical error, or
    /// EXEC support.
    pub(super) fn handle_raw_program_int(&mut self, vector: u8) -> Result<Option<u8>, BusError> {
        let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = self.cpu.registers.esp() as u16;
        let flags_addr = (ss + u32::from(sp.wrapping_add(4))) as usize;

        if vector == 0x20 {
            return Ok(Some(0));
        }
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        match ah {
            0x4c => return Ok(Some(ax as u8)),
            0x01 => {
                const KBD_BDA_BASE: usize = 0x400;
                const KBD_HEAD: usize = 0x1a;
                const KBD_TAIL: usize = 0x1c;
                const KBD_RING_START: u16 = 0x1e;
                const KBD_RING_END: u16 = 0x3e;
                let head = self.memory.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
                let tail = self.memory.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
                if head == tail {
                    // Blocking read with an empty ring: rewind the stacked
                    // return IP by 2 so the IRET stub re-enters the same
                    // `CD 21`, and set IF so IRQ1 can run the keyboard ISR
                    // before the retry (a wait-for-key spin).
                    let ip_addr = (ss + u32::from(sp)) as usize;
                    let ret_ip = self.memory.read_u16(ip_addr)?;
                    self.write_guest_ram_u16(ip_addr, ret_ip.wrapping_sub(2))?;
                    let mut flags = self.memory.read_u16(flags_addr)?;
                    flags |= 0x0200; // IF
                    self.write_guest_ram_u16(flags_addr, flags)?;
                    return Ok(None);
                }
                let word = self.memory.read_u16(KBD_BDA_BASE + usize::from(head))?;
                let mut next = head + 2;
                if next >= KBD_RING_END {
                    next = KBD_RING_START;
                }
                self.write_guest_ram_u16(KBD_BDA_BASE + KBD_HEAD, next)?;
                let ch = word as u8;
                self.program_output.push(ch);
                self.cpu.registers.set_eax(u32::from(ch));
            }
            0x02 => {
                let dl = (self.cpu.registers.edx() & 0xff) as u8;
                self.program_output.push(dl);
            }
            0x06 => {
                let dl = (self.cpu.registers.edx() & 0xff) as u8;
                if dl == 0xff {
                    // Char-in-no-wait: report "nothing available" via ZF, the
                    // same shape int14_fossil_keyboard_read uses for polling.
                    const KBD_BDA_BASE: usize = 0x400;
                    const KBD_HEAD: usize = 0x1a;
                    const KBD_TAIL: usize = 0x1c;
                    let head = self.memory.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
                    let tail = self.memory.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
                    let mut flags = self.memory.read_u16(flags_addr)?;
                    if head == tail {
                        flags |= 0x0040; // ZF set: nothing available
                        self.cpu.registers.set_eax(0);
                    } else {
                        let word = self.memory.read_u16(KBD_BDA_BASE + usize::from(head))?;
                        let mut next = head + 2;
                        const KBD_RING_START: u16 = 0x1e;
                        const KBD_RING_END: u16 = 0x3e;
                        if next >= KBD_RING_END {
                            next = KBD_RING_START;
                        }
                        self.write_guest_ram_u16(KBD_BDA_BASE + KBD_HEAD, next)?;
                        flags &= !0x0040; // ZF clear: a char is in AL
                        self.cpu.registers.set_eax(u32::from(word as u8));
                    }
                    self.write_guest_ram_u16(flags_addr, flags)?;
                } else {
                    self.program_output.push(dl);
                }
            }
            0x09 => {
                let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
                let dx = self.cpu.registers.edx() as u16;
                let mut addr = ds + u32::from(dx);
                loop {
                    let byte = self.memory.read_u8(addr as usize)?;
                    if byte == b'$' {
                        break;
                    }
                    self.program_output.push(byte);
                    addr = addr.wrapping_add(1);
                }
            }
            _ => {
                let mut flags = self.memory.read_u16(flags_addr)?;
                flags |= 0x0001; // CF
                self.write_guest_ram_u16(flags_addr, flags)?;
                self.cpu.registers.set_eax(0x0007);
            }
        }
        Ok(None)
    }

    /// Perform the IzarraCD doorbell rung through Lotura port 0xE8. Command 1
    /// executes the CD device request whose far pointer the ROM strategy stub
    /// stored in the low-RAM mailbox, then drops the status back to 0 so the
    /// ROM interrupt stub's poll loop completes. The mailbox holds the
    /// request's real-mode offset word then segment word; both stub and
    /// mailbox live in identity-mapped low memory, so the physical read is
    /// the value the stub wrote.
    ///
    /// A ring with any other command, or with a null mailbox, only parks the
    /// status: port 0xE8 was open bus before this port existed, so a stray
    /// OUT must stay inert instead of decoding low memory as a request.
    pub(super) fn perform_cd_doorbell(&mut self, command: u8) {
        if command != 0x01 {
            self.cd_doorbell_status = 0xFF; // unknown command
            return;
        }
        let off = u32::from(self.read_physical_u8(CD_DEVICE_MAILBOX_ADDRESS as u32))
            | (u32::from(self.read_physical_u8(CD_DEVICE_MAILBOX_ADDRESS as u32 + 1)) << 8);
        let seg = u32::from(self.read_physical_u8(CD_DEVICE_MAILBOX_ADDRESS as u32 + 2))
            | (u32::from(self.read_physical_u8(CD_DEVICE_MAILBOX_ADDRESS as u32 + 3)) << 8);
        if seg == 0 && off == 0 {
            self.cd_doorbell_status = 0xFE; // no request stored
            return;
        }
        let header = (seg << 4).wrapping_add(off);
        self.icdex_device_request(header);
        self.cd_doorbell_status = 0;
    }

    /// Perform a Toka-DOS service requested through Lotura port 0xE3, recording the
    /// status the BIOS reads back. Cmd 0x01 (Repair Toka-DOS) resets the Katea host
    /// folder's CONFIG.SYS/AUTOEXEC.BAT. The retired Rust DOS kernel's legacy
    /// 0x10 HLE C: boot shim is no longer present.
    pub(super) fn perform_toka_service(&mut self, command: u8) {
        self.toka_service_status = match command {
            0x01 => self.katea_repair(),
            _ => 0xff,
        };
    }

    /// Repair Toka-DOS: back the user's CONFIG.SYS/AUTOEXEC.BAT up to .OLD, write
    /// fresh defaults from the committed payload, and re-mount so the boot uses
    /// them. Returns the BIOS status (0 ok, 1 no Katea folder, 0xfe write/mount error).
    fn katea_repair(&mut self) -> u8 {
        let Some(root) = self.katea_root.clone() else {
            return 1; // no Katea host folder mounted
        };
        let sound_blaster = self.profile.sound_blaster;
        let (config, autoexec) = default_config_pair(&sound_blaster, self.cmos_mpu_port());
        // Per-file, not atomic across the two: if AUTOEXEC's write fails after
        // CONFIG was already rewritten, the folder is left half-repaired (default
        // CONFIG, no live AUTOEXEC). No data is lost — both originals survive in
        // their .OLD — and 0xfe tells the user it failed; Repair is a rare manual
        // recovery, so the simple sequence is acceptable.
        for (live_name, old_name, bytes) in [
            ("CONFIG.SYS", "CONFIG.OLD", &config),
            ("AUTOEXEC.BAT", "AUTOEXEC.OLD", &autoexec),
        ] {
            let live = root.join(live_name);
            if live.exists() {
                // Best-effort backup (std::fs::rename replaces an existing .OLD on Windows).
                let _ = std::fs::rename(&live, root.join(old_name));
            }
            if std::fs::write(&live, bytes).is_err() {
                return 0xfe;
            }
        }
        // Rebuild the volume from the repaired folder so the subsequent boot reads it.
        if self.mount_hdd_folder(&root).is_err() {
            return 0xfe;
        }
        0
    }

    /// An ASCIZ string at a guest LINEAR address, stopping at the terminator or
    /// after `max` bytes.
    ///
    /// Read a page at a time rather than byte at a time: a caller string can
    /// cross a page boundary, and one translation does not cover both sides.
    /// The scan stops at the first terminator, so a short name costs one page
    /// read rather than the whole bound -- and, more to the point, a name that
    /// ends before a page the caller's tables do not map is not turned into a
    /// walk of that page.
    pub(super) fn read_guest_linear_asciiz_lossy(&mut self, linear: u32, max: usize) -> String {
        let mut bytes = Vec::new();
        while bytes.len() < max {
            let at = linear.wrapping_add(bytes.len() as u32);
            let run = usize::try_from(0x1000 - (at & 0xfff))
                .expect("a page-tail length fits usize")
                .min(max - bytes.len());
            let block = self.read_guest_linear_block(at, run);
            match block.iter().position(|&byte| byte == 0) {
                Some(end) => {
                    bytes.extend_from_slice(&block[..end]);
                    break;
                }
                None => bytes.extend_from_slice(&block),
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Mirror any console output produced since the last call onto the VGA
    /// text screen. Programs write CON through INT 21h, which is buffered in
    /// `self.program_output` for the native `new_raw_program` runtime; real
    /// DOS renders that to the screen via the BIOS teletype. We do the same
    /// here so a session is visible on the framebuffer, sharing the BDA cursor
    /// at 0040:0050 with the BIOS.
    pub(super) fn flush_dos_console_to_screen(&mut self) {
        let total = self.console_output().len();
        if self.dos_screen_shown >= total {
            return;
        }
        let pending: Vec<u8> = self.console_output()[self.dos_screen_shown..].to_vec();
        self.dos_screen_shown = total;
        for byte in pending {
            self.teletype_char(byte);
        }
    }

    /// The live console output buffer. With the Rust DOS kernel retired every
    /// machine uses the native `program_output` buffer.
    fn console_output(&self) -> &[u8] {
        &self.program_output
    }
}
