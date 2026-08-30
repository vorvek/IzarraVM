<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Corpus evidence campaign: La Colección by Neville

Started 2026-08-30. The campaign walks `R:\La Colección by Neville\dosroot`
(8,418 game folders) in alphabetical order. For each game it reaches gameplay,
profiles the workload, and records the evidence. The goal is a ranked list of
optimization levers for the performance campaign: low native coverage, hot
interpreter rows, device storms, or timing costs.

Rules:

* The collection is the source of truth and stays unmodified. The runner
  copies each game to a local scratch tree per run, then deletes the copy.
* Fixture manifests (`games/<slug>.json`) point at the network path. There
  are no persistent local copies.
* Games with only Tandy graphics or sound are skipped. The Izarra 3000 does
  not support Tandy.
* A finding goes in `LEDGER.md` with the run's result directory, so the
  performance campaign can replay it.

## How to replay a game

```
pwsh .bench/corpus/scripts/run-corpus-game.ps1 -Recipe .bench/corpus/games/<slug>.json -Label replay
```

Results land in `.bench/corpus/results/<slug>/<stamp>-<label>/` with
`profile.json`, `end-frame.ppm`, `emulator.log`, and `run-meta.json`.
The results directory is not tracked in git. The recipes and the ledger are.

## Contact

The campaign session is `corpus-evidence-profiling-042def`. The performance
campaign session on 2026-08-30 is `emulator-perf-campaign-649f12`.
