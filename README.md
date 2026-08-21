<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# <img src="docs/star.svg" alt="" height="30"> IzarraVM

IzarraVM is a Rust emulator for the Izarra3000, a DOS-era workstation that
almost went on sale in 1997. It emulates one fixed machine: custom video and
audio around an MS-DOS compatible core. The machine boots Toka-DOS, a real
FreeDOS-based system. Toka-DOS runs in virtual-8086 mode, below the memory
manager of the machine.

<p align="center">
  <img src="docs/izarra-3000-chassis.jpg" alt="The Izarra3000 desktop tower" width="300">
  &nbsp;&nbsp;
  <a href="docs/izarravm-screenshot.png"><img src="docs/izarravm-screenshot.png" alt="The Izarra BIOS POST screen running in IzarraVM" width="420"></a>
</p>

IzarraVM runs DOS games from the early 1990s and the middle 1990s, as the
Izarra3000 ran them. It already runs much of the software of that period, and
the work continues.

## Origin

Izarra started in Zamudio, Bizkaia, in 1982. Mikel Etxeberria did the hardware
work and the repairs. Txema Goikoetxea did the work with the customers and the
parts, and collected the invoices. The first products were electronic
typewriters. Izarra gave local service with them, thus a customer did not have
to send a broken machine away for some weeks. Izarra also made parts for
Olivetti, and that work gave a constant income.

In the late 1980s, the same customers asked about PCs. Izarra did not put its
name on AT compatibles. It made custom workstations. The machines kept the
usual DOS software, but Izarra built its own hardware and its own operating
system around that software. This made the machines different from the others,
and some Spanish magazines called Izarra "the Basque Amiga". It also made the
machines expensive.

The computer line started with the Izarra1000 in 1990, which used a 286. The
Izarra2000 followed in 1992, with a 386. The Izarra2700 followed in 1994, with
a 486. Each machine went further from office work. Better graphics, better
sound, and CD-ROM drives made multimedia and home use more practical.

The Izarra3000 was the largest project of the line. It was a custom 586-class
workstation, with its own BIOS, Toka-DOS 3.0, VEGA graphics, and ReSonique II
sound. But the market changed before the machine was ready. Buyers wanted
Windows 95, and the machine could not run it. MS-DOS compatibility was no
longer sufficient for the cost of the custom design. The Izarra3000 did not
reach the stores.

Photos, manuals, BIOS dumps, and incomplete developer notes remain. IzarraVM
keeps the machine from those records.

## Tech Specs

The emulator has one fixed machine. You cannot select different hardware. The
emulator gives the Izarra3000 as it was built.

| Area | Izarra3000 hardware |
| --- | --- |
| CPU | GSW-586 at 166 MHz, on a 66 MHz bus. It has a 32 KB L1 cache and a 512 KB L2 cache. It is a Pentium-class part with an x87 unit and no SIMD extension. The BIOS or the GSWMODE tool can set it to a 486DX2 at 66 MHz, a 386DX at 22 MHz, or the same 386 instruction set at 7.33 MHz. A restart is not necessary. |
| Memory | 64 MB PC100 SDRAM. |
| Graphics | VEGA chipset: Margo 2D with a 4 MB frame store; Distira 3D with a 2 MB frame buffer and 2 MB for each TMU; VESA VBE 2.0, VGA mode 13h, and a maximum of 1024x768 at 32-bit color. |
| Sound | ReSonique II: Sound Blaster 16 compatible digital audio (PCM and Creative ADPCM), OPL3 FM, pin headers for a wavetable daughterboard, and a rear MPU-401/gameport. |
| Storage | A 3.6 GB UDMA2 IDE hard disk on a PIIX4-compatible controller, a 12x PIO ATAPI CD-ROM with CD audio, and a 1.44 MB floppy drive. |
| Display | 15-inch CRT. The maximum mode is 1024x768 at 75 Hz. |
| Firmware | 2 MB ROM with the Izarra BIOS, Toka-DOS, and the supplied tools. |
| I/O | PS/2 keyboard and mouse, serial, parallel, VGA, line out, line in, and MIDI/game port. |

## Current State

The emulator boots to a DOS that you can use. It has sound, a mouse, a CD-ROM,
and floppy images. The CD-ROM accepts an ISO, a CUE/BIN pair, or a host folder
as a disc. The emulator models the legacy video personalities (CGA, EGA, VGA,
Hercules) at the register level. The display can go through a CRT shader,
which is optional and light. The compatibility is wide, and the work
continues.

If a game does not run, or if it runs badly with no clear cause,
[open an issue](https://github.com/vorvek/IzarraVM/issues). But first read this
limit. The Izarra3000 is a 166 MHz Pentium-class machine. Software that needed
more than that in the period also needs more than that here. 3D at a high
resolution and a high frame rate is more than the hardware can do. A late game
that recommended a Pentium II or a faster CPU is also more than the hardware
can do. These are limits of the hardware, and not defects in the emulation.

## Quick Start

```powershell
cargo run -p izarravm -- --headless-config-check
cargo run -p izarravm -- --headless-test-rom
cargo run -p izarravm -- --headless-boot-suite
cargo run -p izarravm -- --config examples/machine.toml
```

On a host that is not Windows, change the `c_drive` path in
`examples/machine.toml`. You can also use `--c-drive /path/to/dosroot`.

By default, the C: drive, `cmos.bin`, and `izarravm.conf` are in the
`~/.izarravm` directory of the user. Controller profiles are in
`~/.izarravm/Controller Profiles`, and screenshots are in
`~/.izarravm/screenshots`. Thus the program writes nothing into the working
directory. Use `--portable` to keep the C: drive, `cmos.bin`, and
`izarravm.conf` beside the executable. Controller profiles and screenshots
stay in the application state directory.

## Optional MT-32 and Glide files

IzarraVM can send the rear MPU-401 connection of the Izarra3000 to Munt. Munt
needs one control ROM, and the PCM ROM of the same machine:

| Module | Control ROM | PCM ROM |
| --- | --- | --- |
| MT-32, old generation | `ctrl_mt32_1_04.rom`, `ctrl_mt32_1_05.rom`, `ctrl_mt32_1_06.rom`, or `ctrl_mt32_1_07.rom` | `pcm_mt32.rom` |
| MT-32, new generation | `ctrl_mt32_2_04.rom`, `ctrl_mt32_2_06.rom`, or `ctrl_mt32_2_07.rom` | `pcm_mt32.rom` |
| CM-32L | `ctrl_cm32l_1_00.rom` or `ctrl_cm32l_1_02.rom` | `pcm_cm32l.rom` |
| CM-32LN | `ctrl_cm32ln_1_00.rom` | `pcm_cm32l.rom` |

In **Settings**, select **MIDI emulation...**. In **MIDI EMULATION**, select
**Munt (MT-32)**. The ROM selectors then appear. Each selector accepts any
control path or PCM path.
Thus you can select the archive file names directly. For automatic discovery,
copy an MT-32 pair and give the copies the names
`~/.izarravm/MT32_CONTROL.ROM` and
`~/.izarravm/MT32_PCM.ROM`. For a CM-32L or a CM-32LN pair, use the names
`~/.izarravm/CM32L_CONTROL.ROM` and `~/.izarravm/CM32L_PCM.ROM`. The discovery
ignores the letter case of ASCII characters. `--portable` disables the
discovery, but a path in **MIDI EMULATION** continues to operate. The names
in the table are the names in the `mt32pi` directory of the
[Roland MT-32 ROMs archive](https://archive.org/details/Roland-MT-32-ROMs).

Many DOS Glide games include a `GLIDE2X.OVL` file, and that local copy has
priority. If a game has no copy, put a compatible Voodoo Graphics OVL at
`~/.izarravm/GLIDE2X.OVL`. IzarraVM finds the name without the letter case of
ASCII characters. It makes the file available through `C:\DOS`, as the global
`PATH` fallback. A
[3DBVoodoo2 driver-disc image](https://archive.org/details/3-dbvoodoo-2_202302)
is in an archive, for reference. If neither copy exists, IzarraVM supplies no
replacement OVL and no diagnostic OVL.

IzarraVM does not download ROMs, Glide drivers, or game data, and it does not
distribute them. The metadata of an archive item is not a license. Use these
files only if you have the legal right to use them.

## Validation

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

## License

GNU GPL version 3 only (`GPL-3.0-only`; see [LICENSE](LICENSE)).
