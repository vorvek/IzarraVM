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

    // The overwrite switch is forced on: without it the guest stops on
    // "Overwrite (Yes/No/All)?" and eats the keys meant for the game.
    assert!(out.lines.iter().any(|l| l.starts_with("copy /Y .\\sb16")));
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
fn a_choice_branch_pointing_backwards_is_dropped_and_the_rest_re_scored() {
    // `:sb` sits ABOVE the choice, which is a menu loop: taking it would walk
    // the ladder again until the step limit and refuse a title that runs. The
    // remaining forward branch wins even though it scores lower.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("SB.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("SPK.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        ":sb\r\necho Press 1 for SoundBlaster\r\necho Press 2 for PC Speaker\r\n\
         choice /C:12\r\nif errorlevel 2 goto spk\r\nif errorlevel 1 goto sb\r\n\
         :spk\r\n@SPK\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert!(out.failure.is_none(), "{:?}", out.failure);
    assert_eq!(out.launch.expect("a launch").resolved, "SPK.EXE");
    assert!(out.flags.contains("MENU-FLATTENED"));
}

#[test]
fn a_menu_with_no_safe_branch_is_key_injected_rather_than_installed() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("INSTALL.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("SETUP.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "echo Press 1 to Install\r\necho Press 2 to run Setup\r\necho Press 3 to Quit\r\n\
         choice /C:123\r\nif errorlevel 3 goto quit\r\nif errorlevel 2 goto setup\r\n\
         if errorlevel 1 goto inst\r\n@GAME\r\n:inst\r\n@INSTALL\r\n:setup\r\n@SETUP\r\n\
         :quit\r\nexit\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert!(out.flags.contains("MENU-KEY-INJECTED"), "{:?}", out.flags);
    assert!(out.choices.is_empty(), "{:?}", out.choices);
    // Falling through past the ladder is what a real un-answered CHOICE does.
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");
}

#[test]
fn order_is_scored_as_a_word_so_border_is_not_refused() {
    assert_eq!(branch_score("border 1 for Borderwo w/ SoundBlaster"), 100);
    assert_eq!(branch_score("recorder 1 w/ SoundBlaster"), 100);
    assert_eq!(
        branch_score("order 4 to Order the full version"),
        REFUSED_BRANCH_SCORE
    );
    assert_eq!(
        branch_score("info 4 for an order form"),
        REFUSED_BRANCH_SCORE
    );
    assert_eq!(branch_score("help 5 for Help"), REFUSED_BRANCH_SCORE);
    // `helper` is not `help`.
    assert_eq!(branch_score("helper 1 w/ Adlib"), 40);
}

#[test]
fn a_chained_if_is_evaluated_rather_than_dropped() {
    // `if exist X if not exist Y goto Z`: the INNER test owns the jump. Handing
    // the whole tail to the command emitter would resolve nothing and carry on
    // down a branch the guest never takes.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("HERE.DAT"), b"x").unwrap();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("OTHER.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "if exist HERE.DAT if not exist GONE.DAT goto ok\r\n@OTHER\r\n:ok\r\n@GAME\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");
    assert!(!out.flags.contains("COMMAND-UNRESOLVED"), "{:?}", out.flags);

    // And the inner test failing means the jump is NOT taken.
    std::fs::write(dir.path().join("GONE.DAT"), b"x").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "OTHER.EXE");
}

#[test]
fn a_choice_named_with_its_extension_is_still_a_menu() {
    // The game ships its own CHOICE.COM. Without stripping the extension the
    // walker resolves it as a program and records the MENU as the launch.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("CHOICE.COM"), b"x").unwrap();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "choice.com /c:12 /n Pick:\r\n@GAME\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");
    assert!(out.flags.contains("MENU-KEY-INJECTED"));
}

#[test]
fn a_nested_call_chain_is_inlined_until_the_depth_bound() {
    // SWXWCD needs three nested levels (run -> XWINGCD -> XWINGCD2), so the
    // bound sits past that, and it still has to be a bound.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("RUN.BAT"), "call INNER\r\n").unwrap();
    std::fs::write(dir.path().join("INNER.BAT"), "call DEEPER\r\n").unwrap();
    std::fs::write(dir.path().join("DEEPER.BAT"), "call DEEPEST\r\n").unwrap();
    std::fs::write(dir.path().join("DEEPEST.BAT"), "@GAME\r\n").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");

    std::fs::write(dir.path().join("DEEPEST.BAT"), "call TOOFAR\r\n").unwrap();
    std::fs::write(dir.path().join("TOOFAR.BAT"), "@GAME\r\n").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.failure.as_deref(), Some("bat-call-too-deep"));
}

#[test]
fn a_set_variable_decides_the_guard_on_the_launch_line() {
    // Ppersia: `set cheats=no` at the top, then the branch that runs the game is
    // guarded on it. With SET dropped the guard was undecidable and the only
    // line that launches anything went on the floor.
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("PRINCE11")).unwrap();
    std::fs::write(dir.path().join("PRINCE11").join("PRINCE1.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "set cheats=no\r\ncd prince11\r\n\
         if %cheats%==no prince1 gblast\r\n\
         if %cheats%==yes prince1 gblast MEGAHIT\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    let launch = out.launch.expect("a launch");
    assert_eq!(launch.command, "prince1 gblast");
    assert_eq!(launch.resolved, "PRINCE11/PRINCE1.EXE");
}

#[test]
fn an_unset_variable_reads_as_empty_the_way_dos_reads_it() {
    // XCOMUF's ArchFile.Bat tests %PROCESSOR_ARCHITECTURE% and %OS%, neither of
    // which exists in DOS, and reaches its 16-bit branch because both are empty.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("OTHER.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "if %OS%. == Windows_NT. goto nt\r\n@GAME\r\ngoto done\r\n:nt\r\n@OTHER\r\n:done\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");
    assert!(out.flags.contains("VAR-UNSET-EXPANDED"), "{:?}", out.flags);
}

#[test]
fn an_echo_with_a_redirect_is_a_file_the_game_reads_and_is_kept() {
    // kq1vga rebuilds RESOURCE.CFG out of guarded echo lines and starts in the
    // wrong video mode without them. A bare echo is still screen noise.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("SIERRA.COM"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "set VIDEO=EGA\r\necho Choose:\r\n\
         if %VIDEO%==CGA echo videoDrv=CGA320M.DRV>>resource.cfg\r\n\
         if %VIDEO%==EGA echo videoDrv=EGA320.DRV>>resource.cfg\r\n\
         echo kbdDrv=IBMKBD.DRV>>resource.cfg\r\n@sierra\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert!(
        out.lines
            .iter()
            .any(|l| l == "echo videoDrv=EGA320.DRV>>resource.cfg"),
        "{:?}",
        out.lines
    );
    assert!(!out.lines.iter().any(|l| l.contains("CGA320M")));
    assert!(!out.lines.iter().any(|l| l == "echo Choose:"));
    assert_eq!(out.launch.expect("a launch").resolved, "SIERRA.COM");
}

#[test]
fn a_call_to_a_program_is_a_launch_like_any_other_invocation() {
    // ultimau1 ends its chosen branch on `call UW`, which is UW.EXE.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("UW.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "cls\r\ncall UW\r\ngoto quit\r\n:quit\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    let launch = out.launch.expect("a launch");
    assert_eq!(launch.resolved, "UW.EXE");
    assert_eq!(launch.command, "UW");
    assert!(out.failure.is_none(), "{:?}", out.failure);
}

#[test]
fn a_called_bat_returns_to_its_caller_and_hands_back_its_variables() {
    // XCOMUF calls a helper BAT that only sets a variable; treating the call as
    // a transfer of control stopped the launcher dead at the helper.
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("SUB")).unwrap();
    std::fs::write(dir.path().join("SUB").join("ARCH.BAT"), "set ARCH=16\r\n").unwrap();
    std::fs::write(dir.path().join("GAME16.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "call SUB\\ARCH.BAT\r\nif %ARCH%==16 goto sixteen\r\ngoto done\r\n\
         :sixteen\r\n@GAME16\r\n:done\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME16.EXE");
}

#[test]
fn an_imgmount_inside_the_bat_leaves_as_a_disc_not_as_a_line() {
    // MechW2 mounts its 749 MB disc inside the CHOICE branch, and conf.rs only
    // ever saw the [autoexec] ones.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "imgmount d .\\eXoDOS\\MechW2\\cd\\MECH2.CUE -t cdrom \r\nimgmount -u d\r\n@GAME\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.imgmounts.len(), 1);
    assert_eq!(out.imgmounts[0].image, ".\\eXoDOS\\MechW2\\cd\\MECH2.CUE");
    assert_eq!(out.imgmounts[0].kind, "cdrom");
    assert_eq!(out.imgmounts[0].drive, 'd');
    // The unmount form names no image and must not be read as one.
    assert!(out.flags.contains("IMGMOUNT-UNPARSED"), "{:?}", out.flags);
    assert!(!out.lines.iter().any(|l| l.contains("imgmount")));
}

#[test]
fn an_errorlevel_branch_after_a_program_refuses_rather_than_guesses() {
    // ecstatic runs `testmem` and branches on ERRORLEVEL 18. The exit code is
    // unknowable here, and taking testmem as the launch measured a memory-sizing
    // utility and then fell off the end of the AUTOEXEC while reporting a run.
    let dir = TempDir::new();
    for name in ["TESTMEM.EXE", "ECST4MEG.EXE", "ECST8MEG.EXE"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "@echo off\r\ntestmem\r\nif ERRORLEVEL 18 goto meg8\r\necst4meg\r\ngoto end\r\n\
         \r\n:meg8\r\necst8meg\r\n\r\n:end\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert!(out.launch.is_none(), "{:?}", out.launch);
    assert_eq!(
        out.failure.as_deref(),
        Some("errorlevel-branch-after-program")
    );
    assert!(out.flags.contains("ERRORLEVEL-BRANCH"), "{:?}", out.flags);
}

#[test]
fn an_errorlevel_ladder_whose_branches_agree_keeps_the_launch() {
    // The other half of the same rule: when every branch runs the same program
    // the exit code does not matter and there is nothing to guess at.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("TESTMEM.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("GAME.EXE"), b"x").unwrap();
    std::fs::write(
        dir.path().join("RUN.BAT"),
        "testmem\r\nif ERRORLEVEL 18 goto big\r\n@GAME\r\ngoto end\r\n\
         :big\r\n@GAME\r\n:end\r\n",
    )
    .unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    let mut out = Flattened::default();
    Flattener::new(&tree).flatten_bat("", "RUN.BAT", 1, &mut out);
    assert_eq!(out.launch.expect("a launch").resolved, "GAME.EXE");
    assert!(out.lines.iter().any(|l| l == "testmem"), "{:?}", out.lines);
    assert!(out.flags.contains("ERRORLEVEL-BRANCH-CONVERGED"));
}

#[test]
fn a_game_blaster_branch_is_not_read_as_a_sound_blaster_one() {
    // The word "blaster" is in both, and a substring test made the CMS branch
    // win the tie in Ppersia and kq1vga.
    assert!(
        branch_score("gb Press 1 for Prince of Persia w/ Game Blaster")
            < branch_score("sb16 Press 2 for Prince of Persia w/ Sound Blaster")
    );
    // The machine is a VGA, so a menu offering video modes is taken at the best.
    assert!(
        branch_score("ega Press 2 for the game EGA")
            > branch_score("cga Press 1 for the game Monochrome CGA")
    );
    // And a video word only counts as a whole word: "Legacy" contains "ega",
    // and a substring test made MechWarrior 2's menu pick the expansion.
    assert_eq!(
        branch_score("gbl Press 2 to play MechWarrior 2: Ghost Bear's Legacy"),
        branch_score("mw2 Press 1 to play MechWarrior 2: 31st Century Combat")
    );
}
