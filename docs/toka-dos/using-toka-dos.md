<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Using Toka-DOS

Toka-DOS is the operating system bundled with the Izarra 3000. It is a
FreeDOS-based DOS, assembled and branded by General Simulation Works, held in
the machine's system ROM and mounted onto the hard disk at power-on. This page
covers the disk layout, the shell, and the two startup files.

## What's on the disk

The root of C: holds only the files DOS requires there, plus a `DOS` directory
containing the command interpreter and every tool.

```
C:\
    DOS\            AUTOEXEC.BAT    CONFIG.SYS      LICENSE.TXT
```

`KERNEL.SYS` is in the root as well, because the boot sector loads it by name,
but it carries the hidden and system attributes, so a plain `DIR` of C:\ lists
only the `DOS` directory, `AUTOEXEC.BAT`, `CONFIG.SYS`, and `LICENSE.TXT`, in
that order: `DIR` sorts by name and groups directories first. `C:\LICENSE.TXT`
holds the terms Toka-DOS is distributed under; see
[Toka-DOS licensing](licensing.md).

Every program you run is in `C:\DOS`:

```
C:\DOS\
    COMMAND.COM     GSWMODE.COM     MOVE.EXE        MORE.EXE
    TOKAMOUS.COM    SNDCTRL.COM     SORT.EXE        FIND.EXE
    TOKAEMM.SYS     SNDMIXER.COM    MEM.EXE         LABEL.EXE
    TOKACD.SYS      UNHALT.COM      ATTRIB.EXE      DELTREE.COM
    IZCDEX.COM                      CHOICE.EXE      XCOPY.EXE
                                    EDIT.COM        HELLO.TXT
```

`AUTOEXEC.BAT` puts `C:\DOS` on the `PATH`, so every tool runs from any
directory by name. See the [DOS command reference](commands.md) for what each
one does.

One more file can appear in the root: `C:\VOLCONF.CFG`, written the first time
mixer levels are saved with `SNDMIXER`. It is not part of the shipped system.
See [SNDMIXER](commands.md#sndmixer).

## The system ROM

Toka-DOS is not stored on the hard disk. It is held in the Izarra 3000's system
ROM and mounted onto C: at power-on, so the operating system comes up the same
on every boot regardless of what the last program did to the disk. The hidden
`KERNEL.SYS`, the shell, and everything under `C:\DOS` are mounted read-only
and cannot be deleted or overwritten from DOS. General Simulation Works built
the ROM to be reflashed for updates, but no update ever shipped.

`CONFIG.SYS` and `AUTOEXEC.BAT` are the two exceptions. The machine writes
editable copies of them to C: the first time it boots and then leaves them
alone, so the startup configuration can be changed while the system files
underneath it stay as they are. [Repair Toka-DOS](#repair-toka-dos) resets
those two files to the ROM defaults.

## Drive letters

| Drive | What it is |
| --- | --- |
| **A:** | The floppy drive. Mount a `.img`/`.ima`/`.flp` image from the IzarraVM GUI. |
| **C:** | The hard disk. In IzarraVM, a real host folder presented as a FAT disk. |
| **D:** | The ATAPI CD-ROM. Mount an ISO, a CUE/BIN pair, or a host folder as a data disc from the GUI. |

`CONFIG.SYS` sets `LASTDRIVE=D`, which covers exactly these three drives with
nothing spare. See the [IzarraVM GUI guide](../izarravm-gui/guide.md) for how
to point C: at a folder and mount removable media.

## The shell

Toka-DOS's shell is FreeCOM, FreeDOS's COMMAND.COM. Booting ends at:

```
C:\>
```

set by `PROMPT $P$G` in `AUTOEXEC.BAT`. The standard FreeCOM facilities are
available:

- **DIR**, **COPY**, **DEL**, **REN**, and the rest of the built-in command
  set.
- **Batch files**: `.BAT` scripts with the usual `%1`-style parameters,
  `IF`/`GOTO`/`FOR`, and `CALL`.
- **Redirection and pipes**: `>`, `>>`, `<`, and `|`, which is what makes
  `MORE` and `FIND` useful as filters (see the
  [command reference](commands.md)).
- **Command history and editing**: the usual FreeCOM line-editing keys recall
  and edit previous commands at the prompt.

## CONFIG.SYS

The kernel reads `CONFIG.SYS` from the root of the boot drive before the shell
starts. The stock file is:

```
FILES=40
LASTDRIVE=D
DEVICE=C:\DOS\TOKAEMM.SYS RAM /T
DOS=HIGH,UMB
DEVICEHIGH=C:\DOS\TOKACD.SYS
SHELL=C:\DOS\COMMAND.COM C:\DOS /E:2048 /P=C:\AUTOEXEC.BAT
```

`TOKAEMM.SYS` is the memory manager. `RAM` gives both EMS and upper memory
blocks; `/T` is the tree-styled boot line the driver prints instead of its
full banner. `DOS=HIGH,UMB` moves the kernel into the HMA and links the upper
memory blocks into the DOS chain, which is what makes `DEVICEHIGH` and `LH`
work. `TOKACD.SYS` is the ATAPI CD-ROM device driver; the redirector that
assigns it a drive letter runs later, from `AUTOEXEC.BAT`. The `SHELL=` line
starts FreeCOM with a 2048-byte environment, `C:\DOS` as the directory
`COMSPEC` is built from, and `/P=` naming the startup batch file.

See the [TOKAEMM manual](../tokaemm/manual.md) for the memory manager's
switches.

## AUTOEXEC.BAT

The stock startup file is a batch file that calls itself once per driver:

```
@ECHO OFF
IF NOT "%1"=="" GOTO %1
PROMPT $P$G
PATH C:\DOS
SET BLASTER=A220 I7 D1 H5 P300 T6
FOR %%C IN (CDROM MOUSE SOUND) DO CALL C:\AUTOEXEC.BAT %%C
ECHO ╞════════════════════════════════════════════════════════════════════════════╕
ECHO │   Starting in text mode. Run TOKADESK to enable the visual workbench.      │
ECHO ╘════════════════════════════════════════════════════════════════════════════╛
GOTO END
:CDROM
IZCDEX /I /D:TOKACD01 /L:D /T
GOTO END
:MOUSE
LH TOKAMOUS /T
GOTO END
:SOUND
SNDCTRL /B /T
SNDMIXER /CFG C:\VOLCONF.CFG /S
GOTO END
:END
```

The first run has no parameter, so `IF NOT "%1"==""` falls through and the file
sets up the environment: `PROMPT`, `PATH`, and `BLASTER`. The `FOR` loop then
calls the same file three more times, once with `CDROM`, once with `MOUSE`,
and once with `SOUND`. On each of those runs the `IF` line jumps straight to
the matching label, the block there loads one driver, and `GOTO END` returns.
The point of the arrangement is ordering on screen: each driver prints its own
line, in its own turn, under the boot screen's tree styling, rather than all of
them printing together after the loop. The three `ECHO` lines that follow draw
the closed box at the foot of the boot screen.

The individual lines:

| Line | What it does |
| --- | --- |
| `SET BLASTER=A220 I7 D1 H5 P300 T6` | Advertises the [ReSonique 2](../resonique2/manual.md)'s Sound Blaster-compatible digital audio to programs that read the variable: base 0x220, IRQ 7, 8-bit DMA 1, 16-bit DMA 5, MPU-401 at 0x300, card type 6. |
| `IZCDEX /I /D:TOKACD01 /L:D /T` | The CD-ROM redirector. `/D:` names the device driver `TOKACD.SYS` installed from `CONFIG.SYS`, `/L:D` assigns it drive D:. |
| `LH TOKAMOUS /T` | Loads the INT 33h mouse driver into an upper memory block if [TOKAEMM](../tokaemm/manual.md) has one free, and into conventional memory otherwise. |
| `SNDCTRL /B /T` | Prints the sound card's current IRQ and DMA assignment. It writes nothing. |
| `SNDMIXER /CFG C:\VOLCONF.CFG /S` | Restores the mixer levels last saved with F10 in the full-screen mixer. `/S` suppresses its output. If the file does not exist, the card keeps its power-on levels. |

`/T` on the driver lines selects the short, tree-styled boot line in place of
the driver's full sign-on banner.

Toka-DOS regenerates the `SET BLASTER` line from the machine's current
configuration, so it always matches the card's resources; `SNDCTRL` rewrites
the same line when it moves them. Once you edit `AUTOEXEC.BAT` yourself, the
file becomes yours and is left alone.

## Repair Toka-DOS

If the installed copy is damaged (files deleted, `COMMAND.COM` overwritten),
the [IZBIOS setup panel](../izbios/configuration-panel.md) has a **Repair
Toka-DOS** row that reinstalls the system files from the copy built into ROM.

Repair does two things. It writes `CONFIG.SYS` and `AUTOEXEC.BAT` back to the
stock contents shown above, renaming the existing files to `CONFIG.OLD` and
`AUTOEXEC.OLD` first, so an edited startup is recoverable. It then re-mounts
the drive, which restores the kernel, the shell, and the contents of `C:\DOS`
from ROM.

Repair is not a full restore of the drive. Files on C: that are not part of
Toka-DOS are left alone. Note that each repair overwrites the previous
`CONFIG.OLD` and `AUTOEXEC.OLD`: running it twice replaces the backup of your
edited startup with a backup of the stock one. Copy anything you want to keep
out of the way before the second run.

## Next

- [DOS command reference](commands.md): every shipped external command, with
  its switches.
- [Toka-DOS licensing](licensing.md): what the system is built from, and under
  what terms.
- [The TOKAEMM manual](../tokaemm/manual.md): memory management, covering XMS,
  EMS, UMBs, and the V86 monitor underneath it.
- [GSWMODE](commands.md#gswmode): change CPU speed class without leaving DOS.
- [SNDCTRL](commands.md#sndctrl): move the sound card's IRQ and DMA assignment
  without leaving DOS.
- [SNDMIXER](commands.md#sndmixer): set the card's volume levels, and keep them
  across reboots.
