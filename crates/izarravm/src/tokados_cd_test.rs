// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "tokados_cd_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("create CD test directory");
    path
}

fn cd_autoexec(command: &str) -> Vec<u8> {
    format!(
        "@ECHO OFF\r\nPATH C:\\DOS\r\nSET BLASTER=A220 I5 D1 H5 P300 T6\r\n\
         IZCDEX /I /D:TOKACD01 /L:D /Q\r\n{command}\r\n"
    )
    .into_bytes()
}

fn run_cd_memory_command(tag: &str, command: &str) -> (StopReason, String) {
    let hdd_dir = scratch_dir(tag);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            &hdd_dir,
            vec![("AUTOEXEC.BAT".to_string(), cd_autoexec(command))],
        )
        .expect("mount Toka-DOS folder");

    let mut stop = machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("run memory command");
    for _ in 0..4 {
        if matches!(stop, StopReason::CpuError(_)) {
            break;
        }
        machine.inject_key_scancodes(&[0x1c, 0x9c]);
        stop = machine
            .run_until_halt_or_cycles(150_000_000)
            .expect("continue memory command");
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&hdd_dir).ok();
    (stop, text)
}

#[test]
#[ignore = "boots Toka-DOS and reads a folder-backed CD through the guest driver"]
fn guest_cd_stack_owns_d_and_reads_a_file() {
    let hdd_dir = scratch_dir("hdd");
    let cd_dir = scratch_dir("disc");
    std::fs::write(cd_dir.join("PROBE.TXT"), b"TOKA-CD-OK\r\n").expect("write CD probe");
    let folder = izarravm_machine::build_cd_folder(&cd_dir).expect("build folder CD");
    let image = izarravm_machine::CdImage::from_folder(folder).expect("mount folder CD");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_cd(image);
    machine
        .mount_hdd_folder_with(
            &hdd_dir,
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
    let pio_bytes = machine.cd_pio_byte_count();
    std::fs::remove_dir_all(&hdd_dir).ok();
    std::fs::remove_dir_all(&cd_dir).ok();

    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while exercising the guest CD stack: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "CDTEST failure code identifies the broken layer.\n{text}"
    );
    assert!(
        pio_bytes >= 2048,
        "the guest test passed without a sector crossing the ATAPI PIO path"
    );
}

#[test]
#[ignore = "boots Toka-DOS and directly exercises TOKACD request packets"]
fn guest_tokacd_protocol_matrix() {
    let hdd_dir = scratch_dir("protocol_hdd");
    let cd_dir = scratch_dir("protocol_disc");
    std::fs::write(cd_dir.join("PROBE.TXT"), b"TOKA-CD-PROTOCOL\r\n")
        .expect("write protocol disc file");
    let folder = izarravm_machine::build_cd_folder(&cd_dir).expect("build folder CD");
    let image = izarravm_machine::CdImage::from_folder(folder).expect("mount folder CD");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_cd(image);
    machine
        .mount_hdd_folder_with(
            &hdd_dir,
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
        .expect("run TOKACD protocol fixture");
    let text = machine.screen_text().as_text();
    let pio_bytes = machine.cd_pio_byte_count();
    let media_present = machine.cd_audio_state().media_present;
    std::fs::remove_dir_all(&hdd_dir).ok();
    std::fs::remove_dir_all(&cd_dir).ok();

    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while exercising TOKACD requests: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA6 },
        "CDPROT failure code identifies the failed request group.\n{text}"
    );
    assert!(pio_bytes >= 4096, "protocol reads did not cross ATAPI PIO");
    assert!(
        !media_present,
        "the final protocol Eject did not remove media"
    );
}

#[test]
#[ignore = "boots Toka-DOS, swaps data media for mixed mode, and tests CD audio"]
fn guest_tokacd_live_swap_and_audio_sequence() {
    const DATA_SECTOR: usize = 2048;
    const RAW_SECTOR: usize = 2352;
    let hdd_dir = scratch_dir("audio_hdd");
    let cd_dir = scratch_dir("audio_disc");
    let built = izarravm_machine::build_cd_folder(&cd_dir).expect("build empty data CD");
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
            &hdd_dir,
            vec![
                ("AUTOEXEC.BAT".to_string(), cd_autoexec("ECHO SWAP READY")),
                (
                    "CDAUDIO.COM".to_string(),
                    izarravm_firmware::cdaudio_com().to_vec(),
                ),
            ],
        )
        .expect("mount Toka-DOS folder");

    let first_stop = machine
        .run_until_halt_or_cycles(600_000_000)
        .expect("boot before media swap");
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
    std::fs::remove_dir_all(&hdd_dir).ok();
    std::fs::remove_dir_all(&cd_dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while testing CD audio: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA7 },
        "CDAUDIO failure code identifies the failed audio transition.\n{text}"
    );
}

#[test]
#[ignore = "boots Toka-DOS and verifies TOKACD escapes an unanswered PACKET"]
fn guest_tokacd_packet_timeout_is_bounded() {
    let hdd_dir = scratch_dir("timeout_hdd");
    let cd_dir = scratch_dir("timeout_disc");
    let built = izarravm_machine::build_cd_folder(&cd_dir).expect("build timeout CD");
    let image = izarravm_machine::CdImage::from_folder(built).expect("mount timeout CD");
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_cd(image);
    machine
        .mount_hdd_folder_with(
            &hdd_dir,
            vec![
                (
                    "AUTOEXEC.BAT".to_string(),
                    cd_autoexec("ECHO TIMEOUT READY"),
                ),
                (
                    "CDTIME.COM".to_string(),
                    izarravm_firmware::cdtime_com().to_vec(),
                ),
            ],
        )
        .expect("mount Toka-DOS folder");
    let first_stop = machine
        .run_until_halt_or_cycles(600_000_000)
        .expect("boot before timeout test");
    let first_text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &first_stop {
        panic!("CPU fault before timeout test: {msg}\n{first_text}");
    }
    assert!(
        first_text.contains("TIMEOUT READY") && first_text.to_ascii_lowercase().contains("c:\\>"),
        "Toka-DOS did not reach the timeout point (stop={first_stop:?}).\n{first_text}"
    );

    machine.set_test_cd_packet_stall(true);
    let keys = "CDTIME\r"
        .chars()
        .flat_map(ascii_to_set1)
        .collect::<Vec<_>>();
    machine.inject_key_scancodes(&keys);
    let stop = machine
        .run_until_halt_or_cycles(900_000_000)
        .expect("run packet timeout fixture");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&hdd_dir).ok();
    std::fs::remove_dir_all(&cd_dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault during packet timeout: {msg}\n{text}");
    }
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA8 },
        "CDTIME did not return the bounded timeout status.\n{text}"
    );
}

#[test]
#[ignore = "boots Toka-DOS with the guest CD stack and an empty drive"]
fn guest_cd_stack_boots_without_media() {
    let hdd_dir = scratch_dir("empty");
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            &hdd_dir,
            vec![("AUTOEXEC.BAT".to_string(), cd_autoexec("ECHO CD EMPTY OK"))],
        )
        .expect("mount Toka-DOS folder");

    let stop = machine
        .run_until_halt_or_cycles(600_000_000)
        .expect("boot without CD media");
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&hdd_dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while booting with no CD media: {msg}\n{text}");
    }
    assert!(
        text.contains("CD EMPTY OK") && text.to_ascii_lowercase().contains("c:\\>"),
        "the empty CD drive prevented a normal boot (stop={stop:?}).\n{text}"
    );
}

#[test]
#[ignore = "boots Toka-DOS and inspects upper-memory residency"]
fn guest_cd_components_reside_in_upper_memory() {
    let (stop, text) = run_cd_memory_command("memory", "LH TOKAMOUS\r\nMEM /P");
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while checking CD upper-memory residency: {msg}\n{text}");
    }
    let upper = text.to_ascii_uppercase();
    let upper_detail = upper
        .split_once("UPPER MEMORY DETAIL:")
        .map(|(_, rows)| rows)
        .unwrap_or("");
    assert!(
        upper_detail.contains("TOKACD") && upper_detail.contains("IZCDEX"),
        "TOKACD and IZCDEX must both appear in the upper-memory section.\n{text}"
    );
}

#[test]
#[ignore = "boots Toka-DOS and checks conventional memory with the CD stack loaded"]
fn guest_cd_stack_keeps_about_600k_conventional_free() {
    let (stop, text) =
        run_cd_memory_command("conventional", "LH TOKAMOUS\r\nMEM /CLASSIFY /NOSUMMARY");
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while checking conventional memory: {msg}\n{text}");
    }
    let free = text
        .lines()
        .find(|line| line.trim_start().starts_with("Free"))
        .unwrap_or_else(|| panic!("MEM /CLASSIFY did not list free memory.\n{text}"));
    assert!(
        free.contains("(600K)"),
        "the CD stack should leave about 600 KiB of conventional memory free.\n{text}"
    );
}
