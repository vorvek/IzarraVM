# Toka-DOS

The Izarra 3000's bundled operating system, an MS-DOS 6.22 clone. This tree holds
the source for the IZCMD shell, the boot record, the DOS tools, and the
packer that bundles them into the motherboard ROM image.

The product version shown on screen is "Toka-DOS v3.0". The DOS API version
reported to programs through INT 21h AH=30h stays 6.22, so software sees a
DOS 6.22-compatible system.

## Layout

- `runtime/` shared C runtime (`toka.h`, `toka.c`): INT 21h and INT 10h wrappers,
  command-tail parsing, EXEC. Linked into every binary.
- `izcmd/` the command interpreter (IZCMD.COM). COMMAND.COM is a duplicate.
- `boot/` TOKABOOT, the 512-byte boot record (NASM).
- `tools/` one C source per external tool (added per phase).
- `pack/` the ROM packer (authoring-only Rust binary).
- `build/` checked-in build outputs.
- `tokados.rom` checked-in packed ROM blob, embedded by `izarravm-firmware`.

## Building

Authoring only. CI does not build this; the binaries and `tokados.rom` are
checked in and embedded by the firmware crate, the same way the BIOS `.bin` is.

Requires Open Watcom (`D:\DevTools\OpenWatcom`) and NASM on PATH.

    pwsh toka-dos/build.ps1

The C tools build small model and link `system com` to produce tiny `.COM`
files. Do not pass `-mt` to `wcc`; tiny model comes from `wlink system com`.
