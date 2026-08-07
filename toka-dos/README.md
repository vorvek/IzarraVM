<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Toka-DOS

The Izarra 3000's bundled operating system: a rebranded, real **FreeDOS**. The
kernel and shell are vendored FreeDOS source (see `freedos/VENDOR.md` for exact
tags/commits and the local rebrand patches), built from source and packaged
into a bootable FAT32 hard-disk image. There is no custom DOS clone here; the
guest runs the genuine FreeDOS kernel and FreeCOM shell under the Izarra BIOS.

The product banner reads "Toka-DOS 3.0"; the kernel reports DOS compatibility
7.10 (8086 target + FAT32 support). `TOKAEMM.SYS`, our own memory manager
(XMS/UMB/EMS + the V86 monitor everything else runs under), and small guest
tools like `TOKAMOUS.COM` (PS/2 mouse TSR) and `GSWMODE.COM` (runtime CPU-speed
switch) are Izarra-specific additions layered on top of stock FreeDOS.
`TOKACD.SYS` and `IZCDEX.COM` provide the guest-owned ATAPI and ISO 9660 CD-ROM
stack for drive D:.

## Layout

- `freedos/` vendored FreeDOS source trees plus the permissively licensed
  SHSUCDX redirector, with upstream revisions and local patches recorded in
  `freedos/VENDOR.md`.
- `tools-src/` original (not vendored) Toka-DOS project tools, GPL-3, written
  from documented command-line behavior rather than ported from another
  project. See `tools-src/README.md`.
- `tools/tokamous.asm` the INT 33h PS/2 mouse driver TSR (hand-written NASM,
  not vendored FreeDOS), assembled straight into `TOKAMOUS.COM`.
- `build-freedos.ps1` the build script: builds the FreeDOS kernel + FreeCOM
  shell (Open Watcom cross-compile), builds `MOVE.EXE`/`SORT.EXE` from the
  vendored userland sources, assembles `TOKAMOUS.COM`, `TOKAEMM.SYS`, and
  `TOKACD.SYS`, builds `IZCDEX.COM`, and invokes
  `scripts/build-freedos-hdd-image.py` to assemble the committed disk image.

Small standalone guest `.COM`/`.SYS` tools that aren't vendored FreeDOS source
(`TOKAEMM.SYS`, `GSWMODE.COM`, and DOS test fixtures like `MOUSETST.COM`) live
as NASM source + a committed built binary under
`crates/izarravm-firmware/roms/dos/`, next to the other small DOS fixtures the
firmware crate embeds, not under `toka-dos/`.

## Building

Authoring only. CI does not build this; `crates/izarravm-firmware/roms/tokados-hdd.img`
(and the small `.com`/`.sys` binaries under `crates/izarravm-firmware/roms/dos/`)
are committed and embedded by the firmware crate, the same way the BIOS `.bin` is.

Requires Open Watcom (`D:\DevTools\OpenWatcom`) and NASM on PATH.

    pwsh toka-dos/build-freedos.ps1

This builds `kernel.sys`, the FAT12/FAT32-LBA/MBR boot sectors, `command.com`,
the DOS tools and drivers, `IZCDEX.COM`, and `GSWMODE.COM`, then runs
`scripts/build-freedos-hdd-image.py` to assemble them into
`crates/izarravm-firmware/roms/tokados-hdd.img`. At runtime,
`crates/izarravm-machine/src/katea_volume.rs::extract_system_payload` parses
that image and overlays every payload file except `HELLO.TXT`/`CONFIG.SYS`/
`AUTOEXEC.BAT` onto the guest's C: drive.

If the Open Watcom kernel/FreeCOM build artifacts are absent (e.g. a
from-image rebuild after only touching `build-freedos-hdd-image.py` itself),
the Python script falls back to re-extracting `KERNEL.SYS`/`COMMAND.COM`/
`TOKAMOUS.COM`/`IZCDEX.COM` from the previously committed image, so the image
can still be regenerated without a full Open Watcom rebuild.

## Default CD-ROM setup

The stock `CONFIG.SYS` loads `TOKACD.SYS` high after TOKAEMM has enabled upper
memory. `AUTOEXEC.BAT` then assigns D: with IZCDEX before loading the mouse
driver. Host-folder installations upgrade each file only when its bytes match
the immediately preceding stock version. Customized files are left alone. Add
these lines manually when keeping a customized setup:

    DEVICEHIGH=C:\DOS\TOKACD.SYS
    IZCDEX /I /D:TOKACD01 /L:D /T

The first line belongs after `DOS=HIGH,UMB` in `CONFIG.SYS`; the second belongs
after `SET BLASTER` and before `LH TOKAMOUS` in `AUTOEXEC.BAT`. `/T` is the
optional boot-tree styling (a one-line tree-styled install banner instead of
plain output); drop it for plain output. BIOS Setup's Repair Toka-DOS command
still backs up both files and restores the complete current defaults.

## Adding a new guest tool

- A small standalone NASM `.COM`/`.SYS` (no FreeDOS source dependency): add
  the `.asm` under `crates/izarravm-firmware/roms/dos/`, assemble it (either
  by hand or by adding an `nasm -f bin` step to `build-freedos.ps1`, following
  the `GSWMODE.COM` step as a template), add an `include_bytes!` constant plus
  a `pub fn xxx_com()` accessor in `crates/izarravm-firmware/src/lib.rs`, then
  add `("XXX.COM", xxx)` to the files list in
  `scripts/build-freedos-hdd-image.py` and re-run it (or `build-freedos.ps1`,
  which runs it as its last step) to regenerate and commit
  `tokados-hdd.img`.
- A real FreeDOS/MS-DOS-4.0 userland tool: vendor its source under
  `toka-dos/freedos/<tool>/` (see `freedos/VENDOR.md` for the pattern,
  upstream tag/commit recorded, local patches noted per file), add a build
  step to `build-freedos.ps1` mirroring the existing `move`/`sort` steps, and
  wire the built binary into `build-freedos-hdd-image.py` the same way.
- An original (not vendored) tool, written from documented command-line
  behavior rather than ported from another project: put its source under
  `toka-dos/tools-src/<tool>/` (see `tools-src/README.md`; this is
  project-authored GPL-3 code, unlike `freedos/`'s GPL-2-only vendored
  sources), add a build step to `build-freedos.ps1` mirroring the `xcopy`
  step, and wire the built binary into `build-freedos-hdd-image.py` the same
  way.
