// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_machine::StopReason;

/// Boot the committed FAT32 HDD image (no floppy): INT 19h reads LBA 0 (the
/// MBR), which chains to the partition VBR, which loads KERNEL.SYS. The kernel
/// then mounts the FAT32 partition as C: and launches the shell.
fn boot_hdd(cycles: u64) -> (Machine, StopReason) {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd(izarravm_firmware::tokados_hdd_img().to_vec());
    let stop = machine
        .run_until_halt_or_cycles(cycles)
        .expect("run machine");
    (machine, stop)
}

// Katea-1 Milestone-0 GO/NO-GO gate: the real FreeDOS kernel must boot to
// C:\> from the synthesized static FAT32 HDD image (no floppy) and read a file
// off the volume. If this passes, the static-image approach is proven and the
// lazy host-folder facade work can begin; if it cannot reach C:\>, it is a
// NO-GO and we stop before any facade work.
#[test]
#[ignore = "boots a full DOS image from a FAT32 HDD (slow in debug); run with --ignored"]
fn katea_static_hdd_boots() {
    let (mut machine, stop) = boot_hdd(500_000_000);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        panic!("CPU fault during Katea HDD boot: {msg}\nstop={stop:?}\n{text}");
    }
    let text = machine.screen_text().as_text().to_ascii_lowercase();
    // The kernel must assign the FAT32 partition to C: and prompt there, NOT A:.
    assert!(
        text.contains("c:\\>"),
        "no C:\\> prompt after HDD boot (stop={stop:?}).\n{text}"
    );

    // VER: the kernel responds with its version banner.
    for ch in "ver\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type ver");
    }
    machine
        .run_until_halt_or_cycles(20_000_000)
        .expect("settle ver");
    let ver_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        ver_text.contains("c:\\>ver"),
        "VER not echoed at the C: prompt.\n{ver_text}"
    );

    // DIR C:\DOS must list a system binary from the DOS subdirectory, proving
    // the kernel read the volume's filesystem AND descended into the subdir where
    // the tools now live (the root itself holds only KERNEL.SYS (hidden),
    // CONFIG.SYS, AUTOEXEC.BAT and LICENSE.TXT).
    for ch in "dir c:\\dos\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type dir");
    }
    machine
        .run_until_halt_or_cycles(40_000_000)
        .expect("settle dir");
    let dir_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        dir_text.contains("command"),
        "DIR C:\\DOS did not list COMMAND.COM off the FAT32 volume.\n{dir_text}"
    );
}

/// The Katea host-folder facade end-to-end: mount a real host directory as C:
/// through `mount_hdd_folder` (the lazy facade, not a flat image), boot the
/// real FreeDOS kernel, and confirm it reaches C:\> and lists a file that lives
/// only in the host folder. This proves the mount path — system files from the
/// committed image + host files folded to 8.3 — boots and reads host files,
/// the M0 deliverable.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn katea_host_folder_boots_and_lists_a_host_file() {
    // A unique scratch folder under the system temp dir, holding one host file
    // whose 8.3 name (GREETING.TXT — already a valid 8.3 name, no ~n fold) is
    // distinct from the system files, so spotting it in DIR proves the host
    // folder reached the volume.
    let dir = std::env::temp_dir().join(format!(
        "katea_folder_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("GREETING.TXT"), b"hi from the host folder\r\n")
        .expect("write host file");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("run machine");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during Katea folder boot: {msg}\nstop={stop:?}\n{text}");
    }
    let text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        text.contains("c:\\>"),
        "no C:\\> prompt after folder boot (stop={stop:?}).\n{text}"
    );

    // DIR C: must list the host file by its 8.3 name (GREETING.TXT).
    for ch in "dir c:\\\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type dir");
    }
    machine
        .run_until_halt_or_cycles(40_000_000)
        .expect("settle dir");
    let dir_text = machine.screen_text().as_text().to_ascii_lowercase();

    // TYPE the host file: this drives the kernel to READ the file's data, which
    // the facade serves lazily (open+seek+read) straight from the host file on
    // disk — proving the data path, not just the synthesized directory entry.
    // The host folder must still exist while the read happens, so the cleanup
    // waits until after both screens are captured.
    for ch in "type c:\\greeting.txt\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type the type command");
    }
    machine
        .run_until_halt_or_cycles(40_000_000)
        .expect("settle type");
    let type_text = machine.screen_text().as_text().to_ascii_lowercase();

    std::fs::remove_dir_all(&dir).ok();
    assert!(
        dir_text.contains("greeti"),
        "DIR C: did not list the host file off the folder facade.\n{dir_text}"
    );
    assert!(
        type_text.contains("hi from the host folder"),
        "TYPE did not print the host file's contents read lazily through the facade.\n{type_text}"
    );
}

/// Regression: with the full graphical POST (the GUI default), the BIOS leaves
/// the Margo linear framebuffer active. A real-MBR FreeDOS boot never sets a
/// video mode, so without the izbios mode-03h reset before the boot jump the
/// GUI stays frozen on the POST splash while the booted OS writes to the hidden
/// VGA text buffer. Boot a host folder with `set_fast_post(false)` (so the
/// graphical POST runs and activates Margo) and assert the display latch is
/// cleared — i.e. the BIOS handed the screen back to VGA text — by the C:\>
/// prompt. The headless `screen_text()` rasterizes the VGA core regardless of
/// the latch, so the `margo_active()` check (not the screen text) is what
/// guards the GUI-visible bug.
#[test]
#[ignore = "boots a full DOS image under the slow graphical POST; run with --ignored"]
fn katea_full_post_boot_hands_display_back_to_vga() {
    let dir = std::env::temp_dir().join(format!(
        "katea_fullpost_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("GREETING.TXT"), b"hi\r\n").expect("write host file");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    // The GUI runs the full graphical POST, which lights the Margo LFB; headless
    // defaults to fast POST (text only), so this is the path the e2e tests miss.
    machine.set_fast_post(false);
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let stop = machine
        .run_until_halt_or_cycles(600_000_000)
        .expect("run machine");
    let text = machine.screen_text().as_text().to_ascii_lowercase();
    std::fs::remove_dir_all(&dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault during full-POST Katea boot: {msg}\n{text}");
    }
    assert!(
        text.contains("c:\\>"),
        "no C:\\> prompt after full-POST folder boot (stop={stop:?}).\n{text}"
    );
    assert!(
        !machine.margo_active(),
        "the BIOS must hand the display back to VGA text before booting; the \
             Margo LFB is still active, so the GUI would show the frozen POST splash"
    );
}

/// The Katea-1 M1 boot-coherence gate: a host folder with a file *in a
/// subfolder* boots the real FreeDOS kernel, the subfolder is navigable, and a
/// file at depth reads lazily through the recursive tree facade. This is the
/// milestone's success gate — it proves the tree volume (`KateaTreeVolume`)
/// produces a self-consistent, bootable disk and that `CD` into a synthesized
/// subdirectory plus a `TYPE` of a file two levels down (`C:\GAMES\HELLO\`)
/// returns the host file's bytes, which M0's flat facade could never expose.
#[test]
#[ignore = "boots a full DOS image from a host-folder tree (slow in debug); run with --ignored"]
fn katea_host_folder_tree_reads_a_file_in_a_subfolder() {
    // A unique scratch tree under the system temp dir: GAMES\HELLO\READAT.TXT
    // lives two directory levels down, so reaching it proves the subfolder
    // chain (root -> GAMES -> HELLO) is navigable and the file reads at depth.
    let dir = std::env::temp_dir().join(format!(
        "katea_tree_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let depth = dir.join("GAMES").join("HELLO");
    std::fs::create_dir_all(&depth).expect("scratch tree");
    std::fs::write(depth.join("READAT.TXT"), b"read at depth ok\r\n").expect("write depth file");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder(&dir)
        .expect("mount host folder tree");
    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("run machine");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during Katea tree boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        boot_text.contains("c:\\>"),
        "no C:\\> prompt after tree boot (stop={stop:?}).\n{boot_text}"
    );

    // CD into the subfolder two levels down, then DIR it. DIR listing the file
    // proves the synthesized subdirectory chain (with its `.`/`..` entries) is
    // navigable.
    for cmd in ["cd games\\hello\r", "dir\r"] {
        for ch in cmd.chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type cmd");
        }
        machine
            .run_until_halt_or_cycles(40_000_000)
            .expect("settle cmd");
    }
    let dir_text = machine.screen_text().as_text().to_ascii_lowercase();

    // TYPE the file at depth: the kernel reads its data clusters, which the
    // tree facade serves lazily (open+seek+read) straight from the host file
    // under GAMES\HELLO — proving the lazy data path at depth, not just the
    // synthesized directory entry. The host tree must still exist while the
    // read happens, so cleanup waits until both screens are captured.
    for ch in "type readat.txt\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type the type command");
    }
    machine
        .run_until_halt_or_cycles(40_000_000)
        .expect("settle type");
    let type_text = machine.screen_text().as_text().to_ascii_lowercase();

    std::fs::remove_dir_all(&dir).ok();
    assert!(
        dir_text.contains("readat"),
        "DIR in the subfolder did not list the file at depth.\n{dir_text}"
    );
    assert!(
        type_text.contains("read at depth ok"),
        "TYPE did not print the subfolder file's contents read lazily through the tree facade.\n{type_text}"
    );
}

/// Katea-1 M2 success gate: real FreeDOS, booted from a host folder, creates a
/// new file, overwrites an existing one, and grows one — all in a subfolder,
/// plus MKDIR a host subdir with a file at depth — and the changes appear
/// correctly in the host folder (verified by host-side read-back), with the
/// read tree + boot intact.
#[test]
#[ignore = "boots a full DOS image and writes through Katea (slow in debug); run with --ignored"]
fn katea_host_folder_writes_create_overwrite_grow_in_a_subfolder() {
    let dir = std::env::temp_dir().join(format!(
        "katea_m2_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let saves = dir.join("SAVES");
    std::fs::create_dir_all(&saves).expect("scratch tree");
    // Pre-seed a file to overwrite and a batch file holding the write commands
    // as exact bytes (so `>`/`>>`/`\` never go through the keyboard).
    std::fs::write(saves.join("OLD.TXT"), b"before\r\n").expect("seed OLD.TXT");
    let make_bat = b"echo created>NEW.TXT\r\n\
echo overwritten>OLD.TXT\r\n\
echo line1>GROW.TXT\r\n\
echo line2>>GROW.TXT\r\n\
mkdir SUB\r\n\
echo deep>SUB\\DEEP.TXT\r\n";
    std::fs::write(saves.join("MAKE.BAT"), make_bat).expect("seed MAKE.BAT");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("run machine");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during Katea M2 boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        boot_text.contains("c:\\>"),
        "no C:\\> prompt after boot (stop={stop:?}).\n{boot_text}"
    );

    // Only letters are typed: cd into SAVES (cheap), then run MAKE.BAT (six
    // file-writing commands, each doing disk I/O + an inline reconcile, so it
    // gets a far larger settle budget than the trivial cd).
    for (cmd, settle) in [("cd saves\r", 40_000_000u64), ("make\r", 120_000_000u64)] {
        for ch in cmd.chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type cmd");
        }
        machine
            .run_until_halt_or_cycles(settle)
            .expect("settle cmd");
    }

    // Final reconcile, then read the host folder back.
    machine.flush_hdd_folder();

    let read = |p: std::path::PathBuf| -> String {
        String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).to_string()
    };
    let new_txt = read(saves.join("NEW.TXT"));
    let old_txt = read(saves.join("OLD.TXT"));
    let grow_txt = read(saves.join("GROW.TXT"));
    let deep_txt = read(saves.join("SUB").join("DEEP.TXT"));
    let sub_is_dir = saves.join("SUB").is_dir();
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        new_txt.contains("created"),
        "NEW.TXT not created: {new_txt:?}"
    );
    assert!(
        old_txt.contains("overwritten") && !old_txt.contains("before"),
        "OLD.TXT not overwritten: {old_txt:?}"
    );
    assert!(
        grow_txt.contains("line1") && grow_txt.contains("line2"),
        "GROW.TXT not grown: {grow_txt:?}"
    );
    assert!(sub_is_dir, "SUB subdir not created on host");
    assert!(
        deep_txt.contains("deep"),
        "SUB\\DEEP.TXT not created: {deep_txt:?}"
    );
}

/// Katea-1 M3 gate: real FreeDOS deletes a file, renames a file in place, moves
/// a file into a subdir, RMDIRs an emptied dir, renames a subdir, and deletes a
/// PRE-EXISTING host file — all reflected in the host folder, read back.
#[test]
#[ignore = "boots a full DOS image and mutates host files via Katea (slow); run with --ignored"]
fn katea_host_folder_delete_rename_move_rmdir() {
    let dir = std::env::temp_dir().join(format!(
        "katea_m3_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let ops = dir.join("OPS");
    std::fs::create_dir_all(ops.join("SUB")).expect("scratch tree");
    std::fs::create_dir_all(ops.join("EMPTYDIR")).expect("emptydir");
    std::fs::create_dir_all(ops.join("OLDDIR")).expect("olddir");
    std::fs::write(ops.join("DELME.TXT"), b"delete me\r\n").unwrap();
    std::fs::write(ops.join("KEEP.TXT"), b"rename me\r\n").unwrap();
    std::fs::write(ops.join("MOVEME.TXT"), b"move me\r\n").unwrap();
    std::fs::write(ops.join("PREEXIST.TXT"), b"existed first\r\n").unwrap();
    // Only built-in FreeCOM commands (MOVE.EXE is not on the Katea C: drive —
    // the boot payload carries only KERNEL/COMMAND/CONFIG/AUTOEXEC). The file
    // move is COPY+DEL; the directory rename is REN, which this FreeCOM's
    // cmd_rename accepts on directories (its findfirst mask includes FA_DIREC).
    // Redirection/`\` chars are exact bytes in the batch so only letters are
    // ever typed.
    let ops_bat = b"del DELME.TXT\r\n\
ren KEEP.TXT RENAMED.TXT\r\n\
copy MOVEME.TXT SUB\\\r\n\
del MOVEME.TXT\r\n\
rmdir EMPTYDIR\r\n\
ren OLDDIR NEWDIR\r\n\
del PREEXIST.TXT\r\n";
    std::fs::write(ops.join("OPS.BAT"), ops_bat).expect("seed OPS.BAT");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("run machine");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during Katea M3 boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        boot_text.contains("c:\\>"),
        "no C:\\> prompt (stop={stop:?}).\n{boot_text}"
    );

    // `ops` runs six commands incl. a COPY (heavier than M2's echoes) + an inline
    // reconcile each, hence a larger settle budget than the M2 e2e's 120M.
    for (cmd, settle) in [("cd ops\r", 40_000_000u64), ("ops\r", 150_000_000u64)] {
        for ch in cmd.chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type cmd");
        }
        machine
            .run_until_halt_or_cycles(settle)
            .expect("settle cmd");
    }
    machine.flush_hdd_folder();

    let exists = |p: std::path::PathBuf| p.exists();
    let del_gone = !exists(ops.join("DELME.TXT"));
    let renamed_new = exists(ops.join("RENAMED.TXT"));
    let renamed_old_gone = !exists(ops.join("KEEP.TXT"));
    let moved_to_sub = exists(ops.join("SUB").join("MOVEME.TXT"));
    let moved_from_root_gone = !exists(ops.join("MOVEME.TXT"));
    let rmdir_gone = !exists(ops.join("EMPTYDIR"));
    let dir_renamed_new = exists(ops.join("NEWDIR"));
    let dir_renamed_old_gone = !exists(ops.join("OLDDIR"));
    let preexist_gone = !exists(ops.join("PREEXIST.TXT"));
    std::fs::remove_dir_all(&dir).ok();

    assert!(del_gone, "DELME.TXT not deleted");
    assert!(renamed_new, "RENAMED.TXT not present after rename");
    assert!(renamed_old_gone, "KEEP.TXT still present after rename");
    assert!(moved_to_sub, "MOVEME.TXT did not arrive in SUB");
    assert!(
        moved_from_root_gone,
        "MOVEME.TXT still in the root (move's delete didn't apply)"
    );
    assert!(rmdir_gone, "EMPTYDIR not removed");
    assert!(dir_renamed_new, "NEWDIR not present after dir rename");
    assert!(
        dir_renamed_old_gone,
        "OLDDIR still present after dir rename"
    );
    assert!(preexist_gone, "pre-existing PREEXIST.TXT not deleted");
}

/// The katea-run gate: a program that exits 42, run through real FreeDOS via
/// --katea-run, makes `katea_run` return 42 — proving boot -> AUTOEXEC -> RUNNER
/// -> EXEC -> AH=4Dh -> unit-tester exit -> TestExit, end to end.
#[test]
#[ignore = "boots a full FreeDOS image to run one program (slow); run with --ignored"]
fn katea_run_captures_a_program_exit_code() {
    // Self-cleaning dir (drops at the end of the test body, after the assert),
    // so a panic mid-test can't leak it — same guard the production katea_run uses.
    let dir = TempDir::new(std::env::temp_dir().join(format!(
            "katea_run_e2e_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
    .unwrap();
    let prog = dir.path().join("EXIT42.COM");
    std::fs::write(&prog, izarravm_firmware::exit42_com()).unwrap();

    let code =
        katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Et4000Ax)).expect("katea_run");
    assert_eq!(
        code, 42,
        "the program's DOS exit code must reach the host process"
    );
}

/// The round-1 CRITICAL regression gate: a guest that does `sti; hlt` under the
/// default TOKAEMM boot must resume and exit cleanly, not crash the host. HLT is
/// privileged on real 386+ (CPL != 0 -> #GP(0)); a V86 task is always CPL 3, so
/// every guest HLT traps into TOKAEMM's monitor, which must emulate it (a real
/// ring-0 `sti; hlt`, IRQ-wake, then resume the guest past the F4 byte) without
/// misrouting the waking IRQ's ring-0 frame through the V86 reflect path (the
/// bug: `irq_body` reflected on the frame regardless of who was interrupted,
/// corrupting it and loading a guest IVT segment as a protected-mode selector).
#[test]
#[ignore = "boots a full FreeDOS image to run one program (slow); run with --ignored"]
fn katea_run_guest_hlt_resumes_and_exits_under_tokaemm() {
    let dir = TempDir::new(std::env::temp_dir().join(format!(
            "katea_run_hlt_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
    .unwrap();
    let prog = dir.path().join("HLTTEST.COM");
    std::fs::write(&prog, izarravm_firmware::hlttest_com()).unwrap();

    let code =
        katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Et4000Ax)).expect("katea_run");
    assert_eq!(
        code, 1,
        "a guest sti;hlt must resume past the F4 byte and reach its own exit, \
             not crash the host with a CpuError"
    );
}

/// Same gate as above, repeated five times in a guest loop: catches drift across
/// repeated halts (a corrupted saved register or a stack-depth leak in TOKAEMM's
/// HLT emulation that only shows up on the second or later wake).
#[test]
#[ignore = "boots a full FreeDOS image to run one program (slow); run with --ignored"]
fn katea_run_repeated_guest_hlt_resumes_and_exits_under_tokaemm() {
    let dir = TempDir::new(std::env::temp_dir().join(format!(
            "katea_run_multihlt_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
    .unwrap();
    let prog = dir.path().join("MULTIHLT.COM");
    std::fs::write(&prog, izarravm_firmware::multihlt_com()).unwrap();

    let code =
        katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Et4000Ax)).expect("katea_run");
    assert_eq!(
        code, 7,
        "five sequential guest HLTs must all resume cleanly under TOKAEMM"
    );
}

/// TokaEdit e2e: EDIT.COM opens a new file at the prompt, text is typed,
/// File>Save and File>Exit are driven via Alt-menu keys, and the saved bytes
/// arrive on the host through the Katea folder.
///
/// Root-caused a real, if rare, phantom-Alt bug (not the startup
/// `EV_ALT_TAP` a prior investigation suspected -- that edge detector is
/// fine, and a live COM1 trace showed it never fired here). The actual
/// mechanism, isolated with a `dispatch()`-side COM1 trace: a single
/// `_bios_keybrd(_KEYBRD_SHIFTSTATUS)` (`int 16h ah=02h`) call -- and even
/// a direct peek of the BDA `KB_FLAGS` byte (0x417) itself, bypassing INT
/// 16h entirely -- can occasionally read the Alt bit set for exactly one
/// poll iteration coinciding with an ordinary keystroke (observed: 'h',
/// scancode 0x23), with no sustained held key behind it and nothing
/// visible to external host-side sampling even at 2500-cycle granularity.
/// `ev_wait` then reports that key as Alt+H, which `dispatch()` treats as
/// the Alt+H menu hotkey, opening Help instead of typing the letter. This
/// reproduces deterministically with the committed image (fails 8/8,
/// passed with an earlier image build whose embedded FreeDOS
/// kernel-build timestamp shifted every downstream byte offset) -- a
/// genuine timing-sensitive glitch in the keyboard delivery path, not a
/// logic bug in the BIOS assembly (`kbd-bios-core.inc`'s `.flags` handler
/// is a plain `mov al,[KB_FLAGS]`), the CPU's INT/IRET/IRQ dispatch
/// (independently audited clean: IRQ1 only lands at instruction
/// boundaries and never touches AX), the 8042 model, or the mouse driver
/// (the glitch reproduces with TOKAMOUS unloaded too). A same-iteration
/// debounce (reading shift status twice and ANDing) was tried and
/// rejected: the glitch is wide enough to survive two back-to-back INT
/// 16h calls, and the debounce then started eating the *real* Alt+F
/// chord too. Fixed on the editor side in `ev_wait`
/// (toka-dos/tools-src/edit/tui.c) with a cross-poll confirmation
/// counter per modifier bit: a bit only counts toward `e->mods` once it
/// has read high on two consecutive `ev_wait` *loop iterations* (not two
/// reads within one), which a one-poll glitch cannot satisfy but a
/// genuinely held key (down across many polls) satisfies almost
/// instantly. Spacing each of the four Alt+F scancodes with its own
/// `run_until_halt_or_cycles` call (below) lets the menu open reliably.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn tokaedit_edits_and_saves_a_file() {
    let dir = std::env::temp_dir().join(format!(
        "tokaedit_e2e_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("run machine");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during TokaEdit e2e boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    if !boot_text.contains("c:\\>") {
        std::fs::remove_dir_all(&dir).ok();
        panic!("no C:\\> prompt after boot (stop={stop:?}).\n{boot_text}");
    }

    // `edit HELLO` opens a new file named HELLO (no '.' keystroke needed).
    for ch in "edit hello\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type edit command");
    }
    machine
        .run_until_halt_or_cycles(100_000_000)
        .expect("settle edit launch");
    let editor_text = machine.screen_text().as_text();
    let editor_text_upper = editor_text.to_ascii_uppercase();
    if !editor_text_upper.contains("HELLO") {
        std::fs::remove_dir_all(&dir).ok();
        panic!("EDIT did not open HELLO (stop={stop:?}).\n{editor_text}");
    }

    // Type the document body: plain characters, one at a time.
    for ch in "hi".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type document text");
    }

    let body_text = machine.screen_text().as_text();
    if !body_text.contains("hi") {
        let trace = String::from_utf8_lossy(machine.serial_output()).into_owned();
        std::fs::remove_dir_all(&dir).ok();
        panic!(
            "document text 'hi' did not land in the buffer.\n{body_text}\n=== COM1 ===\n{trace}"
        );
    }

    // File > Save: Alt+F opens the File menu, then 's' picks "&Save".
    // Each scancode gets its own run slice -- see the root-cause note
    // above for why a single batched injection can't deliver this chord.
    for code in [0x38u8, 0x21, 0xa1, 0xb8] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("alt+f chord step");
    }
    let menu_text = machine.screen_text().as_text();
    if !menu_text.contains("Save") {
        std::fs::remove_dir_all(&dir).ok();
        panic!("File menu did not open after Alt+F.\n{menu_text}");
    }
    for code in ascii_to_set1('s') {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("save hotkey step");
    }
    machine
        .run_until_halt_or_cycles(50_000_000)
        .expect("settle save");

    // File > Exit: Alt+F, then 'x' picks "E&xit".
    for code in [0x38u8, 0x21, 0xa1, 0xb8] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("alt+f chord step (exit)");
    }
    for code in ascii_to_set1('x') {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("exit hotkey step");
    }
    machine
        .run_until_halt_or_cycles(100_000_000)
        .expect("settle exit");

    let after_exit = machine.screen_text().as_text().to_ascii_lowercase();
    if !after_exit.contains("c:\\>") {
        std::fs::remove_dir_all(&dir).ok();
        panic!("did not return to the C:\\> prompt after Save/Exit.\n{after_exit}");
    }

    machine.flush_hdd_folder();
    let saved = std::fs::read(dir.join("HELLO")).expect("HELLO written to host folder");
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        saved, b"hi\r\n",
        "saved HELLO bytes did not match the typed document body"
    );
}

/// SP-4b M0 GO/NO-GO: `DEVICE=C:\DOS\TOKAEMM.SYS` puts the running kernel into V86
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
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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

/// SP-4b M1 GO/NO-GO: a guest program install-checks XMS, allocates a 64 KB EMB,
/// locks it, moves a pattern conventional->EMB->conventional, verifies it, then
/// unlocks and frees — all in V86 under TOKAEMM's monitor (block MOVE traps to
/// the monitor's flat memcpy). XMSTEST.COM signals 0xA5 (success) via the
/// unit-tester exit port; any other code names the step that broke.
///
/// The config is NOEMS so host EMS reserves no extended RAM and the guest XMS
/// driver owns all of it (the M2 EMS-coexistence reconciliation is separate).
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

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nXMSTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M3 GO/NO-GO: with DEVICE=TOKAEMM.SYS + DOS=UMB, a guest program sets
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

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDOS=UMB\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M3 mechanism test: drives TOKAEMM's XMS 10h/11h/12h directly (no
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

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBMECH\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M2 GO/NO-GO: with DEVICE=TOKAEMM.SYS RAM, a guest program drives the
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M2 coexistence: with DEVICE=TOKAEMM.SYS RAM *and* DOS=UMB, the UMB
/// window ends below the EMS page frame (umb_win_end = 0xE000) and DOS=UMB
/// still links and allocates upper memory from the carved window — the frame
/// and the UMBs share the upper area under the guest driver's own bookkeeping.
/// Reuses the M3 UMBTEST fixture (seg >= 0xC800 + write/read pattern).
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M2 default-off contract: a bare DEVICE=TOKAEMM.SYS (no RAM argument)
/// presents a FRAMELESS manager — INT 67h answers present/version 4.0, the
/// frame query returns 80h, page counts are zero, and allocation is refused
/// with 87h (the EMM386 NOEMS contract). EMSNONE signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_ems_frameless_default_in_v86() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_m2f_{}_{}",
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
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMSNONE\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// VCPI M0 presence: under a bare DEVICE=TOKAEMM.SYS (frameless default,
/// no EMS pool — the stock-boot shape), INT 67h AX=DE00h answers VCPI 1.0
/// present (AH=0, BX=0100h), a not-yet-implemented DExx subfunction
/// answers 8Fh, untouched registers survive the call, and the plain EMS
/// interface keeps working on the shared vector. VCPIDET signals
/// 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m0_de00_present_on_frameless_default() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_vcpi0_{}_{}",
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
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIDET\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// VCPI M1 queries + page pool: under a bare DEVICE=TOKAEMM.SYS, the
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

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIMEM\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
        "VCPI M1 query/page-pool contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI M2 DE01: under a bare DEVICE=TOKAEMM.SYS, Get Protected Mode
/// Interface fills the client page-table buffer (identity first-MB
/// entries, software bits 9-11 cleared, exactly 0x110 entries, DI
/// advanced), furnishes the three server GDT descriptors (32-bit CPL0
/// code / flat-4GB data / driver data sharing the code base), and
/// returns a nonzero in-segment PM entry offset. VCPIIF signals
/// 0xA5 / 0xEn. (The PM entry itself is exercised by the M3 switch
/// fixture — it can only be far-called from protected mode.)
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

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIIF\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
        "VCPI M2 DE01 interface contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI M3 DE0C: a minimal REAL VCPI client walks the full extender
/// lifecycle under a bare DEVICE=TOKAEMM.SYS — DE01 interface setup,
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

    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPISW\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
        "VCPI M3 switch round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step (0xEF = DE0C returned).\n{text}"
    );
}

/// VCPI M4 real-monitor contract: a V86 program that hooks INT 0Dh and
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// VCPI M6 privileged-0F emulation (386MAX GP_ESCOD surface port): a V86
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M4 GO/NO-GO: a fresh (empty) user folder gets the NEW defaults seeded
/// (`ensure_user_config`) — DEVICE=TOKAEMM.SYS NOEMS + DOS=HIGH,UMB + LH
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");

    // The seeding wrote real, editable defaults into the user folder.
    let seeded = std::fs::read_to_string(dir.join("CONFIG.SYS")).expect("seeded CONFIG.SYS");
    assert!(
        seeded.contains("DEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS") && seeded.contains("DOS=HIGH,UMB"),
        "seeded CONFIG.SYS lacks the M4 defaults:\n{seeded}"
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

/// GSWMODE (coverage audit item 18): a runtime CPU-speed switch guest tool.
/// Default V86 boot (TOKAEMM resident, DOS=HIGH,UMB), then AUTOEXEC drives
/// `GSWMODE 486` — a downgrade from the default 586 that, unlike 286, keeps
/// the CPU at `has_pentium_isa()`-adjacent 386+ ISA (no 32-bit-prefix #UD
/// gate; see the 286 test below for why that gate matters). Then `VER` to
/// prove DOS still works post-switch, then `GSWMODE 586` to prove switching
/// back also works. Driven entirely through AUTOEXEC.BAT bytes (not injected
/// keystrokes): the default keyboard layout is European, so scancode
/// injection can garble punctuation, and this test needs none of that.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_gswmode_486_switch_survives_v86_monitor() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_gsw486_{}_{}",
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
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGSWMODE 486\r\nVER\r\nGSWMODE 586\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
            "CPU fault after the GSWMODE 486 switch while TOKAEMM's ring-0 \
                 monitor was resident: {msg}\nstop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "GSWMODE 486 then GSWMODE 586 should leave the machine back at 586 \
             (stop={stop:?}).\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("switched to 486") && lower.contains("switched to 586"),
        "GSWMODE confirmation output missing for one of the two switches.\n{text}"
    );
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the GSWMODE 486/VER/GSWMODE 586 sequence \
             (stop={stop:?}).\n{text}"
    );
}

/// GSWMODE 286 while TOKAEMM's ring-0 monitor is resident — the risk case
/// flagged when this tool was built (coverage audit item 18). This is the
/// inverted (survives) shape of the retired pinned-limitation test
/// `tokaemm_gswmode_286_switch_hits_the_known_monitor_limit`; it needs TWO
/// fixes to pass, landing in separate PRs:
///
///  1. izarravm-cpu: the I286 guest ISA gate (66h/67h prefixes + 386-only
///     0F opcodes) must exempt ring-0 protected mode (`is_ring0_protected`)
///     the way it exempts firmware ROM, so TOKAEMM's 32-bit monitor keeps
///     running below the V86 guest (branch vorvek/i286-monitor-gate).
///  2. TOKAEMM itself: the guest-facing V86 code (XMS/EMS/UMB entry points)
///     must be 286-clean — V86 and real mode stay at true-286 ISA fidelity,
///     so a MOVZX there (e.g. the old `movzx bx, ah` in ems_int67, hit by
///     EMS-presence probing on every EXEC) raises #UD, cascades through
///     TOKAEMM's unpopulated low IDT gates, and kills the machine (this PR;
///     the whole V86 section now assembles under `cpu 286`).
///
/// Sequence mirrors the 486 sibling above: GSWMODE 286 (the EXEC's own EMS
/// probe already exercises ems_int67 at I286), VER at the 286 level, then
/// GSWMODE 586 (a second EXEC at I286) to prove switching back works.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_gswmode_286_switch_survives_the_monitor() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_gsw286_{}_{}",
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
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGSWMODE 286\r\nVER\r\nGSWMODE 586\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
            "CPU fault after the GSWMODE 286 switch while TOKAEMM's ring-0 \
                 monitor was resident: {msg}\nstop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "GSWMODE 286 then GSWMODE 586 should leave the machine back at 586 \
             (stop={stop:?}).\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("switched to 286") && lower.contains("switched to 586"),
        "GSWMODE confirmation output missing for one of the two switches.\n{text}"
    );
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the GSWMODE 286/VER/GSWMODE 586 sequence \
             (stop={stop:?}).\n{text}"
    );
}

/// The minimal, CPU-side-only companion to the full-boot sibling above:
/// pins the izarravm-cpu ring-0 ISA-gate exemption in isolation, with no
/// GSWMODE.COM EXEC and no XMS/EMS traffic involved, so a regression in
/// the CPU gate is distinguishable from a regression in TOKAEMM's
/// guest-facing 286-clean code (each has its own test).
///
/// FORMERLY A KNOWN LIMITATION (see the CPU-side fix): TOKAEMM's monitor
/// code is 32-bit-default (`vec13_entry` opens with `pushad` then
/// `66 B8 10 00 / mov ds, ax` -- the `66` operand-size prefix is required
/// just to load a 16-bit segment register from 32-bit-default code). The
/// I286-level guest ISA gate in `crates/izarravm-cpu/src/lib.rs`
/// (`read_prefixes`, `check_two_byte_isa_gate`) used to reject
/// `0x66`/`0x67` and 386+ 0F opcodes with #UD (vector 6) for ANY
/// non-firmware-ROM fetch, including the monitor's own ring-0
/// protected-mode code -- which then faulted a second time because
/// TOKAEMM's IDT only populates gates for vector 8 upward (IRQ0..IRQ15),
/// so vector 6 read a zeroed/garbage gate and GP-faulted loading a null
/// code selector.
///
/// FIXED: the gate now also exempts ring-0 protected-mode fetches
/// (`is_ring0_protected()`: PE set, CPL 0, not V86) -- the monitor is
/// chipset-side code, not guest software, so the guest-facing 286 ISA
/// boundary should never apply to it. V86 and real-mode code stay gated
/// exactly as before; the exemption ASSUMES ring-0 PM is only ever the
/// chipset-side monitor (see the gate comments in izarravm-cpu for the
/// full assumption + the VCPI/DPMI revisit trigger). Proven directly
/// below by driving the monitor entry point (a reflected IRQ) at the
/// I286 level with no DOS session involved.
///
/// (A second, separate gap this test deliberately avoided -- TOKAEMM's
/// guest-facing V86 XMS/EMS code used 386-only instructions, so a full
/// 286 DOS session died in `ems_int67` on the first EXEC's EMS probe --
/// was RESOLVED by PR #388, which made the whole V86-facing section
/// assemble under `cpu 286`. The full-boot sibling above now covers that
/// path end to end.)
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_gswmode_286_monitor_entry_survives_at_i286() {
    // Minimal, EMS/XMS-free repro of the ring-0 monitor path the CPU fix
    // covers: boot normally (TOKAEMM resident, default speed, so
    // init/SYSINIT run at full ISA and the monitor is live), THEN drop
    // straight to I286 with `Machine::set_mode` (bypassing GSWMODE.COM's
    // EXEC and all XMS/EMS traffic), and inject a keystroke.
    // A keypress raises IRQ1, which the monitor's `reflect_vector` machinery
    // (entered via `vec13_entry`, the same ring-0 entry point the fault used
    // to hit) must service and reflect into the V86 guest's INT 09h -- all
    // while the CPU is throttled to the I286 ISA. Before the fix this GP
    // faulted with "loading selector 0x0000" at CS=0x0008 (vec13_entry
    // itself); after the fix the reflect completes and the guest is still
    // alive and still in V86.
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_gsw286mon_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let config = b"FILES=40\r\nLASTDRIVE=D\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
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

    let stop = machine
        .run_until_halt_or_cycles(500_000_000)
        .expect("machine run");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during the default V86 boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text();
    assert!(
        boot_text.to_ascii_lowercase().contains("c:\\>"),
        "no C:\\> prompt on the default boot (stop={stop:?}).\n{boot_text}"
    );
    // The cycle budget can expire while the CPU is transiently inside the
    // ring-0 monitor (a reflected IRQ), where in_v86() reads false on a
    // healthy boot (same re-sampling pattern as the M4 default-boot test).
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
        in_v86,
        "the default boot must leave the guest running in V86.\n{boot_text}"
    );

    // Drop to true 286 ISA (no Lotura port write, no GSWMODE.COM EXEC --
    // isolates the CPU gate from the still-open TOKAEMM EMS/XMS gap).
    // `set_mode` drives `cpu.set_level` internally (see
    // `set_mode_drives_cpu_level_and_cache_table` in izarravm-machine).
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);

    // A keypress raises IRQ1, routing through the monitor's reflect path.
    machine.inject_key_scancodes(&[0x1e, 0x9e]); // 'a' make + break
    let mut stop = machine
        .run_until_halt_or_cycles(20_000_000)
        .expect("machine run after the I286 switch");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!(
            "CPU fault reflecting a keyboard IRQ through TOKAEMM's ring-0 \
                 monitor at the I286 level: {msg}\nstop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();
    assert!(
        text.contains("C:\\>a"),
        "the reflected keystroke never reached the guest prompt \
             (stop={stop:?}).\n{text}"
    );
    // Same transient re-sample as above: in_v86() can read false while the
    // CPU is inside the monitor servicing the tail of the reflected IRQ.
    let mut in_v86 = machine.in_v86();
    for _ in 0..4 {
        if in_v86 || matches!(stop, StopReason::CpuError(_)) {
            break;
        }
        stop = machine
            .run_until_halt_or_cycles(1_000_000)
            .expect("machine re-sample run after the I286 switch");
        in_v86 = machine.in_v86();
    }
    std::fs::remove_dir_all(&dir).ok();
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault reflecting a keyboard IRQ through TOKAEMM's ring-0 \
                 monitor at the I286 level: {msg}\nstop={stop:?}\n{text}"
        );
    }
    assert!(
        in_v86,
        "the guest must still be running in V86 after the I286 switch \
             and a reflected IRQ (stop={stop:?}).\n{text}"
    );
}

/// UDPROBE.COM: a 62-byte guest fixture that installs its own INT 06h
/// handler and then executes a 386-only opcode (`movzx ax, bl`). At the
/// 286 ISA level the opcode raises #UD; a faithful V86 monitor reflects
/// vector 6 to the guest IVT and the handler prints "UDPROBE CAUGHT". On
/// a 386+ level the opcode simply executes and it prints "UDPROBE MISSED"
/// (so running it at the wrong level is visible). NASM source:
/// ```text
/// cpu 286
/// org 0x100
/// start:  mov ax, 0x2506          ; DOS set INT 06h -> DS:DX (DS=CS in a .COM)
///         mov dx, handler
///         int 0x21
///         db 0x0F, 0xB6, 0xC3     ; movzx ax, bl -- 386-only, #UD at 286
///         mov dx, msg_missed      ; fell through: the opcode executed
///         jmp print_exit
/// handler: mov dx, msg_caught     ; reflected #UD lands here; never IRETs back
/// print_exit:
///         mov ah, 9
///         int 0x21
///         mov ax, 0x4C00
///         int 0x21
/// msg_caught: db 'UDPROBE CAUGHT', 0x0D, 0x0A, '$'
/// msg_missed: db 'UDPROBE MISSED', 0x0D, 0x0A, '$'
/// ```
const UDPROBE_COM: [u8; 62] = [
    0xb8, 0x06, 0x25, 0xba, 0x10, 0x01, 0xcd, 0x21, 0x0f, 0xb6, 0xc3, 0xba, 0x2d, 0x01, 0xeb, 0x03,
    0xba, 0x1c, 0x01, 0xb4, 0x09, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, 0x55, 0x44, 0x50, 0x52,
    0x4f, 0x42, 0x45, 0x20, 0x43, 0x41, 0x55, 0x47, 0x48, 0x54, 0x0d, 0x0a, 0x24, 0x55, 0x44, 0x50,
    0x52, 0x4f, 0x42, 0x45, 0x20, 0x4d, 0x49, 0x53, 0x53, 0x45, 0x44, 0x0d, 0x0a, 0x24,
];

/// A guest PROGRAM hitting a 386-only instruction at GSWMODE 286 must not
/// kill the machine: TOKAEMM's monitor now has IDT gates for the CPU
/// exceptions V86 code can raise (#DE/#UD/#NM) and reflects them to the
/// guest's real-mode IVT — period-faithful (DOS-era INT 06h handling: the
/// program deals with it or dies; the system survives). Before those gates
/// existed, the #UD hit a zeroed IDT descriptor and the whole machine died
/// with "general protection fault while loading selector 0x0000".
/// Same dependency as the test above: needs the izarravm-cpu ring-0 gate
/// exemption (PR #387) so the monitor itself runs at I286.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_gswmode_286_guest_ud_reflects_to_the_ivt() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_udreflect_{}_{}",
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
    let autoexec =
        b"@ECHO OFF\r\nPATH C:\\DOS\r\nGSWMODE 286\r\nUDPROBE\r\nGSWMODE 586\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
                ("UDPROBE.COM".to_string(), UDPROBE_COM.to_vec()),
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
            "CPU fault: a guest-program #UD at the 286 level must reflect to \
                 the IVT, not kill the machine: {msg}\nstop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("udprobe caught"),
        "UDPROBE's INT 06h handler never ran — the #UD was not reflected to \
             the guest IVT (stop={stop:?}).\n{text}"
    );
    assert!(
        !lower.contains("udprobe missed"),
        "UDPROBE fell through the MOVZX — the CPU was not at the 286 ISA \
             level when it ran (stop={stop:?}).\n{text}"
    );
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "the run should end back at 586 (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the UDPROBE sequence (stop={stop:?}).\n{text}"
    );
}

/// A cold boot in GSW-286 mode must reach a working prompt. POST applies
/// the CMOS-seeded mode (port 0xE1) before INT 19h, so the whole boot
/// chain runs at the true-286 ISA level — and the Katea boot chain (the
/// repo MBR + the FreeDOS FAT32-LBA VBR inside tokados-hdd.img) was 386
/// code executing from RAM: the MBR's first `66`-prefixed instruction
/// (`mov eax, [si+8]`, linear 0x642) raised #UD into IVT[6] = a bare IRET
/// stub, which returned to the same instruction — an infinite fault loop
/// with a blank screen. The kernel itself is XCPU=86 (8086-compiled), so
/// the boot sectors were the only 386 code in the chain; both are now
/// 8086/286-clean (word-pair LBA math). This matters because the Del
/// setup panel offers 286 as a saved boot mode.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn gsw286_cold_boot_reaches_a_prompt() {
    let dir = std::env::temp_dir().join(format!(
        "diag286_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let config = b"FILES=40\r\nLASTDRIVE=D\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVER\r\n".to_vec();
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.cpu = GswMode::Gsw386Slow;
    profile.clock_hz = GswMode::Gsw386Slow.clock_hz();
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
            ],
        )
        .expect("mount");
    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!("CPU fault during a GSW-286 cold boot: {msg}\nstop={stop:?}\n{text}");
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386Slow,
        "the machine should still be in the saved 286 mode (stop={stop:?}).\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("toka-dos version"),
        "VER output missing after a 286 cold boot (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after a 286 cold boot (stop={stop:?}).\n{text}"
    );
}

/// A 286 cold boot with DEVICE=TOKAEMM.SYS in CONFIG.SYS: TOKAEMM's INIT
/// is inherently 386-only (its whole job is building the 386 PM/paging
/// monitor), so on a pre-386 level it must decline like real EMM386 on a
/// 286 — read the Lotura mode register (port 0xE1, code 3 = 286), print a
/// "requires a 386" line, return a failed INIT with nothing resident —
/// and DOS then boots on bare metal (no V86, no XMS/UMB), which is what a
/// real 286 box looks like. Without the guard, INIT's first MOVZX raises
/// #UD into the IVT's bare-IRET default and the boot hangs forever.
/// Needs no CPU-side exemption: the monitor never installs.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn tokaemm_init_bails_gracefully_on_a_286_boot() {
    let dir = std::env::temp_dir().join(format!(
        "tokaemm_286boot_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    // No DOS=HIGH,UMB: with TOKAEMM declining to install there is no XMS
    // provider, and this test is about the INIT bail, not kernel warnings.
    let config = b"FILES=40\r\nLASTDRIVE=D\r\n\
DEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVER\r\n".to_vec();

    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.cpu = GswMode::Gsw386Slow;
    profile.clock_hz = GswMode::Gsw386Slow.clock_hz();
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
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        panic!(
            "CPU fault during a 286-mode boot: TOKAEMM INIT must detect the \
                 pre-386 level and decline, not execute 386 code: {msg}\n\
                 stop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();

    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("requires a 386"),
        "TOKAEMM INIT's pre-386 bail message is missing (stop={stop:?}).\n{text}"
    );
    assert!(
        !lower.contains("system running in v86"),
        "TOKAEMM printed its signon banner — it must not install on a 286 \
             (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after the 286-mode boot (stop={stop:?}).\n{text}"
    );
}

/// Audit item 10: the vendored FreeDOS MEM (toka-dos/freedos/mem) runs under
/// the default V86 boot and both `MEM` and `MEM /P` produce sane output.
/// Toka-DOS diverges from upstream MEM here: upstream's `/P` is only a
/// prefix of `/PAGE` (pause after each screenful); the owner's spec wants
/// `/P` to list resident programs with size + segment, so mem2.c's main()
/// was patched to make `/PAGE` (and therefore `/P`) also imply `/FULL`
/// (see toka-dos/freedos/VENDOR.md). Each invocation gets its own boot (the
/// 25-row text console can't hold both outputs at once — /P's per-program
/// table alone is longer than a screenful), driven by AUTOEXEC.BAT (never
/// injected keystrokes, per the guest-testing convention).
fn run_mem_command(dir_suffix: &str, mem_args: &str) -> (String, StopReason) {
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
        format!("@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\nMEM {mem_args}\r\n").into_bytes();
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
    let text = machine.screen_text().as_text();
    std::fs::remove_dir_all(&dir).ok();
    (text, stop)
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_plain_reports_conventional_memory() {
    let (text, stop) = run_mem_command("plain", "");
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
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_p_lists_resident_programs() {
    let (text, stop) = run_mem_command("p", "/P");
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running MEM /P under V86: {msg}\n{text}");
    }
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after MEM /P ran (stop={stop:?}).\n{text}"
    );
    // Toka-DOS divergence check: /P must produce the per-program size +
    // segment listing (upstream /P is only pagination). TOKAMOUS was
    // loaded (LH) right before MEM /P ran, so it must be a resident name
    // in the table; COMMAND.COM is always resident as a fallback check.
    let upper = text.to_ascii_uppercase();
    assert!(
        upper.contains("TOKAMOUS") || upper.contains("COMMAND"),
        "MEM /P output doesn't list a known resident program (TOKAMOUS/COMMAND) \
             (stop={stop:?}).\n{text}"
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
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M4: the PS/2 mouse works under the default V86 boot — a host-injected
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
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// SP-4b M4: SB16 IRQ5 under V86 — IRQ5 lands on vector 13, shared with #GP,
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
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
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

/// V86 trap tax (dev_docs/2026-07-02-v86-trap-tax) owner measurement: not a CI
/// gate, a local one-off ad hoc report. Boots a real corpus game (Prince of
/// Persia) through the Katea host-folder facade at a given GSW mode and prints
/// the vec13 monitor-trip rate, the monitor-resident core-clock share, and the
/// framebuffer-progress rate, comparing before/after the trap-tax fix. Skips
/// (does not fail) when the local corpus path is absent, since it is
/// machine-local, not a repo fixture.
#[test]
#[ignore = "owner measurement only; needs a local corpus path, not a CI fixture"]
fn v86_trap_tax_prince_of_persia_measurement() {
    let corpus_dir = std::path::Path::new(
        "R:\\La Colecci\u{f3}n by Neville\\dosroot\\Prince of Persia (Castellano)",
    );
    if !corpus_dir.is_dir() {
        eprintln!("skipping: corpus dir not found at {}", corpus_dir.display());
        return;
    }
    // FNV-1a over the VGA graphics window (0xA0000) plus a text-window slab
    // (0xB8000): the framebuffer-progress hash. A change between two slices
    // means the game drew something new, whatever mode it is in.
    fn vram_hash(machine: &mut Machine) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for addr in (0xA0000u32..0xB0000).chain(0xB8000..0xB9000) {
            h ^= u64::from(machine.read_physical_u8(addr));
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }
    for (label, mode) in [("486", GswMode::Gsw486), ("586", GswMode::Gsw586)] {
        let profile = MachineProfile {
            cpu: mode,
            clock_hz: mode.clock_hz(),
            ..MachineProfile::gsw_386(16, VideoCard::Et4000Ax)
        };
        let mut machine =
            Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
        // Launch the game: PRINCE ADLIB is Prince of Persia's own command
        // line (ADLIB selects AdLib sound), run from AUTOEXEC so the measured
        // window is the game's title/attract sequence, not the DOS prompt.
        machine
            .mount_hdd_folder_with(
                corpus_dir,
                vec![(
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nPATH C:\\DOS\r\nPRINCE ADLIB\r\n".to_vec(),
                )],
            )
            .expect("mount corpus folder");
        // Phase 1: skip the boot + intro start (BIOS, FreeDOS boot, PRINCE.EXE
        // load, first title fade-in) so the measured window is steady-state
        // title/attract animation, not one-time setup. Same guest-seconds at
        // every mode (clock_hz differs), so both modes measure the same
        // simulated game time.
        let intro_skip_s = 10.0f64;
        let measure_s = 12.0f64;
        let intro_stop = machine
            .run_cycles((intro_skip_s * mode.clock_hz() as f64) as u64)
            .expect("intro skip run");
        assert!(
            matches!(intro_stop, StopReason::CycleLimit { .. }),
            "the game must still be running at the end of the intro skip \
                 (a halt/fault here means the workload died, e.g. an unmapped \
                 probe port faulting the VM): {intro_stop:?}\n{}",
            machine.screen_text().as_text()
        );
        // Snapshot the cumulative counters at the start of the measured window.
        let perf0 = machine.cpu().perf_counters().clone();
        let elapsed0 = machine.elapsed_clocks();
        let mut hash = vram_hash(&mut machine);
        let mut fb_changes = 0u64;
        // Phase 2: the measured window, in 100 slices, hashing the VGA window
        // after each slice. Wall time brackets ONLY this window.
        let slices = 100u64;
        let slice_cycles = (measure_s * mode.clock_hz() as f64) as u64 / slices;
        let wall0 = std::time::Instant::now();
        for _ in 0..slices {
            machine.run_cycles(slice_cycles).expect("measured slice");
            let next = vram_hash(&mut machine);
            if next != hash {
                fb_changes += 1;
                hash = next;
            }
        }
        let wall = wall0.elapsed();
        let perf1 = machine.cpu().perf_counters().clone();
        let elapsed1 = machine.elapsed_clocks();

        let elapsed = elapsed1 - elapsed0;
        let guest_seconds = elapsed as f64 / mode.clock_hz() as f64;
        let wall_seconds = wall.as_secs_f64();
        let trips = perf1.monitor_trips_vec13 - perf0.monitor_trips_vec13;
        let monitor_clocks =
            perf1.monitor_resident_core_clocks - perf0.monitor_resident_core_clocks;
        let brk_step = perf1.brk_step - perf0.brk_step;
        let trips_per_s = trips as f64 / guest_seconds;
        let monitor_share = monitor_clocks as f64 / elapsed.max(1) as f64;
        let clocks_per_trip = monitor_clocks as f64 / trips.max(1) as f64;
        let brk_step_per_trip = brk_step as f64 / trips.max(1) as f64;
        // Game pace: per guest second it MUST match before/after (guest timing
        // is unchanged by design); per WALL second is the user-felt pace the
        // trap tax was suppressing.
        let fb_per_guest_s = fb_changes as f64 / guest_seconds;
        let fb_per_wall_s = fb_changes as f64 / wall_seconds;
        let rt_factor = guest_seconds / wall_seconds;
        println!(
            "cpu={label} window={measure_s}s(after {intro_skip_s}s intro) \
                 trips/s={trips_per_s:.1} monitor_share={monitor_share:.4} \
                 core_clocks/trip={clocks_per_trip:.1} brk_step/trip={brk_step_per_trip:.3} \
                 fb_changes={fb_changes} fb/guest_s={fb_per_guest_s:.2} \
                 fb/wall_s={fb_per_wall_s:.2} wall_ms={:.1} rt_factor={rt_factor:.3} \
                 trips={trips} monitor_clocks={monitor_clocks} brk_step={brk_step} \
                 elapsed_clocks={elapsed}",
            wall_seconds * 1000.0,
        );
        if fb_changes == 0 {
            // The measured window saw no drawing: dump the text screen so the
            // report can say where the machine actually was.
            println!("--- screen at window end ---\n{}", {
                machine.screen_text().as_text()
            });
        }
    }
}
