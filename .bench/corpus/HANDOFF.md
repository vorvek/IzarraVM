<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Corpus campaign handoff (written 2026-08-30, end of day 1)

State: games 1-12 of 8,418 processed, in `LEDGER.md`. Era at close:
main 0885da4c rebased in, exe sha 4f3381cf0b7d.

## Where to resume

Game 13, '1869', is HALF DONE. The discovery agent parked at the
player-name prompt with this verified schedule (486, `-Mouse`, entry
`1869.EXE`):

```
1716000000:{space};3300000000:test\r;4290000000:{space};5280000000:1\r
```

That covers: logo -> title ({space}) -> copy protection (accepts ANY
text) -> new game ({space}) -> 1 player. Remaining screens, per the
manual: player name (`Capt\r` at ~6300000000), sex (M/F, maybe mouse),
firm name (text), firm location (five towns, LIKELY MOUSE - home+click),
confirm, then the Main Chart = gameplay. Probes live in
`results/1869/probe*`. rt ran 1.2-1.8 in probes, so expect
`-BarrierCensus` on the deep run and an F1-class verdict.

Then game 14: '18th Airborne' (AIR18.EXE + LEAD.EXE). The pre-survey of
games up to '1944 - Across the Rhine' sits in the day-1 session log;
'1942 - The Pacific Air War Gold' has NO top-level executable - explore
subdirectories.

## Pending events to act on

1. **PR #777 merge** (izarravm-f2: EMS function 53h + tokados-hdd.img
   rebuild): rebase, rebuild, replay `games/1830-railroads-robber-barons.json`.
   If it unblocks, tell f2 - it is their regression fixture. The image
   era is inside exe_sha256 (include_bytes).
2. **Background task task_c969be5c** (1830 INT 67h trace) is running in a
   separate session; its result may land first and refine 1.
3. **F1 lever** (V86 reflection HLE-or-VME) is queued on the performance
   campaign; when it lands, re-run every FLAGGED row. F1 members:
   pyramid, 21-for-1-to-4, 1000-miglia, 10rogue, 10th-frame,
   15-move-hole-puzzle. Post-flip 1000-miglia (rt 1.80) is the clean
   single-lever case.
4. `123-talk-shareware` is flagged on a DIFFERENT shape (2 kHz IRQ0
   speech clock + OUT storms) - not an F1 member, keep it distinct.

## Protocol reminders

* BOX PROTOCOL: hold all builds/runs on "BENCH START (WALL)" from the
  perf session; resume on "BENCH DONE". They send advance notice; drain
  agents rather than killing them.
* LEDGER.md is TWO-WAY: the perf session appends write-backs; this
  session commits them.
* rt targets: 486 -> 5.0, 586 -> 2.0. Below target = FLAGGED + census.
* Census: rank non_continuable rows by unbound_exits; hits counts a row
  SHAPE (no addresses); `-NoEmm` is the F1 attribution control, and it
  measures attribution, NOT cost (removing TOKAEMM makes guests SLOWER).
* Subagent claims of JIT divergence need an equal-schedule A/B before
  they enter the ledger (game 4 precedent).
* Spawn one Sonnet agent per game with AGENT-BRIEF.md; tell it to run
  emulator calls FOREGROUND and never park on background notifications.
* Every new recipe JSON needs a LICENSE_MANIFEST.tsv row (class
  `test-fixture`, origin `IzarraVM project`, GPL-3.0-only, sha256),
  and the manifest is PATH-SORTED. CI's file policy fails without it.
  One-liner before each push:
  `for f in .bench/corpus/games/*.json; do ...` (see day-1 close, CI fix).
