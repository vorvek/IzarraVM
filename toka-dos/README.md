# Toka-DOS

The Izarra 3000's bundled operating system: a rebranded, real **FreeDOS**. The
kernel and shell are vendored FreeDOS source (see `freedos/VENDOR.md` for exact
tags/commits and the local rebrand patches), built from source and packaged
into a bootable FAT32 hard-disk image. There is no custom DOS clone here — the
guest runs the genuine FreeDOS kernel and FreeCOM shell under the Izarra BIOS.

The product banner reads "Toka-DOS 3.0"; the kernel reports DOS compatibility
7.10 (8086 target + FAT32 support). `TOKAEMM.SYS`, our own memory manager
(XMS/UMB/EMS + the V86 monitor everything else runs under), and small guest
tools like `TOKAMOUS.COM` (PS/2 mouse TSR) and `GSWMODE.COM` (runtime CPU-speed
switch) are Izarra-specific additions layered on top of stock FreeDOS.

## Layout

- `freedos/` vendored FreeDOS source trees (`kernel`, `freecom`, `move`, `sort`),
  each with its own `.github`/build scripts from upstream plus the local
  rebrand patches described in `freedos/VENDOR.md`.
- `tools/tokamous.asm` the INT 33h PS/2 mouse driver TSR (hand-written NASM,
  not vendored FreeDOS), assembled straight into `TOKAMOUS.COM`.
- `build-freedos.ps1` the build script: builds the FreeDOS kernel + FreeCOM
  shell (Open Watcom cross-compile), builds `MOVE.EXE`/`SORT.EXE` from the
  vendored userland sources, assembles `TOKAMOUS.COM` and `GSWMODE.COM`, then
  invokes `scripts/build-freedos-hdd-image.py` to assemble the committed disk
  image.

Small standalone guest `.COM`/`.SYS` tools that aren't vendored FreeDOS source
(`TOKAEMM.SYS`, `GSWMODE.COM`, and DOS test fixtures like `MOUSETST.COM`) live
as NASM source + a committed built binary under
`crates/izarravm-firmware/roms/dos/`, next to the other small DOS fixtures the
firmware crate embeds — not under `toka-dos/`.

## Building

Authoring only. CI does not build this; `crates/izarravm-firmware/roms/tokados-hdd.img`
(and the small `.com`/`.sys` binaries under `crates/izarravm-firmware/roms/dos/`)
are committed and embedded by the firmware crate, the same way the BIOS `.bin` is.

Requires Open Watcom (`D:\DevTools\OpenWatcom`) and NASM on PATH.

    pwsh toka-dos/build-freedos.ps1

This builds `kernel.sys`, the FAT12/FAT32-LBA/MBR boot sectors, `command.com`,
`MOVE.EXE`, `SORT.EXE`, `TOKAMOUS.COM`, and `GSWMODE.COM` (all gitignored
intermediates), then runs `scripts/build-freedos-hdd-image.py` to assemble
those into `crates/izarravm-firmware/roms/tokados-hdd.img`. At runtime,
`crates/izarravm-machine/src/katea_volume.rs::extract_system_payload` parses
that image and overlays every payload file except `HELLO.TXT`/`CONFIG.SYS`/
`AUTOEXEC.BAT` onto the guest's C: drive.

If the Open Watcom kernel/FreeCOM build artifacts are absent (e.g. a
from-image rebuild after only touching `build-freedos-hdd-image.py` itself),
the Python script falls back to re-extracting `KERNEL.SYS`/`COMMAND.COM`/
`TOKAMOUS.COM` from the previously committed image, so the image can still be
regenerated without a full Open Watcom rebuild.

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
  `toka-dos/freedos/<tool>/` (see `freedos/VENDOR.md` for the pattern —
  upstream tag/commit recorded, local patches noted per file), add a build
  step to `build-freedos.ps1` mirroring the existing `move`/`sort` steps, and
  wire the built binary into `build-freedos-hdd-image.py` the same way.
