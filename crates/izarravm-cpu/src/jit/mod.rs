//! Template JIT (default feature `jit`). The interpreter remains the source of
//! truth and the fallback everywhere; a compiled loop-region's only legal observable is wall
//! time. Non-(Windows|Linux)-x86-64 hosts compile nothing and run the interpreter unchanged.

pub(crate) const HOST_SUPPORTED: bool = cfg!(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
));

pub(crate) mod block;
pub(crate) mod encoder;
pub(crate) mod exec_mem;
mod region;
pub(crate) mod step;

pub(crate) use region::RegionTable;
