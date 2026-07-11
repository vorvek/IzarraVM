// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Template JIT (default feature `jit`). The interpreter remains the source of
//! truth and the fallback everywhere; a compiled loop-region's only legal observable is wall
//! time. Non-(Windows|Linux)-x86-64 hosts compile nothing and run the interpreter unchanged.

pub(crate) const HOST_SUPPORTED: bool = cfg!(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
));

pub(crate) mod block;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod code_watch;
pub(crate) mod direct;
pub(crate) mod encoder;
pub(crate) mod exec_mem;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod fast_map;
#[allow(dead_code)]
pub(crate) mod native_x87;
mod region;
pub(crate) mod step;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod x87_emit;

pub(crate) use region::RegionTable;
