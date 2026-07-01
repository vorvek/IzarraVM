# GSW-586 86Box-Backed Speed Plan

## Status

Status: implemented, verified, and staged.

Branch: `codex/gsw586-86box-speedups`.

Scope guard: no native codegen, JIT, or dynarec was added.

## Summary

Implement the non-JIT 86Box-style speedups already identified: profile first,
add direct RAM data paths, specialize REP string operations, broaden lazy flags,
and only simplify cache timing if profiling says `CacheModel::data_tier` is hot.

## Implementation Tracker

- [x] Create branch `codex/gsw586-86box-speedups`.
- [x] Add this doc before code changes so the work can be resumed from the repo.
- [x] Add diagnostics counters for direct/slow data reads and writes, REP string
  iterations vs fast iterations, flag materializations, and cache tier lookups.
- [x] Print the new counters from `--headless-bench`.
- [x] Add `CpuBus` direct-RAM helpers for one-shot aligned data reads/writes and
  page-local string chunks. Default implementations fall back to existing
  `read_memory`/`write_memory`.
- [x] Implement `MachineBus` direct helpers using the existing page lookup.
- [x] Route ordinary CPU data operands through the direct helpers after the
  existing segmentation, paging, alignment, A20, and device-window checks.
- [x] Add page-local REP fast handling for RAM-backed `MOVS`, `STOS`, `LODS`,
  `CMPS`, and `SCAS`. IO string ops stay on the existing per-element path.
- [x] Preserve DF, CX/ECX, segment overrides, ZF termination, faults, and
  per-access bus timing in the REP paths.
- [x] Broaden lazy flags with the existing `LazyFlags` machinery for logical ops
  and carry-preserving `INC`/`DEC`.
- [x] Profile cache tier lookup cost. The new counters showed millions of cache
  lookups per bench run, so the hot-path repeated geometry/index work was
  replaced with cached per-level geometry masks.
- [x] Keep current guest timing and tier behavior.
- [x] Compare with baseline and keep only correctness-neutral changes that help a
  target path or profile without materially regressing synthetic benches.
- [x] Add an arbitrary-EXE `rt_factor` harness:
  `--headless-bench-exe <path>` runs the supplied DOS EXE through the existing
  raw-program benchmark path in GSW-586.
- [x] Build the host release target and run the real release EXE benchmark:
  `.\target\release\izarravm.exe --headless-bench`. GSW-586 `rt_factor`
  results are recorded below.

## Baseline

Captured before code changes:

| Check | 586 baseline |
| --- | --- |
| `--headless-bench` sieve `rt_factor` | 0.252 |
| `--headless-bench` fp-mandel `rt_factor` | 0.443 |
| `--headless-bench` dhrystone `rt_factor` | 0.257 |
| `--headless-bench` whetstone `rt_factor` | 0.354 |
| `ram_lookup_profile` bus read low RAM | 4.25 ns/op |
| `ram_lookup_profile` bus read extended RAM | 6.87 ns/op |
| `ram_lookup_profile` bus read ROM | 7.14 ns/op |

The baseline bandwidth sweep was in band for all modes and blocks.

## Final Measurements

Latest direct release-EXE sample after implementation:

| Check | 586 final |
| --- | --- |
| `.\target\release\izarravm.exe --headless-bench` sieve `rt_factor` | 0.270 |
| `.\target\release\izarravm.exe --headless-bench` fp-mandel `rt_factor` | 0.434 |
| `.\target\release\izarravm.exe --headless-bench` dhrystone `rt_factor` | 0.274 |
| `.\target\release\izarravm.exe --headless-bench` whetstone `rt_factor` | 0.364 |
| `ram_lookup_profile` bus read low RAM | 4.03 ns/op |
| `ram_lookup_profile` bus read extended RAM | 6.36 ns/op |
| `ram_lookup_profile` bus read ROM | 6.86 ns/op |

The final bandwidth sweep stayed unchanged and in band for all modes and blocks.

The arbitrary-EXE harness was smoke-tested with the checked-in Dhrystone EXE:

| Command | Result |
| --- | --- |
| `cargo run --release -p izarravm -- --headless-bench-exe crates/izarravm-firmware/roms/neurketa-c/dhrystone.exe` | `dhrystone.exe 586 ... rt_factor 0.270` |

`--headless-bench` now also prints diagnostic counters. In the final sample:

| Bench | Direct reads | Slow reads | Direct writes | Slow writes | REP fast/all | Flag materializations | Cache lookups |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| sieve 586 | 327608 | 0 | 927448 | 0 | 327600/327600 | 0 | 1255058 |
| fp-mandel 586 | 71531 | 332984 | 1575 | 138280 | 0/0 | 33968 | 1958164 |
| dhrystone 586 | 1810215 | 0 | 1280090 | 0 | 200000/200000 | 190000 | 3090305 |
| whetstone 586 | 2356862 | 163200 | 1228199 | 221100 | 0/0 | 83300 | 5122261 |

Wall-time `rt_factor` samples are noisy on this host. Guest cycle counts and
band verdicts stayed stable.

## Follow-Up: Page-Pointer Lookup + Fetch Cache

Implemented the missing 86Box-style page-pointer lane without adding a dynarec:

- `CpuBus::direct_page` exposes full RAM pages as raw pointers, with ROM, VGA,
  EMS, Distira windows, A20-folded accesses, unmapped pages, and partial final
  pages rejected.
- `CpuBus::charge_direct_memory` preserves the existing wait-state/cache timing
  accounting for direct pointer accesses.
- `MachineBus` backs direct pages from `Memory::as_mut_ptr()` and marks the CPU
  direct/fetch/code caches dirty when the Distira PCI decode rebuilds the RAM
  lookup.
- `Cpu386` now has read/write direct-page caches for aligned page-local
  byte/word/dword data operands and a pccache-style instruction fetch page cache.
- Self-modifying code, A20 changes, paging/TLB/code-cache invalidations, device
  memory writes, and direct-map changes invalidate the relevant direct caches.
- `--headless-bench` prints direct page lookup, pointer read/write, fetch page,
  slow prefetch refill, and direct-map invalidation counters.

Latest direct release samples for this follow-up:

| Check | 586 sample |
| --- | --- |
| `.\target\release\izarravm.exe --headless-bench` sieve `rt_factor` | 0.259 |
| `.\target\release\izarravm.exe --headless-bench` fp-mandel `rt_factor` | 0.405 |
| `.\target\release\izarravm.exe --headless-bench` dhrystone `rt_factor` | 0.265 |
| `.\target\release\izarravm.exe --headless-bench` whetstone `rt_factor` | 0.355 |
| `.\target\release\izarravm.exe --headless-bench-exe ...\dhrystone.exe` | 0.255 |
| `.\target\release\izarravm.exe --headless-bench-exe ...\whetstone.exe` | 0.329 |
| `ram_lookup_profile` bus read low RAM | 4.21 ns/op |
| `ram_lookup_profile` bus read extended RAM | 6.83 ns/op |
| `ram_lookup_profile` bus read ROM | 7.93 ns/op |

Bandwidth remained in band for every mode/block. The page-pointer counters show
that the fast lane is active, but the current low-memory synthetic/EXE proxies
are neutral/noisy rather than a clear win over the previous direct-helper path.
That is expected for conventional DOS RAM because the previous `MachineBus`
direct helper was already very cheap there; the new path mainly removes repeated
page-map lookup once a CPU page cache entry is warm.

Representative 586 counters from `--headless-bench` after this follow-up:

| Bench | Ptr reads | Ptr writes | Page lookups h/m | Fetch page h/m | Slow refills |
| --- | ---: | ---: | ---: | ---: | ---: |
| sieve | 327608 | 599848 | 11/1 | 285/5 | 1 |
| fp-mandel | 71531 | 1575 | 9/1 | 408/5 | 1 |
| dhrystone | 1610215 | 1080090 | 7/0 | 1431/1 | 0 |
| whetstone | 2356862 | 1228199 | 5/0 | 2307/1 | 0 |

## Verification

- [x] `cargo test -p izarravm-bus --lib`
- [x] `cargo test -p izarravm-cpu --lib`
- [x] `cargo test -p izarravm-machine --lib`
- [x] `cargo test --workspace`
- [x] `cargo build --release -p izarravm`
- [x] `cargo run --release -p izarravm -- --headless-bench`
- [x] `.\target\release\izarravm.exe --headless-bench`
- [x] `cargo run --release -p izarravm -- --headless-bench-exe crates/izarravm-firmware/roms/neurketa-c/dhrystone.exe`
- [x] `cargo run --release -p izarravm -- --headless-bandwidth`
- [x] `cargo test --release -p izarravm-machine ram_lookup_profile -- --ignored --nocapture`
- [x] `cargo fmt --check`
- [x] `git diff --check`

Focused coverage added:

- [x] Direct RAM helpers fall back for ROM, VGA, EMS, split-page/split-width, and
  A20-folded accesses.
- [x] Direct pages fall back for ROM, VGA, EMS, Distira BAR overlap, A20-folded
  accesses, unmapped/non-RAM memory, and partial final pages.
- [x] Direct pointer data and fetch caches are exercised by an in-RAM raw-program
  test.
- [x] Distira BAR relocation invalidates the CPU direct/fetch caches before the
  next guest batch.
- [x] REP fast paths cover forward `MOVS`, `STOS`, `LODS`, `CMPS`, and `SCAS`;
  DF-set backwards `MOVS` stays on the slow path and is covered.
- [x] Lazy logical and `INC`/`DEC` flag behavior is checked against
  architectural flag reads.

## Files

- `crates/izarravm-bus/src/lib.rs`
- `crates/izarravm-cpu/src/lib.rs`
- `crates/izarravm-machine/src/lib.rs`
- `crates/izarravm/src/main.rs`
- `dev_docs/2026-07-01-gsw586-86box-speed-plan.md`

## Release Target Note

The release EXE is `target\release\izarravm.exe`, built with
`cargo build --release -p izarravm`. It has been run directly with
`--headless-bench`; the GSW-586 `rt_factor` values are recorded above.

`--headless-bench-exe <path>` remains available for any future supplied DOS EXE
workload that needs the same `rt_factor` table.

## Assumptions

- "All these" means the non-JIT 86Box-backed items: direct RAM operands,
  REP/string specialization, broader lazy flags, and cache-model hot-path
  cleanup.
- Native codegen/dynarec remains out of scope.
- Guest timing and architectural behavior must not change except where an
  existing test already encodes the intended timing.
