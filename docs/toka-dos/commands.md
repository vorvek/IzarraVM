<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# DOS Command Reference

Toka-DOS has two types of command. The **internal commands** (`DIR`, `COPY`,
`DEL`, and the others) are part of `COMMAND.COM`, and are always available.
The **external commands** are the programs in `C:\DOS` (`XCOPY`, `MEM`,
`ATTRIB`, and the others). Each external command is a file. Most of them come
from FreeDOS with a Toka-DOS name, and General Simulation Works wrote some of
them. This page lists the internal commands first. It then describes each
external command and its switches. Where the behavior in Toka-DOS is different
from the usual behavior, this page says so.

## Internal commands

These commands are part of `COMMAND.COM`. They are not files on the disk. The
shell holds them in memory. Thus they are available at the `C:\>` prompt and
in a batch file, even before `PATH` is set, and even if `C:\DOS` is absent.
Add `/?` to a command to see its full help.

Some commands have a short name and a long name with the same function:
`MD`/`MKDIR`, `RD`/`RMDIR`, `CD`/`CHDIR`, `DEL`/`ERASE`, `REN`/`RENAME`, and
`LH`/`LOADHIGH`.

### Files and directories

| Command | Syntax | What it does |
| --- | --- | --- |
| `COPY` | `COPY [/A\|/B] source [+ src ...] [dest] [/V] [/Y\|/-Y]` | Copy files. `src1+src2` joins two files into one. `/A` ASCII, `/B` binary, `/V` verify, `/Y` overwrite with no question, `/-Y` ask first. |
| `DEL` / `ERASE` | `DEL [path]file [/P] [/V]` | Delete files. Wildcards are permitted. `/P` asks about each file. `/V` lists the deleted files. |
| `REN` / `RENAME` | `REN [path]oldname newname` | Change the name of a file or a directory. |
| `TYPE` | `TYPE [path]file` | Print a text file on the screen. |
| `DIR` | `DIR [path][file] [/P] [/W] [/A[:attrs]] [/O[:order]] [/S] [/B] [/L]` | List the files. `/P` one page at a time, `/W` wide, `/S` include the subdirectories, `/B` names only, `/A` select by attribute, `/O` sort. `DIR` puts the directories first and sorts by name, unless you give a different order. `/O:U` gives the order on the disk. The `DIRCMD` variable sets the defaults. |
| `MD` / `MKDIR` | `MD [drive:]path` | Make a directory. |
| `RD` / `RMDIR` | `RD [drive:]path` | Remove an empty directory. |
| `CD` / `CHDIR` | `CD [drive:][path]` | Show or change the current directory. `CD -` returns to the previous directory. |
| `CDD` | `CDD [drive:][path]` | Change the current directory and the current drive together. |
| `TRUENAME` | `TRUENAME [path]` | Show the full path of a name. |
| `VOL` | `VOL [drive:]` | Show the volume label and the serial number of a disk. |

### Batch files and scripts

| Command | Syntax | What it does |
| --- | --- | --- |
| `ECHO` | `ECHO [ON\|OFF]` / `ECHO message` / `ECHO.` | Print a message, or set the command echo to on or off. `ECHO.` prints an empty line. |
| `REM` | `REM [comment]` | A comment line in a batch file or in `CONFIG.SYS`. (`TITLE` is a synonym. DOS has no window title.) |
| `IF` | `IF [NOT] ERRORLEVEL n cmd` / `IF [NOT] a==b cmd` / `IF [NOT] EXIST file cmd` | Run `cmd` when the condition is true. `IF /I` ignores the letter case in a text comparison. |
| `FOR` | `FOR %v IN (set) DO cmd` | Do `cmd` for each item in `set`. Write `%%v` in a batch file. |
| `GOTO` | `GOTO label` | Go to a `:label` line in a batch file. |
| `CALL` | `CALL [path]file [args]` | Run a different batch file, and then return. |
| `SHIFT` | `SHIFT [DOWN]` | Move the `%1 %2 ...` batch parameters along by one. |
| `PAUSE` | `PAUSE [message]` | Wait for a keypress. The message is "Press any key to continue...". |
| `EXIT` | `EXIT` | Leave this shell. The boot shell starts with `/P`, thus it ignores `EXIT`. |

### Environment and shell

| Command | Syntax | What it does |
| --- | --- | --- |
| `SET` | `SET [/P] [/C] [var[=value]]` | Show, set, or clear an environment variable. `/P` reads the value from the user. `SET var` with no value removes the variable. |
| `PATH` | `PATH [dir[;...]]` | Show or set the program search path. |
| `PROMPT` | `PROMPT [text]` | Change the prompt. The default is `$P$G`. |
| `ALIAS` | `ALIAS [name[=]string]` | Show, set, or remove a command alias. |
| `VER` | `VER [/R] [/W] [/D] [/C]` | Show the version. `/R` adds the kernel details. `/W`, `/D`, and `/C` show the warranty, the redistribution terms, and the contributors. |
| `DATE` | `DATE [/D] [date]` | Show or set the date. `/D` does not ask for a value. |
| `TIME` | `TIME [/T] [time]` | Show or set the time. `/T` does not ask for a value. |
| `CHCP` | `CHCP [nnn]` | Show or set the active code page. |
| `VERIFY` | `VERIFY [ON\|OFF]` | Set the verify-after-write function to on or off. |
| `BREAK` | `BREAK [ON\|OFF]` | Set the extended Ctrl+C check to on or off. |
| `CLS` | `CLS` | Clear the screen. |
| `BEEP` | `BEEP` | Sound the speaker. |
| `CTTY` | `CTTY device` | Move the console input and output to a different device, for example `COM1`. |

### How to load a program high

| Command | Syntax | What it does |
| --- | --- | --- |
| `LH` / `LOADHIGH` | `LH [path]file [args]` | Load a program into an upper memory block. It needs [TOKAEMM](../tokaemm/manual.md) UMBs. If no block is free, it does a normal load. |
| `LOADFIX` | `LOADFIX [path]file [args]` | Load a program above the first 64 KB. Use it for an old program that fails there with "Packed file corrupt". |

### History and the directory stack

| Command | Syntax | What it does |
| --- | --- | --- |
| `DOSKEY` | `DOSKEY` | The command recall and edit functions in the shell. Up and Down recall a previous line. Tab completes a file name. |
| `HISTORY` | `HISTORY [size]` | Show the command history, or change the size of its buffer. |
| `PUSHD` | `PUSHD [path]` | Put the current directory on a stack. It can also change to `path`. |
| `POPD` | `POPD` | Return to the directory that `PUSHD` put on the stack last. |
| `DIRS` | `DIRS` | Show the directory stack. |

### Help and diagnostics

| Command | Syntax | What it does |
| --- | --- | --- |
| `?` | `?` | List each internal command. |
| `WHICH` | `WHICH command...` | Show the program that a command name runs. |
| `LFNFOR` | `LFNFOR [ON\|OFF]` | Set the long-filename expansion in `FOR` to on or off. |
| `MEMORY` | `MEMORY` | Report the internal memory use of the shell. This is not [MEM](#mem), the external memory report. |

## External commands

The remainder of this page describes the programs in `C:\DOS`, one in each
section. They are on the `PATH`. Thus you can run them by name from any
directory.

## XCOPY

XCOPY copies files and directory trees. The Toka-DOS XCOPY is project code,
and not a FreeDOS binary. Its behavior agrees with the documented behavior of
XCOPY. It has fewer switches than the MS-DOS XCOPY.

```
XCOPY source [destination] [/S] [/E] [/P] [/V] [/W] [/Y] [/-Y]
```

| Switch | Effect |
| --- | --- |
| `/S` | Copy the directories and the subdirectories, but not the empty ones. |
| `/E` | Copy the subdirectories, including the empty ones. This includes `/S`. |
| `/P` | Ask `<file> (Y/N)?` before it makes each destination file. |
| `/V` | Verify each write. It sets the DOS verify-after-write flag for the run. |
| `/W` | Print "Press any key to begin copying..." and wait before the copy. |
| `/Y` | Overwrite an existing destination file with no question. |
| `/-Y` | Ask before it overwrites an existing file. This is also the default. |

These switches are not available: `/C`, `/D`, `/H`, `/K`, `/N`, `/O`, `/T`,
`/U`, `/L`, `/Z`.

The MS-DOS XCOPY asks "(F = file, D = directory)?" when the type of the
destination is not clear. The Toka-DOS XCOPY does not ask. It uses `/S`, `/E`,
or a wildcard source with more than one file to select between a file and a
directory.

Exit codes: 0 success, 1 no file found, 4 initialization error (incorrect
usage, insufficient memory, or an incorrect path), 5 disk write error.

## MEM

MEM reports the memory use. Toka-DOS adds a category display. It also changes
the function of `/P` from the FreeDOS MEM.

```
MEM [/P] [/FULL] [/DEBUG] [/PAGE] [...]
```

By default, `MEM` shows a memory map of four lines and 79 columns after the
numeric summary. Conventional memory, upper memory, and extended memory come
one after the other, in light blue, light cyan, and light green. In each
colored range, `▓` (CP437 `B2`) is memory in use, and `░` (CP437 `B0`) is free
memory.

On the standard 64 MiB Izarra3000, the map gives 3 cells to conventional
memory, 2 cells to upper memory, and 311 cells to extended memory. Each of the
316 cells is approximately 207 KiB. The summary shows 640 KiB of conventional
memory, the full 384 KiB upper region, and 64,512 KiB in the row with the name
`Extended (XMS)`. The upper category covers the full address region from
`A0000` to `FFFFF`, with the video memory and the ROMs. With its default EMS
frame, TOKAEMM can allocate 96 KiB there. Under `NOEMS`, the UMB space
increases to 160 KiB.

There is no separate EMS row, and there is no EMS partition. XMS blocks, VCPI
pages, and EMS pages all come from one extended pool. Thus the
`Extended (XMS)` row has a star, and a footnote says that the manager
simulates EMS from XMS as necessary. MS-DOS 6.22 with EMM386 uses the same
convention.

In the FreeDOS MEM, `/P` is only a prefix of `/PAGE`, which stops after each
screen. That MEM needs `/FULL` or `/DEBUG` for the size and the segment of
each program.

In Toka-DOS, `MEM /P` shows each program in memory, with its size and its
segment, one screen at a time. The headings `Conventional Memory Detail` and
`Upper Memory Detail` show the position of each block. `/P` does not print the
default summary, so that the program table stays on the screen at the end. Use
`MEM /P /SUMMARY` to add the numeric summary and the memory map. `/FULL` and
`/DEBUG` continue to operate alone.

## ATTRIB

ATTRIB shows or changes the file attributes.

```
ATTRIB { options | [path\][file] | /@[list] }
```

| Option | Effect |
| --- | --- |
| `+H` / `-H` | Set or clear the Hidden attribute. |
| `+S` / `-S` | Set or clear the System attribute. |
| `+R` / `-R` | Set or clear the Read-only attribute. |
| `+A` / `-A` | Set or clear the Archive attribute. |
| `/S` | Process the files in all of the directories below the given path. |
| `/D` | Process the directory names for a wildcard argument. |
| `/@` | Process the files in the given list file, or in the standard input. |

A comma before a file name (`,file`) clears all of the attributes together.
The MS-DOS ATTRIB has the same behavior, although its documentation does not
give it.

## CHOICE

CHOICE asks for a keypress and returns it as an exit code. Use it in a batch
file.

```
CHOICE [/B] [/C[:]choices] [/N] [/S] [/T[:]c,nn] [text]
```

| Switch | Effect |
| --- | --- |
| `/B` | Sound the speaker when the prompt appears. |
| `/C[:]choices` | The permitted keys. The default is `yn`. |
| `/N` | Do not print the list of choices after the prompt text. |
| `/S` | Obey the letter case in the comparison. |
| `/T[:]c,nn` | Select key `c` automatically after `nn` seconds, if the user presses no key. |

## MORE

MORE shows text one screen at a time.

```
command | MORE [/T4]
MORE [/T4] file...
MORE [/T4] < file
```

`/T1` to `/T9` set the tab width. The default is 4. Space shows the next
screen. N moves to the next file. Q stops the program.

## FIND

FIND looks for an exact string in text.

```
FIND [/C] [/I] [/N] [/V] "string" [file ...]
```

| Switch | Effect |
| --- | --- |
| `/C` | Print only the number of lines with a match. |
| `/I` | Ignore the letter case. |
| `/N` | Show the line number with each match. |
| `/V` | Print the lines that do not contain the string. |

Exit codes: 0 if FIND found one match or more, 1 if it found no match, and 2
for a usage error.

## DELTREE

DELTREE deletes a directory and all of its contents.

```
DELTREE [/Y] [/V]
```

**Warning:** DELTREE deletes the files permanently. You cannot recover them.

Without `/Y`, DELTREE asks Y/N for each item before it deletes anything. The
MS-DOS DELTREE does the same. `/Y` deletes without the questions. `/V` reports
the item counts and the totals at the end.

## LABEL

LABEL makes, changes, or deletes the volume label of a disk.

```
LABEL [drive:][label] [/?]
```

If you give no label, LABEL asks for one. If you then give an empty label,
LABEL asks you to confirm the deletion of the existing label.

## MOVE

MOVE moves a file, or changes the name of a directory.

```
MOVE [/Y | /-Y] source1[,source2[,...]] destination
```

| Switch | Effect |
| --- | --- |
| `/Y` | Overwrite an existing destination file with no question. |
| `/-Y` | Ask before it overwrites a file. This is the default, unless `COPYCMD` gives a different value. |
| `/V` | Verify each file as MOVE writes it to the destination. |
| `/S` | Use the source as a directory, without a wildcard. Use it to move a full tree. The usage text of MOVE does not list this switch, but the switch operates. |

The `COPYCMD` environment variable can hold `/Y`, `/N`, or `/-Y`. It then
changes the default overwrite behavior, as it does for COPY and XCOPY.

## SORT

SORT sorts text line by line, from the standard input to the standard output.

```
SORT [/R] [/+num] [/A] [/?] [file]
```

| Switch | Effect |
| --- | --- |
| `/R` | Reverse the sort order. |
| `/+num` | Start the sort at column `num`. The first column is 1. |
| `/A` | Sort in ASCII order, and not with the active country table. |
| `/N` | Sort with the country-aware (NLS) collation. This is the default. |

## GSWMODE

GSWMODE is a General Simulation Works tool. It changes the CPU speed class of
the GSW-586 from inside DOS. A restart is not necessary.

```
GSWMODE 386-slow | 386 | 486 | 586 [/T]
```

The letter case of a mode name is not important. With a correct mode, GSWMODE
writes the related code to the mode port of the Lotura chipset. It saves the
code in CMOS. It then shows:

```
GSWMODE: switched to <mode>, saved.
```

The speed stays after a restart, as if you set it in the
[Del setup panel](../izbios/configuration-panel.md). Add `/T` to change the
speed for this session only. `/T` does not change the saved speed:

```
GSWMODE 386-slow /T
GSWMODE: switched to 386-slow for this session only.
```

Use `/T` to run one program at a different speed. Without `/T`, GSWMODE saves
the speed, as the machine saves its other settings.

With no argument, or with an unknown argument, GSWMODE prints the usage text
and both speeds. It changes nothing. The two values are different only after a
`/T`:

```
Usage: GSWMODE 386-slow|386|486|586 [/T]
  The speed is saved and survives a reboot; /T applies it
  for this session only.
Current mode: 386-slow
Saved mode:   486
```

GSWMODE refuses the old `286` name. It shows this message:

```
CPU mode '286' was removed; use '386-slow'.
```

The [Tab boot menu](../izarra-3000/user-manual.md#the-tab-boot-menu) and the
Del setup panel write the same CMOS byte. Thus all three controls agree about
the speed at the next start.

## UNHALT

UNHALT is a General Simulation Works tool. It makes the BIOS keyboard wait use
a loop, and not a halt of the CPU.

```
UNHALT      use a loop while it waits for a key
UNHALT /H   halt while it waits (the default)
UNHALT /?   usage
```

A program can ask the BIOS for a keystroke (INT 16h) when no keystroke is
available. The IzarraVM BIOS then halts the CPU until the next interrupt. It
does not loop on the keyboard buffer. A keypress raises IRQ1 and starts the
CPU immediately, thus the response is not slower. But the emulator does not
interpret a loop that does no work.

A BIOS of the period could do either of these two things, thus both are
correct. A program can detect the difference in two conditions:

- The program masks the timer interrupt and the keyboard interrupt, and then
  waits for a key. No key can arrive, thus the program stops in both
  conditions. But a loop continues to read the keyboard buffer, and a halt has
  no interrupt to start the CPU again.
- The program expects the time to increase smoothly during the wait, and not
  in steps of approximately 1/18 second.

These two conditions are not frequent. If a program operates incorrectly while
it waits for input, run `UNHALT` first:

```
UNHALT
MYGAME
```

`UNHALT /H` sets the halt again, without a restart. The machine does not save
this setting, and each reset starts with the halt. Put `UNHALT` in
`AUTOEXEC.BAT` if a program needs the loop at each boot.

UNHALT changes the BIOS keyboard wait only. Toka-DOS does its own halt while
DOS waits for input. To stop that halt, put `IDLEHALT=0` in `CONFIG.SYS` and
restart the machine.

## SNDCTRL

SNDCTRL is the setup utility of the [ReSonique II](../resonique2/manual.md)
sound card. It changes the IRQ and the DMA of the card from inside DOS.

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

With no argument, SNDCTRL draws a configuration screen. The arrow keys or Tab
move between the values. Enter opens the list of values that the resource
supports. F10 applies the values, and Esc cancels. A `*` marks a value that
does not apply to that device. With a switch on the command line, SNDCTRL
applies the switch and draws nothing.

`/B` prints a two-row boot summary: a heading, and then a line of values for
both devices in the BLASTER style. It then exits. It does not write the mixer,
CMOS, the environment, or `AUTOEXEC.BAT`. `/T` puts the tree connector of the
Toka-DOS boot screen in front of that summary. `/T` has an effect only with
`/B`. `SNDCTRL /T` alone opens the configuration screen.

A change moves both devices immediately, without a restart. SNDCTRL then saves
the assignment in CMOS, changes `BLASTER` in the current environment, and
writes the `SET BLASTER` line in `C:\AUTOEXEC.BAT` again. The full-screen
display and the command line do the same operations.

The Sound Blaster and the Windows Sound System codec cannot share an IRQ line
or a DMA channel. The full-screen lists do not show the resources that the
other device holds. The command line refuses such a combination, and writes
nothing.

The usual reason to run this utility is a game with a fixed IRQ in its code,
which does not read `BLASTER`. The card uses IRQ 7 because almost all such
games expect IRQ 7. But some of them need IRQ 5. See
[Why the Sound Blaster uses IRQ 7](../resonique2/manual.md#why-the-sound-blaster-uses-irq-7).

## SNDMIXER

SNDMIXER sets the volume levels on the mixer of the
[ReSonique II](../resonique2/manual.md) card. The full-screen display has seven
vertical faders, one for each source.

```
SNDMIXER                full-screen mixer
SNDMIXER /L             list the current levels
SNDMIXER /CFG file      restore the levels saved in a file
SNDMIXER /M n           MASTER      0 (mute) to 10 (full)
SNDMIXER /F n           FMSYNTH     OPL3 music
SNDMIXER /W n           WAVE        SB16 DSP and WSS codec
SNDMIXER /C n           CD-ROM      Red Book audio
SNDMIXER /I n           MIDI        wavetable synthesis
SNDMIXER /P n           PC speaker  four positions: 0 3 7 10
SNDMIXER /A n           AMP         output gain, same four positions
SNDMIXER /S             suppress all output
SNDMIXER /?             usage
```

Tab and the arrow keys select a fader. Up and Down move the fader. Home and
End move it to the top and to the bottom of its travel. The digit keys set a
level directly. On the six faders that decrease the level, the bottom is a
mute. On `AMP`, which increases the level, the bottom is 0 dB, which is no
gain. SNDMIXER writes each level to the hardware immediately.

Two buttons are at the bottom of the box, after the seven faders in the same
Tab sequence:

- **Accept** closes the mixer and keeps the new levels. It prints
  `Settings applied.`
- **Cancel** sets the levels that were in effect when the mixer opened.
  **Esc** does the same.
- **Enter** or **Space** operates the selected button. It does nothing when a
  fader is selected.
- **F10** saves the levels to `C:\VOLCONF.CFG` and exits. It is the only key
  that writes a file.

Each step is 4 dB. The volume registers of the card have 2 dB for each step,
over a range of 62 dB. Ten fader positions at equal distances in the register
range would put seven of them in the top 12 dB. Ten steps of 4 dB give ten
positions that a listener can hear apart. Position 10 is the power-on level of
the card, and position 0 is a mute.

The PC-speaker fader has four positions, and not ten, because the card gives
that input two bits and not five. A value between two positions goes up to the
next position. Thus a low setting does not become silence.

`AMP` is the output gain of the card (mixer registers `0x41` and `0x42`). It
is the one fader that adds level. It has the same four positions as the PC
speaker, because that register is also two bits wide. But its positions
increase the level: step 0 is 0 dB, step 3 is +6 dB, step 7 is +12 dB, and
step 10 is +18 dB. Step 0 is the power-on setting. It is not a mute. It is the
amplifier with no gain. A value between two positions goes up, as it does on
the PC speaker.

**Warning:** gain above 0 dB can clip the signal. The mix keeps 6 dB of
headroom after the mixer, which is sufficient for one leg at full scale. Step
3 uses that headroom. Steps 7 and 10 are more than the headroom, and a loud
source will distort. The card has these positions, thus the fader has them.
To make a quiet game louder, use `AMP` after `MASTER` is at 10.

A MIDI level of 0 writes the wavetable mute bit. It does not write a level of
zero. On the wavetable, the card makes a level of zero the quietest audible
step, and not silence. No other control in the machine reaches that leg.
Without this behavior, a program that cleared the mixer registers would make
MIDI silent for the remainder of the session, with no cause on the screen.

`WAVE` sets the two digital-audio paths together: the Sound Blaster DSP and
the Windows Sound System codec. No program uses both at the same time, and
they carry the same class of audio.

`/CFG` saves the levels to a file. The default `AUTOEXEC.BAT` sets them again
at the next boot:

```
SNDMIXER /CFG C:\VOLCONF.CFG /S
```

`/CFG` alone reads the file and writes to the card. `/CFG` with a channel
switch applies the switches, and then writes them into the file. `/S` stops
all output, which keeps the boot screen clear. `/S` can be at any position on
the command line.

F10 in the full-screen mixer always saves to `C:\VOLCONF.CFG`, which is the
file that the boot line reads. `/CFG` is the boot-restore form, and it does
not open the mixer. To keep the levels in a different file, use the
command-line form: `SNDMIXER /M 8 /F 6 /CFG C:\GAMES\QUIET.CFG`.

The file is plain text. You can read it with `TYPE`, and you can change it
with the `EDIT` editor. Each line has the form `CHANNEL=step`. The channel is
`MASTER`, `FMSYNTH`, `WAVE`, `CD`, `MIDI`, `SPEAKER`, or `AMP`. Spaces around
the `=` are permitted. A `;` or a `#` starts a comment. The parser ignores a
line that it does not recognize, and does not refuse the file. A channel that
the file does not name keeps its current level.

The default file is in the root of `C:`, and not in `C:\DOS`, because `C:` is
not always the Toka-DOS image. If you give IzarraVM a folder of games, that
folder becomes `C:` and has no `DOS` directory. A save into a directory that
does not exist fails. The root of a mounted drive always exists.

## TOKAMOUS

TOKAMOUS is the General Simulation Works PS/2 mouse driver. It is a
terminate-and-stay-resident (TSR) program. It supplies the standard `INT 33h`
mouse interface, which is Microsoft Mouse compatible. It also supplies the
CuteMouse wheel extension.

```
TOKAMOUS [/T]
```

The driver installs itself and returns to the prompt. `AUTOEXEC.BAT` loads it
with `LH TOKAMOUS`, which puts it in a [TOKAEMM](../tokaemm/manual.md) upper
memory block when one is free. After the installation, it prints:

```
Toka-DOS mouse driver installed.
```

A DOS program then uses `INT 33h` to reach the driver. The functions include
show and hide of the cursor, the position and the button state, the motion
callbacks, and the wheel functions. Software finds the wheel functions with
the CuteMouse `AX=0x11` detection call.

The driver draws the cursor in the 80-column text mode and in the 320x200
256-colour VGA mode (mode 13h). A program changes the cursor shape with
`AX=0x09`. In the other graphics modes the program draws its own cursor. A
video mode change hides the cursor until the program shows it again.

`/T` (or `-T`) puts the tree connector of the Toka-DOS boot screen in front of
the sign-on line:

```
├─> Toka-DOS mouse driver installed.
```

It is off by default.

## Next

- [How to use Toka-DOS](using-toka-dos.md): the shell, the disk layout, and
  the default boot.
- [The TOKAEMM manual](../tokaemm/manual.md): the memory manager below these
  commands and the shell.
- [Toka-DOS licensing](licensing.md): the commands that come from FreeDOS, and
  their license terms.
