# VGA Display-Address Wraparound (Slice 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the slice-1 display-address approximations with the faithful VGA display-address counter (byte/word/doubleword addressing transform) so the 256 KB wraparound seam matches hardware: wrapped scanout pixels equal the top-of-VRAM pixels.

**Architecture:** A pure `display_offset(mode_control, underline_loc, ma)` function maps the 16-bit display-address counter to a per-plane byte offset, applying the CR17/CR14 byte/word/doubleword transform plus the 64 KB counter wrap. `render_active_row` computes the counter (`start_address + source_row*offset*2 + byte_col`) and runs it through this function. CR17 (Mode Control) and CR14 (Underline Location) become live state on `CrtcTiming`, defaulted per mode and writable through the CRTC ports.

**Tech Stack:** Rust, the `izarravm-video` and `izarravm-machine` crates. Tests are inline `#[cfg(test)]` modules. Gates (Windows-first): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --workspace`.

**Reference:** Bit semantics are from `dev_docs/reference/rbil/PORTS.B` (CR17 table P0657, CR14/CR13 under P0708) and the VGADOC mirror (https://pdos.csail.mit.edu/6.828/2014/readings/hardware/vgadoc/VGAREGS.TXT). The spec is `docs/superpowers/specs/2026-06-17-vga-display-address-wrap-design.md`.

**Working directory:** This worktree (`.claude/worktrees/strange-mclaren-4ab007`). Use `cargo` from the worktree root.

---

## File Structure

- `dev_docs/reference/vga/crtc-addressing.md` (create) - the citable register reference for the addressing transform.
- `crates/izarravm-video/src/vga.rs` (modify) - `CrtcTiming` fields + constructors, `write_crtc`, the `display_offset` function, `render_active_row`, and tests.
- `crates/izarravm-machine/src/lib.rs` (modify) - the end-to-end seam test.
- `docs/vga-core/README.md` (modify) - conformance contract update.

---

## Task 1: Cache the VGA register reference

**Files:**
- Create: `dev_docs/reference/vga/crtc-addressing.md`

No code, no test. This fulfills slice-1 spec section 10 (which named `dev_docs/reference/vga/` but never populated it) and gives the addressing transform a citable source.

- [ ] **Step 1: Create the reference note**

Create `dev_docs/reference/vga/crtc-addressing.md` with exactly this content:

```markdown
# VGA CRTC Display-Address Generation

The display-address counter and its byte/word/doubleword addressing transform.
Bit semantics below are quoted from two cached references:

- RBIL `dev_docs/reference/rbil/PORTS.B`: CR17 table P0657, CR14/CR13 under P0708.
- VGADOC `VGAREGS.TXT`, mirrored at
  https://pdos.csail.mit.edu/6.828/2014/readings/hardware/vgadoc/VGAREGS.TXT

## CRTC Mode Control (index 17h, "CR17")

- bit 7: 0 = CRTC reset and stop, 1 = resume.
- bit 6: 0 = word mode, 1 = byte mode. (VGADOC: "If clear system is in word
  mode. Addresses are rotated 1 position up".)
- bit 5: address wrap select, word mode only. (VGADOC: "When in Word Mode bit 15
  is rotated to bit 0 if this bit is set else bit 13 is rotated into bit 0".)
  RBIL: "0 = 14bit, 1 = 16bit address wrap".
- bit 3: linear address counter clock, 0 = standard, 1 = clock/2 ("count by 2").
- bit 2: horizontal retrace clock divide.
- bit 1: 0 = substitute row-scan bit 1 for address bit 14 (Hercules compat),
  1 = address bit 14 unaltered.
- bit 0: 0 = substitute row-scan bit 0 for address bit 13 (6845/CGA compat),
  1 = address bit 13 unaltered.

The standard VGA BIOS programs CR17 = 0xE3 for the 16-color planar modes
(0Dh/0Eh/10h/12h): byte mode, address bits 13/14 unaltered, no clock division.
Text and mode 13h use CR17 = 0xA3 (word mode).

## Underline Location (index 14h, "CR14")

- bit 6: 0 = word mode, 1 = doubleword mode (see CR17 bit 6).
- bit 5: 0 = standard address-counter clock, 1 = clock/4 ("count by 4").
- bits 4-0: horizontal underline row scan (not modeled here).

## Offset (index 13h)

Logical line width. Bytes per scanline = offset * K, with K = 2 (byte mode),
4 (word mode), 8 (doubleword mode). Because the counter step is 1/2/4 bytes
respectively, the per-scanline counter increment is `offset * 2` in every mode.

## The address-counter transform (MA -> per-plane byte offset)

`MA` is the 16-bit display-address counter. The per-plane byte offset is:

- Byte mode (CR17 bit 6 = 1): identity. `off = MA`.
- Word mode (CR17 bit 6 = 0, CR14 bit 6 = 0): rotate left 1, bringing MA15
  (CR17 bit 5 = 1) or MA13 (CR17 bit 5 = 0) into bit 0.
  `off = (MA << 1) | ((MA >> wrap_bit) & 1)`.
- Doubleword mode (CR14 bit 6 = 1): rotate left 2, bringing MA13 into bit 1 and
  MA12 into bit 0. `off = (MA << 2) | ((MA >> 12) & 3)`.

Result masked to 16 bits: the counter wraps at 64 KB per plane, and one counter
value addresses the same offset in all four parallel 64 KB planes, so the 64 KB
counter wrap is the 256 KB display wraparound.

Precedence: byte mode wins over doubleword; word mode is the fallthrough.

### Pending validation

The doubleword bit positions (MA13 -> bit 1, MA12 -> bit 0) are transcribed from
recollection of the FreeVGA / Matrox CRTC17 table; the fetchable VGADOC mirror
documents byte and word exactly but not the doubleword rotation, and the FreeVGA
and OSDev mirrors that hold the Matrox tables were unreachable (expired
certificate / HTTP 403) when this was written. Confirm against an unbroken mirror
when one is reachable. No 16-color planar workload exercises doubleword mode.
```

- [ ] **Step 2: Commit**

```bash
git add dev_docs/reference/vga/crtc-addressing.md
git commit -m "Cache the VGA CRTC display-address generation reference"
```

---

## Task 2: Wire CR17/CR14 into CrtcTiming, defaults, and the CRTC ports

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs` (the `CrtcTiming` struct around lines 18-33, the five constructors `text_03h`/`mode_0dh`/`mode_0eh`/`mode_10h`/`mode_12h` lines 38-131, and `write_crtc` lines 617-635)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/izarravm-video/src/vga.rs`:

```rust
#[test]
fn crtc_addressing_registers_are_wired_and_default_per_mode() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // 16-color planar modes power up in byte mode (CR17 = 0xE3).
    assert_eq!(vga.crtc.mode_control, 0xE3);
    assert_eq!(vga.crtc.underline_loc, 0x00);
    // A guest write through the CRTC ports updates the live registers.
    vga.write_port(0x3D4, 0x17); // CRTC index 17h
    vga.write_port(0x3D5, 0xA3); // word mode
    assert_eq!(vga.crtc.mode_control, 0xA3);
    vga.write_port(0x3D4, 0x14); // CRTC index 14h
    vga.write_port(0x3D5, 0x40); // doubleword bit
    assert_eq!(vga.crtc.underline_loc, 0x40);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p izarravm-video crtc_addressing_registers_are_wired -- --nocapture`
Expected: FAIL to compile, `no field mode_control on type CrtcTiming`.

- [ ] **Step 3: Add the fields, defaults, and port wiring**

In the `CrtcTiming` struct, after `pub offset: u32,` add:

```rust
    pub mode_control: u8,  // CRTC index 17h
    pub underline_loc: u8, // CRTC index 14h
```

In every one of the five constructors (`text_03h`, `mode_0dh`, `mode_0eh`, `mode_10h`, `mode_12h`), add these two fields to the struct literal after `offset: ...,`. Use `mode_control: 0xE3` for `mode_0dh`/`mode_0eh`/`mode_10h`/`mode_12h` (byte mode) and `mode_control: 0xA3` for `text_03h` (word mode). Use `underline_loc: 0x00` in all five. For example, `mode_0dh` becomes:

```rust
    pub fn mode_0dh() -> Self {
        Self {
            htotal_chars: 100,
            char_width: 8,
            hdisp_end: 320,
            vtotal: 449,
            vdisp_end: 400,
            vblank_start: 407,
            vblank_end: 442,
            vretrace_start: 412,
            vretrace_end: 414,
            max_scan: 1,
            double_scan: true,
            start_address: 0,
            offset: 20,
            mode_control: 0xE3,
            underline_loc: 0x00,
        }
    }
```

In `write_crtc`, add two arms before the `_ => {}` arm:

```rust
            0x14 => self.crtc.underline_loc = value,
            0x17 => self.crtc.mode_control = value,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p izarravm-video crtc_addressing_registers_are_wired`
Expected: PASS.

- [ ] **Step 5: Run the full video crate tests (regression)**

Run: `cargo test -p izarravm-video`
Expected: PASS (all existing slice-1/2 tests still green).

- [ ] **Step 6: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Store CRTC Mode Control and Underline Location as live VGA state"
```

---

## Task 3: The display_offset addressing transform

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs` (add a free function near `read_planes`/`write_planes`, after line ~836)
- Test: same file's `tests` module

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn display_offset_applies_byte_word_dword_transforms() {
    // Byte mode (CR17 bit 6 = 1): identity, wrapped at 64 KB.
    assert_eq!(display_offset(0xE3, 0x00, 0x1234), 0x1234);
    assert_eq!(display_offset(0xE3, 0x00, 0x1_0005), 0x0005); // 64 KB counter wrap
    // Word mode, 16-bit wrap (CR17 = 0xA3: bit 6 = 0, bit 5 = 1): rotate left 1,
    // MA15 into bit 0.
    assert_eq!(display_offset(0xA3, 0x00, 0x4001), 0x8002); // MA15 = 0
    assert_eq!(display_offset(0xA3, 0x00, 0x8000), 0x0001); // MA15 = 1 -> bit 0
    // Word mode, 14-bit wrap (CR17 = 0x83: bit 6 = 0, bit 5 = 0): MA13 into bit 0.
    assert_eq!(display_offset(0x83, 0x00, 0x2000), 0x4001); // MA13 = 1 -> bit 0
    // Doubleword mode (CR14 bit 6 = 1, word base): rotate left 2, MA13 -> bit 1,
    // MA12 -> bit 0.
    assert_eq!(display_offset(0xA3, 0x40, 0x3000), 0xC003);
    // Byte mode wins over the doubleword bit.
    assert_eq!(display_offset(0xE3, 0x40, 0x1234), 0x1234);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p izarravm-video display_offset_applies`
Expected: FAIL to compile, `cannot find function display_offset`.

- [ ] **Step 3: Implement display_offset**

Add this free function to `crates/izarravm-video/src/vga.rs`, just after the `write_planes` function (near line 836, outside the `impl Vga` block):

```rust
/// Map a display-address counter value `ma` to a per-plane byte offset, applying
/// the CRTC byte/word/doubleword addressing transform and the 16-bit (64 KB)
/// counter wrap. `mode_control` is CRTC index 17h, `underline_loc` is index 14h.
/// See `dev_docs/reference/vga/crtc-addressing.md`.
pub fn display_offset(mode_control: u8, underline_loc: u8, ma: u32) -> usize {
    let addr = if mode_control & 0x40 != 0 {
        ma // byte mode (CR17 bit 6): identity
    } else if underline_loc & 0x40 != 0 {
        // doubleword mode (CR14 bit 6): rotate left 2, MA13 -> bit 1, MA12 -> bit 0
        (ma << 2) | ((ma >> 12) & 0x3)
    } else {
        // word mode: rotate left 1, MA15 (CR17 bit 5 = 1) or MA13 (= 0) -> bit 0
        let wrap_bit = if mode_control & 0x20 != 0 { 15 } else { 13 };
        (ma << 1) | ((ma >> wrap_bit) & 1)
    };
    (addr as usize) % VGA_PLANE_SIZE
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p izarravm-video display_offset_applies`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Add the VGA display-address byte/word/doubleword transform"
```

---

## Task 4: Route render_active_row through display_offset, and prove the seam

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs` (`render_active_row`, lines 333-354)
- Test: same file's `tests` module

- [ ] **Step 1: Write the failing test**

Add to the `tests` module. This test exercises word mode through the render path, which the current byte-only code gets wrong:

```rust
#[test]
fn word_mode_render_rotates_the_address() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.crtc.mode_control = 0xA3; // force word mode (bit 6 = 0), 16-bit wrap
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    // The second character (byte_col 1) has counter ma = 1. Word mode maps ma = 1
    // to plane offset 2 ((1 << 1) | 0); byte mode would read offset 1. Mark only
    // offset 2, so a correct word-mode read shows index 1 at pixel 8.
    vga.vram[2] = 0x80; // bit 7 -> the first pixel of that character
    let row = vga.render_active_row(0);
    assert_eq!(row[8], 1, "word mode reads plane offset 2 for the 2nd character");
    assert_eq!(row[0], 0, "char 0 (offset 0) is clear");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p izarravm-video word_mode_render_rotates`
Expected: FAIL: `assertion failed: row[8] == 1` (current code reads offset 1, which is 0).

- [ ] **Step 3: Rewrite render_active_row to use display_offset**

Replace the body of `render_active_row` (lines 333-354) with:

```rust
    pub fn render_active_row(&self, counter_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        let pan = (self.attr.pixel_pan & 0x0F) as usize;
        let mut row = vec![0u8; width];
        let source_row = counter_line / self.scan_factor();
        // Address-counter base for this row. The per-scanline counter increment is
        // offset*2 in every addressing mode; the byte/word/doubleword transform
        // lives in display_offset, not the stride.
        let row_base = self.crtc.start_address + source_row * self.crtc.offset * 2;
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

Run: `cargo test -p izarravm-video word_mode_render_rotates`
Expected: PASS.

- [ ] **Step 5: Add the done-signal seam test**

Add to the `tests` module. This is the slice's done-signal: wrapped pixels equal the top-of-VRAM pixels.

```rust
#[test]
fn byte_mode_wrap_scanout_equals_top_of_vram() {
    let mut vga = Vga::default();
    vga.set_mode_0dh(); // byte mode (CR17 = 0xE3)
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    // Distinct mark at the very top of VRAM (plane 0 offset 0): pixels 0..7 = index 1.
    vga.vram[0] = 0xFF;
    // Reference row from start_address 0: its first 8 pixels come from offset 0.
    vga.crtc.start_address = 0;
    let top = vga.render_active_row(0);
    assert_eq!(&top[0..8], &[1u8; 8], "top-of-VRAM byte renders 8 pixels of index 1");
    // Start 8 bytes before the 64 KB wrap: byte_col 0..7 read 0xFFF8..0xFFFF (clear),
    // byte_col 8 wraps to offset 0 (the marked byte). So pixels 64..71 must equal
    // the top-of-VRAM pixels, not tear.
    vga.crtc.start_address = 0xFFF8;
    let wrapped = vga.render_active_row(0);
    assert_eq!(&wrapped[0..64], &[0u8; 64], "pre-wrap pixels read the cleared tail");
    assert_eq!(
        &wrapped[64..72],
        &top[0..8],
        "wrapped scanout pixels equal the top-of-VRAM pixels at the seam"
    );
}
```

- [ ] **Step 6: Run the seam test and the full video crate tests (regression)**

Run: `cargo test -p izarravm-video`
Expected: PASS. The byte-mode transform is the identity, so every slice-1/2 golden (scanout, double-scan, pel-pan, geometry, copper bar) stays green.

- [ ] **Step 7: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Render the planar scanout through the faithful display-address counter"
```

---

## Task 5: End-to-end seam through the machine bus

**Files:**
- Modify: `crates/izarravm-machine/src/lib.rs` (add a test beside `copper_bar_split_through_the_machine`, around line 2564)
- Test: same file's `tests` module

- [ ] **Step 1: Write the test**

Add to the `tests` module in `crates/izarravm-machine/src/lib.rs`. This proves the seam through the full path: mode set, A0000 datapath fill, start address via the CRTC ports, beam advanced by the machine clock. The start address latches at the next vertical retrace, so two frames are advanced: the first latches the buffered value, the second renders with it.

```rust
#[test]
fn display_address_wrap_seam_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh(); // byte mode
    // Plane 0 datapath: map mask plane 0, full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01);
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF);
    // Mark the top of VRAM: plane 0 offset 0 = 0xFF (pixels 0..7 -> attribute index 1).
    machine.write_physical_u8(0x000A_0000, 0xFF);
    // Identity palette so index 1 -> DAC 1.
    machine.video_mut().read_status1(); // reset attr flip-flop to index
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, i);
        machine.video_mut().write_port(0x3C0, i);
    }
    // Set start_address = 0xFFF8 through the CRTC ports (buffered until vretrace).
    machine.video_mut().write_port(0x3D4, 0x0C); // start address high
    machine.video_mut().write_port(0x3D5, 0xFF);
    machine.video_mut().write_port(0x3D4, 0x0D); // start address low
    machine.video_mut().write_port(0x3D5, 0xF8);
    // First frame latches the buffered start address; the second renders with it.
    machine.advance_devices(400_000);
    machine.advance_devices(400_000);
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize; // 320
    // Row 0: pixels 0..63 read 0xFFF8..0xFFFF (clear), pixels 64..71 wrap to offset 0.
    assert_eq!(raster.pixels[0], 0, "pre-wrap pixel reads the cleared tail");
    assert_eq!(
        raster.pixels[64], 1,
        "wrapped scanout pixel equals the top-of-VRAM pixel (no tear)"
    );
    // Sanity: still on row 0 of the active area.
    assert!(w >= 72);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p izarravm-machine display_address_wrap_seam_through_the_machine`
Expected: PASS. (If the wrapped pixel lands a column off due to pel-pan defaulting non-zero, check `attr.pixel_pan` is 0 after `set_vga_mode_0dh`; it is, since `Attribute::default()` zeroes it.)

- [ ] **Step 3: Run the full machine crate tests (regression)**

Run: `cargo test -p izarravm-machine`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/izarravm-machine/src/lib.rs
git commit -m "Prove the display-address wrap seam end to end through the bus"
```

---

## Task 6: Update the conformance contract

**Files:**
- Modify: `docs/vga-core/README.md`

- [ ] **Step 1: Remove the 256 KB wrap from the slice-1 deferred list**

In the "Slice 1 coverage" section, find the deferred paragraph beginning "Deferred to later slices: the 256 KB display-address wraparound (only the per-plane 64 KB wrap is modeled), line-compare..." and delete the "the 256 KB display-address wraparound (only the per-plane 64 KB wrap is modeled), " clause so the sentence starts "Deferred to later slices: line-compare split screens...".

- [ ] **Step 2: Add the Slice 3 coverage section**

After the "## Slice 2 coverage" section (after its table and the "Corrected double-scan model" paragraph, before "## Latch rules"), insert:

```markdown
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

Scope: 16-color planar modes only. Mode-X / unchained 256-color and its wrap stay
deferred to the mode-X slice. The done-signal is the seam: wrapped scanout pixels
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
```

- [ ] **Step 3: Retire the resolved slice-1 approximations**

In the "## Slice-1 implementation approximations" section, replace the first two bullets (the "Start address" bullet and the "CRTC Offset register" bullet) with a single note:

```markdown
- **Start address and offset pitch** are now handled by the faithful
  display-address counter and the byte/word/doubleword transform (see "Slice 3
  coverage"); these are no longer approximations.
```

Leave the "A0000 aperture" and "Full CRTC vertical timing" bullets unchanged.

- [ ] **Step 4: Commit**

```bash
git add docs/vga-core/README.md
git commit -m "Document the slice-3 display-address model in the conformance contract"
```

---

## Task 7: Verify all four gates

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

- **Spec coverage:** addressing transform (Tasks 3, 4) covers spec section 3; CR17/CR14 wiring + per-mode defaults (Task 2) covers section 4; the 64 KB = 256 KB wrap (Task 3 mask, Task 4 seam) covers section 3; the done-signal unit + e2e tests (Tasks 4, 5) cover section 7; reference cache (Task 1) + conformance update (Task 6) cover section 8; the divergences (Task 6) cover section 6.
- **Type consistency:** `display_offset(mode_control: u8, underline_loc: u8, ma: u32) -> usize` is defined in Task 3 and called identically in Task 4. `CrtcTiming.mode_control` / `.underline_loc` are added in Task 2 and read in Tasks 3/4. `row_base` is `u32` after the rewrite (was `usize`); `ma = row_base + byte as u32`.
- **Out of scope (by decision):** CR17 bits 0/1 and the clock dividers are stored (Task 2 stores the whole CR17/CR14 byte) but not applied; documented as divergences in Task 6.
