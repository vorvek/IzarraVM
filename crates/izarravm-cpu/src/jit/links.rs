// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Backend-neutral link-chaining vocabulary (Track C C1d-pre hoist).
//!
//! `LinkTarget`, `BlockPortal` (plus `zero_portal`), `LinkCell`, and `LinkSource` moved out of
//! `jit::direct` VERBATIM (field-for-field, method-for-method): none of the four referenced
//! anything Direct-specific in their own definitions, and the extraction is behavior-preserving
//! for Direct (see `dev_docs/plans/2026-07-19-track-c1d-design.md` section 5.1). `LinkSource` is
//! generic over its own `SourceId` type parameter so a future backend can
//! instantiate it over its own unit-index type without pulling in Direct's `BlockId` (which stays
//! in `direct.rs`, since it carries Direct's generational-slot semantics and is not itself
//! mechanism-neutral); Direct instantiates `LinkSource<BlockId>`.
//!
//! These types are mechanism-neutral pointers-with-ordering ONLY: they carry no assumption about
//! what a "hidden" or "unresolved" link looks like. `zero_portal()` and `BlockPortal`/`LinkCell`'s
//! zero-address default are DIRECT'S OWN mechanism (an inline zero-check against a permanent
//! sentinel portal whose `body` is always 0), documented on each method below; a different backend
//! may publish its own sentinel portal with a non-zero, backend-specific address and compare
//! against THAT instead of zero (the now-removed clif backend's design did exactly this, see the
//! C1d design doc section 3.3b, historical). Do not read `zero_portal`/the zero-body convention
//! as a shared invariant every backend must honor.
//!
//! CACHE-level bookkeeping (the tables that key, resolve, and invalidate against these types) and
//! the resolution algorithms that operate on them stay OWNED separately by each backend's own
//! cache type; only this shared vocabulary is common.

use std::sync::{
    OnceLock,
    atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LinkTarget {
    pub(crate) linear: u32,
    pub(crate) mode_key: u32,
}

/// A published jump target: the address of whatever landing record a resolved link should
/// transfer to. Direct publishes a `CompiledBlock`'s native entry point here; a different
/// backend may publish a different kind of landing record (e.g. a descriptor address), since
/// the portal itself does not interpret `body`, it only stores and orders it.
#[repr(C)]
pub(crate) struct BlockPortal {
    pub(crate) body: AtomicUsize,
    /// The address an INTEGER source jumps to. For an integer target this equals `body`; for a
    /// float target it is the shared x87 re-entry pad, which enters the x87 register cache the
    /// target's prologue would have entered and then jumps to `body`. Direct's mechanism; a
    /// backend with no pad publishes `body` here and never notices the field.
    ///
    /// Held in the portal rather than the cell because it is a property of the TARGET and must
    /// survive `compact_arena` relocation. A float block's value is always the one pad address,
    /// so no publisher has an address argument it could get wrong.
    pub(crate) integer_entry: AtomicUsize,
}

impl BlockPortal {
    pub(crate) fn new() -> Self {
        Self {
            body: AtomicUsize::new(0),
            integer_entry: AtomicUsize::new(0),
        }
    }

    pub(crate) fn address(&self) -> usize {
        std::ptr::from_ref(self) as usize
    }

    /// Direct's own hide: publish the zero sentinel value. `zero`'s meaning ("unresolved" or
    /// "hidden") is Direct's convention, not a property this type enforces; a backend that needs
    /// a different hidden representation should not call `clear` and should instead `publish`
    /// its own sentinel address (see the module doc comment).
    pub(crate) fn clear(&self) {
        self.integer_entry.store(0, Ordering::Release);
        self.body.store(0, Ordering::Release);
    }

    /// Publish a target with no x87 re-entry pad: an integer source and a float source both land
    /// on `body`. Correct for every backend that does not build a pad, which is why this keeps a
    /// one-argument signature.
    pub(crate) fn publish(&self, body: usize) {
        debug_assert_ne!(body, 0);
        self.integer_entry.store(body, Ordering::Release);
        self.body.store(body, Ordering::Release);
    }

    /// Publish a FLOAT target: an integer source is routed through the shared x87 re-entry pad.
    /// `body` is stored last, so `visible()` never reports a portal whose `integer_entry` has not
    /// been written yet.
    /// `pad` is `None` only when the host could not build the shared pad. Publishing ZERO then is
    /// deliberate and is the fail-safe direction: an integer source reading zero takes the
    /// unresolved path, exactly as it does for an unlinked cell. Publishing `body` instead would
    /// enter a float block with an unloaded x87 register cache, and it would make correctness
    /// depend on `try_link_inner` also refusing the edge rather than on this value alone.
    pub(crate) fn publish_x87(&self, body: usize, pad: Option<usize>) {
        debug_assert_ne!(body, 0);
        self.integer_entry
            .store(pad.unwrap_or(0), Ordering::Release);
        self.body.store(body, Ordering::Release);
    }

    /// Direct's own linked-ness predicate: non-zero `body`. This is DIRECT'S mechanism (the
    /// inline zero-check unresolved sentinel), not a shared invariant; a backend using a
    /// non-zero sentinel representation must not rely on this predicate and should define its
    /// own comparison against its own sentinel address instead.
    pub(crate) fn visible(&self) -> bool {
        let body = self.body.load(Ordering::Acquire);
        // One direction only. The reverse does not hold when the x87 pad could not be allocated:
        // `try_link_inner` refuses those edges, so a float target can legitimately sit with a live
        // `body` and a zero `integer_entry`, and an integer source reading zero takes the
        // unresolved path exactly as it does today.
        debug_assert!(body != 0 || self.integer_entry.load(Ordering::Acquire) == 0);
        body != 0
    }
}

/// Direct's own permanent zero-body sentinel portal: every fresh `LinkCell` points at this until
/// linked, and hiding a block repoints its portal back to the zero body. This is Direct's
/// mechanism for representing "unresolved"/"hidden"; it is not shared, mechanism-neutral storage
/// in the sense the rest of this module is; a different backend that needs its own sentinel
/// representation (e.g. a non-zero sentinel descriptor address) publishes its own static portal
/// instead of calling this function. Kept in the shared module because it is, structurally, just
/// a static `BlockPortal` instance holding a permanent value; only its zero-body INTERPRETATION is
/// Direct-specific.
pub(crate) fn zero_portal() -> &'static BlockPortal {
    static ZERO_PORTAL: OnceLock<BlockPortal> = OnceLock::new();
    ZERO_PORTAL.get_or_init(BlockPortal::new)
}

#[repr(C)]
pub(crate) struct LinkCell {
    pub(crate) portal: AtomicUsize,
    pub(crate) target_eip: AtomicU32,
    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) direct_link_refusal_census_id: AtomicU32,
    /// The baked `x87_entry_top` of the block this cell is bound to, or `NO_ENTRY_TOP` when the
    /// target has none. The shared x87 re-entry pad compares it against the CPU's live TOP and
    /// bails when they differ, because a float block's emitted code addresses its register cache
    /// relative to the TOP baked at compile time.
    ///
    /// Per EDGE rather than per target on purpose: `try_link_inner` is the single writer and
    /// `LinkCell::clear` the single resetter, against the portal's three publishers and nine
    /// clear sites.
    pub(crate) entry_top: AtomicU8,
    /// Direct's own x87 link-relaxation mechanism (not shared with Clif): set when this slot's
    /// edge is a float source linking to an integer target, so the emitted jump at the link site
    /// knows to flush the live x87 cache back to `CpuGsw.fpu` before handing control to a target
    /// that never reads it. Every other edge shape (both integer, both float, or unresolved)
    /// leaves this at 0. Read with a plain byte test in emitted code (no atomic instruction
    /// needed there, same as `portal`/`target_eip`); stored with `Release`/loaded with `Acquire`
    /// on the Rust side, matching the other fields here. Those orderings are a formality for a
    /// single-owner cache, not a cross-thread guarantee: izarravm-cpu spawns no threads and the
    /// block cache is mutated only by the thread running native code, so there is no concurrent
    /// relink for them to guard against. The emitted reader plainly loads `portal` and, many
    /// instructions later, plainly loads this byte, so if that assumption ever stopped holding,
    /// a genuinely concurrent relink could still pair a stale `portal` with a fresh flag.
    pub(crate) spilling: AtomicU8,
}

/// "This edge's target has no baked x87 entry TOP." Deliberately OUT of the legal 0..=7 range:
/// TOP 0 is the value a guest sits at after `FINIT`, so a zero sentinel would make a forgotten
/// publisher fail SILENTLY into a wrong-TOP entry rather than bailing.
pub(crate) const NO_ENTRY_TOP: u8 = 0xFF;

impl LinkCell {
    /// A fresh cell defaults to Direct's own zero-body sentinel portal (`zero_portal`); see that
    /// function's doc comment for why this default is Direct-mechanism, not a shared invariant.
    pub(crate) fn new() -> Self {
        Self {
            portal: AtomicUsize::new(zero_portal().address()),
            target_eip: AtomicU32::new(0),
            #[cfg(feature = "direct-link-refusal-census")]
            direct_link_refusal_census_id: AtomicU32::new(0),
            entry_top: AtomicU8::new(NO_ENTRY_TOP),
            spilling: AtomicU8::new(0),
        }
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) fn set_direct_link_refusal_census_id(&self, id: u32) {
        self.direct_link_refusal_census_id
            .store(id, Ordering::Release);
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) fn direct_link_refusal_census_id(&self) -> u32 {
        self.direct_link_refusal_census_id.load(Ordering::Acquire)
    }

    pub(crate) fn address(&self) -> usize {
        std::ptr::from_ref(self) as usize
    }

    /// Direct's own unlink: repoint this cell at the zero-body sentinel portal (`zero_portal`)
    /// and drop the spilling flag back to its default. Every path that rebinds or discards a
    /// cell's target goes through this (see `unlink_outbound` in `direct.rs`), so this is the
    /// one place the flag needs resetting; a caller that only ever turns it on (see
    /// `mark_spilling`) relies on that.
    pub(crate) fn clear(&self) {
        self.portal
            .store(zero_portal().address(), Ordering::Release);
        self.entry_top.store(NO_ENTRY_TOP, Ordering::Release);
        self.spilling.store(0, Ordering::Release);
    }

    /// Record the target's baked x87 entry TOP for the shared re-entry pad's runtime guard. Like
    /// `mark_spilling`, this is never called to un-set: a cell that should carry no TOP gets
    /// there through `clear`, so a caller cannot forget to reset it by skipping a call here.
    pub(crate) fn set_entry_top(&self, top: u8) {
        debug_assert!(top < 8);
        self.entry_top.store(top, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn entry_top(&self) -> u8 {
        self.entry_top.load(Ordering::Acquire)
    }

    /// Mark this slot's edge as needing the x87 boundary spill. Never called with `false`: a
    /// cell that should NOT spill gets there through `clear` (a fresh link starts unmarked), not
    /// through this method, so a caller cannot forget to turn the flag back off by skipping a
    /// call here.
    pub(crate) fn mark_spilling(&self) {
        self.spilling.store(1, Ordering::Release);
    }

    /// Emitted code reads the flag directly with a raw byte test at the link site (see
    /// `emit_completed_path`); this accessor only exists for tests to assert on the flag from
    /// the Rust side, so it is test-only.
    #[cfg(test)]
    pub(crate) fn is_spilling(&self) -> bool {
        self.spilling.load(Ordering::Acquire) != 0
    }

    pub(crate) fn set(&self, portal: &BlockPortal) {
        self.portal.store(portal.address(), Ordering::Release);
    }

    pub(crate) fn set_dynamic(&self, target_eip: u32, portal: &BlockPortal) {
        self.target_eip.store(target_eip, Ordering::Relaxed);
        self.set(portal);
    }

    /// Direct's own linked-ness predicate: compares the loaded portal address against
    /// `zero_portal`'s address, then defers to `BlockPortal::visible` (also a Direct-mechanism
    /// zero-body check). A different backend's linked-ness predicate should compare against its
    /// own sentinel portal's address instead (see the module doc comment).
    pub(crate) fn linked(&self) -> bool {
        let portal = self.portal.load(Ordering::Acquire);
        if portal == zero_portal().address() {
            return false;
        }
        // A live cache owns every published portal in stable Arc storage. BlockCache::drop clears
        // every cell to the permanent sentinel before releasing that storage.
        unsafe { &*(portal as *const BlockPortal) }.visible()
    }

    /// Whether this cell is CLEARED: its portal is the shared zero sentinel, which is where `new`
    /// starts it and where `clear` returns it.
    ///
    /// **This is not `linked()` and it must never be confused with it.** `linked()` is a
    /// VISIBILITY predicate: it defers to `BlockPortal::visible`, which a decode-slot suspension
    /// or an arena compaction flips without touching the cell or the link graph, so it reads
    /// false for an edge that is still logically there and will become usable again with no
    /// merge. This one reads only the cell's own state, which is what an ordering argument about
    /// the unlink paths needs.
    pub(crate) fn is_cleared(&self) -> bool {
        self.portal.load(Ordering::Acquire) == zero_portal().address()
    }
}

/// One inbound reference: the source unit and which of its (at most two) outbound slots points
/// at the target this `LinkSource` is recorded against. Generic over `Id` so each backend can
/// instantiate this with its own unit-identity type (Direct: `BlockId`, staying in `direct.rs`
/// since it carries Direct's generational-slot semantics and is not itself mechanism-neutral).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LinkSource<Id> {
    pub(crate) block: Id,
    pub(crate) slot: u8,
}
