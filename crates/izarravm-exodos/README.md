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
  floppy image, a booter disk, no launch command, a multi-CD swap, BASIC.

Measured over all 7,666 confs on 2026-08-16: 79.36% translatable, 9.35%
recoverable, 11.28% untranslatable.

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
must never come out of here.

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
