// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const DOOM: &str = "[dosbox]\nmachine=svga_s3\nmemsize=16\n[cpu]\ncycles=auto\n[gus]\ngus=true\n\
[autoexec]\ncls\nmount c .\\eXoDOS\\DOOM\nc:\n@cls\ncall run\nexit\n";

#[test]
fn reads_the_keyed_sections() {
    let conf = DosboxConf::parse(DOOM);
    assert_eq!(conf.machine(), "svga_s3");
    assert_eq!(conf.memsize_mib(), 16);
    assert_eq!(conf.cycles(), "auto");
    assert!(conf.wants_gus());
    assert!(!conf.wants_mt32());
}

#[test]
fn classifies_the_autoexec_verbs() {
    let conf = DosboxConf::parse(DOOM);
    let verbs: Vec<&str> = conf.autoexec.iter().map(AutoexecStep::verb).collect();
    assert_eq!(
        verbs,
        vec!["noise", "mount", "drive", "noise", "call", "exit"]
    );
    assert!(matches!(
        &conf.autoexec[1],
        AutoexecStep::Mount { drive: 'c', path } if path == ".\\eXoDOS\\DOOM"
    ));
    assert!(matches!(&conf.autoexec[4], AutoexecStep::Call(target) if target == "run"));
}

#[test]
fn strips_the_at_prefix_from_every_verb() {
    let conf = DosboxConf::parse("[autoexec]\n@mount c .\\eXoDOS\\X\n@cd SUB\n@call run\n");
    assert!(matches!(conf.autoexec[0], AutoexecStep::Mount { .. }));
    assert!(matches!(&conf.autoexec[1], AutoexecStep::Cd(dir) if dir == "SUB"));
    assert!(matches!(conf.autoexec[2], AutoexecStep::Call(_)));
}

#[test]
fn keeps_a_quoted_image_path_whole() {
    let conf = DosboxConf::parse(
        "[autoexec]\nimgmount d \".\\eXoDOS\\XCOMUF\\cd\\X COM UFO.iso\" -t cdrom\n",
    );
    match &conf.autoexec[0] {
        AutoexecStep::ImgMount { drive, image, kind } => {
            assert_eq!(*drive, 'd');
            assert_eq!(image, ".\\eXoDOS\\XCOMUF\\cd\\X COM UFO.iso");
            assert_eq!(kind, "cdrom");
        }
        other => panic!("expected an imgmount, got {other:?}"),
    }
}

#[test]
fn parses_a_non_boolean_dos_section_without_panicking() {
    // Two confs in the corpus carry `ems=emm386`, which a strict boolean
    // parser throws on. Nothing here parses a value as a boolean.
    let conf = DosboxConf::parse("[dos]\nems=emm386\nxms=false\n");
    assert_eq!(conf.get("dos", "ems"), Some("emm386"));
}

#[test]
fn reads_the_fixed_part_of_a_cycles_value() {
    assert_eq!(
        DosboxConf::parse("[cpu]\ncycles=auto\n").cycles_fixed(),
        None
    );
    assert_eq!(
        DosboxConf::parse("[cpu]\ncycles=max\n").cycles_fixed(),
        None
    );
    assert_eq!(
        DosboxConf::parse("[cpu]\ncycles=fixed 3000\n").cycles_fixed(),
        Some(3000)
    );
    assert_eq!(
        DosboxConf::parse("[cpu]\ncycles=20000\n").cycles_fixed(),
        Some(20000)
    );
}

#[test]
fn tokenizes_quoted_arguments() {
    assert_eq!(
        tokenize("imgmount d \"a b.iso\" -t cdrom"),
        vec!["imgmount", "d", "a b.iso", "-t", "cdrom"]
    );
}
