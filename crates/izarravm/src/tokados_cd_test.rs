// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Full-boot tests for the IzarraCD ROM extension: the kernel claims drive D:
//! at boot (no guest driver files), DOS file I/O on D: is served by the host
//! redirector, and request packets reach the host driver through the ROM
//! header's strategy/interrupt stubs and the Lotura doorbell.

use super::*;

fn cd_autoexec(command: &str) -> Vec<u8> {
    format!("@ECHO OFF\r\nPATH C:\\DOS\r\nSET BLASTER=A220 I5 D1 H5 P300 T6\r\n{command}\r\n")
        .into_bytes()
}

fn run_cd_memory_command(
    tag: &str,
    command: &str,
    mut complete: impl FnMut(&Machine) -> bool,
) -> (TokaScratch, StopReason, String) {
    let hdd_scratch = TokaScratch::new(tag);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            hdd_scratch.path(),
            vec![("AUTOEXEC.BAT".to_string(), cd_autoexec(command))],
        )
        .expect("mount Toka-DOS folder");

    let (mut stop, _) = run_until_toka_condition(&mut machine, 200_000_000, &mut complete);
    for _ in 0..4 {
        if !matches!(stop, StopReason::CycleLimit { .. }) || complete(&machine) {
            break;
        }
        machine.inject_key_scancodes(&[0x1c, 0x9c]);
        (stop, _) = run_until_toka_condition(&mut machine, 150_000_000, &mut complete);
    }
    let text = machine.screen_text().as_text();
    if !matches!(stop, StopReason::CpuError(_)) && !complete(&machine) {
        panic!(
            "CD memory command did not return to a complete shell state (stop={stop:?}).\n{text}"
        );
    }
    (hdd_scratch, stop, text)
}

#[test]
#[ignore = "boots Toka-DOS and reads a folder-backed CD through the BIOS redirector"]
fn guest_cd_stack_owns_d_and_reads_a_file() {
    let hdd_scratch = TokaScratch::new("hdd");
    let cd_scratch = TokaScratch::new("disc");
    std::fs::write(cd_scratch.path().join("PROBE.TXT"), b"TOKA-CD-OK\r\n").expect("write CD probe");
    let folder = izarravm_machine::build_cd_folder(cd_scratch.path()).expect("build folder CD");
    let image = izarravm_machine::CdImage::from_folder(folder).expect("mount folder CD");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_cd(image);
    machine
        .mount_hdd_folder_with(
            hdd_scratch.path(),
            vec![
                ("AUTOEXEC.BAT".to_string(), cd_autoexec("CDTEST")),
                (
                    "CDTEST.COM".to_string(),
                    izarravm_firmware::cdtest_com().to_vec(),
                ),
            ],
        )
        .expect("mount Toka-DOS folder");

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("run guest CD fixture");
    let text = machine.screen_text().as_text();
    let redirector_bytes = machine.cd_redirector_read_bytes();

    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while exercising the CD stack: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "CDTEST failure code identifies the broken layer.\n{text}"
    );
    assert!(
        redirector_bytes >= 12,
        "the probe file's bytes must be delivered by the host redirector \
         (got {redirector_bytes})"
    );
    assert_eq!(
        machine.cd_pio_byte_count(),
        0,
        "no sector may cross the retired ATAPI PIO path on the Toka boot"
    );
}

#[test]
#[ignore = "boots Toka-DOS and directly exercises IzarraCD request packets"]
fn guest_izarracd_request_protocol_matrix() {
    let hdd_scratch = TokaScratch::new("protocol_hdd");
    let cd_scratch = TokaScratch::new("protocol_disc");
    std::fs::write(cd_scratch.path().join("PROBE.TXT"), b"TOKA-CD-PROTOCOL\r\n")
        .expect("write protocol disc file");
    let folder = izarravm_machine::build_cd_folder(cd_scratch.path()).expect("build folder CD");
    let image = izarravm_machine::CdImage::from_folder(folder).expect("mount folder CD");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_cd(image);
    machine
        .mount_hdd_folder_with(
            hdd_scratch.path(),
            vec![
                ("AUTOEXEC.BAT".to_string(), cd_autoexec("CDPROT")),
                (
                    "CDPROT.COM".to_string(),
                    izarravm_firmware::cdprot_com().to_vec(),
                ),
            ],
        )
        .expect("mount Toka-DOS folder");

    let stop = machine
        .run_until_halt_or_cycles(900_000_000)
        .expect("run CD protocol fixture");
    let text = machine.screen_text().as_text();
    let media_present = machine.cd_audio_state().media_present;

    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while exercising CD requests: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA6 },
        "CDPROT failure code identifies the failed request group.\n{text}"
    );
    assert!(
        !media_present,
        "the final protocol Eject did not remove media"
    );
}

#[test]
#[ignore = "boots Toka-DOS, swaps data media for mixed mode, and tests CD audio"]
fn guest_izarracd_live_swap_and_audio_sequence() {
    const DATA_SECTOR: usize = 2048;
    const RAW_SECTOR: usize = 2352;
    let hdd_scratch = TokaScratch::new("audio_hdd");
    let cd_scratch = TokaScratch::new("audio_disc");
    let built = izarravm_machine::build_cd_folder(cd_scratch.path()).expect("build empty data CD");
    let mut mixed_bin = built.meta.clone();
    mixed_bin.resize(24 * DATA_SECTOR, 0);
    mixed_bin.resize(mixed_bin.len() + 30 * RAW_SECTOR, 0x20);
    let replacement = izarravm_machine::CdImage::from_cue(
        "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
         TRACK 02 AUDIO\nINDEX 01 00:00:24\n",
        mixed_bin,
    )
    .expect("build mixed-mode replacement");
    let initial = izarravm_machine::CdImage::from_folder(built).expect("mount initial data CD");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_cd(initial);
    machine
        .mount_hdd_folder_with(
            hdd_scratch.path(),
            vec![
                ("AUTOEXEC.BAT".to_string(), cd_autoexec("ECHO SWAP READY")),
                (
                    "CDAUDIO.COM".to_string(),
                    izarravm_firmware::cdaudio_com().to_vec(),
                ),
            ],
        )
        .expect("mount Toka-DOS folder");

    let (first_stop, _) = run_until_toka_condition(&mut machine, 600_000_000, |machine| {
        current_root_prompt(machine) && machine.screen_text().as_text().contains("SWAP READY")
    });
    let first_text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &first_stop {
        panic!("CPU fault before CD replacement: {msg}\n{first_text}");
    }
    assert!(
        first_text.contains("SWAP READY") && first_text.to_ascii_lowercase().contains("c:\\>"),
        "Toka-DOS did not reach the swap point (stop={first_stop:?}).\n{first_text}"
    );

    machine.mount_cd(replacement);
    let keys = "CDAUDIO\r"
        .chars()
        .flat_map(ascii_to_set1)
        .collect::<Vec<_>>();
    machine.inject_key_scancodes(&keys);
    let stop = machine
        .run_until_halt_or_cycles(900_000_000)
        .expect("run CD audio fixture after replacement");
    let text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while testing CD audio: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA7 },
        "CDAUDIO failure code identifies the failed audio transition.\n{text}"
    );
}

// The `guest_tokacd_packet_timeout_is_bounded` test retired with TOKACD.SYS:
// its subject was the guest driver's bounded ATAPI polling loop (CDTIME.COM +
// `set_test_cd_packet_stall`), and no guest code polls the ATAPI ports on the
// Toka boot any more. The ATAPI device model and its stall hook remain for
// machine-level tests.

#[test]
#[ignore = "boots Toka-DOS with the IzarraCD claim and an empty drive"]
fn guest_cd_stack_boots_without_media() {
    let hdd_scratch = TokaScratch::new("empty");
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            hdd_scratch.path(),
            vec![("AUTOEXEC.BAT".to_string(), cd_autoexec("ECHO CD EMPTY OK"))],
        )
        .expect("mount Toka-DOS folder");

    let (stop, _) = run_until_toka_condition(&mut machine, 600_000_000, |machine| {
        current_root_prompt(machine) && machine.screen_text().as_text().contains("CD EMPTY OK")
    });
    let text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while booting with no CD media: {msg}\n{text}");
    }
    assert!(
        text.contains("CD EMPTY OK") && text.to_ascii_lowercase().contains("c:\\>"),
        "the empty CD drive prevented a normal boot (stop={stop:?}).\n{text}"
    );
    assert!(
        text.contains("IzarraCD ROM Extensions"),
        "the kernel must still claim drive D: with no media in the drive.\n{text}"
    );
}

#[test]
#[ignore = "boots Toka-DOS and verifies the CD stack leaves no resident components"]
fn guest_cd_stack_has_no_resident_components() {
    let (_scratch, stop, text) =
        run_cd_memory_command("memory", "LH TOKAMOUS\r\nMEM /P", |machine| {
            let upper = machine.screen_text().as_text().to_ascii_uppercase();
            current_root_prompt(machine) && upper.contains("UPPER MEMORY DETAIL:")
        });
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while checking CD residency: {msg}\n{text}");
    }
    let upper = text.to_ascii_uppercase();
    // The consolidation's memory claim: the CD stack costs zero resident
    // bytes. Neither retired component may appear anywhere in MEM's output,
    // while the upper-memory machinery itself still works (TOKAMOUS loads
    // high and is listed).
    assert!(
        !upper.contains("TOKACD") && !upper.contains("IZCDEX"),
        "a retired guest CD component is still resident.\n{text}"
    );
    let upper_detail = upper
        .split_once("UPPER MEMORY DETAIL:")
        .map(|(_, rows)| rows)
        .unwrap_or("");
    assert!(
        upper_detail.contains("TOKAMOUS"),
        "TOKAMOUS must still load into upper memory.\n{text}"
    );
}
