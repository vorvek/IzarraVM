// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn texture_write(offset: usize) -> QueuedCommand {
    QueuedCommand::TextureWrite(QueuedTextureWrite {
        tmu: 0,
        offset,
        bytes: [0; 4],
    })
}

/// S2 of `dev_docs/2026-09-05-distira-async-slice1-review.md`: the OLD
/// `recycle` only adopted a returned batch allocation when `pending` was
/// ITSELF empty. Under async, the swap-flushed batch is joined at a call
/// where the guest has ALREADY queued the next frame's first entries --
/// `pending` is non-empty at exactly the moment the lever cares about --
/// so that guard silently dropped the allocation on the floor every
/// time, reintroducing the per-frame allocation #840's nit D removed.
/// This reproduces that shape directly against `RasterQueue` alone (no
/// `Distira` needed): recycle a big allocation while `pending` holds an
/// unrelated, already-queued entry, then drain and recycle THAT small
/// batch too, and confirm the next push ends up backed by the big
/// allocation -- not a fresh, zero-capacity one.
#[test]
fn recycle_keeps_the_allocation_even_when_pending_already_holds_the_next_batch() {
    let mut queue = RasterQueue::default();

    let mut big_batch = Vec::with_capacity(64);
    for offset in 0..64 {
        big_batch.push(texture_write(offset));
    }
    let big_capacity = big_batch.capacity();
    assert!(big_capacity >= 64);

    // The guest has already queued the next frame's first entry by the
    // time this recycle runs.
    assert!(queue.push(texture_write(1000)));
    assert_eq!(queue.len(), 1);

    // `pending` is non-empty, so the OLD guard would drop `big_batch`'s
    // allocation right here.
    queue.recycle(big_batch);

    let drained = queue.take();
    assert_eq!(drained.len(), 1);
    queue.recycle(drained);

    assert!(queue.push(texture_write(2000)));
    assert_eq!(
        queue.pending_capacity(),
        big_capacity,
        "the big allocation recycled while `pending` was non-empty must \
         still be the one backing `pending` after it next empties and \
         grows, not a fresh zero-capacity Vec"
    );
}
