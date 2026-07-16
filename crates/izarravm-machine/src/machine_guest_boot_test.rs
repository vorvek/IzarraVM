// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn sound_blaster_env_entries_default_config() {
    let entries = sound_blaster_env_entries(&SoundBlasterConfig::default());
    assert_eq!(
        entries,
        vec![
            ("BLASTER".to_string(), "A220 I5 D1 H5 P300 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I5 D1 H5 P300 T6".to_string()),
        ]
    );
}

#[test]
fn sound_blaster_env_entries_non_default_routing() {
    let config = SoundBlasterConfig {
        enabled: true,
        irq: SbIrq::I7,
        dma: SbDma8::D3,
        high_dma: SbDma16::D5,
    };
    assert_eq!(
        sound_blaster_env_entries(&config),
        vec![
            ("BLASTER".to_string(), "A220 I7 D3 H5 P300 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I7 D3 H5 P300 T6".to_string()),
        ]
    );
}

#[test]
fn sound_blaster_env_entries_disabled_omits_the_string() {
    let config = SoundBlasterConfig {
        enabled: false,
        ..SoundBlasterConfig::default()
    };
    assert!(sound_blaster_env_entries(&config).is_empty());
}

#[test]
fn new_raw_program_seeds_psp_env_pointer_with_blaster() {
    // A trivial exit-only program is enough: the env is seeded at load.
    let com: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21];
    let machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    let env_seg = psp_env_segment(&machine);
    assert_ne!(env_seg, 0, "PSP:0x2C must name the env segment");
    // The env data sits one paragraph above the 64 KiB .COM program block
    // (PSP:0x02), past the env block's reserved MCB header.
    let prog_top = machine
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)
        .unwrap();
    assert_eq!(env_seg, prog_top + 1);
    assert_eq!(
        parse_env_block(&machine, env_seg),
        vec![
            ("BLASTER".to_string(), "A220 I5 D1 H5 P300 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I5 D1 H5 P300 T6".to_string()),
        ]
    );
}

#[test]
fn dos_env_block_carries_the_configured_routing() {
    // A non-default routing (IRQ7 / DMA3) flows from the host config through
    // the loader into the env block a guest scans via PSP:0x2C.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster = SoundBlasterConfig {
        enabled: true,
        irq: SbIrq::I7,
        dma: SbDma8::D3,
        high_dma: SbDma16::D5,
    };
    let machine = Machine::new_raw_program(profile, &[0xb8, 0x00, 0x4c, 0xcd, 0x21]).unwrap();
    let env_seg = psp_env_segment(&machine);
    assert_ne!(env_seg, 0, "PSP:0x2C must name the env segment");
    assert_eq!(
        parse_env_block(&machine, env_seg),
        vec![
            ("BLASTER".to_string(), "A220 I7 D3 H5 P300 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I7 D3 H5 P300 T6".to_string()),
        ]
    );
}

#[test]
fn keyboard_rom_echoes_injected_keys_to_the_screen() {
    let profile = MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::kbd_bios()).unwrap();
    // Let the ROM run its init (install vectors, unmask IRQ1, STI, enter loop).
    machine.run_until_halt_or_cycles(200_000).unwrap();
    // Inject 'h' then 'i' (Set 1 make+break for H=0x23, I=0x17).
    machine.inject_key_scancodes(&[0x23, 0xa3, 0x17, 0x97]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    let screen = machine.screen_text();
    assert!(
        screen.line_string(0).starts_with("hi"),
        "screen line 0 was {:?}",
        screen.line_string(0)
    );
}

#[test]
fn dos_machine_routes_irq1_to_the_keyboard_isr() {
    // A do-nothing program that just spins (jmp $) so the machine keeps running.
    // org 0x100: jmp $  (EB FE)
    let com: &[u8] = &[0xeb, 0xfe];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    machine.inject_key_scancodes(&[0x1e, 0x9e]); // 'a' make + break
    machine.run_until_halt_or_cycles(200_000).unwrap();
    // The real INT 09h ISR should have moved 'a' into the BDA ring.
    let head = machine.memory_read_u16_for_test(0x41a);
    let tail = machine.memory_read_u16_for_test(0x41c);
    assert_ne!(head, tail, "ISR enqueued a key into the BDA ring");
}

#[test]
fn dos_program_reads_typed_keys_through_int21() {
    // org 0x100: read two chars with AH=01 (each echoes to stdout), then exit.
    //   mov ah,1 / int 21h / mov ah,1 / int 21h / mov ax,4c00h / int 21h
    let com: &[u8] = &[
        0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    // Type 'h' then 'i' as Set 1 make+break (H=0x23, I=0x17).
    machine.inject_key_scancodes(&[0x23, 0xa3, 0x17, 0x97]);
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"hi");
}

#[test]
fn tokados_sndtst_delivers_sb_irq5_under_v86() {
    let dir = std::env::temp_dir().join(format!("katea_sndtst_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.set_cmos_byte(0x11, 1); // disk-first
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "SNDTST.COM".to_string(),
                    izarravm_firmware::sndtst_com().to_vec(),
                ),
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nSNDTST\r\n".to_vec(),
                ),
            ],
        )
        .unwrap();

    let reason = machine.run_until_halt_or_cycles(250_000_000).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        reason,
        StopReason::TestExit { code: 0xA5 },
        "SNDTST.COM should complete under TOKAEMM V86, got {reason:?}"
    );
}

#[test]
fn tokados_vcpi_de0b_remaps_sb_irq5_vector() {
    let dir = std::env::temp_dir().join(format!("katea_vcpipic_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.set_cmos_byte(0x11, 1); // disk-first
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                ("VCPIPIC.COM".to_string(), VCPIPIC_COM.to_vec()),
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nVCPIPIC\r\n".to_vec(),
                ),
            ],
        )
        .unwrap();

    let reason = machine.run_until_halt_or_cycles(250_000_000).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        reason,
        StopReason::TestExit { code: 0xA5 },
        "VCPIPIC.COM should receive SB IRQ5 on remapped vector 25h, got {reason:?}"
    );
}

#[test]
fn protected_mode_sb_dma_irq5_reaches_client_idt() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine =
            Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PMIRQ5_COM)
                .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 4)
            .unwrap();
        assert!(
            matches!(reason, StopReason::TestExit { code: 0xA5 }),
            "{mode:?}: protected-mode SB IRQ5 fixture stopped with {reason:?}"
        );
    }
}

#[test]
fn lotura_reports_id_and_switches_mode_live() {
    // org 0x100: mov al,2; out 0xe1,al; mov ax,4c00h; int 21h
    let com: &[u8] = &[0xb0, 0x02, 0xe6, 0xe1, 0xb8, 0x00, 0x4c, 0xcd, 0x21];
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, izarravm_core::VideoCard::Vega),
        com,
    )
    .unwrap();
    assert_eq!(machine.active_mode(), GswMode::Gsw386); // boot mode
    let id = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e0, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(id, LOTURA_ID_VALUE);
    let code = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e1, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(code, 0);
    // An out-of-range write records no pending switch.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x00e1, BusWidth::Byte, 9, false).unwrap()
    });
    assert!(machine.pending_mode.is_none());
    assert_eq!(machine.active_mode(), GswMode::Gsw386);
    // Running the program writes 2 to 0xE1; the run loop applies the live switch.
    machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(machine.active_mode(), GswMode::Gsw586);
    let code = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e1, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(code, 2);
}

#[test]
fn izarra_bios_post_publishes_result_block() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    // The full-screen RLE background blit delays the POST step loop to ~10M
    // cycles, so the result block fills out later than the old mode-13h screen.
    let reason = machine.run_until_halt_or_cycles(20_000_000).unwrap();
    // POST completes and the BIOS idles (it keeps running, not halting).
    assert!(matches!(reason, StopReason::CycleLimit { .. }));
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    // The live result builder owns the header: declared count must match the
    // parsed records and the additive checksum must validate (parse succeeded).
    assert_eq!(
        usize::from(results.declared_record_count),
        results.records.len()
    );
    // The suite opens with a BEGIN record and the foundation reference step.
    assert_eq!(
        results.records[0].status,
        izarravm_firmware::SuiteRecordStatus::Begin
    );
    assert_eq!(results.records[0].name, "suite.izarra");
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "self.framework"
    }));
    // self.extaccess proves the unreal-mode >1 MiB helpers work in the live BIOS.
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "self.extaccess"
    }));
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "component.optical_atapi"
    }));
    let cpu = results
        .records
        .iter()
        .position(|record| record.name == "component.cpu_gsw")
        .unwrap();
    let memory = results
        .records
        .iter()
        .position(|record| record.name == "memory.ramtest")
        .unwrap();
    let video = results
        .records
        .iter()
        .position(|record| record.name == "component.video_margo")
        .unwrap();
    assert!(cpu < memory, "CPU POST should run before RAM");
    assert!(video < memory, "VGA POST should run before RAM");
}

#[test]
fn izarra_bios_slow_post_continues_after_ramtest() {
    let profile = MachineProfile::gsw_386(2, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_fast_post(false);
    let mut results = None;
    for _ in 0..30 {
        machine.run_until_halt_or_cycles(10_000_000).unwrap();
        let parsed = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        let complete = parsed
            .records
            .iter()
            .any(|record| record.name == "component.optical_atapi");
        results = Some(parsed);
        if complete {
            break;
        }
    }
    let results = results.unwrap();
    assert!(
        results
            .records
            .iter()
            .any(|record| record.name == "component.optical_atapi"),
        "{:?}",
        results.records
    );
}

#[test]
fn izarra_bios_ramtest_esc_skips_and_continues_post() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_fast_post(false);
    for _ in 0..40 {
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        if results
            .records
            .iter()
            .any(|record| record.name == "component.cpu_gsw")
        {
            break;
        }
    }
    machine.inject_key_scancodes(&[0x01]);
    let reason = machine.run_until_halt_or_cycles(100_000_000).unwrap();
    assert!(matches!(reason, StopReason::CycleLimit { .. }));
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results
            .records
            .iter()
            .any(|record| record.name == "component.cpu_lotura"),
        "{:?}",
        results.records
    );
}

#[test]
fn izarra_bios_tab_before_ramtest_wins_over_later_del() {
    let profile = MachineProfile::gsw_386(2, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_fast_post(false);
    for _ in 0..40 {
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
        if let Ok(results) = izarravm_firmware::parse_result_block(machine.memory().as_slice()) {
            if results
                .records
                .iter()
                .any(|record| record.name == "video.margo_caps")
            {
                break;
            }
        }
    }

    machine.inject_key_scancodes(&[0x0f, 0x8f, 0x53, 0xd3]); // Tab, then Del.

    let mut red = 0;
    for _ in 0..40 {
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        red = (64..72u32)
            .flat_map(|y| (28..130u32).map(move |x| (x, y)))
            .filter(|&(x, y)| machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) == 24)
            .count();
        if red > 20 {
            break;
        }
    }
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert!(
        red > 20,
        "Tab should open the boot menu; found {red} red title pixels"
    );
}

#[test]
fn izarra_bios_draws_art_post_screen() {
    // The POST screen is the RLE art (izbios-art.inc): a cream field with the
    // wordmark, mascot and grey component icons baked into the background, plus
    // the top-left header text drawn over it by lfb_text. Pixels are read as raw
    // palette indices from the LFB VRAM at MARGO_LFB_BASE + y*320 + x. The
    // full-screen RLE blit is heavy, so POST needs a large cycle budget.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    // A clear top-left spot is the cream field; the screen is not monochrome.
    let field = machine.read_physical_u8(MARGO_LFB_BASE + 4 * 320 + 4);
    // The wordmark sits top-right in the art (x 213..303, y 11..60): non-field
    // pixels there prove the background RLE blitted, not just a flat clear.
    let mut wordmark = 0;
    for y in 11..60u32 {
        for x in 213..303u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) != field {
                wordmark += 1;
            }
        }
    }
    assert!(
        wordmark > 100,
        "expected the baked-in wordmark in the background, found {wordmark} non-field pixels"
    );
    // The version line "Izarra-BIOS v3.01 - 1997" renders top-left (y 12..20)
    // via lfb_text; any non-field pixels there are glyphs, guarding the LFB
    // glyph path on the art.
    let mut header = 0;
    for y in 12..20u32 {
        for x in 8..200u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) != field {
                header += 1;
            }
        }
    }
    assert!(
        header > 60,
        "expected the top-left version line, found {header} non-field pixels"
    );
    // The DEL/TAB key hints render in the gap above the icon row (y 134..154,
    // x 8..200), telling the user how to reach setup and the boot menu.
    let mut hints = 0;
    for y in 134..154u32 {
        for x in 8..200u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) != field {
                hints += 1;
            }
        }
    }
    assert!(
        hints > 60,
        "expected the DEL/TAB key hints, found {hints} non-field pixels"
    );
}

#[test]
fn izarra_bios_post_lights_component_icons() {
    // As each wired probe passes, console_step_line blits the colour icon sprite
    // over its grey background icon. The VEGA monitor sprite (cell x 42..66,
    // y 166..192) carries saturated colour bars once lit, whereas the grey icon
    // is near-monochrome. A saturated pixel in the cell after a full POST sweep
    // proves component.video_margo passed and the grey->colour reveal fired.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    let (words, _w, _h) = machine.frame_argb();
    let saturated = |x: u32, y: u32| {
        let p = words[(y * 320 + x) as usize];
        let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
        r.max(g).max(b).saturating_sub(r.min(g).min(b)) > 60
    };
    let lit = (42..66u32).any(|x| (166..192u32).any(|y| saturated(x, y)));
    assert!(
        lit,
        "VEGA icon cell never lit to colour — the reveal did not fire"
    );
}

#[test]
fn serial_tx_is_captured_and_lsr_reports_empty() {
    // COM1 bytes reach the capture sink only after their programmed baud time.
    // THRE/TEMT then report that both the holding and shift registers are empty.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x03f8, BusWidth::Byte, u32::from(b'H'), false)
            .unwrap();
        bus.write_io(0x03f8, BusWidth::Byte, u32::from(b'i'), false)
            .unwrap();
    });
    assert!(machine.serial_output().is_empty());
    machine.advance_devices_ticks(machine.serial.ticks_until_idle());
    assert!(machine.serial_text().ends_with("Hi"));
    let lsr = machine.read_io_port_u8(0x03fd);
    assert_ne!(lsr & 0x20, 0, "THRE set");
    assert_ne!(lsr & 0x40, 0, "TEMT set");
}

#[test]
fn izarra_bios_post_log_is_disabled_by_default() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert!(
        machine.serial_output().is_empty(),
        "a fresh CMOS must leave BIOS debug output disabled"
    );
}

#[test]
fn izarra_bios_ignores_unknown_com1_debug_values() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_cmos_byte(0x14, 2);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert!(
        machine.serial_output().is_empty(),
        "only CMOS value 1 may enable BIOS debug output"
    );
}

#[test]
fn izarra_bios_mirrors_post_log_to_com1_when_enabled() {
    // POST initializes COM1 and writes each step's status and name to 0x3F8.
    // After a full POST run the serial log carries the header and the
    // foundation reference step, proving the mirror is live.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_cmos_byte(0x14, 1);
    // The RLE background blit delays com1_init/the step loop to ~10M cycles.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    let serial = machine.serial_text();
    assert!(
        serial.contains("Izarra 3000 POST"),
        "COM1 log missing the POST header: {serial:?}"
    );
    assert!(
        serial.contains("PASS self.framework"),
        "COM1 log missing the framework step line: {serial:?}"
    );
    // MEASURE steps must carry their value: this 16 MB machine reports 16384 KiB
    // detected, so the COM1 line ends with the eight-digit value, not a bare name.
    assert!(
        serial.contains("MEASURE memory.detected_kib 00016384"),
        "COM1 MEASURE line missing its value: {serial:?}"
    );
}

#[test]
fn fast_post_port_reflects_the_flag() {
    // Port 0xE2 is the Lotura POST-pacing flag the BIOS reads before the
    // cosmetic RAM count-up. It defaults to fast (1) so headless runs and
    // tests skip the ~8 s pacing; the GUI clears it for the full experience.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    let fast = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e2, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(fast, 1, "fast POST is the default");
    machine.set_fast_post(false);
    let full = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e2, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(full, 0, "clearing the flag selects the full-pacing path");
}

#[test]
fn izarra_bios_int19_boots_floppy_sector_zero() {
    // INT 19h must load sector 0 of the mounted floppy to 0000:7C00 and far
    // jump there with no signature check. The boot sector writes a sentinel
    // and halts; if the sentinel lands, the bootstrap loaded and jumped.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();

    let mut img = vec![0u8; 737_280];
    // Boot sector at 0000:7C00: mov bx,0x0500; mov al,0x99; mov [bx],al; hlt.
    // boot_entry enters with DS=0, so [bx] addresses 0000:0500.
    let boot = [0xBB, 0x00, 0x05, 0xB0, 0x99, 0x88, 0x07, 0xF4];
    img[..boot.len()].copy_from_slice(&boot);
    machine.mount_floppy(img).unwrap();

    machine.run_until_halt_or_cycles(50_000_000).unwrap();
    assert_eq!(
        machine.read_physical_u8(0x0500),
        0x99,
        "the boot sector ran from 0000:7C00, so INT 19h loaded and jumped"
    );
}

#[test]
fn floppy_booter_owns_int21_through_its_ivt_handler() {
    // QuickDOS-style self-booting disks provide their own DOS personality.
    // After INT 19h boots A:, INT 21h must run through the disk's IVT handler
    // rather than the Toka-DOS HLE. The boot sector installs INT 21h at
    // 0000:7C1E, calls AH=4Ch, then writes a post-return marker and halts. If
    // HLE owns the call, AH=4Ch reports StopReason::DosExit before either
    // marker lands.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();

    let mut img = vec![0u8; 737_280];
    let boot = [
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x84, 0x00, 0x1E, 0x7C, // mov word [0084h], 7C1Eh
        0xC7, 0x06, 0x86, 0x00, 0x00, 0x00, // mov word [0086h], 0000h
        0xB8, 0x2A, 0x4C, // mov ax, 4C2Ah
        0xCD, 0x21, // int 21h
        0xBB, 0x01, 0x05, // mov bx, 0501h
        0xB0, 0x7E, // mov al, 7Eh
        0x88, 0x07, // mov [bx], al
        0xFA, // cli
        0xF4, // hlt
        // INT 21h handler at 0000:7C1E.
        0xBB, 0x00, 0x05, // mov bx, 0500h
        0xB0, 0x21, // mov al, 21h
        0x88, 0x07, // mov [bx], al
        0xCF, // iret
    ];
    img[..boot.len()].copy_from_slice(&boot);
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(50_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x0500),
        0x21,
        "the boot sector's INT 21h handler ran instead of Toka-DOS HLE"
    );
    assert_eq!(
        machine.read_physical_u8(0x0501),
        0x7E,
        "the boot sector returned from its INT 21h handler and kept running"
    );
}

#[test]
fn int13_through_ff00_0000_returns_to_caller() {
    // Period PC booters (e.g. Wizardry III) repoint IVT[0x13] to FF00:0000 to
    // chain disk calls through the ROM-BIOS handler, then issue INT 13h. The
    // host intercepts the INT 13h instruction by vector number regardless of
    // the IVT target, so it still services the read; the redirected vector at
    // FF00:0000 only needs a valid IRET to land on. This test proves control
    // returns to the caller (no reset, no runaway) and the disk read happened.
    let mut img = vec![0u8; 737_280];
    img[0] = 0xEB;
    img[1] = 0x55;
    let rom = rom_with_code(&[
        // Point IVT[0x13] (at 0000:004C) to FF00:0000.
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x4C, 0x00, 0x00, 0x00, // mov word [0x004C], 0x0000 (offset)
        0xC7, 0x06, 0x4E, 0x00, 0x00, 0xFF, // mov word [0x004E], 0xFF00 (segment)
        // Read 1 sector at CHS(0,0,1) of drive 0 into ES:BX = 0000:2000.
        0x8E, 0xC0, // mov es, ax
        0xBB, 0x00, 0x20, // mov bx, 0x2000
        0xB8, 0x01, 0x02, // mov ax, 0x0201
        0xB9, 0x01, 0x00, // mov cx, 0x0001
        0xBA, 0x00, 0x00, // mov dx, 0x0000
        0xCD, 0x13, // int 13h  -> vector now targets FF00:0000
        // If the IRET at FF00:0000 returned cleanly, we reach this marker.
        0xBB, 0x00, 0x05, // mov bx, 0x0500
        0xB0, 0x42, // mov al, 0x42
        0x88, 0x07, // mov [bx], al   (DS=0, so writes 0000:0500)
        0xF4, // hlt
    ]);
    // The Izarra BIOS emits an IRET at ROM offset 0xF000 (FF00:0000); the
    // synthetic test ROM gets the same stub so the redirected vector lands on
    // a clean return point.
    let mut rom = rom;
    rom[0xF000] = 0xCF; // iret
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The INT 13h read still placed the sector bytes at 0x2000.
    assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
    assert_eq!(machine.read_physical_u8(0x2001), 0x55);
    // The IRET at FF00:0000 returned to the caller, which ran the marker store.
    assert_eq!(
        machine.read_physical_u8(0x0500),
        0x42,
        "control returned past the redirected INT 13h vector"
    );
    let flags = machine.cpu().registers.eflags;
    assert_eq!(flags & 0x0001, 0, "CF must be clear after a good read");
}

#[test]
fn int13_ah01_returns_last_status() {
    // A failed read (drive B:, unbacked) sets the last status; AH=01h reads it back.
    let rom = rom_with_code(&[
        0xB4, 0x02, 0xB0, 0x01, // AH=02h read, AL=1 sector
        0xB5, 0x00, 0xB1, 0x01, // CH=0 cyl, CL=1 sector
        0xB6, 0x00, 0xB2, 0x01, // DH=0 head, DL=1 (drive B:, unbacked)
        0xCD, 0x13, 0xB4, 0x01, 0xCD, 0x13, // AH=01h get last status
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    // Mount media in A: so handle_int13 runs; the read targets B:, which is unbacked.
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // Drive B: is unbacked: the transfer reported AH=01 (invalid drive); AH=01h returns it
    // in AH (the documented register) and mirrors it into AL for PS/2 compatibility.
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax as u8, 0x01, "AL = last disk status");
    assert_eq!((ax >> 8) as u8, 0x01, "AH = last disk status");
}

#[test]
fn simulated_int_dispatch_through_the_ivt_services_the_hle() {
    // The Quake-under-CWSDPMI mechanism in miniature: a DPMI host services
    // a real-mode interrupt request by PUSHF + far CALL through the IVT,
    // never executing an INT opcode. The per-vector stub's fetch seam must
    // service the HLE anyway. Here: a simulated INT 10h AX=0013 mode set.
    let code: &[u8] = &[
        0xb8, 0x13, 0x00, // mov ax, 0x0013
        0x31, 0xdb, // xor bx, bx
        0x8e, 0xdb, // mov ds, bx
        0x9c, // pushf
        0xff, 0x1e, 0x40, 0x00, // call far [0x0040]  (IVT[0x10])
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x449),
        0x13,
        "the simulated INT 10h mode set must reach the HLE (BDA current mode)"
    );
}

#[test]
fn int_opcode_dispatch_services_exactly_once() {
    // INT 10h AH=0Eh teletype 'A' via the opcode path: the opcode arm
    // stands down for a default vector (the stub fetch posts instead), so
    // the character must appear exactly once (a double service advances
    // the BDA cursor column twice).
    let code: &[u8] = &[
        0xb8, 0x41, 0x0e, // mov ax, 0x0e41 ('A' teletype)
        0xcd, 0x10, // int 0x10
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x450),
        1,
        "one teletype call advances the cursor column exactly once"
    );
}

#[test]
fn hook_chaining_to_the_saved_default_services_exactly_once() {
    // Reviewer reproducer (finding 1): a guest hooks an intercepted vector
    // and chains to the saved default (the per-vector stub). The hook gets
    // no post at the opcode (it owns the vector); the chain landing posts
    // exactly one service. A double poster advances the BDA cursor column
    // twice.
    let mut image_code = vec![
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        // Save IVT[0x10] (the default per-vector stub) at 0000:7D80.
        0xa1, 0x40, 0x00, // mov ax, [0x0040]
        0xa3, 0x80, 0x7d, // mov [0x7D80], ax
        0xa1, 0x42, 0x00, // mov ax, [0x0042]
        0xa3, 0x82, 0x7d, // mov [0x7D82], ax
        // Hook IVT[0x10] = 0000:7D00.
        0xc7, 0x06, 0x40, 0x00, 0x00, 0x7d, // mov word [0x0040], 0x7D00
        0xc7, 0x06, 0x42, 0x00, 0x00, 0x00, // mov word [0x0042], 0x0000
        0xb8, 0x41, 0x0e, // mov ax, 0x0e41 ('A' teletype)
        0xcd, 0x10, // int 0x10
        0xfa, 0xf4, // cli; hlt
    ];
    image_code.resize(0x100, 0x90);
    // The hook body at 0000:7D00 (boot-sector offset 0x100): chain to the
    // saved default with a far jump through the saved pointer.
    image_code.extend_from_slice(&[0xff, 0x2e, 0x80, 0x7d]); // jmp far [0x7D80]
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(&image_code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x450),
        1,
        "hook-then-chain must service the teletype exactly once (column 1, not 2)"
    );
}

#[test]
fn copied_vector_services_once_as_the_landed_vector() {
    // Reviewer reproducer (finding 2): a guest copies one intercepted
    // vector's IVT entry over another (IVT[0x42] <- IVT[0x10]) and issues
    // the copy. Real hardware runs the landed handler exactly once; a
    // dispatch that posts at both the opcode (as 0x42) and the landing
    // (as 0x10) services twice.
    let code: &[u8] = &[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xa1, 0x40, 0x00, // mov ax, [0x0040]  (IVT[0x10] offset)
        0xa3, 0x08, 0x01, // mov [0x0108], ax  (IVT[0x42] offset)
        0xa1, 0x42, 0x00, // mov ax, [0x0042]  (IVT[0x10] segment)
        0xa3, 0x0a, 0x01, // mov [0x010A], ax  (IVT[0x42] segment)
        0xb8, 0x41, 0x0e, // mov ax, 0x0e41 ('A' teletype)
        0xcd, 0x42, // int 0x42
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x450),
        1,
        "a copied vector services once, as the landed vector (column 1, not 2)"
    );
}

#[test]
fn hook_chain_to_legacy_iret_survives_an_uninterceded_stub_landing() {
    // Round-2 review finding 1 (deterministic stand-in for the timer-tick
    // race): a guest hooks INT 13h and its hook body dispatches a
    // NON-intercepted interrupt (INT 1Ch here, exactly what the machine's
    // own timer ISR chains every tick) before chaining to the hardcoded
    // legacy FF00:0000. The 1Ch stub landing must NOT disarm the live
    // 0x13 legacy stash, or the chained disk service is silently dropped.
    let rom = rom_with_code(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        // IVT[0x13] = F000:0023 (the hook below, in this ROM).
        0xc7, 0x06, 0x4c, 0x00, 0x23, 0x00, // mov word [0x4c], 0x0023
        0xc7, 0x06, 0x4e, 0x00, 0x00, 0xf0, // mov word [0x4e], 0xf000
        // A failing read on unbacked drive B sets the last status...
        0xb4, 0x02, 0xb0, 0x01, // AH=02h read, AL=1 sector
        0xb5, 0x00, 0xb1, 0x01, // CH=0, CL=1
        0xb6, 0x00, 0xb2, 0x01, // DH=0, DL=1 (drive B:, unbacked)
        0xcd, 0x13, // int 0x13
        0xb4, 0x01, 0xcd, 0x13, // ...and AH=01h reads it back.
        0xf4, // hlt (offset 0x22)
        // hook (offset 0x23): tick-chain stand-in, then legacy chain.
        0xcd, 0x1c, // int 0x1c   (lands stub 0x1C, not intercepted)
        0xea, 0x00, 0x00, 0x00, 0xff, // jmp far FF00:0000
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(
        (ax >> 8) as u8,
        0x01,
        "the hook-chained INT 13h must survive an interleaved non-intercepted \
             stub landing (AH = last status)"
    );
}

#[test]
fn booter_hardcoded_legacy_iret_keeps_int13_serviced() {
    // Period booters repoint IVT[0x13] at the legacy shared chain target
    // FF00:0000 (not the per-vector stub) and then issue INT 13h. That
    // address is shared by every vector, so the fetch seam attributes the
    // landing through the vector the INT opcode stashed (last_int_vector).
    let rom = rom_with_code(&[
        // IVT[0x13] = FF00:0000, the hardcoded legacy chain target.
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xc7, 0x06, 0x4c, 0x00, 0x00, 0x00, // mov word [0x4c], 0x0000
        0xc7, 0x06, 0x4e, 0x00, 0x00, 0xff, // mov word [0x4e], 0xff00
        // A failing read on unbacked drive B: sets the last status...
        0xb4, 0x02, 0xb0, 0x01, // AH=02h read, AL=1 sector
        0xb5, 0x00, 0xb1, 0x01, // CH=0, CL=1
        0xb6, 0x00, 0xb2, 0x01, // DH=0, DL=1 (drive B:, unbacked)
        0xcd, 0x13, // int 0x13
        0xb4, 0x01, 0xcd, 0x13, // ...and AH=01h reads it back.
        0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(
        (ax >> 8) as u8,
        0x01,
        "the hardcoded-legacy-vector INT 13h was still serviced (AH = last status)"
    );
}

#[test]
fn izarra_bios_isr_enqueues_injected_key() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    // Run POST so the BIOS reaches its idle loop (past the setup hotkey window,
    // which would otherwise drain the key). Then inject a key: IRQ1 reaches the
    // installed INT 09h, which enqueues it into the BDA ring. The idle loop does
    // not consume keys, so it stays there. The budget tracks POST's length: the
    // setup-page incremental-redraw work (f56c0197) pushed POST past the old
    // 5M-cycle budget, which parked this test inside the hotkey window.
    machine.run_until_halt_or_cycles(10_000_000).unwrap();
    machine.inject_key_scancodes(&[0x1e, 0x9e]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    let head = machine.memory_read_u16_for_test(0x41a);
    let tail = machine.memory_read_u16_for_test(0x41c);
    assert_ne!(head, tail, "the installed INT 09h enqueued the key");
}

#[test]
fn izarra_setup_saves_a_changed_value_to_cmos() {
    // Drive the Del setup page end to end: enter it during POST, change the
    // keyboard layout (CMOS 0x10, default 0 = en-US) to the next entry, save,
    // and confirm the persisted CMOS byte changed. The setup menu blocks on a
    // keyboard read between keystrokes, so each key is injected then run.
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    assert_eq!(
        machine.cmos_byte(0x10),
        0,
        "the keyboard-layout NVRAM byte starts at en-US (0)"
    );

    // Queue Del before POST reaches the hotkey window so the window finds it.
    // Make + break; only the make enqueues into the BDA ring (0x53 = Del).
    machine.inject_key_scancodes(&[0x53, 0xd3]);
    // Run past POST. The window consumes Del and enters the menu, which then
    // blocks on a keyboard read, so the rest of the budget just spins there.
    // The full-screen RLE POST background pushes the hotkey window to ~15M
    // cycles, so this budget must clear it.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();

    // Down moves the highlight from Time (row 0) to Keyboard (row 1). Each
    // keystroke repaints the whole page (title + boxed menu + help footer) on
    // the Margo LFB; the per-pixel unreal-mode box/fill primitives cost more
    // guest cycles than the old mode-13h gfx_clear + gfx_text redraw, so these
    // budgets are larger than the pre-LFB page needed.
    machine.inject_key_scancodes(&[0x50, 0xd0]); // Down
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    // Right cycles the keyboard layout forward (en-US -> UK).
    machine.inject_key_scancodes(&[0x4d, 0xcd]); // Right
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    // F10 saves: writes CMOS 0x10/0x12, refreshes the checksum, and exits.
    machine.inject_key_scancodes(&[0x44, 0xc4]); // F10
    machine.run_until_halt_or_cycles(4_000_000).unwrap();

    assert_eq!(
        machine.cmos_byte(0x10),
        1,
        "saving the setup page persisted the new keyboard layout to CMOS 0x10"
    );
    // The save also refreshes the NVRAM checksum, so a reload validates.
    let saved = machine.cmos_bytes();
    let mut reloaded = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    assert!(
        reloaded.load_cmos(&saved),
        "the saved CMOS image carries a valid checksum"
    );
    assert_eq!(reloaded.cmos_byte(0x10), 1);
}

#[test]
fn boot_menu_marks_one_speed_row_on_the_lfb() {
    // Open the LFB boot menu (focus seeds on the Floppy device row, so every
    // speed row is unfocused) and confirm exactly one speed row shows the marker
    // diamond. The marker sits at x 172 on a speed row; an unfocused marked row
    // paints an ink diamond (index ART_INK_INDEX = 0) on the cream field, so ink
    // pixels in that column flag the mark. This guards the full-repaint render
    // (a stale or missing marker would change the count).
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.inject_key_scancodes(&[0x0f, 0x8f]); // Tab opens the menu.
    machine.run_until_halt_or_cycles(25_000_000).unwrap();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);

    // Speed rows top at y 144 + row*12; the marker glyph is at +2, x 172..180.
    let marker_inked = |m: &mut Machine, row: u32| -> bool {
        let y0 = 144 + row * 12 + 2;
        (y0..y0 + 8)
            .any(|y| (172..180u32).any(|x| m.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) == 0))
    };
    let marked = (0..4u32)
        .filter(|&row| marker_inked(&mut machine, row))
        .count();
    assert_eq!(marked, 1, "exactly one speed row shows the marker diamond");
}

#[test]
fn int1b_and_int1c_vectors_point_at_valid_iret_handlers() {
    // Use a ROM that carries the IRET byte at FF00:0000, the way the real BIOS
    // does, so the seeded vector lands on a genuine IRET.
    let mut m = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&[]),
    )
    .unwrap();
    for vector in [0x1bu32, 0x1c] {
        let off = read_u16(&mut m, vector * 4);
        let seg = read_u16(&mut m, vector * 4 + 2);
        assert_eq!(
            seg, BIOS_ROM_IRET_SEG,
            "INT {vector:02X}h targets the ROM IRET segment"
        );
        let target = (u32::from(seg) << 4) + u32::from(off);
        assert_eq!(
            m.read_physical_u8(target),
            0x90,
            "INT {vector:02X}h target is its per-vector stub's NOP"
        );
        assert_eq!(
            m.read_physical_u8(target + 1),
            0xcf,
            "INT {vector:02X}h stub ends in an IRET"
        );
    }
}

#[test]
fn dos_reserved_vectors_point_at_a_valid_iret_handler() {
    let mut m = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&[]),
    )
    .unwrap();

    for vector in [
        0x2bu32, 0x2c, 0x2d, 0x2e, 0x32, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c,
        0x3d, 0x3e, 0x3f, 0x45, 0x48, 0x49, 0x4a, 0x59, 0x5a, 0x5b, 0x5c, 0x60, 0x61, 0x62, 0x63,
        0x64, 0x65, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x78, 0x79, 0x7a, 0x7b, 0x7c,
        0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0xe0, 0xe4, 0xef, 0xf0, 0xf1,
        0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
    ] {
        let off = read_u16(&mut m, vector * 4);
        let seg = read_u16(&mut m, vector * 4 + 2);
        assert_eq!(seg, BIOS_ROM_IRET_SEG, "INT {vector:02X}h IRET segment");
        let target = (u32::from(seg) << 4) + u32::from(off);
        assert_eq!(
            m.read_physical_u8(target),
            0x90,
            "INT {vector:02X}h target is its per-vector stub's NOP"
        );
        assert_eq!(
            m.read_physical_u8(target + 1),
            0xcf,
            "INT {vector:02X}h stub ends in an IRET"
        );
    }
}

#[test]
fn int70_vector_points_at_the_rtc_isr_stub() {
    let mut m = int15_machine(4);
    let off = read_u16(&mut m, 0x70 * 4);
    let seg = read_u16(&mut m, 0x70 * 4 + 2);
    assert_eq!(seg, 0);
    assert_eq!(off, BIOS_RTC_ISR_ADDRESS as u16);
    // The stub starts with PUSH AX and ends with IRET.
    assert_eq!(m.read_physical_u8(BIOS_RTC_ISR_ADDRESS as u32), 0x50);
    assert_eq!(m.read_physical_u8(BIOS_RTC_ISR_ADDRESS as u32 + 14), 0xcf);
}

#[test]
fn slave_irq_vectors_point_at_the_eoi_stub() {
    let mut m = int15_machine(4);
    for vector in [0x74u32, 0x75, 0x76] {
        let off = read_u16(&mut m, vector * 4);
        let seg = read_u16(&mut m, vector * 4 + 2);
        assert_eq!(seg, 0, "INT {vector:02X}h segment");
        assert_eq!(
            off, BIOS_SLAVE_IRQ_ISR_ADDRESS as u16,
            "INT {vector:02X}h offset"
        );
    }
    assert_eq!(m.read_physical_u8(BIOS_SLAVE_IRQ_ISR_ADDRESS as u32), 0x50);
    assert_eq!(
        m.read_physical_u8(BIOS_SLAVE_IRQ_ISR_ADDRESS as u32 + 8),
        0xcf
    );
}

#[test]
fn enabled_rtc_periodic_interrupt_requests_irq8() {
    let mut m = int15_machine(4);
    // Enable the periodic interrupt (select Reg B, set PIE bit 6).
    m.rtc.write_port(0x70, 0x0b);
    m.rtc.write_port(0x71, 0x40);
    let deadline = m.rtc.ticks_until_periodic_irq().unwrap();
    m.advance_devices_ticks(deadline);
    assert!(m.pic.irr_bit(8), "IRQ8 became pending");
}

#[test]
fn c207_stores_the_mouse_handler_far_pointer_in_the_ebda() {
    let mut m = int15_machine(4);
    // ES:BX = 1234:5678, the handler the guest installs.
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
    m.cpu.registers.set_ebx(0x5678);
    m.cpu.registers.set_eax(0xC207);
    m.handle_int15();
    // CF clear, AH=0: success.
    let flags_carry = {
        let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = m.cpu.registers.esp() as u16;
        read_u16(&mut m, ss + u32::from(sp.wrapping_add(4))) & 1
    };
    assert_eq!(flags_carry, 0, "C207 returns CF clear");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
    // The EBDA holds the far pointer: offset word then segment word.
    let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
    assert_eq!(read_u16(&mut m, base), 0x5678, "offset stored");
    assert_eq!(read_u16(&mut m, base + 2), 0x1234, "segment stored");
}

#[test]
fn c205_init_enables_intellimouse_wheel_and_sets_ebda_packet_size() {
    let mut m = int15_machine(4);
    // The aux device powers up as a standard 3-byte mouse.
    assert!(!m.keyboard.mouse_wheel_enabled(), "starts in 3-byte mode");
    // INT 15h AX=C205, BH=3 (the standard init the driver issues at startup).
    m.cpu.registers.set_eax(0xC205);
    m.cpu.registers.set_ebx(0x0300);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "C205 returns AH=0"
    );
    // The platform enables wheel mode at mouse-enable: the device is now 4-byte,
    assert!(
        m.keyboard.mouse_wheel_enabled(),
        "device in IntelliMouse mode"
    );
    // and the BIOS-visible EBDA packet size is 4 so int74 assembles the Z byte.
    let pkt_size = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_PKT_SIZE_OFF;
    assert_eq!(m.read_physical_u8(pkt_size), 4, "EBDA packet size is 4");
}

#[test]
fn c202_sample_rate_is_reported_by_c206_status() {
    let mut m = int15_machine(4);
    m.cpu.registers.set_eax(0xC202);
    m.cpu.registers.set_ebx(0x0600); // BIOS rate code 6 = 200 Hz
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);

    m.cpu.registers.set_eax(0xC206);
    m.cpu.registers.set_ebx(0x0000); // BH=0 status
    m.handle_int15();
    assert_eq!(m.cpu.registers.edx() as u8, 200);
}

#[test]
fn c200_enable_turns_on_the_wheel_and_disable_leaves_it() {
    let mut m = int15_machine(4);
    // C200 enable (BH=1) flips on IntelliMouse 4-byte mode and packet size 4.
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int15();
    assert!(
        m.keyboard.mouse_wheel_enabled(),
        "enable turns on the wheel"
    );
    let pkt_size = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_PKT_SIZE_OFF;
    assert_eq!(m.read_physical_u8(pkt_size), 4, "enable sets packet size 4");
    // C200 disable (BH=0) stops reporting but leaves the wheel mode and the EBDA
    // packet size as-is (the known no-resize ceiling).
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int15();
    assert!(
        m.keyboard.mouse_wheel_enabled(),
        "disable leaves wheel mode untouched"
    );
    assert_eq!(
        m.read_physical_u8(pkt_size),
        4,
        "disable leaves the packet size untouched"
    );
}

#[test]
fn int19_floppy_boot_loads_sector_and_jumps_to_7c00() {
    let mut m = int15_machine(4);
    // A 360 KB image with a marker byte at the start of sector 0.
    let mut image = vec![0u8; 368_640];
    image[0] = 0xeb; // a plausible boot-sector first byte (JMP short)
    image[1] = 0x3c;
    m.mount_floppy(image).unwrap();
    m.handle_int19();
    // Boot sector copied to 0000:7C00, DL = 0 (floppy), CS:IP = 0000:7C00.
    assert_eq!(m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32), 0xeb);
    assert_eq!(m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32 + 1), 0x3c);
    assert_eq!(m.cpu.registers.edx() as u8, 0x00);
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Cs).selector, 0x0000);
    assert_eq!(m.cpu.registers.eip, BOOT_SECTOR_ADDRESS as u32);
}

#[test]
fn int19_without_bootable_media_falls_to_int18_halt_stub() {
    let mut m = int15_machine(4);
    // No floppy and no Toka-DOS install: INT 19h must reach the INT 18h halt.
    m.handle_int19();
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Cs).selector,
        0x0000,
        "CS points at the low-RAM halt stub"
    );
    assert_eq!(m.cpu.registers.eip, BIOS_HALT_STUB_ADDRESS as u32);
    // The stub is CLI;HLT, which halts the machine for good.
    assert_eq!(m.read_physical_u8(BIOS_HALT_STUB_ADDRESS as u32), 0xfa);
    assert_eq!(m.read_physical_u8(BIOS_HALT_STUB_ADDRESS as u32 + 1), 0xf4);
}

#[test]
fn int18_halt_stub_actually_stops_the_machine() {
    let mut m = int15_machine(4);
    m.handle_int18();
    // Run from the halt stub: CLI then HLT, with IF cleared, gives a genuine stop.
    let reason = m.run_until_halt_or_cycles(10_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
}

#[test]
fn lotura_e7_banks_a_codepage_font_page_into_the_window() {
    // mov al,3 ; out 0E7h,al ; int 20h -> bank CP850 8x16 (cp=1, size=0) into 0xC4000.
    // sel=3: cp=3/3=1, size_index=3%3=0 => CP850, 8x16 block.
    const PROG: [u8; 6] = [0xB0, 0x03, 0xE6, 0xE7, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PROG).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();
    // CP850 8x16 block is CODEPAGE_FONTS[9728 .. 9728+4096]; it must now be at 0xC4000.
    for k in [0u32, 1, 0x41 * 16 + 2, 4095] {
        assert_eq!(
            machine.read_physical_u8(0xC4000 + k),
            izarravm_firmware::CODEPAGE_FONTS[(9728 + k) as usize],
            "byte {k} mismatch"
        );
    }
}

#[test]
fn boot_codepage_byte_loads_font_into_generator() {
    // CP850 8x16 block is CODEPAGE_FONTS[9728 .. 9728+4096]. Glyph 0xB5 there is
    // A-acute; under CP437 it is a box-drawing piece. Booting with CMOS 0x13 = 1
    // must leave the VGA font generator holding the CP850 glyph.
    let want: Vec<u8> = (0..16)
        .map(|r| izarravm_firmware::CODEPAGE_FONTS[9728 + 0xB5 * 16 + r])
        .collect();
    let got = boot_and_read_font_rows(1, 0xB5, 16);
    assert_eq!(got, want);
    // CP437 (cmos 0) keeps the box-drawing glyph.
    let want437: Vec<u8> = (0..16)
        .map(|r| izarravm_firmware::CODEPAGE_FONTS[0xB5 * 16 + r])
        .collect();
    assert_eq!(boot_and_read_font_rows(0, 0xB5, 16), want437);
}
