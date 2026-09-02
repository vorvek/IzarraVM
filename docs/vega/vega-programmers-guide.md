<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# VEGA for Programmers

This guide tells you how to draw on the Izarra3000 through the VEGA chipset.
The other volume, the VEGA Technical Reference, is the register-level
contract. This guide shows you how to use that contract.

All of this guide is about **Margo**, the 2D engine. Distira, the 3D engine,
gets its own guide in a later revision.

## How to read the examples

Each example has a tag:

- **(verified)** means that the example ran on the machine and gave the result
  in the text.
- **(target)** means that the example shows the documented interface before
  the implementation. The register sequence obeys the Technical Reference, but
  your build can be without the operation. Read `CAPS` (offset `0x0004`) to
  find the operations of your build.

When an operation is complete, its example changes from (target) to
(verified). No example in this guide claims a result that the hardware does
not give.

## The shape of the hardware

Margo gives you three things:

1. A **linear frame buffer** at `0xE0000000`. After you set a graphics mode,
   this buffer is the screen, as a flat array of pixels. You can write a pixel
   to it directly.
2. A **256-color palette** for the 8-bit modes, through the VGA DAC ports.
3. A **blit engine**, at the register block `0xE0400000`. It fills, copies,
   draws text, and draws lines. The CPU does not touch each pixel.

The fast method for a desktop is the blit engine. The CPU writes a small
number of registers, and then writes a command. The engine then does the work.
On a slow CPU, for example in Izarra1000 compatibility mode, this is the
difference between a fast desktop and a slow one.

The engine operates while the CPU does other work. An operation needs time.
Thus a program starts the operation, prepares the next one, and waits on
`BUSY` only when it needs the result. This overlap gives the speed.

## A convention for the examples

The examples use these definitions. They assume a flat-mode or protected-mode
program that can reach the frame buffer and the register block.

```c
#define LFB        ((volatile unsigned char *)0xE0000000)
#define MARGO_BASE 0xE0400000
#define REG(off)   (((volatile unsigned long *)MARGO_BASE)[(off) >> 2])

/* Wait for the blit engine to go idle. */
static void margo_wait(void) {
    while (REG(0x0008) & 1)   /* STATUS.BUSY */
        ;
}
```

## How to set a mode

Set a mode through the VESA BIOS, with `INT 10h` and `AX = 4F02h`. Set bit 14
of the mode number to request the linear frame buffer.

This example selects mode `0x101`, which is 640x480 at 8-bit color, with the
linear frame buffer.

```c
/* (verified) */
#include <dos.h>

void set_mode_640x480x8(void) {
    union REGS r;
    r.x.ax = 0x4F02;
    r.x.bx = 0x0101 | 0x4000;   /* mode 0x101, linear frame buffer */
    int86(0x10, &r, &r);
    /* r.x.ax == 0x004F on success */
}
```

After you set the mode, the display registers describe it. Read `DISP_WIDTH`,
`DISP_HEIGHT`, `DISP_BPP`, and `DISP_PITCH`. Do not assume these values. Then
the same drawing code operates in each mode.

## How to load the palette

In an 8-bit mode, a pixel value is an index into the DAC. To load a color,
write one time to the index port, and then three times to the data port, in
the order red, green, blue. Each component is from 0 to 63.

```c
/* (target) */
#include <conio.h>

void set_palette_entry(int index, int r, int g, int b) {
    outp(0x03C8, index);
    outp(0x03C9, r);
    outp(0x03C9, g);
    outp(0x03C9, b);
}
```

## How to write pixels directly

The linear frame buffer is memory. The address of a pixel is its offset:
`y * pitch + x * bytes_per_pixel`.

```c
/* (target) */
void plot8(int x, int y, int pitch, unsigned char color) {
    LFB[y * pitch + x] = color;
}
```

A direct write is sufficient for a small number of pixels. For a rectangle,
for text, and for a scroll, the blit engine is much faster. The remainder of
this guide is about the blit engine.

## How to write pixels in a hi-color mode

A 16-bit mode holds each pixel as `R5G6B5`, in two bytes, with no palette. Set
the mode. Then pack the 8-bit color components into 5/6/5, and write the
16-bit value.

```c
/* (verified) */
#include <dos.h>

void set_mode_640x480x16(void) {
    union REGS r;
    r.x.ax = 0x4F02;
    r.x.bx = 0x0111 | 0x4000;   /* mode 0x111 (R5G6B5), linear frame buffer */
    int86(0x10, &r, &r);
}

void plot16(int x, int y, int pitch, int red, int green, int blue) {
    unsigned short pixel = ((red >> 3) << 11) | ((green >> 2) << 5) | (blue >> 3);
    unsigned short *p = (unsigned short *)(LFB + y * pitch + x * 2);
    *p = pixel;
}
```

A 15-bit mode (`0x110`) has the layout `X1R5G5B5`. Pack it as
`((red >> 3) << 10) | ((green >> 3) << 5) | (blue >> 3)`. Read `DISP_BPP` and
the color masks of the mode (VBE `4F01h`). Do not assume the format.

## How to fill a rectangle

The engine model is the same for each operation: write the parameters, write
the command, and then wait for idle. A solid fill uses `FG_COLOR` and the
`PATCOPY` raster operation.

```c
/* (verified) */
void fill_rect(unsigned long base, int pitch, int bpp,
               int x, int y, int w, int h, unsigned long color) {
    margo_wait();
    REG(0x0100) = base;                 /* DST_BASE */
    REG(0x0104) = pitch;                /* DST_PITCH */
    REG(0x0110) = bpp / 8;              /* DEPTH in bytes */
    REG(0x0114) = (y << 16) | x;        /* DST_XY */
    REG(0x011C) = (h << 16) | w;        /* DIM */
    REG(0x0120) = color;                /* FG_COLOR */
    REG(0x0128) = 0xF0;                 /* ROP = PATCOPY */
    REG(0x0130) = 0;                    /* FLAGS: none */
    REG(0x0150) = 0x01;                 /* COMMAND = FILL */
    margo_wait();
}
```

## How to copy and scroll

`COPY` moves a rectangle from one position in the frame store to another. The
source and the destination can overlap. Thus a screen can scroll: it copies
itself, with an offset of one text line. The engine obeys the overlap.

```c
/* (verified) */
void copy_rect(unsigned long base, int pitch, int bpp,
               int sx, int sy, int dx, int dy, int w, int h) {
    margo_wait();
    REG(0x0100) = base;                 /* DST_BASE */
    REG(0x0104) = pitch;                /* DST_PITCH */
    REG(0x0108) = base;                 /* SRC_BASE (same surface) */
    REG(0x010C) = pitch;                /* SRC_PITCH */
    REG(0x0110) = bpp / 8;              /* DEPTH */
    REG(0x0114) = (dy << 16) | dx;      /* DST_XY */
    REG(0x0118) = (sy << 16) | sx;      /* SRC_XY */
    REG(0x011C) = (h << 16) | w;        /* DIM */
    REG(0x0128) = 0xCC;                 /* ROP = SRCCOPY */
    REG(0x0130) = 0;                    /* FLAGS: none */
    REG(0x0150) = 0x02;                 /* COMMAND = COPY */
    margo_wait();
}
```

To blit an icon with a transparent color, set `COLORKEY` to that color, and
set `COLORKEY_EN` in `FLAGS`. The engine does not write a source pixel with
that value.

## How to draw text

Text is a monochrome bitmap that the engine expands into two colors. Set
`FG_COLOR` and `BG_COLOR`. Then write `COLOR_EXPAND_DATA`. Then send the glyph
bits through `MONO_DATA`, one 32-bit word at a time, with the most significant
bit first. Each row starts on a word boundary.

An 8x8 font has eight bytes for each glyph, one for each row. Each row needs
one word, with 8 bits and padding. Set `EXPAND_TRANSPARENT` to draw the glyph
above the current screen contents. The engine then does not change the
background.

```c
/* (verified) */
void draw_glyph_8x8(unsigned long base, int pitch, int bpp,
                    int x, int y, const unsigned char glyph[8],
                    unsigned long fg) {
    int row;
    margo_wait();
    REG(0x0100) = base;                 /* DST_BASE */
    REG(0x0104) = pitch;                /* DST_PITCH */
    REG(0x0110) = bpp / 8;              /* DEPTH */
    REG(0x0114) = (y << 16) | x;        /* DST_XY */
    REG(0x011C) = (8 << 16) | 8;        /* DIM = 8x8 */
    REG(0x0120) = fg;                   /* FG_COLOR */
    REG(0x0130) = 0x04;                 /* FLAGS = EXPAND_TRANSPARENT */
    REG(0x0128) = 0xCC;                 /* ROP = SRCCOPY (S = expanded pixel) */
    REG(0x0150) = 0x03;                 /* COMMAND = COLOR_EXPAND_DATA */
    for (row = 0; row < 8; row++)
        REG(0x0160) = (unsigned long)glyph[row] << 24;  /* bits in the high byte */
    margo_wait();
}
```

## How to draw lines

`LINE` draws between two points, in `FG_COLOR`. With the ROP `0x5A`
(`PATINVERT`), the line does an exclusive-OR into the screen. This is the
classic method to draw and erase a rubber-band selection. You do not have to
save the background.

```c
/* (verified) */
void draw_line(unsigned long base, int pitch, int bpp,
               int x0, int y0, int x1, int y1, unsigned long color) {
    margo_wait();
    REG(0x0100) = base;                 /* DST_BASE */
    REG(0x0104) = pitch;                /* DST_PITCH */
    REG(0x0110) = bpp / 8;              /* DEPTH */
    REG(0x013C) = (y0 << 16) | x0;      /* LINE_START */
    REG(0x0140) = (y1 << 16) | x1;      /* LINE_END */
    REG(0x0120) = color;                /* FG_COLOR */
    REG(0x0128) = 0xF0;                 /* ROP = PATCOPY (solid; LINE has no source) */
    REG(0x0150) = 0x05;                 /* COMMAND = LINE */
    margo_wait();
}
```

## How to clip

Set `CLIP_TL` and `CLIP_BR` to a rectangle, and set `CLIP_EN` in `FLAGS`. Each
operation then stays in that rectangle. A window manager sets the clip to the
visible area of a window. It can then draw without a check of the edges.

```c
/* (verified) */
void set_clip(int x0, int y0, int x1, int y1) {
    REG(0x0134) = (y0 << 16) | x0;      /* CLIP_TL, inclusive */
    REG(0x0138) = (y1 << 16) | x1;      /* CLIP_BR, exclusive */
    /* OR CLIP_EN into FLAGS on the next operation */
}
```

## How to tile a pattern

`PATTERN_FILL` tiles an 8x8 pattern across a rectangle, in place of a solid
color. First put the 8x8 tile in offscreen memory, in the pixel format of the
screen. Then set `PAT_BASE` to its address. The tiling aligns to the origin of
the surface. Thus two adjacent fills join with no visible seam.

```c
/* (verified) */
void pattern_fill(unsigned long base, int pitch, int bpp,
                  unsigned long pat_offset,
                  int x, int y, int w, int h) {
    margo_wait();
    REG(0x0100) = base;                 /* DST_BASE */
    REG(0x0104) = pitch;                /* DST_PITCH */
    REG(0x0110) = bpp / 8;              /* DEPTH */
    REG(0x0114) = (y << 16) | x;        /* DST_XY */
    REG(0x011C) = (h << 16) | w;        /* DIM */
    REG(0x0144) = pat_offset;           /* PAT_BASE: the 8x8 tile */
    REG(0x0128) = 0xF0;                 /* ROP = PATCOPY (P = pattern, no source) */
    REG(0x0150) = 0x06;                 /* COMMAND = PATTERN_FILL */
    margo_wait();
}
```

## The hardware cursor

The cursor is a 64x64 two-plane bitmap in offscreen memory, and a position.
Set the engine to the bitmap, set the two colors, and enable the cursor. After
that, a move of the pointer is one register write for each frame, and the CPU
does not touch the screen below it.

```c
/* (verified) */
void enable_cursor(unsigned long bitmap_offset,
                   unsigned long fg, unsigned long bg) {
    REG(0x002C) = bitmap_offset;        /* CURSOR_ADDR: 64x64 two-plane (AND then XOR) */
    REG(0x0034) = fg;                   /* CURSOR_FG */
    REG(0x0038) = bg;                   /* CURSOR_BG */
    REG(0x0028) = 1;                    /* CURSOR_CTRL = ENABLE */
}

void move_cursor(int x, int y) {
    REG(0x0030) = ((y & 0xFFFF) << 16) | (x & 0xFFFF);   /* CURSOR_POS */
}
```

## How to play video through the overlay

The overlay takes a YUV image, converts it to RGB, and scales it into a
window. It does all of this in hardware. Decode each frame into a YUV buffer
in offscreen memory. Set the overlay to that buffer, and key it through the
desktop. To show the overlay, write the color key into the window. To hide a
region, draw over the key in the usual way.

```c
/* (verified) */
/* Show a YUY2 frame (already in offscreen memory) scaled into a window. */
void show_overlay(unsigned long y_offset, int src_pitch, int sw, int sh,
                  int dx, int dy, int dw, int dh, unsigned long key) {
    REG(0x0044) = y_offset;             /* OVL_SRC_Y (packed surface) */
    REG(0x0048) = src_pitch;            /* OVL_SRC_PITCH */
    REG(0x004C) = (sh << 16) | sw;      /* OVL_SRC_DIM */
    REG(0x0058) = (dy << 16) | dx;      /* OVL_DST_XY */
    REG(0x005C) = (dh << 16) | dw;      /* OVL_DST_DIM, the scaled size */
    REG(0x0060) = key;                  /* OVL_COLORKEY */
    REG(0x0040) = 1 | (0 << 1) | (1 << 3);  /* ENABLE, FORMAT=YUY2, KEY_EN */
    /* Then fill the window with `key` so the overlay shows through. */
}
```

## How to drive the engine by DMA

In a large redraw, the slow part is the register writes from the CPU, and not
the drawing. With the DMA pusher, you can build a batch of operations in a
ring buffer in memory, and give the full batch to Margo one time. Each command
is a header word, `(count << 16) | method`. After the header come `count` data
words, which go into consecutive registers.

```c
/* (verified) */
static unsigned long ring[256];          /* system memory, 16-byte aligned */

#define PKT(count, method) (((unsigned long)(count) << 16) | (method))

void start_pusher(void) {
    REG(0x0084) = (unsigned long)ring;   /* PUSH_BASE */
    REG(0x0088) = sizeof(ring);          /* PUSH_SIZE */
    REG(0x0080) = 1;                     /* PUSH_CTRL = ENABLE */
}

/* Queue a solid fill into the ring, then ring the doorbell. */
void fill_via_pusher(int *put, unsigned long base, int pitch, int bpp,
                     int x, int y, int w, int h, unsigned long color) {
    int i = *put / 4;
    ring[i++] = PKT(3, 0x0100);          /* DST_BASE, DST_PITCH, SRC_BASE */
    ring[i++] = base;
    ring[i++] = pitch;
    ring[i++] = base;                    /* SRC_BASE: unused by fill */
    ring[i++] = PKT(1, 0x0110); ring[i++] = bpp / 8;          /* DEPTH */
    ring[i++] = PKT(1, 0x0114); ring[i++] = (y << 16) | x;    /* DST_XY */
    ring[i++] = PKT(1, 0x011C); ring[i++] = (h << 16) | w;    /* DIM */
    ring[i++] = PKT(1, 0x0120); ring[i++] = color;            /* FG_COLOR */
    ring[i++] = PKT(1, 0x0128); ring[i++] = 0xF0;             /* ROP = PATCOPY */
    ring[i++] = PKT(1, 0x0150); ring[i++] = 0x01;             /* COMMAND = FILL */
    *put = i * 4;
    REG(0x008C) = *put;                  /* PUSH_PUT: doorbell, the pusher runs */
}
```

## Dithering

In a 15-bit or 16-bit mode, a true-color image and the scaled video overlay
can show bands. Set `DITHER_EN` in `CONTROL` one time, and Margo dithers them
as it writes.

```c
/* (verified) */
REG(0x000C) = 0x02;     /* CONTROL: DITHER_EN */
```

## A full redraw

A desktop redraw uses these operations in sequence. It fills the background.
It expands the text into the title bars and icons. It then draws the frames,
as lines or as thin filled rectangles. The CPU writes a small number of
registers for each object, and the engine moves the pixels.

TokaDesk, the first preview of the Toka-DOS visual workbench, draws this way
today: every keypress and mouse click redraws the whole screen with FILL and
EXPAND commands, with no damage tracking and no offscreen cache. The MG_CMD_COPY
command exists on the engine for a future cached redraw, but nothing in
TokaDesk uses it yet.
