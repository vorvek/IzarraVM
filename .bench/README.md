<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# .bench - JIT wall A/B workspace (local-only, git-excluded)

Staging area for the JIT region wall-clock measurements. Everything here is
untracked (in `.git/info/exclude` under `/.bench/` and the per-worktree
`/.claude/worktrees/*/.bench/`). Game fixtures are copyrighted and must
never be committed.

## Layout

```
.bench/
  README.md            this file
  RESULTS.md           the running results ledger (the source of truth)
  PROTOCOL.md          the A/B recipe, fixtures, traps, settled invariants
  jemmex_doom_c/       Doom fixture (jemmex + timedemo demo3, 586)
  quake_c/             Quake fixture (calibration anchor, QCONSOLE.LOG oracle)
  prince_c/            Prince of Persia (needs a memory manager; uses TokaEMM)
  nascar1_c/           NASCAR Racing 1 - integer 3D attract mode, no input needed
  gp2_c/               Grand Prix 2 - x87 physics, driven in by --inject-mouse
  bench16_c/           3DBENCH, real mode, no input needed
  duke3d_c/ wolf3d_c/  further game fixtures
  ab_doom.sh           the interleaved Doom wall A/B harness
  baseline-main/       (created on demand) origin/main worktree for the baseline exe
```

## Fixtures that need input

Two of these cannot be reached by booting alone, and PROTOCOL.md carries the
exact schedules and invariants:

- **NASCAR Racing 1** needs none. Its 3D attract mode starts by itself, which
  is why it is the fixture and NASCAR 2 (menu, no attract mode) is not.
- **Grand Prix 2** is MOUSE ONLY -- keystrokes leave its first screen
  bit-identical. `--inject-mouse` drives the three clicks into a Monza quick
  race. Both the menus and the race replay byte-identically, because a headless
  run seeds no entropy (`--hdd-folder` never calls `seed_rtc`).

Both games need `LH TOKAMOUS` in the fixture AUTOEXEC; NASCAR refuses to start
without a mouse driver even though its attract mode ignores the mouse.

## Why this exists

The Doom wall A/B (the JIT kill gate) needs the game fixture, which lives in
a temp scratchpad that can vanish between sessions. This folder is the stable
copy. Copy a fixture here before measuring; the `.git/info/exclude` entry
keeps it out of git.

## Quick start (when the machine is free, NOT during a render)

```
# 1. Both exes must exist (baseline at origin/main, v2 on the feature branch):
cargo build -j8 --release -p izarravm                           # v2
cargo build -j8 --release -p izarravm --manifest-path .claude/worktrees/baseline-main/Cargo.toml

# 2. Run the interleaved A/B (equal guest event, minimal idle tail):
bash .bench/ab_doom.sh
```

Record the result in RESULTS.md. The verdict rule: v2 must WIN wall (not
tie). A tie refutes the JIT thesis for this loop shape; record and pivot to
the 586 dial recalibration round.

## Budgeting a run

Wall cost is the thing that decides whether a fixture is usable in a loop. An
A/B/B/A set is four legs, so a 12-minute fixture is a 50-minute experiment.
Grand Prix 2 was first cut at 40G cycles (752 s/leg, over an hour for a set)
and shortened to 16G (~290 s/leg) with no loss: the race is in steady state
within seconds of the lights, so a longer run measures the same thing for
longer. Check PROTOCOL.md for each fixture's shipping budget before raising
one.
