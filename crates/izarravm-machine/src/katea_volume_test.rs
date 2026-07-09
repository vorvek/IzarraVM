// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The extractor pulls the exact system payload back out of the committed,
/// proven-bootable image: the five files at their known sizes/first bytes, and
/// both boot sectors carrying the 0x55AA signature.
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
    assert_eq!(
        by_name.get("KERNEL.SYS").map(|d| d.len()),
        Some(70062),
        "KERNEL.SYS size"
    );
    assert_eq!(
        by_name.get("COMMAND.COM").map(|d| d.len()),
        Some(87422),
        "COMMAND.COM size"
    );
    assert!(by_name.contains_key("CONFIG.SYS"), "CONFIG.SYS present");
    assert!(by_name.contains_key("AUTOEXEC.BAT"), "AUTOEXEC.BAT present");
    assert!(by_name.contains_key("HELLO.TXT"), "HELLO.TXT present");

    // The kernel signon points at "See C:\\LICENSE.TXT for more.", so the full
    // FreeDOS / Toka-DOS licensing ships as a real file on the C: payload.
    let license = by_name.get("LICENSE.TXT").expect("LICENSE.TXT present");
    assert!(
        String::from_utf8_lossy(license).contains("GNU GENERAL PUBLIC LICENSE"),
        "LICENSE.TXT carries the full GPL text"
    );

    // TOKAMOUS.COM ships as a synthesized binary; the default AUTOEXEC loads it
    // HIGH into a TOKAEMM UMB (SP-4b M4).
    assert!(by_name.contains_key("TOKAMOUS.COM"), "TOKAMOUS.COM present");
    let autoexec = by_name.get("AUTOEXEC.BAT").expect("AUTOEXEC.BAT present");
    assert!(
        String::from_utf8_lossy(autoexec).contains("SET BLASTER=A220 I5 D1 H5 T6"),
        "default AUTOEXEC advertises the Sound Blaster"
    );
    assert!(
        String::from_utf8_lossy(autoexec).contains("LH TOKAMOUS"),
        "default AUTOEXEC loads the mouse driver high"
    );

    // SP-4b M4: TOKAEMM.SYS ships on the payload and the default CONFIG.SYS
    // loads it (frameless NOEMS) with DOS=HIGH,UMB — every default boot runs
    // FreeDOS in V86 under the guest memory manager. BYTE compare, not a
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
        config_text.contains("DEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS"),
        "default CONFIG.SYS loads TOKAEMM from C:\\DOS"
    );
    assert!(
        config_text.contains("DOS=HIGH,UMB"),
        "default CONFIG.SYS uses the HMA + UMBs"
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

    // The rebranded, trimmed signon banner is compiled into the kernel.
    let has = |needle: &str| kernel.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(
        has("General Simulation Works"),
        "the rebranded signon company name is in the kernel"
    );
    assert!(
        !has("JTM Soluciones"),
        "the old company name was removed from the kernel"
    );
}
