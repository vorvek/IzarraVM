<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# How to Use Toka-DOS

Toka-DOS is the operating system that comes with the Izarra3000. It is a DOS
based on FreeDOS. General Simulation Works assembled it and gave it the
Toka-DOS name. The system ROM of the machine holds it, and the machine mounts
it on the hard disk at power-on. This page describes the disk layout, the
shell, and the two startup files.

## The files on the disk

The root of C: holds only the files that DOS must find there. It also holds a
`DOS` directory with the command interpreter and all of the tools.

```
C:\
    DOS\            AUTOEXEC.BAT    CONFIG.SYS      LICENSE.TXT
```

`KERNEL.SYS` is also in the root, because the boot sector loads it by name.
But it has the hidden attribute and the system attribute. Thus a plain `DIR`
of C:\ shows only the `DOS` directory, `AUTOEXEC.BAT`, `CONFIG.SYS`, and
`LICENSE.TXT`, in that order. `DIR` puts the directories first, and then sorts
by name. `C:\LICENSE.TXT` holds the terms for the distribution of Toka-DOS.
See [Toka-DOS licensing](licensing.md).

All of the programs are in `C:\DOS`:

```
C:\DOS\
    COMMAND.COM     GSWMODE.COM     MOVE.EXE        MORE.EXE
    TOKAMOUS.COM    SNDCTRL.COM     SORT.EXE        FIND.EXE
    TOKAEMM.SYS     SNDMIXER.COM    MEM.EXE         LABEL.EXE
    TOKACD.SYS      UNHALT.COM      ATTRIB.EXE      DELTREE.COM
    IZCDEX.COM                      CHOICE.EXE      XCOPY.EXE
                                    EDIT.COM        HELLO.TXT
```

`AUTOEXEC.BAT` adds `C:\DOS` to the `PATH`. Thus you can run each tool by its
name from any directory. The [DOS command reference](commands.md) describes
what each tool does.

One more file can occur in the root: `C:\VOLCONF.CFG`. `SNDMIXER` writes it
when you save the mixer levels for the first time. It is not part of the
supplied system. See [SNDMIXER](commands.md#sndmixer).

## The system ROM

Toka-DOS is not on the hard disk. The system ROM of the Izarra3000 holds it,
and the machine mounts it on C: at power-on. Thus the operating system is the
same at each boot, whatever the last program did to the disk. The hidden
`KERNEL.SYS`, the shell, and all of the files in `C:\DOS` are read-only. You
cannot delete them or write over them from DOS. General Simulation Works made
the ROM so that an update could write to it, but no update was released.

`CONFIG.SYS` and `AUTOEXEC.BAT` are the two exceptions. At the first boot, the
machine writes a copy of each file to C:. You can edit these copies, and the
machine does not change them again. Thus you can change the startup
configuration while the system files below it stay the same.
[Repair Toka-DOS](#repair-toka-dos) sets these two files back to the ROM
defaults.

## Drive letters

| Drive | What it is |
| --- | --- |
| **A:** | The floppy drive. Mount an `.img`, `.ima`, or `.flp` image from the IzarraVM GUI. |
| **C:** | The hard disk. In IzarraVM, this is a host folder that the machine shows as a FAT disk. |
| **D:** | The ATAPI CD-ROM. From the GUI, mount an ISO, a CUE/BIN pair, or a host folder as a data disc. |

`CONFIG.SYS` sets `LASTDRIVE=D`. This value covers these three drives and no
more. The [IzarraVM GUI guide](../izarravm-gui/guide.md) tells you how to
select the folder for C: and how to mount removable media.

## The shell

The shell of Toka-DOS is FreeCOM, the COMMAND.COM of FreeDOS. After the boot,
the screen shows:

```
C:\>
```

The `PROMPT $P$G` line in `AUTOEXEC.BAT` sets this prompt. The standard
FreeCOM functions are available:

- **DIR**, **COPY**, **DEL**, **REN**, and the other internal commands.
- **Batch files**: `.BAT` files with the usual `%1` parameters, `IF`, `GOTO`,
  `FOR`, and `CALL`.
- **Redirection and pipes**: `>`, `>>`, `<`, and `|`. The pipe lets you use
  `MORE` and `FIND` as filters. See the [command reference](commands.md).
- **Command history**: the usual FreeCOM line-edit keys recall a previous
  command at the prompt, and let you change it.

## CONFIG.SYS

The kernel reads `CONFIG.SYS` from the root of the boot drive before the shell
starts. The default file is:

```
FILES=40
LASTDRIVE=D
DEVICE=C:\DOS\TOKAEMM.SYS RAM /T
DOS=HIGH,UMB
DEVICEHIGH=C:\DOS\TOKACD.SYS
SHELL=C:\DOS\COMMAND.COM C:\DOS /E:2048 /P=C:\AUTOEXEC.BAT
```

`TOKAEMM.SYS` is the memory manager. `RAM` supplies EMS and upper memory
blocks. `/T` puts the tree connector of the boot screen in front of the
sign-on line of the driver.

`DOS=HIGH,UMB` moves the kernel into the HMA. It also adds the upper memory
blocks to the DOS chain. `DEVICEHIGH` and `LH` need that chain.

`TOKACD.SYS` is the ATAPI CD-ROM device driver. The redirector that gives it a
drive letter starts later, from `AUTOEXEC.BAT`.

The `SHELL=` line starts FreeCOM with a 2048-byte environment. `C:\DOS` is the
directory that FreeCOM makes `COMSPEC` from. `/P=` gives the name of the
startup batch file.

See the [TOKAEMM manual](../tokaemm/manual.md) for the switches of the memory
manager.

## AUTOEXEC.BAT

The default startup file is a batch file. It calls itself one time for each
driver:

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

The first run has no parameter. Thus the `IF NOT "%1"==""` line does not jump,
and the file sets the environment: `PROMPT`, `PATH`, and `BLASTER`. The `FOR`
loop then calls the same file three more times: with `CDROM`, with `MOUSE`,
and with `SOUND`. On each of those runs, the `IF` line jumps to the related
label. The block at that label loads one driver, and `GOTO END` goes to the
end of the file.

This arrangement controls the order of the lines on the screen. Each driver
prints its own line, at its own time, in the tree style of the boot screen.
Without the arrangement, all of the lines print together after the loop. The
three `ECHO` lines after the loop draw the closed box at the bottom of the
boot screen. The workbench in that box is not part of this release. In text
mode, the line is information only.

The individual lines:

| Line | What it does |
| --- | --- |
| `SET BLASTER=A220 I7 D1 H5 P300 T6` | Gives the digital audio resources of the [ReSonique II](../resonique2/manual.md) to a program that reads the variable: base 0x220, IRQ 7, 8-bit DMA 1, 16-bit DMA 5, MPU-401 at 0x300, and card type 6. |
| `IZCDEX /I /D:TOKACD01 /L:D /T` | The CD-ROM redirector. `/D:` gives the device name of the driver, `TOKACD01`. That is the name of the driver in memory, and not its file name. `/L:D` gives it drive D:. `/I` installs the redirector even when a different redirector is already in memory. |
| `LH TOKAMOUS /T` | Loads the INT 33h mouse driver. It uses an upper memory block if [TOKAEMM](../tokaemm/manual.md) has one free. If not, it uses conventional memory. |
| `SNDCTRL /B /T` | Shows the current IRQ and DMA of the sound card. It writes nothing. |
| `SNDMIXER /CFG C:\VOLCONF.CFG /S` | Sets the mixer levels that F10 saved in the full-screen mixer. `/S` stops the output of the tool. If the file does not exist, the card keeps its power-on levels. |

`/T` puts the tree connector of the boot screen in front of the sign-on line
of the driver. It does not make the line shorter. On `SNDCTRL`, `/B` selects
the two-row summary, and `/T` changes only the style.

Toka-DOS writes the `SET BLASTER` line again from the current configuration of
the machine. Thus the line always agrees with the resources of the card.
`SNDCTRL` writes the same line again when it moves those resources. After you
edit `AUTOEXEC.BAT`, the machine does not write to the file again.

## Repair Toka-DOS

The installed copy can become damaged. For example, a program can delete a
file, or write over `COMMAND.COM`. The
[IZBIOS setup panel](../izbios/configuration-panel.md) has a **Repair
Toka-DOS** row. It installs the system files again, from the copy in the ROM.

The repair does two things:

1. It writes `CONFIG.SYS` and `AUTOEXEC.BAT` again, with the default contents
   above. It makes the `SET BLASTER` line from the current configuration of
   the machine. First it changes the names of the existing files to
   `CONFIG.OLD` and `AUTOEXEC.OLD`. Thus you can recover an edited startup
   file.
2. It mounts the drive again. This puts the kernel, the shell, and the
   contents of `C:\DOS` back from the ROM.

The repair does not replace the full drive. It does not change the files on C:
that are not part of Toka-DOS.

**Warning:** each repair writes over the `CONFIG.OLD` file and the
`AUTOEXEC.OLD` file of the previous repair. If you do the repair two times,
you lose the backup of your edited startup files. Copy the files that you want
to keep before you do the repair a second time.

## Next

- [DOS command reference](commands.md): each external command, with its
  switches.
- [Toka-DOS licensing](licensing.md): the parts of the system, and their
  license terms.
- [The TOKAEMM manual](../tokaemm/manual.md): memory management, with XMS,
  EMS, UMBs, and the V86 monitor.
- [GSWMODE](commands.md#gswmode): change the CPU speed class from DOS.
- [SNDCTRL](commands.md#sndctrl): move the IRQ and the DMA of the sound card
  from DOS.
- [SNDMIXER](commands.md#sndmixer): set the volume levels of the card, and
  keep them for the next boot.
