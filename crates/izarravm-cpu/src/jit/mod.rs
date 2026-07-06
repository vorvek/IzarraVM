//! P3 template JIT (feature `jit`, off by default). The interpreter remains the source of
//! truth and the fallback everywhere; a compiled loop-region's only legal observable is wall
//! time. Non-(Windows|Linux)-x86-64 hosts compile nothing and run the interpreter unchanged.

pub(crate) mod drawcolumn;
mod encoder;
mod exec_mem;
mod region;
pub(crate) mod step;

pub(crate) use region::RegionTable;
