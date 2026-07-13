// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// `DEVICE=C:\DOS\TOKAEMM.SYS` puts the running kernel into V86
/// under TOKAEMM's ring-0 monitor at SYSINIT, and real FreeDOS still finishes
/// booting to C:\> — every instruction and hardware IRQ from the DEVICE= line
/// onward runs virtualized. The gate: the DOS prompt reaches the screen.
///
/// CONFIG.SYS and TOKAEMM.SYS are both passed as `mount_hdd_folder_with`
/// overrides (which replace/append onto the committed system files). The host
/// `dir` stays empty: a CONFIG.SYS written there would collide with the
/// system CONFIG.SYS whose 8.3 name is reserved first, and lose the `~n` fold.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn tokaemm_m0_freedos_survives_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_t3a_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    // The stock CONFIG.SYS (from the committed image) plus a DEVICE= line for
    // the bespoke driver. Passed as an override so it replaces the system copy.
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine.run_until_halt_or_cycles(500_000_000);
    let text = machine.screen_text().as_text();
    // FreeDOS boots to the C:\> prompt with the whole system running in V86
    // under TOKAEMM's monitor (SYSINIT + FreeCOM + every IRQ virtualized).
    if !text.to_lowercase().contains("c:\\>") {
        std::fs::remove_dir_all(&dir).ok();
        panic!("FreeDOS did not reach C:\\> in V86 (stop={stop:?}).\n{text}");
    }

    // Run a command at the virtualized prompt: type `VER` and confirm the shell
    // executes it and returns to a fresh prompt — interactive DOS in V86.
    for ch in "ver\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        let _ = machine.run_until_halt_or_cycles(20_000_000);
    }
    let _ = machine.run_until_halt_or_cycles(60_000_000);
    let after = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    let prompts = after.to_lowercase().matches("c:\\>").count();
    assert!(
        prompts >= 2,
        "VER did not run at the V86 prompt (expected a second C:\\>).\n{after}"
    );
}

#[test]
#[ignore = "boots three full DOS images in V86 (slow in debug); run with --ignored"]
fn tokaemm_small_ram_layouts_do_not_expose_out_of_range_pools() {
    for memory_mib in [1, 2, 4] {
        let dir = std::env::temp_dir().join(format!(
            "tokaemm_small_{memory_mib}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let config = b"FILES=20\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:1024 /P=C:\\AUTOEXEC.BAT\r\n"
            .to_vec();
        let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVER\r\n".to_vec();
        let profile = MachineProfile::gsw_386(memory_mib, VideoCard::Vega);
        let mut machine =
            Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
        machine
            .mount_hdd_folder_with(
                &dir,
                vec![
                    ("CONFIG.SYS".to_string(), config),
                    ("AUTOEXEC.BAT".to_string(), autoexec),
                    (
                        "TOKAEMM.SYS".to_string(),
                        izarravm_firmware::tokaemm_sys().to_vec(),
                    ),
                ],
            )
            .expect("mount host folder with overrides");

        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("machine run");
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        if let StopReason::CpuError(msg) = &stop {
            panic!("TOKAEMM faulted with {memory_mib} MiB: {msg}\n{text}");
        }
        assert!(
            text.to_ascii_lowercase().contains("c:\\>"),
            "Toka-DOS did not reach a prompt with {memory_mib} MiB (stop={stop:?}).\n{text}"
        );
    }
}

/// A guest program install-checks XMS, allocates a 64 KB EMB,
/// locks it, moves a pattern conventional->EMB->conventional, verifies it, then
/// unlocks and frees — all in V86 under TOKAEMM's monitor (block MOVE traps to
/// the monitor's flat memcpy). XMSTEST.COM signals 0xA5 (success) via the
/// unit-tester exit port; any other code names the step that broke.
///
/// The config is NOEMS so host EMS reserves no extended RAM and the guest XMS
/// driver owns all of it. EMS coexistence is covered separately.
/// XMSTEST runs from AUTOEXEC, so the machine stops as soon as it signals — no
/// interactive settling needed.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m1_xms_alloc_move_free_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m1_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nXMSTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "XMSTEST.COM".to_string(),
                    izarravm_firmware::xmstest_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "XMS round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// With DEVICE=TOKAEMM.SYS + DOS=UMB, a guest program sets
/// the high-first allocation strategy and AH=48h-allocates a block that lands in
/// upper memory (segment >= 0xC800) with real RAM behind it (write/read a
/// pattern) — proving TOKAEMM page-mapped extended RAM into the upper holes and
/// FreeDOS's DOS=UMB linked our region. UMBTEST signals 0xA5 via the exit port;
/// a 0xEn code names the failed step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m3_umb_load_high_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m3_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDOS=UMB\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "UMBTEST.COM".to_string(),
                    izarravm_firmware::umbtest_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "UMB load-high did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// Drives TOKAEMM's XMS 10h/11h/12h directly (no
/// DOS=UMB) to exercise the allocator paths the DOS=UMB e2e doesn't reach — the
/// too-big probe, alloc, grow, release, reuse-after-free — plus a write/read of
/// the paged RAM. UMBMECH signals 0xA5; a 0xEn code names the failed step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m3_umb_direct_xms_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m3d_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBMECH\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "UMBMECH.COM".to_string(),
                    izarravm_firmware::umbmech_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "UMB mechanism round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// With DEVICE=TOKAEMM.SYS RAM, a guest program drives the
/// LIM EMS 4.0 API — version, frame segment, page counts, allocate — then maps
/// logical pages through the frame slots, writing distinct patterns and reading
/// them back through OTHER slots: the runtime page remap through the paged
/// frame, serviced by the monitor's INT 0xC0 'PM' PTE-rewrite. EMSTEST signals
/// 0xA5 via the exit port; a 0xEn code names the failed step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_ems_map_write_read_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m2_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMSTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "EMSTEST.COM".to_string(),
                    izarravm_firmware::emstest_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "EMS map/write/read round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// With DEVICE=TOKAEMM.SYS RAM and DOS=UMB, the UMB
/// window ends below the EMS page frame (umb_win_end = 0xE000) and DOS=UMB
/// still links and allocates upper memory from the carved window — the frame
/// and the UMBs share the upper area under the guest driver's own bookkeeping.
/// Reuses the UMBTEST fixture (seg >= 0xC800 + write/read pattern).
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_umb_coexists_with_ems_frame_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m2u_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDOS=UMB\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "UMBTEST.COM".to_string(),
                    izarravm_firmware::umbtest_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "UMB alongside the EMS frame did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// DEVICE=TOKAEMM.SYS NOEMS
/// presents a FRAMELESS manager — INT 67h answers present/version 4.0, the
/// frame query returns 80h, page counts are zero, and allocation is refused
/// with 87h (the EMM386 NOEMS contract). EMSNONE signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_ems_frameless_noems_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m2f_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMSNONE\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "EMSNONE.COM".to_string(),
                    izarravm_firmware::emsnone_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "frameless-default EMS contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI presence under DEVICE=TOKAEMM.SYS NOEMS (frameless mode,
/// no EMS pool — the stock-boot shape), INT 67h AX=DE00h answers VCPI 1.0
/// present (AH=0, BX=0100h), a not-yet-implemented DExx subfunction
/// answers 8Fh, untouched registers survive the call, and the plain EMS
/// interface keeps working on the shared vector. VCPIDET signals
/// 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m0_de00_present_on_frameless_noems() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi0_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIDET\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPIDET.COM".to_string(),
                    izarravm_firmware::vcpidet_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "VCPI DE00 presence contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI queries and page-pool behavior under DEVICE=TOKAEMM.SYS NOEMS. The
/// DE02-DE0B set answers — free-page count over a real pool, max-page
/// query, alloc/free round-trip with 12-LSB masking, bad-free and
/// double-free rejection, V86 page-table lookups (identity + out-of-range
/// 8Bh), CR0 with PE|PG, the debug-register array shape, and the 8259
/// mapping report/record round-trip. VCPIMEM signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m1_queries_and_page_pool() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi1_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIMEM\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPIMEM.COM".to_string(),
                    izarravm_firmware::vcpimem_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "VCPI query/page-pool contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// Under DEVICE=TOKAEMM.SYS NOEMS, VCPI DE01 Get Protected Mode
/// Interface fills the client page-table buffer (identity first-MB
/// entries, software bits 9-11 cleared, exactly 0x110 entries, DI
/// advanced), furnishes the three server GDT descriptors (32-bit CPL0
/// code / flat-4GB data / driver data sharing the code base), and
/// returns a nonzero in-segment PM entry offset. VCPIIF signals
/// 0xA5 / 0xEn. The protected-mode entry is exercised by the switch
/// fixture because it can only be far-called from protected mode.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m2_de01_pm_interface() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi2_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIIF\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPIIF.COM".to_string(),
                    izarravm_firmware::vcpiif_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "VCPI DE01 interface contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// A minimal real VCPI client uses DE0C to walk the full extender
/// lifecycle under DEVICE=TOKAEMM.SYS NOEMS: DE01 interface setup,
/// DE0C into 16-bit protected mode under its own CR3/GDT/TSS (the
/// JEMM-traced switch flow), far-calls to the server PM entry (DE03
/// equal to the V86 baseline, DE04/DE05 round-trip), DE0C back to V86,
/// with marker registers proving the spec's register-preservation
/// contract across both switches and the pool balanced at the end.
/// VCPISW signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m3_de0c_switch_round_trip() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi3_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPISW\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPISW.COM".to_string(),
                    izarravm_firmware::vcpisw_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "VCPI switch round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step (0xEF = DE0C returned).\n{text}"
    );
}

/// A V86 program hooks INT 0Dh and
/// executes a privileged instruction the monitor does not emulate (the
/// literal DOS16M o32 LGDT startup shape) receives its own reflected
/// fault with fault-IP semantics and can skip-and-resume — instead of
/// the old signal32 diagnostic abort. GPREFLCT signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m4_unhandled_gp_reflects_to_guest() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi4_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGPREFLCT\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "GPREFLCT.COM".to_string(),
                    izarravm_firmware::gpreflct_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "V86 #GP reflection contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI privileged-0F emulation for the 386MAX GP_ESCOD surface: a V86
/// task executes MOV r32,CR0/CR3/CR2, MOV CR0,r32 (with PE|PG cleared in
/// the source — the monitor must force them back on), CLTS, and LMSW —
/// all #GP at CPL 3 — and the monitor must EMULATE them transparently
/// (the extender CR0-probe path) instead of reflecting a fault.
/// GPEMUL signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m6_privileged_0f_emulation() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi6_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGPEMUL\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "GPEMUL.COM".to_string(),
                    izarravm_firmware::gpemul_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "V86 privileged-0F emulation did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// A fresh empty user folder gets the current defaults seeded
/// (`ensure_user_config`): DEVICE=TOKAEMM.SYS RAM + DOS=HIGH,UMB + LH
/// TOKAMOUS — and the boot reaches a C:\> prompt RUNNING IN V86 under the
/// TOKAEMM monitor, with the driver's signon banner on screen.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m4_default_boot_runs_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m4_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");

    // The seeding wrote real, editable defaults into the user folder.
    let seeded = std::fs::read_to_string(dir.join("CONFIG.SYS")).expect("seeded CONFIG.SYS");
    assert!(
        seeded.contains("DEVICE=C:\\DOS\\TOKAEMM.SYS RAM") && seeded.contains("DOS=HIGH,UMB"),
        "seeded CONFIG.SYS lacks the expected defaults:\n{seeded}"
    );

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during the default V86 boot: {msg}\n{text}");
    }
    let text = machine.screen_text().as_text();
    let lower = text.to_ascii_lowercase();
    // The cycle budget can expire while the CPU is transiently inside the
    // ring-0 monitor (a reflected IRQ), where in_v86() reads false on a
    // healthy boot. Re-sample over a few short bursts rather than
    // asserting one instant.
    let mut in_v86 = machine.in_v86();
    for _ in 0..4 {
        if in_v86 {
            break;
        }
        machine
            .run_until_halt_or_cycles(1_000_000)
            .expect("machine re-sample run");
        in_v86 = machine.in_v86();
    }
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt on the default boot (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("tokaemm:"),
        "the TOKAEMM signon banner is missing.\n{text}"
    );
    assert!(
        in_v86,
        "the default boot must leave the guest running in V86 (stop={stop:?}).\n{text}"
    );

    // Presentation leak guard (audit item 9): run `ver /w` at the live prompt,
    // which used to print FreeDOS/Tim-Norman/sourceforge.net copyright text
    // straight from FreeCOM's DEFAULT.lng. The whole in-universe boot+shell
    // transcript (banner through the VER output) must stay leak-free.
    for ch in "ver /w\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        let _ = machine.run_until_halt_or_cycles(20_000_000);
    }
    let _ = machine.run_until_halt_or_cycles(60_000_000);
    let ver_text = machine.screen_text().as_text();
    let ver_lower = ver_text.to_ascii_lowercase();
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        !ver_lower.contains("freedos"),
        "boot/VER transcript leaks \"FreeDOS\" branding.\n{ver_text}"
    );
    assert!(
        !ver_lower.contains("sourceforge"),
        "boot/VER transcript leaks a sourceforge.net URL.\n{ver_text}"
    );
}

/// Code 3 boots as 386-slow with TOKAEMM resident and DOS=HIGH,UMB. AUTOEXEC
/// checks the removed 286 name, selects 386-slow by its canonical name, runs
/// VER, then switches to 586. The commands come from AUTOEXEC.BAT because the
/// default keyboard layout can garble injected punctuation.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_and_gswmode_support_code_3_as_386_slow() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_gsw386slow_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=D\r\n\
DEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\nDOS=HIGH,UMB\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGSWMODE 286\r\n\
GSWMODE 386-slow\r\nVER\r\nGSWMODE 586\r\n"
        .to_vec();

    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw386Slow;
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "GSWMODE.COM".to_string(),
                    izarravm_firmware::gswmode_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!(
            "CPU fault after the GSWMODE 386-slow switch while TOKAEMM's ring-0 \
                 monitor was resident: {msg}\nstop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "GSWMODE 386-slow then GSWMODE 586 should leave the machine at 586 \
             (stop={stop:?}).\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("tokaemm: xms/umb/ems memory manager; system running in v86"),
        "TOKAEMM did not install while code 3 was active (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("switched to 386-slow") && lower.contains("switched to 586"),
        "GSWMODE confirmation output missing for one of the two switches.\n{text}"
    );
    assert!(
        lower.contains("cpu mode '286' was removed; use '386-slow'"),
        "GSWMODE did not explain how to migrate the removed 286 name.\n{text}"
    );
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the GSWMODE 386-slow/VER/GSWMODE 586 sequence \
             (stop={stop:?}).\n{text}"
    );
}

/// Audit item 10: the vendored FreeDOS MEM (toka-dos/freedos/mem) runs under
/// the default V86 boot and both `MEM` and `MEM /P` produce sane output.
/// Toka-DOS diverges from upstream MEM here: upstream's `/P` is only a
/// prefix of `/PAGE` (pause after each screenful); the owner's spec wants
/// `/P` to list resident programs with size + segment, so mem2.c's main()
/// was patched to make `/PAGE` (and therefore `/P`) imply `/FULL` and omit
/// the summary unless `/SUMMARY` is given (see toka-dos/freedos/VENDOR.md).
/// Each invocation gets its own boot because the 25-row text console cannot
/// hold both outputs at once. AUTOEXEC.BAT drives the commands, with no
/// injected typing.
struct MemScreen {
    text: String,
    columns: usize,
    cells: Vec<(u8, u8)>,
}

fn run_mem_autoexec(dir_suffix: &str, commands: &str) -> (MemScreen, StopReason) {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_mem_{dir_suffix}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let autoexec =
        format!("@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\n{commands}\r\n").into_bytes();
    let profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(&dir, vec![("AUTOEXEC.BAT".to_string(), autoexec)])
        .expect("mount host folder with overrides");

    // /P retains upstream's /PAGE pausing behavior on top of the Toka-DOS
    // /FULL addition, so a long listing (like the per-program table) may
    // stop at a "Press <Enter> to continue" pager prompt. Run in a few
    // short bursts, injecting Enter between them: harmless once the boot
    // has already reached the next C:\> prompt, but dismisses the pager
    // (if hit) so the run always makes it back to a prompt.
    let mut stop = machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("machine run");
    for _ in 0..4 {
        if matches!(stop, StopReason::CpuError(_)) {
            break;
        }
        machine.inject_key_scancodes(&[0x1c, 0x9c]); // Enter: dismiss any pager
        stop = machine
            .run_until_halt_or_cycles(150_000_000)
            .expect("machine re-run");
    }
    let frame = machine.screen_text();
    let screen = MemScreen {
        text: frame.as_text(),
        columns: frame.columns,
        cells: frame
            .cells
            .iter()
            .map(|cell| (cell.character, cell.attribute))
            .collect(),
    };
    std::fs::remove_dir_all(&dir).ok();
    (screen, stop)
}

fn run_mem_command(dir_suffix: &str, mem_args: &str) -> (MemScreen, StopReason) {
    run_mem_autoexec(dir_suffix, &format!("MEM {mem_args}"))
}

fn memory_map_rows(screen: &MemScreen) -> Vec<&[(u8, u8)]> {
    screen
        .cells
        .chunks_exact(screen.columns)
        .filter_map(|row| {
            let map = &row[..79];
            (map.iter()
                .all(|(character, _)| matches!(*character, 0xB0 | 0xB2))
                && row[79].0 == b' ')
                .then_some(map)
        })
        .collect()
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_plain_reports_conventional_memory() {
    let (screen, stop) = run_mem_command("plain", "");
    let text = &screen.text;
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running MEM under V86: {msg}\n{text}");
    }
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after MEM ran (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("conventional"),
        "MEM output doesn't mention conventional memory (stop={stop:?}).\n{text}"
    );
    assert!(
        !lower.contains("ems internal error"),
        "MEM could not enumerate TokaEMM's EMS handles.\n{text}"
    );
    for (label, total) in [
        ("Conventional", "640K"),
        ("Upper", "384K"),
        ("Expanded (EMS)", "3,072K"),
        ("Extended (XMS)", "20,480K"),
    ] {
        let line = text
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("MEM row {label:?} missing.\n{text}"));
        assert!(
            line.contains(total),
            "MEM row {label:?} has the wrong total.\n{text}"
        );
    }

    let rows = memory_map_rows(&screen);
    assert_eq!(rows.len(), 4, "MEM map should occupy four rows.\n{text}");
    let map = rows.into_iter().flatten().copied().collect::<Vec<_>>();
    assert_eq!(map.len(), 316);
    assert!(
        map.iter()
            .all(|(character, _)| matches!(*character, 0xB0 | 0xB2))
    );
    assert!(map.iter().any(|(character, _)| *character == 0xB0));
    assert!(map.iter().any(|(character, _)| *character == 0xB2));
    for (range, attribute) in [(0..8, 0x09), (8..13, 0x0B), (13..53, 0x0D), (53..316, 0x0A)] {
        assert!(
            map[range.clone()]
                .iter()
                .all(|(_, actual)| *actual == attribute),
            "MEM map range {range:?} should use attribute {attribute:#04x}.\n{text}"
        );
    }
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_redirect_keeps_raw_uncolored_bars() {
    let (screen, stop) = run_mem_autoexec("redirect", "MEM > C:\\MEM.TXT\r\nTYPE C:\\MEM.TXT");
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while redirecting MEM under V86: {msg}\n{}",
            screen.text
        );
    }
    let rows = memory_map_rows(&screen);
    assert_eq!(
        rows.len(),
        4,
        "redirected MEM map should occupy four rows.\n{}",
        screen.text
    );
    let map = rows.into_iter().flatten().copied().collect::<Vec<_>>();
    assert_eq!(map.len(), 316);
    assert!(
        map.iter()
            .all(|(character, _)| matches!(*character, 0xB0 | 0xB2))
    );
    assert!(
        map.iter()
            .all(|(_, attribute)| !matches!(*attribute, 0x09 | 0x0A | 0x0B | 0x0D))
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_p_lists_resident_programs() {
    let (screen, stop) = run_mem_command("p", "/P");
    let text = &screen.text;
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running MEM /P under V86: {msg}\n{text}");
    }
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after MEM /P ran (stop={stop:?}).\n{text}"
    );
    // Toka-DOS divergence check: /P must produce the per-program size and
    // segment listing (upstream /P is only pagination). TOKAMOUS was loaded
    // high right before MEM ran, so it must appear in that listing. /P omits
    // the large summary unless /SUMMARY is also specified, which keeps the
    // final program rows visible.
    let upper = text.to_ascii_uppercase();
    assert!(
        upper.contains("TOKAMOUS"),
        "MEM /P output doesn't list the resident TOKAMOUS module \
             (stop={stop:?}).\n{text}"
    );
    assert!(
        !lower.contains("memory map:"),
        "bare MEM /P should leave the summary out.\n{text}"
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_p_summary_restores_memory_map() {
    let (screen, stop) = run_mem_command("p_summary", "/P /SUMMARY");
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while running MEM /P /SUMMARY under V86: {msg}\n{}",
            screen.text
        );
    }
    assert!(
        screen.text.to_ascii_lowercase().contains("memory map:"),
        "MEM /P /SUMMARY should restore the memory summary.\n{}",
        screen.text
    );
    assert_eq!(
        memory_map_rows(&screen).len(),
        4,
        "MEM /P /SUMMARY should restore all four map rows.\n{}",
        screen.text
    );
}

/// Regression for the V86 IRET/IOPL gate (vorvek/v86-iret-iopl): TOKAEMM
/// virtualizes IF by trapping CLI/STI/PUSHF/POPF/INT n/IRET to the monitor
/// and stamping the guest IRET frame's image-IF from its own VIF (often 0 in
/// ISR context). If IRET is not IOPL-gated like its siblings, a V86 guest's
/// own IRET pops that monitor-stamped image straight into REAL EFLAGS via
/// load_flags (no IOPL gating) -- killing real IF inside V86 so interrupts
/// never deliver again (this was the Prince of Persia livelock root cause).
/// This test samples real IF at several points across a real TOKAEMM boot
/// and asserts it is never 0 while the guest is in V86 mode -- the invariant
/// that would have caught this whole class of bug. Cheap: reuses the MEM
/// harness's boot (LH TOKAMOUS + MEM reaches a prompt in ~200-350M cycles),
/// split into small bursts so the sample points fall throughout the run
/// rather than only at the very end.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_real_if_never_zero_in_v86_across_a_boot() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_ifinvariant_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\nMEM\r\n".to_vec();
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(&dir, vec![("AUTOEXEC.BAT".to_string(), autoexec)])
        .expect("mount host folder with overrides");

    const FLAG_IF: u32 = 0x0000_0200;
    const BURST: u64 = 20_000_000;
    const BURSTS: u32 = 25; // 500M cycles total, well past the MEM prompt

    let mut saw_v86 = false;
    let mut stop = StopReason::CycleLimit { requested: 0 };
    for _ in 0..BURSTS {
        if matches!(stop, StopReason::CpuError(_)) {
            break;
        }
        stop = machine
            .run_until_halt_or_cycles(BURST)
            .expect("machine run");
        if machine.in_v86() {
            saw_v86 = true;
            assert_ne!(
                machine.cpu().registers.eflags & FLAG_IF,
                0,
                "real IF was 0 while the guest was in V86 mode (stop={stop:?})"
            );
        }
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault during the IF-invariant boot: {msg}\n{text}");
    }
    assert!(
        saw_v86,
        "the boot never entered V86 mode; the invariant was never exercised"
    );
}

/// Audit items 3+10 external tool batch (toka-dos/freedos/VENDOR.md): smoke
/// tests three of the newly-vendored tools in one boot -- ATTRIB (set +
/// query the read-only flag), CHOICE (piped default answer), and FIND
/// (string match against a text file) -- each producing assertable screen
/// output. The rest of the batch (MORE, LABEL, DELTREE) are covered by "the
/// image builds and boots" (the default-boot e2e test above stays green).
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_tool_batch_attrib_choice_find_smoke() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_toolbatch_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    // A two-line text file so FIND's match is unambiguous against the
    // non-matching line right next to it.
    let hello_txt = b"Hello from Toka-DOS\r\nWelcome to the IZARRA 3000\r\n".to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\n\
ATTRIB +R HELLO.TXT\r\n\
ATTRIB HELLO.TXT\r\n\
ECHO Y | CHOICE /C:YN Continue\r\n\
FIND \"IZARRA\" HELLO.TXT\r\n"
        .to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("AUTOEXEC.BAT".to_string(), autoexec),
                ("HELLO.TXT".to_string(), hello_txt),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(400_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running the tool batch under V86: {msg}\n{text}");
    }

    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the tool batch ran (stop={stop:?}).\n{text}"
    );

    // ATTRIB: the second invocation (plain query, no +/-) must show the R
    // flag the first invocation just set. Attribute column order is
    // D,H,S,R,A (attr2str in ATTRIB.C), so a read-only, non-hidden,
    // non-system, archived file prints "[---RA]".
    let upper = text.to_ascii_uppercase();
    assert!(
        upper.contains("[---RA]"),
        "ATTRIB HELLO.TXT didn't show the R flag set by ATTRIB +R \
             (stop={stop:?}).\n{text}"
    );

    // CHOICE: piped "Y" must be accepted (not left hanging on a prompt);
    // the prompt text itself must have appeared on screen.
    assert!(
        upper.contains("CONTINUE"),
        "CHOICE prompt text didn't appear on screen (stop={stop:?}).\n{text}"
    );

    // FIND: must print the matching line, not the non-matching one.
    assert!(
        upper.contains("IZARRA 3000"),
        "FIND didn't print the matching line (stop={stop:?}).\n{text}"
    );
}

/// XCOPY (toka-dos/tools-src/xcopy/xcopy.c, an original Toka-DOS project
/// tool, not vendored -- see toka-dos/msdos4/VENDOR.md): builds a small
/// source tree (a top-level file plus a subdirectory with its own file),
/// copies it recursively with `/S /Y`, then verifies the copy landed at
/// the right depth (TYPE on the nested file) and that DIR + the XCOPY
/// summary line both show up on screen.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_tool_xcopy_recursive_smoke() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_xcopy_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\n\
MD SRC\r\n\
ECHO hello > SRC\\A.TXT\r\n\
MD SRC\\SUB\r\n\
ECHO world > SRC\\SUB\\B.TXT\r\n\
XCOPY SRC DEST /S /Y\r\n\
TYPE DEST\\SUB\\B.TXT\r\n\
DIR DEST\r\n"
        .to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(&dir, vec![("AUTOEXEC.BAT".to_string(), autoexec)])
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running the XCOPY batch under V86: {msg}\n{text}");
    }

    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the XCOPY batch ran (stop={stop:?}).\n{text}"
    );

    let upper = text.to_ascii_uppercase();

    // TYPE DEST\SUB\B.TXT: the nested file was copied to the right depth
    // and its contents are intact.
    assert!(
        lower.contains("world"),
        "TYPE didn't print the recursively-copied nested file's contents \
             (stop={stop:?}).\n{text}"
    );

    // DIR DEST: the top-level copied file and the copied subdirectory
    // both show up in the destination.
    assert!(
        upper.contains("A.TXT") && upper.contains("SUB"),
        "DIR DEST didn't list the copied file and subdirectory \
             (stop={stop:?}).\n{text}"
    );

    // XCOPY prints a final "N File(s) copied" summary; two files (A.TXT,
    // SUB\B.TXT) were copied.
    assert!(
        upper.contains("2 FILE(S) COPIED"),
        "XCOPY's File(s) copied summary line didn't show the expected count \
             (stop={stop:?}).\n{text}"
    );
}

/// The PS/2 mouse works under the default V86 boot. A host-injected
/// wheel detent travels 8042 -> slave IRQ12 -> vector 0x74 -> the monitor's
/// slave reflect stub -> guest INT 74h -> TOKAMOUS (loaded HIGH) -> INT 33h
/// fn 03h, where MOUSETST polls it. Signals 0xA5; a 0xEn names the step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m4_mouse_wheel_under_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m4m_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\nMOUSETST\r\n".to_vec();
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "MOUSETST.COM".to_string(),
                    izarravm_firmware::mousetst_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    // Run in chunks, injecting a wheel detent between them: the fixture polls
    // fn 03h in a bounded loop, so extra/early detents are harmless and a late
    // boot still sees one.
    let mut stop = machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("machine run");
    for _ in 0..10 {
        if matches!(stop, StopReason::TestExit { .. } | StopReason::CpuError(_)) {
            break;
        }
        machine.inject_mouse_wheel(1);
        stop = machine
            .run_until_halt_or_cycles(200_000_000)
            .expect("machine run");
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "mouse wheel under V86 did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// Under V86, SB16 IRQ5 lands on vector 13, shared with #GP,
/// and the monitor's discriminator must route each correctly. SNDTST hooks
/// INT 0Dh, resets the DSP, then requests immediate 8-bit IRQs (DSP 0xF2)
/// inside a CLI/STI-dense loop. Signals 0xA5; a 0xEn names the step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m4_sb16_irq5_under_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m4s_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nSNDTST\r\n".to_vec();
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "SNDTST.COM".to_string(),
                    izarravm_firmware::sndtst_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "SB16 IRQ5 under V86 did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// V86 trap tax regression: IRQ5 delivered while the interrupted code sits
/// at IP == 0. The vec13 frame-shape check cannot decide this case alone --
/// the error-code slot reads 0 for a #GP AND for an IRQ frame whose return
/// EIP is 0 -- so the monitor must fall through to its opcode-peek + cold
/// PIC-probe layers. A slot-only discriminator mis-routed such a delivery
/// into the #GP path, hit the non-sensitive byte at CS:0, and hard-killed
/// the VM (the review probe); this pins the three-layer scheme.
///
/// IRQ5IP0 makes IP == 0 the common case with SB16 auto-init DMA (NOT the
/// one-shot DSP 0xF2, whose re-arm races the ISR -- see the fixture header):
/// once armed, the DMA block boundary raises IRQ5 continuously on the card's
/// own schedule while the guest simply parks on a `jmp $` at offset 0 of a
/// segment, so deliveries land at IP == 0 with no re-arm. This test is RED
/// on the buggy slot-only monitor (the VM dies, a foreign TestExit code) and
/// GREEN only on the three-layer fix.
#[test]
#[ignore = "boots a full FreeDOS image (slow); run with --ignored"]
fn tokaemm_irq5_at_ip0_discriminated_under_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_ip0_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nIRQ5IP0\r\n".to_vec();
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "IRQ5IP0.COM".to_string(),
                    izarravm_firmware::irq5ip0_com().to_vec(),
                ),
            ],
        )
        .expect("mount host folder with overrides");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "IRQ5 at IP==0 under V86 did not report success (stop={stop:?}); \
             0xE1 = DSP reset failed, a hang/CycleLimit or a foreign TestExit \
             code means the discriminator mis-routed the delivery.\n{text}"
    );
}
