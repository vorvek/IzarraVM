# VGA Display-Address Wraparound (Slice 3) Design

Date: 2026-06-17
Status: Approved (brainstormed with the project owner)
Topic: Legacy VGA core, faithful display-address counter and the 256 KB wraparound

## 0. North star

Slice 3 replaces the slice-1 display-address approximations with the real VGA
display-address-counter model, so the address presented to the planes and the
point at which it wraps match the hardware. The done-signal is the seam: wrapped
scanout pixels EQUAL the top-of-VRAM pixels at the wrap, not merely that the
address wrapped. This is what lets a 16-color planar hardware scroller (Commander
Keen, Xenon 2) scroll seamlessly past the end of VRAM instead of tearing.

## 1. Scope

**In:**
- A faithful display-address counter feeding a byte/word/doubleword addressing
  transform (CRTC Mode Control 17h bit 6, Underline Location 14h bit 6, Address
  Wrap Select 17h bit 5), with the per-plane 64 KB counter wrap that IS the
  256 KB display wraparound.
- Wiring CR17 (Mode Control) and CR14 (Underline Location) through `write_crtc`
  and the per-mode register defaults, so the addressing mode is explicit state,
  not an unflagged assumption.
- The seam proof at unit and end-to-end levels, plus pure-function tests pinning
  the word and doubleword transforms.
- Conformance-doc update: move the 256 KB wrap out of deferred, document the
  addressing model and its divergences.

**Scope, settled with the owner:** 16-color planar only. Full byte/word/doubleword
addressing model.

**Deferred (unchanged from the conformance doc):**
- Mode-X / unchained 256-color and its wrap. Zone 66 and any other unchained
  256-color scroller belong to the mode-X slice, not here. This slice does not
  touch the chained mode-13h path.
- Line-compare split screen (CRTC 18h), pel-pan smooth-scroll polish, Jazz
  320x199 exact CRTC timing, the bad-card mid-scanline latch shake,
  pixel-granular catch-up.

**Out of scope by deliberate decision (divergences, see section 6):** CR17 bits 0
and 1 (6845/Hercules row-scan address substitution into address bits 13/14) and
the address-clock dividers (CR17 bit 3 "count by 2", CR14 bit 5 "count by 4").
These are separate from byte/word/doubleword addressing and belong to CGA-compat
and 256-color-count modes; the 16-color planar modes set CR17 = 0xE3, which clears
the substitution bits and selects no division.

## 2. The finding that reshapes the slice

Two facts from the cached register reference (VGADOC `VGAREGS.TXT`, mirrored, and
RBIL `PORTS.B` CR17/CR14/CR13 bit tables) collapse this from a rewrite into a
small, surgical change:

1. **The 16-color planar modes run byte addressing.** The standard VGA BIOS
   programs CR17 = 0xE3 for modes 0Dh/0Eh/10h/12h, and 0xE3 bit 6 = 1 selects
   byte mode. Word mode (CR17 = 0xA3) is the text / mode-13h / power-on default,
   and those paths do not use the planar scanout code.

2. **The per-scanline counter increment is `offset * 2` in all three modes.** The
   Offset register holds bytes-per-line / K, where K = 2/4/8 for byte/word/dword,
   and the counter step is 1/2/4 bytes respectively, so the address-counter
   increment per scanline is always `offset * 2`. The byte/word/dword distinction
   lives entirely in the counter-to-address transform, not in the stride.

Therefore the current inline expression

```
off = (start_address + source_row * offset * 2 + byte_col) % VGA_PLANE_SIZE
```

is already the faithful BYTE-mode address counter, and the modes that scroll past
VRAM all run byte mode. The slice-1 approximations (start address treated as a raw
byte offset, pitch = offset*2) only diverge from hardware in word/doubleword
modes, which the in-scope workloads never enter. The slice's real work is to make
the addressing mode explicit, prove the seam, and implement word/doubleword
correctly for the cases that use them.

## 3. The address-counter model

`render_active_row` computes the 16-bit display-address counter and runs it
through one pure transform:

```
ma  = start_address + source_row * (offset * 2) + byte_col    // address counter
off = display_offset(ma)                                       // transform + 64 KB wrap
```

`byte_col` is the character-clock index along the line (`px / 8`, with pel-pan
already folded into `px`), so the counter advances by one per character clock,
exactly as the hardware counter does.

`display_offset(ma)` dispatches on the live registers:

- **Byte mode** (CR17 bit 6 = 1): identity. `off = ma & 0xFFFF`.
- **Word mode** (CR17 bit 6 = 0 and CR14 bit 6 = 0): the counter is rotated one
  position up, bringing MA15 (CR17 bit 5 = 1) or MA13 (CR17 bit 5 = 0) into bit 0
  (VGADOC: "When in Word Mode bit 15 is rotated to bit 0 if this bit is set else
  bit 13 is rotated into bit 0"; "If clear system is in word mode. Addresses are
  rotated 1 position up").
  `off = ((ma << 1) | ((ma >> wrap_bit) & 1)) & 0xFFFF`, `wrap_bit = 15 or 13`.
- **Doubleword mode** (CR14 bit 6 = 1): the counter is rotated two positions up,
  bringing MA13 and MA12 into bits 1 and 0.
  `off = ((ma << 2) | ((ma >> 12) & 0x3)) & 0xFFFF`.

  The exact doubleword bit positions (MA13 to bit 1, MA12 to bit 0) are
  transcribed from my recollection of the FreeVGA / Matrox CRTC17 table and MUST
  be confirmed against the cached reference during implementation. The fetchable
  VGADOC mirror documents byte and word exactly but not the doubleword rotation;
  the FreeVGA and OSDev mirrors that hold the Matrox tables were unreachable
  during the brainstorm (expired certificate / 403). No in-scope workload
  exercises doubleword, so this is a low-risk validation task, not a blocker.

The `& 0xFFFF` is the 16-bit counter wrapping at 64 KB per plane. One counter
value addresses the same byte offset in all four parallel 64 KB planes, so the
64 KB counter wrap IS the 256 KB display wraparound. `VGA_PLANE_SIZE` is 0x10000,
so masking with `% VGA_PLANE_SIZE` and `& 0xFFFF` are equivalent; the
implementation keeps `% VGA_PLANE_SIZE` for continuity with the existing code and
the named constant.

## 4. Register wiring and state

CR17 (Mode Control) and CR14 (Underline Location) become explicit live state.

- **Storage.** Add `mode_control: u8` and `underline_loc: u8` to `CrtcTiming`,
  beside the `start_address` and `offset` fields that already live there.
  `CrtcTiming` is already the live CRTC register file rather than pure timing
  (start address is latched into it, offset is written into it), so this is
  consistent. `display_offset` reads `self.crtc.mode_control` and
  `self.crtc.underline_loc`.
- **Per-mode defaults.** Each mode constructor sets its canonical value:
  `mode_0dh/0eh/10h/12h` set `mode_control = 0xE3` (byte mode) and
  `underline_loc = 0x00`; `text_03h` sets `mode_control = 0xA3` (word mode, for
  honesty even though the text path does not use the transform). This makes the
  byte-mode default correct after an INT 10h mode-set even if the guest never
  writes CR17 itself.
- **Guest writes.** `write_crtc` currently drops index 0x14 and 0x17 into the
  `_ => {}` arm. Add `0x17 => self.crtc.mode_control = value` and
  `0x14 => self.crtc.underline_loc = value`. Both already run after `catch_up()`
  (every `write_port` call catches up first), so a mid-frame addressing change
  affects only the scanlines the beam has not yet crossed, like every other
  register. A guest programming a tweaked mode by writing CR17/CR14 directly is
  thereby honored.

No new I/O ports: 0x3D4/0x3D5 already route to `write_crtc`. No change to the
beam, catch-up, latch, or double-scan logic.

## 5. Logical scanout, unchanged otherwise

The active-pixel path is otherwise unchanged from slices 1 and 2: assemble the
4-bit planar index from the four planes at `off`, apply the Attribute pel-pan
left shift, map through the 16-entry Attribute palette to a DAC index. VRAM is
read at render time, so a planar write is not a catch-up point. Border and blank
region coloring, double-scan source-row division, and the top-justified active
field are all unchanged.

## 6. Documented divergences (fidelity directive)

These extend the conformance doc's existing divergence list:

1. **Doubleword bit positions pending reference confirmation.** Implemented to the
   recollected FreeVGA/Matrox transform (MA13 to bit 1, MA12 to bit 0); to be
   confirmed against the cached reference. Unexercised by any in-scope workload.
2. **CR17 bits 0/1 row-scan substitution not modeled.** The 6845 (bit 0, address
   bit 13) and Hercules (bit 1, address bit 14) row-scan address substitutions are
   not applied; address bits 13 and 14 are always taken from the counter. The
   16-color planar modes set these bits, disabling substitution, so this matches
   hardware for every in-scope mode. CGA/Hercules-compat modes that clear them are
   out of scope.
3. **Address-clock dividers not modeled.** CR17 bit 3 (count by 2) and CR14 bit 5
   (count by 4) are stored but not applied to the counter rate. The 16-color
   planar modes select no division. Relevant only to 256-color-count and
   doubleword-clocked modes, which are out of scope.
4. **14-bit vs 16-bit wrap realized through the rotation.** In word mode the CR17
   bit 5 selection of MA13 vs MA15 into bit 0 reproduces the CGA 16 KB interleave
   versus the 64 KB linear layout via the rotation itself; the physical plane
   mask stays 64 KB. Byte mode (every in-scope mode) ignores CR17 bit 5.

## 7. Testing and the done-signal

- **Seam, unit (the done-signal).** Mode 0Dh (byte mode). Write a known pattern at
  plane offset 0, set `start_address` near the top of the plane (for example
  0xFFE0) so row 0 crosses the 64 KB wrap, render the row, and assert the pixels
  after the wrap EQUAL the pixels produced from offset 0. Assert equality of
  pixels, not just that the address wrapped.
- **Seam, end-to-end through the bus.** INT 10h AH=00h selects a 16-color planar
  mode, an A0000 datapath fill writes the pattern, the CRTC ports set the start
  address, the machine clock advances the beam across the wrap, and the pulled
  raster shows the seamless seam.
- **Transform unit tests.** Pure-function assertions on `display_offset` for byte
  (identity), word (rotate-1 with MA13 and MA15 wrap-bit selection via CR17 bit 5),
  and doubleword (rotate-2) against the documented transform. The user chose the
  full model, so the word and doubleword paths get correctness checks even though
  no in-scope workload drives them.
- **Register wiring tests.** Writing CR17 / CR14 through ports 0x3D4/0x3D5 updates
  `crtc.mode_control` / `crtc.underline_loc` and runs catch-up first; a mid-frame
  addressing-mode change splits at the beam row.
- **Regression.** The existing slice-1/2 goldens (byte-mode scanout, double-scan,
  pel-pan, mode geometry, copper-bar split) must stay green; the byte-mode
  transform is the identity, so they are unaffected.

## 8. Reference and conformance doc

- Cache the fetchable VGADOC `VGAREGS.TXT` into `dev_docs/reference/vga/`,
  fulfilling the slice-1 spec section 10 item that named that path but never
  populated it. When an unbroken FreeVGA / Matrox mirror is reachable, transcribe
  the doubleword address-generation table there and confirm divergence 1.
- Update `docs/vga-core/README.md`: move "256 KB display-address wraparound" out
  of the deferred list, add a "Slice 3 coverage" section documenting the
  addressing model (byte/word/doubleword transform, the 64 KB counter wrap as the
  256 KB wraparound, CR17/CR14 wiring), retire the two relevant "Slice-1
  implementation approximations" bullets (start address as raw byte offset; offset
  pitch), and record the section 6 divergences.

## 9. Files touched

- `crates/izarravm-video/src/vga.rs`: `CrtcTiming` fields and constructors,
  `write_crtc` indices 0x14/0x17, the `display_offset` helper, `render_active_row`
  rewrite, and the new tests.
- `crates/izarravm-machine/src/lib.rs`: only if the end-to-end seam test lands
  here (it likely belongs beside the existing `planar_mode_presents_a_vga_raster`
  machine tests).
- `docs/vga-core/README.md`: conformance update.
- `dev_docs/reference/vga/VGAREGS.TXT`: cached reference.
