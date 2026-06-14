# Compatibility Corpus

The game collection at `R:\La Colección by Neville\dosroot\` is a local, read-only compatibility corpus. It is not required by CI and must not be modified by tooling.

Use `VIRTUALDOS_DOSROOT` or `--dosroot` to point tools at the collection. Development is Windows-first for now; other hosts should use the same interface later with a host-local path for the same data.

The first corpus milestone is a curated Wolfenstein 3D-era shortlist. Full-library sweeps are local/manual until the emulator can produce useful pass/fail signals.

Generated scan manifests, run logs, screenshots, and notes belong in ignored directories:

- `corpus-output/`
- `compatibility-output/`
- `virtualdos-output/`
