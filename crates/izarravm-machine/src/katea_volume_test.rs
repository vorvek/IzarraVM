// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The extractor pulls the exact system payload back out of the committed,
/// proven-bootable image, including both boot sectors with the 0x55AA signature.
#[test]
fn extracts_the_embedded_image_payload() {
    let img = izarravm_firmware::tokados_hdd_img();
    let payload = extract_system_payload(img);

    assert_eq!(&payload.mbr[510..512], &[0x55, 0xAA], "MBR boot signature");
    assert_eq!(&payload.vbr[510..512], &[0x55, 0xAA], "VBR boot signature");

    // The files, in directory order, with their known sizes.
    let by_name: std::collections::HashMap<&str, &Vec<u8>> =
        payload.files.iter().map(|(n, d)| (n.as_str(), d)).collect();
    // Sizes shifted from prior builds after audit item 9 (presentation-leak
    // cleanup): KERNEL.SYS shrank because the boot-time Diskbuffers message
    // is now #ifdef DEBUG-gated (dropping its printf + format string in a
    // non-debug build); COMMAND.COM shrank because DEFAULT.lng's VER
    // /W,/D,/C copyright blocks were rewritten to drop FreeDOS branding.
    //
    // 70062 -> 70076: the fallback shell string grew from "command.com" plus
    // " /P /E:256" to the full "C:\DOS\COMMAND.COM" plus " C:\DOS /P /E:256",
    // which is 14 more bytes of .data. That default is what F5 (skip
    // CONFIG.SYS) lands on, and the bare name pointed at the boot root, where
    // Toka-DOS ships no COMMAND.COM. The idle-halt and F5-window patches in the
    // same kernel added no net size: their four instruction bytes fit padding.
    //
    // 87422 -> 87495: DIR defaults to the /O:NG sort order, which costs the
    // default-order install in cmd_dir(), the optOdefaulted flag that keeps a
    // failed sort-buffer allocation quiet when nobody asked for the sort, and
    // scanOrder() tracking its own argument start instead of reading the byte
    // in front of it. See toka-dos/freedos/VENDOR.md.
    //
    // 70076 -> 71084 (styled init screen): KERNEL.SYS grew net +1008 bytes.
    // signon() replaced the old single-line "\r%S ..." banner with the
    // rainbow boot logo (drawn straight into B800:0 text RAM) plus the
    // welcome box, added signon_box_edge()/signon_box_text() box-drawing
    // helpers, and initdisk.c's drive-assignment line now goes out through
    // TOKA_TREE_PREFIX (the CP437 elbow "\xC3\xC4> ") instead of a bare
    // printf. Those additions are only partly offset by what they replaced:
    // the old "\r%S" signon format string and dsk_init()'s unterminated
    // " - InitDisk" progress fragment (initdisk.c, ~line 1369) are both gone,
    // the latter because a stray unanchored fragment would dangle ahead of
    // the fixed-row tree lines whenever no DOS partition is found.
    //
    // 87495 -> 87447 (styled init screen): COMMAND.COM shrank -48 bytes.
    // FreeCOM's startup ver() banner is now suppressed at both call sites so
    // the styled boot tree owns the screen instead of racing FreeCOM's own
    // signon: the /P (resident shell) branch's unconditional `cmd_ver(NULL)`
    // in initialize() -- the path CONFIG.SYS's shipped
    // `SHELL=...COMMAND.COM ... /P=C:\AUTOEXEC.BAT` line actually takes --
    // and the non-/P `showinfo` block's `cmd_ver(NULL)` a few lines below it,
    // which a COMMAND.COM invoked without /P (e.g. a nested interactive
    // shell) still reaches. VER remains fully functional as an explicit
    // command in both cases; only the automatic startup call is gone.
    assert_eq!(
        by_name.get("KERNEL.SYS").map(|d| d.len()),
        Some(71084),
        "KERNEL.SYS size"
    );
    assert_eq!(
        by_name.get("COMMAND.COM").map(|d| d.len()),
        Some(87447),
        "COMMAND.COM size"
    );
    assert!(by_name.contains_key("CONFIG.SYS"), "CONFIG.SYS present");
    assert!(by_name.contains_key("AUTOEXEC.BAT"), "AUTOEXEC.BAT present");
    assert!(by_name.contains_key("HELLO.TXT"), "HELLO.TXT present");

    // The kernel signon points at "** See LICENSE.TXT for more.", so the full
    // FreeDOS / Toka-DOS licensing ships as a real file on the C: payload.
    let license = by_name.get("LICENSE.TXT").expect("LICENSE.TXT present");
    assert!(
        String::from_utf8_lossy(license).contains("GNU GENERAL PUBLIC LICENSE"),
        "LICENSE.TXT carries the full GPL text"
    );

    // TOKAMOUS.COM ships as a synthesized binary; the default AUTOEXEC loads it
    // HIGH into a TOKAEMM UMB.
    assert!(by_name.contains_key("TOKAMOUS.COM"), "TOKAMOUS.COM present");
    let autoexec = by_name.get("AUTOEXEC.BAT").expect("AUTOEXEC.BAT present");
    assert!(
        String::from_utf8_lossy(autoexec).contains("SET BLASTER=A220 I7 D1 H5 P300 T6"),
        "default AUTOEXEC advertises the Sound Blaster on its default IRQ 7"
    );
    assert!(
        String::from_utf8_lossy(autoexec).contains("LH TOKAMOUS"),
        "default AUTOEXEC loads the mouse driver high"
    );
    assert!(by_name.contains_key("TOKACD.SYS"), "TOKACD.SYS present");
    assert!(by_name.contains_key("IZCDEX.COM"), "IZCDEX.COM present");

    // SNDCTRL.COM ships from the committed firmware binary, not from whatever
    // the previous image happened to contain: a stale copy here would be a tool
    // that writes an older CMOS layout than the host reads.
    assert_eq!(
        by_name.get("SNDCTRL.COM").map(|d| d.as_slice()),
        Some(izarravm_firmware::sndctrl_com()),
        "SNDCTRL.COM on the payload must be byte-identical to the committed \
         binary (rebuild sndctrl.com, then regenerate roms/tokados-hdd.img \
         via scripts/build-freedos-hdd-image.py)"
    );
    let autoexec_text = String::from_utf8_lossy(autoexec);
    let izcdex_pos = autoexec_text
        .find("IZCDEX /I /D:TOKACD01 /L:D /T")
        .expect("default AUTOEXEC assigns the guest CD-ROM as D:");
    let mouse_pos = autoexec_text
        .find("LH TOKAMOUS")
        .expect("default AUTOEXEC loads the mouse driver");
    assert!(
        izcdex_pos < mouse_pos,
        "default AUTOEXEC installs IZCDEX before the mouse driver"
    );

    // TOKAEMM.SYS ships on the payload and the default CONFIG.SYS
    // loads it (EMS pages drawn on demand from the shared arena) with
    // DOS=HIGH,UMB, so every default boot runs FreeDOS in V86 under the
    // guest memory manager. BYTE compare, not a
    // length compare: the driver's resident envelope is padded to a fixed
    // size, so two different builds are routinely the same length — a stale
    // tokados-hdd.img sailed through the old length check while every real
    // boot silently ran the previous monitor (V86 trap tax review finding).
    assert_eq!(
        by_name.get("TOKAEMM.SYS").map(|d| d.as_slice()),
        Some(izarravm_firmware::tokaemm_sys()),
        "TOKAEMM.SYS on the payload must be byte-identical to the committed \
             driver (regenerate roms/tokados-hdd.img via \
             scripts/build-freedos-hdd-image.py after any tokaemm.asm rebuild)"
    );
    let config = by_name.get("CONFIG.SYS").expect("CONFIG.SYS present");
    let config_text = String::from_utf8_lossy(config);
    assert!(
        config_text.contains("DEVICE=C:\\DOS\\TOKAEMM.SYS RAM"),
        "default CONFIG.SYS loads TOKAEMM from C:\\DOS"
    );
    assert!(
        config_text.contains("DOS=HIGH,UMB"),
        "default CONFIG.SYS uses the HMA + UMBs"
    );
    let umb_pos = config_text
        .find("DOS=HIGH,UMB")
        .expect("default CONFIG.SYS enables upper memory");
    let tokacd_pos = config_text
        .find("DEVICEHIGH=C:\\DOS\\TOKACD.SYS")
        .expect("default CONFIG.SYS loads TOKACD high");
    assert!(
        umb_pos < tokacd_pos,
        "default CONFIG.SYS enables upper memory before TOKACD"
    );
    assert!(
        config_text.contains("LASTDRIVE=D"),
        "default CONFIG.SYS caps LASTDRIVE at D (A: floppy, C: HDD, D: CD)"
    );

    // FreeDOS KERNEL.SYS is a raw binary, not an MZ: it begins with a short
    // JMP (0xEB) past the embedded BPB — the load-bearing first byte the boot
    // sector relies on.
    let kernel = by_name.get("KERNEL.SYS").unwrap();
    assert_eq!(kernel[0], 0xEB, "KERNEL.SYS begins with a short JMP");

    // The rebranded, trimmed signon banner is compiled into the kernel. The
    // welcome box's copyright line (TOKA_BUILD_LINE_2 in version.h) supersedes
    // the older single-line "General Simulation Works" byline the pre-styled
    // kernel printed.
    let has = |needle: &str| kernel.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(
        has("Izarra SL"),
        "the rebranded signon company name is in the kernel"
    );
    assert!(
        !has("JTM Soluciones"),
        "the old company name was removed from the kernel"
    );
}
