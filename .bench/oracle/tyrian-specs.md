<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# tyrian-specs: Tyrian 2000 Ship Specs, a DOSBox-X oracle schedule

Verified 2026-09-02. A key/timing schedule only -- no timing or rt number
recorded here or anywhere else. This screen is the same one measured (and
withdrawn) in `dev_docs/2026-09-02-tyrian-dosbox-comparison.md`; that
document's schedule discovery (section 1.3, "Screen identity is now proved
by picture") is not in question, only its rt numbers were withdrawn. This
file re-derives and re-verifies the schedule independently against a fresh
mount and a fresh screenshot, because the earlier schedule (`-w 14 -p 2.0`)
did NOT reproduce on this host/build pairing -- see "What failed" below.

## What the schedule reaches

Main menu -> Load Game -> One Player Saved Games (slot 1, "LAST LEVEL
GAUNTLET Episode3") -> Game Menu -> Ship Specs. Ship Specs is the ship-data
screen (ship art, name banner e.g. "MicroCorp Stalker-B", descriptive text,
"Press a key" prompt at the bottom) -- not the main menu, not the save-slot
list, not black.

## The tree under test

Mount dir must be a *fresh copy* of `D:\ctd\tyr586\oracle\t2k` (the owner's
Tyrian 2000 install, `TYRIAN.SAV` and `TYRIAN.CFG` included -- the config
file being present is what skips the first-run sound-setup prompt). Do not
mount `D:\ctd\tyr586` itself; it is a live throwaway harness for other work
and must not be written to or locked by a DOSBox-X process.

```powershell
robocopy D:\ctd\tyr586\oracle\t2k D:\ctd\corpus-scratch\tyrian-specs /E
```

## The verified invocation (EXACT; the key schedule is part of the pin)

```powershell
.\.bench\oracle\Invoke-DosboxOracle.ps1 -Dir D:\ctd\corpus-scratch\tyrian-specs `
    -Command 'AUTOTYPE -w 16 -p 2.5 down enter enter down enter','TYRIAN.EXE' `
    -Persona 586 -TimeLimit 34 -WallLimit 150 -RunName tyrian-specs `
    -Set 'dos:ems=true','joystick:joysticktype=none'
```

Notes on the pin:

- **`joystick:joysticktype=none` is mandatory**, not optional polish. A
  detected joystick eats the menu navigation on this screen (documented
  trap, `PROTOCOL.md` and the 2026-09-02 comparison doc section 1.5) --
  `in al,dx` on port 0x201 dominates guest time and desynchronises any
  timed AUTOTYPE schedule built without it.
- **Do not add `-Turbo`.** It desynchronises AUTOTYPE against the emulated
  clock; a schedule tuned under `-Turbo` will not reproduce without it and
  vice versa (comparison doc section 6.3).
- `-Set 'cpu:core=...'` was left at the harness default (`auto`, i.e.
  dynamic core) for this verification. `core=normal` also works but is far
  slower wall-clock and is only needed for an instruction trace, not for
  reaching this screen.
- The exact `-w`/`-p` pair matters and is NOT portable across DOSBox-X
  builds -- see "What failed" below. Re-verify by screenshot (this file's
  method) before trusting this pin on a different build or a different
  host.
- Once on Ship Specs the screen is idle/stable (it waits for a keypress),
  so any `-TimeLimit` at or above the schedule's last key (here, keys land
  at guest t = 16, 18.5, 21, 23.5, 26 s; the screen is reached and stable by
  t ~ 24 s) is a valid dwell window. 34 s was used for margin.

## Capturing a verification screenshot

The stock oracle harness has no framebuffer capture (`PROTOCOL.md` section
7, "known gaps"). Verification here used the local, GPL-2.0-derivative,
scratch-only DOSBox-X debug build already documented in
`dev_docs/2026-09-02-tyrian-dosbox-comparison.md` section 1.2
(`D:\ctd\dosbox-x-debug\bin\x64\Release\dosbox-x.exe`, never entering repo
history), which adds `DOSBOX_SHOT_DIR` / `DOSBOX_SHOT_INTERVAL_MS`
env-var-driven periodic mode-13h PPM dumps. Pass `-DosboxExe` pointing at
that binary and set both env vars before invoking the script above; no
other change is needed. A stock (non-debug) DOSBox-X build has no
command-line screenshot path at all -- the mapper screenshot key is
interactive-only and was not used.

## What failed (2 attempts before this one verified)

**Attempt 1** -- reused the comparison doc's `AUTOTYPE -w 14 -p 2.0 down
enter enter down enter` verbatim, same persona/joystick/EMS settings,
against a fresh mount copied for this verification. It did NOT reproduce:
the leading `down` key landed too early (before the main menu's input
handling was live) and was dropped, so the four following keys
(`enter enter down enter`) shifted into the *Start New Game* path instead
of *Load Game*: Players menu (t~13s) -> "1 Player Full Game" (default,
picked by the now-first `enter`) -> Select an Episode -> `down` moves to
Episode 2 -> `enter` -> **Difficulty Level** menu, where the schedule ran
out. Final frame at t=32s: Difficulty Level (Easy/Normal/Hard), not Ship
Specs. This is the same failure shape the comparison doc describes for its
*own* first (also-withdrawn-as-a-timing-source) schedule, just landing one
menu deeper -- confirming the fix is more margin before the first key, not
a different key sequence.

**Fix and attempt 2** -- widened both the initial wait and the inter-key
period (`-w 16 -p 2.5`) to give the first `down` more margin past the menu
becoming interactive. This reproduced the intended path end to end (main
menu -> Load Game -> One Player Saved Games -> Game Menu -> Ship Specs) and
is the pin above.

## Evidence

- `D:\ctd\oracle-scratch\tyrian-specs-verified\tyrian-specs-shipspecs-t34.png`
  (and the raw `.ppm` beside it) -- the dwell-window frame at guest t=34s
  from the verifying run, 2026-09-02. Shows the Ship Specs screen: ship art
  and name banner "MicroCorp Stalker-B", descriptive text, "Press a key"
  prompt. Scratch, outside the repo (`.bench` binary data is never tracked
  in git per repo convention); regenerate by re-running the invocation
  above with the shot-dump env vars if this path is gone.
- Frames at t=17s through t=30s from the same run all show either "One
  Player Saved Games" (17-19s), "Game Menu" (21-22s, "Ship Specs"
  highlighted at 22s) or Ship Specs itself (24-34s) -- the transition is
  clean and monotonic, not a lucky single-frame hit.

## What this file does NOT claim

No timing, wall-clock, or rt number is recorded here. The box this
schedule was verified on is mid-campaign; any such number would be void.
This file exists so a later quiet-box run can point `-RunName`/`-TimeLimit`
at the invocation above and get straight to a measurement, without
re-discovering the menu path or the joystick trap first.
