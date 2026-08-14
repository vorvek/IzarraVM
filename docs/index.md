<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# IzarraVM

IzarraVM is a Rust emulator for the Izarra3000. The Izarra3000 is a DOS-era
games computer that almost went on sale in 1997. IzarraVM emulates one fixed
machine: custom video and audio around an MS-DOS compatible core. The Toka
Disk Operating System (Toka-DOS) is the ROM shell and the launcher.

IzarraVM runs DOS games from the early 1990s and the middle 1990s. It runs
them as the Izarra3000 ran them.

The [README](https://github.com/vorvek/IzarraVM) gives the build instructions
and the installation instructions. These pages are the manuals for the
machine.

## Start here

- **[Izarra3000 user manual](izarra-3000/user-manual.md)**: the machine. It
  describes the start sequence, the POST screen, the boot menu, and the setup
  panel.
- **[How to use Toka-DOS](toka-dos/using-toka-dos.md)**: the supplied
  operating system, its shell, and its disk layout.
- **[DOS command reference](toka-dos/commands.md)**: each command in the
  system, with its switches.
- **[Toka-DOS licensing](toka-dos/licensing.md)**: the parts of the system,
  and their license terms.
- **[IzarraVM GUI guide](izarravm-gui/guide.md)**: the host application. It
  describes the configuration, how to mount media, and where the files are.
- **[Troubleshooting and FAQ](troubleshooting.md)**: frequent problems, and
  what to do about them.

## Recipes

A recipe gives a procedure for one task. The manuals below are reference
documents.

- **[Recipes index](recipes/index.md)**: all of the recipes.
- **[How to use your own MIDI player](recipes/host-midi-player.md)**: send the
  MPU-401 music from the guest to a player on the host.
- **[The Roland Sound Canvas VSTi](recipes/sound-canvas-vsti.md)**: the same
  route, which ends at SC-55/88-class playback.
- **[Nuked-SC55 for Sound Canvas playback](recipes/nuked-sc55.md)**: the same
  instrument, emulated from its own ROMs.
- **[MT-32 ROMs with the P330 receiver](recipes/mt32-roms.md)**: the files
  that the ROM loader accepts, and the meaning of each failure.

## Hardware manuals

- **[IZBIOS Configuration Panel guide](izbios/configuration-panel.md)**: each
  screen of the Del setup panel.
- **[TOKAEMM.SYS memory manager](tokaemm/manual.md)**: XMS, EMS, UMBs, and the
  V86 monitor below Toka-DOS.
- **[ReSonique II sound card manual](resonique2/manual.md)**: the audio
  hardware, and its limits.

## Technical references

- **[VGA core](vga-core/README.md)**: the IBM VGA-compatible video path, with
  its Hercules and CGA modes.
- **[VEGA programmer's guide](vega/vega-programmers-guide.md)**: how to draw
  through the VEGA chipset (Margo, the 2D engine).
- **[VEGA technical reference](vega/vega-technical-reference.md)**: the
  register-level contract for Margo and Distira (3D).
