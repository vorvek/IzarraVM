<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# VEGA Technical Reference

VEGA is the Visual Engine for Graphics Acceleration. This is the hardware
reference for the VEGA chipset in the Izarra3000. It describes the programming
interface: the memory map, the display modes, and the register set.

VEGA has two graphics engines, each with its own memory:

- **Margo**, the 2D engine. It drives the desktop and all of the 2D display
  modes. It has a 4 MB frame store, and a blit engine for accelerated fills,
  copies, text, and lines.
- **Distira**, the 3D engine. It has a 2 MB frame buffer and two independent
  2 MB texture stores.

VEGA shows the scanout of the active engine. Margo and Distira do not share
memory addresses.

This revision describes both engines. Margo is below. Distira is in section
10.

---

## 1. Margo overview

Margo has a flat frame store, a set of linear display modes, and a
memory-mapped register block. The VESA BIOS interface gives access to the
display modes, and the register block operates the blit engine. A driver sets
a mode through the VBE software interface. It then uses the register block to
move pixels without the CPU.

- A 4 MB frame store, private to Margo. It is a flat byte space, from offset
  `0x000000` to `0x3FFFFF`.
- Display modes to a maximum of 1024x768 at 32 bits for each pixel.
- A palette of 256 entries for the 8-bit modes, through the standard VGA DAC
  ports.
- A blit engine with a solid fill, a screen-to-screen copy, a monochrome color
  expand (text), and a line draw. Each operation has a raster operation,
  optional clipping, and an optional color key.
- A tiled pattern fill, a 64x64 hardware cursor, and a scaled video overlay
  with YUV color conversion. Use them for desktop work and for CD video
  playback.
- VESA VBE 2.0 compatibility, with a linear frame buffer.

The legacy VGA text mode and mode 13h continue to be available, with no
change. The VGA core document describes them. This document does not.

### 1.1 Datasheet

| Parameter | Value |
|-----------|-------|
| Host interface | 66 MHz, 32-bit, bus-mastering port to the custom chipset (PCI derived) |
| Host bandwidth | about 266 MB/s peak |
| Margo core clock | 100 MHz |
| Frame store | 4 MB SGRAM, 128-bit, 100 MHz, dedicated to Margo |
| Memory bandwidth | about 1.6 GB/s |
| RAMDAC | 206 MHz, integrated |
| 2D solid fill | up to about 200 Mpixels/s |
| 2D screen-to-screen blit | up to about 100 Mpixels/s |
| Maximum mode | 1024x768 at 32-bit color |
| Process | 350 nm |

These are the rated figures. The emulator does not model the graphics timing
cycle by cycle. Thus the fill rate and the blit rate describe the part, and
not the emulated behavior. See section 9.

---

## 2. Physical memory map

| Range | Size | Contents |
|-------|------|----------|
| `0x000A0000` to `0x000BFFFF` | 128 KB | Legacy VGA aperture (mode 13h at `0xA0000`, text at `0xB8000`) |
| `0xE0000000` to `0xE03FFFFF` | 4 MB | Margo linear frame buffer. Frame store offset 0 maps to `0xE0000000`. |
| `0xE0400000` to `0xE040FFFF` | 64 KB | Margo register block (memory mapped) |

The linear frame buffer gives access to the full 4 MB frame store. The visible
surface starts at the offset in `DISP_START`, which is 0 by default. The
memory above the visible surface is free for offscreen work: blit sources,
cached fonts, and saved screen regions.

The frame buffer and the register block are above the 64 MB of system memory.
Thus a program reaches them from protected mode or flat mode. Real-mode code
uses mode 13h or the legacy VGA aperture.

---

## 3. Display modes

A program selects a mode through the VBE interface (section 6). Margo supports
the standard VESA mode numbers, thus existing VESA software finds them. The
32-bit modes use numbers in the OEM range, because VESA assigned no standard
numbers for 32-bit color.

Mode `0x150` is a VEGA OEM mode: 320x240 at 8 bits for each pixel. The monitor
doubles its lines. The graphical POST of the Izarra-BIOS uses it. Function
`4F00h` returns the mode list in ascending order, thus the OEM entries come
after the VESA entries.

| Mode | Resolution | Depth | Pixel format | Bytes/pixel |
|------|------------|-------|--------------|-------------|
| `0x100` | 640x400 | 8 | Indexed | 1 |
| `0x101` | 640x480 | 8 | Indexed | 1 |
| `0x103` | 800x600 | 8 | Indexed | 1 |
| `0x105` | 1024x768 | 8 | Indexed | 1 |
| `0x110` | 640x480 | 15 | X1R5G5B5 | 2 |
| `0x111` | 640x480 | 16 | R5G6B5 | 2 |
| `0x113` | 800x600 | 15 | X1R5G5B5 | 2 |
| `0x114` | 800x600 | 16 | R5G6B5 | 2 |
| `0x116` | 1024x768 | 15 | X1R5G5B5 | 2 |
| `0x117` | 1024x768 | 16 | R5G6B5 | 2 |
| `0x14A` | 640x480 | 32 | X8R8G8B8 | 4 |
| `0x14C` | 800x600 | 32 | X8R8G8B8 | 4 |
| `0x14E` | 1024x768 | 32 | X8R8G8B8 | 4 |
| `0x150` | 320x240 | 8 | Indexed | 1 |

The scanline pitch is the visible width multiplied by the bytes for each
pixel. There is no padding. The largest surface is 1024x768 at 32-bit, which
is 3 MB. This leaves 1 MB of the frame store for offscreen use.

### 3.1 Scanout timing

Each mode refreshes at 60.000 Hz. The pixel clock is not an independent
parameter. It is the clock that gives the total dots of the mode at that rate.
Thus the refresh rate is the same in each mode. It does not change with the
resolution or with the depth.

| Displayed resolution | Total dots | Total lines | Sync start | Sync width | Pixel clock |
|----------------------|------------|-------------|------------|------------|-------------|
| 640x400 | 800 | 449 | line 412 | 2 lines | 21.552 MHz |
| 640x480 | 800 | 525 | line 490 | 2 lines | 25.200 MHz |
| 800x600 | 1056 | 628 | line 601 | 4 lines | 39.790 MHz |
| 1024x768 | 1344 | 806 | line 771 | 6 lines | 64.996 MHz |

The totals and the sync positions are the standard values for these
resolutions. Thus a monitor of the period locks to them. The clocks are
different from the usual published figures, in the third digit or the fourth
digit. The published figures are for 59.94 Hz at 640x480, and for 60.32 Hz at
800x600, and not for a flat 60 Hz.

The 640x400 signal is a different type of exception. Usually a card drives it
at 70 Hz. Margo drives it at 60 Hz, as it drives each other mode. Thus its
clock is lower.

Mode `0x150` holds 320x240. The display doubles its lines. Thus it scans out
on the 640x480 signal in the table.

Software can read the vertical retrace interval through Input Status 1 (port
`03DAh` bit 3) and Input Status 0 (port `03C2h` bit 7). A VGA program reads
the same two registers. A read of `03DAh` also resets the address/data
flip-flop of the attribute controller, because that behavior belongs to the
port and not to the engine that drives the display. Bits 1 and 2 of `03DAh`
read 0, because the machine has no light pen. The diagnostic DAC readback bits
4 and 5 also read 0, because they have no attribute-controller path to sample
while Margo owns the display.

---

## 4. Pixel formats

| Format | Bits | Layout (high to low) |
|--------|------|----------------------|
| Indexed | 8 | Palette index. Color comes from the DAC. |
| X1R5G5B5 | 16 | 1 unused, 5 red, 5 green, 5 blue |
| R5G6B5 | 16 | 5 red, 6 green, 5 blue |
| X8R8G8B8 | 32 | 8 unused, 8 red, 8 green, 8 blue |

Margo has no packed 24-bit color. The 32-bit format gives true color, and it
has no three-byte pixel.

---

## 5. VGA DAC (palette)

The 8-bit indexed modes and mode 13h take their colors from the DAC, which has
256 entries. They use the standard VGA ports.

| Port | Access | Function |
|------|--------|----------|
| `0x03C8` | Write | Palette write index. It sets the entry for the next data writes. |
| `0x03C7` | Write | Palette read index. It sets the entry for the next data reads. |
| `0x03C9` | Read/Write | Palette data. Each entry needs three accesses: red, green, then blue. |

Each component is 6 bits, from 0 to 63. After three writes to `0x03C9`, the
index moves to the next entry. Thus a full palette load is one write to
`0x03C8`, and then 768 writes to `0x03C9`.

---

## 6. VBE software interface

The mode set and the frame buffer information come through `INT 10h` with
`AH = 4Fh`. This is the VESA BIOS Extensions interface. `AL` selects the
function. On return, `AL = 4Fh` shows that the function is available, and `AH`
is the status. A status of 0 is success.

| Function | Name | Notes |
|----------|------|-------|
| `4F00h` | Return controller information | Fills a VbeInfoBlock at `ES:DI`. Signature `VESA`, version `0x0200`, total memory 64 (in 64 KB units), and a pointer to the mode list. |
| `4F01h` | Return mode information | Fills a ModeInfoBlock at `ES:DI` for the mode in `CX`: resolution, depth, pitch, color masks, and `PhysBasePtr = 0xE0000000`. |
| `4F02h` | Set mode | Mode number in `BX`. Bit 14 (`0x4000`) requests the linear frame buffer. Bit 15 (`0x8000`) keeps the memory contents. |
| `4F03h` | Return current mode | Current mode number in `BX`. |
| `4F05h` | Set/get display window | Banked access to the frame store, through the legacy aperture at `A000h`. `BH=00h` selects the bank in `DX`, and `BH=01h` returns it. `BL` selects the window, and it must be `00h`, because only window A exists. The granularity and the window size are both 64 KB. Thus bank `n` maps frame store offset `n * 64 KB` to `A0000h`. The function fails with status `03h` in a mode that uses the linear frame buffer. |
| `4F07h` | Set/get display start | `BL=00h` queues an `(x,y)` origin. `BL=01h` returns the active origin. `BL=80h` queues the origin and waits for the next 60 Hz frame boundary. Use these for panning and for page flips. |
| `4F08h` | Set/get DAC palette width | `BL=00h` selects the width in `BH`. `BL=01h` returns the active width in `BH`. The DAC supports six-bit and eight-bit entries. |
| `4F09h` | Set/get palette data | `BL=00h` loads, `BL=01h` reads, and `BL=80h` loads at the next frame boundary. Each entry at `ES:DI` is a four-byte record: B, G, R, and alignment. `CX` is the count, and `DX` is the first index. |
| `4F0Ah` | Return protected-mode interface | `BL=00h` returns a table of protected-mode entry points at `ES:DI`, with a length of `CX` bytes. The table holds far-callable routines for set window, set display start, and set primary palette data. After them comes the list of the I/O ports that they use. Thus a protected-mode driver reaches them without `INT 10h`. Other `BL` values are not defined. |

Functions `4F00h` to `4F03h` are the core of the mode set. The other functions
are extensions.

A mode set through `4F02h` programs the display timing again. While a VBE mode
is active, the CRT status registers report that timing. They do not report the
timing of the previous VGA mode. See section 3 and section 9.

---

## 7. Margo register block

The register block is 64 KB at `0xE0400000`. Each register is 32 bits. Use
aligned 32-bit reads and writes. A byte access or a 16-bit access to the block
is not defined.

The offsets below are relative to the base of the block.

### 7.1 Identification and control

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0000` | `ID` | R | Identity and interface version. It reads `0x4D470100`. `0x4D47` is the Margo signature, and the low half is version 1.00. |
| `0x0004` | `CAPS` | R | Feature bitmap. A driver reads it to find the operations of this build. See below. |
| `0x0008` | `STATUS` | R | Bit 0 `BUSY`: an operation is in progress. The bit stays set until the modeled completion time (section 9). Bit 1 `FIFO_FULL`: reserved, reads 0. |
| `0x000C` | `CONTROL` | R/W | Bit 0 `RESET`: write 1 to stop the current operation and clear the engine. The bit clears itself. Bit 1 `DITHER_EN`: dither where the color precision decreases (section 7.10). The other bits are reserved. Write 0 to them. |

`CAPS` bits:

| Bit | Meaning |
|-----|---------|
| 0 | `FILL` available |
| 1 | `COPY` available |
| 2 | `COLOR_EXPAND` available |
| 3 | `LINE` available |
| 4 | Full ROP3 set available (more than the plain copy and fill) |
| 5 | `CLIP` available |
| 6 | `COLORKEY` available |
| 7 | `PATTERN_FILL` available |
| 8 | Hardware cursor available |
| 9 | Video overlay available |
| 10 | DMA pusher available |
| 11 | Hardware dithering available |

The register map in this manual is fixed. `CAPS` gives the parts that the
current build has. Thus a driver for the full map continues to operate
correctly on an early build.

### 7.2 Display controller

These registers describe the surface that Margo scans out. `4F02h` sets them.
A driver can write `DISP_START` to pan the display or to flip pages.

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0010` | `DISP_MODE` | R | Current VBE mode number. |
| `0x0014` | `DISP_WIDTH` | R | Visible width in pixels. |
| `0x0018` | `DISP_HEIGHT` | R | Visible height in pixels. |
| `0x001C` | `DISP_BPP` | R | Bits for each pixel (8, 15, 16, 32). |
| `0x0020` | `DISP_PITCH` | R | Bytes for each scanline of the visible surface. |
| `0x0024` | `DISP_START` | R/W | Frame store byte offset of the top-left visible pixel. The default is 0. It takes effect on the next frame. |

### 7.3 Blit engine

Write the parameters. Then write `COMMAND` to start an operation.

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0100` | `DST_BASE` | R/W | Frame store byte offset of the destination surface origin. |
| `0x0104` | `DST_PITCH` | R/W | Bytes for each scanline of the destination surface. |
| `0x0108` | `SRC_BASE` | R/W | Frame store byte offset of the source surface origin. |
| `0x010C` | `SRC_PITCH` | R/W | Bytes for each scanline of the source surface. |
| `0x0110` | `DEPTH` | R/W | Bytes for each pixel that the engine operates on (1, 2, or 4). This is usually the surface format. |
| `0x0114` | `DST_XY` | R/W | Destination top-left. Y in bits 31..16, X in bits 15..0, in pixels. |
| `0x0118` | `SRC_XY` | R/W | Source top-left, with the same packing. |
| `0x011C` | `DIM` | R/W | Rectangle size. Height in bits 31..16, width in bits 15..0, in pixels. |
| `0x0120` | `FG_COLOR` | R/W | Foreground or fill color, right-justified in the destination format. |
| `0x0124` | `BG_COLOR` | R/W | Background color for a color expand. |
| `0x0128` | `ROP` | R/W | Raster operation, in the low 8 bits (ROP3 code). See section 7.6. |
| `0x012C` | `COLORKEY` | R/W | Transparent color value, in the destination format. |
| `0x0130` | `FLAGS` | R/W | Bit 0 `COLORKEY_EN`, bit 1 `CLIP_EN`, bit 2 `EXPAND_TRANSPARENT`. See section 7.5. |
| `0x0134` | `CLIP_TL` | R/W | Clip rectangle top-left (Y:X packed). Inclusive. |
| `0x0138` | `CLIP_BR` | R/W | Clip rectangle bottom-right (Y:X packed). Exclusive. |
| `0x013C` | `LINE_START` | R/W | Line start point (Y:X packed). |
| `0x0140` | `LINE_END` | R/W | Line end point (Y:X packed). |
| `0x0144` | `PAT_BASE` | R/W | Frame store offset of an 8x8 pattern in the destination format. The row pitch is `8 * DEPTH` bytes. `PATTERN_FILL` uses it. |
| `0x0150` | `COMMAND` | W | Write a command code to start an operation. See section 7.4. |
| `0x0160` | `MONO_DATA` | W | Monochrome data port for `COLOR_EXPAND_DATA`. See section 7.4. |

### 7.4 Commands

Write one of these codes to `COMMAND`. The engine does the operation with the
values in the registers. `BUSY` stays set until the operation is complete.

| Code | Name | Operation |
|------|------|-----------|
| `0x01` | `FILL` | Fill the destination rectangle (`DST_XY`, `DIM`) with `FG_COLOR`, through `ROP`. ROP `0xF0` gives a solid fill. ROP `0x5A` does an exclusive-OR of `FG_COLOR` into the destination, for a rubber-band box. |
| `0x02` | `COPY` | Copy the source rectangle (`SRC_XY`, `DIM`) to `DST_XY`, through `ROP`. The engine selects a safe order when the source and the destination overlap. With `COLORKEY_EN`, the engine does not write a source pixel that is equal to `COLORKEY`. |
| `0x03` | `COLOR_EXPAND_DATA` | Expand a monochrome bitmap into the destination rectangle. `MONO_DATA` carries the bitmap. A set bit gets `FG_COLOR`. A clear bit gets `BG_COLOR`, or stays unchanged with `EXPAND_TRANSPARENT`. |
| `0x04` | `COLOR_EXPAND_MEM` | The same operation, but the engine reads the monochrome source from the frame store at `SRC_BASE` and `SRC_XY`, with `SRC_PITCH`, at 1 bit for each pixel, most significant bit first. |
| `0x05` | `LINE` | Draw a line from `LINE_START` to `LINE_END` in `FG_COLOR`, through `ROP`. |
| `0x06` | `PATTERN_FILL` | Fill the destination rectangle with tiles of the 8x8 pattern at `PAT_BASE`, in the destination format. The pattern phase aligns to the surface origin, thus adjacent fills join with no visible seam. `ROP` and the color key apply, thus a hatch pattern can key its background through. For a monochrome GDI brush, expand the brush one time into an 8x8 color tile. |

`COLOR_EXPAND_DATA` reads its source as a stream. After you write the command,
write the bitmap to `MONO_DATA` one 32-bit word at a time, with the most
significant bit first. Each scanline starts on a word boundary. Thus a row of
W pixels needs `ceil(W / 32)` words. The engine reads
`ceil(width / 32) * height` words, and it holds `BUSY` until the last word
arrives.

### 7.5 Flags

| Bit | Name | Effect |
|-----|------|--------|
| 0 | `COLORKEY_EN` | On `COPY`, the engine does not write a source pixel that is equal to `COLORKEY`. Use it for a transparent sprite or icon. |
| 1 | `CLIP_EN` | The engine clips each operation to the rectangle in `CLIP_TL` and `CLIP_BR`. It discards a pixel outside that rectangle. |
| 2 | `EXPAND_TRANSPARENT` | On a color expand, the engine does not write a clear bit with `BG_COLOR`. Thus a glyph draws over the existing pixels. |

### 7.6 Raster operations

`ROP` holds an 8-bit ROP3 code. The code is the boolean function of the source
(S), the destination (D), and the pattern (P). For `FILL` and `LINE`, the
pattern is `FG_COLOR`, and there is no source. For `COPY` and for a color
expand, the source is the moved pixel or the expanded pixel.

| Code | Name | Result |
|------|------|--------|
| `0x00` | `BLACKNESS` | 0 |
| `0x55` | `DSTINVERT` | ~D |
| `0x5A` | `PATINVERT` | D ^ P |
| `0x66` | `SRCINVERT` | D ^ S |
| `0x88` | `SRCAND` | D & S |
| `0xCC` | `SRCCOPY` | S |
| `0xEE` | `SRCPAINT` | D \| S |
| `0xF0` | `PATCOPY` | P |
| `0xFF` | `WHITENESS` | all ones |

The default is `0xCC` for `COPY` and for a color expand. It is `0xF0` for
`FILL`. A code that is not in this table is reserved. `CAPS` bit 4 gives the
set of the build: the full set, or the plain copy and fill only.

### 7.7 Hardware cursor

The cursor is a 64x64 two-plane bitmap. The display path composites it, thus
the CPU does not blit the pointer. The bitmap is in the frame store, as two
planes of 512 bytes. The AND plane is first, and the XOR plane follows at
`CURSOR_ADDR + 512`. Each plane is 64x64 at 1 bit for each pixel, with 8 bytes
for each row, and the most significant bit first. Thus each pixel has one AND
bit and one XOR bit, and the total is 1024 bytes.

| AND | XOR | Result |
|-----|-----|--------|
| 0 | 0 | Background color (`CURSOR_BG`) |
| 0 | 1 | Foreground color (`CURSOR_FG`) |
| 1 | 0 | Transparent. The screen is visible. |
| 1 | 1 | The screen pixel, inverted |

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0028` | `CURSOR_CTRL` | R/W | Bit 0 `ENABLE`. The other bits are reserved. |
| `0x002C` | `CURSOR_ADDR` | R/W | Frame store offset of the 1024-byte cursor bitmap. |
| `0x0030` | `CURSOR_POS` | R/W | Top-left screen position. Y in bits 31..16, X in bits 15..0. Each value is a signed 16-bit value, thus the cursor can go off the top edge and the left edge. The engine clips the visible part to the screen. |
| `0x0034` | `CURSOR_FG` | R/W | Foreground color, in the display format. In an 8-bit mode, it is a palette index. |
| `0x0038` | `CURSOR_BG` | R/W | Background color. |

A move of the pointer is one write to `CURSOR_POS` for each frame. This is the
purpose of the hardware cursor.

### 7.8 Video overlay

The overlay is a scaled video window that the engine composites at scanout.
The source is a YUV image in the frame store. The engine converts it to RGB
with the BT.601 coefficients. It then scales it from the source size to a
destination rectangle on the screen. A color key controls the overlay, thus a
desktop window can cover it. Use this path for CD video, and keep the color
conversion and the scaling off the CPU.

Source formats:

- **YUY2**: packed 4:2:2, 16 bits for each pixel, in the byte order Y0, U, Y1,
  V.
- **YV12**: planar 4:2:0, with an 8-bit Y plane, and then 8-bit V and U planes
  at half width and half height.

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0040` | `OVL_CTRL` | R/W | Bit 0 `ENABLE`. Bits 2..1 `FORMAT` (0 YUY2, 1 YV12). Bit 3 `KEY_EN`. |
| `0x0044` | `OVL_SRC_Y` | R/W | Frame store offset of the Y plane, or of the packed surface for YUY2. |
| `0x0048` | `OVL_SRC_PITCH` | R/W | Bytes for each scanline of the Y plane or the packed plane. |
| `0x004C` | `OVL_SRC_DIM` | R/W | Source size. Height in bits 31..16, width in bits 15..0. |
| `0x0050` | `OVL_SRC_U` | R/W | Frame store offset of the U plane (YV12 only). |
| `0x0054` | `OVL_SRC_V` | R/W | Frame store offset of the V plane (YV12 only). |
| `0x0058` | `OVL_DST_XY` | R/W | Destination top-left on the screen (Y:X packed). |
| `0x005C` | `OVL_DST_DIM` | R/W | Destination size. Height in bits 31..16, width in bits 15..0. This is the scaled size on the screen. |
| `0x0060` | `OVL_COLORKEY` | R/W | With `KEY_EN`, the overlay appears only where the primary surface is equal to this value. |

In the destination rectangle, the engine samples the source at the destination
size, converts YUV to RGB, and shows the result. With `KEY_EN`, an application
writes `OVL_COLORKEY` into its video window, and the overlay is visible there.
Where another window draws over the key, the overlay is not visible. The
engine upsamples the chroma for the 4:2:0 format.

### 7.9 DMA pusher

The pusher is a bus-master command engine. A driver does not have to write the
registers one at a time. It builds a stream of commands in a ring buffer in
system memory, and Margo reads the commands and does them. Thus the CPU stays
off the bus during a long sequence of operations. This keeps the desktop fast
on a slow CPU.

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0080` | `PUSH_CTRL` | R/W | Bit 0 `ENABLE`. |
| `0x0084` | `PUSH_BASE` | R/W | System physical address of the command ring, aligned to 16 bytes. |
| `0x0088` | `PUSH_SIZE` | R/W | Ring size in bytes. It must be a power of two. |
| `0x008C` | `PUSH_PUT` | R/W | Byte offset into the ring of the end of the submitted commands. A write to this register is the doorbell that starts the pusher. |
| `0x0090` | `PUSH_GET` | R | The current read offset of the pusher. It is equal to `PUSH_PUT` when the ring is empty. |

The ring holds 32-bit words. Each command starts with a header word:

    header = (count << 16) | method

`method` (bits 15..0) is a byte offset into this register block, and it is a
multiple of 4. `count` (bits 31..16) is the number of data words after the
header. The pusher writes the data words to `method`, `method + 4`,
`method + 8`, and so on. The result is the same as a write of those registers
by the CPU, in the same order. A write to `COMMAND` (offset `0x0150`) through
the pusher starts an operation, as a direct write does.

The pusher moves `PUSH_GET` past each word that it reads. It returns to the
start of the ring at `PUSH_SIZE`. It stops when `GET` reaches `PUT`. For
separate registers, use one header for each register, with a `count` of 1. For
a continuous run of registers, use one header.

### 7.10 Dithering

With `CONTROL.DITHER_EN` set, Margo applies an ordered 4x4 dither where it
decreases the color precision. There are two such conditions. In the first,
the blit engine writes a color of higher precision into a 15-bit or 16-bit
surface. In the second, the video overlay goes to a 15-bit or 16-bit display.
The dither adds a small quantity of spatial noise, and it removes the bands.
It has no effect on a 32-bit surface, where the precision does not decrease.

---

## 8. Coordinates, colors, and bounds

- A point is packed as `(Y << 16) | X`. Both values are unsigned 16-bit
  values, in pixels.
- A color is right-justified in the destination pixel format. An 8-bit fill
  uses the low 8 bits of `FG_COLOR`. A 16-bit fill uses the low 16 bits. A
  32-bit fill uses all 32 bits.
- The engine operates in the 4 MB frame store. If the source or the
  destination of an operation is outside the frame store, the engine ignores
  the operation. It does not wrap the address.

---

## 9. Timing and fidelity

The Izarra3000 is an invented machine. This section gives each difference
between the emulator and real hardware.

### Timing

Margo calculates the result of an operation in one step. It does not simulate
its datapath cycle by cycle. But its register interface is correct. A cost
model gives each operation a duration: a fixed setup time, and then the work
divided by the rated throughput (section 1.1). `STATUS.BUSY` stays set until
that time is complete. `PUSH_GET` of the pusher moves through the ring as the
pusher reads the commands.

The result bytes go into the frame store immediately. But software cannot read
the engine as idle before the modeled time is complete. The machine clock
measures that time.

This document defines Margo, and no measurement of silicon defines it. Thus
the timing is exact by construction: the cost model is the specification, and
the emulator obeys it. Software behaves as it does on the real part. This
includes software that reads `BUSY`, software that races the engine, and
software that supplies the pusher as a producer to a consumer. This is a
stronger guarantee than the CPU compatibility modes, which give only an
approximation of a real 386 or 486.

One part stays an approximation: the memory contention. An operation in
progress uses frame-store bandwidth and host-port bandwidth. That use can stop
a CPU access to the same memory. The emulator approximates this effect. It
does not model it exactly.

### Display scanout

Margo does not scan the frame store out pixel by pixel behind the CPU. It
takes the full visible surface in one step, and shows it. Thus a frame is
atomic. Margo reads each pixel of a frame from the frame store at one moment,
through one palette and one `DISP_START`. Thus no frame can contain two
display states. There is no incomplete frame, and there is no tearing.

The frame rate controls the moment when the display registers latch. It does
not control the moment when Margo reads the surface. A write to the frame
store is visible in the next frame after the write. Thus a program with a
complete picture has two methods. It can draw into an offscreen surface and
then flip with `DISP_START`, which latches on the frame boundary (below). Or
it can accept a frame that Margo takes in the middle of the drawing.

This is the specified behavior of the part. It is not an approximation of a
more exact behavior. A driver can depend on it. For this reason, the display
controller has no register for the position of the beam in the active area.

Three properties follow, and software can depend on all three.

- **The frame rate is readable.** The vertical retrace interval at `03DAh` and
  `03C2h` runs at the frame rate of the active mode. That rate is 60.000 Hz in
  each VBE mode (section 3.1), and the rate of the VGA mode when a VGA mode is
  active. It is the same clock that moves the frame boundary below. Thus a
  program that paces on the retrace and flips pages uses one clock, and not
  two.
- **`DISP_START` takes effect at a frame boundary.** A write to `DISP_START`,
  or a `4F07h` call with `BL=00h` or `BL=80h`, queues the new origin. Margo
  applies it at the next frame boundary, and never before it. Thus the frame
  in progress does not change, and Margo draws the next frame fully from the
  new origin. `BL=80h` also holds the caller until that boundary, thus the
  origin is active when the call returns. A second queued origin replaces the
  first one, and only the last one applies. The frame boundary follows the
  retrace interval in the same blanking period, which is the order that a
  program expects when it pans during the retrace.
- **The palette latches one time for each frame.** Margo samples the DAC state
  one time, when it takes the frame. It decodes the full frame through that
  one sample. Thus a palette load cannot put two color sets in one frame.
  `4F09h` with `BL=80h` also holds the caller until the frame boundary, thus
  the load is active for the frame that follows.

### Other liberties

- A mode change has an immediate effect. There is no analog settling time, as
  on a real RAMDAC and monitor.
- The video overlay scales with point sampling. Real silicon interpolated,
  which gave a smoother scaled image.

---

## 10. Distira (3D)

Distira is the second half of VEGA. It is a fixed-function 3D rasterizer, with
its own frame buffer, its own texture stores, and a 60 Hz scanout. Margo uses
a register interface of its own, and Distira uses a real one. It obeys the
same PCI, MMIO, frame buffer, and register contract as the 3dfx Voodoo
Graphics generation, the SST-1 chipset. Thus Glide software of the period can
find it and drive it, as it drives real Voodoo hardware.

This is a compatibility target, and not a licensed design. It comes from
public documentation and from a study of the behavior of that generation of
hardware. The method is the clean-room method of the remainder of the
Izarra3000.

### 10.1 The board: BigDistira and SmallDistira

A Distira card has two identical rendering chips. The wiring is the wiring of
a Voodoo Graphics board for its 3D chip and its texelFX units. The Izarra lab
notes give them the names **BigDistira** and **SmallDistira**. These are the
two chips on the board. They are not two products. A Distira card always has
both chips, and software finds one device with two texture mapping units
(TMUs). There is no selection between them, in the same way that a Voodoo
Graphics board does not sell its 3D chip and its TMU chip separately.

### 10.2 Identification and memory

| Item | Value |
|------|-------|
| PCI vendor ID | `0x121A` |
| PCI device ID | `0x0001` |
| BAR0 size | 16 MB (MMIO and LFB windows in it) |
| MMIO window | 64 KB, register-mapped |
| Framebuffer | 2 MB, dedicated (not shared with the 4 MB frame store of Margo) |
| Texture memory | 2 MB for each TMU |
| TMU count | 2 |
| Native ID register | `0x44540100` ("DT", version 1.00) |

Distira does not share the 4 MB frame store of Margo. It has its own frame
buffer, and its own texture memory for each TMU. A driver reaches them through
the PCI base address register of the board, and not through a fixed physical
range. The driver finds the board with a PCI configuration scan, on ports
`0x0CF8` and `0x0CFC`. It reads the vendor ID and the device ID above to
confirm the board. It then programs BAR0 to put the MMIO window and the frame
buffer window at the addresses that it wants. This is the same procedure as
for real Voodoo Graphics hardware.

### 10.3 Register interface

Distira answers the real SST-1 register set, at the offsets that Voodoo
Graphics software expects. The set includes the status register, the vertex
and gradient registers (integer and floating point), `triangleCMD` and
`fTriangleCMD`, the pixel pipeline controls (`fbzColorPath`, `fogMode`,
`alphaMode`, `fbzMode`, `lfbMode`), the clipping registers, `fastfillCMD`,
`swapbufferCMD`, the `fbiInit0` to `fbiInit7` initialization registers, the
DAC data port, and the texture mode, LOD, base address, and NCC table
registers of each TMU. A real Glide 2.x driver and DOS Glide software program
the same registers directly. Distira has no new API.

Distira also has a small native register block, at MMIO offset `0xF000`. It
holds `ID`, `CAPS`, `STATUS`, `CONTROL`, `MODEL`, and some frame-buffer
geometry, clear, and command registers. It has the same purpose as the `ID`
and `CAPS` pair of Margo. Software that knows the Izarra3000 can confirm the
chip and its capabilities there, and it does not have to read the full SST-1
map.

The pixel pipeline supports the usual linear frame buffer pixel formats:
RGB565, RGB555, ARGB1555, RGB888, and ARGB8888. It also supports a combined
depth and color LFB write. The rasterizer does the triangle setup and the edge
functions. It interpolates the color and the texture coordinates with
barycentric coordinates. It does the depth test, the alpha test, the alpha
blend, the chroma key, the fog, and the dither. It samples a texture on each
TMU, in the LIM formats of DOS Glide 2.x software.

### 10.4 Software status

The register, PCI, MMIO, LFB, and texture-aperture contracts are complete. The
rasterizer draws triangles with depth, alpha, fog, chroma key, and textures. A
direct SST-1 test program operates the device from end to end, as a regression
check.

The direct DOS Glide path is verified in the game:

- The original Voodoo Graphics executable of Tomb Raider detects Distira and
  renders in the game. Its Glide implementation is in the executable. Thus the
  emulator supplies no `GLIDE2X.OVL`.

The dynamic DOS Glide path is verified with `test00.exe`, from the 3dfx Glide
2.43 SDK. Without the OVL, the program fails at the DLL load. With a local
Voodoo Graphics `GLIDE2X.OVL`, it opens Distira at 640x480, renders its
expected frame, and returns exit code 0. The current Carmageddon fixture
renders without an OVL, and it has no LE import modules. Thus it is not
evidence for this path. A dynamic in-game corpus run is not done yet.

IzarraVM does not supply proprietary Glide binaries, and it does not supply
game data. A local OVL and a local game are untracked test fixtures.
