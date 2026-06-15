# IzarraVM

IzarraVM is a Rust emulator for the Izarra 3000, a fictional late-1990s
computer. The Izarra 3000 is a fully integrated machine in the spirit of an Amiga
or a Mac: fixed custom hardware running its own GUI operating system, Belunza Disk
System, over an MS-DOS compatible core. The emulator reproduces that one machine
rather than offering a configurable PC.

The aim is to run early to mid 1990s DOS games the way the Izarra would have run
them. It does not run DOS games yet. Today it boots clean-room test ROMs and
validates configuration and the local game corpus.

There was never a real Izarra 3000. In the project's fiction the company went
bankrupt in early 1997, before the machine shipped, which is also why it is a good
DOS computer that arrived just as Windows 95 took over the market.

## The Izarra 3000

Fixed hardware. None of it is user-selectable; the point of the machine is that it
is one tight design.

- CPU: GSW-586, a Pentium and K6 class part at 200 MHz on a 66 MHz bus. Two
  backward compatibility modes throttle it to behave like the earlier machines in
  the line: 486DX2 at 66 MHz (the Izarra 2000) and 386DX at 25 MHz (the Izarra
  1000), switchable from Belunza without a reboot.
- Memory: 24 MB SDRAM.
- Graphics: VEGA, an integrated chipset built from a 2D chip (Margo) and a
  Glide-capable 3D chip (Distira). 4 MB shared, VESA VBE 2.0, up to 1024x768 at
  32-bit color.
- Sound: Resonique 2, compatible with the Sound Blaster 16, with an OPL3 FM chip,
  an MPU-401 MIDI port, and a wavetable daughterboard.
- Storage: 3.6 GB IDE hard disk, 12x CD-ROM with CD audio, 1.44 MB floppy.
- Display: 15-inch CRT, up to 1024x768.
- Firmware: a 2 MB ROM holding a simplified custom BIOS and Belunza Disk System.

Games see a DOS environment. Belunza runs an MS-DOS 6.1 compatible core with the
usual memory managers, and maps itself out of conventional memory so games that
need most of the first 640 KB (Wing Commander, for one) still get it.

## Implementation order

One machine, built in layers. The rough sequence:

- CPU: a 386 core first, which doubles as the Izarra 1000 compatibility mode, then
  486DX2 behavior, then the full GSW-586.
- Graphics: VGA text and Mode 13h, then VESA SVGA on Margo, then the Distira 3D
  unit and the Glide path.
- Sound: PC speaker, then OPL3 and Sound Blaster 16, then MPU-401 MIDI and
  wavetable.
- Host integration: keyboard and mouse, DOS joystick, and host-folder C: mounts
  with no virtual disk image required.

## Development Policy

Active development is Windows-first for now, matching the local DOS library and
current workstation. Keep the architecture portable: use cross-platform crates for
windowing, graphics, audio, MIDI, input, paths, and process boundaries, and avoid
Windows-only APIs unless they are isolated behind a backend trait.

## Current State

This is an early emulator scaffold. It validates TOML and CLI configuration,
includes local compatibility-corpus scanning tools, and boots a clean-room 80386
test ROM far enough to show deterministic VGA text-mode checks. It does not run DOS
games yet.

## Quick Start

```powershell
cargo run -p izarravm -- --headless-config-check
cargo run -p izarravm -- --headless-test-rom
cargo run -p izarravm -- --headless-boot-suite
cargo run -p izarravm -- --config examples/izarravm.toml
```

For non-Windows hosts later, replace the `c_drive` path in
`examples/izarravm.toml` or pass `--c-drive /path/to/dosroot`.

## Compatibility Corpus

The local DOS collection is treated as read-only and machine-local. Configure it
with either `IZARRAVM_DOSROOT` or `--dosroot`:

```powershell
$env:IZARRAVM_DOSROOT = 'R:\La Colección by Neville\dosroot\'
cargo run -p izarravm-corpus -- --output corpus-output\scan.json
```

Generated corpus output belongs in ignored local directories such as
`corpus-output/` or `compatibility-output/`.

## Validation

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

## License

GNU GPL v3.0 only (see [LICENSE](LICENSE)). The subsystems are clean-room
reimplementations built from primary hardware/OS documentation; reference
implementations (e.g. FreeDOS for the DOS layer, Nuked-OPL3 for audio) are used
only as behavioral oracles to check assumptions, never copied or translated.
