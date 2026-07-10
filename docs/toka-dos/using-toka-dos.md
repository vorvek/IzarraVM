<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Using Toka-DOS

Toka-DOS is the Izarra 3000's bundled operating system: a FreeDOS-based
DOS built to General Simulation Works's own product identity, shipped in the
machine's system ROM and mounted onto the hard disk at boot. This page covers
what you get at the C:\> prompt and how the disk is laid out.

## Boot banner

The kernel signs on with:

```
Toka-DOS 3.0 (C) 1997 General Simulation Works - tongue firmly in cheek.
See C:\LICENSE.TXT for more.
```

`C:\LICENSE.TXT` explains what Toka-DOS is actually built from: the FreeDOS
kernel and FreeCOM shell, plus MOVE, SORT, MEM, and other tools from the
FreeDOS project, all free software under the GNU GPL. General Simulation
Works's own additions (`GSWMODE`, `TOKAMOUS`, `TOKAEMM.SYS`) are layered on
top of that stock FreeDOS base; the shell and kernel underneath are
otherwise unmodified.

## What's on the disk

The C: drive root stays sparse: only the files DOS needs there, plus a `DOS`
directory that holds the command interpreter and every tool.

```
C:\
    CONFIG.SYS      AUTOEXEC.BAT    LICENSE.TXT     DOS\
```

`KERNEL.SYS` lives in the root too, because the boot sector loads it by name, but it
is hidden, so a plain `DIR` of C:\ shows only `CONFIG.SYS`, `AUTOEXEC.BAT`,
`LICENSE.TXT`, and the `DOS` folder. Everything you actually run lives in
`C:\DOS`:

```
C:\DOS\
    COMMAND.COM     GSWMODE.COM     MEM.EXE         FIND.EXE
    TOKAMOUS.COM    MOVE.EXE        ATTRIB.EXE      LABEL.EXE
    TOKAEMM.SYS     SORT.EXE        CHOICE.EXE      DELTREE.COM
                    MORE.EXE        XCOPY.EXE       HELLO.TXT
```

`AUTOEXEC.BAT` puts `C:\DOS` on the `PATH`, so every tool runs from any
directory by name. See the [DOS command reference](commands.md) for what each
one does.

## The system ROM

Toka-DOS does not really live on the hard disk. It ships inside the Izarra
3000's system ROM and is mounted onto C: at power-on, so the operating system
comes up the same on every boot no matter what the last program did to the
disk. General Simulation Works built the ROM to be reflashed for updates, but
none ever shipped, so Toka-DOS ended up immutable: the hidden `KERNEL.SYS`, the
shell, and everything under `C:\DOS` are mounted read-only and cannot be
deleted or overwritten from DOS. Most DOS machines of the era ran the whole
system off a writable disk; the Izarra 3000 kept it in ROM instead.

`CONFIG.SYS` and `AUTOEXEC.BAT` are the two exceptions. The machine writes
editable copies of them to C: the first time it boots and then leaves them
alone, so your startup configuration is yours to change while the system files
underneath it never drift. [Repair Toka-DOS](#repair-toka-dos) resets just
those two files to the ROM defaults if you want the stock startup back.

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

set by `PROMPT $P$G` in `AUTOEXEC.BAT`. Everything you'd expect from a
FreeCOM shell works normally:

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
SET BLASTER=A220 I5 D1 H5 P300 T6
LH TOKAMOUS
```

`SET BLASTER` advertises the ReSonique 2's Sound Blaster-compatible digital
audio to any program that looks for the environment variable (base 0x220,
IRQ 5, 8-bit DMA 1, 16-bit DMA 5). See the
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
