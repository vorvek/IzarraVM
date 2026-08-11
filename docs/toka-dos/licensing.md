<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Toka-DOS Licensing

Toka-DOS is free software. It is assembled from the FreeDOS kernel and shell,
a set of FreeDOS command-line tools, and programs written for the Izarra 3000
by General Simulation Works. Each part keeps the license it came with.

The full text is on the disk itself, in `C:\LICENSE.TXT`. Read it from the
prompt with:

```
TYPE C:\LICENSE.TXT | MORE
```

The file is about 25 KB: a short summary, the project's attribution notice,
and the complete GNU General Public License, version 2.

## What comes from FreeDOS

| Part | Files | Copyright and license |
| --- | --- | --- |
| Kernel | `KERNEL.SYS` | (C) 1995-2012 Pasquale J. Villani and The FreeDOS Project. GPL v2 or later. |
| Shell | `COMMAND.COM` (FreeCOM) | (C) 1994-2005 Tim Norman and others. GPL v2 or later, with supplement-library files under LGPL v2 or later. |
| Commands | `MOVE.EXE`, `SORT.EXE`, `MEM.EXE`, `ATTRIB.EXE`, `CHOICE.EXE`, `MORE.EXE`, `FIND.EXE`, `LABEL.EXE`, `DELTREE.COM` | Various FreeDOS authors, listed per program in `LICENSE.TXT`. GPL v2 or later. |
| Message tables | kitten (NLS) | (C) 1999-2000 Jim Hall and others. LGPL v2.1 or later. The `tnyprntf` printf used with it is (C) 1995 Pasquale J. Villani, GPL v2 or later. |

The kernel, the shell, and several of the commands carry Toka-DOS changes.
Each edited file carries a note saying so, and `toka-dos/freedos/VENDOR.md`
in the IzarraVM repository lists them one by one, with the upstream revision
each was taken from. In the kernel they are the boot-screen restyle (the
sign-on, the welcome box, and the drive-assignment lines), the product
rebrand, the replacement of the sign-on's GPL and copyright block with the
pointer to `C:\LICENSE.TXT`, and changed `SHELL=` and `IDLEHALT` defaults; in
FreeCOM, the suppressed startup banner and the product strings. Among the
commands, `MEM` reports memory in a four-line color map and pages its program
list under `/P`, `MOVE`, `SORT` and `FIND` carry product-name rebrands, and
`ATTRIB` took two portability fixes with no change in behavior.

## What comes from elsewhere

`IZCDEX.COM`, the CD-ROM redirector, is Jason Hood's SHSUCDX, (C) 2005-2020,
under the Zlib license. Toka-DOS rebrands it and restyles its install line for
the boot screen; the altered source is marked as such, as that license
requires.

`TOKAEMM.SYS` is original NASM code, but its VCPI 1.0 server and its emulation
of privileged instructions in virtual-8086 mode follow the mechanisms of
386MAX, (C) 1987-98 Qualitas, Inc. and (C) 1990-2012 Sudley Place Software,
under GPL v3.

## What General Simulation Works wrote

`GSWMODE.COM`, `SNDCTRL.COM`, `SNDMIXER.COM`, `TOKAMOUS.COM`, `TOKACD.SYS`,
`TOKAEMM.SYS`, `UNHALT.COM`, `XCOPY.EXE`, and `EDIT.COM` are project code, not
ported FreeDOS binaries. They are under GPL version 3 only, as is IzarraVM
itself.

## Warranty

There is none. The program is provided as is, without warranty of any kind, to
the extent permitted by applicable law. The exact terms are in section 11 of
the GPL v2 text in `C:\LICENSE.TXT`, and in the `LICENSE` file of the IzarraVM
repository for the GPL v3 parts.

## Source code

The corresponding source for every part of Toka-DOS is in the IzarraVM
repository:

- `toka-dos/freedos/` holds the FreeDOS kernel, FreeCOM, the vendored command
  sources, and the SHSUCDX source IZCDEX is built from, with the Toka-DOS
  modifications applied. `toka-dos/freedos/VENDOR.md` records each upstream
  revision and every local patch.
- `crates/izarravm-firmware/roms/dos/` holds the NASM sources for the drivers
  and tools assembled straight into the image: `tokaemm.asm`, `tokacd.asm`,
  `gswmode.asm`, `sndctrl.asm`, `sndmixer.asm`, and `unhalt.asm`.
- `toka-dos/tools/tokamous.asm` is the mouse driver.
- `toka-dos/tools-src/` holds the C sources for `XCOPY.EXE` and `EDIT.COM`.
- `NOTICE` at the repository root records every attribution, including the
  components used by IzarraVM outside Toka-DOS.
- `LICENSE_MANIFEST.tsv` records the origin, license, and SHA-256 of each
  tracked binary and generated file, the disk image among them.

## Next

- [Using Toka-DOS](using-toka-dos.md): the shell, the startup files, and the
  disk layout.
- [DOS command reference](commands.md): every shipped external command, with
  its switches.
