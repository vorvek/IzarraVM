<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

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
| `DIR` | `DIR [path][file] [/P] [/W] [/A[:attrs]] [/O[:order]] [/S] [/B] [/L]` | List files. `/P` page, `/W` wide, `/S` recurse, `/B` bare names, `/A` filter by attribute, `/O` sort. Listings are sorted by name with directories first unless you say otherwise; `/O:U` gives the raw on-disk order. Defaults come from the `DIRCMD` variable. |
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

Reports memory usage. Toka-DOS adds a category display and changes how `/P`
works compared with stock FreeDOS MEM.

```
MEM [/P] [/FULL] [/DEBUG] [/PAGE] [...]
```

By default, `MEM` shows a four-line, 79-column memory map after its numeric
summary. Conventional, upper, and extended memory appear consecutively in
light blue, light cyan, and light green. Within each colored range, `▓`
(CP437 `B2`) marks memory in use and `░` (CP437 `B0`) marks free memory.

On the standard 64 MiB Izarra 3000, the map gives conventional memory 3
cells, upper memory 2, and extended memory 311. Each of the 316 cells
represents about 207 KiB. The exact summary categories are 640 KiB
conventional memory, the full 384 KiB upper region, and 64,512 KiB in the row
labelled `Extended (XMS)`. The upper category covers the whole `A0000` to
`FFFFF` address region, including video memory and ROMs. TOKAEMM can allocate
96 KiB there with its default EMS frame; under `NOEMS` the allocatable UMB
space grows to 160 KiB.

There is no separate EMS row or partition: XMS blocks, VCPI pages, and EMS
pages all draw from one shared extended pool, so the `Extended (XMS)` row is
starred and a footnote explains that EMS is simulated from XMS as required,
the same convention MS-DOS 6.22 uses with EMM386.

Upstream FreeDOS MEM's `/P` is only a prefix match for `/PAGE`, which pauses
after each screenful. The per-program size and segment listing normally needs
`/FULL` or `/DEBUG`. In Toka-DOS, `MEM /P` pages through every program in
memory with its size and segment. Separate `Conventional Memory Detail` and
`Upper Memory Detail` headings show where each block resides. `/P` leaves out
the default summary so the program table remains visible at the end. Use
`MEM /P /SUMMARY` to append the numeric summary and memory map. `/FULL` and
`/DEBUG` still work on their own.

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

General Simulation Works's own tool: switches the GSW-586's CPU speed class
from inside DOS, without rebooting.

```
GSWMODE 386-slow | 386 | 486 | 586 [/T]
```

Mode names are case-insensitive. Given a valid mode, GSWMODE writes the
matching code to the Lotura chipset's mode port, saves it in CMOS, and
confirms:

```
GSWMODE: switched to <mode>, saved.
```

The speed then **survives a reboot**, exactly as though you had set it in the
[Del setup panel](../izbios/configuration-panel.md). Add `/T` to change the
speed for this session only and leave the saved one alone:

```
GSWMODE 386-slow /T
GSWMODE: switched to 386-slow for this session only.
```

That is the right switch for running one program slower without committing to
it. Everything else about the machine's setup is remembered, so the speed is
too unless you say otherwise.

With no argument or an unrecognized one, GSWMODE prints usage and both speeds,
and changes nothing. The two differ after a `/T`, which is the only time the
distinction matters and exactly when you would want to see it:

```
Usage: GSWMODE 386-slow|386|486|586 [/T]
  The speed is saved and survives a reboot; /T applies it
  for this session only.
Current mode: 386-slow
Saved mode:   486
```

The retired `286` name is rejected with a migration hint:

```
CPU mode '286' was removed; use '386-slow'.
```

The [Tab boot menu](../izarra-3000/user-manual.md#the-tab-boot-menu) and the
Del setup panel write the same CMOS byte, so all three agree about what the
machine will start at next time.

## UNHALT

General Simulation Works's own tool: makes the BIOS keyboard wait spin instead
of halting the CPU.

```
UNHALT      spin while waiting for a key
UNHALT /H   halt while waiting (the default)
UNHALT /?   usage
```

When a program asks the BIOS for a keystroke (INT 16h) and none is waiting,
IzarraVM's BIOS halts the CPU until the next interrupt instead of spinning on
the keyboard buffer. Pressing a key raises IRQ1 and wakes it immediately, so
nothing responds any slower, but the emulator stops interpreting a busy loop
that does nothing.

BIOSes of the era did this both ways, so neither behaviour is unfaithful. A
program can only tell the difference in two situations:

- It masks the timer and keyboard interrupts and *then* waits for a key. A spin
  loops forever; a halt has nothing left to wake it. Both are hung, since no key
  can ever arrive, but they hang differently.
- It expects time to pass smoothly across the wait rather than in steps of about
  1/18 second.

Neither is common. If a program misbehaves while waiting for input, run `UNHALT`
before it:

```
UNHALT
MYGAME
```

`UNHALT /H` restores halting without a reboot. The setting is not saved, so
every reset starts out halting; put `UNHALT` in `AUTOEXEC.BAT` if a program
needs it every time.

This covers the **BIOS** keyboard wait only. Toka-DOS separately halts while DOS
itself is waiting for input; to turn that off, put `IDLEHALT=0` in `CONFIG.SYS`
and reboot.

## SNDCTRL

The [ReSonique 2](../resonique2/manual.md) sound card's own setup utility:
moves the card's IRQ and DMA assignment from inside DOS, the way you would have
run a real sound card's configuration program.

```
SNDCTRL                 full-screen configuration
SNDCTRL /S              show the current assignment and exit
SNDCTRL /B              two-row boot summary and exit (no CMOS writes)
SNDCTRL /B /T           boot summary with a tree-styled prefix
SNDCTRL /SBIRQ:n        Sound Blaster IRQ         2, 5, 7, 10
SNDCTRL /SBDMAL:n       Sound Blaster 8-bit DMA   0, 1, 3
SNDCTRL /SBDMAH:n       Sound Blaster 16-bit DMA  5, 6, 7
SNDCTRL /WSSIRQ:n       Windows Sound System IRQ  7, 9, 10, 11
SNDCTRL /WSSDMA:n       Windows Sound System DMA  0, 1, 3
SNDCTRL /MPU:nnn        MPU-401 port              300, 330
SNDCTRL /?              usage
```

With no arguments it draws a configuration screen. Arrow keys or Tab move
between values, Enter opens the list of values that resource supports, F10
applies, Esc cancels. A `*` marks a value that does not apply to that device.
Any switch on the command line is applied without drawing anything.

`/B` prints a two-row boot summary — a heading, then a BLASTER-style values
line for both devices — and exits without touching the mixer, CMOS, the
environment, or `AUTOEXEC.BAT`. `/T` adds the tree-styled connector used by
the Toka-DOS boot screen in front of that summary; it only means something
paired with `/B`, so `SNDCTRL /T` alone just opens the configurator.

Whichever way you set them, applying moves both devices **live** — neither
needs a reboot — then saves the assignment in CMOS, updates `BLASTER` in the
current environment, and rewrites the `SET BLASTER` line in `C:\AUTOEXEC.BAT`.

The Sound Blaster and the Windows Sound System codec cannot share an IRQ line
or a DMA channel. The full-screen lists simply omit whatever the other device
holds; the command line refuses the combination and writes nothing.

Most people need this for one reason: a game that hardwires an IRQ instead of
reading `BLASTER`. The card ships on IRQ 7 because that is what such games
almost always assume, but a few want IRQ 5. See
[Why the Sound Blaster sits on IRQ 7](../resonique2/manual.md#why-the-sound-blaster-sits-on-irq-7).

## SNDMIXER

The [ReSonique 2](../resonique2/manual.md) card's volume mixer: six vertical
faders, one per source. `SNDCTRL` decides where the card lives; this decides how
loud it is.

```
SNDMIXER                full-screen mixer
SNDMIXER /L             list the current levels and exit
SNDMIXER /CFG file      restore the levels saved in a file
SNDMIXER /M n           MASTER      0 (mute) to 10 (full)
SNDMIXER /F n           FMSYNTH     OPL3 music
SNDMIXER /W n           WAVE        SB16 DSP and WSS codec
SNDMIXER /C n           CD-ROM      Red Book audio
SNDMIXER /I n           MIDI        wavetable synthesis
SNDMIXER /P n           PC speaker  four positions: 0, 3, 7, 10
SNDMIXER /S             say nothing at all
SNDMIXER /?             usage
```

Left and Right pick a fader, Up and Down move it, Home and End go to full and
to mute, the digit keys jump straight to a level. A level takes effect **as you
set it**, so you can hear what you are doing; F10 saves and leaves, Esc puts
back the levels the mixer opened on.

Each step is 4 dB. That is deliberate: the card's own volume registers are
2 dB per step over a 62 dB range, so spreading ten fader positions evenly over
the *numbers* would put seven of them inside the top 12 dB, where they all sound
the same. Ten even 4 dB steps gives ten positions you can tell apart, with 10
being the level the card powers on at and 0 a real mute.

The PC-speaker fader has four positions rather than ten, because the card gives
that input two bits and not five. A value between them rounds up to the next
one, so asking for a little never gets you silence.

`WAVE` moves both digital-audio paths at once — the Sound Blaster DSP and the
Windows Sound System codec — since no program uses both and to a listener they
are the same thing.

Levels are saved to a file with `/CFG`, and the default `AUTOEXEC.BAT` restores
them on the next boot:

```
SNDMIXER /CFG C:\VOLCONF.CFG /S
```

`/CFG` on its own reads the file and writes the card. `/CFG` together with any
channel switch does the opposite: it applies the switches and then writes them
into the file. `/S` prints nothing at all, which is what keeps the boot screen
clean. F10 in the full-screen mixer saves to whatever `/CFG` named, or to
`C:\VOLCONF.CFG` when it named nothing — the same file the boot line reads.

The default sits in the root of `C:` rather than in `C:\DOS`, because `C:` is
not always the Toka-DOS image: point IzarraVM at a folder of games and that
folder becomes `C:`, with no `DOS` directory in it. A save into a directory
that is not there fails; the root of a mounted drive is always there.

## TOKAMOUS

General Simulation Works's PS/2 mouse driver: a terminate-and-stay-resident
program implementing the standard `INT 33h` mouse API (Microsoft Mouse
compatible, plus the CuteMouse wheel extension).

```
TOKAMOUS [/T]
```

It installs itself and returns to the prompt, or is loaded from
`AUTOEXEC.BAT` with `LH TOKAMOUS` to load high into a
[TOKAEMM](../tokaemm/manual.md) upper memory block when one is free. Once
resident, it prints:

```
Toka-DOS mouse driver installed.
```

and any mouse-aware DOS program talks to it through `INT 33h` from then on:
cursor show/hide, position and button state, motion callbacks, and the
wheel functions software checks for via CuteMouse's `AX=0x11` detection.

`/T` (or `-T`) prefixes the signon line with the tree-styled connector used
by the Toka-DOS boot screen instead:

```
├─> Toka-DOS mouse driver installed.
```

Off by default.

## Next

- [Using Toka-DOS](using-toka-dos.md): the shell, the disk layout, and what
  boots by default.
- [The TOKAEMM manual](../tokaemm/manual.md): the memory manager these
  commands and the shell run on top of.
