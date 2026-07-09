// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn mount_hdd_seeds_the_bda_fixed_disk_count() {
    let m = machine_with_hdd(64);
    assert_eq!(m.memory.read_u8(0x475).unwrap(), 1, "one fixed disk");
}

#[test]
fn apply_overrides_replaces_by_name_and_appends_new() {
    let mut base = vec![
        ("AUTOEXEC.BAT".to_string(), b"old".to_vec()),
        ("KERNEL.SYS".to_string(), b"k".to_vec()),
    ];
    apply_overrides(
        &mut base,
        vec![
            ("autoexec.bat".to_string(), b"new".to_vec()), // case-insensitive replace
            ("RUNNER.COM".to_string(), b"r".to_vec()),     // append
        ],
    );
    let autoexec = base
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("autoexec.bat"))
        .unwrap();
    assert_eq!(autoexec.1, b"new");
    // A replace updates bytes in place but keeps the original key's case
    // (KateaTreeVolume folds names case-insensitively, so the stored case is
    // cosmetic — pinned here so the intent is explicit).
    assert_eq!(
        autoexec.0, "AUTOEXEC.BAT",
        "original key case preserved on replace"
    );
    assert!(base.iter().any(|(n, b)| n == "KERNEL.SYS" && b == b"k"));
    assert!(base.iter().any(|(n, b)| n == "RUNNER.COM" && b == b"r"));
    assert_eq!(base.len(), 3, "one replace + one append");
}

#[test]
fn ensure_user_config_seeds_missing_files_only() {
    let dir = std::env::temp_dir().join(format!("katea_cfg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A user-owned AUTOEXEC stays; a missing CONFIG.SYS is seeded.
    std::fs::write(dir.join("AUTOEXEC.BAT"), b"@ECHO OFF\r\nMYGAME\r\n").unwrap();
    super::ensure_user_config(&dir, b"FILES=40\r\n", b"@ECHO OFF\r\nDEFAULT\r\n").unwrap();
    assert_eq!(
        std::fs::read(dir.join("AUTOEXEC.BAT")).unwrap(),
        b"@ECHO OFF\r\nMYGAME\r\n",
        "the user's AUTOEXEC must not be overwritten"
    );
    assert_eq!(
        std::fs::read(dir.join("CONFIG.SYS")).unwrap(),
        b"FILES=40\r\n",
        "a missing CONFIG.SYS is seeded with the default"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn user_folder_overlay_keeps_binaries_drops_config() {
    let payload = vec![
        ("KERNEL.SYS".to_string(), vec![1u8]),
        ("COMMAND.COM".to_string(), vec![2u8]),
        ("CONFIG.SYS".to_string(), vec![3u8]),
        ("AUTOEXEC.BAT".to_string(), vec![4u8]),
        ("HELLO.TXT".to_string(), vec![5u8]),
        ("LICENSE.TXT".to_string(), vec![6u8]),
        ("TOKAMOUS.COM".to_string(), vec![7u8]),
    ];
    let names: Vec<String> = super::user_folder_overlay(payload)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.contains(&"KERNEL.SYS".to_string()));
    assert!(names.contains(&"TOKAMOUS.COM".to_string()));
    assert!(names.contains(&"LICENSE.TXT".to_string()));
    assert!(
        !names.contains(&"CONFIG.SYS".to_string()),
        "config is the user's"
    );
    assert!(
        !names.contains(&"AUTOEXEC.BAT".to_string()),
        "autoexec is the user's"
    );
    assert!(
        !names.contains(&"HELLO.TXT".to_string()),
        "demo file dropped"
    );
}

#[test]
fn flush_hdd_folder_runs_a_final_reconcile() {
    // Mount a temp folder, then flush; confirm flush is callable and a no-op on
    // an unwritten folder (creates nothing beyond the config mount seeds). The
    // end-to-end create/overwrite/grow is covered by the e2e smoke test; this
    // only proves the plumbing exists.
    let dir = std::env::temp_dir().join(format!("katea_flush_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    m.mount_hdd_folder(&dir).unwrap();
    // mount_hdd_folder seeds the user-owned CONFIG.SYS/AUTOEXEC.BAT.
    let listing = |dir: &std::path::Path| -> std::collections::BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let after_mount = listing(&dir);
    assert!(
        after_mount.contains("CONFIG.SYS") && after_mount.contains("AUTOEXEC.BAT"),
        "mount seeds the user-owned config"
    );
    m.flush_hdd_folder();
    // With nothing written by the guest, flush creates nothing new.
    assert_eq!(
        listing(&dir),
        after_mount,
        "flush on an unwritten folder creates nothing beyond the seed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mount_hdd_publishes_int41_fixed_disk_parameter_table() {
    let mut m = machine_with_hdd(4032); // 4 cylinders, 16 heads, 63 spt
    let off = read_u16(&mut m, 0x41 * 4);
    let seg = read_u16(&mut m, 0x41 * 4 + 2);
    let table = (u32::from(seg) << 4) + u32::from(off);
    assert_eq!(table, BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR);
    assert_eq!(read_u16(&mut m, table), 4, "cylinder count");
    assert_eq!(m.read_physical_u8(table + 2), 16, "head count");
    assert_eq!(read_u16(&mut m, table + 3), 0, "no XT reduced current");
    assert_eq!(read_u16(&mut m, table + 5), 0, "no write precomp");
    assert_eq!(m.read_physical_u8(table + 8), 0x08, "more-than-8-heads bit");
    assert_eq!(read_u16(&mut m, table + 12), 4, "landing zone");
    assert_eq!(m.read_physical_u8(table + 14), 63, "sectors per track");

    let bytes = m.eject_hdd().unwrap();
    assert_eq!(bytes.len(), 4032 * 512);
    assert_eq!(m.memory.read_u8(0x475).unwrap(), 0, "no fixed disks");
    assert_eq!(read_u16(&mut m, 0x41 * 4), 0, "INT 41h offset cleared");
    assert_eq!(read_u16(&mut m, 0x41 * 4 + 2), 0, "INT 41h segment cleared");
}

#[test]
fn int46_secondary_fixed_disk_parameter_table_is_absent() {
    let mut m = machine_with_hdd(4032);
    assert_eq!(read_u16(&mut m, 0x46 * 4), 0, "INT 46h offset absent");
    assert_eq!(read_u16(&mut m, 0x46 * 4 + 2), 0, "INT 46h segment absent");

    let bytes = m.eject_hdd().unwrap();
    assert_eq!(bytes.len(), 4032 * 512);
    assert_eq!(
        read_u16(&mut m, 0x46 * 4),
        0,
        "INT 46h offset remains absent"
    );
    assert_eq!(
        read_u16(&mut m, 0x46 * 4 + 2),
        0,
        "INT 46h segment remains absent"
    );
}

#[test]
fn int13_ah02_reads_a_hard_disk_sector_through_es_bx() {
    let mut m = machine_with_hdd(4032); // 16*63 = one cylinder of 1008, 4 cyls
    // Read LBA 63 (CHS cyl 0, head 1, sector 1). AL=1, CH=0, CL=1 (sector),
    // DH=1 (head), DL=0x80 (C:), ES:BX = 4000:0000 (physical 0x40000).
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001); // CH=0, CL=1
    m.cpu.registers.set_edx(0x0180); // DH=1, DL=0x80
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    assert_eq!(m.cpu.registers.eax() as u8, 0x01, "AL=1 sector moved");
    // The marker for LBA 63 is 63 + 0x10.
    assert_eq!(m.read_physical_u8(0x4_0000), 63u8.wrapping_add(0x10));
}

#[test]
fn int13_ah03_write_then_ah02_read_round_trips() {
    let mut m = machine_with_hdd(64);
    // Seed a pattern in a guest buffer at ES:BX = 2000:0000 (0x20000).
    for i in 0..512u32 {
        m.write_physical_u8(0x2_0000 + i, (i & 0xff) as u8 ^ 0x5A);
    }
    // Write LBA 0 (CHS 0,0,1): AH=03 AL=1, CH=0 CL=1, DH=0 DL=0x80.
    m.cpu.registers.set_eax(0x0301);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write AH=0");
    assert!(m.hdd_dirty(), "the write marked the image dirty");

    // Read it back into a fresh buffer at 3000:0000.
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
    for i in 0..512u32 {
        assert_eq!(m.read_physical_u8(0x3_0000 + i), (i & 0xff) as u8 ^ 0x5A);
    }
}

#[test]
fn int13_ah0a_read_long_includes_synthetic_ecc_bytes() {
    let mut m = machine_with_hdd(64);
    for i in 0..516u32 {
        m.write_physical_u8(0x4_0000 + i, 0xAA);
    }

    m.cpu.registers.set_eax(0x0A01);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(m.cpu.registers.eax() as u8, 0x01, "AL=1 sector moved");
    assert_eq!(m.read_physical_u8(0x4_0000), 0x10, "sector data copied");
    for i in 0..4u32 {
        assert_eq!(
            m.read_physical_u8(0x4_0000 + 512 + i),
            0x00,
            "synthetic ECC byte {i}"
        );
    }
}

#[test]
fn int13_ah0b_write_long_ignores_ecc_bytes() {
    let mut m = machine_with_hdd(64);
    for i in 0..512u32 {
        m.write_physical_u8(0x2_0000 + i, (i as u8).wrapping_mul(3));
    }
    for i in 0..4u32 {
        m.write_physical_u8(0x2_0000 + 512 + i, 0xE0 + i as u8);
    }

    m.cpu.registers.set_eax(0x0B01);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write long AH=0");

    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
    for i in 0..512u32 {
        assert_eq!(m.read_physical_u8(0x3_0000 + i), (i as u8).wrapping_mul(3));
    }
}

#[test]
fn int13_ah08_reports_hard_disk_geometry() {
    let mut m = machine_with_hdd(4032); // 4 cylinders, 16 heads, 63 spt
    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_edx(0x0080); // DL = C:
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    let cx = m.cpu.registers.ecx() as u16;
    let dx = m.cpu.registers.edx() as u16;
    let cl = cx as u8;
    let ch = (cx >> 8) as u8;
    let sectors = cl & 0x3f;
    let max_cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
    assert_eq!(sectors, 63, "63 sectors per track");
    assert_eq!(max_cyl, 3, "max cylinder index = 4 - 1");
    assert_eq!((dx >> 8) as u8, 15, "max head index = 16 - 1");
    assert_eq!(dx as u8, 1, "one fixed disk in DL");
}

#[test]
fn int13_ah15_reports_fixed_disk_dasd_and_capacity() {
    let mut m = machine_with_hdd(4032);
    m.cpu.registers.set_eax(0x1500);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x03, "AH=03 fixed disk");
    let cx = m.cpu.registers.ecx() as u16;
    let dx = m.cpu.registers.edx() as u16;
    let total = (u32::from(cx) << 16) | u32::from(dx);
    assert_eq!(total, 4032, "CX:DX = total sectors");
}

#[test]
fn int13_hard_disk_read_past_end_sets_carry() {
    let mut m = machine_with_hdd(8); // 8 sectors, all on cylinder 0
    // Read at CHS that maps past the image (cyl 1 does not exist).
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001); // sector 1
    m.cpu.registers.set_edx(0x0180); // head 1, DL=0x80
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    // head 1 * 63 spt = LBA 63, past an 8-sector disk: sector-not-found.
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x04, "AH=04 not found");
    assert_eq!(m.memory.read_u8(0x474).unwrap(), 0x04, "fixed-disk status");
}

#[test]
fn int13_ah41_edd_install_check() {
    let mut m = machine_with_hdd(64);
    m.cpu.registers.set_eax(0x4100);
    m.cpu.registers.set_ebx(0x55AA); // the documented input magic
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!(m.cpu.registers.ebx() as u16, 0xAA55, "BX=0xAA55 present");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x30, "EDD version 3.0");
    assert_eq!(m.cpu.registers.ecx() as u16 & 0x0001, 0x0001, "ext access");
}

#[test]
fn int13_legacy_fixed_disk_controls_report_status() {
    let mut m = machine_with_hdd(64);

    m.cpu.registers.set_eax(0x12ff);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=12 success");
    assert_eq!(m.cpu.registers.eax() as u8, 0x00, "AL=0 diagnostic code");

    m.cpu.registers.set_eax(0x1300);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=13 success");

    m.cpu.registers.set_eax(0x1900);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=19 success");

    m.cpu.registers.set_eax(0x0600);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x01, "format rejected");
    assert_eq!(m.memory.read_u8(0x474).unwrap(), 0x01, "fixed status");
}

#[test]
fn int13_ah42_extended_read_via_disk_address_packet() {
    let mut m = machine_with_hdd(64);
    // Build a Disk Address Packet at DS:SI = 5000:0000 (physical 0x50000):
    // size 16, reserved 0, blocks 1, reserved 0, buffer 6000:0000, LBA 7.
    let dap = 0x5_0000u32;
    m.write_physical_u8(dap, 16); // packet size
    m.write_physical_u8(dap + 2, 1); // block count
    // buffer offset (0) at 4-5, segment 0x6000 at 6-7.
    m.write_physical_u8(dap + 6, 0x00);
    m.write_physical_u8(dap + 7, 0x60);
    m.write_physical_u8(dap + 8, 7); // LBA low byte = 7
    m.cpu.registers.set_eax(0x4200);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
    m.cpu.registers.set_esi(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    // The buffer at 0x60000 holds the LBA 7 marker (7 + 0x10).
    assert_eq!(m.read_physical_u8(0x6_0000), 7u8.wrapping_add(0x10));
    // The packet's block count was rewritten to 1 (sectors moved).
    assert_eq!(m.read_physical_u8(dap + 2), 1);
}

#[test]
fn int13_ah48_extended_drive_params() {
    let mut m = machine_with_hdd(4032);
    let buf = 0x5_0000u32;
    m.cpu.registers.set_eax(0x4800);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
    m.cpu.registers.set_esi(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    let total = (0..8u32).fold(0u64, |acc, i| {
        acc | (u64::from(m.read_physical_u8(buf + 16 + i)) << (i * 8))
    });
    assert_eq!(total, 4032, "qword total sectors");
    let bps =
        u16::from(m.read_physical_u8(buf + 24)) | (u16::from(m.read_physical_u8(buf + 25)) << 8);
    assert_eq!(bps, 512, "bytes per sector");
}

#[test]
fn primary_channel_ports_read_open_bus_when_empty() {
    // With no disk mounted, the primary channel reads 0xFF (open bus) so a
    // probe sees no device, and a write is harmlessly dropped.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x1F2, BusWidth::Byte, 0x55, false).unwrap();
        let v = bus.read_io(0x1F7, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(v, 0xFF, "empty channel reads open bus");
    });
}

#[test]
fn primary_channel_identify_runs_through_the_bus() {
    let mut machine = int15_machine(16);
    machine.mount_hdd(vec![0u8; 4032 * 512]);
    with_bus(&mut machine, |bus| {
        // IDENTIFY DEVICE on the command port, then drain word 0 of the block.
        bus.write_io(0x1F7, BusWidth::Byte, 0xEC, false).unwrap();
        let lo = bus.read_io(0x1F0, BusWidth::Byte, 0, false).unwrap();
        let hi = bus.read_io(0x1F0, BusWidth::Byte, 0, false).unwrap();
        let word0 = u16::from(lo as u8) | (u16::from(hi as u8) << 8);
        assert_eq!(word0, 0x0040, "fixed ATA device general config");
    });
}

#[test]
fn booter_inert_stands_down_dos_vectors_but_keeps_the_bios() {
    let mut m = int15_machine(16);

    // The Rust DOS kernel that used to service INT 21h/25h/26h/27h/29h/2Ah/2Eh
    // was retired in SP-3, so those pure-DOS vectors are no longer intercepted
    // in EITHER mode; they always pass straight through to the guest's IVT.
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h is never intercepted (the DOS kernel was retired)"
    );

    // The DOS multiplex vector (INT 2Fh) IS intercepted by default. INT 67h is
    // not intercepted at all any more: the TOKAEMM guest driver owns the EMS
    // API (SP-4b M2).
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int,
        Some(0x2f),
        "INT 2Fh (multiplex) is intercepted by default"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x67);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 67h (EMS) is never intercepted (the guest driver owns it)"
    );

    // Booter-inert mode stands the multiplex vector down so the guest's own
    // handlers run through the IVT.
    m.set_booter_inert(true);
    assert!(m.booter_inert());
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h is still not intercepted in booter mode"
    );
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 2Fh (multiplex) stands down too"
    );

    // The BIOS hardware services stay intercepted even in booter mode.
    ack_and_dispatch(&mut m, 0x10);
    assert_eq!(
        m.pending_soft_int,
        Some(0x10),
        "INT 10h (BIOS video) stays intercepted"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x13);
    assert_eq!(
        m.pending_soft_int,
        Some(0x13),
        "INT 13h (BIOS disk) stays intercepted"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x40);
    assert_eq!(
        m.pending_soft_int,
        Some(0x40),
        "INT 40h (relocated floppy) stays intercepted"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x42);
    assert_eq!(
        m.pending_soft_int,
        Some(0x42),
        "INT 42h (relocated video) stays intercepted"
    );

    // A vector the HLE never intercepts is recorded in neither mode.
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x80);
    assert_eq!(
        m.pending_soft_int, None,
        "an un-intercepted vector is ignored"
    );
}

#[test]
fn int2f_stands_down_when_a_guest_dpmi_host_hooks_the_vector() {
    let mut m = int15_machine(16);

    // Default boot: IVT[0x2F] is still the ROM IRET stub, so the multiplex
    // HLE intercepts as always.
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int,
        Some(0x2f),
        "default boot: INT 2Fh is intercepted (no guest hook present)"
    );
    m.pending_soft_int = None;

    // Simulate a guest DPMI host (e.g. JEMMEX) hooking IVT[0x2F] to point at
    // its own handler in guest RAM instead of the ROM IRET stub.
    {
        let bus = m.make_bus();
        bus.memory.write_u16(0x2f * 4, 0x128e).unwrap();
        bus.memory.write_u16(0x2f * 4 + 2, 0x00d8).unwrap();
    }
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int, None,
        "guest-hooked INT 2Fh: the HLE stands down so the guest's own handler runs \
             (this is what lets a real DPMI host answer AX=1686h/1687h instead of the \
             HLE's stale \"no host\" answer)"
    );
}

#[test]
fn program_runtime_reintercepts_dos_vectors_for_the_raw_program_loader() {
    // The raw-program runtime (new_raw_program) still services INT 20h/21h/27h
    // itself (terminate + minimal console I/O), so interrupt_acknowledge must
    // record those vectors when program_runtime is set — even though the
    // retired HLE no longer intercepts them for a normal boot. This pins the
    // exact branch the SP-3 seam deletion added to interrupt_acknowledge.
    let prog: &[u8] = &[0xcd, 0x20]; // int 20h
    let mut raw =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    for vector in [0x20u8, 0x21, 0x27] {
        raw.pending_soft_int = None;
        ack_and_dispatch(&mut raw, vector);
        assert_eq!(
            raw.pending_soft_int,
            Some(vector),
            "INT {vector:02X}h is intercepted for the raw-program runtime"
        );
    }

    // A normal (non-program-runtime) machine passes them straight through.
    let mut boot = int15_machine(16);
    ack_and_dispatch(&mut boot, 0x21);
    assert_eq!(
        boot.pending_soft_int, None,
        "INT 21h passes through for a normal boot (no raw-program runtime)"
    );
}

#[test]
fn absent_resident_api_vectors_intercept_only_default_iret() {
    let mut m = int15_machine(16);

    for vector in [0x5C, 0x60, 0x68, 0x6F, 0x7A, 0x86, 0xE4] {
        ack_and_dispatch(&mut m, vector);
        assert_eq!(m.pending_soft_int, Some(vector), "INT {vector:02X}h");
        m.pending_soft_int = None;
    }

    m.memory.write_u16(0x60 * 4, 0x1234).unwrap();
    m.memory.write_u16(0x60 * 4 + 2, 0x5678).unwrap();
    ack_and_dispatch(&mut m, 0x60);

    assert_eq!(
        m.pending_soft_int, None,
        "guest-owned INT 60h is not stolen"
    );
}

#[test]
fn absent_resident_api_vectors_report_not_installed() {
    let mut m = int15_machine(16);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
    m.cpu.registers.set_ebx(0x0020);
    m.write_physical_u8(0x30020 + 1, 0);
    m.handle_absent_resident_api(0x5C);
    assert_eq!(m.cpu.registers.eax() as u8, 0xFB);
    assert_eq!(m.read_physical_u8(0x30020 + 1), 0xFB);

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0xCAFE_01FF);
    m.handle_absent_resident_api(0x60);
    assert_eq!(m.cpu.registers.eax(), 0xCAFE_01FF);
    assert_eq!(dos_int_flags(&m) & 1, 0, "driver-info clears CF");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0xCAFE_0400);
    m.cpu.registers.set_edx(0x1111_2222);
    m.handle_absent_resident_api(0x60);
    assert_eq!((m.cpu.registers.edx() >> 8) as u8, 0x0B);
    assert_ne!(dos_int_flags(&m) & 1, 0, "packet send sets CF");

    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x4000));
    m.cpu.registers.set_edx(0x0100);
    m.cpu.registers.set_eax(0x0500);
    m.write_guest_block(0x40100, &[0; 0x18]);
    m.handle_absent_resident_api(0x68);
    assert_eq!(&m.read_guest_block(0x40114, 4), &[0xF0, 0x01, 0x00, 0x00]);

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x0200);
    m.handle_absent_resident_api(0x6F);
    assert_eq!(m.cpu.registers.eax() as u16, 0x08FF);
    assert_ne!(dos_int_flags(&m) & 1, 0, "10NET node status sets CF");

    m.cpu.registers.set_eax(0x0001);
    m.cpu.registers.set_ebx(0x1111_2222);
    m.cpu.registers.set_ecx(0x3333_4444);
    m.cpu.registers.set_edx(0x5555_6666);
    m.handle_absent_resident_api(0x7A);
    assert_eq!(m.cpu.registers.eax() as u16, 0);
    assert_eq!(m.cpu.registers.ebx() as u16, 0);
    assert_eq!(m.cpu.registers.ecx() as u16, 0);
    assert_eq!(m.cpu.registers.edx() as u16, 0);

    m.cpu.registers.set_eax(0);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_absent_resident_api(0x7A);
    assert_eq!(m.cpu.registers.eax() as u8, 0xF0);
}

#[test]
fn int19_floppy_boot_marks_the_machine_booter_inert() {
    // Booting any floppy hands the machine to the disk's own sector-0 code,
    // so the HLE Toka-DOS stands down the way it would on real hardware:
    // whatever is in the boot sector is the OS now, not the HLE.
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 1.44 MB, readable sector 0
    assert!(!m.booter_inert(), "booter-inert defaults off");
    m.handle_int19();
    assert!(
        m.booter_inert(),
        "a floppy boot stands the HLE down so the disk owns the DOS interrupts"
    );
    assert_eq!(
        m.cpu.registers.edx() as u8,
        0x00,
        "DL=00h: the floppy branch ran"
    );
}

#[test]
fn int19_boots_from_ata_when_no_floppy() {
    // Booting from a fixed disk (ATA primary master) hands the machine to the
    // disk's own sector-0 code, so the HLE Toka-DOS stands down exactly the
    // same way the floppy path does. DL=0x80 signals the first fixed disk.
    let mut m = int15_machine(16);
    // Build a minimal 4-sector image with the 0x55AA boot signature.
    let mut img = vec![0u8; 512 * 4];
    img[0] = 0xEB; // recognisable first byte
    img[510] = 0x55;
    img[511] = 0xAA;
    m.mount_hdd(img);
    assert!(!m.booter_inert(), "booter-inert defaults off");
    m.handle_int19();
    assert!(
        m.booter_inert(),
        "an ATA boot stands the HLE down so the disk owns the DOS interrupts"
    );
    assert_eq!(
        m.cpu.registers.edx() as u8,
        0x80,
        "DL=80h: the ATA fixed-disk branch ran"
    );
    assert_eq!(
        m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32),
        0xEB,
        "sector 0 byte 0 must land at 0x7C00"
    );
}

#[test]
fn int19_skips_ata_without_boot_signature() {
    // An ATA disk whose LBA 0 lacks the 0x55AA signature is not bootable: the
    // ATA branch must fall through (to the C: HLE / int18 path) without copying
    // sector 0 or standing the HLE down. Tasks 3-5 rely on this fall-through.
    let mut m = int15_machine(16);
    let mut img = vec![0u8; 512 * 4];
    img[0] = 0xEB; // sentinel first byte, but NO 0x55AA signature
    m.mount_hdd(img);
    m.handle_int19();
    assert!(
        !m.booter_inert(),
        "an unsigned ATA disk must not stand the HLE down"
    );
    assert_ne!(
        m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32),
        0xEB,
        "an unsigned ATA disk's sector 0 must not be copied to 0x7C00"
    );
}

#[test]
fn floppy_booted_machine_stands_dos_down_at_interrupt_ack() {
    // The end-to-end guarantee: after a floppy boot the next INT 21h must
    // stand down so the disk's own handler runs, not the HLE. This catches a
    // stale booter-inert snapshot in the per-interrupt bus.
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    m.handle_int19();
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h stands down once a floppy has booted"
    );
}
