// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Swept-mode differential test: every monomorphized kernel against
//! `RasterView::raster_row`, the unspecialized oracle every kernel body was
//! copied from. Two independently allocated frame stores rasterise the SAME
//! triangle through the two paths and the byte contents, not just the
//! counters, must come out identical — a kernel that skipped a write the
//! oracle makes would still get the counters right but paint the wrong
//! pixel, and only the frame comparison catches that.

use super::*;

fn frame_store() -> FrameStore {
    FrameStore::new(DISTIRA_FB_SIZE)
}

/// Read back only the bytes the 8x8 scene can possibly touch: the colour
/// buffer's and the depth buffer's first 8 rows. Reading the whole 2 MB
/// frame store with one atomic load per byte, times two arms times twelve
/// mode combinations, would make this test slow for no extra coverage —
/// nothing outside these two windows is reachable from an 8x8 triangle at
/// the origin.
fn read_frame(fb: &FrameStore, display: &DistiraDisplay, aux_base: u32) -> Vec<u8> {
    let rows = 8 * display.pitch as usize;
    let color_start = display.back_base as usize;
    let depth_start = aux_base as usize;
    (color_start..color_start + rows)
        .chain(depth_start..depth_start + rows)
        .map(|i| fb.get(i).unwrap())
        .collect()
}

/// A small triangle plus the register state a `ModeKey`'s four flags read,
/// built directly rather than through the guest-facing register API: this
/// exercises `RasterView::raster_row` and `raster_row_specialized` alone,
/// with no queue, no lane split and no `Distira` device in between.
fn scene(
    depth: bool,
    wbuffer: bool,
    textured: bool,
    blend: bool,
) -> (RasterParams, TriangleContext) {
    let display = DistiraDisplay::default();
    let aux_base = display.back_base + display.pitch * display.height;

    let mut fbz_mode = FBZ_RGB_WMASK | FBZ_DRAW_BACK;
    if depth {
        // ALWAYS, not LESSTHAN: the depth buffer starts zeroed and these
        // scenes use positive depth codes, so LESSTHAN against an
        // uninitialized zero would reject every pixel and defeat the
        // "the scene must actually paint" check below. The depth TEST
        // path is exercised regardless — `depth_test_passes` still runs,
        // still reads the buffer, still writes it back — only the
        // comparison result changes.
        fbz_mode |= FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT);
    }
    let params = RasterParams {
        display,
        aux_base,
        dither_enabled: false,
        fbz_mode,
        // RGB_SELECT_TEXTURE, not just FBZCP_TEXTURE_ENABLED: the combine
        // unit's own colour-select bits decide whether `combined_texture`'s
        // RESULT reaches the output pixel at all. `FBZCP_TEXTURE_ENABLED`
        // alone only gates whether it is CALLED — with the iterated colour
        // still selected downstream, a kernel that skipped the call
        // entirely would be observationally identical to one that made it,
        // which would make this scene unable to tell a correct kernel from
        // a broken one. `texture_mode` is left at 0 (`TEX_RGB332`), which
        // `selected_color_or_source` accepts for `RGB_SELECT_TEXTURE`.
        fbz_color_path: if textured {
            FBZCP_TEXTURE_ENABLED | RGB_SELECT_TEXTURE
        } else {
            0
        },
        alpha_mode: if blend {
            ALPHA_BLEND_ENABLE | (0x1 << 8) | (0x5 << 12)
        } else {
            0
        },
        fog_mode: 0,
        fog_color: 0,
        za_color: 0,
        chroma_key: 0,
        color0: 0,
        color1: 0,
        stipple: 0,
        texture_mode: 0,
        texture_mode_tmu1: 0,
        texture_lod: 0,
        texture_lod_tmu1: 0,
        texture_detail: 0,
        texture_detail_tmu1: 0,
        tex_base_addr: 0,
        tex_base_addr_tmu1: 0,
        tex_base_addr1: [0, 0],
        tex_base_addr2: [0, 0],
        tex_base_addr38: [0, 0],
        trex_init1: [0, 0],
        force_point_sampling: false,
    };

    let a = DistiraVertex {
        x: 0.0,
        y: 0.0,
        r: 10,
        g: 200,
        b: 40,
        a: 255,
        s: 0.0,
        t: 0.0,
    };
    let b = DistiraVertex {
        x: 8.0,
        y: 0.0,
        r: 250,
        g: 20,
        b: 60,
        a: 200,
        s: 8.0,
        t: 0.0,
    };
    let c = DistiraVertex {
        x: 0.0,
        y: 8.0,
        r: 30,
        g: 90,
        b: 220,
        a: 128,
        s: 0.0,
        t: 8.0,
    };
    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    let depths = depth.then_some(if wbuffer {
        TriangleDepth::W([2.0, 2.5, 1.8])
    } else {
        TriangleDepth::Z([1_000_000.0, 1_200_000.0, 900_000.0])
    });

    // A REAL `TextureRaster`, not `None`, when textured: `None` takes the
    // procedural-affine branch in the raster loop, which never calls
    // `TextureRaster::samples_masked` and so never reads `tmu_need` at
    // all — hoist #1 (`tmu_need`'s `if TEXTURED` gate) would be dead code
    // in this sweep with `None` here, exactly the gap N1 in the review
    // found. Built the same way `texture_raster_test.rs`'s own
    // `samples_masked` test builds one: a `TextureIteratorState` fed
    // small positive `START_S`/`START_T` values through `write_register`.
    let texture = textured.then(|| {
        fn write_start(state: &mut TextureIteratorState, chip: usize, register: usize, value: u32) {
            for (byte, b) in value.to_le_bytes().into_iter().enumerate() {
                state.write_register(chip, register, byte, b);
            }
        }
        // Register units are 18 fractional bits below the integer texel
        // coordinate (`fixed_to_internal`'s S/T shift of 14, then
        // `RegisterPlane::as_f64`'s divide by 2^32): `value << 18` puts a
        // whole-number texel coordinate at the far side of that math. Small
        // integer registers (as `texture_raster_test.rs` uses) land well
        // under 1.0 after the same conversion — a real, non-`UNUSED` sample,
        // but one that rounds to texel (0, 0), the SAME texel the `UNUSED`
        // placeholder decodes to. That would make this scene just as blind
        // to hoist #1 as `texture: None` was, only for a subtler reason.
        let mut state = TextureIteratorState::default();
        write_start(&mut state, CHIP_TREX0, SST_START_S, 10 << 18);
        write_start(&mut state, CHIP_TREX0, SST_START_T, 20 << 18);
        write_start(&mut state, CHIP_TREX1, SST_START_S, 120 << 18);
        write_start(&mut state, CHIP_TREX1, SST_START_T, 200 << 18);
        state.raster([0, 0], [0, 0], (0.0, 0.0))
    });

    let context = TriangleContext {
        vertices: [a, b, c],
        area,
        depths,
        texture,
        coverage: None,
        count_fbi_pixels: true,
        affine_lods: [0, 0],
        min_x: 0,
        max_x: 8,
        min_y: 0,
        max_y: 8,
    };
    (params, context)
}

/// RED PROOFS, all applied to the branch, run, and reverted (`git status`
/// clean afterwards) — recorded here as the evidence the harness actually
/// discriminates a wrong kernel from a merely-different one, rather than
/// always agreeing by construction.
///
/// **A.** Changing `if TEXTURED {` to `if false {` at the `combined_texture`
/// call site in `raster_view.rs` (skip the call, always take the `else`
/// arm) fails on the `textured` combinations and passes on the untextured
/// ones.
///
/// **B (hoist #1, added for review nit N1).** An earlier version of this
/// sweep used `texture: None` in every scene, which takes the
/// procedural-affine branch and never calls `TextureRaster::samples_masked`
/// at all — `tmu_need`'s `if TEXTURED { self.tmu_need() } else { [false,
/// false] }` hoist was dead code in the test, invisible to any mutation of
/// it. `scene()` now builds a real `TextureRaster` when `textured` is set,
/// and the sweep fills TMU memory with a position-varying gradient rather
/// than a uniform fill, so sampling the WRONG texel — in particular the
/// `UNUSED` placeholder's fixed (0, 0), which `tmu_need` reporting a TMU
/// unneeded produces — decodes to a different colour than the real
/// interpolated position.
///
/// Two mutations of that line were tried:
///
/// - Replacing the whole expression with unconditional `[true, true]`
///   (over-sampling both TMUs always) stays GREEN, and provably must: the
///   colour combine only ever reads the TMU slot(s) `texture_combine_target`
///   says it needs, so a slot sampled but never read cannot reach the
///   output no matter what it contains. This is the same fact section 2 of
///   the review used to call hoist #1 "exact by construction" — the test
///   confirms it rather than missing it.
/// - Replacing it with unconditional `[false, false]` (under-sampling —
///   reporting every TMU unneeded) goes RED on `depth=false wbuffer=false
///   textured=true blend=false`: `color_written_nonblack` diverges (36 vs
///   0), because the specialized path now decodes the `UNUSED` placeholder
///   texel instead of the real one. This is the failure mode hoist #1
///   actually has to avoid, and the one worth a red proof.
///
/// **C.** Swapping `alpha_blend_color`'s call for a no-op skip at the
/// `if BLEND {` site fails at the FRAME-STORE assertion, not the stats one,
/// on `blend=true` — independent evidence for the half of the harness that
/// exists to catch a kernel skipping a write while still getting every
/// counter right.
#[test]
fn every_kernel_matches_the_generic_oracle_across_the_mode_space() {
    for depth in [false, true] {
        for wbuffer in [false, true] {
            for textured in [false, true] {
                for blend in [false, true] {
                    // `wbuffer` only means something when `depth` is set;
                    // skip the redundant half of the sweep.
                    if !depth && wbuffer {
                        continue;
                    }
                    let (params, context) = scene(depth, wbuffer, textured, blend);
                    let ncc = NccState::default();
                    // A GRADIENT, not a uniform fill: uniform texture memory
                    // decodes to the same colour no matter which texel offset
                    // gets read, which would make a kernel that samples the
                    // wrong position — in particular, the `UNUSED` placeholder
                    // at texel (0, 0) instead of `scene`'s real interpolated
                    // position — look identical to a correct one. The
                    // gradient makes offset 0 decode differently from every
                    // other offset the triangle's real texture coordinates
                    // reach, so hoist #1 (`tmu_need`) has something to lose
                    // by wrongly reporting a TMU unneeded.
                    let gradient: Vec<u8> =
                        (0..DISTIRA_TEX_SIZE).map(|i| (i % 256) as u8).collect();
                    let texture: [Vec<u8>; 2] = [gradient.clone(), gradient];

                    let fb_generic = frame_store();
                    let view_generic = RasterView {
                        params,
                        fb: &fb_generic,
                        texture: &texture,
                        ncc: &ncc,
                    };
                    let mut stats_generic = PixelStats::new(0);
                    for y in context.min_y..context.max_y {
                        view_generic.raster_row(&context, y, &mut stats_generic);
                    }

                    let fb_kernel = frame_store();
                    let view_kernel = RasterView {
                        params,
                        fb: &fb_kernel,
                        texture: &texture,
                        ncc: &ncc,
                    };
                    let key = ModeKey {
                        depth,
                        wbuffer,
                        textured,
                        blend,
                    };
                    let kernel = select_kernel(key);
                    let mut stats_kernel = PixelStats::new(0);
                    for y in context.min_y..context.max_y {
                        kernel(&view_kernel, &context, y, &mut stats_kernel);
                    }

                    assert_eq!(
                        stats_generic, stats_kernel,
                        "stats diverged for depth={depth} wbuffer={wbuffer} \
                         textured={textured} blend={blend}"
                    );
                    assert_eq!(
                        read_frame(&fb_generic, &params.display, params.aux_base),
                        read_frame(&fb_kernel, &params.display, params.aux_base),
                        "frame store diverged for depth={depth} wbuffer={wbuffer} \
                         textured={textured} blend={blend}"
                    );
                    assert!(
                        stats_generic.color_written > 0,
                        "the scene must actually paint for depth={depth} wbuffer={wbuffer} \
                         textured={textured} blend={blend}"
                    );
                }
            }
        }
    }
}

#[test]
fn select_kernel_is_total_over_the_mode_space() {
    // Every boolean combination `ModeKey` can hold maps to a real kernel:
    // there is no "unlisted key" case for these four flags, so a caller
    // never falls back to the generic path for coverage reasons. This test
    // is here mainly so the match arms stay exhaustive as the enum grows —
    // rustc already enforces it at compile time, but a compile error on an
    // unrelated file is a confusing way to learn the table is incomplete.
    for depth in [false, true] {
        for wbuffer in [false, true] {
            for textured in [false, true] {
                for blend in [false, true] {
                    let _ = select_kernel(ModeKey {
                        depth,
                        wbuffer,
                        textured,
                        blend,
                    });
                }
            }
        }
    }
}
