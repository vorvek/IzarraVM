# VGA Line-Compare Split Screen (Slice 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the CRTC Line Compare split screen so the region below the split renders from VRAM offset 0 (with pel-pan forced to 0) independent of the start-address scroll above it, matching hardware: the bottom region equals the offset-0 content.

**Architecture:** `line_compare` becomes a live 10-bit value on `CrtcTiming`, assembled from CRTC 18h / Overflow 07h bit 4 / Maximum Scan Line 09h bit 6, defaulted 0x3FF (disabled) per mode. `render_active_row` gains one branch: when `counter_line > line_compare` the address base resets to 0, rows count from `line_compare + 1`, and pel-pan is forced to 0 when Attribute Mode Control (10h) bit 5 is set. The comparison is in scan-counter units, the same units the beam uses, so it is not divided by the double-scan factor.

**Tech Stack:** Rust, the `izarravm-video` and `izarravm-machine` crates. Tests are inline `#[cfg(test)]` modules. Gates (Windows-first): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --workspace`.

**Reference:** Semantics are pinned to Abrash's Graphics Programming Black Book chapter 30 ("Video Est Omnis Divisa") and RBIL `dev_docs/reference/rbil/PORTS.B` (CRTC table P0708, Mode Control P0657, Attribute Mode Control P0664). The spec is `docs/superpowers/specs/2026-06-17-vga-line-compare-split-design.md`.

**Working directory:** This worktree (`.claude/worktrees/peaceful-engelbart-50d04d`). Use `cargo` from the worktree root.

**Run `cargo fmt` before every commit** so intermediate commits stay formatted.

---

## File Structure

- `crates/izarravm-video/src/vga.rs` (modify) - the `CrtcTiming.line_compare` field + five constructors, `write_crtc` indices 0x18/0x07/0x09, the `render_active_row` split branch, and unit tests.
- `crates/izarravm-machine/src/lib.rs` (modify) - the end-to-end split test.
- `docs/vga-core/README.md` (modify) - conformance contract update.

---

## Task 1: Add line_compare state, per-mode defaults, and CRTC register wiring

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs` (the `CrtcTiming` struct around lines 18-35, the five constructors `text_03h`/`mode_0dh`/`mode_0eh`/`mode_10h`/`mode_12h` lines 40-143, and `write_crtc` lines 631-651)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/izarravm-video/src/vga.rs`:

```rust
    #[test]
    fn line_compare_registers_assemble_ten_bits_and_default_per_mode() {
        let mut vga = Vga::default();
        vga.set_mode_0dh();
        // 16-color planar modes power up with the split disabled (line compare 0x3FF).
        assert_eq!(vga.crtc.line_compare, 0x3FF);
        // Assemble a split at scanline 0x150: low byte via 18h, bit 8 set via the
        // Overflow register 07h bit 4, bit 9 cleared via the Maximum Scan Line 09h bit 6.
        vga.write_port(0x3D4, 0x18);
        vga.write_port(0x3D5, 0x50);
        vga.write_port(0x3D4, 0x07);
        vga.write_port(0x3D5, 0x10); // bit 4 set -> line compare bit 8 = 1
        vga.write_port(0x3D4, 0x09);
        vga.write_port(0x3D5, 0x00); // bit 6 clear -> line compare bit 9 = 0
        assert_eq!(vga.crtc.line_compare, 0x150);
        // Clearing the overflow bit 4 drops line compare bit 8.
        vga.write_port(0x3D4, 0x07);
        vga.write_port(0x3D5, 0x00);
        assert_eq!(vga.crtc.line_compare, 0x050);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p izarravm-video line_compare_registers_assemble -- --nocapture`
Expected: FAIL to compile, `no field line_compare on type CrtcTiming`.

- [ ] **Step 3: Add the field, defaults, and port wiring**

In the `CrtcTiming` struct, after `pub underline_loc: u8, // CRTC index 14h` add:

```rust
    pub line_compare: u32, // assembled 10-bit value: CRTC 18h + 07h.4 + 09h.6
```

In every one of the five constructors (`text_03h`, `mode_0dh`, `mode_0eh`, `mode_10h`, `mode_12h`), add this field to the struct literal after `underline_loc: 0x00,`:

```rust
            line_compare: 0x3FF,
```

In `write_crtc`, add three arms before the `_ => {}` arm (after the existing `0x17` arm):

```rust
            0x18 => self.crtc.line_compare = (self.crtc.line_compare & !0xFF) | u32::from(value),
            0x07 => {
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x100) | (u32::from((value >> 4) & 1) << 8);
            }
            0x09 => {
                self.crtc.line_compare =
                    (self.crtc.line_compare & !0x200) | (u32::from((value >> 6) & 1) << 9);
            }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p izarravm-video line_compare_registers_assemble`
Expected: PASS.

- [ ] **Step 5: Run the full video crate tests (regression)**

Run: `cargo test -p izarravm-video`
Expected: PASS (all existing slice-1/2/3 tests still green; with `line_compare` at the default 0x3FF nothing reads it yet).

- [ ] **Step 6: Format and commit**

```bash
cargo fmt
git add crates/izarravm-video/src/vga.rs
git commit -m "Store the assembled CRTC line compare value as live VGA state"
```

---

## Task 2: The line-compare split branch in render_active_row

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs` (`render_active_row`, lines 345-368)
- Test: same file's `tests` module

- [ ] **Step 1: Write the failing done-signal test**

Add to the `tests` module. This is the slice's done-signal: top scrolled and panned, bottom from offset 0 with pel-pan forced to 0. It uses mode 12h (not double-scanned, scan factor 1) for a clean equality.

```rust
    #[test]
    fn line_compare_split_renders_top_scrolled_and_bottom_from_offset_zero() {
        // A distinct byte per plane-0 offset so each source row is recognizable.
        fn pattern(off: usize) -> u8 {
            ((off as u32).wrapping_mul(7).wrapping_add(1) & 0xFF) as u8
        }
        // Reference renderer: no split (line compare stays 0x3FF), configurable scroll
        // and pel-pan, rendering one row.
        fn reference(s: u32, pan: u8, row: u32) -> Vec<u8> {
            let mut r = Vga::default();
            r.set_mode(0x12);
            r.attr.palette = core::array::from_fn(|i| i as u8);
            for off in 0..VGA_PLANE_SIZE {
                r.vram[off] = pattern(off);
            }
            r.crtc.start_address = s;
            r.attr.pixel_pan = pan;
            r.render_active_row(row)
        }

        let mut vga = Vga::default();
        vga.set_mode(0x12); // 640x480, not double-scanned, offset 40 (byte pitch 80)
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        for off in 0..VGA_PLANE_SIZE {
            vga.vram[off] = pattern(off);
        }
        let start = 0x1000u32;
        let split = 300u32;
        vga.crtc.start_address = start;
        vga.crtc.line_compare = split;
        vga.attr.pixel_pan = 3;
        vga.attr.mode_control = 0x20; // bit 5: pel-pan up to line compare only

        // Top row 200 (<= split): scrolled by `start`, panned by 3.
        assert_eq!(
            vga.render_active_row(200),
            reference(start, 3, 200),
            "top region renders scrolled and pel-panned"
        );
        // First split scanline (split+1): source row 0 from offset 0, pel-pan forced 0.
        assert_eq!(
            vga.render_active_row(split + 1),
            reference(0, 0, 0),
            "first split line renders source row 0 from offset 0 with pel-pan forced to 0"
        );
        // Split region row k: source row k from offset 0, pel-pan forced 0.
        assert_eq!(
            vga.render_active_row(split + 11),
            reference(0, 0, 10),
            "split region row k renders source row k from offset 0"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p izarravm-video line_compare_split_renders_top_scrolled -- --nocapture`
Expected: FAIL: the bottom rows still render from `start_address` (no split branch), so they do not equal the offset-0 reference.

- [ ] **Step 3: Rewrite render_active_row with the split branch**

Replace the body of `render_active_row` (lines 345-368) with:

```rust
    pub fn render_active_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        // Line Compare split (CRTC 18h + 07h.4 + 09h.6). Abrash, Graphics Programming
        // Black Book ch.30: the scanline matching line compare is the last line above
        // the split; the split starts on the following line and reloads the display
        // address counter to 0. The comparison is in scan-counter units, the same units
        // the beam and the other vertical timing registers use, so it is not divided by
        // the double-scan factor.
        let below_split = counter_line > self.crtc.line_compare;
        let (start, first_line) = if below_split {
            (0, self.crtc.line_compare + 1)
        } else {
            (self.crtc.start_address, 0)
        };
        // Below the split, pel-pan is forced to 0 when Attribute Mode Control (10h)
        // bit 5 is set ("enable pixel panning: 0 = all, 1 = up to line compare").
        let pan = if below_split && (self.attr.mode_control & 0x20 != 0) {
            0
        } else {
            (self.attr.pixel_pan & 0x0F) as usize
        };
        let source_row = (counter_line - first_line) / self.scan_factor();
        // The per-scanline counter increment is offset*2 in every addressing mode; the
        // byte/word/doubleword transform lives in display_offset, not the stride.
        let row_base = start + source_row * self.crtc.offset * 2;
        let mut row = vec![0u8; width];
        for (x, slot) in row.iter_mut().enumerate() {
            let px = x + pan;
            let byte = px / 8;
            let bit = 7 - (px % 8);
            let ma = row_base + byte as u32;
            let off = display_offset(self.crtc.mode_control, self.crtc.underline_loc, ma);
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

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p izarravm-video line_compare_split_renders_top_scrolled`
Expected: PASS.

- [ ] **Step 5: Add the off-by-one boundary test**

Add to the `tests` module. Pins Abrash's "the split starts on the following scan line."

```rust
    #[test]
    fn line_compare_split_starts_on_the_line_after_the_match() {
        let split = 100u32;
        let mut vga = Vga::default();
        vga.set_mode(0x12);
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0xFF; // offset 0 marked: index 1 across pixels 0..7
        // Scroll the top region past the marked byte so the top reads cleared VRAM.
        vga.crtc.start_address = 0x4000;
        vga.crtc.line_compare = split;
        // The matching scanline is the last top line: reads start_address (clear) -> 0.
        assert_eq!(
            vga.render_active_row(split)[0],
            0,
            "scanline == line_compare is still the top region"
        );
        // The next scanline is the first split line: reads offset 0 (marked) -> 1.
        assert_eq!(
            vga.render_active_row(split + 1)[0],
            1,
            "scanline line_compare+1 is the first split line, from offset 0"
        );
    }
```

- [ ] **Step 6: Add the double-scan comparison-point test**

Add to the `tests` module. Proves the comparison is against the scan-counter line, not the source row, and exercises a split above the 8-bit range in a doubled mode.

```rust
    #[test]
    fn line_compare_compares_against_the_scan_counter_line_in_a_doubled_mode() {
        let mut vga = Vga::default();
        vga.set_mode_0dh(); // double-scanned: 400 active scanlines, source rows 0..200
        vga.attr.palette = core::array::from_fn(|i| i as u8);
        vga.vram[0] = 0xFF; // offset 0 marked -> index 1
        // Split at scan-counter line 320. The source row counter only reaches ~200, so a
        // split here can only match if the comparison is in scan-counter units.
        let split = 320u32;
        vga.crtc.start_address = 0x4000; // top region reads cleared VRAM
        vga.crtc.line_compare = split;
        assert_eq!(
            vga.render_active_row(320)[0],
            0,
            "scanline 320 == line_compare is the last top line"
        );
        // Scanlines 321 and 322 are the first two split scanlines: the same doubled
        // source row 0, read from offset 0.
        assert_eq!(vga.render_active_row(321)[0], 1, "first split scanline, offset 0");
        assert_eq!(
            vga.render_active_row(322)[0],
            1,
            "second scanline holds the same doubled source row 0"
        );
    }
```

- [ ] **Step 7: Add the pel-pan-below toggle test**

Add to the `tests` module.

```rust
    #[test]
    fn pel_pan_below_split_is_forced_to_zero_only_when_enabled() {
        // Render the first split-region row (offset 0) with a non-uniform byte so a
        // pel-pan shift is visible. `mode_control` carries Attribute index 10h, `pan`
        // the pel-pan value.
        fn render(mode_control: u8, pan: u8) -> Vec<u8> {
            let mut vga = Vga::default();
            vga.set_mode(0x12);
            vga.attr.palette = core::array::from_fn(|i| i as u8);
            vga.vram[0] = 0b0101_0101; // alternating pixels in source row 0
            vga.crtc.line_compare = 100;
            vga.attr.pixel_pan = pan;
            vga.attr.mode_control = mode_control;
            vga.render_active_row(101) // first split line: source row 0, offset 0
        }
        // bit 5 set: pel-pan forced to 0 below the split, so pan 1 equals pan 0.
        assert_eq!(
            render(0x20, 1),
            render(0x20, 0),
            "Attribute 10h bit 5 set forces split-region pel-pan to 0"
        );
        // bit 5 clear: pel-pan applies below the split, so pan 1 differs from pan 0.
        assert_ne!(
            render(0x00, 1),
            render(0x00, 0),
            "Attribute 10h bit 5 clear pans the split region"
        );
    }
```

- [ ] **Step 8: Run the new tests and the full video crate tests (regression)**

Run: `cargo test -p izarravm-video`
Expected: PASS. Every slice-1/2/3 golden stays green: with `line_compare` at the default 0x3FF the split branch is never taken, so the address base and pel-pan are exactly as before.

- [ ] **Step 9: Format and commit**

```bash
cargo fmt
git add crates/izarravm-video/src/vga.rs
git commit -m "Render the line-compare split below the compare row from offset zero"
```

---

## Task 3: End-to-end split through the machine bus

**Files:**
- Modify: `crates/izarravm-machine/src/lib.rs` (add a test beside `copper_bar_split_through_the_machine`)
- Test: same file's `tests` module

- [ ] **Step 1: Write the test**

Add to the `tests` module in `crates/izarravm-machine/src/lib.rs`. This proves the split through the full path: mode set, A0000 datapath fill, CRTC and Attribute ports, beam advanced by the machine clock. The start address latches at the next vertical retrace, so two frames are advanced.

```rust
    #[test]
    fn line_compare_split_through_the_machine() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh(); // double-scanned byte mode
        // A0000 writes fill plane 0 with a full bit mask, write mode 0 (reset default).
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        // Mark the top of VRAM (plane 0 offset 0) with bit 7 only: pixel 0 set, the rest
        // clear. The split region reads this; a non-uniform byte also detects a
        // wrongly-applied pel-pan below the split.
        machine.write_physical_u8(0x000A_0000, 0x80);
        // Identity attribute palette so index 1 -> DAC 1. read_status1 resets the
        // flip-flop to "index"; 16 entries * 2 writes leaves it in "index" mode.
        machine.video_mut().read_status1();
        for i in 0..16u8 {
            machine.video_mut().write_port(0x3C0, i); // index
            machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
        }
        // Lock pel-pan below the split (Attribute Mode Control 10h bit 5) and pan the
        // top by 4. The flip-flop is in "index" mode here.
        machine.video_mut().write_port(0x3C0, 0x10); // attr index 0x10 (mode control)
        machine.video_mut().write_port(0x3C0, 0x20); // bit 5: pel-pan up to line compare
        machine.video_mut().write_port(0x3C0, 0x13); // attr index 0x13 (pixel pan)
        machine.video_mut().write_port(0x3C0, 0x04); // pan 4
        // Program a split at scan-counter line 100. The mode default line compare is
        // 0x3FF, so the overflow (07h) bit 8 and max-scan (09h) bit 9 must be cleared.
        // The 09h write touches only line compare bit 9, not the double-scan bit.
        machine.video_mut().write_port(0x3D4, 0x07);
        machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 8 = 0
        machine.video_mut().write_port(0x3D4, 0x09);
        machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 9 = 0
        machine.video_mut().write_port(0x3D4, 0x18);
        machine.video_mut().write_port(0x3D5, 0x64); // line compare low 8 bits = 100
        // Scroll the top region to a cleared area of VRAM (start address 0x4000),
        // buffered until the next vertical retrace.
        machine.video_mut().write_port(0x3D4, 0x0C);
        machine.video_mut().write_port(0x3D5, 0x40); // start address high
        machine.video_mut().write_port(0x3D4, 0x0D);
        machine.video_mut().write_port(0x3D5, 0x00); // start address low
        // First frame latches the buffered start address; the second renders with it.
        machine.advance_devices(400_000);
        machine.advance_devices(400_000);
        let raster = machine.vga_raster().expect("a frame presented");
        let w = raster.width as usize; // 320
        // A top scanline (50 < 100) reads the scrolled, cleared region: index 0.
        assert_eq!(
            raster.pixels[50 * w],
            0,
            "top region is scrolled to cleared VRAM"
        );
        // The first split scanline (101 = line_compare + 1) reads offset 0 (the marked
        // byte), with pel-pan forced to 0 below the split: pixel 0 is the marked index 1.
        assert_eq!(
            raster.pixels[101 * w],
            1,
            "split region reads offset 0 with pel-pan forced to 0"
        );
    }
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p izarravm-machine line_compare_split_through_the_machine -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Run the full machine crate tests (regression)**

Run: `cargo test -p izarravm-machine`
Expected: PASS.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt
git add crates/izarravm-machine/src/lib.rs
git commit -m "Prove the line-compare split end to end through the bus"
```

---

## Task 4: Update the conformance contract

**Files:**
- Modify: `docs/vga-core/README.md`

- [ ] **Step 1: Remove line-compare from the slice-1 deferred list**

In the "Slice 1 coverage" section, find the deferred paragraph beginning "Deferred to later slices: line-compare split screens (and pel-pan forced to 0 below the split), pel-pan smooth-scroll polish, ..." and delete the "line-compare split screens (and pel-pan forced\nto 0 below the split), " clause so the sentence starts "Deferred to later slices: pel-pan smooth-scroll polish, mode-X / unchained ...".

- [ ] **Step 2: Add the Slice 4 coverage section**

After the "## Slice 3 coverage" section (after its divergence list, before "## Latch rules"), insert:

```markdown
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
```

- [ ] **Step 3: Commit**

```bash
git add docs/vga-core/README.md
git commit -m "Document the line-compare split in the conformance contract"
```

---

## Task 5: Verify all four gates

**Files:** none (verification only).

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
Expected: no output, exit 0. If it reports diffs, run `cargo fmt` and amend the relevant commit.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings, exit 0.

- [ ] **Step 3: Tests**

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 4: Build**

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 5: Confirm**

All four gates green. The slice is complete and ready to merge to local `main`.

---

## Self-review notes

- **Spec coverage:** the 10-bit register assembly + per-mode default (Task 1) covers spec section 4; the split branch, the `counter_line > line_compare` reset to offset 0, the scan-counter-unit comparison, and pel-pan-below (Task 2) cover sections 2, 3, and the section 7 unit done-signal; the end-to-end split (Task 3) covers the section 7 end-to-end done-signal; the conformance update + divergences + honesty note (Task 4) cover sections 6 and 8.
- **Type consistency:** `line_compare: u32` is added in Task 1 and read in Task 2 (`self.crtc.line_compare`, compared against the `u32` `counter_line`). `render_active_row(counter_line: u32) -> Vec<u8>` keeps its signature; `first_line`, `start`, `source_row`, and `row_base` are all `u32`; `pan` is `usize`. The test helper `reference(s: u32, pan: u8, row: u32) -> Vec<u8>` and `pattern(off: usize) -> u8` are self-contained in Task 2 step 1.
- **Out of scope (by decision):** bit 9 is assembled (Task 1) but unexercised in scope; 07h/09h non-line-compare fields are not honored from guest writes; byte panning and exact preset-row-scan re-alignment are not modeled. All documented as divergences in Task 4.
- **Regression safety:** the default `line_compare = 0x3FF` keeps the split branch untaken in every existing mode, so all slice-1/2/3 goldens are unaffected.
