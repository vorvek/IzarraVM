// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::tree::tests::tempdir::TempDir;

const DOOM_CONF: &str = "[dosbox]\nmachine=svga_s3\nmemsize=16\n[gus]\ngus=true\n\
[autoexec]\ncls\nmount c .\\eXoDOS\\DOOM\nc:\n@cls\ncall run\nexit\n";

const RUN_BAT: &str = ":menu\r\necho Press 1 for Doom w/ Gravis Ultrasound\r\n\
echo Press 2 for Doom w/ SoundBlaster\r\nchoice /C:12\r\n\
if errorlevel = 2 goto SB16\r\nif errorlevel = 1 goto GUS\r\n\
:GUS\r\ncopy .\\gus\\*.*\r\n@DOOM\r\ngoto quit\r\n\
:SB16\r\ncopy .\\sb16\\*.*\r\n@DOOM\r\ngoto quit\r\n:quit\r\nexit\r\n";

fn doom_extraction() -> TempDir {
    let dir = TempDir::new();
    let game = dir.path().join("DOOM");
    std::fs::create_dir_all(game.join("SB16")).unwrap();
    std::fs::create_dir_all(game.join("GUS")).unwrap();
    std::fs::write(game.join("RUN.BAT"), RUN_BAT).unwrap();
    std::fs::write(game.join("DOOM.EXE"), b"x").unwrap();
    std::fs::write(game.join("DOOM (1993).exo"), b"").unwrap();
    dir
}

fn options(dir: &TempDir, write: bool) -> TranslateOptions {
    TranslateOptions {
        extract_root: dir.path().to_path_buf(),
        short: "DOOM".to_string(),
        persona: "586".to_string(),
        clock_hz: 166_000_000,
        cycle_budget: 20_000_000_000,
        recipe: Recipe::generic(),
        write,
    }
}

#[test]
fn translates_a_call_recipe_end_to_end() {
    let dir = doom_extraction();
    let conf = DosboxConf::parse(DOOM_CONF);
    let result = translate(&conf, &options(&dir, true)).expect("translate");

    assert_eq!(result.class, Class::Translatable);
    assert_eq!(result.hdd_folder, dir.path().join("DOOM"));
    assert_eq!(result.config_sys_shape, ConfigShape::B);
    assert_eq!(result.launch_resolved.as_deref(), Some("DOOM.EXE"));
    assert!(result.flags.contains(&"WANTS-GUS".to_string()));
    assert!(result.flags.contains(&"MENU-FLATTENED".to_string()));
    assert_eq!(result.memory_mib, 16);

    let autoexec = result.autoexec.join("\n");
    assert!(autoexec.starts_with("@echo off\nPATH C:\\DOS\nSET BLASTER=A220 I7 D1 H5 P300 T6"));
    assert!(autoexec.contains("copy /Y .\\sb16\\*.*"));
    assert!(!autoexec.contains("copy .\\gus"));
    assert!(autoexec.ends_with("DOOM\nC:\\EXITVM.COM"));

    // The files are on disk and the marker that would have exercised the FAT
    // fold is gone.
    let game = dir.path().join("DOOM");
    assert!(game.join("AUTOEXEC.BAT").is_file());
    assert!(game.join("CONFIG.SYS").is_file());
    assert_eq!(
        std::fs::read(game.join("EXITVM.COM")).unwrap(),
        EXITVM_COM.to_vec()
    );
    assert!(!game.join("DOOM (1993).exo").exists());
}

#[test]
fn a_parent_mount_resolves_to_the_extraction_root() {
    let dir = doom_extraction();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[autoexec]\nmount c .\\eXoDOS\\\nc:\ncd DOOM\ncall run\nexit\n",
    );
    let result = translate(&conf, &options(&dir, false)).expect("translate");
    assert_eq!(result.hdd_folder, dir.path().to_path_buf());
    assert!(result.autoexec.contains(&"cd \\DOOM".to_string()));
    assert_eq!(result.launch_resolved.as_deref(), Some("DOOM/DOOM.EXE"));
}

#[test]
fn a_conf_cd_naming_a_missing_directory_is_a_flag_not_a_failure() {
    // The `Borderwo` shape: the conf says `cd Borderwo` but C: already is the
    // game directory. DOSBox prints a warning and runs the game anyway.
    let dir = doom_extraction();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[autoexec]\nmount c .\\eXoDOS\\DOOM\nc:\n@cd DOOM\n@DOOM\nexit\n",
    );
    let result = translate(&conf, &options(&dir, false)).expect("translate");
    assert!(result.flags.contains(&"CONF-CD-MISSING".to_string()));
    assert_eq!(result.launch_resolved.as_deref(), Some("DOOM.EXE"));
    assert_eq!(result.class, Class::Translatable);
}

#[test]
fn a_cd_title_gets_shape_a_and_the_image_argument() {
    let dir = TempDir::new();
    let game = dir.path().join("Duke3DAt");
    std::fs::create_dir_all(game.join("cd")).unwrap();
    std::fs::write(game.join("cd/ATOMIC15.cue"), b"FILE").unwrap();
    std::fs::write(game.join("DUKE3D.EXE"), b"x").unwrap();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\nmemsize=63\n[autoexec]\nmount c .\\eXoDOS\\Duke3DAt\n\
         imgmount d \".\\eXoDOS\\Duke3DAt\\cd\\ATOMIC15.cue\" -t cdrom\nc:\n@DUKE3D\nexit\n",
    );
    let mut opts = options(&dir, false);
    opts.short = "Duke3DAt".to_string();
    let result = translate(&conf, &opts).expect("translate");

    assert_eq!(result.config_sys_shape, ConfigShape::A);
    assert_eq!(result.cd_image, Some(game.join("cd").join("ATOMIC15.cue")));
    // The IzarraCD ROM extension needs no AUTOEXEC line; the kernel claims D:.
    assert!(!result.autoexec.iter().any(|l| l.contains("IZCDEX")));
    // 63 is eXo's workaround for a 64 MB machine.
    assert_eq!(result.memory_mib, 64);
    assert!(result.invocation.windows(2).any(|w| w[0] == "--cd-image"));
}

#[test]
fn a_title_with_its_own_memory_manager_gets_no_tokaemm() {
    let dir = doom_extraction();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[autoexec]\nmount c .\\eXoDOS\\DOOM\nc:\ncwsdpmi -p\ncall run\nexit\n",
    );
    let result = translate(&conf, &options(&dir, false)).expect("translate");
    assert_eq!(result.config_sys_shape, ConfigShape::C);
    assert!(result.flags.contains(&"OWN-MEMORY-MANAGER".to_string()));
    assert!(result.flags.contains(&"B6-BLIND".to_string()));
}

#[test]
fn the_confs_own_ems_key_names_the_titles_that_host_their_own_manager() {
    // eXo writes `[dos] ems=false` on 106 confs -- ultima71 among them -- which
    // is the conf SAYING what the jemm/cwsdpmi name-sniff was guessing at.
    let dir = doom_extraction();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[dos]\nems=false\n\
         [autoexec]\nmount c .\\eXoDOS\\DOOM\nc:\ncall run\nexit\n",
    );
    let result = translate(&conf, &options(&dir, false)).expect("translate");
    assert_eq!(result.config_sys_shape, ConfigShape::C);
    assert!(result.flags.contains(&"CONF-EMS-FALSE".to_string()));
    assert!(result.flags.contains(&"OWN-MEMORY-MANAGER".to_string()));
    assert!(!result.autoexec.iter().any(|l| l.contains("TOKAEMM")));
    // A CD title that hosts its own manager still mounts its disc (shape D);
    // the drive itself comes from the kernel's IzarraCD claim, not a driver
    // line, so no shape emits one.
    let game = dir.path().join("DOOM");
    std::fs::create_dir_all(game.join("cd")).unwrap();
    std::fs::write(game.join("cd/DISC.cue"), b"FILE").unwrap();
    let with_cd = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[dos]\nems=false\n\
         [autoexec]\nmount c .\\eXoDOS\\DOOM\n\
         imgmount d \".\\eXoDOS\\DOOM\\cd\\DISC.cue\" -t cdrom\nc:\ncall run\nexit\n",
    );
    let result = translate(&with_cd, &options(&dir, false)).expect("translate");
    assert_eq!(result.config_sys_shape, ConfigShape::D);
    assert!(!result.autoexec.iter().any(|l| l.contains("IZCDEX")));
    assert!(result.cd_image.is_some());
}

#[test]
fn a_disc_named_only_inside_the_launcher_bat_still_mounts() {
    // MechWarrior 2's conf mounts no disc at all; run.bat mounts MECH2.CUE
    // inside the CHOICE branch, and reading only [autoexec] left 749 MB behind.
    let dir = TempDir::new();
    let game = dir.path().join("MechW2");
    std::fs::create_dir_all(game.join("cd")).unwrap();
    std::fs::write(game.join("cd/MECH2.CUE"), b"FILE").unwrap();
    std::fs::write(game.join("MECH2.EXE"), b"x").unwrap();
    std::fs::write(
        game.join("RUN.BAT"),
        "imgmount d .\\eXoDOS\\MechW2\\cd\\MECH2.CUE -t cdrom \r\ncls\r\n@MECH2\r\n",
    )
    .unwrap();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[autoexec]\nmount c .\\eXoDOS\\MechW2\nc:\n@call run\nexit\n",
    );
    let mut opts = options(&dir, false);
    opts.short = "MechW2".to_string();
    let result = translate(&conf, &opts).expect("translate");
    assert_eq!(result.cd_image, Some(game.join("cd").join("MECH2.CUE")));
    assert_eq!(result.config_sys_shape, ConfigShape::A);
    assert!(result.flags.contains(&"CD-FROM-BAT".to_string()));
    assert!(result.invocation.windows(2).any(|w| w[0] == "--cd-image"));
}

#[test]
fn every_generated_autoexec_loads_the_mouse_driver() {
    // Blood runs the game through `bmouse`, which aborts when its INT 33h probe
    // finds nothing. A game that ignores INT 33h pays only TOKAMOUS's residency.
    let dir = doom_extraction();
    let conf = DosboxConf::parse(DOOM_CONF);
    let result = translate(&conf, &options(&dir, false)).expect("translate");
    assert!(result.autoexec.contains(&"LH TOKAMOUS".to_string()));

    // With no memory manager there is no upper memory to load high into.
    let own = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[dos]\nems=false\n\
         [autoexec]\nmount c .\\eXoDOS\\DOOM\nc:\ncall run\nexit\n",
    );
    let result = translate(&own, &options(&dir, false)).expect("translate");
    assert!(result.autoexec.contains(&"TOKAMOUS".to_string()));
}

#[test]
fn a_recipe_mouse_step_reaches_the_invocation_and_a_bad_one_refuses_the_row() {
    let dir = doom_extraction();
    let conf = DosboxConf::parse(DOOM_CONF);
    let mut opts = options(&dir, false);
    opts.recipe = Recipe {
        notes: "GPrix2 startup menu is mouse-only".to_string(),
        keys: Vec::new(),
        mouse: vec![
            crate::recipe::MouseStep {
                guest_ms: 20_000,
                action: "home".to_string(),
            },
            crate::recipe::MouseStep {
                guest_ms: 20_500,
                action: "move:320,544".to_string(),
            },
            crate::recipe::MouseStep {
                guest_ms: 21_000,
                action: "click".to_string(),
            },
        ],
    };
    let result = translate(&conf, &opts).expect("translate");
    assert_eq!(result.class, Class::Translatable);
    let spec = result.inject_mouse.expect("a mouse schedule");
    // 20,000 guest ms at 166 MHz, then the two later steps, in order.
    assert!(spec.starts_with("3320000000:home;"), "{spec}");
    assert!(spec.ends_with(":click"), "{spec}");
    assert!(result.inject_keys.is_none());
    let at = result
        .invocation
        .iter()
        .position(|a| a == "--inject-mouse")
        .expect("the flag");
    assert_eq!(result.invocation[at + 1], spec);

    // A typo fails the translation, not the run: --inject-mouse is parsed before
    // the machine is built, so the cost of finding it late is a whole boot.
    opts.recipe.mouse[1].action = "move 320 544".to_string();
    let result = translate(&conf, &opts).expect("translate");
    assert_eq!(result.class, Class::Untranslatable);
    assert!(result.reasons.contains(&"recipe-mouse-invalid".to_string()));
}

#[test]
fn the_config_sys_shapes_match_the_fixtures() {
    assert_eq!(
        render_config_sys(ConfigShape::B),
        "FILES=40\r\nLASTDRIVE=D\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\nDOS=HIGH,UMB\r\n\
         SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
    );
    assert_eq!(
        render_config_sys(ConfigShape::A),
        "FILES=40\r\nLASTDRIVE=D\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM /T\r\nDOS=HIGH,UMB\r\n\
         SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
    );
}

#[test]
fn a_root_level_dos_folder_is_refused() {
    let dir = doom_extraction();
    std::fs::create_dir_all(dir.path().join("DOOM/DOS")).unwrap();
    let conf = DosboxConf::parse(DOOM_CONF);
    let result = translate(&conf, &options(&dir, false)).expect("translate");
    assert_eq!(result.class, Class::Untranslatable);
    assert!(result.reasons.contains(&"reserved-root-name".to_string()));
}

#[test]
fn a_cd_the_translator_cannot_mount_is_refused_not_translated() {
    // The conf imgmounts a CUE that is not in the extraction, so no
    // `--cd-image` resolves. The `D:` prelude would then run on a drive that
    // does not exist, and there is no launch command either.
    let dir = doom_extraction();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=svga_s3\n[autoexec]\nmount c .\\eXoDOS\\DOOM\n\
         imgmount d .\\eXoDOS\\DOOM\\cd\\MISSING.cue -t cdrom\nd:\n@GAME\nexit\n",
    );
    let result = translate(&conf, &options(&dir, true)).expect("translate");
    assert!(result.cd_image.is_none());
    assert_eq!(result.class, Class::Untranslatable);
    assert!(
        result.reasons.contains(&"cd-mount-unsupported".to_string()),
        "{:?}",
        result.reasons
    );
    assert!(!dir.path().join("DOOM/AUTOEXEC.BAT").exists());
}

#[test]
fn an_untranslatable_title_writes_nothing() {
    // tandy, not cga: IzarraVM has a CGA path and cga TRANSLATES since
    // 2026-08-29. tandy has no video path at all, so it is a stable way to
    // build an untranslatable title without depending on the card list.
    let dir = doom_extraction();
    let conf = DosboxConf::parse(
        "[dosbox]\nmachine=tandy\n[autoexec]\nmount c .\\eXoDOS\\DOOM\nc:\ncall run\nexit\n",
    );
    let result = translate(&conf, &options(&dir, true)).expect("translate");
    assert_eq!(result.class, Class::Untranslatable);
    assert!(!dir.path().join("DOOM/AUTOEXEC.BAT").exists());
}
