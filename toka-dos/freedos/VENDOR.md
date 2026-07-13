<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

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
- Toka-DOS changes `/P` handling. Upstream MEM treats `/P` as a prefix for
  `/PAGE`, which only pauses after each screenful. The program list normally
  needs `/FULL` or `/DEBUG`. Toka-DOS makes `/PAGE`, including its `/P` prefix,
  imply `/FULL` while keeping the pause. It also omits the default summary so
  the final program rows stay visible. `/P /SUMMARY` restores the summary.
  `/FULL` and `/DEBUG` still work by themselves.
- Toka-DOS replaces the default summary with a four-line, 79-column map for
  conventional, upper, EMS, and XMS memory. The kinds appear consecutively
  with BIOS attributes `0x09` (light blue), `0x0B` (light cyan), `0x0D`
  (light magenta), and `0x0A` (light green). CP437 `B2` marks used memory and
  CP437 `B0` marks free memory within each colored range. MEM uses 640 KiB
  conventional and 384 KiB upper categories. TOKAEMM supplies the 3 MiB EMS
  and 20 MiB XMS category sizes through its private XMS query on the 24 MiB
  machine. The upper region includes video memory and ROMs; only 96 KiB is
  available for UMB allocation with the default EMS frame, or 160 KiB under
  `NOEMS`. EMS has its own top-of-RAM partition. XMS and VCPI share the
  allocation arena inside the XMS category. Under `NOEMS`, the EMS category
  becomes zero and the XMS category grows to 23 MiB.

## Audit items 3+10 external tool batch (ATTRIB, CHOICE, MORE, FIND, DELTREE)

Target list was ATTRIB, CHOICE, MORE, FIND, DELTREE, XCOPY, TREE, LABEL, EDIT
(priority order). Five plus LABEL (six total) were vendored and built; XCOPY,
TREE, and EDIT were skipped -- see "Skipped tools" below. FORMAT/FDISK/SYS,
SMARTDRV, DEBUG, and QBASIC were explicitly out of scope for this batch.

- attrib:  github.com/FDOS/attrib  commit f670ebb0cb18dc08712bfcf56584ecb89e0bbd18
  (2015-09-27; no tags/releases exist upstream, this is the tip of `master`).
  GPLv2-or-later (Phil Brutsche, 1998; maintained by Brian E. Reifsnyder).
  Upstream ships only a Turbo C build (`CC.BAT`/`TURBOC.CFG`, plus
  `malloc.inc`/`setvbuf.inc`/`setupio.inc`/`stdio.inc` Borland-runtime shims
  guarded by `#ifdef __BORLANDC__`, so those never compile under Watcom and
  were not vendored). `_dos_findfirst`/`_dos_findnext`/`_dos_setfileattr` and
  the `_A_*` attribute constants are standard `<dos.h>` across DOS compilers
  (Turbo C and Open Watcom both ship them), so a Watcom port of the single
  `ATTRIB.C` source file was attempted and succeeded in two small patches
  (each commented "modified by the Toka-DOS project, 2026" in `ATTRIB.C`):
  an explicit `(ATTR)` cast on an `~0u`-into-`byte` init Turbo C accepted
  silently but Watcom's `-we` (warnings-as-errors) rejects, and a portable
  `stpcpy()` replacement (a GNU libc extension Turbo C's runtime happened to
  expose but Watcom's DOS target does not declare). No behavior change.
- choice:  github.com/FDOS/choice  commit fba777282f44aa2ab81d170b1f1655bfc7544473
  (2022-05-11; no tags/releases exist upstream, this is the tip of `master`).
  GPLv2-or-later (Jim Hall, 1994-2002). Vendors its own kitten (NLS) submodule
  pin, commit 62654352bd887fbed99db72bdb6c4dc3e99493a1 (shared with more/find
  below, which pin the identical commit), and tnyprntf pin 450ab904 (byte-
  identical to the tnyprntf already vendored under move/, reused rather than
  duplicated).
- more:    github.com/FDOS/more    commit a071c3b60993d32117372d24ab5bc19a9eb3cde9
  (2022-05-11; tip of `master`). GPLv2-or-later (Jim Hall, 1994-2002). Same
  kitten (62654352) and tnyprntf (450ab904) pins as choice, reused.
- find:    github.com/FDOS/find    commit a6e245d50bc6c4513651e65c3defab554be44da1
  (2022-02-18; tip of `master`). GPLv2-or-later (Jim Hall, 1994-2002; Eric
  Auer, 2003). Same kitten (62654352) and tnyprntf (450ab904) pins as choice,
  reused. Rebrand: the `usage()` banner ("FreeDOS Find, version 2.9" ->
  "Toka-DOS Find, version 2.9") in `src/find.c`, commented "modified by the
  Toka-DOS project, 2026".
- label:   github.com/FDOS/label   commit 21994ae096de6a74a0669fe641cc08aca7198202
  (tip of `master`; upstream also has tags up to v1.6, but master is ahead of
  v1.6 and carries no separate license change). GPLv2-or-later (Joe
  Cosentino/Brian E. Reifsnyder/Eric Auer). Pins kitten commit
  3b9947fc5ae08434d0d3f61cb2ea22d13911732d -- byte-identical to the kitten
  already vendored under move/kitten (same pin move uses), reused rather than
  duplicated -- and tnyprntf 450ab904, also reused from move/.
- deltree: github.com/FDOS/deltree commit ed472787c705ecc19c4d087f0ead1e6d599cad5d
  (2006-07-04; tip of `master`, no tags). GPLv2-or-later (C. Dye, 1998-2003).
  Pure NASM assembly (`deltree.asm`, upstream's own `.S` naming for NASM
  source), no C toolchain at all -- built directly with
  `nasm deltree.asm -o deltree.com` per the header's own build comment. Built
  the full-featured (non-`-DDEFANGED`) variant: `-DDEFANGED` only removes the
  `/Y` (assume-yes) switch, it does not change the default interactive Y/N
  delete confirmation, so the full variant matches real MS-DOS DELTREE
  behavior most closely per the project's emulation-fidelity policy.

Build: all four kitten-linked C tools (choice/more/find/label) use the same
Open Watcom `wcl` recipe as move/sort/mem -- small model (`-ms`), house-style
flags (`-bt=DOS -bcl=DOS -D__MSDOS__ -zp1 -ms -oas -s -wx -we -zq -fm`) rather
than upstream's own `build.sh` Linux/dosemu-Watcom flags (whose `-lr`, for
example, is not a real combined `wcl` flag on this Windows host). ATTRIB uses
the same flags without the kitten/tnyprntf link. DELTREE is NASM-only, no wcl
involved.

### Skipped tools

- **xcopy** (github.com/FDOS/xcopy): **license blocker**, not a build
  blocker -- it builds cleanly with the same Watcom recipe as the other
  kitten-linked tools (`source/xcopy.c` + `source/kitten.c` + `source/prf.c`,
  a `source/makefile` already targets `wcl`). `source/xcopy.c`'s own file
  header reads "under the terms of the GNU General Public License version 2
  as published by the Free Software Foundation" with NO "or (at your option)
  any later version" clause -- GPLv2-only (Rene Ableidinger, 2001-2003;
  bugfixes/kitten linking by Eric Auer, 2005). This repository is GPL-3.0-only
  (see LICENSE), so GPLv2-only source cannot be vendored per the project's
  attribution policy. Not vendored; skip the tool, note kept here per policy.
- **tree** (github.com/FDOS/tree, "pdTree" by Kenneth J. Davis): license is
  fine (`tree.cpp` itself is public domain / MIT-style permissive; only the
  optional Cats/NLS support files -- catgets.c/db.c/get_line.c -- are LGPL,
  and a NOCATS build sidesteps that entirely), but the **toolchain is not a
  reasonable Watcom port**: upstream targets Turbo C / Borland C++ / MSVC5
  only (`make.bat`/`makedos.bat`/`makwinvc.bat`/`makwinbc.bat`), the "DOS"
  build is actually a dual DOS-stub-plus-Win32-console executable built with
  a custom post-link stub-fixing tool (`extra\fixstub`), and `tree.cpp`
  unconditionally pulls in `<windows.h>`/`winbase.h` unless the bundled
  `w32fDOS.h`/`w32fDOS.cpp` per-memory-model shim is spliced in by hand. No
  Watcom makefile exists anywhere in the tree to adapt. Not attempted beyond
  this triage; foreign-toolchain porting effort judged out of proportion to
  the tool's value.
- **edit** (github.com/FDOS/edit, the FreeDOS Editor / Dflat-based full-screen
  editor): GPLv2-or-later license is fine, but it is a ~65-source-file GUI
  application (the whole Dflat text-mode window toolkit: menus, dialogs,
  listboxes, mouse, clipboard, help browser, etc.), LARGE memory model, and
  its own `source/makefile` says outright "For use with Turbo C 2.01, Turbo
  C++ 1.01, Turbo C/C++ 3.0 and Borland C/C++ 3.1 -- if you have another
  compiler, please send us your adjusted makefile". No Watcom port exists or
  was attempted; this is a heavy from-scratch port of a GUI framework, well
  beyond "reasonable effort" for this batch. Not attempted.

### Layout note

All eighteen files (kernel/shell/config plus the userland tool set) live flat
in the C:\ root; `scripts/build-freedos-hdd-image.py` has no subdirectory
support (the FAT32 builder only ever writes a root directory and per-file
cluster chains, no nested directory entries), so a `C:\DOS` layout was not
attempted for this batch. The root directory itself needed a fix to become a
proper multi-cluster chain (previously hardcoded to exactly cluster 2, which
held at most 16 entries at this image's 1-sector-per-cluster geometry) to fit
past the audit item 10 (MEM) baseline of 12 files; `CONFIG.SYS`/`AUTOEXEC.BAT`
already point `PATH` at `C:\`, so no PATH change was needed.
