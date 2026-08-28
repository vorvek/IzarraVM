<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Toka-DOS Licensing

Toka-DOS is free software. It contains the FreeDOS kernel, the FreeDOS shell,
a set of FreeDOS command-line tools, and programs that General Simulation
Works wrote for the Izarra3000. Each part keeps its own license.

The full text is on the disk, in `C:\LICENSE.TXT`. To read it from the prompt,
type:

```
TYPE C:\LICENSE.TXT | MORE
```

The file is approximately 25 KB. It contains a short summary, the attribution
notice of the project, and the full GNU General Public License, version 2.

## The parts from FreeDOS

| Part | Files | Copyright and license |
| --- | --- | --- |
| Kernel | `KERNEL.SYS` | (C) 1995-2012 Pasquale J. Villani and The FreeDOS Project. GPL v2 or later. |
| Shell | `COMMAND.COM` (FreeCOM) | (C) 1994-2005 Tim Norman and others. GPL v2 or later. The supplement-library files are under LGPL v2 or later. |
| Commands | `MOVE.EXE`, `SORT.EXE`, `MEM.EXE`, `ATTRIB.EXE`, `CHOICE.EXE`, `MORE.EXE`, `FIND.EXE`, `LABEL.EXE`, `DELTREE.COM` | Different FreeDOS authors. `LICENSE.TXT` gives the author of each program. GPL v2 or later. |
| Message tables | kitten (NLS) | (C) 1999-2000 Jim Hall and others. LGPL v2.1 or later. The `tnyprntf` printf that goes with it is (C) 1995 Pasquale J. Villani, GPL v2 or later. |

The kernel, the shell, and some of the commands contain Toka-DOS changes. Each
changed file has a note about the change. The file
`toka-dos/freedos/VENDOR.md` in the IzarraVM repository lists the changed
files one by one, with the upstream revision of each one.

In the kernel, the changes are the new style of the boot screen (the sign-on,
the welcome box, and the drive-assignment lines), the new product name, and
new `SHELL=` and `IDLEHALT` defaults. A pointer to `C:\LICENSE.TXT` also
replaces the GPL block and the copyright block in the sign-on.

In FreeCOM, the changes are the removal of the startup banner and the new
product strings. In the commands, `MEM` reports memory in a four-line color
map and shows its program list one page at a time under `/P`. `MOVE`, `SORT`,
and `FIND` have the new product name. `ATTRIB` has two portability fixes that
do not change its behavior.

## The parts from other sources

The image carries no CD-ROM driver files: the IzarraCD ROM Extensions in the
system BIOS serve the CD-ROM. Earlier releases shipped `IZCDEX.COM`, a build
of the SHSUCDX of Jason Hood, (C) 2005-2020, under the Zlib license. Its
source stays in the repository for reference, with the Toka-DOS marks that
license requires.

`TOKAEMM.SYS` is original NASM code. But its VCPI 1.0 server and its emulation
of privileged instructions in virtual-8086 mode use the mechanisms of 386MAX,
(C) 1987-98 Qualitas, Inc. and (C) 1990-2012 Sudley Place Software, under
GPL v3.

## The parts that General Simulation Works wrote

`GSWMODE.COM`, `SNDCTRL.COM`, `SNDMIXER.COM`, `TOKAMOUS.COM`,
`TOKAEMM.SYS`, `UNHALT.COM`, `XCOPY.EXE`, and `EDIT.COM` are project code.
They are not FreeDOS binaries. They are under GPL version 3 only, as IzarraVM
is.

## Warranty

There is no warranty. The program is provided as is, without warranty of any
kind, to the extent permitted by applicable law. Section 11 of the GPL v2 text
in `C:\LICENSE.TXT` gives the exact terms. For the GPL v3 parts, the `LICENSE`
file of the IzarraVM repository gives them.

## Source code

The IzarraVM repository holds the source code for each part of Toka-DOS:

- `toka-dos/freedos/` holds the FreeDOS kernel, FreeCOM, the vendored command
  sources, and the SHSUCDX source (kept for reference). The Toka-DOS changes
  are in these sources. `toka-dos/freedos/VENDOR.md` records each upstream
  revision and each local patch.
- `crates/izarravm-firmware/roms/dos/` holds the NASM sources for the drivers
  and the tools that go directly into the image: `tokaemm.asm`, `tokacd.asm`,
  `gswmode.asm`, `sndctrl.asm`, `sndmixer.asm`, and `unhalt.asm`.
- `toka-dos/tools/tokamous.asm` is the mouse driver.
- `toka-dos/tools-src/` holds the C sources for `XCOPY.EXE` and `EDIT.COM`.
- `NOTICE` at the root of the repository records each attribution. This
  includes the components that IzarraVM uses outside Toka-DOS.
- `LICENSE_MANIFEST.tsv` records the origin, the license, and the SHA-256 of
  each tracked binary and each generated file. The disk image is one of them.

## Next

- [How to use Toka-DOS](using-toka-dos.md): the shell, the startup files, and
  the disk layout.
- [DOS command reference](commands.md): each external command, with its
  switches.
