// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn el_torito_iso(media: u8) -> CdImage {
    let image_lba = 20u32;
    let image_512 = match media {
        1 => 2400,
        2 => 2880,
        3 => 5760,
        _ => 64,
    };
    let sectors = image_lba as usize + (image_512 as usize).div_ceil(4);
    let mut iso = vec![0u8; sectors * cdimage::DATA_SECTOR];
    let record = 17 * cdimage::DATA_SECTOR;
    iso[record] = 0;
    iso[record + 1..record + 7].copy_from_slice(b"CD001\x01");
    iso[record + 7..record + 30].copy_from_slice(b"EL TORITO SPECIFICATION");
    iso[record + 0x47..record + 0x4B].copy_from_slice(&18u32.to_le_bytes());
    let catalog = 18 * cdimage::DATA_SECTOR;
    iso[catalog] = 1;
    iso[catalog + 1] = 0;
    iso[catalog + 30] = 0x55;
    iso[catalog + 31] = 0xAA;
    let sum = iso[catalog..catalog + 32]
        .chunks_exact(2)
        .fold(0u16, |sum, w| {
            sum.wrapping_add(u16::from_le_bytes([w[0], w[1]]))
        });
    iso[catalog + 28..catalog + 30].copy_from_slice(&0u16.wrapping_sub(sum).to_le_bytes());
    iso[catalog + 32] = 0x88;
    iso[catalog + 33] = media;
    iso[catalog + 34..catalog + 36].copy_from_slice(&0x2000u16.to_le_bytes());
    iso[catalog + 38..catalog + 40].copy_from_slice(&4u16.to_le_bytes());
    iso[catalog + 40..catalog + 44].copy_from_slice(&image_lba.to_le_bytes());
    let boot = image_lba as usize * cdimage::DATA_SECTOR;
    iso[boot..boot + 8].copy_from_slice(&[0xFA, 0xBB, 0x00, 0x05, 0x88, 0x17, 0xF4, 0x90]); // CLI; MOV BX,0500; MOV [BX],DL; HLT
    iso[boot + 512] = 0xA5;
    CdImage::from_iso(iso).unwrap()
}

fn bootable_floppy(marker: u8) -> Vec<u8> {
    let mut image = vec![0u8; 1_474_560];
    image[0] = marker;
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

fn bootable_hdd(marker: u8) -> Vec<u8> {
    let mut image = vec![0u8; 512 * 4];
    image[0] = marker;
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

fn boot_result(machine: &mut Machine) -> (u16, u8, u8) {
    let segment = machine.cpu.registers.segment(SegmentIndex::Cs).selector;
    let address = if segment == 0x2000 {
        0x2_0000
    } else {
        BOOT_SECTOR_ADDRESS as u32
    };
    (
        segment,
        machine.cpu.registers.edx() as u8,
        machine.read_physical_u8(address),
    )
}

#[test]
fn el_torito_boots_no_emulation_and_every_common_emulation_mode() {
    for media in 0u8..=4 {
        let mut m = int15_machine(16);
        m.mount_cd(el_torito_iso(media));
        assert_eq!(
            m.read_physical_u8(0x9FC0C),
            1,
            "media {media} bootable flag"
        );
        m.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, 2);
        m.handle_int19();
        assert_eq!(m.cpu.registers.segment(SegmentIndex::Cs).selector, 0x2000);
        assert_eq!(
            m.cpu.registers.edx() as u8,
            if media == 0 {
                0xE0
            } else if media == 4 {
                0x80
            } else {
                0
            }
        );
        assert_eq!(m.read_physical_u8(0x20000), 0xFA);
        if media != 0 {
            prime_dos_int_frame(&mut m);
            m.cpu
                .registers
                .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
            m.cpu.registers.set_ebx(0);
            m.cpu.registers.set_eax(0x0201);
            m.cpu.registers.set_ecx(0x0002);
            m.cpu.registers.set_edx(if media == 4 { 0x80 } else { 0 });
            m.handle_int13();
            assert_eq!(
                m.read_physical_u8(0x30000),
                0xA5,
                "media {media} emulated read"
            );
            assert_eq!(dos_int_flags(&m) & 1, 0);
        }
    }
}

#[test]
fn el_torito_cd_edd_reads_2048_byte_blocks_and_4b_terminates_emulation() {
    let mut m = int15_machine(16);
    m.mount_cd(el_torito_iso(2));
    m.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, 2);
    m.handle_int19();

    prime_dos_int_frame(&mut m);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0));
    m.cpu.registers.set_esi(0x1000);
    let mut dap = [0u8; 16];
    dap[0] = 16;
    dap[2..4].copy_from_slice(&1u16.to_le_bytes());
    dap[6..8].copy_from_slice(&0x3000u16.to_le_bytes());
    dap[8..16].copy_from_slice(&20u64.to_le_bytes());
    m.write_guest_block(0x1000, &dap);
    m.cpu.registers.set_eax(0x4200);
    m.cpu.registers.set_edx(0xE0);
    m.handle_int13();
    assert_eq!(m.read_physical_u8(0x30000), 0xFA);

    m.cpu.registers.set_esi(0x1200);
    m.cpu.registers.set_eax(0x4B00);
    m.cpu.registers.set_edx(0);
    m.handle_int13();
    assert_eq!(
        m.read_physical_u8(0x1201),
        2,
        "reported floppy-emulation media"
    );
    assert!(m.eltorito_emulation.is_none());
}

#[test]
fn int19_uses_primary_device_then_the_exact_fallback_policy() {
    for (primary, expected) in [
        (BootDevice::Floppy, (0x0000, 0x00, 0x11)),
        (BootDevice::HardDisk, (0x0000, 0x80, 0x22)),
        (BootDevice::CdRom, (0x2000, 0xe0, 0xfa)),
    ] {
        let mut machine = int15_machine(16);
        machine.mount_floppy(bootable_floppy(0x11)).unwrap();
        machine.mount_hdd(bootable_hdd(0x22));
        machine.mount_cd(el_torito_iso(0));
        machine.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, primary as u8);
        machine.handle_int19();
        assert_eq!(boot_result(&mut machine), expected, "primary {primary:?}");
    }

    let cases = [
        (BootDevice::Floppy, false, true, true, (0x0000, 0x80, 0x22)),
        (
            BootDevice::HardDisk,
            true,
            false,
            true,
            (0x0000, 0x00, 0x11),
        ),
        (BootDevice::CdRom, true, true, false, (0x0000, 0x80, 0x22)),
    ];
    for (primary, floppy, hdd, cd, expected) in cases {
        let mut machine = int15_machine(16);
        if floppy {
            machine.mount_floppy(bootable_floppy(0x11)).unwrap();
        }
        if hdd {
            machine.mount_hdd(bootable_hdd(0x22));
        }
        if cd {
            machine.mount_cd(el_torito_iso(0));
        }
        machine.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, primary as u8);
        machine.handle_int19();
        assert_eq!(
            boot_result(&mut machine),
            expected,
            "fallback from {primary:?}"
        );
    }
}

#[test]
fn int19_skips_present_but_unbootable_primary_media() {
    let mut floppy_primary = int15_machine(16);
    floppy_primary.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    floppy_primary.mount_hdd(bootable_hdd(0x22));
    floppy_primary.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, BootDevice::Floppy as u8);
    floppy_primary.handle_int19();
    assert_eq!(boot_result(&mut floppy_primary), (0x0000, 0x80, 0x22));

    let mut hdd_primary = int15_machine(16);
    hdd_primary.mount_hdd(vec![0u8; 512 * 4]);
    hdd_primary.mount_floppy(bootable_floppy(0x11)).unwrap();
    hdd_primary.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, BootDevice::HardDisk as u8);
    hdd_primary.handle_int19();
    assert_eq!(boot_result(&mut hdd_primary), (0x0000, 0x00, 0x11));

    let mut cd_primary = int15_machine(16);
    cd_primary.mount_cd(CdImage::from_iso(vec![0u8; 32 * cdimage::DATA_SECTOR]).unwrap());
    cd_primary.mount_hdd(bootable_hdd(0x22));
    cd_primary.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, BootDevice::CdRom as u8);
    cd_primary.handle_int19();
    assert_eq!(boot_result(&mut cd_primary), (0x0000, 0x80, 0x22));
}

#[test]
fn invalid_el_torito_catalog_is_not_advertised() {
    // Rebuild a plain non-bootable ISO rather than reaching through CdImage internals.
    let image = CdImage::from_iso(vec![0u8; 32 * cdimage::DATA_SECTOR]).unwrap();
    let mut m = int15_machine(16);
    m.mount_cd(image);
    assert_eq!(m.read_physical_u8(0x9FC0C), 0);
}

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
    crate::storage::ensure_user_config(
        &dir,
        b"FILES=40\r\n",
        b"@ECHO OFF\r\nSET BLASTER=A220 I5 D1 H5 P300 T6\r\n",
        &SoundBlasterConfig::default(),
        WAVETABLE_MPU_BASE,
    )
    .unwrap();
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
fn ensure_user_config_upgrades_each_previous_stock_file_independently() {
    let base = std::env::temp_dir().join(format!("katea_cfg_upgrade_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let new_config = b"new config\r\n";
    let new_autoexec = b"new autoexec\r\n";

    let config_only = base.join("config_only");
    std::fs::create_dir_all(&config_only).unwrap();
    std::fs::write(
        config_only.join("CONFIG.SYS"),
        crate::storage::PREVIOUS_STOCK_CONFIG_SYS,
    )
    .unwrap();
    std::fs::write(config_only.join("AUTOEXEC.BAT"), b"@ECHO OFF\r\nMYGAME\r\n").unwrap();
    crate::storage::ensure_user_config(
        &config_only,
        new_config,
        new_autoexec,
        &SoundBlasterConfig::default(),
        WAVETABLE_MPU_BASE,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(config_only.join("CONFIG.SYS")).unwrap(),
        new_config
    );
    assert_eq!(
        std::fs::read(config_only.join("AUTOEXEC.BAT")).unwrap(),
        b"@ECHO OFF\r\nMYGAME\r\n"
    );

    let autoexec_only = base.join("autoexec_only");
    std::fs::create_dir_all(&autoexec_only).unwrap();
    std::fs::write(autoexec_only.join("CONFIG.SYS"), b"FILES=41\r\n").unwrap();
    std::fs::write(
        autoexec_only.join("AUTOEXEC.BAT"),
        crate::storage::PREVIOUS_STOCK_AUTOEXEC_BAT,
    )
    .unwrap();
    crate::storage::ensure_user_config(
        &autoexec_only,
        new_config,
        new_autoexec,
        &SoundBlasterConfig::default(),
        WAVETABLE_MPU_BASE,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(autoexec_only.join("CONFIG.SYS")).unwrap(),
        b"FILES=41\r\n"
    );
    assert_eq!(
        std::fs::read(autoexec_only.join("AUTOEXEC.BAT")).unwrap(),
        new_autoexec
    );

    std::fs::remove_dir_all(&base).ok();
}

fn nondefault_sound_blaster() -> SoundBlasterConfig {
    SoundBlasterConfig {
        enabled: true,
        irq: izarravm_core::SbIrq::I10,
        dma: izarravm_core::SbDma8::D3,
        high_dma: izarravm_core::SbDma16::D7,
    }
}

#[test]
fn stock_autoexec_tracks_default_nondefault_and_disabled_sb_profiles() {
    let base = b"@ECHO OFF\r\nSET BLASTER=A220 I7 D1 H5 P300 T6\r\n\
SET SETSOUND=A220 I7 D1 H5 P300 T6\r\nLH TOKAMOUS\r\n";
    assert_eq!(
        crate::storage::stock_autoexec(base, &SoundBlasterConfig::default(), WAVETABLE_MPU_BASE),
        base
    );
    assert_eq!(
        crate::storage::stock_autoexec(base, &nondefault_sound_blaster(), WAVETABLE_MPU_BASE),
        b"@ECHO OFF\r\nSET BLASTER=A220 I10 D3 H7 P300 T6\r\n\
SET SETSOUND=A220 I10 D3 H7 P300 T6\r\nLH TOKAMOUS\r\n"
    );
    let disabled = SoundBlasterConfig {
        enabled: false,
        ..SoundBlasterConfig::default()
    };
    assert_eq!(
        crate::storage::stock_autoexec(base, &disabled, WAVETABLE_MPU_BASE),
        b"@ECHO OFF\r\nLH TOKAMOUS\r\n"
    );
}

#[test]
fn ensure_user_config_migrates_only_exact_emulator_autoexec_variants() {
    let base_dir =
        std::env::temp_dir().join(format!("katea_cfg_sb_variants_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base_dir);
    let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
    let current_base = payload_file(&payload, "AUTOEXEC.BAT");
    let disabled = SoundBlasterConfig {
        enabled: false,
        ..SoundBlasterConfig::default()
    };
    let target_config = nondefault_sound_blaster();
    let target = crate::storage::stock_autoexec(&current_base, &target_config, WAVETABLE_MPU_BASE);
    let variants = [
        current_base.clone(),
        crate::storage::stock_autoexec(&current_base, &disabled, WAVETABLE_MPU_BASE),
        crate::storage::stock_autoexec(
            &current_base,
            &SoundBlasterConfig::default(),
            WAVETABLE_MPU_BASE,
        ),
        crate::storage::PREVIOUS_STOCK_AUTOEXEC_BAT.to_vec(),
        crate::storage::stock_autoexec(
            crate::storage::PREVIOUS_STOCK_AUTOEXEC_BAT,
            &disabled,
            WAVETABLE_MPU_BASE,
        ),
        crate::storage::stock_autoexec(
            crate::storage::PREVIOUS_STOCK_AUTOEXEC_BAT,
            &nondefault_sound_blaster(),
            WAVETABLE_MPU_BASE,
        ),
    ];
    for (index, variant) in variants.into_iter().enumerate() {
        let dir = base_dir.join(format!("owned_{index}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AUTOEXEC.BAT"), variant).unwrap();
        crate::storage::ensure_user_config(
            &dir,
            b"FILES=40\r\n",
            &current_base,
            &target_config,
            WAVETABLE_MPU_BASE,
        )
        .unwrap();
        assert_eq!(std::fs::read(dir.join("AUTOEXEC.BAT")).unwrap(), target);
    }

    let custom_dir = base_dir.join("custom");
    std::fs::create_dir_all(&custom_dir).unwrap();
    let mut custom = current_base.clone();
    custom.extend_from_slice(b"REM USER CHANGE\r\n");
    std::fs::write(custom_dir.join("AUTOEXEC.BAT"), &custom).unwrap();
    crate::storage::ensure_user_config(
        &custom_dir,
        b"FILES=40\r\n",
        &current_base,
        &target_config,
        WAVETABLE_MPU_BASE,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(custom_dir.join("AUTOEXEC.BAT")).unwrap(),
        custom
    );
    std::fs::remove_dir_all(&base_dir).ok();
}

#[test]
fn explicit_autoexec_override_wins_after_profile_stock_transform() {
    let dir = std::env::temp_dir().join(format!("katea_sb_override_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster = nondefault_sound_blaster();
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    let expected = b"@ECHO OFF\r\nSECOND\r\n";
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nFIRST\r\n".to_vec(),
                ),
                ("autoexec.bat".to_string(), expected.to_vec()),
            ],
        )
        .unwrap();

    let disk = machine.ata.as_ref().unwrap();
    let part_start = crate::katea_volume::PART_START;
    let vbr = disk.read_lba(part_start).unwrap();
    let sectors_per_cluster = u32::from(vbr[0x0D]);
    let reserved = u32::from(u16::from_le_bytes([vbr[0x0E], vbr[0x0F]]));
    let fats = u32::from(vbr[0x10]);
    let fat_sectors = u32::from_le_bytes([vbr[0x24], vbr[0x25], vbr[0x26], vbr[0x27]]);
    let root_cluster = u32::from_le_bytes([vbr[0x2C], vbr[0x2D], vbr[0x2E], vbr[0x2F]]);
    let data_start = part_start + reserved + fats * fat_sectors;
    let root_lba = data_start + (root_cluster - 2) * sectors_per_cluster;
    let root = disk.read_lba(root_lba).unwrap();
    let slot = (0..16)
        .map(|index| index * 32)
        .find(|&offset| &root[offset..offset + 11] == b"AUTOEXECBAT")
        .expect("AUTOEXEC.BAT in root directory");
    let first_cluster = (u32::from(u16::from_le_bytes([root[slot + 20], root[slot + 21]])) << 16)
        | u32::from(u16::from_le_bytes([root[slot + 26], root[slot + 27]]));
    let size = u32::from_le_bytes([
        root[slot + 28],
        root[slot + 29],
        root[slot + 30],
        root[slot + 31],
    ]) as usize;
    let file_lba = data_start + (first_cluster - 2) * sectors_per_cluster;
    assert_eq!(&disk.read_lba(file_lba).unwrap()[..size], expected);
    drop(machine);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn repair_uses_the_disabled_stock_autoexec() {
    let dir = std::env::temp_dir().join(format!("katea_sb_repair_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let sound_blaster = profile.sound_blaster;
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.mount_hdd_folder(&dir).unwrap();
    std::fs::write(dir.join("CONFIG.SYS"), b"REM USER CONFIG\r\n").unwrap();
    std::fs::write(dir.join("AUTOEXEC.BAT"), b"SET BLASTER=USER\r\n").unwrap();

    machine.perform_toka_service(0x01);

    assert_eq!(machine.toka_service_status, 0);
    let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
    let expected = crate::storage::stock_autoexec(
        &payload_file(&payload, "AUTOEXEC.BAT"),
        &sound_blaster,
        WAVETABLE_MPU_BASE,
    );
    assert_eq!(std::fs::read(dir.join("AUTOEXEC.BAT")).unwrap(), expected);
    assert_eq!(
        std::fs::read(dir.join("AUTOEXEC.OLD")).unwrap(),
        b"SET BLASTER=USER\r\n"
    );
    drop(machine);
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
        MachineProfile::gsw_386(16, VideoCard::Vega),
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
fn flush_hdd_folder_forces_a_changed_direct_write_past_the_inline_debounce() {
    let dir = std::env::temp_dir().join(format!("katea_flush_direct_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("A.TXT"), b"before!!").unwrap();
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.mount_hdd_folder(&dir).unwrap();

    // Resolve A.TXT through the synthesized BPB and root directory, then write its
    // data sector through AtaDisk::write_lba. The BIOS/INT 13 paths use this direct
    // route and deliberately do not run the inline ATA-command reconcile.
    let part_start = crate::katea_volume::PART_START;
    let disk = machine.ata.as_ref().unwrap();
    let vbr = disk.read_lba(part_start).unwrap();
    let spc = u32::from(vbr[0x0D]);
    let reserved = u32::from(u16::from_le_bytes([vbr[0x0E], vbr[0x0F]]));
    let fats = u32::from(vbr[0x10]);
    let fatsz = u32::from_le_bytes([vbr[0x24], vbr[0x25], vbr[0x26], vbr[0x27]]);
    let root_cluster = u32::from_le_bytes([vbr[0x2C], vbr[0x2D], vbr[0x2E], vbr[0x2F]]);
    let data_start = part_start + reserved + fats * fatsz;
    let root_lba = data_start + (root_cluster - 2) * spc;
    let root = disk.read_lba(root_lba).unwrap();
    let slot = (0..16)
        .map(|index| index * 32)
        .find(|&offset| &root[offset..offset + 11] == b"A       TXT")
        .expect("A.TXT in root directory");
    let first_cluster = (u32::from(u16::from_le_bytes([root[slot + 20], root[slot + 21]])) << 16)
        | u32::from(u16::from_le_bytes([root[slot + 26], root[slot + 27]]));
    let file_lba = data_start + (first_cluster - 2) * spc;
    let mut sector = disk.read_lba(file_lba).unwrap();
    sector[..8].copy_from_slice(b"FIRST!!!");
    assert!(machine.ata.as_mut().unwrap().write_lba(file_lba, &sector));
    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"before!!");
    machine.flush_hdd_folder();
    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"FIRST!!!");

    // With a completed gather now recorded, an inline reconcile would debounce
    // this second direct change. One explicit flush must bypass that debounce.
    sector[..8].copy_from_slice(b"AFTER!!!");
    assert!(machine.ata.as_mut().unwrap().write_lba(file_lba, &sector));
    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"FIRST!!!");
    machine.flush_hdd_folder();
    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"AFTER!!!");
    drop(machine);
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

fn write_dap_lba(machine: &mut Machine, dap: u32, lba: u64) {
    for (offset, byte) in lba.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(dap + 8 + offset as u32, byte);
    }
}

#[test]
fn int13_edd_high_lba_never_aliases_sector_zero() {
    for ah in [0x42u8, 0x43, 0x44] {
        let mut machine = machine_with_hdd(64);
        let dap = 0x5_0000u32;
        machine.write_physical_u8(dap, 16);
        machine.write_physical_u8(dap + 2, 1);
        machine.write_physical_u8(dap + 7, 0x60);
        write_dap_lba(&mut machine, dap, 1u64 << 32);
        machine.write_physical_u8(0x6_0000, 0xcc);
        let sector_zero_before = machine.ata.as_ref().unwrap().read_lba(0).unwrap();
        machine.cpu.registers.set_eax(u32::from(ah) << 8);
        machine.cpu.registers.set_edx(0x0080);
        machine
            .cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
        machine.cpu.registers.set_esi(0);

        machine.handle_int13();

        assert_eq!(
            (machine.cpu.registers.eax() >> 8) as u8,
            0x04,
            "AH={ah:02x}"
        );
        assert_eq!(machine.read_physical_u8(dap + 2), 0, "AH={ah:02x} count");
        assert_eq!(
            machine.ata.as_ref().unwrap().read_lba(0).unwrap(),
            sector_zero_before,
            "AH={ah:02x} sector zero"
        );
    }
}

#[test]
fn int13_edd_rejects_overflow_and_flat_buffer_packets() {
    for (size, lba, flat) in [(16, u64::MAX, false), (24, 0, true)] {
        let mut machine = machine_with_hdd(64);
        let dap = 0x5_0000u32;
        machine.write_physical_u8(dap, size);
        machine.write_physical_u8(dap + 2, 2);
        if flat {
            for offset in 4..8 {
                machine.write_physical_u8(dap + offset, 0xff);
            }
        } else {
            machine.write_physical_u8(dap + 7, 0x60);
        }
        write_dap_lba(&mut machine, dap, lba);
        machine.cpu.registers.set_eax(0x4200);
        machine.cpu.registers.set_edx(0x0080);
        machine
            .cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
        machine.cpu.registers.set_esi(0);

        machine.handle_int13();

        assert_ne!((machine.cpu.registers.eax() >> 8) as u8, 0);
        assert_eq!(machine.read_physical_u8(dap + 2), 0);
    }
}

fn assert_chs_fixed_disk_deadline(mode: GswMode, ah: u8, count: u8) {
    let mut machine = machine_with_hdd(64);
    machine.set_mode(mode);
    machine
        .cpu
        .registers
        .set_eax((u32::from(ah) << 8) | u32::from(count));
    machine.cpu.registers.set_ecx(0x0001);
    machine.cpu.registers.set_edx(0x0080);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    machine.cpu.registers.set_ebx(0);
    let ticks_before = machine.master_ticks();
    let stalls_before = machine.io_stall_ticks();

    machine.handle_int13();

    let expected = ata::pio_transfer_ticks(u32::from(count));
    assert_eq!(
        machine.master_ticks() - ticks_before,
        expected,
        "{mode} CHS AH={ah:02X} master deadline"
    );
    assert_eq!(
        machine.io_stall_ticks() - stalls_before,
        expected,
        "{mode} CHS AH={ah:02X} I/O stall"
    );
    assert_eq!(
        (machine.cpu.registers.eax() >> 8) as u8,
        0,
        "{mode} CHS AH={ah:02X} succeeds"
    );
}

fn assert_edd_fixed_disk_deadline(mode: GswMode, ah: u8, count: u16) {
    let mut machine = machine_with_hdd(64);
    machine.set_mode(mode);
    let dap = 0x5_0000u32;
    machine.write_physical_u8(dap, 16);
    for (offset, byte) in count.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(dap + 2 + offset as u32, byte);
    }
    machine.write_physical_u8(dap + 7, 0x60);
    machine.write_physical_u8(dap + 8, 7);
    machine.cpu.registers.set_eax(u32::from(ah) << 8);
    machine.cpu.registers.set_edx(0x0080);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
    machine.cpu.registers.set_esi(0);
    let ticks_before = machine.master_ticks();
    let stalls_before = machine.io_stall_ticks();

    machine.handle_int13();

    let expected = ata::pio_transfer_ticks(u32::from(count));
    assert_eq!(
        machine.master_ticks() - ticks_before,
        expected,
        "{mode} EDD AH={ah:02X} master deadline"
    );
    assert_eq!(
        machine.io_stall_ticks() - stalls_before,
        expected,
        "{mode} EDD AH={ah:02X} I/O stall"
    );
    assert_eq!(
        (machine.cpu.registers.eax() >> 8) as u8,
        0,
        "{mode} EDD AH={ah:02X} succeeds"
    );
    assert_eq!(read_u16(&mut machine, dap + 2), count, "DAP block count");
}

#[test]
fn fixed_disk_bios_deadlines_are_cpu_mode_invariant() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for ah in [0x02, 0x03, 0x04, 0x0A, 0x0B] {
            assert_chs_fixed_disk_deadline(mode, ah, 2);
        }
        for ah in [0x42, 0x43, 0x44] {
            assert_edd_fixed_disk_deadline(mode, ah, 2);
        }
    }
}

#[test]
fn fixed_disk_bios_stall_advances_devices_and_is_batch_invariant() {
    let mut bios = machine_with_hdd(300);
    let mut split = machine_with_hdd(300);
    for machine in [&mut bios, &mut split] {
        with_bus(machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0x40, false).unwrap();
            bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
        });
    }
    let beam_before = bios.video().beam_dots();
    let pit_before = bios.pit.channel_out(2);
    bios.cpu.registers.set_eax(0x02ff);
    bios.cpu.registers.set_ecx(0x0001);
    bios.cpu.registers.set_edx(0x0080);
    bios.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    bios.cpu.registers.set_ebx(0);

    bios.handle_int13();
    let expected = ata::pio_transfer_ticks(255);
    split.stall_for_master_ticks(expected / 3);
    split.stall_for_master_ticks(expected - expected / 3);

    assert_eq!(bios.master_ticks(), expected);
    assert_eq!(bios.io_stall_ticks(), expected);
    assert_ne!(bios.video().beam_dots(), beam_before, "video beam advanced");
    assert_ne!(
        bios.pit.channel_out(2),
        pit_before,
        "PIT channel 2 advanced"
    );
    assert_eq!(
        bios.timeline.excluding_tsc(),
        split.timeline.excluding_tsc()
    );
    assert_eq!(bios.video().beam_dots(), split.video().beam_dots());
    assert_eq!(bios.pit.channel_out(2), split.pit.channel_out(2));
    let bios_audio = bios.speaker.drain(512);
    let split_audio = split.speaker.drain(512);
    assert!(
        bios_audio.iter().any(|&sample| sample != 0),
        "speaker audio advanced"
    );
    assert_eq!(bios_audio, split_audio, "audio advance is batch invariant");
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
        bus.write_io(0x1F7, BusWidth::Byte, 0xEC, false).unwrap();
    });
    let deadline = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(deadline);
    with_bus(&mut machine, |bus| {
        // Drain word 0 after IDENTIFY reaches its scheduled DRQ boundary.
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
    // is retired, so those pure-DOS vectors are no longer intercepted
    // in EITHER mode; they always pass straight through to the guest's IVT.
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h is never intercepted (the DOS kernel was retired)"
    );

    // The DOS multiplex vector (INT 2Fh) IS intercepted by default. INT 67h is
    // not intercepted at all any more: the TOKAEMM guest driver owns the EMS
    // API.
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
    // corresponding branch in interrupt_acknowledge.
    let prog: &[u8] = &[0xcd, 0x20]; // int 20h
    let mut raw =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
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

    // 0x5C, 0x7A, 0x86 and 0xE4 carry their per-vector ROM stub out of POST.
    for vector in [0x5C, 0x7A, 0x86, 0xE4] {
        ack_and_dispatch(&mut m, vector);
        assert_eq!(m.pending_soft_int, Some(vector), "INT {vector:02X}h");
        m.pending_soft_int = None;
    }

    // 0x60, 0x68 and 0x6F sit in the user/unused range the AT BIOS leaves at
    // 0000:0000. POST no longer seeds them (defect E2) and the predicate no
    // longer lists them, so even a vector pointed at the ROM stub by hand is
    // not intercepted: the range belongs to the guest outright.
    for vector in [0x60u8, 0x68, 0x6F] {
        let base = usize::from(vector) * 4;
        m.memory.write_u16(base, bios_int_stub_off(vector)).unwrap();
        m.memory.write_u16(base + 2, BIOS_ROM_IRET_SEG).unwrap();
        ack_and_dispatch(&mut m, vector);
        assert_eq!(m.pending_soft_int, None, "INT {vector:02X}h is guest-owned");
    }

    m.memory.write_u16(0x60 * 4, 0x1234).unwrap();
    m.memory.write_u16(0x60 * 4 + 2, 0x5678).unwrap();
    ack_and_dispatch(&mut m, 0x60);

    assert_eq!(
        m.pending_soft_int, None,
        "guest-owned INT 60h is not stolen"
    );

    // And the POST default - a null vector - is not intercepted either.
    m.memory.write_u16(0x60 * 4, 0).unwrap();
    m.memory.write_u16(0x60 * 4 + 2, 0).unwrap();
    ack_and_dispatch(&mut m, 0x60);
    assert_eq!(
        m.pending_soft_int, None,
        "a null INT 60h vector is not intercepted"
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
    let mut image = vec![0u8; 1_474_560];
    image[510] = 0x55;
    image[511] = 0xaa;
    m.mount_floppy(image).unwrap();
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
    let mut image = vec![0u8; 1_474_560];
    image[510] = 0x55;
    image[511] = 0xaa;
    m.mount_floppy(image).unwrap();
    m.handle_int19();
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h stands down once a floppy has booted"
    );
}

/// The BIOS fixed-disk census must be SILENT until armed and must count once it
/// is. Both halves are asserted here because the census is an instrument and the
/// house rule is that a default-off instrument proves it stays off, not just that
/// it works: a census that quietly counted on every run would be exactly the kind
/// of hot-path tax this repo has already paid for once.
#[test]
fn int13_census_is_silent_until_armed_and_counts_after() {
    fn read_one_sector(machine: &mut Machine, count: u8) {
        machine
            .cpu
            .registers
            .set_eax((0x02 << 8) | u32::from(count));
        machine.cpu.registers.set_ecx(0x0001);
        machine.cpu.registers.set_edx(0x0080);
        machine
            .cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
        machine.cpu.registers.set_ebx(0);
        machine.handle_int13();
        assert_eq!(
            (machine.cpu.registers.eax() >> 8) as u8,
            0,
            "the read has to SUCCEED, or the census would be counting a failure path"
        );
    }

    let mut quiet = machine_with_hdd(64);
    read_one_sector(&mut quiet, 1);
    assert_eq!(
        quiet.int13_profile(),
        crate::storage::Int13Profile::default(),
        "unarmed census must stay all-zero after a real successful transfer"
    );

    let mut armed = machine_with_hdd(64);
    armed.enable_int13_profile();
    let stall_before = armed.io_stall_ticks();
    read_one_sector(&mut armed, 1);
    read_one_sector(&mut armed, 8);
    // AH=08 get-parameters: a control call, not a data call.
    armed.cpu.registers.set_eax(0x08 << 8);
    armed.cpu.registers.set_edx(0x0080);
    armed.handle_int13();

    let p = armed.int13_profile();
    assert_eq!(p.read_calls, 2, "two data reads counted");
    assert_eq!(p.read_sectors, 9, "1 + 8 sectors counted");
    assert_eq!(p.control_calls, 1, "AH=08 counted as a control call");
    assert_eq!(p.write_calls, 0);
    // Buckets are 1, 2, 3-4, 5-8, ...; the 1-sector read and the 8-sector read
    // land in the first and fourth.
    assert_eq!(p.read_count_hist[0], 1, "one single-sector read");
    assert_eq!(p.read_count_hist[3], 1, "one 5-8 sector read");
    // Both reads start at the same CHS address, so the 8-sector read's first
    // sector is already resident from the 1-sector read: 9 sectors requested,
    // 8 charged, 1 served by the host-side cache. That overlap is deliberate --
    // it makes this assertion fail if the census ever prices a transfer with the
    // uncached formula while the machine charges the cached one.
    assert_eq!(p.cache_hits, 1, "the repeated first sector was a cache hit");
    assert_eq!(
        p.stall_ticks,
        armed.io_stall_ticks() - stall_before,
        "the census figure must equal what the MACHINE actually charged the guest, \
         not merely what the same helper recomputes"
    );
    assert_eq!(
        p.stall_ticks,
        ata::pio_transfer_ticks_cached(1, 0) + ata::pio_transfer_ticks_cached(8, 1),
        "charged ticks must equal what the ATA model actually charged"
    );
    assert!(
        p.stall_ticks < ata::pio_transfer_ticks(9),
        "and must be strictly less than the uncached charge for the same 9 sectors"
    );
    assert!(
        p.host_wall_ns > 0,
        "the service was timed, so some host wall was recorded"
    );
}

/// One INT 13h CHS read of `count` sectors starting at LBA `(cyl,head,sector)`
/// = (0,0,1) + `lba`, into ES:BX = 0x2000:0. Returns the buffer's first byte of
/// each sector so a test can prove WHAT was served, not only what it cost.
fn int13_read_at(machine: &mut Machine, lba: u32, count: u8) -> Vec<u8> {
    let sectors_per_track = 63u32;
    let cyl = lba / (16 * sectors_per_track);
    let rem = lba % (16 * sectors_per_track);
    let head = rem / sectors_per_track;
    let sector = rem % sectors_per_track + 1;
    let cx = ((cyl & 0xFF) << 8) | ((cyl & 0x300) >> 2) | sector;
    machine
        .cpu
        .registers
        .set_eax((0x02 << 8) | u32::from(count));
    machine.cpu.registers.set_ecx(cx);
    machine.cpu.registers.set_edx((head << 8) | 0x80);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    machine.cpu.registers.set_ebx(0);
    machine.handle_int13();
    assert_eq!(
        (machine.cpu.registers.eax() >> 8) as u8,
        0,
        "the read has to SUCCEED or the test is measuring an error path"
    );
    (0..u32::from(count))
        .map(|i| machine.read_physical_u8(0x20000 + i * 512))
        .collect()
}

/// One INT 13h CHS write of `count` sectors from ES:BX = 0x2000:0 to LBA `lba`.
fn int13_write_at(machine: &mut Machine, lba: u32, count: u8) {
    let sectors_per_track = 63u32;
    let cyl = lba / (16 * sectors_per_track);
    let rem = lba % (16 * sectors_per_track);
    let head = rem / sectors_per_track;
    let sector = rem % sectors_per_track + 1;
    let cx = ((cyl & 0xFF) << 8) | ((cyl & 0x300) >> 2) | sector;
    machine
        .cpu
        .registers
        .set_eax((0x03 << 8) | u32::from(count));
    machine.cpu.registers.set_ecx(cx);
    machine.cpu.registers.set_edx((head << 8) | 0x80);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    machine.cpu.registers.set_ebx(0);
    machine.handle_int13();
    assert_eq!(
        (machine.cpu.registers.eax() >> 8) as u8,
        0,
        "the write has to SUCCEED or the test is measuring an error path"
    );
}

/// The charged model has to deliver the machine's stated storage rate. With the
/// per-command latency at zero, a bulk fixed-disk read must price out at
/// 16.7 MB/s to within the one-tick-per-sector rounding of the tick model, and
/// -- the half that matters -- a run of SINGLE-sector reads has to price out at
/// the SAME rate. Under the old 100 us command latency those two differed by a
/// factor of four, and 98.7% of a real Duke Nukem 3D load was the single-sector
/// case.
///
/// NON-VACUOUS: restoring `COMMAND_LATENCY_TICKS` to `MASTER_CLOCK_HZ / 10_000`
/// drops the single-sector rate to 3.9 MB/s and fails the second assertion;
/// it also fails the first, at 14.9 MB/s for a 64-sector read.
///
/// Measured off the MACHINE's `io_stall_ticks`, never off the census: the census
/// recomputes its own figure with the same helper the charge uses, so a census
/// assertion cannot tell the two apart.
#[test]
fn the_charged_fixed_disk_rate_is_sixteen_point_seven_megabytes_per_second() {
    fn charged_rate(sectors: u64, ticks: u64) -> f64 {
        (sectors * 512) as f64 * izarravm_core::MASTER_CLOCK_HZ as f64 / ticks as f64
    }

    let mut bulk = machine_with_hdd(4096);
    bulk.enable_int13_profile();
    let before = bulk.io_stall_ticks();
    int13_read_at(&mut bulk, 0, 64);
    let bulk_ticks = bulk.io_stall_ticks() - before;
    let bulk_rate = charged_rate(64, bulk_ticks);
    assert!(
        (bulk_rate - 16_700_000.0).abs() < 16_700.0,
        "a 64-sector read must charge 16.7 MB/s, got {bulk_rate:.0} B/s"
    );
    assert_eq!(
        bulk.int13_profile().stall_ticks,
        bulk_ticks,
        "the census must report what the machine actually charged"
    );

    // 64 DISTINCT single-sector reads: no repeat, so the cache never answers one
    // and every sector is charged. Only the per-call overhead can differ.
    let mut singles = machine_with_hdd(4096);
    singles.enable_int13_profile();
    let before = singles.io_stall_ticks();
    for lba in 100..164 {
        int13_read_at(&mut singles, lba, 1);
    }
    let single_ticks = singles.io_stall_ticks() - before;
    assert_eq!(
        singles.int13_profile().cache_hits,
        0,
        "distinct LBAs cannot hit the cache"
    );
    let single_rate = charged_rate(64, single_ticks);
    assert!(
        (single_rate - 16_700_000.0).abs() < 16_700.0,
        "64 single-sector reads must charge the same 16.7 MB/s, got {single_rate:.0} B/s"
    );
}

/// The host-side sector cache, end to end through the BIOS service: a repeat
/// read is charged NOTHING and returns the same bytes, and a write makes the
/// next read return the WRITTEN bytes rather than a stale cached copy.
///
/// MEASURED OFF THE MACHINE, NOT THE CENSUS. `int13_profile().stall_ticks` is
/// recomputed by `note_int13_data` through the very same
/// `pio_transfer_ticks_cached` call that `stall_for_hdd_sectors_cached` uses, so
/// asserting it proves only that the helper agrees with itself: the earlier shape
/// of this test passed with the machine charging the UNCACHED form. What the
/// guest can observe is `io_stall_ticks` and the master timeline, so those are
/// what is asserted here.
///
/// NON-VACUOUS in both directions. Removing the `cache.borrow_mut().put` from
/// `write_lba` leaves the pre-write bytes resident and fails the write-back
/// assertion — that is the invalidation half, and without it the cache would be
/// a correctness bug rather than an accelerator. Charging
/// `pio_transfer_ticks(done)` instead of the cached form in
/// `stall_for_hdd_sectors_cached` makes the repeat read cost the same as the
/// first and fails the free-repeat assertions on both the stall counter and the
/// timeline.
#[test]
fn a_repeat_read_costs_nothing_and_a_write_is_never_served_stale() {
    let mut machine = machine_with_hdd(4096);
    machine.enable_int13_profile();

    let stall_before = machine.io_stall_ticks();
    let clock_before = machine.master_ticks();
    let first = int13_read_at(&mut machine, 7, 1);
    let first_stall = machine.io_stall_ticks() - stall_before;
    let first_elapsed = machine.master_ticks() - clock_before;
    assert!(first_stall > 0, "the first read reached the medium");

    let stall_before = machine.io_stall_ticks();
    let clock_before = machine.master_ticks();
    let repeat = int13_read_at(&mut machine, 7, 1);
    assert_eq!(repeat, first, "a hit serves the same bytes");
    assert_eq!(
        machine.io_stall_ticks() - stall_before,
        0,
        "a repeat read charges the guest NOTHING: the medium was never touched \
         (the first read charged {first_stall})"
    );
    assert_eq!(
        machine.master_ticks() - clock_before,
        0,
        "and the guest-visible timeline does not advance either \
         (the first read advanced it {first_elapsed})"
    );
    assert_eq!(machine.int13_profile().cache_hits, 1);
    assert_eq!(
        machine.int13_profile().stall_ticks,
        first_stall,
        "the census must agree with the machine's own charge"
    );

    // Write sector 7 the way the guest would -- INT 13h AH=03 -- then read it
    // back through the same service.
    for i in 0..512u32 {
        machine.write_physical_u8(0x20000 + i, if i == 0 { 0x5E } else { 0xC7 });
    }
    int13_write_at(&mut machine, 7, 1);
    let after_write = int13_read_at(&mut machine, 7, 1);
    assert_eq!(
        after_write[0], 0x5E,
        "the write invalidated the cached sector; a stale hit would still read {:#04x}",
        first[0]
    );
}

/// Determinism: the charge is a pure function of the guest's own access history.
/// Two machines driven through the identical sequence must agree tick for tick,
/// including on which reads were free.
///
/// NON-VACUOUS: this is the property the whole charge model rests on. Any
/// residency decision seeded from host state (an address, a clock, a hash order)
/// diverges here once the sequence repeats.
#[test]
fn the_same_read_sequence_charges_the_same_ticks_every_time() {
    // Repeats and re-touches, so hits and misses interleave rather than the
    // sequence being all-miss (which any broken cache would also reproduce).
    let sequence: Vec<(u32, u8)> = (0..400u32)
        .map(|i| ((i * 13) % 97, if i % 5 == 0 { 4 } else { 1 }))
        .collect();

    // The MACHINE's charge, not the census's recomputation of it.
    let charge = |()| {
        let mut machine = machine_with_hdd(4096);
        machine.enable_int13_profile();
        let mut per_call = Vec::with_capacity(sequence.len());
        let mut last = machine.io_stall_ticks();
        for &(lba, count) in &sequence {
            int13_read_at(&mut machine, lba, count);
            let now = machine.io_stall_ticks();
            per_call.push(now - last);
            last = now;
        }
        (per_call, machine.int13_profile().cache_hits)
    };

    let first = charge(());
    let second = charge(());
    assert_eq!(
        first.0, second.0,
        "the PER-CALL charge series must be identical, not just the total"
    );
    assert_eq!(first.1, second.1);
    assert!(
        first.1 > 0 && first.0.contains(&0),
        "the sequence must actually produce free calls, or this proves nothing"
    );
}

// ---- the sector cache on a KATEA HOST-FOLDER backing ------------------
//
// Everything above exercises `Backing::Image`, which cannot fail a read: an
// image is a `Vec` and a sector is either in range or it is not. The synthesized
// Katea volume is the backing that reads real host files mid-run, so it is the
// only one where the cache can be asked to remember something that was never
// true. These tests use it.

/// Mount `dir` as C: on a fresh machine, Katea-synthesized.
fn machine_with_hdd_folder(dir: &std::path::Path) -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.mount_hdd_folder(dir).unwrap();
    machine
}

/// A scratch host folder, emptied first so a previous run cannot seed it.
fn katea_scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("izarra_hdd_cache_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Walk the synthesized VBR and root directory to find `name`'s first data LBA
/// and its size. Reads go through `read_lba`, which is the same path the guest
/// uses, so the volume is warm afterwards exactly as a real boot would leave it.
fn katea_file_lba(disk: &ata::AtaDisk, name: &[u8; 11]) -> (u32, u32) {
    let part_start = crate::katea_volume::PART_START;
    let vbr = disk.read_lba(part_start).unwrap();
    let spc = u32::from(vbr[0x0D]);
    let reserved = u32::from(u16::from_le_bytes([vbr[0x0E], vbr[0x0F]]));
    let fats = u32::from(vbr[0x10]);
    let fat_sectors = u32::from_le_bytes([vbr[0x24], vbr[0x25], vbr[0x26], vbr[0x27]]);
    let root_cluster = u32::from_le_bytes([vbr[0x2C], vbr[0x2D], vbr[0x2E], vbr[0x2F]]);
    let data_start = part_start + reserved + fats * fat_sectors;
    let root_lba = data_start + (root_cluster - 2) * spc;
    for sector in 0..spc {
        let root = disk.read_lba(root_lba + sector).unwrap();
        for slot in (0..512).step_by(32) {
            if &root[slot..slot + 11] != name {
                continue;
            }
            let first_cluster = (u32::from(u16::from_le_bytes([root[slot + 20], root[slot + 21]]))
                << 16)
                | u32::from(u16::from_le_bytes([root[slot + 26], root[slot + 27]]));
            let size = u32::from_le_bytes([
                root[slot + 28],
                root[slot + 29],
                root[slot + 30],
                root[slot + 31],
            ]);
            return (data_start + (first_cluster - 2) * spc, size);
        }
    }
    panic!(
        "{} not in the synthesized root directory",
        String::from_utf8_lossy(name)
    );
}

/// Distinct, sector-identifying bytes so a served sector says which one it is
/// and a zero fallback is unmistakable.
fn patterned(sectors: usize) -> Vec<u8> {
    (0..sectors * 512).map(|i| (i / 512) as u8 | 0x40).collect()
}

/// Hit, miss and charge on the KATEA backing, through the guest's own INT 13h
/// service. The image-backed tests above cannot cover this: the two backings
/// reach the cache through different code (`read_lba_uncached`), and only this
/// one synthesizes its sectors and reads host files.
///
/// NON-VACUOUS: charging `pio_transfer_ticks(done)` instead of the cached form
/// in `stall_for_hdd_sectors_cached` makes the repeat read cost the same as the
/// first and fails the free-repeat assertion; disabling the cache
/// (`SectorCache::new(false)`) fails the hit-counter assertion.
#[test]
fn the_sector_cache_hits_misses_and_charges_on_a_katea_host_folder() {
    let dir = katea_scratch("hitmiss");
    std::fs::write(dir.join("GAME.DAT"), patterned(8)).unwrap();
    let mut machine = machine_with_hdd_folder(&dir);
    let (lba, size) = katea_file_lba(machine.ata.as_ref().unwrap(), b"GAME    DAT");
    assert_eq!(size, 8 * 512, "the whole file is visible to the guest");

    let (hits_before, misses_before) = machine.hdd_sector_cache_counters().unwrap();
    let stall_before = machine.io_stall_ticks();
    let first = int13_read_at(&mut machine, lba, 4);
    let first_stall = machine.io_stall_ticks() - stall_before;
    let (hits_after, misses_after) = machine.hdd_sector_cache_counters().unwrap();
    assert_eq!(
        first,
        vec![0x40, 0x41, 0x42, 0x43],
        "host bytes, per sector"
    );
    assert_eq!(hits_after, hits_before, "a cold read cannot hit");
    assert_eq!(misses_after - misses_before, 4, "four sectors missed");
    assert!(first_stall > 0, "and were charged the medium");

    let stall_before = machine.io_stall_ticks();
    let repeat = int13_read_at(&mut machine, lba, 4);
    assert_eq!(repeat, first, "a hit serves the same bytes");
    assert_eq!(
        machine.io_stall_ticks() - stall_before,
        0,
        "the repeat is free (the first read charged {first_stall})"
    );
    assert_eq!(
        machine.hdd_sector_cache_counters().unwrap().0 - hits_after,
        4,
        "all four sectors came out of the cache"
    );

    drop(machine);
    std::fs::remove_dir_all(&dir).ok();
}

/// A TRANSIENT HOST READ FAILURE MUST NEVER BE CACHED.
///
/// The reviewer's reproduction, made permanent. The Katea read path answers an
/// unreadable host file with ZEROS so a vanished or shrunk file cannot panic the
/// guest, and it drops its cached host handle so the next sector re-opens -- a
/// deliberate retry design. The host-side sector cache sits above all of that
/// and remembers whatever the backing returned, so without a degraded signal
/// those zeros become the sector's permanent content: every later read hits the
/// cache and gets zeros, for the life of the mount, even after the host file has
/// been restored byte for byte.
///
/// Sector A proves the cache is live in this same run; sector B is the one the
/// failure hits.
///
/// NON-VACUOUS: deleting the `if !served.degraded` guard in `AtaDisk::read_lba`
/// (i.e. always filling the cache, which is what the code did before this test
/// existed) fails the restored-bytes assertion -- the restored sector still
/// reads back as zeros. Removing the `degraded = true` assignment from
/// `read_source_span`'s read-error arm fails it identically.
#[test]
fn a_failed_host_read_is_served_as_zeros_but_never_cached() {
    let dir = katea_scratch("degraded");
    let path = dir.join("GAME.DAT");
    let original = patterned(8);
    std::fs::write(&path, &original).unwrap();
    let mut machine = machine_with_hdd_folder(&dir);
    let (lba, _) = katea_file_lba(machine.ata.as_ref().unwrap(), b"GAME    DAT");
    let (sector_a, sector_b) = (lba, lba + 1);

    // A: a healthy read, then proof the cache really is answering in this run.
    assert_eq!(int13_read_at(&mut machine, sector_a, 1), vec![0x40]);
    let hits_before = machine.hdd_sector_cache_counters().unwrap().0;
    assert_eq!(int13_read_at(&mut machine, sector_a, 1), vec![0x40]);
    assert_eq!(
        machine.hdd_sector_cache_counters().unwrap().0 - hits_before,
        1,
        "the cache is live, so the B assertions below are about caching"
    );

    // Truncate the host file underneath the running machine. B is now past EOF.
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let misses_before = machine.hdd_sector_cache_counters().unwrap().1;
    assert_eq!(
        int13_read_at(&mut machine, sector_b, 1),
        vec![0x00],
        "a failed host read is SERVED as zeros rather than faulting the guest"
    );
    assert_eq!(
        machine.hdd_sector_cache_counters().unwrap().1 - misses_before,
        1,
        "and it went to the backing, as a miss"
    );

    // Restore the host file byte for byte. The guest did nothing; only the host
    // recovered.
    std::fs::write(&path, &original).unwrap();
    let (hits_before, misses_before) = machine.hdd_sector_cache_counters().unwrap();
    assert_eq!(
        int13_read_at(&mut machine, sector_b, 1),
        vec![0x41],
        "the recovered sector must read back its REAL bytes: the zeros were a \
         failure, not content, and caching them would have pinned them here for \
         the life of the mount"
    );
    let (hits_now, misses_now) = machine.hdd_sector_cache_counters().unwrap();
    assert_eq!(
        (hits_now - hits_before, misses_now - misses_before),
        (0, 1),
        "the retry reached the backing, which is only possible if the failed \
         read was never stored"
    );

    // And the good bytes ARE cached, so the skip is scoped to the failure.
    let hits_before = machine.hdd_sector_cache_counters().unwrap().0;
    assert_eq!(int13_read_at(&mut machine, sector_b, 1), vec![0x41]);
    assert_eq!(
        machine.hdd_sector_cache_counters().unwrap().0 - hits_before,
        1,
        "the successful re-read filled the cache normally"
    );

    drop(machine);
    std::fs::remove_dir_all(&dir).ok();
}

// --- INT 13h buffers under a paged caller -----------------------------------
//
// `run.rs` dispatches the INT 13h HLE for any caller that is not in ring-0
// protected mode, which includes a V86 task under TOKAEMM. DOS loaded with
// DOS=HIGH,UMB puts drivers and their buffers in upper memory that TOKAEMM
// supplies out of extended memory, so a Disk Address Packet, an EDD result
// buffer or a sector transfer target can all sit outside the identity map --
// the same defect the VBE information blocks had. The fixture is the one from
// the VBE fix: guest pages C8h and C9h mapped to two non-adjacent frames.

/// The identity address of the fixture's caller buffer, and a decoy laid over
/// it so a handler that treats a caller pointer as physical reads something
/// recognisably wrong instead of the real packet.
const UMB_IDENTITY: u32 = 0x000c_8c60;

fn poison_identity_range(machine: &mut Machine, len: usize) {
    for offset in 0..len {
        machine.write_physical_u8(UMB_IDENTITY + offset as u32, 0x5a);
    }
}

/// EDD AH=42h with both the Disk Address Packet and its transfer buffer in
/// non-identity-mapped upper memory. The packet is read, the sector is
/// delivered, and the packet's block count is rewritten -- all three have to
/// address the caller's pages.
#[test]
fn int13_edd_transfer_uses_the_non_identity_mapped_packet_and_buffer() {
    let mut m = machine_with_hdd(64);
    super::margo::install_umb_paging(&mut m);
    prime_dos_int_frame(&mut m);

    // DAP at DS:SI = C8C6:0000, transfer buffer at C8C6:0200, both inside the
    // first mapped page. LBA 7 carries the marker 7 + 10h.
    let dap = super::margo::UMB_BUFFER_PHYSICAL;
    m.write_physical_u8(dap, 16); // packet size
    m.write_physical_u8(dap + 2, 1); // block count
    m.write_physical_u8(dap + 4, 0x00); // buffer offset 0200h
    m.write_physical_u8(dap + 5, 0x02);
    m.write_physical_u8(dap + 6, 0xc6); // buffer segment C8C6h
    m.write_physical_u8(dap + 7, 0xc8);
    m.write_physical_u8(dap + 8, 7); // LBA
    poison_identity_range(&mut m, 16);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0xc8c6));
    m.cpu.registers.set_esi(0);
    m.cpu.registers.set_eax(0x4200);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    assert_eq!(
        m.read_physical_u8(super::margo::UMB_FRAME_LOW + 0x0e60),
        7u8.wrapping_add(0x10),
        "the sector must land in the frame the caller's page is mapped to"
    );
    assert_eq!(
        m.read_physical_u8(dap + 2),
        1,
        "the packet's block count must be rewritten in the mapped frame"
    );
}

/// The packet's block-count rewrite, on a packet that straddles the page
/// boundary with the count field split across the two frames. The planted
/// count is 4 and the LBA is out of range, so the handler must rewrite the
/// count to 0 THROUGH THE MAPPING: byte dap+2 in the low frame's last byte,
/// byte dap+3 in the high frame's first. A rewrite through the physical
/// address leaves the planted 4 in the mapped frame, which is exactly the
/// vacuity the first version of this suite had -- the success-path fixtures
/// rewrote the same value the test planted.
#[test]
fn int13_edd_short_transfer_rewrites_the_straddling_packet_count() {
    let mut m = machine_with_hdd(64);
    super::margo::install_umb_paging(&mut m);
    prime_dos_int_frame(&mut m);

    // DAP at DS:SI = C8C6:039D -> guest linear C8FFDh: bytes +0..+2 end the
    // low page, +3.. start the high page. Write the packet through the
    // mapped frames, not the identity addresses.
    let low = super::margo::UMB_FRAME_LOW;
    let high = super::margo::UMB_FRAME_HIGH;
    m.write_physical_u8(low + 0xffd, 16); // packet size
    m.write_physical_u8(low + 0xffe, 0); // reserved
    m.write_physical_u8(low + 0xfff, 4); // block count, low byte
    m.write_physical_u8(high, 0); // block count, high byte
    m.write_physical_u8(high + 1, 0x00); // buffer offset 0200h
    m.write_physical_u8(high + 2, 0x02);
    m.write_physical_u8(high + 3, 0xc6); // buffer segment C8C6h
    m.write_physical_u8(high + 4, 0xc8);
    for i in 0..8 {
        // LBA far past the 64-sector disk, so the transfer shortens to 0.
        m.write_physical_u8(high + 5 + i, if i < 2 { 0xff } else { 0 });
    }

    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0xc8c6));
    m.cpu.registers.set_esi(0x39d);
    m.cpu.registers.set_eax(0x4200);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();

    assert_ne!(dos_int_flags(&m) & 1, 0, "an out-of-range LBA must fail");
    assert_eq!(
        m.read_physical_u8(low + 0xfff),
        0,
        "the count's low byte must be rewritten through the low frame"
    );
    assert_eq!(
        m.read_physical_u8(high),
        0,
        "the count's high byte must be rewritten through the high frame"
    );
}

/// El Torito AH=02h reads the emulated floppy into ES:BX.
#[test]
fn el_torito_emulated_read_lands_in_a_non_identity_mapped_caller_buffer() {
    let mut m = int15_machine(16);
    m.mount_cd(el_torito_iso(2));
    m.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, 2);
    m.handle_int19();
    super::margo::install_umb_paging(&mut m);
    prime_dos_int_frame(&mut m);
    assert_eq!(
        m.read_physical_u32(super::margo::UMB_BUFFER_PHYSICAL),
        0,
        "precondition: the mapped frame must be clear"
    );

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));
    m.cpu.registers.set_ebx(0);
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0002); // cylinder 0, sector 2
    m.cpu.registers.set_edx(0);
    m.handle_int13();

    assert_eq!(dos_int_flags(&m) & 1, 0, "the read must succeed");
    assert_eq!(
        m.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL),
        0xA5,
        "the emulated sector must land in the mapped frame"
    );
}

/// El Torito AH=42h: the packet, the 2048-byte transfer and the rewritten block
/// count all address the caller's pages. The transfer starts at C8C6:0200 and
/// runs 2048 bytes, so it crosses into the second frame, which the fixture maps
/// well away from the first.
#[test]
fn el_torito_cd_extended_read_uses_the_non_identity_mapped_packet_and_buffer() {
    let mut m = int15_machine(16);
    m.mount_cd(el_torito_iso(2));
    m.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, 2);
    m.handle_int19();
    super::margo::install_umb_paging(&mut m);
    prime_dos_int_frame(&mut m);

    let dap = super::margo::UMB_BUFFER_PHYSICAL;
    let mut packet = [0u8; 16];
    packet[0] = 16;
    packet[2..4].copy_from_slice(&1u16.to_le_bytes());
    packet[4..6].copy_from_slice(&0x0200u16.to_le_bytes());
    packet[6..8].copy_from_slice(&0xc8c6u16.to_le_bytes());
    packet[8..16].copy_from_slice(&20u64.to_le_bytes());
    m.write_guest_block(dap, &packet);
    poison_identity_range(&mut m, 16);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0xc8c6));
    m.cpu.registers.set_esi(0);
    m.cpu.registers.set_eax(0x4200);
    m.cpu.registers.set_edx(0xE0);
    m.handle_int13();

    assert_eq!(dos_int_flags(&m) & 1, 0, "the extended read must succeed");
    assert_eq!(
        m.read_physical_u8(super::margo::UMB_FRAME_LOW + 0x0e60),
        0xFA,
        "the head of the sector must follow the first page's mapping"
    );
    // Guest linear C8E60h + 512 is C9060h, in the second page.
    assert_eq!(
        m.read_physical_u8(super::margo::UMB_FRAME_HIGH + 0x60),
        0xA5,
        "the tail of the sector must follow the second page's mapping"
    );
    assert_eq!(
        m.read_physical_u8(dap + 2),
        1,
        "the packet's block count must be rewritten in the mapped frame"
    );
}

/// El Torito AH=4Bh returns its 19-byte specification packet at DS:SI.
#[test]
fn el_torito_status_packet_lands_in_a_non_identity_mapped_caller_buffer() {
    let mut m = int15_machine(16);
    m.mount_cd(el_torito_iso(2));
    m.write_physical_u8(BIOS_BOOT_CHOICE_ADDR, 2);
    m.handle_int19();
    super::margo::install_umb_paging(&mut m);
    prime_dos_int_frame(&mut m);
    assert_eq!(
        m.read_physical_u32(super::margo::UMB_BUFFER_PHYSICAL),
        0,
        "precondition: the mapped frame must be clear"
    );

    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0xc8c6));
    m.cpu.registers.set_esi(0);
    m.cpu.registers.set_eax(0x4B00);
    m.cpu.registers.set_edx(0);
    m.handle_int13();

    assert_eq!(
        m.read_guest_block(super::margo::UMB_BUFFER_PHYSICAL, 3),
        vec![19, 2, 0],
        "packet size, floppy-emulation media and emulated drive must arrive \
         at the mapped frame"
    );
    assert!(m.eltorito_emulation.is_none());
}
