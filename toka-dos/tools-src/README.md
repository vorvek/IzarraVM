<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# tools-src

Original Toka-DOS project source, licensed GPL-3 like the rest of this repo.
Unlike the vendored upstream trees under `toka-dos/freedos/` and
`toka-dos/msdos4/` (see their `VENDOR.md` files), nothing under here is a port
or derivative of another project's source. Each tool is written from
documented command-line behavior only.

- `xcopy/` `XCOPY.EXE`, a small-model Open Watcom C reimplementation of the
  classic DOS XCOPY command (see `xcopy/xcopy.c` for the switch list and the
  documented simplifications). `toka-dos/msdos4/VENDOR.md` records why the
  real MS-DOS 4.0 XCOPY source was investigated and rejected as a port
  target.
- `edit/` `EDIT.COM` ("TokaEdit"), a full-screen MS-DOS-EDIT-style editor:
  menus, dialogs, mouse, clipboard. Open Watcom large-model C; the ANSI-clean
  buffer core (`buffer.c`) is self-checked at build time by a native
  `test_buffer.c` harness.
- `tokadesk/` `TOKADESK.EXE`, the 32-bit visual workbench (Open Watcom `wcc386`
  payload plus a NASM VCPI stub). Authoring build: `tokadesk/build.ps1`.
