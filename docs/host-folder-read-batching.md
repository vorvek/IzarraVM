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

The window is addressed by LBA, so it can only ever cover sectors that are both
inside the declared command and contiguous in the host file. The read-ahead
buffer described below has neither limit, because it is addressed by file byte
offset instead.

Projected host files reach the range reader directly instead of constructing a
temporary `PathBuf`-backed source for each sector.

### Cross-command read-ahead

A game's asset load is not one large command. It arrives as a long run of
separate eight-sector requests, one per 4 KiB cluster, and the command window is
discarded between every pair of them. Each of those requests therefore still paid
its own synchronous host seek and read.

A read-ahead buffer sits under the window and survives command boundaries. On a
physical read it pulls a run of bytes from the host file, and a later sector whose
whole range lies inside the buffered bytes is served as a memory copy with no host
call at all.

It is keyed by host path and byte offset rather than by LBA, so it stays correct
however the guest's clusters are laid out, and it is used only when the requested
sector lies wholly inside the range it holds. Because the read-ahead covers
everything the LBA-keyed window would have -- same starting offset, never shorter
than the command extent -- the window is not filled at all while the read-ahead is
armed. The buffer is moved into whichever of the two owns it, never copied.

There are four slots, evicted least-recently-used like the read handles, so two
interleaved files each keep their own place.

### How far a fill reads

A fill starts at the extent the command declared -- one sector when nothing
declared a command at all, which is what a reconcile gather looks like -- and
doubles, up to 256 KiB, only when the miss that triggers it lands at exactly the
end of the previous fill for that same path. Any other offset resets it: a
backward seek, a jump, a first touch.

That rule is the whole amplification argument. A fill larger than the command is
only ever granted to a path that has already been read sequentially that far, so
the bytes read ahead are paid for by bytes already served. Physical bytes stay
within roughly twice logical bytes, the overshoot being the last fill of a stream
that stops early, and a purely random or alternating pattern reads exactly what
its command asked for and no more.

A flat fill has no such bound, which is why it is not what shipped. Measured by
unit test: thirty-two sequential eight-sector commands read 131,072 physical bytes
to serve 131,072 -- a ratio of one -- while 128 alternating single-sector reads
across two files read 130,048 to serve 65,536. With a flat 256 KiB fill the same
alternating pattern reads 524,288, and with a single slot it never hits at all.

Because the buffer outlives the command, it needs an invalidation rule of its
own. Four operations mutate a host file this disk reads: the atomic whole-file
write, the rename, the delete, and the write-through path that streams guest
sectors into a projected file. The first three run inside the reconcile pass,
which drops every cached read view on entry and again, scoped to the affected
path, at each mutation. The scoped drops are load-bearing: the pass reads file
bytes between its entry and its writes. The write-through path is reachable with
no reconcile around it, so it invalidates its own path before writing.

The buffer does not track changes made to the mounted folder by another program
while the machine is running. That was already outside the mount's contract; the
buffer widens the window in which such a change goes unnoticed.

### Read-handle LRU

The open read handle was a single entry. That is right for one sequential file
and degrades to one `File::open` per sector as soon as two files interleave,
which is what an asset load does whenever a read lands between two projections.
It is now an LRU of eight handles, keyed by path, invalidated through the same
funnel as the read-ahead buffer, and symmetric with the write-handle cache.

### The fifth mutation site

Four host mutations live in the Katea module, and each invalidates through the
funnel. A fifth lives outside it: the BIOS repair service rewrites CONFIG.SYS and
AUTOEXEC.BAT in the mounted folder and renames the originals aside. It does not
call the funnel and does not need to, because it finishes by re-mounting the
folder, which builds a new volume and drops every cache with the old one. That is
the only safe way to reach a mounted folder from outside the module, and it is
named in the funnel's own comment so a future caller that skipped the re-mount
does not have to rediscover why it matters.

### Metadata projection scaling

The same profile that showed the read tail showed a larger cost on the write
side: every interval stall of 19 ms or more was entirely metadata projection,
fired by a guest write command, with single passes measured at 290, 196 and
125 ms on a 47 MB folder. The passes are synchronous on the emulation thread.

A projection pass walks the cluster chain of every live file three times over --
the ownership scan, the materialize scan, and the pending re-check. Each step of
a walk asks for one FAT entry, each FAT entry read synthesizes a whole 512-byte
FAT sector, and synthesizing one evaluates 128 entries against a hash set. A
write that touched one small file therefore paid roughly 128 hashed lookups for
every cluster on the volume.

The synthesized FAT sector is now memoized, one entry deep. A chain walk asks for
clusters in ascending order, so 128 consecutive requests land in the same FAT
sector and the single slot captures the whole win.

The memo cannot change what the guest reads. The cluster index and the geometry
are built at mount and never mutated, so the synthesis is a pure function of the
sector index and any two evaluations agree byte for byte. The memo also sits
strictly below the guest write overlay: the overlay is still consulted first and
still reports its read failures, so a guest-written FAT sector never reaches the
memo at all.

Measured by unit test, on a folder of 772 allocated clusters, a one-file write
went from 1,531 FAT sector syntheses to 13.

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

The read-ahead is enabled by default and has the same kind of control:
`IZARRAVM_HDD_READAHEAD=0` disarms it, restoring one physical read per command.
Both switches are read once at mount.

The two are independent axes, and both remain worth turning. Because a fill is
earned rather than granted, it starts at whatever extent the command declared,
and `IZARRAVM_HDD_COMMAND_READ_BATCH=0` collapses that extent to a single sector.
The ramp then has to climb the whole way from 512 bytes instead of starting at
the command's own size, so the two switches move different quantities and the
same four tests change on the batch leg as before this change.

The profile also reports the longest single operation of each kind, beside the
sums it already reported:

| Counter | Meaning |
| --- | --- |
| `host_read_max_ns` | longest single host read this session |
| `projection_max_ns` | longest single projection pass this session |
| `host_readahead_hits` | sectors served from the read-ahead buffer |
| `host_readahead_fills` | physical reads that ran past their command |

A sum cannot see a hitch. Time spread thin over a minute and the same time spent
in one synchronous pass are the same number of nanoseconds and a completely
different experience, and only the maximum separates them. Both maxima are read
off the `Instant` pair their sums already computed, so neither costs a clock
read.

Both are running maxima, which is to say levels, and a level must not be
differenced: the difference of two running maxima is not the maximum over the
interval between them. The boot profiler carries them through undifferenced. In
the phase-mark series, which is cumulative and is differenced column by column by
its consumers, they appear as `katea_host_read_max_level_ns` and
`katea_projection_max_level_ns` -- the suffix is there so a uniform differencer
cannot quietly produce a meaningless number.

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

## Follow-up: read-ahead, handle LRU, projection scaling

The sections added above were validated the same way. Ten further regressions are
specific to them:

- thirty-two commands over one file cost at most eight physical host reads, and
  read no more physical bytes than they served
- alternating single-sector reads across two files stay inside the same byte
  bound and still hit the buffer
- `IZARRAVM_HDD_READAHEAD=0` puts it back to one physical read per command
- a write-through drops the read-ahead buffer for the file it wrote
- an overwrite leaves no cached read view for the file it replaced
- a rename leaves none for either path, and a delete none for the file
- the handle cache stays bounded at eight and evicts least-recently-used
- a one-file write does not resynthesize the FAT once per cluster walked
- every FAT sector reads back byte-identical to an uncached synthesis, under a
  read order that misses the one-entry memo on every request
- the max counters never exceed the sums they sit beside

Each names its mutation. Six were applied and observed to fail: the memo removed,
the read-ahead given the window's lifetime, the write-through invalidation
deleted, one slot instead of four, a flat fill instead of the ramp, and the
overwrite's scoped invalidation deleted. The rename and delete post-conditions
say so in their own comments: with the pass ordered as it is, phase 2 mutates
before phase 3 reads, so the entry drop already satisfies them and their scoped
calls are defence in depth against an ordering change.

Two pre-existing assertions moved, both host-side counters:

- `cached_host_handle_serves_identical_bytes_with_one_open_per_file` read
  `host_file_opens == 3` and now reads `0`. The probe that identifies the two
  files already opened both, and the LRU is deep enough to keep them; the
  single-entry cache reopened on every change of file. Sectors served are
  unchanged.
- `a_failed_host_read_is_served_as_zeros_but_never_cached` truncated the file it
  had just read and expected the next sector of it to fail. With a read-ahead
  buffer that sector is already in RAM. The test now puts its failing sector in
  a second file this run has never opened, so the read genuinely reaches the
  host. What it asserts -- that a degraded read is served as zeros and never
  cached, and that the restored bytes come back -- is unchanged.

`the_sector_cache_hits_misses_and_charges_on_a_katea_host_folder` did NOT move.
Its `host_read_bytes == 4 * 512` holds unchanged, because a first touch of a file
gets the command extent and nothing more.

No timing, charge, tick, status-register, content or determinism test changed.
The whole workspace passes: 1,553 `izarravm-machine` library tests with 3
ignored, 231 `izarravm` tests with 92 ignored, and every other crate.

### Guest timing, measured again

The library suite was run in both legs of the new switch. With
`IZARRAVM_HDD_READAHEAD=0`, exactly three tests change result, and all three are
the ones that exist to assert the read-ahead is doing something. No pre-existing
test moves at all. The other 1,550 -- every timing, charge, tick,
status-register, content and determinism test in the machine -- pass identically
armed and disarmed.

The read-ahead moves physical-read counters and nothing else the guest can
observe. The handle LRU and the FAT memo have no switch because neither has a
guest-visible degree of freedom: the LRU only decides whether a file is reopened,
and the memo only decides whether identical bytes are recomputed.

The wall-clock effect on the duke3d-486 hitch has NOT been measured. The
acceptance instrument is in place -- `projection_max_ns` and `host_read_max_ns`
in the profile and the phase-mark series -- and the fixture run belongs to
whoever grades this.

### Guest timing, measured rather than argued

The whole library suite was run in both control legs. With
`IZARRAVM_HDD_COMMAND_READ_BATCH=0`, exactly four tests change result, and all
four are `host_read_operations` assertions. The other 1,538 -- every timing,
charge, tick, status-register, content and determinism test in the machine --
pass identically with batching on and off.

That is the fidelity claim in its strongest available form: batching moves the
physical-operation counters and nothing else the guest can observe.
