# Distira Voodoo Compatibility Spec

Distira is the VEGA 3D device, but its guest-visible behavior should converge on
86Box-like 3dfx Voodoo hardware compatibility. The goal is not a custom 3D API.
The goal is that period DOS Glide software and drivers can detect the device,
program it through the same PCI, MMIO, LFB, texture, FIFO, and status contracts
that they expect from a Voodoo Graphics or Voodoo 2 board, and render with the
same practical behavior that 86Box exposes.

This document is the long path to that target. It also defines the first safe
implementation slices so the emulator can keep passing deterministic tests while
we replace the earlier Distira-native scaffold.

## Compatibility target

### Gold standard

The behavioral reference is the 86Box Voodoo implementation, used with permission
for adaptation into this codebase. The important source areas are:

| 86Box area | Role | IzarraVM destination |
|------------|------|----------------------|
| `vid_voodoo_regs.h` | Register offsets, bitfields, texture formats, buffer selectors | `crates/izarravm-video/src/distira/regs.rs` |
| `vid_voodoo_common.h` | Device state, FIFO entries, render params, counters | `crates/izarravm-video/src/distira/state.rs` |
| `vid_voodoo.c` | PCI/device setup, memory map, main MMIO/LFB dispatch | `crates/izarravm-machine` plus `izarravm-video` front door APIs |
| `vid_voodoo_reg.c` | FBI/TMU register read/write behavior | `crates/izarravm-video/src/distira/reg.rs` |
| `vid_voodoo_fifo.c` | FIFO queueing, command ordering, wake/stall behavior | `crates/izarravm-video/src/distira/fifo.rs` |
| `vid_voodoo_fb.c` | Linear framebuffer read/write formats and buffer selection | `crates/izarravm-video/src/distira/lfb.rs` |
| `vid_voodoo_setup.c` | Triangle setup packet path | `crates/izarravm-video/src/distira/setup.rs` |
| `vid_voodoo_render.c` | Triangle rasterization, depth, alpha, fog, texture combine | `crates/izarravm-video/src/distira/render.rs` |
| `vid_voodoo_texture.c` | Texture memory, mip layout, palette/NCC tables, cache invalidation | `crates/izarravm-video/src/distira/texture.rs` |
| `vid_voodoo_display.c` | Scanout, DAC filtering, retrace, swap timing | `crates/izarravm-video/src/distira/display.rs` |
| `vid_voodoo_blitter.c` | Voodoo 2 fast fill and blitter helpers | later Voodoo 2 slice |

When code is adapted directly, keep the Rust shaped to IzarraVM conventions:
small modules, safe default paths, deterministic tests, explicit fixed-width
math, no global mutable state, and no hidden host thread dependency in unit tests.

### Device variants

The first target is a Voodoo Graphics class device with two TMUs, matching the
current BigDistira plus SmallDistira naming and the earlier Distira decision that
`video = "distira"` is one selectable device. It should behave closest to 86Box's
`VOODOO_SB50` path: Voodoo Graphics generation, dual TMU present.

The second target is `video = "voodoo2"`, backed by the Voodoo 2 paths in 86Box.
That should be a separate compatibility profile once the Voodoo Graphics path is
solid.

Do not expose `BigDistira`, `SmallDistira`, `distira1`, or `distira2` as video
card names. They are chip names inside the board, not selectable host devices.

## Definition of done

Distira is considered 90 percent complete when:

1. Real Glide DOS software can detect the card through the expected PCI/MMIO
   path without a custom Izarra driver.
2. The LFB path supports the common Voodoo formats and buffer selectors used by
   installers, diagnostics, and early game splash screens.
3. Triangle register writes and setup packets render stable untextured and
   textured triangles with the common Voodoo 1 fixed-point conventions.
4. Texture memory supports the formats used by DOS Glide 2.x games: RGB332,
   YIQ/NCC, A8, I8, AI8, paletted 8-bit, ARGB8332, RGB565, ARGB1555, ARGB4444,
   A8I8, and mip layout enough for real content.
5. The pixel pipeline implements depth, alpha test, alpha blend, chroma key,
   fog, color combine, texture combine, dithering, and write masks close enough
   for games to look right.
6. FIFO behavior, busy bits, command ordering, and swap timing are close enough
   that software polling loops do not hang or outrun the device.
7. Scanout and buffer swaps work through the machine display path with stable
   frame CRC tests.
8. A small DOS-side probe suite exercises detection, LFB, registers, FIFO,
   untextured triangles, textured triangles, depth, alpha, fog, and swap.
9. The host GUI render-thread setting controls the same partitioning concept as
   86Box's `render_threads = 1, 2, 4`, even if deterministic tests run the queue
   synchronously.
10. Focused unit tests and the normal IzarraVM CI gates pass.

It is considered perfect only when a curated set of Glide games renders with no
known device-side hacks and the remaining differences are timing or game bugs,
not missing Voodoo features.

## Architecture

### Module layout

Split the old single-file Distira scaffold into a Voodoo-shaped module tree:

```text
crates/izarravm-video/src/distira.rs          public re-exports and thin facade
crates/izarravm-video/src/distira/regs.rs     offsets, masks, enums, constants
crates/izarravm-video/src/distira/state.rs    Distira, params, counters, buffers
crates/izarravm-video/src/distira/reg.rs      register read/write semantics
crates/izarravm-video/src/distira/lfb.rs      LFB read/write and format decode
crates/izarravm-video/src/distira/fifo.rs     FIFO entries and synchronous test queue
crates/izarravm-video/src/distira/setup.rs    setup packet to render params
crates/izarravm-video/src/distira/texture.rs  TMU memory, formats, palettes, NCC
crates/izarravm-video/src/distira/render.rs   raster pipeline
crates/izarravm-video/src/distira/display.rs  scanout, swap, filter behavior
```

This keeps the source map close to 86Box while still matching Rust ownership and
IzarraVM's crate boundaries.

### Machine integration

The current fixed Distira apertures are good for host tests, but Glide software
expects PCI discovery and BAR programming. The machine path should move in this
order:

1. Keep the existing fixed aperture tests as a low-level host seam.
2. Add a minimal PCI config mechanism in `izarravm-machine` if one does not
   exist yet.
3. Expose a Voodoo-compatible PCI function for Distira when the profile selects
   `VideoCard::Distira`.
4. Map BAR writes into the Distira MMIO, LFB, and texture regions.
5. Make real guest programs reach Distira only through the PCI configured map,
   while host tests can still call `read_physical_u8` on deterministic constants.

### Memory regions

Voodoo Graphics class hardware exposes a large memory-mapped region that decodes
registers, LFB, texture memory, and FIFO windows. IzarraVM should keep internal
buffers separate and make the external map decode to those buffers.

Internal buffers:

| Buffer | Initial target |
|--------|----------------|
| Framebuffer | 4 MiB allocated, with configurable 2 or 4 MiB mask |
| Texture memory TMU0 | 4 MiB default, configurable where useful |
| Texture memory TMU1 | 4 MiB default for the dual-TMU Distira profile |
| Aux/depth | Aliased into framebuffer space per Voodoo layout |

The current 2 MiB Distira framebuffer is enough for the old scaffold but not for
86Box-like behavior. Move to the Voodoo allocation model before texture work.

### Register model

Port the 86Box register names and bitfields first. The public constants should
use the `SST_*` names so tests and future docs can be compared directly against
86Box and old 3dfx references.

Minimum first register groups:

- `SST_status`, `SST_intrCtrl`
- integer vertex and gradient registers
- float vertex and gradient registers
- `SST_triangleCMD`, `SST_ftriangleCMD`
- `SST_fbzColorPath`, `SST_fogMode`, `SST_alphaMode`, `SST_fbzMode`, `SST_lfbMode`
- `SST_clipLeftRight`, `SST_clipLowYHighY`
- `SST_nopCMD`, `SST_fastfillCMD`, `SST_swapbufferCMD`
- `SST_fogColor`, `SST_zaColor`, `SST_chromaKey`
- statistics registers
- `SST_fbiInit0` through `SST_fbiInit7`
- TMU registers from `SST_textureMode` through NCC tables
- command FIFO registers

Unsupported registers should be readable as 0 or ignored only where 86Box does
that. Otherwise, store the value even before its side effect is implemented, so
future slices do not break driver initialization sequences.

### LFB behavior

The LFB path should be feature-complete before texture rendering. It is easier to
test deterministically and many drivers use it for clears, uploads, and splash
screens.

Required LFB behavior:

- Select read buffer from `lfbMode`: front, back, aux.
- Select write buffer from `lfbMode`: front, back, aux/depth, both where needed.
- Decode common write formats: RGB565, RGB555, ARGB1555, XRGB8888, ARGB8888,
  depth plus color variants, and depth-only.
- Apply byte and word write widths correctly.
- Apply write masks and chroma/depth rules when those modes are enabled.
- Mark dirty scanout lines or otherwise refresh the active frame after writes.

### FIFO and render threading

86Box runs a FIFO thread and one, two, or four render workers. IzarraVM should
not require host threads for deterministic unit tests. The model should have:

- A queue data structure equivalent to 86Box `fifo_entry_t`.
- A render-params ring equivalent to 86Box `params_buffer`.
- Synchronous drain methods used by tests and headless deterministic runs.
- Optional host workers for GUI runs once the synchronous path is correct.
- The same `1, 2, 4` render-thread partitioning semantics exposed by the GUI.
- Busy and not-full behavior visible through status registers, even if the test
  queue drains immediately.

### Triangle and setup paths

There are two command paths:

1. Direct register triangle commands: software writes vertices, gradients, and
   render state, then writes `triangleCMD` or `ftriangleCMD`.
2. Setup packet commands: software writes packed vertex data and draw commands,
   which build the same render params.

Both paths should feed the same raster core.

Raster acceptance order:

1. Solid fast fill.
2. Untextured flat triangle.
3. Untextured Gouraud triangle.
4. Depth test and depth write.
5. Alpha test.
6. Alpha blend.
7. Chroma key.
8. Fog.
9. One TMU nearest texture.
10. One TMU bilinear texture.
11. Mip and LOD selection.
12. Two TMU combine.

### Texture and TMU behavior

Texture memory should be ported after the register and LFB base is stable. Do
not invent a simplified texture format API. Use the Voodoo textureMode, tLOD,
base address, palette, and NCC contracts.

Required format order:

1. RGB565 and ARGB1555, because they are direct and useful for early tests.
2. RGB332, I8, A8, AI8.
3. Paletted 8-bit and APAL8.
4. ARGB4444, ARGB8332, A8I8.
5. YIQ/NCC formats and NCC table update behavior.
6. Mip layout, LOD clamp, detail, trilinear flag handling.
7. Texture cache invalidation and per-TMU dirty ranges.

### Display and swap

The display path should eventually use Voodoo timing registers instead of the
old fixed 640x480 Distira display. Until then, tests can keep deterministic frame
sizes. The compatibility target needs:

- Front/back buffer offsets derived the way 86Box derives them.
- `swapbufferCMD` behavior with immediate and pending swap modes.
- Swap interval and retrace behavior good enough for polling loops.
- Dirty line handling or equivalent frame invalidation.
- Optional DAC filter behavior after the pixel pipeline is correct.

### PCI path

Glide software typically finds Voodoo through PCI. Add this after the first MMIO
register model exists, not before. The PCI slice should cover:

- Config mechanism access through ports `0xCF8` and `0xCFC`.
- One Distira PCI function behind `VideoCard::Distira`.
- Vendor/device/class IDs compatible enough for Glide detection.
- BAR sizing and assignment behavior.
- MMIO decode through programmed BARs.
- Tests that scan PCI config space and locate the card from a DOS program.

## Test strategy

Use three layers of tests.

### Unit tests in `izarravm-video`

These prove the Voodoo core without CPU or bus noise:

- Register constants match the 86Box offsets we depend on.
- Register writes round-trip where hardware stores values.
- Side-effect registers run clear, swap, fastfill, triangle, and FIFO actions.
- LFB mode selects the correct buffers and formats.
- Texture format decoders return known colors.
- Raster operations produce tiny deterministic images.

### Machine tests in `izarravm-machine`

These prove address decode and display integration:

- MMIO and LFB physical paths reach Distira.
- Voodoo-shaped register writes produce scanout frames.
- Future PCI config writes map the BAR and the guest can reach it there.
- INT 10h/VGA mode sets hand display back to VGA and disable Distira scanout.

### Guest tests in the boot suite or Toka-DOS tools

These are the compatibility evidence:

- PCI probe finds the card.
- MMIO register probe validates identity, init, status, and buffer bits.
- LFB probe writes known pixels in each common format and checks a host CRC.
- FIFO probe writes a short command stream and waits on status without hanging.
- Triangle probe renders a known untextured triangle and checks a CRC.
- Texture probe uploads a tiny texture and renders a known textured triangle.

## Implementation phases

### Phase 0: Spec and provenance

- Add this spec.
- Keep a local source map from 86Box files to Rust modules as code is adapted.
- Do not delete the existing Distira tests until equivalent Voodoo-shaped tests
  replace them.

Acceptance: spec exists, current tests still pass.

### Phase 1: Voodoo register constants and state shell

- Add `SST_*`, `FBZ_*`, `LFB_*`, `TEX_*`, and init bit constants.
- Add a Voodoo-shaped state struct inside Distira for FBI/TMU params, init regs,
  `lfbMode`, `fbzMode`, counters, and front/back offsets.
- Keep the old clear/swap/scanout tests passing through compatibility helpers.

Acceptance: constants and basic register storage tests pass.

### Phase 2: Voodoo MMIO front door

- Route reads and writes through Voodoo register offsets.
- Store FBI init registers and core render state.
- Implement `swapbufferCMD`, `fastfillCMD`, `nopCMD`, and status reads.
- Keep old Distira command offsets only as a temporary host-test shim if needed.

Acceptance: machine tests can drive a clear/swap using `SST_*` registers.

### Phase 3: LFB buffers and formats

- Expand framebuffer allocation to Voodoo size.
- Add front/back/aux buffer selection through `lfbMode` and `fbzMode`.
- Implement RGB565, RGB555, ARGB1555, XRGB8888, ARGB8888, depth, and depth plus
  color LFB formats.

Acceptance: unit tests write each format to back/front and scan out known colors.

### Phase 4: PCI config and BAR mapping

- Add minimal PCI config mechanism if missing.
- Expose Distira as a PCI device with BAR sizing and assignment.
- Map programmed BARs into the existing machine memory path.

Acceptance: a guest or machine test can find and map Distira through PCI.

### Phase 5: FIFO skeleton

- Add FIFO entry queue and params ring.
- Queue register, LFB, and texture writes.
- Drain synchronously in tests.
- Expose status/busy/full/empty enough for polling loops.

Acceptance: FIFO command stream clear/swap and register writes match direct MMIO.

### Phase 6: Triangle setup and untextured raster

- Port integer and float triangle setup conventions.
- Render flat and Gouraud untextured triangles through the Voodoo params path.
- Add clip registers and write masks.

Acceptance: tiny triangle CRC tests pass through direct and setup paths.

### Phase 7: Depth, alpha, chroma, fog

- Port depth compare and write behavior.
- Port alpha test and alpha blend modes.
- Port chroma key and fog table behavior.

Acceptance: each feature has a small deterministic frame test.

### Phase 8: Texture memory and one TMU

- Add texture memory writes and dirty tracking.
- Port direct texture formats first, then paletted and NCC.
- Render nearest and bilinear one-TMU textured triangles.

Acceptance: textured triangle tests pass for common formats.

### Phase 9: Dual TMU and Voodoo 2 profile

- Enable TMU1 combine.
- Add Voodoo 2-specific init, framebuffer, texture, and blitter behavior.
- Wire `VideoCard::Voodoo2` separately.

Acceptance: dual-TMU tests pass and Voodoo 2 has its own profile tests.

### Phase 10: Game-led compatibility hardening

- Run known Glide diagnostics and a small game set.
- Add guest probes for every hang or visual defect.
- Tune timing only after functional gaps are closed.

Acceptance: curated Glide smoke set runs with documented results.

## Development progress

Mark each implementation iteration here when the tested slice is committed and
pushed. Keep entries tied to guest-visible behavior, not internal refactors.

- [x] Iteration 1: Voodoo register, MMIO, LFB, PCI BAR, and FIFO groundwork.
      Commit `8693fad`. Validated by Distira video and machine tests plus the
      workspace gates.
- [x] Iteration 2: command FIFO aperture for type-1 register packets. Commit
      `b6cc3d3`. Validated by the machine command FIFO aperture test plus the
      workspace gates.
- [x] Iteration 3: command FIFO type-5 framebuffer packets. Commit `7fc4652`.
      Validated by the machine framebuffer packet test plus the workspace gates.
- [x] Iteration 4: command FIFO type-5 texture packets. Commit `b6ef44c`.
      Validated by the video texture packet test plus the workspace gates.
- [x] Iteration 5: direct Voodoo triangle command registers for a flat
      untextured triangle through `SST_triangleCMD`. Validated by the video
      triangle command test plus the workspace gates.
- [x] Iteration 6: float Voodoo triangle command registers for a flat
      untextured triangle through `SST_ftriangleCMD`. Validated by the video
      float triangle command test plus the workspace gates.
- [x] Iteration 7: Gouraud color gradients for untextured Voodoo triangles
      through integer color derivative registers. Validated by the video
      triangle gradient test plus the workspace gates.
- [x] Iteration 8: float color derivative registers for `SST_ftriangleCMD`.
      Validated by the video float triangle gradient test plus the workspace
      gates.
- [x] Iteration 9: depth test and depth write for untextured integer
      triangles. Validated by the video depth rejection test plus the workspace
      gates.
- [x] Iteration 10: float Z and Z derivative registers for `SST_ftriangleCMD`.
      Validated by the video float-depth acceptance test plus the workspace
      gates.
- [x] Iteration 11: alpha test for untextured integer Voodoo triangles.
      Validated by the video alpha rejection test plus the workspace gates.
- [x] Iteration 12: float alpha and alpha derivative registers for
      `SST_ftriangleCMD`. Validated by the video float-alpha rejection test plus
      the workspace gates.
- [x] Iteration 13: alpha blending for untextured Voodoo triangles. Validated by
      the video source-over-destination alpha blend test plus the workspace
      gates.
- [x] Iteration 14: chroma key rejection for untextured Voodoo triangles.
      Validated by the video chroma-key rejection test plus the workspace gates.
- [x] Iteration 15: constant fog color application for untextured Voodoo
      triangles. Validated by the video constant-fog test plus the workspace
      gates.
- [x] Iteration 16: texture-enabled color path for nearest RGB565 texels.
      Validated by the video RGB565 texture sample test plus the workspace gates.
- [x] Iteration 17: S/T texture coordinate gradients for nearest RGB565
      sampling. Validated by the video S-gradient texture sample test plus the
      workspace gates.
- [x] Iteration 18: float S/T texture coordinate registers for nearest RGB565
      sampling. Validated by the video float S-gradient texture sample test plus
      the workspace gates.
- [x] Iteration 19: one-TMU bilinear RGB565 texture sampling. Validated by the
      video bilinear texture sample test plus the workspace gates.
- [x] Iteration 20: mip and LOD selection for RGB565 texture sampling. Validated
      by the video tLOD-min mip selection test plus the workspace gates.
- [x] Iteration 21: two-TMU texture combine for RGB565 sampling. Validated by
      the video two-TMU RGB565 add-combine test plus the workspace gates.
- [x] Iteration 22: RGB332 texture sampling. Validated by the video RGB332
      texture sample test plus the workspace gates.
- [x] Iteration 23: I8 texture sampling. Validated by the video I8 texture
      sample test plus the workspace gates.
- [x] Iteration 24: A8 texture sampling. Validated by the video A8 texture
      sample test plus the workspace gates.
- [x] Iteration 25: AI44 texture sampling. Validated by the video AI44 texture
      sample test plus the workspace gates.
- [x] Iteration 26: AI88 texture sampling. Validated by the video AI88 texture
      sample test plus the workspace gates.
- [x] Iteration 27: ARGB8332 texture sampling. Validated by the video ARGB8332
      texture sample test plus the workspace gates.
- [x] Iteration 28: ARGB1555 texture sampling. Validated by the video ARGB1555
      texture sample test plus the workspace gates.
- [x] Iteration 29: ARGB4444 texture sampling. Validated by the video ARGB4444
      texture sample test plus the workspace gates.
- [x] Iteration 30: PAL8 texture sampling. Validated by the video PAL8 texture
      sample test plus the workspace gates.
- [x] Iteration 31: APAL8 texture sampling. Validated by the video APAL8
      texture sample test plus the workspace gates.
- [x] Iteration 32: APAL88 texture sampling. Validated by the video APAL88
      texture sample test plus the workspace gates.
- [x] Iteration 33: YIQ/NCC texture sampling. Validated by the video Y4I2Q2
      NCC texture sample test plus the workspace gates.
- [x] Iteration 34: A8Y4I2Q2 texture sampling. Validated by the video A8Y4I2Q2
      NCC texture sample test plus the workspace gates.
- [x] Iteration 35: LOD clamp behavior. Validated by the video tLOD max clamp
      texture sample test plus the workspace gates.
- [x] Iteration 36: S/T texture clamp behavior. Validated by the video S clamp
      texture sample test plus the workspace gates.
- [x] Iteration 37: S/T texture mirror behavior. Validated by the video S mirror
      texture sample test plus the workspace gates.
- [x] Iteration 38: texture multibase LOD address behavior. Validated by the
      video RGB565 multibase LOD address test plus the workspace gates.
- [ ] Next: split/odd LOD selection behavior.

## First 90 percent push for this branch

Today should not try to port all of 86Box in one commit. The largest safe slice
is phases 1 and 2 plus the start of phase 3:

1. Introduce Voodoo register constants and state shell.
2. Add Voodoo-shaped MMIO reads and writes for status, init regs, render state,
   `lfbMode`, `fbzMode`, `fastfillCMD`, and `swapbufferCMD`.
3. Keep the existing Distira framebuffer/scanout path working through those
   Voodoo registers.
4. Add unit and machine tests that use `SST_*` names instead of the custom
   `DISTIRA_REG_*` command path.
5. Leave PCI, FIFO, and texture work as the next slices once the register model
   is stable.

That does not reach 90 percent of final Glide compatibility. It reaches a much
better 90 percent of the first architectural pivot: from custom Distira commands
to a Voodoo-shaped device core.
