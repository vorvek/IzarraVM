<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Recipes

The manuals in this set describe the Izarra3000 and its host application.
These pages describe how to do a specified task with them. Each page gives the
steps in order. It gives the reason for a step when the reason is not clear.

A recipe assumes that the machine is already in operation. It also assumes
that you read the manual for the part that the recipe uses. A setting is named
as the program shows it.

## Sound and music

- **[How to use your own MIDI player](host-midi-player.md)**: send the MPU-401
  music from the guest to a synthesizer or a player on the host. This
  procedure uses a virtual MIDI cable.
- **[The Roland Sound Canvas VSTi](sound-canvas-vsti.md)**: the same route,
  which ends at an SC-55/88-class instrument. Most General MIDI music of the
  middle 1990s was written for that hardware.
- **[Nuked-SC55 for Sound Canvas playback](nuked-sc55.md)**: the same
  instrument again, emulated from its own ROMs and not modelled.
- **[MT-32 ROMs with the P330 receiver](mt32-roms.md)**: the files that the
  ROM loader accepts, and the meaning of each failure message.

## Next

- [IzarraVM GUI guide](../izarravm-gui/guide.md): the host application and its
  Settings windows.
- [ReSonique II sound card manual](../resonique2/manual.md): the card behind
  P300 and P330.
- [Troubleshooting and FAQ](../troubleshooting.md): problems that a setup
  procedure does not solve.
