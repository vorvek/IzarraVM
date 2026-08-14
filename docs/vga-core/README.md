<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# VGA core

The VGA core is the IBM VGA compatible video path of IzarraVM. It is in the
Margo video chip, as a compatibility personality. A personality is one
operation mode of the chip. The core shares the frame store and the RAMDAC
with Margo, thus it is not a separate card. Its behavior agrees with real IBM
VGA hardware, and not with an invented part.

The core is a raster engine, and the CPU supplies its clock. A beam counter
from the CPU cycles operates a catch-up rasterizer. Thus a register write in
the middle of a frame changes only the scanlines below the beam. This behavior
is necessary for a split screen and for hardware scrolling. The output is a
frame with square pixels. The renderer does the aspect correction and the
scaling later.

## Text modes

| Mode    | Size  | Colors     |
|---------|-------|------------|
| 00h/01h | 40x25 | 16         |
| 02h/03h | 80x25 | 16         |
| 07h     | 80x25 | monochrome |

A text mode has a hardware cursor, blink attributes, and the internal CP437
character set. You can load a font through `INT 10h AH=11h`. The core supports
the 8-dot character cell and the 9-dot character cell.

## Graphics modes

| Mode   | Resolution | Colors     | Refresh |
|--------|------------|------------|---------|
| 0Dh    | 320x200    | 16         | 70 Hz   |
| 0Eh    | 640x200    | 16         | 70 Hz   |
| 0Fh    | 640x350    | monochrome | 70 Hz   |
| 10h    | 640x350    | 16         | 70 Hz   |
| 11h    | 640x480    | monochrome | 60 Hz   |
| 12h    | 640x480    | 16         | 60 Hz   |
| 13h    | 320x200    | 256        | 70 Hz   |
| Mode X | 320x240    | 256        | 60 Hz   |
| Mode Y | 320x200    | 256        | 70 Hz   |

Modes 0Dh to 12h are the standard EGA and VGA 16-color planar modes. Mode 13h
is the chained 256-color mode. Mode X and mode Y are the unchained 256-color
modes, with square pixels and page flipping.

To set a graphics mode, give its number to `INT 10h AH=00h`. To go from mode
13h to mode X, clear the chain-4 bit of the sequencer.

## Features

- Hardware scrolling: a start-address latch for each frame, and pixel panning
  (Attribute Controller register 13h).
- A line-compare split screen. Use it for a playfield that scrolls below a
  fixed status panel.
- Mode-X page flipping through the start-address latch.
- A DAC with 256 entries, the power-on palettes, and a programmable pel mask.
- The `INT 10h` BIOS video services: mode set, window scroll, character and
  pixel I/O, palette and DAC control, and state query.

## Legacy personalities

The core also has two older adapters as alternative personalities. They share
the same frame store and the same RAMDAC. They are the CGA (320x200x4 and
640x200x2, with the even/odd B800 frame buffer) and the Hercules Graphics Card
(HGC).

### Hercules Graphics Card (HGC)

Hercules graphics has no INT 10h mode number, although each mode in the table
above has one. Real Hercules software sets BIOS mode 07h, the MDA-compatible
80x25 monochrome text mode. It then writes the ports of the card directly to
select graphics:

| Port  | Name                 | Access | Bits |
|-------|----------------------|--------|------|
| 3B8h  | Mode Control         | write  | bit 1 GRPH (graphics or text), bit 3 video enable, bit 5 blink (text mode only), bit 7 page select (0 = B0000, 1 = B8000) |
| 3BFh  | Configuration Switch | write  | bit 0 allow GRPH, bit 1 enable the B8000 page |
| 3BAh  | CRT status           | read   | bit 0 horizontal retrace, bit 3 video pixel output, bit 7 vertical sync (active LOW, the inverse of the VGA/CGA status1 polarity) |

3BFh controls what 3B8h can do. The GRPH bit in Mode Control has no effect
until the Configuration Switch sets its allow-graphics bit. The second 32K
page decodes at B8000 only after the switch sets its page-enable bit. Page 0
(B0000) is always available. Both pages can hold data at the same time, and
the page-select bit of Mode Control selects the page that the CRTC reads.

The usual detection method reads 3BAh bit 7 in a loop, until the bit changes.
The loop usually has a limit of approximately 0x8000 iterations. The method
operates against the beam-coupled vertical retrace state, as the VGA status
ports do.

3B8h, 3BFh, and 3BAh are monochrome-only addresses. Thus they decode with any
value of the color-emulation bit in Misc Output. The 3B4/3B5/3BA aliases are
different, because the monochrome text personality shares them with
3D4/3D5/3DA.

The graphics mode is 720x348, with 1 bit for each pixel, in monochrome. The
frame buffer uses a four-way scanline interleave. This is an extension of the
two-bank even/odd scheme of the CGA. Scanline `y` is in bank `y mod 4` of the
active 32K page, at 90 bytes for each scanline. At the first entry into the
graphics mode, the core installs a monochrome phosphor DAC preset (green,
P39). Text mode 07h keeps its own palette until a program uses Hercules
graphics.

## Limitations

- The core makes a raster buffer, and not an analog VGA signal. A real monitor
  can lose sync on a nonstandard line count. The core does not do this, and it
  renders the nonstandard frame.
- The core does not model a register change in the middle of a scanline. Such
  a change gives more than 256 colors on one line. The catch-up operates on a
  full scanline.
- The VGA planar memory is a separate buffer of 256 KB. It is not the linear
  VRAM of Margo.
