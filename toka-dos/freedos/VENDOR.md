# Vendored FreeDOS source (corresponding source for shipped Toka-DOS binaries)

These trees are unmodified FreeDOS source from the FreeDOS 1.4 release, with Toka-DOS
rebrand edits applied on top (each modified file carries a "modified by the Toka-DOS
project, 2026" note; original FreeDOS/Villani copyright + GPL notices preserved verbatim).
FreeDOS is GPLv2-or-later; this project is GPL-3.0-only. This tree is the GPL
"corresponding source" for the committed crates/izarravm-firmware/roms/tokados.img.

- kernel:  github.com/FDOS/kernel  tag ke2043  commit 4f7bdda16a84c416a82a2616aa67335ca4f2bd74
- freecom: github.com/FDOS/freecom tag com086  commit f1b8f4f464eae5a70348b6d362484d733d45c427

Neither tag has git submodules. (Upstream master has since diverged: kernel 2045 / FreeCom 0.87.)

## Local patches applied to the vendored source
- Rebrand (Toka-DOS 3.0): kernel `hdr/version.h` (KVS banner), `kernel/main.c` (added Toka banner; FreeDOS copyright preserved); FreeCOM `shell/ver.c` (shellname/shellver), `VERSION.TXT`, `strings/DEFAULT.lng` (product strings; GPL/copyright preserved). Each edited file carries a "modified by the Toka-DOS project, 2026" note.
- Build fix: FreeCOM `shell/wlinker.bat` adds `op caseexact` — Open Watcom 2.0's wlink defaults to case-insensitive symbol resolution, which collides FreeCOM's libc toupper_/tolower_ with its own toUpper_/toLower_ (infinite recursion at the first console char-translation). Required for a working shell.
- Build target: kernel built XCPU=86 XFAT=32 (8086 + FAT32 => DOS 7.10), no UPX. XCPU=386 is NOT usable (emits 386 opcodes, e.g. PUSH FS, the emulator lacks).

## SP-3 userland vendored source
- move:    github.com/FDOS/move    tag v3.5a  commit 1e2de517   (+ kitten 3b9947fc, tnyprntf 450ab904)
- sort:    github.com/FDOS/sort    commit f55bb171 (self-IDs "v1.4"; no tag)   (+ kitten bd5695d8, tnyprntf 450ab904)
Rebrand: move src/version.h + move.c product-name string; sort src/sort.c banner. GPL/copyright headers preserved; modified files carry a "modified by the Toka-DOS project, 2026" note.
