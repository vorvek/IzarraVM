# VGA Planar Modes (Slice 2) Design

Date: 2026-06-17
Status: Draft
Topic: Generalize the VGA raster engine to the standard 16-color planar modes

## 0. Where this sits

Slice 1 built the cycle-coupled raster engine and rendered one planar mode, 0Dh
(320x200x16), pixel-perfectly. Slice 2 proves the engine is not 0Dh-specific by
adding the other three standard 16-color planar modes and correcting one modeling
mistake the single-mode slice left behind.

The render path is already mode-agnostic for 16-color planar: `render_active_row`,
`render_scanline`, `raster_height`, and `render_full_frame` all derive geometry
from `CrtcTiming` and assemble a 4-bit index from the four planes. The only
0Dh-specific pieces are the `CrtcTiming::mode_0dh()` data table and the
`set_mode_0dh()` entry point. So the new modes are mostly data plus entry points,
with one engine correction underneath them.

## 1. Scope

**In, slice 2:**

- Modes **0Eh (640x200x16), 10h (640x350x16), 12h (640x480x16)**, each as a
  `CrtcTiming` table with canonical values.
- A corrected **double-scan model** so doubled and non-doubled modes share one
  consistent raster geometry (section 2). This is the foundation the new modes
  sit on, not an optional cleanup.
- A `set_mode(u8)` dispatch on `Vga` mapping 0x0D/0x0E/0x10/0x12 to their timing,
  with `set_mode_0dh()` kept as an alias.
- Machine `set_vga_mode(u8)` and minimal `INT 10h, AH=00h` wiring for AL =
  0Dh/0Eh/10h/12h, matching the existing AL=13h path.

**Deferred (unchanged from the slice-1 backlog):** 256 KB display-address
wraparound and the faithful byte/word/dword address-counter model, line-compare
split screens, mode-X / unchained 256-color, the bad-card mid-scanline latch
(shake), pixel-granular catch-up, and mid-frame Vertical-Display-End tricks. The
256 KB wrap in particular is a slice-3 candidate because doing it faithfully means
replacing the slice-1 address approximations (start address as a raw byte offset,
pitch as `offset * 2`, per-plane 64 KB wrap), which is its own focused piece.

## 2. The double-scan correction (foundation)

Slice 1 applies double-scan twice. `render_scanline` emits `scan_factor` (2 for a
doubled mode) raster rows per counter line, and `render_active_row(counter_line)`
reads a distinct VRAM row per counter line (`counter_line * offset * 2`). For mode
0Dh that produces 400 active counter lines, each emitted twice, reading 400
distinct VRAM rows: a 320x800-active raster reading 16000 bytes per plane. The
faithful signal is 400 active scanlines showing 200 distinct rows, each row held
for two scanlines, reading 8000 bytes per plane.

The all-`0xFF` copper-bar golden cannot catch this because every row is identical,
and no test pins `raster_height`. So the value is `vtotal * scan_factor` = 898 for
0Dh today, roughly twice what the hardware scans out.

**Faithful model:**

- The beam and catch-up count in scanlines, 0 to `vtotal`. One scanline emits
  exactly one raster row, so `raster_height = vtotal` (drop the `* scan_factor`).
- Double-scan divides the source address, not the output. A scanline reads source
  row `counter_line / scan_factor`, so a doubled mode holds each VRAM row for two
  scanlines.
- `vdisp_end` is in **scanline** units everywhere. Active when
  `counter_line < vdisp_end`.

Result for 0Dh: 320 wide by 449 total, 400 active scanlines, 200 distinct rows,
8000 bytes per plane. The three new modes then sit consistently:

| Mode | Output | Total | Active | Distinct | Doubled | Refresh |
|------|--------|-------|--------|----------|---------|---------|
| 0Dh  | 320x200 | 320x449 | 400 | 200 | yes | 70 Hz |
| 0Eh  | 640x200 | 640x449 | 400 | 200 | yes | 70 Hz |
| 10h  | 640x350 | 640x449 | 350 | 350 | no  | 70 Hz |
| 12h  | 640x480 | 640x525 | 480 | 480 | no  | 60 Hz |

The functions that change are `raster_height`, `render_active_row` (source-row
divide), and `render_scanline` (single emit). `catch_up`, `finalize_frame`,
`render_full_frame`, the beam math, the latch rules, and the planar datapath are
unchanged.

**Slice-1 test fallout (expected, small):** tests that read `vdisp_end` as a
source-row count adopt the scanline convention. `short_display_end_top_justifies`
keeps `vdisp_end = 199` (now meaning 199 active scanlines) and the assertions on
row 0 (active) and the last row (border/blank) still hold; only the doubled-vs-
undoubled comment changes. The copper-bar goldens (unit and end-to-end) keep their
row checks because the checked rows stay on the correct side of the split. The
executor confirms each via a failing test first.

## 3. Mode timing tables

All four planar modes use the 25.175 MHz dot clock, 8-dot characters, and
`htotal_chars = 100` (800 dots per line, 31.469 kHz horizontal). Per-plane byte
pitch is `offset * 2`, so `offset = bytes_per_line / 2`: 20 for the 320-wide mode,
40 for the 640-wide modes (80 bytes per line).

- **0Eh (640x200):** identical vertical timing to 0Dh (`vtotal 449`, `vdisp_end
  400`, doubled), `hdisp_end 640`, `offset 40`.
- **10h (640x350):** `vtotal 449`, `vdisp_end 350`, not doubled, `hdisp_end 640`,
  `offset 40`, 70 Hz.
- **12h (640x480):** `vtotal 525`, `vdisp_end 480`, not doubled, `hdisp_end 640`,
  `offset 40`, 60 Hz.

Following slice 1, the exact vertical border and blank offsets (`vblank_start/end`,
`vretrace_start/end`) are conventional values in the code and are validated against
the cached FreeVGA reference in the conformance doc as each mode lands. The
load-bearing fields for slice 2 are resolution, doubled-or-not, refresh family, and
pitch; the precise border split is a conformance output, not a spec-asserted
constant.

## 4. Mode dispatch and INT 10h

- A private `set_planar_mode(timing: CrtcTiming)` does what `set_mode_0dh` does
  today: install the timing, switch to `VideoMode::Planar`, reset the beam and
  `last_line`, drop any stale presented frame, and resize the work buffer.
- `pub fn set_mode(&mut self, mode: u8) -> bool` maps 0x0D/0x0E/0x10/0x12 to their
  `CrtcTiming` and calls `set_planar_mode`, returning `false` for an unhandled
  number. `set_mode_0dh()` stays as a thin alias so existing callers and tests do
  not churn.
- The machine gains `pub fn set_vga_mode(&mut self, mode: u8)` beside the existing
  `set_vga_mode_0dh`.
- `handle_int10` gains AL = 0x0D/0x0E/0x10/0x12 cases that call
  `self.video.set_mode(al)` and set `margo_active = false`, mirroring the existing
  AL=0x13 path. No screen clear or palette reset beyond the buffer reset that
  `set_planar_mode` already does; full BIOS mode-set semantics are a later slice.

No `gui.rs` or `main.rs` change: the presented `VgaRaster` carries its own width
and height, and the GUI's existing `ActiveDisplay::VgaRaster` arm renders any size.
Aspect correction stays a downstream renderer concern; the per-mode refresh family
is renderer metadata only and is out of scope for slice 2.

## 5. Testing and done-signal

- **Per-mode geometry:** each of 0Eh/10h/12h reports the expected `raster_width`,
  `raster_height == vtotal`, and active-row count `== vdisp_end`.
- **Double-scan proof:** a doubled mode (0Eh) shows one source row on exactly two
  consecutive raster rows; a non-doubled mode (12h) is one-to-one. A regression
  pin on 0Dh asserts `raster_height == 449`, catching the old 2x.
- **Wide active render:** a 640-wide mode assembles correct 4-bit indices across
  the full line from the four planes, catching any width assumption in the
  scanout.
- **End-to-end (the slice done-signal):** set 12h through `handle_int10`, draw
  through the A0000 datapath, run a frame, and assert the presented 640x480 raster
  carries the expected pixels.

## 6. Conformance doc updates

Update `docs/vga-core/README.md` as the slice lands:

- Add a covered-modes table (0Eh/10h/12h: resolution, refresh, doubled, pitch),
  and move these modes out of the deferred list.
- Replace the double-scan description with the corrected model: `raster_height =
  vtotal`, one raster row per scanline, source row `= counter_line /
  (max_scan + 1)`. Relabel the doubled-vs-undoubled note so the new geometry reads
  correctly.
- Note the start-address byte-offset and `offset * 2` pitch approximations are
  unchanged and still carried, with the faithful address-counter model and the
  256 KB wrap remaining deferred.

## 7. Divergences and approximations carried forward

These are inherited from slice 1, unchanged here, and listed so the slice does not
silently look more complete than it is:

1. Start address is a raw byte offset into a plane; per-plane wrap is 64 KB. The
   256 KB display-address wrap and byte/word/dword addressing remain deferred.
2. CRTC Offset stores the raw register value; pitch is `offset * 2`.
3. Vertical border and blank offsets for the new modes are conventional until
   validated against the FreeVGA reference in the conformance doc.
4. Aspect correction is a downstream renderer layer, not modeled here.
