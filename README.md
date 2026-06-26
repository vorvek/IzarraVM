# IzarraVM

IzarraVM is a Rust emulator for the Izarra 3000, a fictional DOS-era games
computer that almost shipped in 1997. It models one fixed machine: custom video
and audio around an MS-DOS compatible core, with Toka Disk System as its ROM
shell and launcher.

The goal is to run early to mid 1990s DOS games as if the Izarra had reached
store shelves. It does not run retail DOS games yet, but it boots the Izarra
BIOS, enters Toka-DOS, runs the ICOMMAND shell and bundled tools, and exercises
most of the machine through headless boot tests.

## Fictional Origin

There was never a real Izarra 3000. In the project's fiction, Izarra Computer
Systems started in 1992 as a small Spanish workstation shop that built graphics
terminals for schools and local studios. Its engineers wanted a home computer
that could run DOS games without feeling like a beige PC, so they designed a
tight motherboard around VGA, MIDI, CD-ROM audio, and a friendly ROM shell.

The prototype was fast, but the timing was brutal. Windows 95 made compatibility
the only spec retailers cared about, so Izarra kept adding bridge chips and
fallback modes to reassure publishers. The board became expensive and late. In
April 1997, with the first production run still in testing and suppliers asking
for cash, the company filed for bankruptcy. In this fiction, IzarraVM is what
survived in the lab notes.

## Fictional Tech Specs

The emulator targets one fixed machine. None of this is user-selectable; the
point is to make the Izarra 3000 feel like a real, opinionated computer.

| Area | Izarra 3000 hardware |
| --- | --- |
| CPU | GSW-586, a K6-class 266 MHz part on a 66 MHz bus. Toka can throttle it to 486DX2 66 MHz or 386DX 25 MHz without rebooting. |
| Memory | 24 MB SDRAM, with Toka mapping itself out of conventional memory when DOS games need the first 640 KB. |
| Graphics | VEGA chipset: Margo 2D, Distira 3D, 4 MB video memory, VESA VBE 2.0, VGA mode 13h, and up to 1024x768 at 32-bit color. |
| Sound | Resonique 2: Sound Blaster 16 compatible digital audio, OPL3 FM, MPU-401 MIDI, wavetable daughterboard, and Yamaha ADPCM-B playback. |
| Storage | 3.6 GB IDE hard disk, 12x ATAPI CD-ROM with CD audio, and a 1.44 MB floppy drive. |
| Display | 15-inch CRT, tuned for 320x200 DOS modes and 1024x768 desktop output. |
| Firmware | 2 MB ROM with the Izarra BIOS, Toka Disk System, ICOMMAND, and bundled tools. |
| I/O | PS/2 keyboard and mouse, serial, parallel, VGA, line out, line in, and MIDI/game port. |

## Current State

IzarraVM is early. The emulator boots its own BIOS and Toka-DOS path, and some
simple DOS software may work, but it is not a dependable DOS game runner yet.

## Quick Start

```powershell
cargo run -p izarravm -- --headless-config-check
cargo run -p izarravm -- --headless-test-rom
cargo run -p izarravm -- --headless-boot-suite
cargo run -p izarravm -- --config examples/izarravm.toml
```

For non-Windows hosts later, replace the `c_drive` path in
`examples/izarravm.toml` or pass `--c-drive /path/to/dosroot`.

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
