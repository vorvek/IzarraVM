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
| Legacy | Text mode (now a `VgaRaster` through the raster engine, slice 9; `frame()` / `TextFrame` stay as the headless cell view), mode `13h` (chained 256-color, routed through the same raster engine as the planar and mode-X paths), the 256-entry DAC, the text cursor — folded in from the former `VgaTextMode` |
| Planar memory | Four-plane model, write modes 0–3, read modes 0–1, latches, data-rotate/ALU, Map Mask, Bit Mask, Set/Reset |
| Sequencer (`3C4/3C5`) | Map Mask (2), Memory Mode (4); Clocking Mode (1) char-width feeds the dot count |
| Graphics Ctrl (`3CE/3CF`) | Set/Reset (0), Enable Set/Reset (1), Color Compare (2), Data Rotate/Function (3), Read Map (4), Mode (5), Color Don't Care (7), Bit Mask (8) |
| Attribute (`3C0`) | 16 palette entries (power-up identity: register N = N), Mode Control (10h), Overscan (11h), Pixel Pan (13h), readback through `3C1`; the `3DA` flip-flop reset. Plane Enable (12h) and Color Select (14h) are **stored, not yet applied** (unused in mode 0Dh; applying 12h's default 0 would blank the screen) |
| CRTC (`3D4/3D5`) | Start Address Hi/Lo (0C/0D), Offset (13h), text cursor location (0E/0F) and shape (0A/0B); full vertical timing carried in `CrtcTiming` |
| DAC (`3C7/3C8/3C9`) | Read/write index + 6-bit RGB data; the pel mask (`3C6`) is stored and applied to the rendered DAC index. The 256-entry palette powers up to the stock VGA mode-13h default (the 16 EGA colors with brown at index 6, then the gray and color ramps) |
| Misc (`3C2/3CC`) | Misc Output write/readback (clock-select bits stored, not applied to the dot clock) |
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
pixels) and 320x200 unchained (mode Y). Unlike the chained mode 13h (whose chain-4
CPU write decode auto-routes byte N to plane N & 3 at plane-offset N >> 2), these run
the 256 KB planar memory with chain-4 off, so each CPU write targets the Map Mask
plane and the raw offset; each pixel is a full 8-bit DAC index living in one of four
column-interleaved planes. The CRTC display scanout is shared by both (see "Slice 8
coverage").

| Area | Covered in slice 5 |
|------|--------------------|
| Entry | A guest write clearing the Sequencer Memory Mode (04h) chain-4 bit while in mode 13h enters `VideoMode::ModeX` with a 320x200 unchained base; setting the bit again reverts to chained mode 13h |
| Geometry | Honored guest CRTC vertical timing (06h/07h/09h/10h/11h/12h/15h/16h) via a raw register file and `recompute_vertical_timing`, so Abrash's bang retunes to 320x240 (vtotal 527, vdisp_end 480, 240 source rows double-scanned to 480) and a bare mode Y stays 320x200 |
| Scanout | `render_256color_row`: `plane = x & 3`, plane offset `row_base + (x >> 2)`, the byte as the 8-bit DAC index directly (no attribute palette, no 6-bit mask) |
| Writes | The existing planar datapath (Map Mask + write modes + latches); the A0000 planar aperture is the full 64 KB |
| Page flipping | The existing start-address vretrace latch; `render_256color_row` bases `row_base` on it |
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

1. **Mode X combined with the line-compare split landed in slice 6.** The split
   (slice 4) now applies to the mode-X scanout via the shared `split_origin`; see
   "Slice 6 coverage."
2. **Mode-X pel-pan landed in slice 7.** The 8-bit 256-color pixel pan is now
   applied as a 0-3 fine column shift via the shared `pel_pan`; see "Slice 7
   coverage." Byte-granular start-address scroll and page flipping (the primary
   mode-X mechanism) remain the coarse-scroll path. Chained mode 13h shares the
   planar scanout and the same 0-3 pel-pan (see "Slice 8 coverage"), so there is no
   distinct 13h pel-pan to defer.
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

## Slice 6 coverage

Slice 6 extends the slice-4 CRTC Line Compare split to the unchained 256-color
(mode X) scanout, so a mode-X scrolling playfield with a hardware-split status
panel renders the panel from offset 0 independent of the top-region start-address
scroll. The split is address-only: the column-interleaved mode-X addressing is
unchanged, and only the `row_base` origin changes below the split.

| Area | Covered in slice 6 |
|------|--------------------|
| Shared origin | `split_origin`: the display-address origin decision (start address vs the reload-to-zero below-split region) factored out of the 16-color path and shared by both render paths |
| Mode-X split | `render_256color_row` routes through `split_origin`; below the compare row the address counter reloads to 0 and source-row counting restarts at `line_compare + 1` |
| Comparison units | Scan-counter lines, the same units the beam and the other vertical-timing registers use; not divided by the double-scan factor |
| Double-scan | Divides the split source row exactly as it divides the top region, so a 320x240 split holds each split source row for two scanlines |

The semantics carry over verbatim from slice 4, pinned to Abrash's Graphics
Programming Black Book chapter 30 (line compare) and chapters 47-49 (Mode X). No
new hardware behavior is introduced: the line-compare reload is color-depth-
independent, operating on the linear display-address counter below the
sequencer/attribute color path.

The done-signal is equality: the top region renders scrolled by the start address
and the bottom region renders from offset 0, proven at unit
(`vga::tests::mode_x_line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero`,
`mode_x_line_compare_split_starts_on_the_line_after_the_match`,
`mode_x_line_compare_compares_in_scan_counter_units_double_scanned`) and
end-to-end (`mode_x_line_compare_split_through_the_machine`) levels.

Target, honesty note: the mode-X split status bar is the 256-color scroller genre
workload (a locked panel under a scrolling playfield). A specific released-source
320x240 title was not inspected, matching the slice-5 honesty note.

Divergences (fidelity directive):

1. **Mode-X pel-pan-below-split forcing landed in slice 7.** Mode X now has
   pel-pan (0-3), and the AC Mode Control (10h) bit 5 forcing applies to it
   through the shared `pel_pan`, exactly as slice 6 anticipated. The
   address-only split here has no further pel-pan dependency.

## Slice 7 coverage

Slice 7 adds the Attribute Horizontal Pixel Panning register (AC index 13h) to
the unchained 256-color (mode X) scanout, the fine 0-3 pel sub-scroll that
pairs with the slice-5 start-address coarse scroll to make Abrash's mode-X
smooth-scroll loop (pan 0 -> 1 -> 2 -> 3, then bump start + 1). It also carries
the pel-pan-below-split forcing (AC Mode Control 10h bit 5) over to mode X for
free, restoring the symmetry the 16-color path already had.

| Area | Covered in slice 7 |
|------|--------------------|
| Shared pel-pan | `pel_pan`: the effective pel-pan decision (the 13h value masked to 0-15, forced to 0 below the split when AC 10h bit 5 is set) factored out of the 16-color path and shared by both render paths |
| Mode-X pel-pan | `render_256color_row` derives `pan = pel_pan(below_split) & 0x03` (one plane per pel, so the fine range is 0-3; a pan of 4 equals a start-address bump) and addresses column x as plane `(x+pan)&3` at plane offset `row_base + ((x+pan)>>2)` |
| Below-split forcing | The AC 10h bit 5 forcing applies to mode X through the same `pel_pan`, so a below-split scanline ignores the pan exactly as the 16-color path does |
| Live (not latched) | Pel-pan applies at the scanline of the write, so per-scanline mode-X pel-pan raster effects work, as they do for 16-color |

The semantics are pinned to Abrash's Graphics Programming Black Book chapters
47-49 (Mode X) and chapter 30 (the split / pel-pan principle), with the register
and the below-split bit confirmed against RBIL `PORTS.B` (AC index 13h,
table P0668; AC Mode Control 10h bit 5, table P0664). Abrash states the pel-pan
range as the pixel count between coarse start-address bumps (8 px per byte for
16-color -> 0-7); applied to mode X's 4-px-per-address-unit organization that is
0-3. Chained mode 13h shares this scanout and the 0-3 range (see "Slice 8
coverage").

The done-signal is equality, proven at unit
(`vga::tests::mode_x_pel_pan_shifts_the_column_origin_by_the_pan_value`,
`mode_x_pel_pan_rotates_the_plane_sequence`,
`mode_x_pel_pan_below_split_is_forced_to_zero_only_when_enabled`) and
end-to-end (`mode_x_pel_pan_smooth_scroll_through_the_machine`) levels: a
non-zero pel-pan scans out the fine-shifted plane at the leftmost column, as its
full 8-bit DAC index.

Target, honesty note: the mode-X smooth scroll is the 256-color scroller genre
workload (a playfield that scrolls a fraction of a pixel per frame), the same
genre as the slice-5/6 mode-X coverage. A specific released-source title was not
inspected, matching the prior honesty notes. The chained mode-13h linear-buffer
scroller (Doom-class mode 13h) is covered in slice 8.

Divergences (fidelity directive):

1. **Chained mode 13h pel-pan landed in slice 8.** Chained mode 13h shares the
   planar 256-color scanout with mode X, so it inherits the 0-3 pel-pan (one plane
   per pel, four pels per plane-offset address) and has no distinct pel-pan to
   defer; see "Slice 8 coverage."
2. **CRTC 08h byte panning and preset-row-scan re-alignment at the split stay
   deferred** (slice-4 divergence 4), unchanged for mode X.
3. **Pel-pan values 4-7 fold into a start-address bump** in mode X and are
   masked out (`& 0x03`); they are beyond-useful, not a distinct behavior.

## Slice 8 coverage

Slice 8 deletes the early linear-buffer shortcut for chained mode 13h and routes
it through the same raster engine (beam, catch-up, finalize, `VgaRaster`
presentation) as the planar and mode-X paths. The key finding is that chain-4
(Sequencer Memory Mode 04h bit 3) changes **only the CPU write/read decode**; the
CRTC display scanout reads the raw four-plane VRAM identically whether chain-4 is
on (mode 13h) or off (mode X). So mode 13h reuses the mode-X renderer; the only
13h-specific behavior is the chain-4 CPU write routing. The stale flat
`Framebuffer` field, its accessors, the separate `ActiveDisplay::Mode13h`
presentation, and the 64000-byte aperture cap are all gone.

| Area | Covered in slice 8 |
|------|--------------------|
| Renderer | `render_256color_row` (renamed from `render_modex_row`): the shared planar scanout, `plane = x & 3`, plane offset `row_base + (x >> 2)`, the byte as the 8-bit DAC index; `render_scanline` dispatches both `Mode13h` and `ModeX` to it |
| Chain-4 writes | `cpu_write_chain4` / `cpu_read_chain4`: byte N routes straight to plane `N & 3` at plane-offset `N >> 2`, bypassing the planar datapath (Abrash, Graphics Programming Black Book ch.47) |
| Timing | `CrtcTiming::mode13h`: standard 320x200 70 Hz, double-scanned to 400 scanlines (200 source rows), offset 40; `set_mode13h` installs it, resets the beam, and resizes the work buffer |
| Bus | The 64 KB A0000 window serves Planar, ModeX, and Mode13h alike; mode 13h picks the chain-4 decode; `active_display` returns `VgaRaster` for all three |
| Start-address vretrace latch | The generic CRTC 0C/0D latch applies to mode 13h by construction |
| CRTC Line Compare split | `split_origin` applies to mode 13h by construction |
| AC pel-pan (0-3) | The shared `pel_pan` applies to mode 13h by construction (same mask, same below-split forcing as mode X) |

The semantics are pinned to Abrash's Graphics Programming Black Book chapter 47
("Mode X: 256-Color VGA Magic"): the `M = N/4, P = N mod 4` scanout definition
and the statement that entering mode X from mode 13h needs "no need to alter any
horizontal values, because mode 13H and Mode X both have 320-pixel horizontal
resolutions." The 0-3 pel-pan range is corroborated by RBIL `PORTS.B` (AC Mode
Control 10h bit 6 PELCLK/2, the 8-bit-color gate set only in mode 13h; AC index
13h table P0662) and FreeVGA (AC 10h bit 6 "8BIT ... set to 0 in all other
modes"): in 256-color the serializer emits four pixels per character clock, so
the useful pel-pan range is 0 to 3.

The done-signal is equality, proven at unit
(`vga::tests::mode13h_scanout_is_column_interleaved_8bit_direct`,
`mode13h_chain4_write_routes_byte_n_to_plane_n_mod_4`,
`mode13h_pel_pan_shifts_the_column_origin_by_the_pan_value`,
`mode13h_pel_pan_below_split_is_forced_to_zero_only_when_enabled`,
`mode13h_line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero`) and
end-to-end (`mode13h_320x200_through_the_machine`,
`mode13h_pel_pan_smooth_scroll_through_the_machine`,
`mode13h_line_compare_split_through_the_machine`) levels: a chain-4 write scans
out at its pixel with its full 8-bit value, the pel-pan rotates the column
origin, and the split reloads to offset 0.

Presentation dimension note: mode 13h goes from the old 320x200 flat surface to
the full-frame raster (320 x `vtotal` ~449, active region 400 scanlines = 200
source rows double-scanned), consistent with how mode 0Dh and mode X already
present. The active content for a standard linear 320x200 fill is byte-identical
to the old flat path: chain-4 writes byte N where the shared scanout reads pixel
N.

Target, honesty note: the workload is the linear-buffer 256-color scroller genre,
a playfield drawn to the A0000 linear aperture under chain-4 (Doom-class mode
13h). The chain-4 write pattern and the shared planar scanout are pinned to
Abrash ch.47. No released-source title was inspected for the chain-4 write
pattern; the Doom source is public and runs mode 13h, but it was not consulted in
this slice.

Divergences (fidelity directive):

1. **Mode-13h guest vertical-CRTC-bang retuning is deferred.** Mode 13h installs
   fixed 320x200 70Hz timing at entry; only mode X honors the guest's vertical
   CRTC bang. The tweaked-13h geometry (320x400, 360-wide) is mode-X-adjacent
   territory and stays deferred.
2. **CRTC 08h byte panning and preset-row-scan re-alignment at the split stay
   deferred** (slice-4 divergence 4), unchanged for mode 13h.
3. **Pel-pan values 4-7 fold into a start-address bump** and are masked out
   (`& 0x03`); they are beyond-useful, not a distinct behavior.

## Slice 9 coverage — text-mode scanout and the loadable character generator

Slice 9 routes text mode through the same beam-coupled raster engine as the
graphics modes, with a built-in CP437 character generator, and makes the font
store writable through the Sequencer Character Map Select and the `INT 10h AH=11h`
font services. Text now presents a `VgaRaster` (the GUI no longer rasterizes the
cells itself); `frame()` / `TextFrame` / `screen_text()` remain as the cell view
for the headless ASCII dump.

| Area | Covered in slice 9 |
|------|--------------------|
| Character generator | The three ROM glyph fonts (8x8, 8x14, 8x16) byte-for-byte from the LGPL VGABios `vgafonts.h`; the 8x16 table is the mode-03h default |
| Sequencer Clocking Mode (1) | Bit 0 selects 8-dot vs 9-dot text; the renderer derives the effective character width |
| Text scanout | `render_text_row`: 80 columns of `max_scan + 1` scanlines each (16 for 720x400 mode 03h), the CRTC Line Compare split via the shared `split_origin`, foreground/background nibbles mapped through the 16-entry Attribute palette to DAC indices with the pel mask applied |
| 9-dot geometry | The 9th pixel column replicates the 8th for the box-drawing glyphs 0xC0-0xDF (a solid line join) and is the background otherwise |
| Attribute decode | AC Mode Control 10h bit 3 toggles blink (attribute bit 7 = blink, 8 backgrounds) vs background intensity (bit 7 = intensity, 16 backgrounds); blink collapses the foreground to the background on its hide phase |
| Writable font store | Eight 256-glyph tables (32 bytes per slot) seeded from the ROM 8x16 font; the renderer reads the active table |
| Sequencer Character Map Select (3) | Map A (bits 0, 1, 4) selects the active table for 256-glyph text |
| `INT 10h AH=11h` | AL=00/10 user font (ES:BP), AL=01/11 ROM 8x14, AL=02/12 ROM 8x8, AL=04/14 ROM 8x16, AL=03 set block specifier; the 1x variants reprogram the CRTC character height |
| Presentation | Text mode presents a `VgaRaster` (`ActiveDisplay::VgaRaster`); `frame()` / `TextFrame` stay for the headless cell view |

The semantics are pinned to the IBM VGA character generator and Abrash's
Graphics Programming Black Book (the 9-dot replicate rule, the Character Map
Select decode), RBIL `PORTS.B` (AC Mode Control 10h bit 3, the blink/background-
intensity toggle), and RBIL `INT 10h AH=11h` (register conventions verified
against the LGPL VGABios `biosfn_load_text_*`).

The done-signals are equality, proven at unit
(`font::tests::cp437_rom_glyphs_match_the_reference`,
`vga::tests::text_scanout_renders_cp437_glyph_rows_at_9x16`,
`vga::tests::text_scanout_maps_attribute_through_the_palette_to_dac`,
`vga::tests::text_scanout_blink_toggles_foreground_only_when_enabled`,
`vga::tests::text_scanout_presents_a_720x400_raster`,
`vga::tests::font_store_is_writable_per_table`,
`vga::tests::sequencer_char_map_select_picks_the_active_font`) and end-to-end
(`text_mode_scanout_through_the_machine`, `int10_11h_loads_user_font`,
`int10_11h_loads_rom_8x16`) levels.

Target, honesty note: text mode covers a large class of titles (text adventures,
roguelikes, BBS door games, shareware menus, boot/setup screens). The CP437
glyph data, the 9-dot geometry, the attribute/blink decode, and the font-store
and AH=11h behavior are verified directly against the code paths above; no
released-source title was inspected.

Divergences (fidelity directive):

1. **`AH=11h AL=30` (get font info) and `AL=20-24` (graphics-mode text) are
   deferred.** AL=30 returns a far pointer to the active font; the graphics-mode
   text services render text in graphics modes. Both are the Slice B follow-up
   (graphics-mode text); see the Slice A coverage for the text-mode work that did
   land.

The remaining slice-9 text-mode gaps closed in Slice A:

2. **The hardware text cursor is rendered (slice 11), now including cursor skew
   (Slice A).** The cursor renders reverse video on the cell at
   `cursor_offset`; the `frame()` / `TextFrame` cursor offset stays for the
   headless ASCII view. Cursor skew (CRTC 0Bh bits 5-6) now delays the cursor
   onset by 0-3 character clocks (see the Slice 11 coverage and Slice A).
3. **The blink cadence is modeled.** Attribute blink and cursor blink share one
   16-on / 16-off vertical-retrace phase (`blink_hide_phase`); the hardware
   cadence is no longer a refinement (Slice A).
4. **Text-mode pel-pan (AC 13h) and start-address smooth scroll are applied.**
   The text scanout reads cells from the CRTC start-address origin and shifts the
   column origin by the AC 13h pel-pan, with the 9-dot replicate shifting with
   the cell (Slice A).
5. **CRTC 08h preset-row-scan is modeled.** Bits 4-0 offset the first displayed
   font scanline (vertical sub-row scroll); bits 6-5 add a byte pan to the
   origin; both reset below the line-compare split (Slice A).
6. **512-character / dual-font mode is modeled.** `char_map_b_decode` selects
   the second font table; attribute bit 3 selects map A vs map B and the
   foreground drops to 8 colors while active (Slice A). The Tier-2/3 graphics
   register gaps (16-color Color Select 14h, guest vertical timing, odd/even
   addressing) stay parked in the backlog spec; none blocks a standard title.

## Slice 10 coverage — default palettes, BIOS video services, and ports

Slice 10 loads the stock default palettes, adds the host-locked (HLE) `INT 10h`
video services a title reaches when it drives video through the BIOS rather than
port-banging, and fills in the remaining unhandled register ports. The raster
engine's geometry and scanout are unchanged except that the DAC pel mask now
gates the rendered DAC index (the default `0xFF` is a no-op).

| Area | Covered in slice 10 |
|------|---------------------|
| Default palettes | The 16 ATC palette registers power up to identity (N → N), and the 256-entry DAC powers up to the stock VGA mode-13h palette (byte-for-byte from the LGPL VGABios `palette3`) |
| `INT 10h AH=00h` | Mode set including the text-family return (`AL` 0-7) via `Vga::set_text_mode`, alongside the existing planar (0D-12) and chained-13h branches |
| `INT 10h AH=0Bh` | Border/overscan color (`BH=0`); the CGA palette select (`BH=1`) is deferred |
| `INT 10h AH=10h` | ATC palette register set/get individual (`AL=00/07`) and block (`AL=02`), overscan set (`AL=01`), DAC individual (`AL=10/15`) and block (`AL=12/17`) set/get |
| Ports | Misc Output (`3C2` write / `3CC` read), DAC pel mask (`3C6`), ATC readback (`3C1`); the pel mask is applied in both render paths |
| CRTC | Cursor-shape registers `0A/0B` are now stored and read back (cursor *location* 0E/0F was already handled) |

The HLE services run *after* the software `INT` retires (registers intact) and
operate on register state through public `Vga` accessors rather than the port
flip-flops. VBE (`AH=4Fh`) still routes to Margo; `AH=00/0B/10` route to the VGA
core. Unhandled `INT 10h` services leave `AX` unchanged, so adding these is
strictly additive.

Divergences (fidelity directive):

1. **Misc Output clock-select is stored, not applied.** `3C2` bits 2-3 are read
   back faithfully through `3CC`, but the dot clock stays mode-fixed; retuned
   refresh (the 360-wide modes) is not modeled.
2. **The rare `AH=10h` sub-functions are deferred**: overscan get (`AL=08`), the
   palette/overscan block get (`AL=09`), intensity/blink toggle (`AL=03`), color
   paging (`AL=13/14/1A`), and the font services (`AL=20-24`). The font services
   belong with the loadable-character-generator slice.

## Slice 11 coverage — text-mode hardware cursor

Slice 11 renders the hardware text cursor through the same raster engine
as the rest of text mode. The cursor shape (`cursor_start` /
`cursor_end`) and location (`cursor_offset`) were already stored and read
back in slice 10; slice 11 decodes them in `render_text_row` and applies
reverse video on the active scanlines.

| Area | Covered in slice 11 |
|------|---------------------|
| Cursor fill | Reverse video: on the cell at `cursor_offset`, the foreground and background swap on the active cursor scanlines (the Bochs `draw_char_common` text-cursor path; QEMU's solid-foreground approximation coincides for the blank-cell case) |
| Cursor shape | CRTC 0A bit 5 disables the cursor; bits 0-4 of 0A/0B bound the scanline range; a start greater than end wraps to two regions (a faithful superset of Bochs, which treats that case as invisible) |
| Cursor blink | The cursor blinks on the same frame hide phase as attribute blink, but is not gated on the AC Mode Control 10h bit 3 attribute-blink enable |

The semantics are pinned to the IBM VGA character generator and RBIL
`PORTS.B` (CRTC cursor start/end 0A/0B, the bit-5 disable), cross-checked
against the Bochs `draw_char_common` and QEMU `vga_draw_text` text-cursor
paths.

The done-signals are equality at unit level:
`vga::tests::text_cursor_renders_reverse_video_on_the_cursor_cell`,
`vga::tests::text_cursor_respects_start_and_end_scanlines`,
`vga::tests::text_cursor_disable_bit_hides_it`,
`vga::tests::text_cursor_blinks_on_the_frame_phase`, and
`vga::tests::text_cursor_wrap_shape_covers_two_regions`.

Target, honesty note: the hardware text cursor is the one visible defect
left after the slice-9 text scanout, and affects every text title that
shows a cursor. The reverse-video fill rule, the 0A/0B shape decode, and
the blink phase are verified directly against the code paths above; no
released-source title was inspected.

Divergences (fidelity directive):

The slice-11 text-cursor gaps closed in Slice A:

1. **Cursor skew (CRTC 0Bh bits 5-6) is modeled**, as a 0-3 character-clock
   delay of the cursor onset (3 = max delay, not disable); the separate 0Ah
   bit 5 remains the cursor disable (Slice A).
2. **The blink cadence is modeled.** The cursor reuses the attribute-blink
   16-on / 16-off vertical-retrace phase (`blink_hide_phase`); the hardware
   cursor rate is no longer a refinement (Slice A).
3. **Text-mode start-address / pel-pan interaction with the cursor is
   modeled.** The text scanout reads cells from the CRTC start-address
   origin and matches the cursor on the displayed cell's absolute index, so
   the cursor match moves with the start address (Slice A).

## Slice A coverage — text-mode addressing, scroll, and polish

Slice A closes the remaining text-mode cell-scanout gaps over seven commits.
The text aperture grows to the full 32 KB (B8000-BFFFF, eight 4096-byte pages)
and `render_text_row` is reworked to read cells from the CRTC start-address
origin, with pel-pan, preset-row-scan, byte pan, dual-font selection, and cursor
skew applied in the per-cell loop. `frame()` / `TextFrame` / `screen_text()`
follow the same origin so the headless cell view matches the pixels. The vretrace
start-address latch is honored and cleared on `set_text_mode`.

| Commit | Area | Covered in Slice A |
|--------|------|--------------------|
| C1 | 32 KB aperture + start-address scanout | `VGA_TEXT_MEMORY_SIZE` grows to 32768; `render_text_row` reads from the start-address origin above the line-compare split and from 0 below it; `pending_start` cleared on mode set |
| C2 | `INT 10h AH=05h` set-display-page | `start_address = page * 2048` (page_size 4096 bytes) routed through the vretrace latch |
| C3 | AC pel-pan (13h) in text | Column origin shifted by the 0..char_width pel-pan; the 9-dot replicate shifts with the cell; below-split forcing via AC 10h bit 5 reuses the shared `pel_pan` |
| C4 | CRTC 08h preset-row-scan | Bits 4-0 offset the first displayed font scanline (vertical sub-row scroll); bits 6-5 add a byte pan to the origin; both reset below the split |
| C5 | 512-character / dual-font | `char_map_b_decode` selects the second font table; attribute bit 3 set selects map B (Abrash/vgabios/Bochs polarity; FreeVGA's opposite wording is recorded as a known conflict and overridden); the foreground drops to 8 colors while active |
| C6 | Cursor skew (0Bh bits 5-6) | A 0-3 character-clock delay of the cursor onset; skew 3 is max delay, not disable (the disable stays 0Ah bit 5) |
| C7 | Unified blink cadence | Attribute blink and cursor blink share one `blink_hide_phase` helper at the hardware 16-on / 16-off vertical-retrace rate |

The semantics are pinned to Abrash's *Graphics Programming Black Book*, RBIL
`PORTS.B` (CRTC 08h preset-row-scan, 0Bh skew, the start-address / cursor-location
word units) and `INT 10h AH=05h`, the LGPL vgabios, and Bochs/QEMU, recorded in
the per-slice confirm notes. Mode 03h is word mode, so the start-address and
cursor-location registers are word/cell addresses (the displayed cell at
`(row, col)` reads `text_memory[(start + row*offset + col) * 2]`).

The done-signals are equality at unit level:
`vga::tests::text_start_address_scrolls_the_display_origin`,
`vga::tests::text_start_address_below_the_split_starts_from_zero`,
`vga::tests::text_memory_aperture_is_32kb_eight_pages`,
`vga::tests::frame_cell_view_follows_the_start_address`,
`vga::tests::text_pel_pan_shifts_the_column_origin`,
`vga::tests::text_pel_pan_below_split_forces_zero_when_enabled`,
`vga::tests::text_pel_pan_9dot_replicates_the_shifted_box_glyph`,
`vga::tests::text_preset_row_scan_offsets_the_first_font_line`,
`vga::tests::text_byte_pan_shifts_whole_cells`,
`vga::tests::text_preset_row_resets_below_the_split`,
`vga::tests::char_map_b_decode_picks_the_second_font_table`,
`vga::tests::attribute_bit_3_selects_the_font_in_512_char_mode`,
`vga::tests::int10_11h_loads_two_fonts_for_512_char_text`,
`vga::tests::text_cursor_skew_delays_the_cursor_onset`,
`vga::tests::text_cursor_skew_three_is_max_delay_not_disabled`,
`vga::tests::attribute_blink_runs_at_the_hardware_cadence`, and
`vga::tests::text_cursor_blinks_at_the_hardware_cadence`, with the page-flip path
proven end-to-end through the machine at
`int10_ah05_sets_the_text_page_via_start_address` and
`int10_ah05_page_flip_scrolls_through_the_machine`.

Out of Slice A (Slice B, graphics-mode text): `AH=11h AL=30` get-font-info and
`AL=20-24` graphics-mode text setup, plus the graphics-mode text-output services
(`AH=09/0E/13`) and BIOS cursor tracking (`AH=02/03`). Graphics-mode pel-pan /
preset-row-scan for the planar paths stay parked as before; this slice applies
08h / 13h to text only.

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
  (Text mode emits the foreground/background DAC index per pixel the same way;
  see "Slice 9 coverage.")
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
8. **Misc Output clock-select is stored, not applied.** The `3C2` bits are read
   back through `3CC`, but the dot clock stays mode-fixed; retuned refresh is not
   modeled (see Slice 10).

## Slice-1 implementation approximations

These are simplifications recorded for later tightening, not hardware behavior:

- **Start address and offset pitch** are now handled by the faithful
  display-address counter and the byte/word/doubleword transform (see "Slice 3
  coverage"); these are no longer approximations.
- **The A0000 aperture** routes to the planar datapath when the core is in a
  planar mode, to the chain-4 decode in mode 13h, and to the unchained planar
  datapath in mode X; the window is the 64 KB `A0000..AFFFF` range. (Mode 13h was
  once a separate flat buffer; slice 8 routed it through the raster engine.)
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
