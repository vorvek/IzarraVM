// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The forward-decoded instruction slot shared by `jit::block`'s basic-block builder
//! (`build_block`) and the poll-loop scanner it backs (`build_poll_loop`). The region JIT that
//! used to execute compiled slot tables through a per-slot step call (`RegionCtx`/`region_step`
//! and friends) is gone; only the slot representation the builder produces survives, because the
//! poll-loop matcher still walks it.

use crate::DecodedInsn;

/// One guest instruction slot of a forward-decoded block: the decoded instruction (refreshed
/// wholesale on every re-build, which is how self-patched immediates stay current) and the linear
/// address of its first byte.
pub(crate) struct Slot {
    pub insn: DecodedInsn,
    pub lin: u32,
    pub physical: u32,
}
