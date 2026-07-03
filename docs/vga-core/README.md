# VGA core

The VGA core is IzarraVM's IBM VGA compatible video path. It lives inside the
Margo video chip as a compatibility personality and shares the frame store and
RAMDAC, so it is not a separate card. It aims to match the behavior of real IBM
VGA hardware rather than a made-up part.

The core is a raster engine clocked off the CPU. A beam counter derived from CPU
cycles drives a catch-up rasterizer, so a mid-frame register write affects only
the scanlines the beam has not yet reached. That is what makes raster tricks such
as split screens and hardware scrolling work. The output is a square-pixel frame;
aspect correction and scaling happen later, in the renderer.

## Text modes

| Mode    | Size  | Colors     |
|---------|-------|------------|
| 00h/01h | 40x25 | 16         |
| 02h/03h | 80x25 | 16         |
| 07h     | 80x25 | monochrome |

Text mode has a hardware cursor, blinking attributes, and the built-in CP437
character set. Fonts are loadable through `INT 10h AH=11h`, and both 8-dot and
9-dot character cells are supported.

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

Modes 0Dh through 12h are the standard EGA and VGA 16-color planar modes. Mode
13h is the chained 256-color mode. Mode X and mode Y are the unchained 256-color
modes, with square pixels and page flipping.

Set a graphics mode by number through `INT 10h AH=00h`. Mode X is entered from
mode 13h by clearing the sequencer chain-4 bit.

## Features

- Hardware scrolling: a per-frame start-address latch plus fine pixel panning
  (Attribute Controller register 13h).
- Line-compare split screen, for a scrolling playfield under a fixed status
  panel.
- Mode-X page flipping through the start-address latch.
- A 256-entry DAC with the stock power-on palettes and a programmable pel mask.
- `INT 10h` BIOS video services: mode set, window scroll, character and pixel
  I/O, palette and DAC control, and state query.

## Legacy personalities

Besides the VGA text and graphics modes above, the core hosts two older
adapters as alternate personalities sharing the same frame store and RAMDAC:
CGA (320x200x4 / 640x200x2, the classic even/odd B800 framebuffer) and the
Hercules Graphics Card (HGC).

### Hercules Graphics Card (HGC)

Unlike every mode in the table above, there was never an INT 10h mode number
for Hercules graphics. Real Hercules software sets BIOS mode 07h (the
MDA-compatible 80x25 monochrome text mode) and then switches to graphics by
writing the card's own ports directly:

| Port  | Name                 | Access | Bits |
|-------|----------------------|--------|------|
| 3B8h  | Mode Control         | write  | bit 1 GRPH (graphics vs text), bit 3 video enable, bit 5 blink (text mode only), bit 7 page select (0 = B0000, 1 = B8000) |
| 3BFh  | Configuration Switch | write  | bit 0 allow GRPH, bit 1 enable the B8000 page |
| 3BAh  | CRT status           | read   | bit 0 horizontal retrace, bit 3 video pixel output, bit 7 vertical sync (active LOW, the inverse of the VGA/CGA status1 polarity) |

3BFh gates what 3B8h may do: setting the GRPH bit in Mode Control has no
effect until the Configuration Switch has set its allow-graphics bit, and the
second 32K page only decodes at B8000 once the switch's page-enable bit is
also set. Page 0 (B0000) is always addressable. Both pages can hold data
simultaneously; Mode Control's page-select bit only picks which one the CRTC
scans out. The classic software detection idiom is to poll 3BAh bit 7 in a tight
loop (traditionally up to ~0x8000 iterations) until it is seen to change. It
works against the beam-coupled vertical retrace state, the same way the VGA
status ports do.

Because 3B8h/3BFh/3BAh are specific mono-only addresses, they decode
regardless of the Misc Output color-emulation bit, unlike the 3B4/3B5/3BA
alias pair the mono text personality shares with 3D4/3D5/3DA.

Graphics mode is 720x348, 1 bit per pixel, monochrome. The framebuffer uses a
four-way scanline interleave (a generalization of CGA's two-bank even/odd
scheme): scanline `y` lives in bank `y mod 4` of the active 32K page, at
90 bytes per scanline. A monochrome phosphor DAC preset (classic green, P39)
installs automatically the first time graphics mode is entered; text mode 07h
keeps its own identity palette unless Hercules graphics has been used.

## Limitations

- The core produces a raster buffer, not an analog VGA signal. Effects that rely
  on a monitor losing sync on a nonstandard line count do not happen; the
  nonstandard frame is still rendered.
- Mid-scanline register changes (more than 256 colors on a single line) are not
  modeled. Catch-up works at whole-scanline granularity.
- VGA planar memory is a separate 256 KB buffer, distinct from Margo's linear
  VRAM.
