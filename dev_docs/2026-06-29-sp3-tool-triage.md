# SP-3 Tool-migration Triage

**Date:** 2026-06-29
**Status:** Decision record (executed incrementally after SP-3)

---

## Principle

Toka-DOS distinguishes two categories of external tool:

- **Branded OS components** (e.g. the shell, mouse driver, memory manager, CD-ROM
  extensions) carry a `toka-` primary name plus an MS-DOS classic alias
  (`TOKACMD`/`COMMAND`, `TOKAMOUS`/`MOUSE`, `IZEMM`, `IZCDEX`). These are
  Izarra-3000-specific and have no drop-in FreeDOS equivalent we want.
- **Generic command-line utilities** (file copy helpers, text filters, disk tools) keep
  their classic MS-DOS names (`MOVE.EXE`, `SORT.EXE`). No `toka-` prefix; they
  are not branded components.

The broader Toka-DOS userland is also gated on the storage and host-filesystem story:
SP-7 (host-folder redirector) is the prerequisite for shipping a full set of disk tools.
SP-3 therefore scopes narrowly to what fits on the boot floppy.

---

## SP-3 execution scope

SP-3 executed exactly three migrations:

| Tool | Action |
|---|---|
| `move.c` | Replaced by FreeDOS `MOVE.EXE` built from source (vendored `FDOS/move@v3.5a`) |
| `sort.c` | Replaced by FreeDOS `SORT.EXE` built from source (vendored `FDOS/sort@f55bb171`) |
| `izmouse.asm` | Rebranded to `TOKAMOUS.COM`; transient install code discarded, resident footprint reduced 3024 → 2912 bytes; loaded via `AUTOEXEC.BAT` |

Everything else below is decided here but executed incrementally in later sub-projects.

---

## Triage table

43 source files in `toka-dos/tools/` (42 `.c` + 1 `.asm`) plus `izcmd/izcmd.c`.

| Tool | File | Verdict | Reason |
|---|---|---|---|
| **KEEP (Toka infrastructure)** | | | |
| editor | `tools/editor.c` | KEEP | Full-screen VGA text editor with BIOS keyboard (INT 16h) and B800 display; no faithful FreeDOS analog. Rename `TOKAED` when carried forward. |
| help | `tools/help.c` | KEEP | Toka-DOS-specific command reference hardcoded to this OS; generic FreeDOS help covers different commands. Rename `TOKAHELP` when carried forward. |
| izemm | `tools/izemm.c` | KEEP | Queries INT 67h EMS state exposed by the IZEMM memory manager — Izarra-specific hardware/HLE. Already branded `IZEMM`. |
| izcdex | `tools/izcdex.c` | KEEP | Queries INT 2Fh CD-ROM state from IZCDEX; Izarra-specific. Already branded `IZCDEX`. |
| izbasic | `tools/izbasic.c` | KEEP (stub) | Izarra BASIC interpreter; currently a placeholder banner. Toka-specific branded component. Carry forward as `IZBASIC`. |
| keyb | `tools/keyb.c` | KEEP | Reads/writes CMOS 0x10/0x13 and BDA KB_LAYOUT — wired directly to Izarra BIOS layout + code-page scheme; no FreeDOS KEYB can do this. Rename `TOKAKEYB` when carried forward. |
| mem | `tools/mem.c` | KEEP | Walks the real MCB chain via INT 21h AH=52h, probes XMS and IZEMM EMS — a live hardware query, not cosmetic. Rename `TOKAMEM` when carried forward. |
| ps | `tools/ps.c` | KEEP | Reports the single-tasking Toka-DOS process state. Cosmetic today but architecturally a Toka OS component; carry forward. Rename `TOKAPS` when carried forward. |
| top | `tools/top.c` | KEEP | Same rationale as `ps`; complements it with CPU/resource view. Rename `TOKATOP` when carried forward. |
| **REPLACE-with-FreeDOS (later)** | | | |
| attrib | `tools/attrib.c` | REPLACE | Lists file attributes but treats set/clear as no-ops (Toka host does not model DOS attribute bits). FreeDOS `ATTRIB` is real. Replace when attribute bits are wired up. |
| chkdsk | `tools/chkdsk.c` | REPLACE | Counts files and sums sizes; result is cosmetic. FreeDOS `CHKDSK` does real FAT validation. Replace after SP-7 (real FAT on host volume). |
| choice | `tools/choice.c` | REPLACE | Functional but minimal (key set + ERRORLEVEL). FreeDOS `CHOICE` has /T timeout and richer UI. Replace when a FreeDOS CHOICE is vendored. |
| comp | `tools/comp.c` | REPLACE | Reads both files into 16 KB static buffers and byte-compares. FreeDOS `COMP` handles larger files and more options. Replace with FreeDOS version. |
| deltree | `tools/deltree.c` | REPLACE | Functional recursive delete. FreeDOS `DELTREE` is more complete. Replace with FreeDOS version. |
| doskey | `tools/doskey.c` | REPLACE | Cosmetic stub that prints "DOSKEY installed." A real DOSKEY TSR (command history + macros) is the correct long-term answer; use FreeDOS DOSKEY. |
| fc | `tools/fc.c` | REPLACE | Reads both files into 16 KB buffers, compares. FreeDOS `FC` handles line-mode, binary-mode, offset reporting. Replace with FreeDOS version. |
| find | `tools/find.c` | REPLACE | Functional line-search with /V /C /N. FreeDOS `FIND` is more complete. Replace; drop `findstr` overlap at the same time (see DROP). |
| label | `tools/label.c` | REPLACE | Cosmetic (hardcoded "TOKA-DOS" label; no real volume label storage). Replace after SP-7 when FAT volume labels are writable. |
| more | `tools/more.c` | REPLACE | Functional 23-line pager for named files. FreeDOS `MORE` handles stdin piping (once available). Replace with FreeDOS version. |
| replace | `tools/replace.c` | REPLACE | Functional byte-copy with "replaced" confirmation. FreeDOS `REPLACE` has add/update/subdirectory options. Replace with FreeDOS version. |
| tree | `tools/tree.c` | REPLACE | Functional directory tree walker. FreeDOS `TREE` has /F (files) and /A (ASCII art). Replace with FreeDOS version. |
| xcopy | `tools/xcopy.c` | REPLACE | Functional wildcard+/S copy. FreeDOS `XCOPY` has date/attribute filters and better error handling. Replace with FreeDOS version. |
| **DROP** | | | |
| append | `tools/append.c` | DROP | Self-described stub: "nothing is actually appended." No real search-path logic. Zero functional value. |
| assign | `tools/assign.c` | DROP | Self-described cosmetic: "ASSIGN is cosmetic on Toka-DOS." Toka has a single host-filesystem view; no drive redirection. |
| backup | `tools/backup.c` | DROP | Self-described cosmetic: walks directory and prints "Backing up NAME" but writes no archive. Misleading to ship. |
| debug | `tools/debug.c` | DROP | Stub: prints "Not yet implemented." A real debugger is not a near-term priority; drop the placeholder. |
| defrag | `tools/defrag.c` | DROP | Self-described cosmetic: "files are never fragmented." Prints a fake tidy report. Zero functional value. |
| diskcomp | `tools/diskcomp.c` | DROP | Self-described stub: "does no real floppy access." Prints fake "Compare OK." |
| diskcopy | `tools/diskcopy.c` | DROP | Cosmetic: "walks the operator through the motions" with no real floppy I/O. |
| edlin | `tools/edlin.c` | DROP | Stub: prints "Not yet implemented." `editor` (TOKAED) covers the editing use case. |
| exe2bin | `tools/exe2bin.c` | DROP | Strips MZ header to produce a flat binary. A developer utility with no user-facing DOS persona; not worth shipping as a general tool. |
| expand | `tools/expand.c` | DROP | "Expanding" is a straight file copy (files are never compressed). Misleading wrapper around COPY semantics. |
| fasthelp | `tools/fasthelp.c` | DROP | One-line help table duplicate of `help`; covered by `TOKAHELP`. Maintaining two help tools creates drift. |
| fastopen | `tools/fastopen.c` | DROP | Self-described cosmetic cache stub: "it installed" with no real directory cache. |
| findstr | `tools/findstr.c` | DROP | Overlaps `find` (which is being REPLACED with FreeDOS FIND). One well-specified tool is enough. |
| graphics | `tools/graphics.c` | DROP | Cosmetic: prints "GRAPHICS loaded." No printer-graphics mode changed. |
| mirror | `tools/mirror.c` | DROP | Cosmetic: prints "MIRROR process was successful" with no real disk imaging. |
| mode | `tools/mode.c` | DROP | Stub that reports "Columns=80 / Lines=25" and acknowledges arguments with "MODE set." No real device state changed. |
| restore | `tools/restore.c` | DROP | Cosmetic: prints "No backup files found." Companion to the dropped BACKUP. |
| setver | `tools/setver.c` | DROP | Cosmetic version table: no real per-program version map stored or read. |
| undelete | `tools/undelete.c` | DROP | Cosmetic: "no entries found" with no deletion log. |
| **DONE (SP-3)** | | | |
| izmouse | `tools/izmouse.asm` | DONE | Rebranded to `TOKAMOUS.COM`; transient install code discarded (resident footprint 3024 → 2912 bytes); ships on `tokados.img` via `AUTOEXEC.BAT` with `MOUSE.COM` alias. |
| move | `tools/move.c` | DONE | Superseded by FreeDOS `MOVE.EXE` built from source (`FDOS/move@v3.5a`). |
| sort | `tools/sort.c` | DONE | Superseded by FreeDOS `SORT.EXE` built from source (`FDOS/sort@f55bb171`). |
| **SUPERSEDED** | | | |
| izcmd | `izcmd/izcmd.c` | DROP (SUPERSEDED) | Superseded by `TOKACMD` (FreeCOM + branding), which is the shell shipped on `tokados.img` since SP-2. |

---

## Summary

| Verdict | Count |
|---|---|
| KEEP (Toka infrastructure) | 9 |
| REPLACE-with-FreeDOS (later) | 13 |
| DROP | 19 |
| DONE (SP-3) | 3 |
| SUPERSEDED (izcmd) | 1 |
| **Total** | **45** |

Source file count: 44 files in `toka-dos/tools/` (43 `.c` + 1 `.asm`) plus `toka-dos/izcmd/izcmd.c` = 45 files, one row each.
