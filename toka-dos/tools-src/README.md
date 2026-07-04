# tools-src

Original Toka-DOS project source, licensed GPL-3 like the rest of this repo.
Unlike `toka-dos/freedos/` (vendored upstream FreeDOS/MS-DOS-4.0 userland,
GPL-2-only, see each subdir's `VENDOR.md`), nothing under here is a port or
derivative of another project's source. Each tool is written from documented
command-line behavior only.

- `xcopy/` `XCOPY.EXE`, a small-model Open Watcom C reimplementation of the
  classic DOS XCOPY command (see `xcopy/xcopy.c` for the switch list and the
  documented simplifications). `toka-dos/msdos4/VENDOR.md` records why the
  real MS-DOS 4.0 XCOPY source was investigated and rejected as a port
  target.
- `edit/` `EDIT.COM` ("TokaEdit"), a full-screen MS-DOS-EDIT-style editor:
  menus, dialogs, mouse, clipboard. Open Watcom large-model C; the ANSI-clean
  buffer core (`buffer.c`) is self-checked at build time by a native
  `test_buffer.c` harness. Design: `dev_docs/2026-07-03-tokados-edit-design.md`.
