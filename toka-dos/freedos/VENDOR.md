<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Vendored DOS source for shipped Toka-DOS binaries

The FreeDOS trees come from the FreeDOS 1.4 release with Toka-DOS rebrand edits
applied on top. Each modified file carries a "modified by the Toka-DOS project,
2026" note, and the original FreeDOS/Villani copyright and GPL notices remain
verbatim. FreeDOS is GPLv2-or-later; this project is GPL-3.0-only. These trees
are the GPL corresponding source for the committed
`crates/izarravm-firmware/roms/tokados-hdd.img`.

- kernel:  github.com/FDOS/kernel  tag ke2043  commit 4f7bdda16a84c416a82a2616aa67335ca4f2bd74
- freecom: github.com/FDOS/freecom tag com086  commit f1b8f4f464eae5a70348b6d362484d733d45c427

Neither tag has git submodules. (Upstream master has since diverged: kernel 2045 / FreeCom 0.87.)

## SHSUCDX CD-ROM redirector

- shsucd: github.com/adoxa/shsucd commit
  `5ea0787549f60270df5f70a0d6b8d4d9f5cb49da` (SHSUCDX 3.09, 2022-09-02).
  The complete upstream tree is vendored under `shsucd/`.
- License: the zlib-style permissive terms in `shsucd/LICENSE.txt`. The original
  copyright, credits, documentation, and redirector ABI are preserved.
- Build: NASM `-O9 -Di8086` produces the compatibility build. Toka-DOS ships it
  as `IZCDEX.COM`; the 386 build saves only about 140 bytes and does not build
  cleanly with the current NASM release.
- Local patch: `shsucdx.nsm` marks the altered source and changes only visible
  product strings from SHSUCDX to IZCDEX. The SHSUCDX v3 installation signature,
  feature set, command-line interface, and DOS redirector behavior stay intact.

## Local patches applied to the vendored source
- Rebrand (Toka-DOS 3.0): kernel `kernel/hdr/version.h` (KVS banner; also adds
  the `TOKA_BUILD_LINE_*` welcome-box strings -- `KERNEL_VERSION_STRING`/
  `os_release` themselves are untouched), `kernel/kernel/init-mod.h`
  (boot-screen styling constants -- `TOKA_BOX_W`, `TOKA_TREE_PREFIX`, and
  their derived `TOKA_BOX_INNER_W`/`TOKA_BOX_TEXT_END`/`TOKA_BOX_TEXT_W` --
  shared between `main.c` and `initdisk.c` within kernel-init only; userland
  `/T` tools will each carry their own copy of the prefix bytes -- kernel-init
  is not meant to be a project-wide single source), `kernel/kernel/main.c`
  (`signon()` rewritten: a rainbow TOKA logo via direct text-RAM writes, a
  CP437 welcome box (merged title line with build number + compile date, plus
  an Izarra SL copyright line naming LICENSE.TXT, shipped at C:\LICENSE.TXT),
  and a tree-styled kernel-compatibility line, all inside the kernel's 25-row
  boot budget; the verbose FreeDOS/Villani copyright + GPL block that used to
  print on the boot banner was removed and replaced by the "See LICENSE.TXT
  for more." pointer -- the full GPL/copyright is preserved verbatim in
  C:\LICENSE.TXT, assembled by `scripts/license_txt.py`
  from the project NOTICE + kernel `COPYING` and shipped on the Katea C:
  payload; the compiler `#if` chain that picked the banner's compiler name is
  narrowed to Watcom-only, the upstream BORLANDC/TURBOC/MSC/GNUC arms removed
  -- Toka-DOS only ships a Watcom build, so this is a deliberate upstream
  divergence, not dead-code drift), `kernel/kernel/initdisk.c` (the
  drive-assignment line restyled to match the boot tree -- one
  `TOKA_TREE_PREFIX`-led line per unit, the Pri/Ext partition tag and CHS
  geometry dropped per the boot-screen spec; the unterminated `" - InitDisk"`
  progress fragment `dsk_init()` used to print ahead of it was also dropped,
  since the styled screen has no free row for it to dangle on); FreeCOM
  `shell/ver.c` (shellname/shellver), `VERSION.TXT`, `strings/DEFAULT.lng`
  (product strings; GPL/copyright preserved); FreeCOM
  `shell/init.c` -- startup banner suppressed (silent start for the
  Toka-DOS boot tree); VER unchanged. Each edited file carries a
  "modified by the Toka-DOS project, 2026" note.
- Idle CPU behavior (predates the boot-screen campaign, missing from this
  ledger until now): kernel `kernel/kernel/config.c` (the `SHELL=` default
  changed from upstream's bare `command.com` to the full
  `C:\DOS\COMMAND.COM` path with tail ` C:\DOS /P /E:256`, ~line 138/156, so
  the F5 config-bypass load points at the same interpreter and directory as
  the shipped CONFIG.SYS `SHELL=` line and finds itself in `C:\DOS` too --
  the switches differ, though: the built-in default keeps upstream's
  `/E:256`, while the shipped CONFIG.SYS asks for `/E:2048`, so an F5 boot
  still gets a smaller 256-byte environment; the `IDLEHALT` default changed
  from 0 to 1 at
  ~line 844, "safe hooks" halt-on-CON-wait behavior, safe here because
  IzarraVM ships its own CPU and does not have the power-draw-transient
  hardware class upstream's off-by-default is guarding against; a new
  `idle_hlt()` inline-asm pragma at ~line 1045 wraps HLT in PUSHF/STI/POPF;
  the CON character-input wait loop at ~line 1061 calls it) and
  `kernel/kernel/main.c` (`HaltCpuWhileIdle` initialized to 1 at ~line 295,
  ahead of CONFIG.SYS parsing, matching the config.c default so the window
  between kernel entry and CONFIG.SYS is not spent busy-waiting either).
- Build fix: FreeCOM `shell/wlinker.bat` adds `op caseexact`. Open Watcom 2.0's wlink defaults to case-insensitive symbol resolution, which collides FreeCOM's libc toupper_/tolower_ with its own toUpper_/toLower_ (infinite recursion at the first console char-translation). Required for a working shell.
- Build target: kernel built XCPU=86 XFAT=32 (8086 + FAT32 => DOS 7.10), no UPX. XCPU=386 is NOT usable (emits 386 opcodes, e.g. PUSH FS, the emulator lacks).
- Toka-DOS changes the default `DIR` sort order. Upstream FreeCOM lists entries
  in raw on-disk order unless `/O` is given; Toka-DOS installs the `/O:NG`
  order (by name, directories grouped first) in `cmd_dir()` before the DIRCMD
  and command-line scans, so both still override it and `DIR /O:U` restores
  the unsorted listing. Because the sort buffers (a 64 KiB DOS block plus a
  ~3 KiB index) are now allocated on every `DIR`, the two allocation-failure
  paths in `dir_list()` fall back to the unsorted listing *silently* when the
  order was only the default, rather than printing an out-of-memory error for
  a sort the user never asked for. `scanOrder()`'s reverse-order test no longer
  reads the byte in front of its argument: for a command line that byte is
  merely uninteresting, but the default order is a string literal, and a '-'
  parked ahead of it by the linker would silently invert the listing to Z-to-A.
  `strings/DEFAULT.lng` notes the new default in the `DIR` help; the other
  language files are untouched.

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
  the final program rows stay visible, and splits the listing into conventional
  and upper-memory sections. `/P /SUMMARY` restores the summary. `/FULL` and
  `/DEBUG` still work by themselves. MEM also identifies the HMA resident as
  Toka-DOS instead of the upstream FreeDOS product name.
- Toka-DOS replaces the default summary with a four-line, 79-column map for
  conventional, upper, EMS, and XMS memory. The kinds appear consecutively
  with BIOS attributes `0x09` (light blue), `0x0B` (light cyan), `0x0D`
  (light magenta), and `0x0A` (light green). CP437 `B2` marks used memory and
  CP437 `B0` marks free memory within each colored range. MEM uses 640 KiB
  conventional and 384 KiB upper categories. TOKAEMM supplies the 3 MiB EMS
  and 20 MiB XMS category sizes through its private XMS query on the 24 MiB
  machine. The upper region includes video memory and ROMs; only 96 KiB is
  available for UMB allocation with the default EMS frame, or 160 KiB under
  `NOEMS`. EMS has its own top-of-RAM partition. Standard XMS blocks use up to
  2 MiB and VCPI owns the rest of the extended category. Under `NOEMS`, the
  EMS category becomes zero and the extended category grows to 23 MiB.
  TOKAEMM's versioned private query returns free VCPI memory so MEM can add it
  to standard XMS free space. The MEM executable and driver must be rebuilt
  and shipped together. Pairing an older MEM with the new driver is safe, but
  the old program may display free VCPI pages as used.

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

`scripts/build-freedos-hdd-image.py` builds a `C:\DOS` subdirectory -- the
`dos_files` list (:385) is sized into
`dos_first`/`dos_entries`/`dos_clusters` (:458-461) and linked into the root
via the hardcoded `dir_entry(name11("DOS"), dos_first, 0, ATTR_SUBDIR)`
(:470) -- and the current image uses it. The root holds KERNEL.SYS,
CONFIG.SYS, AUTOEXEC.BAT, LICENSE.TXT, and that DOS subdirectory entry
itself (5 entries, one cluster). `C:\DOS` holds COMMAND.COM, every
command-line tool, the two drivers (TOKAEMM.SYS, TOKACD.SYS), and
HELLO.TXT -- everything else in the eighteen-plus-file set above, not just
the command-line tools. `CONFIG.SYS`'s `SHELL=` and `AUTOEXEC.BAT`'s `PATH`
both point at `C:\DOS` (see `kernel/kernel/config.c`'s built-in `SHELL=`
default in the "Idle CPU behavior" entry above, which points at the same
interpreter and directory -- its switches differ, see that entry).

Historical: the root directory once needed a fix to become a proper
multi-cluster chain (previously hardcoded to exactly cluster 2, which held
at most 16 entries at this image's 1-sector-per-cluster geometry) to fit
past the audit item 10 (MEM) baseline of 12 files. That fix predates the
`C:\DOS` subdirectory work and is no longer load-bearing for the root itself:
now that the command-line tools and drivers live in `C:\DOS`, the shipped
root is back down to 5 entries (a single cluster) and `C:\DOS` -- with its
20 files plus `.`/`..` -- is the multi-cluster chain instead.
