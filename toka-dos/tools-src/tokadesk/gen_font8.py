#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

"""Emit font8.c from crates/izarravm-video/src/font.rs VGAFONT_8X8."""
import re
from pathlib import Path

root = Path(__file__).resolve().parents[3]
text = (root / "crates" / "izarravm-video" / "src" / "font.rs").read_text(encoding="utf-8")
match = re.search(r"pub const VGAFONT_8X8: \[u8; 2048\] = \[([\s\S]*?)\];", text)
if not match:
    raise SystemExit("VGAFONT_8X8 not found")
nums = re.findall(r"0x[0-9A-Fa-f]+", match.group(1))
if len(nums) != 2048:
    raise SystemExit("expected 2048 bytes, got %d" % len(nums))

lines = [
    "/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */",
    "/* SPDX-License-Identifier: GPL-3.0-only */",
    "",
    "/* CP437 8x8, same glyphs as izarravm-video VGAFONT_8X8. */",
    '#include "font.h"',
    "",
    "const unsigned char font8[256 * 8] = {",
]
for i in range(0, 2048, 16):
    lines.append("    " + ", ".join(nums[i : i + 16]) + ",")
lines.append("};")
lines.append("")
out = Path(__file__).with_name("font8.c")
out.write_text("\n".join(lines), encoding="utf-8")
print("wrote", out)
