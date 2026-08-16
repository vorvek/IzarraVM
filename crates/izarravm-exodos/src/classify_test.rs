// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn verdict(autoexec: &str, head: &str) -> ConfVerdict {
    classify_conf(&DosboxConf::parse(&format!("{head}[autoexec]\n{autoexec}")))
}

const VGA: &str = "[dosbox]\nmachine=svga_s3\nmemsize=16\n";

#[test]
fn a_plain_call_recipe_is_translatable() {
    let out = verdict("mount c .\\eXoDOS\\DOOM\nc:\ncall run\nexit\n", VGA);
    assert_eq!(out.class, Class::Translatable);
    assert!(out.reasons.is_empty(), "{:?}", out.reasons);
    assert!(out.has_call);
}

#[test]
fn a_cd_title_keeps_its_image() {
    let out = verdict(
        "mount c .\\eXoDOS\\Duke3DAt\nimgmount d \".\\eXoDOS\\Duke3DAt\\cd\\ATOMIC15.cue\" -t cdrom\n\
         c:\n@call run\nexit\n",
        VGA,
    );
    assert_eq!(out.class, Class::Translatable);
    assert_eq!(
        out.cd_image.as_deref(),
        Some(".\\eXoDOS\\Duke3DAt\\cd\\ATOMIC15.cue")
    );
}

#[test]
fn a_non_vga_machine_is_hard_blocked() {
    let out = verdict(
        "mount c .\\eXoDOS\\X\nc:\ngame\nexit\n",
        "[dosbox]\nmachine=cga\n",
    );
    assert_eq!(out.class, Class::Untranslatable);
    assert!(out.reasons.contains(&"machine-non-vga".to_string()));
}

#[test]
fn the_vga_family_includes_the_exo_typo() {
    for machine in [
        "svga_s3",
        "svga_et4000",
        "vesa_nolfb",
        "vesa_noflb",
        "vgaonly",
        "",
    ] {
        assert!(is_vga_family(machine), "{machine}");
    }
    for machine in ["cga", "tandy", "pcjr", "ega", "hercules", "amstrad"] {
        assert!(!is_vga_family(machine), "{machine}");
    }
}

#[test]
fn a_pause_is_recoverable_not_fatal() {
    let out = verdict("mount c .\\eXoDOS\\X\nc:\npause\ngame\nexit\n", VGA);
    assert_eq!(out.class, Class::Recoverable);
    assert_eq!(out.reasons, vec!["pause-prompt".to_string()]);
}

#[test]
fn a_booter_and_a_floppy_image_are_both_hard() {
    let out = verdict("mount c .\\eXoDOS\\X\nc:\nboot a.img\n", VGA);
    assert!(out.reasons.contains(&"booter-disk".to_string()));
    let out = verdict(
        "mount c .\\eXoDOS\\X\nimgmount a .\\eXoDOS\\X\\disk.img -t floppy\nc:\ngame\n",
        VGA,
    );
    assert!(out.reasons.contains(&"floppy-image".to_string()));
}

#[test]
fn a_recipe_with_no_payload_has_nothing_to_launch() {
    let out = verdict("mount c .\\eXoDOS\\X\nc:\ncls\nexit\n", VGA);
    assert_eq!(out.class, Class::Untranslatable);
    assert!(out.reasons.contains(&"no-launch-command".to_string()));
}

#[test]
fn host_side_navigation_before_the_mount_is_not_a_guest_cd() {
    let out = verdict(
        "cd ..\ncd ..\nmount c .\\eXoDOS\\X\nc:\ncd SUB\ngame\nexit\n",
        VGA,
    );
    assert_eq!(out.class, Class::Translatable);
}

#[test]
fn a_low_cycles_title_is_flagged_speed_sensitive() {
    let out = verdict(
        "mount c .\\eXoDOS\\X\nc:\ngame\n",
        "[dosbox]\nmachine=svga_s3\n[cpu]\ncycles=fixed 500\n",
    );
    assert!(out.speed_sensitive);
    let out = verdict(
        "mount c .\\eXoDOS\\X\nc:\ngame\n",
        "[dosbox]\nmachine=svga_s3\n[cpu]\ncycles=max\n",
    );
    assert!(!out.speed_sensitive);
}
