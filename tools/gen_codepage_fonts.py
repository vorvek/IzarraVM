#!/usr/bin/env python3
# Authoring-only. Build crates/izarravm-firmware/roms/codepage-fonts.bin:
# five code pages (437,850,860,863,865) x three sizes (8x16,8x14,8x8).
# CP437 and every character also present in CP437 are copied from the shipped
# font (tools/cp437-fonts.bin) so CP437 stays byte-identical; the characters the
# alternate pages add beyond CP437 are rendered from the native Modern DOS TTFs.
# Two characters Modern DOS may lack get a built-in fallback.
import pathlib
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
TTF_BY_H = {16: "ModernDOS8x16.ttf", 14: "ModernDOS8x14.ttf", 8: "ModernDOS8x8.ttf"}
HEIGHTS = [16, 14, 8]  # 8x16, 8x14, 8x8 order within a code-page block
CPS = ["cp437", "cp850", "cp860", "cp863", "cp865"]

# Shipped CP437 glyphs: 8x8 (2048), 8x14 (3584), 8x16 (4096).
cp437_blob = (ROOT / "tools/cp437-fonts.bin").read_bytes()
assert len(cp437_blob) == 9728, len(cp437_blob)
CP437_BY_H = {
    8: cp437_blob[0:2048],
    14: cp437_blob[2048:2048 + 3584],
    16: cp437_blob[2048 + 3584:],
}


def cp437_glyph(h, byte):
    g = CP437_BY_H[h]
    return g[byte * h:(byte + 1) * h]


# Reverse map: Unicode char -> its CP437 byte (upper half 0x80..0xFF). The lower
# half 0x00..0x7F is identical across all five pages and copied by byte index.
cp437_char_to_byte = {}
for b in range(0x80, 0x100):
    try:
        cp437_char_to_byte[bytes([b]).decode("cp437")] = b
    except UnicodeDecodeError:
        pass

_fonts = {h: ImageFont.truetype(str(ROOT / "tools/fonts" / TTF_BY_H[h]), h) for h in HEIGHTS}


def render_extra(ch, h):
    # Render one character from the native Modern DOS TTF for height h into an
    # 8-wide, h-tall 1bpp glyph (50% threshold). Modern DOS covers all but two of
    # the codepoints the alternate pages add; those render blank and get a glyph
    # the code page expects: soft hyphen (U+00AD, blank at every size) shows as a
    # dash, and the spacing acute accent (U+00B4, blank only at 8px) as a small
    # top mark. Any other blank is a hard error so we never ship an empty cell.
    img = Image.new("L", (8, h), 0)
    ImageDraw.Draw(img).text((0, 0), ch, fill=255, font=_fonts[h])
    px = img.load()
    rows = []
    for y in range(h):
        bits = 0
        for x in range(8):
            if px[x, y] >= 128:
                bits |= 0x80 >> x
        rows.append(bits)
    if any(rows):
        return bytes(rows)
    if ch == "­":  # soft hyphen -> the DOS dash glyph
        return cp437_glyph(h, ord("-"))
    if ch == "´":  # acute accent -> a small right-leaning mark near the top
        out = [0] * h
        r = h // 8
        out[r] = 0x06
        out[r + 1] = 0x0C
        return bytes(out)
    sys.exit(f"Modern DOS has no glyph for U+{ord(ch):04X} at {h}px")


def glyph_for(cp, byte, h):
    if byte < 0x80:
        return cp437_glyph(h, byte)  # identical across pages
    try:
        ch = bytes([byte]).decode(cp)
    except UnicodeDecodeError:
        return cp437_glyph(h, byte)  # undefined slot: keep the cp437 cell
    if ch in cp437_char_to_byte:
        return cp437_glyph(h, cp437_char_to_byte[ch])
    return render_extra(ch, h)


def build():
    out = bytearray()
    for cp in CPS:
        for h in HEIGHTS:
            for byte in range(256):
                out += glyph_for(cp, byte, h)
    assert len(out) == 48640, len(out)
    (ROOT / "crates/izarravm-firmware/roms/codepage-fonts.bin").write_bytes(out)
    print(f"wrote {len(out)} bytes")


if __name__ == "__main__":
    build()
