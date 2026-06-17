# VEGA for Programmers

A working guide to drawing on the Izarra 3000 through the VEGA chipset. The
companion volume, the VEGA Technical Reference, is the register-level contract.
This guide shows how to use it.

Everything here targets **Margo**, the 2D engine. Distira, the 3D engine, has
its own guide in a later revision.

## How to read the examples

Each example is tagged:

- **(verified)** has been run on the machine and produces the result described.
- **(target)** shows the documented interface ahead of its implementation. The
  register sequence follows the Technical Reference, but the operation may not
  be wired up in your build yet. Check `CAPS` (offset `0x0004`) to see what the
  running build implements.

As each operation lands, its example moves from (target) to (verified). Nothing
in this guide claims a result the hardware does not produce.

## The shape of the hardware

Margo gives you three things:

1. A **linear frame buffer** at `0xE0000000`. Once you set a graphics mode, this
   is your screen as a flat array of pixels. You can write pixels directly.
2. A **256-color palette** for 8-bit modes, through the VGA DAC ports.
3. A **blit engine**, reached through the register block at `0xE0400000`, that
   fills, copies, draws text, and draws lines without the CPU touching each
   pixel.

The fast path for a desktop is the blit engine. The CPU sets up an operation by
writing a handful of registers, then writes a command, and the engine does the
work. On a slow CPU, in Izarra 1000 compatibility mode for example, this is the
difference between a responsive desktop and a crawling one.

The engine runs while the CPU does other work. An operation takes real time, so a
program issues it, goes off to prepare the next one, and only waits on `BUSY` when
it actually needs the result. That overlap is where the speed comes from.

## A convention for the examples

The examples use these definitions. They assume a flat or protected-mode program
that can reach the frame buffer and register block.

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

## Setting a mode

Modes are set through the VESA BIOS, `INT 10h` with `AX = 4F02h`. Set bit 14 of
the mode number to ask for the linear frame buffer.

This example selects mode `0x101`, 640x480 at 8-bit color, with the linear frame
buffer.

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

After the mode is set, the display registers describe it. Read `DISP_WIDTH`,
`DISP_HEIGHT`, `DISP_BPP`, and `DISP_PITCH` rather than assuming them, so the same
drawing code works across modes.

## Loading the palette

In an 8-bit mode, pixel values are indices into the DAC. Load a color with one
write to the index port and three to the data port, in red, green, blue order.
Each component runs 0 to 63.

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

## Writing pixels directly

The linear frame buffer is just memory. The address of a pixel is its offset:
`y * pitch + x * bytes_per_pixel`.

```c
/* (target) */
void plot8(int x, int y, int pitch, unsigned char color) {
    LFB[y * pitch + x] = color;
}
```

Direct writes are fine for a handful of pixels. For rectangles, text, and
scrolling, the blit engine is far faster, and that is the rest of this guide.

## Writing pixels in a hi-color mode

A 16-bit mode stores each pixel as `R5G6B5`, two bytes, no palette. Set the mode,
then pack 8-bit color components down to 5/6/5 and write the 16-bit value.

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

For a 15-bit mode (`0x110`), the layout is `X1R5G5B5`: pack as
`((red >> 3) << 10) | ((green >> 3) << 5) | (blue >> 3)`. Read `DISP_BPP` and the
mode's color masks (VBE `4F01h`) rather than assuming the format.

## Filling a rectangle

The engine model is the same for every operation: latch the parameters, write
the command, wait for idle. A solid fill uses `FG_COLOR` and the `PATCOPY`
raster op.

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

## Copying and scrolling

`COPY` moves a rectangle from one place in the frame store to another. Source and
destination may overlap, so a screen can scroll by copying itself shifted by one
text line. The engine handles overlap.

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

To blit an icon with a transparent color, set `COLORKEY` to that color and
`COLORKEY_EN` in `FLAGS`. Source pixels of that value are left untouched.

## Drawing text

Text is a monochrome bitmap expanded into two colors. Set `FG_COLOR` and
`BG_COLOR`, issue `COLOR_EXPAND_DATA`, then stream the glyph bits through
`MONO_DATA`, one 32-bit word at a time, most significant bit first. Each row
starts on a word boundary.

For an 8x8 font, each glyph is eight bytes, one per row. Each row needs one word
(8 bits, padded). Set `EXPAND_TRANSPARENT` to draw the glyph over whatever is
already on screen, leaving the background untouched.

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

## Drawing lines

`LINE` draws between two points in `FG_COLOR`. With ROP `0x5A` (`PATINVERT`) the
line exclusive-ORs into the screen, which is the classic way to draw and erase a
rubber-band selection without saving the background.

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

## Clipping

Set `CLIP_TL` and `CLIP_BR` to a rectangle and set `CLIP_EN` in `FLAGS`, and
every operation is confined to that rectangle. A window manager sets the clip to
a window's visible area, then draws freely without checking edges itself.

```c
/* (verified) */
void set_clip(int x0, int y0, int x1, int y1) {
    REG(0x0134) = (y0 << 16) | x0;      /* CLIP_TL, inclusive */
    REG(0x0138) = (y1 << 16) | x1;      /* CLIP_BR, exclusive */
    /* OR CLIP_EN into FLAGS on the next operation */
}
```

## Tiling a pattern

`PATTERN_FILL` tiles an 8x8 pattern across a rectangle instead of a solid color.
Put the 8x8 tile somewhere in offscreen memory first, in the screen's pixel
format, then point `PAT_BASE` at it. The tiling lines up to the surface origin,
so two adjacent fills meet seamlessly.

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

The cursor is a 64x64 two-plane bitmap in offscreen memory and a position. Point
the engine at the bitmap, set the two colors, and enable it. From then on, moving
the pointer is one register write per frame, and the CPU never touches the
screen under it.

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

## Playing video through the overlay

The overlay takes a YUV image, converts it to RGB, and scales it into a window,
all in hardware. Decode each frame into a YUV buffer in offscreen memory, point
the overlay at it, and key it through the desktop. To show the overlay, paint the
color key into the window; to hide a region, draw over the key as usual.

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

## Driving the engine by DMA

On a busy redraw, writing every register from the CPU is the slow part, not the
drawing. The DMA pusher lets you build a batch of operations in a ring buffer in
memory and hand the whole thing to Margo at once. Each command is a header word,
`(count << 16) | method`, followed by `count` data words that land in consecutive
registers.

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

In a 15 or 16-bit mode, true-color images and the scaled video overlay can band.
Set `DITHER_EN` in `CONTROL` once and Margo dithers them as it writes.

```c
/* (verified) */
REG(0x000C) = 0x02;     /* CONTROL: DITHER_EN */
```

## Putting it together

A desktop redraw is these primitives in sequence: fill the background, copy
cached window contents up from offscreen memory, expand text into the title
bars, and draw the frames as lines or thin filled rectangles. The CPU issues a
few register writes per object and the engine moves the pixels. That is what
keeps the Belunza desktop responsive even when the machine is throttled to its
slowest compatibility mode.
