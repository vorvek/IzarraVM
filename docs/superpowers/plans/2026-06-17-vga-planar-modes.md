# VGA Planar Modes (Slice 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generalize the VGA raster engine to the standard 16-color planar modes 0Eh (640x200), 10h (640x350), and 12h (640x480), on a corrected double-scan model, with a guest-driven INT 10h mode-set.

**Architecture:** The render path is already mode-agnostic for 16-color planar; only the `mode_0dh()` data table and `set_mode_0dh()` entry point are 0Dh-specific. Slice 2 first corrects the double-scan model (one raster row per scanline; double-scan divides the source address), then adds three timing tables, a `set_mode(u8)` dispatch, the machine entry point, and minimal INT 10h wiring. The presented `VgaRaster` is self-describing, so the GUI needs no change.

**Tech Stack:** Rust workspace. Inline `#[cfg(test)] mod tests`, run with `cargo test -p <crate>`. Follow the existing `vga.rs` conventions.

**Source of truth:** `docs/superpowers/specs/2026-06-17-vga-planar-modes-design.md`. Section refs (e.g. section 2) point there.

**Worktree:** All work happens in `D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes` on branch `vga-planar-modes`. The Bash working directory does not reliably persist between calls; use absolute paths or `git -C <worktree>` for git, and `cd <worktree> && <cmd>` (single compound command) for cargo.

**House rules:** No `// ponytail:` or any AI-tooling reference in tracked code. Commit messages are capitalized imperative, no `feat:`/`fix:` prefix, humanizer-clean (no em-dashes, no inflated tone, no rule-of-three filler). No AI attribution or Co-Authored-By trailer.

**Verification per task:** Run `cargo test -p izarravm-video` (Tasks 1-2) or `cargo test -p izarravm-machine` (Tasks 3-4); both must pass. Run `cargo fmt -p izarravm-video -p izarravm-machine` to format. Run `cargo clippy -p izarravm-video -p izarravm-machine -- -D warnings` and fix warnings **in code you added**; pre-existing warnings in untouched lines are out of scope (the tree carries known stable-toolchain drift in these two files). The double-scan change in Task 1 must keep every existing test green; run the full `izarravm-video` suite at the end of Task 1.

---

## File Structure

- **Modify** `crates/izarravm-video/src/vga.rs` — double-scan correction (Task 1); three `CrtcTiming` tables, `set_planar_mode`, `set_mode(u8)`, `set_mode_0dh` alias (Task 2). All unit-tested in the same file.
- **Modify** `crates/izarravm-machine/src/lib.rs` — `set_vga_mode(u8)` (Task 3); INT 10h AL=0D/0E/10/12 wiring and the end-to-end golden (Task 4).
- **Modify** `docs/vga-core/README.md` — conformance updates (Task 5).
- No `gui.rs` / `main.rs` change: the raster carries its own width/height.

---

## Task 1: Correct the double-scan model

The foundation (spec section 2). One raster row per scanline (`raster_height = vtotal`), and double-scan divides the source address (`source_row = counter_line / scan_factor`) instead of doubling the output. `vdisp_end` becomes scanline units.

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs`
- Test: in `vga.rs`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `vga.rs`:

```rust
#[test]
fn mode_0dh_raster_height_equals_vtotal_not_doubled() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // One raster row per scanline: vtotal (449), not vtotal * scan_factor.
    assert_eq!(vga.raster_height(), 449);
}

#[test]
fn double_scan_holds_each_source_row_for_two_scanlines() {
    let mut vga = Vga::default();
    vga.set_mode_0dh(); // doubled mode
    // Source row 0 has pixel 0 set in plane 0; source row 1 (byte pitch
    // offset*2 = 40) is clear.
    vga.vram[0] = 0x80;
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    let r0 = vga.render_active_row(0);
    let r1 = vga.render_active_row(1);
    let r2 = vga.render_active_row(2);
    assert_eq!(r0, r1, "scanlines 0 and 1 read the same source row");
    assert_ne!(r0, r2, "scanline 2 reads the next source row");
    assert_eq!(r0[0], 1, "source row 0 pixel 0 is attribute index 1");
    assert_eq!(r2[0], 0, "source row 1 pixel 0 is attribute index 0");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-video vga::tests::mode_0dh_raster_height_equals_vtotal vga::tests::double_scan_holds`
Expected: FAIL. `raster_height` returns 898 (it is `vtotal * scan_factor` today); `render_active_row(1)` reads source row 1, so `r0 != r1`.

- [ ] **Step 3: Apply the correction**

In `vga.rs`, change `raster_height` to drop the doubling:

```rust
    /// Full visible frame height in raster lines. One raster row per scanline, so
    /// this is `vtotal`; double-scan divides the source address (see
    /// `render_active_row`) rather than multiplying the output.
    pub fn raster_height(&self) -> u32 {
        self.crtc.vtotal
    }
```

Change `render_active_row` so the parameter is the scanline (counter line) and the source VRAM row is `counter_line / scan_factor`:

```rust
    /// Assemble one active scanline into `hdisp_end` DAC indices, applying pel-pan
    /// and the attribute palette. `counter_line` is the scanline in scan-counter
    /// units; double-scan maps it to source row `counter_line / scan_factor`, so a
    /// doubled mode holds each VRAM row for two scanlines.
    pub fn render_active_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        let pan = (self.attr.pixel_pan & 0x0F) as usize;
        let mut row = vec![0u8; width];
        let source_row = counter_line / self.scan_factor();
        // byte pitch per plane = 2 * offset register
        let row_base = self.crtc.start_address as usize
            + source_row as usize * self.crtc.offset as usize * 2;
        for (x, slot) in row.iter_mut().enumerate() {
            let px = x + pan;
            let byte = px / 8;
            let bit = 7 - (px % 8);
            let off = (row_base + byte) % VGA_PLANE_SIZE;
            let mut index = 0u8;
            for plane in 0..VGA_PLANES {
                let b = self.vram[plane * VGA_PLANE_SIZE + off];
                index |= ((b >> bit) & 1) << plane;
            }
            *slot = self.attr.palette[index as usize] & 0x3F;
        }
        row
    }
```

Change `render_scanline` to emit exactly one raster row per counter line (drop the `for sub in 0..factor` doubling):

```rust
    /// Render one scanline (counter line) into a single raster row. Active lines
    /// come from the planes; below `vdisp_end` the row is the border or blank
    /// color. `catch_up` and `render_full_frame` both step in counter lines, the
    /// space the beam counts in.
    fn render_scanline(&mut self, counter_line: u32) {
        let width = self.raster_width() as usize;
        let pixels = if counter_line < self.crtc.vdisp_end {
            self.render_active_row(counter_line)
        } else {
            vec![self.region_color(counter_line); width]
        };
        let dst = counter_line as usize * width;
        if dst + width <= self.work.len() {
            self.work[dst..dst + width].copy_from_slice(&pixels);
        }
    }
```

`scan_factor`, `region_color`, `catch_up`, `finalize_frame`, and `render_full_frame` are unchanged: `render_full_frame` already sizes `work` to `raster_width() * raster_height()` and loops `counter_line in 0..vtotal`, which is now correct.

- [ ] **Step 4: Run the new tests, then the whole crate**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-video`
Expected: PASS, including the two new tests and **every** existing `vga::tests::*`. If `short_display_end_top_justifies_with_shortfall_at_bottom` or either copper-bar test fails, re-read it: the checked rows should stay on the correct side of the split under the new geometry (they were written to). Update only a stale comment if needed, never an assertion to mask a real regression.

- [ ] **Step 5: Commit**

```bash
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes add crates/izarravm-video/src/vga.rs
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes commit -m "Correct the VGA double-scan model to one raster row per scanline"
```

---

## Task 2: Add modes 0Eh/10h/12h, set_planar_mode, and set_mode dispatch

Three `CrtcTiming` tables (spec section 3) and a number-driven dispatch (spec section 4).

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs`
- Test: in `vga.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn set_mode_selects_geometry_for_each_planar_number() {
    let mut vga = Vga::default();

    assert!(vga.set_mode(0x0E));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.raster_height(), 449); // doubled 640x200, vtotal 449
    assert_eq!(vga.active_mode(), VideoMode::Planar);

    assert!(vga.set_mode(0x10));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.raster_height(), 449); // 640x350, vtotal 449

    assert!(vga.set_mode(0x12));
    assert_eq!(vga.raster_width(), 640);
    assert_eq!(vga.raster_height(), 525); // 640x480, vtotal 525

    assert!(vga.set_mode(0x0D));
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.raster_height(), 449);

    assert!(!vga.set_mode(0x99)); // unknown number leaves a false result
}

#[test]
fn wide_mode_assembles_four_bit_index_across_the_full_line() {
    let mut vga = Vga::default();
    vga.set_mode(0x12); // 640 wide, not doubled
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    // Column 639 is byte 79, bit 0. Set that bit in all four planes so the
    // assembled index is 0b1111 = 15.
    for plane in 0..VGA_PLANES {
        vga.vram[plane * VGA_PLANE_SIZE + 79] = 0x01;
    }
    let row = vga.render_active_row(0);
    assert_eq!(row.len(), 640);
    assert_eq!(row[639], 15, "column 639 reads bit 0 of all four planes");
    assert_eq!(row[0], 0, "column 0 is clear");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-video vga::tests::set_mode_selects vga::tests::wide_mode_assembles`
Expected: FAIL to compile — `set_mode`, `mode_0eh`, `mode_10h`, `mode_12h` not defined.

- [ ] **Step 3: Add the timing tables and dispatch**

In `impl CrtcTiming`, beside `mode_0dh`, add:

```rust
    /// Mode 0Eh: 640x200x16 planar, 70 Hz, double-scanned, 8-dot chars. Same
    /// vertical timing as 0Dh, wider active, 80-byte line (offset 40).
    pub fn mode_0eh() -> Self {
        Self {
            htotal_chars: 100, char_width: 8, hdisp_end: 640,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 1, double_scan: true,
            start_address: 0, offset: 40,
        }
    }

    /// Mode 10h: 640x350x16 planar, 70 Hz, not double-scanned, 8-dot chars.
    pub fn mode_10h() -> Self {
        Self {
            htotal_chars: 100, char_width: 8, hdisp_end: 640,
            vtotal: 449, vdisp_end: 350, vblank_start: 355, vblank_end: 442,
            vretrace_start: 387, vretrace_end: 389,
            max_scan: 0, double_scan: false,
            start_address: 0, offset: 40,
        }
    }

    /// Mode 12h: 640x480x16 planar, 60 Hz, not double-scanned, 8-dot chars.
    pub fn mode_12h() -> Self {
        Self {
            htotal_chars: 100, char_width: 8, hdisp_end: 640,
            vtotal: 525, vdisp_end: 480, vblank_start: 490, vblank_end: 520,
            vretrace_start: 490, vretrace_end: 492,
            max_scan: 0, double_scan: false,
            start_address: 0, offset: 40,
        }
    }
```

In `impl Vga`, replace the existing `set_mode_0dh` with a shared helper, an alias, and the dispatch:

```rust
    /// Install a planar mode's timing and reset the beam to the top of frame.
    fn set_planar_mode(&mut self, timing: CrtcTiming) {
        self.crtc = timing;
        self.beam = 0;
        self.last_line = 0;
        self.mode = VideoMode::Planar;
        self.presented = None; // drop any stale frame from a prior mode
        self.resize_work();
    }

    /// Switch to mode 0Dh. Kept as an alias so existing callers do not churn.
    pub fn set_mode_0dh(&mut self) {
        self.set_planar_mode(CrtcTiming::mode_0dh());
    }

    /// Select a planar mode by its INT 10h number. Returns false for a number this
    /// slice does not implement, leaving the current mode untouched.
    pub fn set_mode(&mut self, mode: u8) -> bool {
        let timing = match mode {
            0x0D => CrtcTiming::mode_0dh(),
            0x0E => CrtcTiming::mode_0eh(),
            0x10 => CrtcTiming::mode_10h(),
            0x12 => CrtcTiming::mode_12h(),
            _ => return false,
        };
        self.set_planar_mode(timing);
        true
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-video`
Expected: PASS, including the two new tests and all existing ones.

- [ ] **Step 5: Commit**

```bash
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes add crates/izarravm-video/src/vga.rs
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes commit -m "Add planar modes 0Eh, 10h, 12h with a set_mode dispatch"
```

---

## Task 3: Machine entry point set_vga_mode

A number-driven host API beside the existing `set_vga_mode_0dh`, clearing the Margo latch so a VGA mode-set hands the display back (spec section 4).

**Files:**
- Modify: `crates/izarravm-machine/src/lib.rs`
- Test: in that crate's `mod tests`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn set_vga_mode_selects_planar_geometry_per_number() {
    let mut machine = test_machine();

    assert!(machine.set_vga_mode(0x0E));
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 449);

    assert!(machine.set_vga_mode(0x12));
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 525);

    assert!(!machine.set_vga_mode(0x99));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-machine set_vga_mode_selects_planar_geometry`
Expected: FAIL to compile — `set_vga_mode` not defined.

- [ ] **Step 3: Add the entry point**

Beside `set_vga_mode_0dh` in `impl Machine`, add:

```rust
    /// Select a VGA planar mode by its INT 10h number from the host side. Returns
    /// false for an unimplemented number. On success it hands the display back to
    /// the VGA core (clears the Margo latch), like the guest INT 10h path.
    pub fn set_vga_mode(&mut self, mode: u8) -> bool {
        let ok = self.video.set_mode(mode);
        if ok {
            self.margo_active = false;
        }
        ok
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-machine set_vga_mode_selects_planar_geometry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes add crates/izarravm-machine/src/lib.rs
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes commit -m "Add the machine set_vga_mode entry point"
```

---

## Task 4: INT 10h wiring and the end-to-end golden

Wire AL=0D/0E/10/12 into `handle_int10`, and prove a guest mode-set plus a draw presents a correct raster (the slice done-signal, spec section 5).

**Files:**
- Modify: `crates/izarravm-machine/src/lib.rs`
- Test: in that crate's `mod tests`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn int10_sets_mode_12h_then_draws_and_presents_640x480() {
    // mov ax, 0012h; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x12, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine =
        Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 525);

    // Draw attribute index 1 into the first byte of plane 0 (first 8 pixels of
    // the top row) through the A0000 datapath, with an identity palette.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000, 0xFF);
    machine.video_mut().read_status1(); // reset attr flip-flop to index
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, i); // index
        machine.video_mut().write_port(0x3C0, i); // palette[i] = i
    }

    // A 12h frame is 800 * 525 = 420 000 dots; 600 000 clocks (~604 000 dots)
    // completes at least one frame.
    machine.advance_devices(600_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 640);
    assert_eq!(raster.height, 525);
    assert_eq!(raster.pixels[0], 1, "top-left pixel is attribute index 1");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-machine int10_sets_mode_12h`
Expected: FAIL — `handle_int10` ignores AL=12h, so `active_display()` is still `Text` (or no raster presents).

- [ ] **Step 3: Wire the planar mode numbers into handle_int10**

In `handle_int10`, add the planar cases after the existing `0x0013` block and before the VBE `0x4f` check. The `0x0013` case stays first because it routes to the chained mode 13h, not the planar path:

```rust
    fn handle_int10(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        if ax == 0x0013 {
            self.video.set_mode13h();
            self.margo_active = false;
            return;
        }
        // AH=00h, AL = a planar mode number this slice implements.
        if (ax >> 8) == 0x00 && matches!(ax as u8, 0x0D | 0x0E | 0x10 | 0x12) {
            self.video.set_mode(ax as u8);
            self.margo_active = false;
            return;
        }
        if (ax >> 8) == 0x4f {
            self.handle_vbe(ax as u8);
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes && cargo test -p izarravm-machine int10_sets_mode_12h`
Expected: PASS. Then run the full crate: `cargo test -p izarravm-machine` — all green.

- [ ] **Step 5: Commit**

```bash
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes add crates/izarravm-machine/src/lib.rs
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes commit -m "Wire INT 10h planar mode-sets and prove an end-to-end frame"
```

---

## Task 5: Update the conformance doc

Record the new coverage and the corrected double-scan model in the contract (spec section 6).

**Files:**
- Modify: `docs/vga-core/README.md`

- [ ] **Step 1: Add a Slice 2 coverage section**

Immediately after the "## Slice 1 coverage" section (before "## Latch rules"), insert:

```markdown
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
```

- [ ] **Step 2: Remove the new modes from the deferred list**

In the "## Slice 1 coverage" section, the paragraph beginning "Deferred to later
slices:" lists "other planar modes (0Eh/10h/12h)". Delete that clause (and fix the
surrounding commas) so the deferred list no longer claims those modes are pending.
The 256 KB wraparound, line-compare, mode-X, the bad-card shake, pixel-granular
catch-up, and mid-frame Vertical-Display-End tricks remain in the deferred list.

- [ ] **Step 3: Update the raster-layout unit-discipline note**

In the "## Raster layout (CRTC-derived, top-justified)" section, the "**Unit
discipline:**" paragraph says "a counter line emits `scan_factor` (2 for mode 0Dh)
raster rows" and that worked numbers are in doubled space. Replace it with:

```markdown
**Unit discipline:** the beam and the catch-up loop count in scan-counter lines
(0..Vertical Total), and each scanline emits one raster row, so the raster height
is `Vertical Total`. Double-scan divides the *source* address (source row =
counter line / (max_scan + 1)), so a doubled mode holds each VRAM row for two
scanlines; it does not multiply the output. `Vertical Display End` is in scanline
units. Exact per-mode border offsets are validated against real hardware as those
modes land.
```

- [ ] **Step 4: Commit**

```bash
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes add docs/vga-core/README.md
git -C D:\dev\IzarraVM\.claude\worktrees\vga-planar-modes commit -m "Document planar modes and the corrected double-scan model"
```

---

## Self-Review

**Spec coverage:** section 2 (double-scan correction) -> Task 1; section 3 (mode tables) -> Task 2; section 4 (dispatch, machine entry, INT 10h) -> Tasks 2,3,4; section 5 (done-signal goldens: per-mode geometry, double-scan proof, wide render, end-to-end) -> Tasks 1,2,4; section 6 (conformance doc) -> Task 5. The deferred items (256 KB wrap, line-compare, mode-X, shake) are explicitly out of scope and have no task. Covered.

**Placeholder scan:** every code step shows complete code; no "TBD"/"handle edge cases"/"similar to". Verification commands are exact.

**Type consistency:** `set_planar_mode`, `set_mode`, `set_mode_0dh`, `mode_0eh`, `mode_10h`, `mode_12h`, `render_active_row`, `raster_height`, `render_scanline`, `set_vga_mode`, `handle_int10` are named identically across tasks. `render_active_row` takes a counter line (scanline) in both Task 1 and Task 2. `set_mode`/`set_vga_mode` both return `bool`. `raster_height` returns `vtotal` consistently (449 for the 70 Hz family, 525 for 12h) in every assertion.

**Known approximations the executor should expect:** vertical border/blank offsets for the new modes are conventional values; the start-address byte-offset and `offset * 2` pitch are slice-1 approximations carried forward; the 256 KB wrap stays deferred. All recorded in the conformance doc (Task 5).
