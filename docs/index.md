<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# IzarraVM

IzarraVM is a Rust emulator for the Izarra 3000, a DOS-era games computer
that almost shipped in 1997. It models one fixed machine: custom video and
audio around an MS-DOS compatible core, with Toka Disk Operating System as
its ROM shell and launcher.

The goal is to run early to mid 1990s DOS games as if the Izarra had reached
store shelves.

See the [README](https://github.com/vorvek/IzarraVM) for build and
installation instructions. These pages are the machine's manuals.

## Start here

- **[Izarra 3000 user manual](izarra-3000/user-manual.md)**, the machine
  itself: powering on, the POST screen, the boot menu, and the setup panel.
- **[Company story](izarra-3000/company-story.md)**: where the Izarra 3000
  and Toka-DOS came from.
- **[Using Toka-DOS](toka-dos/using-toka-dos.md)**: the bundled operating
  system, its shell, and its disk layout.
- **[DOS command reference](toka-dos/commands.md)**: every shipped
  command, with its switches.
- **[IzarraVM GUI guide](izarravm-gui/guide.md)**, the host application:
  config, mounting media, and where your files live.
- **[Troubleshooting & FAQ](troubleshooting.md)**: common problems and what
  to try.

## Recipes

Task-oriented procedures, as opposed to the reference manuals below.

- **[Recipes index](recipes/index.md)**: all of them.
- **[Using your own MIDI player](recipes/host-midi-player.md)**: send the
  guest's MPU-401 music to a player on the host.
- **[The Roland Sound Canvas VSTi](recipes/sound-canvas-vsti.md)**: that route,
  ending at SC-55/88-class playback.
- **[Nuked-SC55 for Sound Canvas playback](recipes/nuked-sc55.md)**: the same
  instrument, emulated from its own ROMs.
- **[MT-32 ROMs with the P330 receiver](recipes/mt32-roms.md)**: what the ROM
  loader accepts, and what each failure means.

## Hardware manuals

- **[IZBIOS Configuration Panel guide](izbios/configuration-panel.md)**: a
  per-screen walkthrough of the Del setup panel.
- **[TOKAEMM.SYS memory manager](tokaemm/manual.md)**: XMS, EMS, UMBs, and
  the V86 monitor underneath Toka-DOS.
- **[ReSonique 2 sound card manual](resonique2/manual.md)**: the audio
  hardware and what it does and doesn't support today.

## Technical references

- **[VGA core](vga-core/README.md)**: the IBM VGA-compatible video path,
  including its Hercules and CGA personalities.
- **[VEGA programmer's guide](vega/vega-programmers-guide.md)**: a working
  guide to drawing through the VEGA chipset (Margo, the 2D engine).
- **[VEGA technical reference](vega/vega-technical-reference.md)**: the
  register-level contract for Margo and Distira (3D).
