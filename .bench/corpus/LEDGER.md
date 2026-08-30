<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Corpus campaign ledger

One row per profiled game, in corpus order. Status words: `PROFILED` (reached
gameplay, evidence recorded), `SKIP-TANDY`, `SKIP-NONGAME` (utilities, demos,
data-only folders), `BLOCKED` (could not reach gameplay; reason in notes),
`FLAGGED` (evidence shows a lever worth performance work; details below the
table).

rt is informational triage, taken under uncontrolled host load. Deterministic
counters (coverage, pit writes, instructions) are trustworthy. Every run's
binary sha is in its `run-meta.json`.

Targets (owner, 2026-08-30): 486 should reach rt 5.0 or more, 586 should
reach rt 2.0. A row below target is FLAGGED with census evidence below.

| # | game | slug | persona | status | rt | coverage | notes |
|---|------|------|---------|--------|----|----------|-------|
| 1 | $100,000 Pyramid | 100-000-pyramid | 486 | FLAGGED | 1.18 | 0.694 | CGA quiz; below the 486 target; IRET-dominated, see findings |
| 2 | ''21'' For 1 to 4 | 21-for-1-to-4 | 486 | FLAGGED | 1.50 | 0.778 | QuickBASIC blackjack; below the 486 target; INT/IRET-dominated, see findings |
| 3 | +K Cheiw | k-cheiw | 486 | PROFILED | 4.92 | 0.967 | Spanish VGA platformer; at target; census total_unbound only 60K; game-port probe on open bus 0x216-0x21e |
| 4 | 007 - License to Kill | 007-license-to-kill | 486 | BLOCKED | 0.61 | 0.073 | FiRM cracktro only; BOND.COM has no path into the game. An agent-reported JIT-vs-interpreter divergence did NOT reproduce: a controlled 3-arm A/B (arm on / arm off / --interpreter, equal key schedule) produced bit-identical end frames, sha ad1bd224 |
| 5 | 1 Ton | 1-ton | 486 | PROFILED | 17.91 | 0.787 | Mouse-only arcade; needs TOKAMOUS (-Mouse); real weight-drag interaction in window; mostly idle (~1M insns/guest-s) |
| 6 | 1000 Miglia | 1000-miglia | 486 | FLAGGED | 0.60 | 0.382 | Simulmondo racer; live driving 213-350s behind code-wheel; PIT latch-poll STORM 1.28M writes/guest-s + IRET domination, see F2 |

## Findings for the performance campaign

### F1: interrupt-boundary domination in slow real-mode games (2026-08-30)

Both games below the 486 target are dominated by software-interrupt
boundaries, not by missing instruction coverage. Census runs
(`-BarrierCensus`, plain release, results directories
`100-000-pyramid/20260830-152108-census` and
`21-for-1-to-4/20260830-152243-census`), top rows by
`dynamic_unbound_exits` over 120 guest seconds:

$100,000 Pyramid (total 26.4M):
- `IRET` (0xCF, dword) 26,374,252 from ONE static site - 99.97% of the total
- next rows are 2,240 and below (LES/MOV-seg/JCXZ)

''21'' For 1 to 4 (total 43.8M):
- `IRET` (0xCF, dword) 23,342,857, again one static site
- `INT imm8` (0xCD) 11,927,958 across 1,493 sites
- `MOVSW` (0xA5, word) 5,194,072
- `LOOP` (0xE2) 2,244,015
- `POPF` (0x9D) 717,993

Context: `timer.irq0_edges` is only ~2,185 per run in both games, so these
are NOT hardware interrupts. The games sit in DOS/BIOS polling loops
(keyboard idle via INT 16h is the usual suspect) at roughly 100-220K
interrupt round trips per guest second. Every round trip crosses the JIT
boundary twice (INT in, IRET out). `perf.brk_cont_not_continuable` agrees:
89M (pyramid) and 53M (21) against 1.4M for the at-target platformer.

The lever shape: a native or fast-path INT/IRET round trip for real-mode
software interrupts, or a poll-coalescing treatment like the existing
call-out program. The dword-IRET single-site row looks like common
infrastructure (the same address shape appears in both games) and may be
one shared handler worth attribution.

+K Cheiw (at target, coverage 0.967) has census total 59,807 - three
orders of magnitude lower - which corroborates the diagnosis.

**Attribution (2026-08-30, control run):** the dword IRET is the TOKAEMM
V86 monitor. A rerun of Pyramid with TOKAEMM removed from CONFIG.SYS
(`-NoEmm`, results `100-000-pyramid/20260830-152718-census-noemm`) drops
the census total from 26,374,252 to 7,223 and the dword IRET row to zero.
So the cost is the ring-0 IRETD that reflects each software interrupt back
into V86 - the same family as the wolf3d V86 RETF far-transfer lever.
Games that poll DOS/BIOS from V86 pay it 100-220K times per guest second.

### F2: 1000 Miglia - the corpus's first PIT latch-poll storm (2026-08-30)

Deep census run `1000-miglia/20260830-200959-deepcensus` (350 guest
seconds, ~137 of them live driving, 486, mode 13h):

- rt 0.598 against the 5.0 target; coverage 0.382.
- `timer.pit_writes` 448,720,210 = **1,282,058 per guest second**, 4.3x
  the storm threshold. `irq0_edges` 179,747 (~514/s - the game runs a
  fast timer).
- Census total 8.60M unbound exits; the dword IRET again dominates
  (8,250,532 across 4 sites - the TOKAEMM V86 reflection path of F1).
- `brk_cont_not_continuable` 492M over 6.16B instructions - 8%.

So the game hammers the PIT latch while running a 514 Hz timer, and every
resulting service crosses the V86 reflection boundary. This is the exact
workload class the ISA I/O wait-state slice reprices; re-run this row
first when the flip lands.

## Write-backs from the performance campaign

This file is shared through the `.bench` junction. Performance sessions:
append your rows here directly (what you changed, which corpus rows you
re-ran, PR number). The corpus session commits this file on its branch.

(none yet)
