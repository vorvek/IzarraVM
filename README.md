# VirtualDOS

VirtualDOS is a Rust-based, games-first DOS emulator project. The initial target is early-90s DOS game compatibility, with Wolfenstein 3D-era hardware as the first practical milestone:

- 386 real-mode CPU execution and timing tests first.
- VGA Mode 13h and ET4000-style SVGA boundaries first.
- PC speaker, AdLib/OPL, Sound Blaster, and MIDI backend boundaries.
- Keyboard, mouse, DOS joystick, and future optional Steam Input support.
- Host-folder DOS `C:` mounts, with no virtual disk image requirement.

The repository is MIT licensed. FreeDOS may be used as a behavioral reference, but this project does not copy GPL FreeDOS code.

## Development Policy

Active development is Windows-first for now, matching the local DOS library and current workstation. Keep the architecture portable: use cross-platform crates for windowing, graphics, audio, MIDI, input, paths, and process boundaries; avoid Windows-only APIs unless they are isolated behind a backend trait.

## Current State

This is an early emulator scaffold. It validates TOML/CLI configuration, includes local compatibility-corpus scanning tools, and boots a clean-room 80386DX-25 test ROM far enough to show deterministic VGA text-mode compatibility checks. It does not run DOS games yet.

## Quick Start

```powershell
cargo run -p virtualdos -- --headless-config-check
cargo run -p virtualdos -- --headless-test-rom
cargo run -p virtualdos -- --headless-boot-suite
cargo run -p virtualdos -- --config examples/virtualdos.toml
```

For non-Windows hosts later, replace the `c_drive` path in `examples/virtualdos.toml` or pass `--c-drive /path/to/dosroot`.

## Compatibility Corpus

The local DOS collection is treated as read-only and machine-local. Configure it with either `VIRTUALDOS_DOSROOT` or `--dosroot`:

```powershell
$env:VIRTUALDOS_DOSROOT = 'R:\La Colección by Neville\dosroot\'
cargo run -p virtualdos-corpus -- --output corpus-output\scan.json
```

Generated corpus output belongs in ignored local directories such as `corpus-output/` or `compatibility-output/`.

## Validation

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```


## Objectives
CPU levels: 
1) i386 @ 25MHz
2) 486DX2 @ 66MHz
3) Pentium 133MHz
4) Pentium MMX 233Mhz

GPU levels:
1) Tseng Labs ET4000AX
2) S3 Trio
3) Voodoo 2 single/SLI

RAM:
Manually selectable from 1MB to 128MB

Sound:
1) PC Speaker
2) Adlib / OPL2
3) Sound Blaster 16 / OPL3
4) MIDI