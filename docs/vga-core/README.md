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

Deferred to later slices: pel-pan smooth-scroll polish, the bad-card
mid-scanline latch (shake) reproduction, pixel-granular catch-up, and
mid-frame Vertical-Display-End / blank-register tricks.

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

## Slice 3 coverage

Slice 3 replaces the slice-1 display-address approximations with the faithful
display-address counter. The address presented to the planes and the point at
which it wraps now match hardware.

| Area | Covered in slice 3 |
|------|--------------------|
| Addressing mode | Byte / word / doubleword, from CRTC Mode Control (17h) bit 6 and Underline Location (14h) bit 6, with the Address Wrap Select (17h bit 5) choosing MA13 vs MA15 for word mode |
| Transform | `display_offset`: byte = identity; word = rotate left 1 with MA13/MA15 into bit 0; doubleword = rotate left 2 with MA13/MA12 into bits 1/0 |
| Counter | `start_address + source_row*offset*2 + byte_col`; the `offset*2` per-scanline increment is mode-independent |
| Wrap | 16-bit counter wraps at 64 KB per plane; one counter value addresses all four parallel 64 KB planes, so this is the 256 KB display wraparound |
| Registers | CR17 (Mode Control) and CR14 (Underline Location) are live state on `CrtcTiming`, defaulted per mode (16-color planar = 0xE3 byte mode) and writable through 3D4/3D5 |

Scope: 16-color planar modes only. Mode-X / unchained 256-color wrap landed in
slice 5. The done-signal is the seam: wrapped scanout pixels
EQUAL the top-of-VRAM pixels (`vga::tests::byte_mode_wrap_scanout_equals_top_of_vram`
and `display_address_wrap_seam_through_the_machine`).

Divergences (fidelity directive):

1. **Doubleword bit positions pending reference confirmation.** Implemented as
   MA13 -> bit 1, MA12 -> bit 0 from the recollected FreeVGA/Matrox transform; to
   be confirmed against an unbroken mirror. Unexercised by any in-scope workload.
2. **CR17 bits 0/1 row-scan substitution not modeled.** Address bits 13/14 always
   come from the counter. The 16-color planar modes set these bits (substitution
   off), so this matches hardware for every in-scope mode; CGA/Hercules-compat
   modes that clear them are out of scope.
3. **Address-clock dividers not modeled.** CR17 bit 3 (count by 2) and CR14 bit 5
   (count by 4) are stored but not applied to the counter rate. The 16-color
   planar modes select no division.

## Slice 4 coverage

Slice 4 adds the CRTC Line Compare split screen. When the beam's scanline reaches
the Line Compare value, the display-address counter reloads to offset 0 for the
rest of the frame, so the region below the split shows VRAM from offset 0
independent of the start-address scroll above it. This is the hardware split used
for a scrolling playfield with a static status panel, the companion to the slice-3
hardware scroll.

| Area | Covered in slice 4 |
|------|--------------------|
| Line Compare | Full 10-bit value assembled from CRTC 18h (bits 0-7), the Overflow register 07h bit 4 (bit 8), and the Maximum Scan Line register 09h bit 6 (bit 9); live state on `CrtcTiming`, defaulted 0x3FF (disabled) per mode |
| Split | Below the split (`counter_line > line_compare`) the address counter reloads to 0; rows are counted from the first split scanline (`line_compare + 1`), so the bottom region addresses VRAM from offset 0 |
| Comparison point | Against the scan-counter line, in the same scan-counter units the beam and the other vertical timing registers use; not divided by the double-scan factor |
| Pel-pan below split | Forced to 0 below the split when Attribute Mode Control (10h) bit 5 is set ("enable pixel panning: 0 = all, 1 = up to line compare"); applies everywhere when clear |
| Registers | CRTC 18h / 07h / 09h writable through 3D4/3D5; each write updates only its line-compare bit |

The semantics are pinned to Abrash's Graphics Programming Black Book chapter 30
("Video Est Omnis Divisa"): the scanline matching line compare is the last line
above the split, the split starts on the following line, and the split reloads the
display-memory pointer to zero. The register assembly matches RBIL `PORTS.B`
(CRTC table P0708, Attribute Mode Control table P0664).

The done-signal is equality: the top region renders from a non-zero start address
(scrolled and pel-panned) and the bottom region renders from offset 0 (pel-pan
forced to 0), proven at unit
(`vga::tests::line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero`)
and end-to-end (`line_compare_split_through_the_machine`) levels.

Target, honesty note: the split-screen-with-scroll technique is verified against
Abrash chapter 30. A specific 16-color shipping-title attribution was not confirmed
from released source: the released id EGA sources (Keen Dreams, Catacomb 3-D) do not
program the line-compare register, and the common claim that Commander Keen uses
split-screen for its status bar is not backed by released source. The workload is the
genre, an EGA/VGA 16-color hardware scroller with a locked status panel.

Divergences (fidelity directive):

1. **EGA two-lines-lower split not modeled.** Abrash chapter 30 notes the EGA split
   may display two scan lines lower; this core targets the VGA, where the split
   starts on the line after the match, so the EGA variance is out of scope.
2. **Line Compare bit 9 assembled but unexercised in scope.** Bit 9 only matters for
   splits at scanline 512 or higher; the in-scope modes top out at Vertical Display
   End 480, so no in-scope split reaches it. Assembled for fidelity, flagged like
   slice 3's doubleword bits.
3. **Overflow / Maximum Scan Line non-line-compare fields not honored from guest
   writes.** A 07h or 09h write updates only the line-compare bit; the vertical timing
   high bits (07h) and the double-scan / max-scan fields (09h) stay mode-defaulted,
   consistent with the existing full-timing-via-set_mode simplification.
4. **Byte panning and exact preset-row-scan re-alignment at the split not modeled.**
   CRTC 08h byte panning is not modeled, so it is not reset at the split; the bottom
   region restarts row counting at `line_compare + 1` without a sub-double-scan-row
   phase offset.

## Slice 5 coverage

Slice 5 adds the unchained 256-color planar modes: classic Mode X (320x240, square
pixels) and 320x200 unchained (mode Y). Unlike the chained mode 13h (a flat linear
64000-byte buffer), these run the 256 KB planar memory with chain-4 off, so each
pixel is a full 8-bit DAC index living in one of four column-interleaved planes.

| Area | Covered in slice 5 |
|------|--------------------|
| Entry | A guest write clearing the Sequencer Memory Mode (04h) chain-4 bit while in mode 13h enters `VideoMode::ModeX` with a 320x200 unchained base; setting the bit again reverts to chained mode 13h |
| Geometry | Honored guest CRTC vertical timing (06h/07h/09h/10h/11h/12h/15h/16h) via a raw register file and `recompute_vertical_timing`, so Abrash's bang retunes to 320x240 (vtotal 527, vdisp_end 480, 240 source rows double-scanned to 480) and a bare mode Y stays 320x200 |
| Scanout | `render_modex_row`: `plane = x & 3`, plane offset `row_base + (x >> 2)`, the byte as the 8-bit DAC index directly (no attribute palette, no 6-bit mask) |
| Writes | The existing planar datapath (Map Mask + write modes + latches); the A0000 planar aperture is the full 64 KB |
| Page flipping | The existing start-address vretrace latch; `render_modex_row` bases `row_base` on it |
| Presentation | The `VgaRaster` path, pixels resolved through the 256-entry DAC |

The semantics are pinned to Abrash's Graphics Programming Black Book chapters 47-49
("Mode X"), the origin of the mode (Listing 47.1's 320x240 register dump). The
done-signal is equality: an unchained pixel written at column x scans out at column
x with its full 8-bit value, and the guest's CRTC bang yields a 320x240 raster,
proven at unit (`vga::tests::mode_x_scanout_is_column_interleaved_8bit_direct`,
`guest_crtc_bang_retunes_mode_x_to_320x240`) and end-to-end
(`mode_x_320x240_through_the_machine`) levels.

Target, honesty note: the 320x240 register sequence is Abrash's de-facto-standard
primary source. The released id Wolfenstein 3D source (`WOLFSRC/ID_VL.C`) runs
unchained (chain-4 = 0), the mode-Y 320x200 variant; the 320x240 workload is the
pinball / double-buffered 256-color genre (Pinball Illusions at 320x240). No
released-source 320x240 title was inspected.

Divergences (fidelity directive):

1. **Mode X combined with the line-compare split is not modeled.** The split (slice
   4) applies to the 16-color render path; extending it to the mode-X scanout is
   deferred.
2. **Mode-X pel-pan is not modeled.** The 8-bit 256-color pixel pan is deferred;
   byte-granular start-address scroll and page flipping (the primary mode-X
   mechanism) are modeled.
3. **Only the chain-4 to unchained-planar transition is modeled,** not the full
   odd/even host-addressing matrix.
4. **The 256-color byte is the DAC index directly.** Attribute Mode Control bit 6
   (the 8-bit-color gate) is not modeled as a separate switch; unchained 256-color
   implies it.
5. **Exact 320x240 blank and retrace offsets follow the guest's registers,** with
   the end fields as line-counter compares; the load-bearing fields are 320x240
   resolution, 60 Hz, double-scan, and pitch.
6. **The CRTC End Vertical Retrace (11h) write-protect bit (bit 7) is not enforced.**
   When set, real hardware locks CRTC registers 00h-07h against further writes; the
   core honors all subsequent vertical-timing writes regardless. No in-scope mode-X
   setup depends on the protection.

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

- **Start address and offset pitch** are now handled by the faithful
  display-address counter and the byte/word/doubleword transform (see "Slice 3
  coverage"); these are no longer approximations.
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
