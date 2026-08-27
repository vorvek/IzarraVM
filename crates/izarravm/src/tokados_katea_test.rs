// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// Katea-1 Milestone-0 GO/NO-GO gate: the real FreeDOS kernel must boot to
// C:\> from the synthesized static FAT32 HDD image (no floppy) and read a file
// off the volume. If this passes, the static-image approach is proven and the
// lazy host-folder facade work can begin; if it cannot reach C:\>, it is a
// NO-GO and we stop before any facade work.
#[test]
#[ignore = "boots a full DOS image from a FAT32 HDD (slow in debug); run with --ignored"]
fn katea_static_hdd_boots() {
    let (mut machine, stop, boot_cycles) = boot_hdd(500_000_000);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        panic!(
            "CPU fault during Katea HDD boot after {boot_cycles} requested cycles: {msg}\n\
             stop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text().to_ascii_lowercase();
    // The kernel must assign the FAT32 partition to C: and prompt there, NOT A:.
    assert!(
        text.contains("c:\\>"),
        "no C:\\> prompt after {boot_cycles} requested cycles (stop={stop:?}).\n{text}"
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
    run_until_toka_condition(&mut machine, 20_000_000, |machine| {
        current_root_prompt(machine)
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .contains("c:\\>ver")
    });
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
    run_until_toka_condition(&mut machine, 40_000_000, |machine| {
        current_root_prompt(machine)
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .contains("command")
    });
    let dir_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        dir_text.contains("command"),
        "DIR C:\\DOS did not list COMMAND.COM off the FAT32 volume.\n{dir_text}"
    );
}

/// The Katea host-folder facade end-to-end: mount a real host directory as C:
/// through `mount_hdd_folder` (the lazy facade, not a flat image), boot the
/// real FreeDOS kernel, and confirm it reaches C:\> and lists a file that lives
/// only in the host folder. This proves that system files from the committed
/// image and host files folded to 8.3 can share a bootable mount.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn katea_host_folder_boots_and_lists_a_host_file() {
    // A unique scratch folder under the system temp dir, holding one host file
    // whose 8.3 name (GREETING.TXT — already a valid 8.3 name, no ~n fold) is
    // distinct from the system files, so spotting it in DIR proves the host
    // folder reached the volume.
    let scratch = TokaScratch::new("katea_folder");
    let dir = scratch.path();
    std::fs::write(dir.join("GREETING.TXT"), b"hi from the host folder\r\n")
        .expect("write host file");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 500_000_000, current_root_prompt);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
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
    run_until_toka_condition(&mut machine, 40_000_000, |machine| {
        current_root_prompt(machine)
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .contains("greeti")
    });
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
    run_until_toka_condition(&mut machine, 40_000_000, |machine| {
        current_root_prompt(machine)
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .contains("hi from the host folder")
    });
    let type_text = machine.screen_text().as_text().to_ascii_lowercase();

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
    let scratch = TokaScratch::new("katea_fullpost");
    let dir = scratch.path();
    std::fs::write(dir.join("GREETING.TXT"), b"hi\r\n").expect("write host file");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    // The GUI runs the full graphical POST, which lights the Margo LFB; headless
    // defaults to fast POST (text only), so this is the path the e2e tests miss.
    machine.set_fast_post(false);
    machine.mount_hdd_folder(dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 600_000_000, |machine| {
        current_root_prompt(machine) && !machine.margo_active()
    });
    let text = machine.screen_text().as_text().to_ascii_lowercase();
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

/// A host folder with a file *in a
/// subfolder* boots the real FreeDOS kernel, the subfolder is navigable, and a
/// file at depth reads lazily through the recursive tree facade. This is the
/// test proves the tree volume (`KateaTreeVolume`)
/// produces a self-consistent, bootable disk and that `CD` into a synthesized
/// subdirectory plus a `TYPE` of a file two levels down (`C:\GAMES\HELLO\`)
/// returns the host file's bytes, which the flat facade could never expose.
#[test]
#[ignore = "boots a full DOS image from a host-folder tree (slow in debug); run with --ignored"]
fn katea_host_folder_tree_reads_a_file_in_a_subfolder() {
    // A unique scratch tree under the system temp dir: GAMES\HELLO\READAT.TXT
    // lives two directory levels down, so reaching it proves the subfolder
    // chain (root -> GAMES -> HELLO) is navigable and the file reads at depth.
    let scratch = TokaScratch::new("katea_tree");
    let dir = scratch.path();
    let depth = dir.join("GAMES").join("HELLO");
    std::fs::create_dir_all(&depth).expect("scratch tree");
    std::fs::write(depth.join("READAT.TXT"), b"read at depth ok\r\n").expect("write depth file");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder(dir)
        .expect("mount host folder tree");
    let (stop, _) = run_until_toka_condition(&mut machine, 500_000_000, current_root_prompt);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
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
        run_until_toka_condition(&mut machine, 40_000_000, |machine| {
            current_prompt(machine, "C:\\GAMES\\HELLO>")
                && (cmd != "dir\r"
                    || machine
                        .screen_text()
                        .as_text()
                        .to_ascii_lowercase()
                        .contains("readat"))
        });
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
    run_until_toka_condition(&mut machine, 40_000_000, |machine| {
        current_prompt(machine, "C:\\GAMES\\HELLO>")
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .contains("read at depth ok")
    });
    let type_text = machine.screen_text().as_text().to_ascii_lowercase();

    assert!(
        dir_text.contains("readat"),
        "DIR in the subfolder did not list the file at depth.\n{dir_text}"
    );
    assert!(
        type_text.contains("read at depth ok"),
        "TYPE did not print the subfolder file's contents read lazily through the tree facade.\n{type_text}"
    );
}

/// Real FreeDOS, booted from a host folder, creates a
/// new file, overwrites an existing one, and grows one — all in a subfolder,
/// plus MKDIR a host subdir with a file at depth — and the changes appear
/// correctly in the host folder (verified by host-side read-back), with the
/// read tree + boot intact.
#[test]
#[ignore = "boots a full DOS image and writes through Katea (slow in debug); run with --ignored"]
fn katea_host_folder_writes_create_overwrite_grow_in_a_subfolder() {
    let scratch = TokaScratch::new("katea_m2");
    let dir = scratch.path();
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
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 500_000_000, current_root_prompt);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        panic!("CPU fault during Katea write boot: {msg}\nstop={stop:?}\n{text}");
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
        run_until_toka_condition(&mut machine, settle, |machine| {
            if !current_prompt(machine, "C:\\SAVES>") {
                return false;
            }
            if cmd != "make\r" {
                return true;
            }
            let reads = (
                std::fs::read(saves.join("NEW.TXT")),
                std::fs::read(saves.join("OLD.TXT")),
                std::fs::read(saves.join("GROW.TXT")),
                std::fs::read(saves.join("SUB").join("DEEP.TXT")),
            );
            let (Ok(new), Ok(old), Ok(grow), Ok(deep)) = reads else {
                return false;
            };
            let new = String::from_utf8_lossy(&new);
            let old = String::from_utf8_lossy(&old);
            let grow = String::from_utf8_lossy(&grow);
            let deep = String::from_utf8_lossy(&deep);
            new.contains("created")
                && old.contains("overwritten")
                && !old.contains("before")
                && grow.contains("line1")
                && grow.contains("line2")
                && std::fs::metadata(saves.join("SUB")).is_ok_and(|entry| entry.is_dir())
                && deep.contains("deep")
        });
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

/// Real FreeDOS deletes a file, renames a file in place, moves
/// a file into a subdir, RMDIRs an emptied dir, renames a subdir, and deletes a
/// PRE-EXISTING host file — all reflected in the host folder, read back.
#[test]
#[ignore = "boots a full DOS image and mutates host files via Katea (slow); run with --ignored"]
fn katea_host_folder_delete_rename_move_rmdir() {
    let scratch = TokaScratch::new("katea_m3");
    let dir = scratch.path();
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
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 500_000_000, current_root_prompt);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        panic!("CPU fault during Katea mutation boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        boot_text.contains("c:\\>"),
        "no C:\\> prompt (stop={stop:?}).\n{boot_text}"
    );

    // `ops` runs six commands including a COPY and an inline reconcile for each,
    // so it needs a larger settle budget than the create-and-overwrite test.
    for (cmd, settle) in [("cd ops\r", 40_000_000u64), ("ops\r", 150_000_000u64)] {
        for ch in cmd.chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type cmd");
        }
        run_until_toka_condition(&mut machine, settle, |machine| {
            if !current_prompt(machine, "C:\\OPS>") {
                return false;
            }
            if cmd != "ops\r" {
                return true;
            }
            matches!(ops.join("DELME.TXT").try_exists(), Ok(false))
                && matches!(ops.join("RENAMED.TXT").try_exists(), Ok(true))
                && matches!(ops.join("KEEP.TXT").try_exists(), Ok(false))
                && matches!(ops.join("SUB").join("MOVEME.TXT").try_exists(), Ok(true))
                && matches!(ops.join("MOVEME.TXT").try_exists(), Ok(false))
                && matches!(ops.join("EMPTYDIR").try_exists(), Ok(false))
                && matches!(ops.join("NEWDIR").try_exists(), Ok(true))
                && matches!(ops.join("OLDDIR").try_exists(), Ok(false))
                && matches!(ops.join("PREEXIST.TXT").try_exists(), Ok(false))
        });
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
    let scratch = TokaScratch::new("katea_run_e2e");
    let prog = scratch.path().join("EXIT42.COM");
    std::fs::write(&prog, izarravm_firmware::exit42_com()).unwrap();

    let code = katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Vega)).expect("katea_run");
    assert_eq!(
        code, 42,
        "the program's DOS exit code must reach the host process"
    );
}

/// The kernel's .EXE relocation loop reads the fixup table in 32-entry spans
/// (task.c, RELOC_SPAN). RELOCCHK.EXE carries 130 fixups — four full spans
/// plus a 2-entry remainder — each pointing at a slot the program verifies
/// against its own load segment, plus an unlisted canary word that must not
/// move. Exit 42 = every fixup applied exactly once; 1 = a slot is missing
/// or doubled; 2 = the canary was patched (a span applied one entry too
/// many).
#[test]
#[ignore = "boots a full FreeDOS image to run one program (slow); run with --ignored"]
fn katea_run_applies_exe_relocations_across_spans() {
    let scratch = TokaScratch::new("katea_run_relocchk");
    let prog = scratch.path().join("RELOCCHK.EXE");
    std::fs::write(&prog, izarravm_firmware::relocchk_exe()).unwrap();

    let code = katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Vega)).expect("katea_run");
    assert_eq!(
        code, 42,
        "RELOCCHK must see all 130 fixups applied exactly once (1 = missing/doubled, 2 = canary hit)"
    );
}

/// The instrument's red path: the same fixture with e_crlc lowered to 129
/// must exit 1, because the kernel then leaves the last slot unrelocated and
/// RELOCCHK sees it. This proves the loader applies exactly the declared
/// count, and it proves the 42-pass above cannot pass vacuously.
#[test]
#[ignore = "boots a full FreeDOS image to run one program (slow); run with --ignored"]
fn katea_run_exe_relocation_check_goes_red_on_a_short_count() {
    let scratch = TokaScratch::new("katea_run_relocred");
    let mut bytes = izarravm_firmware::relocchk_exe().to_vec();
    let count = u16::from_le_bytes([bytes[6], bytes[7]]);
    assert_eq!(count, 130, "fixture shape moved; update this mutant");
    bytes[6..8].copy_from_slice(&129u16.to_le_bytes());
    let prog = scratch.path().join("RELOCRED.EXE");
    std::fs::write(&prog, bytes).unwrap();

    let code = katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Vega)).expect("katea_run");
    assert_eq!(
        code, 1,
        "a lying fixup count must leave the last slot unmoved, and RELOCCHK must see it"
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
    let scratch = TokaScratch::new("katea_run_hlt");
    let prog = scratch.path().join("HLTTEST.COM");
    std::fs::write(&prog, izarravm_firmware::hlttest_com()).unwrap();

    let code = katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Vega)).expect("katea_run");
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
    let scratch = TokaScratch::new("katea_run_multihlt");
    let prog = scratch.path().join("MULTIHLT.COM");
    std::fs::write(&prog, izarravm_firmware::multihlt_com()).unwrap();

    let code = katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Vega)).expect("katea_run");
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
    let scratch = TokaScratch::new("tokaedit_e2e");
    let dir = scratch.path();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 500_000_000, current_root_prompt);
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        panic!("CPU fault during TokaEdit e2e boot: {msg}\nstop={stop:?}\n{text}");
    }
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    if !boot_text.contains("c:\\>") {
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
    run_until_toka_condition(&mut machine, 100_000_000, current_root_prompt);

    let after_exit = machine.screen_text().as_text().to_ascii_lowercase();
    if !after_exit.contains("c:\\>") {
        panic!("did not return to the C:\\> prompt after Save/Exit.\n{after_exit}");
    }

    machine.flush_hdd_folder();
    let saved = std::fs::read(dir.join("HELLO")).expect("HELLO written to host folder");
    assert_eq!(
        saved, b"hi\r\n",
        "saved HELLO bytes did not match the typed document body"
    );
}

/// DIR's default listing order: by name, directories grouped first, without
/// the user giving `/O`. Stock FreeCOM lists entries in raw on-disk order and
/// only sorts when asked; Toka-DOS installs the `/O:NG` order as the default
/// (see `toka-dos/freedos/VENDOR.md`). The C: root is the fixture because its
/// on-disk order (CONFIG.SYS, AUTOEXEC.BAT, LICENSE.TXT, then the DOS subdir
/// entry, written in that sequence by `build-freedos-hdd-image.py`) is neither
/// alphabetical nor directories-first, so an unsorted DIR cannot pass this.
/// `DIR /O:U` must still get that raw order back.
#[test]
#[ignore = "boots a full DOS image from a FAT32 HDD (slow in debug); run with --ignored"]
fn dir_defaults_to_name_order_with_directories_first() {
    let (mut machine, stop, boot_cycles) = boot_hdd(500_000_000);
    let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
    assert!(
        boot_text.contains("c:\\>"),
        "no C:\\> prompt after {boot_cycles} requested cycles (stop={stop:?}).\n{boot_text}"
    );

    // CLS first, so each capture holds one listing and nothing scrolled up
    // from the previous one.
    let listing = |machine: &mut Machine, command: &str| -> Vec<String> {
        for ch in "cls\r".chars().chain(command.chars()).chain(['\r']) {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type a DIR command");
        }
        run_until_toka_condition(machine, 40_000_000, |machine| {
            current_root_prompt(machine)
                && machine
                    .screen_text()
                    .as_text()
                    .to_ascii_lowercase()
                    .contains("bytes free")
        });
        let text = machine.screen_text().as_text();
        assert!(
            text.to_ascii_lowercase().contains("license"),
            "DIR did not list the C: root.\n{text}"
        );
        dir_entry_names(&text)
    };

    let sorted = listing(&mut machine, "dir c:\\");
    let unsorted = listing(&mut machine, "dir c:\\ /o:u");

    assert_eq!(
        sorted,
        vec!["DOS", "AUTOEXEC.BAT", "CONFIG.SYS", "LICENSE.TXT"],
        "DIR without /O did not default to name order with directories first.\n\
         DIR /O:U order was {unsorted:?}"
    );
    assert_ne!(
        unsorted, sorted,
        "DIR /O:U returned the sorted order, so the default sort proves nothing"
    );
    let mut same_entries = unsorted.clone();
    same_entries.sort();
    let mut expected_entries = sorted.clone();
    expected_entries.sort();
    assert_eq!(
        same_entries, expected_entries,
        "DIR /O:U listed a different set of entries, so the two captures are not comparable"
    );
}

/// The entry names of a DIR listing, in the order DIR printed them. A wide-form
/// row is `NAME     EXT      <DIR>  ...` or `NAME     EXT   1,234 ...`, with the
/// 8.3 name in the first 12 columns; every other screen line (header, summary,
/// prompt) is skipped.
fn dir_entry_names(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter_map(|line| {
            let (name_field, rest) = line.split_at_checked(12)?;
            if rest.trim().is_empty() {
                return None;
            }
            let (stem, ext) = name_field.split_at(8);
            let stem = stem.trim_end();
            let ext = ext.trim();
            if stem.is_empty()
                || stem.starts_with(' ')
                || !stem.bytes().all(|b| {
                    b.is_ascii_uppercase() || b.is_ascii_digit() || b"_~-!@#$%^&(){}".contains(&b)
                })
            {
                return None;
            }
            Some(if ext.is_empty() {
                stem.to_string()
            } else {
                format!("{stem}.{ext}")
            })
        })
        .collect()
}
