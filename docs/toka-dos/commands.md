# DOS Command Reference

Toka-DOS commands come in two kinds. The **built-in commands** (`DIR`, `COPY`,
`DEL`, and the rest) live inside `COMMAND.COM` and are always available. The
**external commands** are the programs in `C:\DOS` (`XCOPY`, `MEM`, `ATTRIB`,
and so on), each a real file, most carried over from FreeDOS with a Toka-DOS
rebrand and a few written by General Simulation Works. This page lists the
built-ins first, then documents each external command with the switches it
actually implements. Where Toka-DOS diverges from a command's usual behavior,
it says so.

## Built-in commands

These are part of `COMMAND.COM` itself, not separate files on disk. They are
available at the `C:\>` prompt and in batch files even before `PATH` is set or
if `C:\DOS` is missing, because the shell carries them in memory. Add `/?` to
any of them for its full built-in help.

Some have a short and a long spelling that do the same thing: `MD`/`MKDIR`,
`RD`/`RMDIR`, `CD`/`CHDIR`, `DEL`/`ERASE`, `REN`/`RENAME`, and `LH`/`LOADHIGH`.

### Files and directories

| Command | Syntax | What it does |
| --- | --- | --- |
| `COPY` | `COPY [/A\|/B] source [+ src ...] [dest] [/V] [/Y\|/-Y]` | Copy files, or join several into one with `src1+src2`. `/A` ASCII, `/B` binary, `/V` verify, `/Y` overwrite without asking, `/-Y` ask first. |
| `DEL` / `ERASE` | `DEL [path]file [/P] [/V]` | Delete files (wildcards allowed). `/P` confirm each, `/V` list what was deleted. |
| `REN` / `RENAME` | `REN [path]oldname newname` | Rename a file or directory. |
| `TYPE` | `TYPE [path]file` | Print a text file to the screen. |
| `DIR` | `DIR [path][file] [/P] [/W] [/A[:attrs]] [/O[:order]] [/S] [/B] [/L]` | List files. `/P` page, `/W` wide, `/S` recurse, `/B` bare names, `/A` filter by attribute, `/O` sort. Defaults come from the `DIRCMD` variable. |
| `MD` / `MKDIR` | `MD [drive:]path` | Create a directory. |
| `RD` / `RMDIR` | `RD [drive:]path` | Remove an empty directory. |
| `CD` / `CHDIR` | `CD [drive:][path]` | Show or change the current directory; `CD -` returns to the previous one. |
| `CDD` | `CDD [drive:][path]` | Change the current directory and drive together. |
| `TRUENAME` | `TRUENAME [path]` | Show the full, canonical path of a name. |
| `VOL` | `VOL [drive:]` | Show a disk's volume label and serial number. |

### Batch and scripting

| Command | Syntax | What it does |
| --- | --- | --- |
| `ECHO` | `ECHO [ON\|OFF]` / `ECHO message` / `ECHO.` | Print a message, or turn command echo on/off. `ECHO.` prints a blank line. |
| `REM` | `REM [comment]` | A comment line in a batch file or `CONFIG.SYS`. (`TITLE` is accepted as a synonym; DOS has no window title to set.) |
| `IF` | `IF [NOT] ERRORLEVEL n cmd` / `IF [NOT] a==b cmd` / `IF [NOT] EXIST file cmd` | Run `cmd` when a condition holds. `IF /I` compares text case-insensitively. |
| `FOR` | `FOR %v IN (set) DO cmd` | Repeat `cmd` for each item in `set` (write `%%v` in a batch file). |
| `GOTO` | `GOTO label` | Jump to a `:label` line in a batch file. |
| `CALL` | `CALL [path]file [args]` | Run another batch file and return afterward. |
| `SHIFT` | `SHIFT [DOWN]` | Shift the `%1 %2 ...` batch parameters along. |
| `PAUSE` | `PAUSE [message]` | Wait for a keypress ("Press any key to continue..."). |
| `EXIT` | `EXIT` | Leave this shell. The boot shell starts with `/P`, so it ignores `EXIT`. |

### Environment and shell

| Command | Syntax | What it does |
| --- | --- | --- |
| `SET` | `SET [/P] [/C] [var[=value]]` | Show, set, or clear environment variables. `/P` reads the value from the user. `SET var` with no value removes it. |
| `PATH` | `PATH [dir[;...]]` | Show or set the program search path. |
| `PROMPT` | `PROMPT [text]` | Change the prompt (the default is `$P$G`). |
| `ALIAS` | `ALIAS [name[=]string]` | Show, set, or remove command aliases. |
| `VER` | `VER [/R] [/W] [/D] [/C]` | Show the version. `/R` adds kernel details; `/W`, `/D`, `/C` show warranty, redistribution, and contributors. |
| `DATE` | `DATE [/D] [date]` | Show or set the date. `/D` skips the interactive prompt. |
| `TIME` | `TIME [/T] [time]` | Show or set the time. `/T` skips the prompt. |
| `CHCP` | `CHCP [nnn]` | Show or set the active code page. |
| `VERIFY` | `VERIFY [ON\|OFF]` | Turn write-after-verify on or off. |
| `BREAK` | `BREAK [ON\|OFF]` | Turn extended Ctrl+C checking on or off. |
| `CLS` | `CLS` | Clear the screen. |
| `BEEP` | `BEEP` | Beep the speaker. |
| `CTTY` | `CTTY device` | Move console input and output to another device, such as `COM1`. |

### Loading programs high

| Command | Syntax | What it does |
| --- | --- | --- |
| `LH` / `LOADHIGH` | `LH [path]file [args]` | Load a program into an upper memory block. Needs [TOKAEMM](../tokaemm/manual.md) UMBs; falls back to a normal load if none are free. |
| `LOADFIX` | `LOADFIX [path]file [args]` | Load a program above the first 64 KB, for old programs that fail there with "Packed file corrupt". |

### History and the directory stack

| Command | Syntax | What it does |
| --- | --- | --- |
| `DOSKEY` | `DOSKEY` | Command-line recall and editing, built into the shell: Up/Down recall previous lines, Tab completes filenames. |
| `HISTORY` | `HISTORY [size]` | Show the command history, or resize its buffer. |
| `PUSHD` | `PUSHD [path]` | Save the current directory on a stack, optionally changing to `path`. |
| `POPD` | `POPD` | Return to the directory last saved by `PUSHD`. |
| `DIRS` | `DIRS` | Show the directory stack. |

### Help and diagnostics

| Command | Syntax | What it does |
| --- | --- | --- |
| `?` | `?` | List every built-in command. |
| `WHICH` | `WHICH command...` | Show which program a command name would run. |
| `LFNFOR` | `LFNFOR [ON\|OFF]` | Turn long-filename expansion in `FOR` on or off. |
| `MEMORY` | `MEMORY` | Report the shell's own internal memory use. This is not [MEM](#mem), the external memory report. |

## External commands

The rest of this page documents the programs in `C:\DOS`, one per section. They
sit on the `PATH`, so you run them by name from any directory.

## XCOPY

Copies files and directory trees. Toka-DOS's XCOPY is original project code
written to XCOPY's documented behavior, not a ported FreeDOS binary. It
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
for an ambiguous destination. It infers file-versus-directory from `/S`,
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
each screenful). The per-program size-and-segment listing normally needs
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

A leading comma before a filename (`,file`) clears all attributes at once,
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
confirmation before removing anything, matching real MS-DOS DELTREE rather
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
| `/N` | Force country-aware (NLS) collation, the default even without it. |

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
or the [Del setup panel](../izbios/configuration-panel.md)) is unaffected.
Your next cold boot still starts at whatever speed you saved there.

## TOKAMOUS

General Simulation Works's PS/2 mouse driver: a terminate-and-stay-resident
program implementing the standard `INT 33h` mouse API (Microsoft Mouse
compatible, plus the CuteMouse wheel extension).

```
TOKAMOUS
```

No arguments: it installs itself and returns to the prompt, or is loaded
from `AUTOEXEC.BAT` with `LH TOKAMOUS` to load high into a
[TOKAEMM](../tokaemm/manual.md) upper memory block when one is free. Once
resident, it prints:

```
Toka-DOS mouse driver installed.
```

and any mouse-aware DOS program talks to it through `INT 33h` from then on:
cursor show/hide, position and button state, motion callbacks, and the
wheel functions software checks for via CuteMouse's `AX=0x11` detection.

## Next

- [Using Toka-DOS](using-toka-dos.md): the shell, the disk layout, and what
  boots by default.
- [The TOKAEMM manual](../tokaemm/manual.md): the memory manager these
  commands and the shell run on top of.
