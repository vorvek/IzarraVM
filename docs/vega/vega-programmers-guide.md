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
/* (target) */
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

## Filling a rectangle

The engine model is the same for every operation: latch the parameters, write
the command, wait for idle. A solid fill uses `FG_COLOR` and the `PATCOPY`
raster op.

```c
/* (target) */
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
/* (target) */
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
/* (target) */
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
/* (target) */
void draw_line(unsigned long base, int pitch, int bpp,
               int x0, int y0, int x1, int y1, unsigned long color) {
    margo_wait();
    REG(0x0100) = base;                 /* DST_BASE */
    REG(0x0104) = pitch;                /* DST_PITCH */
    REG(0x0110) = bpp / 8;              /* DEPTH */
    REG(0x013C) = (y0 << 16) | x0;      /* LINE_START */
    REG(0x0140) = (y1 << 16) | x1;      /* LINE_END */
    REG(0x0120) = color;                /* FG_COLOR */
    REG(0x0128) = 0xCC;                 /* ROP = SRCCOPY */
    REG(0x0150) = 0x05;                 /* COMMAND = LINE */
    margo_wait();
}
```

## Clipping

Set `CLIP_TL` and `CLIP_BR` to a rectangle and set `CLIP_EN` in `FLAGS`, and
every operation is confined to that rectangle. A window manager sets the clip to
a window's visible area, then draws freely without checking edges itself.

```c
/* (target) */
void set_clip(int x0, int y0, int x1, int y1) {
    REG(0x0134) = (y0 << 16) | x0;      /* CLIP_TL, inclusive */
    REG(0x0138) = (y1 << 16) | x1;      /* CLIP_BR, exclusive */
    /* OR CLIP_EN into FLAGS on the next operation */
}
```

## Putting it together

A desktop redraw is these primitives in sequence: fill the background, copy
cached window contents up from offscreen memory, expand text into the title
bars, and draw the frames as lines or thin filled rectangles. The CPU issues a
few register writes per object and the engine moves the pixels. That is what
keeps the Belunza desktop responsive even when the machine is throttled to its
slowest compatibility mode.
