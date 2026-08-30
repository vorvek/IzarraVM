<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Per-game agent brief

You profile ONE game from `R:\La Colección by Neville\dosroot`. Your goal has
three parts: reach gameplay, record a deep profile, and leave a replayable
recipe. Do not modify the collection. Do not commit anything to git.

## Tools

The runner: `pwsh D:\dev\IzarraVM\.bench\corpus\scripts\run-corpus-game.ps1`

| parameter | meaning |
|---|---|
| `-Game '<name>'` | directory name under dosroot, exact |
| `-Cpu 486\|586` | persona; 486 = 66 MHz, 586 = 166 MHz |
| `-GuestSeconds N` | run length in guest seconds |
| `-Entry '<cmd>'` | AUTOEXEC command; default `CALL GAME.BAT`, else the only executable |
| `-InjectKeys 'cycles:text;...'` | key schedule; offsets are GUEST CYCLES (seconds x 66e6 or 166e6), strictly increasing; `\r` = Enter, `{esc}`, `{space}` |
| `-InjectMouse 'cycles:action;...'` | `home`, `move:<dx>,<dy>` (mickeys), `down`, `up`, `click` |
| `-ScreenDumpMs 5000` | periodic screenshots (steering only, never for the profile run) |
| `-BarrierCensus` | census counters, for the final profile of a slow game |
| `-Label <word>` | names the result directory |
| `-NoLoop` | do not restart the game when it exits |
| `-ConfigExtra @('...')` | extra CONFIG.SYS lines |
| `-Mouse` | load C:\DOS\TOKAMOUS.COM before the game; REQUIRED for any `-InjectMouse` schedule (no driver loads by default, and without one the packets go nowhere); recipe field `mouse_driver: true` |
| `-NoEmm` | omit TOKAEMM from CONFIG.SYS (control runs) |
| `-EmuExtra @('...')` | extra emulator flags, e.g. `--interpreter` for an A/B |
| `-CdImage <path>` | mount a CD image |

Results: `D:\dev\IzarraVM\.bench\corpus\results\<slug>\<stamp>-<label>\` with
`profile.json`, `emulator.log`, `run-meta.json`, `end-frame.ppm`, `screens/`.

View a screen: `magick <file>.ppm <file>.png`, then Read the PNG.

## Procedure

1. Inspect the game directory on R:. Read `game.bat`, any `.txt`/`.diz`/config
   files. Note the video hardware the game targets. If the game supports ONLY
   Tandy graphics or sound, stop and report status `SKIP-TANDY`. If the folder
   is not a playable game (utility, data), report `SKIP-NONGAME`.
2. Probe: 60 guest seconds, `-ScreenDumpMs 5000`, no schedule. Convert and
   read the screens. Find where the game stops waiting: title screens, menus,
   sound setup prompts.
3. Iterate a key/mouse schedule until the screens show GAMEPLAY (the engine
   running: a level, a match, a track, attract-demo only if nothing better is
   reachable). Prefer the shortest schedule that works. Leave 5 or more guest
   seconds between steps; menus draw slowly at 486. All keys must land in the
   first half of the run.
4. Pick the persona: 486 for real-mode-era titles, 586 for protected-mode 3D.
   When unsure, run both once and report both. If the game crashes at 586 but
   plays at 486, that is a finding; record it.
5. Deep profile: one run of 120 guest seconds or more, schedule in place, NO
   screen dumps, `-Label deep`. Add `-BarrierCensus` when the probe showed
   rt below the persona target: below 5.0 at 486, below 2.0 at 586. Confirm gameplay actually held: check `end-frame.ppm`.
6. Write the recipe to `D:\dev\IzarraVM\.bench\corpus\games\<slug>.json`:

```json
{
  "game": "<exact dir name>",
  "cpu": "486",
  "guest_seconds": 120,
  "entry": "CALL GAME.BAT",
  "inject_keys": "990000000:\\r",
  "inject_mouse": "",
  "config_extra": [],
  "notes": "what the schedule does, what gameplay phase the window covers"
}
```

7. Report back, as data, the ledger row fields plus the evidence block below.

## What to report (the evidence block)

Print it with the helper, then add the two prose items (phase covered,
log anomalies):

```
python D:\dev\IzarraVM\.bench\corpus\scripts\summarize-run.py <result_dir>
```

From the deep run's `profile.json`, top level unless noted:

* `real_time_factor`, `direct_native_coverage`, `guest_seconds`,
  `stop.kind`
* `perf.instructions` if present, else the instruction count you find
* `timer.pit_writes`, `timer.irq0_edges` (pit_writes per guest second above
  ~300,000 flags a latch-poll storm — report it)
* `perf.brk_cont_not_continuable`
* If `-BarrierCensus` ran: the top 3 census rows by `unbound_exits`
* The video mode fields (`mode`, `legacy_video_mode`, `active_display`)
* One sentence: what phase of the game the profile window covered
* Anything anomalous in `emulator.log` (port faults, unclaimed I/O)

rt numbers are INFORMATIONAL triage, not gated measurements; other sessions
may load the host. Do not tune schedules by rt. Deterministic counters are
trustworthy regardless of load.

The performance targets: 486 should reach rt 5.0 or more, 586 should reach
rt 2.0. A deep run below its target is a FINDING. Report it as such, and
include the barrier-census evidence.

## Traps

* A black screen with frozen counters and busy CPU is usually a game WAITING
  for a key, not a hang. Try Enter or Space before diagnosing.
* One key too many can open a menu that blocks the attract demo forever.
  Keep schedules minimal.
* `--inject-keys` offsets are cycles, not milliseconds. Wrong rate = schedule
  fires at the wrong time at the other persona.
* Games that reboot or exit loop through AUTOEXEC. If the screens show the
  same early phase repeatedly, the game is crashing; read `emulator.log`.
* Do not trust the game's own speed feel; only counters and screens.
