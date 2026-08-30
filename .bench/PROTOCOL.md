<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# JIT wall A/B protocol

The recipe, the fixtures, the traps (do not re-derive), and the settled
invariants. Carry forward verbatim into future JIT sessions.

## Settled invariants (verified, do not re-litigate)

- CURRENT DOOM IDENTITY (re-pinned 2026-08-03 at `1d560fda`, the FastMap
  aperture-scoping slice, ACCEPTED on a six-pair ladder): 8G/586:
  realtics 826, gametics 2134, instr 4,768,523,775,
  jit_direct_entries 70,851,041,
  direct_native_coverage 0.6049873453760855, decode_probes 1,933,174,593.
  A VGA direct-write-token move used to wipe the WHOLE linear FastMap and both
  direct-page caches. Doom moves that token 3,425,430 times per timedemo — all
  of them the Mode X map mask (port 0x3C5, sequencer index 2, cycling planes
  0-3), MEASURED, not inferred — and the global form destroyed 42,046,095 live
  page entries doing it, **89.30% of them RAM whose host pointers had not
  moved**. The fix scopes the invalidation to physical 0xA0000..0xAFFFF and,
  inseparably, stops advancing the global direct-mapping epoch for that cause;
  either half alone re-imposes the coarse behaviour. Destruction falls 88.71%,
  and the APERTURE half is untouched (4,498,034 vs 4,497,270, 0.017% apart),
  which is the sharpest evidence the scope is right.
  `jit_direct_exit_unavailable_or_kind` 61,268,353 -> 19,500,509 (-68.2%) is the
  largest single effect and it is on the NATIVE side, not the interpreter side:
  41.8M compiled-block side exits were blocks hitting an entry the wipe had just
  deleted. Interpreter miss rate 10.159% -> 7.726%, coverage +0.877pp.
  GUEST TIMING DID NOT MOVE AT ALL: `executed_cpu_core_clocks + scaled_bus_clocks`
  is BIT-IDENTICAL at 7,799,155,653 and master_ticks is identical; the -406 core
  clocks are exactly +406 scaled bus clocks, which is exactly the 406 accesses
  (`data_slow_reads` +161, `data_slow_writes` +245) that moved to the slow path.
  Instructions move -1,156 only because the budget is fixed. Use that ledger
  instead of asserting "formation margin" when an instruction count drifts.
  ACCEPTED on a six-pair ladder vs a build of the instrument commit `448eefa9`:
  geomean 1.0569, median 1.0583, min-wall 1.0576, **lower95 1.0485**, all three
  estimators within 0.0014, 12 of 12 ratios above 1, both roles' absolute walls
  in band (base 104.695-107.489 s, cand 98.989-102.296 s), `contaminated` false.
  That is **+5.69%, IMPROVED class** — the first since the SMC lane campaign.
  QUAKE IS A COUNTER-IDENTICAL NULL CONTROL HERE (it never moves the token), so
  its ladder reading of +1.07% IS this binary pair's layout confound, measured
  rather than assumed. No derived number is claimed from dividing the two.
  Conformance EXACT (pass=663210 vector_mismatch=2). Six mutations, six kills.
  CAVEAT, disclosed: the wall ladder ran on binaries built from the source
  BEFORE a diagnostic-counter correction that `1d560fda` also carries. The
  corrected rebuild is identical to the laddered binary on every behavioural
  field — all 120 perf counters, both timedemo values, all bus and tick totals —
  differing only in the three wipe counters whose definition changed and in
  `jit_direct_compile_ns`. Evidence:
  .bench/results/fastmap-wipe-20260803/README.md.
- HISTORY (the `3a34d5cb` pin, superseded by the block above; Slices 3+3b's
  mechanism still stands): re-pinned 2026-08-02 at `3a34d5cb`, the
  rejected-row campaign's Slices 3+3b, accepted as a PAIR): 8G/586:
  realtics 826, gametics 2134, instr 4,768,523,807,
  jit_direct_entries 115,658,430,
  direct_native_coverage 0.5837080594866788, decode_probes 2,079,452,776.
  doom `instructions` and `executed_cpu_core_clocks` are BYTE-IDENTICAL to
  the Slice 2 pin — verified cross-role across 24 observations, not assumed.
  Slice 3 reached the sixteen-bit MEMORY forms (MOVZX, MOV imm, the 0x83 ALU
  group); Slice 3b built the sixteen-bit register SHIFT lane. They are pinned
  together because Slice 3 alone REGRESSED quake -1.10% by relocating exits
  onto `0xC1` SHL, and 3b is the repair: quake formation recovered 84-97%
  toward its pre-Slice-3 baseline and its wall returned to +0.75% (neutral).
  **NO WALL CLAIM IS MADE FOR EITHER FIXTURE HERE.** doom read +1.56%
  (`positive_but_inconclusive`) but its own mechanism predicts ~+0.06%, and
  the excess was traced to the confound on the BASE side across rounds — a
  wall number that outruns its mechanism by an order of magnitude is not
  banked. Quake's lower95 grazes unity at 0.999953, which is neutrality, not
  a win. Evidence:
  .bench/results/rejected-rows-20260802/paired-3-3b/VERDICT.md.
  CONFOUND CALIBRATION: SUPERSEDED — see the block below, which measured it
  directly and found TWO regimes. The figures once quoted here (~2.4% unequal
  path, ~1.6% path-controlled) understate the dominant one.
  DRIFT SINCE THIS PIN (Slices 4-7 of the rejected-row campaign, none of which
  was accepted on a ladder, so none re-pins): the GUEST-VISIBLE halves are
  intact and are what an identity check must use — doom realtics 826 /
  gametics 2134 and quake 969 frames are EXACT at every slice. The COUNTER
  halves above are stale and must NOT be used as a determinism gate:
  at slice 7 (`c014f8e7`) doom reads instr 4,768,524,931 (+1,124 from Slice 4,
  disclosed and reconciled there), jit_direct_entries 112,489,342,
  direct_native_coverage 0.5962176409, decode_probes 2,016,631,905; quake
  reads instr 3,501,073,158 (-553, reconciled to the unit — core clocks -931
  reappear as +930 scaled bus, budget -1), jit_direct_entries 24,536,481,
  direct_native_coverage 0.8670187885, decode_probes 483,107,376.
  Evidence: .bench/results/rejected-rows-20260802/slice7/README.md §4.
  ABSOLUTE WALLS on the slice-7 binary, recorded per regime (b) below and NOT
  a result: doom 105.337 s, quake 28.237 s, against Slice 6's 112.372 /
  29.775 and Slice 5's 118.345 / 31.217 — both fixtures ~5-6% faster against
  a mechanism predicting 0.05%, i.e. a lucky code-placement draw for one
  build. Do not read it as a trend.

- **THE LAYOUT CONFOUND HAS TWO REGIMES** (measured 2026-08-02 on `abeff4ac`,
  quake 6.2G, EIGHT six-pair ladders / 192 observations, with five
  guest-inert non-elidable perturbations each proved counter-identical on
  183 fields before laddering). Evidence:
  `.bench/results/layout-confound-20260802/README.md`.

  **(a) Same build tree, small source edit: <= 1.2% geomean, <= 2.0%
  min-wall.** Five varied perturbations — dead code at two sites in two
  files, a PURE PERMUTATION of byte-identical `.text`, and a 64 KiB `.rdata`
  displacement — moved quake by -1.24%..+0.11% geomean. Sign is NOT
  predictable from the edit.

  **(b) Independent rebuild of the SAME SOURCE: up to 6%.** One build in
  seven landed ~6% slow. Two builds of `abeff4ac` from byte-identical source
  laddered at geomean **0.9425**, lower95 0.9340, 12/12 ratios,
  uncontaminated. That is the largest and best-supported wall difference this
  campaign has measured, and it corresponds to NO code change whatsoever.
  Build-path length is NOT the cause (the same-length `s6l-cand` build is
  normal); it is an ordinary bad draw of code placement for one build.

  **Operational rules that follow:**
  - Compare binaries built in the SAME session and tree wherever possible.
  - RECORD ABSOLUTE WALLS per binary, and treat any role whose absolute wall
    sits outside the round's normal band as a suspect BUILD before
    interpreting any ratio. This is the only way to catch regime (b) — no
    amount of statistical tightness inside a ladder can, because the ladder
    is faithfully measuring that binary.
  - Rig floor: a null control (byte-identical `.text` and `.data`) reads
    +0.11% geomean, but individual ratios still span +-3%. A single A/B pair
    is worthless.
  - **DOOM'S LADDER IS THE LESS RELIABLE INSTRUMENT.** On a guest-inert
    change it could not produce an uncontaminated six-pair ladder in two
    attempts, tripping the mechanical rule in OPPOSITE directions, with
    individual ratios spanning 10.6 pp against quake's 3 pp. A doom ladder
    that passes on the third attempt is not obviously more trustworthy than
    the two that failed. Treat doom wall numbers as weaker evidence than
    quake ones, and never as load-bearing without an agreeing mechanism.
- HISTORY (the `ca10d4f0` pin, superseded by the Slices 3+3b pair above;
  Slice 2's mechanism and its +2.44% doom result still stand): 8G/586:
  realtics 826, gametics 2134, instr 4,768,523,807,
  jit_direct_entries 117,249,301,
  direct_native_coverage 0.5824073487738802, decode_probes 2,087,246,117.
  Slice 2 lowered the F7 REGISTER group natively — `/5` IMUL, `/6` DIV,
  `/7` IDIV — with no call-out: a call-out frame prices at ~29 ns and these
  run 4.15M times, ~60 ms of pure overhead, against a native guard of one
  not-taken branch. DIV/IDIV faults are kept out of the HOST by guards that
  side-exit BEFORE any architectural effect, so the interpreter re-executes
  and faults by its own rules; the unsigned guard (`cmp edx,ecx / jae`) is
  EXACTLY the host fault set, and the signed path divides at 64 bits so that
  after the 0 and -1 guards no dividend — `i64::MIN` included — can trap.
  `side_exit_divide_guard` reads 0 on both fixtures: neither fixture ever
  divides by zero or overflows a quotient. A CLEAN census deletion, unlike
  Slice 1: the rejected class fell by exactly the three rows' total
  (-4,150,314) with every surviving row byte-identical and no relocation.
  **doom `instructions` byte-identical to the Slice 1 pin**; quake moved
  +336 (formation margin, verified constant across 24 observations).
  ACCEPTED on a six-pair ladder vs `00a7ae4f`: geomean 1.0244, lower95
  1.0190, min-wall 1.0236, estimators within 0.0009, faster in 23 of 24
  observations; quake +0.70% (inside the relayout confound, not
  load-bearing). That is **+2.44%, positive_but_inconclusive class — lower95
  1.0190 is just BELOW the 1.02 IMPROVED bar, do not quote it as IMPROVED**.
  Evidence: .bench/results/rejected-rows-20260802/slice2/README.md and
  ladder/VERDICT.md.
- HISTORY (the `7dd403e2` pin, superseded by Slice 2 above; Slice 1's
  mechanism and wall result still stand): 8G/586: realtics 826,
  gametics 2134, instr 4,768,523,807, jit_direct_entries 120,859,985,
  direct_native_coverage 0.58000063519448, decode_probes 2,102,333,272.
  REALTICS 826 IS THE DOOM ORACLE at 8G/586. Slice 1 lowered the coupled
  prologue/epilogue family off the barrier list — `0x60` PUSHAD and `0x61`
  POPAD as MEMORY-class call-out slots (eight inline stores would blow the
  one-host-page block budget; a call-out is a fixed cost that fits), and
  `0x83 /5` word natively via the classify Word allowlist. The three rows
  are gone from the census with no neighbour absorbing them. **The
  instruction count did NOT move** — unusual and the slice's distinguishing
  property, verified 12/12 per role across the acceptance ladder; only the
  JIT accounting moved: entries -7.64%, unresolved_static_unbound -23.39%,
  decode_probes -3.27%, coverage +1.28pp. Conformance exact
  (pass=663210 vector_mismatch=2); all eight brk_* and every smc_lane_*
  counter byte-identical (no lane give-back). ACCEPTED on a six-pair ladder
  vs `8ac01d9f`: geomean 1.0135, lower95 1.0056, min-wall 1.0083, all three
  within 0.0052, quake noise_only. That is **+1.35%, positive_but_
  inconclusive class, BELOW the 1.02 lower95 IMPROVED bar — do not quote it
  as IMPROVED**. Evidence: .bench/results/rejected-rows-20260802/slice1/
  README.md and ladder/VERDICT.md.
  KNOWN PROPERTY, not a defect: 10.19% of memory call-outs fail closed
  (1,934,940 of 18,983,022) because the frame's page is not FastMap-resident
  — doom wipes the whole map 3,425,432 times per run in BOTH roles, so this
  is pre-existing workload behaviour. The mechanism self-heals (the
  pre-check is `&self` and never populates; the interpreter's re-execution
  does). Priced at ~56 ms, ~0.05% of wall — not the lever.
- HISTORY (the `b2b26add` pin, superseded by Slice 1 above; the Phase 5
  call-out arc, whose wall result and mechanism still stand): 8G/586:
  realtics 826, gametics 2134, instr 4,768,523,807,
  jit_direct_entries 130,858,089,
  direct_native_coverage 0.5671845741924803, decode_probes 2,173,445,068.
  REALTICS 826 IS THE DOOM ORACLE at 8G/586, and it did NOT move here.
  THIS IS A RE-PIN. The five commits between this and the `94b8b8e7` block
  below are the dynarec Phase 5 call-out slot arc — `24bcb52c` (the Phase 3
  merge, guest-invisible), then `fd874711` "Execute port reads through a
  block call-out slot", `295bc9c2` "Refuse call-outs that would probe the
  TSS", `62a05642` "Keep call-out blocks off the privileged path" and
  `b2b26add` "Charge the call-out only through the runtime lane". Doom's
  `IN AL,DX` barriers became native call-out slots, so 20.6M dispatcher
  unbound exits disappeared: entries -13.57%, decode_probes -3.65%,
  coverage +1.30pp. Guest STATE is untouched (gametics 2134 exact,
  conformance exact at pass=663210 vector_mismatch=2) and the call-out's
  charge is EXACTLY the interpreter's, so realtics did not move; the
  instruction count moves +4,465 only because block formation changed and
  a fixed 8G clock budget therefore retires a slightly different number of
  instructions. ACCEPTED on a six-pair ladder against a `24bcb52c` build:
  geomean 1.0141, median 1.0148, min-wall 1.0143, lower95 1.0067, all
  three estimators within 0.0007, quake noise_only. That is a +1.4%
  result, BELOW the 1.02 lower95 bar for an IMPROVED-class claim — do not
  quote it as one. Evidence:
  .bench/results/refactor-phase5-20260801/README.md and task3/README.md.
  CAVEAT: `b2b26add` fixed a +2-raw-clock-per-completed-call-out
  double-charge that the three preceding commits carried, so any number
  measured on an `fd874711`..`62a05642` build is NOT this identity.
- HISTORY (the `94b8b8e7` re-anchor, 2026-08-01 — superseded by the
  Phase 5 call-out block above): 8G/586: realtics 826, gametics 2134,
  instr 4,768,519,342, jit_direct_entries 151,407,546,
  direct_native_coverage 0.5542079239824546, decode_probes 2,255,872,004.
  This block is also what a build of `24bcb52c` (the merged Phase 3 tip)
  reproduces byte-for-byte, verified 12/12 as the base role of the Phase 5
  acceptance ladder — Phase 3 was guest-invisible.
  THIS WAS A RE-ANCHOR, NOT A RE-PIN: every value is byte-identical to the
  `dd4db734` block below. The seventeen commits between the two (PRs #661
  through #668, plus `ab947dd6` #661) are a non-CPU domain refactor — GUI
  session/runtime split, startup resolution, host-input policy, firmware
  contract, Toka-DOS scratch and TokaEMM scenario setup, and the SB16
  device-path extraction — and they moved NOTHING guest-visible on either
  fixture. All 179 non-wall json fields are identical across the boundary,
  doom and quake alike. Verified over 4 recorded observations per fixture
  plus warm-ups and profiled runs, all byte-identical within each fixture
  (the gate for this round was DETERMINISM, and it passed with zero drift).
  CAVEAT, do not over-read this: the headless fixtures do not exercise the
  GUI or the audio-synthesis path (trap 4 below), so this demonstrates the
  CPU/timing core is untouched — it is not evidence about the GUI, audio,
  or startup surfaces those commits actually rewrote.
  Evidence: .bench/results/baseline-94b8b8e7/README.md.
- HISTORY (the `dd4db734` pin, 2026-07-31, PR #660 and PR #659 combined —
  superseded in its commit label only, every value carries forward to the
  block above unchanged): 8G/586: realtics 826, gametics 2134,
  instr 4,768,519,342, jit_direct_entries 151,407,546,
  direct_native_coverage 0.5542079239824546, decode_probes 2,255,872,004.
  The counter half of this pin is PR #659's, not the lane campaign's: on
  `166490e8` (all of #660, none of #659) all eight counters below reproduce
  the lane-campaign pin bit-for-bit on BOTH fixtures, so 100% of the drift
  from that pin is #659's nine BIOS/RTC/CMOS/boot commits. #659 and #660
  were concurrent — `e32ecd08` vs `796f8a0e` is NOT a valid isolation (it
  reads realtics 828 because it lacks the whole dynarec campaign); the
  valid one is `166490e8` vs `dd4db734`. Root cause inside #659 is NOT
  bisected: intermediate commits of that branch are WIP and not safe A/B
  points (a midpoint build at `2df46d53` reads realtics 828). Leading
  hypotheses are the boot-path commits (`2df46d53`, `dc7a507c`), which fits
  realtics/gametics/frames being EXACT while instruction counts moved — the
  demo phase is untouched and the delta is boot-time. Gameport/joystick
  (`db39d26f`, `0f02416d`) are ruled out with no host joystick configured.
  OPEN ODDITY worth a look if quake JIT numbers ever matter: quake's
  `jit_direct_entries` moved +3.07% and `decode_probes` +0.34% for only
  +5,788 guest instructions — disproportionate for a boot-only delta, and
  the shape of a decode-cache aliasing shift (BIOS ISRs that run during the
  demo landing on different lines). Evidence:
  .bench/results/postmerge-dd4db734/ and, for a four-observation
  re-measurement of the same commit, .bench/results/baseline-dd4db734/.
- HISTORY (the lane-campaign pin, superseded by the line above only in its
  counter values — the mechanism and the wall result below still stand):
  8G/586 at 796f8a0e: realtics 826, gametics 2134, instr 4,768,483,859,
  jit_direct_entries 151,403,859,
  direct_native_coverage 0.5542105373833037, decode_probes 2,255,840,538.
  Realtics moved 827 -> 826 here. The lane commits
  are 28efd9ee "Read patched immediates through mutable block lanes",
  9b215a2a "Create lanes only from fetch-cached pages" and 796f8a0e
  "Differential-test the mutable lane matrix"; the trace that preceded
  them is 867937f9 + 5c15103e. Doom's four R_DrawColumn/R_DrawSpan
  ADD EBP,imm32 patch sites now keep their compiled blocks alive across
  a patch instead of retiring them, so smc_heat_demotions went 2,920 -> 0
  and the renderer loops stay natively admitted: coverage +4.32pp,
  jit_direct_entries -47.9% (work moved inside chains rather than back
  through the dispatcher). Guest TIMING re-anchors by design under the
  state-exact/timing-approx contract, which is why realtics moved
  827 -> 826; guest STATE is untouched (gametics 2134 exact) and
  conformance is exact (pass=663210 vector_mismatch=2). ACCEPTED on a
  balanced six-pair ladder: doom wall geomean 1.1238, lower95 1.1100,
  min-wall 1.1204, all estimators agreeing, quake noise_only.
  Evidence: .bench/results/smc-lanes-20260731/README.md and step4/VERDICT.md.
- HISTORY (superseded by the line above; this WAS the current pin from the
  phase-2 final e0f3ac97 until the lane commits above landed): 8G/586:
  realtics 827, gametics 2134, instr 4,760,669,086,
  jit_direct_entries 290,802,306,
  direct_native_coverage 0.5110531923243194, decode_probes 2,597,255,100.
  The decode-memo constant grew to 65536 lines in that commit and guest
  TIMING moves with it by design (a decode hit charges one collapsed
  I-cache access, a miss charges per fetched byte), which is why realtics
  moved 828 -> 827. Guest STATE is untouched and conformance is exact.
  Evidence: .bench/results/refactor-phase2-20260731/README.md.
- HISTORY (superseded by the lines above). The guest-visible pair through
  phases 0-1 was realtics 828, instr 4,759,944,877 on BOTH eras below, but
  the JIT accounting differs per era — label them, do not mix:
  phase 0 (93f62e88 era): jit_direct_entries 294,228,126, coverage 0.5095;
  phase 1 final (f087b858): jit_direct_entries 293,566,164, coverage
  0.509914735090324 (measured 12/12 in refactor-phase2-20260731/task6/
  step2-doom base legs; the phase-2 Task 6 review caught this file gating
  on the phase-0 value against phase-1 binaries).
  HISTORY (region JIT REMOVED as of the dynarec-refactor Task 2 commit):
  the GUEST-VISIBLE pair (realtics 828, instr 4,759,944,877) was
  byte-identical armed and unarmed; the JIT accounting was NOT —
  arming (IZARRAVM_JIT_REGION=473DF8, a variable that no longer exists)
  used to force region admission (jit_region_entries 0 -> 30,042,234) and
  Direct ceded to it: jit_direct_entries 252,219,369, coverage 0.4608
  armed. The old 773/4,924,354,749/"jit entries 1,542,342" triple was a
  region-JIT-era identity and is RETIRED - do not compare pre-Direct
  doom numbers to current runs
  (evidence: .bench/results/decodecache-20260731/RESULTS.md step 0).
- CURRENT QUAKE IDENTITY (re-pinned 2026-08-03 at `1d560fda`): 6.2G/586,
  instr 3,501,073,158, jit_direct_entries 24,536,481,
  direct_native_coverage 0.86701878852884, decode_probes 483,107,376,
  969 frames. FRAMES 969 IS THE QUAKE ORACLE. Every one of these values is
  BYTE-IDENTICAL to the slice-7 drift figures — quake records ZERO
  direct-write-token moves, so the aperture-scoping slice cannot touch it, and
  it does not. Across the whole gate JSON the ONLY field that differs is
  `jit_direct_compile_ns`, which is a host-nanosecond measurement and not a
  counter. That is what makes quake a genuine null control for this slice rather
  than merely a second workload, and it is why its +1.07% ladder reading is a
  confound measurement rather than a result.
- HISTORY (the `3a34d5cb` quake pin, superseded by the block above): re-pinned
  2026-08-02 at `3a34d5cb`, Slices 3+3b as
  a PAIR): 6.2G/586, instr 3,501,073,711, jit_direct_entries 24,681,797,
  direct_native_coverage 0.867053891628276, decode_probes 483,130,038,
  969 frames. FRAMES 969 IS THE QUAKE ORACLE and did not move. The
  instruction count moved +562 across the pair — exactly Slice 3's -616 then
  Slice 3b's +1,178, verified constant across 12 observations per role, not
  assumed. Coverage rises +0.0126 pp, the FIRST coverage gain in the 16-bit
  work (Slice 3 alone was flat to seven decimals). Quake is the fixture that
  drove this pair: it took the regression and it took the repair. Wall
  +0.75%, lower95 0.999953 — NEUTRAL, no positive claim.
- HISTORY (the `ca10d4f0` quake pin, superseded by the pair above):
  6.2G/586, instr 3,501,073,149, jit_direct_entries 24,829,319,
  direct_native_coverage 0.8669281194158791, decode_probes 483,717,835,
  969 frames. FRAMES 969 IS THE QUAKE ORACLE and did not move. The
  instruction count moved +336 (+0.0000096%) across Slices 1-2 — formation
  margin against a fixed budget, verified CONSTANT across 24 ladder
  observations, two orders below the +4,465 the `b2b26add` block already
  records. Quake carries the same three F7 rows doom does (33,830 exits);
  its larger Slice 2 story is formation — `blocks_installed` -14.98%,
  `arena_compactions` -42.22%, `dormant_other` 1,772,771 -> 912,391.
  `smc_lane_accepts` is 0 in BOTH roles (quake's standing control property:
  it registers lanes and never accepts a patch), so any movement in
  `smc_lane_registrations` or `smc_heat_demotions` there is formation
  margin, NOT a lane regression. Wall +0.70%, inside the ~1% relayout
  confound and not independently load-bearing.
  Evidence: .bench/results/rejected-rows-20260802/slice2/ladder/VERDICT.md.
- HISTORY (the `b2b26add` quake pin, superseded by Slice 2 above):
  6.2G/586, instr 3,501,072,813, jit_direct_entries 25,632,357,
  direct_native_coverage 0.866582453165335, decode_probes 485,731,064,
  969 frames (22.3 seconds, 43.5 fps). The FRAMES oracle is exact and the
  INSTRUCTION COUNT IS BYTE-IDENTICAL to the `94b8b8e7` block below —
  quake's 2,395,648 call-outs all STEP-BREAK, so each one converts a
  dispatcher unbound exit into a native side exit at the SAME guest
  boundary rather than deleting it (jit_direct_side_exits +66.71%,
  unbound_static -19.03%), which is what makes the guest identity
  survive unchanged. Only the JIT accounting moved. Same five call-out
  commits as the doom block above; wall reads noise_only on a six-pair
  ladder against `24bcb52c` (geomean 1.0009, lower95 0.9889, min-wall
  1.0043).
  ANY drift in either fixture = investigate before measuring wall.
  Evidence: .bench/results/refactor-phase5-20260801/README.md.
- HISTORY (the `94b8b8e7` re-anchor, 2026-08-01 — superseded by the
  Phase 5 call-out block above in its JIT accounting only; the instruction
  count and the frames oracle carry forward unchanged): 6.2G/586,
  instr 3,501,072,813, jit_direct_entries 25,637,518,
  direct_native_coverage 0.8652165198484834, decode_probes 490,518,457,
  969 frames (22.3 seconds, 43.5 fps). Same story as doom: that was a
  RE-ANCHOR, not a re-pin — byte-identical to the `dd4db734` block below
  across all 178 non-wall json fields, because PRs #661-#668 are a non-CPU
  domain refactor. It is also what a build of `24bcb52c` reproduces
  byte-for-byte, verified 12/12 as the base role of the Phase 5 ladder.
  Same caveat: headless says nothing about the GUI/audio surfaces those
  commits rewrote.
  Evidence: .bench/results/baseline-94b8b8e7/README.md.
- HISTORY (the `dd4db734` pin, superseded in its commit label only — every
  value carries forward to the block above unchanged): 6.2G/586,
  instr 3,501,072,813, jit_direct_entries 25,637,518,
  direct_native_coverage 0.8652165198484834, decode_probes 490,518,457,
  969 frames. The FRAMES oracle is exact and the counter movement from the
  lane-campaign pin is #659's, not the lane campaign's. See the doom entry
  for the isolation and the disproportionate-entries oddity.
- HISTORY (the lane-campaign pin at 796f8a0e, superseded in counters only):
  6.2G/586, instr 3,501,067,025, jit_direct_entries 24,874,134,
  direct_native_coverage 0.8654745651434651, decode_probes 488,852,002,
  969 frames. Quake compiles 66 lane-bearing blocks and accepts ZERO lane
  patches (it patches disp32, which lanes do not cover), which is what made
  it the campaign's control: every guest and diagnostic counter was
  identical across both roles of the step-4 ladder, 24 observations.
- HISTORY (phase 0): 6.2G/586, instr 3,499,647,370,
  jit_direct_entries 26,820,640, direct_native_coverage 0.8638. Phase 1
  moved instr to 3,499,652,885 (+5,515, the unexplained CDQ/CWDE-slice
  drift recorded in the phase-1 README); phase 2 moved it again with the
  decode-memo constant.
- v1 (PR #430, squash 77bd6fd0): the R_DrawColumn loop region, trampoline-
  maximal. 16.5% of retired instructions in the region, WALL-NEUTRAL (the
  per-slot region_step call ate the back-edge savings).
- HISTORY: v2 (its round, now retired along with v1): inline the 7
  register-only slots natively (mov r,r / add r,imm / shr r,imm) against
  gpr[] + flag helpers. Memory slots kept the v1 step. Target was 2-3x on
  the loop. Both v1 and v2 were the region JIT, which is REMOVED as of the
  dynarec-refactor Task 2 commit.
- Conformance oracle: pass=663210 vector_mismatch=2 EXACT
  (IZARRAVM_386_TESTS=D:/dev/IzarraVM/.local/386/json). The hard gate for
  any flag/gpr semantic change. v2 holds this byte-identical.
- HISTORY: "armed" meant `IZARRAVM_JIT_REGION=473DF8` (the linear address
  of the R_DrawColumn loop entry; forced admission, spike-only). The
  region JIT is REMOVED as of the dynarec-refactor Task 2 commit; that
  variable no longer does anything.

## Fixtures

### Doom (the wall A/B target)

- Location: `.bench/jemmex_doom_c` (stable copy; original was the
  `e51b16ef` scratchpad, which can vanish from temp).
- Config: CONFIG.SYS loads JEMMEX.EXE; AUTOEXEC.BAT runs
  `doom -config MAX.CFG -timedemo demo3`.
- Invocation: `--cpu 586 --hdd-folder <dir> --cycles <budget>`. HISTORY:
  older rounds set `IZARRAVM_JIT_REGION=473DF8` in the env; that variable
  no longer does anything (region JIT removed).
- Output: the perf row prints `instr=`, `decode_hit=`, `jit[entries/insns]=`,
  and `insns/run=`. The guest prints `timed <gametics> gametics in <realtics>
  realtics` (the determinism oracle; realtics 826 at 8G since the SMC
  mutable-imm32-lane campaign, and STILL 826 at the current pin
  `b2b26add` — the oracle moved neither across PRs #661-#668 nor across
  the Phase 5 call-out slot arc. HISTORY: 827 from the Phase-2
  decode-memo resize `e0f3ac97`; 828 through phases 0-1).
- Identity check: realtics + instr count + jit_direct_entries must match the
  settled invariants. If they drift, the A/B is confounded.
- **CORRECTION, 2026-08-11: the COUNTER half of the 8G/586 pin above is STALE
  on current main and must NOT be used as a drift gate.** Measured on main
  `f9847498` with the protocol invocation (`--cpu 586 --hdd-folder <dir>
  --cycles 8000000000`), two independent 6-pair ladders, both roles
  deterministic across 24 observations each:

      instructions             2,765,437,596   (pin says 4,768,523,775)
      timedemo                 2134 gametics / 979 realtics (pin says 826)
      jit_direct_entries          46,098,071   (pin says 70,851,041)
      direct_native_coverage        0.967161   (pin says 0.604987)
      decode_probes              133,388,799   (pin says 1,933,174,593)
      elapsed_budget_clocks    8,000,001,201
      guest_seconds                   48.193

  The pin was recorded before the 166 MHz/64 MB 586 respec, so a fixed 8e9
  cycle budget no longer buys the same amount of guest work — the run is now
  ~48 guest seconds instead of the ~85 the old tier gave, which is the whole
  discrepancy. Coverage really is 0.967 now, not 0.605.

  **`gametics` 2134 is unchanged and is the invariant that gates.** Nothing
  enforced was touched: the scoreboard and the realtime gate key on the guest
  oracle, and it matches. What is dead is the practice of eyeballing
  `instructions` / `jit_direct_entries` / `decode_probes` against the numbers
  in this section — an agent doing that on current main will conclude every
  build is confounded. Compare those fields CROSS-ROLE inside one ladder, as
  the sixpair summary already does, and treat the block above as the current
  reading rather than a new pin. `realtics` remains session-local (see the
  doom-realtics note in the campaign memory) and 979 is not a pin either.

### Quake (the calibration anchor, NOT the wall target)

- The fixture lives at `.bench/quake_c` (used by every 2026-07-31 campaign
  run). HISTORY: it was once missing from this machine after the
  `friendly-bardeen` scratchpad was cleaned up; if it ever vanishes again,
  it must be re-provided from the owner's copy.
- WORKING config (per the prior results ledger): CWSDPMI, `-condebug`,
  `EXITVM.COM`. The fps lands in `QUAKE/ID1/QCONSOLE.LOG`, not stdout.
- WARNING: there are TWO quake_c folders in history. `e51b16ef`'s copy is
  BROKEN (CONFIG.SYS references TOKAEMM.SYS + missing shell; the guest
  never boots and idles at a prompt). Use the `friendly-bardeen` copy ONLY.
- Anchor: 969 frames 22.9 seconds 42.4 fps at 140G/586. Byte-identical to
  the owner's anchor. HISTORY: the (now-removed) region JIT stayed inert
  on Quake (its loop was not the drawcolumn shape; 0 jit entries), so this
  is a regression check, not a wall target.

### NASCAR Racing 1 (integer 3D, no input needed)

- Location: `.bench/nascar1_c`, from the owner's `c_drive\GAMES\NASCAR`.
- WHY THIS ONE AND NOT NASCAR 2. NASCAR 2 has no attract mode: it boots to a
  mouse-driven main menu and sits there, so reaching a race needs a click
  schedule. NASCAR 1 starts a 3D attract demo on its own, which is the whole
  reason it is the fixture. NASCAR 2 was retired from `.bench` on 2026-08-05.
- SOUND CONFIG IS LOAD-BEARING. Use the owner's `SOUND.CFG` as installed. The
  default tries a MIDI output on port 330 and hangs. The boot log must show
  `Digital Sound / Port 220 IRQ 7 DMA 1 / Confirmed with SB16 8 ST` and
  `Initialized music driver`.
- Config: CONFIG.SYS loads `DEVICE=C:\DOS\TOKAEMM.SYS`; AUTOEXEC.BAT does
  `PATH C:\DOS`, `LH TOKAMOUS`, then `NASCAR.EXE -H` (the `-H` of the shipped
  SVGA.BAT). The mouse driver is required even though the attract mode needs no
  input.
- Invocation: `--cpu 586 --memory-mib 64 --hdd-folder .bench/nascar1_c
  --cycles 4980000000 --result-ppm <path>`.
- Output: NO timedemo line. The game never prints a result, so the oracle is
  the FRAMEBUFFER — but no longer the end-of-budget frame's hash.
- GRADING IS A FRAME CONTRACT (2026-08-18). The 4.98G frame lands mid-attract
  with the camera in flight, so its hash was a sample of the demo's PHASE and
  moved on any cadence-adjacent change: the IOPL-3 follow-up moved 12.41% of its
  pixels with rendering perfect, the camera a beat along the trackside banner
  (`.bench/results/postiopl-nascar-attribution/`). What grades the row now: an
  EXACT frame hash at 0.445G, the game's static startup logo, whose frame is
  bit-identical across a measured 0.395-0.495G window bounded on both sides by
  an all-black transition — 50M cycles of margin either way, 77x the phase shift
  the IOPL-3 change produced; plus bands at the budget on non-black coverage
  ([84.0, 100.0]%) and distinct colours ([45, 256]); plus the display class
  (Margo LFB 640x480 8bpp mode 0x0101), the `cycle_limit` stop, and retired
  instructions to ±5%. The end-of-budget hash is still RECORDED, as
  `final_frame_sha256`, and never graded — it is what an attribution cycle
  starts from. Derivations for every band are in `New-FrameContract` in
  `scripts/run-fixture-scoreboard.ps1`.
- Historical invariants at 586, measured 2026-08-05 (kept as the
  animating-vs-static evidence, no longer the grading path):
  - 6G under TOKAEMM: Margo LFB 640x480, 307,168 non-zero (100.0%), 129 colours,
    sha256 `654509fa6b44ff76...`. Two runs BYTE-IDENTICAL.
  - 7G (measured under JEMMEX): 307,037 non-zero (99.9%), 154 colours. Kept only
    as the animating-vs-static check; re-measure if you need it exact.
  - MANAGER-SENSITIVE: under JEMMEX the 6G frame was 307,185 / 129. The attract
    demo is animating continuously, so a manager swap shifts it by a frame.
  - Both checks are needed. Identical-at-6G proves determinism; 6G-differs-
    from-7G proves the demo is ANIMATING. A fixture parked on one static frame
    would pass the first check alone and measure nothing.
- Needs VBE, but the OLD VBE 1.2 kind: measured with `IZARRAVM_INT_TRACE=10`
  over 4G cycles it calls `4F01` once, `4F02` twice, and `4F05` (bank switch)
  **2,325 times**. It never asks for `4F0Ah`, so it did NOT need the VBE 2.0
  protected-mode work that NASCAR 2 forced. Without any VESA path at all it
  exits with "Unable to set SVGA mode. VESA driver loaded?".
- Those 2,325 bank switches are 2,325 INT 10h round trips, which is exactly the
  cost the VBE 2.0 protected-mode interface exists to remove. If this fixture
  ever needs to go faster, teaching it the PM path (or checking whether the
  game will take a linear framebuffer) is the lever, not the JIT.

### Prince of Persia (16-bit, V86 under TokaEMM, 486)

- Location: `.bench/prince_c`. CONFIG.SYS MUST load a memory manager or the game
  refuses to start ("Requires 318 KBytes... 304 KBytes is Available") and
  AUTOEXEC's `:loop` relaunches it forever, so a run that looks busy measures the
  relaunch, not the game.
- USES TOKAEMM as of 2026-08-06: `DEVICE=C:\DOS\TOKAEMM.SYS`. The older
  "JEMMEX, not TokaEMM" instruction dated from when TokaEMM had the memory-layout
  issues that PRs #702/#705/#707 closed, and is stale. The boot log must show
  `TOKAEMM: XMS/UMB/EMS memory manager; system running in V86.`
- THE TWO MANAGERS WERE GUEST-EQUIVALENT HERE, measured on the OLD 586/20G
  schedule: byte-identical framebuffer under TokaEMM and under JEMMEX. PoP is
  the only one of the three game fixtures where that held -- NASCAR 1 and GP2
  both moved by a frame on the same swap. The difference is that PoP's schedule
  ends at a settled point while the racers animate continuously. Do not
  generalise from this; re-measure per fixture.
- It runs in V86, not real mode, so it is NOT a clean 16-bit-real-mode workload.
  Use `.bench/bench16_c` (3DBENCH) for that.
- THE INTRO IS MOSTLY IDLE. Without keystrokes PoP sits in its attract sequence
  and halt fast-forward makes RTF meaningless (386 once read 18.3). A no-input
  run is not a workload; it is a screensaver. Frames do change across budgets,
  so "the screen is moving" does NOT prove the fixture is measuring anything.
- Shift is the action key -- chosen by the game because as a modifier it had
  better keyboard rollover -- and it is what advances the title screens. There
  is no menu. In game, Shift alone does nothing, so extra Shifts are harmless
  and the schedule can over-provision them safely.
- RUN IT AT 486, NOT 586. A 1989 game does not need a 166 MHz persona, and 486
  is 66 MHz, so the same guest time costs a third of the cycles. Together with
  a tighter schedule this took the fixture from 410 s of wall to 60 s.
- Schedule (`--cpu 486 --memory-mib 64 --video vega --cycles 4000000000`):

      --inject-keys '400000000:{shift};600000000:{shift};800000000:{shift};1000000000:{shift};1200000000:{shift};1400000000:{shift};1600000000:{+right}'

  Six Shifts to reach level 1, then `{+right}` HELD so the prince runs instead
  of standing. The Shifts sit at ~6-21 guest seconds and the hold starts at
  ~24 s, leaving ~36 s of running. HISTORY: the first schedule spread the
  Shifts over 13G at 586 and spent 65 guest seconds crawling through an idle
  intro to buy 35 seconds of play. `{+key}`/`{-key}` are press-only and release-only; a bare
  `{right}` is a tap and leaves him standing, because the guest tracks key-down
  state from the scancode stream.
- Where he ends up: he runs right over the dropping floor and out of the
  starting room, and the view follows him. The final frame is the start room
  with the dropped floor open and no prince in it (77,790 non-zero / 60.8% /
  17 colours, against 78,900 / 24 colours for the standing-prince frame at the
  same budget). That is expected, not a broken schedule. Whether he survives
  does not matter for a fixture; it only has to be reproducible.
- REPRODUCIBLE: two runs of the schedule above produced BYTE-IDENTICAL PPMs.
  Invariant at 4G/486/TokaEMM: 77,802 non-zero (60.8%), 17 distinct colours,
  sha256 `32b5eefa8e7013dc...`.
- Budget: ~60 s of wall, so an A/B/B/A set is ~4 minutes. The cheapest of the
  game fixtures by a wide margin.
- 586 USED TO DIE HERE at 6.28 guest seconds on `unsupported I/O port 0x7421`.
  That was never a 586 bug -- see the open-bus policy change. It now plays.
- PoP writes 0x7420-0x7423 continuously (~17,000 per 1G cycles in the intro).
  Nothing is known to live there. `IZARRAVM_PORT_FATAL=7420` names the CS:IP.

### Grand Prix 2 (x87 physics, mouse-driven menus)

- Location: `.bench/gp2_c`.
- Config: CONFIG.SYS loads `DEVICE=C:\DOS\TOKAEMM.SYS`; AUTOEXEC.BAT does
  `PATH C:\DOS`, `LH TOKAMOUS`, then `GP2.EXE NOINTRO`.
- The menus are MOUSE ONLY. Four Enter presses through `--inject-keys` leave
  the framebuffer bit-identical to no input at all, so `--inject-mouse` is the
  only way in.
- Mouse calibration, measured rather than assumed: GP2 sets its own INT 33h
  ratio and moves **1 pixel per mickey on BOTH axes**. That is NOT TOKAMOUS's
  default, which is 1:1 horizontally but 2 mickeys per pixel vertically. A
  schedule built on the driver default overshoots vertically by 2x and clicks
  nothing.
- The calibration method, if it has to be redone: home the pointer, move a
  known delta, and diff the resulting PPM against a no-input PPM of the same
  screen. The cursor is a 5-pixel dot on the credits box, invisible by eye but
  trivially found with `ImageChops.difference(...).getbbox()`.
- A CLICK MUST HOLD THE BUTTON. GP2's startup menu samples the button once a
  frame and never saw a press/release 5 ms apart, with the pointer verified to
  be on the button. `--inject-mouse click` now holds for 100 ms; an explicit
  `down` ... `up` pair with a gap works too.
- `F1GSTATE.SAV` is NOT written back through Katea (its mtime survives many
  runs), so the fixture does not drift between runs.
- THE DEFAULT CIRCUIT IS DETERMINISTIC. Quickrace always lands on Select
  Circuit with Italy/Monza already ticked, so the schedule does NOT need to
  click a track -- just OK. Verified by byte-comparing the Select Circuit PPM
  from two runs that reached it by DIFFERENT input paths (an explicit
  `down`/`up` pair and a `click`): identical. The headless machine seeds no
  entropy -- `--hdd-folder` never calls `seed_rtc`, so the RTC sits at
  2026-01-01 00:00:00 every run -- which is why a menu that looks randomised to
  a player replays exactly here.
- Menu-phase invariants at 586 (`--cpu 586 --memory-mib 64`):
  - credits box, no input: 298,655 non-zero (97.2%), 171 colours
  - startup menu, after the first click: 295,451 (96.2%), 168 colours
  - select circuit, after the second: 305,847 (99.6%), 117 colours
- THE RACE ITSELF IS DETERMINISTIC. Two independent runs of the schedule below
  produced BYTE-IDENTICAL framebuffers, AI cars and all. The intuition that a
  race full of AI drivers cannot be reproduced does not hold headless: nothing
  here varies between runs. That makes a fixed-cycle A/B equal-work by
  construction, so PROTOCOL trap 1 (idle-tail confounding) does not apply --
  both legs execute the same guest instruction stream.
- SHIPPING SCHEDULE, ~30 guest seconds of race in 13.28G cycles:

      --cpu 586 --memory-mib 64 --hdd-folder .bench/gp2_c --cycles 13280000000
      --inject-mouse '3320000000:home;3652000000:move:320,386;3984000000:click;4648000000:move:0,-115;5146000000:click;5976000000:move:-273,181;6474000000:click'

  The three clicks are the credits OK (320,386), Quickrace (320,271), and the
  Select Circuit OK button (46,452). Coordinates are pixels, which equal mickeys
  for this game.
- Race invariant at 16G under TOKAEMM: Margo LFB 640x480, 307,152 non-zero
  (100.0%), 199 distinct colours, sha256 `1006e75142cc83f3...`. Two runs
  BYTE-IDENTICAL. The cockpit dashboard reads POS 26, LAP 1 OF 3, CAR 27,
  RUNNERS 26 -- a human-readable check to eyeball a screenshot against.
- THIS FIXTURE IS WHY THE HASH IS RECORDED. Under JEMMEX the same schedule gives
  307,152 non-zero and 199 colours -- THE SAME AGGREGATES -- from a DIFFERENT
  frame (sha256 `1e594bdfc99ee529...`). Counts alone cannot tell the two apart,
  so a count-only invariant would have passed a real change silently. Compare
  the hash.
- THE HASH IS A CUTOFF-PHASE SAMPLE, NOT A HARD GATE (2026-08-11, measured).
  GP2 copies its rendered scene into the LFB in ~28-row bands top-to-bottom and
  the fixed-cycle axe lands mid-copy. A +100k-cycle control (7.5 ppm of the
  budget, ~0.6 ms guest) on an UNCHANGED binary moved the hash to a new value
  with the same one-band signature: one contiguous ~28-row band carries the
  adjacent frame's content, everything else byte-identical, HUD readout
  unchanged. So the hash IS deterministic per binary (two runs byte-identical
  -- that part of the invariant is real and stays) but any timing-model change
  that shifts cycle charging by ppm moves it legitimately: expect a reviewed
  re-pin on timing slices, same as nascar's animation-phase sample. Judge a
  moved hash by the band signature + the POS/LAP/CAR dashboard text, not by
  the hex. Attribution evidence: scoreboard-20260811-*-gp2-attribution runs.
  LONGER TERM this row deserves a cutoff-insensitive invariant of the Duke3D
  kind (guest-driven end or settled-frame budget).
- BUDGET. 16G costs ~290 s of wall, so an A/B/B/A set is ~20 minutes. An earlier
  40G schedule (2 minutes of race, menu phase starting at 8G) cost 752 s per
  run, which is over an hour for the same four legs and buys nothing: the race
  is in steady state within seconds. Do not lengthen this without a reason.
  4G is already past the credits box (verified at 4G and 5G), which is what
  lets the menu phase start there instead of at 8G.

### Duke Nukem 3D / DUKEMARK (Build engine, guest-driven, no input needed)

- Location: `.bench/duke3d_c`. The game is **Duke Nukem 3D Atomic Edition**
  (44 MB GRP) plus DUKEMARK.EXE, DOS4GW.EXE and BENCH1/2/3.DMO. Atomic is not
  a preference: DUKEMARK is a modified Atomic build and its CON parser rejects
  the 1.3D `USER.CON` outright (`gamestartup` takes more parameters in 1.4+),
  which stalls the boot on a "use internal defaults (Y/N)?" prompt.
- Config: CONFIG.SYS loads JEMMEX.EXE (NOT TOKAEMM — a TokaEMM image change
  cannot move this fixture). **The whole run is guest-driven**, and the
  AUTOEXEC is the entire protocol:

      @echo off
      cd \DUKE3D
      DUKEMARK.EXE /bqBENCH2 > C:\DUKEMARK.TXT
      C:\EXITVM.COM

  No `goto loop`, no injected keys or mouse. The demo needs no input, DOS
  redirection captures the report, and `EXITVM.COM` — the house 15-byte Lotura
  unit-tester exit poke, the same file the bench16 fixture carries and the one
  the Doom fixture's AUTOEXEC already names — ends the VM.
- Invocation (as embedded in `scripts/run-fixture-scoreboard.ps1`):
  `--cpu 486 --memory-mib 64 --video vega --cycles 26400000000`, or
  `--cpu 586` with `--cycles 79680000000`.
- **THE CYCLE BUDGET IS A GUARD, NOT THE LENGTH OF THE RUN.** The guest ends
  the VM itself when the demo is over; at 486 that lands at about 10.8e9
  (163 guest seconds) against a 26.4e9 guard, and at 586 at about 23.2e9
  (140 guest seconds) against a 79.68e9 guard. A run that reaches the budget has
  failed to finish and the stop-reason invariant says so directly, so there is
  no budget to size against a moving demo any more. The two figures were 293.6
  and 168.8 guest seconds until 2026-08-10, when the HDD-geometry slice took the
  FAT-chain walking out of the load phase; the guards were sized against those
  and are now generous rather than merely adequate. They were deliberately left
  alone, since a guard that is too large costs nothing on a run that finishes.
- **REDIRECTION CAPTURES THE REPORT — verified, and it was the design's one
  real risk.** DUKEMARK prints its report through ordinary DOS stdout, so
  `> C:\DUKEMARK.TXT` catches all of it; the 80x25 text page is BLANK on a
  redirected run, which is the proof. If a future build ever painted that
  screen directly instead, the file would come back empty and the row would
  fail loudly rather than grade wrong numbers. Katea holds guest writes until
  `flush_hdd_folder()`, which the end-of-run path performs whatever the stop
  reason was, so the file is host-side by the time the harness reads it out of
  the working copy. The tail of a good file:

      DukeMark by DXZeff

      Info         : 2,320,200,2,0,1,1,1
      FPS Minimum  : 11
      FPS Maximum  : 50
      FPS Average  : 31
      Extrapolated : 919 Samples

- FOUR INVARIANTS. The three timing-INSENSITIVE ones are pinned by
  `New-DukemarkPins` in the scoreboard script, since none of them can move
  without the fixture being rebuilt. The fourth, the sample count, lives in
  `scripts/fixture-scoreboard-invariants.json` beside the frame hashes as
  `dukemark_samples` / `dukemark_samples_tolerance`, per persona, because it
  follows emulated timing and therefore does move (see below).
  1. **The run stopped as `test_exit` with code 0x51.** The game returned to
     DOS on its own and the batch reached `EXITVM.COM`. Completely
     timing-insensitive, and it is what replaced the framebuffer hash.
  2. **`DUKEMARK.TXT` exists and parses.** Guards the redirection path itself.
  3. **Info String `2,320,200,2,0,1,1,1`** — DUKEMARK's config fingerprint,
     `Demo,Width,Height,Mode,Hud,Detail,Sound,Music` read out of DUKE3D.CFG.
     320x200, ScreenMode 2 (screen-buffered), ScreenSize 0, Detail 1, and the
     trailing `1,1` is sound and music both ENABLED, so a regression that
     silences the game cannot quietly present itself as a speedup. Also
     timing-insensitive.
  4. **The extrapolation count, to a TOLERANCE of ±2%**, pinned PER PERSONA
     in the sidecar (919 at 486, 1026 at 586; ±2% is ±18 and ±21 counts).
     This is DUKEMARK's stall detector.
- **The extrapolation count is not machine-independent, whatever the DUKEMARK
  docs say.** Same demo, same config, same fixture, and the two personas read
  materially different counts — reproducibly, two runs each. It is a function
  of emulated timing, so an exact pin would rebuild the re-pin treadmill this
  fixture was rewritten to escape. ±2% is far tighter than the "stalls very
  hard" case it exists to catch: a multi-second stall inside a ~131 s demo
  moves the count by several percent. Widen the band before re-pinning the
  value.
- **±2% SIZED AGAINST MEASUREMENT, 2026-08-10.** A band nobody has stressed is
  a guess, and a band that a routine JIT slice can breach is the re-pin
  treadmill under a new name, so the question was asked with the largest lever
  the harness has: `-Arm off`, which turns off both halves of the JIT and takes
  duke3d-486's native coverage from 0.7235 to 0.5932 and its wall from 141.1 s
  to 155.2 s. **The sample count moved by ONE — 919 to 920, 0.11% of the pin,
  against an allowance of 18.** The band survives its worst available stress
  with a factor of 18 in hand, so it stays at ±2%.
- The reason it survives is worth writing down, because it is what makes the
  band meaningful rather than lucky: `-Arm off` barely moves GUEST time at all.
  Guest seconds went 163.150 → 163.103, three parts in ten thousand, and the
  instruction count moved by 0.02%. Cycle charging is per-instruction and does
  not care which backend retired the instruction, so changing the JIT mix
  changes wall and leaves the emulated clock — which is what DUKEMARK samples
  against — very nearly where it was.
- **So the band covers JIT-mix work and deliberately does NOT cover timing-model
  work.** The storage-charge and HDD-geometry slices earlier the same day moved
  the 486 count 580 → 919 (+58%) and the 586 count 962 → 1026 (+6.7%), which no
  sane band absorbs. That is the correct outcome: a change to what the emulated
  machine charges for I/O SHOULD land on a reviewer's desk as a pin move with a
  stated cause, and a change to how fast we execute the same charged work should
  not. Widening the band until timing-model changes fit would only hide the one
  class of move that is worth reading.
- **Within one build the count is EXACT, and so is everything else guest-side.**
  Two 486 runs twenty minutes apart, on a host busy enough that their WALL times
  differed by 38%, agreed to the digit on 919 samples, 163.15 guest seconds,
  13,631 INT 13h read calls, 11,936 sector-cache hits and 0.375 guest seconds of
  charged I/O stall. That is the property the storage charge model is built on,
  and it is worth re-checking whenever the band is widened: a count that starts
  varying WITHIN a build is a determinism bug, not drift.
- FPS minimum / maximum / average are **MEASUREMENTS, never invariants.** They
  are guest-observed frame rates and move with host load. Report them; do not
  assert on them.
- The Info String does NOT identify the demo: its first field reads `2` for
  BENCH1, BENCH2 and BENCH3 alike (measured). The extrapolation count is the
  only field that tells them apart — 880, 573 and 1248 samples respectively at
  486 under the earlier screen-scraped shape. **BENCH2 was chosen because it is
  the shortest**, and this fixture is already the most expensive in the set.
- Shape of the guest run at 486: about 32 guest seconds of LOADING (CON
  compile, 44 MB Atomic GRP), then ~131 s of demo, then the exit. It was 161 s
  of loading until 2026-08-10, when the synthesized Katea volume stopped
  deriving 512-byte clusters; that load was ~86% FAT-chain walking, not reading
  the game.
- WHERE THE FIXTURE SITS, from the last full sweep
  (`.bench/results/scoreboard-20260810-081504-armon-post-hdd-audio-merge`, arm
  `on`, both one-lookup knobs `1`, host load ~2%):

  | | guest s | wall s | rt | coverage | insns/entry | samples | fps min/avg/max |
  |---|---|---|---|---|---|---|---|
  | duke3d-486 | 163.15 | 139.4 | 1.17 | 0.7235 | 10.17 | 919 | 11 / 31 / 50 |
  | duke3d-586 | 140.04 | 470.1 | 0.2979 | 0.7183 | 10.92 | 1026 | 83 / 154 / 213 |

  The 586 row is the furthest below real time of anything in the set, which is
  why it is the workload the campaign's merge rule protects. Both rows moved
  with the HDD-geometry slice on 2026-08-10 (486 was 293.6 guest s / 580
  samples, 586 was 168.8 / 962); numbers recorded before that date are not
  comparable.
- RETIRED 2026-08-09: the framebuffer-PPM sha256 invariant and its
  cutoff-phase acceptance test. The end-of-budget frame was cutoff-phase
  sensitive (the fixed-cycle axe landed mid-render in an animating demo), so
  every JIT-mix change that shifted cycle charging moved it legitimately — six
  benign moves in three days, each one a failed sweep plus a manual re-pin,
  and the automated re-pin that replaced the manual one turned main red three
  times over the LICENSE_MANIFEST sha it also had to move. Nothing about the
  new invariants depends on where a cycle budget happens to land, because
  nothing about the new run depends on the budget at all.
  - CAUSE OF THOSE THREE REDS, corrected 2026-08-10: the re-pin script simply
    did not touch the manifest at all, so each of those commits carried the
    PREVIOUS commit's sha. All three blobs are plain LF. `Write-Invariants`
    now re-records the sha in the same breath as the json, which is the fix.
    A separate CRLF defect in the same helper (`Set-Content -Encoding utf8`
    writes CRLF, git stores LF, so a recorded sha could never match the
    committed blob) was found and fixed at the same time, but it was LATENT
    and is NOT what those three reds were. The commit message on `12e2331c`
    says otherwise; this is the correction.

### Duke Nukem 3D SHORT (`duke3d-586-short`, the cheap ladder row)

Added 2026-08-16 at main `89dc3a69`. The long duke3d-586 row is the workload
the campaign's merge rule protects and it costs ~342 s of wall a leg, which
makes an A/B/B/A set 23 minutes and a six-leg floor most of an evening. This is
the row to LADDER candidates on. **It does not replace the long row: re-run
duke3d-586 before any merge decision.**

- Location: `.bench/duke3d_short_c`, built by
  `scripts/make-duke-short-fixture.ps1` (idempotent, `-Force` to rebuild). It is
  `.bench/duke3d_c` with exactly two files changed:
  - `DUKE3D\BENCH2S.DMO` — BENCH2.DMO with its record count lowered from 3909
    to **1560**. sha256
    `b3dbe8313b51d16cb4e4ba9bf7473cfe80b530340d057b83c9b7feaf1a35dce4`.
  - `AUTOEXEC.BAT` — `DUKEMARK.EXE /bqBENCH2S` instead of `/bqBENCH2`.
- **WHY A RECORD COUNT AND NOT A CYCLE BUDGET.** The duke row is guest-driven:
  DUKEMARK plays the demo to its end, prints its report through DOS stdout and
  the batch pokes `EXITVM.COM`. Cutting `--cycles` would stop the run before any
  of that, so all four DUKEMARK invariants — the `test_exit` 0x51 stop, the
  result file, the Info String, the sample count — would be destroyed at once,
  and what replaced them would be the cutoff-phase-sensitive framebuffer hash
  this fixture was rewritten in 2026-08-09 to escape. The budget is still only a
  guard here (33.2e9 = 200 guest seconds against the ~60 where EXITVM fires).
- **THE DEMO HEADER IS WHERE THE LENGTH LIVES.** A Duke3D `.DMO` opens with a
  little-endian dword record count, then `0x74` (Atomic BYTEVERSION), then
  volume / level / skill bytes, and carries the recorder's name at offset 30
  (`DXZEFF` on all three BENCH demos). Playback counts records down from that
  dword and ends when it reaches zero, so lowering it ends playback early **at a
  record boundary with the rest of the file untouched**. Nothing is truncated
  and nothing is re-encoded, which is what makes the short row a genuine PREFIX
  of the long row's guest instruction stream rather than a different workload.
  The build script asserts both the `0x74` and the 3909, so a fixture swap under
  it fails loudly instead of silently producing a different demo.
- Invocation (as embedded in `scripts/run-fixture-scoreboard.ps1`):
  `--cpu 586 --memory-mib 64 --video vega --cycles 33200000000`, arm `on`, both
  one-lookup knobs `1`, no injection, no CD image.

      pwsh scripts/run-fixture-scoreboard.ps1 -Fixtures duke3d-586-short -Label <label>

- PINS, measured seven times on `89dc3a69` (six A/A legs plus the record leg):

  | | value |
  |---|---|
  | stop | `test_exit`, code 0x51 |
  | Info String | `2,320,200,2,0,1,1,1` (unchanged — DUKE3D.CFG is copied byte-exact) |
  | DUKEMARK samples | **404** ±2% (sidecar `duke3d-586-short`) |
  | guest seconds | **59.97** |
  | instructions | **9,816,023,763** |
  | entries | **706,581,301** |
  | direct insns | **7,670,921,489** |
  | interpreted insns | **2,145,102,274** |
  | fps min/avg/max | 91 / 158 / 213 (MEASUREMENTS, never asserted) |

  **Every one of those guest-side numbers was byte-identical across all six A/A
  legs**, digit for digit, including the sample count and the fps triple. That
  is the same property the long row has and it is what makes the row usable.
- A/A FLOOR, 2026-08-16, six legs, one binary, host load 1.5–3.4%, pinned to
  processor 8: **min 142.12 s, max 144.97 s, geomean 143.490 s, spread 2.01%,
  sd/mean 0.77%.** Rolling daily floor per the cost-scaling protocol — re-floor
  before reading any effect off this row on another day.
- COST. 143.5 s against the long row's 341.9 s measured the same night: a
  **2.38x** cut, so an A/B/B/A set falls from 23 minutes to 9.6 minutes.
  **It is not 3x, and it cannot be made 3x at 60 guest seconds.** The run has a
  FIXED load phase — 8.05 guest / 20.01 wall seconds of CON compile and 44 MB
  GRP read that no demo trim touches — so wall is `20.01 + 0.082334 * records`
  and guest is `8.05 + 0.033293 * records` (two measured points: 200 records ran
  14.707 guest / 36.473 wall, 3909 ran 138.19 / 341.851). A 3.00x cut needs 1141
  records and 46 guest seconds. 1560 was chosen over 1141 because it holds the
  load phase to 13% of the row's guest time instead of 17% (the long row is 6%),
  and representativeness is the thing that would make a cheap row worthless.
- REPRESENTATIVENESS — MEASURED, NOT ASSUMED. Both rows were run under
  `IZARRAVM_DIRECT_BARRIER_CENSUS=1` on a census-feature build of the SAME
  commit (`.bench/results/duke-short-row-20260816/run-census.ps1`; the census
  taxes the run ~1.35x, so those walls are not board numbers).

  | decline class | short | long | ratio |
  |---|---|---|---|
  | dormant_probe | 67.12% | 70.27% | 0.955x |
  | rejected_probe | 20.22% | 17.47% | 1.157x |
  | heat_refusal | 12.61% | 12.24% | 1.030x |
  | key_failure | 0.05% | 0.02% | 2.511x — see below |

  Every major fraction is within 1.16x; the row is the same workload class.
  Declines per guest instruction: 0.2038 short against 0.2180 long, 0.935x, so
  the INTENSITY carries too and not just the mix. `key_failure` reads 2.5x only
  because it is a FIXED-COUNT class: **1,043,023 events on the short row against
  1,043,690 on the long one**, i.e. essentially all of it happens before record
  1560, and the smaller denominator inflates the percentage. It is 0.05% of the
  row; do not read it as a mix change.

  The static-unbound split moves more, and one class nearly doubles: dormant_heat
  49.44% / 46.03% (1.074x), rejected 36.71% / 33.64% (1.091x), compiled 4.25% /
  3.85% (1.104x), but **dormant_other 7.04% against 13.85%, 0.51x**. The long
  row accumulates dormant_other late in the demo. It is just inside a 2x band
  rather than comfortably inside it: a slice aimed specifically at
  `dormant_other` should be laddered on the LONG row.

  Closure is exact on both (`unattributed_static`, `unattributed_dynamic` and
  `rejected_unattributed` all 0).

  Two side results worth keeping. The long row's split at `89dc3a69` is
  70.27 / 17.47 / 12.24 / 0.02, **identical to the 2026-08-15 measurement at
  `365668ac`** — the sticky-decline memo did not move the decline mix, so the
  20260815 baseline is still live. And the census build reproduces the plain
  build's instruction count to the digit on both rows, which says the census
  perturbs wall and nothing else.

### Wolfenstein 3D (16-bit real mode, the JIT16 heavyweight)

- Location: `.bench/wolf3d_c`. Config loads `DEVICE=C:\DOS\TOKAEMM.SYS`;
  the run needs one injected Enter (`--inject-keys "2000000000:<CR>"`) to get
  past the signon screen into the title/credits/demo rotation.
- **THAT ARGUMENT ENDS IN A LITERAL NEWLINE CONTROL BYTE, written `<CR>`
  above because the byte cannot be rendered inline.** The syntax is
  `<cycles>:<text>` and the text here is the Enter CHARACTER, so the argument
  as the script holds it is the eleven bytes `2000000000:` followed by one
  control byte (historically 0x0D; as of the 2026-08-10 hardening commit the
  script's byte is 0x0A — `ascii_key` maps both `'\r'` and `'\n'` to the same
  Enter scancode 0x1c, so the two spellings are functionally identical, and
  `.gitattributes` keeps 0x0A checkout-stable). This matters because the failure is SILENT: a step with an empty text
  still parses, expands to zero scancode groups and injects nothing, so a
  hand-copied `--inject-keys "2000000000:"` leaves the run sitting on the
  signon screen and produces a plausible-looking wall number for a game that
  never started. That is the shape every pre-2026-08-08 wolf3d number had.
- **Two spellings ARE representable, and either reproduces the run** —
  `--inject-keys "2000000000:\r"` (the flag rewrites the two-character escape
  `\r` to 0x0D precisely because a bare CR does not survive a shell argument)
  or `--inject-keys "2000000000:{enter}"` (the named key, scancode 0x1c). Use
  one of those rather than retyping the raw byte; the raw byte in
  `scripts/run-fixture-scoreboard.ps1` is only there because the string is
  inside a script file rather than on a command line.
- **Every wolf3d number recorded before 2026-08-08 measured an out-of-memory
  CRASH LOOP, not the game** — the full post-mortem is in `HARNESS.md`; do
  not compare against pre-fix history.
- Budgets: 4e9 at 486 (61 s guest), 12e9 at 586 (72 s, so the end frame
  lands inside demo playback). Framebuffer sha256 pinned per persona in
  `scripts/fixture-scoreboard-invariants.json`.

### Tomb Raider Gold (Pentium 3D + CD-streamed FMV, 586 ONLY)

- Location: `.bench/tombraid_c`; disc at `.bench/tombraid_cd/tombeng.cue`.
  The disc is REQUIRED (FMV, CD audio, CD check) and mounts via `--cd-image`;
  it is read-only at run time and is not copied per run.
- Invocation: `--cpu 586 --memory-mib 64 --hdd-folder <fresh copy>
  --cd-image .bench/tombraid_cd/tombeng.cue --cycles 28000000000
  --result-ppm <path>`. No input schedule: the title menu starts a demo level
  by itself after idling. Do NOT run it at 486 - the game needs a Pentium+FPU.
- CONFIG.SYS loads `TOKAEMM.SYS RAM /T` and nothing else; AUTOEXEC sets
  BLASTER and loops `TOMB.EXE`. There is NO CD driver line and NO `IZCDEX`
  line: PRs #755/#756 moved CD service into the BIOS, which claims drive D:
  at boot, and neither `TOKACD.SYS` nor `IZCDEX.COM` has shipped in `C:\DOS`
  since. A fixture that still names them boots with a three-line CONFIG.SYS
  error and a `Bad command or filename - "IZCDEX".`, costs nothing visible
  because the BIOS serves D: anyway, and is a trap for the next fixture that
  needs a real driver loaded high. Removed from all four Tomb Raider and
  Descent II trees on 2026-08-30.
  `TOMBPATH.TXT` in `C:\` (17 bytes, no trailing newline) names
  `C:\GAMES\TOMBRAID`. Sound: SB16 220/7/1 via `HMISET.CFG`, no MIDI.
- Timeline and the 28e9 budget rationale: see `HARNESS.md` (mid-demo end
  frame; 30e9 lands on the demo-to-menu transition).
- Oracle: a FRAME CONTRACT, not the end-of-budget hash (2026-08-18). The 28G
  frame is mid-demo by design, which is exactly where a hash cannot survive a
  cadence change: the IOPL-3 V86 monitor moved 84.31% of its pixels while
  rendering stayed perfect — the camera a beat further along plus the blinking
  "Demo Mode" caption in its other phase
  (`.bench/results/iopl3-tombraid-attribution/`). The exact-frame invariant now
  sits at 0.5G, the DOS/4GW banner page, pinned as a SET of two hashes because
  the only moving thing there is the DOS underline cursor — measured as exactly
  18 pixels, a 9x2 block at x0-8/y334-335 toggling #000000/#AAAAAA — and that
  frame is byte-identical across the pre-IOPL-3, IOPL-3 and post-PR-725 builds.
  At the budget the row is graded on bands instead: non-black coverage
  ([89.0, 100.0]%), distinct colours ([79, 256], where 256 is the 8bpp palette
  bound and rejects the deeper-mode FMV frames), the display class, the
  `cycle_limit` stop, and retired instructions to ±5%. The end-of-budget hash is
  RECORDED but not graded. Pins in
  `scripts/fixture-scoreboard-invariants.json`; derivations in
  `New-FrameContract` in `scripts/run-fixture-scoreboard.ps1`.

### The two Glide rows (Distira, 586 ONLY, added 2026-08-30)

`tombraid3d-586` and `descent2-3dfx-586` are the first fixtures that render
through Distira, the Voodoo-Graphics-class part, instead of through the CPU or
Margo. They exist because nothing else on either board exercises a single
triangle of hardware 3D: before them, Distira had register-level unit tests and
no game had ever driven it.

They ship the BYTE-IDENTICAL `glide2x.ovl`, md5 `341b8f5d82daa46fd1ce2363...`.
That is the point of running both. Whatever the library asks of the device it
asks identically in each, so a defect that moves one row and not the other is
in the GAME's use of Glide, and one that moves both is in the device.

**586 only.** Both are DOS/4GW protected-mode titles that need a Pentium and an
FPU, exactly like the software `tombraid-586` row.

#### Tomb Raider Gold, 3dfx build

- Location: `.bench/tombraid3d_c`. Disc: `.bench/tombraid_cd/tombeng.cue` --
  the SAME image `tombraid-586` mounts, because it is the same game and the
  same pressing. Mounted read-only, never copied per run.
- Invocation: `--cpu 586 --memory-mib 64 --video vega --hdd-folder <fresh copy>
  --cd-image .bench/tombraid_cd/tombeng.cue --cycles 19000000000
  --inject-keys 5000000000:{esc} --presented-ppm <path>`.
- The tree is `.bench/tombraid_c` with its game directory swapped for the
  packagers' `TOMB3D`, Windows `.dll` files removed. `TOMBPATH.TXT` in `C:\`
  holds `C:\GAMES\TOMB3D` with NO trailing newline; CONFIG.SYS and AUTOEXEC.BAT
  are the software row's, with LF line endings, and carry NO CD driver or
  `IZCDEX` line -- see that row's entry for why.
- Measured timeline: boot 0-4 guest seconds, Glide splash 5-9, a BLACK WAIT
  9-24, title 35-50, attract DEMO 50-85, title 85-100, DEMO 100-130, and so on
  in that rhythm. The 19e9 budget is 114.5 guest seconds and lands 14 seconds
  into the second demo. rt 0.87, 132 s wall.

#### Descent II, 3dfx patch

- Location: `.bench/descent2_c`. Disc: `.bench/descent2_cd/DESCENT_II.cue`
  (691 MB), REQUIRED for Redbook audio and the game's own CD check.
- Invocation: `--cpu 586 --memory-mib 64 --video vega --hdd-folder <fresh copy>
  --cd-image .bench/descent2_cd/DESCENT_II.cue --cycles 9000000000
  --inject-keys "3000000000:{esc};4000000000:{esc}" --presented-ppm <path>`.
- AUTOEXEC runs `D2_3DFX.EXE -nomovies -autodemo`. `-autodemo` plays the
  shipped `DEMOS\DESCENT2.DEM`, so the row needs no gameplay input schedule --
  only the two keys that clear the release-notice screen.
- The tree does NOT carry the three `.MVL` movie files. They are 220 MB of the
  source tree's 266 MB, `-nomovies` never opens them, and the scoreboard
  robocopies the whole fixture per run.
- Measured timeline: boot 0-4, splash 5-10, release notice 23-24, demo from 29
  onward and still running past 170. The 9e9 budget is 54.2 guest seconds and
  lands 25 seconds into the demo. rt 0.32, 170 s wall.

#### THE TRAP THAT COST MOST OF A SESSION

**Both 3dfx builds wait for a keypress on a screen that shows nothing useful,
and a run without an input schedule reads exactly like a hung emulator.**

Tomb Raider's wait screen is BLACK: zero non-black pixels, zero Distira
register traffic, zero CD access, and the CPU retiring 500 million instructions
per guest second across 81,236 distinct addresses. Every device counter freezes
and stays frozen from 1.5e9 cycles to 20e9. Descent II's is a page of green
text that stops mid-word.

That signature was chased through a JIT hypothesis (refuted: `--interpreter`
reproduces it with the SAME published frame hash), a texture-aperture
hypothesis (refuted: 86Box's `voodoo_tex_writel` refuses the identical writes),
the memory FIFO, and Glide's own init trace (which completes cleanly and prints
nothing after). The answer was one Escape key.

Before diagnosing a DOS 3D title as hung, spend one run on `--inject-keys`.

#### The input schedules are minimal ON PURPOSE

Tomb Raider gets ONE Escape and Descent II gets TWO, and both numbers were
measured rather than chosen.

- Tomb Raider ignores input until somewhere between 3.5e9 and 4e9 cycles, and
  any key from 4e9 to at least 6e9 works. 5e9 keeps roughly a quarter of the
  window in hand on the early side. A SECOND key lands on the title screen,
  opens the ring menu, and the attract demo then never starts at all -- the row
  would pin a still picture and look perfectly healthy doing it.
- Descent II needs two Escapes to clear its notice screen. A THIRD lands after
  the demo has started and raises "ABORT AUTODEMO?", which the demo survives
  but which puts a dialogue box in the middle of the graded frame.

#### Grading

Both rows grade the PUBLISHED frame (`gradePresentedFrame = $true`), not a
re-render. On a double-buffered Voodoo a re-render reports the buffer nobody is
looking at; `--presented-ppm` reports what a user sees. See the psycho-486
block for the general argument and the measurement behind it.

Both use a FRAME CONTRACT rather than an end-of-budget hash, for the reason
`tombraid-586` lost its own: the end frame is mid-demo, where a cadence change
moves it legitimately. The anchor is the Toka-DOS boot text at 0.5e9, two
phases for the cursor blink -- the same anchor as the software row, because the
two trees boot the same DOS and their first four guest seconds are
byte-identical to each other.

The class check is `active_display = Distira` with `legacy_video_mode = Text`.
Together those two say "the Voodoo is driving the screen and the VGA card is
sitting in text mode behind it", which is the Glide state and nothing else.

Determinism: two repeat runs from a fresh copy are bit-identical on both rows.

| row | non-black | colours | retired instructions |
|---|---|---|---|
| `tombraid3d-586` | 305,875 / 307,200 (99.57%) | 365 | 19,864,778,122 |
| `descent2-3dfx-586` | 255,267 / 307,200 (83.09%) | 834 | 8,579,000,326 |

### Tyrian 2000 (Loudness audio clock + DPMI16, 486 and 586)

Two fixtures, three rows, added 2026-08-29. They exist because Tyrian's
Loudness driver is a different failure surface from every other row: it
reprograms PIT channel 0 (mode 3, reload 0x4300, ~70 Hz) once per video frame
from its main loop, paces music as MPU-401 UART MIDI through the wavetable
part at P300, and keeps a single-cycle SB DSP block chain (command 0x14, 384
samples at 10989 Hz) re-armed off the same clock. The 2026-08-28 PIT
write-edge bug (a control word that raises OUT low-to-high produced no IRQ0
edge; fixed in `854237ed`) silenced all of it while every scoreboard row
stayed green — no other fixture drives the timer that way.

- Location: `.bench/tyrian_setup_c` (SETUP.EXE) and `.bench/tyrian_c`
  (TYRIAN.EXE, the launcher for `file0001.exe`, Borland DPMI16). Both trees
  carry the owner's `TYRIAN.CFG` (music "Midi 300h", effects SoundBlaster) and
  `TYRIAN.SAV`; AUTOEXEC sets BLASTER and loops the program.
- Rows and exact invocations: the fixture table in
  `scripts/run-fixture-scoreboard.ps1` (`tyrian-setup-486`, `tyrian-486`,
  `tyrian-586`). Key/mouse offsets are CPU cycles: 66 M/guest-second at 486,
  166 M at 586.
  - `tyrian-setup-486`, 4.7e9 (~71 guest s): sit on the settings menu (music
    must KEEP PLAYING there — that silence was the owner's first symptom),
    five {down} to Jukebox at ~25 s, {enter}, jukebox to ~59 s, {esc} back.
    End frame = the static settings menu, exact hash in the sidecar.
  - `tyrian-486`, 3.2e9 (~48.5 guest s): title -> Start New Game -> 1 Player
    Full Game -> episode -> difficulty -> station menu -> Start Level, left
    mouse button HELD from ~31 s. The stationary ship dies at ~53 s, so the
    budget ends ~4.5 s before that, inside gameplay; no end-frame hash.
  - `tyrian-586`, 8.05e9: the same schedule at 586 guest-second offsets. The
    PERF row: the owner reports ~10% realtime loads at this persona.
- Oracle: PROFILE BANDS (`profileBands` in the fixture table, graded in
  `Invoke-Fixture`) on `timer.irq0_edges`, `mpu.wavetable.data_writes` and
  `sb_dsp.command_bytes`, plus the `cycle_limit` stop; the setup row adds the
  end-frame hash. The bands are liveness floors, NOT cadence pins: a starved
  70 Hz clock reads ~100 IRQ0 edges total against a floor of 3500 (the ~35x
  primary discriminator); the MIDI/DSP byte floors sit between the two arms
  with thinner margins (per-row derivations beside each row in the fixture
  table) and catch a music-only collapse. rt is the performance measurement,
  never asserted.

## Traps (these bit prior rounds)

0. COUNT-ONLY FRAMEBUFFER INVARIANTS DO NOT DISCRIMINATE. Grand Prix 2 under
   JEMMEX and under TokaEMM produces the SAME non-zero pixel count (307,152)
   and the SAME colour count (199) from DIFFERENT frames -- the sha256 values
   are `1e594bdf...` and `1006e751...`. A fixture pinned on counts alone would
   have passed a real change silently. Every graphics-mode fixture here records
   a sha256 of the result PPM; compare that, and keep the counts only as the
   human-readable summary. This trap is why the 2026-08-18 redesign of the
   tombraid-586 and nascar-586 rows did NOT simply drop their frame hash the way
   duke3d could: it moved the exact-frame check to an early anchor where the
   picture is not animating, and pairs the end-of-budget bands so that neither
   count stands alone (nascar's mid-load screen is inside the colour band and is
   rejected only by coverage).

1. FIXED-CYCLE-BUDGET wall A/B is confounded by the idle tail whenever
   guest timing differs between legs (bit round 2: +3.1% at fixed 8G was
   the tail, not the code). A/B at EQUAL GUEST EVENT with minimal tail,
   or compare per-instruction wall + demo-phase wall. The harness below
   uses the equal-guest-event form (the demo completes in every leg; cut
   the budget to just past demo completion).
2. The benches (sieve/whetstone/dhrystone/fp-mandel) are tiny real-mode
   payloads that do NOT exercise the drawcolumn shape. They prove the
   interpreter is unchanged (guest columns byte-identical) but say
   nothing about Doom's wall win. Doom is the wall target. HISTORY: this
   trap was written when "the wall win" meant the region JIT specifically;
   that backend is REMOVED as of the dynarec-refactor Task 2 commit, and
   Doom's role as the wall target is unrelated to which backend is active.
3. The benches are ~100% decode-hit; a warm-fetch dial moves them. So a
   timing-dial change that looks right on Doom can shift the benches.
   Keep both in view when touching dials.
4. Headless wall OMITS audio synthesis (render_audio is pulled only by the
   GUI audio thread). Device-level sound emulation still runs headless
   (guest-visible state exact). Verify near-target milestones against a
   real GUI run.
5. cargo -j8 ALWAYS. Full-core builds crash the owner's apps (and a render
   in progress). Never raise the parallelism.

## The A/B recipe (equal guest event, interleaved)

The kill gate. v2 must WIN wall vs merged main (origin/main), not tie.

1. Build both exes (v2 feature branch, baseline origin/main).
2. Confirm determinism on the candidate exe against the CURRENT pinned
   identity at the top of this file. If any value drifts, STOP and
   investigate before trusting wall numbers. Do not copy literals from this
   recipe into gates — the top-of-file block is the single source of truth,
   and this step deliberately no longer quotes it (it has gone stale here
   twice: once at the phase-1/phase-2 boundary and once at the lane
   campaign's re-pin).
3. Interleave: run N rounds alternating baseline and v2, same env
   (HISTORY: this used to mean "armed" via IZARRAVM_JIT_REGION; that
   variable is gone along with the region JIT, so every leg is now simply
   unarmed), capturing wall time per leg. Equal guest event (the demo
   completes in every leg), minimal idle tail.
4. Best-of and mean. v2 must be measurably faster (target ~10% Doom wall
   from ~2-3x on the loop). A result inside the noise floor is a TIE:
   the JIT thesis for this loop shape is refuted; record and pivot.

## Kill-if-it-loses rule (HISTORICAL — this call was already made)

The region JIT (v1 and v2 both) is REMOVED as of the dynarec-refactor
Task 2 commit; this whole rule describes a decision already taken, kept
verbatim for the record, not a live procedure to re-run.

If v2 ties the interpreter again: the call-overhead floor (one
region_inline_slot call per inline slot + the flag-helper calls for
add/shr) is the limiting factor, not the decode dispatch. Record the
refutation in RESULTS.md and pivot to the 586 dial recalibration round
(the brief's alternative): restore era-apparent Doom speed via the
warm-fetch charge calibration, keeping Quake 42.4 fps exact.

## peachdrm-586 (the pure-B2 fixture, added 2026-08-17)

Peach's Dream (1999), 16-bit real-mode, mode 13h. The corpus's B2-alone
exemplar chosen for the B2 admission campaign: at 60 guest seconds it
retires 11,209,183,121 instructions of which 2,634,366,903 decline
dispatch (23.5%) and 7,914,139,724 execute as sixteen-bit native (insns
per entry 12.0 in the stage-1 profile, the highest of the B2-alone set).
Both counters are BIT-IDENTICAL across back-to-back runs at 20 and 60
guest seconds, measured on main a4ed9935.

Invocation (EXACT; the key schedule is part of the pin):

    izarravm --cpu 586 --memory-mib 64 --video vega
        --hdd-folder <fresh copy of .bench/peachdrm_c>
        --cycles 9960000000
        --inject-keys "996000000:1;1162000000:\r;1992000000:\r;3320000000:{space};4980000000:\r;6640000000:{space};8300000000:\r"
        --profile-json <out>

Copy the tree fresh per run like every fixture. Pinned invariants at
this budget on a4ed9935: perf.instructions = 11209183121,
jit_direct_dispatch_declines = 2634366903 (guest-deterministic; a move
means guest state changed). rt reference ~0.33 at 586 (MEASUREMENT, not
a pin). The A/B currency for the admission lever is
jit_direct_dispatch_declines and wall per PROTOCOL rules; rank 16-bit
work by insns/entry, never coverage.

Provenance: eXoDOS "Peach's Dream (1999)" (READ-ONLY corpus), stage-1
archive D:\exo-stage1\passB-20260816\PeachDrm (translate.json carries
the recipe), screened against GameRobo and Redhook (also deterministic;
PeachDrm won on insns/entry and decline density).
