<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Vendored MS-DOS 4.0 keyboard tables

The 16 `KDF*.ASM` files and two shared includes are unmodified files from
Microsoft's MS-DOS repository at commit `2d04cacc5322951f187bb17e017c12920ac8ebe2`.
The layout files came from `v4.0/src/DEV/KEYBOARD`; the repository is published
under the MIT license. The exact upstream permission text is preserved in
`LICENSE`. `tools/gen_keyboard_layouts.py` converts the tables into the firmware
format.
