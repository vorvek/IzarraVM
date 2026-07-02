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

## Limitations

- The core produces a raster buffer, not an analog VGA signal. Effects that rely
  on a monitor losing sync on a nonstandard line count do not happen; the
  nonstandard frame is still rendered.
- Mid-scanline register changes (more than 256 colors on a single line) are not
  modeled. Catch-up works at whole-scanline granularity.
- VGA planar memory is a separate 256 KB buffer, distinct from Margo's linear
  VRAM.
