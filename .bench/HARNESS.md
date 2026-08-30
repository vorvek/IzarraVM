<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Benchmark harness: fixtures, metrics and phase sampling

Written 2026-08-06. Companion to `PROTOCOL.md` (the A/B measurement rules) and
`README.md`. This file covers WHAT to run and HOW to read it; `PROTOCOL.md`
covers how to compare two builds without lying to yourself.

---

## The fixtures

Each is a C: drive: `CONFIG.SYS`, an `AUTOEXEC.BAT` that launches the game, and
the game directory. DOS itself comes from the Katea FreeDOS overlay, not the
fixture. **Copy fresh per run** — several of them mutate their own tree. Most
AUTOEXECs `goto loop` so the game restarts for the whole budget; `duke3d_c` is
the exception and runs DUKEMARK exactly once, then ends the VM itself with
`EXITVM.COM`.

| fixture | persona | what it is | why it is here |
|---|---|---|---|
| `jemmex_doom_c` | 486, 586 | Doom, 32-bit DOS4GW under JEMMEX | the long-standing oracle |
| `quake_c` | 586 | Quake, 32-bit | x87 and the second oracle |
| `prince_c` | **486** | Prince of Persia, real mode | a real 16-bit game |
| `wolf3d_c` | 486, 586 | Wolfenstein 3D, real mode | the 16-bit heavyweight (see the 2026-08-08 crash-loop note) |
| `duke3d_c` | 486, 586 | Duke Nukem 3D **Atomic** + DUKEMARK | a late 32-bit title, and the only fixture that scores itself |
| `nascar1_c` | 586 | NASCAR Racing 1, integer 3D | 640x480, the SLOWEST fixture |
| `gp2_c` | 586 | Grand Prix 2, x87 physics | mouse-driven, x87-heavy |
| `bench16_c` | 586 | synthetic 16-bit loop | historical; see the warning below |
| `tombraid_c` | **586** | Tomb Raider Gold, DOS4GW, CD REQUIRED | late-DOS Pentium 3D + CD-streamed FMV; see below |
| `duke3d_short_c` | 586 | the same DUKEMARK run, demo cut to 1560 records | the CHEAP duke ladder row: 143 s a leg against 342 s |
| `tyrian_setup_c` | 486 | Tyrian 2000 SETUP.EXE, settings menu + jukebox | the guest 70 Hz audio clock (PIT rewrite per frame + MPU-401 MIDI + DSP 0x14 chain); see PROTOCOL.md |
| `tyrian_c` | 486, 586 | Tyrian 2000 gameplay, scripted to level 1 with fire held | same audio clock under play; the 586 row is the perf row |
| `psycho_c` | **486** | Psycho Pinball, DOS/4GW, a table in play | the only row that replays its CRTC register table EVERY FRAME; grades the PUBLISHED frame, not a re-render |
| `tombraid3d_c` | **586** | Tomb Raider Gold, 3dfx build, CD REQUIRED | the same game as `tombraid_c` with every pixel through GLIDE, so the two rows split a regression between engine and rasteriser |
| `descent2_c` | **586** | Descent II, 3dfx patch, recorded demo, CD REQUIRED | the HEAVIER Glide row: rt 0.32 against Tomb Raider's 0.87, and it ships the byte-identical `glide2x.ovl` |

`duke3d_short_c` is generated, not authored: `scripts/make-duke-short-fixture.ps1`
writes it from `duke3d_c` by lowering the record count in BENCH2.DMO's header and
pointing the AUTOEXEC at the copy. Ladder candidates on it; re-run the LONG
`duke3d-586` row before any merge decision. Full account in `PROTOCOL.md`.

`nascar1_c` and `gp2_c` need input schedules and have framebuffer invariants —
`gp2_c` an end-of-budget hash, `nascar1_c` a frame contract since 2026-08-18
(below); both are documented in full in `PROTOCOL.md`, which is the authority for
every fixture's exact invocation. Do not paraphrase an invocation from here: the
recorded sha256 values were measured under those arguments, so changing a
persona, a memory size or a video card invalidates the invariant silently
instead of failing.

### Which persona, and why it is not a free choice

* **Prince of Persia must run at 486.** At 586 it takes a CPU-detection path that
  ends in `OUT DX,AL` to port 0x7421 and dies after ~7 s. That is not gameplay,
  and a census taken there is dominated by the port-sweep loop that leads to the
  fault. Do not benchmark PoP at 586.
* **Doom runs at both.** 486 and 586 are separate policies in the realtime gate.
* **Wolf3D and Duke3D run at both.**

### Tomb Raider (added 2026-08-14)

`tombraid_c` is the first CD fixture. The disc image lives at
`.bench/tombraid_cd/tombeng.cue` (+`.bin`, one MODE1/2352 data track and 59
audio tracks) and mounts through the `--cd-image` flag the scoreboard passes
for any fixture row carrying a `cdImage` field. The image is read-only at run
time, so it is NOT copied per run; the fixture folder still is.

**586 only.** The game needs a Pentium and an FPU; the 486 persona cannot hold
it. Software renderer. No input schedule: the title menu starts a demo level by
itself after idling.

Measured timeline at 586 (phase marks, 1000 ms,
`.bench/results/tombraid-bringup/timeline-20260814-231557/`): boot 0-3 guest
seconds, CD-streamed RPL intro FMV 3-125 (rt ~0.45, the slow phase the fixture
exists to expose), title menu 126-141 (rt ~3.2), level load 142-144 (rt ~0.10,
interpreter-bound), demo level 1 at 145-179 (rt ~0.45, coverage 0.95,
insns/entry 55-60), then menu/demo rotation repeats. The 28e9 budget (169
guest seconds) covers FMV + menu + load + most of demo 1 and lands the end
frame MID-DEMO on purpose: that is where the fixture's real work is, and 30e9
would land on the demo-to-menu transition. The end frame is graded on BANDS,
not on a hash — see below.

The game's sound config (`HMISET.CFG`) selects Sound Blaster 16 at 220/7/1
with no MIDI device. `TOMBPATH.TXT` sits in `C:\` (17 bytes, NO trailing
newline - the copy is byte-exact from the owner's install) and points the game
at `C:\GAMES\TOMBRAID`.

### The two Glide rows (added 2026-08-30)

`tombraid3d_c` and `descent2_c` are the first fixtures that render through
Distira. Full account in `PROTOCOL.md`; three things belong here.

**The discs.** `tombraid3d_c` mounts `.bench/tombraid_cd/tombeng.cue`, the SAME
image `tombraid_c` uses -- same game, same pressing, so a second 643 MB copy
would buy nothing. `descent2_c` needs its own, `.bench/descent2_cd/`, 691 MB.

**`descent2_c` deliberately omits the movies.** The three `.MVL` files are
220 MB of the source tree's 266 MB, the row runs with `-nomovies`, and the
scoreboard robocopies the whole fixture per run. Adding them back would cost
more per run than the rest of the fixture put together.

**Both rows carry an input schedule, and it is minimal on purpose.** Each 3dfx
build waits for a keypress on a screen that shows nothing useful -- Tomb
Raider's is BLACK -- and a run without the schedule reads exactly like a hung
emulator, with every device counter frozen and the CPU still busy. But ONE key
too many and Tomb Raider opens its ring menu and never starts the attract demo
at all, so the row would pin a still picture and pass. The measured windows are
in `PROTOCOL.md`.

### Psycho Pinball grades the PUBLISHED frame (added 2026-08-29)

`psycho_c` is the only row whose picture comes from `--presented-ppm` rather
than `--result-ppm`, and the distinction is the reason the row exists.
`--result-ppm` RE-RENDERS the whole frame at stop-time register state, so it
reports what video memory holds; `--presented-ppm` writes the frame the scanout
actually published, which is what a user sees. A defect that fills video memory
correctly and never publishes it is invisible to the first and plain in the
second: measured on this fixture with the `resize_work` raster wipe restored,
the same run reads **82.9% non-black / 127 colours** through `--result-ppm` and
**0.0% / 1 colour** through `--presented-ppm`.

The row also carries the only per-frame CRTC replay in the table. The other
graded rows touch `resize_work` with an unchanged pixel count two or three times
across a whole run; this one does it 4144 times, roughly once a frame. That is
why a raster-lifetime defect could sit in the VGA core with every board row
green.

WHAT IT CATCHES, both arms measured by restoring the defect and re-running:

* raster wipe restored -> FAIL, `non-black coverage % is 0, outside the band
  [60, 95]`, plus `distinct colours is 1`.
* mode X CRTC reseed restored -> PASS. It does NOT catch that one. The reseed
  damages only the FIRST mode set, which is the menu phase; by the budget the
  game has re-entered its gameplay mode from inside mode X, so the geometry is
  320x368 either way. Geometry regressions are covered by the video-crate unit
  tests, which is where they belong.

No end-of-budget hash, for the reason Duke3D and Tomb Raider lost theirs: the
picture animates continuously. Three repeat runs are bit-identical, so the
determinism is real; it is robustness to CODE change that a hash would lack.
The anchor is the Toka-DOS boot text at 0.6 guest seconds -- the ONLY run of
four identical 250 ms samples in the first 25 guest seconds, measured rather
than assumed -- so it pins boot determinism and nothing about the graphics.

### The warning about `bench16_c`

It reads 5.175 native instructions per entry and about a 20% loss. Real 16-bit
games read 9.68 (PoP-486) and 2.10 (Wolf3D-486). **It under-reads a real game by
roughly 2x and it mis-ranks**, and the 16-bit campaign spent months steered by it.
Prefer PoP and Wolf3D for any 16-bit conclusion.

**2026-08-08 correction: every wolf3d number recorded before this date measured
an out-of-memory CRASH LOOP, not the game.** The fixture's CONFIG.SYS was
missing its `DEVICE=C:\DOS\TOKAEMM.SYS` line, so DOS could not load high,
WOLF3D.EXE printed "You do not have enough memory" ~0.4 s after each launch,
and the AUTOEXEC `goto loop` restarted it forever -- the recorded "signon
screen" frame was each fresh attempt's redraw, and the 1.2-2.1 insns/entry
"16-bit worst case" reading was program startup + DOS reload, not raycasting.
The tell in hindsight: wolf3d-486 and wolf3d-586 shared ONE frame hash. The
fixture now loads TOKAEMM and injects one Enter at the signon (see the
scoreboard table); wolf3d-586's budget lands the end frame inside demo
playback.

---

## Which metric, and when

| metric | fixtures | properties |
|---|---|---|
| `realtics` / `gametics` | doom | guest-reported, robust to host noise. LOWER realtics is faster |
| `QCONSOLE.LOG` last line | quake | `969 frames 22.3 seconds 43.5 fps` |
| DUKEMARK report file | Duke3D | clean EXITVM stop + Info String + sample count are INVARIANTS; fps is a MEASUREMENT |
| framebuffer PPM SHA | PoP, Wolf3D | a CORRECTNESS invariant, see below |
| `real_time_factor` | Wolf3D, Duke3D | the performance metric for the two that print no score |
| wall seconds | all | fragile; see `PROTOCOL.md` |

### Duke3D scores itself and ends the run itself

The fixture's AUTOEXEC is the whole protocol:

```
@echo off
cd \DUKE3D
DUKEMARK.EXE /bqBENCH2 > C:\DUKEMARK.TXT
C:\EXITVM.COM
```

DUKEMARK plays its demo, prints a report through DOS stdout (so redirection
catches it — verified, the 80x25 text page is blank on a redirected run), and
`EXITVM.COM` ends the VM through the Lotura unit-tester exit port. **The cycle
budget is a guard, not the length of the run.** Katea flushes the guest's writes
on the way out, and the harness reads `DUKEMARK.TXT` out of the working copy.

Invariant (all four must hold): the run stopped as `test_exit` code `0x51`, the
result file exists and parses, the Info String is `2,320,200,2,0,1,1,1` (the
config fingerprint `Demo,Width,Height,Mode,Hud,Detail,Sound,Music`, so the
trailing `1,1` pins sound and music ENABLED), and the extrapolation count is
within ±2% of its pin — **pinned PER PERSONA**, because the count is not
machine-independent the way DUKEMARK's docs claim. The first three are
timing-insensitive. See `PROTOCOL.md` for the reasoning. **FPS min/max/avg are
measurements** — they move with host load and are reported in the row's notes,
never asserted.

This replaced the framebuffer-PPM hash on 2026-08-09. That hash was
cutoff-phase sensitive: the fixed-cycle axe landed mid-render in an animating
demo, so any JIT-mix change that shifted cycle charging moved it legitimately —
six benign moves in three days, then an automatic re-pin path that turned main
red three times over the LICENSE_MANIFEST sha it had to move with it. The full
account is in `PROTOCOL.md`.

### Tomb Raider and NASCAR grade a FRAME CONTRACT (2026-08-18)

Both rows lost their end-of-budget framebuffer hash for the reason Duke3D lost
its own: the frame lands mid-attract-demo with the camera in flight, so the hash
samples the demo's PHASE and any cadence-adjacent change moves it while rendering
stays perfect. It moved twice in one day — tombraid-586 by 84.31% of its pixels
under the IOPL-3 V86 monitor, nascar-586 by 12.41% under the same day's
follow-up — and each move cost a full attribution cycle to prove nothing was
wrong. What replaces it, per row: **one exact frame hash at an EARLY anchor**
where the picture is not moving (tombraid at 0.5G, the DOS/4GW banner page,
pinned as a set of two hashes because the DOS underline cursor blinks there —
18 pixels, measured; nascar at 0.445G, the static startup logo, sitting at the
centre of a measured 100M-cycle window of bit-identical frames), plus **bands at
the budget** on non-black coverage and distinct-colour count, plus the **display
class** (path, depth, mode, geometry), the `cycle_limit` stop, and **retired
instructions to ±5%**. The end-of-budget hash is still recorded, as
`final_frame_sha256`, and never graded. Unlike Duke3D these rows print no score
of their own, so the early anchor is not optional: `PROTOCOL.md` trap 0 records
that count-only framebuffer invariants do not discriminate. Every band's
derivation, and the two-sided justification for each number, is in
`New-FrameContract` in `scripts/run-fixture-scoreboard.ps1`. Cost: one extra
short emulator run per row, ~7 s against tombraid's ~250 s.

### The framebuffer hash is a real invariant, not a fallback

Still true for PoP, Wolf3D and GP2; Duke3D is now scored by DUKEMARK instead
(above), and Tomb Raider and NASCAR by a frame contract (above).
Verified on both new fixtures: the frame at a fixed guest-cycle budget is
IDENTICAL across `IZARRAVM_JIT16=0` and `=1`, even though on Wolf3D that is the
difference between 0% and 58.7% native coverage and 387 million compiled 16-bit
blocks. Demo playback is driven by guest tics, so a fixed guest budget reaches a
fixed demo state no matter how fast the host got there.

So a moved hash means guest state changed, which is a correctness bug and not
noise.

### realtics is SESSION-LOCAL

The same commit has produced 813 and 769 hours apart, and the value also moves
with the memory manager (JEMMEX 826, TokaEMM 812). **Compare only within one
sitting, against a control measured in the same window.** A 7xx reading is not
evidence of an improvement.

---

## Where every fixture sat on 2026-08-08 (the 166 MHz / 64 MB baseline)

Arm `on`, one-lookup store/load `1`, quiet rebooted box (~4-5% background),
main `79216248`. Guest anchors: doom-586 998 realtics (74.8 fps), quake 41.2
fps. wolf3d rows are the FIXED fixture (real demo playback, see the
crash-loop note above); their pre-fix history is not comparable.

| fixture | rt | coverage | insns/entry |
|---|---|---|---|
| doom-486 | 3.300 | 0.911 | 22.9 |
| quake-586 | 1.554 | 0.962 | 61.2 |
| doom-586 | 1.169 | 0.929 | 27.0 |
| duke3d-486 | 1.094 | 0.701 | 7.0 |
| wolf3d-486 | 0.769 | 0.664 | 2.0 |
| prince-486 | 0.868 | 0.980 | 1.6 |
| gp2-586 | 0.347 | 0.886 | 17.0 |
| nascar-586 | 0.278 | 0.908 | 24.2 |
| duke3d-586 | 0.257 | 0.635 | 11.9 |
| **wolf3d-586** | **0.138** | 0.675 | 2.1 |

Campaign targets (~2x rt on most 586 loads): wolf3d-586 needs ~14x,
duke3d-586 ~8x, nascar-586 ~7x, gp2-586 ~6x.

**The two duke3d rows in both tables above are NOT comparable to anything
measured after 2026-08-09.** The fixture changed game (1.3D to Atomic), workload
(a 120 s window inside a looping demo, to a whole guest-driven DUKEMARK run) and
run length. The first post-change readings, on the shipped EXITVM shape:
duke3d-486 rt 1.080 / coverage 0.733 / 6.27 insns per entry / 272 s of wall, and
duke3d-586 rt 0.273 / 0.725 / 8.44 / 618 s. Both tiers pass their DUKEMARK
invariants and both are bit-identical across back-to-back runs.

Wall cost of the change: duke3d-486 went from ~110 s to ~272 s a run, duke3d-586
from ~745 s to ~618 s (it got CHEAPER: the old row burned a fixed 120 s of guest
time inside the demo, the new one exits as soon as the demo ends). The whole
sweep measured 29 minutes.

## Where every fixture sat on 2026-08-06 (STALE: 200 MHz / 24 MB, arm off)

Baseline, `IZARRAVM_JIT16=0 IZARRAVM_JIT16_486=0`, taken with the scoreboard on
`44242147`. rt is guest seconds per wall second; 1.0 is real time.

| fixture | rt | coverage | insns/entry |
|---|---|---|---|
| doom-486 | 2.451 | 0.784 | 32.0 |
| prince-486 | 1.596 | 0.002 | 8.9 |
| wolf3d-486 | 1.430 | 0 | n/a |
| quake-586 | 1.339 | 0.940 | 123.7 |
| doom-586 | 1.047 | 0.832 | 70.9 |
| duke3d-486 | 0.912 | 0.313 | 21.3 |
| wolf3d-586 | 0.359 | 0 | n/a |
| gp2-586 | 0.317 | 0.880 | 18.3 |
| duke3d-586 | 0.195 | 0.522 | 17.6 |
| **nascar-586** | **0.095** | 0.895 | 22.1 |

Read these three things off it before planning any work:

**NASCAR-586 is the slowest fixture by a factor of two over the next one**, and
it is NOT a dynarec problem: 89.5% native coverage at 22.1 instructions per
entry. It retires about 17 M instructions per wall second where doom-486 manages
55 M, so roughly 3.3x more host work per guest instruction. gp2-586 has the same
shape at 88% coverage. Whatever costs that is outside the JIT.

**Wolf3D compiles nothing at all with JIT16 off** (0 entries at both personas)
because it is 100% 16-bit. Its numbers are pure interpreter, which makes it the
cleanest available A/B for the 16-bit flip.

**The absolutes were taken under 14-18% background load and are soft by roughly
10%.** quake-586 reads 1.339 here against the gate's pinned 1.462-1.587, which is
measurement drag rather than a regression. Cross-arm deltas from one window are
trustworthy; absolutes across sessions are not. See `PROTOCOL.md`.

## Guest-time budgets

Cycles are guest clocks, so the budget for a wall-clock span depends on the
persona's rate: 486 is 66 MHz, 586 is 166 MHz.

| fixture | persona | budget | guest span | notes |
|---|---|---|---|---|
| doom | 486 | 8e9 | 121 s | gate policy `doom-486` |
| doom | 586 | 6.64e9 | 40 s | |
| quake | 586 | 6.2e9 | 37 s | quake at 166 MHz needs the demo to finish; 31 s was too tight |
| PoP | 486 | 4e9 | 61 s | |
| Wolf3D | 486 | 4e9 | 61 s | |
| Duke3D | 486 | 26.4e9 | 400 s | a GUARD; EXITVM fires at ~10.8e9 / 163 s |
| Duke3D | 586 | 79.68e9 | 480 s | same guard role; EXITVM fires at ~23.2e9 / 140 s |
| Tyrian setup | 486 | 4.7e9 | 71 s | menu to 25 s, jukebox 27-59 s, menu tail |
| Tyrian | 486 | 3.2e9 | 48 s | gameplay from ~31 s; the ship dies at ~53 s, the budget stops before it |
| Tyrian | 586 | 8.05e9 | 48 s | the same guest-second schedule at 166 MHz |

**The Duke3D budget no longer sets the length of the run.** The guest exits
itself through `EXITVM.COM` when DUKEMARK is done, so the budget is only there
to stop a hung run; a row that reaches it fails on the stop-reason invariant
rather than quietly grading a truncated demo. At 486 the guest run is about
32 guest seconds of loading (CON compile, 44 MB Atomic GRP), ~131 s of demo,
then the exit. The loading phase was 161 guest seconds until 2026-08-10; the
budgets below were sized against that and are now very generous guards.

BENCH2 is the shortest of DUKEMARK's three demos (573 samples, against 880 for
BENCH1 and 1248 for BENCH3, measured at 486), which is why it is the one wired
in.

---

## The fixture scoreboard

`scripts/run-fixture-scoreboard.ps1` runs every fixture ONCE and reports real
time factor beside each one's correctness invariant. It is the companion to
`run-realtime-gate.ps1`, not a replacement:

| | scoreboard | formal gate |
|---|---|---|
| workloads | all ten fixture/persona combos | doom-486, doom-586, quake-586 |
| baseline | none, it reports absolutes | a pinned tree, rebuilt and re-measured |
| observations | one per fixture | six pairs per workload per role |
| cost | about 29 minutes | 45 to 60 minutes |
| answers | "where does everything sit right now" | "did this candidate regress against the pin" |

```
pwsh scripts/run-fixture-scoreboard.ps1 -Label before-slice
pwsh scripts/run-fixture-scoreboard.ps1 -Fixtures doom-486,wolf3d-486 -Arm off
pwsh scripts/run-fixture-scoreboard.ps1 -ListFixtures
```

Output lands in `.bench/results/scoreboard-<stamp>-arm<arm>-<label>/` as
`scoreboard.json`, a `scoreboard.md` table, and `profiles/<fixture>.json` holding
the RAW profile JSON per fixture. Keep those: they carry the whole perf block
(fastmap hit/miss, `dev_write`, bus counters, `direct_stalls` with its link,
dormant and unbound splits) and which field matters is never known in advance.

### `-Arm` drives both JIT arms from ONE binary

`IZARRAVM_JIT16` and `IZARRAVM_JIT16_486` are read from the environment, so a
single executable measures both sides and the comparison carries no
build-to-build or build-path-length variance at all. That matters here: this box
has produced a 6% wall difference between two builds of byte-identical source.

| `-Arm` | JIT16 | JIT16_486 | what it is |
|---|---|---|---|
| `on` | 1 | 1 | the shipped default |
| `off` | 0 | 0 | pre-flip behaviour |
| `jit16` | 1 | 0 | the 16-bit half alone |
| `word486` | 0 | 1 | the 32-bit half alone |

Both variables are set EXPLICITLY on every arm. Never unset one to turn it off:
`IZARRAVM_JIT16` parses as `u8` and falls back to **1**, so an empty value reads
as ON, and `IZARRAVM_JIT16_486` is on for every value except exactly `"0"`.

`-OneLookupStore 1|0` (default `1`) drives `IZARRAVM_ONE_LOOKUP_STORE` the same
way — the one-lookup store emission A/B
(`dev_docs/2026-08-07-one-lookup-store-design.md` D8), orthogonal to `-Arm`,
also set explicitly on every run and on for every value except exactly `"0"`.
The chosen value is recorded per row and in the summary as `one_lookup_store`.

### The frame hash is asserted, not just reported

Expected hashes live in `scripts/fixture-scoreboard-invariants.json`. Duke3D has
no frame hash — it has the DUKEMARK invariants instead — but it does have an
entry there, and since 2026-08-10 that entry carries the thing about Duke3D that
is NOT a property of the fixture's config and demo: `dukemark_samples` and
`dukemark_samples_tolerance`, the extrapolation-count band. The timing-insensitive
half of the DUKEMARK pins (demo, Info String, result filename, EXITVM's exit
code) stays in `New-DukemarkPins` in the script, because none of it can move
without the fixture itself being rebuilt. **The sample count DOES follow emulated
timing** — the HDD-geometry slice moved the 486 count from 580 to 919 — so it
goes through the sidecar with everything else that moves, and a pin move is a
one-line reviewable diff with the manifest sha updated in the same breath rather
than an edit buried in a script constant. (An earlier version of this paragraph
claimed the Duke pins were properties of the fixture rather than of our timing.
That is true of three of the four and false of the one that matters.)
`-RecordInvariants` writes them and REFUSES to overwrite a hash that disagrees
with the recorded one unless `-Force`, so a real change cannot be papered over by
re-recording. The sample band works the same way: re-recording a count that has
drifted outside its own band needs `-Force`. The ±2% band was sized against a
measurement rather than guessed — `-Arm off`, the largest lever here, moves the
486 count by ONE against an allowance of 18 — and the derivation, including why
it covers JIT-mix changes but deliberately not timing-model changes, is in
`PROTOCOL.md`'s Duke3D section. A fixture with no recorded hash (or
no recorded sample pin) reports `unpinned` rather than `pass`, so "never checked"
and "checked and fine" are different words.

**`-RecordInvariants` REFUSES to write the frame-contract bands, by design.** It
records what a single run can honestly establish — the anchor frame hash and the
retired-instruction centre — and for the coverage and colour bands it writes
nothing and says so in the row's notes. A band recorded from one run is a band of
width zero around whatever that run happened to do, which is the fragile
invariant these rows were rewritten to escape wearing a different hat. The bands
come from a PHASE-SPREAD measurement instead — the row sampled at several budgets
either side of the graded one, which moves the camera far further than any
cadence change can — and are edited into the sidecar deliberately. The anchor
hash gets one extra guard: a row may declare that its anchor takes more than one
frame (tombraid's two cursor-blink phases), and `-RecordInvariants` will union a
new phase into the set under `-Force` but will NOT exceed the declared count, so
an unexplained third frame is a real change and cannot be absorbed by
re-recording.

The sweep's exit code is 0 only when every row read `pass` or `unpinned`.
Anything else fails it, which since 2026-08-10 includes the row a CRASHED
emulator produces: a run that writes no profile JSON, or writes one that will not
parse, used to report a third word that the exit check did not count, so a sweep
in which every fixture crashed exited 0 and read as a clean sweep.

Running arm `off` with `-RecordInvariants` and then arm `on` against those hashes
makes the pair a correctness check as well as a scoreboard: the framebuffer must
be bit-identical across the two, and on Wolf3D that is the difference between 0%
and full 16-bit coverage.

### THE PINS COME FROM THE SCRIPT'S TREE, NOT FROM THE BINARY'S

**Read this before scoreboarding a worktree.** `run-fixture-scoreboard.ps1:77`
resolves the repository root from its own location:

```powershell
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
```

`$invariantPath` is derived from `$PSScriptRoot` too. So the invariants asserted
against are **whichever copy of the script you invoked**, and `-Executable` is a
free parameter pointing anywhere. Invoke the main checkout's script against a
worktree's binary and you are testing one tree's code against another tree's
pins.

Both orientations happen, and they are NOT equally safe:

| you invoke | you test | result | how it reads |
|---|---|---|---|
| main's script | worktree's exe | worktree code vs **main's old pins** | **false FAIL** — loud, wastes an hour |
| worktree's script | main's exe | main's code vs **worktree's new pins** | **false PASS — SILENT, blesses a regression** |

The second is the dangerous one. A branch that legitimately re-pins a hash, then
measures an unrelated or reverted binary through its own script, gets a green
board that means nothing. Nothing in the output says which invariants file was
read.

**Rule: invoke the script from the SAME tree as the binary under test.** For a
worktree, run the worktree's own copy and let `-Executable` default:

```bash
pwsh D:\path\to\worktree\scripts\run-fixture-scoreboard.ps1 -Fixtures gp2-586 -Label whatever
```

**But a fresh worktree cannot do that yet, and that is the trap's real bait.**
`/.bench/` is in `.gitignore`, so a worktree has NO fixtures — the run dies with
`Fixture folder is missing: <worktree>\.bench\nascar1_c`, and the obvious repair
is to invoke the main checkout's script instead, which is exactly the wrong fix.
The right one is a directory junction, so the worktree's script sees the one
shared fixture tree:

```powershell
New-Item -ItemType Junction -Path D:\path\to\worktree\.bench -Target D:\dev\IzarraVM\.bench
```

Ignored by git, costs no disk, and results still land in the one shared
`.bench/results/`. Do this once per worktree, before the first scoreboard.

Cross-tree `-Executable` is legitimate for a deliberate A/B of two binaries
against ONE pin set — but then say so in the report, because the pins are the
invoking tree's and that is the whole point of doing it.

Hit for real on 2026-08-12: the VBE/Margo slice-1 branch re-pinned gp2-586 in its
own commit, was scoreboarded through the MAIN checkout's script, and reported
`FAIL expected b4f4bcda… got ac11e66a…` — the got-value being the branch's own
correctly committed pin. The expected-vs-got pair naming the *old* hash is the
tell; without it the failure looks like a real regression. It cost two full gp2
runs (~7 minutes each) before the mechanism was traced.

One more reason to read the OUTPUT and not the exit code, which this same episode
produced twice: a PowerShell `try { … } catch { "SCRIPT THREW: $_" }` wrapper
swallows the script's throw and the shell still exits 0, and a bash
`cargo test … | tail -60` reports `tail`'s status rather than cargo's. Both make a
failed run read as a clean one. Assert on the row text (`pass` / `FAIL` /
`unpinned`), never on `$?` alone.

And it misfires in the other direction too, on this very script: `exit 1` at
line 1060 runs in the SCRIPT's scope, so a caller that invokes it with `&` keeps
going, and the wrapper's exit status ends up reflecting a stale `$LASTEXITCODE`
left by one of the emulator invocations inside. The `confirm3` run on 2026-08-12
reported exit 1 with **both rows `pass`** and the failure branch provably not
taken (it prints the failing row names, and printed nothing). So the exit code
can be red on a green board as well as green on a red one. The row text is the
verdict; everything else is decoration.

### Background load is sampled DURING the run

The median whole-machine load, with the emulator's own consumption subtracted,
is recorded per fixture; above 30% the row is marked contaminated and its wall
and rt are not to be trusted. Its deterministic counters still are.

Two traps this instrument was built wrong for first, both worth keeping in mind
for any similar one. Sampling AFTER the run measured Defender chewing through the
robocopy that preceded it, not anything present during the measurement. And a
threshold of 12% marked every observation contaminated, because this host's
resting load with the owner's usual tray software is about 17.7%; a threshold has
to be calibrated against a measured resting state, not guessed.

## Periodic phase sampling

`IZARRAVM_PHASE_INTERVAL_MS=<guest ms>` samples counters from inside the run loop
and emits the series to `--profile-json` as `phase_marks`. Default off.

```
IZARRAVM_PHASE_INTERVAL_MS=500 izarravm --cpu 486 --memory-mib 64 --video vega \
    --hdd-folder <fresh copy> --cycles 7920000000 --profile-json out.json
```

Each entry carries `wall_offset_ns`, `master_ticks`, `elapsed_clocks`,
`instructions`, `jit_direct_insns`, `jit_direct_entries`, `io_stall_ticks`,
`halted_ticks` and the Katea counters. Entries are ABSOLUTE; difference
consecutive ones yourself.

It exists so a fixture that loads and then runs can be split without guessing
where the boundary is: find the knee in the data. Duke's is where rt falls from
~0.31 to ~0.16.

### Three things to know before reading the series

**It does not perturb the run.** Measured: `perf.instructions` and the frame SHA
are identical marks-on against marks-off, and wall differs by 0.14% against a
0.6-0.8% run-to-run spread. It samples at an existing batch boundary and never
slices the host's single `run_until_halt_or_cycles` call — slicing would move
where devices are serviced, which is why the benchmark path must not do it.

**The rt in this series is not the gate's rt.** `stall_for_master_ticks` grants
guest time for zero emulation work while the host burns real wall inside Katea,
so a loading interval looks fast for an accounting reason rather than an
emulation-rate one. Net out `katea_host_wall_ns` and `io_stall_ticks` before
comparing two intervals.

**Load-phase samples are lumpy.** A GRP-read interval and a CON-compile interval
behave nothing alike, so expect a findable knee rather than a clean step.

**Injection schedules manufacture a knee at the schedule's end.** When
`--inject-keys` or `--inject-mouse` is present the run is sliced into one short
`run_until_halt_or_cycles` call per scancode or mouse packet, front-loaded into
the schedule window, then one long call for the remainder. Early intervals
therefore carry hundreds of run re-entries and late ones none, so the series
shows a knee exactly where the injection schedule ends that has nothing to do
with the guest's phases. PoP-486 is injection-driven; do not read its series
without this in mind.

**Filter the series by `id` before differencing.** Arming periodic marks enables
ALL phase marks, so a `POST_END` (POST happens inside the run on the
`--hdd-folder` path) or a guest `CMD_MARK` lands mid-series, and differencing
consecutive rows across one of those produces one short bogus interval. The
periodic rows carry their own id; difference only those.

---

## Still open

* Wall obligations for the sampler that need a quiet box: non-inferiority of the
  compiled-in-but-off build against its parent (the loop is layout-sensitive),
  and validating the series against the zero-code oracle
  `wall(120 s) - wall(70 s)` from two plain runs.
* Wolf3D and Duke3D have no realtime-gate policy entries yet.
* `IZARRAVM_JIT16=2` (the BIOS/ROM window) is measured WASTE: 531 extra compile
  attempts, zero extra installs, because `install`'s page cover wants RAM and ROM
  is not. Do not set it.
* `--bios` is silently ignored with `--hdd-folder`; that path always uses the
  built-in Izarra BIOS.
