# Vendored FreeDOS source (corresponding source for shipped Toka-DOS binaries)

These trees are unmodified FreeDOS source from the FreeDOS 1.4 release, with Toka-DOS
rebrand edits applied on top (each modified file carries a "modified by the Toka-DOS
project, 2026" note; original FreeDOS/Villani copyright + GPL notices preserved verbatim).
FreeDOS is GPLv2-or-later; this project is GPL-3.0-only. This tree is the GPL
"corresponding source" for the committed crates/izarravm-firmware/roms/tokados-hdd.img.

- kernel:  github.com/FDOS/kernel  tag ke2043  commit 4f7bdda16a84c416a82a2616aa67335ca4f2bd74
- freecom: github.com/FDOS/freecom tag com086  commit f1b8f4f464eae5a70348b6d362484d733d45c427

Neither tag has git submodules. (Upstream master has since diverged: kernel 2045 / FreeCom 0.87.)

## Local patches applied to the vendored source
- Rebrand (Toka-DOS 3.0): kernel `hdr/version.h` (KVS banner), `kernel/main.c` (Toka signon banner — "General Simulation Works", tongue-in-cheek; the verbose FreeDOS/Villani copyright + GPL block was removed from the boot banner and replaced by a "See C:\LICENSE.TXT for more." pointer. The full GPL/copyright is preserved verbatim in C:\LICENSE.TXT, assembled by `scripts/license_txt.py` from the project NOTICE + kernel `COPYING` and shipped on the Katea C: payload); FreeCOM `shell/ver.c` (shellname/shellver), `VERSION.TXT`, `strings/DEFAULT.lng` (product strings; GPL/copyright preserved). Each edited file carries a "modified by the Toka-DOS project, 2026" note.
- Build fix: FreeCOM `shell/wlinker.bat` adds `op caseexact` — Open Watcom 2.0's wlink defaults to case-insensitive symbol resolution, which collides FreeCOM's libc toupper_/tolower_ with its own toUpper_/toLower_ (infinite recursion at the first console char-translation). Required for a working shell.
- Build target: kernel built XCPU=86 XFAT=32 (8086 + FAT32 => DOS 7.10), no UPX. XCPU=386 is NOT usable (emits 386 opcodes, e.g. PUSH FS, the emulator lacks).

## SP-3 userland vendored source
- move:    github.com/FDOS/move    tag v3.5a  commit 1e2de517   (+ kitten 3b9947fc, tnyprntf 450ab904)
- sort:    github.com/FDOS/sort    commit f55bb171 (self-IDs "v1.4"; no tag)   (+ kitten bd5695d8, tnyprntf 450ab904)
Rebrand: move src/version.h + move.c product-name string; sort src/sort.c banner. GPL/copyright headers preserved; modified files carry a "modified by the Toka-DOS project, 2026" note.

## Audit item 10 (MEM) vendored source
- mem: github.com/FDOS/mem commit 2b2c83328d9301aa0e484e909f252e32def6c2c7 (2021-02-14;
  no tags/releases exist upstream, this is the tip of `master`). Self-identifies
  MEM_VERSION "1.11". Ships its own `kitten.c`/`kitten.h` (a different, incompatible
  API from the move/sort kitten -- each MEM_OBJS is self-contained) and its own
  abbreviated-printf `prf.c` (Pasquale J. Villani, from DOS-C); no tnyprntf
  dependency. `source/test/` (upstream test fixtures, not part of the build) was
  dropped when vendoring.
- Build: Open Watcom `wcl`, small memory model (`-ms` -- required: `kitten.c` has
  a `sizeof(void*) == 2` static-assert-style array that only compiles under a
  16-bit-pointer model), mirroring `source/mkfiles/watcom.mak`'s CFLAGS
  (`-oahls -s -wx -we -zq -fm`). `mem.c` `#include`s `mem2.c` (one translation
  unit); `prf.c` and `kitten.c` are compiled and linked in separately, matching
  upstream's `MEM_OBJS=prf.obj kitten.obj $(MEMSUPT)` (MEMSUPT is empty for the
  Watcom target, so `memsupt.asm` is unused/unbuilt).
- Toka-DOS divergence from upstream switch semantics: upstream's MEM has NO `/P`
  switch. `/P` is a bare prefix match against `/PAGE` ("pause after each
  screenful"); the per-program size+segment listing lives under `/FULL`
  (new-style `/F`, or `/DEBUG`/new-style `/D` for the fuller device-inclusive
  form). The Toka-DOS spec requires `MEM /P` to list the programs in memory
  with their size and memory position, so `source/mem2.c`'s `main()` was
  patched (smallest possible change, commented "modified by the Toka-DOS
  project, 2026") to make `/PAGE` (and therefore its `/P` prefix) also imply
  `/FULL`, on top of upstream's original pagination behavior. `/FULL` itself
  (and `/DEBUG`) are unchanged and still work as upstream intends.
