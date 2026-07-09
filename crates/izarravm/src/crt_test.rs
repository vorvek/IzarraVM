// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::SHADER;

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
