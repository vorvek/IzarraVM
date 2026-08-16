// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::tree::tests::tempdir::TempDir;

/// The eXoDOS launcher template, trimmed to the shape every `run.bat` shares.
const RUN_BAT: &str = r#"@echo off
:start
if not exist *.sel goto menu
cls
echo Would you like to change this?
choice
if errorlevel = 2 goto N
if errorlevel = 1 goto menu

:N
cls
@DOOM
goto quit

:menu
cls
echo Press 1 for Doom w/ Gravis Ultrasound
echo Press 2 for Doom w/ SoundBlaster
echo Press 3 for Doom w/ Sound Canvas
echo Press 4 to play Network Multiplayer
echo Press 5 to launch Setup
echo Press 6 to Quit
choice /C:123456 /N Please Choose:

if errorlevel = 6 goto quit
if errorlevel = 5 goto setup
if errorlevel = 4 goto network
if errorlevel = 3 goto SC55
if errorlevel = 2 goto SB16
if errorlevel = 1 goto GUS

:setup
@setup
goto start

:GUS
del *.sel
CONFIG -set ""mididevice=default""
copy .\gus\*.*
cls
@DOOM
goto quit

:SB16
del *.sel
CONFIG -set ""mididevice=default""
del DEFAULT.CFG
copy .\sb16\*.*
type .>SB16.SEL
cls
@DOOM
goto quit

:SC55
copy .\sc55\*.*
@DOOM
goto quit

:quit
exit
"#;

fn doom_tree() -> (TempDir, Tree) {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("SB16")).unwrap();
    std::fs::create_dir_all(dir.path().join("GUS")).unwrap();
    std::fs::write(dir.path().join("RUN.BAT"), RUN_BAT).unwrap();
    std::fs::write(dir.path().join("DOOM.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("SETUP.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("DEFAULT.CFG"), b"x").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    (dir, tree)
}

#[test]
fn takes_the_sound_blaster_branch_and_ends_at_the_game() {
    let (_guard, tree) = doom_tree();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);

    assert!(out.failure.is_none(), "{:?}", out.failure);
    let launch = out.launch.expect("a launch command");
    assert_eq!(launch.command, "DOOM");
    assert_eq!(launch.resolved, "DOOM.EXE");
    assert!(!launch.by_search);
    assert!(out.flags.contains("MENU-FLATTENED"));
    assert!(out.flags.contains("CONFIG-SET-DROPPED"));
    assert_eq!(out.choices.len(), 1);
    assert!(out.choices[0].contains(":sb16"), "picked {:?}", out.choices);
}

#[test]
fn the_flattened_output_carries_the_branch_body_and_no_control_flow() {
    let (_guard, tree) = doom_tree();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);

    assert!(out.lines.iter().any(|l| l.starts_with("copy .\\sb16")));
    assert!(out.lines.iter().any(|l| l == "del DEFAULT.CFG"));
    // `del *.sel` does not prompt, so it is kept as written.
    assert!(out.lines.iter().any(|l| l == "del *.sel"));
    for line in &out.lines {
        let lower = line.to_ascii_lowercase();
        assert!(!lower.starts_with("goto "), "{line}");
        assert!(!lower.starts_with("choice"), "{line}");
        assert!(!lower.starts_with(':'), "{line}");
        assert!(!lower.starts_with("if "), "{line}");
    }
}

#[test]
fn only_the_prompting_del_wildcard_is_dropped() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "del *.*\r\ndel *.cfg\r\n@GAME\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert!(out.flags.contains("DEL-WILDCARD-DROPPED"));
    assert_eq!(out.lines, vec!["del *.cfg".to_string()]);
}

#[test]
fn a_backward_goto_refuses_rather_than_looping() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        ":loop\r\n@GAME\r\ngoto loop\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    // The launch is found before the loop closes, which is the useful answer:
    // one run of the game, no relaunch.
    assert!(out.launch.is_some());
}

#[test]
fn a_leading_backward_goto_is_untranslatable() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("RUN.BAT"), ":loop\r\ngoto loop\r\n").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.failure.as_deref(), Some("bat-backward-goto"));
}

#[test]
fn tracks_cd_so_the_launch_resolves_in_the_right_directory() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("DUKE3D")).unwrap();
    std::fs::write(dir.path().join("DUKE3D/DUKE3D.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "cd DUKE3d\r\ncls\r\n@DUKE3D\r\ngoto quit\r\n:quit\r\nexit\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    let launch = out.launch.expect("a launch command");
    assert_eq!(launch.dir, "DUKE3D");
    assert_eq!(launch.resolved, "DUKE3D/DUKE3D.EXE");
    assert!(out.lines.contains(&"cd \\DUKE3D".to_string()));
}

#[test]
fn an_ascending_errorlevel_ladder_picks_the_branch_the_guest_would_reach() {
    // DOS `if errorlevel N` is `>= N`, so an ascending ladder sends every key
    // to the first line. Choosing by equality would name a branch nothing
    // reaches.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("FIRST.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("SB.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "echo Press 1 for Gravis\r\necho Press 2 for SoundBlaster\r\nchoice /C:12\r\n\
         if errorlevel 1 goto one\r\nif errorlevel 2 goto two\r\n:one\r\n@FIRST\r\n\
         goto quit\r\n:two\r\n@SB\r\n:quit\r\nexit\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "FIRST.EXE");
}

#[test]
fn scores_a_sound_blaster_branch_above_the_cards_we_do_not_have() {
    assert!(
        branch_score("sb16 2 for Doom w/ SoundBlaster")
            > branch_score("gus 1 for Doom w/ Gravis Ultrasound")
    );
    assert!(
        branch_score("sc55 3 for Doom w/ Sound Canvas") < branch_score("sb16 2 w/ SoundBlaster")
    );
    assert_eq!(branch_score("quit 6 to Quit"), -1000);
    assert_eq!(branch_score("network 4 to play Network Multiplayer"), -1000);
    assert_eq!(branch_score("setup 5 to launch Setup"), -1000);
}

#[test]
fn a_second_level_call_is_inlined_and_a_third_is_refused() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("RUN.BAT"), "call INNER\r\n").unwrap();
    std::fs::write(dir.path().join("INNER.BAT"), "@GAME\r\n").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");

    std::fs::write(dir.path().join("INNER.BAT"), "call DEEPER\r\n").unwrap();
    std::fs::write(dir.path().join("DEEPER.BAT"), "@GAME\r\n").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.failure.as_deref(), Some("bat-call-too-deep"));
}
