<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# MS-DOS 4.0 XCOPY: investigated, rejected as a port target

Toka-DOS needed an XCOPY. FreeDOS's own XCOPY is GPL-2-only, which cannot be
vendored into this GPL-3 repository, so porting the MS-DOS 4.0 XCOPY source
was investigated as an alternative (an independent, differently-licensed
implementation with a documented upstream).

Investigated at the `microsoft/MS-DOS` GitHub mirror, commit `2d04cacc`,
path `v4.0/src/CMD/XCOPY`.

Rejected: disproportionate for what Toka-DOS needs. `XCOPY.ASM` is pure MASM
sharing a roughly 6,600-line command-line parser and message-catalog engine
(`PARSE.ASM`, `MSGSERV.ASM`, and friends) with the rest of the MS-DOS 4.0
`CMD` tree. That infrastructure isn't self-contained to XCOPY: pulling it in
means either dragging in the shared parser/catalog framework wholesale, or
picking it apart by hand. Worse, the message catalog is built by 1988-era
tools that only exist as binaries in the tree (no source, no modern
replacement), so a faithful build isn't reproducible with the Open Watcom
toolchain this project already uses for its other guest tools.

Decision: write a new, small, from-scratch XCOPY as original Toka-DOS project
code (GPL-3, `toka-dos/tools-src/xcopy/xcopy.c`), implemented directly from
documented XCOPY command-line behavior — not consulting or copying any
FreeDOS or MS-DOS XCOPY source.

If a future attempt wants to revisit porting the real MS-DOS 4.0 XCOPY, the
blocker to solve first is the binary-only 1988 message-catalog build tools;
until then, this note stands as the record of why that path was rejected.
