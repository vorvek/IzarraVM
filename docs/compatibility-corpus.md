# Compatibility Corpus

The game collection at `R:\La Colección by Neville\dosroot\` is a local, read-only compatibility corpus. It is not required by CI and must not be modified by tooling.

Use `IZARRAVM_DOSROOT` or `--dosroot` to point tools at the collection. Development is Windows-first for now; other hosts should use the same interface later with a host-local path for the same data.

The first corpus milestone is a curated Wolfenstein 3D-era shortlist. Full-library sweeps are local/manual until the emulator can produce useful pass/fail signals.

Generated scan manifests, run logs, screenshots, and notes belong in ignored directories:

- `corpus-output/`
- `compatibility-output/`
- `izarravm-output/`

## Running corpus programs

The Rust HLE single-program runner (`--headless-run`) has been retired with the
HLE. Run corpus programs through Katea (real FreeDOS) instead:

- **Crash-triage a game with its data folder:**
  `izarravm --hdd-folder <gamedir>` — boots real FreeDOS with the whole folder as
  C: and prints CS:IP / stop reason / the screen. Richer than the old exit code
  for finding where a launch faults.
- **Run a standalone program for its exit code:**
  `izarravm --katea-run <prog>` — boots a temp Katea disk, EXECs the program, and
  exits with its DOS exit code.

`izarravm-corpus` still scans the collection into a JSON manifest; it does not run
the emulator.
