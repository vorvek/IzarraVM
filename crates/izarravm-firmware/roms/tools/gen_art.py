#!/usr/bin/env python3
"""Generate izbios-art.inc for the Izarra-BIOS v3.01 graphical POST screen.

One-off, run by hand; its output (izbios-art.inc) is committed. No build.rs.

Quantizes the three 320x240 reference images to ONE shared palette (they use 31
colours total, so this is an exact index map, not lossy quantization) and emits:

  * art_palette       - ART_PAL_COUNT entries of 6-bit DAC RGB (for INT 10h 1012h)
  * art_bg_rle        - izarra3000.png (grey, full screen) as flat [count,value] RLE
  * art_icon_0..6_rle - the 7 colour icon cells from izarra3000_color.png, + geometry
  * art_bootbox_rle   - the boot-box frame from izarra3000_boot.png, + geometry
  * %defines          - ART_PAL_COUNT, ART_FIELD_INDEX, ART_INK_INDEX, ICON_n_X/Y/W/H,
                        BOOTBOX_X/Y/W/H

RLE: a flat byte stream of [count, value] runs, count 1..255 (longer runs split),
count never 0; runs may cross row boundaries. The decoder (lfb_blit_rle) writes
exactly w*h pixels. The laziest scheme that crushes the flat cream field.

Usage:  python gen_art.py [--src DIR] [--out izbios-art.inc]
        --src defaults to the user's Desktop (where the reference art lives).
"""
import argparse
import os
import re
import sys

try:
    from PIL import Image
except ImportError:
    sys.exit("Pillow is required: pip install Pillow")

FIELD = (236, 230, 223)  # the cream background colour
ICON_BODY_Y = (167, 191)  # rows that hold the icon bodies (excludes the y=161 rule)
ICON_PAD_Y = (166, 192)   # extracted cell rows (1px margin top/bottom)


def load(src, name):
    return Image.open(os.path.join(src, name)).convert("RGB")


def build_palette(images):
    """Shared palette = sorted unique colours across all images."""
    seen = set()
    for im in images:
        seen.update(im.getdata())
    pal = sorted(seen)
    if len(pal) > 256:
        sys.exit(f"palette has {len(pal)} colours (>256) — need real quantization")
    index = {c: i for i, c in enumerate(pal)}
    return pal, index


def index_region(im, index, x0, y0, w, h):
    """Return a flat row-major list of palette indices for the sub-rect."""
    px = im.load()
    return [index[px[x0 + c, y0 + r]] for r in range(h) for c in range(w)]


def rle(indices):
    """Flat [count, value] RLE, count 1..255, never 0."""
    out = bytearray()
    i, n = 0, len(indices)
    while i < n:
        v = indices[i]
        run = 1
        while i + run < n and indices[i + run] == v and run < 255:
            run += 1
        out += bytes((run, v))
        i += run
    return bytes(out)


def detect_icons(grey):
    """Find the 7 icon cells: non-field column runs within the icon body rows."""
    px = grey.load()
    W, _ = grey.size
    y0, y1 = ICON_BODY_Y
    # Scan only the icon strip; the mascot's lower body (x>=232) also dips into
    # these rows, so cap the scan before it.
    x_max = 226
    occupied = [
        x < x_max and any(px[x, y] != FIELD for y in range(y0, y1))
        for x in range(W)
    ]
    cells, run_start = [], None
    for x in range(W):
        if occupied[x] and run_start is None:
            run_start = x
        elif not occupied[x] and run_start is not None:
            cells.append((run_start, x - 1))
            run_start = None
    if run_start is not None:
        cells.append((run_start, W - 1))
    # Merge cells separated by a gap of <=2 columns (icons that nearly touch don't
    # happen here, but split on gap>2 keeps the rule line — already excluded — out).
    merged = [cells[0]]
    for a, b in cells[1:]:
        if a - merged[-1][1] <= 2:
            merged[-1] = (merged[-1][0], b)
        else:
            merged.append((a, b))
    if len(merged) != 7:
        sys.exit(f"expected 7 icon cells, found {len(merged)}: {merged}")
    # Pad x by 1 each side, fixed padded y rows.
    py0, py1 = ICON_PAD_Y
    rects = []
    for a, b in merged:
        x = max(0, a - 1)
        w = min(grey.size[0] - x, (b + 1) - x + 1)
        rects.append((x, py0, w, py1 - py0))
    return rects


def detect_bootbox(boot):
    """Bbox of the boot-box frame: non-field pixels in the left region."""
    px = boot.load()
    W, H = boot.size
    nf = [
        (x, y)
        for x in range(5, 200)
        for y in range(40, 235)
        if px[x, y] != FIELD
    ]
    if not nf:
        sys.exit("no boot-box frame detected")
    xs = [p[0] for p in nf]
    ys = [p[1] for p in nf]
    x0, y0, x1, y1 = min(xs), min(ys), max(xs), max(ys)
    return (x0, y0, x1 - x0 + 1, y1 - y0 + 1)


def emit_db(name, blob):
    lines = [f"{name}:"]
    for i in range(0, len(blob), 16):
        row = ", ".join(f"0x{b:02x}" for b in blob[i : i + 16])
        lines.append(f"    db {row}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    default_src = os.path.join(os.path.expanduser("~"), "Desktop")
    ap.add_argument("--src", default=default_src)
    ap.add_argument(
        "--out",
        default=os.path.join(os.path.dirname(__file__), "..", "izbios-art.inc"),
    )
    args = ap.parse_args()

    grey = load(args.src, "izarra3000.png")
    color = load(args.src, "izarra3000_color.png")
    boot = load(args.src, "izarra3000_boot.png")
    for im in (grey, color, boot):
        if im.size != (320, 240):
            sys.exit(f"image is {im.size}, expected (320, 240)")

    pal, index = build_palette([grey, color, boot])
    field_idx = index[FIELD]
    ink_idx = min(range(len(pal)), key=lambda i: sum(pal[i]))
    # A strong red (for menu titles) and a mid grey (for disabled menu rows), used
    # by the LFB boot menu. Picked from the shared palette so they survive a regen.
    red_idx = max(range(len(pal)), key=lambda i: pal[i][0] - max(pal[i][1], pal[i][2]))
    grey_idx = min(
        range(len(pal)),
        key=lambda i: (max(pal[i]) - min(pal[i])) + abs(sum(pal[i]) / 3 - 150),
    )

    bg_rle = rle(index_region(grey, index, 0, 0, 320, 240))
    icons = detect_icons(grey)
    icon_rle = [
        rle(index_region(color, index, x, y, w, h)) for (x, y, w, h) in icons
    ]
    bx, by, bw, bh = detect_bootbox(boot)
    box_rle = rle(index_region(boot, index, bx, by, bw, bh))

    # --- assemble the .inc -------------------------------------------------
    out = []
    out.append(
        "; izbios-art.inc - GENERATED by tools/gen_art.py (do not hand-edit).\n"
        "; Shared palette + RLE assets for the v3.01 graphical POST screen.\n"
        "; Palette: 6-bit DAC RGB triples for INT 10h AX=1012h. RLE: flat\n"
        "; [count,value] runs (count 1..255), decoded by lfb_blit_rle.\n"
    )
    out.append(f"%define ART_PAL_COUNT   {len(pal)}")
    out.append(f"%define ART_FIELD_INDEX {field_idx}")
    out.append(f"%define ART_INK_INDEX   {ink_idx}")
    out.append(f"%define ART_RED_INDEX   {red_idx}")
    out.append(f"%define ART_GREY_INDEX  {grey_idx}")
    for i, (x, y, w, h) in enumerate(icons):
        out.append(
            f"%define ICON_{i}_X {x}\n%define ICON_{i}_Y {y}\n"
            f"%define ICON_{i}_W {w}\n%define ICON_{i}_H {h}"
        )
    out.append(
        f"%define BOOTBOX_X {bx}\n%define BOOTBOX_Y {by}\n"
        f"%define BOOTBOX_W {bw}\n%define BOOTBOX_H {bh}"
    )
    out.append("")
    palbytes = bytearray()
    for r, g, b in pal:
        palbytes += bytes((r >> 2, g >> 2, b >> 2))
    out.append(emit_db("art_palette", palbytes))
    out.append("")
    out.append(emit_db("art_bg_rle", bg_rle))
    for i, blob in enumerate(icon_rle):
        out.append("")
        out.append(emit_db(f"art_icon_{i}_rle", blob))
    out.append("")
    out.append(emit_db("art_bootbox_rle", box_rle))
    out.append("")

    text = "\n".join(out)
    outpath = os.path.normpath(args.out)
    with open(outpath, "w", newline="\n") as f:
        f.write(text)

    # --- summary ----------------------------------------------------------
    total = len(palbytes) + len(bg_rle) + sum(map(len, icon_rle)) + len(box_rle)
    print(f"wrote {outpath}")
    print(f"palette: {len(pal)} colours  field_idx={field_idx} ink_idx={ink_idx}")
    print(f"art_palette : {len(palbytes)} B")
    print(f"art_bg_rle  : {len(bg_rle)} B  ({320*240} px)")
    for i, (rect, blob) in enumerate(zip(icons, icon_rle)):
        print(f"art_icon_{i}  : {len(blob):4d} B  cell x={rect[0]} y={rect[1]} w={rect[2]} h={rect[3]}")
    print(f"art_bootbox : {len(box_rle)} B  box x={bx} y={by} w={bw} h={bh}")
    print(f"TOTAL data  : {total} B  (ROM headroom before 0xF000 is ~45 KB)")


if __name__ == "__main__":
    main()
