# VGA Raster Core — Slice 1 Design

Date: 2026-06-17
Status: Draft (revised after two rounds of adversarial review)
Topic: Legacy VGA compatibility block (Margo's VGA personality), raster engine

## 0. North star: a pixel-perfect raster that matches the real VGA signal

The legacy VGA core exists to run real DOS-era software. A quirk is **done when
the rasterizer's pixel-perfect output matches what the real VGA chip scanned out**
— active video positioned within the visible raster exactly as the CRTC timing
dictates, borders and all.

**Layer boundary (decided):**

- **The VGA rasterizer (this slice) emits a pixel-perfect, square-pixel raster**
  into a machine-owned buffer. Integer, deterministic, golden-testable. It models
  the *signal* geometry — horizontal dot layout, vertical double-scan, and
  active / border / blank placement straight from the CRTC registers. **No aspect
  correction.**
- **4:3 aspect correction, scaling, and any CRT effect happen downstream in the
  Vulkan renderer** — out of scope here. (So "circles must not be ellipses" is a
  renderer concern; the rasterizer's square pixels are *correct* at this layer.)
- **Presentation is pull-based, matching the existing integration.** The host
  already snapshots machine state after each run slice (`render_current_frame` →
  `active_display()` + framebuffer + `palette_argb()`). The rasterizer finalizes a
  completed frame into a machine-owned buffer that the host reads through a new
  `ActiveDisplay::VgaRaster` arm. **No push, no callback** into the host.

Worked example — the acceptance bar (Jazz Jackrabbit's 320×199 tweak):

> Jazz shortens the CRTC **Vertical Display End** so the ~60 Hz / 240-line-class
> raster runs only 199 active lines (double-scanned to 398). The active field is
> **top-justified** after the top border; the **shortfall falls at the bottom**,
> not centered — straight from the CRTC vertical-timing registers, exactly as the
> "derive it from the registers" principle demands. The visible-region color is
> per the blank window (border vs black, §5). Exact line offsets are a
> conformance-doc output validated against real hardware, **not** a spec-asserted
> constant. (Round-1 said 70 Hz — wrong, it's 60 Hz; round-2 said 40/42-centered —
> also wrong, real hardware top-justifies with the gap at the bottom.)

The buffer size and the active-area offsets are **derived from the mode's CRTC +
dot-clock registers**, never hardcoded and never a centering rule — which is
exactly what lets the Jazz quirk render where the real card put it.

## 1. Scope

Full raster engine (chosen over a framebuffer snapshot, because copper bars,
mid-frame CRTC effects, and the Keen pel-pan technique all require the video beam
to be coupled to CPU cycles).

**In, slice 1:**
- The four-plane planar memory model (write modes 0–3, read modes 0–1, latches,
  data-rotate/ALU, map mask, bit mask, set/reset) — implemented faithfully once.
- The beam clock + **catch-up rasterization** engine (§6), with arithmetic
  frame rollover inside `vga.advance` (no separate run-loop clamp).
- Mode **0Dh** (320×200×16 planar) rendered scanline-by-scanline into a
  pixel-perfect, machine-owned raster buffer.
- `3DA` reporting live beam position (display-enable + vertical-retrace).
- The raster-output model (§7) — visible raster (top border + active + bottom
  border) sized and positioned from CRTC timing, double-scan applied.
- **One demonstrated mid-frame effect** — a palette split (copper bar) — as the
  proof the catch-up seam works end to end.

**Deferred (features *on* the engine, not engine rework):** 256 KB
display-address wraparound, Jazz 320×199 CRTC geometry as a conformance case,
line-compare split screens (incl. pel-pan forced to 0 below the split), pel-pan
smooth-scroll polish, mode-X / unchained 256-color, other planar modes
(0Eh/10h/12h), the bad-card mid-scanline latch (shake) reproduction,
pixel-granular catch-up, mid-frame Vertical-Display-End / blank-register tricks.

## 2. Placement: Margo's VGA block, one chip

Not a third chip. Real SVGA parts of VEGA's era integrated the VGA core on the
same die as the accelerator, sharing the frame store and RAMDAC. So the VGA core
is a **block inside Margo**: VEGA stays two engines (Margo 2D, Distira 3D) on one
memory pool.

- `VgaTextMode` (currently in `lib.rs`) grows up into a new module
  `crates/izarravm-video/src/vga.rs`, renamed `Vga`. It absorbs text, mode 13h,
  the DAC, and the CRTC cursor it already owns, and gains the planar/raster core.
- `lib.rs` keeps the shared layer: `Framebuffer`, `Dac`, traits, constants.
- **Palette path (scoped down):** the VGA `Dac` is the *only* palette state and
  ports `3C7/3C8/3C9` are its *only* door — each calls `catch_up()` first. Margo
  has **no** palette path today (its blit colors are raw values into VRAM); the
  host already reads this same `Dac` to colorize the Margo LFB. **No Margo palette
  work in slice 1.** If a Margo palette path is ever added, it must route through
  this `Dac` + `catch_up()` — noted so a future slice doesn't open a bypass.
- VGA planar memory stays its **own 256 KB buffer**, separate from Margo's 4 MB
  VRAM. Documented simplification (no DOS-era planar game maps the same bytes
  through both apertures); revisit only on a real workload. Confirmed: no Margo or
  DMA writer touches the VGA buffer in slice 1.

**Blast radius of the rename (budget it):** `VgaTextMode` → `Vga` touches the
`Machine.video` / `MachineBus.video` field types, `read_port`/`write_port`
(`lib.rs:841/877` — and the new handlers must call `catch_up()` at the top for the
*existing* `3D4/3D5/3C7/3C8/3C9` ports too, not only new ones), `palette_argb`
(`lib.rs:553`), `active_mode`, the `video_*_offset` decode helpers, and adds an
`ActiveDisplay::VgaRaster` variant (`lib.rs:115`) that ripples to the host match
in `main.rs`. The three *memory* routing points are unchanged; the type rename is
wider.

## 3. State the core holds

- **Planar VRAM**: 4 planes × 64 KB = 256 KB.
- **Sequencer** (`3C4/3C5`): Map Mask (idx 2), Memory Mode (idx 4), **Clocking
  Mode (idx 1)** — the 8/9 dot-clock and divide bits are *used* (CRTC Horizontal
  Total is in character clocks; dots = chars × char-width).
- **Graphics Controller** (`3CE/3CF`): Set/Reset (0), Enable Set/Reset (1), Color
  Compare (2), Data Rotate/Function (3), Read Map Select (4), Mode (5), Misc (6),
  Color Don't Care (7), Bit Mask (8).
- **Attribute Controller** (`3C0`, via the flip-flop reset by reading `3DA`):
  16 palette regs, Mode Control (10h), **Overscan/border color (11h)**, Color
  Plane Enable (12h), **Horizontal Pixel Pan (13h)**, Color Select (14h).
- **CRTC** (`3D4/3D5`) — load-bearing for beam position *and* the border/blank
  layout, so *used* in slice 1: Horizontal Total, Horizontal Display End, Vertical
  Total, **Overflow (07)** (high bits), Vertical Display End, **Start/End Vertical
  Blank**, Vertical Retrace Start/End, **Max Scan Line (09)** incl. the
  **double-scan** bit, Start Address Hi/Lo (0C/0D), Offset (13h). Line Compare
  (18h) stored; used when the split slice lands.
- **Misc Output** (`3C2`): dot-clock select (25.175 vs 28.322 MHz), page bits.
- **Beam clock**: an f64 dot-unit accumulator (§6). The `Vga` boots with the
  **default text-mode (03h) CRTC timing**, so `frame_dots` (derived from the CRTC
  totals) is well-defined from instruction 0 — *before* any graphics mode-set.
  `vga.advance` also no-ops while `frame_dots == 0`, as a belt-and-suspenders
  guard against a divide-by-zero on an un-programmed CRTC. (Round-3 blocker: the
  per-instruction advance runs from clock 0 in text mode; an undefined
  `frame_dots` would panic on the first instruction of every boot.)

## 4. The planar datapath (faithful, implemented once)

The four 8-bit latch registers + the write-mode ALU are a small, fully-documented
state machine. Get it right once and every planar trick (mode-X, fast clears, the
Keen scroll/copy path) works for free; a one-mode shortcut would be a bug farm.

**CPU write to `A0000`** runs the real pipeline per write mode:
- **WM0**: host byte → data-rotate → per plane pick set/reset value (where
  Enable-Set/Reset) else rotated data → logical op (copy/AND/OR/XOR) against the
  latch → Bit Mask selects ALU-result vs latch per bit → Map Mask gates planes.
- **WM1**: latches written straight to the planes (block move after a read).
- **WM2**: each plane filled from one bit of the host color nibble, via Bit Mask,
  through the ALU + latch, Map Mask gating.
- **WM3**: rotated host byte AND Bit-Mask → effective mask; color from Set/Reset;
  latch supplies the unmasked bits.

**CPU read from `A0000`** loads all four latch registers from that offset, then
returns: **RM0** the Read-Map-Select plane byte; **RM1** the per-bit color-compare
result (planes gated by Color Don't Care).

The `A0000` handler **decomposes 16/32-bit writes into per-byte latch operations**
— the VGA datapath is byte-wide, so `mov [A0000],ax` is two independent
latch/ALU passes. (Mode 13h keeps its flat fast path.) A bounded self-checking
test covers each write/read mode against its documented byte result and latch
state.

## 5. Logical scanout (per scanline)

Catch-up (§6) renders one scanline at a time into the raster buffer. The buffer
spans the mode's **full visible frame**, each row colored by the CRTC region it
falls in — **active**, **border**, or **blank** — so a shortened raster's
letterbox is present *in the buffer* for the renderer (this is how Jazz's black
bars appear: blank rows are rendered black and included, not dropped). All
boundaries come from the CRTC, computed in **scan-counter (undoubled) units**,
then double-scan is applied.

- **Active pixel**: assemble the 4-bit index from the four planes at
  `start_address + row*offset + col` (with the **256 KB wrap** as a follow-on),
  apply the **Attribute pel-pan** left-margin shift, look up the 16-entry
  Attribute palette (+ Color Select) → DAC index → `Dac` color. VRAM is read **at
  render time** (not via the latches), so a planar VRAM write is **not** a
  catch-up point: lines already rendered keep their pixels (the beam passed them),
  lines below render from final VRAM.
- **Border vs blank color** (the round-2 correction): `[Vertical Display End,
  Start Vertical Blank)` and `[End Vertical Blank, Vertical Total)` render in the
  **Overscan color** (this is the classic border-flash region — Wolf3D/DOOM flash
  it red); `[Start Vertical Blank, End Vertical Blank)` renders **black**. So a
  short raster's letterbox is overscan-then-black per the registers, not
  uniformly black — Jazz's bars read black because that's where its blank window
  lands, not by a "letterbox ⇒ black" rule.

Geometry — visible width/height, active-area offset, double-scan — is **derived
from the CRTC**, never hardcoded.

## 6. Beam clock + catch-up rasterization (the crux)

CPU↔beam coherence is the whole point of the "full raster" choice.

**Beam clock — f64 carry, like every other device.** The core holds an f64
dot-unit accumulator. The machine advances it by elapsed CPU clocks each step,
mirroring `margo_ns` (`lib.rs:582`): `vga_dots += clocks as f64 * dot_hz /
clock_hz; let whole = vga_dots.floor(); vga.advance(whole as u64); vga_dots -=
whole;`. Integer `cycles*dot_hz/clock_hz` is wrong — it truncates ~0.7%/step and
drifts. Beam position (scan-counter units, *before* double-scan collapse):

```
line     = (dots / htotal_dots) mod vtotal_lines   // htotal_dots = htotal_chars * char_width(8|9)
dot_in_l =  dots mod htotal_dots
display_enable = line < vdisp_end && dot_in_l < hdisp_end
vretrace       = vretrace_start <= line < vretrace_end
```

`3DA` (bit 0 display-disabled, bit 3 vertical-retrace) reports the live beam, so
vsync-waits and beam-racing time correctly. Reading `3DA` also resets the
Attribute flip-flop — both in the one handler.

**Catch-up.** One primitive: `catch_up()` renders every scanline from the last
rendered line up to (not including) the beam's current line, using register state
in effect **now**, and runs **at the top of every video-register/DAC write and
`3DA` read** (existing ports included). It is **incremental and idempotent**:
when the beam hasn't crossed a scanline it renders **zero** lines (just the beam
arithmetic). A tight `in al,3DA` poll loop therefore costs O(0) rendering per read
— mandatory; re-rendering from frame top would be O(lines²).

**Frame rollover lives inside `vga.advance` — and nowhere else** (resolving the
round-2 clamp conflict). `vga.advance(dots)`:
1. If `dots` crosses one or more frame boundaries, advance arithmetically:
   `frames = (beam + dots) / frame_dots`. **Finalize only the final frame's
   visible state** into the machine-owned presented buffer (the host samples at
   ~50 Hz wall-clock, so intermediate skipped frames need not be surfaced); set
   `beam = (beam + dots) mod frame_dots`. **O(1), not O(frames)** — a multi-second
   `HLT` does not loop millions of times.
2. Finalizing = catch up the remaining lines of the completing frame, copy the
   completed raster into `presented`, reset `last_rendered_line`.

There is **no run-loop frame clamp**. The existing HLT fast-forward
(`lib.rs:718`) and `next_timer_wake` deadline (`lib.rs:625`) size the step from
the PIT; `vga.advance` absorbs whatever step it's given and rolls over correctly.
This avoids the two clamps fighting over the same step.

**Latch semantics — start-address and pel-pan differ** (the actual Vogons
mechanism):
- **Start Address (CRTC 0C/0D) latches once per frame at the start of vertical
  retrace.** A mid-frame write does not shift the lines below it. catch-up
  **buffers** the write as `pending_start`, and the **vretrace-start line
  crossing inside `vga.advance` snapshots `pending → active_start`**. Ordering
  rule (the round-2 off-by-one): a write whose `catch_up()` lands the beam at
  `line >= vretrace_start` for the current frame is captured into *this* frame's
  snapshot and becomes visible *next* frame — never a two-frame lag. Games write
  start-address in the vsync handler, i.e. during retrace; this rule makes that
  the common, correct case. A golden test pins it: write at `vretrace_start` and
  at `vretrace_start+1` both apply on the next visible frame.
- **Pel Pan (Attribute 13h) is not latched** — applied at the scanline of the
  write (per-scanline); catch-up uses the live value.

Getting the good-card per-frame start-address latch right is the prerequisite for
the later bad-card-shake slice (the shake *is* a card latching at hblank instead).

**Timing honesty.** The beam a `3DA` read returns reflects the cycle counter as of
the *previous* retired instruction (device advance runs after `cpu.cycle`,
`lib.rs:702`) — a fixed, deterministic **one-instruction lag**. Documented as
"deterministic," not "cycle-exact at the instruction boundary."

**Ceiling (deliberate):** catch-up is at **scanline** granularity. Mid-scanline
changes (Amiga-style >256-colors-per-line) are effectively impossible to time on a
386 — pixel-granular catch-up deferred. `ponytail:` scanline catch-up, go
pixel-granular only if a title demands it.

## 7. Raster output (pixel-perfect, pulled by the host)

The rasterizer produces a **pixel-perfect, square-pixel** raster into a
machine-owned buffer; the host reads it post-run via `ActiveDisplay::VgaRaster`.
No aspect correction (that's the renderer, §0).

- **Size is purely CRTC + dot-clock derived** — *not* two hardcoded families.
  Width = `htotal`-derived visible dots × char-width; height = the **full visible
  frame** (active + border + blank-rendered-black) with double-scan applied. The
  70 Hz 200-line family,
  the 70 Hz 350/400-line modes, 720×400 text (9-dot chars), and 60 Hz 480-line
  modes all fall out of the same derivation. The refresh family is **renderer
  metadata only** (so the renderer picks the right 4:3 reference) — it does **not**
  gate buffer size.
- **The visible raster includes the overscan border rows** (so the border-flash
  effect renders) in the Overscan color; vertical blank is black (§5). A normal
  320×200 is therefore 400 active **plus** its small top/bottom border rows, not a
  bare 640×400 — dropping the border would lose DOOM's red flash.
- **Vertical double-scan** (Max Scan Line) is applied after offsets are computed
  in counter space: a 200-line image is 400 raster lines, 199 → 398. Signal-
  faithful, not a stretch. (Worked numbers are in *doubled* space; the CRTC values
  that produce them are *undoubled* — the conformance doc labels both to avoid a
  2× misplacement.)
- **Jazz**: active top-justified, shortfall at the bottom, all from the CRTC
  vertical-timing registers; exact offsets validated against hardware in the
  conformance doc.

Because the output is integer and deterministic, golden tests assert exact pixels.

## 8. Bus + machine integration

- Memory routing points unchanged (§2). `read_port/write_port` gain the VGA ports
  (`3C0/3C1/3C2/3C4/3C5/3CE/3CF/3DA`) beside `3C7/3C8/3C9/3D4/3D5`, and call
  `catch_up()` at the top — for the existing ports too.
- The machine advances the beam each step via the f64-carry path (§6); **rollover
  and frame finalize live inside `vga.advance`**, no run-loop clamp.
- `active_display()` gains `VgaRaster`; the host pulls the **last presented**
  buffer (read-only, never re-rasterized live) when a planar mode is active and
  Margo is inactive (mutual exclusion with the LFB, as today).
- `INT 10h, AH=00h, AL=0Dh` programs the canonical mode-0Dh register set, flips to
  planar, and **resets the beam clock / last-rendered line to a frame boundary**.
- **Coherence audit (slice-1 deliverable):** confirm no path mutates visible state
  or reads beam-dependent state without `catch_up()` — planar VRAM writes are
  intentionally exempt (content sampled at render time); all register/DAC/`3DA`
  paths route through it; no Margo/DMA writer touches the VGA buffer.

## 9. Fidelity decisions / documented divergences

1. **Aspect correction is a downstream renderer layer**, not a divergence — the
   rasterizer is intentionally square-pixel (§0).
2. **Presentation is pull-based** — the rasterizer finalizes into a machine-owned
   buffer the host reads; no push/callback.
3. **Multi-frame skips**: a long `HLT` that spans several frames surfaces only the
   final frame's visible state (intermediate frames not presented). Acceptable —
   the host samples at ~50 Hz.
4. **Start Address latches per frame at vretrace** (good-card); the bad-card
   hblank-latch shake is a later opt-in.
5. **Scanline catch-up granularity** (§6 ceiling).
6. **One-instruction beam lag** — deterministic, not cycle-exact (§6).
7. **No analog signal.** Jazz's "No Signal on an LCD scaler" cannot occur — we
   emit a raster buffer, not VGA sync. Refresh family is per-frame metadata for the
   renderer (YAGNI to do more now).
8. **Separate 256 KB VGA buffer** vs one unified frame store (§2).

## 10. Testing + conformance contract

- **Datapath**: per-write-mode / per-read-mode unit tests vs documented bytes and
  latch state; color-compare; bit-mask; aligned-word per-byte decomposition.
- **Beam/catch-up**: beam position vs accumulated dots; `3DA` bits across a frame;
  catch-up renders exactly `[last_line, current_line)` and zero when the beam
  hasn't moved a line; `vga.advance` finalizes a frame with *no* video access;
  a multi-frame advance presents once (final state) without looping per frame;
  start-address written mid-frame and during vretrace both apply next frame (the
  off-by-one golden); pel-pan applies same frame.
- **The proof (pixel-perfect golden)**: mode 0Dh, draw, change the palette
  mid-frame at a known scanline, assert the presented raster shows the split at
  the exact row. Slice 1's done-signal.
- **Raster layout golden**: a 199-line frame presents top-justified with the
  shortfall at the bottom, active count 398, offsets derived from (and asserted
  against) the CRTC registers — *not* a centered split.
- **Border-flash golden**: a 320×200 frame with a non-black Overscan color shows
  the border rows in that color (black-only would be wrong).
- **256 KB wrap (when that slice lands)**: assert wrapped scanout pixels **equal
  the top-of-VRAM pixels** at the seam, not merely that the address wrapped.
- **Reference**: cache a VGA register reference (FreeVGA-style) in
  `dev_docs/reference/vga/`, beside the 386 manuals.
- **Conformance doc**: write `docs/vga-core/` — the contract, analog to
  `docs/vega/` — enumerating covered registers/modes, every §9 decision, the
  start-address vs pel-pan latch rule, the CRTC-derived raster layout (with Jazz's
  hardware-validated offsets, labeled doubled vs undoubled), and the border/blank
  color boundaries.

## 11. Open items (post round 2)

These are **conformance-validation tasks, not design blockers** — the design is
implementable:

1. **Validate Jazz's exact line offsets** against real hardware or a reference
   emulator and record them in the conformance doc. (The *direction* —
   top-justified, gap at bottom, derived from Vertical Display End — is settled;
   the exact pixel counts are an empirical output.)
2. **Confirm the border/blank window boundaries** per mode (where Overscan color
   ends and black begins) against the FreeVGA register reference once cached.
3. **Run the coherence audit** (§8) as code lands, ticking off every register/DAC
   path through `catch_up()`.
