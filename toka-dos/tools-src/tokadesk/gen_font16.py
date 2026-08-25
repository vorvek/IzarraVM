#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

"""Emit font16.c from crates/izarravm-video/src/font.rs VGAFONT_8X16."""
import re
from pathlib import Path

root = Path(__file__).resolve().parents[3]
text = (root / "crates" / "izarravm-video" / "src" / "font.rs").read_text(encoding="utf-8")
match = re.search(r"pub const VGAFONT_8X16: \[u8; 4096\] = \[([\s\S]*?)\];", text)
if not match:
    raise SystemExit("VGAFONT_8X16 not found")
nums = re.findall(r"0x[0-9A-Fa-f]+", match.group(1))
if len(nums) != 4096:
    raise SystemExit("expected 4096 bytes, got %d" % len(nums))

lines = [
    "/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */",
    "/* SPDX-License-Identifier: GPL-3.0-only */",
    "",
    "/* CP437 8x16, same glyphs as izarravm-video VGAFONT_8X16. */",
    '#include "font.h"',
    "",
    "const unsigned char font16[256 * 16] = {",
]
for i in range(0, 4096, 16):
    lines.append("    " + ", ".join(nums[i : i + 16]) + ",")
lines.append("};")
lines.append("")
out = Path(__file__).with_name("font16.c")
out.write_text("\n".join(lines), encoding="utf-8")
print("wrote", out)
