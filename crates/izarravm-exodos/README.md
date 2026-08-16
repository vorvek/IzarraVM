<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# izarravm-exodos

Classify and translate the eXoDOS corpus. Read-only against the corpus: nothing
here opens a path under the collection root for write.

The companion harness is `scripts/sweep-exodos.ps1`, which extracts, calls this
tool, runs the emulator, archives every artifact and deletes the extracted game
files. Design: `dev_docs/exodos-sweep-design.md`.

## census

Classify every `<dos-root>/<short>/dosbox.conf` without extracting anything.

    izarravm-exodos census --dos-root "E:\eXo\eXo\eXoDOS\!dos" --output <dir>

Writes `census.jsonl`, `census.tsv` and `census-summary.json`, and prints the
summary. Three classes:

- `TRANSLATABLE` — the conf maps onto an IzarraVM invocation with no special
  handling. A `call run` recipe is here, because the launcher BAT is flattened.
- `RECOVERABLE` — translatable with work the translator does: a `pause` prompt,
  several launch commands, a second directory mount, a composed `cd`, a
  `memsize` above 64.
- `UNTRANSLATABLE` — a hard blocker with a reason code: a non-VGA `machine`, a
  floppy image, a booter disk, no launch command, a multi-CD swap, BASIC, or a
  CD this machine cannot serve (see below).

Measured over all 7,666 confs on 2026-08-16: 79.36% translatable (6,084), 9.20%
recoverable (705), 11.44% untranslatable (877).

### CDs the machine cannot serve

There is one CD in this machine and it comes from `--cd-image`, which reads a
`.cue` as a sheet and hands anything else to the ISO reader. Two conf shapes are
therefore refused outright rather than translated into an AUTOEXEC that dies at
its first line:

- `cd-mount-unsupported` — the guest switches off C: (`d:`) with no image the
  translator can mount, which is what a conf that mounts a host DIRECTORY as its
  CD looks like. 54 confs, all of which already carried another reason; 12 of
  them moved out of `RECOVERABLE` when this was added.
- `cd-image-unsupported` — an `imgmount` naming a bare `.bin`. A `.bin` is
  2352-byte raw sectors and is only readable through its sheet, so counting it
  as a working CD would overstate the census. No conf in the collection does
  this today; the reason exists so a silent drop cannot reappear.

## translate

Turn one already-extracted game into a runnable folder plus an invocation.
Extraction is the caller's job, so this never opens a zip.

    izarravm-exodos translate --conf <dosbox.conf> --extract-root <scratch> \
        --short DOOM --persona 586 --cycles 20000000000 --output plan.json

It writes `CONFIG.SYS`, `AUTOEXEC.BAT` and `EXITVM.COM` into the resolved mount
root, removes eXo's zero-byte `.exo` title marker, clears read-only bits, and
emits the emulator argument vector along with the classification and a flag set.

### What the flattener does

38% of corpus autoexecs end in `call run`, and `RUN.BAT` is a `CHOICE` sound
card menu. `CHOICE.EXE` exists in Toka-DOS, so an unflattened menu does not
error out: it sits waiting for a keypress while the run looks alive and measures
nothing. The flattener walks the BAT the way COMMAND.COM would, with
`if exist` answered from the real extracted tree and `CHOICE` answered by
preferring the branch whose menu text names a Sound Blaster. DOS
`if errorlevel N` is `>= N`, so the branch chosen is the one a keypress would
actually reach rather than the one an equality read would name. Output is a
linear AUTOEXEC with no labels, no `goto` and no `choice`.

A backward `goto` is a loop and demotes the title to `UNTRANSLATABLE` rather
than being emitted; the fixture AUTOEXECs' own `:loop` shape is exactly what
must never come out of here. The same rule governs a `CHOICE` branch: a branch
label ABOVE the choice line is a menu loop, so it leaves the reachable set and
the remaining branches are re-scored without it.

Two refusals bound what a flattened menu may do. A menu whose every reachable
branch scores as refused — Setup, Install, Quit and nothing else — takes no
branch at all and falls through to key injection, because the alternative is
running an installer over the game. And branch scoring reads `order` and `help`
as whole words, so the `Border` in a title is not read as an order form.

### Recipes

Flattening removes the launcher menu but not the game's own title screen. Every
run carries a timed key schedule, expressed in GUEST milliseconds and converted
against the persona clock, so one recipe replays at the same guest time on 486
and 586. `--recipe-dir <dir>` looks for `<short>.json`; without one the built-in
generic sequence is used. `izarravm-exodos default-recipe` prints it as a
template.

Steps land inside the first 55 guest seconds on purpose: the classification
window is the last 60 seconds of a 120-second run, and an injection schedule
slices the run into one short call per scancode, so a schedule reaching into the
window would put a knee inside it.

## classify

Turn archived sweep rows into structural-bucket verdicts. The orchestrator
collects; this decides.

    izarravm-exodos classify --input <sweep-dir> --output <dir> [--tsv]

`--input` takes one game's archive directory (`profile.json` plus
`screens/screens.jsonl`), a whole sweep output directory (game subdirectories,
with `rows.jsonl` read for the host-side outcomes the archive cannot carry), a
directory of bare profile JSONs, or a single profile JSON. Writes
`classify.jsonl` and `classify.tsv`.

### The window

Marks are guest-clock driven. Measured on the smoke archive: across 60 periodic
marks the guest spacing holds 1999.25-2000.84 ms while the wall spacing between
the same marks ranges 615 ms to 5,114 ms. The window is the delta between the
last mark and the mark nearest 60 guest seconds earlier, and it is a
guest-deterministic input.

Only B1, B2 and B3 can use it. The mark subset carries neither the callout
family, the x87 counters nor `jit_direct_callout_port_v86_served`, so B4, B5a,
B5b and B6 read whole-run totals and say so in their `windowed` field.

A profile with an EMPTY mark series is a run where `IZARRAVM_PHASE_INTERVAL_MS`
was never armed, which is not a short run: it keeps its counter-derived outcome
and carries `NO-MARKS`. `SHORT-RUN` means marks were armed and there were fewer
than 31 of them.

### The buckets

The repaired rules of the design's §9.2, each carrying the measured value that
put the row there:

| bucket | rule | fires on |
|---|---|---|
| B1 | `insns/entry < 4` and `entries/I > 0.05` | prince-486 |
| B2 | `1 - jit_direct_insns/I > 0.15` | duke3d x2 |
| B3 | `smc_heat_demotions/I > 1e-7` | duke3d x2, nascar, wolf3d x2 |
| B4 | `(callout_executed + step_break + abnormal)/I > 0.015` | gp2, wolf3d x2 |
| B5a | `side_exit_x87_eligibility/I > 1e-3` | tombraid |
| B5b | `jit_direct_x87_pad_bails/I > 1e-5` | duke3d x2 |
| B6 | `callout_port_v86_served/I > 0.01` | wolf3d x2 |

B7-B10 were cut and survive as reported columns only. The eleven-row fixture
table is the acceptance gate and lives in `bucket_test.rs`; the numbers there
are read off the scoreboard board and no threshold may be moved to make a row
pass.

### Idle detection

The design proposed `device_write_bytes` as the frame proxy and a flat window
delta as the idle signal. That is refuted and the code records the refutation:
DOOM runs the full 120 guest seconds in mode X with its frame hash changing 11
distinct times inside the window, and its window `device_write_bytes` delta is
exactly zero, because the counter stops accruing once the VGA aperture goes
through the direct-data path. The frame-hash index from `screens.jsonl` is the
flatness test instead — one distinct hash across at least three in-window
samples — and the counters serve as the corroborating polling signature. A flat
picture with no polling term stays `RAN` and carries `FLAT-PICTURE-NOT-IDLE`
rather than disappearing into an idle bucket.
