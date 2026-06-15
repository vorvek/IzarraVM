# VEGA Technical Reference

Visual Engine for Graphics Acceleration. This is the hardware reference for the
VEGA chipset fitted to the Izarra 3000. It documents the programming interface:
the memory map, the display modes, and the register set.

VEGA is two chips that share one pool of memory:

- **Margo**, the 2D engine. Drives the desktop and all 2D display modes, and
  carries a blit engine for accelerated fills, copies, text, and lines.
- **Distira**, the 3D engine. Documented in a later revision of this manual.

Both chips read and write a single 4 MB frame store. Memory is allocated by
mode, so a high resolution 32-bit 2D surface and a 3D scene do not coexist.

This revision covers Margo. The Distira sections are reserved.

---

## 1. Margo overview

Margo presents a flat frame store, a set of linear display modes reachable
through the VESA BIOS interface, and a memory mapped register block that drives
the blit engine. A driver sets a mode through the VBE software interface, then
talks to the engine through the register block to move pixels without the CPU.

- 4 MB frame store, shared with Distira, addressed as a flat byte space from
  offset `0x000000` to `0x3FFFFF`.
- Display modes up to 1024x768 at 32 bits per pixel.
- 256-entry palette for 8-bit modes, through the standard VGA DAC ports.
- A blit engine: solid fill, screen to screen copy, monochrome color expand
  (text), and line draw, each with a raster operation, optional clipping, and
  optional color key.
- VESA VBE 2.0 compatible, with a linear frame buffer.

The legacy VGA text mode and mode 13h remain available and are unchanged. They
are documented with the rest of the VGA core, not here.

---

## 2. Physical memory map

| Range | Size | Contents |
|-------|------|----------|
| `0x000A0000` to `0x000BFFFF` | 128 KB | Legacy VGA aperture (mode 13h at `0xA0000`, text at `0xB8000`) |
| `0xE0000000` to `0xE03FFFFF` | 4 MB | Margo linear frame buffer. Frame store offset 0 maps to `0xE0000000`. |
| `0xE0400000` to `0xE040FFFF` | 64 KB | Margo register block (memory mapped) |

The linear frame buffer exposes the whole 4 MB frame store. The visible surface
starts at the offset in `DISP_START` (0 by default). Memory above the visible
surface is free for offscreen work: blit sources, cached fonts, and saved screen
regions.

The frame buffer and register block sit above the 24 MB of system memory, so
they are reached from protected or flat mode. Real mode code uses mode 13h or
the legacy VGA aperture.

---

## 3. Display modes

Modes are selected through the VBE interface (section 5). The standard VESA mode
numbers are honored so existing VESA software finds them. The 32-bit modes use
numbers in the OEM range, since VESA never assigned standard numbers for 32-bit
color.

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

Scanline pitch is the visible width times bytes per pixel, with no padding. The
largest surface, 1024x768 at 32-bit, is 3 MB, which leaves 1 MB of the frame
store for offscreen use.

---

## 4. Pixel formats

| Format | Bits | Layout (high to low) |
|--------|------|----------------------|
| Indexed | 8 | Palette index. Color comes from the DAC. |
| X1R5G5B5 | 16 | 1 unused, 5 red, 5 green, 5 blue |
| R5G6B5 | 16 | 5 red, 6 green, 5 blue |
| X8R8G8B8 | 32 | 8 unused, 8 red, 8 green, 8 blue |

Packed 24-bit color is not provided. The 32-bit format covers true color and
avoids three-byte pixels.

---

## 5. VGA DAC (palette)

The 8-bit indexed modes and mode 13h take their colors from the 256-entry DAC,
through the standard VGA ports.

| Port | Access | Function |
|------|--------|----------|
| `0x03C8` | Write | Palette write index. Sets the entry that the next data writes target. |
| `0x03C7` | Write | Palette read index. Sets the entry that the next data reads target. |
| `0x03C9` | Read/Write | Palette data. Three accesses per entry, red then green then blue. |

Each component is 6 bits (0 to 63). After three writes to `0x03C9` the index
advances to the next entry, so a full palette load is one write to `0x03C8`
followed by 768 writes to `0x03C9`.

---

## 6. VBE software interface

Mode setting and frame buffer information come through `INT 10h` with `AH = 4Fh`,
the VESA BIOS Extensions interface. `AL` selects the function. On return,
`AL = 4Fh` confirms the function is supported and `AH` is the status (0 on
success).

| Function | Name | Notes |
|----------|------|-------|
| `4F00h` | Return controller information | Fills a VbeInfoBlock at `ES:DI`. Signature `VESA`, version `0x0200`, total memory 64 (in 64 KB units), and a pointer to the mode list. |
| `4F01h` | Return mode information | Fills a ModeInfoBlock at `ES:DI` for the mode in `CX`: resolution, depth, pitch, color masks, and `PhysBasePtr = 0xE0000000`. |
| `4F02h` | Set mode | Mode number in `BX`. Bit 14 (`0x4000`) requests the linear frame buffer. Bit 15 (`0x8000`) preserves memory. |
| `4F03h` | Return current mode | Current mode number in `BX`. |
| `4F07h` | Set/get display start | Maps to `DISP_START`. Used for panning and page flips. |
| `4F08h` | Set/get DAC palette width | Selects 6-bit or 8-bit DAC entries. |
| `4F09h` | Set/get palette data | Bulk palette load, an alternative to the DAC ports. |

Functions `4F00h` through `4F03h` are the mode-setting core. The rest extend it.

---

## 7. Margo register block

The register block is 64 KB at `0xE0400000`. All registers are 32 bits and are
accessed with aligned 32-bit reads and writes. Byte and 16-bit access to the
block is not defined.

Offsets below are relative to the block base.

### 7.1 Identification and control

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0000` | `ID` | R | Identity and interface version. Reads `0x4D470100`: `0x4D47` is the Margo signature, the low half is version 1.00. |
| `0x0004` | `CAPS` | R | Feature bitmap. A driver reads it to learn which operations this build implements. See below. |
| `0x0008` | `STATUS` | R | Bit 0 `BUSY`: the blit engine is working. Bit 1 `FIFO_FULL`: reserved, reads 0. |
| `0x000C` | `CONTROL` | R/W | Bit 0 `RESET`: write 1 to abort the current operation and clear the engine. Self-clearing. Other bits reserved, write 0. |

`CAPS` bits:

| Bit | Meaning |
|-----|---------|
| 0 | `FILL` available |
| 1 | `COPY` available |
| 2 | `COLOR_EXPAND` available |
| 3 | `LINE` available |
| 4 | Full ROP3 set honored (beyond plain copy and fill) |
| 5 | `CLIP` honored |
| 6 | `COLORKEY` honored |

The register map in this manual is fixed. `CAPS` reports which parts the running
build implements, so a driver written against the full map degrades cleanly on
an early build.

### 7.2 Display controller

These describe the surface being scanned out. `4F02h` sets them. A driver may
write `DISP_START` to pan or to flip pages.

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0010` | `DISP_MODE` | R | Current VBE mode number. |
| `0x0014` | `DISP_WIDTH` | R | Visible width in pixels. |
| `0x0018` | `DISP_HEIGHT` | R | Visible height in pixels. |
| `0x001C` | `DISP_BPP` | R | Bits per pixel (8, 15, 16, 32). |
| `0x0020` | `DISP_PITCH` | R | Bytes per scanline of the visible surface. |
| `0x0024` | `DISP_START` | R/W | Frame store byte offset of the top-left visible pixel. Default 0. Takes effect on the next frame. |

### 7.3 Blit engine

Latch the parameters, then write `COMMAND` to run an operation.

| Offset | Name | Access | Description |
|--------|------|--------|-------------|
| `0x0100` | `DST_BASE` | R/W | Frame store byte offset of the destination surface origin. |
| `0x0104` | `DST_PITCH` | R/W | Bytes per scanline of the destination surface. |
| `0x0108` | `SRC_BASE` | R/W | Frame store byte offset of the source surface origin. |
| `0x010C` | `SRC_PITCH` | R/W | Bytes per scanline of the source surface. |
| `0x0110` | `DEPTH` | R/W | Bytes per pixel the engine operates on (1, 2, or 4). Normally the surface format. |
| `0x0114` | `DST_XY` | R/W | Destination top-left. Y in bits 31..16, X in bits 15..0, in pixels. |
| `0x0118` | `SRC_XY` | R/W | Source top-left, same packing. |
| `0x011C` | `DIM` | R/W | Rectangle size. Height in bits 31..16, width in bits 15..0, in pixels. |
| `0x0120` | `FG_COLOR` | R/W | Foreground or fill color, right-justified in the destination format. |
| `0x0124` | `BG_COLOR` | R/W | Background color for color expand. |
| `0x0128` | `ROP` | R/W | Raster operation, low 8 bits (ROP3 code). See section 7.6. |
| `0x012C` | `COLORKEY` | R/W | Transparent color value, destination format. |
| `0x0130` | `FLAGS` | R/W | Bit 0 `COLORKEY_EN`, bit 1 `CLIP_EN`, bit 2 `EXPAND_TRANSPARENT`. See section 7.5. |
| `0x0134` | `CLIP_TL` | R/W | Clip rectangle top-left (Y:X packed). Inclusive. |
| `0x0138` | `CLIP_BR` | R/W | Clip rectangle bottom-right (Y:X packed). Exclusive. |
| `0x013C` | `LINE_START` | R/W | Line start point (Y:X packed). |
| `0x0140` | `LINE_END` | R/W | Line end point (Y:X packed). |
| `0x0150` | `COMMAND` | W | Write a command code to start an operation. See section 7.4. |
| `0x0160` | `MONO_DATA` | W | Monochrome data port for `COLOR_EXPAND_DATA`. See section 7.4. |

### 7.4 Commands

Write one of these codes to `COMMAND`. The engine runs the operation against the
latched registers, with `BUSY` set for the duration.

| Code | Name | Operation |
|------|------|-----------|
| `0x01` | `FILL` | Fill the destination rectangle (`DST_XY`, `DIM`) with `FG_COLOR` through `ROP`. ROP `0xF0` is a solid fill; ROP `0x5A` exclusive-ORs `FG_COLOR` into the destination, for rubber-band boxes. |
| `0x02` | `COPY` | Copy the source rectangle (`SRC_XY`, `DIM`) to `DST_XY` through `ROP`. The engine picks a safe traversal order when source and destination overlap. With `COLORKEY_EN`, source pixels equal to `COLORKEY` are skipped. |
| `0x03` | `COLOR_EXPAND_DATA` | Expand a monochrome bitmap, streamed through `MONO_DATA`, into the destination rectangle. Set bits take `FG_COLOR`; clear bits take `BG_COLOR`, or are left untouched when `EXPAND_TRANSPARENT`. |
| `0x04` | `COLOR_EXPAND_MEM` | As above, but the monochrome source is read from the frame store at `SRC_BASE` / `SRC_XY` with `SRC_PITCH`, 1 bit per pixel, most significant bit first. |
| `0x05` | `LINE` | Draw a line from `LINE_START` to `LINE_END` in `FG_COLOR` through `ROP`. |

`COLOR_EXPAND_DATA` streams its source. After writing the command, write the
bitmap to `MONO_DATA` one 32-bit word at a time, most significant bit first. Each
scanline starts on a word boundary, so a row of W pixels takes `ceil(W / 32)`
words. The engine consumes `ceil(width / 32) * height` words and holds `BUSY`
until the last one arrives.

### 7.5 Flags

| Bit | Name | Effect |
|-----|------|--------|
| 0 | `COLORKEY_EN` | On `COPY`, source pixels equal to `COLORKEY` are not written. Used for transparent sprites and icons. |
| 1 | `CLIP_EN` | All operations are clipped to the rectangle in `CLIP_TL` and `CLIP_BR`. Pixels outside it are discarded. |
| 2 | `EXPAND_TRANSPARENT` | On color expand, clear bits are skipped instead of painted with `BG_COLOR`, so glyphs draw over existing pixels. |

### 7.6 Raster operations

`ROP` holds an 8-bit ROP3 code, the boolean function of source (S), destination
(D), and pattern (P). For `FILL` and `LINE` the pattern is `FG_COLOR` and there
is no source. For `COPY` and color expand the source is the moved or expanded
pixel.

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

The default is `0xCC` for `COPY` and color expand, and `0xF0` for `FILL`. Codes
outside this table are reserved. `CAPS` bit 4 reports whether the build honors
the full set or only plain copy and fill.

---

## 8. Coordinates, colors, and bounds

- Points are packed as `(Y << 16) | X`, both unsigned 16-bit, in pixels.
- Colors are right-justified in the destination pixel format. An 8-bit fill uses
  the low 8 bits of `FG_COLOR`, a 16-bit fill the low 16, a 32-bit fill all 32.
- The engine works inside the 4 MB frame store. An operation whose source or
  destination would fall outside the frame store is ignored rather than wrapped.

---

## 9. Notes on fidelity

The Izarra 3000 is a fantasy machine, and the emulator marks where it bends real
hardware. For Margo:

- Blits complete before the write to `COMMAND` returns, so `BUSY` reads 0
  immediately after. Software that polls `BUSY` still behaves correctly. Real
  silicon would take measurable time, and `BUSY` would clear later.
- Mode changes and `DISP_START` take effect cleanly, without the analog timing
  of a real RAMDAC.

---

## 10. Distira (3D)

Reserved. Documented in a later revision.
