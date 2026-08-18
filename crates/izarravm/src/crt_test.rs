// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{SHADER, pack_argb_rows};

/// Parse and validate the WGSL through naga so a shader error fails the test
/// suite instead of panicking at pipeline creation when the GUI launches.
/// Catches the easy-to-trip cases: textureSample outside uniform control flow,
/// type mismatches, and uniform-buffer layout errors.
#[test]
fn shader_compiles_under_naga() {
    let module = wgpu::naga::front::wgsl::parse_str(SHADER)
        .unwrap_or_else(|e| panic!("WGSL parse error: {e}"));
    let mut validator = wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("WGSL validation error: {e}"));
}

#[test]
fn row_packing_reuses_storage_and_omits_clean_rows() {
    let words = [0, 0, 0x00ab_cdef, 0x0012_3456, 0, 0];
    let mut rgba = Vec::with_capacity(64);
    let capacity = rgba.capacity();

    pack_argb_rows(&words, 2, 1..2, &mut rgba);

    assert_eq!(rgba.capacity(), capacity);
    assert_eq!(rgba, [0xab, 0xcd, 0xef, 0xff, 0x12, 0x34, 0x56, 0xff]);
}
