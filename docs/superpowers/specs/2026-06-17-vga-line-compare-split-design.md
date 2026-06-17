# VGA Line-Compare Split Screen (Slice 4) Design

Date: 2026-06-17
Status: Approved (brainstormed with the project owner)
Topic: Legacy VGA core, the CRTC Line Compare register and the hardware split screen

## 0. North star

Slice 4 adds the CRTC Line Compare split screen: when the beam's scanline reaches
the Line Compare value, the display-address counter reloads to 0 for the rest of
the frame, so the region below the split shows VRAM from offset 0 independent of
the start-address scroll above it. This is the hardware split used for a scrolling
playfield with a static status panel. It is the companion to the slice-3 hardware
scroll: a scroller with a locked status bar needs both.

The done-signal is equality, not approximation. The top region renders from a
non-zero start address (scrolled and pel-panned); the bottom region (below the
line-compare row) renders from VRAM offset 0 regardless of the start address, with
pel-pan forced to 0. Proven at unit and end-to-end-through-the-bus levels.

## 1. Scope

**In:**
- Full 10-bit Line Compare, assembled across three CRTC registers: index 18h
  (bits 0-7), the Overflow register 07h bit 4 (bit 8), and the Maximum Scan Line
  register 09h bit 6 (bit 9).
- The split itself in `render_active_row`: below the split the address counter
  reloads to 0 and rows are counted from the first split scanline.
- Pel-pan forced to 0 below the split when Attribute Mode Control (10h) bit 5 is
  set, per the hardware "enable pixel panning: 0 = all, 1 = up to line compare"
  semantics.
- Wiring the three CRTC registers through `write_crtc`, and the per-mode default
  (line compare disabled, 0x3FF).
- The split proof at unit and end-to-end levels, plus the 10-bit register-assembly
  and off-by-one boundary tests.
- Conformance-doc update: move line-compare out of the deferred list, document the
  register assembly, the reset semantics, and the divergences.

**Scope, settled with the owner:** 16-color planar only. Full 10-bit line compare.
Model pel-pan-below-split now (it is part of the done-signal).

**Deferred (unchanged from the conformance doc):** mode-X / unchained 256-color,
Jazz 320x199 exact CRTC timing, the bad-card mid-scanline latch shake,
pixel-granular catch-up.

## 2. What the references pin (do not guess)

The semantics below are taken from the cached references, not recollection:

1. **Line Compare is 10 bits across three registers (VGA).** RBIL `PORTS.B`
   (CRTC table P0708): "bit4: bit8 of line compare (18h)" in the Overflow register
   07h, and "bit6: (VGA) bit9 of line compare (18h)" in the Maximum Scan Line
   register 09h. Abrash's Graphics Programming Black Book chapter 30 confirms the
   same assembly: bits 7-0 in 18h, bit 8 in Overflow 07h bit 4, bit 9 in Maximum
   Scan Line 09h bit 6.

2. **The split starts on the line after the match.** Abrash chapter 30: "the scan
   line that matches the split screen scan line is not part of the split screen;
   the split screen starts on the following scan line." So the bottom region begins
   at scanline `line_compare + 1`; the comparison is `counter_line > line_compare`.

3. **The address counter reloads to 0 at the split.** Abrash chapter 30: the split
   "resets the internal pointer which addresses the next byte of display memory to
   be read for video data to zero."

4. **Pel-pan below the split.** RBIL `PORTS.B` Attribute Mode Control bitfield
   (P0664): bit 5 is "(VGA) enable pixel panning (0 = all, 1 = up to line compare
   register value)." So bit 5 = 1 forces pel-pan to 0 below the split; bit 5 = 0
   pans the whole screen including the bottom region.

5. **The comparison is against the scan-counter line, not the source row.** The VGA
   vertical line counter that Line Compare is compared against is the same counter
   compared against Vertical Total, Vertical Display End, and the retrace registers,
   all in scan-counter units. This core already models exactly that: `beam_line`
   counts 0..vtotal, `vdisp_end` is the doubled value (400 for mode 0Dh), and
   double-scan only divides the source row. So Line Compare is compared against
   `counter_line` and is not divided by the double-scan factor.

The reset off-by-one and the address reset are pinned from Abrash, a primary
reference, the same way slice 3 pinned the byte and word transforms from VGADOC.

## 3. The split model

`render_active_row(counter_line)` gains one branch driven by the live
`line_compare`:

- `below = counter_line > line_compare`.
- Above the split: `start = start_address`, `first_line = 0`, pel-pan is the live
  Attribute pel-pan value.
- Below the split: `start = 0`, `first_line = line_compare + 1`, pel-pan is forced
  to 0 when Attribute Mode Control (10h) bit 5 is set, otherwise the live value.
- `source_row = (counter_line - first_line) / scan_factor()`.
- `row_base = start + source_row * offset * 2`.

The rest of the active-pixel path is unchanged: the counter runs through
`display_offset` (the slice-3 byte/word/doubleword transform and 64 KB wrap), the
4-bit planar index is assembled, and the Attribute palette maps it to a DAC index.

Because the comparison is against `counter_line`, a split in a double-scanned mode
falls at a scan-counter line. The bottom region restarts row counting at the split
(`first_line = line_compare + 1`), so its first displayed scanline reads source row
0 from offset 0 and the address counter advances from there, matching the hardware
pointer reload. The exact preset-row-scan re-alignment at the split (the EGA
"two scan lines lower" behavior) is a divergence, see section 6.

## 4. Register wiring and state

`line_compare` becomes explicit live state on `CrtcTiming`, the same place
`start_address`, `offset`, `mode_control`, and `underline_loc` already live.

- **Storage.** Add `line_compare: u32` to `CrtcTiming`. It holds the assembled
  10-bit value.
- **Per-mode defaults.** Each of the five constructors sets `line_compare: 0x3FF`.
  This is the BIOS value (18h = 0xFF, Overflow bit 4 = 1, Maximum Scan Line bit 6
  = 1) that the vertical counter never reaches in these modes, so the split is
  disabled until a guest programs it. The default keeps every slice-1/2/3 golden
  unaffected: with line compare at 0x3FF the `below` branch is never taken.
- **Guest writes.** `write_crtc` gains three arms that update only the
  line-compare bits:
  - `0x18` sets bits 0-7: `line_compare = (line_compare & !0xFF) | value`.
  - `0x07` carries bit 8: `line_compare = (line_compare & !0x100) | (((value >> 4) & 1) << 8)`.
  - `0x09` carries bit 9: `line_compare = (line_compare & !0x200) | (((value >> 6) & 1) << 9)`.
  The Overflow (07h) and Maximum Scan Line (09h) registers also carry vertical
  timing high bits and the double-scan / max-scan fields. This slice honors only
  their line-compare bits from a guest write; the timing fields stay mode-defaulted,
  the same simplification the codebase already documents as "full timing via
  set_mode." All three arms run after `catch_up()` (every `write_port` catches up
  first), so a mid-frame split change affects only the scanlines the beam has not
  yet crossed.

No new I/O ports: 0x3D4/0x3D5 already route to `write_crtc`. No change to the beam,
catch-up, latch, double-scan, or `display_offset` logic.

## 5. Logical scanout, unchanged otherwise

Border and blank region coloring, the top-justified active field, the start-address
vretrace latch, and the slice-3 addressing transform are all unchanged. The split
affects only the active-row address base and the pel-pan shift. VRAM is still read
at render time, so a planar write is still not a catch-up point.

## 6. Documented divergences (fidelity directive)

These extend the conformance doc's divergence list:

1. **EGA two-lines-lower split is not modeled.** Abrash chapter 30 notes "on the
   EGA, the split screen may display two scan lines lower." This core targets the
   VGA, where the split starts on the line after the match (`line_compare + 1`), so
   the VGA behavior is implemented and the EGA variance is out of scope.
2. **Line Compare bit 9 is assembled but unexercised in scope.** Bit 9 (09h bit 6)
   only matters for splits at scanline 512 or higher. The in-scope modes top out at
   Vertical Display End 480, so no in-scope split reaches bit 9; it is assembled for
   fidelity and flagged the way slice 3 flagged the doubleword bit positions.
3. **Overflow / Maximum Scan Line non-line-compare fields not honored from guest
   writes.** A guest write to 07h or 09h updates only the line-compare bit; the
   vertical timing high bits (07h) and the double-scan / max-scan fields (09h) stay
   mode-defaulted, consistent with the existing "full timing via set_mode"
   simplification.
4. **Byte panning and exact preset-row-scan re-alignment at the split not modeled.**
   CRTC 08h byte panning is not modeled at all, so it is not reset at the split. The
   bottom region restarts row counting at `line_compare + 1`; any sub-double-scan-row
   split phase offset is not modeled. No in-scope workload depends on either.

## 7. Testing and the done-signal

- **Split, unit (the done-signal).** A non-double-scanned mode (12h) for a clean
  equality. Write a per-row pattern into VRAM, set a non-zero scrolled
  `start_address`, a mid-screen `line_compare`, a non-zero pel-pan, and Attribute
  Mode Control 10h bit 5. Render the full frame and assert:
  - top rows (counter_line <= line_compare) equal the scrolled and pel-panned
    reference,
  - bottom rows (counter_line > line_compare) equal the address-0, unpanned
    reference.
  Assert equality of pixels, not merely that the address changed.
- **Off-by-one boundary, unit.** Assert the scanline equal to `line_compare` is the
  last top row and `line_compare + 1` is the first bottom row, pinning the Abrash
  "split starts on the following scan line" semantics.
- **Double-scan comparison point, unit.** Mode 0Dh, a split at a scan-counter line
  above 255 (for example scanline 320, source row 160), proving the comparison is in
  scan-counter units and that the high bit (bit 8) is required and assembled.
- **10-bit register assembly, unit.** Writing 18h / 07h / 09h through the CRTC ports
  assembles the expected 10-bit `line_compare`, and the per-mode default is 0x3FF.
- **pel-pan-below toggle, unit.** With Attribute 10h bit 5 clear, the bottom region
  is pel-panned like the top; with it set, the bottom region pel-pan is 0.
- **Split, end-to-end through the bus.** A mode set, an A0000 datapath fill of a
  recognizable pattern, the start address and line compare and Attribute 10h bit 5
  and pel-pan programmed through the ports, the machine clock advancing two frames
  (start address latches at the next vertical retrace), and the pulled raster
  showing the scrolled and pel-panned top and the address-0 unpanned bottom.
- **Regression.** Every slice-1/2/3 golden stays green: with `line_compare` at the
  default 0x3FF the split branch is never taken, so the address base and pel-pan are
  exactly as before.

## 8. Reference and conformance doc

- The semantics are cited from Abrash's Graphics Programming Black Book chapter 30
  ("Video Est Omnis Divisa") and RBIL `dev_docs/reference/rbil/PORTS.B` (CRTC tables
  P0708, the Mode Control table P0657, and the Attribute Mode Control table P0664).
  Optionally seed a line-compare section into the gitignored local reference note
  `dev_docs/reference/vga/crtc-addressing.md` (slice 3 named that path; it is not on
  disk yet).
- Update `docs/vga-core/README.md`: move line-compare split screens out of the
  deferred list, add a "Slice 4 coverage" section documenting the 10-bit register
  assembly, the `counter_line > line_compare` reset to offset 0, the pel-pan-below
  behavior, the comparison in scan-counter units, and the section 6 divergences.
- **Target citation, honesty note.** The split-screen-with-scroll technique is
  verified against Abrash chapter 30, the primary reference. A specific 16-color
  shipping-title attribution was not verified: the released id EGA sources (Keen
  Dreams `id_vw_ae.asm`, Catacomb 3-D `ID_VW_AE.ASM`) do not program the line-compare
  register, and the common claim that Commander Keen uses split-screen for its status
  bar is not backed by released source. The conformance note cites Abrash and the
  genre (an EGA/VGA 16-color hardware scroller with a locked status panel, the
  companion to the slice-3 scroll) rather than asserting a named game.

## 9. Files touched

- `crates/izarravm-video/src/vga.rs`: the `CrtcTiming.line_compare` field and the
  five constructors, `write_crtc` indices 0x18 / 0x07 / 0x09, the
  `render_active_row` split branch, and the new tests.
- `crates/izarravm-machine/src/lib.rs`: the end-to-end split test, beside the
  existing slice-3 seam and copper-bar machine tests.
- `docs/vga-core/README.md`: conformance update.
- `dev_docs/reference/vga/crtc-addressing.md`: optional local reference seed (not
  committed; `dev_docs/` is gitignored).
