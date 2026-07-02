# Using Toka-DOS

Toka-DOS is the Izarra 3000's bundled operating system: a FreeDOS-based
DOS built to General Simulation Works's own product identity, shipped
pre-installed on the hard disk. This page covers what you get at the C:\>
prompt and how the disk is laid out.

## Boot banner

The kernel signs on with:

```
Toka-DOS 3.0 (C) 1997 General Simulation Works - tongue firmly in cheek.
See C:\LICENSE.TXT for more.
```

`C:\LICENSE.TXT` explains what Toka-DOS is actually built from: the FreeDOS
kernel and FreeCOM shell, plus MOVE, SORT, MEM, and other tools from the
FreeDOS project, all free software under the GNU GPL. General Simulation
Works's own additions — `GSWMODE`, `TOKAMOUS`, `TOKAEMM.SYS` — are layered on
top of that stock FreeDOS base; the shell and kernel underneath are
otherwise unmodified.

## What's on the disk

The C: drive root holds everything Toka-DOS needs to run, flat — no
subdirectories:

```
KERNEL.SYS      CONFIG.SYS      TOKAMOUS.COM    ATTRIB.EXE
COMMAND.COM     AUTOEXEC.BAT    TOKAEMM.SYS     CHOICE.EXE
GSWMODE.COM     MOVE.EXE        SORT.EXE        MORE.EXE
MEM.EXE         FIND.EXE        LABEL.EXE       DELTREE.COM
XCOPY.EXE       HELLO.TXT       LICENSE.TXT
```

See the [DOS command reference](commands.md) for what each tool does.

## Drive letters

| Drive | What it is |
| --- | --- |
| **A:** | The floppy drive. Mount a `.img`/`.ima`/`.flp` image from the IzarraVM GUI. |
| **C:** | The hard disk — in IzarraVM, a real host folder presented as a FAT disk. |
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
  `SHELL=C:\COMMAND.COM C:\ /E:2048 /P=C:\AUTOEXEC.BAT`.
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
PATH C:\
SET BLASTER=A220 I5 D1 H5 T6
LH TOKAMOUS
```

`SET BLASTER` advertises the ReSonique 2's Sound Blaster–compatible digital
audio to any program that looks for the environment variable (base 0x220,
IRQ 5, 8-bit DMA 1, 16-bit DMA 5) — see the
[ReSonique 2 manual](../resonique2/manual.md) for what those numbers mean.
`LH TOKAMOUS` loads the mouse driver high, into an upper memory block if
[TOKAEMM](../tokaemm/manual.md) has one free, falling back to a normal load
otherwise.

## Repair Toka-DOS

If the installed copy is damaged — files deleted, `COMMAND.COM` overwritten,
whatever went wrong — the [IZBIOS setup panel](../izbios/configuration-panel.md)
has a **Repair Toka-DOS** row that reinstalls this same disk layout from the
image built into ROM. It only touches the Toka-DOS system files; anything
else you've put on C: is left alone. This is a repair action, not a full
restore: your `CONFIG.SYS` and `AUTOEXEC.BAT` are backed up to `.OLD` copies
first, then replaced with the stock defaults, rather than silently
overwritten.

## Next

- [DOS command reference](commands.md) — every shipped external command,
  with its switches.
- [The TOKAEMM manual](../tokaemm/manual.md) — memory management: XMS, EMS,
  UMBs, and the V86 monitor underneath it.
- [GSWMODE](commands.md#gswmode) — change CPU speed class without leaving
  DOS.
