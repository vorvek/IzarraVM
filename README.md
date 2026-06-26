# IzarraVM

IzarraVM is a Rust emulator for the Izarra 3000, a fictional late-1990s
computer. The Izarra 3000 is a fully integrated machine in the spirit of an Amiga
or a Mac: fixed custom hardware running its own GUI operating system, Toka Disk
System, over an MS-DOS compatible core. The emulator reproduces that one machine
rather than offering a configurable PC.

The aim is to run early to mid 1990s DOS games the way the Izarra would have run
them. It does not run retail DOS games yet, but it now boots the Izarra BIOS,
enters Toka-DOS, runs the ICOMMAND shell and bundled tools, and exercises most of
the machine through headless boot tests.

There was never a real Izarra 3000. In the project's fiction the company went
bankrupt in early 1997, before the machine shipped, which is also why it is a good
DOS computer that arrived just as Windows 95 took over the market.

## The Izarra 3000

Fixed hardware. None of it is user-selectable; the point of the machine is that it
is one tight design.

- CPU: GSW-586, a K6 class part at 266 MHz on a 66 MHz bus. Two
  backward compatibility modes throttle it to behave like the earlier machines in
  the line: 486DX2 at 66 MHz (the Izarra 2000) and 386DX at 25 MHz (the Izarra
  1000), switchable from Toka without a reboot.
- Memory: 24 MB SDRAM.
- Graphics: VEGA, an integrated chipset built from a 2D chip (Margo) and a
  Glide-capable 3D chip (Distira). 4 MB shared, VESA VBE 2.0, up to 1024x768 at
  32-bit color.
- Sound: Resonique 2, compatible with the Sound Blaster 16, with an OPL3 FM chip,
  an MPU-401 MIDI port, a wavetable daughterboard, and a Yamaha ADPCM-B DAC.
- Storage: 3.6 GB IDE hard disk, 12x CD-ROM with CD audio, 1.44 MB floppy.
- Display: 15-inch CRT, up to 1024x768.
- Firmware: a 2 MB ROM holding a simplified custom BIOS and Toka Disk System.

Games see a DOS environment. Toka runs an MS-DOS 6.22 compatible core with the
usual memory managers, and maps itself out of conventional memory so games that
need most of the first 640 KB (Wing Commander, for one) still get it.

## Current State

IzarraVM is in bring-up. It is past the blank-machine stage, but it is not a
general DOS game runner yet.

What works today:

- The clean-room Izarra BIOS boots, reports device status, and can enter the
  Toka-DOS boot path.
- Toka-DOS installs a host-folder C: drive, boots to the ICOMMAND prompt, and runs
  bundled tools such as VER, MEM, and IEMM.
- The CPU core covers a large 386 real/protected-mode subset plus selected 486 and
  GSW-586 features used by the fantasy machine.
- VGA text, mode 13h, planar paths, and the Margo 2D chip are modeled far enough
  for deterministic headless tests and the Toka-DOS shell.
- PC speaker, OPL3, Sound Blaster 16, and WSS/AD1848 paths exist, with DMA-backed
  playback coverage for the common SB paths. A Yamaha ADPCM-B streaming DAC
  (4-bit Yamaha/Creative/Y8950-family ADPCM, ported from the `superctr/adpcm`
  codec library) is wired as a fourth sound device at I/O 0x240 on its own IRQ/DMA.
- IDE hard disk, floppy, ATAPI CD-ROM, XMS, EMS, UMBs, keyboard, mouse, PIT, PIC,
  RTC, serial, and parallel devices are wired into the machine model.

The current headless gates pass. The boot suite still has a known red row for
Sound Blaster ADPCM, and the Distira 3D/Glide side of VEGA is still future work.
Game compatibility work should start from specific titles and turn their first
failure into a repeatable test.

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
