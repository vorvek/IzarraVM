# VirtualDOS

VirtualDOS is a Rust-based, games-first DOS emulator project. The initial target is early-90s DOS game compatibility, with Wolfenstein 3D-era hardware as the first practical milestone:

- 386 real-mode CPU execution and timing tests first.
- VGA Mode 13h and ET4000-style SVGA boundaries first.
- PC speaker, AdLib/OPL, Sound Blaster, and MIDI backend boundaries.
- Keyboard, mouse, DOS joystick, and future optional Steam Input support.
- Host-folder DOS `C:` mounts, with no virtual disk image requirement.

The repository is MIT licensed. FreeDOS may be used as a behavioral reference, but this project does not copy GPL FreeDOS code.

## Current State

This is a scaffold. It opens a native `winit` window, validates TOML/CLI configuration, initializes placeholder emulator subsystems, and includes local compatibility-corpus scanning tools. It does not run DOS games yet.

## Quick Start

```powershell
cargo run -p virtualdos -- --headless-config-check
cargo run -p virtualdos -- --config examples/virtualdos.toml
```

On Linux, replace the `c_drive` path in `examples/virtualdos.toml` or pass `--c-drive /path/to/dosroot`.

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
