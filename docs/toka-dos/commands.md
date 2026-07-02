# DOS Command Reference

Every external command Toka-DOS ships, with the switches it actually
implements. Most of these are FreeDOS tools carried over with a Toka-DOS
rebrand; a few are General Simulation Works's own additions. Where Toka-DOS
diverges from the command's usual behavior, this page says so.

## XCOPY

Copies files and directory trees. Toka-DOS's XCOPY is original project code
written to XCOPY's documented behavior, not a ported FreeDOS binary — it
implements a deliberately smaller switch set than real MS-DOS XCOPY.

```
XCOPY source [destination] [/S] [/E] [/P] [/V] [/W] [/Y] [/-Y]
```

| Switch | Effect |
| --- | --- |
| `/S` | Copy directories and subdirectories, except empty ones. |
| `/E` | Copy subdirectories even if empty. Implies `/S`. |
| `/P` | Prompt `<file> (Y/N)?` before creating each destination file. |
| `/V` | Verify each write (sets the DOS verify-after-write flag for the run). |
| `/W` | Print "Press any key to begin copying..." and wait before starting. |
| `/Y` | Overwrite existing destination files without prompting. |
| `/-Y` | Prompt before overwriting an existing file (also the default). |

Not implemented: `/C`, `/D`, `/H`, `/K`, `/N`, `/O`, `/T`, `/U`, `/L`, `/Z`.
Unlike real XCOPY, Toka-DOS's XCOPY never asks "(F = file, D = directory)?"
for an ambiguous destination — it infers file-versus-directory from `/S`,
`/E`, or a multi-file wildcard source instead of prompting.

Exit codes: 0 success, 1 no files found, 4 initialization error (bad usage,
out of memory, or a bad path), 5 disk write error.

## MEM

Reports memory usage. Toka-DOS's MEM carries one deliberate behavior change
from stock FreeDOS MEM.

```
MEM [/P] [/FULL] [/DEBUG] [/PAGE] [...]
```

By default, `MEM` prints the usual conventional/upper/extended summary.
Upstream FreeDOS MEM's `/P` is only a prefix match for `/PAGE` (pause after
each screenful) — the per-program size-and-segment listing normally needs
`/FULL` or `/DEBUG` instead. **Toka-DOS divergence:** `MEM /P` pauses *and*
lists every program in memory with its size and position, folding `/FULL`'s
behavior into `/P` so the one switch does what a Toka-DOS user would expect
from the letter P. `/FULL` and `/DEBUG` are unchanged from upstream and
still work on their own.

## ATTRIB

Displays or changes file attributes.

```
ATTRIB { options | [path\][file] | /@[list] }
```

| Option | Effect |
| --- | --- |
| `+H` / `-H` | Set/clear Hidden. |
| `+S` / `-S` | Set/clear System. |
| `+R` / `-R` | Set/clear Read-only. |
| `+A` / `-A` | Set/clear Archive. |
| `/S` | Process files in all directories under the given path. |
| `/D` | Process directory names for wildcard arguments. |
| `/@` | Process the files listed in the given file (or stdin). |

A leading comma before a filename (`,file`) clears all attributes at once —
an undocumented but real behavior carried over from real MS-DOS ATTRIB.

## CHOICE

Prompts for a keypress and returns it as an exit code, for use in batch
files.

```
CHOICE [/B] [/C[:]choices] [/N] [/S] [/T[:]c,nn] [text]
```

| Switch | Effect |
| --- | --- |
| `/B` | Beep when the prompt appears. |
| `/C[:]choices` | The allowed keys (default `yn`). |
| `/N` | Don't print the choice list after the prompt text. |
| `/S` | Case-sensitive matching. |
| `/T[:]c,nn` | Auto-pick key `c` after `nn` seconds if nothing is pressed. |

## MORE

Pages output a screen at a time.

```
command | MORE [/T4]
MORE [/T4] file...
MORE [/T4] < file
```

`/T1` through `/T9` set the tab width (default 4). While paging: Space shows
the next page, N moves to the next file, Q quits.

## FIND

Searches text for a literal string.

```
FIND [/C] [/I] [/N] [/V] "string" [file ...]
```

| Switch | Effect |
| --- | --- |
| `/C` | Print only the count of matching lines. |
| `/I` | Case-insensitive match. |
| `/N` | Show line numbers with each match. |
| `/V` | Print lines that do *not* contain the string. |

Exit codes: 0 if at least one match was found, 1 if none was, 2 on a usage
error.

## DELTREE

Deletes a directory and everything under it.

```
DELTREE [/Y] [/V]
```

`/Y` deletes without the usual per-item Y/N confirmation. `/V` reports item
counts and totals when it finishes. Without `/Y`, DELTREE asks for
confirmation before removing anything — matching real MS-DOS DELTREE rather
than deleting silently.

## LABEL

Creates, changes, or deletes a disk volume label.

```
LABEL [drive:][label] [/?]
```

Run with no label, LABEL prompts for one interactively. Entering an empty
label over an existing one prompts to confirm deleting it.

## MOVE

Moves files, or renames directories.

```
MOVE [/Y | /-Y] source1[,source2[,...]] destination
```

| Switch | Effect |
| --- | --- |
| `/Y` | Overwrite an existing destination file without prompting. |
| `/-Y` | Prompt before overwriting (default, unless `COPYCMD` says otherwise). |
| `/V` | Verify each file as it's written to the destination. |
| `/S` | Treat the source as directory-shaped even without a wildcard, for moving whole trees. (Not listed in MOVE's own usage text, but implemented and working.) |

The `COPYCMD` environment variable, if set to `/Y`, `/N`, or `/-Y`, changes
the default overwrite behavior the same way it does for COPY and XCOPY.

## SORT

Sorts text, line by line, from stdin to stdout.

```
SORT [/R] [/+num] [/A] [/?] [file]
```

| Switch | Effect |
| --- | --- |
| `/R` | Reverse the sort order. |
| `/+num` | Start sorting at column `num` (1-based). |
| `/A` | Sort by raw ASCII order instead of the active country/collation table. |
| `/N` | Force country-aware (NLS) collation — the default even without it. |

## GSWMODE

General Simulation Works's own tool: switches the GSW-586's live CPU speed
class from inside DOS, without rebooting.

```
GSWMODE 286 | 386 | 486 | 586
```

Case-insensitive. Run with no argument, or an argument it doesn't
recognize, GSWMODE prints usage and the *current* mode (read back live) and
changes nothing:

```
Usage: GSWMODE 286|386|486|586
Current mode: <mode>
```

Given a valid mode, it writes the matching code straight to the Lotura
chipset's mode port and confirms:

```
GSWMODE: switched to <mode>.
```

This is a **runtime-only** switch: it never touches CMOS, so the BIOS's
saved boot-time speed (set from the [Tab boot menu](../izarra-3000/user-manual.md#the-tab-boot-menu)
or the [Del setup panel](../izbios/configuration-panel.md)) is unaffected —
your next cold boot still starts at whatever speed you saved there.

## TOKAMOUS

General Simulation Works's PS/2 mouse driver: a terminate-and-stay-resident
program implementing the standard `INT 33h` mouse API (Microsoft Mouse
compatible, plus the CuteMouse wheel extension).

```
TOKAMOUS
```

No arguments — it installs itself and returns to the prompt, or is loaded
from `AUTOEXEC.BAT` with `LH TOKAMOUS` to load high into a
[TOKAEMM](../tokaemm/manual.md) upper memory block when one is free. Once
resident, it prints:

```
Toka-DOS mouse driver installed.
```

and any mouse-aware DOS program talks to it through `INT 33h` from then on
— cursor show/hide, position and button state, motion callbacks, and the
wheel functions software checks for via CuteMouse's `AX=0x11` detection.

## Next

- [Using Toka-DOS](using-toka-dos.md) — the shell, the disk layout, and what
  boots by default.
- [The TOKAEMM manual](../tokaemm/manual.md) — the memory manager these
  commands and the shell run on top of.
