<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# <img src="docs/star.svg" alt="" height="30"> IzarraVM

IzarraVM is a Rust emulator for the Izarra 3000, a DOS-era games computer that
almost shipped in 1997. It models one fixed machine: custom video and audio
around an MS-DOS compatible core, booting Toka-DOS: a real FreeDOS-based
system running in virtual-8086 mode under the machine's own memory manager.

<p align="center">
  <img src="docs/izarra-3000-chassis.jpg" alt="The Izarra 3000 desktop tower" width="300">
  &nbsp;&nbsp;
  <a href="docs/izarravm-screenshot.png"><img src="docs/izarravm-screenshot.png" alt="The Izarra BIOS POST screen running in IzarraVM" width="420"></a>
</p>

The goal is to run early to mid 1990s DOS games as if the Izarra had reached
store shelves. A growing set of games already runs, and the throttled CPU modes
run at or near real-time on a modern host; the full-speed 586 mode is still
being tuned.

## Origin

Izarra Computer Systems started in 1987 as a small Spanish workstation shop that
built graphics terminals for schools and local studios. Its engineers wanted a
home computer that could run DOS games without feeling like a beige PC, and they
spent the next decade chasing that idea across three machines.

The Izarra 1000 arrived in 1990 around a 286 at 12 MHz, followed in 1993 by the
386-based Izarra 2000 at 25 MHz. Both sold modestly to the schools and studios
that already knew the brand. Work on the 3000 began in late 1994 as the most
ambitious of the three: a tight motherboard around VGA, MIDI, CD-ROM audio, and
a friendly ROM shell, fast enough to make DOS games feel at home.

The prototype was fast, but the timing was brutal. Windows 95 made compatibility
the only spec retailers cared about, so Izarra kept adding bridge chips and
fallback modes to reassure publishers. The board became expensive and late. In
April 1997, with the first production run still in testing and suppliers asking
for cash, the company filed for bankruptcy. IzarraVM is what survived in the lab
notes.

## Tech Specs

The emulator targets one fixed machine. None of this is user-selectable; it
reproduces the Izarra 3000 exactly as it was built.

| Area | Izarra 3000 hardware |
| --- | --- |
| CPU | GSW-586, a Pentium MMX at 200 MHz on a 66 MHz bus. The BIOS or the bundled GSWMODE tool can throttle it to a 486DX2 at 66 MHz, a 386DX at 22 MHz, or the same 386 ISA at 7.33 MHz without rebooting. |
| Memory | 24 MB SDRAM, with Toka mapping itself out of conventional memory when DOS games need the first 640 KB. |
| Graphics | VEGA chipset: Margo 2D with a 4 MB frame store; Distira 3D with a 2 MB framebuffer and 2 MB per TMU; VESA VBE 2.0, VGA mode 13h, and up to 1024x768 at 32-bit color. |
| Sound | ReSonique 2: Sound Blaster 16 compatible digital audio (PCM and Creative ADPCM), OPL3 FM, pin headers for a wavetable daughterboard, and a rear MPU-401/gameport. |
| Storage | 3.6 GB UDMA2 IDE hard disk on a PIIX4-compatible controller, 12x PIO ATAPI CD-ROM with CD audio, and a 1.44 MB floppy drive. |
| Display | 15-inch CRT, up to 1024x768@75hz. |
| Firmware | 2 MB ROM with the Izarra BIOS, Toka-DOS (FreeDOS-based), and bundled tools. |
| I/O | PS/2 keyboard and mouse, serial, parallel, VGA, line out, line in, and MIDI/game port. |

## How it works

The firmware is a clean-room BIOS with a graphical POST, a boot menu, and a
full setup panel. It boots a real FreeDOS kernel and shell (rebranded
Toka-DOS 3.0) inside virtual-8086 mode under TOKAEMM, a guest-side memory
manager that provides XMS, EMS, and UMBs through the CPU's own paging, the
same way a period memory manager did it. The C: drive is a folder on the host,
served to the guest as a real ATA disk; the classic external DOS tools
(XCOPY, ATTRIB, FIND, MORE, MEM, CHOICE, DELTREE, MOVE, SORT, LABEL, and more)
ship on it, built from FreeDOS sources or written for the project.

## Current State

The emulator boots to a usable DOS with sound, mouse, CD-ROM (ISO, CUE/BIN, or
a host folder mounted as a disc), and floppy images. Legacy video personalities
(CGA, EGA, VGA, Hercules) are modeled down to the register level, and the
display goes through an optional, subtle CRT shader. Plenty of games run;
plenty more don't yet. Compatibility work is ongoing.

## Quick Start

```powershell
cargo run -p izarravm -- --headless-config-check
cargo run -p izarravm -- --headless-test-rom
cargo run -p izarravm -- --headless-boot-suite
cargo run -p izarravm -- --config examples/izarravm.toml
```

For non-Windows hosts later, replace the `c_drive` path in
`examples/izarravm.toml` or pass `--c-drive /path/to/dosroot`.

By default the C: drive, `cmos.bin`, and `izarravm.conf` live under the per-user
`~/.izarravm` directory, so launching the binary from any folder leaves nothing
behind in the working directory. Pass `--portable` to keep them in a `c_drive`
beside the executable instead, for a self-contained install.

## Optional MT-32 and Glide files

For convenience, IzarraVM can route the Izarra 3000's rear MPU-401 connection
through Munt. Munt needs one control ROM and its matching PCM ROM:

| Module | Control ROM | PCM ROM |
| --- | --- | --- |
| MT-32, old generation | `ctrl_mt32_1_04.rom`, `ctrl_mt32_1_05.rom`, `ctrl_mt32_1_06.rom`, or `ctrl_mt32_1_07.rom` | `pcm_mt32.rom` |
| MT-32, new generation | `ctrl_mt32_2_04.rom`, `ctrl_mt32_2_06.rom`, or `ctrl_mt32_2_07.rom` | `pcm_mt32.rom` |
| CM-32L | `ctrl_cm32l_1_00.rom` or `ctrl_cm32l_1_02.rom` | `pcm_cm32l.rom` |
| CM-32LN | `ctrl_cm32ln_1_00.rom` | `pcm_cm32l.rom` |

Settings accepts arbitrary control and PCM paths, so the archive filenames can
be selected directly. For automatic discovery, copy and rename an MT-32 pair
to `~/.izarravm/MT32_CONTROL.ROM` and `~/.izarravm/MT32_PCM.ROM`. Copy and
rename a CM-32L or CM-32LN pair to `~/.izarravm/CM32L_CONTROL.ROM` and
`~/.izarravm/CM32L_PCM.ROM`. Discovery is ASCII case-insensitive and is disabled
under `--portable`; paths selected in Settings still work. The source names in
the table match the `mt32pi` directory in the [Roland MT-32 ROMs
archive](https://archive.org/details/Roland-MT-32-ROMs).

Many DOS Glide games already include `GLIDE2X.OVL`; that game-local copy takes
priority. Otherwise, place a compatible Voodoo Graphics OVL at
`~/.izarravm/GLIDE2X.OVL`. IzarraVM finds the name without regard to ASCII case
and exposes it through `C:\DOS` as the global `PATH` fallback. A
[3DBVoodoo2 driver-disc image](https://archive.org/details/3-dbvoodoo-2_202302)
is archived for reference. IzarraVM does not bundle a replacement or
diagnostic OVL when neither copy is present.

IzarraVM neither downloads nor redistributes ROMs, Glide drivers, or game data.
Archive item metadata is not a license. Use these files only when you have the
lawful right to do so.

## Validation

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

## License

GNU GPL version 3 only (`GPL-3.0-only`; see [LICENSE](LICENSE)).
