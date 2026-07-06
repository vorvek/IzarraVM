//! P3 template JIT (feature `jit`, off by default). The interpreter remains the source of
//! truth and the fallback everywhere; a compiled loop-region's only legal observable is wall
//! time. Non-(Windows|Linux)-x86-64 hosts compile nothing and run the interpreter unchanged.

#[allow(dead_code)] // ponytail: consumed by the loop-region compiler in the next spike step
mod exec_mem;
#[allow(dead_code)] // ponytail: consumed by the compile driver + dispatch in the next commits
mod region;

pub(crate) use region::RegionTable;
