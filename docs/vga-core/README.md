# VGA Core Conformance

The legacy VGA core is Margo's VGA-compatibility personality — one block on the
VEGA die, sharing the frame store and RAMDAC. It is *not* a separate chip. This
document is the conformance contract for the VGA core, the analog of `docs/vega/`
for Margo. Unlike VEGA (a fantasy part defined by its manual), the VGA core
targets the *real* IBM VGA, so the contract is real hardware behavior; see
`dev_docs/reference/vga/` for the register references.

The core lives in `crates/izarravm-video/src/vga.rs` (the `Vga` type). It is a
cycle-coupled **raster engine**: a beam clock derived from CPU cycles drives a
catch-up rasterizer, so mid-frame register changes affect only the scanlines the
beam has not yet crossed (copper bars, raster splits, the Commander Keen
pel-pan/start-address scroll). Aspect correction and final scaling are a
downstream renderer concern, out of scope here — the rasterizer emits a
**pixel-perfect, square-pixel** raster into a machine-owned buffer the host pulls.

## Slice 1 coverage

Slice 1 implements one renderable planar mode and the raster engine that drives
it. What is in:

| Area | Covered in slice 1 |
|------|--------------------|
| Mode | `0Dh` (320×200×16 planar), set via `set_mode_0dh` / `INT 10h AH=00h` analog |
| Legacy | Text mode, mode `13h` (chained 256-color), the 256-entry DAC, the text cursor — folded in from the former `VgaTextMode` |
| Planar memory | Four-plane model, write modes 0–3, read modes 0–1, latches, data-rotate/ALU, Map Mask, Bit Mask, Set/Reset |
| Sequencer (`3C4/3C5`) | Map Mask (2), Memory Mode (4); Clocking Mode (1) char-width feeds the dot count |
| Graphics Ctrl (`3CE/3CF`) | Set/Reset (0), Enable Set/Reset (1), Color Compare (2), Data Rotate/Function (3), Read Map (4), Mode (5), Color Don't Care (7), Bit Mask (8) |
| Attribute (`3C0`) | 16 palette entries, Mode Control (10h), Overscan (11h), Pixel Pan (13h); the `3DA` flip-flop reset. Plane Enable (12h) and Color Select (14h) are **stored, not yet applied** (unused in mode 0Dh; applying 12h's default 0 would blank the screen) |
| CRTC (`3D4/3D5`) | Start Address Hi/Lo (0C/0D), Offset (13h), text cursor (0E/0F); full vertical timing carried in `CrtcTiming` |
| DAC (`3C7/3C8/3C9`) | Read/write index + 6-bit RGB data |
| Status (`3DA`) | Display-disabled (bit 0), vertical retrace (bit 3), beam-derived |
| Beam | Cycle-coupled dot clock, catch-up rasterization, per-frame finalize |

Deferred to later slices: the 256 KB display-address wraparound (only the
per-plane 64 KB wrap is modeled), line-compare split screens (and pel-pan forced
to 0 below the split), pel-pan smooth-scroll polish, mode-X / unchained
256-color, the bad-card mid-scanline latch (shake) reproduction,
pixel-granular catch-up, and mid-frame Vertical-Display-End / blank-register
tricks.

## Slice 2 coverage

Slice 2 generalizes the engine to the other standard 16-color planar modes, on a
corrected double-scan model. The datapath, beam, catch-up, and latch rules are
unchanged.

| Mode | Output | Raster | Active | Distinct rows | Double-scanned | Refresh |
|------|--------|--------|--------|---------------|----------------|---------|
| 0Dh  | 320x200 | 320x449 | 400 | 200 | yes | 70 Hz |
| 0Eh  | 640x200 | 640x449 | 400 | 200 | yes | 70 Hz |
| 10h  | 640x350 | 640x449 | 350 | 350 | no  | 70 Hz |
| 12h  | 640x480 | 640x525 | 480 | 480 | no  | 60 Hz |

All four use the 25.175 MHz dot clock, 8-dot characters, and `htotal_chars` 100
(800 dots per line). Per-plane byte pitch is `offset * 2` (offset 20 for the
320-wide mode, 40 for the 640-wide modes). A mode is selected by number through
`Vga::set_mode(u8)`, `Machine::set_vga_mode(u8)`, or `INT 10h, AH=00h` with AL =
0Dh/0Eh/10h/12h (the host clears the Margo latch on a VGA mode-set). The exact
vertical border and blank offsets per mode are conventional values validated
against the FreeVGA reference; the load-bearing fields are resolution,
double-scan, refresh family, and pitch.

**Corrected double-scan model.** The beam and catch-up count in scanlines
(0..Vertical Total), and one scanline emits exactly one raster row, so the raster
height is `Vertical Total` (not doubled). Double-scan divides the source address:
a scanline reads source row `counter_line / (max_scan + 1)`, so a doubled mode
holds each VRAM row for two scanlines. `Vertical Display End` is in scanline
units. (Slice 1 doubled the output instead, which made 0Dh's raster roughly twice
its real height; this is corrected here.)

## Latch rules

- **CRTC Start Address (0C/0D) latches once per frame, at the start of vertical
  retrace.** A mid-frame write is buffered (`pending_start`) and snapshotted into
  the active address when the frame finalizes — it never tears the current frame,
  it applies to the next. Both bytes assemble through the same pending value, so
  neither byte alone can tear. This is what makes Commander Keen's hardware scroll
  smooth on a good card.
- **Attribute Pixel Pan (13h) is not latched.** It applies at the scanline of the
  write (live during scanout), so per-scanline pel-pan effects work.

The bad-card "shake" (cards that latch start-address/pel-pan at hblank instead of
vblank) is a later opt-in; modeling the good-card per-frame latch correctly is its
prerequisite.

## Raster layout (CRTC-derived, top-justified)

The presented buffer spans the mode's **full visible frame** (active + border +
vertical blank). Each row is colored by the CRTC region it falls in:

- **Active** (`counter_line < Vertical Display End`): the 4-bit planar index per
  pixel, pel-pan-shifted, mapped through the Attribute palette to a DAC index.
- **Border** (`[Vertical Display End, Start Vertical Blank)` and `[End Vertical
  Blank, Vertical Total)`): the Overscan color (this is the border-flash region).
- **Blank** (`[Start Vertical Blank, End Vertical Blank)`): black.

The active field is **top-justified**: it begins at counter line 0, so a mode
that shortens `Vertical Display End` leaves the shortfall **at the bottom**, not
centered. This is the Jazz Jackrabbit behavior (its tweaked 320×199 / ~60 Hz mode
renders 199 active lines top-justified, the rest below held black) — derived from
the registers, never from a centering rule.

**Unit discipline:** the beam and the catch-up loop count in scan-counter lines
(0..Vertical Total), and each scanline emits one raster row, so the raster height
is `Vertical Total`. Double-scan divides the *source* address (source row =
counter line / (max_scan + 1)), so a doubled mode holds each VRAM row for two
scanlines; it does not multiply the output. `Vertical Display End` is in scanline
units. Exact per-mode border offsets are validated against real hardware as those
modes land.

## Documented divergences (fidelity directive)

1. **Aspect correction is a downstream renderer layer**, not a divergence — the
   rasterizer is intentionally square-pixel.
2. **Presentation is pull-based.** The rasterizer finalizes into a machine-owned
   buffer the host reads (`ActiveDisplay::VgaRaster` → `Machine::vga_raster`);
   there is no push or callback.
3. **Multi-frame skips.** A long `HLT` spanning several frames surfaces only the
   final frame's visible state (the host samples at display rate).
4. **Scanline catch-up granularity.** Mid-*scanline* register changes
   (Amiga-style >256-colors-per-line) are not modeled; the catch-up renders whole
   counter lines.
5. **One-instruction beam lag.** A `3DA` read reflects the cycle counter as of the
   previous retired instruction (device advance runs after the instruction). The
   beam is deterministic, not cycle-exact at the instruction boundary.
6. **No analog signal.** The core emits a raster buffer, not VGA sync, so a real
   monitor/scaler losing lock on a nonstandard line count (e.g. Jazz on a cheap
   LCD scaler) cannot occur — the nonstandard frame is rendered faithfully.
7. **Separate 256 KB VGA buffer.** VGA planar memory is its own buffer, distinct
   from Margo's 4 MB linear VRAM. Unifying matters only if software maps the same
   bytes through both apertures at once — no DOS-era planar game does.

## Slice-1 implementation approximations

These are simplifications recorded for later tightening, not hardware behavior:

- **Start address** is treated as a direct byte offset within a plane (real VGA
  applies word/byte/dword addressing modes); page flips at byte boundaries work.
- **CRTC Offset register** stores the raw register value; the per-plane byte pitch
  is `2 × offset` (so mode 0Dh's offset register = 20 gives a 40-byte pitch).
- **The A0000 aperture** routes to the planar datapath when the core is in a
  planar mode, and to the flat mode-13h buffer otherwise; the planar window is the
  64 KB `A0000..AFFFF` range.
- **Full CRTC vertical timing** (`text_03h`, `mode_0dh`) uses conventional values;
  per-mode timing tables are added as modes land.

## Proof

Slice 1's done-signal is the copper-bar seam, verified at two levels:
`vga::tests::mid_frame_palette_change_splits_the_raster_at_the_beam_row` (unit)
and `copper_bar_split_through_the_machine` (end to end through the bus: an A0000
planar fill, a mid-frame palette change via the attribute port, the beam advanced
by the machine clock, and the presented raster showing the split in the active
region).

Slice 2's done-signal is `int10_sets_mode_12h_then_draws_and_presents_640x480`:
a guest `INT 10h, AH=00h, AL=12h` selects mode 12h, a plane fill through the
A0000 datapath draws into it, the machine clock completes a frame, and the
presented raster is the expected 640x480 with the drawn pixel in place. The
double-scan correction is pinned by `mode_0dh_raster_height_equals_vtotal_not_doubled`
and `double_scan_holds_each_source_row_for_two_scanlines`.
