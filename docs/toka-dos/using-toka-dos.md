<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Using Toka-DOS

Toka-DOS is the Izarra 3000's bundled operating system: a FreeDOS-based
DOS built to General Simulation Works's own product identity, shipped in the
machine's system ROM and mounted onto the hard disk at boot. This page covers
what you get at the C:\> prompt and how the disk is laid out.

## Boot banner

The kernel's boot banner box signs on with:

```
Toka-DOS 3.0 - Kernel build 2043 - Compiled <date>
(C) 1992-1997 Izarra SL - All Rights Reserved ** See LICENSE.TXT for more.
```

`C:\LICENSE.TXT` explains what Toka-DOS is built from: the FreeDOS
kernel and FreeCOM shell, plus MOVE, SORT, MEM, and other tools from the
FreeDOS project, all free software under the GNU GPL. General Simulation
Works's own additions (`GSWMODE`, `SNDCTRL`, `SNDMIXER`, `TOKAMOUS`,
`TOKAEMM.SYS`) are layered on
top of that stock FreeDOS base; the shell and kernel underneath are
otherwise unmodified.

## What's on the disk

The C: drive root holds only the files DOS requires there, plus a `DOS`
directory containing the command interpreter and every tool.

```
C:\
    DOS\            AUTOEXEC.BAT    CONFIG.SYS      LICENSE.TXT
```

`KERNEL.SYS` lives in the root too, because the boot sector loads it by name, but it
is hidden, so a plain `DIR` of C:\ shows only the `DOS` folder, `AUTOEXEC.BAT`,
`CONFIG.SYS`, and `LICENSE.TXT` — in that order, since `DIR` sorts by name and
groups directories first. Every program you run is in `C:\DOS`:

```
C:\DOS\
    COMMAND.COM     GSWMODE.COM     MEM.EXE         FIND.EXE
    TOKAMOUS.COM    SNDCTRL.COM     ATTRIB.EXE      LABEL.EXE
    TOKAEMM.SYS     SNDMIXER.COM    CHOICE.EXE      DELTREE.COM
                    MOVE.EXE        MORE.EXE        XCOPY.EXE
                    SORT.EXE                        HELLO.TXT
```

`AUTOEXEC.BAT` puts `C:\DOS` on the `PATH`, so every tool runs from any
directory by name. See the [DOS command reference](commands.md) for what each
one does.

## The system ROM

Toka-DOS is not stored on the hard disk. It is held in the Izarra 3000's system
ROM and mounted onto C: at power-on, so the operating system comes up the same
on every boot regardless of what the last program did to the disk. General
Simulation Works built the ROM to be reflashed for updates, but no update ever
shipped, so Toka-DOS is immutable. The hidden `KERNEL.SYS`, the shell, and
everything under `C:\DOS` are mounted read-only and cannot be deleted or
overwritten from DOS. Most DOS machines of the era ran the whole system from a
writable disk. The Izarra 3000 holds it in ROM instead.

`CONFIG.SYS` and `AUTOEXEC.BAT` are the two exceptions. The machine writes
editable copies of them to C: the first time it boots and then leaves them
alone, so the startup configuration can be edited while the system files
underneath it remain unchanged. [Repair Toka-DOS](#repair-toka-dos) resets those
two files to the ROM defaults, for returning to the stock startup.

## Drive letters

| Drive | What it is |
| --- | --- |
| **A:** | The floppy drive. Mount a `.img`/`.ima`/`.flp` image from the IzarraVM GUI. |
| **C:** | The hard disk. In IzarraVM, a real host folder presented as a FAT disk. |
| **D:** | The ATAPI CD-ROM. Mount an ISO, a CUE/BIN pair, or a host folder as a data disc from the GUI. |

`CONFIG.SYS` sets `LASTDRIVE=D`, matching exactly these three drives with
nothing spare. See the [IzarraVM GUI guide](../izarravm-gui/guide.md) for how
to point C: at a folder and mount removable media.

## The shell

Toka-DOS's shell is FreeCOM, FreeDOS's COMMAND.COM. Booting drops you at:

```
C:\>
```

set by `PROMPT $P$G` in `AUTOEXEC.BAT`. The standard FreeCOM shell facilities
are available:

- **DIR**, **COPY**, **DEL**, **REN**, and the rest of the built-in command
  set.
- **Batch files**: `.BAT` scripts with the usual `%1`-style parameters,
  `IF`/`GOTO`/`FOR`, and `CALL`. `CONFIG.SYS` boots straight into one via
  `SHELL=C:\DOS\COMMAND.COM C:\DOS /E:2048 /P=C:\AUTOEXEC.BAT`.
- **Redirection and pipes**: `>`, `>>`, `<`, and `|` all work, which is what
  makes `MORE` and `FIND` useful as filters (see the
  [command reference](commands.md)).
- **Command history and editing**: the usual FreeCOM line-editing keys
  recall and edit previous commands at the prompt.

## AUTOEXEC.BAT

The stock startup script is short:

```
@ECHO OFF
PROMPT $P$G
PATH C:\DOS
SET BLASTER=A220 I7 D1 H5 P300 T6
LH TOKAMOUS
```

`SET BLASTER` advertises the ReSonique 2's Sound Blaster-compatible digital
audio to any program that looks for the environment variable (base 0x220,
IRQ 7, 8-bit DMA 1, 16-bit DMA 5). Toka-DOS regenerates the line from the
machine's configuration, so it matches the card's current resources. A
hand-edited `AUTOEXEC.BAT` is left alone. See the
[ReSonique 2 manual](../resonique2/manual.md) for what those numbers mean.
`LH TOKAMOUS` loads the mouse driver high, into an upper memory block if
[TOKAEMM](../tokaemm/manual.md) has one free, falling back to a normal load
otherwise.

## Repair Toka-DOS

If the installed copy is damaged (files deleted, `COMMAND.COM` overwritten,
whatever went wrong), the [IZBIOS setup panel](../izbios/configuration-panel.md)
has a **Repair Toka-DOS** row that reinstalls this same disk layout from the
image built into ROM. It only touches the Toka-DOS system files; anything
else you've put on C: is left alone. This is a repair action, not a full
restore: your `CONFIG.SYS` and `AUTOEXEC.BAT` are backed up to `.OLD` copies
first, then replaced with the stock defaults, rather than silently
overwritten.

## Next

- [DOS command reference](commands.md): every shipped external command,
  with its switches.
- [The TOKAEMM manual](../tokaemm/manual.md): memory management, covering XMS, EMS,
  UMBs, and the V86 monitor underneath it.
- [GSWMODE](commands.md#gswmode): change CPU speed class without leaving
  DOS.
- [SNDCTRL](commands.md#sndctrl): move the sound card's IRQ and DMA
  assignment without leaving DOS.
- [SNDMIXER](commands.md#sndmixer): set the card's volume levels, and keep
  them across reboots.
