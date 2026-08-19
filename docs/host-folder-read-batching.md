<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Host-folder read batching

## Problem

Games that stream assets from a Katea host-folder disk could pause briefly when
they first loaded a sound or graphic. Duke Nukem 3D made the pause easy to see:
the GUI disk light turned on at the same time, even though gameplay was otherwise
smooth under the 486 persona.

The existing sector cache helps repeated reads, but a cold asset load is mostly
cache misses. Before this change, each missed 512-byte sector reached the host
file separately. A multi-sector BIOS request therefore repeated the FAT lookup,
host read call, and guest-buffer delivery work for every sector.

Profiles separated two kinds of time:

- Guest time includes the emulated disk delay charged to the DOS program.
- Wall time includes the real work IzarraVM performs while servicing the read.

The Duke load bursts changed in wall time while their guest time, instruction
count, cache results, and modeled disk stalls remained stable. The hitch was
therefore host-side service overhead, not a guest timing defect.

## Reference implementations

86Box keeps an open image handle, performs multi-sector reads with one seek and
one host read, bounds IDE read-ahead to a small window, and completes ATA work on
scheduled controller events. Its flat image also gives it contiguous storage
that a live host folder does not have.

DOSBox-X takes a different shortcut for local drives: DOS file operations can
reach host files directly instead of walking a synthesized FAT disk through BIOS
sectors. That is not compatible with Katea's disk-level contract.

The useful common principle is to amortize host work across the sectors the guest
already requested. This change does not adopt image-only contiguity, bypass the
Katea filesystem, or alter guest-visible controller timing.

## Design

### Command-local host range reads

A read path declares the starting LBA and sector count of the run it is about to
walk to `AtaDisk`. Host-folder disks use that boundary to read contiguous bytes
from one host file in a single operation, capped at 64 sectors. The cap matches
the useful scale of 86Box's default IDE window without reading beyond the active
guest request or the file's contiguous allocation.

The extra bytes live only until the command ends. They are discarded before the
next one, so an unrequested sector cannot stay stale if a host file is changed
between two guest reads. This is why a declared range must open and close inside
a single host-side call, and why the one path that cannot do that is excluded
below. The guest overlay is still checked first, and the ordinary sector cache
still observes each requested sector as the guest asks for it.

The existing one-file open-handle cache remains in place. Projected host files
now reach the range reader directly instead of constructing a temporary
`PathBuf`-backed source for each sector.

### Which callers declare a range

Every read path that walks a contiguous run inside a single host-side call
declares that run:

| Caller | Path |
| --- | --- |
| `AH=02` / `AH=0A` | INT 13h CHS read, and long-sector read |
| `AH=04` | INT 13h CHS verify |
| `AH=42` / `AH=44` | EDD read, and EDD verify |
| `read_dma_payload` | bus-master DMA, device to memory |

Native PIO reads (`prepare_read_sector`) are deliberately left unbatched. A PIO
command serves one sector per DRQ and the guest drains each through the data
port before the next is scheduled, so a window would have to survive an interval
bounded by guest execution rather than by a host-side call. That breaks the
property the whole design rests on -- a host folder cannot change underneath a
declared range -- and the command has no single exit to close the range at: it
ends in `read_data_byte`, in `abort`, in `soft_reset`, or nowhere at all if the
guest simply stops draining. Lifting this means giving a PIO command one owned
lifetime, which is a separate change.

DMA and PIO both schedule their deadlines at command time, before any sector is
looked up, and price with the uncached formula. Their guest-visible timing is
therefore structurally independent of how the host performs the reads.

### Batched guest-buffer delivery

INT 13h CHS reads assemble the command payload and walk the guest page tables by
page when copying it to the caller. EDD reads do the same in bounded 64-sector
chunks, avoiding an allocation proportional to the maximum DAP count. Long
sector reads retain their per-sector layout because each 512-byte payload has a
four-byte ECC trailer.

The number of sectors reported and charged remains the number that reached the
caller. Image-backed disks and write paths are unchanged.

### Failure behavior

A vanished or unreadable host file continues to return a degraded zero sector,
which is never inserted into the sector cache. If a live host file shrinks
during a batched command, complete leading sectors are preserved. A partial
following sector is discarded and retried through the existing degraded-read
path instead of being cached as content.

A read that fails part-way is degraded in full. `read_exact` leaves its buffer
unspecified on failure, and it fails only after copying whatever did arrive, so
a sector torn by a truncation would otherwise reach the guest half real and half
zero with no signal distinguishing it from a clean failure. The sector is zeroed
before it is returned.

A sector the guest has written resolves through the guest write store before the
coalesced window is consulted, so a window filled from the host file cannot
serve pre-write bytes for a sector the guest has since changed.

### Measurement controls

The storage profile now reports physical host read operations and bytes in
addition to logical host-file sectors. Command batching is enabled by default;
setting `IZARRAVM_HDD_COMMAND_READ_BATCH=0` disables it for a same-binary A/B
comparison.

## DukeMark results

The measured fixture used the 486 persona, 64 MiB RAM, VEGA video, and a
200-record byte-exact prefix of Atomic Edition's `BENCH2.DMO`. Its SHA-256 was
`23a89ef6e1da1bbc88b5612e2cb29cad22e7297f964d8932c23c2373f24e889d`.
Each leg rebuilt a private writable host-folder copy. The table covers intervals
with host-file reads during guest time 30.000 through 31.000 seconds, which is
the cold asset-loading burst. Values are the mean of two off and two on legs in
an A/B/B/A sequence.

| Metric | Batching off | Batching on | Change |
| --- | ---: | ---: | ---: |
| Physical host reads | 4,561 | 1,563 | -65.7% |
| Time inside host reads | 16.33 ms | 7.38 ms | -54.8% |
| INT 13h handler wall time | 57.53 ms | 41.96 ms | -27.1% |
| Asset-burst wall time | 632.34 ms | 536.09 ms | -15.2% |
| Worst observed asset burst | 755.36 ms | 553.73 ms | -26.7% |

Every comparison leg produced the same guest invariants:

- 37.779333 guest seconds
- 1,352,968,347 instructions
- 8,863 sector-cache hits and 10,695 misses
- 1,801,253,025 modeled disk-stall ticks
- 529.174 ms of guest time in the measured asset intervals

A final current-build run measured the asset intervals at 507.609 ms wall,
including 6.288 ms in physical host reads and 40.323 ms in the INT 13h handler.
It completed DukeMark with the expected info string and reported 11 minimum, 50
maximum, and 35 average FPS.

Host scheduling makes whole-run wall time noisy, so these figures do not claim a
general frame-rate increase. They demonstrate fewer physical operations and a
smaller, more stable cold-load service burst while guest timing stays unchanged.

## Validation

The implementation was checked with:

- 1,542 passing `izarravm-machine` library tests, with 3 ignored
- 231 passing `izarravm` tests, and the 92 ignored guest rows under `--release`
- strict Clippy for all `izarravm-machine` and `izarravm` targets
- formatting, release build, and the file-policy check

Seven regressions are specific to this change. Each names, in its doc comment,
the mutation that makes it fail, and each mutation was applied and observed:

- a coalesced span does not serve host bytes for a sector the guest wrote
- a shrink to a sector boundary keeps its one complete leading sector
- a partial trailing sector in a short batch is dropped, not padded and cached
- a read torn mid-sector returns zeros, not the prefix that survived
- a DMA read coalesces its whole contiguous request into one host read
- a DMA span that fails part-way reports what the per-sector path reported
- a CHS verify coalesces its run the way its EDD twin already did

The pre-existing transient-read, cached-handle, reconciliation, CHS, and EDD
tests were re-run unchanged; this change did not add to them.

### Guest timing, measured rather than argued

The whole library suite was run in both control legs. With
`IZARRAVM_HDD_COMMAND_READ_BATCH=0`, exactly four tests change result, and all
four are `host_read_operations` assertions. The other 1,538 -- every timing,
charge, tick, status-register, content and determinism test in the machine --
pass identically with batching on and off.

That is the fidelity claim in its strongest available form: batching moves the
physical-operation counters and nothing else the guest can observe.
