// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Template JIT (default feature `jit`). The interpreter remains the source of
//! truth and the fallback everywhere; a compiled loop-region's only legal observable is wall
//! time. Non-(Windows|Linux)-x86-64 hosts compile nothing and run the interpreter unchanged.

pub(crate) const HOST_SUPPORTED: bool = cfg!(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
));

/// Whether the diagnostic single-address admission override is active. This is shared with the
/// fast-map fill gate so `IZARRAVM_JIT=0` keeps the large map unallocated unless a forced compile
/// was explicitly requested.
pub(crate) fn forced_admission_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("IZARRAVM_JIT_REGION")
            .ok()
            .and_then(|value| u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok())
            .is_some()
    })
}

pub(crate) mod block;
pub(crate) mod direct;
pub(crate) mod encoder;
pub(crate) mod exec_mem;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod fast_map;
mod region;
pub(crate) mod step;

pub(crate) use region::RegionTable;
