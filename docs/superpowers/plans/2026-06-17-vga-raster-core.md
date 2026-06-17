# VGA Raster Core (Slice 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the legacy VGA core as a cycle-coupled raster engine inside the video crate, rendering planar mode 0Dh pixel-perfectly with a working mid-frame palette split (copper bar).

**Architecture:** A new `Vga` type in `crates/izarravm-video/src/vga.rs` owns 256 KB of planar VRAM, the VGA register blocks, an f64 dot-clock beam accumulator, and a catch-up rasterizer. The host pulls a machine-owned, pixel-perfect raster buffer; aspect correction is a downstream renderer concern (out of scope). Built bottom-up: pure datapath → beam/catch-up → scanout → machine wiring → goldens.

**Tech Stack:** Rust workspace. Tests are inline `#[cfg(test)] mod tests`, run with `cargo test -p <crate>`. Follow `margo.rs` conventions: pure functions over `&mut [u8]` + param structs, bounded loops, saturating offset math. Commit style matches the repo: capitalized imperative, no `feat:`/`fix:` prefix.

**Source of truth:** `docs/superpowers/specs/2026-06-17-vga-raster-core-design.md`. Section refs below (e.g. §4) point there.

**House rule:** No `// ponytail:` or any AI-tooling reference in tracked code.

**Note on line numbers:** Reference symbols/functions, not line numbers — the tree drifts. Verify the symbol exists before editing.

---

## File Structure

- **Create** `crates/izarravm-video/src/vga.rs` — the whole VGA core: planar VRAM, register structs, planar datapath, beam clock, catch-up, scanout, mode-set, ports. (Mirrors `margo.rs` as a single focused module.)
- **Modify** `crates/izarravm-video/src/lib.rs` — `pub mod vga;`, re-export `Vga`; keep `Framebuffer`, `Dac`, constants. `VgaTextMode` is renamed to `Vga` and relocated (Task 13).
- **Modify** `crates/izarravm-machine/src/lib.rs` — field type `video: Vga`, beam advance in `advance_devices`, `ActiveDisplay::VgaRaster`, port/route wiring already present.
- **Modify** `crates/izarravm/src/main.rs` — `render_current_frame` match arm for `VgaRaster`.

Tasks 1–12 live entirely in `vga.rs` (pure, no machine deps) so they unit-test in isolation. Tasks 13–17 wire into the machine and add integration goldens.

---

## Task 1: Module scaffold + default boot state

**Files:**
- Create: `crates/izarravm-video/src/vga.rs`
- Modify: `crates/izarravm-video/src/lib.rs` (add `pub mod vga;` and `pub use vga::Vga;`)
- Test: in `vga.rs`

- [ ] **Step 1: Write the failing test**

```rust
// at the bottom of vga.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boots_with_defined_frame_dots_and_zeroed_vram() {
        let vga = Vga::default();
        assert_eq!(vga.vram.len(), VGA_PLANAR_SIZE);
        assert!(vga.vram.iter().all(|&b| b == 0));
        // frame_dots must be non-zero at boot (default text timing) so the
        // per-instruction beam advance never divides by zero. (Spec §3/§6.)
        assert!(vga.frame_dots() > 0, "frame_dots must be defined before any mode-set");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::boots_with_defined_frame_dots`
Expected: FAIL to compile — `Vga`, `VGA_PLANAR_SIZE`, `frame_dots` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
//! The legacy VGA core: 256 KB planar VRAM, the VGA register blocks, a
//! cycle-coupled beam clock, and a catch-up rasterizer. This is Margo's
//! VGA-compatibility personality (one chip, one frame store, one RAMDAC).

pub const VGA_PLANE_SIZE: usize = 64 * 1024;
pub const VGA_PLANES: usize = 4;
pub const VGA_PLANAR_SIZE: usize = VGA_PLANE_SIZE * VGA_PLANES; // 256 KB

/// CRTC vertical/horizontal timing, in scan-counter (undoubled) units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrtcTiming {
    pub htotal_chars: u32,
    pub char_width: u32, // 8 or 9, from Sequencer Clocking Mode
    pub hdisp_end: u32,  // dots
    pub vtotal: u32,
    pub vdisp_end: u32,
    pub vblank_start: u32,
    pub vblank_end: u32,
    pub vretrace_start: u32,
    pub vretrace_end: u32,
    pub max_scan: u32,     // Max Scan Line low bits; double_scan adds the doubling
    pub double_scan: bool,
    pub start_address: u32,
    pub offset: u32,       // logical line width in addressing units
}

impl CrtcTiming {
    /// Standard 80x25 text (mode 03h): 70 Hz, 9-dot chars. Used as the boot
    /// default so the beam math is valid before any graphics mode-set.
    pub fn text_03h() -> Self {
        Self {
            htotal_chars: 100, char_width: 9, hdisp_end: 720,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 15, double_scan: false,
            start_address: 0, offset: 80,
        }
    }

    /// Mode 0Dh: 320x200x16 planar, 70 Hz, double-scanned, 8-dot chars.
    pub fn mode_0dh() -> Self {
        Self {
            htotal_chars: 100, char_width: 8, hdisp_end: 320,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 1, double_scan: true,
            start_address: 0, offset: 40,
        }
    }

    /// Total dots per frame = htotal_dots * vtotal (scan-counter lines).
    pub fn frame_dots(&self) -> u64 {
        (self.htotal_chars * self.char_width) as u64 * self.vtotal as u64
    }
}

#[derive(Debug, Clone)]
pub struct Vga {
    pub(crate) vram: Vec<u8>,
    pub(crate) crtc: CrtcTiming,
    // remaining register/beam state added in later tasks
}

impl Default for Vga {
    fn default() -> Self {
        Self {
            vram: vec![0; VGA_PLANAR_SIZE],
            crtc: CrtcTiming::text_03h(),
        }
    }
}

impl Vga {
    pub fn frame_dots(&self) -> u64 {
        self.crtc.frame_dots()
    }
}
```

Add to `lib.rs` (near the existing `pub mod margo;`):

```rust
pub mod vga;
pub use vga::{Vga, VGA_PLANAR_SIZE};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::boots_with_defined_frame_dots`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs crates/izarravm-video/src/lib.rs
git commit -m "Scaffold the VGA core module with default boot timing"
```

---

## Task 2: Graphics Controller state + planar write modes 0–3

The byte-wide write datapath (§4). Pure function over the four planes.

**Files:**
- Modify: `crates/izarravm-video/src/vga.rs`
- Test: in `vga.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn write_mode_0_applies_rotate_setreset_logic_and_bitmask() {
    // Latches preloaded to 0xFF on all planes; write 0x0F with bit mask 0xF0,
    // copy logic, no set/reset. Result per plane = (data & mask) | (latch & !mask)
    // = (0x0F & 0xF0) | (0xFF & 0x0F) = 0x00 | 0x0F = 0x0F.
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let mut gc = GfxController::default();
    gc.bit_mask = 0xF0;
    let latches = [0xFFu8; VGA_PLANES];
    write_planes(&mut planes, 0, 0x0F, &gc, &latches);
    for p in &planes {
        assert_eq!(p[0], 0x0F);
    }
}

#[test]
fn write_mode_0_set_reset_substitutes_color_per_plane() {
    // Enable set/reset on all planes, set/reset value = 0b1010 (planes 1 and 3).
    // With full bit mask and copy, each enabled plane writes its set/reset bit
    // expanded to 0xFF or 0x00.
    let mut planes = [[0u8; 1]; VGA_PLANES];
    let mut gc = GfxController::default();
    gc.bit_mask = 0xFF;
    gc.enable_set_reset = 0x0F;
    gc.set_reset = 0b1010;
    let latches = [0u8; VGA_PLANES];
    write_planes(&mut planes, 0, 0x00, &gc, &latches);
    assert_eq!(planes[0][0], 0x00);
    assert_eq!(planes[1][0], 0xFF);
    assert_eq!(planes[2][0], 0x00);
    assert_eq!(planes[3][0], 0xFF);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::write_mode_0`
Expected: FAIL to compile — `GfxController`, `write_planes` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct GfxController {
    pub set_reset: u8,        // idx 0, low 4 bits
    pub enable_set_reset: u8, // idx 1, low 4 bits
    pub color_compare: u8,    // idx 2
    pub rotate: u8,           // idx 3 bits 0..2
    pub logic: u8,            // idx 3 bits 3..4: 0 copy,1 AND,2 OR,3 XOR
    pub read_map: u8,         // idx 4
    pub write_mode: u8,       // idx 5 bits 0..1
    pub read_mode: u8,        // idx 5 bit 3
    pub color_dont_care: u8,  // idx 7
    pub bit_mask: u8,         // idx 8
}

fn apply_logic(logic: u8, value: u8, latch: u8) -> u8 {
    match logic {
        1 => value & latch,
        2 => value | latch,
        3 => value ^ latch,
        _ => value,
    }
}

/// Write one byte through the VGA write datapath into all four planes at
/// `offset`. `planes[i]` is plane i's slice; `latches` are the four latch
/// registers (loaded by a prior read). Spec §4.
pub fn write_planes(
    planes: &mut [[u8; 1]; VGA_PLANES],
    offset: usize,
    data: u8,
    gc: &GfxController,
    latches: &[u8; VGA_PLANES],
) {
    let _ = offset; // single-byte helper; callers index the plane slices
    let rotated = data.rotate_right(u32::from(gc.rotate & 7));
    for plane in 0..VGA_PLANES {
        let latch = latches[plane];
        let value = match gc.write_mode {
            1 => {
                // WM1: latches straight to planes.
                planes[plane][0] = latch;
                continue;
            }
            2 => {
                // WM2: plane filled from one bit of the host color nibble.
                if (data >> plane) & 1 != 0 { 0xFF } else { 0x00 }
            }
            3 => {
                // WM3: set/reset is the color; rotated data ANDs the bit mask.
                if (gc.set_reset >> plane) & 1 != 0 { 0xFF } else { 0x00 }
            }
            _ => {
                // WM0: set/reset substitution where enabled, else rotated data.
                if (gc.enable_set_reset >> plane) & 1 != 0 {
                    if (gc.set_reset >> plane) & 1 != 0 { 0xFF } else { 0x00 }
                } else {
                    rotated
                }
            }
        };
        let mask = if gc.write_mode == 3 {
            gc.bit_mask & rotated
        } else {
            gc.bit_mask
        };
        let alu = apply_logic(gc.logic, value, latch);
        planes[plane][0] = (alu & mask) | (latch & !mask);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::write_mode_0`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Implement VGA planar write modes 0-3"
```

---

## Task 3: Read modes 0/1 + latch load

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn read_mode_0_returns_selected_plane_and_loads_latches() {
    let planes = [[0x11u8; 1], [0x22u8; 1], [0x33u8; 1], [0x44u8; 1]];
    let mut gc = GfxController::default();
    gc.read_map = 2;
    let mut latches = [0u8; VGA_PLANES];
    let byte = read_planes(&planes, &gc, &mut latches);
    assert_eq!(byte, 0x33);
    assert_eq!(latches, [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn read_mode_1_color_compares_each_bit() {
    // Plane bytes form per-bit nibbles; color_compare matches bit positions
    // where all (dont-care-masked) planes equal color_compare's bits.
    let planes = [[0xFFu8; 1], [0x00u8; 1], [0xFFu8; 1], [0x00u8; 1]];
    let mut gc = GfxController::default();
    gc.read_mode = 1;
    gc.color_dont_care = 0x0F; // care about all four planes
    gc.color_compare = 0b0101; // planes 0 and 2 set, 1 and 3 clear
    let mut latches = [0u8; VGA_PLANES];
    let byte = read_planes(&planes, &gc, &mut latches);
    assert_eq!(byte, 0xFF); // every bit position matches the pattern
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::read_mode`
Expected: FAIL — `read_planes` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Read one byte through the VGA read datapath, loading the four latches.
/// Spec §4.
pub fn read_planes(
    planes: &[[u8; 1]; VGA_PLANES],
    gc: &GfxController,
    latches: &mut [u8; VGA_PLANES],
) -> u8 {
    for plane in 0..VGA_PLANES {
        latches[plane] = planes[plane][0];
    }
    if gc.read_mode == 0 {
        return planes[(gc.read_map & 3) as usize][0];
    }
    // Read mode 1: per bit, set the result bit where every cared-about plane
    // matches the corresponding color_compare bit.
    let mut result = 0u8;
    for bit in 0..8 {
        let mut matches = true;
        for plane in 0..VGA_PLANES {
            if (gc.color_dont_care >> plane) & 1 == 0 {
                continue;
            }
            let plane_bit = (planes[plane][0] >> bit) & 1;
            let cmp_bit = (gc.color_compare >> plane) & 1;
            if plane_bit != cmp_bit {
                matches = false;
                break;
            }
        }
        if matches {
            result |= 1 << bit;
        }
    }
    result
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::read_mode`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Implement VGA planar read modes 0-1 with latch load"
```

---

## Task 4: A0000 CPU access on the Vga (plane addressing + map mask + word→byte)

Wire the pure datapath to VRAM through `Vga`, honoring the Sequencer Map Mask and the per-byte decomposition of aligned word writes (§4).

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cpu_write_then_read_round_trips_through_latches() {
    let mut vga = Vga::default();
    vga.seq.map_mask = 0x0F; // all planes enabled
    vga.gc.write_mode = 0;
    vga.gc.bit_mask = 0xFF;
    vga.cpu_write(0x10, 0xA5);
    // A read loads latches; read map 0 returns plane 0's byte.
    vga.gc.read_map = 0;
    assert_eq!(vga.cpu_read(0x10), 0xA5);
    assert_eq!(vga.latches, [0xA5; VGA_PLANES]);
}

#[test]
fn map_mask_gates_which_planes_are_written() {
    let mut vga = Vga::default();
    vga.seq.map_mask = 0b0001; // only plane 0
    vga.gc.bit_mask = 0xFF;
    vga.cpu_write(0, 0xFF);
    assert_eq!(vga.plane_byte(0, 0), 0xFF);
    assert_eq!(vga.plane_byte(1, 0), 0x00);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::cpu_write`
Expected: FAIL — `seq`, `gc`, `latches`, `cpu_write`, `cpu_read`, `plane_byte` not defined.

- [ ] **Step 3: Write minimal implementation**

Extend the `Vga` struct and add the methods:

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct Sequencer {
    pub map_mask: u8,    // idx 2, low 4 bits
    pub memory_mode: u8, // idx 4
}

// add fields to Vga:
//   pub(crate) seq: Sequencer,
//   pub(crate) gc: GfxController,
//   pub(crate) latches: [u8; VGA_PLANES],
// and initialize them in Default (Sequencer::default(), GfxController::default(),
// [0; VGA_PLANES]).

impl Vga {
    pub fn plane_byte(&self, plane: usize, offset: usize) -> u8 {
        self.vram[plane * VGA_PLANE_SIZE + offset]
    }

    fn plane_slice_mut(&mut self, offset: usize) -> [[u8; 1]; VGA_PLANES] {
        // Snapshot the four plane bytes at `offset` into a working set.
        let mut planes = [[0u8; 1]; VGA_PLANES];
        for plane in 0..VGA_PLANES {
            planes[plane][0] = self.vram[plane * VGA_PLANE_SIZE + offset];
        }
        planes
    }

    fn store_planes(&mut self, offset: usize, planes: &[[u8; 1]; VGA_PLANES]) {
        for plane in 0..VGA_PLANES {
            if (self.seq.map_mask >> plane) & 1 != 0 {
                self.vram[plane * VGA_PLANE_SIZE + offset] = planes[plane][0];
            }
        }
    }

    pub fn cpu_write(&mut self, offset: usize, data: u8) {
        if offset >= VGA_PLANE_SIZE {
            return;
        }
        let mut planes = self.plane_slice_mut(offset);
        write_planes(&mut planes, 0, data, &self.gc, &self.latches);
        self.store_planes(offset, &planes);
    }

    pub fn cpu_read(&mut self, offset: usize) -> u8 {
        if offset >= VGA_PLANE_SIZE {
            return 0xFF;
        }
        let planes = self.plane_slice_mut(offset);
        read_planes(&planes, &self.gc, &mut self.latches)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::cpu_write`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Route A0000 CPU access through the planar datapath and map mask"
```

---

## Task 5: Beam clock — position math + f64 advance + frame rollover

The cycle-coupled beam (§6). Position is a pure function; `advance` carries f64 and rolls over arithmetically.

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn beam_position_tracks_dots_in_scan_counter_units() {
    let t = CrtcTiming::mode_0dh();
    let htotal = (t.htotal_chars * t.char_width) as u64; // 800
    // 5 full lines + 10 dots in.
    let dots = htotal * 5 + 10;
    assert_eq!(beam_line(&t, dots), 5);
    assert_eq!(beam_dot(&t, dots), 10);
    assert!(beam_display_enable(&t, dots)); // line 5 < 400, dot 10 < 320
    assert!(!beam_vretrace(&t, dots));      // 5 < vretrace_start 412
}

#[test]
fn advance_rolls_over_one_frame_in_o1() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    let frame = vga.frame_dots();
    // Advance just past two frames in a single call (e.g. a long HLT).
    vga.advance(frame * 2 + 7);
    assert_eq!(vga.beam_dots(), 7); // (2*frame+7) mod frame
    assert_eq!(vga.frames_completed(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::beam_position` and `::advance_rolls_over`
Expected: FAIL — beam functions, `set_mode_0dh`, `advance`, `beam_dots`, `frames_completed` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
pub fn htotal_dots(t: &CrtcTiming) -> u64 {
    (t.htotal_chars * t.char_width) as u64
}
pub fn beam_line(t: &CrtcTiming, dots: u64) -> u32 {
    ((dots / htotal_dots(t)) % t.vtotal as u64) as u32
}
pub fn beam_dot(t: &CrtcTiming, dots: u64) -> u32 {
    (dots % htotal_dots(t)) as u32
}
pub fn beam_display_enable(t: &CrtcTiming, dots: u64) -> bool {
    beam_line(t, dots) < t.vdisp_end && beam_dot(t, dots) < t.hdisp_end
}
pub fn beam_vretrace(t: &CrtcTiming, dots: u64) -> bool {
    let line = beam_line(t, dots);
    line >= t.vretrace_start && line < t.vretrace_end
}

// Add to Vga: pub(crate) beam: u64, pub(crate) dot_carry: f64,
// pub(crate) last_line: u32, pub(crate) frames: u64. Init to 0 in Default.

impl Vga {
    pub fn beam_dots(&self) -> u64 { self.beam }
    pub fn frames_completed(&self) -> u64 { self.frames }

    pub fn set_mode_0dh(&mut self) {
        self.crtc = CrtcTiming::mode_0dh();
        self.beam = 0;
        self.dot_carry = 0.0;
        self.last_line = 0;
    }

    /// Advance the beam by whole dots. Rolls over each completed frame
    /// arithmetically (O(1)); finalize hook lands in Task 7.
    pub fn advance(&mut self, dots: u64) {
        let frame = self.frame_dots();
        if frame == 0 {
            return; // guard: un-programmed CRTC (spec §3/§6)
        }
        let total = self.beam + dots;
        self.frames += total / frame;
        self.beam = total % frame;
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::beam_position` and `::advance_rolls_over`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Add the cycle-coupled beam clock with arithmetic frame rollover"
```

---

## Task 6: Attribute Controller + Status Register 1 (3DA) + flip-flop

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn status1_reports_beam_and_resets_attribute_flipflop() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Park the beam in vertical retrace.
    let htotal = htotal_dots(&vga.crtc);
    vga.beam = htotal * (vga.crtc.vretrace_start as u64);
    let status = vga.read_status1();
    assert_eq!(status & 0x08, 0x08); // bit 3 vertical retrace
    assert_eq!(status & 0x01, 0x01); // bit 0 display disabled (in retrace)
    // Reading 3DA resets the attribute address/data flip-flop to "address".
    assert!(!vga.attr.flip_flop_data);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::status1_reports_beam`
Expected: FAIL — `attr`, `read_status1` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct Attribute {
    pub palette: [u8; 16], // idx 0..15
    pub mode_control: u8,  // idx 0x10
    pub overscan: u8,      // idx 0x11
    pub plane_enable: u8,  // idx 0x12
    pub pixel_pan: u8,     // idx 0x13, low 4 bits
    pub color_select: u8,  // idx 0x14
    pub flip_flop_data: bool, // false = next 3C0 write is an index
    pub index: u8,
}

// Add to Vga: pub(crate) attr: Attribute. Init Attribute::default() in Default.

impl Vga {
    pub fn read_status1(&mut self) -> u8 {
        self.attr.flip_flop_data = false; // reading 3DA resets the flip-flop
        let mut status = 0u8;
        if !beam_display_enable(&self.crtc, self.beam) {
            status |= 0x01; // display disabled (blank or retrace)
        }
        if beam_vretrace(&self.crtc, self.beam) {
            status |= 0x08; // vertical retrace
        }
        status
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::status1_reports_beam`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Add the Attribute Controller and 3DA status with flip-flop reset"
```

---

## Task 7: Catch-up rasterizer + frame finalize

`catch_up()` renders `[last_line, current_line)` into a working raster; finalize copies it to the presented buffer (§5, §6). Scanout row content is stubbed here (solid border color) and filled in Task 8 — this task proves the *line-stepping*, incrementality, and finalize.

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn catch_up_is_incremental_and_zero_when_beam_has_not_moved_a_line() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * 3 + 5); // beam at line 3
    let drawn = vga.catch_up();
    assert_eq!(drawn, 3); // lines 0,1,2 rendered
    let drawn_again = vga.catch_up();
    assert_eq!(drawn_again, 0); // no line crossed since
}

#[test]
fn advance_past_a_frame_finalizes_a_presented_buffer() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    assert!(vga.take_presented().is_none());
    vga.advance(vga.frame_dots() + 10); // cross one frame
    assert!(vga.presented_ready());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::catch_up_is_incremental` and `::advance_past_a_frame`
Expected: FAIL — `catch_up`, `take_presented`, `presented_ready` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
/// The pixel-perfect raster the host pulls. Square pixels, no aspect correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgaRaster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // DAC indices; renderer resolves through the Dac
}

// Add to Vga:
//   pub(crate) work: Vec<u8>,            // current frame being assembled
//   pub(crate) presented: Option<VgaRaster>,
// Init work = vec![0; raster_width*raster_height for text mode], presented = None.

impl Vga {
    pub fn raster_width(&self) -> u32 {
        self.crtc.hdisp_end // 320-mode emits one sample per VGA pixel; doubling in Task 8
    }
    pub fn raster_height(&self) -> u32 {
        let factor = if self.crtc.double_scan { 2 } else { 1 };
        self.crtc.vdisp_end * factor // active-only for now; full-frame in Task 8
    }

    /// Render scanlines from last_line up to (not including) the current beam
    /// line, using current register state. Returns how many lines were drawn.
    pub fn catch_up(&mut self) -> u32 {
        let current = beam_line(&self.crtc, self.beam);
        let mut drawn = 0;
        while self.last_line < current {
            self.render_scanline(self.last_line);
            self.last_line += 1;
            drawn += 1;
        }
        drawn
    }

    fn render_scanline(&mut self, _line: u32) {
        // Stubbed: real content lands in Task 8. Proves line-stepping only.
    }

    fn finalize_frame(&mut self) {
        // Catch up the remainder, snapshot the work buffer, reset for next frame.
        self.last_line = self.crtc.vtotal; // render through end (stub)
        self.presented = Some(VgaRaster {
            width: self.raster_width(),
            height: self.raster_height(),
            pixels: self.work.clone(),
        });
        self.last_line = 0;
    }

    pub fn presented_ready(&self) -> bool {
        self.presented.is_some()
    }
    pub fn take_presented(&mut self) -> Option<VgaRaster> {
        self.presented.take()
    }
}
```

Update `advance` (Task 5) to finalize on a frame crossing:

```rust
    pub fn advance(&mut self, dots: u64) {
        let frame = self.frame_dots();
        if frame == 0 { return; }
        let total = self.beam + dots;
        let crossed = total / frame;
        if crossed > 0 {
            self.finalize_frame(); // finalize only the final completed frame
            self.frames += crossed;
        }
        self.beam = total % frame;
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::catch_up_is_incremental` and `::advance_past_a_frame`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Add incremental catch-up and per-frame finalize of the raster"
```

---

## Task 8: Scanline content — active pixels, border, blank, double-scan, top-justification

Fill `render_scanline` (§5, §7). Each raster row is classified active/border/blank by the CRTC and colored accordingly; active pixels come from the planes through pel-pan + attribute palette.

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn short_display_end_top_justifies_with_shortfall_at_bottom() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Jazz-like tweak: cut active to 199 lines, keep a 525-total 60Hz-class frame.
    vga.crtc.vdisp_end = 199;
    vga.crtc.vtotal = 525;
    vga.crtc.vblank_start = 245;
    vga.crtc.vblank_end = 520;
    vga.crtc.vretrace_start = 247;
    vga.crtc.vretrace_end = 249;
    // Plane 0 all 0x01 so active rows render attribute index 1.
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() { *b = 0xFF; }
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    let raster = vga.render_full_frame();
    // Doubled: 199*2 = 398 active rows at the top; the rest down to visible
    // bottom are border/blank (not the active index 1). Row 0 is active; the
    // last visible row is not.
    let w = raster.width as usize;
    assert_ne!(raster.pixels[0], 0, "row 0 should be active (top-justified)");
    let last = (raster.height as usize - 1) * w;
    assert_eq!(raster.pixels[last], 0, "bottom row is border/blank, not active");
}

#[test]
fn pixel_pan_shifts_the_active_row_left() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Encode a 1-pixel-wide marker at column 0 in plane 0.
    vga.vram[0] = 0x80; // MSB = pixel 0 set
    vga.attr.palette = core::array::from_fn(|i| i as u8);
    vga.attr.pixel_pan = 0;
    let row0 = vga.render_active_row(0);
    vga.attr.pixel_pan = 1;
    let row1 = vga.render_active_row(0);
    assert_eq!(row1[0], row0[1], "pan=1 shifts the row one pixel left");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::short_display_end` and `::pixel_pan_shifts`
Expected: FAIL — `render_full_frame`, `render_active_row` not defined; layout asserts fail.

- [ ] **Step 3: Write minimal implementation**

Rewrite `raster_height`, `render_scanline`, and add `render_active_row` / `render_full_frame`:

```rust
impl Vga {
    fn scan_factor(&self) -> u32 { if self.crtc.double_scan { 2 } else { 1 } }

    /// Full visible frame height in raster (doubled) lines: active + border +
    /// blank-as-black, i.e. the whole vtotal minus vertical retrace, doubled.
    pub fn raster_height(&self) -> u32 {
        // Visible = everything except the retrace window. For slice 1 we emit
        // the full vtotal span (blank rendered black) so a short raster's
        // letterbox is present. Spec §5/§7.
        self.crtc.vtotal * self.scan_factor()
    }

    /// Assemble one active scanline (`src_line` in undoubled active space) into
    /// `hdisp_end` DAC indices, applying pel-pan and the attribute palette.
    pub fn render_active_row(&self, src_line: u32) -> Vec<u8> {
        let width = self.crtc.hdisp_end as usize;
        let pan = (self.attr.pixel_pan & 0x0F) as usize;
        let mut row = vec![0u8; width];
        let row_base = self.crtc.start_address as usize
            + src_line as usize * self.crtc.offset as usize * 2; // byte addr
        for x in 0..width {
            let px = x + pan;
            let byte = px / 8;
            let bit = 7 - (px % 8);
            let off = (row_base + byte) % VGA_PLANE_SIZE;
            let mut index = 0u8;
            for plane in 0..VGA_PLANES {
                let b = self.vram[plane * VGA_PLANE_SIZE + off];
                index |= ((b >> bit) & 1) << plane;
            }
            row[x] = self.attr.palette[index as usize] & 0x3F;
        }
        row
    }

    fn region_color(&self, scan_line: u32) -> u8 {
        // scan_line in undoubled counter space.
        if scan_line < self.crtc.vdisp_end {
            unreachable!("active handled separately");
        } else if scan_line < self.crtc.vblank_start || scan_line >= self.crtc.vblank_end {
            self.attr.overscan & 0x3F // border = overscan color
        } else {
            0 // vertical blank = black
        }
    }

    fn render_scanline(&mut self, raster_line: u32) {
        let factor = self.scan_factor();
        let counter_line = raster_line / factor; // undoubled
        let width = self.raster_width() as usize;
        let dst = raster_line as usize * width;
        if counter_line < self.crtc.vdisp_end {
            let row = self.render_active_row(counter_line);
            self.work[dst..dst + width].copy_from_slice(&row);
        } else {
            let color = self.region_color(counter_line);
            for px in &mut self.work[dst..dst + width] {
                *px = color;
            }
        }
    }

    /// Render an entire frame to a fresh raster (used by goldens/tests).
    pub fn render_full_frame(&mut self) -> VgaRaster {
        let w = self.raster_width();
        let h = self.raster_height();
        self.work = vec![0u8; (w * h) as usize];
        for line in 0..h {
            self.render_scanline(line);
        }
        VgaRaster { width: w, height: h, pixels: self.work.clone() }
    }
}
```

Also resize `work` in `set_mode_0dh` and `Default` to `raster_width*raster_height`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::short_display_end` and `::pixel_pan_shifts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Render active, border, and blank scanlines with pel-pan and double-scan"
```

---

## Task 9: Start-address latch at vretrace; pel-pan stays live

Start-address writes buffer to `pending_start` and snapshot at the vretrace-start crossing; pel-pan applies immediately (§6).

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn start_address_write_applies_next_frame_not_mid_frame() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * 100); // beam mid-frame, line 100
    vga.set_start_address(0x2000); // buffered, not active yet
    assert_eq!(vga.crtc.start_address, 0, "start address unchanged this frame");
    // Cross the frame boundary (passes vretrace_start).
    vga.advance(vga.frame_dots());
    assert_eq!(vga.crtc.start_address, 0x2000, "applied on the next frame");
}

#[test]
fn start_address_write_during_retrace_still_applies_next_frame() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Park beam inside retrace.
    vga.advance(htotal_dots(&vga.crtc) * (vga.crtc.vretrace_start as u64 + 1));
    vga.set_start_address(0x4000);
    vga.advance(vga.frame_dots());
    assert_eq!(vga.crtc.start_address, 0x4000, "no two-frame lag");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::start_address`
Expected: FAIL — `set_start_address`, `pending_start` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// Add to Vga: pub(crate) pending_start: Option<u32>. Init None.

impl Vga {
    pub fn set_start_address(&mut self, addr: u32) {
        self.pending_start = Some(addr); // snapshot at next vretrace (finalize)
    }
}
```

Snapshot inside `finalize_frame` (the vretrace→frame-end window) before resetting:

```rust
    fn finalize_frame(&mut self) {
        self.last_line = self.crtc.vtotal;
        self.presented = Some(VgaRaster {
            width: self.raster_width(),
            height: self.raster_height(),
            pixels: self.work.clone(),
        });
        if let Some(addr) = self.pending_start.take() {
            self.crtc.start_address = addr; // latched for the next frame
        }
        self.last_line = 0;
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::start_address`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Latch the CRTC start address at vretrace; keep pel-pan live"
```

---

## Task 10: Register ports — read_port/write_port with catch-up first

All VGA ports, each calling `catch_up()` before mutating/reporting (§6, §8). Mirrors the existing `VgaTextMode::read_port/write_port` shape.

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn gc_and_seq_ports_round_trip_and_catch_up_runs_first() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    vga.advance(htotal_dots(&vga.crtc) * 4); // beam at line 4
    // Write GC index 8 (bit mask) = 0x0F via 3CE/3CF.
    vga.write_port(0x3CE, 8);
    vga.write_port(0x3CF, 0x0F);
    assert_eq!(vga.gc.bit_mask, 0x0F);
    // The write should have caught up rendering through line 4.
    assert_eq!(vga.last_line, 4);
}

#[test]
fn attribute_flipflop_alternates_index_then_data() {
    let mut vga = Vga::default();
    vga.read_status1(); // reset flip-flop to "index"
    vga.write_port(0x3C0, 0x13); // pixel pan index
    vga.write_port(0x3C0, 0x02); // value
    assert_eq!(vga.attr.pixel_pan, 0x02);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::gc_and_seq_ports` and `::attribute_flipflop`
Expected: FAIL — `write_port`/`read_port` not defined on `Vga` (or don't handle these ports).

- [ ] **Step 3: Write minimal implementation**

```rust
// Add to Vga: pub(crate) seq_index: u8, pub(crate) gc_index: u8,
// pub(crate) crtc_index: u8. Init 0.

impl Vga {
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.catch_up();
        match port {
            0x3C4 => { self.seq_index = value; true }
            0x3C5 => { self.write_seq(self.seq_index, value); true }
            0x3CE => { self.gc_index = value; true }
            0x3CF => { self.write_gc(self.gc_index, value); true }
            0x3D4 => { self.crtc_index = value; true }
            0x3D5 => { self.write_crtc(self.crtc_index, value); true }
            0x3C0 => { self.write_attr(value); true }
            // DAC + remaining ports kept on the existing Dac path (Task 13).
            _ => false,
        }
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x3DA => Some(self.read_status1()),
            _ => None,
        }
    }

    fn write_seq(&mut self, index: u8, value: u8) {
        match index {
            0x02 => self.seq.map_mask = value & 0x0F,
            0x04 => self.seq.memory_mode = value,
            _ => {}
        }
    }

    fn write_gc(&mut self, index: u8, value: u8) {
        match index {
            0x00 => self.gc.set_reset = value & 0x0F,
            0x01 => self.gc.enable_set_reset = value & 0x0F,
            0x02 => self.gc.color_compare = value & 0x0F,
            0x03 => { self.gc.rotate = value & 7; self.gc.logic = (value >> 3) & 3; }
            0x04 => self.gc.read_map = value & 3,
            0x05 => { self.gc.write_mode = value & 3; self.gc.read_mode = (value >> 3) & 1; }
            0x07 => self.gc.color_dont_care = value & 0x0F,
            0x08 => self.gc.bit_mask = value,
            _ => {}
        }
    }

    fn write_crtc(&mut self, index: u8, value: u8) {
        match index {
            0x0C => self.crtc.start_address =
                (self.crtc.start_address & 0x00FF) | (u32::from(value) << 8),
            0x0D => {
                let addr = (self.crtc.start_address & 0xFF00) | u32::from(value);
                self.set_start_address(addr);
            }
            0x13 => self.crtc.offset = u32::from(value),
            _ => {} // full timing programmed via set_mode_0dh in slice 1
        }
    }

    fn write_attr(&mut self, value: u8) {
        if !self.attr.flip_flop_data {
            self.attr.index = value & 0x1F;
            self.attr.flip_flop_data = true;
        } else {
            match self.attr.index {
                0x00..=0x0F => self.attr.palette[self.attr.index as usize] = value & 0x3F,
                0x10 => self.attr.mode_control = value,
                0x11 => self.attr.overscan = value,
                0x12 => self.attr.plane_enable = value,
                0x13 => self.attr.pixel_pan = value & 0x0F,
                0x14 => self.attr.color_select = value,
                _ => {}
            }
            self.attr.flip_flop_data = false;
        }
    }
}
```

Note: writing CRTC 0C/0D through `write_crtc` routes the *low* byte commit through `set_start_address` so the buffered-latch path (Task 9) is exercised. The high byte updates the shadow only.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::gc_and_seq_ports` and `::attribute_flipflop`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Add VGA register ports with catch-up before every mutation"
```

---

## Task 11: Port-driven copper-bar unit test (the seam, in isolation)

Prove a mid-frame palette change splits the frame at the right row, before machine integration.

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn mid_frame_palette_change_splits_the_raster_at_the_beam_row() {
    let mut vga = Vga::default();
    vga.set_mode_0dh();
    // Active content = attribute index 1 everywhere (plane 0 set).
    for b in vga.vram[0..VGA_PLANE_SIZE].iter_mut() { *b = 0xFF; }
    vga.attr.palette = core::array::from_fn(|i| i as u8); // index 1 -> DAC 1
    // Run to undoubled line 50 (raster row 100) and repaint palette[1] = 9.
    vga.advance(htotal_dots(&vga.crtc) * 50);
    vga.write_port(0x3C0, 0x01); // attr index 1
    vga.write_port(0x3C0, 9);    // palette[1] = 9
    // Finish the frame.
    vga.advance(vga.frame_dots());
    let raster = vga.take_presented().unwrap();
    let w = raster.width as usize;
    assert_eq!(raster.pixels[0], 1, "above the split uses old palette");
    let below = 120 * w; // raster row 120 (> doubled split at 100)
    assert_eq!(raster.pixels[below], 9, "below the split uses new palette");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-video vga::tests::mid_frame_palette_change`
Expected: FAIL initially if the finalize path doesn't catch up remaining lines before snapshotting. Fix by having `finalize_frame` call `catch_up()`-equivalent rendering for all lines up to vtotal using current state.

- [ ] **Step 3: Make finalize render the whole frame incrementally**

Ensure `finalize_frame` renders any not-yet-rendered lines (from `last_line` to `vtotal`) before snapshotting, so the lines after the split pick up the new palette:

```rust
    fn finalize_frame(&mut self) {
        let total = self.crtc.vtotal * self.scan_factor();
        while self.last_line < total {
            self.render_scanline(self.last_line);
            self.last_line += 1;
        }
        self.presented = Some(VgaRaster {
            width: self.raster_width(),
            height: self.raster_height(),
            pixels: self.work.clone(),
        });
        if let Some(addr) = self.pending_start.take() {
            self.crtc.start_address = addr;
        }
        self.last_line = 0;
    }
```

(And `catch_up` already renders `[last_line, current_line)` with live state, so the pre-split lines were rendered with the old palette.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::mid_frame_palette_change`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Prove the mid-frame palette split lands at the beam row"
```

---

## Task 12: Mode-set entry + raster output accessor

A public `set_mode_0dh` (already present) plus a host-facing accessor returning the last presented raster, and a `dac`-resolved variant later. Add a `Dac` field shared with the rest of the core (move the existing `Dac` usage in from `VgaTextMode` during the rename in Task 13).

**Files:** Modify `vga.rs`; Test in `vga.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn mode_set_resets_beam_and_reports_planar_geometry() {
    let mut vga = Vga::default();
    vga.advance(12345); // dirty the beam in text mode
    vga.set_mode_0dh();
    assert_eq!(vga.beam_dots(), 0);
    assert_eq!(vga.raster_width(), 320);
    assert_eq!(vga.frame_dots(), CrtcTiming::mode_0dh().frame_dots());
}
```

- [ ] **Step 2: Run test to verify it fails / passes**

Run: `cargo test -p izarravm-video vga::tests::mode_set_resets_beam`
Expected: PASS if `set_mode_0dh` (Task 5/7) already resets `beam`/`last_line`; otherwise add the resets. Confirm geometry getters return planar values.

- [ ] **Step 3: Fill any gap**

If `set_mode_0dh` does not already zero `beam`, `dot_carry`, `last_line`, and resize `work`, add those lines. No new code if Task 7 covered it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-video vga::tests::mode_set_resets_beam`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-video/src/vga.rs
git commit -m "Confirm mode-set resets the beam and reports planar geometry"
```

---

## Task 13: Fold the existing text/13h/DAC core into `Vga` (rename + relocate)

Move `VgaTextMode`'s responsibilities (text memory, mode 13h, `Dac`, CRTC cursor, existing ports) into `Vga`, and rename the type across the machine crate. Mechanical; keep all existing tests green.

**Files:**
- Modify: `crates/izarravm-video/src/lib.rs` (relocate `VgaTextMode` bodies into `vga.rs` as `Vga` methods/fields; keep `Dac`, `Framebuffer`, constants in `lib.rs`)
- Modify: `crates/izarravm-machine/src/lib.rs` (`video: Vga`, all `VgaTextMode` references)
- Test: existing crate tests

- [ ] **Step 1: Establish the baseline**

Run: `cargo test -p izarravm-video && cargo test -p izarravm-machine`
Expected: PASS (current tree is green before the rename).

- [ ] **Step 2: Move fields/methods**

Add to `Vga`: `text_memory: [u8; VGA_TEXT_MEMORY_SIZE]`, `mode13h: Framebuffer`, `dac: Dac`, `cursor_offset: u16`, `mode: VideoMode`. Move the bodies of `VgaTextMode`'s `read_u8/write_u8/read_mode13h_u8/write_mode13h_u8/set_mode13h/active_mode/frame/palette_argb/read_port/write_port` into `Vga` (merging the existing DAC/CRTC-cursor port handling into the `read_port/write_port` from Task 10 — DAC ports `3C7/3C8/3C9` call `self.catch_up()` then the `Dac`).

- [ ] **Step 3: Rename across the machine crate**

Replace `VgaTextMode` with `Vga` in `crates/izarravm-machine/src/lib.rs` (field types, `use`, constructors) and update the `lib.rs` re-export. Delete the old `VgaTextMode` definition.

- [ ] **Step 4: Run all tests**

Run: `cargo test -p izarravm-video && cargo test -p izarravm-machine`
Expected: PASS — same behavior, new type name, planar core now attached.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Rename VgaTextMode to Vga and fold the legacy core into vga.rs"
```

---

## Task 14: Advance the beam from the machine clock

Drive `vga.advance` from `advance_devices` with an f64 carry, beside `margo_ns` (§6, §8).

**Files:** Modify `crates/izarravm-machine/src/lib.rs`; Test in that crate.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn machine_advances_the_vga_beam_with_cpu_clocks() {
    let mut machine = Machine::i386dx25(1, VideoCard::Et4000Ax); // existing ctor
    machine.set_vga_mode_0dh(); // thin wrapper over self.video.set_mode_0dh()
    let before = machine.video().beam_dots();
    machine.run_for_clocks(10_000);
    assert!(machine.video().beam_dots() != before || machine.video().frames_completed() > 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-machine machine_advances_the_vga_beam`
Expected: FAIL — `set_vga_mode_0dh`, `video()` accessor, or the advance wiring missing.

- [ ] **Step 3: Wire the advance**

Add a `vga_dots: f64` accumulator field to `Machine`. In `advance_devices` (where `margo_ns` is handled), add:

```rust
self.vga_dots += clocks as f64 * VGA_DOT_HZ as f64 / self.profile.clock_hz as f64;
let whole = self.vga_dots.floor();
self.video.advance(whole as u64);
self.vga_dots -= whole;
```

with `const VGA_DOT_HZ: u64 = 25_175_000;` and a `pub fn video(&self) -> &Vga` accessor plus `pub fn set_vga_mode_0dh(&mut self)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-machine machine_advances_the_vga_beam`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-machine/src/lib.rs
git commit -m "Advance the VGA beam from the machine clock with an f64 carry"
```

---

## Task 15: `ActiveDisplay::VgaRaster` + host pull

Surface the presented raster: a new `ActiveDisplay` variant, the machine exposing the last presented frame, and `main.rs` rendering it.

**Files:** Modify `crates/izarravm-machine/src/lib.rs` and `crates/izarravm/src/main.rs`; Test in the machine crate.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn planar_mode_presents_a_vga_raster() {
    let mut machine = Machine::i386dx25(1, VideoCard::Et4000Ax);
    machine.set_vga_mode_0dh();
    machine.run_for_clocks(2_000_000); // enough to complete a frame
    assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
    assert!(machine.vga_raster().is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-machine planar_mode_presents_a_vga_raster`
Expected: FAIL — `ActiveDisplay::VgaRaster`, `vga_raster()` not defined.

- [ ] **Step 3: Add the variant + accessor + display selection**

Add `VgaRaster` to the `ActiveDisplay` enum. In `active_display()`, before the `Mode13h`/`Text` cases (and after `margo_active`), return `VgaRaster` when the VGA core is in a planar graphics mode. Add:

```rust
pub fn vga_raster(&mut self) -> Option<VgaRaster> {
    self.video.take_presented()
}
```

In `main.rs::render_current_frame`, add a match arm for `ActiveDisplay::VgaRaster` that reads `vga_raster()` (the last presented buffer, read-only — never re-rasterized) and resolves each index through `palette_argb()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-machine planar_mode_presents_a_vga_raster && cargo build -p izarravm`
Expected: PASS and the binary builds.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Present the VGA raster through a new ActiveDisplay arm"
```

---

## Task 16: End-to-end copper-bar golden through the machine

The slice's done-signal: drive registers through the bus and assert the presented raster shows the split.

**Files:** Test in `crates/izarravm-machine/src/lib.rs` (or `tests/`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn copper_bar_split_through_the_bus() {
    let mut machine = Machine::i386dx25(1, VideoCard::Et4000Ax);
    machine.set_vga_mode_0dh();
    // Fill plane 0 so the active area is attribute index 1; set palette[1]=1.
    // (Use the existing physical-write helpers used by the Margo tests.)
    fill_plane0_index1(&mut machine);
    set_attr_palette_identity(&mut machine);
    // Run to ~line 50, then rewrite palette[1] via 3C0; finish the frame.
    machine.run_for_clocks(clocks_for_line(&machine, 50));
    write_io(&mut machine, 0x3C0, 0x01);
    write_io(&mut machine, 0x3C0, 9);
    machine.run_for_clocks(clocks_for_frame(&machine));
    let raster = machine.vga_raster().expect("a frame was presented");
    let w = raster.width as usize;
    assert_eq!(raster.pixels[0], 1);
    assert_eq!(raster.pixels[120 * w], 9);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p izarravm-machine copper_bar_split_through_the_bus`
Expected: FAIL until the helpers exist and the wiring is correct.

- [ ] **Step 3: Implement the test helpers**

Add small test helpers (`fill_plane0_index1`, `set_attr_palette_identity`, `write_io`, `clocks_for_line`, `clocks_for_frame`) using the existing `write_physical_u8` / port-write patterns from the Margo machine tests. `write_io` routes through the CPU bus `write_io` path so it exercises `Vga::write_port` and its `catch_up()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p izarravm-machine copper_bar_split_through_the_bus`
Expected: PASS — slice 1 renders a planar frame and a beam-timed split, end to end.

- [ ] **Step 5: Commit**

```bash
git add crates/izarravm-machine/src/lib.rs
git commit -m "Add the end-to-end copper-bar split golden"
```

---

## Task 17: Cache the VGA reference + start the conformance doc

Per §10, drop a register reference beside the 386 manuals and open `docs/vga-core/` recording the slice-1 decisions.

**Files:**
- Create: `docs/vga-core/README.md`
- Create: `dev_docs/reference/vga/` (cache a FreeVGA-style register reference)

- [ ] **Step 1: Cache the reference**

Save a FreeVGA register reference (CRTC, Sequencer, Graphics, Attribute) into `dev_docs/reference/vga/`. Mirror how the 386 manuals are cached.

- [ ] **Step 2: Write the conformance doc**

Create `docs/vga-core/README.md` enumerating: registers/modes covered in slice 1, the start-address-vs-pel-pan latch rule, the CRTC-derived raster layout (top-justified, shortfall at bottom; Jazz example labeled doubled vs undoubled), the border/blank color boundaries, and the §9 divergences.

- [ ] **Step 3: Commit**

```bash
git add docs/vga-core/ dev_docs/reference/vga/
git commit -m "Cache the VGA register reference and open the conformance doc"
```

---

## Self-Review

**Spec coverage:** §4 datapath → Tasks 2–4; §5 scanout → Task 8; §6 beam/catch-up/latch → Tasks 5,7,9,11; §3 boot timing → Task 1; §7 raster output → Tasks 7,8,15; §8 bus integration → Tasks 13–15; §10 tests → Tasks 11,16,17; §9 divergences → Task 17. The 256 KB wrap, line-compare, and bad-card shake are explicitly deferred in the spec (§1) and are not slice-1 tasks. Covered.

**Placeholder scan:** no "TBD"/"handle edge cases" steps; every code step shows code. Task 12 is a confirm-or-fill task (acceptable — it verifies Task 7's resets).

**Type consistency:** `Vga`, `CrtcTiming`, `GfxController`, `Sequencer`, `Attribute`, `VgaRaster`, `write_planes`, `read_planes`, `catch_up`, `advance`, `set_start_address`, `set_mode_0dh`, `take_presented` are named identically across tasks. `ActiveDisplay::VgaRaster` and `vga_raster()` match between Task 15 and 16.

**Known approximations the executor should expect:** the exact CRTC timing constants in `mode_0dh()`/`text_03h()` are conventional values; the conformance doc (Task 17) validates Jazz's exact border offsets against the cached reference. The double-scan and 720-text width generalizations beyond 0Dh are out of slice-1 scope.
